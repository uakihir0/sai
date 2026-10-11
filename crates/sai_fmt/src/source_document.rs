use sai_syntax::{
    SyntaxKind, TriviaKind,
    ast::{self, AstNode, AstToken, Expr, Item, LiteralToken, Pattern, Stmt},
};

use crate::{Doc, ParsedSource};

/// Syntax shapes that the source document builder does not support yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceDocumentError {
    UnsupportedShape,
}

/// Build a document for the deliberately small source-formatting foundation.
///
/// The builder is private and fail-closed while source-formatting coverage is
/// being implemented. Its first unit is a function with no parameters or
/// clauses, one literal `let`, and a final name expression.
pub(super) fn source_document(source: &ParsedSource) -> Result<Doc, SourceDocumentError> {
    if source.lexed().trivia.iter().any(|trivia| {
        matches!(
            trivia.kind,
            TriviaKind::LineComment | TriviaKind::BlockComment | TriviaKind::Shebang
        )
    }) {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let tokens = source
        .lexed()
        .tokens
        .iter()
        .filter(|token| !matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF))
        .collect::<Vec<_>>();
    let expected = [
        SyntaxKind::FN_KW,
        SyntaxKind::LIDENT,
        SyntaxKind::L_PAREN,
        SyntaxKind::R_PAREN,
        SyntaxKind::L_BRACE,
        SyntaxKind::LET_KW,
        SyntaxKind::LIDENT,
        SyntaxKind::EQ,
        SyntaxKind::INT,
        SyntaxKind::LIDENT,
        SyntaxKind::R_BRACE,
    ];
    if tokens.len() != expected.len()
        || !tokens
            .iter()
            .zip(expected)
            .all(|(token, expected)| token.kind == expected)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let file = ast::SourceFile::cast(source.root().clone())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if file.module_header().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let items = file.items().collect::<Vec<_>>();
    if items.len() != 1 {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let Item::Function(function) = items.into_iter().next().expect("one source item") else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if function.docs().is_some()
        || function.attrs().next().is_some()
        || function.visibility().is_some()
        || function.generic_params().is_some()
        || function.ret_type().is_some()
        || function.where_clause().is_some()
        || function.borrows_clause().is_some()
        || function.effects_clause().is_some()
        || function.requires_clauses().next().is_some()
        || function.ensures_clauses().next().is_some()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let params = function
        .params()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if params.params().next().is_some() || params.self_param().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let block = function
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let mut statements = block.statements();
    let Some(Stmt::Let(binding_stmt)) = statements.next() else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    let Some(Stmt::Expr(tail_stmt)) = statements.next() else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if statements.next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let pattern = binding_stmt
        .pattern()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let Pattern::Binding(pattern) = pattern else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if pattern.mode().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = function
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let binding = pattern
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let initializer = binding_stmt
        .initializer()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let Expr::Literal(initializer) = initializer else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    let Some(LiteralToken::Int(literal)) = initializer.token() else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    let tail = tail_stmt
        .expression()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let Expr::Path(tail) = tail else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if tail.generic_args().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let tail_name = tail.name().ok_or(SourceDocumentError::UnsupportedShape)?;

    let name = node_text(source, name.syntax());
    let binding = node_text(source, binding.syntax());
    let literal = token_text(source.text(), literal.syntax());
    let literal = normalize_integer_literal(literal);
    let tail = node_text(source, tail_name.syntax());
    let statement_separator = source
        .text()
        .get(
            binding_stmt.syntax().text_range().end().to_usize()
                ..tail_stmt.syntax().text_range().start().to_usize(),
        )
        .unwrap_or_default();
    let blank_line = statement_separator.matches('\n').count() > 1;

    let rhs = Doc::choice([
        Doc::concat([Doc::text(" "), Doc::flat(Doc::text(literal.clone()))]),
        Doc::indent(Doc::concat([
            Doc::HardLine,
            Doc::flat(Doc::text(literal.clone())),
        ])),
        Doc::concat([Doc::text(" "), Doc::text(literal)]),
    ]);
    let separator = if blank_line {
        Doc::concat([Doc::HardLine, Doc::HardLine])
    } else {
        Doc::HardLine
    };
    Ok(Doc::concat([
        Doc::text(format!("fn {name}() {{")),
        Doc::indent(Doc::concat([
            Doc::HardLine,
            Doc::text(format!("let {binding} =")),
            rhs,
            separator,
            Doc::text(tail),
        ])),
        Doc::HardLine,
        Doc::text("}"),
    ]))
}

fn token_text<'a>(source: &'a str, token: &sai_syntax::SyntaxToken) -> &'a str {
    let range = token.text_range();
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or_default()
}

