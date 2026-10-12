use std::{error::Error, fmt};

use sai_syntax::{
    GreenElement, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TriviaKind,
    ast::{AstNode, Expr},
};

use crate::{
    ParsedSource, PrintError, SourceInputError, SourceRefusal, parse_source, print,
    source_document::{
        self, SourceDocumentError, normalize_integer_literal, normalized_string_value,
    },
};

/// The source-format verification check that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceVerificationCheck {
    /// The formatter output could not be parsed.
    OutputSyntax,
    /// The normalized syntax tree changed.
    SyntaxTreePreservation,
    /// Comment text or comment multiplicity changed.
    CommentPreservation,
    /// Formatting the canonical output changed it again.
    Idempotency,
}

/// Details about a failed source-format verification check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceVerificationFailure {
    /// The check that failed.
    pub check: SourceVerificationCheck,
    /// First differing byte in the input, when one can be identified.
    pub input_byte_offset: Option<usize>,
}

/// Errors returned while formatting source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceFormatError {
    /// Input-limit, lexical, or parser errors refuse formatting.
    Refused(SourceRefusal),
    /// The source parses, but its syntax is not yet supported by the formatter.
    UnsupportedShape,
    /// Parsing, document construction, or verification of oversized output failed internally.
    InternalError,
    /// The document printer rejected the generated document.
    Print(PrintError),
    /// The formatted result failed a preservation or fixed-point check.
    VerificationFailed(SourceVerificationFailure),
}

impl fmt::Display for SourceFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(
                formatter,
                "source input contains {} input, {} lexical, and {} syntax errors",
                refusal.input_errors.len(),
                refusal.lexical_errors.len(),
                refusal.syntax_errors.len()
            ),
            Self::UnsupportedShape => {
                formatter.write_str("source formatter does not support this syntax shape yet")
            }
            Self::InternalError => formatter.write_str("source formatter could not render input"),
            Self::Print(error) => write!(formatter, "source formatter printer failed: {error:?}"),
            Self::VerificationFailed(failure) => write!(
                formatter,
                "source formatter verification failed: {:?}",
                failure.check
            ),
        }
    }
}

impl Error for SourceFormatError {}

