use sai_syntax::{
    GreenElement, GreenNode, GreenTrivia, SyntaxElement, SyntaxKind, SyntaxToken, TriviaKind,
    ast::{self, AstNode, AstToken, Expr, Item, LiteralToken, Pattern, Stmt},
};

use crate::{Doc, GroupId, ParsedSource, printer::MAX_LINE_WIDTH};

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
    let mut expression_group_ids = ExpressionGroupIds::default();
    for item in items {
        let Item::Function(function) = item else {
            return Err(SourceDocumentError::UnsupportedShape);
        };
        functions.push(function_document(
            source,
            &function,
            &mut expression_group_ids,
        )?);
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

#[derive(Clone, Copy)]
enum ExpressionContext {
    Unprotected,
    Protected,
    ParenthesizedWhole,
}

#[derive(Default)]
struct ExpressionGroupIds {
    next_id: u32,
}

impl ExpressionGroupIds {
    fn next(&mut self) -> GroupId {
        // Source-offset groups stay below the 16 MiB input limit; expression
        // groups use this disjoint namespace and a per-document sequence.
        const EXPRESSION_GROUP_START: u32 = 1 << 31;
        debug_assert!(self.next_id < EXPRESSION_GROUP_START);
        let group = GroupId(EXPRESSION_GROUP_START | self.next_id);
        self.next_id += 1;
        group
    }
}

fn function_document(
    source: &ParsedSource,
    function: &ast::FnDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<FunctionDocument, SourceDocumentError> {
    if function.docs().is_some()
        || function.attrs().next().is_some()
        || function.visibility().is_some()
        || function.unsafe_token().is_some()
        || function.async_token().is_some()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut clauses = Vec::with_capacity(7);
    if let Some(clause) = function.where_clause() {
        clauses.push(where_clause_document(source, &clause)?);
    }
    if let Some(clause) = function.borrows_clause() {
        clauses.push(borrows_clause_document(source, &clause)?);
    }
    if let Some(clause) = function.effects_clause() {
        clauses.push(effects_clause_document(source, &clause)?);
    }
    clauses.extend(
        function
            .requires_clauses()
            .map(|clause| requires_clause_document(source, &clause, expression_group_ids))
            .collect::<Result<Vec<_>, _>>()?,
    );
    clauses.extend(
        function
            .ensures_clauses()
            .map(|clause| ensures_clause_document(source, &clause, expression_group_ids))
            .collect::<Result<Vec<_>, _>>()?,
    );
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
    let generic_params = function
        .generic_params()
        .map(|params| {
            let document = generic_params_document(source, &params)?;
            let prefix_width = flat_document_width(&document)
                .and_then(|generic_width| {
                    "fn "
                        .chars()
                        .count()
                        .checked_add(name.chars().count())?
                        .checked_add(generic_width)?
                        .checked_add(1)
                })
                .unwrap_or(usize::MAX);
            Ok(if prefix_width <= MAX_LINE_WIDTH {
                Doc::flat(document)
            } else {
                document
            })
        })
        .transpose()?
        .unwrap_or_else(|| Doc::text(""));
    let statements = block.statements().collect::<Vec<_>>();
    let body = block_document(source, &block, expression_group_ids)?;
    let head = Doc::concat([
        Doc::text(format!("fn {name}")),
        generic_params,
        parameters,
        return_type,
    ]);
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

fn block_document(
    source: &ParsedSource,
    block: &ast::Block,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let statements = block.statements().collect::<Vec<_>>();
    if block.syntax().children().count() != statements.len()
        || direct_token_kinds(block.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::L_BRACE, SyntaxKind::R_BRACE]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if statements.is_empty() {
        return Ok(Doc::text("{}"));
    }

    let mut body = vec![Doc::HardLine];
    for (index, statement) in statements.iter().enumerate() {
        if index > 0 {
            let previous_end = statements[index - 1].syntax().text_range().end().to_usize();
            let next_start = statement.syntax().text_range().start().to_usize();
            let separator = source
                .text()
                .get(previous_end..next_start)
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            body.push(Doc::HardLine);
            if separator.matches('\n').count() > 1 {
                body.push(Doc::HardLine);
            }
        }
        body.push(statement_document(source, statement, expression_group_ids)?);
    }
    Ok(Doc::concat([
        Doc::text("{"),
        Doc::indent(Doc::concat(body)),
        Doc::HardLine,
        Doc::text("}"),
    ]))
}

fn block_expression_document(
    source: &ParsedSource,
    block_expr: &ast::BlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let block = block_expr
        .block()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if block_expr.syntax().children().count() != 1
        || !direct_token_kinds(block_expr.syntax()).is_empty()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    block_document(source, &block, expression_group_ids)
}

fn if_expression_document(
    source: &ParsedSource,
    if_expr: &ast::IfExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let condition = if_expr
        .condition()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let then_branch = if_expr
        .then_branch()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let else_branch = if_expr.else_branch();
    let if_tokens = direct_token_kinds(if_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if if_expr.syntax().children().count() != 2 + usize::from(else_branch.is_some())
        || if_tokens != [SyntaxKind::IF_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![
        Doc::text("if "),
        condition_expression_document(source, &condition, expression_group_ids)?,
        Doc::text(" "),
        block_document(source, &then_branch, expression_group_ids)?,
    ];
    if let Some(else_branch) = else_branch {
        if else_branch.syntax().children().count() != 1
            || direct_token_kinds(else_branch.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>()
                != [SyntaxKind::ELSE_KW]
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let block = else_branch.block();
        let else_if = else_branch.else_if();
        if block.is_some() == else_if.is_some() {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.push(Doc::text(" else "));
        if let Some(block) = block {
            documents.push(block_document(source, &block, expression_group_ids)?);
        } else if let Some(else_if) = else_if {
            documents.push(if_expression_document(
                source,
                &else_if,
                expression_group_ids,
            )?);
        }
    }
    Ok(Doc::concat(documents))
}

fn condition_expression_document(
    source: &ParsedSource,
    condition: &ast::Condition,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let expression = condition
        .expression()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if condition.let_condition().is_some()
        || condition.syntax().children().count() != 1
        || !direct_token_kinds(condition.syntax()).is_empty()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    expression_document(
        source,
        &expression,
        ExpressionContext::Unprotected,
        expression_group_ids,
    )
}

fn loop_label_document(label: &ast::LoopLabel) -> Result<Doc, SourceDocumentError> {
    let token = label.label().ok_or(SourceDocumentError::UnsupportedShape)?;
    let label_tokens = direct_token_kinds(label.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if label.syntax().children().count() != 0
        || label_tokens != [SyntaxKind::LABEL, SyntaxKind::COLON]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(token.syntax().text().to_string()),
        Doc::text(": "),
    ]))
}

fn loop_expression_document(
    source: &ParsedSource,
    loop_expr: &ast::LoopExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let label = loop_expr.label();
    let body = loop_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let loop_tokens = direct_token_kinds(loop_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if loop_expr.syntax().children().count() != 1 + usize::from(label.is_some())
        || loop_tokens != [SyntaxKind::LOOP_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(3);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.extend([
        Doc::text("loop "),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn while_expression_document(
    source: &ParsedSource,
    while_expr: &ast::WhileExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let label = while_expr.label();
    let condition = while_expr
        .condition()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = while_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let while_tokens = direct_token_kinds(while_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if while_expr.syntax().children().count() != 2 + usize::from(label.is_some())
        || while_tokens != [SyntaxKind::WHILE_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(5);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.extend([
        Doc::text("while "),
        condition_expression_document(source, &condition, expression_group_ids)?,
        Doc::text(" "),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn for_pattern_document(
    source: &ParsedSource,
    pattern: &ast::Pattern,
) -> Result<Doc, SourceDocumentError> {
    match pattern {
        ast::Pattern::Binding(binding) => {
            let mode = binding.mode();
            let name = binding
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let mut documents = Vec::with_capacity(2);
            if binding.syntax().children().count() != 1 + usize::from(mode.is_some())
                || !direct_token_kinds(binding.syntax()).is_empty()
                || name.syntax().children().count() != 0
                || direct_token_kinds(name.syntax()) != [SyntaxKind::LIDENT]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            if let Some(mode) = mode {
                let mode_tokens = direct_token_kinds(mode.syntax());
                let mode_text = match mode_tokens.as_slice() {
                    [SyntaxKind::VAR_KW] => "var ",
                    [SyntaxKind::BORROW_KW] => "borrow ",
                    [SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW] => "borrow mut ",
                    _ => return Err(SourceDocumentError::UnsupportedShape),
                };
                if mode.syntax().children().count() != 0 {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                documents.push(Doc::text(mode_text));
            }
            documents.push(Doc::text(node_text(source, name.syntax())));
            Ok(Doc::concat(documents))
        }
        ast::Pattern::Wildcard(wildcard) => {
            if wildcard.syntax().children().count() != 0
                || direct_token_kinds(wildcard.syntax()) != [SyntaxKind::UNDERSCORE]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text("_"))
        }
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn for_expression_document(
    source: &ParsedSource,
    for_expr: &ast::ForExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let label = for_expr.label();
    let pattern = for_expr
        .pattern()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let iterable = for_expr
        .iterable()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = for_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let for_tokens = direct_token_kinds(for_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let is_await = for_tokens.contains(&SyntaxKind::AWAIT_KW);
    let mut expected_tokens = vec![SyntaxKind::FOR_KW];
    if is_await {
        expected_tokens.push(SyntaxKind::AWAIT_KW);
    }
    expected_tokens.push(SyntaxKind::IN_KW);
    if for_expr.syntax().children().count() != 3 + usize::from(label.is_some())
        || for_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(6);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.push(Doc::text(if is_await { "for await " } else { "for " }));
    documents.push(for_pattern_document(source, &pattern)?);
    documents.push(Doc::text(" in "));
    documents.push(expression_document(
        source,
        &iterable,
        ExpressionContext::Unprotected,
        expression_group_ids,
    )?);
    documents.push(Doc::text(" "));
    documents.push(block_document(source, &body, expression_group_ids)?);
    Ok(Doc::concat(documents))
}

fn async_block_expression_document(
    source: &ParsedSource,
    async_block: &ast::AsyncBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let body = async_block
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(async_block.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let move_prefix = match tokens.as_slice() {
        [SyntaxKind::ASYNC_KW] => "",
        [SyntaxKind::ASYNC_KW, SyntaxKind::MOVE_KW] => "move ",
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    if async_block.syntax().children().count() != 1 {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("async "),
        Doc::text(move_prefix),
        block_document(source, &body, expression_group_ids)?,
    ]))
}

fn region_expression_document(
    source: &ParsedSource,
    region_expr: &ast::RegionExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let name = region_expr
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = region_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let prefix_token = region_expr
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if region_expr.syntax().children().count() != 2
        || direct_token_kinds(region_expr.syntax()) != [SyntaxKind::LIDENT]
        || prefix_token.text() != "region"
        || name.syntax().children().count() != 0
        || direct_token_kinds(name.syntax()) != [SyntaxKind::LIDENT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("region "),
        Doc::text(node_text(source, name.syntax())),
        Doc::text(" "),
        block_document(source, &body, expression_group_ids)?,
    ]))
}

fn first_choice_layout(document: &Doc) -> Doc {
    match document {
        Doc::Text(text) => Doc::text(text.clone()),
        Doc::Line => Doc::Line,
        Doc::SoftLine => Doc::SoftLine,
        Doc::HardLine => Doc::HardLine,
        Doc::Indent(child) => Doc::indent(first_choice_layout(child)),
        Doc::Group(id, child) => Doc::group(*id, first_choice_layout(child)),
        Doc::IfBroken(id, broken, flat) => {
            Doc::if_broken(*id, first_choice_layout(broken), first_choice_layout(flat))
        }
        Doc::LineSuffix(child) => Doc::line_suffix(first_choice_layout(child)),
        Doc::Choice(alternatives) => alternatives
            .first()
            .map(first_choice_layout)
            .unwrap_or_else(|| Doc::choice([])),
        Doc::Flat(child) => Doc::flat(first_choice_layout(child)),
        Doc::Verbatim(text) => Doc::verbatim(text.clone()),
        Doc::Concat(documents) => Doc::concat(documents.iter().map(first_choice_layout)),
    }
}

fn unsafe_block_expression_document(
    source: &ParsedSource,
    unsafe_block: &ast::UnsafeBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let body = unsafe_block
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if unsafe_block.syntax().children().count() != 1
        || direct_token_kinds(unsafe_block.syntax()) != [SyntaxKind::UNSAFE_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let statements = body.statements().collect::<Vec<_>>();
    let broken_block = block_document(source, &body, expression_group_ids)?;
    if let [Stmt::Expr(statement)] = statements.as_slice() {
        let flat_statement =
            statement_document(source, &Stmt::Expr(statement.clone()), expression_group_ids)?;
        let flat_statement = first_choice_layout(&flat_statement);
        return Ok(Doc::choice([
            Doc::concat([
                Doc::text("unsafe { "),
                Doc::flat(flat_statement),
                Doc::text(" }"),
            ]),
            Doc::concat([Doc::text("unsafe "), broken_block]),
        ]));
    }
    Ok(Doc::concat([Doc::text("unsafe "), broken_block]))
}

fn statement_document(
    source: &ParsedSource,
    statement: &Stmt,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    match statement {
        Stmt::Let(statement) => {
            let pattern = statement
                .pattern()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let Pattern::Binding(pattern) = pattern else {
                return Err(SourceDocumentError::UnsupportedShape);
            };
            let name = pattern
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = statement.ty();
            let initializer = statement
                .initializer()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let type_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::LET_KW];
            if ty.is_some() {
                expected_tokens.push(SyntaxKind::COLON);
            }
            expected_tokens.push(SyntaxKind::EQ);
            if pattern.mode().is_some()
                || pattern.syntax().children().count() != 1
                || !direct_token_kinds(pattern.syntax()).is_empty()
                || name.syntax().children().count() != 0
                || !matches!(
                    direct_token_kinds(name.syntax()).as_slice(),
                    [SyntaxKind::LIDENT]
                )
                || statement.else_clause().is_some()
                || statement.syntax().children().count() != 2 + usize::from(ty.is_some())
                || type_tokens != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut binding = vec![
                Doc::text("let "),
                Doc::text(node_text(source, name.syntax())),
            ];
            if let Some(ty) = ty {
                binding.extend([Doc::text(": "), type_document(source, &ty)?]);
            }
            binding.extend([
                Doc::text(" ="),
                right_hand_side_document(
                    source,
                    &initializer,
                    ExpressionContext::Unprotected,
                    expression_group_ids,
                    true,
                )?,
            ]);
            Ok(Doc::concat(binding))
        }
        Stmt::Var(statement) => {
            let name = statement
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = statement.ty();
            let initializer = statement
                .initializer()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let type_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::VAR_KW];
            if ty.is_some() {
                expected_tokens.push(SyntaxKind::COLON);
            }
            expected_tokens.push(SyntaxKind::EQ);
            if name.syntax().children().count() != 0
                || !matches!(
                    direct_token_kinds(name.syntax()).as_slice(),
                    [SyntaxKind::LIDENT]
                )
                || statement.syntax().children().count() != 2 + usize::from(ty.is_some())
                || type_tokens != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut binding = vec![
                Doc::text("var "),
                Doc::text(node_text(source, name.syntax())),
            ];
            if let Some(ty) = ty {
                binding.extend([Doc::text(": "), type_document(source, &ty)?]);
            }
            binding.extend([
                Doc::text(" ="),
                right_hand_side_document(
                    source,
                    &initializer,
                    ExpressionContext::Unprotected,
                    expression_group_ids,
                    true,
                )?,
            ]);
            Ok(Doc::concat(binding))
        }
        Stmt::Assign(statement) => {
            let lhs = statement
                .lhs()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let rhs = statement
                .rhs()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let (operator, _) = statement
                .op()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let operator_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            if statement.syntax().children().count() != 2 || operator_tokens != [operator.kind()] {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                expression_document(
                    source,
                    &lhs,
                    ExpressionContext::Protected,
                    expression_group_ids,
                )?,
                Doc::text(" "),
                Doc::text(operator.text()),
                right_hand_side_document(
                    source,
                    &rhs,
                    ExpressionContext::Unprotected,
                    expression_group_ids,
                    true,
                )?,
            ]))
        }
        Stmt::Assert(statement) => {
            let condition = statement
                .condition()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let message = statement.message();
            let mut expected_tokens = vec![SyntaxKind::ASSERT_KW];
            if message.is_some() {
                expected_tokens.push(SyntaxKind::COMMA);
            }
            if statement.syntax().children().count() != 1 + usize::from(message.is_some())
                || direct_token_kinds(statement.syntax()) != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![
                Doc::text("assert "),
                expression_document(
                    source,
                    &condition,
                    ExpressionContext::Unprotected,
                    expression_group_ids,
                )?,
            ];
            if let Some(message) = message {
                documents.extend([
                    Doc::text(", "),
                    expression_document(
                        source,
                        &message,
                        ExpressionContext::Unprotected,
                        expression_group_ids,
                    )?,
                ]);
            }
            Ok(Doc::concat(documents))
        }
        Stmt::Expr(statement) => {
            let expression = statement
                .expression()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if statement.syntax().children().count() != 1
                || !direct_token_kinds(statement.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            expression_document(
                source,
                &expression,
                ExpressionContext::Unprotected,
                expression_group_ids,
            )
        }
    }
}

fn right_hand_side_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
    allow_move: bool,
) -> Result<Doc, SourceDocumentError> {
    let document = expression_document(source, expression, context, expression_group_ids)?;
    let inline = Doc::concat([Doc::text(" "), Doc::flat(document.clone())]);
    let block_like = matches!(
        expression.syntax().kind(),
        SyntaxKind::BLOCK_EXPR
            | SyntaxKind::UNSAFE_BLOCK_EXPR
            | SyntaxKind::ASYNC_BLOCK_EXPR
            | SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::LOOP_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::CLOSURE_EXPR
            | SyntaxKind::REGION_EXPR
    );
    let mut alternatives = vec![inline.clone()];
    if !block_like && allow_move {
        alternatives.push(Doc::indent(Doc::concat([
            Doc::HardLine,
            Doc::flat(document.clone()),
        ])));
    }
    alternatives.push(Doc::concat([Doc::text(" "), document.clone()]));
    if !block_like && allow_move {
        alternatives.push(Doc::indent(Doc::concat([Doc::HardLine, document.clone()])));
    }
    alternatives.push(Doc::concat([Doc::text(" "), document]));
    Ok(Doc::choice(alternatives))
}

fn where_clause_document(
    source: &ParsedSource,
    clause: &ast::WhereClause,
) -> Result<Doc, SourceDocumentError> {
    let predicates = clause.predicates().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(clause.syntax());
    let significant_tokens = token_kinds
        .iter()
        .copied()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let expected_tokens = std::iter::once(SyntaxKind::WHERE_KW)
        .chain(std::iter::repeat_n(
            SyntaxKind::COMMA,
            predicates.len().saturating_sub(1),
        ))
        .collect::<Vec<_>>();
    if predicates.is_empty()
        || clause.syntax().children().count() != predicates.len()
        || significant_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let documents = predicates
        .iter()
        .map(|predicate| where_predicate_document(source, predicate))
        .collect::<Result<Vec<_>, _>>()?;
    let range = clause.syntax().text_range();
    let mut contents = Vec::with_capacity(documents.len() * 2);
    for (index, predicate) in documents.into_iter().enumerate() {
        if index > 0 {
            contents.extend([Doc::text(","), Doc::Line]);
        }
        contents.push(predicate);
    }
    Ok(Doc::group(
        GroupId(range.start().raw()),
        Doc::concat([
            Doc::text("where"),
            Doc::indent(Doc::concat([Doc::Line, Doc::concat(contents)])),
        ]),
    ))
}

fn where_predicate_document(
    source: &ParsedSource,
    predicate: &ast::WherePred,
) -> Result<Doc, SourceDocumentError> {
    let ty = predicate
        .ty()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let bounds = predicate.bounds().collect::<Vec<_>>();
    let elements = predicate
        .syntax()
        .children_with_tokens()
        .collect::<Vec<_>>();
    let children = predicate.syntax().children().collect::<Vec<_>>();
    let ty_syntax = ty.syntax();
    if bounds.is_empty()
        || children.len() != bounds.len() + 1
        || children.first().is_none_or(|child| {
            child.kind() != ty_syntax.kind() || child.text_range() != ty_syntax.text_range()
        })
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut cursor = 0;
    if !matches!(
        elements.get(cursor),
        Some(SyntaxElement::Node(node))
            if node.kind() == ty_syntax.kind() && node.text_range() == ty_syntax.text_range()
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    cursor += 1;
    if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::COLON)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    cursor += 1;

    let mut bound_documents = Vec::with_capacity(bounds.len());
    for (index, bound) in bounds.iter().enumerate() {
        let parenthesized = matches!(
            elements.get(cursor),
            Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::L_PAREN
        );
        if parenthesized {
            cursor += 1;
        }
        let bound_syntax = bound.syntax();
        if !matches!(
            elements.get(cursor),
            Some(SyntaxElement::Node(node))
                if node.kind() == bound_syntax.kind() && node.text_range() == bound_syntax.text_range()
        ) {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        cursor += 1;
        if parenthesized {
            if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::R_PAREN)
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            cursor += 1;
        }
        let document = bound_document(source, bound.clone())?;
        bound_documents.push(if parenthesized {
            Doc::concat([Doc::text("("), document, Doc::text(")")])
        } else {
            document
        });
        if index + 1 < bounds.len() {
            if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::PLUS)
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            cursor += 1;
        }
    }
    if cursor != elements.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let range = predicate.syntax().text_range();
    let mut contents = vec![type_document(source, &ty)?, Doc::text(": ")];
    for (index, bound) in bound_documents.into_iter().enumerate() {
        if index > 0 {
            contents.extend([
                Doc::text(" "),
                Doc::text("+"),
                Doc::indent(Doc::concat([Doc::Line, bound])),
            ]);
        } else {
            contents.push(bound);
        }
    }
    Ok(Doc::group(
        GroupId(range.start().raw()),
        Doc::concat(contents),
    ))
}

fn requires_clause_document(
    source: &ParsedSource,
    clause: &ast::RequiresClause,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    contract_clause_document(source, clause.syntax(), "requires", expression_group_ids)
}

fn ensures_clause_document(
    source: &ParsedSource,
    clause: &ast::EnsuresClause,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    contract_clause_document(source, clause.syntax(), "ensures", expression_group_ids)
}

fn contract_clause_document(
    source: &ParsedSource,
    clause: &sai_syntax::SyntaxNode,
    keyword: &str,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let expression = clause
        .children()
        .filter_map(Expr::cast)
        .next()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let token = clause
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if clause.children().count() != 1
        || direct_token_kinds(clause) != [SyntaxKind::LIDENT]
        || token_text(source, &token)? != keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(format!("{keyword} ")),
        expression_document(
            source,
            &expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?,
    ]))
}

fn expression_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    if let Some(chain) = member_chain_document(source, expression, context, expression_group_ids)? {
        return Ok(chain);
    }
    match expression {
        Expr::Literal(literal) => {
            let token = literal
                .token()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::text(normalize_literal(source, token)?))
        }
        Expr::FormatString(format_string) => Ok(Doc::Verbatim(
            node_text(source, format_string.syntax()).to_owned(),
        )),
        Expr::Path(path) => {
            let name = path.name().ok_or(SourceDocumentError::UnsupportedShape)?;
            let generic_args = path.generic_args();
            if path.syntax().children().count() != 1 + usize::from(generic_args.is_some())
                || !direct_token_kinds(path.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text(node_text(source, name.syntax()))];
            if let Some(generic_args) = generic_args {
                documents.push(generic_args_document(source, &generic_args)?);
            }
            Ok(Doc::concat(documents))
        }
        Expr::ImplicitMember(member) => {
            let name = member.name().ok_or(SourceDocumentError::UnsupportedShape)?;
            let tokens = direct_token_kinds(member.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            if member.syntax().children().count() != 1
                || tokens != [SyntaxKind::DOT]
                || direct_token_kinds(name.syntax()) != [SyntaxKind::UIDENT]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                Doc::text("."),
                Doc::text(node_text(source, name.syntax())),
            ]))
        }
        Expr::Unit(unit) => {
            if unit.syntax().children().count() != 0
                || direct_token_kinds(unit.syntax())
                    .into_iter()
                    .filter(|kind| *kind != SyntaxKind::NEWLINE)
                    .collect::<Vec<_>>()
                    != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text("()"))
        }
        Expr::Tuple(tuple) => tuple_expression_document(source, tuple, expression_group_ids),
        Expr::Array(array) => array_expression_document(source, array, expression_group_ids),
        Expr::Block(block) => block_expression_document(source, block, expression_group_ids),
        Expr::UnsafeBlock(unsafe_block) => {
            unsafe_block_expression_document(source, unsafe_block, expression_group_ids)
        }
        Expr::AsyncBlock(async_block) => {
            async_block_expression_document(source, async_block, expression_group_ids)
        }
        Expr::If(if_expr) => if_expression_document(source, if_expr, expression_group_ids),
        Expr::Loop(loop_expr) => loop_expression_document(source, loop_expr, expression_group_ids),
        Expr::While(while_expr) => {
            while_expression_document(source, while_expr, expression_group_ids)
        }
        Expr::For(for_expr) => for_expression_document(source, for_expr, expression_group_ids),
        Expr::Region(region_expr) => {
            region_expression_document(source, region_expr, expression_group_ids)
        }
        Expr::SelfValue(value) => {
            if value.syntax().children().count() != 0
                || direct_token_kinds(value.syntax()) != [SyntaxKind::SELF_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text(node_text(source, value.syntax())))
        }
        Expr::Field(field) => {
            let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
            let documents = vec![
                expression_document(source, &base, context, expression_group_ids)?,
                field_suffix_document(source, field)?,
            ];
            Ok(Doc::concat(documents))
        }
        Expr::Paren(paren) => {
            let expression = paren
                .expression()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if paren.syntax().children().count() != 1
                || direct_token_kinds(paren.syntax()) != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let group_id = expression_group_ids.next();
            Ok(Doc::group(
                group_id,
                Doc::concat([
                    Doc::text("("),
                    Doc::indent(Doc::concat([
                        Doc::SoftLine,
                        expression_document(
                            source,
                            &expression,
                            ExpressionContext::ParenthesizedWhole,
                            expression_group_ids,
                        )?,
                    ])),
                    Doc::SoftLine,
                    Doc::text(")"),
                ]),
            ))
        }
        Expr::Call(call) => {
            let callee = call.callee().ok_or(SourceDocumentError::UnsupportedShape)?;
            let arguments = call.args().ok_or(SourceDocumentError::UnsupportedShape)?;
            if call.syntax().children().count() != 2
                || !direct_token_kinds(call.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                expression_document(source, &callee, context, expression_group_ids)?,
                call_arguments_document(source, &arguments, expression_group_ids)?,
            ]))
        }
        Expr::TupleIndex(tuple_index) => {
            let base = tuple_index
                .base()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &base, context, expression_group_ids)?,
                tuple_index_suffix_document(tuple_index)?,
            ]))
        }
        Expr::Await(await_expr) => {
            let operand = await_expr
                .operand()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &operand, context, expression_group_ids)?,
                await_suffix_document(await_expr)?,
            ]))
        }
        Expr::Try(try_expr) => {
            let operand = try_expr
                .operand()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &operand, context, expression_group_ids)?,
                try_suffix_document(try_expr)?,
            ]))
        }
        Expr::Index(index_expr) => {
            let base = index_expr
                .base()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &base, context, expression_group_ids)?,
                index_suffix_document(source, index_expr, expression_group_ids)?,
            ]))
        }
        Expr::Return(return_expr) => {
            let value = return_expr.value();
            let tokens = direct_token_kinds(return_expr.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            if return_expr.syntax().children().count() != usize::from(value.is_some())
                || tokens != [SyntaxKind::RETURN_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text("return")];
            if let Some(value) = value {
                documents.push(right_hand_side_document(
                    source,
                    &value,
                    context,
                    expression_group_ids,
                    false,
                )?);
            }
            Ok(Doc::concat(documents))
        }
        Expr::Break(break_expr) => {
            let label = break_expr.label();
            let value = break_expr.value();
            let tokens = direct_token_kinds(break_expr.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::BREAK_KW];
            if label.is_some() {
                expected_tokens.push(SyntaxKind::LABEL);
            }
            if break_expr.syntax().children().count() != usize::from(value.is_some())
                || tokens != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text("break")];
            if let Some(label) = label {
                documents.push(Doc::text(" "));
                documents.push(Doc::text(label.syntax().text().to_string()));
            }
            if let Some(value) = value {
                documents.push(right_hand_side_document(
                    source,
                    &value,
                    context,
                    expression_group_ids,
                    false,
                )?);
            }
            Ok(Doc::concat(documents))
        }
        Expr::Continue(continue_expr) => {
            let label = continue_expr.label();
            let tokens = direct_token_kinds(continue_expr.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::CONTINUE_KW];
            if label.is_some() {
                expected_tokens.push(SyntaxKind::LABEL);
            }
            if continue_expr.syntax().children().count() != 0 || tokens != expected_tokens {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text("continue")];
            if let Some(label) = label {
                documents.push(Doc::text(" "));
                documents.push(Doc::text(label.syntax().text().to_string()));
            }
            Ok(Doc::concat(documents))
        }
        Expr::Binary(binary) => {
            operator_chain_document(source, binary, context, expression_group_ids)
        }
        Expr::Prefix(prefix) => {
            prefix_expression_document(source, prefix, context, expression_group_ids)
        }
        Expr::Range(range) => {
            range_expression_document(source, range, context, expression_group_ids)
        }
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn tuple_index_suffix_document(
    tuple_index: &ast::TupleIndexExpr,
) -> Result<Doc, SourceDocumentError> {
    let index = tuple_index
        .index()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(tuple_index.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if tuple_index.syntax().children().count() != 1 || tokens != [SyntaxKind::DOT, SyntaxKind::INT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text(format!(".{index}")))
}

fn await_suffix_document(await_expr: &ast::AwaitExpr) -> Result<Doc, SourceDocumentError> {
    let tokens = direct_token_kinds(await_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if await_expr.syntax().children().count() != 1
        || tokens != [SyntaxKind::DOT, SyntaxKind::AWAIT_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text(".await"))
}

fn try_suffix_document(try_expr: &ast::TryExpr) -> Result<Doc, SourceDocumentError> {
    let tokens = direct_token_kinds(try_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if try_expr.syntax().children().count() != 1 || tokens != [SyntaxKind::QUESTION] {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text("?"))
}

fn index_suffix_document(
    source: &ParsedSource,
    index_expr: &ast::IndexExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let index = index_expr
        .index()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(index_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if index_expr.syntax().children().count() != 2
        || tokens != [SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("["),
        expression_document(
            source,
            &index,
            ExpressionContext::Protected,
            expression_group_ids,
        )?,
        Doc::text("]"),
    ]))
}

fn tuple_expression_document(
    source: &ParsedSource,
    tuple: &ast::TupleExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let elements = tuple.elements().collect::<Vec<_>>();
    let tokens = direct_token_kinds(tuple.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if elements.is_empty()
        || tuple.syntax().children().count() != elements.len()
        || !comma_delimited_tokens_match(
            tuple.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            elements.len(),
        )
        || (elements.len() == 1
            && tokens != [SyntaxKind::L_PAREN, SyntaxKind::COMMA, SyntaxKind::R_PAREN])
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = elements
        .iter()
        .map(|element| {
            expression_document(
                source,
                element,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = tuple.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "(",
        ")",
        documents,
        true,
    ))
}

fn array_expression_document(
    source: &ParsedSource,
    array: &ast::ArrayExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let elements = array.elements().collect::<Vec<_>>();
    if array.syntax().children().count() != elements.len()
        || !comma_delimited_tokens_match(
            array.syntax(),
            SyntaxKind::L_BRACKET,
            SyntaxKind::R_BRACKET,
            elements.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = elements
        .iter()
        .map(|element| {
            expression_document(
                source,
                element,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = array.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "[",
        "]",
        documents,
        false,
    ))
}

fn prefix_expression_document(
    source: &ParsedSource,
    prefix: &ast::PrefixExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let mut operators = Vec::new();
    let mut expression = Expr::Prefix(prefix.clone());
    while let Expr::Prefix(prefix) = expression {
        let (operator, kind) = prefix.op().ok_or(SourceDocumentError::UnsupportedShape)?;
        let operand = prefix
            .operand()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let expected_tokens = match kind {
            ast::PrefixOp::Neg => vec![SyntaxKind::MINUS],
            ast::PrefixOp::BitNot => vec![SyntaxKind::TILDE],
            ast::PrefixOp::Deref => vec![SyntaxKind::STAR],
            ast::PrefixOp::Borrow => vec![SyntaxKind::BORROW_KW],
            ast::PrefixOp::BorrowMut => vec![SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW],
            ast::PrefixOp::Not => vec![SyntaxKind::NOT_KW],
        };
        let direct_tokens = direct_token_kinds(prefix.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>();
        if prefix.syntax().children().count() != 1
            || direct_tokens != expected_tokens
            || expected_tokens.first() != Some(&operator.kind())
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let text = match kind {
            ast::PrefixOp::Neg | ast::PrefixOp::BitNot | ast::PrefixOp::Deref => {
                token_text(source, &operator)?.to_owned()
            }
            ast::PrefixOp::Borrow => "borrow ".to_owned(),
            ast::PrefixOp::BorrowMut => "borrow mut ".to_owned(),
            ast::PrefixOp::Not => "not ".to_owned(),
        };
        operators.push(text);
        expression = operand;
    }

    let operand_context = match context {
        ExpressionContext::ParenthesizedWhole => ExpressionContext::Protected,
        context => context,
    };
    let mut document =
        expression_document(source, &expression, operand_context, expression_group_ids)?;
    for operator in operators.into_iter().rev() {
        document = Doc::concat([Doc::text(operator), document]);
    }
    Ok(document)
}

fn range_expression_document(
    source: &ParsedSource,
    range: &ast::RangeExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let start = range.start();
    let end = range.end();
    let direct_tokens = direct_token_kinds(range.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let operators = direct_tokens
        .iter()
        .copied()
        .filter(|kind| matches!(kind, SyntaxKind::DOT2 | SyntaxKind::DOT2EQ))
        .collect::<Vec<_>>();
    let children = usize::from(start.is_some()) + usize::from(end.is_some());
    if range.syntax().children().count() != children
        || operators.len() != 1
        || direct_tokens != operators
        || (operators[0] == SyntaxKind::DOT2EQ && end.is_none())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let operator = range
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == operators[0] => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let endpoint_context = match context {
        ExpressionContext::ParenthesizedWhole => ExpressionContext::Protected,
        context => context,
    };

    let mut documents = Vec::with_capacity(3);
    if let Some(start) = start {
        documents.push(expression_document(
            source,
            &start,
            endpoint_context,
            expression_group_ids,
        )?);
    }
    documents.push(Doc::text(token_text(source, &operator)?.to_owned()));
    if let Some(end) = end {
        documents.push(expression_document(
            source,
            &end,
            endpoint_context,
            expression_group_ids,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn operator_chain_document(
    source: &ParsedSource,
    binary: &ast::BinExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let (_, _, _, root_operator) = binary_parts(binary)?;
    let precedence = binary_precedence(root_operator);
    let is_comparison = matches!(
        root_operator,
        ast::BinOp::Eq
            | ast::BinOp::Ne
            | ast::BinOp::Lt
            | ast::BinOp::Le
            | ast::BinOp::Gt
            | ast::BinOp::Ge
    );
    let mut operands = Vec::new();
    let mut operators = Vec::new();
    collect_binary_chain(
        &Expr::Binary(binary.clone()),
        precedence,
        &mut operands,
        &mut operators,
    )?;
    if operands.len() != operators.len() + 1 || (is_comparison && operators.len() != 1) {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut operand_documents = operands
        .iter()
        .map(|operand| {
            expression_document(
                source,
                operand,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first = operand_documents
        .drain(..1)
        .next()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let mut suffixes = Vec::with_capacity(operators.len() * 4);
    for (operator, operand) in operators.iter().zip(operand_documents) {
        suffixes.push(Doc::Line);
        suffixes.push(Doc::text(token_text(source, operator)?.to_owned()));
        suffixes.push(Doc::text(" "));
        suffixes.push(operand);
    }

    let group_id = expression_group_ids.next();
    match context {
        ExpressionContext::Protected => Ok(Doc::group(
            group_id,
            Doc::concat([first, Doc::indent(Doc::concat(suffixes))]),
        )),
        ExpressionContext::ParenthesizedWhole => Ok(Doc::group(
            group_id,
            Doc::concat([first, Doc::concat(suffixes)]),
        )),
        ExpressionContext::Unprotected => Ok(Doc::group(
            group_id,
            Doc::concat([
                Doc::if_broken(group_id, Doc::text("("), Doc::text("")),
                Doc::indent(Doc::concat([Doc::SoftLine, first, Doc::concat(suffixes)])),
                Doc::SoftLine,
                Doc::if_broken(group_id, Doc::text(")"), Doc::text("")),
            ]),
        )),
    }
}

fn binary_parts(
    binary: &ast::BinExpr,
) -> Result<(Expr, SyntaxToken, Expr, ast::BinOp), SourceDocumentError> {
    let lhs = binary.lhs().ok_or(SourceDocumentError::UnsupportedShape)?;
    let rhs = binary.rhs().ok_or(SourceDocumentError::UnsupportedShape)?;
    let (operator, kind) = binary.op().ok_or(SourceDocumentError::UnsupportedShape)?;
    let token_kinds = direct_token_kinds(binary.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if binary.syntax().children().count() != 2 || token_kinds != [operator.kind()] {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok((lhs, operator, rhs, kind))
}

fn binary_precedence(operator: ast::BinOp) -> u8 {
    match operator {
        ast::BinOp::Or => 1,
        ast::BinOp::And => 2,
        ast::BinOp::Eq
        | ast::BinOp::Ne
        | ast::BinOp::Lt
        | ast::BinOp::Le
        | ast::BinOp::Gt
        | ast::BinOp::Ge => 3,
        ast::BinOp::BitOr => 4,
        ast::BinOp::BitXor => 5,
        ast::BinOp::BitAnd => 6,
        ast::BinOp::Shl | ast::BinOp::Shr => 7,
        ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::WrappingAdd | ast::BinOp::WrappingSub => 8,
        ast::BinOp::Mul | ast::BinOp::Div | ast::BinOp::Rem | ast::BinOp::WrappingMul => 9,
    }
}

fn collect_binary_chain(
    expression: &Expr,
    precedence: u8,
    operands: &mut Vec<Expr>,
    operators: &mut Vec<SyntaxToken>,
) -> Result<(), SourceDocumentError> {
    enum WorkItem {
        Expression(Expr),
        Operator(SyntaxToken),
    }

    let mut pending = vec![WorkItem::Expression(expression.clone())];
    while let Some(item) = pending.pop() {
        match item {
            WorkItem::Operator(operator) => operators.push(operator),
            WorkItem::Expression(Expr::Binary(binary)) => {
                let (lhs, operator, rhs, kind) = binary_parts(&binary)?;
                if binary_precedence(kind) == precedence {
                    pending.push(WorkItem::Expression(rhs));
                    pending.push(WorkItem::Operator(operator));
                    pending.push(WorkItem::Expression(lhs));
                } else {
                    operands.push(Expr::Binary(binary));
                }
            }
            WorkItem::Expression(operand) => operands.push(operand),
        }
    }
    Ok(())
}

fn member_chain_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Option<Doc>, SourceDocumentError> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum OperationKind {
        Segment,
        CallSegment,
        AwaitSegment,
        TupleIndexSegment,
        JoinPrevious,
        AttachPrevious,
    }

    struct Operation {
        kind: OperationKind,
        document: Doc,
    }

    let mut head = expression.clone();
    let mut reverse_operations = Vec::new();

    loop {
        match &head {
            Expr::Call(call) => {
                let callee = call.callee().ok_or(SourceDocumentError::UnsupportedShape)?;
                let Expr::Field(field) = callee else {
                    break;
                };
                let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
                let joins_previous = match direct_token_kinds(name.syntax()).as_slice() {
                    [SyntaxKind::LIDENT] => false,
                    [SyntaxKind::UIDENT] => true,
                    _ => break,
                };
                let arguments = call.args().ok_or(SourceDocumentError::UnsupportedShape)?;
                let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
                if call.syntax().children().count() != 2
                    || !direct_token_kinds(call.syntax()).is_empty()
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                reverse_operations.push(Operation {
                    kind: if joins_previous {
                        OperationKind::JoinPrevious
                    } else {
                        OperationKind::CallSegment
                    },
                    document: Doc::concat([
                        field_suffix_document(source, &field)?,
                        call_arguments_document(source, &arguments, expression_group_ids)?,
                    ]),
                });
                head = base;
            }
            Expr::Field(field) => {
                let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
                let kind = match direct_token_kinds(name.syntax()).as_slice() {
                    [SyntaxKind::LIDENT] => OperationKind::Segment,
                    [SyntaxKind::UIDENT] => OperationKind::JoinPrevious,
                    _ => return Err(SourceDocumentError::UnsupportedShape),
                };
                let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind,
                    document: field_suffix_document(source, field)?,
                });
                head = base;
            }
            Expr::Await(await_expr) => {
                let operand = await_expr
                    .operand()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AwaitSegment,
                    document: await_suffix_document(await_expr)?,
                });
                head = operand;
            }
            Expr::TupleIndex(tuple_index) => {
                let base = tuple_index
                    .base()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::TupleIndexSegment,
                    document: tuple_index_suffix_document(tuple_index)?,
                });
                head = base;
            }
            Expr::Index(index_expr) => {
                let base = index_expr
                    .base()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AttachPrevious,
                    document: index_suffix_document(source, index_expr, expression_group_ids)?,
                });
                head = base;
            }
            Expr::Try(try_expr) => {
                let operand = try_expr
                    .operand()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AttachPrevious,
                    document: try_suffix_document(try_expr)?,
                });
                head = operand;
            }
            _ => break,
        }
    }

    if reverse_operations
        .iter()
        .filter(|operation| operation.kind == OperationKind::CallSegment)
        .count()
        < 2
    {
        return Ok(None);
    }

    reverse_operations.reverse();
    let first_call_segment = reverse_operations
        .iter()
        .position(|operation| operation.kind == OperationKind::CallSegment)
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let first_segment = if matches!(head, Expr::Call(_)) {
        reverse_operations[..first_call_segment]
            .iter()
            .position(|operation| {
                matches!(
                    operation.kind,
                    OperationKind::AwaitSegment | OperationKind::TupleIndexSegment
                )
            })
            .unwrap_or(first_call_segment)
    } else {
        first_call_segment
    };
    let mut head_suffixes = Vec::new();
    let mut segments = Vec::with_capacity(reverse_operations.len() - first_segment);
    for (index, operation) in reverse_operations.into_iter().enumerate() {
        if index < first_segment {
            head_suffixes.push(operation.document);
            continue;
        }
        match operation.kind {
            OperationKind::Segment
            | OperationKind::CallSegment
            | OperationKind::AwaitSegment
            | OperationKind::TupleIndexSegment => {
                segments.push(operation.document);
            }
            OperationKind::JoinPrevious | OperationKind::AttachPrevious => {
                if let Some(previous) = segments.pop() {
                    segments.push(Doc::concat([previous, operation.document]));
                } else {
                    head_suffixes.push(operation.document);
                }
            }
        }
    }

    let mut chain = vec![expression_document(
        source,
        &head,
        context,
        expression_group_ids,
    )?];
    chain.extend(head_suffixes);
    let mut suffixes = Vec::with_capacity(segments.len() * 2);
    for segment in segments {
        suffixes.push(Doc::SoftLine);
        suffixes.push(segment);
    }
    chain.push(Doc::indent(Doc::concat(suffixes)));
    Ok(Some(Doc::group(
        expression_group_ids.next(),
        Doc::concat(chain),
    )))
}

fn field_suffix_document(
    source: &ParsedSource,
    field: &ast::FieldExpr,
) -> Result<Doc, SourceDocumentError> {
    let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_args = field.generic_args();
    if field.syntax().children().count() != 2 + usize::from(generic_args.is_some())
        || direct_token_kinds(field.syntax()) != [SyntaxKind::DOT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("."), Doc::text(node_text(source, name.syntax()))];
    if let Some(generic_args) = generic_args {
        documents.push(generic_args_document(source, &generic_args)?);
    }
    Ok(Doc::concat(documents))
}

fn call_arguments_document(
    source: &ParsedSource,
    arguments: &ast::ArgList,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let argument_count = arguments.syntax().children().count();
    let mut documents = Vec::with_capacity(argument_count);
    let mut has_trailing_record_update = false;
    for node in arguments.syntax().children() {
        if has_trailing_record_update {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let document = match node.kind() {
            SyntaxKind::ARG => {
                let argument = ast::Arg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 1
                    || !direct_token_kinds(argument.syntax()).is_empty()
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                expression_document(
                    source,
                    &expression,
                    ExpressionContext::Protected,
                    expression_group_ids,
                )?
            }
            SyntaxKind::NAMED_ARG => {
                let argument =
                    ast::NamedArg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let name = argument
                    .name()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 2
                    || direct_token_kinds(argument.syntax()) != [SyntaxKind::COLON]
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                Doc::concat([
                    Doc::text(node_text(source, name.syntax())),
                    Doc::text(": "),
                    expression_document(
                        source,
                        &expression,
                        ExpressionContext::Protected,
                        expression_group_ids,
                    )?,
                ])
            }
            SyntaxKind::BASE_ARG => {
                let argument =
                    ast::BaseArg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 1
                    || direct_token_kinds(argument.syntax()) != [SyntaxKind::DOT2]
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                has_trailing_record_update = true;
                Doc::concat([
                    Doc::text(".."),
                    expression_document(
                        source,
                        &expression,
                        ExpressionContext::Protected,
                        expression_group_ids,
                    )?,
                ])
            }
            _ => return Err(SourceDocumentError::UnsupportedShape),
        };
        documents.push((document, has_trailing_record_update));
    }
    if !comma_delimited_tokens_match(
        arguments.syntax(),
        SyntaxKind::L_PAREN,
        SyntaxKind::R_PAREN,
        documents.len(),
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if documents.is_empty() {
        return Ok(Doc::text("()"));
    }

    let range = arguments.syntax().text_range();
    let group = GroupId(range.start().raw());
    let mut contents = Vec::with_capacity(documents.len() * 2 + 2);
    contents.push(Doc::SoftLine);
    for (index, (document, is_record_update)) in documents.iter().enumerate() {
        if index > 0 {
            contents.push(Doc::text(","));
            contents.push(Doc::Line);
        }
        contents.push(document.clone());
        if index + 1 == documents.len() && !is_record_update {
            contents.push(Doc::if_broken(group, Doc::text(","), Doc::text("")));
        }
    }
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

fn generic_params_document(
    source: &ParsedSource,
    params: &ast::GenericParamList,
) -> Result<Doc, SourceDocumentError> {
    let parameters = params.params().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(params.syntax());
    if parameters.is_empty()
        || params.syntax().children().count() != parameters.len()
        || token_kinds.first() != Some(&SyntaxKind::LT)
        || token_kinds.last() != Some(&SyntaxKind::GT)
        || token_kinds[1..token_kinds.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::COMMA | SyntaxKind::NEWLINE))
        || token_kinds
            .iter()
            .filter(|kind| **kind == SyntaxKind::COMMA)
            .count()
            < parameters.len() - 1
        || token_kinds
            .iter()
            .filter(|kind| **kind == SyntaxKind::COMMA)
            .count()
            > parameters.len()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = parameters
        .into_iter()
        .map(|parameter| generic_param_document(source, parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let range = params.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "<",
        ">",
        documents,
        false,
    ))
}

fn generic_param_document(
    source: &ParsedSource,
    parameter: ast::GenericParam,
) -> Result<Doc, SourceDocumentError> {
    match parameter {
        ast::GenericParam::Type(parameter) => {
            let bounds = parameter
                .bounds()
                .map(|bounds| bound_list_document(source, &bounds))
                .transpose()?;
            let expected_tokens = if bounds.is_some() {
                vec![SyntaxKind::COLON]
            } else {
                Vec::new()
            };
            if parameter.attrs().next().is_some()
                || parameter.syntax().children().count() != 1 + usize::from(bounds.is_some())
                || direct_token_kinds(parameter.syntax()) != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = parameter
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let mut documents = vec![Doc::text(node_text(source, name.syntax()))];
            if let Some(bounds) = bounds {
                documents.extend([Doc::text(": "), bounds]);
            }
            Ok(Doc::concat(documents))
        }
        ast::GenericParam::Const(parameter) => {
            if parameter.attrs().next().is_some()
                || parameter.syntax().children().count() != 2
                || direct_token_kinds(parameter.syntax())
                    != [SyntaxKind::CONST_KW, SyntaxKind::COLON]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = parameter
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = parameter
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                Doc::text("const "),
                Doc::text(node_text(source, name.syntax())),
                Doc::text(": "),
                type_document(source, &ty)?,
            ]))
        }
    }
}

fn bound_list_document(
    source: &ParsedSource,
    bound_list: &ast::BoundList,
) -> Result<Doc, SourceDocumentError> {
    let bounds = bound_list.bounds().collect::<Vec<_>>();
    let syntax = bound_list.syntax();
    if bounds.is_empty()
        || syntax.children().count() != bounds.len()
        || direct_token_kinds(syntax) != vec![SyntaxKind::PLUS; bounds.len() - 1]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let documents = bounds
        .into_iter()
        .map(|bound| bound_document(source, bound))
        .collect::<Result<Vec<_>, _>>()?;
    let range = syntax.text_range();
    let mut list = Vec::with_capacity(documents.len() * 2 - 1);
    for (index, bound) in documents.into_iter().enumerate() {
        if index > 0 {
            list.extend([
                Doc::text(" "),
                Doc::text("+"),
                Doc::indent(Doc::concat([Doc::Line, bound])),
            ]);
        } else {
            list.push(bound);
        }
    }
    Ok(Doc::group(GroupId(range.start().raw()), Doc::concat(list)))
}

fn bound_document(source: &ParsedSource, bound: ast::Bound) -> Result<Doc, SourceDocumentError> {
    match bound {
        ast::Bound::Trait(bound) => {
            let path = bound.path().ok_or(SourceDocumentError::UnsupportedShape)?;
            let effects = bound.effects();
            if bound.syntax().children().count() != 1 + usize::from(effects.is_some())
                || !direct_token_kinds(bound.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![type_document(source, &ast::Type::PathType(path))?];
            if let Some(effects) = effects {
                documents.extend([Doc::text(" "), effects_clause_document(source, &effects)?]);
            }
            Ok(Doc::concat(documents))
        }
        ast::Bound::FnSugar(bound) => fn_sugar_bound_document(source, &bound),
    }
}

fn fn_sugar_bound_document(
    source: &ParsedSource,
    bound: &ast::FnSugarBound,
) -> Result<Doc, SourceDocumentError> {
    let name = bound.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let parameters = bound.params().collect::<Vec<_>>();
    let return_type = bound.ret_type();
    let effects = bound.effects();
    let mut expected_tokens = vec![SyntaxKind::L_PAREN];
    expected_tokens.extend(std::iter::repeat_n(
        SyntaxKind::COMMA,
        parameters.len().saturating_sub(1),
    ));
    expected_tokens.push(SyntaxKind::R_PAREN);
    if return_type.is_some() {
        expected_tokens.push(SyntaxKind::THIN_ARROW);
    }
    let direct_tokens = direct_token_kinds(bound.syntax());
    if !parameters.is_empty() && direct_tokens.get(parameters.len()) == Some(&SyntaxKind::COMMA) {
        expected_tokens.insert(parameters.len(), SyntaxKind::COMMA);
    }
    if bound.syntax().children().count()
        != 1 + parameters.len()
            + usize::from(return_type.is_some())
            + usize::from(effects.is_some())
        || direct_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let parameter_documents = parameters
        .iter()
        .map(|parameter| type_document(source, parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let range = bound.syntax().text_range();
    let mut documents = vec![
        Doc::text(node_text(source, name.syntax())),
        delimited_list_document(
            GroupId(range.start().raw()),
            "(",
            ")",
            parameter_documents,
            false,
        ),
    ];
    if let Some(return_type) = return_type {
        documents.extend([Doc::text(" -> "), type_document(source, &return_type)?]);
    }
    if let Some(effects) = effects {
        documents.extend([Doc::text(" "), effects_clause_document(source, &effects)?]);
    }
    Ok(Doc::concat(documents))
}

fn flat_document_width(document: &Doc) -> Option<usize> {
    let mut width = 0usize;
    let mut pending = vec![document];
    while let Some(document) = pending.pop() {
        let added_width = match document {
            Doc::Text(text) => text.chars().count(),
            Doc::Line => 1,
            Doc::SoftLine | Doc::LineSuffix(_) => 0,
            Doc::HardLine => return None,
            Doc::Indent(child) | Doc::Group(_, child) | Doc::Flat(child) => {
                pending.push(child);
                continue;
            }
            Doc::IfBroken(_, _, flat) => {
                pending.push(flat);
                continue;
            }
            Doc::Choice(alternatives) => {
                pending.push(alternatives.first()?);
                continue;
            }
            Doc::Verbatim(text) if !text.contains(['\r', '\n']) => text.chars().count(),
            Doc::Verbatim(_) => return None,
            Doc::Concat(documents) => {
                pending.extend(documents.iter().rev());
                continue;
            }
        };
        width = width.checked_add(added_width)?;
    }
    Some(width)
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

fn comma_delimited_tokens_match(
    node: &sai_syntax::SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    item_count: usize,
) -> bool {
    let direct_tokens = direct_token_kinds(node)
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let mut expected = vec![open];
    expected.extend(std::iter::repeat_n(
        SyntaxKind::COMMA,
        item_count.saturating_sub(1),
    ));
    expected.push(close);
    if item_count > 0 && direct_tokens.get(item_count) == Some(&SyntaxKind::COMMA) {
        expected.insert(item_count, SyntaxKind::COMMA);
    }
    direct_tokens == expected
}

fn delimited_list_document(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    keep_singleton_comma: bool,
) -> Doc {
    if items.is_empty() {
        return Doc::concat([Doc::text(open), Doc::text(close)]);
    }
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

fn token_text<'a>(
    source: &'a ParsedSource,
    token: &SyntaxToken,
) -> Result<&'a str, SourceDocumentError> {
    let range = token.text_range();
    source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(SourceDocumentError::UnsupportedShape)
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
        ast::{AstNode, Expr, RangeExpr, Type},
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
    fn formats_function_type_and_const_generic_parameters() {
        for (input, expected) in [
            (
                b"fn identity<T>(value:T)->T{}".as_slice(),
                "fn identity<T>(value: T) -> T {}\n",
            ),
            (
                b"fn make<const N:USize,T>(value:Array<T,N>)->Array<T,N>{}",
                "fn make<const N: USize, T>(value: Array<T, N>) -> Array<T, N> {}\n",
            ),
            (
                b"fn f<T,>(value:T)borrows{value}effects{io}{}",
                "fn f<T>(value: T)\n    borrows { value }\n    effects { io }\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("function generics are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_function_generic_parameter_lists() {
        let first = "A".repeat(48);
        let second = "B".repeat(48);
        let input = format!("fn f<{first},{second}>(){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic parameters are supported");
        let expected = format!("fn f<\n    {first},\n    {second},\n>() {{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped generics parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_function_generic_trait_and_function_bounds() {
        for (input, expected) in [
            (
                b"fn f<T:Copy+Clone>(){}".as_slice(),
                "fn f<T: Copy + Clone>() {}\n",
            ),
            (
                b"fn f<T:Iterator<Item=U32>+Clone>(){}",
                "fn f<T: Iterator<Item = U32> + Clone>() {}\n",
            ),
            (
                b"fn f<T:Fn(I32)->I32 effects{io}>(){}",
                "fn f<T: Fn(I32) -> I32 effects { io }>() {}\n",
            ),
            (b"fn f<T:Fn(I32,)>(){}", "fn f<T: Fn(I32)>() {}\n"),
            (b"fn f<T:Fn(I32,Str,)>(){}", "fn f<T: Fn(I32, Str)>() {}\n"),
            (
                b"fn f<T:Clone effects{io}>(){}",
                "fn f<T: Clone effects { io }>() {}\n",
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
    fn breaks_long_function_generic_bounds_after_plus() {
        let first = format!("Trait{}", "A".repeat(42));
        let second = format!("Clone{}", "B".repeat(42));
        let input = format!("fn f<T:{first}+{second}>(){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long generic bounds are supported");
        let expected = format!("fn f<\n    T: {first} +\n        {second},\n>() {{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped bounds parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_function_where_clauses_and_breaks_by_predicate() {
        for (input, expected) in [
            (
                b"fn f<T>(x:T)\nwhere T:Clone+Hash\n{}".as_slice(),
                "fn f<T>(x: T)\n    where T: Clone + Hash\n{}\n",
            ),
            (
                b"fn f<T>()\nwhere T:(Fn() effects{io})\n{}".as_slice(),
                "fn f<T>()\n    where T: (Fn() effects { io })\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted where clause parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let first = format!("Trait{}", "A".repeat(42));
        let second = format!("Trait{}", "B".repeat(42));
        let input = format!("fn f<T,U>()\nwhere T:{first},U:{second}\n{{}}");
        let parsed = parse_source(input.as_bytes()).expect("long input parses");
        let output = format_shape(input.as_bytes()).expect("where predicates are supported");
        let expected =
            format!("fn f<T, U>()\n    where\n        T: {first},\n        U: {second}\n{{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped where clause parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first_bound = format!("Trait{}", "C".repeat(42));
        let second_bound = format!("Trait{}", "D".repeat(42));
        let input = format!("fn f<T>()\nwhere T:{first_bound}+{second_bound}\n{{}}");
        let parsed = parse_source(input.as_bytes()).expect("long bounds parse");
        let output = format_shape(input.as_bytes()).expect("long where bounds are supported");
        let expected = format!(
            "fn f<T>()\n    where\n        T: {first_bound} +\n            {second_bound}\n{{}}\n"
        );
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped where bounds parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn orders_where_before_borrows_and_effects() {
        let input = b"fn f<T>() effects{io} borrows{self} where T:Clone {}";
        let expected = concat!(
            "fn f<T>()\n",
            "    where T: Clone\n",
            "    borrows { self }\n",
            "    effects { io }\n",
            "{}\n",
        );
        parse_source(input).expect("input parses with a clause-order diagnostic");
        assert_eq!(format_shape(input), Ok(expected.to_owned()));
        parse_source(expected.as_bytes()).expect("canonical output parses");
        assert_eq!(format_shape(expected.as_bytes()), Ok(expected.to_owned()));
    }

    #[test]
    fn formats_requires_and_ensures_clauses_in_canonical_order() {
        let input =
            b"fn f() ensures result>=Money(0) requires amount<=balance requires true ensures false {}";
        let expected = concat!(
            "fn f()\n",
            "    requires amount <= balance\n",
            "    requires true\n",
            "    ensures result >= Money(0)\n",
            "    ensures false\n",
            "{}\n",
        );
        parse_source(input).expect("input parses with a clause-order diagnostic");
        let output = format_shape(input).expect("contract clauses are supported");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("canonical clauses parse");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_binary_operators_by_precedence_and_source_order() {
        for (input, expected) in [
            (
                b"fn f() requires a+b*c>=limit {}".as_slice(),
                "fn f()\n    requires a + b * c >= limit\n{}\n",
            ),
            (
                b"fn f() ensures a-b+c==d {}",
                "fn f()\n    ensures a - b + c == d\n{}\n",
            ),
            (
                b"fn f() requires a or b and c {}",
                "fn f()\n    requires a or b and c\n{}\n",
            ),
            (
                b"fn f() requires a<<b|c {}",
                "fn f()\n    requires a << b | c\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("binary operator expression parses");
            let output = format_shape(input).expect("binary operator expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_prefix_operators_with_canonical_spacing() {
        for (expression, expected_expression) in [
            ("-value", "-value"),
            ("~ value", "~value"),
            ("* value", "*value"),
            ("borrow value", "borrow value"),
            ("borrow   mut   value", "borrow mut value"),
            ("not value", "not value"),
            ("not not value", "not not value"),
            ("-~value", "-~value"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes()).expect("prefix expression parses");
            let output = format_shape(input.as_bytes()).expect("prefix expression is supported");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted prefix expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_long_prefix_chains_iteratively() {
        let prefix_count = 256;
        let expression = format!("{}value", "-".repeat(prefix_count));
        let input = format!("fn f() requires {expression} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long prefix chain parses");
        let output = format_shape(input.as_bytes()).expect("long prefix chain is supported");
        assert_eq!(output, format!("fn f()\n    requires {expression}\n{{}}\n"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted prefix chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_ranges_without_spaces_and_preserves_endpoint_precedence() {
        for (expression, expected_expression) in [
            ("start..end", "start..end"),
            ("start ..= end", "start..=end"),
            ("..end", "..end"),
            ("check(start..)", "check(start..)"),
            ("(..)", "(..)"),
            ("start+step..end*factor", "start + step..end * factor"),
            ("start..end>=limit", "start..end >= limit"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted range expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn range_endpoints_can_break_without_breaking_the_range_operator() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        let third = "third".repeat(6);
        let expression = format!("{first}+{second}+{third}..limit");
        let input = format!("fn f() requires {expression} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long range endpoint parses");
        let output = format_shape(input.as_bytes()).expect("long range endpoint is supported");
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert!(output.contains(")..limit\n"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted range parses");
        let original_range = parsed
            .root()
            .descendants()
            .find_map(RangeExpr::cast)
            .expect("original range expression exists");
        let formatted_range = reparsed
            .root()
            .descendants()
            .find_map(RangeExpr::cast)
            .expect("formatted range expression exists");
        let original_start = original_range.start().expect("original range start");
        let formatted_start = formatted_range.start().expect("formatted range start");
        let formatted_start = match formatted_start {
            Expr::Paren(paren) => paren
                .expression()
                .expect("formatter-inserted parentheses retain the endpoint"),
            expression => expression,
        };
        assert_eq!(
            syntax_shape(original_start.syntax()),
            syntax_shape(formatted_start.syntax())
        );
        assert_eq!(
            syntax_shape(original_range.end().expect("original range end").syntax()),
            syntax_shape(formatted_range.end().expect("formatted range end").syntax())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn parenthesized_prefix_and_range_operands_use_protected_chain_layout() {
        let first = "first".repeat(8);
        let second = "second".repeat(7);
        let third = "third".repeat(5);

        let input = format!("fn f() requires (not {first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        not {first}\n            + {second}\n            + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("parenthesized prefix parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized prefix is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted prefix parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({first}+{second}+{third}..limit) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n            + {second}\n            + {third}..limit\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("parenthesized range parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized range is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted range parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_unit_tuple_array_and_postfix_expression_forms() {
        for (expression, expected_expression) in [
            ("()", "()"),
            ("(value,)", "(value,)"),
            ("(first,second,)", "(first, second)"),
            ("[]", "[]"),
            ("[first,second,]", "[first, second]"),
            (".Variant", ".Variant"),
            (".Variant(payload)", ".Variant(payload)"),
            ("value.0", "value.0"),
            ("value?", "value?"),
            ("value.await", "value.await"),
            ("items[index+offset]", "items[index + offset]"),
            ("items[index].field().next()", "items[index].field().next()"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {expression}: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_named_and_record_update_call_arguments_in_source_order() {
        let input = "fn f() requires call(first,named: value,last,..base,) {}\n";
        let expected = "fn f()\n    requires call(first, named: value, last, ..base)\n{}\n";
        let parsed = parse_source(input.as_bytes()).expect("mixed call arguments parse");
        let output = format_shape(input.as_bytes()).expect("mixed call arguments are supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted call arguments parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(8);
        let name = format!("named_{}", "n".repeat(15));
        let value = "value".repeat(7);
        let base = "base".repeat(8);
        let input = format!("fn f() requires call({first},{name}:{value},..{base},) {{}}\n");
        let expected = format!(
            "fn f()\n    requires call(\n        {first},\n        {name}: {value},\n        ..{base}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long mixed call arguments parse");
        let output =
            format_shape(input.as_bytes()).expect("long mixed call arguments are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert!(!output.contains(&format!("..{base},\n")));
        let reparsed = parse_source(output.as_bytes()).expect("broken mixed call arguments parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_tuple_and_array_lists_with_required_trailing_commas() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        for (expression, expected_expression) in [
            (
                format!("({first},{second})"),
                format!("(\n        {first},\n        {second},\n    )"),
            ),
            (
                format!("[{first},{second}]"),
                format!("[\n        {first},\n        {second},\n    ]"),
            ),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes()).expect("long delimited expression parses");
            let output =
                format_shape(input.as_bytes()).expect("long delimited expression is supported");
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed =
                parse_source(output.as_bytes()).expect("broken delimited expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let left = "left".repeat(12);
        let right = "right".repeat(10);
        let input = format!("fn f() requires items[{left}+{right}] {{}}");
        let output = format_shape(input.as_bytes()).expect("long index expression is supported");
        assert!(output.contains(&format!("items[{left}\n        + {right}]")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken index expression parses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
        assert!(reparsed.diagnostics().is_empty());
    }

    #[test]
    fn member_chains_keep_postfix_operations_inside_the_chain_group() {
        let first = format!("first_call_{}", "a".repeat(20));
        let second = format!("second_call_{}", "b".repeat(20));
        let third = format!("third_call_{}", "c".repeat(20));
        let input = format!(
            "fn f() requires root[index].{first}().{second}()[offset]?.await.0.{third}() {{}}"
        );
        let expected = format!(
            "fn f()\n    requires root[index]\n        .{first}()\n        .{second}()[offset]?\n        .await\n        .0\n        .{third}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("mixed postfix chain parses");
        let output = format_shape(input.as_bytes()).expect("mixed postfix chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken postfix chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn member_chains_start_segments_at_await_and_tuple_index_before_calls() {
        let first = format!("first_call_{}", "a".repeat(40));
        let second = format!("second_call_{}", "b".repeat(40));

        for (postfix, expected_postfix) in [("await", ".await"), ("0", ".0")] {
            let input = format!("fn f() requires root().{postfix}.{first}().{second}() {{}}");
            let expected = format!(
                "fn f()\n    requires root()\n        {expected_postfix}\n        .{first}()\n        .{second}()\n{{}}\n"
            );
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} chain parses: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} chain is supported: {error:?}"));
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {postfix} chain parses: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));

            let input = format!("fn f() requires root.{postfix}.{first}().{second}() {{}}");
            let expected = format!(
                "fn f()\n    requires root{expected_postfix}\n        .{first}()\n        .{second}()\n{{}}\n"
            );
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} head parses: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} head is supported: {error:?}"));
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {postfix} head parses: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_operator_chains_with_context_appropriate_parentheses() {
        let first = "first".repeat(8);
        let second = "second".repeat(7);
        let third = "third".repeat(5);

        let input = format!("fn f() requires {first}+{second}+{third} {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n        + {second}\n        + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("top-level chain parses");
        let output = format_shape(input.as_bytes()).expect("top-level chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped chain parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires check({first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires check(\n        {first}\n            + {second}\n            + {third},\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("protected chain parses");
        let output = format_shape(input.as_bytes()).expect("protected chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("protected chain parses again");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n        + {second}\n        + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("explicit chain parses");
        let output = format_shape(input.as_bytes()).expect("explicit chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("explicit chain reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_long_operator_chains_without_recursive_flattening() {
        let term_count = 4096;
        let terms = (0..term_count)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join("+");
        let input = format!("fn f() requires {terms} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long operator chain parses");
        let output = format_shape(input.as_bytes()).expect("long operator chain is supported");
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.trim_start().starts_with("+ "))
                .count(),
            term_count - 1
        );
        let reparsed = parse_source(output.as_bytes()).expect("formatted long chain parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_contract_comparisons_with_safe_parentheses() {
        let lhs = "left".repeat(12);
        let rhs = "right".repeat(10);
        let input = format!("fn f() requires {lhs}=={rhs} {{}}");
        let expected =
            format!("fn f()\n    requires (\n        {lhs}\n        == {rhs}\n    )\n{{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("comparison contract is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped contract parses");
        assert!(reparsed.diagnostics().is_empty());
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn preserves_protected_and_explicitly_parenthesized_comparisons() {
        let lhs = "left".repeat(12);
        let rhs = "right".repeat(10);

        let input = format!("fn f() requires check({lhs}=={rhs}) {{}}");
        let parsed = parse_source(input.as_bytes()).expect("protected comparison parses");
        let output = format_shape(input.as_bytes()).expect("protected comparison is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted call parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let chain_rhs = "right".repeat(18);
        let input = format!("fn f() requires root.first().second()=={chain_rhs} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("comparison with a chain parses");
        let output = format_shape(input.as_bytes()).expect("comparison with a chain is supported");
        assert_eq!(
            output,
            format!(
                "fn f()\n    requires (\n        root.first().second()\n        == {chain_rhs}\n    )\n{{}}\n"
            )
        );
        let reparsed = parse_source(output.as_bytes()).expect("wrapped chain comparison parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({lhs}=={rhs}) {{}}");
        let parsed = parse_source(input.as_bytes()).expect("parenthesized comparison parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized comparison is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted parentheses parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_field_and_call_contract_expressions() {
        for (input, expected) in [
            (
                b"fn f() requires user.active {}".as_slice(),
                "fn f()\n    requires user.active\n{}\n",
            ),
            (
                b"fn f() requires user.is_active() {}".as_slice(),
                "fn f()\n    requires user.is_active()\n{}\n",
            ),
            (
                b"fn f() ensures check(user.value,amount,) {}".as_slice(),
                "fn f()\n    ensures check(user.value, amount)\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("contract input parses");
            let output = format_shape(input).expect("field and call expressions are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted contract parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_generic_method_calls_and_format_string_contracts() {
        for (input, expected) in [
            (
                b"fn f() requires user.check<I32>(value) {}".as_slice(),
                "fn f()\n    requires user.check<I32>(value)\n{}\n",
            ),
            (
                b"fn f() requires f\"hello {name}\" {}",
                "fn f()\n    requires f\"hello {name}\"\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("contract input parses");
            let output = format_shape(input).expect("supported contract expression is formatted");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted contract parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn keeps_empty_call_arguments_inline_after_a_long_callee() {
        let callee = "a".repeat(100);
        let input = format!("fn f() requires {callee}() {{}}");
        let output = format_shape(input.as_bytes()).expect("empty call is supported");
        assert!(output.contains(&format!("{callee}()")));
        assert!(!output.contains("(\n\n)"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted empty call parses");
        assert!(reparsed.diagnostics().is_empty());
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_method_chains_before_every_call_segment() {
        let first = format!("first{}", "a".repeat(45));
        let second = format!("second{}", "b".repeat(45));
        let input = format!("fn f() requires root.{first}().{second}() {{}}");
        let expected =
            format!("fn f()\n    requires root\n        .{first}()\n        .{second}()\n{{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("method chain parses");
        let output = format_shape(input.as_bytes()).expect("method chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken method chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_intermediate_fields_in_broken_member_chains() {
        let middle = "middle".repeat(12);
        let input = format!("fn f() requires root.first().{middle}.second() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .first()\n        .{middle}\n        .second()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("member chain parses");
        let output = format_shape(input.as_bytes()).expect("member chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("broken member chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_trailing_fields_and_joins_uppercase_members_to_the_prior_segment() {
        let trailing = "trailing".repeat(10);
        let input = format!("fn f() requires root.first().second().{trailing} {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .first()\n        .second()\n        .{trailing}\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("trailing field chain parses");
        let output = format_shape(input.as_bytes()).expect("trailing field chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("broken trailing field chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(5);
        let second = "second".repeat(5);
        let last = "last".repeat(5);
        let input = format!("fn f() requires root.{first}().Variant.{second}().{last}() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .{first}().Variant\n        .{second}()\n        .{last}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("uppercase field chain parses");
        let output = format_shape(input.as_bytes()).expect("uppercase field joins prior segment");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted uppercase chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input =
            format!("fn f() requires root.{first}().Variant(payload).{second}().{last}() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .{first}().Variant(payload)\n        .{second}()\n        .{last}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("uppercase call chain parses");
        let output = format_shape(input.as_bytes()).expect("uppercase call joins prior segment");
        assert_eq!(output, expected);
        let reparsed =
            parse_source(output.as_bytes()).expect("formatted uppercase call chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(18);
        let input = format!("fn f() requires root.{first}().Variant(payload) {{}}");
        let expected = format!(
            "fn f()\n    requires root.{first}().Variant(\n        payload,\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("single call segment parses");
        let output = format_shape(input.as_bytes()).expect("one segment stays attached");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("single call segment reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_short_generics_flat_when_function_parameters_wrap() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        let input = format!("fn f<T>({first}:I32,{second}:U32){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic signature is supported");
        let expected = format!("fn f<T>(\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("wrapped parameters parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_short_generics_flat_when_return_type_overflows() {
        let return_type = "R".repeat(92);
        let input = format!("fn f<T>()->{return_type}{{}}");
        let expected = format!("fn f<T>() -> {return_type} {{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic signature is supported");
        assert_eq!(output, expected);
        assert!(output.lines().next().unwrap().chars().count() > 100);
        let reparsed = parse_source(output.as_bytes()).expect("output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
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
    fn formats_all_basic_statement_forms_and_preserves_one_blank_line() {
        let input = concat!(
            "fn f() {\n",
            "let value:I32=\n1\n",
            "var current:I32=2\n\n",
            "current+=\nvalue\n",
            "assert current>value,\"message\"\n",
            "consume(current)\n",
            "current\n",
            "}\n",
        );
        let expected = concat!(
            "fn f() {\n",
            "    let value: I32 = 1\n",
            "    var current: I32 = 2\n",
            "\n",
            "    current += value\n",
            "    assert current > value, \"message\"\n",
            "    consume(current)\n",
            "    current\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("statement forms parse");
        let output = format_shape(input.as_bytes()).expect("statement forms are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted statements parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn moves_a_short_statement_rhs_to_the_next_line_when_it_fits() {
        let name = format!("binding_{}", "x".repeat(81));
        let input = format!("fn f() {{ let {name}=1\n{name}\n}}\n");
        let expected = format!("fn f() {{\n    let {name} =\n        1\n    {name}\n}}\n");
        let parsed = parse_source(input.as_bytes()).expect("long binding name parses");
        let output = format_shape(input.as_bytes()).expect("long binding name is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("moved rhs parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_return_break_and_continue_expressions() {
        let input = concat!(
            "fn f() {\n",
            "return\n",
            "return result\n",
            "break\n",
            "break 'outer result\n",
            "continue\n",
            "continue 'outer\n",
            "}\n",
        );
        let expected = concat!(
            "fn f() {\n",
            "    return\n",
            "    return result\n",
            "    break\n",
            "    break 'outer result\n",
            "    continue\n",
            "    continue 'outer\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("control expressions parse");
        let output = format_shape(input.as_bytes()).expect("control expressions are supported");
        assert_eq!(output, expected);
        let reparsed =
            parse_source(output.as_bytes()).expect("formatted control expressions parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let value = format!("result_{}", "r".repeat(88));
        let input = format!("fn f() {{ return {value}\n}}\n");
        let expected = format!("fn f() {{\n    return {value}\n}}\n");
        let output = format_shape(input.as_bytes()).expect("long return value is supported");
        assert_eq!(output, expected);
        assert!(output.lines().any(|line| line.chars().count() > 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_block_and_conditional_expressions() {
        for (input, expected) in [
            (
                b"fn f(){if ready {work()}}".as_slice(),
                "fn f() {\n    if ready {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){if ready {} else {work()}}".as_slice(),
                "fn f() {\n    if ready {} else {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){if first {one()} else if second {two()} else {three()}}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    if first {\n",
                    "        one()\n",
                    "    } else if second {\n",
                    "        two()\n",
                    "    } else {\n",
                    "        three()\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){\nif ready {\nwork()\n}\nelse {\nwait()\n}\n}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    if ready {\n",
                    "        work()\n",
                    "    } else {\n",
                    "        wait()\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){{value}}".as_slice(),
                "fn f() {\n    {\n        value\n    }\n}\n",
            ),
            (
                b"fn f(){let result=if ready {yes()} else {no()}\nresult}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    let result = if ready {\n",
                    "        yes()\n",
                    "    } else {\n",
                    "        no()\n",
                    "    }\n",
                    "    result\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){consume(if ready {yes()} else {no()})}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    consume(\n",
                    "        if ready {\n",
                    "            yes()\n",
                    "        } else {\n",
                    "            no()\n",
                    "        },\n",
                    "    )\n",
                    "}\n",
                ),
            ),
        ] {
            let parsed = parse_source(input).expect("control and block expressions parse");
            let output =
                format_shape(input).expect("supported control and block expressions format");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted expressions parse again");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn unsupported_if_let_conditions_remain_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(){if let Some(value)=input {value}}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
    }

    #[test]
    fn breaks_long_if_conditions_with_parentheses() {
        let left = format!("first_{}", "a".repeat(43));
        let right = format!("second_{}", "b".repeat(42));
        let input = format!("fn f(){{if {left} or {right}{{act()}}}}");
        let expected = format!(
            "fn f() {{\n    if (\n        {left}\n        or {right}\n    ) {{\n        act()\n    }}\n}}\n"
        );

        parse_source(input.as_bytes()).expect("long condition parses");
        let output = format_shape(input.as_bytes()).expect("long condition is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        parse_source(output.as_bytes()).expect("wrapped condition reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_loop_expressions_and_labels() {
        for (input, expected) in [
            (
                b"fn f(){loop {tick()}}".as_slice(),
                "fn f() {\n    loop {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){'outer:\nloop {break 'outer}}".as_slice(),
                "fn f() {\n    'outer: loop {\n        break 'outer\n    }\n}\n",
            ),
            (b"fn f(){'scan:loop{}}", "fn f() {\n    'scan: loop {}\n}\n"),
        ] {
            let parsed = parse_source(input).expect("loop expression parses");
            let output = format_shape(input).expect("loop expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted loop parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_while_expressions_and_labels() {
        for (input, expected) in [
            (
                b"fn f(){while ready {tick()}}".as_slice(),
                "fn f() {\n    while ready {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){'scan:\nwhile ready {break 'scan}}".as_slice(),
                "fn f() {\n    'scan: while ready {\n        break 'scan\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("while expression parses");
            let output = format_shape(input).expect("while expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted while parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn unsupported_while_let_conditions_remain_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(){while let Some(value)=next() {value}}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
    }

    #[test]
    fn formats_for_expressions_with_simple_patterns() {
        for (input, expected) in [
            (
                b"fn f(){for row in rows {consume(row)}}".as_slice(),
                "fn f() {\n    for row in rows {\n        consume(row)\n    }\n}\n",
            ),
            (
                b"fn f(){for _ in rows {tick()}}".as_slice(),
                "fn f() {\n    for _ in rows {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){for var row in rows {row+=1}}".as_slice(),
                "fn f() {\n    for var row in rows {\n        row += 1\n    }\n}\n",
            ),
            (
                b"fn f(){for borrow mut row in rows {mutate(row)}}".as_slice(),
                "fn f() {\n    for borrow mut row in rows {\n        mutate(row)\n    }\n}\n",
            ),
            (
                b"fn f(){'rows:\nfor row in rows {break 'rows}}".as_slice(),
                "fn f() {\n    'rows: for row in rows {\n        break 'rows\n    }\n}\n",
            ),
            (
                b"fn f(){for await row in rows {consume(row)}}".as_slice(),
                "fn f() {\n    for await row in rows {\n        consume(row)\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("for expression parses");
            let output = format_shape(input).expect("simple for expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted for parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn unsupported_destructuring_for_patterns_remain_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(){for (left,right) in pairs {consume(left)}}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
    }

    #[test]
    fn formats_async_and_region_block_expressions() {
        for (input, expected) in [
            (
                b"fn f(){async {work()}}".as_slice(),
                "fn f() {\n    async {\n        work()\n    }\n}\n",
            ),
            (b"fn f(){async move{}}", "fn f() {\n    async move {}\n}\n"),
            (
                b"fn f(){region arena{work()}}".as_slice(),
                "fn f() {\n    region arena {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){region arena{}}",
                "fn f() {\n    region arena {}\n}\n",
            ),
            (b"fn f(){region}", "fn f() {\n    region\n}\n"),
        ] {
            let parsed = parse_source(input).expect("block wrapper parses");
            let output = format_shape(input).expect("async or region block is supported");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted block wrapper reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_unsafe_blocks_flat_only_for_one_fitting_expression() {
        for (input, expected) in [
            (
                b"fn f(){unsafe{}}".as_slice(),
                "fn f() {\n    unsafe {}\n}\n",
            ),
            (
                b"fn f(){unsafe{operate()}}".as_slice(),
                "fn f() {\n    unsafe { operate() }\n}\n",
            ),
            (
                b"fn f(){unsafe{unsafe{operate()}}}".as_slice(),
                "fn f() {\n    unsafe { unsafe { operate() } }\n}\n",
            ),
            (
                b"fn f(){unsafe{let value=1}}".as_slice(),
                "fn f() {\n    unsafe {\n        let value = 1\n    }\n}\n",
            ),
            (
                b"fn f(){unsafe{one()\ntwo()}}".as_slice(),
                "fn f() {\n    unsafe {\n        one()\n        two()\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("unsafe block parses");
            let output = format_shape(input).expect("unsafe block is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted unsafe block parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let operation = format!("operation_{}", "x".repeat(101));
        let input = format!("fn f(){{unsafe{{{operation}}}}}");
        let expected = format!("fn f() {{\n    unsafe {{\n        {operation}\n    }}\n}}\n");
        let output = format_shape(input.as_bytes()).expect("overlong unsafe expression formats");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("broken unsafe block reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let nested_operation = format!("nested_{}", "y".repeat(92));
        let input = format!("fn f(){{unsafe{{unsafe{{{nested_operation}}}}}}}");
        let expected = format!(
            "fn f() {{\n    unsafe {{\n        unsafe {{\n            {nested_operation}\n        }}\n    }}\n}}\n"
        );
        let output = format_shape(input.as_bytes()).expect("nested overlong unsafe block formats");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("nested broken unsafe block reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
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
            format_shape(b"fn f(){let (x,y)=pair\nx}"),
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
                            SyntaxKind::PARAM_LIST
                            | SyntaxKind::ARG_LIST
                            | SyntaxKind::FN_PTR_TYPE
                            | SyntaxKind::FN_SUGAR_BOUND => Some(SyntaxKind::R_PAREN),
                            SyntaxKind::GENERIC_ARG_LIST | SyntaxKind::GENERIC_PARAM_LIST => {
                                Some(SyntaxKind::GT)
                            }
                            SyntaxKind::TUPLE_TYPE
                                if node.children().filter_map(Type::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::TUPLE_EXPR
                                if node.children().filter_map(Expr::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::ARRAY_EXPR => Some(SyntaxKind::R_BRACKET),
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

    fn contract_expression_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        let clause = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::REQUIRES_CLAUSE)
            .expect("requires clause exists");
        let expression = clause
            .children()
            .find_map(Expr::cast)
            .expect("requires expression exists");
        match expression {
            Expr::Paren(paren) => {
                let inner = paren.expression().expect("parenthesized expression exists");
                syntax_shape(inner.syntax())
            }
            expression => syntax_shape(expression.syntax()),
        }
    }
}