fn node_text<'a>(source: &'a ParsedSource, node: &sai_syntax::SyntaxNode) -> &'a str {
    let range = node.text_range();
    source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or_default()
}

fn normalize_integer_literal(literal: &str) -> String {
    if let Some(digits) = literal.strip_prefix("0x") {
        format!("0x{}", digits.to_ascii_uppercase())
    } else {
        literal.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceDocumentError, source_document};
    use crate::{parse_source, print};
    use sai_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

    fn format_shape(source: &[u8]) -> Result<String, SourceDocumentError> {
        let parsed = parse_source(source).expect("source parses");
        let document = source_document(&parsed)?;
        Ok(print(&document).expect("document prints"))
    }

    #[test]
    fn formats_supported_function_shape_independently_of_input_spacing() {
        let expected = "fn f() {\n    let x = 1\n    x\n}\n";

        assert_eq!(format_shape(b"fn f(){let x=1\nx}"), Ok(expected.to_owned()));
        assert_eq!(
            format_shape(b"fn  f ( ) {\n    let   x = 1\n\n    x\n}\n"),
            Ok(expected.replace("1\n    x", "1\n\n    x"))
        );
    }

    #[test]
    fn supported_output_is_a_fixed_point() {
        let once = format_shape(b"fn f(){let x=1\nx}").expect("supported shape");
        assert_eq!(format_shape(once.as_bytes()), Ok(once));
    }

    #[test]
    fn normalizes_hexadecimal_digits_and_moves_long_rhs() {
        assert_eq!(
            format_shape(b"fn f(){let x=0xff\nx}"),
            Ok("fn f() {\n    let x = 0xFF\n    x\n}\n".to_owned())
        );

        let binding = "x".repeat(70);
        let literal = "340282366920938463463374607431768211455";
        let input = format!("fn f(){{let {binding}={literal}\n{binding}}}");
        let parsed_input = parse_source(input.as_bytes()).expect("long input parses");
        let output = format_shape(input.as_bytes()).expect("supported long binding");
        assert!(output.contains(&format!("let {binding} =\n        {literal}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed_output = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed_output.root())
        );
    }

    #[test]
    fn output_reparses_to_the_same_nontrivia_cst_shape() {
        let input = parse_source(b"fn  f ( ) {\n let x = 0xff\n\n x\n}\n").expect("input parses");
        let output =
            format_shape(b"fn  f ( ) {\n let x = 0xff\n\n x\n}\n").expect("input is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted source parses");

        assert_eq!(syntax_shape(input.root()), syntax_shape(reparsed.root()));
    }

    #[test]
    fn unsupported_shapes_and_comments_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(a: I32) { let x = 1\nx }"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"// header\nfn f(){let x=1\nx}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(){let x=1\nx\nx}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    enum ShapeElement {
        NodeStart(SyntaxKind),
        NodeEnd,
        Token(SyntaxKind, String),
    }

    enum Work {
        Node(SyntaxNode),
        Token(SyntaxToken),
        NodeEnd,
    }

    fn syntax_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        let mut shape = Vec::new();
        let mut pending = vec![Work::Node(root.clone())];
        while let Some(work) = pending.pop() {
            match work {
                Work::Node(node) => {
                    shape.push(ShapeElement::NodeStart(node.kind()));
                    pending.push(Work::NodeEnd);
                    pending.extend(
                        node.children_with_tokens()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .map(|element| match element {
                                SyntaxElement::Node(node) => Work::Node(node),
                                SyntaxElement::Token(token) => Work::Token(token),
                            }),
                    );
                }
                Work::Token(token) => {
                    if !matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::EOF) {
                        let text = if token.kind() == SyntaxKind::INT {
                            super::normalize_integer_literal(token.text())
                        } else {
                            token.text().to_owned()
                        };
                        shape.push(ShapeElement::Token(token.kind(), text));
                    }
                }
                Work::NodeEnd => shape.push(ShapeElement::NodeEnd),
            }
        }
        shape
    }
}
