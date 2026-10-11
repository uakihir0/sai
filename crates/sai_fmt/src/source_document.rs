use sai_syntax::{
    GreenElement, GreenNode, GreenTrivia, SyntaxElement, SyntaxKind, TriviaKind,
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
        || function.where_clause().is_some()
        || function.requires_clauses().next().is_some()
        || function.ensures_clauses().next().is_some()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut clauses = Vec::with_capacity(2);
    if let Some(clause) = function.borrows_clause() {
        clauses.push(borrows_clause_document(source, &clause)?);
    }
    if let Some(clause) = function.effects_clause() {
        clauses.push(effects_clause_document(source, &clause)?);
    }
    let params = function
        .params()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let parameters = parameter_list_document(source, &params)?;
    let return_type = function
        .ret_type()
        .map(|return_type| {
            let ty = return_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            type_document(source, &ty)
        })
        .transpose()?
        .map(|type_doc| Doc::concat([Doc::text(" -> "), type_doc]))
        .unwrap_or_else(|| Doc::text(""));
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
    let head = Doc::concat([Doc::text(format!("fn {name}")), parameters, return_type]);
    let has_clauses = !clauses.is_empty();
    let document = if clauses.is_empty() {
        Doc::concat([head, Doc::text(" "), body])
    } else {
        let mut clause_lines = vec![Doc::HardLine];
        for (index, clause) in clauses.into_iter().enumerate() {
            if index > 0 {
                clause_lines.push(Doc::HardLine);
            }
            clause_lines.push(clause);
        }
        Doc::concat([
            head,
            Doc::indent(Doc::concat(clause_lines)),
            Doc::HardLine,
            body,
        ])
    };
    let range = function.syntax().text_range();
    Ok(FunctionDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline: has_clauses || !statements.is_empty(),
    })
}

fn parameter_list_document(
    source: &ParsedSource,
    params: &ast::ParamList,
) -> Result<Doc, SourceDocumentError> {
    let mut parameters = Vec::new();
    let mut self_params = params.syntax().children().filter_map(ast::SelfParam::cast);
    let self_param = self_params.next();
    if self_params.next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if let Some(self_param) = self_param {
        if params.params().next().is_some_and(|param| {
            self_param.syntax().text_range().start().raw()
                > param.syntax().text_range().start().raw()
        }) {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        parameters.push(self_parameter_document(self_param)?);
    }
    parameters.extend(
        params
            .params()
            .map(|param| parameter_document(source, param))
            .collect::<Result<Vec<_>, _>>()?,
    );
    if parameters.is_empty() {
        return Ok(Doc::text("()"));
    }

    let group = GroupId(params.syntax().text_range().start().raw());
    Ok(delimited_list_document(group, "(", ")", parameters, false))
}

fn parameter_document(
    source: &ParsedSource,
    param: ast::Param,
) -> Result<Doc, SourceDocumentError> {
    if param.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = param.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = param.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let type_doc = type_document(source, &ty)?;
    let mutable = if param.is_mutable() { "var " } else { "" };
    Ok(Doc::concat([
        Doc::text(format!("{mutable}{}: ", node_text(source, name.syntax()))),
        type_doc,
    ]))
}

fn self_parameter_document(param: ast::SelfParam) -> Result<Doc, SourceDocumentError> {
    if param.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let ty = param.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let type_name = match ty {
        ast::Type::SelfType(self_type) if self_type.segments().next().is_none() => {
            "Self".to_owned()
        }
        ast::Type::BorrowType(borrow_type)
            if matches!(
                borrow_type.ty(),
                Some(ast::Type::SelfType(self_type)) if self_type.segments().next().is_none()
            ) =>
        {
            let mutable = if borrow_type.mut_token().is_some() {
                "mut "
            } else {
                ""
            };
            format!("borrow {mutable}Self")
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    let mutable = if param.is_mutable() { "var " } else { "" };
    Ok(Doc::text(format!("{mutable}self: {type_name}")))
}

fn type_document(source: &ParsedSource, ty: &ast::Type) -> Result<Doc, SourceDocumentError> {
    match ty {
        ast::Type::PathType(path) => path_type_document(source, path),
        ast::Type::SelfType(self_type) => self_type_document(source, self_type),
        ast::Type::BorrowType(borrow_type) => {
            if borrow_type.syntax().children().count() != 1 {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let prefix = match direct_token_kinds(borrow_type.syntax()).as_slice() {
                [SyntaxKind::BORROW_KW] => "borrow",
                [SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW] => "borrow mut",
                _ => return Err(SourceDocumentError::UnsupportedShape),
            };
            let inner = borrow_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = borrow_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text(prefix),
                    Doc::indent(Doc::concat([Doc::Line, type_document(source, &inner)?])),
                ]),
            ))
        }
        ast::Type::OwnedType(owned_type) => {
            if owned_type.syntax().children().count() != 1
                || direct_token_kinds(owned_type.syntax()) != [SyntaxKind::OWNED_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let inner = owned_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = owned_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text("owned"),
                    Doc::indent(Doc::concat([Doc::Line, type_document(source, &inner)?])),
                ]),
            ))
        }
        ast::Type::TupleType(tuple_type) => {
            let elements = tuple_type.elements().collect::<Vec<_>>();
            if tuple_type.syntax().children().count() != elements.len() {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            if elements.is_empty() {
                return Ok(Doc::text("()"));
            }
            let documents = elements
                .iter()
                .map(|element| type_document(source, element))
                .collect::<Result<Vec<_>, _>>()?;
            let range = tuple_type.syntax().text_range();
            Ok(delimited_list_document(
                GroupId(range.start().raw()),
                "(",
                ")",
                documents,
                elements.len() == 1,
            ))
        }
        ast::Type::ParenType(paren_type) => {
            if paren_type.syntax().children().count() != 1 {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let inner = paren_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = paren_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text("("),
                    Doc::indent(Doc::concat([Doc::SoftLine, type_document(source, &inner)?])),
                    Doc::SoftLine,
                    Doc::text(")"),
                ]),
            ))
        }
        ast::Type::InferType(infer_type) if infer_type.syntax().children().next().is_none() => {
            Ok(Doc::text("_"))
        }
        ast::Type::FnPtrType(fn_ptr_type) => fn_ptr_type_document(source, fn_ptr_type),
        ast::Type::InferType(_) => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn path_type_document(
    source: &ParsedSource,
    path: &ast::PathType,
) -> Result<Doc, SourceDocumentError> {
    let segments = path.segments().collect::<Vec<_>>();
    if segments.is_empty() || path.syntax().children().count() != segments.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        let name = segment
            .name()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        documents.push(Doc::text(node_text(source, name.syntax())));
        if let Some(args) = segment.generic_args() {
            documents.push(generic_args_document(source, &args)?);
        }
    }
    Ok(Doc::concat(documents))
}

