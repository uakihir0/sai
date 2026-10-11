use std::sync::Arc;

use sai_diagnostics::{Code, Diagnostic, Location, Severity, SourceRef, Value, ValueObject};
use sai_syntax::{Parse, SyntaxNode};
use sai_vfs::{
    FileId, MAX_SOURCE_BYTES, SnapshotFile, SourceDecodeError, SourceFile, Span, TextRange,
    TextSize, WorkspaceSnapshot,
};

const SOURCE_PATH: &str = "<stdin>";

/// Diagnostics that make a source file unsafe to format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRefusal {
    /// Input-limit errors reported before the source can be decoded.
    pub input_errors: Vec<Diagnostic>,
    /// Lexical errors in the source bytes.
    pub lexical_errors: Vec<Diagnostic>,
    /// Parser errors that prevent the syntax tree from having one meaning.
    pub syntax_errors: Vec<Diagnostic>,
}

/// Errors raised while preparing source bytes for formatting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceInputError {
    /// An input-limit error or a lexer or parser error refuses formatting.
    Refused(SourceRefusal),
    /// Decoding, snapshot construction, or parsing failed internally.
    InternalError,
}

/// A lossless source parse ready for the formatter document builder.
#[derive(Debug)]
pub struct ParsedSource {
    source: Arc<SourceFile>,
    _snapshot: WorkspaceSnapshot,
    parse: Parse,
}

impl ParsedSource {
    /// The decoded source text, with a leading BOM removed.
    pub fn text(&self) -> &str {
        self.source.text()
    }

    /// The root of the lossless concrete syntax tree.
    pub fn root(&self) -> &SyntaxNode {
        &self.parse.root
    }

    /// All diagnostics emitted while lexing and parsing, including
    /// non-refusing validation diagnostics.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.parse.errors
    }
}

/// Decode, lex, and parse one source file, applying the formatter refusal
/// rules from `docs/impl/14-formatter.md` §14.2.2.
pub fn parse_source(bytes: &[u8]) -> Result<ParsedSource, SourceInputError> {
    let file_id = FileId::new(0);
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(SourceInputError::Refused(SourceRefusal {
            input_errors: vec![source_size_limit_diagnostic(file_id, bytes.len())],
            lexical_errors: Vec::new(),
            syntax_errors: Vec::new(),
        }));
    }
    let source = Arc::new(
        SourceFile::decode(file_id, bytes).map_err(|error| match error {
            SourceDecodeError::SourceTooLarge => SourceInputError::Refused(SourceRefusal {
                input_errors: vec![source_size_limit_diagnostic(file_id, bytes.len())],
                lexical_errors: Vec::new(),
                syntax_errors: Vec::new(),
            }),
            SourceDecodeError::UnremovedLeadingBom => SourceInputError::InternalError,
        })?,
    );
    let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
        Arc::clone(&source),
        SOURCE_PATH,
        None,
        None,
    )])
    .map_err(|_| SourceInputError::InternalError)?;

    let lexed = sai_syntax::lex(&snapshot, file_id).map_err(|_| SourceInputError::InternalError)?;
    let lexical_errors = lexed
        .errors
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .cloned()
        .collect::<Vec<_>>();
    let parse = match sai_syntax::parse(&snapshot, file_id) {
        Ok(parse) => parse,
        Err(_) if !lexical_errors.is_empty() => {
            return Err(SourceInputError::Refused(SourceRefusal {
                input_errors: Vec::new(),
                lexical_errors,
                syntax_errors: Vec::new(),
            }));
        }
        Err(_) => return Err(SourceInputError::InternalError),
    };
    let syntax_errors = parser_refusal_errors(&parse);
    if !lexical_errors.is_empty() || !syntax_errors.is_empty() {
        return Err(SourceInputError::Refused(SourceRefusal {
            input_errors: Vec::new(),
            lexical_errors,
            syntax_errors,
        }));
    }

    Ok(ParsedSource {
        source,
        _snapshot: snapshot,
        parse,
    })
}

fn parser_refusal_errors(parse: &Parse) -> Vec<Diagnostic> {
    parse
        .parser_diagnostics()
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.code != Code::ClauseOrder
                && is_parser_error(diagnostic.code)
        })
        .cloned()
        .collect()
}

fn source_size_limit_diagnostic(file_id: FileId, byte_len: usize) -> Diagnostic {
    let limit_mib = MAX_SOURCE_BYTES / (1024 * 1024);
    let message = format!("source file is {byte_len} bytes, exceeding the {limit_mib} MiB limit");
    let location = Location {
        file: SourceRef {
            path: SOURCE_PATH.to_owned(),
            package: None,
            snapshot: None,
        },
        span: Span::new(file_id, TextRange::empty(TextSize::new(0))),
    };
    let mut diagnostic = Diagnostic::new(
        Code::ImplementationLimitExceeded,
        Severity::Error,
        message,
        Some(location),
    );
    diagnostic.data = ValueObject::from([
        (
            "limit".to_owned(),
            Value::String("size of a source file".to_owned()),
        ),
        ("value".to_owned(), Value::Integer(byte_len as u128)),
    ]);
    diagnostic
}

