#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use sai_package::lexer::{PackageLexError, PackagePunctuator, PackageToken, PackageTokenKind};
use sai_package::manifest::PackageParseError;

mod package_document;
mod package_input;
mod package_surface;
mod printer;

use package_input::PackageInput;
use package_surface::PackageSyntaxIndex;
pub use printer::{Doc, GroupId, PrintError, print, print_with_forced_groups};

/// Source errors that caused package formatting to be refused.
///
/// Spans are byte ranges into the input passed to [`format_package`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    /// Lexical errors reported by the package lexer.
    pub lexical_errors: Vec<PackageLexError>,
    /// Grammar errors and parser-limit errors reported by the package parser.
    pub syntax_errors: Vec<PackageParseError>,
}

/// The package-format verification check that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageVerificationCheck {
    /// The formatter output could not be indexed or parsed.
    OutputSyntax,
    /// Significant package tokens changed.
    TokenPreservation,
    /// Parser errors or warnings changed.
    DiagnosticPreservation,
    /// Comment text or order changed.
    CommentPreservation,
    /// Formatting the canonical output changed it again.
    Idempotency,
}

/// Details needed to report a package-format verification failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageVerificationFailure {
    /// The check that failed.
    pub check: PackageVerificationCheck,
    /// First differing byte in the original input, when one exists.
    pub input_byte_offset: Option<usize>,
}

/// Errors returned while formatting a `sai.package` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageFormatError {
    /// The package lexer or parser rejected the input.
    Refused(Refusal),
    /// A valid package declaration could not be indexed or rendered.
    InternalError,
    /// The document printer rejected an invalid document.
    Print(PrintError),
    /// The formatted result failed one of its preservation checks.
    VerificationFailed(PackageVerificationFailure),
}

impl fmt::Display for PackageFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(
                formatter,
                "package input contains {} lexical and {} syntax errors",
                refusal.lexical_errors.len(),
                refusal.syntax_errors.len()
            ),
            Self::InternalError => formatter.write_str("package formatter could not render input"),
            Self::Print(error) => write!(formatter, "package formatter printer failed: {error:?}"),
            Self::VerificationFailed(failure) => write!(
                formatter,
                "package formatter verification failed: {:?}",
                failure.check
            ),
        }
    }
}

impl Error for PackageFormatError {}

/// Format one `sai.package` file into its canonical layout.
///
/// The result is parsed and checked for token, diagnostic, and comment
/// preservation. It is formatted a second time to verify idempotency.
pub fn format_package(source: &[u8]) -> Result<String, PackageFormatError> {
    let formatted = format_package_once(source)?;
    let repeated = format_package_once(formatted.as_bytes())?;
    if formatted != repeated {
        return Err(verification_failure(
            PackageVerificationCheck::Idempotency,
            None,
        ));
    }
    Ok(formatted)
}

fn format_package_once(source: &[u8]) -> Result<String, PackageFormatError> {
    let input = PackageInput::new(source);
    if !input.refusals().is_empty() {
        return Err(PackageFormatError::Refused(Refusal {
            lexical_errors: input.lexed().errors.clone(),
            syntax_errors: input.refusing_parse_errors(),
        }));
    }
    let index = PackageSyntaxIndex::new(&input).map_err(|_| PackageFormatError::InternalError)?;
    let document = package_document::package_document(&input, &index)
        .map_err(|_| PackageFormatError::InternalError)?;
    let formatted = print(&document).map_err(PackageFormatError::Print)?;

    let formatted_input = PackageInput::new(formatted.as_bytes());
    if !formatted_input.refusals().is_empty() {
        let output_offset = first_refusal_offset(&formatted_input).unwrap_or(formatted.len());
        let output_index = PackageSyntaxIndex::new(&formatted_input).ok();
        return Err(verification_failure(
            PackageVerificationCheck::OutputSyntax,
            Some(package_output_offset_to_input(
                &input,
                &index,
                &formatted_input,
                output_index.as_ref(),
                output_offset,
            )),
        ));
    }
    let formatted_index = PackageSyntaxIndex::new(&formatted_input).map_err(|_| {
        verification_failure(
            PackageVerificationCheck::OutputSyntax,
            Some(input.source().len()),
        )
    })?;
    package_token_forms_match(&input, &index, &formatted_input, &formatted_index).map_err(
        |mismatch| {
            verification_failure(
                PackageVerificationCheck::TokenPreservation,
                mismatch.input_byte_offset,
            )
        },
    )?;
    package_diagnostics_match(&input, &formatted_input).map_err(|mismatch| {
        verification_failure(
            PackageVerificationCheck::DiagnosticPreservation,
            mismatch.input_byte_offset,
        )
    })?;
    if !package_comments_match(&input, &formatted_input) {
        return Err(verification_failure(
            PackageVerificationCheck::CommentPreservation,
            None,
        ));
    }
    Ok(formatted)
}

