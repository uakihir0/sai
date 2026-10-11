// The lossless input layer is introduced before its document builder so
// intermediate commits can verify byte and trivia handling independently.
#![allow(dead_code)]

use std::ops::Range;

use sai_package::lexer::{
    PackageLexOutput, PackageToken, PackageTokenKind, PackageTrivia, PackageTriviaKind, lex_tokens,
};
use sai_package::manifest::{
    MAX_PACKAGE_MANIFEST_BYTES, PackageParseErrorKind, PackageParseOutput, parse,
};

/// Keeps the package parser's semantic projection beside the original input
/// and its lossless lexical stream.
#[derive(Clone, Debug)]
pub(crate) struct PackageInput<'a> {
    source: &'a [u8],
    lexed: PackageLexOutput,
    parsed: PackageParseOutput,
    comments: Vec<AttachedComment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageRefusal {
    Lexical,
    Grammar,
    ParserLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommentAttachment {
    Leading(usize),
    Trailing(usize),
    EndOfFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttachedComment {
    trivia_index: usize,
    attachment: CommentAttachment,
    physical_line: usize,
    /// Number of physical line breaks since the immediately preceding comment
    /// or significant token, whichever is closer.
    line_breaks_before: usize,
}

impl<'a> PackageInput<'a> {
    pub(crate) fn new(source: &'a [u8]) -> Self {
        let parsed = parse(source);
        let lexed = if source.len() <= MAX_PACKAGE_MANIFEST_BYTES {
            lex_tokens(source)
        } else {
            PackageLexOutput::default()
        };
        let comments = attach_comments(source, &lexed.tokens, &parsed.file().trivia);
        Self {
            source,
            lexed,
            parsed,
            comments,
        }
    }

    pub(crate) fn source(&self) -> &[u8] {
        self.source
    }

    pub(crate) fn parsed(&self) -> &PackageParseOutput {
        &self.parsed
    }

    pub(crate) fn lexed(&self) -> &PackageLexOutput {
        &self.lexed
    }

    pub(crate) fn comments(&self) -> &[AttachedComment] {
        &self.comments
    }

    pub(crate) fn comment_kind(&self, comment: &AttachedComment) -> PackageTriviaKind {
        self.parsed.file().trivia[comment.trivia_index].kind
    }

    pub(crate) fn comment_text<'b>(&'b self, comment: &AttachedComment) -> &'b str {
        let span = &self.parsed.file().trivia[comment.trivia_index].span;
        std::str::from_utf8(&self.source[span.clone()])
            .expect("package comment spans point to valid UTF-8")
    }

    /// Refusal is deliberately narrower than `PackageParseOutput::is_syntax_valid`:
    /// duplicate and missing declarations are structural diagnostics, while
    /// malformed values are retained in the original token stream for later
    /// formatting and verification.
    pub(crate) fn refusals(&self) -> Vec<PackageRefusal> {
        let mut refusals = self
            .lexed
            .errors
            .iter()
            .map(|_| PackageRefusal::Lexical)
            .collect::<Vec<_>>();
        refusals.extend(
            self.parsed
                .errors()
                .iter()
                .filter_map(|error| match &error.kind {
                    PackageParseErrorKind::UnknownDeclaration { .. }
                    | PackageParseErrorKind::UnknownArtifactKind(_)
                    | PackageParseErrorKind::ExpectedToken(_)
                    | PackageParseErrorKind::ExpectedListSeparator(_)
                    | PackageParseErrorKind::ExpectedDeclaration
                    | PackageParseErrorKind::ExpectedProfileItem => Some(PackageRefusal::Grammar),
                    PackageParseErrorKind::ManifestTooLarge => Some(PackageRefusal::ParserLimit),
                    PackageParseErrorKind::MissingPackageDeclaration
                    | PackageParseErrorKind::PackageDeclarationNotFirst
                    | PackageParseErrorKind::DuplicateDeclaration(_)
                    | PackageParseErrorKind::InvalidPackageIdentity(_)
                    | PackageParseErrorKind::InvalidPackageVersion(_)
                    | PackageParseErrorKind::InvalidContentPath(_)
                    | PackageParseErrorKind::InvalidDependencyPath(_)
                    | PackageParseErrorKind::InvalidRepositoryDirectory(_)
                    | PackageParseErrorKind::ConflictingLintLevel
                    | PackageParseErrorKind::InvalidVersionRequirement(_) => None,
                }),
        );
        refusals
    }
}