fn self_type_document(
    source: &ParsedSource,
    self_type: &ast::SelfType,
) -> Result<Doc, SourceDocumentError> {
    let segments = self_type.segments().collect::<Vec<_>>();
    if self_type.syntax().children().count() != segments.len()
        || segments
            .iter()
            .any(|segment| segment.generic_args().is_some())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("Self")];
    for segment in segments {
        let name = segment
            .name()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        documents.push(Doc::text("."));
        documents.push(Doc::text(node_text(source, name.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn generic_args_document(
    source: &ParsedSource,
    args: &ast::GenericArgList,
) -> Result<Doc, SourceDocumentError> {
    let children = args.syntax().children().collect::<Vec<_>>();
    if children.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = children
        .into_iter()
        .map(|child| {
            if let Some(type_arg) = ast::TypeArg::cast(child.clone()) {
                if type_arg.syntax().children().count() != 1 {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let ty = type_arg.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
                type_document(source, &ty)
            } else if let Some(const_arg) = ast::ConstArg::cast(child.clone()) {
                const_argument_document(&const_arg)
            } else if let Some(binding) = ast::AssocTypeBinding::cast(child) {
                if binding.syntax().children().count() != 2 {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let name = binding
                    .name()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let ty = binding.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
                Ok(Doc::concat([
                    Doc::text(node_text(source, name.syntax())),
                    Doc::text(" = "),
                    type_document(source, &ty)?,
                ]))
            } else {
                Err(SourceDocumentError::UnsupportedShape)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = args.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "<",
        ">",
        documents,
        false,
    ))
}

fn const_argument_document(arg: &ast::ConstArg) -> Result<Doc, SourceDocumentError> {
    let tokens = arg
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let text = match tokens.as_slice() {
        [integer] if integer.kind() == SyntaxKind::INT => normalize_integer_literal(integer.text()),
        [minus, integer]
            if minus.kind() == SyntaxKind::MINUS && integer.kind() == SyntaxKind::INT =>
        {
            format!("-{}", normalize_integer_literal(integer.text()))
        }
        [character] if character.kind() == SyntaxKind::CHAR => character.text().to_owned(),
        [boolean] if matches!(boolean.kind(), SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW) => {
            boolean.text().to_owned()
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    Ok(Doc::text(text))
}

fn direct_token_kinds(node: &sai_syntax::SyntaxNode) -> Vec<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token.kind()),
            SyntaxElement::Node(_) => None,
        })
        .collect()
}

fn delimited_list_document(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    keep_singleton_comma: bool,
) -> Doc {
    let mut contents = Vec::with_capacity(items.len() * 2 + 2);
    contents.push(Doc::SoftLine);
    for (index, item) in items.iter().cloned().enumerate() {
        if index > 0 {
            contents.push(Doc::text(","));
            contents.push(Doc::Line);
        }
        contents.push(item);
    }
    contents.push(Doc::if_broken(
        group,
        Doc::text(","),
        Doc::text(if keep_singleton_comma && items.len() == 1 {
            ","
        } else {
            ""
        }),
    ));
    Doc::group(
        group,
        Doc::concat([
            Doc::text(open),
            Doc::indent(Doc::concat(contents)),
            Doc::SoftLine,
            Doc::text(close),
        ]),
    )
}

fn fn_ptr_type_document(
    source: &ParsedSource,
    fn_ptr_type: &ast::FnPtrType,
) -> Result<Doc, SourceDocumentError> {
    let params = fn_ptr_type.params().collect::<Vec<_>>();
    let return_type = fn_ptr_type.ret_type();
    let effects = fn_ptr_type.effects();
    if fn_ptr_type.syntax().children().count()
        != params.len() + return_type.is_some() as usize + effects.is_some() as usize
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let token_kinds = direct_token_kinds(fn_ptr_type.syntax());
    let unsafe_prefix = token_kinds.first() == Some(&SyntaxKind::UNSAFE_KW);
    let prefix = if unsafe_prefix {
        &[
            SyntaxKind::UNSAFE_KW,
            SyntaxKind::FN_KW,
            SyntaxKind::L_PAREN,
        ][..]
    } else {
        &[SyntaxKind::FN_KW, SyntaxKind::L_PAREN][..]
    };
    if !token_kinds.starts_with(prefix)
        || token_kinds.iter().any(|kind| {
            !matches!(
                kind,
                SyntaxKind::UNSAFE_KW
                    | SyntaxKind::FN_KW
                    | SyntaxKind::L_PAREN
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::COMMA
                    | SyntaxKind::THIN_ARROW
                    | SyntaxKind::NEWLINE
            )
        })
        || return_type.is_some() != token_kinds.contains(&SyntaxKind::THIN_ARROW)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let params = params
        .iter()
        .map(|param| type_document(source, param))
        .collect::<Result<Vec<_>, _>>()?;
    let range = fn_ptr_type.syntax().text_range();
    let params = if params.is_empty() {
        Doc::text("()")
    } else {
        delimited_list_document(GroupId(range.start().raw()), "(", ")", params, false)
    };
    let mut documents = vec![
        Doc::text(if unsafe_prefix { "unsafe fn" } else { "fn" }),
        params,
    ];
    if let Some(return_type) = return_type {
        documents.push(Doc::text(" -> "));
        documents.push(type_document(source, &return_type)?);
    }
    if let Some(effects) = effects {
        documents.push(Doc::text(" "));
        documents.push(effects_clause_document(source, &effects)?);
    }
    Ok(Doc::concat(documents))
}

fn effects_clause_document(
    source: &ParsedSource,
    clause: &ast::EffectsClause,
) -> Result<Doc, SourceDocumentError> {
    let has_effects_keyword = clause
        .syntax()
        .children_with_tokens()
        .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT && token.text() == "effects"));
    if clause.syntax().children().count() != 1
        || direct_token_kinds(clause.syntax()) != [SyntaxKind::LIDENT]
        || !has_effects_keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let set = clause.set().ok_or(SourceDocumentError::UnsupportedShape)?;
    effect_set_document(source, &set).map(|set| Doc::concat([Doc::text("effects "), set]))
}

fn borrows_clause_document(
    source: &ParsedSource,
    clause: &ast::BorrowsClause,
) -> Result<Doc, SourceDocumentError> {
    let has_borrows_keyword = clause
        .syntax()
        .children_with_tokens()
        .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT && token.text() == "borrows"));
    let provenances = clause.provenances().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(clause.syntax());
    if clause.syntax().children().count() != provenances.len()
        || token_kinds.first() != Some(&SyntaxKind::LIDENT)
        || token_kinds.get(1) != Some(&SyntaxKind::L_BRACE)
        || token_kinds.last() != Some(&SyntaxKind::R_BRACE)
        || token_kinds[2..token_kinds.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::COMMA | SyntaxKind::NEWLINE))
        || !has_borrows_keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = provenances
        .iter()
        .map(|provenance| provenance_document(source, provenance))
        .collect::<Result<Vec<_>, _>>()?;
    let range = clause.syntax().text_range();
    Ok(Doc::concat([
        Doc::text("borrows "),
        brace_list_document(GroupId(range.start().raw()), documents),
    ]))
}