fn verification_failure(
    check: PackageVerificationCheck,
    input_byte_offset: Option<usize>,
) -> PackageFormatError {
    PackageFormatError::VerificationFailed(PackageVerificationFailure {
        check,
        input_byte_offset,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PackageMismatch {
    input_byte_offset: Option<usize>,
}

fn package_token_forms_match(
    left: &PackageInput<'_>,
    left_index: &PackageSyntaxIndex<'_>,
    right: &PackageInput<'_>,
    right_index: &PackageSyntaxIndex<'_>,
) -> Result<(), PackageMismatch> {
    let left_list_commas = package_list_commas(left, left_index);
    let right_list_commas = package_list_commas(right, right_index);
    let left_source_len = left.source().len();
    let mut left_tokens = left.lexed().tokens.iter().enumerate();
    let mut right_tokens = right.lexed().tokens.iter().enumerate();
    loop {
        let left = next_package_token(left, &mut left_tokens, &left_list_commas);
        let right = next_package_token(right, &mut right_tokens, &right_list_commas);
        match (left, right) {
            (Some(left), Some(right)) if left.kind == right.kind && left.text == right.text => {}
            (None, None) => return Ok(()),
            (left, _) => {
                return Err(PackageMismatch {
                    input_byte_offset: Some(
                        left.map_or(left_source_len, |token| token.byte_offset),
                    ),
                });
            }
        }
    }
}

fn package_list_commas(input: &PackageInput<'_>, index: &PackageSyntaxIndex<'_>) -> HashSet<usize> {
    let mut commas = HashSet::new();
    for list in index.brace_lists() {
        for &separator in index.separators(list).into_iter().flatten() {
            let token_index = separator as usize;
            if input.lexed().tokens.get(token_index).is_some_and(|token| {
                token.kind == PackageTokenKind::Punctuator(PackagePunctuator::Comma)
            }) {
                commas.insert(token_index);
            }
        }
    }
    commas
}

fn package_significant_token_spans(
    input: &PackageInput<'_>,
    index: Option<&PackageSyntaxIndex<'_>>,
) -> Vec<std::ops::Range<usize>> {
    let list_commas = index.map(|index| package_list_commas(input, index));
    input
        .lexed()
        .tokens
        .iter()
        .enumerate()
        .filter(|(token_index, token)| {
            !matches!(
                token.kind,
                PackageTokenKind::Newline | PackageTokenKind::Eof
            ) && !list_commas
                .as_ref()
                .is_some_and(|commas| commas.contains(token_index))
        })
        .map(|(_, token)| token.span.clone())
        .collect()
}

fn package_output_offset_to_input(
    input: &PackageInput<'_>,
    input_index: &PackageSyntaxIndex<'_>,
    output: &PackageInput<'_>,
    output_index: Option<&PackageSyntaxIndex<'_>>,
    output_offset: usize,
) -> usize {
    let output_spans = package_significant_token_spans(output, output_index);
    if output_spans.is_empty() {
        return input.source().len();
    }
    let token_ordinal = output_spans
        .iter()
        .position(|span| span.end > output_offset)
        .unwrap_or(output_spans.len());
    package_significant_token_spans(input, Some(input_index))
        .get(token_ordinal)
        .map_or(input.source().len(), |span| span.start)
}

fn first_refusal_offset(input: &PackageInput<'_>) -> Option<usize> {
    input
        .lexed()
        .errors
        .iter()
        .map(|error| error.span.start)
        .chain(
            input
                .refusing_parse_errors()
                .iter()
                .map(|error| error.span.start),
        )
        .min()
}

fn next_package_token<'a>(
    input: &'a PackageInput<'_>,
    tokens: &mut impl Iterator<Item = (usize, &'a PackageToken)>,
    list_commas: &HashSet<usize>,
) -> Option<PackageTokenForm<'a>> {
    for (index, token) in tokens {
        if matches!(
            token.kind,
            PackageTokenKind::Newline | PackageTokenKind::Eof
        ) || list_commas.contains(&index)
        {
            continue;
        }
        let text = std::str::from_utf8(&input.source()[token.span.clone()])
            .expect("package token spans point to valid UTF-8");
        return Some(PackageTokenForm {
            kind: token.kind,
            text,
            byte_offset: token.span.start,
        });
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PackageTokenForm<'a> {
    kind: PackageTokenKind,
    text: &'a str,
    byte_offset: usize,
}

fn package_diagnostics_match(
    left: &PackageInput<'_>,
    right: &PackageInput<'_>,
) -> Result<(), PackageMismatch> {
    let left_errors = left.parsed().errors();
    let right_errors = right.parsed().errors();
    for (left_error, right_error) in left_errors.iter().zip(right_errors) {
        if left_error.kind != right_error.kind {
            return Err(PackageMismatch {
                input_byte_offset: Some(left_error.span.start),
            });
        }
    }
    if left_errors.len() != right_errors.len() {
        return Err(PackageMismatch {
            input_byte_offset: left_errors
                .get(right_errors.len())
                .map_or(left.source().len(), |error| error.span.start)
                .into(),
        });
    }

    let left_warnings = left.parsed().warnings();
    let right_warnings = right.parsed().warnings();
    for (left_warning, right_warning) in left_warnings.iter().zip(right_warnings) {
        if left_warning.kind != right_warning.kind {
            return Err(PackageMismatch {
                input_byte_offset: Some(left_warning.span.start),
            });
        }
    }
    if left_warnings.len() != right_warnings.len() {
        return Err(PackageMismatch {
            input_byte_offset: left_warnings
                .get(right_warnings.len())
                .map_or(left.source().len(), |warning| warning.span.start)
                .into(),
        });
    }
    Ok(())
}

fn package_comments_match(left: &PackageInput<'_>, right: &PackageInput<'_>) -> bool {
    let mut left_comments = left.comments().iter().map(|comment| {
        left.comment_text(comment)
            .replace("\r\n", "\n")
            .split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    });
    let mut right_comments = right.comments().iter().map(|comment| {
        right
            .comment_text(comment)
            .replace("\r\n", "\n")
            .split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    });
    loop {
        match (left_comments.next(), right_comments.next()) {
            (Some(left), Some(right)) if left == right => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PackageFormatError, PackageInput, PackageSyntaxIndex, PackageVerificationCheck,
        PackageVerificationFailure, first_refusal_offset, format_package,
        package_diagnostics_match, package_output_offset_to_input, package_token_forms_match,
        verification_failure,
    };
    use sai_package::manifest::{MAX_PACKAGE_MANIFEST_BYTES, PackageParseErrorKind, parse};

    #[test]
    fn formats_a_complete_package_and_is_idempotent() {
        let source = b"package acme:photo-service@1.3.0\n\
compiler ^1.2, <2\n\
source \"src\"\n\
wit \"wit\"\n\
artifact component { wasm32-wasi \"dist/photo.wasm\" }\n\
targets { wasm32-core, wasm32-wasi }\n\
export world photo-service\n\
requires {\n\
    sai:http @ ^1.4 from git \"https://example.invalid/http\" only { wasm32-wasi } grant { wasi:http }\n\
}\n\
allow { alloc, wasi:http }\n\
deny { wasi:filesystem }\n\
policy { deny unsafe, deny ambient_imports }\n\
lints { warn PUBLIC_EFFECTS_INFERRED, allow NAMING_CONVENTION }\n\
profile tiny {\n\
    allow { alloc }\n\
    budget { wasm<=64 KiB }\n\
}\n\
allocator acme:allocator\n\
overrides { acme:image use artifact }\n\
description \"photo package\"\n\
license \"MIT\"\n\
repository \"https://example.invalid/photo\"\n\
authors { \"A. Author\", \"B. Author\" }\n\
// preserved at EOF\n";
        let formatted = format_package(source).expect("complete package should format");
        let expected = concat!(
            "package acme:photo-service @ 1.3.0\n",
            "compiler ^1.2, <2\n",
            "source \"src\"\n",
            "wit \"wit\"\n",
            "artifact component {\n",
            "    wasm32-wasi \"dist/photo.wasm\"\n",
            "}\n",
            "targets { wasm32-core, wasm32-wasi }\n",
            "export world photo-service\n",
            "requires {\n",
            "    sai:http @ ^1.4\n",
            "        from git \"https://example.invalid/http\"\n",
            "        only { wasm32-wasi }\n",
            "        grant { wasi:http }\n",
            "}\n",
            "allow { alloc, wasi:http }\n",
            "deny { wasi:filesystem }\n",
            "policy { deny unsafe, deny ambient_imports }\n",
            "lints { warn PUBLIC_EFFECTS_INFERRED, allow NAMING_CONVENTION }\n",
            "profile tiny {\n",
            "    allow { alloc }\n",
            "    budget { wasm <= 64 KiB }\n",
            "}\n",
            "allocator acme:allocator\n",
            "overrides {\n",
            "    acme:image use artifact\n",
            "}\n",
            "description \"photo package\"\n",
            "license \"MIT\"\n",
            "repository \"https://example.invalid/photo\"\n",
            "authors { \"A. Author\", \"B. Author\" }\n",
            "// preserved at EOF\n",
        );
        assert_eq!(formatted, expected);
        assert_eq!(
            format_package(formatted.as_bytes()).expect("canonical package is a fixed point"),
            formatted
        );
        assert!(parse(formatted.as_bytes()).errors().is_empty());
    }

    #[test]
    fn preserves_eof_comment_spacing_and_empty_files() {
        let source = b"package acme:app@1.0.0\nsource \"src\"\n\n// first eof comment\n\n/* second\n   eof */\n";
        assert_eq!(
            format_package(source).expect("valid package should format"),
            "package acme:app @ 1.0.0\nsource \"src\"\n\n// first eof comment\n\n/* second\n   eof */\n"
        );
        assert_eq!(
            format_package(b"").expect("empty input is structural"),
            "\n"
        );
    }

    #[test]
    fn preserves_package_heading_section_and_comment_only_files() {
        let source = b"// package heading\npackage acme:app@1.0.0\n\n// source section\nsource \"src\"\n// eof\n";
        assert_eq!(
            format_package(source).expect("comments around declarations should format"),
            "// package heading\npackage acme:app @ 1.0.0\n\n// source section\nsource \"src\"\n// eof\n"
        );
        let blank_after_comment =
            b"package acme:app@1.0.0\nsource \"src\"\n// section\n\nwit \"wit\"\n";
        assert_eq!(
            format_package(blank_after_comment).expect("section comments should format"),
            "package acme:app @ 1.0.0\nsource \"src\"\n// section\n\nwit \"wit\"\n"
        );
        assert_eq!(
            format_package(b"// only a comment\n").expect("comment-only package should format"),
            "// only a comment\n"
        );
    }

    #[test]
    fn preserves_parser_limit_details_in_the_refusal() {
        let source = vec![b' '; MAX_PACKAGE_MANIFEST_BYTES + 1];
        let error = format_package(&source).expect_err("oversized input should be refused");
        assert!(matches!(
            error,
            PackageFormatError::Refused(ref refusal)
                if refusal.lexical_errors.is_empty()
                    && matches!(
                        refusal.syntax_errors.as_slice(),
                        [error] if error.kind == PackageParseErrorKind::ManifestTooLarge
                    )
        ));
    }

    #[test]
    fn formats_many_duplicate_declarations_with_leading_comments() {
        let mut source = b"package acme:app@1.0.0\n".to_vec();
        for _ in 0..256 {
            source.extend_from_slice(b"// repeated declaration\ntargets { wasm32-core }\n");
        }

        let formatted = format_package(&source).expect("structural duplicates should format");
        assert_eq!(formatted.matches("// repeated declaration").count(), 256);
        assert_eq!(formatted.matches("targets { wasm32-core }").count(), 256);
    }

    #[test]
    fn reports_the_check_and_input_offset_for_token_verification_failures() {
        let original = PackageInput::new(b"package acme:app@1.0.0\n");
        let changed = PackageInput::new(b"package acme:other@1.0.0\n");
        let original_index =
            PackageSyntaxIndex::new(&original).expect("original package should index");
        let changed_index =
            PackageSyntaxIndex::new(&changed).expect("changed package should index");
        let mismatch =
            package_token_forms_match(&original, &original_index, &changed, &changed_index)
                .expect_err("changed package token should fail verification");
        assert_eq!(mismatch.input_byte_offset, Some(8));
        assert_eq!(
            verification_failure(
                PackageVerificationCheck::TokenPreservation,
                mismatch.input_byte_offset
            ),
            PackageFormatError::VerificationFailed(PackageVerificationFailure {
                check: PackageVerificationCheck::TokenPreservation,
                input_byte_offset: Some(8),
            })
        );
    }

    #[test]
    fn maps_output_only_tokens_and_diagnostics_to_the_input_eof() {
        let source = b"package acme:app@1.0.0\n";
        let input = PackageInput::new(source);
        let input_index = PackageSyntaxIndex::new(&input).expect("input should index");
        let output = PackageInput::new(b"package acme:app@1.0.0\nextra\n");
        let output_index = PackageSyntaxIndex::new(&output).expect("output should index");
        let extra_token_offset = output.source().len() - b"extra\n".len();

        assert_eq!(
            package_output_offset_to_input(
                &input,
                &input_index,
                &output,
                Some(&output_index),
                extra_token_offset,
            ),
            source.len()
        );

        let duplicate = PackageInput::new(b"package acme:app@1.0.0\npackage acme:app@1.0.0\n");
        let mismatch = package_diagnostics_match(&input, &duplicate)
            .expect_err("an output-only parser diagnostic should fail verification");
        assert_eq!(mismatch.input_byte_offset, Some(source.len()));

        let oversized_bytes = vec![b' '; MAX_PACKAGE_MANIFEST_BYTES + 1];
        let oversized_output = PackageInput::new(&oversized_bytes);
        let mismatch_offset = package_output_offset_to_input(
            &input,
            &input_index,
            &oversized_output,
            None,
            first_refusal_offset(&oversized_output).expect("oversized output should be refused"),
        );
        assert_eq!(mismatch_offset, source.len());
    }

    #[test]
    fn formats_structural_package_errors_but_refuses_syntax_errors() {
        use sai_package::lexer::PackageLexErrorKind;

        let duplicate = b"package acme:app@1.0.0\npackage acme:app@1.0.0\n";
        assert_eq!(
            format_package(duplicate).expect("duplicate declarations are formatable"),
            "package acme:app @ 1.0.0\npackage acme:app @ 1.0.0\n"
        );

        let error = format_package(b"package acme:app@1.0.0\ntargets { wasm32-core\n")
            .expect_err("missing brace should be refused");
        assert!(matches!(
            error,
            PackageFormatError::Refused(ref refusal)
                if refusal.lexical_errors.is_empty()
                    && !refusal.syntax_errors.is_empty()
        ));
        let lexical_error = format_package(b"package acme:app@1.0.0\nsource \"bad\\q\"\n")
            .expect_err("invalid escapes should be refused");
        assert!(matches!(
            lexical_error,
            PackageFormatError::Refused(ref refusal)
                if refusal.lexical_errors.iter().any(|error| {
                    error.kind == PackageLexErrorKind::InvalidEscape
                })
        ));
    }
}
