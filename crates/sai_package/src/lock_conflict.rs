//! Reconstruction of the two sides of Git conflict markers in `sai.lock`.

use std::ops::Range;

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
    use crate::lock::{LockParseErrorKind, parse};

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
}