impl AttachedComment {
    pub(crate) const fn attachment(&self) -> CommentAttachment {
        self.attachment
    }

    pub(crate) const fn physical_line(&self) -> usize {
        self.physical_line
    }

    pub(crate) const fn line_breaks_before(&self) -> usize {
        self.line_breaks_before
    }
}

fn attach_comments(
    source: &[u8],
    tokens: &[PackageToken],
    trivia: &[PackageTrivia],
) -> Vec<AttachedComment> {
    let significant_tokens = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| {
            !matches!(
                token.kind,
                PackageTokenKind::Newline | PackageTokenKind::Eof
            )
        })
        .collect::<Vec<_>>();

    let mut previous_comment_end_line = None;
    let mut token_cursor_after_previous_comment = None;
    let mut token_cursor = 0;
    let mut source_cursor = 0;
    let mut current_line = 0;
    let mut previous_token = None;
    let mut comments = Vec::new();
    for (trivia_index, trivia) in trivia
        .iter()
        .enumerate()
        .filter(|(_, trivia)| is_comment(trivia.kind))
    {
        while let Some((_, token)) = significant_tokens.get(token_cursor)
            && token.span.end <= trivia.span.start
        {
            advance_line_counter(
                source,
                &mut source_cursor,
                &mut current_line,
                token.span.end,
            );
            previous_token = Some((significant_tokens[token_cursor].0, token, current_line));
            token_cursor += 1;
        }
        advance_line_counter(
            source,
            &mut source_cursor,
            &mut current_line,
            trivia.span.start,
        );
        let comment_line = current_line;
        let next_token = significant_tokens
            .get(token_cursor)
            .filter(|(_, token)| token.span.start >= trivia.span.end);
        let attachment = match (previous_token, next_token) {
            (Some((index, _, token_line)), _) if token_line == comment_line => {
                CommentAttachment::Trailing(index)
            }
            (_, Some((index, _))) => CommentAttachment::Leading(*index),
            _ => CommentAttachment::EndOfFile,
        };
        let token_after_previous_comment = token_cursor_after_previous_comment
            .is_some_and(|previous_cursor| token_cursor > previous_cursor);
        let anchor_line = if token_after_previous_comment {
            previous_token.map(|(_, _, token_line)| token_line)
        } else {
            previous_comment_end_line
                .or_else(|| previous_token.map(|(_, _, token_line)| token_line))
                .or(Some(0))
        };
        let line_breaks_before = comment_line.saturating_sub(anchor_line.unwrap_or(0));
        comments.push(AttachedComment {
            trivia_index,
            attachment,
            physical_line: comment_line,
            line_breaks_before,
        });
        advance_line_counter(
            source,
            &mut source_cursor,
            &mut current_line,
            trivia.span.end,
        );
        previous_comment_end_line = Some(current_line);
        token_cursor_after_previous_comment = Some(token_cursor);
    }
    comments
}

fn is_comment(kind: PackageTriviaKind) -> bool {
    matches!(
        kind,
        PackageTriviaKind::LineComment | PackageTriviaKind::BlockComment
    )
}

fn advance_line_counter(source: &[u8], cursor: &mut usize, line: &mut usize, end: usize) {
    let end = end.min(source.len()).max(*cursor);
    for byte in &source[*cursor..end] {
        if *byte == b'\n' {
            *line += 1;
        }
    }
    *cursor = end;
}

fn trivia_span(trivia: &[PackageTrivia], comment: &AttachedComment) -> Range<usize> {
    trivia[comment.trivia_index].span.clone()
}

#[cfg(test)]
mod tests {
    use super::{CommentAttachment, PackageInput, PackageRefusal, trivia_span};
    use sai_package::lexer::PackageTokenKind;
    use sai_package::manifest::PackageParseErrorKind;

    #[test]
    fn keeps_original_bytes_and_offsets_when_input_has_a_bom() {
        let source = b"\xef\xbb\xbfpackage acme:app @ 1.0.0\r\n";
        let input = PackageInput::new(source);

        assert_eq!(input.source(), source);
        assert_eq!(
            input
                .parsed()
                .file()
                .header
                .as_ref()
                .map(|header| header.span.start),
            Some(3)
        );
        assert!(input.lexed().errors.is_empty());
    }

