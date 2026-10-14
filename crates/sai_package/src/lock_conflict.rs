//! Reconstruction of the two sides of Git conflict markers in `sai.lock`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::lock::{LockPackage, LockPackageField, LockParseOutput, LockSource, parse};
use crate::lock_validation::{
    LockValidationError, SUPPORTED_LOCK_FORMAT_VERSION, canonical_git_url, validate,
};
use crate::mvs::MvsNode;
use crate::mvs::MvsPackageVersion;
use crate::package_path::RepositoryPackageDirectory;
use crate::resolver_source::ResolverPackageSource;
use crate::version::{CompatibilityLine, Version};

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

/// A lock input could not be used as a merge baseline.
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
    InvalidCleanMergeSyntax {
        parsed: Box<LockParseOutput>,
    },
    InvalidCleanMergeSemantics {
        parsed: Box<LockParseOutput>,
        errors: Vec<LockValidationError>,
    },
    UnsupportedFormatVersion {
        side: Option<LockConflictSide>,
        version: u64,
        span: Range<usize>,
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
    TooManyCleanMergeEntries {
        package: Box<str>,
        line: CompatibilityLine,
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
    known_git_packages: BTreeMap<(MvsPackageVersion, LockConflictGitSource), LockPackage>,
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

    /// Git package records keyed by exact package version and canonical source.
    ///
    /// Records from both sides are deduplicated only after their shared key's
    /// commit and checksums have been validated as equal. If the remaining
    /// package metadata differs, the bytewise-smaller canonical package block
    /// is retained so swapping conflict sides cannot change the catalog.
    pub const fn known_git_packages(
        &self,
    ) -> &BTreeMap<(MvsPackageVersion, LockConflictGitSource), LockPackage> {
        &self.known_git_packages
    }

    /// The union of the recorded capability footprints from both sides,
    /// including artifact imports, keyed by identity and compatibility line.
    pub const fn previous_footprints(&self) -> &BTreeMap<MvsNode, BTreeSet<Box<str>>> {
        &self.previous_footprints
    }

    /// Returns the retained content record for this exact package version and
    /// selected Git source. Distinct sources for one version stay independent.
    pub fn known_git_content_for(
        &self,
        package_version: &MvsPackageVersion,
        source: &ResolverPackageSource,
    ) -> Option<&LockConflictGitContent> {
        let ResolverPackageSource::Git { url, directory } = source else {
            return None;
        };
        let url = canonical_git_url(url).ok()?.into_boxed_str();
        let directory = directory
            .as_deref()
            .map(RepositoryPackageDirectory::parse)
            .transpose()
            .ok()?
            .map(|directory| directory.as_str().to_owned().into_boxed_str())
            .filter(|directory| directory.as_ref() != ".");
        let key = (
            package_version.node(),
            package_version.version().clone(),
            LockConflictGitSource { url, directory },
        );
        self.known_git_content.get(&key)
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
    let theirs = parse(sides.theirs());
    validate_parsed_conflict_sides(ours, theirs, root_pinned_nodes)
}

/// Treats duplicate package nodes in a clean textual merge as the two sides
/// described by package/03 §3.9.3.
///
/// Entries that are not duplicated are included on both sides. For a node
/// with exactly two entries, the first entry in the file is assigned to ours
/// and the second to theirs. More than two entries for one node are corrupt.
pub fn validate_clean_merge_duplicates(
    parsed: &LockParseOutput,
    root_pinned_nodes: &BTreeSet<MvsNode>,
) -> Result<Option<ValidatedLockConflictSides>, LockConflictValidationError> {
    if let Some(error) = unsupported_format_version(parsed, None) {
        return Err(error);
    }
    if !parsed.is_syntax_valid() {
        return Err(LockConflictValidationError::InvalidCleanMergeSyntax {
            parsed: Box::new(parsed.clone()),
        });
    }

    let mut groups = BTreeMap::<(Box<str>, CompatibilityLine), Vec<usize>>::new();
    for (index, package) in parsed.file.packages.iter().enumerate() {
        groups
            .entry((
                package.value.package.clone(),
                package.value.version.compatibility_line(),
            ))
            .or_default()
            .push(index);
    }
    let duplicates = groups
        .into_iter()
        .filter(|(_, indexes)| indexes.len() > 1)
        .collect::<Vec<_>>();

    if duplicates.is_empty() {
        let validation = validate(parsed);
        if !validation.is_valid() {
            return Err(LockConflictValidationError::InvalidCleanMergeSemantics {
                parsed: Box::new(parsed.clone()),
                errors: validation.errors,
            });
        }
        return Ok(None);
    }

    if let Some(((package, line), _)) = duplicates.iter().find(|(_, indexes)| indexes.len() > 2) {
        return Err(LockConflictValidationError::TooManyCleanMergeEntries {
            package: package.clone(),
            line: *line,
        });
    }

    let mut ours_indices = BTreeSet::new();
    let mut theirs_indices = BTreeSet::new();
    for (_, indexes) in duplicates {
        ours_indices.insert(indexes[0]);
        theirs_indices.insert(indexes[1]);
    }

    let mut ours = parsed.clone();
    let mut theirs = parsed.clone();
    ours.file.packages.clear();
    theirs.file.packages.clear();
    for (index, package) in parsed.file.packages.iter().enumerate() {
        if ours_indices.contains(&index) || !theirs_indices.contains(&index) {
            ours.file.packages.push(package.clone());
        }
        if theirs_indices.contains(&index) || !ours_indices.contains(&index) {
            theirs.file.packages.push(package.clone());
        }
    }

    validate_parsed_conflict_sides(ours, theirs, root_pinned_nodes).map(Some)
}

fn validate_parsed_conflict_sides(
    ours: LockParseOutput,
    theirs: LockParseOutput,
    root_pinned_nodes: &BTreeSet<MvsNode>,
) -> Result<ValidatedLockConflictSides, LockConflictValidationError> {
    if let Some(error) = unsupported_format_version(&ours, Some(LockConflictSide::Ours)) {
        return Err(error);
    }
    if let Some(error) = unsupported_format_version(&theirs, Some(LockConflictSide::Theirs)) {
        return Err(error);
    }
    validate_conflict_side(LockConflictSide::Ours, &ours)?;
    validate_conflict_side(LockConflictSide::Theirs, &theirs)?;
    let mut pinned_nodes = root_pinned_nodes.clone();
    add_path_pins(LockConflictSide::Ours, &ours, &mut pinned_nodes)?;
    add_path_pins(LockConflictSide::Theirs, &theirs, &mut pinned_nodes)?;

    let mut floors = BTreeMap::new();
    let mut known_git_content =
        BTreeMap::<(MvsNode, Version, LockConflictGitSource), LockConflictGitContent>::new();
    let mut known_git_packages =
        BTreeMap::<(MvsPackageVersion, LockConflictGitSource), LockPackage>::new();
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

            let package_version = MvsPackageVersion::new(
                package.value.package.clone(),
                package.value.version.clone(),
            )
            .map_err(|_| LockConflictValidationError::InvalidPackageNode {
                side,
                package: package.value.package.clone(),
            })?;
            let key = (package_version, source);
            let replace_existing = match known_git_packages.get(&key) {
                Some(existing) => {
                    let existing =
                        crate::lock_serialization::serialize_package_for_ordering(existing);
                    let candidate =
                        crate::lock_serialization::serialize_package_for_ordering(&package.value);
                    candidate.as_bytes() < existing.as_bytes()
                }
                None => true,
            };
            if replace_existing {
                known_git_packages.insert(key, package.value.clone());
            }
        }
    }

    Ok(ValidatedLockConflictSides {
        ours,
        theirs,
        floors,
        pinned_nodes,
        known_git_content,
        known_git_packages,
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

fn unsupported_format_version(
    parsed: &LockParseOutput,
    side: Option<LockConflictSide>,
) -> Option<LockConflictValidationError> {
    let version = parsed.file.format_version?;
    (version > SUPPORTED_LOCK_FORMAT_VERSION).then(|| {
        LockConflictValidationError::UnsupportedFormatVersion {
            side,
            version,
            span: parsed.file.format_version_span.clone().unwrap_or(0..0),
        }
    })
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
        LockConflictSide, LockConflictSides, LockConflictValidationError,
        validate_clean_merge_duplicates, validate_conflict_sides,
    };
    use crate::lock::{LockParseErrorKind, parse};
    use crate::mvs::{MvsNode, MvsPackageVersion};
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
    fn splits_clean_merge_duplicates_and_combines_their_baselines() {
        let source = clean_merge_lock(&[
            git_package_entry("1.2.0", 'a', 'a', "alloc"),
            git_package_entry("1.4.0", 'b', 'b', "wasi:filesystem"),
        ]);
        let parsed = parse(source.as_bytes());
        assert!(parsed.is_syntax_valid());
        let merged = validate_clean_merge_duplicates(&parsed, &BTreeSet::new())
            .unwrap()
            .expect("duplicate node is treated as a merge");

        assert_eq!(merged.ours().file().packages.len(), 1);
        assert_eq!(merged.theirs().file().packages.len(), 1);
        assert_eq!(
            merged.ours().file().packages[0].value.version,
            Version::parse("1.2.0").unwrap()
        );
        assert_eq!(
            merged.theirs().file().packages[0].value.version,
            Version::parse("1.4.0").unwrap()
        );
        let node = MvsNode::new(
            "acme:dep",
            Version::parse("1.0.0").unwrap().compatibility_line(),
        )
        .unwrap();
        assert_eq!(
            merged.floors().get(&node),
            Some(&Version::parse("1.4.0").unwrap())
        );
        assert_eq!(
            merged.previous_footprints().get(&node),
            Some(&BTreeSet::from([
                Box::<str>::from("alloc"),
                Box::<str>::from("wasi:filesystem"),
            ]))
        );
    }

    #[test]
    fn retains_non_duplicate_entries_on_both_clean_merge_sides() {
        let source = clean_merge_lock(&[
            git_package_entry("1.2.0", 'a', 'a', "alloc"),
            git_package_entry_for("acme:other", "2.1.0", 'c', 'c', "alloc"),
            git_package_entry("1.4.0", 'b', 'b', "alloc"),
        ]);
        let parsed = parse(source.as_bytes());
        let merged = validate_clean_merge_duplicates(&parsed, &BTreeSet::new())
            .unwrap()
            .expect("duplicate node is treated as a merge");

        for side in [merged.ours(), merged.theirs()] {
            assert_eq!(side.file().packages.len(), 2);
            assert!(
                side.file()
                    .packages
                    .iter()
                    .any(|package| package.value.package.as_ref() == "acme:other")
            );
        }
    }

    #[test]
    fn clean_merge_duplicate_entries_reconcile_known_content() {
        let source = clean_merge_lock(&[
            git_package_entry("1.4.0", 'a', 'a', "alloc"),
            git_package_entry("1.4.0", 'b', 'a', "alloc"),
        ]);
        let parsed = parse(source.as_bytes());
        match validate_clean_merge_duplicates(&parsed, &BTreeSet::new()).unwrap_err() {
            LockConflictValidationError::ConflictingKnownContent(conflict) => {
                assert_eq!(conflict.ours.commit, "a".repeat(40).into_boxed_str());
                assert_eq!(conflict.theirs.commit, "b".repeat(40).into_boxed_str());
            }
            other => panic!("expected known-content conflict, got {other:?}"),
        }
    }

    #[test]
    fn looks_up_known_content_by_exact_version_and_git_source() {
        let ours = git_package_entry("1.4.0", 'a', 'a', "alloc")
            .replace(
                "https://example.test/acme/dep",
                "https://example.test/dep-a",
            )
            .replace("    commit ", "    dir \".\"\n    commit ");
        let theirs = git_package_entry("1.4.0", 'b', 'b', "alloc").replace(
            "https://example.test/acme/dep",
            "https://example.test/dep-b",
        );
        let parsed = parse(clean_merge_lock(&[ours, theirs]).as_bytes());
        let merged = validate_clean_merge_duplicates(&parsed, &BTreeSet::new())
            .unwrap()
            .expect("distinct source records form two sides");
        let package_version =
            MvsPackageVersion::new("acme:dep", Version::parse("1.4.0").expect("valid version"))
                .expect("valid package version");

        let content_a = merged
            .known_git_content_for(
                &package_version,
                &crate::resolver_source::ResolverPackageSource::Git {
                    url: "https://example.test/dep-a".into(),
                    directory: None,
                },
            )
            .expect("repository-root spelling normalizes with an omitted directory");
        assert!(content_a.commit.starts_with('a'));

        let content_b = merged
            .known_git_content_for(
                &package_version,
                &crate::resolver_source::ResolverPackageSource::Git {
                    url: "https://example.test/dep-b".into(),
                    directory: None,
                },
            )
            .expect("the second source retains independent known content");
        assert!(content_b.commit.starts_with('b'));
        assert_eq!(merged.known_git_content().len(), 2);
        assert_eq!(merged.known_git_packages().len(), 2);
        let package_a = merged
            .known_git_packages()
            .get(&(
                package_version.clone(),
                super::LockConflictGitSource {
                    url: "https://example.test/dep-a".into(),
                    directory: None,
                },
            ))
            .expect("the first source retains its complete lock package");
        assert_eq!(package_a, &merged.ours().file().packages[0].value);
        assert_eq!(package_a.package.as_ref(), "acme:dep");
        assert_eq!(package_a.version, Version::parse("1.4.0").unwrap());
        let package_b = merged
            .known_git_packages()
            .get(&(
                package_version.clone(),
                super::LockConflictGitSource {
                    url: "https://example.test/dep-b".into(),
                    directory: None,
                },
            ))
            .expect("the second source retains its complete lock package");
        assert_eq!(package_b, &merged.theirs().file().packages[0].value);
        assert_eq!(package_b.package.as_ref(), "acme:dep");
        assert_eq!(package_b.version, Version::parse("1.4.0").unwrap());
        assert!(
            merged
                .known_git_content_for(
                    &package_version,
                    &crate::resolver_source::ResolverPackageSource::Git {
                        url: "https://example.test/dep-a".into(),
                        directory: Some("packages/dep".into()),
                    },
                )
                .is_none()
        );
    }

    #[test]
    fn rejects_more_than_two_clean_merge_entries_for_one_node() {
        let source = clean_merge_lock(&[
            git_package_entry("1.2.0", 'a', 'a', "alloc"),
            git_package_entry("1.3.0", 'b', 'b', "alloc"),
            git_package_entry("1.4.0", 'c', 'c', "alloc"),
        ]);
        let parsed = parse(source.as_bytes());
        assert!(matches!(
            validate_clean_merge_duplicates(&parsed, &BTreeSet::new()),
            Err(LockConflictValidationError::TooManyCleanMergeEntries { .. })
        ));
    }

    #[test]
    fn clean_merge_validation_does_not_forge_a_conflict_side() {
        let syntax_invalid = parse(b"lock 1\n\nroot acme:app @ 1.0.0\n    targets {\n");
        assert!(matches!(
            validate_clean_merge_duplicates(&syntax_invalid, &BTreeSet::new()),
            Err(LockConflictValidationError::InvalidCleanMergeSyntax { .. })
        ));

        let unsupported_and_syntax_invalid =
            parse(b"lock 3\n\nroot acme:app @ 1.0.0\n    targets {\n");
        assert!(!unsupported_and_syntax_invalid.is_syntax_valid());
        assert!(matches!(
            validate_clean_merge_duplicates(&unsupported_and_syntax_invalid, &BTreeSet::new()),
            Err(LockConflictValidationError::UnsupportedFormatVersion {
                side: None,
                version: 3,
                ..
            })
        ));

        let semantic_invalid = parse(b"lock 1\n\nroot acme:app @ 1.0.0\n");
        assert!(semantic_invalid.is_syntax_valid());
        assert!(matches!(
            validate_clean_merge_duplicates(&semantic_invalid, &BTreeSet::new()),
            Err(LockConflictValidationError::InvalidCleanMergeSemantics { .. })
        ));
    }

    #[test]
    fn rejects_unsupported_lock_format_before_clean_merge_duplicate_count() {
        let source = clean_merge_lock(&[
            git_package_entry("1.2.0", 'a', 'a', "alloc"),
            git_package_entry("1.3.0", 'b', 'b', "alloc"),
            git_package_entry("1.4.0", 'c', 'c', "alloc"),
        ])
        .replacen("lock 1", "lock 3", 1);
        let parsed = parse(source.as_bytes());

        assert!(matches!(
            validate_clean_merge_duplicates(&parsed, &BTreeSet::new()),
            Err(LockConflictValidationError::UnsupportedFormatVersion { version: 3, .. })
        ));
    }

    #[test]
    fn preserves_the_side_for_unsupported_conflict_lock_formats() {
        let source = b"lock 3\n\nroot acme:app @ 1.0.0\n    targets {\n<<<<<<< ours\n=======\n>>>>>>> theirs\n";
        let sides = reconstruct_conflict_sides(source)
            .unwrap()
            .expect("conflict markers present");

        assert!(matches!(
            validate_conflict_sides(&sides, &BTreeSet::new()),
            Err(LockConflictValidationError::UnsupportedFormatVersion {
                side: Some(LockConflictSide::Ours),
                version: 3,
                ..
            })
        ));

        let source = b"<<<<<<< ours\nlock 1\n\nroot acme:app @ 1.0.0\n    targets {\n=======\nlock 3\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n>>>>>>> theirs\n";
        let sides = reconstruct_conflict_sides(source)
            .unwrap()
            .expect("conflict markers present");

        assert!(matches!(
            validate_conflict_sides(&sides, &BTreeSet::new()),
            Err(LockConflictValidationError::UnsupportedFormatVersion {
                side: Some(LockConflictSide::Theirs),
                version: 3,
                ..
            })
        ));
    }

    #[test]
    fn different_compatibility_lines_are_not_clean_merge_duplicates() {
        let source = clean_merge_lock(&[
            git_package_entry("0.4.2", 'a', 'a', "alloc"),
            git_package_entry("1.0.0", 'b', 'b', "alloc"),
        ]);
        let parsed = parse(source.as_bytes());
        assert!(
            validate_clean_merge_duplicates(&parsed, &BTreeSet::new())
                .unwrap()
                .is_none()
        );
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
        assert_eq!(merged.known_git_packages().len(), 1);
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
        assert_eq!(merged.known_git_packages().len(), 1);
        let package_version =
            MvsPackageVersion::new("acme:dep", Version::parse("1.4.0").unwrap()).unwrap();
        let package = merged
            .known_git_packages()
            .get(&(
                package_version,
                super::LockConflictGitSource {
                    url: "https://example.test/dep".into(),
                    directory: None,
                },
            ))
            .expect("identical records share one exact source key");
        assert_eq!(package.package.as_ref(), "acme:dep");
        assert_eq!(package.version, Version::parse("1.4.0").unwrap());
    }

    #[test]
    fn same_source_record_selection_is_independent_of_conflict_side_order() {
        let with_requirement = |package: &str| {
            git_package_entry("1.4.0", 'a', 'a', "alloc").replace(
                "    compiler ^0.1\n",
                &format!("    requires {package} @ ^1.0 -> 1.1.0\n    compiler ^0.1\n"),
            )
        };
        let package_a = with_requirement("acme:a");
        let package_b = with_requirement("acme:b");

        let selected_record = |first: &str, second: &str| {
            let parsed =
                parse(clean_merge_lock(&[first.to_string(), second.to_string()]).as_bytes());
            let merged = validate_clean_merge_duplicates(&parsed, &BTreeSet::new())
                .unwrap()
                .expect("same-node entries form a clean merge");
            let package_version =
                MvsPackageVersion::new("acme:dep", Version::parse("1.4.0").unwrap()).unwrap();
            let package = merged
                .known_git_packages()
                .get(&(
                    package_version,
                    super::LockConflictGitSource {
                        url: "https://example.test/acme/dep".into(),
                        directory: None,
                    },
                ))
                .expect("the common Git source has a lock record");
            crate::lock_serialization::serialize_package_for_ordering(package)
        };

        let forward = selected_record(&package_a, &package_b);
        let reversed = selected_record(&package_b, &package_a);
        assert_eq!(forward, reversed);
        assert!(forward.contains("requires acme:a @ ^1.0 -> 1.1.0"));
        assert!(!forward.contains("requires acme:b @ ^1.0 -> 1.1.0"));
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

    fn clean_merge_lock(entries: &[String]) -> String {
        let mut source =
            "lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n".to_string();
        for entry in entries {
            source.push('\n');
            source.push_str(entry);
        }
        source
    }

    fn git_package_entry(
        version: &str,
        commit: char,
        checksum: char,
        capabilities: &str,
    ) -> String {
        git_package_entry_for("acme:dep", version, commit, checksum, capabilities)
    }

    fn git_package_entry_for(
        package: &str,
        version: &str,
        commit: char,
        checksum: char,
        capabilities: &str,
    ) -> String {
        let source_path = package.replace(':', "/");
        format!(
            "package {package} @ {version}\n    source git \"https://example.test/{source_path}\"\n    commit \"{}\"\n    checksum \"sai1:{}\"\n    compiler ^0.1\n    capabilities {{ {capabilities} }}\n",
            commit.to_string().repeat(40),
            checksum.to_string().repeat(64),
        )
    }
}