fn provenance_document(
    source: &ParsedSource,
    provenance: &ast::Provenance,
) -> Result<Doc, SourceDocumentError> {
    let segments = provenance.segments().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(provenance.syntax());
    if segments.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let expected_tokens =
        std::iter::repeat_n(SyntaxKind::DOT, segments.len() - 1).collect::<Vec<_>>();
    if token_kinds != expected_tokens {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn effect_set_document(
    source: &ParsedSource,
    set: &ast::EffectSet,
) -> Result<Doc, SourceDocumentError> {
    let effects = set.effects().collect::<Vec<_>>();
    if set.syntax().children().count() != effects.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if effects.is_empty() {
        return Ok(Doc::text("{}"));
    }
    let documents = effects
        .iter()
        .map(|effect| effect_ref_document(source, effect))
        .collect::<Result<Vec<_>, _>>()?;
    let range = set.syntax().text_range();
    Ok(brace_list_document(GroupId(range.start().raw()), documents))
}

fn brace_list_document(group: GroupId, items: Vec<Doc>) -> Doc {
    if items.is_empty() {
        return Doc::text("{}");
    }
    let mut contents = Vec::with_capacity(items.len() * 2 + 1);
    contents.push(Doc::Line);
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            contents.push(Doc::if_broken(group, Doc::text(""), Doc::text(",")));
            contents.push(Doc::Line);
        }
        contents.push(item);
    }
    Doc::group(
        group,
        Doc::concat([
            Doc::text("{"),
            Doc::indent(Doc::concat(contents)),
            Doc::Line,
            Doc::text("}"),
        ]),
    )
}