    #[test]
    fn attaches_comments_to_the_nearest_significant_token() {
        let source =
            b"// contract heading\n\npackage acme:app @ 1.0.0 // stable release\nsource \"src\"\n";
        let input = PackageInput::new(source);
        let comments = input.comments();

        assert_eq!(comments.len(), 2);
        assert!(input.lexed().trivia.is_empty());
        assert!(!input.parsed().file().trivia.is_empty());
        assert!(matches!(
            comments[0].attachment,
            CommentAttachment::Leading(_)
        ));
        assert_eq!(comments[0].physical_line, 0);
        assert_eq!(comments[0].line_breaks_before, 0);
        assert_eq!(
            trivia_span(&input.parsed().file().trivia, &comments[0]),
            0..19
        );
        assert!(matches!(
            comments[1].attachment,
            CommentAttachment::Trailing(_)
        ));
        assert_eq!(comments[1].physical_line, 2);
        assert_eq!(comments[1].line_breaks_before, 0);
        assert_eq!(
            &input.source()[trivia_span(&input.parsed().file().trivia, &comments[1])],
            b"// stable release"
        );
    }

    #[test]
    fn counts_blank_lines_between_adjacent_comments() {
        let input = PackageInput::new(b"// first\n\n// second\npackage acme:app @ 1.0.0\n");
        assert_eq!(input.comments().len(), 2);
        assert_eq!(input.comments()[1].line_breaks_before, 2);
    }

    #[test]
    fn counts_comment_gaps_after_tokens_and_multiline_comments() {
        let source = b"package acme:app @ 1.0.0\n// after header\n\n// after blank line\n";
        let input = PackageInput::new(source);
        assert_eq!(input.comments().len(), 2);
        assert_eq!(input.comments()[0].line_breaks_before, 1);
        assert_eq!(input.comments()[1].line_breaks_before, 2);

        let source =
            b"package acme:app @ 1.0.0\n/* first */\nsource \"src\"\n\n// after declaration\n";
        let input = PackageInput::new(source);
        assert_eq!(input.comments().len(), 2);
        assert_eq!(input.comments()[1].line_breaks_before, 2);

        let source = b"package acme:app @ 1.0.0\r\n/* first\r\n second */\r\n// following\r\n";
        let input = PackageInput::new(source);
        assert_eq!(input.comments().len(), 2);
        assert_eq!(input.comments()[0].physical_line, 1);
        assert_eq!(input.comments()[1].line_breaks_before, 1);
    }

    #[test]
    fn distinguishes_grammar_refusals_from_structural_and_value_diagnostics() {
        let structural = PackageInput::new(
            b"targets { wasm32-core }\npackage acme:app @ 1.0.0\npackage acme:app @ 1.0.1\n",
        );
        assert!(structural.refusals().is_empty());
        assert_eq!(structural.parsed().errors().len(), 3);
        assert_eq!(
            structural
                .lexed()
                .tokens
                .iter()
                .filter(|token| &structural.source()[token.span.clone()] == b"package")
                .count(),
            2
        );

        let grammar = PackageInput::new(b"package acme:app @ 1.0.0\nunknown {}\n");
        assert!(grammar.refusals().contains(&PackageRefusal::Grammar));

        let malformed_value =
            PackageInput::new(b"package acme:app @ 1.0.0\nsource \"../outside\"\n");
        assert!(
            malformed_value
                .parsed()
                .errors()
                .iter()
                .any(|error| matches!(
                    &error.kind,
                    sai_package::manifest::PackageParseErrorKind::InvalidContentPath(_)
                ))
        );
        assert!(malformed_value.refusals().is_empty());
    }

