use std::{error::Error, fmt};

use sai_syntax::{
    GreenElement, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TriviaKind,
    ast::{AstNode, AstToken, Expr, UseDecl},
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

    if node.kind() == SyntaxKind::SOURCE_FILE {
        normalize_source_file_use_order(&mut children);
    }
    if node.kind() == SyntaxKind::USE_ITEMS {
        normalize_use_item_order(&mut children);
    }
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

fn normalize_source_file_use_order(children: &mut Vec<SyntaxElement>) {
    let uses = children
        .iter()
        .filter_map(|element| match element {
            SyntaxElement::Node(node) if node.kind() == SyntaxKind::USE_DECL => Some(node.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if uses.is_empty() {
        return;
    }
    let mut keyed_uses = uses
        .into_iter()
        .map(|node| {
            let key = use_declaration_normalization_key(&node)
                .unwrap_or_else(|| (false, normalized_signature(&node)));
            (key, node)
        })
        .collect::<Vec<_>>();
    keyed_uses.sort_by(|(left, _), (right, _)| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_bytes().cmp(right.1.as_bytes()))
    });
    let uses = keyed_uses
        .into_iter()
        .map(|(_, node)| node)
        .collect::<Vec<_>>();

    children.retain(|element| {
        !matches!(element, SyntaxElement::Node(node) if node.kind() == SyntaxKind::USE_DECL)
    });
    let insertion_index = children
        .iter()
        .position(|element| {
            matches!(element, SyntaxElement::Node(node) if node.kind() == SyntaxKind::MODULE_HEADER)
        })
        .map_or(0, |index| index + 1);
    children.splice(
        insertion_index..insertion_index,
        uses.into_iter().map(SyntaxElement::Node),
    );
}

fn normalize_use_item_order(children: &mut [SyntaxElement]) {
    let positions = children
        .iter()
        .enumerate()
        .filter_map(|(index, element)| match element {
            SyntaxElement::Node(node) if node.kind() == SyntaxKind::USE_ITEM => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();
    let items = positions
        .iter()
        .filter_map(|&index| match &children[index] {
            SyntaxElement::Node(node) => Some(node.clone()),
            SyntaxElement::Token(_) => None,
        })
        .collect::<Vec<_>>();
    let mut keyed_items = items
        .into_iter()
        .map(|node| (normalized_signature(&node), node))
        .collect::<Vec<_>>();
    keyed_items.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (position, (_, item)) in positions.into_iter().zip(keyed_items) {
        children[position] = SyntaxElement::Node(item);
    }
}

fn normalized_signature(node: &SyntaxNode) -> String {
    let elements = syntax_normal_form(node)
        .into_iter()
        .map(|event| event.element)
        .collect::<Vec<_>>();
    format!("{elements:?}")
}

fn use_declaration_normalization_key(node: &SyntaxNode) -> Option<(bool, String)> {
    let declaration = UseDecl::cast(node.clone())?;
    let is_public = declaration
        .visibility()
        .is_some_and(|visibility| visibility.pub_token().is_some());
    let package = declaration.package()?.syntax().text().to_owned();
    let alias = if let Some(alias) = declaration.alias() {
        Some(syntax_name_text(alias.name()?.syntax())?)
    } else {
        None
    };
    let use_items = declaration.items();
    let mut items = if let Some(use_items) = &use_items {
        let items = use_items
            .items()
            .map(|item| {
                let alias = if let Some(alias) = item.alias() {
                    Some(syntax_name_text(alias.syntax())?)
                } else {
                    None
                };
                Some((syntax_name_text(item.name()?.syntax())?, alias))
            })
            .collect::<Option<Vec<_>>>()?;
        if items.len() != use_items.syntax().children().count() {
            return None;
        }
        items
    } else {
        Vec::new()
    };
    items.sort_by(|left, right| {
        left.0.as_bytes().cmp(right.0.as_bytes()).then_with(|| {
            left.1
                .as_deref()
                .map(str::as_bytes)
                .cmp(&right.1.as_deref().map(str::as_bytes))
        })
    });
    let alias = alias.map_or_else(String::new, |alias| format!(" as {alias}"));
    let items = if use_items.is_some() {
        format!(
            ".{{{}}}",
            items
                .into_iter()
                .map(|(name, alias)| {
                    alias.map_or(name.clone(), |alias| format!("{name} as {alias}"))
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    Some((is_public, format!("{package}{alias}{items}")))
}

fn syntax_name_text(node: &SyntaxNode) -> Option<String> {
    if node.children().next().is_some() {
        return None;
    }
    let tokens = node
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    match tokens.as_slice() {
        [token] if matches!(token.kind(), SyntaxKind::LIDENT | SyntaxKind::UIDENT) => {
            Some(token.text().to_owned())
        }
        _ => None,
    }
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
            format_source(b"const ANSWER: I32 = 42"),
            Err(SourceFormatError::UnsupportedShape)
        );
    }

    #[test]
    fn formats_file_leading_and_eof_comments() {
        for (source, expected) in [
            (
                b"// file header  \n\nfn f(){}".as_slice(),
                "// file header\n\nfn f() {}\n",
            ),
            (
                b"// file header\n\nfn f(){}",
                "// file header\n\nfn f() {}\n",
            ),
            (
                b"fn f(){}\n// eof comment  \n",
                "fn f() {}\n// eof comment\n",
            ),
            (
                b"fn f(){}\n\n// eof comment\n",
                "fn f() {}\n\n// eof comment\n",
            ),
            (
                b"use sai:http\n/// misplaced",
                "use sai:http\n/// misplaced\n",
            ),
            (b"// comment-only file".as_slice(), "// comment-only file\n"),
            (
                b"// license\n\nfn f(){}\nuse sai:core",
                "// license\n\nuse sai:core\n\nfn f() {}\n",
            ),
            (
                b"// license\n\n//! module\nfn f(){}",
                "// license\n\n//! module\n\nfn f() {}\n",
            ),
        ] {
            let formatted = format_source(source).expect("outer comments format");
            assert_eq!(formatted, expected);
            assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
        }
    }

    #[test]
    fn formats_multiline_file_leading_block_comments_verbatim() {
        let formatted = format_source(b"/* header\n * continued  \n */\n\nfn f(){}")
            .expect("leading block comment formats");
        assert_eq!(formatted, "/* header\n * continued\n */\n\nfn f() {}\n");
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn formats_shebangs_and_preserves_the_following_blank_line() {
        for (source, expected) in [
            (
                b"#!/usr/bin/env sai  \nfn f(){}".as_slice(),
                "#!/usr/bin/env sai\nfn f() {}\n",
            ),
            (
                b"#!/usr/bin/env sai  \r\nfn f(){}",
                "#!/usr/bin/env sai\nfn f() {}\n",
            ),
            (
                b"#!/usr/bin/env sai  \n\n// header\n\nfn f(){}",
                "#!/usr/bin/env sai\n\n// header\n\nfn f() {}\n",
            ),
            (
                b"#!/usr/bin/env sai\n\n//! module\nfn f(){}",
                "#!/usr/bin/env sai\n\n//! module\n\nfn f() {}\n",
            ),
            (
                b"#!/usr/bin/env sai  \n// comment-only\n",
                "#!/usr/bin/env sai\n// comment-only\n",
            ),
            (b"#!/usr/bin/env sai  ", "#!/usr/bin/env sai\n"),
        ] {
            let formatted = format_source(source).expect("shebang formats");
            assert_eq!(formatted, expected);
            assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
        }
    }

    #[test]
    fn keeps_comments_inside_items_fail_closed() {
        let source = "fn f() {\n    let value = /* inside a statement */ 1\n}";
        assert_eq!(
            format_source(source.as_bytes()),
            Err(SourceFormatError::UnsupportedShape),
            "{source}"
        );
    }

    #[test]
    fn formats_comments_attached_to_block_opening_braces() {
        let source = concat!(
            "fn line() { // opening line\n",
            "    1\n",
            "}\n",
            "fn block() {/* opening block */\n",
            "    2\n",
            "}\n",
            "fn multiline() { /* first line\n",
            "    second line */\n",
            "    3\n",
            "}\n",
            "fn empty() { // only content\n",
            "}\n",
            "fn repeated() { /* first\n",
            "second */ /* tail */\n",
            "    4\n",
            "}\n",
            "type User { // fields\n",
            "    id: U32\n",
            "}\n",
            "enum State { /* variants */\n",
            "    Ready\n",
            "}\n",
        );
        let expected = concat!(
            "fn line() { // opening line\n",
            "    1\n",
            "}\n",
            "\n",
            "fn block() { /* opening block */\n",
            "    2\n",
            "}\n",
            "\n",
            "fn multiline() { /* first line\n",
            "    second line */\n",
            "    3\n",
            "}\n",
            "\n",
            "fn empty() { // only content\n",
            "}\n",
            "\n",
            "fn repeated() { /* first\n",
            "second */\n",
            "    /* tail */\n",
            "    4\n",
            "}\n",
            "\n",
            "type User { // fields\n",
            "    id: U32\n",
            "}\n",
            "\n",
            "enum State { /* variants */\n",
            "    Ready\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("opening brace comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn formats_own_line_comments_before_block_opening_braces() {
        let source = concat!(
            "fn work()\n",
            "// before body\n",
            "{\n",
            "    1\n",
            "}\n",
            "fn empty()\n",
            "// before empty body\n",
            "{}\n",
            "fn clause() where T: Copy\n",
            "// before clause body\n",
            "{\n",
            "    2\n",
            "}\n",
        );
        let expected = concat!(
            "fn work()\n",
            "// before body\n",
            "{\n",
            "    1\n",
            "}\n",
            "\n",
            "fn empty()\n",
            "// before empty body\n",
            "{}\n",
            "\n",
            "fn clause()\n",
            "    where T: Copy\n",
            "// before clause body\n",
            "{\n",
            "    2\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("opening brace comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn keeps_own_line_comments_before_record_and_enum_braces_fail_closed() {
        for source in [
            "type User\n// before fields\n{\n    id: U32\n}",
            "enum State\n// before variants\n{\n    Ready\n}",
        ] {
            assert_eq!(
                format_source(source.as_bytes()),
                Err(SourceFormatError::UnsupportedShape),
                "{source}"
            );
        }
    }

    #[test]
    fn formats_own_line_comments_between_record_fields_and_enum_variants() {
        let source = concat!(
            "type User {\n",
            "    // attached to id\n",
            "    id: U32\n",
            "\n",
            "    // detached section\n",
            "\n",
            "    // attached to name\n",
            "    name: Str\n",
            "    // before the closing brace\n",
            "}\n",
            "enum State {\n",
            "    // pending state\n",
            "    Pending\n",
            "    /* active state */\n",
            "    Active\n",
            "    // before the closing brace\n",
            "}\n",
            "type Empty {\n",
            "    // the only list content\n",
            "}\n",
            "resource File {\n",
            "    // file handle\n",
            "    handle: U32\n",
            "}",
        );
        let expected = concat!(
            "type User {\n",
            "    // attached to id\n",
            "    id: U32\n",
            "\n",
            "    // detached section\n",
            "\n",
            "    // attached to name\n",
            "    name: Str\n",
            "    // before the closing brace\n",
            "}\n",
            "\n",
            "enum State {\n",
            "    // pending state\n",
            "    Pending\n",
            "    /* active state */\n",
            "    Active\n",
            "    // before the closing brace\n",
            "}\n",
            "\n",
            "type Empty {\n",
            "    // the only list content\n",
            "}\n",
            "\n",
            "resource File {\n",
            "    // file handle\n",
            "    handle: U32\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("list comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn comments_inside_list_elements_remain_fail_closed() {
        for source in [
            "type User {\n    id: /* inside a field */ U32\n}",
            "enum State {\n    Pending = /* inside a discriminant */ 1\n}",
        ] {
            assert_eq!(
                format_source(source.as_bytes()),
                Err(SourceFormatError::UnsupportedShape),
                "{source}"
            );
        }
    }

    #[test]
    fn preserves_trailing_comments_on_list_elements_and_removed_commas() {
        let source = concat!(
            "type User {\n",
            "    id: U32, // id comment\n",
            "    name: Str // name comment\n",
            "}\n",
            "enum State {\n",
            "    Pending, // pending comment\n",
            "    Active /* active comment */\n",
            "}\n",
            "type Repeated {\n",
            "    id: U32 /* first\n",
            "continued */ /* second */\n",
            "}\n",
            "type FinalBlock {\n",
            "    id: U32 /* only\n",
            "continued */\n",
            "}",
        );
        let expected = concat!(
            "type User {\n",
            "    id: U32 // id comment\n",
            "    name: Str // name comment\n",
            "}\n",
            "\n",
            "enum State {\n",
            "    Pending // pending comment\n",
            "    Active /* active comment */\n",
            "}\n",
            "\n",
            "type Repeated {\n",
            "    id: U32 /* first\n",
            "continued */\n",
            "    /* second */\n",
            "}\n",
            "\n",
            "type FinalBlock {\n",
            "    id: U32 /* only\n",
            "continued */\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("trailing list comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn preserves_comments_after_field_and_variant_list_closing_braces() {
        let source = concat!(
            "type User {\n",
            "    id: U32\n",
            "} // record note\n",
            "enum State {\n",
            "    Pending\n",
            "} /* enum note */",
        );
        let expected = concat!(
            "type User {\n",
            "    id: U32\n",
            "} // record note\n",
            "\n",
            "enum State {\n",
            "    Pending\n",
            "} /* enum note */\n",
        );
        let formatted = format_source(source.as_bytes()).expect("trailing comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn formats_comments_at_block_statement_boundaries() {
        let source = concat!(
            "fn work() {\n",
            "    // initialize\n",
            "    let value = 1\n",
            "\n",
            "    // return this value\n",
            "    value // expression note\n",
            "    // before the closing brace\n",
            "}\n",
            "fn empty() {\n",
            "    // the only block content\n",
            "}\n",
            "fn nested() {\n",
            "    if true {\n",
            "        // nested statement\n",
            "        2\n",
            "    }\n",
            "}\n",
            "fn closure() {\n",
            "    let run = fn() {\n",
            "        // closure statement\n",
            "        3\n",
            "    }\n",
            "}\n",
            "fn unsafe_call() {\n",
            "    unsafe {\n",
            "        // unsafe statement\n",
            "        operation()\n",
            "    }\n",
            "}\n",
            "fn chain(xs: Vec<Item>) {\n",
            "    xs.map(fn() {\n",
            "        // comment-only closure\n",
            "    }).filter(keep)\n",
            "}",
        );
        let expected = concat!(
            "fn work() {\n",
            "    // initialize\n",
            "    let value = 1\n",
            "\n",
            "    // return this value\n",
            "    value // expression note\n",
            "    // before the closing brace\n",
            "}\n",
            "\n",
            "fn empty() {\n",
            "    // the only block content\n",
            "}\n",
            "\n",
            "fn nested() {\n",
            "    if true {\n",
            "        // nested statement\n",
            "        2\n",
            "    }\n",
            "}\n",
            "\n",
            "fn closure() {\n",
            "    let run = fn() {\n",
            "        // closure statement\n",
            "        3\n",
            "    }\n",
            "}\n",
            "\n",
            "fn unsafe_call() {\n",
            "    unsafe {\n",
            "        // unsafe statement\n",
            "        operation()\n",
            "    }\n",
            "}\n",
            "\n",
            "fn chain(xs: Vec<Item>) {\n",
            "    xs\n",
            "        .map(fn() {\n",
            "            // comment-only closure\n",
            "        })\n",
            "        .filter(keep)\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("block comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn attached_comments_move_with_top_level_declarations() {
        let source = concat!(
            "// function comment\n",
            "fn helper(){}\n",
            "// import comment\n",
            "use sai:z // import note\n",
            "use sai:a",
        );
        let expected = concat!(
            "use sai:a\n",
            "// import comment\n",
            "use sai:z // import note\n",
            "\n",
            "// function comment\n",
            "fn helper() {}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("attached comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn detached_top_level_comments_stay_between_their_neighboring_items() {
        for (source, expected) in [
            (
                b"fn first(){}\n\n// section\n\nfn second(){}".as_slice(),
                "fn first() {}\n\n// section\n\nfn second() {}\n",
            ),
            (
                b"fn first(){}\n\n// detached\n\n// attached\nfn second(){}",
                "fn first() {}\n\n// detached\n\n// attached\nfn second() {}\n",
            ),
            (
                b"fn first(){}\n// section one\n// section two\n\nfn second(){}",
                "fn first() {}\n// section one\n// section two\n\nfn second() {}\n",
            ),
        ] {
            let formatted = format_source(source).expect("detached comments format");
            assert_eq!(formatted, expected);
            assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
        }
    }

    #[test]
    fn detached_comments_from_use_declarations_precede_the_sorted_use_block() {
        let source = concat!(
            "fn helper(){}\n",
            "use sai:z // attached\n",
            "// second section\n\n",
            "use sai:a\n",
            "// third section\n\n",
            "use sai:m",
        );
        let expected = concat!(
            "// second section\n\n",
            "// third section\n\n",
            "use sai:a\n",
            "use sai:m\n",
            "use sai:z // attached\n",
            "\n",
            "fn helper() {}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("detached use comments format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn formats_trailing_top_level_line_and_block_comments() {
        for (source, expected) in [
            (
                b"fn first(){} // trailing\nfn second(){}".as_slice(),
                "fn first() {} // trailing\nfn second() {}\n",
            ),
            (
                b"fn first(){} /* trailing */\nfn second(){}",
                "fn first() {} /* trailing */\nfn second() {}\n",
            ),
            (
                b"fn first(){} /* multi\n line */\nfn second(){}",
                "fn first() {} /* multi\n line */\n\nfn second() {}\n",
            ),
            (
                b"fn first(){} // trailing\n// eof\n",
                "fn first() {} // trailing\n// eof\n",
            ),
        ] {
            let formatted = format_source(source).expect("top-level trailing comments format");
            assert_eq!(formatted, expected);
            assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
        }
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
    fn sorts_and_moves_use_declarations_and_selective_items() {
        let source = concat!(
            "use sai:http/client.{HttpError, HttpClient}\n",
            "fn helper() {}\n",
            "pub use acme:app/model/user.{UserId, User}\n",
            "use acme:image/model.{Image}\n",
            "use sai:core/fmt.{Writer as Sink, FormatSpec}\n",
        );
        let expected = concat!(
            "use acme:image/model.{Image}\n",
            "use sai:core/fmt.{FormatSpec, Writer as Sink}\n",
            "use sai:http/client.{HttpClient, HttpError}\n",
            "\n",
            "pub use acme:app/model/user.{User, UserId}\n",
            "\n",
            "fn helper() {}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("use declarations format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn formats_use_aliases_and_places_them_after_module_headers() {
        let source = concat!(
            "//! module\n",
            "fn helper() {}\n",
            "pub use sai:core/fmt as fmt\n",
            "use sai:core as core\n",
        );
        let expected = concat!(
            "//! module\n",
            "\n",
            "use sai:core as core\n",
            "\n",
            "pub use sai:core/fmt as fmt\n",
            "\n",
            "fn helper() {}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("aliased uses format");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn breaks_long_use_item_lists_without_commas() {
        let names = [
            "ExtremelyLongFormatterOptionAlpha",
            "ExtremelyLongFormatterOptionBeta",
            "ExtremelyLongFormatterOptionGamma",
            "ExtremelyLongFormatterOptionDelta",
        ];
        let source = format!(
            "use sai:core/fmt.{{{}}}",
            names.into_iter().rev().collect::<Vec<_>>().join(", ")
        );
        let expected = concat!(
            "use sai:core/fmt.{\n",
            "    ExtremelyLongFormatterOptionAlpha\n",
            "    ExtremelyLongFormatterOptionBeta\n",
            "    ExtremelyLongFormatterOptionDelta\n",
            "    ExtremelyLongFormatterOptionGamma\n",
            "}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("long use item list formats");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn moves_use_docs_and_attributes_with_their_declaration() {
        let source = concat!(
            "/// legacy import\n",
            "@deprecated(\"old\")\n",
            "pub use sai:legacy\n",
            "use sai:core\n",
        );
        let expected = concat!(
            "use sai:core\n",
            "\n",
            "/// legacy import\n",
            "@deprecated(\"old\")\n",
            "pub use sai:legacy\n",
        );
        let formatted = format_source(source.as_bytes()).expect("use prefixes move with items");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn retains_duplicate_uses_stably_and_removes_selective_list_commas() {
        let source = concat!(
            "/// first duplicate\n",
            "use sai:core\n",
            "/// second duplicate\n",
            "use sai:core\n",
            "use sai:core.{B, A,}\n",
        );
        let expected = concat!(
            "/// first duplicate\n",
            "use sai:core\n",
            "/// second duplicate\n",
            "use sai:core\n",
            "use sai:core.{A, B}\n",
        );
        let formatted = format_source(source.as_bytes()).expect("duplicate uses are retained");
        assert_eq!(formatted, expected);
        assert_eq!(format_source(formatted.as_bytes()), Ok(formatted));
    }

    #[test]
    fn ignores_moved_uses_when_preserving_item_blank_lines() {
        let formatted = format_source(b"fn first() {}\nuse sai:core\nfn second() {}")
            .expect("interleaved use declaration formats");
        assert_eq!(formatted, "use sai:core\n\nfn first() {}\nfn second() {}\n");
    }

    #[test]
    fn syntax_normal_form_preserves_order_for_equal_use_sort_keys() {
        let first = parse_source(b"/// z\nuse sai:core\n/// a\nuse sai:core").unwrap();
        let swapped = parse_source(b"/// a\nuse sai:core\n/// z\nuse sai:core").unwrap();
        assert_ne!(
            normal_elements(&syntax_normal_form(first.root())),
            normal_elements(&syntax_normal_form(swapped.root()))
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