/// Format one SAI source file into its canonical layout.
///
/// The result is parsed and checked for normalized syntax-tree preservation,
/// comment preservation, and idempotency. Inputs outside the current
/// document-builder subset return [`SourceFormatError::UnsupportedShape`].
pub fn format_source(source: &[u8]) -> Result<String, SourceFormatError> {
    let input = parse_source(source).map_err(map_source_input_error)?;
    let formatted = format_parsed_source(&input)?;
    let output = match parse_source(formatted.as_bytes()) {
        Ok(output) => output,
        Err(error) => return Err(output_parse_error(error)),
    };

    let syntax_difference = syntax_preservation_offset(&input, &output);
    if syntax_difference.is_some() {
        return Err(verification_failure(
            SourceVerificationCheck::SyntaxTreePreservation,
            syntax_difference,
        ));
    }
    if comment_multiset(input.root()) != comment_multiset(output.root()) {
        return Err(verification_failure(
            SourceVerificationCheck::CommentPreservation,
            None,
        ));
    }

    let repeated = match format_source_once(formatted.as_bytes()) {
        Ok(repeated) => repeated,
        Err(SourceFormatError::UnsupportedShape | SourceFormatError::Refused(_)) => {
            return Err(verification_failure(
                SourceVerificationCheck::Idempotency,
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    if formatted != repeated {
        return Err(verification_failure(
            SourceVerificationCheck::Idempotency,
            None,
        ));
    }
    Ok(formatted)
}

fn map_source_input_error(error: SourceInputError) -> SourceFormatError {
    match error {
        SourceInputError::Refused(refusal) => SourceFormatError::Refused(refusal),
        SourceInputError::InternalError => SourceFormatError::InternalError,
    }
}

fn output_parse_error(error: SourceInputError) -> SourceFormatError {
    match error {
        SourceInputError::Refused(refusal) if refusal.input_errors.is_empty() => {
            verification_failure(SourceVerificationCheck::OutputSyntax, None)
        }
        SourceInputError::Refused(_) | SourceInputError::InternalError => {
            SourceFormatError::InternalError
        }
    }
}

fn format_source_once(source: &[u8]) -> Result<String, SourceFormatError> {
    let parsed = parse_source(source).map_err(map_source_input_error)?;
    format_parsed_source(&parsed)
}

fn format_parsed_source(source: &ParsedSource) -> Result<String, SourceFormatError> {
    let document = source_document::source_document(source).map_err(|error| match error {
        SourceDocumentError::UnsupportedShape => SourceFormatError::UnsupportedShape,
    })?;
    print(&document).map_err(SourceFormatError::Print)
}

fn verification_failure(
    check: SourceVerificationCheck,
    input_byte_offset: Option<usize>,
) -> SourceFormatError {
    SourceFormatError::VerificationFailed(SourceVerificationFailure {
        check,
        input_byte_offset,
    })
}

#[derive(Debug, Eq, PartialEq)]
enum NormalElement {
    NodeStart(SyntaxKind),
    NodeEnd,
    Token(SyntaxKind, String),
}

enum NormalWork {
    Node(SyntaxNode),
    Token(SyntaxToken),
    NodeEnd(usize),
}

struct NormalEvent {
    element: NormalElement,
    byte_offset: usize,
}

fn syntax_normal_form(root: &SyntaxNode) -> Vec<NormalEvent> {
    let mut result = Vec::new();
    let mut pending = vec![NormalWork::Node(root.clone())];

    while let Some(work) = pending.pop() {
        match work {
            NormalWork::Node(node) => {
                if let Some(expression) = unwrapped_parenthesized_expression(&node) {
                    pending.push(NormalWork::Node(expression));
                    continue;
                }

                let byte_offset = node.text_range().start().to_usize();
                result.push(NormalEvent {
                    element: NormalElement::NodeStart(node.kind()),
                    byte_offset,
                });
                pending.push(NormalWork::NodeEnd(byte_offset));
                let children = normalized_children(&node);
                pending.extend(children.into_iter().rev().map(|element| match element {
                    SyntaxElement::Node(child) => NormalWork::Node(child),
                    SyntaxElement::Token(token) => NormalWork::Token(token),
                }));
            }
            NormalWork::Token(token) => {
                if matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::EOF) {
                    continue;
                }
                let text = match token.kind() {
                    SyntaxKind::INT => normalize_integer_literal(token.text()),
                    SyntaxKind::FLOAT => token.text().replace('E', "e"),
                    SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER => {
                        token.text().trim_end_matches([' ', '\t']).to_owned()
                    }
                    SyntaxKind::STRING if token.text().contains(['\n', '\r']) => {
                        normalized_string_value(token.text())
                            .unwrap_or_else(|| token.text().to_owned())
                    }
                    _ => token.text().to_owned(),
                };
                result.push(NormalEvent {
                    element: NormalElement::Token(token.kind(), text),
                    byte_offset: token.text_range().start().to_usize(),
                });
            }
            NormalWork::NodeEnd(byte_offset) => result.push(NormalEvent {
                element: NormalElement::NodeEnd,
                byte_offset,
            }),
        }
    }
    result
}

fn first_normal_form_difference(
    input: &[NormalEvent],
    output: &[NormalEvent],
    input_source_len: usize,
) -> Option<usize> {
    for (input_event, output_event) in input.iter().zip(output) {
        if input_event.element != output_event.element {
            return Some(input_event.byte_offset);
        }
    }
    (input.len() != output.len()).then(|| {
        input
            .get(output.len().min(input.len()))
            .map_or(input_source_len, |event| event.byte_offset)
    })
}

fn syntax_preservation_offset(input: &ParsedSource, output: &ParsedSource) -> Option<usize> {
    let input_normal_form = syntax_normal_form(input.root());
    let output_normal_form = syntax_normal_form(output.root());
    first_normal_form_difference(&input_normal_form, &output_normal_form, input.text().len())
        .map(|offset| input.original_byte_offset(offset).unwrap_or(offset))
}

fn unwrapped_parenthesized_expression(node: &SyntaxNode) -> Option<SyntaxNode> {
    if node.kind() != SyntaxKind::PAREN_EXPR {
        return None;
    }
    let Expr::Paren(paren) = Expr::cast(node.clone())? else {
        return None;
    };
    let expression = paren.expression()?;
    matches!(expression, Expr::Binary(_) | Expr::Range(_)).then(|| expression.syntax().clone())
}

fn normalized_children(node: &SyntaxNode) -> Vec<SyntaxElement> {
    let mut children = node.children_with_tokens().collect::<Vec<_>>();

    if node.kind() == SyntaxKind::FN_DECL {
        normalize_function_clause_order(&mut children);
    }
    if is_brace_separated_list(node.kind()) {
        children.retain(|child| {
            !matches!(child, SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA)
        });
    } else if let Some(close) = trailing_comma_close(node) {
        if let Some(index) = children.windows(2).position(|pair| {
            matches!(&pair[0], SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA)
                && matches!(&pair[1], SyntaxElement::Token(token) if token.kind() == close)
        }) {
            children.remove(index);
        }
    }

    if node.kind() == SyntaxKind::OR_PAT {
        let first_significant = children.iter().position(|element| {
            !matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE)
        });
        if let Some(index) = first_significant.filter(|&index| {
            matches!(
                children.get(index),
                Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::PIPE
            )
        }) {
            children.remove(index);
        }
    }
    children
}

fn is_brace_separated_list(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FIELD_LIST
            | SyntaxKind::VARIANT_LIST
            | SyntaxKind::MATCH_ARM_LIST
            | SyntaxKind::EFFECT_SET
            | SyntaxKind::BORROWS_CLAUSE
            | SyntaxKind::COVERS_CLAUSE
            | SyntaxKind::INVARIANT_EXPR_LIST
            | SyntaxKind::USE_ITEMS
    )
}

fn trailing_comma_close(node: &SyntaxNode) -> Option<SyntaxKind> {
    Some(match node.kind() {
        SyntaxKind::PARAM_LIST
        | SyntaxKind::CLOSURE_PARAM_LIST
        | SyntaxKind::ARG_LIST
        | SyntaxKind::ATTR_ARGS
        | SyntaxKind::FN_PTR_TYPE
        | SyntaxKind::FN_SUGAR_BOUND
        | SyntaxKind::CTOR_PAT_ARGS => SyntaxKind::R_PAREN,
        SyntaxKind::TUPLE_PAT
            if node
                .children()
                .filter_map(sai_syntax::ast::Pattern::cast)
                .count()
                != 1 =>
        {
            SyntaxKind::R_PAREN
        }
        SyntaxKind::SLICE_PAT => SyntaxKind::R_BRACKET,
        SyntaxKind::GENERIC_ARG_LIST | SyntaxKind::GENERIC_PARAM_LIST => SyntaxKind::GT,
        SyntaxKind::TUPLE_SHAPE | SyntaxKind::RECORD_SHAPE => SyntaxKind::R_PAREN,
        SyntaxKind::TUPLE_TYPE
            if node
                .children()
                .filter_map(sai_syntax::ast::Type::cast)
                .count()
                != 1 =>
        {
            SyntaxKind::R_PAREN
        }
        SyntaxKind::TUPLE_EXPR if node.children().filter_map(Expr::cast).count() != 1 => {
            SyntaxKind::R_PAREN
        }
        SyntaxKind::ARRAY_EXPR => SyntaxKind::R_BRACKET,
        _ => return None,
    })
}

fn normalize_function_clause_order(children: &mut Vec<SyntaxElement>) {
    let mut clauses = Vec::new();
    let mut first_clause_index = None;
    for (index, child) in children.iter().enumerate() {
        let SyntaxElement::Node(node) = child else {
            continue;
        };
        let Some(rank) = function_clause_rank(node.kind()) else {
            continue;
        };
        first_clause_index.get_or_insert(index);
        clauses.push((rank, clauses.len(), child.clone()));
    }
    let Some(first_clause_index) = first_clause_index else {
        return;
    };
    clauses.sort_by_key(|(rank, original_order, _)| (*rank, *original_order));
    children.retain(|child| {
        !matches!(child, SyntaxElement::Node(node) if function_clause_rank(node.kind()).is_some())
    });
    let normalized_clauses = clauses
        .into_iter()
        .map(|(_, _, clause)| clause)
        .collect::<Vec<_>>();
    children.splice(first_clause_index..first_clause_index, normalized_clauses);
}

fn function_clause_rank(kind: SyntaxKind) -> Option<u8> {
    Some(match kind {
        SyntaxKind::WHERE_CLAUSE => 0,
        SyntaxKind::BORROWS_CLAUSE => 1,
        SyntaxKind::EFFECTS_CLAUSE => 2,
        SyntaxKind::REQUIRES_CLAUSE => 3,
        SyntaxKind::ENSURES_CLAUSE => 4,
        _ => return None,
    })
}

fn comment_multiset(root: &SyntaxNode) -> Vec<String> {
    let mut comments = Vec::new();
    let mut pending = vec![root.green()];
    while let Some(node) = pending.pop() {
        for element in node.children() {
            match element {
                GreenElement::Node(child) => pending.push(child),
                GreenElement::Token(token) => {
                    for trivia in token.leading_trivia().iter().chain(token.trailing_trivia()) {
                        if matches!(
                            trivia.kind(),
                            TriviaKind::LineComment
                                | TriviaKind::BlockComment
                                | TriviaKind::Shebang
                        ) {
                            comments.push(normalize_comment(trivia.text()));
                        }
                    }
                    if matches!(token.kind(), SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER) {
                        comments.push(normalize_comment(token.text()));
                    }
                }
            }
        }
    }
    comments.sort();
    comments
}

fn normalize_comment(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{SourceFormatError, format_source, syntax_normal_form};
    use crate::parse_source;
    use sai_diagnostics::{Code, Diagnostic, Severity};

    #[test]
    fn formats_and_verifies_a_supported_source_file() {
        let source =
            b"///  greeting  \n@deprecated(\"old\",)\nfn greet(name:Str){let value=0xff\nvalue}";
        let formatted = format_source(source).expect("supported source formats");
        assert_eq!(
            formatted,
            "///  greeting\n@deprecated(\"old\")\nfn greet(name: Str) {\n    let value = 0xFF\n    value\n}\n"
        );
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn refuses_invalid_source_using_the_source_input_refusal() {
        assert!(matches!(
            format_source(b"fn broken("),
            Err(SourceFormatError::Refused(_))
        ));
    }

    #[test]
    fn reports_valid_but_unsupported_shapes_explicitly() {
        assert_eq!(
            format_source(b"fn f() {} // trailing"),
            Err(SourceFormatError::UnsupportedShape)
        );
    }

    #[test]
    fn formats_module_headers_and_separates_their_parts() {
        for (source, expected) in [
            (b"//! module docs  \n".as_slice(), "//! module docs\n"),
            (b"unsafe module\n", "unsafe module\n"),
            (
                b"//! first\n//! second\nunsafe module\nfn f(){}",
                "//! first\n//! second\n\nunsafe module\n\nfn f() {}\n",
            ),
            (
                b"//! module docs\nfn f(){}",
                "//! module docs\n\nfn f() {}\n",
            ),
        ] {
            let formatted = format_source(source).expect("module header formats");
            assert_eq!(formatted, expected);
            assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
        }
    }

    #[test]
    fn formats_empty_source_and_module_header_only_files() {
        assert_eq!(format_source(b""), Ok(String::new()));
        assert_eq!(
            format_source(b"//! only docs\nunsafe module\n"),
            Ok(String::from("//! only docs\n\nunsafe module\n"))
        );
    }

    #[test]
    fn syntax_normal_form_ignores_permitted_integer_and_list_changes() {
        let before = parse_source(b"fn f(x:I32,){let n=0xff\nx + n}").unwrap();
        let after = parse_source(b"fn f(x: I32) {\n    let n = 0xFF\n    (x + n)\n}\n").unwrap();
        assert_eq!(
            normal_elements(&syntax_normal_form(before.root())),
            normal_elements(&syntax_normal_form(after.root()))
        );
    }

    #[test]
    fn verifies_trailing_commas_in_closure_parameter_lists() {
        let formatted = format_source(b"fn f(){fn(value,){value}}")
            .expect("closure parameter trailing comma is formatable");
        assert_eq!(formatted, "fn f() {\n    fn(value) { value }\n}\n");
    }

    #[test]
    fn syntax_normal_form_checks_function_clause_order() {
        let before =
            parse_source(b"fn f() ensures true requires true effects {} borrows {} where T: Copy")
                .unwrap();
        let after =
            parse_source(b"fn f() where T: Copy borrows {} effects {} requires true ensures true")
                .unwrap();
        assert_eq!(
            normal_elements(&syntax_normal_form(before.root())),
            normal_elements(&syntax_normal_form(after.root()))
        );
    }

    #[test]
    fn reports_the_first_input_offset_for_a_syntax_difference() {
        let input = parse_source(b"fn f(){x}").unwrap();
        let output = parse_source(b"fn f(){y}").unwrap();
        assert_eq!(super::syntax_preservation_offset(&input, &output), Some(7));
    }

    #[test]
    fn reports_the_original_input_offset_after_a_bom() {
        let input = parse_source(b"\xef\xbb\xbffn f(){x}").unwrap();
        let output = parse_source(b"fn f(){y}").unwrap();
        assert_eq!(super::syntax_preservation_offset(&input, &output), Some(10));
    }

    #[test]
    fn normal_form_length_differences_report_the_first_missing_or_extra_offset() {
        let input = [super::NormalEvent {
            element: super::NormalElement::Token(sai_syntax::SyntaxKind::LIDENT, "x".to_owned()),
            byte_offset: 4,
        }];
        let output = [];
        assert_eq!(
            super::first_normal_form_difference(&input, &output, 9),
            Some(4)
        );

        let input = [];
        let output = [super::NormalEvent {
            element: super::NormalElement::Token(sai_syntax::SyntaxKind::LIDENT, "x".to_owned()),
            byte_offset: 4,
        }];
        assert_eq!(
            super::first_normal_form_difference(&input, &output, 9),
            Some(9)
        );
    }

    #[test]
    fn classifies_output_size_refusals_as_internal_errors() {
        let error = crate::SourceInputError::Refused(crate::SourceRefusal {
            input_errors: vec![Diagnostic::new(
                Code::ImplementationLimitExceeded,
                Severity::Error,
                "formatted output exceeds the source limit",
                None,
            )],
            lexical_errors: Vec::new(),
            syntax_errors: Vec::new(),
        });
        assert_eq!(
            super::output_parse_error(error),
            SourceFormatError::InternalError
        );
    }

    #[test]
    fn classifies_output_lexical_refusals_as_output_syntax_failures() {
        let error = crate::SourceInputError::Refused(crate::SourceRefusal {
            input_errors: Vec::new(),
            lexical_errors: vec![Diagnostic::new(
                Code::UnexpectedCharacter,
                Severity::Error,
                "invalid formatted character",
                None,
            )],
            syntax_errors: Vec::new(),
        });
        assert_eq!(
            super::output_parse_error(error),
            SourceFormatError::VerificationFailed(super::SourceVerificationFailure {
                check: super::SourceVerificationCheck::OutputSyntax,
                input_byte_offset: None,
            })
        );
    }

    #[test]
    fn classifies_output_parser_refusals_as_output_syntax_failures() {
        let error = crate::parse_source(b"fn broken(").expect_err("invalid source is refused");
        assert_eq!(
            super::output_parse_error(error),
            SourceFormatError::VerificationFailed(super::SourceVerificationFailure {
                check: super::SourceVerificationCheck::OutputSyntax,
                input_byte_offset: None,
            })
        );
    }

    #[test]
    fn returns_a_refusal_with_the_source_diagnostics() {
        let error = format_source(b"fn f() {\n").expect_err("unterminated block is refused");
        let SourceFormatError::Refused(refusal) = error else {
            panic!("expected a source refusal");
        };
        assert!(!refusal.syntax_errors.is_empty());
    }

    #[test]
    fn preserving_comments_is_checked_as_a_multiset() {
        let left = parse_source(b"fn f() {}").unwrap();
        let right = parse_source(b"fn f() {}").unwrap();
        assert_eq!(super::comment_multiset(left.root()), Vec::<String>::new());
        assert_eq!(
            super::comment_multiset(left.root()),
            super::comment_multiset(right.root())
        );
    }

    fn normal_elements(events: &[super::NormalEvent]) -> Vec<&super::NormalElement> {
        events.iter().map(|event| &event.element).collect()
    }
}