    #[test]
    fn refuses_each_package_grammar_error_category() {
        #[derive(Clone, Copy)]
        enum ExpectedError {
            UnknownDeclaration,
            UnknownArtifactKind,
            ExpectedToken,
            ExpectedListSeparator,
            ExpectedDeclaration,
            ExpectedProfileItem,
        }

        for (source, is_expected_kind) in [
            (
                b"package acme:app @ 1.0.0\nunknown {}\n".as_slice(),
                ExpectedError::UnknownDeclaration,
            ),
            (
                b"package acme:app @ 1.0.0\nartifact custom {}\n".as_slice(),
                ExpectedError::UnknownArtifactKind,
            ),
            (
                b"package acme:app @ 1.0.0\ndescription\n".as_slice(),
                ExpectedError::ExpectedToken,
            ),
            (
                b"package acme:app @ 1.0.0\ntargets { wasm32-core wasm32-wasi }\n".as_slice(),
                ExpectedError::ExpectedListSeparator,
            ),
            (
                b"package acme:app @ 1.0.0\n@\n".as_slice(),
                ExpectedError::ExpectedDeclaration,
            ),
            (
                b"package acme:app @ 1.0.0\nprofile tiny { nope }\n".as_slice(),
                ExpectedError::ExpectedProfileItem,
            ),
        ] {
            let input = PackageInput::new(source);
            assert!(
                input.refusals().contains(&PackageRefusal::Grammar),
                "{source:?} produced {:?}",
                input.parsed().errors()
            );
            assert!(
                input
                    .parsed()
                    .errors()
                    .iter()
                    .any(|error| match is_expected_kind {
                        ExpectedError::UnknownDeclaration => {
                            matches!(
                                &error.kind,
                                PackageParseErrorKind::UnknownDeclaration { .. }
                            )
                        }
                        ExpectedError::UnknownArtifactKind => {
                            matches!(&error.kind, PackageParseErrorKind::UnknownArtifactKind(_))
                        }
                        ExpectedError::ExpectedToken => {
                            matches!(&error.kind, PackageParseErrorKind::ExpectedToken(_))
                        }
                        ExpectedError::ExpectedListSeparator => {
                            matches!(&error.kind, PackageParseErrorKind::ExpectedListSeparator(_))
                        }
                        ExpectedError::ExpectedDeclaration => {
                            matches!(&error.kind, PackageParseErrorKind::ExpectedDeclaration)
                        }
                        ExpectedError::ExpectedProfileItem => {
                            matches!(&error.kind, PackageParseErrorKind::ExpectedProfileItem)
                        }
                    })
            );
        }

        for source in [
            b"package acme:app @ 1.0.0\n$\n".as_slice(),
            b"package acme:app @ 1.0.0\n\xff".as_slice(),
        ] {
            let input = PackageInput::new(source);
            assert!(input.refusals().contains(&PackageRefusal::Lexical));
            assert!(!input.lexed().errors.is_empty());
        }
    }

    #[test]
    fn keeps_version_requirement_validation_errors_non_refusing() {
        for source in [
            b"package acme:app @ 1.0.0\ncompiler >1.2\n".as_slice(),
            b"package acme:app @ 1.0.0\ncompiler 1.2\n".as_slice(),
        ] {
            let input = PackageInput::new(source);
            assert!(input.parsed().errors().iter().any(|error| matches!(
                &error.kind,
                PackageParseErrorKind::InvalidVersionRequirement(_)
            )));
            assert!(input.refusals().is_empty());
        }

        let semantic = PackageInput::new(b"package acme:app @ 1.0.0\ncompiler <1.2\n");
        assert!(semantic.parsed().errors().iter().any(|error| matches!(
            &error.kind,
            PackageParseErrorKind::InvalidVersionRequirement(_)
        )));
        assert!(semantic.refusals().is_empty());
    }

    #[test]
    fn distinguishes_parser_version_validation_from_lexical_version_errors() {
        let source = b"package acme:app @ 1.0\n";
        let invalid_value = PackageInput::new(source);
        assert!(invalid_value.lexed().errors.is_empty());
        assert!(invalid_value.parsed().errors().iter().any(|error| {
            matches!(&error.kind, PackageParseErrorKind::InvalidPackageVersion(_))
        }));
        assert!(invalid_value.refusals().is_empty());
        assert!(
            invalid_value
                .lexed()
                .tokens
                .iter()
                .any(|token| { &source[token.span.clone()] == b"1.0" })
        );

        let source = b"package acme:app @ 1.2.3+meta\n";
        let lexical_error = PackageInput::new(source);
        assert!(!lexical_error.lexed().errors.is_empty());
        assert!(lexical_error.refusals().contains(&PackageRefusal::Lexical));
        assert!(
            lexical_error
                .lexed()
                .tokens
                .iter()
                .any(|token| { &source[token.span.clone()] == b"1.2.3+meta" })
        );
    }

