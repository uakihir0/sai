//! Reconstruction of the two sides of Git conflict markers in `sai.lock`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::lock::{LockPackageField, LockParseOutput, LockSource, parse};
use crate::lock_validation::{LockValidationError, validate};
use crate::mvs::MvsNode;
use crate::version::Version;

/// The lock text reconstructed from both sides of a merge conflict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockConflictSides {
    ours: Vec<u8>,
    theirs: Vec<u8>,
}

impl LockConflictSides {
    pub fn ours(&self) -> &[u8] {
        &self.ours
    }

    pub fn theirs(&self) -> &[u8] {
        &self.theirs
    }

    pub fn into_parts(self) -> (Vec<u8>, Vec<u8>) {
        (self.ours, self.theirs)
    }
}

/// A malformed conflict-marker sequence in a lock file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockConflictError {
    pub kind: LockConflictErrorKind,
    pub span: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockConflictErrorKind {
    BaseWithoutStart,
    SeparatorWithoutStart,
    EndWithoutStart,
    NestedStart,
    DuplicateBase,
    BaseAfterSeparator,
    RepeatedSeparator,
    SeparatorAfterBaseMissing,
    EndBeforeSeparator,
    Unterminated,
}

/// Which reconstructed lock side failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockConflictSide {
    Ours,
    Theirs,
}

/// A reconstructed side could not be used as a merge baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockConflictValidationError {
    InvalidSyntax {
        side: LockConflictSide,
        parsed: Box<LockParseOutput>,
    },
    InvalidSemantics {
        side: LockConflictSide,
        parsed: Box<LockParseOutput>,
        errors: Vec<LockValidationError>,
    },
    InvalidPackageNode {
        side: LockConflictSide,
        package: Box<str>,
    },
    MissingGitContentMetadata {
        side: LockConflictSide,
        package: Box<str>,
        version: Version,
    },
    ConflictingKnownContent(Box<LockConflictContentConflict>),
}

/// The two inconsistent records for the same package version and Git source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockConflictContentConflict {
    pub node: MvsNode,
    pub version: Version,
    pub source: LockConflictGitSource,
    pub ours: LockConflictGitContent,
    pub theirs: LockConflictGitContent,
}

/// A Git source identity recorded in a lock entry.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LockConflictGitSource {
    pub url: Box<str>,
    /// `None` identifies the repository root; `dir "."` is normalized to it.
    pub directory: Option<Box<str>>,
}

/// Content metadata that must agree when both sides record the same Git source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockConflictGitContent {
    pub commit: Box<str>,
    pub checksum: Box<str>,
    pub wit_checksum: Option<Box<str>>,
    pub artifact_checksums: BTreeMap<Box<str>, Box<str>>,
}

/// Validated merge inputs and the combined MVS floors from both conflict sides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedLockConflictSides {
    ours: LockParseOutput,
    theirs: LockParseOutput,
    floors: BTreeMap<MvsNode, Version>,
    pinned_nodes: BTreeSet<MvsNode>,
    known_git_content: BTreeMap<(MvsNode, Version, LockConflictGitSource), LockConflictGitContent>,
    previous_footprints: BTreeMap<MvsNode, BTreeSet<Box<str>>>,
}

impl ValidatedLockConflictSides {
    pub const fn ours(&self) -> &LockParseOutput {
        &self.ours
    }

    pub const fn theirs(&self) -> &LockParseOutput {
        &self.theirs
    }

    pub const fn floors(&self) -> &BTreeMap<MvsNode, Version> {
        &self.floors
    }

    /// Nodes pinned by a path entry on either side or by the current root
    /// contract's `use path` overrides.
    pub const fn pinned_nodes(&self) -> &BTreeSet<MvsNode> {
        &self.pinned_nodes
    }