fn is_parser_error(code: Code) -> bool {
    matches!(
        code,
        Code::ExpectedToken
            | Code::ExpectedExpression
            | Code::UnexpectedNewline
            | Code::ChainedComparison
            | Code::ChainedRange
            | Code::DuplicateClause
            | Code::BlockInCondition
            | Code::NoAsyncClosure
            | Code::InvalidAttributeTarget
            | Code::RedundantPubExport
            | Code::InvalidExportTarget
            | Code::NoDoubleColon
            | Code::UseAndOr
            | Code::MissingInitializer
            | Code::ItemInBlock
            | Code::BaseArgNotLast
            | Code::InvalidTupleIndex
            | Code::FloatPattern
            | Code::MultipleRestPatterns
            | Code::UnexpectedFnBody
            | Code::InterfaceFnBody
            | Code::ImplementationLimitExceeded
            | Code::ExplicitDiscriminantOnDataEnum
            | Code::UnknownAbi
            | Code::InvalidExternBlock
            | Code::FeatureNotImplemented
            | Code::GenericInterfaceItem
            | Code::InterfaceFnClause
            | Code::EffectsOnInterfaceFn
            | Code::WorldImportNeedsInterface
    )
}

#[cfg(test)]
mod tests {
    use super::{SourceInputError, parse_source};
    use sai_diagnostics::{Code, Value};
    use sai_vfs::MAX_SOURCE_BYTES;

    #[test]
    fn refuses_lexical_errors_with_the_lexer_diagnostics() {
        let error = parse_source(b"\xff").unwrap_err();

        let SourceInputError::Refused(refusal) = error else {
            panic!("lexical errors must refuse formatting: {error:?}");
        };
        assert!(refusal.input_errors.is_empty());
        assert_eq!(refusal.lexical_errors.len(), 1);
        assert_eq!(refusal.lexical_errors[0].code.as_str(), "INVALID_UTF8");
        assert!(refusal.syntax_errors.is_empty());
    }

    #[test]
    fn refuses_parser_recovery_errors() {
        let error = parse_source(b"fn {\n").unwrap_err();

        let SourceInputError::Refused(refusal) = error else {
            panic!("parser recovery errors must refuse formatting");
        };
        assert!(refusal.input_errors.is_empty());
        assert!(refusal.lexical_errors.is_empty());
        assert!(
            refusal
                .syntax_errors
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "EXPECTED_TOKEN")
        );
    }

    #[test]
    fn refuses_parser_attribute_target_errors() {
        let error =
            parse_source(b"fn f() {\n@allow(unused_imports)\nlet value = 1\n}\n").unwrap_err();

        let SourceInputError::Refused(refusal) = error else {
            panic!("parser attribute-target errors must refuse formatting");
        };
        assert!(
            refusal
                .syntax_errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
        );
    }

    #[test]
    fn retains_parser_refusals_when_lexical_errors_are_also_present() {
        let error = parse_source(b"fn f() {\nlet = 1\nlet other = \"\\q\"\n}\n").unwrap_err();

        let SourceInputError::Refused(refusal) = error else {
            panic!("lexical and parser errors must refuse formatting");
        };
        assert!(refusal.input_errors.is_empty());
        assert!(
            refusal
                .lexical_errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidEscape)
        );
        assert!(
            refusal
                .syntax_errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
    }

    #[test]
    fn clause_order_errors_remain_formatable() {
        let parsed = parse_source(b"fn f() requires true where T: Copy {}\n").unwrap();

        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "CLAUSE_ORDER")
        );
    }

    #[test]
    fn validation_errors_remain_formatable() {
        let parsed = parse_source(b"@unknown\nfn f() {}\n").unwrap();

        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "UNKNOWN_ATTRIBUTE")
        );
    }

    #[test]
    fn missing_function_body_validation_errors_remain_formatable() {
        let parsed = parse_source(b"fn f()\n").unwrap();

        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == Code::MissingFnBody)
        );
    }

    #[test]
    fn refuses_source_files_over_the_documented_byte_limit() {
        let bytes = vec![b'x'; MAX_SOURCE_BYTES + 1];
        let error = parse_source(&bytes).unwrap_err();

        let SourceInputError::Refused(refusal) = error else {
            panic!("oversized source files must be refused");
        };
        assert!(refusal.lexical_errors.is_empty());
        assert!(refusal.syntax_errors.is_empty());
        assert_eq!(refusal.input_errors.len(), 1);
        assert_eq!(
            refusal.input_errors[0].code,
            Code::ImplementationLimitExceeded
        );
        assert_eq!(
            refusal.input_errors[0].data.get("value"),
            Some(&Value::Integer((MAX_SOURCE_BYTES + 1) as u128))
        );
        assert_eq!(
            refusal.input_errors[0]
                .primary()
                .map(|location| location.span.range.start().raw()),
            Some(0)
        );
    }

    #[test]
    fn removes_bom_and_preserves_the_lossless_syntax_root() {
        let parsed = parse_source(b"\xEF\xBB\xBF/// docs\t\r\nfn f() {}\r\n").unwrap();

        assert_eq!(parsed.text(), "/// docs\t\r\nfn f() {}\r\n");
        assert_eq!(parsed.root().kind(), sai_syntax::SyntaxKind::SOURCE_FILE);
        assert_eq!(
            reconstruct_lossless_text(parsed.root().green()),
            parsed.text()
        );
    }

    fn reconstruct_lossless_text(node: &sai_syntax::GreenNode) -> String {
        let mut text = String::new();
        for child in node.children() {
            match child {
                sai_syntax::GreenElement::Node(child) => {
                    text.push_str(&reconstruct_lossless_text(child));
                }
                sai_syntax::GreenElement::Token(token) => {
                    for trivia in token.leading_trivia() {
                        text.push_str(trivia.text());
                    }
                    text.push_str(token.text());
                    for trivia in token.trailing_trivia() {
                        text.push_str(trivia.text());
                    }
                }
            }
        }
        text
    }
}