    #[test]
    fn comments_can_attach_to_eof_without_inventing_a_token() {
        let input = PackageInput::new(b"// only a comment\n");
        assert_eq!(input.comments().len(), 1);
        assert_eq!(input.comments()[0].attachment, CommentAttachment::EndOfFile);
        assert_eq!(input.parsed().file().header, None);
    }

    #[test]
    fn attaches_comments_around_continued_requirement_clauses() {
        let source = b"package acme:app @ 1.0.0\nrequires {\n    acme:dep @ ^1 // chosen version\n    // local source follows\n    from path \"dep\"\n}\n";
        let input = PackageInput::new(source);
        assert!(input.parsed().errors().is_empty());
        let comments = input.comments();
        assert_eq!(comments.len(), 2);

        let version_index = match comments[0].attachment {
            CommentAttachment::Trailing(index) => index,
            attachment => panic!("expected trailing version comment, found {attachment:?}"),
        };
        assert_eq!(
            &source[input.lexed().tokens[version_index].span.clone()],
            b"1"
        );
        let from_index = match comments[1].attachment {
            CommentAttachment::Leading(index) => index,
            attachment => panic!("expected leading `from` comment, found {attachment:?}"),
        };
        assert_eq!(
            &source[input.lexed().tokens[from_index].span.clone()],
            b"from"
        );
    }

    #[test]
    fn identifies_trailing_comma_comments_for_builder_rehoming() {
        let source =
            b"package acme:app @ 1.0.0\ntargets { wasm32-core, // keep with this entry\n    wasm32-wasi }\n";
        let input = PackageInput::new(source);
        assert!(input.parsed().errors().is_empty());
        assert_eq!(input.comments().len(), 1);

        let comma_index = match input.comments()[0].attachment {
            CommentAttachment::Trailing(index) => index,
            attachment => panic!("expected trailing comma comment, found {attachment:?}"),
        };
        let comma = &input.lexed().tokens[comma_index];
        assert_eq!(
            comma.kind,
            PackageTokenKind::Punctuator(sai_package::lexer::PackagePunctuator::Comma)
        );
        assert_eq!(&source[comma.span.clone()], b",");
    }

    #[test]
    fn preserves_nested_multiline_comment_offsets_across_crlf_and_bom() {
        let source =
            b"\xef\xbb\xbfpackage acme:app @ 1.0.0\r\n/* outer\r\n /* nested */\r\n*/\r\ntargets {}\r\n";
        let input = PackageInput::new(source);
        assert!(input.lexed().errors.is_empty());
        assert_eq!(input.comments().len(), 1);
        assert_eq!(input.comments()[0].physical_line, 1);
        assert_eq!(
            &source[trivia_span(&input.parsed().file().trivia, &input.comments()[0])],
            b"/* outer\r\n /* nested */\r\n*/"
        );
    }

    #[test]
    fn attachment_work_scales_with_comment_and_token_count() {
        let mut source = Vec::new();
        for _ in 0..1_000 {
            source.extend_from_slice(b"// header note\n");
        }
        source.extend_from_slice(b"package acme:app @ 1.0.0\n");

        let input = PackageInput::new(&source);
        assert_eq!(input.comments().len(), 1_000);
        assert!(
            input
                .comments()
                .iter()
                .all(|comment| matches!(comment.attachment, CommentAttachment::Leading(_)))
        );
    }

    #[test]
    fn retains_token_spelling_for_non_refusing_structural_and_value_errors() {
        for source in [
            b"targets { wasm32-core }\n".as_slice(),
            b"package wasi:tool @ 1.0.0\n".as_slice(),
        ] {
            let input = PackageInput::new(source);
            assert!(input.refusals().is_empty());
            assert!(!input.lexed().tokens.is_empty());
            assert_eq!(input.source(), source);
        }
    }

    #[test]
    fn applies_the_package_size_limit_before_lexing() {
        let source = vec![b' '; sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES + 1];
        let input = PackageInput::new(&source);

        assert_eq!(input.refusals(), [PackageRefusal::ParserLimit]);
        assert!(input.lexed().tokens.is_empty());
        assert!(input.comments().is_empty());
    }
}