fn effect_ref_document(
    source: &ParsedSource,
    effect: &ast::EffectRef,
) -> Result<Doc, SourceDocumentError> {
    let segments = effect.segments().collect::<Vec<_>>();
    if segments.is_empty() || effect.syntax().children().count() != segments.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
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
    use sai_syntax::{
        SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken,
        ast::{AstNode, Type},
    };

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
    fn formats_var_parameters_and_simple_return_types() {
        let input = b"fn f(var value:I32, _:U32)\n->Bool {}";
        let expected = "fn f(var value: I32, _: U32) -> Bool {}\n";
        let parsed_input = parse_source(input).expect("input parses");
        let output = format_shape(input).expect("simple signature is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted signature parses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
        assert_eq!(
            format_shape(b"fn f(var _:I32){}"),
            Ok("fn f(var _: I32) {}\n".to_owned())
        );
    }

    #[test]
    fn formats_compound_parameter_and_return_types() {
        for (input, expected) in [
            (
                b"fn f(value:http.client.Response<Vec<I32>>) -> Map<Str, User> {}".as_slice(),
                "fn f(value: http.client.Response<Vec<I32>>) -> Map<Str, User> {}\n",
            ),
            (
                b"fn f(value:borrow mut Map<Str,(I32,)>) -> (I32, Str) {}",
                "fn f(value: borrow mut Map<Str, (I32,)>) -> (I32, Str) {}\n",
            ),
            (
                b"fn f(value:owned Self.Item) -> owned Vec<User> {}",
                "fn f(value: owned Self.Item) -> owned Vec<User> {}\n",
            ),
            (
                b"fn f(value: (Vec<I32>)) -> () {}",
                "fn f(value: (Vec<I32>)) -> () {}\n",
            ),
            (b"fn f(value: _) {}", "fn f(value: _) {}\n"),
            (b"fn f(value: Vec<I32,>) {}", "fn f(value: Vec<I32>) {}\n"),
            (
                b"fn f(value:Array<I32,0xff>, other:Pair<' ',true,-0xff>) {}",
                "fn f(value: Array<I32, 0xFF>, other: Pair<' ', true, -0xFF>) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("compound types are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_generic_type_arguments_as_a_delimited_list() {
        let first = "FirstType".repeat(6);
        let second = "SecondType".repeat(6);
        let input = format!("fn f(value:Container<{first},{second}>){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let expected = format!(
            "fn f(\n    value: Container<\n        {first},\n        {second},\n    >,\n) {{}}\n"
        );
        let output = format_shape(input.as_bytes()).expect("long type is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let return_input = format!("fn f(value:I32)->Container<{first},{second}>{{}}");
        let return_output =
            format_shape(return_input.as_bytes()).expect("compound return type is supported");
        assert!(return_output.contains(") -> Container<\n"));
        assert!(return_output.contains(&format!("    {first},\n")));
        assert!(
            return_output
                .lines()
                .all(|line| line.chars().count() <= 100)
        );
        let reparsed = parse_source(return_output.as_bytes()).expect("wrapped return type parses");
        let parsed_return = parse_source(return_input.as_bytes()).expect("input parses");
        assert_eq!(
            syntax_shape(parsed_return.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(return_output.as_bytes()), Ok(return_output));
    }

    #[test]
    fn breaks_long_borrow_owned_and_grouped_types_safely() {
        let long_type = "A".repeat(86);
        for (input, expected) in [
            (
                format!("fn f()->borrow {long_type}{{}}"),
                format!("fn f() -> borrow\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->borrow mut {long_type}{{}}"),
                format!("fn f() -> borrow mut\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->owned {long_type}{{}}"),
                format!("fn f() -> owned\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->({long_type}){{}}"),
                format!("fn f() -> (\n    {long_type}\n) {{}}\n"),
            ),
        ] {
            let parsed = parse_source(input.as_bytes()).expect("input parses");
            let output = format_shape(input.as_bytes()).expect("wrapped type is supported");
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_function_pointer_types_and_effect_sets() {
        for (input, expected) in [
            (
                b"fn f(cb:fn(I32,borrow Vec<User>)->Bool effects {alloc,net.http}){}".as_slice(),
                "fn f(cb: fn(I32, borrow Vec<User>) -> Bool effects { alloc, net.http }) {}\n",
            ),
            (
                b"fn f(cb:unsafe fn(I32)->Str effects {net.http,alloc}){}",
                "fn f(cb: unsafe fn(I32) -> Str effects { net.http, alloc }) {}\n",
            ),
            (
                b"fn f(cb:fn()effects{}){}",
                "fn f(cb: fn() effects {}) {}\n",
            ),
            (
                b"fn f(cb:fn(I32)\n->Bool effects {alloc}){}",
                "fn f(cb: fn(I32) -> Bool effects { alloc }) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_function_pointer_parameters_and_effect_sets() {
        let first_type = format!("T{}", "A".repeat(60));
        let second_type = format!("U{}", "B".repeat(60));
        let first_effect = format!("network.{}", "a".repeat(40));
        let second_effect = format!("memory.{}", "b".repeat(40));
        let input = format!(
            "fn f(callback:fn({first_type},{second_type}) effects {{{first_effect},{second_effect}}}){{}}"
        );
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long function pointer is supported");
        assert!(output.contains("callback: fn(\n"));
        assert!(output.contains(&format!("        {first_type},\n")));
        assert!(output.contains(") effects {\n"));
        assert!(output.contains(&format!("        {first_effect}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_borrows_and_function_effect_clauses() {
        for (input, expected) in [
            (
                b"fn f()effects{io,net.http}{}".as_slice(),
                "fn f()\n    effects { io, net.http }\n{}\n",
            ),
            (
                b"fn f()borrows{self,user.name}effects{alloc}{}",
                "fn f()\n    borrows { self, user.name }\n    effects { alloc }\n{}\n",
            ),
            (b"fn f()borrows{}{}", "fn f()\n    borrows {}\n{}\n"),
            (
                b"fn f()effects{io}{let x=1\nx}",
                "fn f()\n    effects { io }\n{\n    let x = 1\n    x\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn reorders_function_borrows_and_effect_clauses() {
        let input = b"fn f() effects { io } borrows { self } {}";
        let expected = "fn f()\n    borrows { self }\n    effects { io }\n{}\n";
        parse_source(input).expect("input parses with a clause-order diagnostic");
        assert_eq!(format_shape(input), Ok(expected.to_owned()));
        parse_source(expected.as_bytes()).expect("canonical output parses");
        assert_eq!(format_shape(expected.as_bytes()), Ok(expected.to_owned()));
    }

    #[test]
    fn breaks_long_borrows_provenances_without_commas() {
        let first = format!("resource.{}", "a".repeat(45));
        let second = format!("storage.{}", "b".repeat(45));
        let input = format!("fn f() borrows {{{first},{second}}} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long borrows clause is supported");
        assert!(output.contains("    borrows {\n"));
        assert!(output.contains(&format!("        {first}\n")));
        assert!(output.contains(&format!("        {second}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_supported_self_parameter_types() {
        for (input, expected) in [
            (b"fn f(self:Self){}".as_slice(), "fn f(self: Self) {}\n"),
            (b"fn f(var self:Self){}", "fn f(var self: Self) {}\n"),
            (
                b"fn f(self:borrow mut Self){}",
                "fn f(self: borrow mut Self) {}\n",
            ),
            (
                b"fn f(var self:borrow Self){}",
                "fn f(var self: borrow Self) {}\n",
            ),
            (
                b"fn f(var self:borrow mut Self,x:I32){}",
                "fn f(var self: borrow mut Self, x: I32) {}\n",
            ),
            (
                b"fn f(self:borrow   Self){}",
                "fn f(self: borrow Self) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("receiver form is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let first = "a".repeat(39);
        let second = "b".repeat(39);
        let without_receiver = format!("fn f({first}:I32,{second}:U32){{}}");
        let flat = format_shape(without_receiver.as_bytes()).expect("signature is supported");
        assert_eq!(flat, format!("fn f({first}: I32, {second}: U32) {{}}\n"));
        assert!(flat.lines().all(|line| line.chars().count() <= 100));

        let input = format!("fn f(self:Self,{first}:I32,{second}:U32){{}}");
        let expected =
            format!("fn f(\n    self: Self,\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        assert_eq!(format_shape(input.as_bytes()), Ok(expected));
    }

    #[test]
    fn return_type_width_can_break_a_parameter_list() {
        let first = "a".repeat(39);
        let second = "b".repeat(39);
        let no_return_type = format!("fn f({first}:I32,{second}:U32){{}}");
        let flat = format_shape(no_return_type.as_bytes()).expect("flat signature is supported");
        assert_eq!(flat, format!("fn f({first}: I32, {second}: U32) {{}}\n"));
        assert!(flat.lines().all(|line| line.chars().count() <= 100));

        let input = format!("fn f({first}:I32,{second}:U32)->Bool{{}}");
        let expected = format!("fn f(\n    {first}: I32,\n    {second}: U32,\n) -> Bool {{}}\n");
        assert_eq!(format_shape(input.as_bytes()), Ok(expected));
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
            format_shape(b"fn f() requires true {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f() ensures true {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f() where T: Clone {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f<T>() {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Foo) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self.Item) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: borrow Self.Item) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(x: I32, self: Self) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self, self: Self) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self, x: I32, self: Self) {}"),
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
                    if matches!(
                        node.kind(),
                        SyntaxKind::EFFECT_SET | SyntaxKind::BORROWS_CLAUSE
                    ) {
                        elements.retain(
                            |element| !matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA),
                        );
                    } else {
                        let trailing_comma_close = match node.kind() {
                            SyntaxKind::PARAM_LIST | SyntaxKind::FN_PTR_TYPE => {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::GENERIC_ARG_LIST => Some(SyntaxKind::GT),
                            SyntaxKind::TUPLE_TYPE
                                if node.children().filter_map(Type::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            _ => None,
                        };
                        if let Some(close) = trailing_comma_close {
                            let trailing_comma = elements.windows(2).position(|pair| {
                                matches!(&pair[0], SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA)
                                    && matches!(&pair[1], SyntaxElement::Token(token) if token.kind() == close)
                            });
                            if let Some(index) = trailing_comma {
                                elements.remove(index);
                            }
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
