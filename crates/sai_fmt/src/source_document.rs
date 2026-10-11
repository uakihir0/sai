use sai_syntax::{
    GreenElement, GreenNode, GreenTrivia, TriviaKind,
    ast::{self, AstNode, AstToken, Expr, Item, LiteralToken, Pattern, Stmt},
};

use crate::{Doc, GroupId, ParsedSource};

/// Syntax shapes that the source document builder does not support yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceDocumentError {
    UnsupportedShape,
}

/// Build a document for the deliberately small source-formatting foundation.
///
/// The builder is private and fail-closed while source-formatting coverage is
/// being implemented. It accepts files made only of unannotated functions
/// with simple parameters and empty bodies or one single-line literal `let`
/// followed by a final name expression.
pub(super) fn source_document(source: &ParsedSource) -> Result<Doc, SourceDocumentError> {
    if has_comment_trivia(source.root().green()) {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let file = ast::SourceFile::cast(source.root().clone())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if file.module_header().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let items = file.items().collect::<Vec<_>>();
    if source.root().children().count() != items.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut functions = Vec::with_capacity(items.len());
    for item in items {
        let Item::Function(function) = item else {
            return Err(SourceDocumentError::UnsupportedShape);
        };
        functions.push(function_document(source, &function)?);
    }
    if functions.is_empty() {
        return Ok(Doc::concat([]));
    }

    let mut document = Vec::with_capacity(functions.len() * 2 - 1);
    for (index, function) in functions.iter().enumerate() {
        if index > 0 {
            let previous = &functions[index - 1];
            let gap = source
                .text()
                .get(previous.end..function.start)
                .unwrap_or_default();
            let preserve_blank_line = gap.matches('\n').count() > 1;
            let separate_with_blank_line =
                previous.multiline || function.multiline || preserve_blank_line;
            document.push(Doc::HardLine);
            if separate_with_blank_line {
                document.push(Doc::HardLine);
            }
        }
        document.push(function.document.clone());
    }
    Ok(Doc::concat(document))
}

fn has_comment_trivia(root: &GreenNode) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        for child in node.children() {
            match child {
                GreenElement::Node(child) => pending.push(child),
                GreenElement::Token(token) => {
                    if token
                        .leading_trivia()
                        .iter()
                        .chain(token.trailing_trivia())
                        .any(is_comment_trivia)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_comment_trivia(trivia: &GreenTrivia) -> bool {
    matches!(
        trivia.kind(),
        TriviaKind::LineComment | TriviaKind::BlockComment | TriviaKind::Shebang
    )
}

struct FunctionDocument {
    document: Doc,
    start: usize,
    end: usize,
    multiline: bool,
}

fn function_document(
    source: &ParsedSource,
    function: &ast::FnDecl,
) -> Result<FunctionDocument, SourceDocumentError> {
    if function.docs().is_some()
        || function.attrs().next().is_some()
        || function.visibility().is_some()
        || function.unsafe_token().is_some()
        || function.async_token().is_some()
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
    let parameters = parameter_list_document(source, &params)?;
    let block = function
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let name = function
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let name = node_text(source, name.syntax());
    let statements = block.statements().collect::<Vec<_>>();
    let body = match statements.as_slice() {
        [] => Doc::text("{}"),
        [Stmt::Let(binding_stmt), Stmt::Expr(tail_stmt)] => {
            let pattern = binding_stmt
                .pattern()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let Pattern::Binding(pattern) = pattern else {
                return Err(SourceDocumentError::UnsupportedShape);
            };
            if pattern.mode().is_some()
                || binding_stmt.ty().is_some()
                || binding_stmt.else_clause().is_some()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = pattern
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let initializer = binding_stmt
                .initializer()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let Expr::Literal(initializer) = initializer else {
                return Err(SourceDocumentError::UnsupportedShape);
            };
            let literal = initializer
                .token()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let literal = normalize_literal(source, literal)?;
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
            let name_text = node_text(source, name.syntax());
            let tail_text = node_text(source, tail_name.syntax());
            let statement_separator = source
                .text()
                .get(
                    binding_stmt.syntax().text_range().end().to_usize()
                        ..tail_stmt.syntax().text_range().start().to_usize(),
                )
                .unwrap_or_default();
            let separator = if statement_separator.matches('\n').count() > 1 {
                Doc::concat([Doc::HardLine, Doc::HardLine])
            } else {
                Doc::HardLine
            };
            let rhs = Doc::choice([
                Doc::concat([Doc::text(" "), Doc::flat(Doc::text(literal.clone()))]),
                Doc::indent(Doc::concat([
                    Doc::HardLine,
                    Doc::flat(Doc::text(literal.clone())),
                ])),
                Doc::concat([Doc::text(" "), Doc::text(literal)]),
            ]);
            Doc::concat([
                Doc::text("{"),
                Doc::indent(Doc::concat([
                    Doc::HardLine,
                    Doc::text(format!("let {name_text} =")),
                    rhs,
                    separator,
                    Doc::text(tail_text),
                ])),
                Doc::HardLine,
                Doc::text("}"),
            ])
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    let document = Doc::concat([
        Doc::text(format!("fn {name}")),
        parameters,
        Doc::text(" "),
        body,
    ]);
    let range = function.syntax().text_range();
    Ok(FunctionDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline: !statements.is_empty(),
    })
}

fn parameter_list_document(
    source: &ParsedSource,
    params: &ast::ParamList,
) -> Result<Doc, SourceDocumentError> {
    if params.self_param().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let parameters = params
        .params()
        .map(|param| parameter_document(source, param))
        .collect::<Result<Vec<_>, _>>()?;
    if parameters.is_empty() {
        return Ok(Doc::text("()"));
    }

    let group = GroupId(params.syntax().text_range().start().raw());
    let mut contents = Vec::with_capacity(parameters.len() * 2 + 2);
    contents.push(Doc::SoftLine);
    for (index, parameter) in parameters.into_iter().enumerate() {
        if index > 0 {
            contents.push(Doc::text(","));
            contents.push(Doc::Line);
        }
        contents.push(parameter);
    }
    contents.push(Doc::if_broken(group, Doc::text(","), Doc::text("")));

    Ok(Doc::group(
        group,
        Doc::concat([
            Doc::text("("),
            Doc::indent(Doc::concat(contents)),
            Doc::SoftLine,
            Doc::text(")"),
        ]),
    ))
}

fn parameter_document(
    source: &ParsedSource,
    param: ast::Param,
) -> Result<Doc, SourceDocumentError> {
    if param.is_mutable() || param.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = param.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = param.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ast::Type::PathType(path) = ty else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    let segments = path.segments().collect::<Vec<_>>();
    let [segment] = segments.as_slice() else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if segment.generic_args().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let type_name = segment
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    Ok(Doc::text(format!(
        "{}: {}",
        node_text(source, name.syntax()),
        node_text(source, type_name.syntax())
    )))
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

fn normalize_literal(
    source: &ParsedSource,
    literal: LiteralToken,
) -> Result<String, SourceDocumentError> {
    let token = literal.syntax();
    let range = token.text_range();
    let text = source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if text.contains(['\r', '\n']) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(match literal {
        LiteralToken::Int(_) => normalize_integer_literal(text),
        LiteralToken::Float(_) => text.replace('E', "e"),
        LiteralToken::String(_)
        | LiteralToken::ByteString(_)
        | LiteralToken::Char(_)
        | LiteralToken::Byte(_)
        | LiteralToken::True(_)
        | LiteralToken::False(_) => text.to_owned(),
    })
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
    fn formats_simple_parameter_lists_and_breaks_them_at_the_width_limit() {
        assert_eq!(
            format_shape(b"fn f(x:I32){}"),
            Ok("fn f(x: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f( x : I32 , y:U32 ) {}"),
            Ok("fn f(x: I32, y: U32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(_:I32){}"),
            Ok("fn f(_: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(x:I32,){}"),
            Ok("fn f(x: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(\n x:I32,\n y:U32\n){}"),
            Ok("fn f(x: I32, y: U32) {}\n".to_owned())
        );

        let first = "a".repeat(46);
        let second = "b".repeat(46);
        let input = format!("fn f({first}:I32,{second}:U32){{}}");
        let parsed_input = parse_source(input.as_bytes()).expect("long input parses");
        let expected = format!("fn f(\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        let output = format_shape(input.as_bytes()).expect("simple parameters are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped parameter list reparses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn supported_output_is_a_fixed_point() {
        let once = format_shape(b"fn f(){let x=1\nx}").expect("supported shape");
        assert_eq!(format_shape(once.as_bytes()), Ok(once));
    }

    #[test]
    fn formats_empty_functions_and_preserves_top_level_blank_lines() {
        assert_eq!(format_shape(b""), Ok(String::new()));
        assert_eq!(format_shape(b"fn f(){}"), Ok("fn f() {}\n".to_owned()));
        assert_eq!(
            format_shape(b"fn f(){}\nfn g(){}"),
            Ok("fn f() {}\nfn g() {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){}\n\nfn g(){}"),
            Ok("fn f() {}\n\nfn g() {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=1\nx}\nfn g(){}"),
            Ok("fn f() {\n    let x = 1\n    x\n}\n\nfn g() {}\n".to_owned())
        );

        let input = parse_source(b"fn f(){let x=1\nx}\nfn g(){}").expect("input parses");
        let output = format_shape(b"fn f(){let x=1\nx}\nfn g(){}").expect("supported file");
        let reparsed = parse_source(output.as_bytes()).expect("formatted file parses");
        assert_eq!(syntax_shape(input.root()), syntax_shape(reparsed.root()));
    }

    #[test]
    fn normalizes_float_literals_and_formats_other_simple_literal_kinds() {
        assert_eq!(
            format_shape(b"fn f(){let x=1.5E-3\nx}"),
            Ok("fn f() {\n    let x = 1.5e-3\n    x\n}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=true\nx}"),
            Ok("fn f() {\n    let x = true\n    x\n}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=\"value\"\nx}"),
            Ok("fn f() {\n    let x = \"value\"\n    x\n}\n".to_owned())
        );

        for source in [
            b"fn f(){}".as_slice(),
            b"fn f(){}\n\nfn g(){}",
            b"fn f(){let x=false\nx}",
            b"fn f(){let x=1.5E-3\nx}",
        ] {
            let once = format_shape(source).expect("source has a supported shape");
            assert_eq!(format_shape(once.as_bytes()), Ok(once));
        }
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
            format_shape(b"fn f(var a: I32) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(a: Vec<I32>) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"// header\nfn f(){let x=1\nx}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(){} // trailing"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(){} /* trailing */"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(){let x=1\nx\nx}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(){}\n/// trailing"),
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
                    let mut elements = node.children_with_tokens().collect::<Vec<_>>();
                    if node.kind() == SyntaxKind::PARAM_LIST {
                        let trailing_comma = elements.windows(2).position(|pair| {
                            matches!(&pair[0], SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA)
                                && matches!(&pair[1], SyntaxElement::Token(token) if token.kind() == SyntaxKind::R_PAREN)
                        });
                        if let Some(index) = trailing_comma {
                            elements.remove(index);
                        }
                    }
                    pending.extend(elements.into_iter().rev().map(|element| match element {
                        SyntaxElement::Node(node) => Work::Node(node),
                        SyntaxElement::Token(token) => Work::Token(token),
                    }));
                }
                Work::Token(token) => {
                    if !matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::EOF) {
                        let text = match token.kind() {
                            SyntaxKind::INT => super::normalize_integer_literal(token.text()),
                            SyntaxKind::FLOAT => token.text().replace('E', "e"),
                            _ => token.text().to_owned(),
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