    /// Known commit and checksum records, keyed by node, exact version, and
    /// canonical Git source identity.
    pub const fn known_git_content(
        &self,
    ) -> &BTreeMap<(MvsNode, Version, LockConflictGitSource), LockConflictGitContent> {
        &self.known_git_content
    }

    /// The union of the recorded capability footprints from both sides,
    /// including artifact imports, keyed by identity and compatibility line.
    pub const fn previous_footprints(&self) -> &BTreeMap<MvsNode, BTreeSet<Box<str>>> {
        &self.previous_footprints
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Marker {
    Start,
    Base,
    Separator,
    End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Outside,
    Ours,
    Base,
    Theirs,
}

/// Returns the span of the first conflict-marker line, if one is present.
///
/// This uses the same marker spelling rules as lock parsing, but does not
/// require markers to form a balanced conflict region.
pub fn first_conflict_marker(source: &[u8]) -> Option<Range<usize>> {
    for (line, _) in source_lines(source) {
        if let Some((_, span)) = classify_marker(source, line) {
            return Some(span);
        }
    }
    None
}

/// Reconstructs the `ours` and `theirs` lock texts around one or more
/// non-nested conflict regions.
///
/// Bytes outside conflict regions are copied to both sides. Ours content is
/// copied only to `ours`, theirs content only to `theirs`, and a diff3 base
/// section is omitted from both. Line endings and any byte-order mark outside
/// marker text are preserved exactly.
pub fn reconstruct_conflict_sides(
    source: &[u8],
) -> Result<Option<LockConflictSides>, LockConflictError> {
    let mut ours = Vec::with_capacity(source.len());
    let mut theirs = Vec::with_capacity(source.len());
    let mut state = State::Outside;
    let mut has_markers = false;

    for (line, full_line) in source_lines(source) {
        let marker = classify_marker(source, line);
        let Some((kind, marker_span)) = marker else {
            match state {
                State::Outside => {
                    ours.extend_from_slice(&source[full_line.clone()]);
                    theirs.extend_from_slice(&source[full_line]);
                }
                State::Ours => ours.extend_from_slice(&source[full_line]),
                State::Base => {}
                State::Theirs => theirs.extend_from_slice(&source[full_line]),
            }
            continue;
        };

        has_markers = true;
        match (state, kind) {
            (State::Outside, Marker::Start) => state = State::Ours,
            (State::Outside, Marker::Base) => {
                return Err(conflict_error(
                    LockConflictErrorKind::BaseWithoutStart,
                    marker_span,
                ));
            }
            (State::Outside, Marker::Separator) => {
                return Err(conflict_error(
                    LockConflictErrorKind::SeparatorWithoutStart,
                    marker_span,
                ));
            }
            (State::Outside, Marker::End) => {
                return Err(conflict_error(
                    LockConflictErrorKind::EndWithoutStart,
                    marker_span,
                ));
            }
            (State::Ours, Marker::Start)
            | (State::Base, Marker::Start)
            | (State::Theirs, Marker::Start) => {
                return Err(conflict_error(
                    LockConflictErrorKind::NestedStart,
                    marker_span,
                ));
            }
            (State::Ours, Marker::Base) => state = State::Base,
            (State::Ours, Marker::Separator) => state = State::Theirs,
            (State::Ours, Marker::End) => {
                return Err(conflict_error(
                    LockConflictErrorKind::EndBeforeSeparator,
                    marker_span,
                ));
            }
            (State::Base, Marker::Base) => {
                return Err(conflict_error(
                    LockConflictErrorKind::DuplicateBase,
                    marker_span,
                ));
            }
            (State::Base, Marker::Separator) => state = State::Theirs,
            (State::Base, Marker::End) => {
                return Err(conflict_error(
                    LockConflictErrorKind::SeparatorAfterBaseMissing,
                    marker_span,
                ));
            }
            (State::Theirs, Marker::Base) => {
                return Err(conflict_error(
                    LockConflictErrorKind::BaseAfterSeparator,
                    marker_span,
                ));
            }
            (State::Theirs, Marker::Separator) => {
                return Err(conflict_error(
                    LockConflictErrorKind::RepeatedSeparator,
                    marker_span,
                ));
            }
            (State::Theirs, Marker::End) => state = State::Outside,
        }
    }

    if state != State::Outside {
        return Err(conflict_error(
            LockConflictErrorKind::Unterminated,
            source.len()..source.len(),
        ));
    }

    Ok(has_markers.then_some(LockConflictSides { ours, theirs }))
}

/// Parses and validates both reconstructed locks, then combines their version
/// floors. A node pinned by a path entry on either side or by the current root
/// contract does not receive a floor.
pub fn validate_conflict_sides(
    sides: &LockConflictSides,
    root_pinned_nodes: &BTreeSet<MvsNode>,
) -> Result<ValidatedLockConflictSides, LockConflictValidationError> {
    let ours = parse(sides.ours());
    validate_conflict_side(LockConflictSide::Ours, &ours)?;
    let theirs = parse(sides.theirs());
    validate_conflict_side(LockConflictSide::Theirs, &theirs)?;

    let mut pinned_nodes = root_pinned_nodes.clone();
    add_path_pins(LockConflictSide::Ours, &ours, &mut pinned_nodes)?;
    add_path_pins(LockConflictSide::Theirs, &theirs, &mut pinned_nodes)?;

    let mut floors = BTreeMap::new();
    let mut known_git_content =
        BTreeMap::<(MvsNode, Version, LockConflictGitSource), LockConflictGitContent>::new();
    let mut previous_footprints = BTreeMap::<MvsNode, BTreeSet<Box<str>>>::new();
    for (side, parsed) in [
        (LockConflictSide::Ours, &ours),
        (LockConflictSide::Theirs, &theirs),
    ] {
        for package in &parsed.file.packages {
            let node = package_node(side, &package.value.package, &package.value.version)?;
            if !pinned_nodes.contains(&node) {
                floors
                    .entry(node.clone())
                    .and_modify(|floor: &mut Version| {
                        if package.value.version > *floor {
                            *floor = package.value.version.clone();
                        }
                    })
                    .or_insert_with(|| package.value.version.clone());
            }

            let footprint = previous_footprints.entry(node.clone()).or_default();
            let mut source_url = None;
            let mut directory = None;
            let mut commit = None;
            let mut checksum = None;
            let mut wit_checksum = None;
            let mut artifact_checksums = BTreeMap::new();
            for field in &package.value.fields {
                match &field.value {
                    LockPackageField::Source(LockSource::Git(url)) => {
                        source_url = Some(url.clone())
                    }
                    LockPackageField::Directory(value) if value.as_ref() != "." => {
                        directory = Some(value.clone());
                    }
                    LockPackageField::Commit(value) => commit = Some(value.clone()),
                    LockPackageField::Checksum(value) => checksum = Some(value.clone()),
                    LockPackageField::WitChecksum(value) => wit_checksum = Some(value.clone()),
                    LockPackageField::Artifact { target, checksum } => {
                        artifact_checksums.insert(target.clone(), checksum.clone());
                    }
                    LockPackageField::Capabilities(capabilities) => {
                        footprint.extend(
                            capabilities
                                .iter()
                                .map(|capability| capability.value.clone()),
                        );
                    }
                    LockPackageField::Imports { capabilities, .. } => {
                        footprint.extend(
                            capabilities
                                .iter()
                                .map(|capability| capability.value.clone()),
                        );
                    }
                    LockPackageField::Source(LockSource::Path(_))
                    | LockPackageField::Directory(_) => {}
                    LockPackageField::Compiler(_) | LockPackageField::Requires(_) => {}
                }
            }

            let Some(url) = source_url else {
                continue;
            };
            let (Some(commit), Some(checksum)) = (commit, checksum) else {
                return Err(LockConflictValidationError::MissingGitContentMetadata {
                    side,
                    package: package.value.package.clone(),
                    version: package.value.version.clone(),
                });
            };
            let source = LockConflictGitSource { url, directory };
            let content = LockConflictGitContent {
                commit,
                checksum,
                wit_checksum,
                artifact_checksums,
            };
            let key = (node.clone(), package.value.version.clone(), source.clone());
            if let Some(ours) = known_git_content.get(&key) {
                if ours != &content {
                    return Err(LockConflictValidationError::ConflictingKnownContent(
                        Box::new(LockConflictContentConflict {
                            node,
                            version: package.value.version.clone(),
                            source,
                            ours: ours.clone(),
                            theirs: content,
                        }),
                    ));
                }
            } else {
                known_git_content.insert(key, content);
            }
        }
    }

    Ok(ValidatedLockConflictSides {
        ours,
        theirs,
        floors,
        pinned_nodes,
        known_git_content,
        previous_footprints,
    })
}

fn validate_conflict_side(
    side: LockConflictSide,
    parsed: &LockParseOutput,
) -> Result<(), LockConflictValidationError> {
    if !parsed.is_syntax_valid() {
        return Err(LockConflictValidationError::InvalidSyntax {
            side,
            parsed: Box::new(parsed.clone()),
        });
    }
    let validation = validate(parsed);
    if !validation.is_valid() {
        return Err(LockConflictValidationError::InvalidSemantics {
            side,
            parsed: Box::new(parsed.clone()),
            errors: validation.errors,
        });
    }
    Ok(())
}

fn add_path_pins(
    side: LockConflictSide,
    parsed: &LockParseOutput,
    pinned_nodes: &mut BTreeSet<MvsNode>,
) -> Result<(), LockConflictValidationError> {
    for package in &parsed.file.packages {
        if package
            .value
            .fields
            .iter()
            .any(|field| matches!(&field.value, LockPackageField::Source(LockSource::Path(_))))
        {
            pinned_nodes.insert(package_node(
                side,
                &package.value.package,
                &package.value.version,
            )?);
        }
    }
    Ok(())
}

fn package_node(
    side: LockConflictSide,
    package: &str,
    version: &Version,
) -> Result<MvsNode, LockConflictValidationError> {
    MvsNode::new(package, version.compatibility_line()).map_err(|_| {
        LockConflictValidationError::InvalidPackageNode {
            side,
            package: package.into(),
        }
    })
}

fn conflict_error(kind: LockConflictErrorKind, span: Range<usize>) -> LockConflictError {
    LockConflictError { kind, span }
}

fn source_lines(source: &[u8]) -> impl Iterator<Item = (Range<usize>, Range<usize>)> + '_ {
    let mut offset = 0;
    std::iter::from_fn(move || {
        if offset >= source.len() {
            return None;
        }
        let start = offset;
        let content_end = source[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(source.len(), |newline| start + newline);
        let full_end = if content_end < source.len() {
            content_end + 1
        } else {
            content_end
        };
        offset = full_end;
        let mut line_end = content_end;
        if line_end > start && source[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        Some((start..line_end, start..full_end))
    })
}

fn classify_marker(source: &[u8], line: Range<usize>) -> Option<(Marker, Range<usize>)> {
    let mut content_start = line.start;
    if line.start == 0 && source.starts_with(b"\xef\xbb\xbf") {
        content_start += 3;
    }
    let content = source.get(content_start..line.end)?;

    for (prefix, marker) in [
        (b"<<<<<<<".as_slice(), Marker::Start),
        (b"|||||||".as_slice(), Marker::Base),
        (b">>>>>>>".as_slice(), Marker::End),
    ] {
        if content.starts_with(prefix) && content.get(prefix.len()).is_none_or(|byte| *byte == b' ')
        {
            return Some((marker, content_start..content_start + prefix.len()));
        }
    }
    if content == b"=======" {
        return Some((Marker::Separator, content_start..line.end));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{LockConflictErrorKind, first_conflict_marker, reconstruct_conflict_sides};
    use super::{
        LockConflictSide, LockConflictSides, LockConflictValidationError, validate_conflict_sides,
    };
    use crate::lock::{LockParseErrorKind, parse};
    use crate::mvs::MvsNode;
    use crate::version::Version;
    use std::collections::BTreeSet;

    #[test]
    fn reconstructs_both_sides_and_preserves_common_bytes() {
        let source = b"prefix\r\n<<<<<<< ours\r\nours line\n||||||| base\r\nbase line\n=======\r\ntheirs line\n>>>>>>> theirs\r\nsuffix";
        let sides = reconstruct_conflict_sides(source)
            .expect("well-formed conflict")
            .expect("markers present");
        assert_eq!(sides.ours(), b"prefix\r\nours line\nsuffix");
        assert_eq!(sides.theirs(), b"prefix\r\ntheirs line\nsuffix");
    }

    #[test]
    fn reconstructs_multiple_conflicts_and_parses_each_lock_side() {
        let source = b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n<<<<<<< ours\n    requires acme:left @ ^1.0 -> 1.1.0\n=======\n    requires acme:right @ ^2.0 -> 2.1.0\n>>>>>>> theirs\n<<<<<<< ours\npackage acme:left @ 1.1.0\n    source path \"../left\"\n    compiler ^0.1\n=======\npackage acme:right @ 2.1.0\n    source path \"../right\"\n    compiler ^0.1\n>>>>>>> theirs\n";
        let sides = reconstruct_conflict_sides(source)
            .expect("well-formed conflict")
            .expect("markers present");
        assert_eq!(
            sides.ours(),
            b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n    requires acme:left @ ^1.0 -> 1.1.0\npackage acme:left @ 1.1.0\n    source path \"../left\"\n    compiler ^0.1\n"
        );
        assert_eq!(
            sides.theirs(),
            b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n    requires acme:right @ ^2.0 -> 2.1.0\npackage acme:right @ 2.1.0\n    source path \"../right\"\n    compiler ^0.1\n"
        );
        for side in [sides.ours(), sides.theirs()] {
            let parsed = parse(side);
            assert!(parsed.is_syntax_valid(), "{:?}", parsed.errors());
        }
    }

    #[test]
    fn recognizes_marker_spelling_without_accepting_near_misses() {
        let source = b"\xef\xbb\xbf<<<<<<< branch\n=======\r\n";
        assert_eq!(first_conflict_marker(source), Some(3..10));
        assert_eq!(first_conflict_marker(b"<<<<<<<branch\n"), None);
        assert_eq!(first_conflict_marker(b" =======\n"), None);
        assert_eq!(first_conflict_marker(b"========\n"), None);
    }

    #[test]
    fn returns_no_sides_when_no_conflict_marker_exists() {
        assert_eq!(reconstruct_conflict_sides(b"lock 1\nroot\n").unwrap(), None);
    }

    #[test]
    fn rejects_nested_and_unbalanced_marker_sequences() {
        for (source, expected) in [
            (
                b"<<<<<<< ours\n<<<<<<< nested\n".as_slice(),
                LockConflictErrorKind::NestedStart,
            ),
            (
                b"||||||| base\n".as_slice(),
                LockConflictErrorKind::BaseWithoutStart,
            ),
            (
                b"=======\n".as_slice(),
                LockConflictErrorKind::SeparatorWithoutStart,
            ),
            (
                b">>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::EndWithoutStart,
            ),
            (
                b"<<<<<<< ours\nours\n>>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::EndBeforeSeparator,
            ),
            (
                b"<<<<<<< ours\nours\n".as_slice(),
                LockConflictErrorKind::Unterminated,
            ),
            (
                b"<<<<<<< ours\nours\n||||||| base\nbase\n>>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::SeparatorAfterBaseMissing,
            ),
            (
                b"<<<<<<< ours\nours\n||||||| base\nbase\n||||||| duplicate\n=======\ntheirs\n>>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::DuplicateBase,
            ),
            (
                b"<<<<<<< ours\nours\n=======\ntheirs\n=======\n>>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::RepeatedSeparator,
            ),
            (
                b"<<<<<<< ours\nours\n=======\ntheirs\n||||||| late base\n>>>>>>> theirs\n".as_slice(),
                LockConflictErrorKind::BaseAfterSeparator,
            ),
        ] {
            let error = reconstruct_conflict_sides(source).unwrap_err();
            assert_eq!(error.kind, expected);
            assert!(error.span.start <= error.span.end);
        }
    }

    #[test]
    fn lock_parser_still_rejects_the_first_marker_before_lexing() {
        let source = b"\xef\xbb\xbf\xff\n=======\r\n";
        let parsed = parse(source);
        assert_eq!(
            parsed.errors()[0].kind,
            LockParseErrorKind::LockMergeConflict
        );
        assert_eq!(parsed.errors()[0].span, 5..12);
    }

    #[test]
    fn combines_the_highest_git_floor_from_both_valid_sides() {
        let sides = LockConflictSides {
            ours: git_lock("1.2.0").into_bytes(),
            theirs: git_lock("1.4.0").into_bytes(),
        };
        let merged = validate_conflict_sides(&sides, &BTreeSet::new()).unwrap();
        let node = MvsNode::new(
            "acme:dep",
            Version::parse("1.0.0").unwrap().compatibility_line(),
        )
        .unwrap();
        assert_eq!(
            merged.floors().get(&node),
            Some(&Version::parse("1.4.0").unwrap())
        );
        assert!(merged.pinned_nodes().is_empty());
    }

    #[test]
    fn path_pin_on_either_side_suppresses_the_node_floor() {
        let sides = LockConflictSides {
            ours: path_lock("1.2.0").into_bytes(),
            theirs: git_lock("1.4.0").into_bytes(),
        };
        let merged = validate_conflict_sides(&sides, &BTreeSet::new()).unwrap();
        let node = MvsNode::new(
            "acme:dep",
            Version::parse("1.0.0").unwrap().compatibility_line(),
        )
        .unwrap();
        assert!(merged.pinned_nodes().contains(&node));
        assert!(!merged.floors().contains_key(&node));
    }

    #[test]
    fn root_override_pins_suppress_git_floors() {
        let sides = LockConflictSides {
            ours: git_lock("1.2.0").into_bytes(),
            theirs: git_lock("1.4.0").into_bytes(),
        };
        let node = MvsNode::new(
            "acme:dep",
            Version::parse("1.0.0").unwrap().compatibility_line(),
        )
        .unwrap();
        let merged = validate_conflict_sides(&sides, &BTreeSet::from([node.clone()])).unwrap();
        assert!(merged.pinned_nodes().contains(&node));
        assert!(!merged.floors().contains_key(&node));
    }

    #[test]
    fn rejects_a_syntactically_invalid_side_with_its_side_identity() {
        let sides = LockConflictSides {
            ours: b"not a lock".to_vec(),
            theirs: git_lock("1.4.0").into_bytes(),
        };
        assert!(matches!(
            validate_conflict_sides(&sides, &BTreeSet::new()),
            Err(LockConflictValidationError::InvalidSyntax {
                side: LockConflictSide::Ours,
                ..
            })
        ));
    }

    #[test]
    fn rejects_a_semantically_invalid_lock_on_either_side() {
        for (ours, theirs, expected_side) in [
            (
                semantically_invalid_lock(),
                git_lock("1.4.0"),
                LockConflictSide::Ours,
            ),
            (
                git_lock("1.4.0"),
                semantically_invalid_lock(),
                LockConflictSide::Theirs,
            ),
        ] {
            let sides = LockConflictSides {
                ours: ours.into_bytes(),
                theirs: theirs.into_bytes(),
            };
            match validate_conflict_sides(&sides, &BTreeSet::new()).unwrap_err() {
                LockConflictValidationError::InvalidSemantics {
                    side,
                    parsed,
                    errors,
                } => {
                    assert_eq!(side, expected_side);
                    assert!(parsed.is_syntax_valid());
                    assert!(!errors.is_empty());
                }
                other => panic!("expected semantic validation failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_different_content_for_the_same_locked_git_source() {
        for (ours_commit, ours_checksum, theirs_commit, theirs_checksum) in
            [('a', 'a', 'b', 'a'), ('a', 'a', 'a', 'b')]
        {
            let sides = LockConflictSides {
                ours: git_lock_with(
                    "1.4.0",
                    "https://example.test/dep",
                    ours_commit,
                    ours_checksum,
                    "alloc",
                    None,
                    None,
                    None,
                    'c',
                )
                .into_bytes(),
                theirs: git_lock_with(
                    "1.4.0",
                    "https://example.test/dep",
                    theirs_commit,
                    theirs_checksum,
                    "alloc",
                    None,
                    None,
                    None,
                    'c',
                )
                .into_bytes(),
            };
            match validate_conflict_sides(&sides, &BTreeSet::new()).unwrap_err() {
                LockConflictValidationError::ConflictingKnownContent(conflict) => {
                    assert_eq!(conflict.source.url.as_ref(), "https://example.test/dep");
                    assert_eq!(conflict.ours.commit, "a".repeat(40).into_boxed_str());
                    assert_eq!(
                        conflict.theirs.commit,
                        theirs_commit.to_string().repeat(40).into_boxed_str()
                    );
                    assert_eq!(
                        conflict.ours.checksum,
                        format!("sai1:{}", ours_checksum.to_string().repeat(64)).into_boxed_str()
                    );
                    assert_eq!(
                        conflict.theirs.checksum,
                        format!("sai1:{}", theirs_checksum.to_string().repeat(64)).into_boxed_str()
                    );
                }
                other => panic!("expected conflicting content, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_wit_or_artifact_checksum_disagreement() {
        for (ours_wit, theirs_wit, ours_artifact, theirs_artifact) in
            [('b', 'c', 'd', 'd'), ('b', 'b', 'd', 'e')]
        {
            let sides = LockConflictSides {
                ours: git_lock_with(
                    "1.4.0",
                    "https://example.test/dep",
                    'a',
                    'a',
                    "alloc",
                    None,
                    Some(ours_wit),
                    Some("wasi:filesystem"),
                    ours_artifact,
                )
                .into_bytes(),
                theirs: git_lock_with(
                    "1.4.0",
                    "https://example.test/dep",
                    'a',
                    'a',
                    "alloc",
                    None,
                    Some(theirs_wit),
                    Some("wasi:filesystem"),
                    theirs_artifact,
                )
                .into_bytes(),
            };
            assert!(matches!(
                validate_conflict_sides(&sides, &BTreeSet::new()),
                Err(LockConflictValidationError::ConflictingKnownContent(_))
            ));
        }
    }

    #[test]
    fn identical_known_git_content_is_reused_once() {
        let lock = git_lock_with(
            "1.4.0",
            "https://example.test/dep",
            'a',
            'a',
            "alloc",
            Some("."),
            None,
            None,
            'b',
        );
        let sides = LockConflictSides {
            ours: lock.clone().into_bytes(),
            theirs: lock.into_bytes(),
        };
        let merged = validate_conflict_sides(&sides, &BTreeSet::new()).unwrap();
        assert_eq!(merged.known_git_content().len(), 1);
    }

    #[test]
    fn keeps_distinct_sources_for_reuse_and_normalizes_dot_directory() {
        let sides = LockConflictSides {
            ours: git_lock_with(
                "1.4.0",
                "https://example.test/dep",
                'a',
                'a',
                "alloc",
                Some("."),
                None,
                None,
                'c',
            )
            .into_bytes(),
            theirs: git_lock_with(
                "1.4.0",
                "https://example.test/mirror",
                'b',
                'b',
                "alloc",
                None,
                None,
                None,
                'c',
            )
            .into_bytes(),
        };
        let merged = validate_conflict_sides(&sides, &BTreeSet::new()).unwrap();
        assert_eq!(merged.known_git_content().len(), 2);
        assert!(
            merged
                .known_git_content()
                .keys()
                .any(
                    |(_, _, source)| source.url.as_ref() == "https://example.test/dep"
                        && source.directory.is_none()
                )
        );
    }

    #[test]
    fn unions_source_capabilities_and_artifact_imports_by_node() {
        let sides = LockConflictSides {
            ours: git_lock_with(
                "1.2.0",
                "https://example.test/dep",
                'a',
                'a',
                "alloc",
                None,
                None,
                None,
                'c',
            )
            .into_bytes(),
            theirs: git_lock_with(
                "1.4.0",
                "https://example.test/dep",
                'b',
                'b',
                "alloc",
                None,
                Some('e'),
                Some("wasi:filesystem"),
                'd',
            )
            .into_bytes(),
        };
        let merged = validate_conflict_sides(&sides, &BTreeSet::new()).unwrap();
        let node = MvsNode::new(
            "acme:dep",
            Version::parse("1.0.0").unwrap().compatibility_line(),
        )
        .unwrap();
        assert_eq!(
            merged.previous_footprints().get(&node),
            Some(&BTreeSet::from([
                Box::<str>::from("alloc"),
                Box::<str>::from("wasi:filesystem"),
            ]))
        );
    }

    fn semantically_invalid_lock() -> String {
        "lock 1\n\nroot acme:app @ 1.0.0\n".to_string()
    }

    fn git_lock(version: &str) -> String {
        git_lock_with(
            version,
            "https://example.test/dep",
            'a',
            'a',
            "",
            None,
            None,
            None,
            'b',
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn git_lock_with(
        version: &str,
        url: &str,
        commit_byte: char,
        checksum_byte: char,
        capabilities: &str,
        directory: Option<&str>,
        wit_checksum_byte: Option<char>,
        imports: Option<&str>,
        artifact_checksum_byte: char,
    ) -> String {
        let directory = directory.map_or_else(String::new, |directory| {
            format!("    dir \"{directory}\"\n")
        });
        let wit = wit_checksum_byte.map_or_else(String::new, |byte| {
            format!("    wit \"sai1:{}\"\n", byte.to_string().repeat(64))
        });
        let artifact = imports.map_or_else(String::new, |imports| {
            format!(
                "    artifact wasm32-wasi \"sai1:{}\"\n    imports wasm32-wasi {{ {imports} }}\n",
                artifact_checksum_byte.to_string().repeat(64)
            )
        });
        format!(
            "lock 1\n\nroot acme:app @ 1.0.0\n    targets {{ wasm32-wasi }}\n    requires acme:dep @ ^1.0 -> {version}\n\npackage acme:dep @ {version}\n    source git \"{url}\"\n{directory}    commit \"{}\"\n    checksum \"sai1:{}\"\n{wit}    compiler ^0.1\n    capabilities {{ {capabilities} }}\n{artifact}",
            commit_byte.to_string().repeat(40),
            checksum_byte.to_string().repeat(64),
        )
    }

    fn path_lock(version: &str) -> String {
        format!(
            "lock 1\n\nroot acme:app @ 1.0.0\n    targets {{ wasm32-wasi }}\n    requires acme:dep @ ^1.0 -> {version}\n\npackage acme:dep @ {version}\n    source path \"../dep\"\n    compiler ^0.1\n",
        )
    }
}
