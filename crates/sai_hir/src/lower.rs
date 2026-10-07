use crate::Symbol;
use crate::arena::HirArena;
use crate::expressions::{
    Arg, Arm as HirArm, BinOp, Block as HirBlock, Expr, ExprKind, Label as HirLabel,
    MatchSource as HirMatchSource, Stmt as HirStmt, StmtKind, UnOp,
};
use crate::literals::Lit;
use crate::patterns::{BindingMode as HirBindingMode, Pat, PatArg, PatKind, PatRangeEnd};
use crate::source_map::{HirSourceMapBuilder, HirSourceMapError};
use crate::types::{Name, Path, PathRoot, PathSegment};
use crate::{BindingId, DesugaringKind, Mutability, Safety, Span, SpanCtxt};
use sai_diagnostics::Code;
use sai_intern::{InternError, Interner};
use sai_syntax::{
    SyntaxElement, SyntaxKind, SyntaxNode,
    ast::{self, AstNode, AstToken},
};

/// Error returned when lowering cannot allocate source data or encounters a
/// syntax form whose HIR conversion has not been implemented yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirLoweringError {
    SourceMap(HirSourceMapError),
    Interner(InternError),
    UnsupportedSyntax(SyntaxKind),
}

impl From<HirSourceMapError> for HirLoweringError {
    fn from(error: HirSourceMapError) -> Self {
        Self::SourceMap(error)
    }
}

impl From<InternError> for HirLoweringError {
    fn from(error: InternError) -> Self {
        Self::Interner(error)
    }
}

/// Lower one syntax literal to a HIR expression and record its source span.
///
/// The source-map builder validates that the syntax node belongs to its
/// snapshot-bound file root before assigning the HIR ID. Malformed literals
/// become `ExprKind::Err` because their lexical or parser diagnostic already
/// exists.
pub fn lower_literal<'hir>(
    literal: &ast::LiteralExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    lower_literal_with_sign(literal, literal.syntax(), false, arena, symbols, source_map)
}

fn lower_literal_with_sign<'hir>(
    literal: &ast::LiteralExpr,
    expression_syntax: &SyntaxNode,
    negated: bool,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(expression_syntax, SpanCtxt::Root)?;
    let kind = if contains_error_node(expression_syntax) {
        ExprKind::Err
    } else {
        literal
            .token()
            .map(|token| decode_literal(token, arena, symbols, true, negated))
            .transpose()?
            .flatten()
            .map(ExprKind::Lit)
            .unwrap_or(ExprKind::Err)
    };
    Ok(arena.alloc_expr(Expr { hir_id, kind, span }))
}

/// Lower the currently supported expression forms and record their source spans.
///
/// Valid but not-yet-supported expressions are rejected before any HIR nodes
/// or source-map entries are allocated.
pub fn lower_expr<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    ensure_supported_expr(expression)?;
    if contains_error_node(expression.syntax()) && !contains_call_arglist_error(expression.syntax())
    {
        return lower_error_expr(expression.syntax(), arena, source_map);
    }
    lower_expr_inner(expression, arena, symbols, source_map)
}

fn lower_expr_inner<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    match expression {
        ast::Expr::Literal(literal) => lower_literal(literal, arena, symbols, source_map),
        ast::Expr::Binary(binary) => lower_binary_expr(binary, arena, symbols, source_map),
        ast::Expr::Prefix(prefix) => lower_prefix_expr(prefix, arena, symbols, source_map),
        ast::Expr::Range(range) => lower_range_expr(range, arena, symbols, source_map),
        ast::Expr::Index(index) => lower_index_expr(index, arena, symbols, source_map),
        ast::Expr::Try(try_expr) => lower_try_expr(try_expr, arena, symbols, source_map),
        ast::Expr::Await(await_expr) => lower_await_expr(await_expr, arena, symbols, source_map),
        ast::Expr::Call(call) => lower_call_expr(call, arena, symbols, source_map),
        ast::Expr::Return(return_expr) => {
            lower_return_expr(return_expr, arena, symbols, source_map)
        }
        ast::Expr::Break(break_expr) => lower_break_expr(break_expr, arena, symbols, source_map),
        ast::Expr::Continue(continue_expr) => {
            lower_continue_expr(continue_expr, arena, symbols, source_map)
        }
        ast::Expr::Block(block_expr) => lower_block_expr(block_expr, arena, symbols, source_map),
        ast::Expr::If(if_expr) => lower_if_expr(if_expr, arena, symbols, source_map),
        ast::Expr::Match(match_expr) => lower_match_expr(match_expr, arena, symbols, source_map),
        ast::Expr::Path(path) => lower_path_expr(path, arena, symbols, source_map),
        ast::Expr::SelfValue(self_value) => {
            lower_root_path_expr(self_value.syntax(), PathRoot::SelfValue, arena, source_map)
        }
        ast::Expr::SelfType(self_type) => {
            lower_root_path_expr(self_type.syntax(), PathRoot::SelfType, arena, source_map)
        }
        ast::Expr::ImplicitMember(member) => {
            lower_implicit_member_expr(member, arena, symbols, source_map)
        }
        ast::Expr::Field(field) => lower_field_expr(field, arena, symbols, source_map),
        ast::Expr::TupleIndex(index) => lower_tuple_index_expr(index, arena, symbols, source_map),
        ast::Expr::Unit(unit) => {
            let (hir_id, span) = source_map.add_node_with_span(unit.syntax(), SpanCtxt::Root)?;
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Tuple(arena.alloc_exprs(&[])),
                span,
            }))
        }
        ast::Expr::Paren(paren) => match paren.expression() {
            Some(inner) => lower_expr_inner(&inner, arena, symbols, source_map),
            None => lower_error_expr(paren.syntax(), arena, source_map),
        },
        ast::Expr::Tuple(tuple) => {
            let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
            let mut elements = Vec::new();
            for element in collect_expr_list_elements(tuple.syntax()) {
                let lowered = match element {
                    ExprListElementSyntax::Expression(expression) => {
                        lower_expr_inner(&expression, arena, symbols, source_map)?
                    }
                    ExprListElementSyntax::Error(syntax) => {
                        lower_error_expr(&syntax, arena, source_map)?
                    }
                };
                elements.push(*lowered);
            }
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Tuple(arena.alloc_exprs(&elements)),
                span,
            }))
        }
        ast::Expr::Array(array) => {
            let (hir_id, span) = source_map.add_node_with_span(array.syntax(), SpanCtxt::Root)?;
            let mut elements = Vec::new();
            for element in collect_expr_list_elements(array.syntax()) {
                let lowered = match element {
                    ExprListElementSyntax::Expression(expression) => {
                        lower_expr_inner(&expression, arena, symbols, source_map)?
                    }
                    ExprListElementSyntax::Error(syntax) => {
                        lower_error_expr(&syntax, arena, source_map)?
                    }
                };
                elements.push(*lowered);
            }
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Array(arena.alloc_exprs(&elements)),
                span,
            }))
        }
        _ if contains_error_node(expression.syntax()) => {
            lower_error_expr(expression.syntax(), arena, source_map)
        }
        _ => Err(HirLoweringError::UnsupportedSyntax(
            expression.syntax().kind(),
        )),
    }
}

fn lower_path_expr<'hir>(
    path: &ast::PathExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    if path.generic_args().is_some() {
        return Err(HirLoweringError::UnsupportedSyntax(path.syntax().kind()));
    }
    let Some(name) = path.name() else {
        return lower_error_expr(path.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(path.syntax(), SpanCtxt::Root)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(path.syntax(), SpanCtxt::Root)?;
    let (segment_hir_id, _) = source_map.add_node_with_span(name.syntax(), SpanCtxt::Root)?;
    let segment = PathSegment {
        hir_id: segment_hir_id,
        name: lower_name_ref(&name, symbols, source_map)?,
        args: None,
    };
    let path = Path {
        hir_id: path_hir_id,
        root: PathRoot::Plain,
        segments: arena.alloc_path_segments(&[segment]),
        span: path_span,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(path),
        span,
    }))
}

fn lower_root_path_expr<'hir>(
    syntax: &SyntaxNode,
    root: PathRoot,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let path = Path {
        hir_id: path_hir_id,
        root,
        segments: arena.alloc_path_segments(&[]),
        span: path_span,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(path),
        span,
    }))
}

fn lower_implicit_member_expr<'hir>(
    member: &ast::ImplicitMemberExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(name) = member.name() else {
        return lower_error_expr(member.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(member.syntax(), SpanCtxt::Root)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::ImplicitMember(name),
        span,
    }))
}

fn lower_field_expr<'hir>(
    field: &ast::FieldExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    if has_direct_generic_args(field.syntax()) {
        return Err(HirLoweringError::UnsupportedSyntax(field.syntax().kind()));
    }
    let (Some(base), Some(name)) = (field.base(), field.name()) else {
        return lower_error_expr(field.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(field.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Field {
            base,
            name,
            args: None,
        },
        span,
    }))
}

fn lower_tuple_index_expr<'hir>(
    index: &ast::TupleIndexExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(base) = index.base() else {
        return lower_error_expr(index.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(index.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::TupleField {
            base,
            index: index.index(),
        },
        span,
    }))
}

fn lower_binary_expr<'hir>(
    binary: &ast::BinExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(lhs), Some(rhs), Some((_, op))) = (binary.lhs(), binary.rhs(), binary.op()) else {
        return lower_error_expr(binary.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(binary.syntax(), SpanCtxt::Root)?;
    let lhs = lower_expr_inner(&lhs, arena, symbols, source_map)?;
    let rhs = lower_expr_inner(&rhs, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Binary(lower_bin_op(op), lhs, rhs),
        span,
    }))
}

fn lower_prefix_expr<'hir>(
    prefix: &ast::PrefixExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some((_, op)), Some(operand)) = (prefix.op(), prefix.operand()) else {
        return lower_error_expr(prefix.syntax(), arena, source_map);
    };
    if op == ast::PrefixOp::Neg
        && let ast::Expr::Literal(ref literal) = operand
        && matches!(
            literal.token(),
            Some(ast::LiteralToken::Int(_) | ast::LiteralToken::Float(_))
        )
    {
        return lower_literal_with_sign(literal, prefix.syntax(), true, arena, symbols, source_map);
    }

    let (hir_id, span) = source_map.add_node_with_span(prefix.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    let kind = match op {
        ast::PrefixOp::Neg => ExprKind::Unary(UnOp::Neg, operand),
        ast::PrefixOp::BitNot => ExprKind::Unary(UnOp::BitNot, operand),
        ast::PrefixOp::Deref => ExprKind::Unary(UnOp::Deref, operand),
        ast::PrefixOp::Not => ExprKind::Unary(UnOp::Not, operand),
        ast::PrefixOp::Borrow => ExprKind::Borrow(Mutability::Not, operand),
        ast::PrefixOp::BorrowMut => ExprKind::Borrow(Mutability::Mut, operand),
    };
    Ok(arena.alloc_expr(Expr { hir_id, kind, span }))
}

fn lower_range_expr<'hir>(
    range: &ast::RangeExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(inclusive) = range_inclusive(range.syntax()) else {
        return lower_error_expr(range.syntax(), arena, source_map);
    };
    if inclusive && range.end().is_none() {
        return lower_error_expr(range.syntax(), arena, source_map);
    }
    let (hir_id, span) = source_map.add_node_with_span(range.syntax(), SpanCtxt::Root)?;
    let start = range
        .start()
        .map(|start| lower_expr_inner(&start, arena, symbols, source_map))
        .transpose()?;
    let end = range
        .end()
        .map(|end| lower_expr_inner(&end, arena, symbols, source_map))
        .transpose()?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Range {
            start,
            end,
            inclusive,
        },
        span,
    }))
}

fn range_inclusive(syntax: &SyntaxNode) -> Option<bool> {
    syntax
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2 => Some(false),
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2EQ => Some(true),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
}

fn lower_index_expr<'hir>(
    index: &ast::IndexExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(base), Some(index_expr)) = (index.base(), index.index()) else {
        return lower_error_expr(index.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(index.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    let index_expr = lower_expr_inner(&index_expr, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Index(base, index_expr),
        span,
    }))
}

fn lower_try_expr<'hir>(
    try_expr: &ast::TryExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(operand) = try_expr.operand() else {
        return lower_error_expr(try_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(try_expr.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Try(operand),
        span,
    }))
}

fn lower_await_expr<'hir>(
    await_expr: &ast::AwaitExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(operand) = await_expr.operand() else {
        return lower_error_expr(await_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(await_expr.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Await(operand),
        span,
    }))
}

enum CallArgSyntax {
    Positional(ast::Expr),
    PositionalError(SyntaxNode),
    Named {
        name: ast::NameRef,
        expression: ast::Expr,
    },
    NamedError {
        name: ast::NameRef,
        syntax: SyntaxNode,
    },
    Base(ast::Expr),
    BaseError(SyntaxNode),
}

enum ExprListElementSyntax {
    Expression(ast::Expr),
    Error(SyntaxNode),
}

enum BlockStatementSyntax {
    Statement(ast::Stmt),
    Error(SyntaxNode),
}

enum MatchArmSyntax {
    Arm(ast::MatchArm),
    Error(SyntaxNode),
}

fn collect_expr_list_elements(syntax: &SyntaxNode) -> Vec<ExprListElementSyntax> {
    let mut elements = Vec::new();
    for child in syntax.children() {
        if let Some(expression) = ast::Expr::cast(child.clone()) {
            elements.push(ExprListElementSyntax::Expression(expression));
        } else if child.kind() == SyntaxKind::ERROR && !contains_attribute(&child) {
            elements.push(ExprListElementSyntax::Error(child));
        }
    }
    elements
}

fn collect_block_statements(syntax: &SyntaxNode) -> Vec<BlockStatementSyntax> {
    let mut statements = Vec::new();
    for child in syntax.children() {
        if let Some(statement) = ast::Stmt::cast(child.clone()) {
            statements.push(BlockStatementSyntax::Statement(statement));
        } else if child.kind() == SyntaxKind::ERROR {
            statements.push(BlockStatementSyntax::Error(child));
        }
    }
    statements
}

fn collect_match_arms(match_expr: &ast::MatchExpr) -> Vec<MatchArmSyntax> {
    match match_expr.arms() {
        Some(arms) => arms
            .syntax()
            .children()
            .filter_map(|child| {
                ast::MatchArm::cast(child.clone())
                    .map(MatchArmSyntax::Arm)
                    .or_else(|| {
                        (child.kind() == SyntaxKind::ERROR)
                            .then_some(child)
                            .map(MatchArmSyntax::Error)
                    })
            })
            .collect(),
        None => {
            let mut errors = match_expr
                .syntax()
                .children()
                .filter(|child| child.kind() == SyntaxKind::ERROR)
                .collect::<Vec<_>>();
            if match_expr.scrutinee().is_none()
                && let Some(scrutinee_error) =
                    errors.iter().position(|error| !contains_attribute(error))
            {
                errors.remove(scrutinee_error);
            }
            if errors.is_empty() {
                vec![MatchArmSyntax::Error(match_expr.syntax().clone())]
            } else {
                errors.into_iter().map(MatchArmSyntax::Error).collect()
            }
        }
    }
}

fn collect_call_args(call: &ast::CallExpr) -> Option<Vec<CallArgSyntax>> {
    let args = call.args()?;
    let mut collected = Vec::new();
    let mut has_base = false;
    for child in args.syntax().children() {
        if let Some(arg) = ast::Arg::cast(child.clone()) {
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Positional(expression));
            } else {
                collected.push(CallArgSyntax::PositionalError(direct_error_child(
                    arg.syntax(),
                )?));
            }
        } else if let Some(arg) = ast::NamedArg::cast(child.clone()) {
            let name = arg.name()?;
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Named { name, expression });
            } else {
                collected.push(CallArgSyntax::NamedError {
                    name,
                    syntax: direct_error_child(arg.syntax())?,
                });
            }
        } else if let Some(arg) = ast::BaseArg::cast(child.clone()) {
            if has_base {
                return None;
            }
            has_base = true;
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Base(expression));
            } else {
                collected.push(CallArgSyntax::BaseError(direct_error_child(arg.syntax())?));
            }
        } else if child.kind() == SyntaxKind::ERROR && !contains_attribute(&child) {
            // Recovery creates no Arg node for an empty expression such as
            // `call(, value)`. Keep its position so later stages see an Err
            // argument instead of a call with a silently shortened list.
            collected.push(CallArgSyntax::PositionalError(child));
        }
    }
    Some(collected)
}

fn call_argument_expressions(call: &ast::CallExpr) -> Vec<ast::Expr> {
    let Some(args) = call.args() else {
        return Vec::new();
    };
    args.syntax()
        .children()
        .filter_map(|child| {
            ast::Arg::cast(child.clone())
                .and_then(|arg| arg.expression())
                .or_else(|| ast::NamedArg::cast(child.clone()).and_then(|arg| arg.expression()))
                .or_else(|| ast::BaseArg::cast(child).and_then(|arg| arg.expression()))
        })
        .collect()
}

fn direct_error_child(syntax: &SyntaxNode) -> Option<SyntaxNode> {
    syntax
        .children()
        .find(|child| child.kind() == SyntaxKind::ERROR && !contains_attribute(child))
}

fn direct_error_child_from_end(syntax: &SyntaxNode) -> Option<SyntaxNode> {
    syntax
        .children()
        .filter(|child| child.kind() == SyntaxKind::ERROR && !contains_attribute(child))
        .collect::<Vec<_>>()
        .into_iter()
        .next_back()
}

fn contains_attribute(syntax: &SyntaxNode) -> bool {
    syntax
        .descendants()
        .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
}

fn contains_call_arglist_error(syntax: &SyntaxNode) -> bool {
    let root_call = ast::CallExpr::cast(syntax.clone());
    root_call
        .into_iter()
        .chain(syntax.descendants().filter_map(ast::CallExpr::cast))
        .any(|call| {
            call.args()
                .is_some_and(|args| contains_error_node(args.syntax()))
        })
}

fn lower_call_expr<'hir>(
    call: &ast::CallExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(callee), Some(args)) = (call.callee(), collect_call_args(call)) else {
        return lower_error_expr(call.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(call.syntax(), SpanCtxt::Root)?;
    let callee = lower_expr_inner(&callee, arena, symbols, source_map)?;
    let mut lowered_args = Vec::with_capacity(args.len());
    let mut base = None;
    for arg in args {
        match arg {
            CallArgSyntax::Positional(expression) => {
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::PositionalError(syntax) => {
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::Named { name, expression } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::NamedError { name, syntax } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::Base(expression) => {
                base = Some(lower_expr_inner(&expression, arena, symbols, source_map)?);
            }
            CallArgSyntax::BaseError(syntax) => {
                base = Some(lower_error_expr(&syntax, arena, source_map)?);
            }
        }
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Call {
            callee,
            args: arena.alloc_args(&lowered_args),
            base,
        },
        span,
    }))
}

fn lower_return_expr<'hir>(
    return_expr: &ast::ReturnExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(return_expr.syntax(), SpanCtxt::Root)?;
    let value = lower_optional_expr_value(
        return_expr.syntax(),
        return_expr.value(),
        arena,
        symbols,
        source_map,
    )?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Return(value),
        span,
    }))
}

fn lower_break_expr<'hir>(
    break_expr: &ast::BreakExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(break_expr.syntax(), SpanCtxt::Root)?;
    let label = lower_label(break_expr.label(), symbols, source_map)?;
    let value = lower_optional_expr_value(
        break_expr.syntax(),
        break_expr.value(),
        arena,
        symbols,
        source_map,
    )?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Break { label, value },
        span,
    }))
}

fn lower_optional_expr_value<'hir>(
    syntax: &SyntaxNode,
    value: Option<ast::Expr>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Option<&'hir Expr<'hir>>, HirLoweringError> {
    if let Some(value) = value {
        Ok(Some(lower_expr_inner(&value, arena, symbols, source_map)?))
    } else if let Some(error) = direct_error_child(syntax) {
        Ok(Some(lower_error_expr(&error, arena, source_map)?))
    } else {
        Ok(None)
    }
}

fn lower_continue_expr<'hir>(
    continue_expr: &ast::ContinueExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(continue_expr.syntax(), SpanCtxt::Root)?;
    let label = lower_label(continue_expr.label(), symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Continue { label },
        span,
    }))
}

fn lower_block_expr<'hir>(
    block_expr: &ast::BlockExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(block) = block_expr.block() else {
        return lower_error_expr(block_expr.syntax(), arena, source_map);
    };
    lower_block_as_expr(block_expr.syntax(), &block, arena, symbols, source_map)
}

fn lower_block_as_expr<'hir>(
    syntax: &SyntaxNode,
    block: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let block = lower_block(block, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Block(block),
        span,
    }))
}

fn lower_if_expr<'hir>(
    if_expr: &ast::IfExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(then_branch) = if_expr.then_branch() else {
        return lower_error_expr(if_expr.syntax(), arena, source_map);
    };
    if let Some(let_condition) = if_expr
        .condition()
        .and_then(|condition| condition.let_condition())
    {
        return lower_if_let_expr(
            if_expr,
            &let_condition,
            &then_branch,
            arena,
            symbols,
            source_map,
        );
    }
    let (hir_id, span) = source_map.add_node_with_span(if_expr.syntax(), SpanCtxt::Root)?;
    let cond = match if_expr.condition() {
        Some(condition) => match condition.expression() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
            None => match direct_error_child(condition.syntax()) {
                Some(error) => lower_error_expr(&error, arena, source_map)?,
                None => lower_error_expr(condition.syntax(), arena, source_map)?,
            },
        },
        None => lower_error_expr(if_expr.syntax(), arena, source_map)?,
    };
    let then_branch = lower_block(&then_branch, arena, symbols, source_map)?;
    let els = if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            Some(lower_block_as_expr(
                block.syntax(),
                &block,
                arena,
                symbols,
                source_map,
            )?)
        } else if let Some(else_if) = else_branch.else_if() {
            Some(lower_if_expr(&else_if, arena, symbols, source_map)?)
        } else {
            let error = direct_error_child(else_branch.syntax())
                .unwrap_or_else(|| else_branch.syntax().clone());
            Some(lower_error_expr(&error, arena, source_map)?)
        }
    } else {
        None
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::If {
            cond,
            then: then_branch,
            els,
        },
        span,
    }))
}

fn lower_if_let_expr<'hir>(
    if_expr: &ast::IfExpr,
    condition: &ast::LetCondition,
    then_branch: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let desugared = SpanCtxt::Desugared(DesugaringKind::IfLet);
    let (hir_id, span) = source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let scrutinee = match condition.initializer() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_from_end(condition.syntax())
                .unwrap_or_else(|| condition.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let (arm_hir_id, arm_span) = source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let pattern = match condition.pattern() {
        Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(condition.syntax())
                .unwrap_or_else(|| condition.syntax().clone());
            lower_error_pattern(&error, arena, source_map)?
        }
    };
    let body = lower_block_as_expr(
        then_branch.syntax(),
        then_branch,
        arena,
        symbols,
        source_map,
    )?;
    let user_arm = HirArm {
        hir_id: arm_hir_id,
        pat: pattern,
        guard: None,
        body,
        span: arm_span,
        synthesized: false,
    };

    let (fallback_arm_hir_id, fallback_arm_span) =
        source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let (wildcard_hir_id, wildcard_span) =
        source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let wildcard = arena.alloc_pat(Pat {
        hir_id: wildcard_hir_id,
        kind: PatKind::Wild,
        span: wildcard_span,
    });
    let fallback_body = if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            lower_block_as_expr(block.syntax(), &block, arena, symbols, source_map)?
        } else if let Some(else_if) = else_branch.else_if() {
            lower_if_expr(&else_if, arena, symbols, source_map)?
        } else {
            let error = direct_error_child(else_branch.syntax())
                .unwrap_or_else(|| else_branch.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    } else {
        let (empty_hir_id, empty_span) =
            source_map.add_node_with_span(if_expr.syntax(), desugared)?;
        arena.alloc_expr(Expr {
            hir_id: empty_hir_id,
            kind: ExprKind::Tuple(arena.alloc_exprs(&[])),
            span: empty_span,
        })
    };
    let fallback_arm = HirArm {
        hir_id: fallback_arm_hir_id,
        pat: wildcard,
        guard: None,
        body: fallback_body,
        span: fallback_arm_span,
        synthesized: true,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Match {
            scrutinee,
            arms: arena.alloc_arms(&[user_arm, fallback_arm]),
            source: HirMatchSource::IfLet,
        },
        span,
    }))
}

fn lower_match_expr<'hir>(
    match_expr: &ast::MatchExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(match_expr.syntax(), SpanCtxt::Root)?;
    let scrutinee = match match_expr.scrutinee() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(match_expr.syntax())
                .unwrap_or_else(|| match_expr.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let mut arms = Vec::new();
    for arm in collect_match_arms(match_expr) {
        arms.push(match arm {
            MatchArmSyntax::Arm(arm) => lower_match_arm(&arm, arena, symbols, source_map)?,
            MatchArmSyntax::Error(error) => {
                let (arm_hir_id, arm_span) =
                    source_map.add_node_with_span(&error, SpanCtxt::Root)?;
                let pattern = lower_error_pattern(&error, arena, source_map)?;
                let body = lower_error_expr(&error, arena, source_map)?;
                HirArm {
                    hir_id: arm_hir_id,
                    pat: pattern,
                    guard: None,
                    body,
                    span: arm_span,
                    synthesized: false,
                }
            }
        });
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Match {
            scrutinee,
            arms: arena.alloc_arms(&arms),
            source: HirMatchSource::Normal,
        },
        span,
    }))
}

fn lower_match_arm<'hir>(
    arm: &ast::MatchArm,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirArm<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(arm.syntax(), SpanCtxt::Root)?;
    let pattern = match arm.pattern() {
        Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(arm.syntax()).unwrap_or_else(|| arm.syntax().clone());
            lower_error_pattern(&error, arena, source_map)?
        }
    };
    let guard = arm
        .guard()
        .map(|guard| match guard.expression() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map),
            None => {
                let error =
                    direct_error_child(guard.syntax()).unwrap_or_else(|| guard.syntax().clone());
                lower_error_expr(&error, arena, source_map)
            }
        })
        .transpose()?;
    let body = match arm.expression() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error =
                direct_error_child_from_end(arm.syntax()).unwrap_or_else(|| arm.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    Ok(HirArm {
        hir_id,
        pat: pattern,
        guard,
        body,
        span,
        synthesized: false,
    })
}

fn lower_block<'hir>(
    block: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirBlock<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(block.syntax(), SpanCtxt::Root)?;
    let entries = collect_block_statements(block.syntax());
    let entry_count = entries.len();
    let mut statements = Vec::new();
    let mut tail = None;
    let last_is_expression = matches!(
        entries.last(),
        Some(BlockStatementSyntax::Statement(ast::Stmt::Expr(_)))
    );

    for (index, entry) in entries.into_iter().enumerate() {
        match entry {
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement))
                if last_is_expression && index + 1 == entry_count =>
            {
                tail = Some(match expression_statement.expression() {
                    Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
                    None => match direct_error_child(expression_statement.syntax()) {
                        Some(error) => lower_error_expr(&error, arena, source_map)?,
                        None => lower_error_expr(expression_statement.syntax(), arena, source_map)?,
                    },
                });
            }
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement)) => {
                let (stmt_hir_id, stmt_span) =
                    source_map.add_node_with_span(expression_statement.syntax(), SpanCtxt::Root)?;
                let expression = match expression_statement.expression() {
                    Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
                    None => match direct_error_child(expression_statement.syntax()) {
                        Some(error) => lower_error_expr(&error, arena, source_map)?,
                        None => lower_error_expr(expression_statement.syntax(), arena, source_map)?,
                    },
                };
                statements.push(HirStmt {
                    hir_id: stmt_hir_id,
                    kind: StmtKind::Expr(expression),
                    span: stmt_span,
                });
            }
            BlockStatementSyntax::Statement(statement) => {
                return Err(HirLoweringError::UnsupportedSyntax(
                    statement.syntax().kind(),
                ));
            }
            BlockStatementSyntax::Error(error) => {
                let (stmt_hir_id, stmt_span) =
                    source_map.add_node_with_span(&error, SpanCtxt::Root)?;
                let expression = lower_error_expr(&error, arena, source_map)?;
                statements.push(HirStmt {
                    hir_id: stmt_hir_id,
                    kind: StmtKind::Expr(expression),
                    span: stmt_span,
                });
            }
        }
    }

    Ok(arena.alloc_block(HirBlock {
        hir_id,
        stmts: arena.alloc_stmts(&statements),
        tail,
        safety: Safety::Safe,
        span,
    }))
}

fn lower_label(
    label: Option<ast::Label>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Option<HirLabel>, HirLoweringError> {
    label
        .map(|label| {
            let token = label.syntax();
            let parent_span = source_map.span_for(token.parent(), SpanCtxt::Root)?;
            let text = token.text().strip_prefix('\'').unwrap_or(token.text());
            let normalized = sai_syntax::normalize_identifier(text);
            Ok(HirLabel {
                name: symbols.intern(&normalized)?,
                span: Span {
                    range: token.text_range(),
                    ..parent_span
                },
            })
        })
        .transpose()
}

fn lower_bin_op(op: ast::BinOp) -> BinOp {
    match op {
        ast::BinOp::Or => BinOp::Or,
        ast::BinOp::And => BinOp::And,
        ast::BinOp::Eq => BinOp::Eq,
        ast::BinOp::Ne => BinOp::Ne,
        ast::BinOp::Lt => BinOp::Lt,
        ast::BinOp::Le => BinOp::Le,
        ast::BinOp::Gt => BinOp::Gt,
        ast::BinOp::Ge => BinOp::Ge,
        ast::BinOp::BitOr => BinOp::BitOr,
        ast::BinOp::BitXor => BinOp::BitXor,
        ast::BinOp::BitAnd => BinOp::BitAnd,
        ast::BinOp::Shl => BinOp::Shl,
        ast::BinOp::Shr => BinOp::Shr,
        ast::BinOp::Add => BinOp::Add,
        ast::BinOp::Sub => BinOp::Sub,
        ast::BinOp::WrappingAdd => BinOp::WrapAdd,
        ast::BinOp::WrappingSub => BinOp::WrapSub,
        ast::BinOp::Mul => BinOp::Mul,
        ast::BinOp::Div => BinOp::Div,
        ast::BinOp::Rem => BinOp::Rem,
        ast::BinOp::WrappingMul => BinOp::WrapMul,
    }
}

fn lower_error_expr<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Err,
        span,
    }))
}

fn ensure_supported_expr(expression: &ast::Expr) -> Result<(), HirLoweringError> {
    match expression {
        ast::Expr::Literal(_) | ast::Expr::SelfValue(_) | ast::Expr::SelfType(_) => Ok(()),
        ast::Expr::Binary(binary) => {
            if let Some(lhs) = binary.lhs() {
                ensure_supported_expr(&lhs)?;
            }
            if let Some(rhs) = binary.rhs() {
                ensure_supported_expr(&rhs)?;
            }
            Ok(())
        }
        ast::Expr::Prefix(prefix) => match prefix.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Range(range) => {
            if let Some(start) = range.start() {
                ensure_supported_expr(&start)?;
            }
            if let Some(end) = range.end() {
                ensure_supported_expr(&end)?;
            }
            Ok(())
        }
        ast::Expr::Index(index) => {
            if let Some(base) = index.base() {
                ensure_supported_expr(&base)?;
            }
            if let Some(index_expr) = index.index() {
                ensure_supported_expr(&index_expr)?;
            }
            Ok(())
        }
        ast::Expr::Try(try_expr) => match try_expr.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Await(await_expr) => match await_expr.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Return(return_expr) => match return_expr.value() {
            Some(value) => ensure_supported_expr(&value),
            None => Ok(()),
        },
        ast::Expr::Break(break_expr) => match break_expr.value() {
            Some(value) => ensure_supported_expr(&value),
            None => Ok(()),
        },
        ast::Expr::Continue(_) => Ok(()),
        ast::Expr::Call(call) => {
            if let Some(callee) = call.callee() {
                ensure_supported_expr(&callee)?;
            }
            for expression in call_argument_expressions(call) {
                ensure_supported_expr(&expression)?;
            }
            Ok(())
        }
        ast::Expr::Path(path) => {
            if path.generic_args().is_none() {
                Ok(())
            } else {
                Err(HirLoweringError::UnsupportedSyntax(path.syntax().kind()))
            }
        }
        ast::Expr::Field(field) => {
            if has_direct_generic_args(field.syntax()) {
                return Err(HirLoweringError::UnsupportedSyntax(field.syntax().kind()));
            }
            match field.base() {
                Some(base) => ensure_supported_expr(&base),
                None => Ok(()),
            }
        }
        ast::Expr::TupleIndex(index) => match index.base() {
            Some(base) => ensure_supported_expr(&base),
            None => Ok(()),
        },
        ast::Expr::ImplicitMember(_) | ast::Expr::Unit(_) => Ok(()),
        ast::Expr::Paren(paren) => match paren.expression() {
            Some(inner) => ensure_supported_expr(&inner),
            None => Ok(()),
        },
        ast::Expr::Tuple(tuple) => {
            for element in tuple.elements() {
                ensure_supported_expr(&element)?;
            }
            Ok(())
        }
        ast::Expr::Array(array) => {
            for element in array.elements() {
                ensure_supported_expr(&element)?;
            }
            Ok(())
        }
        ast::Expr::Block(block_expr) => match block_expr.block() {
            Some(block) => ensure_supported_block(&block),
            None => Ok(()),
        },
        ast::Expr::If(if_expr) => ensure_supported_if_expr(if_expr),
        ast::Expr::Match(match_expr) => ensure_supported_match_expr(match_expr),
        _ if contains_error_node(expression.syntax()) => Ok(()),
        _ => Err(HirLoweringError::UnsupportedSyntax(
            expression.syntax().kind(),
        )),
    }
}

fn ensure_supported_block(block: &ast::Block) -> Result<(), HirLoweringError> {
    for entry in collect_block_statements(block.syntax()) {
        match entry {
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement)) => {
                if let Some(expression) = expression_statement.expression() {
                    ensure_supported_expr(&expression)?;
                }
            }
            BlockStatementSyntax::Statement(statement) => {
                return Err(HirLoweringError::UnsupportedSyntax(
                    statement.syntax().kind(),
                ));
            }
            BlockStatementSyntax::Error(_) => {}
        }
    }
    Ok(())
}

fn ensure_supported_if_expr(if_expr: &ast::IfExpr) -> Result<(), HirLoweringError> {
    if let Some(condition) = if_expr.condition() {
        if let Some(let_condition) = condition.let_condition() {
            if let Some(pattern) = let_condition.pattern() {
                ensure_supported_pattern(&pattern)?;
            }
            if let Some(expression) = let_condition.initializer() {
                ensure_supported_expr(&expression)?;
            }
        } else if let Some(expression) = condition.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    if let Some(then_branch) = if_expr.then_branch() {
        ensure_supported_block(&then_branch)?;
    }
    if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            ensure_supported_block(&block)?;
        } else if let Some(else_if) = else_branch.else_if() {
            ensure_supported_if_expr(&else_if)?;
        }
    }
    Ok(())
}

fn ensure_supported_match_expr(match_expr: &ast::MatchExpr) -> Result<(), HirLoweringError> {
    if let Some(scrutinee) = match_expr.scrutinee() {
        ensure_supported_expr(&scrutinee)?;
    }
    for arm in collect_match_arms(match_expr) {
        let MatchArmSyntax::Arm(arm) = arm else {
            continue;
        };
        if let Some(pattern) = arm.pattern() {
            ensure_supported_pattern(&pattern)?;
        }
        if let Some(guard) = arm.guard()
            && let Some(expression) = guard.expression()
        {
            ensure_supported_expr(&expression)?;
        }
        if let Some(expression) = arm.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    Ok(())
}

fn has_direct_generic_args(syntax: &SyntaxNode) -> bool {
    syntax
        .children()
        .any(|child| child.kind() == SyntaxKind::GENERIC_ARG_LIST)
}

/// Lower a literal pattern, preserving its direct minus sign for integer
/// patterns and mapping parser-rejected float patterns to `PatKind::Err`.
pub fn lower_literal_pat<'hir>(
    literal: &ast::LiteralPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(literal.syntax(), SpanCtxt::Root)?;
    let negative = literal.minus_token().is_some();
    let kind = if contains_error_node(literal.syntax()) {
        PatKind::Err
    } else {
        literal
            .value_token()
            .and_then(ast::LiteralToken::cast)
            .map(|token| decode_literal(token, arena, symbols, false, negative))
            .transpose()?
            .flatten()
            .map(PatKind::Lit)
            .unwrap_or(PatKind::Err)
    };
    Ok(arena.alloc_pat(Pat { hir_id, kind, span }))
}

/// Lower a range pattern with literal or constant-path endpoints.
///
/// Invalid endpoint syntax becomes `PatKind::Err`.
pub fn lower_range_pat<'hir>(
    range: &ast::RangePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(range.syntax()) {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }

    let start_syntax = range.start();
    let end_syntax = range.end();
    let has_invalid_path = [start_syntax.as_ref(), end_syntax.as_ref()]
        .into_iter()
        .flatten()
        .any(|endpoint| match endpoint {
            ast::RangePatEndpoint::ConstantPath(path) => const_path_is_invalid(path.clone()),
            ast::RangePatEndpoint::Literal(_) => false,
        });
    if has_invalid_path {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }
    let inclusive = range.syntax().children_with_tokens().any(|element| {
        matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2EQ)
    });
    let has_start = start_syntax.is_some();
    let has_end = end_syntax.is_some();
    let start = start_syntax
        .map(|endpoint| prepare_range_endpoint(endpoint, arena, symbols))
        .transpose()?
        .flatten();
    let end = end_syntax
        .map(|endpoint| prepare_range_endpoint(endpoint, arena, symbols))
        .transpose()?
        .flatten();
    let invalid_shape = if inclusive { !has_end } else { !has_start };
    let invalid_endpoint = (has_start && start.is_none()) || (has_end && end.is_none());
    if invalid_shape || invalid_endpoint {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }

    let (hir_id, span) = source_map.add_node_with_span(range.syntax(), SpanCtxt::Root)?;
    let start = start
        .map(|endpoint| lower_prepared_range_endpoint(endpoint, arena, symbols, source_map))
        .transpose()?;
    let end = end
        .map(|endpoint| lower_prepared_range_endpoint(endpoint, arena, symbols, source_map))
        .transpose()?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Range {
            start,
            end,
            inclusive,
        },
        span,
    }))
}

/// Lower the currently supported simple and recursive pattern forms.
///
/// Unsupported, otherwise-valid pattern kinds are returned explicitly so a
/// caller cannot mistake an unfinished lowering case for a source error.
pub fn lower_pattern<'hir>(
    pattern: &ast::Pattern,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    ensure_supported_pattern(pattern)?;
    if contains_error_node(pattern.syntax()) {
        return lower_error_pattern(pattern.syntax(), arena, source_map);
    }
    plan_pattern_bindings(pattern, symbols, source_map, None)?;
    lower_pattern_inner(pattern, arena, symbols, source_map)
}

fn lower_pattern_inner<'hir>(
    pattern: &ast::Pattern,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    match pattern {
        ast::Pattern::Wildcard(wildcard) => {
            let (hir_id, span) =
                source_map.add_node_with_span(wildcard.syntax(), SpanCtxt::Root)?;
            Ok(arena.alloc_pat(Pat {
                hir_id,
                kind: PatKind::Wild,
                span,
            }))
        }
        ast::Pattern::Literal(literal) => lower_literal_pat(literal, arena, symbols, source_map),
        ast::Pattern::Range(range) => lower_range_pat(range, arena, symbols, source_map),
        ast::Pattern::Tuple(tuple) => lower_tuple_pattern(tuple, arena, symbols, source_map),
        ast::Pattern::Slice(slice) => lower_slice_pattern(slice, arena, symbols, source_map),
        ast::Pattern::Binding(binding) => {
            lower_binding_pattern(binding, arena, symbols, source_map)
        }
        ast::Pattern::At(at) => lower_at_pattern(at, arena, symbols, source_map),
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_some_and(const_path_is_invalid) =>
        {
            lower_error_pattern(constructor.syntax(), arena, source_map)
        }
        ast::Pattern::Constructor(constructor) => {
            lower_constructor_pattern(constructor, arena, symbols, source_map)
        }
        ast::Pattern::Or(or) => lower_or_pattern(or, arena, symbols, source_map),
        ast::Pattern::ImplicitMember(member) => {
            lower_implicit_member_pattern(member, arena, symbols, source_map)
        }
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => lower_pattern_inner(&inner, arena, symbols, source_map),
            None => lower_error_pattern(paren.syntax(), arena, source_map),
        },
    }
}

fn lower_error_pattern<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Err,
        span,
    }))
}

fn lower_or_pattern<'hir>(
    or: &ast::OrPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let alternatives = flattened_or_alternatives(or);
    match alternatives.as_slice() {
        [] => lower_error_pattern(or.syntax(), arena, source_map),
        [only] => lower_pattern_inner(only, arena, symbols, source_map),
        _ => {
            let (hir_id, span) = source_map.add_node_with_span(or.syntax(), SpanCtxt::Root)?;
            let alternatives = alternatives
                .iter()
                .map(|alternative| {
                    lower_pattern_inner(alternative, arena, symbols, source_map).copied()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(arena.alloc_pat(Pat {
                hir_id,
                kind: PatKind::Or(arena.alloc_pats(&alternatives)),
                span,
            }))
        }
    }
}

fn lower_tuple_pattern<'hir>(
    tuple: &ast::TuplePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
    let elements = tuple
        .elements()
        .map(|element| lower_pattern_inner(&element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let elements = arena.alloc_pats(&elements);
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Tuple(elements),
        span,
    }))
}

fn lower_slice_pattern<'hir>(
    slice: &ast::SlicePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(slice.syntax()) {
        return lower_error_pattern(slice.syntax(), arena, source_map);
    }

    let rests = slice.rest_patterns().collect::<Vec<_>>();
    if rests.len() > 1 {
        return lower_error_pattern(slice.syntax(), arena, source_map);
    }
    let elements = slice.elements().collect::<Vec<_>>();
    let split = rests.first().map_or(elements.len(), |rest| {
        let rest_start = rest.syntax().text_range().start();
        elements
            .iter()
            .position(|element| element.syntax().text_range().start() > rest_start)
            .unwrap_or(elements.len())
    });

    let (hir_id, span) = source_map.add_node_with_span(slice.syntax(), SpanCtxt::Root)?;
    let before = elements[..split]
        .iter()
        .map(|element| lower_pattern_inner(element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let rest = match rests.first() {
        None => None,
        Some(rest) => Some(
            rest.binding()
                .map(|binding| lower_binding_pattern(&binding, arena, symbols, source_map))
                .transpose()?,
        ),
    };
    let after = elements[split..]
        .iter()
        .map(|element| lower_pattern_inner(element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let before = arena.alloc_pats(&before);
    let after = arena.alloc_pats(&after);
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Slice {
            before,
            rest,
            after,
        },
        span,
    }))
}

fn lower_implicit_member_pattern<'hir>(
    member: &ast::ImplicitMemberPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(member.syntax()) {
        return lower_error_pattern(member.syntax(), arena, source_map);
    }
    let Some(name) = member.name() else {
        return lower_error_pattern(member.syntax(), arena, source_map);
    };
    let args = match member.args() {
        Some(args) => match collect_ctor_pat_args(&args) {
            Some((args, has_rest)) => Some((args, has_rest)),
            None => return lower_error_pattern(member.syntax(), arena, source_map),
        },
        None => None,
    };

    let (hir_id, span) = source_map.add_node_with_span(member.syntax(), SpanCtxt::Root)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    let (args, has_rest) = match args {
        Some((args, has_rest)) => (
            Some(lower_ctor_pat_args(&args, arena, symbols, source_map)?),
            has_rest,
        ),
        None => (None, false),
    };
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::ImplicitMember {
            name,
            args,
            has_rest,
        },
        span,
    }))
}

fn lower_constructor_pattern<'hir>(
    constructor: &ast::CtorPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(constructor.syntax()) {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    }
    let Some(path) = constructor.path() else {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    };
    if path.segments().next().is_none() {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    }
    let args = match constructor.args() {
        Some(args) => match collect_ctor_pat_args(&args) {
            Some((args, has_rest)) => Some((args, has_rest)),
            None => return lower_error_pattern(constructor.syntax(), arena, source_map),
        },
        None => None,
    };

    let (hir_id, span) = source_map.add_node_with_span(constructor.syntax(), SpanCtxt::Root)?;
    let path = lower_const_path(&path, arena, symbols, source_map)?;
    let (args, has_rest) = match args {
        Some((args, has_rest)) => (
            Some(lower_ctor_pat_args(&args, arena, symbols, source_map)?),
            has_rest,
        ),
        None => (None, false),
    };
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Ctor {
            path,
            args,
            has_rest,
        },
        span,
    }))
}

fn lower_const_path<'hir>(
    syntax: &ast::ConstPath,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Path<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax.syntax(), SpanCtxt::Root)?;
    let mut segments = Vec::new();
    for segment in syntax.segments() {
        let (hir_id, _) = source_map.add_node_with_span(segment.syntax(), SpanCtxt::Root)?;
        segments.push(PathSegment {
            hir_id,
            name: lower_name_ref(&segment, symbols, source_map)?,
            args: None,
        });
    }
    Ok(Path {
        hir_id,
        root: PathRoot::Plain,
        segments: arena.alloc_path_segments(&segments),
        span,
    })
}

#[derive(Clone)]
struct PlannedBinding {
    symbol: Symbol,
    mode: HirBindingMode,
    binding: BindingId,
    occurrences: Vec<ast::BindingPat>,
}

fn plan_pattern_bindings(
    pattern: &ast::Pattern,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    match pattern {
        ast::Pattern::Binding(binding) => plan_binding(binding, symbols, source_map, shared),
        ast::Pattern::At(at) => {
            let (Some(binding), Some(inner)) = (at.binding(), at.pattern()) else {
                return Ok(Vec::new());
            };
            if lower_binding_mode(&binding).is_none() {
                return Ok(Vec::new());
            }
            let mut planned = plan_binding(&binding, symbols, source_map, shared)?;
            let inner = plan_pattern_bindings(&inner, symbols, source_map, shared)?;
            merge_planned_bindings(&mut planned, inner, symbols, source_map)?;
            Ok(planned)
        }
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => plan_pattern_bindings(&inner, symbols, source_map, shared),
            None => Ok(Vec::new()),
        },
        ast::Pattern::Tuple(tuple) => {
            let mut planned = Vec::new();
            for element in tuple.elements() {
                let element = plan_pattern_bindings(&element, symbols, source_map, shared)?;
                merge_planned_bindings(&mut planned, element, symbols, source_map)?;
            }
            Ok(planned)
        }
        ast::Pattern::Slice(slice) if slice.rest_patterns().count() > 1 => Ok(Vec::new()),
        ast::Pattern::Slice(slice) => {
            let mut planned = Vec::new();
            for child in slice.syntax().children() {
                let child_bindings = if let Some(pattern) = ast::Pattern::cast(child.clone()) {
                    plan_pattern_bindings(&pattern, symbols, source_map, shared)?
                } else if let Some(rest) = ast::RestPat::cast(child) {
                    match rest.binding() {
                        Some(binding) => plan_binding(&binding, symbols, source_map, shared)?,
                        None => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                merge_planned_bindings(&mut planned, child_bindings, symbols, source_map)?;
            }
            Ok(planned)
        }
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_none_or(const_path_is_invalid) =>
        {
            Ok(Vec::new())
        }
        ast::Pattern::Constructor(constructor) => {
            let Some(args) = constructor.args() else {
                return Ok(Vec::new());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(Vec::new());
            };
            plan_ctor_args(&args, symbols, source_map, shared)
        }
        ast::Pattern::ImplicitMember(member) => {
            if member.name().is_none() {
                return Ok(Vec::new());
            }
            let Some(args) = member.args() else {
                return Ok(Vec::new());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(Vec::new());
            };
            plan_ctor_args(&args, symbols, source_map, shared)
        }
        ast::Pattern::Or(or) => plan_or_bindings(or, symbols, source_map, shared),
        ast::Pattern::Wildcard(_) | ast::Pattern::Literal(_) | ast::Pattern::Range(_) => {
            Ok(Vec::new())
        }
    }
}

fn plan_binding(
    binding_pat: &ast::BindingPat,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let Some(mode) = lower_binding_mode(binding_pat) else {
        return Ok(Vec::new());
    };
    let name = lower_binding_name(binding_pat, symbols, source_map)?;
    if name.sym.is_error() {
        return Ok(Vec::new());
    }
    let binding = match shared
        .and_then(|bindings| bindings.iter().find(|planned| planned.symbol == name.sym))
    {
        Some(planned) => {
            source_map.add_binding_alias(binding_pat.syntax(), planned.binding)?;
            planned.binding
        }
        None => {
            source_map
                .add_binding_with_span(binding_pat.syntax(), SpanCtxt::Root)?
                .0
        }
    };
    Ok(vec![PlannedBinding {
        symbol: name.sym,
        mode,
        binding,
        occurrences: vec![binding_pat.clone()],
    }])
}

fn plan_ctor_args(
    args: &[CtorPatArgSyntax],
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let mut planned = Vec::new();
    for arg in args {
        let pattern = match arg {
            CtorPatArgSyntax::Positional(pattern) | CtorPatArgSyntax::Named { pattern, .. } => {
                pattern
            }
        };
        let bindings = plan_pattern_bindings(pattern, symbols, source_map, shared)?;
        merge_planned_bindings(&mut planned, bindings, symbols, source_map)?;
    }
    Ok(planned)
}

fn merge_planned_bindings(
    planned: &mut Vec<PlannedBinding>,
    incoming: Vec<PlannedBinding>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(), HirLoweringError> {
    for mut binding in incoming {
        let duplicate = planned
            .iter()
            .find(|existing| existing.symbol == binding.symbol);
        if let Some(existing) = duplicate {
            if existing.binding == binding.binding {
                let mut occurrences = binding.occurrences.iter();
                let first = occurrences
                    .next()
                    .expect("a planned binding has at least one syntax occurrence");
                binding.binding = source_map
                    .add_binding_with_span(first.syntax(), SpanCtxt::Root)?
                    .0;
                for occurrence in occurrences {
                    source_map.add_binding_alias(occurrence.syntax(), binding.binding)?;
                }
            }
            let duplicate_syntax = binding
                .occurrences
                .first()
                .expect("a planned binding has at least one syntax occurrence");
            let name = symbols
                .resolve(binding.symbol)
                .map(|name| name.to_string())
                .unwrap_or_else(|_| "<error>".to_owned());
            source_map.push_error_diagnostic_for_node(
                Code::DuplicateBinding,
                format!("`{name}` is bound more than once in this pattern"),
                duplicate_syntax.syntax(),
            )?;
        } else {
            planned.push(binding);
        }
    }
    Ok(())
}

fn plan_or_bindings(
    or: &ast::OrPat,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let alternatives = flattened_or_alternatives(or);
    let Some(first) = alternatives.first() else {
        return Ok(Vec::new());
    };
    let first_bindings = plan_pattern_bindings(first, symbols, source_map, shared)?;
    let mut planned = first_bindings.clone();
    let mut mismatch = None;
    for alternative in alternatives.iter().skip(1) {
        let bindings = plan_pattern_bindings(alternative, symbols, source_map, Some(&planned))?;
        if mismatch.is_none() {
            mismatch = first_binding_mismatch(&first_bindings, &bindings);
        }
        for alternative_binding in bindings {
            if let Some(planned_binding) = planned
                .iter_mut()
                .find(|binding| binding.symbol == alternative_binding.symbol)
            {
                planned_binding
                    .occurrences
                    .extend(alternative_binding.occurrences.iter().cloned());
            } else {
                planned.push(alternative_binding);
            }
        }
    }
    if let Some(symbol) = mismatch {
        let name = symbols
            .resolve(symbol)
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "<error>".to_owned());
        source_map.push_error_diagnostic_for_node(
            Code::OrPatternBindingMismatch,
            format!("or-pattern alternatives do not bind `{name}` consistently"),
            or.syntax(),
        )?;
    }
    Ok(planned)
}

fn first_binding_mismatch(
    expected: &[PlannedBinding],
    actual: &[PlannedBinding],
) -> Option<Symbol> {
    for binding in expected {
        match actual
            .iter()
            .find(|candidate| candidate.symbol == binding.symbol)
        {
            Some(candidate) if candidate.mode == binding.mode => {}
            _ => return Some(binding.symbol),
        }
    }
    actual
        .iter()
        .find(|binding| {
            !expected
                .iter()
                .any(|candidate| candidate.symbol == binding.symbol)
        })
        .map(|binding| binding.symbol)
}

fn flattened_or_alternatives(or: &ast::OrPat) -> Vec<ast::Pattern> {
    fn flatten(pattern: &ast::Pattern, alternatives: &mut Vec<ast::Pattern>) {
        match pattern {
            ast::Pattern::Or(or) => {
                for alternative in or.alternatives() {
                    flatten(&alternative, alternatives);
                }
            }
            ast::Pattern::Parenthesized(paren) => match paren.pattern() {
                Some(inner) => flatten(&inner, alternatives),
                None => alternatives.push(pattern.clone()),
            },
            _ => alternatives.push(pattern.clone()),
        }
    }

    let mut alternatives = Vec::new();
    for alternative in or.alternatives() {
        flatten(&alternative, &mut alternatives);
    }
    alternatives
}

fn ensure_supported_pattern(pattern: &ast::Pattern) -> Result<(), HirLoweringError> {
    if contains_error_node(pattern.syntax()) {
        return Ok(());
    }

    match pattern {
        ast::Pattern::Wildcard(_) | ast::Pattern::Literal(_) | ast::Pattern::Binding(_) => Ok(()),
        ast::Pattern::Range(_) => Ok(()),
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => ensure_supported_pattern(&inner),
            None => Ok(()),
        },
        ast::Pattern::At(at) => match (at.binding(), at.pattern()) {
            (Some(binding), Some(inner)) if lower_binding_mode(&binding).is_some() => {
                ensure_supported_pattern(&inner)
            }
            _ => Ok(()),
        },
        ast::Pattern::Tuple(tuple) => {
            for element in tuple.elements() {
                ensure_supported_pattern(&element)?;
            }
            Ok(())
        }
        ast::Pattern::Slice(slice) if slice.rest_patterns().count() > 1 => Ok(()),
        ast::Pattern::Slice(slice) => {
            for element in slice.elements() {
                ensure_supported_pattern(&element)?;
            }
            Ok(())
        }
        ast::Pattern::Or(or) => {
            for alternative in flattened_or_alternatives(or) {
                ensure_supported_pattern(&alternative)?;
            }
            Ok(())
        }
        ast::Pattern::ImplicitMember(member) => {
            let Some(args) = member.args() else {
                return Ok(());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(());
            };
            for arg in args {
                let pattern = match arg {
                    CtorPatArgSyntax::Positional(pattern)
                    | CtorPatArgSyntax::Named { pattern, .. } => pattern,
                };
                ensure_supported_pattern(&pattern)?;
            }
            Ok(())
        }
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_some_and(const_path_is_invalid) =>
        {
            Ok(())
        }
        ast::Pattern::Constructor(constructor) => {
            let Some(args) = constructor.args() else {
                return Ok(());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(());
            };
            for arg in args {
                let pattern = match arg {
                    CtorPatArgSyntax::Positional(pattern)
                    | CtorPatArgSyntax::Named { pattern, .. } => pattern,
                };
                ensure_supported_pattern(&pattern)?;
            }
            Ok(())
        }
    }
}

enum CtorPatArgSyntax {
    Positional(ast::Pattern),
    Named {
        name: ast::NameRef,
        pattern: ast::Pattern,
    },
}

fn collect_ctor_pat_args(args: &ast::CtorPatArgs) -> Option<(Vec<CtorPatArgSyntax>, bool)> {
    let rests = args.rest_patterns().count();
    if rests > 1 {
        return None;
    }
    let mut lowered = Vec::new();
    for child in args.syntax().children() {
        if let Some(pattern) = ast::Pattern::cast(child.clone()) {
            lowered.push(CtorPatArgSyntax::Positional(pattern));
        } else if let Some(named) = ast::NamedPatArg::cast(child) {
            let (Some(name), Some(pattern)) = (named.name(), named.pattern()) else {
                return None;
            };
            lowered.push(CtorPatArgSyntax::Named { name, pattern });
        }
    }
    Some((lowered, rests == 1))
}

fn lower_ctor_pat_args<'hir>(
    args: &[CtorPatArgSyntax],
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir [PatArg<'hir>], HirLoweringError> {
    let mut lowered = Vec::with_capacity(args.len());
    for arg in args {
        match arg {
            CtorPatArgSyntax::Positional(pattern) => {
                let pattern = lower_pattern_inner(pattern, arena, symbols, source_map)?;
                lowered.push(PatArg {
                    name: None,
                    pat: pattern,
                });
            }
            CtorPatArgSyntax::Named { name, pattern } => {
                let name = lower_name_ref(name, symbols, source_map)?;
                let pattern = lower_pattern_inner(pattern, arena, symbols, source_map)?;
                lowered.push(PatArg {
                    name: Some(name),
                    pat: pattern,
                });
            }
        }
    }
    Ok(arena.alloc_pat_args(&lowered))
}

fn lower_binding_pattern<'hir>(
    binding_pat: &ast::BindingPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(binding_pat.syntax()) {
        return lower_error_pattern(binding_pat.syntax(), arena, source_map);
    }
    let Some(mode) = lower_binding_mode(binding_pat) else {
        return lower_error_pattern(binding_pat.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(binding_pat.syntax(), SpanCtxt::Root)?;
    let name = lower_binding_name(binding_pat, symbols, source_map)?;
    let binding = lower_binding_id(binding_pat.syntax(), source_map)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Binding {
            mode,
            name,
            binding,
            sub: None,
        },
        span,
    }))
}

fn lower_at_pattern<'hir>(
    at: &ast::AtPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(at.syntax()) {
        return lower_error_pattern(at.syntax(), arena, source_map);
    }
    let (Some(binding_pat), Some(inner)) = (at.binding(), at.pattern()) else {
        return lower_error_pattern(at.syntax(), arena, source_map);
    };
    let Some(mode) = lower_binding_mode(&binding_pat) else {
        return lower_error_pattern(at.syntax(), arena, source_map);
    };

    let (hir_id, span) = source_map.add_node_with_span(at.syntax(), SpanCtxt::Root)?;
    let name = lower_binding_name(&binding_pat, symbols, source_map)?;
    let binding = lower_binding_id(binding_pat.syntax(), source_map)?;
    let sub = lower_pattern_inner(&inner, arena, symbols, source_map)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Binding {
            mode,
            name,
            binding,
            sub: Some(sub),
        },
        span,
    }))
}

fn lower_binding_id(
    syntax: &SyntaxNode,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<BindingId, HirLoweringError> {
    match source_map.binding_for_syntax(syntax)? {
        Some(binding) => Ok(binding),
        None => Ok(source_map.add_binding(syntax)?),
    }
}

fn lower_binding_mode(binding_pat: &ast::BindingPat) -> Option<HirBindingMode> {
    let Some(mode) = binding_pat.mode() else {
        return Some(HirBindingMode::Value);
    };
    match mode.kind()? {
        ast::BindingModeKind::Var => Some(HirBindingMode::Var),
        ast::BindingModeKind::Borrow => Some(HirBindingMode::Borrow),
        ast::BindingModeKind::BorrowMut => Some(HirBindingMode::BorrowMut),
    }
}

fn lower_binding_name(
    binding_pat: &ast::BindingPat,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    let Some(name) = binding_pat.name() else {
        return lower_name_spelling(binding_pat.syntax(), None, symbols, source_map);
    };
    let Some(token) = name.token() else {
        return lower_name_spelling(name.syntax(), None, symbols, source_map);
    };

    lower_name_spelling(name.syntax(), Some(token.text()), symbols, source_map)
}

fn lower_name_ref(
    name: &ast::NameRef,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    match name.syntax().first_token() {
        Some(token) => lower_name_spelling(name.syntax(), Some(token.text()), symbols, source_map),
        None => lower_name_spelling(name.syntax(), None, symbols, source_map),
    }
}

fn lower_name_spelling(
    syntax: &SyntaxNode,
    written: Option<&str>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    let span = source_map.span_for(syntax, SpanCtxt::Root)?;
    let Some(written) = written else {
        return Ok(Name {
            sym: Symbol::ERROR,
            span,
        });
    };
    let raw_name = written.strip_prefix("r#");
    let identifier = raw_name.unwrap_or(written);
    let invalid_raw = invalid_raw_identifier(written);
    let reserved = raw_name.is_none() && sai_syntax::is_reserved_identifier(identifier);
    let sym = if invalid_raw || reserved {
        Symbol::ERROR
    } else {
        let normalized = sai_syntax::normalize_identifier(identifier);
        symbols.intern(&normalized)?
    };
    Ok(Name { sym, span })
}

fn invalid_raw_identifier(written: &str) -> bool {
    matches!(written.strip_prefix("r#"), Some("self" | "Self" | "_"))
}

fn const_path_has_invalid_raw_identifier(path: ast::ConstPath) -> bool {
    path.segments().any(|segment| {
        segment
            .syntax()
            .first_token()
            .is_some_and(|token| invalid_raw_identifier(token.text()))
    })
}

fn const_path_is_invalid(path: ast::ConstPath) -> bool {
    path.segments().next().is_none() || const_path_has_invalid_raw_identifier(path)
}

enum PreparedRangeEndpoint<'hir> {
    Literal(Lit<'hir>),
    Path(ast::ConstPath),
}

fn prepare_range_endpoint<'hir>(
    endpoint: ast::RangePatEndpoint,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
) -> Result<Option<PreparedRangeEndpoint<'hir>>, HirLoweringError> {
    match endpoint {
        ast::RangePatEndpoint::Literal(literal) => {
            if contains_error_node(literal.syntax()) {
                return Ok(None);
            }
            let negative = literal.minus_token().is_some();
            let decoded = literal
                .value_token()
                .and_then(ast::LiteralToken::cast)
                .map(|token| decode_literal(token, arena, symbols, false, negative))
                .transpose()?
                .flatten();
            Ok(decoded.map(PreparedRangeEndpoint::Literal))
        }
        ast::RangePatEndpoint::ConstantPath(path) => Ok(Some(PreparedRangeEndpoint::Path(path))),
    }
}

fn lower_prepared_range_endpoint<'hir>(
    endpoint: PreparedRangeEndpoint<'hir>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<PatRangeEnd<'hir>, HirLoweringError> {
    match endpoint {
        PreparedRangeEndpoint::Literal(literal) => Ok(PatRangeEnd::Lit(literal)),
        PreparedRangeEndpoint::Path(path) => Ok(PatRangeEnd::Path(lower_const_path(
            &path, arena, symbols, source_map,
        )?)),
    }
}

fn decode_literal<'hir>(
    token: ast::LiteralToken,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    allow_float: bool,
    negated: bool,
) -> Result<Option<Lit<'hir>>, HirLoweringError> {
    let literal = match token {
        ast::LiteralToken::Int(token) if !token.syntax().has_error() => Some(Lit::Int {
            value: token.value(),
            negated,
        }),
        ast::LiteralToken::Float(token) if allow_float && !token.syntax().has_error() => {
            let text = token.syntax().text().replace('_', "");
            Some(Lit::Float {
                text: symbols.intern(&text)?,
                negated,
            })
        }
        ast::LiteralToken::String(token) if !negated => {
            Some(Lit::Str(symbols.intern(token.value().as_ref())?))
        }
        ast::LiteralToken::ByteString(token) if !negated => {
            Some(Lit::ByteStr(arena.alloc_bytes(token.value().as_ref())))
        }
        ast::LiteralToken::Char(token) if !negated => Some(Lit::Char(token.value())),
        ast::LiteralToken::Byte(token) if !negated => Some(Lit::Byte(token.value())),
        ast::LiteralToken::True(_) if !negated => Some(Lit::Bool(true)),
        ast::LiteralToken::False(_) if !negated => Some(Lit::Bool(false)),
        _ => None,
    };
    Ok(literal)
}

fn contains_error_node(syntax: &SyntaxNode) -> bool {
    syntax
        .descendants()
        .any(|node| node.kind() == SyntaxKind::ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId};
    use sai_intern::PackageInterner;
    use sai_syntax::{SyntaxKind, ast::AstNode};
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};
    use std::sync::Arc;

    fn parse_file(
        file: crate::FileId,
        source: &str,
    ) -> (
        WorkspaceSnapshot,
        sai_syntax::Parse,
        sai_syntax::ast::AstIdMap,
    ) {
        let source = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, Some(1))])
                .expect("valid snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("parse succeeds");
        let ast_ids = sai_syntax::ast::AstIdMap::new(file, &parsed.root).expect("AST ID map");
        (snapshot, parsed, ast_ids)
    }

    fn owner_id(symbols: &Interner) -> OwnerId {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:lowering#1.0")
            .expect("package interns");
        let paths = DefPathInterner::new(package, &packages, symbols).expect("path interner");
        let name = symbols.intern("f").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(name),
                disambiguator: 0,
            }],
            &packages,
            symbols,
        )
        .expect("definition path");
        OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        }
    }

    fn function_node(root: &sai_syntax::SyntaxNode) -> sai_syntax::SyntaxNode {
        root.descendants()
            .find(|node| node.kind() == SyntaxKind::FN_DECL)
            .expect("function declaration")
    }

    #[test]
    fn lowers_decoded_literals_and_records_source_spans() {
        let file = crate::FileId::new(61);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r#"fn f() { (42, 340282366920938463463374607431768211455, 340282366920938463463374607431768211456, 1.5_0, "text\n", b"bytes\x21", 'x', b'y', true, false) }
"#,
        );
        let function = function_node(&parsed.root);
        let literals = function
            .descendants()
            .filter_map(ast::LiteralExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(literals.len(), 10);
        assert!(!contains_error_node(literals[2].syntax()));

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = literals
            .iter()
            .map(|literal| lower_literal(literal, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literals lower");

        assert_eq!(
            lowered[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(42),
                negated: false,
            })
        );
        assert_eq!(
            lowered[1].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(u128::MAX),
                negated: false,
            })
        );
        assert_eq!(
            lowered[2].kind,
            ExprKind::Lit(Lit::Int {
                value: None,
                negated: false,
            })
        );
        assert_eq!(
            lowered[3].kind,
            ExprKind::Lit(Lit::Float {
                text: symbols.intern("1.50").expect("float spelling interns"),
                negated: false,
            })
        );
        let decoded_string = match lowered[4].kind {
            ExprKind::Lit(Lit::Str(value)) => symbols.resolve(value).expect("string resolves"),
            _ => panic!("string literal lowers to Lit::Str"),
        };
        assert_eq!(decoded_string.as_ref(), "text\n");
        assert_eq!(lowered[5].kind, ExprKind::Lit(Lit::ByteStr(b"bytes!")));
        assert_eq!(lowered[6].kind, ExprKind::Lit(Lit::Char('x')));
        assert_eq!(lowered[7].kind, ExprKind::Lit(Lit::Byte(b'y')));
        assert_eq!(lowered[8].kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[9].kind, ExprKind::Lit(Lit::Bool(false)));
        assert!(
            lowered
                .iter()
                .enumerate()
                .all(|(index, expr)| expr.hir_id.local_id.raw() == index as u32 + 1)
        );
        for (expr, syntax) in lowered.iter().zip(&literals) {
            assert_eq!(expr.span.range, syntax.syntax().text_range());
            assert_eq!(expr.span.file, file);
            assert_eq!(expr.span.ctxt, SpanCtxt::Root);
        }

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), literals.len() + 1);
    }

    #[test]
    fn lowers_unit_parenthesized_and_tuple_expressions() {
        let file = crate::FileId::new(72);
        let source = "fn f() { ((), (1, ((true,)))) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("outer tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("supported tuple expression lowers");

        fn tuple_elements<'hir>(expression: &Expr<'hir>) -> &'hir [Expr<'hir>] {
            match expression.kind {
                ExprKind::Tuple(elements) => elements,
                _ => panic!("unit and tuple expressions lower to ExprKind::Tuple"),
            }
        }
        let outer = tuple_elements(lowered);
        assert_eq!(outer.len(), 2);
        assert!(tuple_elements(&outer[0]).is_empty());
        assert_eq!(
            outer[0].span.range,
            function
                .descendants()
                .find(|node| node.kind() == SyntaxKind::UNIT_EXPR)
                .expect("unit expression syntax")
                .text_range()
        );

        let inner = tuple_elements(&outer[1]);
        assert_eq!(inner.len(), 2);
        assert_eq!(
            inner[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let singleton = tuple_elements(&inner[1]);
        assert_eq!(singleton.len(), 1);
        assert_eq!(singleton[0].kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(
            lowered.hir_id.local_id.raw(),
            1,
            "the outer tuple receives its ID before its children"
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                outer[0].hir_id.local_id.raw(),
                outer[1].hir_id.local_id.raw(),
                inner[0].hir_id.local_id.raw(),
                inner[1].hir_id.local_id.raw(),
                singleton[0].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(lowered.span.range, tuple.syntax().text_range());
        let paren = function
            .descendants()
            .find_map(ast::ParenExpr::cast)
            .expect("parenthesized expression syntax");
        let paren_pointer =
            sai_syntax::SyntaxNodePtr::new(file, paren.syntax()).expect("parenthesis pointer");
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 7);
        assert!(
            map.nodes_for_syntax(&paren_pointer).is_empty(),
            "parentheses reuse their contents without a separate HIR node"
        );
    }

    #[test]
    fn lowers_arrays_with_source_order_parent_first_ids_and_spans() {
        let file = crate::FileId::new(88);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { [1, [2], 3] }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("array lowers");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("array syntax lowers to ExprKind::Array");
        };
        assert_eq!(elements.len(), 3);
        assert_eq!(
            elements[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let ExprKind::Array(nested) = elements[1].kind else {
            panic!("nested array remains an array expression");
        };
        assert_eq!(
            nested[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(
            elements[2].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                nested[0].hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5]
        );
        assert_eq!(lowered.span.range, array.syntax().text_range());
        assert_eq!(
            elements[1].span.range,
            array.elements().nth(1).unwrap().syntax().text_range()
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 6);
    }

    #[test]
    fn array_recovery_keeps_missing_elements_and_preflights_supported_children() {
        let file = crate::FileId::new(89);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { [, call(, 2)] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let array_error = array
            .syntax()
            .children()
            .find(|child| child.kind() == SyntaxKind::ERROR)
            .expect("missing array element recovery node");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("array recovers");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("the array is preserved around missing elements");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].kind, ExprKind::Err);
        let ExprKind::Call { args, .. } = elements[1].kind else {
            panic!("the valid call sibling remains in the array");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        let error_ptr = sai_syntax::SyntaxNodePtr::new(file, &array_error).expect("error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&error_ptr).len(), 1);
        assert_eq!(elements[0].span.range, array_error.text_range());

        let file = crate::FileId::new(90);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { [make(..base1, ..base2, value.convert<U32>)] }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let result = lower_expr(&expression, &arena, &symbols, &mut source_map);
        assert!(
            matches!(
                result,
                Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
            ),
            "unexpected lowering result: {result:?}"
        );
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "a valid unsupported child is rejected before the array is allocated"
        );

        let file = crate::FileId::new(91);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { [[value.convert<U32>, ,], call(, 2)] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "all nested array elements are preflighted before any array is allocated"
        );

        let file = crate::FileId::new(92);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { [(@inline value).convert<U32>, call(, 2)] }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "parenthesized recovery wrappers do not hide unsupported children"
        );
    }

    #[test]
    fn lowers_return_break_continue_and_label_spans() {
        let file = crate::FileId::new(93);
        let source = "fn f() { (return, return 1, break, break 'outer 2, continue 'outer) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("control-transfer expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the outer expression is a tuple");
        };
        assert_eq!(elements.len(), 5);
        assert_eq!(elements[0].kind, ExprKind::Return(None));
        let ExprKind::Return(Some(return_value)) = elements[1].kind else {
            panic!("return keeps its optional value");
        };
        assert_eq!(
            return_value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            elements[2].kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        );
        let ExprKind::Break {
            label: Some(break_label),
            value: Some(break_value),
        } = elements[3].kind
        else {
            panic!("labeled break retains its label and value");
        };
        assert_eq!(
            symbols
                .resolve(break_label.name)
                .expect("label name resolves")
                .as_ref(),
            "outer"
        );
        assert_eq!(
            break_value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        let ExprKind::Continue {
            label: Some(continue_label),
        } = elements[4].kind
        else {
            panic!("labeled continue retains its label");
        };
        assert_eq!(continue_label.name, break_label.name);

        let label_tokens = function
            .descendants()
            .filter_map(ast::BreakExpr::cast)
            .filter_map(|break_expr| break_expr.label())
            .chain(
                function
                    .descendants()
                    .filter_map(ast::ContinueExpr::cast)
                    .filter_map(|continue_expr| continue_expr.label()),
            )
            .collect::<Vec<_>>();
        assert_eq!(label_tokens.len(), 2);
        for (label, hir_label) in label_tokens.iter().zip([break_label, continue_label]) {
            assert_eq!(hir_label.span.file, file);
            assert_eq!(hir_label.span.range, label.syntax().text_range());
            assert_eq!(hir_label.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                return_value.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                break_value.hir_id.local_id.raw(),
                elements[4].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 9);

        let file = crate::FileId::new(94);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { return value.convert<U32> }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let return_expr = function
            .descendants()
            .find_map(ast::ReturnExpr::cast)
            .expect("return expression");
        let expression =
            ast::Expr::cast(return_expr.syntax().clone()).expect("return is an expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "unsupported return values are rejected before the return node is allocated"
        );

        let file = crate::FileId::new(95);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { (return 'oops:, break 'oops:, call(, 2)) }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("recovery tuple");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the outer tuple preserves local recovery");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the recovery parent remains a tuple");
        };
        assert_eq!(elements.len(), 3);
        assert!(matches!(
            elements[0].kind,
            ExprKind::Return(Some(value)) if value.kind == ExprKind::Err
        ));
        assert!(matches!(
            elements[1].kind,
            ExprKind::Break {
                label: _,
                value: Some(value),
            } if value.kind == ExprKind::Err
        ));
        assert!(matches!(
            elements[2].kind,
            ExprKind::Call { args, .. }
                if args.first().is_some_and(|arg| arg.expr.kind == ExprKind::Err)
        ));
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn lowers_expression_blocks_with_source_ordered_statements_and_tail() {
        let file = crate::FileId::new(96);
        let source = "fn f() { ({}, { 1\n 2 }) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("blocks lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the tuple holds both block expressions");
        };
        assert_eq!(elements.len(), 2);

        let ExprKind::Block(empty_block) = elements[0].kind else {
            panic!("the first element is an empty block");
        };
        assert!(empty_block.stmts.is_empty());
        assert!(empty_block.tail.is_none());
        assert_eq!(empty_block.safety, Safety::Safe);

        let ExprKind::Block(block) = elements[1].kind else {
            panic!("the second element is a block");
        };
        assert_eq!(block.stmts.len(), 1);
        let StmtKind::Expr(statement_expr) = block.stmts[0].kind else {
            panic!("the preceding expression statement remains in the block");
        };
        assert_eq!(
            statement_expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let tail = block.tail.expect("the final expression is the block tail");
        assert_eq!(
            tail.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                empty_block.hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                block.hir_id.local_id.raw(),
                block.stmts[0].hir_id.local_id.raw(),
                statement_expr.hir_id.local_id.raw(),
                tail.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(block.span.file, file);
        assert_eq!(block.span.ctxt, SpanCtxt::Root);
        assert_eq!(source_map.finish().node_to_syntax().len(), 9);
    }

    #[test]
    fn block_recovery_keeps_call_argument_errors_and_preflights_statements() {
        let file = crate::FileId::new(97);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { { call(, 2) } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument recovery preserves its containing block");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the block is preserved");
        };
        let call = block.tail.expect("the final call is the block tail");
        let ExprKind::Call { args, .. } = call.kind else {
            panic!("the block tail is the call");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let file = crate::FileId::new(98);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { { let x = 1\n x } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::LET_STMT))
        ));
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let file = crate::FileId::new(99);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { { call(, 2)\n@allow(foo)\nfn inner() {}\n} }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let block_syntax = block_expr.block().expect("block syntax");
        let recovered_error = block_syntax
            .syntax()
            .children()
            .find(|child| child.kind() == SyntaxKind::ERROR)
            .expect("attributed item recovery node");
        assert!(contains_attribute(&recovered_error));

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the block retains call and statement recovery");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the block is preserved");
        };
        assert_eq!(block.stmts.len(), 2);
        assert!(block.tail.is_none());
        let StmtKind::Expr(call) = block.stmts[0].kind else {
            panic!("the earlier expression remains a statement");
        };
        assert!(matches!(call.kind, ExprKind::Call { .. }));
        let StmtKind::Expr(error_expr) = block.stmts[1].kind else {
            panic!("the attributed error remains an error statement");
        };
        assert_eq!(error_expr.kind, ExprKind::Err);
        assert_eq!(error_expr.span.range, recovered_error.text_range());
        let error_ptr =
            sai_syntax::SyntaxNodePtr::new(file, &recovered_error).expect("error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&error_ptr).len(), 2);
    }

    #[test]
    fn lowers_plain_if_else_and_else_if_expressions() {
        let file = crate::FileId::new(100);
        let source = "fn f() { (if true { 1 } else { 2 }, if false { 3 } else if true { 4 } else { 5 }, if false { 6 }) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("plain if expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the tuple holds three if expressions");
        };
        assert_eq!(elements.len(), 3);

        let ExprKind::If {
            cond: first_cond,
            then: first_then,
            els: Some(first_else),
        } = elements[0].kind
        else {
            panic!("the first if has an else block");
        };
        assert_eq!(first_cond.kind, ExprKind::Lit(Lit::Bool(true)));
        assert!(first_then.stmts.is_empty());
        assert_eq!(
            first_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let ExprKind::Block(first_else_block) = first_else.kind else {
            panic!("else blocks are represented as block expressions");
        };
        assert_eq!(
            first_else_block.tail.expect("else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let ExprKind::If {
            cond: second_cond,
            then: second_then,
            els: Some(second_else),
        } = elements[1].kind
        else {
            panic!("the second if has an else-if");
        };
        assert_eq!(second_cond.kind, ExprKind::Lit(Lit::Bool(false)));
        assert_eq!(
            second_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        let ExprKind::If {
            cond: else_if_cond,
            then: else_if_then,
            els: Some(else_if_else),
        } = second_else.kind
        else {
            panic!("else-if lowers recursively as an if expression");
        };
        assert_eq!(else_if_cond.kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(
            else_if_then.tail.expect("else-if then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(4),
                negated: false,
            })
        );
        let ExprKind::Block(else_if_else_block) = else_if_else.kind else {
            panic!("the nested else block is preserved");
        };
        assert_eq!(
            else_if_else_block.tail.expect("else-if else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(5),
                negated: false,
            })
        );

        let ExprKind::If {
            cond: third_cond,
            then: third_then,
            els: None,
        } = elements[2].kind
        else {
            panic!("the third if has no else branch");
        };
        assert_eq!(third_cond.kind, ExprKind::Lit(Lit::Bool(false)));
        assert_eq!(
            third_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(6),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                first_cond.hir_id.local_id.raw(),
                first_then.hir_id.local_id.raw(),
                first_then.tail.unwrap().hir_id.local_id.raw(),
                first_else.hir_id.local_id.raw(),
                first_else_block.hir_id.local_id.raw(),
                first_else_block.tail.unwrap().hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                second_cond.hir_id.local_id.raw(),
                second_then.hir_id.local_id.raw(),
                second_then.tail.unwrap().hir_id.local_id.raw(),
                second_else.hir_id.local_id.raw(),
                else_if_cond.hir_id.local_id.raw(),
                else_if_then.hir_id.local_id.raw(),
                else_if_then.tail.unwrap().hir_id.local_id.raw(),
                else_if_else.hir_id.local_id.raw(),
                else_if_else_block.hir_id.local_id.raw(),
                else_if_else_block.tail.unwrap().hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                third_cond.hir_id.local_id.raw(),
                third_then.hir_id.local_id.raw(),
                third_then.tail.unwrap().hir_id.local_id.raw(),
            ],
            std::array::from_fn::<_, 23, _>(|index| index as u32 + 1)
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 24);
    }

    #[test]
    fn lowers_if_let_to_match_with_desugared_synthetic_fallback() {
        let file = crate::FileId::new(111);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { if let 0 = value { 1 } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("if-let lowers to a match");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::IfLet,
        } = lowered.kind
        else {
            panic!("if-let lowers to its dedicated match source");
        };
        assert!(matches!(scrutinee.kind, ExprKind::Path(_)));
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[0].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(0),
                negated: false,
            })
        );
        assert!(!arms[0].synthesized);
        let ExprKind::Block(then_block) = arms[0].body.kind else {
            panic!("the first arm body is the source then block");
        };
        assert_eq!(
            then_block.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert!(arms[1].synthesized);
        assert_eq!(arms[1].pat.kind, PatKind::Wild);
        assert_eq!(arms[1].body.kind, ExprKind::Tuple(&[]));

        let desugared = SpanCtxt::Desugared(DesugaringKind::IfLet);
        let source_range = if_expr.syntax().text_range();
        assert_eq!(lowered.span.range, source_range);
        assert_eq!(lowered.span.ctxt, desugared);
        assert_eq!(arms[0].span.range, source_range);
        assert_eq!(arms[0].span.ctxt, desugared);
        assert_eq!(arms[1].span.range, source_range);
        assert_eq!(arms[1].span.ctxt, desugared);
        assert_eq!(arms[1].pat.span.range, source_range);
        assert_eq!(arms[1].pat.span.ctxt, desugared);
        assert_eq!(arms[1].body.span.range, source_range);
        assert_eq!(arms[1].body.span.ctxt, desugared);
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                scrutinee.hir_id.local_id.raw(),
                match scrutinee.kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match scrutinee.kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[0].hir_id.local_id.raw(),
                arms[0].pat.hir_id.local_id.raw(),
                arms[0].body.hir_id.local_id.raw(),
                then_block.hir_id.local_id.raw(),
                then_block.tail.unwrap().hir_id.local_id.raw(),
                arms[1].hir_id.local_id.raw(),
                arms[1].pat.hir_id.local_id.raw(),
                arms[1].body.hir_id.local_id.raw(),
            ],
            std::array::from_fn::<_, 12, _>(|index| index as u32 + 1)
        );
        let if_pointer =
            sai_syntax::SyntaxNodePtr::new(file, if_expr.syntax()).expect("if expression pointer");
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 13);
        assert_eq!(map.nodes_for_syntax(&if_pointer).len(), 5);
    }

    #[test]
    fn lowers_else_if_let_inside_the_synthesized_fallback() {
        let file = crate::FileId::new(112);
        let source = "fn f() { if let 0 = value {} else if let 1 = other {} else { 2 } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("outer if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("nested if-let lowers");
        let ExprKind::Match {
            arms,
            source: HirMatchSource::IfLet,
            ..
        } = lowered.kind
        else {
            panic!("outer if-let lowers to a match");
        };
        assert_eq!(arms.len(), 2);
        assert!(arms[1].synthesized);
        let ExprKind::Match {
            arms: else_if_arms,
            source: HirMatchSource::IfLet,
            ..
        } = arms[1].body.kind
        else {
            panic!("else-if let recursively lowers to a match");
        };
        assert_eq!(else_if_arms.len(), 2);
        assert!(!else_if_arms[0].synthesized);
        assert!(else_if_arms[1].synthesized);
        let ExprKind::Block(else_block) = else_if_arms[1].body.kind else {
            panic!("the final else block is retained");
        };
        assert_eq!(
            else_block.tail.expect("else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
    }

    #[test]
    fn if_let_preflights_unsupported_scrutinees_before_allocating_nodes() {
        let file = crate::FileId::new(113);
        let source = "fn f() { if let _ = value.convert<U32>() {} }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn if_recovery_is_preserved_and_unsupported_branches_are_atomic() {
        let file = crate::FileId::new(101);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { if call(, true) { 1 } else { 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument errors do not discard the if");
        let ExprKind::If { cond, .. } = lowered.kind else {
            panic!("the if expression is preserved");
        };
        let ExprKind::Call { args, .. } = cond.kind else {
            panic!("the condition call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);

        let file = crate::FileId::new(102);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { if let x = call(, 1) { 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument errors preserve if-let");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::IfLet,
        } = lowered.kind
        else {
            panic!("if-let lowers to its dedicated match source");
        };
        let ExprKind::Call { args, .. } = scrutinee.kind else {
            panic!("the scrutinee call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(arms.len(), 2);
        assert!(matches!(
            arms[0].pat.kind,
            PatKind::Binding {
                mode: HirBindingMode::Value,
                ..
            }
        ));
        assert!(!arms[0].synthesized);
        assert!(arms[1].synthesized);
        assert_eq!(arms[1].pat.kind, PatKind::Wild);
        assert_eq!(arms[1].body.kind, ExprKind::Tuple(&[]));

        let file = crate::FileId::new(103);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { 1 } else { value.convert<U32> } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let result = lower_expr(&expression, &arena, &symbols, &mut source_map);
        assert!(
            matches!(
                result,
                Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
            ),
            "unexpected lowering result: {result:?}"
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let file = crate::FileId::new(104);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { (value.convert<U32>() +) } else { 0 } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression with a recovered binary");
        let then_block = if_expr.then_branch().expect("then block");
        let then_tail = then_block
            .statements()
            .last()
            .and_then(|statement| match statement {
                ast::Stmt::Expr(statement) => statement.expression(),
                _ => None,
            })
            .expect("recovered binary expression statement");
        let ast::Expr::Paren(paren) = then_tail else {
            panic!("the malformed binary retains its parentheses");
        };
        let ast::Expr::Binary(binary) = paren.expression().expect("binary expression") else {
            panic!("the parenthesized expression is binary");
        };
        assert!(binary.rhs().is_none());
        let ast::Expr::Call(call) = binary.lhs().expect("binary left operand") else {
            panic!("the binary left operand is a call");
        };
        let ast::Expr::Field(callee) = call.callee().expect("call callee") else {
            panic!("the call callee is a field expression");
        };
        assert!(has_direct_generic_args(callee.syntax()));
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let result = lower_expr(&expression, &arena, &symbols, &mut source_map);
        assert!(
            matches!(
                result,
                Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
            ),
            "unexpected lowering result: {result:?}"
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let file = crate::FileId::new(105);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { value.convert<U32>()[] } else { 0 } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression with a recovered index");
        let then_block = if_expr.then_branch().expect("then block");
        let then_tail = then_block
            .statements()
            .last()
            .and_then(|statement| match statement {
                ast::Stmt::Expr(statement) => statement.expression(),
                _ => None,
            })
            .expect("recovered index expression statement");
        let ast::Expr::Index(index) = then_tail else {
            panic!("the recovered expression is an index");
        };
        assert!(index.base().is_some());
        assert!(index.index().is_none());
        assert!(
            index
                .base()
                .is_some_and(|base| matches!(base, ast::Expr::Call(ref call)
                    if call.callee().is_some_and(|callee| matches!(callee,
                        ast::Expr::Field(ref field) if has_direct_generic_args(field.syntax())))))
        );
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let result = lower_expr(&expression, &arena, &symbols, &mut source_map);
        assert!(
            matches!(
                result,
                Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
            ),
            "unexpected lowering result: {result:?}"
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn lowers_match_arms_with_patterns_guards_and_source_ordered_ids() {
        let file = crate::FileId::new(106);
        let source = "fn f() { match value { 0 => 1, 1 if test => 2, _ => 3 } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("match lowers");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = lowered.kind
        else {
            panic!("the expression lowers to a normal match");
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(scrutinee.kind, ExprKind::Path(_)));
        assert_eq!(
            arms[0].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(0),
                negated: false,
            })
        );
        assert!(arms[0].guard.is_none());
        assert_eq!(
            arms[0].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            arms[1].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert!(matches!(
            arms[1].guard.map(|guard| guard.kind),
            Some(ExprKind::Path(_))
        ));
        assert_eq!(
            arms[1].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(arms[2].pat.kind, PatKind::Wild);
        assert_eq!(
            arms[2].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        assert!(arms.iter().all(|arm| !arm.synthesized));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                scrutinee.hir_id.local_id.raw(),
                match scrutinee.kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match scrutinee.kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[0].hir_id.local_id.raw(),
                arms[0].pat.hir_id.local_id.raw(),
                arms[0].body.hir_id.local_id.raw(),
                arms[1].hir_id.local_id.raw(),
                arms[1].pat.hir_id.local_id.raw(),
                arms[1].guard.unwrap().hir_id.local_id.raw(),
                match arms[1].guard.unwrap().kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match arms[1].guard.unwrap().kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[1].body.hir_id.local_id.raw(),
                arms[2].hir_id.local_id.raw(),
                arms[2].pat.hir_id.local_id.raw(),
                arms[2].body.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 17);
    }

    #[test]
    fn match_recovery_keeps_arms_and_preflights_nested_expressions() {
        let file = crate::FileId::new(107);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { match call(, true) { _ =>, 1 => 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("scrutinee call errors preserve the match");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = lowered.kind
        else {
            panic!("the match is preserved");
        };
        let ExprKind::Call { args, .. } = scrutinee.kind else {
            panic!("the scrutinee call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(
            arms[1].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );

        let file = crate::FileId::new(108);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match call(, true) { _ => value.convert<U32> } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn missing_match_scrutinee_and_arm_list_keep_distinct_error_nodes() {
        let file = crate::FileId::new(109);
        let source = "fn f() { (match, call(, 2)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        assert!(match_expr.scrutinee().is_none());
        assert!(match_expr.arms().is_none());
        let errors = match_expr
            .syntax()
            .children()
            .filter(|child| child.kind() == SyntaxKind::ERROR)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 2);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let tuple_expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&tuple_expression, &arena, &symbols, &mut source_map)
            .expect("the sibling call keeps the tuple recoverable");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the outer tuple is preserved");
        };
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = elements[0].kind
        else {
            panic!("the malformed match is preserved");
        };
        assert_eq!(scrutinee.kind, ExprKind::Err);
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].pat.kind, PatKind::Err);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(scrutinee.span.range, errors[0].text_range());
        assert_eq!(arms[0].span.range, errors[1].text_range());
        let first_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[0]).expect("scrutinee error pointer");
        let second_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[1]).expect("arm error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&first_error).len(), 1);
        assert_eq!(map.nodes_for_syntax(&second_error).len(), 3);
    }

    #[test]
    fn missing_match_body_uses_its_own_recovery_node() {
        let file = crate::FileId::new(110);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { match call(, true) { _ } }\n");
        let function = function_node(&parsed.root);
        assert!(!parsed.errors.is_empty());
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let arm = function
            .descendants()
            .find_map(ast::MatchArm::cast)
            .expect("match arm");
        let errors = arm
            .syntax()
            .children()
            .filter(|child| child.kind() == SyntaxKind::ERROR)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].text_range(), errors[1].text_range());
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("a missing body is represented as an error expression");
        let ExprKind::Match { arms, .. } = lowered.kind else {
            panic!("the match is preserved: {lowered:?}");
        };
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].pat.kind, PatKind::Wild);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(arms[0].body.span.range, errors[1].text_range());
        let arrow_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[0]).expect("arrow error pointer");
        let body_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[1]).expect("body error pointer");
        let map = source_map.finish();
        assert!(map.nodes_for_syntax(&arrow_error).is_empty());
        assert_eq!(map.nodes_for_syntax(&body_error).len(), 1);
    }

    #[test]
    fn rejects_unsupported_tuple_children_before_allocating_hir_nodes() {
        let file = crate::FileId::new(73);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { (1, value.convert<U32>) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::FIELD_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "preflight rejects generic field arguments before allocating the tuple"
        );
    }

    #[test]
    fn rejects_generic_path_arguments_before_allocating_hir_nodes() {
        let file = crate::FileId::new(75);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { (1, value<I32>) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "generic paths are rejected before allocating the tuple or its children"
        );
    }

    #[test]
    fn lowers_plain_self_and_implicit_member_expression_paths() {
        let file = crate::FileId::new(74);
        let source = "fn f() { (value, self, Self, .Variant) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("path expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);

        let ExprKind::Path(value_path) = elements[0].kind else {
            panic!("plain name lowers to a path");
        };
        assert_eq!(value_path.root, PathRoot::Plain);
        assert_eq!(value_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(value_path.segments[0].name.sym)
                .expect("path name resolves")
                .as_ref(),
            "value"
        );
        assert_eq!(value_path.segments[0].args, None);

        let ExprKind::Path(self_path) = elements[1].kind else {
            panic!("self lowers to a path");
        };
        assert_eq!(self_path.root, PathRoot::SelfValue);
        assert!(self_path.segments.is_empty());

        let ExprKind::Path(self_type_path) = elements[2].kind else {
            panic!("Self lowers to a path");
        };
        assert_eq!(self_type_path.root, PathRoot::SelfType);
        assert!(self_type_path.segments.is_empty());

        let ExprKind::ImplicitMember(variant) = elements[3].kind else {
            panic!("implicit member lowers to its dedicated expression");
        };
        assert_eq!(
            symbols
                .resolve(variant.sym)
                .expect("implicit member name resolves")
                .as_ref(),
            "Variant"
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                value_path.hir_id.local_id.raw(),
                value_path.segments[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                self_path.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                self_type_path.hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );

        let map = source_map.finish();
        let path_span = map
            .syntax_for_node(value_path.hir_id)
            .expect("path has a source mapping")
            .resolve(file, &parsed.root)
            .expect("path syntax resolves")
            .text_range();
        let segment_span = map
            .syntax_for_node(value_path.segments[0].hir_id)
            .expect("segment has a source mapping")
            .resolve(file, &parsed.root)
            .expect("segment syntax resolves")
            .text_range();
        assert_eq!(path_span, elements[0].span.range);
        assert_eq!(segment_span, value_path.segments[0].name.span.range);
        assert_eq!(map.node_to_syntax().len(), 10);
    }

    #[test]
    fn lowers_nested_field_and_tuple_index_expressions() {
        let file = crate::FileId::new(76);
        let source = "fn f() { (value.field.other, value.0, value.00, value.999999999999) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidTupleIndex)
                .count(),
            1
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("field and tuple-index expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);

        let ExprKind::Field {
            base: outer_base,
            name: outer_name,
            args: None,
        } = elements[0].kind
        else {
            panic!("outer field expression is lowered");
        };
        assert_eq!(symbols.resolve(outer_name.sym).unwrap().as_ref(), "other");
        let ExprKind::Field {
            base: inner_base,
            name: inner_name,
            args: None,
        } = outer_base.kind
        else {
            panic!("field chain is represented as nested field expressions");
        };
        assert_eq!(symbols.resolve(inner_name.sym).unwrap().as_ref(), "field");
        let ExprKind::Path(path) = inner_base.kind else {
            panic!("field chain starts with a plain path");
        };
        assert_eq!(path.root, PathRoot::Plain);
        assert_eq!(path.segments.len(), 1);

        let ExprKind::TupleField {
            base: tuple_base,
            index: Some(0),
        } = elements[1].kind
        else {
            panic!("valid tuple index is retained");
        };
        assert!(matches!(tuple_base.kind, ExprKind::Path(_)));
        let ExprKind::TupleField {
            base: invalid_index_base,
            index: None,
        } = elements[2].kind
        else {
            panic!("parser-diagnosed invalid index keeps its recovery value");
        };
        assert!(matches!(invalid_index_base.kind, ExprKind::Path(_)));
        let ExprKind::TupleField {
            base: overflowing_index_base,
            index: None,
        } = elements[3].kind
        else {
            panic!("an index outside u32 has no decoded HIR index");
        };
        assert!(matches!(overflowing_index_base.kind, ExprKind::Path(_)));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                outer_base.hir_id.local_id.raw(),
                inner_base.hir_id.local_id.raw(),
                path.hir_id.local_id.raw(),
                path.segments[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                tuple_base.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                invalid_index_base.hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                overflowing_index_base.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 15, 16]
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 19);
        assert!(
            map.diagnostics().is_empty(),
            "the parser's INVALID_TUPLE_INDEX diagnostic remains authoritative"
        );
    }

    #[test]
    fn lowers_all_binary_operators_with_parent_first_ids() {
        let cases = [
            ("or", BinOp::Or),
            ("and", BinOp::And),
            ("==", BinOp::Eq),
            ("!=", BinOp::Ne),
            ("<", BinOp::Lt),
            ("<=", BinOp::Le),
            (">", BinOp::Gt),
            (">=", BinOp::Ge),
            ("|", BinOp::BitOr),
            ("^", BinOp::BitXor),
            ("&", BinOp::BitAnd),
            ("<<", BinOp::Shl),
            (">>", BinOp::Shr),
            ("+", BinOp::Add),
            ("-", BinOp::Sub),
            ("+%", BinOp::WrapAdd),
            ("-%", BinOp::WrapSub),
            ("*", BinOp::Mul),
            ("/", BinOp::Div),
            ("%", BinOp::Rem),
            ("*%", BinOp::WrapMul),
            ("||", BinOp::Or),
            ("&&", BinOp::And),
        ];
        let source = format!(
            "fn f() {{ ({}) }}\n",
            cases
                .iter()
                .map(|(operator, _)| format!("left {operator} right"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let file = crate::FileId::new(77);
        let (snapshot, parsed, ast_ids) = parse_file(file, &source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::UseAndOr, Code::UseAndOr]
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("binary lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), cases.len());
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        for (index, (element, (_, expected_op))) in elements.iter().zip(cases).enumerate() {
            let ExprKind::Binary(op, lhs, rhs) = element.kind else {
                panic!("tuple element {index} is binary");
            };
            assert_eq!(op, expected_op, "operator at tuple element {index}");
            assert!(matches!(lhs.kind, ExprKind::Path(_)));
            assert!(matches!(rhs.kind, ExprKind::Path(_)));
            assert_eq!(element.hir_id.local_id.raw(), 2 + index as u32 * 7);
            assert_eq!(lhs.hir_id.local_id.raw(), element.hir_id.local_id.raw() + 1);
            assert_eq!(rhs.hir_id.local_id.raw(), element.hir_id.local_id.raw() + 4);
        }
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            2 + cases.len() * 7,
            "the source map also contains the owner node"
        );
    }

    #[test]
    fn lowers_prefix_operators_and_folds_negative_numeric_literals() {
        let file = crate::FileId::new(78);
        let source = "fn f() { (-12, -1.25, -value, ~mask, *pointer, not condition, borrow value, borrow mut value, -true) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("prefixes lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 9);
        assert_eq!(
            elements[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(12),
                negated: true,
            })
        );
        assert_eq!(
            elements[1].kind,
            ExprKind::Lit(Lit::Float {
                text: symbols.intern("1.25").expect("float spelling interns"),
                negated: true,
            })
        );
        assert!(matches!(
            elements[2].kind,
            ExprKind::Unary(UnOp::Neg, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[3].kind,
            ExprKind::Unary(UnOp::BitNot, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[4].kind,
            ExprKind::Unary(UnOp::Deref, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[5].kind,
            ExprKind::Unary(UnOp::Not, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[6].kind,
            ExprKind::Borrow(Mutability::Not, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[7].kind,
            ExprKind::Borrow(Mutability::Mut, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[8].kind,
            ExprKind::Unary(UnOp::Neg, operand)
                if operand.kind == ExprKind::Lit(Lit::Bool(true))
        ));

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 30);
        let negative_float = map
            .syntax_for_node(elements[1].hir_id)
            .expect("folded float has a source mapping")
            .resolve(file, &parsed.root)
            .expect("negative float syntax resolves");
        assert_eq!(negative_float.text_range(), elements[1].span.range);
        assert_eq!(
            &source[negative_float.text_range().start().to_usize()
                ..negative_float.text_range().end().to_usize()],
            "-1.25",
            "a folded negative literal maps to the whole prefix expression"
        );
    }

    #[test]
    fn lowers_all_six_range_expression_forms() {
        let file = crate::FileId::new(80);
        let source = "fn f() { (start..end, start..=end, start.., ..end, ..=end, ..) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("all range expression forms lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 6);
        let expected = [
            (true, true, false),
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, true, true),
            (false, false, false),
        ];
        for (index, (range, (has_start, has_end, inclusive))) in
            elements.iter().zip(expected).enumerate()
        {
            let ExprKind::Range {
                start,
                end,
                inclusive: actual_inclusive,
            } = range.kind
            else {
                panic!("tuple element {index} is a range");
            };
            assert_eq!(start.is_some(), has_start, "range {index} start");
            assert_eq!(end.is_some(), has_end, "range {index} end");
            assert_eq!(actual_inclusive, inclusive, "range {index} inclusivity");
            for endpoint in start.into_iter().chain(end) {
                assert!(matches!(endpoint.kind, ExprKind::Path(_)));
            }
        }
        let ExprKind::Range {
            start: Some(first_start),
            end: Some(first_end),
            inclusive: false,
        } = elements[0].kind
        else {
            panic!("first range has two exclusive endpoints");
        };
        assert_eq!(elements[0].hir_id.local_id.raw(), 2);
        assert_eq!(first_start.hir_id.local_id.raw(), 3);
        assert_eq!(first_end.hir_id.local_id.raw(), 6);
        assert_eq!(elements[1].hir_id.local_id.raw(), 9);
        assert_eq!(elements[2].hir_id.local_id.raw(), 16);
        assert_eq!(elements[3].hir_id.local_id.raw(), 20);
        assert_eq!(elements[4].hir_id.local_id.raw(), 24);
        assert_eq!(elements[5].hir_id.local_id.raw(), 28);

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 29);
        let first_range_syntax = map
            .syntax_for_node(elements[0].hir_id)
            .expect("range has a source mapping")
            .resolve(file, &parsed.root)
            .expect("range syntax resolves");
        assert_eq!(first_range_syntax.text_range(), elements[0].span.range);
        assert_eq!(
            &source[first_range_syntax.text_range().start().to_usize()
                ..first_range_syntax.text_range().end().to_usize()],
            "start..end"
        );
    }

    #[test]
    fn range_recovery_preserves_diagnostics_and_rejects_unsupported_endpoints() {
        let file = crate::FileId::new(81);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { left..value<I32> }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("range expression");
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "an unsupported endpoint is rejected before allocating the range"
        );

        let file = crate::FileId::new(82);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { value..= }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("recovered range expression");
        assert!(contains_error_node(range.syntax()));
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("parser-recovered range lowers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert!(map.diagnostics().is_empty());
        assert_eq!(map.node_to_syntax().len(), 2);
    }

    #[test]
    fn chained_range_diagnostic_does_not_prevent_hir_recovery() {
        let file = crate::FileId::new(83);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { left..middle..right }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::ChainedRange]
        );
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("outer range expression");
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("range lowers");
        let ExprKind::Range {
            start: Some(start),
            end: Some(end),
            inclusive: false,
        } = lowered.kind
        else {
            panic!("outer range retains both endpoints");
        };
        assert!(matches!(
            start.kind,
            ExprKind::Range {
                start: Some(_),
                end: Some(_),
                inclusive: false,
            }
        ));
        assert!(matches!(end.kind, ExprKind::Path(_)));
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn lowers_index_try_and_await_postfix_expressions() {
        let file = crate::FileId::new(84);
        let source = "fn f() { (array[index], optional?, future.await, nested[a][b]?) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("postfix lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);
        assert!(matches!(
            elements[0].kind,
            ExprKind::Index(base, index)
                if matches!(base.kind, ExprKind::Path(_))
                    && matches!(index.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[1].kind,
            ExprKind::Try(operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[2].kind,
            ExprKind::Await(operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        let ExprKind::Try(outer_operand) = elements[3].kind else {
            panic!("postfix question mark wraps the indexed expression");
        };
        let ExprKind::Index(outer_base, outer_index) = outer_operand.kind else {
            panic!("outer index is preserved");
        };
        let ExprKind::Index(inner_base, inner_index) = outer_base.kind else {
            panic!("nested index remains left-associated");
        };
        assert!(matches!(inner_base.kind, ExprKind::Path(_)));
        assert!(matches!(inner_index.kind, ExprKind::Path(_)));
        assert!(matches!(outer_index.kind, ExprKind::Path(_)));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                outer_operand.hir_id.local_id.raw(),
                outer_base.hir_id.local_id.raw(),
                inner_base.hir_id.local_id.raw(),
                inner_index.hir_id.local_id.raw(),
                outer_index.hir_id.local_id.raw(),
            ],
            [1, 2, 9, 13, 17, 18, 19, 20, 23, 26]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 29);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn postfix_recovery_and_preflight_avoid_partial_lowering() {
        let file = crate::FileId::new(85);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { array[value<I32>] }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let index = function
            .descendants()
            .find_map(ast::IndexExpr::cast)
            .expect("index expression");
        let expression =
            ast::Expr::cast(index.syntax().clone()).expect("index is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let file = crate::FileId::new(86);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { array[] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let index = function
            .descendants()
            .find_map(ast::IndexExpr::cast)
            .expect("recovered index expression");
        assert!(contains_error_node(index.syntax()));
        let expression =
            ast::Expr::cast(index.syntax().clone()).expect("index is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("index recovers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 2);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn lowers_calls_with_source_ordered_positional_named_and_base_arguments() {
        let file = crate::FileId::new(87);
        let source = "fn f() { (factory(1, named: value, ..base), object.method(first: 2, value), .Variant(3)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("calls lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 3);

        let ExprKind::Call {
            callee: first_callee,
            args: first_args,
            base: Some(base),
        } = elements[0].kind
        else {
            panic!("the first call retains its record-update base");
        };
        assert!(matches!(first_callee.kind, ExprKind::Path(_)));
        assert_eq!(first_args.len(), 2);
        assert_eq!(first_args[0].name, None);
        assert_eq!(
            first_args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            symbols
                .resolve(first_args[1].name.expect("named argument").sym)
                .expect("argument name resolves")
                .as_ref(),
            "named"
        );
        assert!(matches!(first_args[1].expr.kind, ExprKind::Path(_)));
        assert!(matches!(base.kind, ExprKind::Path(_)));

        let ExprKind::Call {
            callee: method,
            args: method_args,
            base: None,
        } = elements[1].kind
        else {
            panic!("the method call has no base argument");
        };
        assert!(matches!(method.kind, ExprKind::Field { .. }));
        assert_eq!(method_args.len(), 2);
        assert_eq!(
            symbols
                .resolve(method_args[0].name.expect("first named argument").sym)
                .expect("argument name resolves")
                .as_ref(),
            "first"
        );
        assert_eq!(
            method_args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(method_args[1].name, None);
        assert!(matches!(method_args[1].expr.kind, ExprKind::Path(_)));

        let ExprKind::Call {
            callee: variant,
            args: variant_args,
            base: None,
        } = elements[2].kind
        else {
            panic!("implicit variant construction is represented as a call");
        };
        let ExprKind::ImplicitMember(variant_name) = variant.kind else {
            panic!("call callee is the implicit member");
        };
        assert_eq!(
            symbols
                .resolve(variant_name.sym)
                .expect("variant name resolves")
                .as_ref(),
            "Variant"
        );
        assert_eq!(variant_args.len(), 1);
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                first_callee.hir_id.local_id.raw(),
                first_args[0].expr.hir_id.local_id.raw(),
                first_args[1].expr.hir_id.local_id.raw(),
                base.hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                method.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                variant.hir_id.local_id.raw(),
                variant_args[0].expr.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 6, 7, 10, 13, 14, 22, 23, 24]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 25);
        let call_syntax = map
            .syntax_for_node(elements[0].hir_id)
            .expect("call has a source mapping")
            .resolve(file, &parsed.root)
            .expect("call syntax resolves");
        assert_eq!(call_syntax.text_range(), elements[0].span.range);
        assert_eq!(
            &source[call_syntax.text_range().start().to_usize()
                ..call_syntax.text_range().end().to_usize()],
            "factory(1, named: value, ..base)"
        );
    }

    #[test]
    fn call_preflight_and_multiple_base_recovery_are_atomic() {
        let file = crate::FileId::new(88);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { factory<I32>(value) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call expression");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let file = crate::FileId::new(89);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { factory(..first, ..second) }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast]
        );
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("recovered call expression");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("call recovers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 2);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn call_argument_recovery_keeps_valid_arguments_and_preflights_them() {
        let file = crate::FileId::new(90);
        let source = "fn f() { (call(, 1), 2) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::ExpectedExpression]
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple containing the recovered call");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("call recovers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("a malformed argument does not discard its enclosing tuple");
        };
        assert_eq!(elements.len(), 2);
        let ExprKind::Call {
            args, base: None, ..
        } = elements[0].kind
        else {
            panic!("the call retains its HIR node");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            elements[1].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let map = source_map.finish();
        let error_syntax = map
            .syntax_for_node(args[0].expr.hir_id)
            .expect("recovered argument has a source mapping")
            .resolve(file, &parsed.root)
            .expect("recovered argument syntax resolves");
        assert_eq!(error_syntax.kind(), SyntaxKind::ERROR);
        assert!(error_syntax.text_range().is_empty());
        assert!(map.diagnostics().is_empty());

        let file = crate::FileId::new(92);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { call(@inline 1) }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidAttributeTarget]
        );
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call with an invalid argument attribute");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call survives an invalid argument attribute");
        let ExprKind::Call { args, .. } = lowered.kind else {
            panic!("an attribute error is not an argument");
        };
        assert_eq!(args.len(), 1);
        assert_eq!(
            args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );

        let file = crate::FileId::new(93);
        let source = "fn f() { (call(named: @inline), call(..@inline)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            2
        );
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedExpression)
                .count(),
            2
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple containing incomplete named and base arguments");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("incomplete named and base arguments recover");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("incomplete arguments stay local to their calls");
        };
        let ExprKind::Call {
            args: named_args,
            base: None,
            ..
        } = elements[0].kind
        else {
            panic!("first expression is the named-argument call");
        };
        assert_eq!(named_args.len(), 1);
        assert!(named_args[0].name.is_some());
        assert_eq!(named_args[0].expr.kind, ExprKind::Err);

        let ExprKind::Call {
            args: base_args,
            base: Some(base),
            ..
        } = elements[1].kind
        else {
            panic!("second expression is the base-argument call");
        };
        assert!(base_args.is_empty());
        assert_eq!(base.kind, ExprKind::Err);

        let map = source_map.finish();
        for error_expr in [named_args[0].expr, base] {
            let syntax = map
                .syntax_for_node(error_expr.hir_id)
                .expect("missing argument expression has a source mapping")
                .resolve(file, &parsed.root)
                .expect("missing argument syntax resolves");
            assert_eq!(syntax.kind(), SyntaxKind::ERROR);
            assert!(
                syntax.text_range().is_empty(),
                "attribute recovery must not steal the missing-expression source map"
            );
        }

        let file = crate::FileId::new(94);
        let source = "fn f() { (, call(@inline 1)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple with a missing element and an attributed call");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("tuple and nested call recover independently");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the enclosing tuple remains available");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].kind, ExprKind::Err);
        let ExprKind::Call { args, .. } = elements[1].kind else {
            panic!("the valid call remains available");
        };
        assert_eq!(args.len(), 1);
        assert_eq!(
            args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let map = source_map.finish();
        let missing_element = map
            .syntax_for_node(elements[0].hir_id)
            .expect("missing tuple element has a source mapping")
            .resolve(file, &parsed.root)
            .expect("missing tuple element syntax resolves");
        assert_eq!(missing_element.kind(), SyntaxKind::ERROR);
        assert!(missing_element.text_range().is_empty());

        let file = crate::FileId::new(91);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { call(, value<I32>) }\n");
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call with a valid unsupported sibling argument");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "the malformed sibling must not bypass preflight of valid arguments"
        );
    }

    #[test]
    fn rejects_unsupported_binary_children_before_allocating_nodes() {
        let file = crate::FileId::new(79);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { (left + value<I32>) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let binary = function
            .descendants()
            .find_map(ast::BinExpr::cast)
            .expect("binary expression");
        let expression = ast::Expr::cast(binary.syntax().clone())
            .expect("binary expression is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::PATH_EXPR))
        ));
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            1,
            "preflight rejects the generic path before allocating its binary or tuple parents"
        );
    }

    #[test]
    fn lowers_malformed_literal_to_error_without_cascading() {
        let file = crate::FileId::new(62);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { 1__2 }\n");
        let function = function_node(&parsed.root);
        let literal = function
            .descendants()
            .find_map(ast::LiteralExpr::cast)
            .expect("recovered literal");
        assert!(literal.token().expect("integer token").syntax().has_error());

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_literal(&literal, &arena, &symbols, &mut source_map)
            .expect("recovered literals lower");
        assert_eq!(lowered.kind, ExprKind::Err);
    }

    #[test]
    fn retains_recovery_values_for_error_marked_text_and_character_literals() {
        let file = crate::FileId::new(63);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, r#"fn f() { ("\q", b"\q", '\q', b'€') }"#);
        let function = function_node(&parsed.root);
        let literals = function
            .descendants()
            .filter_map(ast::LiteralExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(literals.len(), 4);
        assert!(
            literals
                .iter()
                .all(|literal| !contains_error_node(literal.syntax()))
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = literals
            .iter()
            .map(|literal| lower_literal(literal, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("error-marked literals lower");

        assert_eq!(
            lowered[0].kind,
            ExprKind::Lit(Lit::Str(
                symbols.intern("\u{fffd}").expect("replacement interns")
            ))
        );
        assert_eq!(
            lowered[1].kind,
            ExprKind::Lit(Lit::ByteStr("\u{fffd}".as_bytes()))
        );
        assert_eq!(lowered[2].kind, ExprKind::Lit(Lit::Char('\u{fffd}')));
        assert_eq!(lowered[3].kind, ExprKind::Lit(Lit::Byte(0)));
    }

    #[test]
    fn lowers_literal_patterns_and_preserves_negative_integer_values() {
        let file = crate::FileId::new(64);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r#"fn f() { match value { -42 => (), 340282366920938463463374607431768211456 => (), "text\n" => (), b"bytes\x21" => (), 'x' => (), b'y' => (), true => (), false => (), 1__2 => (), 1.5 => (), -2.5 => () } }"#,
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::LiteralPat::cast)
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 11);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::FloatPattern)
                .count(),
            2
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_literal_pat(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literal patterns lower");

        assert_eq!(
            lowered[0].kind,
            PatKind::Lit(Lit::Int {
                value: Some(42),
                negated: true,
            })
        );
        assert_eq!(
            lowered[1].kind,
            PatKind::Lit(Lit::Int {
                value: None,
                negated: false,
            })
        );
        let decoded_string = match lowered[2].kind {
            PatKind::Lit(Lit::Str(value)) => symbols.resolve(value).expect("string resolves"),
            _ => panic!("string pattern lowers to PatKind::Lit"),
        };
        assert_eq!(decoded_string.as_ref(), "text\n");
        assert_eq!(lowered[3].kind, PatKind::Lit(Lit::ByteStr(b"bytes!")));
        assert_eq!(lowered[4].kind, PatKind::Lit(Lit::Char('x')));
        assert_eq!(lowered[5].kind, PatKind::Lit(Lit::Byte(b'y')));
        assert_eq!(lowered[6].kind, PatKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[7].kind, PatKind::Lit(Lit::Bool(false)));
        assert_eq!(lowered[8].kind, PatKind::Err);
        assert_eq!(lowered[9].kind, PatKind::Err);
        assert_eq!(lowered[10].kind, PatKind::Err);
        assert!(
            lowered
                .iter()
                .enumerate()
                .all(|(index, pattern)| pattern.hir_id.local_id.raw() == index as u32 + 1)
        );
        for (pattern, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(pattern.span.range, syntax.syntax().text_range());
            assert_eq!(pattern.span.file, file);
            assert_eq!(pattern.span.ctxt, SpanCtxt::Root);
        }

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), patterns.len() + 1);
    }

    #[test]
    fn lowers_wildcard_and_strips_parenthesized_patterns() {
        let file = crate::FileId::new(65);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match value { _ => (), (true) => (), ((_)) => () } }\n",
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 3);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..3]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("supported patterns lower");

        assert_eq!(lowered[0].kind, PatKind::Wild);
        assert_eq!(lowered[1].kind, PatKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[2].kind, PatKind::Wild);
        assert_eq!(lowered[0].span.range, patterns[0].syntax().text_range());
        let ast::Pattern::Parenthesized(paren) = &patterns[1] else {
            panic!("second pattern is parenthesized");
        };
        let inner = paren.pattern().expect("parenthesized literal");
        assert_eq!(lowered[1].span.range, inner.syntax().text_range());
        let ast::Pattern::Parenthesized(outer) = &patterns[2] else {
            panic!("third pattern is parenthesized");
        };
        let ast::Pattern::Parenthesized(inner) = outer.pattern().expect("inner parentheses") else {
            panic!("third pattern has nested parentheses");
        };
        let wildcard = inner.pattern().expect("nested wildcard");
        assert_eq!(lowered[2].span.range, wildcard.syntax().text_range());
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 4);
    }

    #[test]
    fn lowers_open_inclusive_literal_and_constant_path_range_patterns() {
        let file = crate::FileId::new(66);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match value { 1..=4 => (), ..=8 => (), 9.. => (), 'a'..'z' => (), b'A'..=b'Z' => (), 1..2 => (), -2..=-1 => (), 1.5..=pkg.Status.High => (), pkg.Status.Low..=pkg.Status.High => () } }\n",
        );
        let function = function_node(&parsed.root);
        let ranges = function
            .descendants()
            .filter_map(ast::RangePat::cast)
            .collect::<Vec<_>>();
        assert_eq!(ranges.len(), 9);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::FloatPattern)
                .count(),
            1
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let patterns = ranges
            .iter()
            .map(|range| ast::Pattern::cast(range.syntax().clone()).expect("range pattern variant"))
            .collect::<Vec<_>>();
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literal range patterns lower");

        assert_eq!(
            lowered[0].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: false,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(4),
                    negated: false,
                })),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[1].kind,
            PatKind::Range {
                start: None,
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(8),
                    negated: false,
                })),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[2].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(9),
                    negated: false,
                })),
                end: None,
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[3].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Char('a'))),
                end: Some(PatRangeEnd::Lit(Lit::Char('z'))),
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[4].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Byte(b'A'))),
                end: Some(PatRangeEnd::Lit(Lit::Byte(b'Z'))),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[5].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: false,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(2),
                    negated: false,
                })),
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[6].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(2),
                    negated: true,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: true,
                })),
                inclusive: true,
            }
        );
        assert_eq!(lowered[7].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        for (pattern, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(pattern.span.range, syntax.syntax().text_range());
            assert_eq!(pattern.span.file, file);
            assert_eq!(pattern.span.ctxt, SpanCtxt::Root);
        }
        let (start_path, end_path) = match lowered[8].kind {
            PatKind::Range {
                start: Some(PatRangeEnd::Path(start_path)),
                end: Some(PatRangeEnd::Path(end_path)),
                inclusive: true,
            } => (start_path, end_path),
            _ => panic!("constant-path range lowers to two HIR paths"),
        };
        let path_names = |path: Path<'_>| {
            path.segments
                .iter()
                .map(|segment| {
                    symbols
                        .resolve(segment.name.sym)
                        .expect("path segment symbol resolves")
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(path_names(start_path).join("."), "pkg.Status.Low");
        assert_eq!(path_names(end_path).join("."), "pkg.Status.High");
        assert_eq!(start_path.root, PathRoot::Plain);
        assert_eq!(end_path.root, PathRoot::Plain);
        assert_eq!(start_path.hir_id.local_id.raw(), 10);
        assert_eq!(start_path.segments[0].hir_id.local_id.raw(), 11);
        assert_eq!(end_path.hir_id.local_id.raw(), 14);
        assert_eq!(end_path.segments[0].hir_id.local_id.raw(), 15);

        let ast::RangePatEndpoint::ConstantPath(start_syntax) =
            ranges[8].start().expect("start path syntax")
        else {
            panic!("range starts with a constant path");
        };
        let ast::RangePatEndpoint::ConstantPath(end_syntax) =
            ranges[8].end().expect("end path syntax")
        else {
            panic!("range ends with a constant path");
        };

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 18);
        assert_eq!(
            map.syntax_for_node(start_path.hir_id)
                .expect("start path source mapping")
                .resolve(file, &parsed.root)
                .expect("start path syntax resolves")
                .text_range(),
            start_syntax.syntax().text_range()
        );
        assert_eq!(
            map.syntax_for_node(end_path.segments[0].hir_id)
                .expect("end path segment source mapping")
                .resolve(file, &parsed.root)
                .expect("end path segment syntax resolves")
                .text_range(),
            end_syntax
                .segments()
                .next()
                .expect("first end path segment")
                .syntax()
                .text_range()
        );
    }

    #[test]
    fn lowers_binding_modes_at_patterns_and_normalized_names() {
        let file = crate::FileId::new(67);
        let source = "fn f() { match value { item => (), var mutable => (), borrow borrowed => (), borrow mut mutable_ref => (), named @ _ => (), outer @ (inner) => (), null => (), r#null => (), r#self => (), cafe\u{301} => (), r#Self => (), r#Self..=4 => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::NoNull)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidRawIdent)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::IdentNotNfc)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 12);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("binding patterns lower");

        let modes = lowered[..4]
            .iter()
            .map(|pattern| match pattern.kind {
                PatKind::Binding { mode, .. } => mode,
                _ => panic!("simple binding pattern lowers to PatKind::Binding"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            [
                HirBindingMode::Value,
                HirBindingMode::Var,
                HirBindingMode::Borrow,
                HirBindingMode::BorrowMut,
            ]
        );
        fn binding_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (Name, crate::BindingId, Option<&'hir Pat<'hir>>) {
            match pattern.kind {
                PatKind::Binding {
                    name, binding, sub, ..
                } => (name, binding, sub),
                _ => panic!("binding pattern lowers to PatKind::Binding"),
            }
        }
        assert_eq!(
            ["item", "mutable", "borrowed", "mutable_ref"]
                .into_iter()
                .enumerate()
                .map(|(index, expected)| {
                    symbols
                        .resolve(binding_parts(lowered[index]).0.sym)
                        .expect("binding symbol resolves")
                        .to_string()
                        == expected
                })
                .collect::<Vec<_>>(),
            [true, true, true, true]
        );
        let (named, named_binding, named_sub) = binding_parts(lowered[4]);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 4);
        assert_eq!(
            named_sub.expect("at pattern has subpattern").kind,
            PatKind::Wild
        );
        let (outer, outer_binding, outer_sub) = binding_parts(lowered[5]);
        assert_eq!(symbols.resolve(outer.sym).unwrap().as_ref(), "outer");
        assert_eq!(outer_binding.raw(), 5);
        let inner = outer_sub.expect("at pattern has subpattern");
        let (inner_name, inner_binding, inner_sub) = binding_parts(inner);
        assert_eq!(symbols.resolve(inner_name.sym).unwrap().as_ref(), "inner");
        assert_eq!(inner_binding.raw(), 6);
        assert_eq!(inner_sub, None);
        assert_eq!(
            named_sub
                .expect("at pattern has subpattern")
                .hir_id
                .local_id
                .raw(),
            6
        );
        assert_eq!(inner.hir_id.local_id.raw(), 8);
        assert!(binding_parts(lowered[6]).0.sym.is_error());
        assert_eq!(
            symbols
                .resolve(binding_parts(lowered[7]).0.sym)
                .unwrap()
                .as_ref(),
            "null"
        );
        assert!(binding_parts(lowered[8]).0.sym.is_error());
        assert_eq!(
            symbols
                .resolve(binding_parts(lowered[9]).0.sym)
                .unwrap()
                .as_ref(),
            "café"
        );
        assert_eq!(lowered[10].kind, PatKind::Err);
        assert_eq!(lowered[11].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 7, 9, 10, 11, 12, 13, 14]
        );
        let ast::Pattern::At(outer_at) = &patterns[5] else {
            panic!("sixth pattern is an at pattern");
        };
        let name_syntax = outer_at
            .binding()
            .and_then(|binding| binding.name())
            .expect("at pattern name");
        assert_eq!(lowered[5].span.range, outer_at.syntax().text_range());
        assert_eq!(
            binding_parts(lowered[5]).0.span.range,
            name_syntax.syntax().text_range()
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 15);
        assert_eq!(map.binding_to_syntax().len(), 11);

        fn collect_bindings(pattern: &ast::Pattern, bindings: &mut Vec<ast::BindingPat>) {
            match pattern {
                ast::Pattern::Binding(binding) => bindings.push(binding.clone()),
                ast::Pattern::At(at) => {
                    if let Some(binding) = at.binding() {
                        bindings.push(binding);
                    }
                    if let Some(inner) = at.pattern() {
                        collect_bindings(&inner, bindings);
                    }
                }
                ast::Pattern::Parenthesized(paren) => {
                    if let Some(inner) = paren.pattern() {
                        collect_bindings(&inner, bindings);
                    }
                }
                _ => {}
            }
        }
        let mut expected_bindings = Vec::new();
        for pattern in &patterns {
            collect_bindings(pattern, &mut expected_bindings);
        }
        assert_eq!(expected_bindings.len(), map.binding_to_syntax().len());
        for (expected_id, ((binding_id, pointer), expected)) in map
            .binding_to_syntax()
            .iter_enumerated()
            .zip(expected_bindings)
            .enumerate()
        {
            assert_eq!(binding_id.raw() as usize, expected_id);
            assert_eq!(
                pointer
                    .resolve(file, &parsed.root)
                    .expect("binding pointer resolves")
                    .text_range(),
                expected.syntax().text_range()
            );
        }
    }

    #[test]
    fn lowers_or_patterns_shares_bindings_and_reports_mismatches_and_duplicates() {
        let file = crate::FileId::new(69);
        let source = "fn f() { match value { Pair(first, second) | Pair(second, first) => (), Pair(var mode) | Pair(mode) => (), Pair(left) | Pair(right) => (), Pair(duplicate, duplicate) => (), ((nested | nested) | nested) => (), | leading | leading => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 6);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("or-patterns lower");

        fn alternatives<'hir>(pattern: &Pat<'hir>) -> &'hir [Pat<'hir>] {
            match pattern.kind {
                PatKind::Or(alternatives) => alternatives,
                _ => panic!("or-pattern lowers to PatKind::Or"),
            }
        }
        fn binding(pattern: &Pat<'_>) -> (Symbol, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name.sym, binding, mode),
                _ => panic!("binding pattern lowers to PatKind::Binding"),
            }
        }
        fn constructor_args<'hir>(pattern: &Pat<'hir>) -> &'hir [PatArg<'hir>] {
            match pattern.kind {
                PatKind::Ctor {
                    args: Some(args), ..
                } => args,
                _ => panic!("constructor pattern has arguments"),
            }
        }

        let valid_alternatives = alternatives(lowered[0]);
        assert_eq!(valid_alternatives.len(), 2);
        let first_args = constructor_args(&valid_alternatives[0]);
        let second_args = constructor_args(&valid_alternatives[1]);
        assert_eq!(binding(first_args[0].pat).1.raw(), 0);
        assert_eq!(binding(first_args[1].pat).1.raw(), 1);
        assert_eq!(binding(second_args[0].pat).1.raw(), 1);
        assert_eq!(binding(second_args[1].pat).1.raw(), 0);

        let mode_alternatives = alternatives(lowered[1]);
        assert_eq!(
            binding(constructor_args(&mode_alternatives[0])[0].pat)
                .1
                .raw(),
            2
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[1])[0].pat)
                .1
                .raw(),
            2
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[0])[0].pat).2,
            HirBindingMode::Var
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[1])[0].pat).2,
            HirBindingMode::Value
        );

        let name_alternatives = alternatives(lowered[2]);
        assert_eq!(
            binding(constructor_args(&name_alternatives[0])[0].pat)
                .1
                .raw(),
            3
        );
        assert_eq!(
            binding(constructor_args(&name_alternatives[1])[0].pat)
                .1
                .raw(),
            4
        );

        let duplicate_args = constructor_args(lowered[3]);
        assert_eq!(binding(duplicate_args[0].pat).1.raw(), 5);
        assert_eq!(binding(duplicate_args[1].pat).1.raw(), 6);

        let nested_alternatives = alternatives(lowered[4]);
        assert_eq!(nested_alternatives.len(), 3);
        assert_eq!(
            nested_alternatives
                .iter()
                .map(|pattern| binding(pattern).1.raw())
                .collect::<Vec<_>>(),
            [7, 7, 7]
        );

        let leading_alternatives = alternatives(lowered[5]);
        assert_eq!(leading_alternatives.len(), 2);
        assert_eq!(
            leading_alternatives
                .iter()
                .map(|pattern| binding(pattern).1.raw())
                .collect::<Vec<_>>(),
            [8, 8]
        );

        let map = source_map.finish();
        assert_eq!(map.binding_to_syntax().len(), 9);
        let duplicate_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 1);
        let duplicate_binding = ast::BindingPat::cast(
            patterns[3]
                .syntax()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::BINDING_PAT)
                .nth(1)
                .expect("second duplicate binding")
                .clone(),
        )
        .expect("binding syntax");
        assert_eq!(
            duplicate_diagnostics[0]
                .primary()
                .expect("duplicate diagnostic location")
                .span
                .range,
            duplicate_binding.syntax().text_range()
        );

        let mismatch_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::OrPatternBindingMismatch)
            .collect::<Vec<_>>();
        assert_eq!(mismatch_diagnostics.len(), 2);
        for ((diagnostic, pattern), expected_name) in mismatch_diagnostics
            .into_iter()
            .zip([&patterns[1], &patterns[2]])
            .zip(["mode", "left"])
        {
            assert_eq!(diagnostic.severity, sai_diagnostics::Severity::Error);
            assert!(diagnostic.message.contains(&format!("`{expected_name}`")));
            assert_eq!(
                diagnostic
                    .primary()
                    .expect("mismatch diagnostic location")
                    .span
                    .range,
                pattern.syntax().text_range()
            );
        }

        let all_binding_syntax = function
            .descendants()
            .filter_map(ast::BindingPat::cast)
            .collect::<Vec<_>>();
        let canonical_binding_indexes = [0, 1, 4, 6, 7, 8, 9, 10, 13];
        for ((_, pointer), expected_index) in map
            .binding_to_syntax()
            .iter_enumerated()
            .zip(canonical_binding_indexes)
        {
            let expected =
                sai_syntax::SyntaxNodePtr::new(file, all_binding_syntax[expected_index].syntax())
                    .expect("expected binding pointer");
            assert_eq!(pointer, &expected);
        }
    }

    #[test]
    fn keeps_nested_or_aliases_together_when_reporting_duplicate_bindings() {
        let file = crate::FileId::new(70);
        let source = "fn f() { match value { Pair(existing, Pair(existing | existing)) | Pair(existing, Pair(existing | existing)) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let pattern = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .find_map(|arm| arm.pattern())
            .expect("match arm pattern");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_pattern(&pattern, &arena, &symbols, &mut source_map)
            .expect("nested or-pattern lowers");
        let PatKind::Or(outer_alternatives) = lowered.kind else {
            panic!("outer pattern lowers to an or-pattern");
        };
        assert_eq!(outer_alternatives.len(), 2);
        let nested_binding_ids = outer_alternatives
            .iter()
            .map(|alternative| {
                let PatKind::Ctor {
                    args: Some(args), ..
                } = alternative.kind
                else {
                    panic!("outer alternative is a constructor");
                };
                let PatKind::Ctor {
                    args: Some(nested_args),
                    ..
                } = args[1].pat.kind
                else {
                    panic!("second constructor is nested");
                };
                let PatKind::Or(inner_alternatives) = nested_args[0].pat.kind else {
                    panic!("nested constructor argument is an or-pattern");
                };
                inner_alternatives
                    .iter()
                    .map(|pattern| match pattern.kind {
                        PatKind::Binding { binding, .. } => binding.raw(),
                        _ => panic!("nested or alternative is a binding"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(nested_binding_ids[0][0], nested_binding_ids[0][1]);
        assert_eq!(nested_binding_ids[1][0], nested_binding_ids[1][1]);

        let map = source_map.finish();
        assert_eq!(
            map.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
                .count(),
            2
        );
    }

    #[test]
    fn checks_bindings_from_later_or_alternatives_against_sibling_patterns() {
        let file = crate::FileId::new(71);
        let source = "fn f() { match value { (left | right | right, right) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let pattern = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .find_map(|arm| arm.pattern())
            .expect("match arm pattern");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_pattern(&pattern, &arena, &symbols, &mut source_map)
            .expect("tuple with an or-pattern lowers");
        let PatKind::Tuple(elements) = lowered.kind else {
            panic!("parenthesized comma pattern lowers to a tuple");
        };
        let PatKind::Or(alternatives) = elements[0].kind else {
            panic!("first tuple element is an or-pattern");
        };
        let alternative_bindings = alternatives
            .iter()
            .map(|pattern| match pattern.kind {
                PatKind::Binding { binding, .. } => binding.raw(),
                _ => panic!("or alternative is a binding"),
            })
            .collect::<Vec<_>>();
        assert_eq!(alternative_bindings, [0, 1, 1]);
        let PatKind::Binding {
            binding: trailing_binding,
            ..
        } = elements[1].kind
        else {
            panic!("second tuple element is a binding");
        };
        assert_eq!(trailing_binding.raw(), 2);

        let map = source_map.finish();
        let duplicate_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 1);
        assert_eq!(map.binding_to_syntax().len(), 3);
        assert_eq!(
            duplicate_diagnostics[0]
                .primary()
                .expect("duplicate diagnostic location")
                .span
                .range,
            elements[1].span.range
        );
        let binding_syntaxes = function
            .descendants()
            .filter_map(ast::BindingPat::cast)
            .collect::<Vec<_>>();
        for ((_, pointer), expected_index) in
            map.binding_to_syntax().iter_enumerated().zip([0, 1, 3])
        {
            let expected =
                sai_syntax::SyntaxNodePtr::new(file, binding_syntaxes[expected_index].syntax())
                    .expect("expected binding pointer");
            assert_eq!(pointer, &expected);
        }
        assert_eq!(
            map.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::OrPatternBindingMismatch)
                .count(),
            1
        );
    }

    #[test]
    fn lowers_tuple_patterns_and_preserves_nested_bindings() {
        let file = crate::FileId::new(68);
        let source = "fn f() { match value { () => (), (only,) => (), (first, (second,), var third,) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 3);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..3]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("tuple patterns lower");

        fn tuple_elements<'hir>(pattern: &Pat<'hir>) -> &'hir [Pat<'hir>] {
            match pattern.kind {
                PatKind::Tuple(elements) => elements,
                _ => panic!("tuple pattern lowers to PatKind::Tuple"),
            }
        }
        assert!(tuple_elements(lowered[0]).is_empty());
        let singleton = tuple_elements(lowered[1]);
        assert_eq!(singleton.len(), 1);
        let binding_parts = |pattern: &Pat<'_>| match pattern.kind {
            PatKind::Binding {
                name,
                binding,
                mode,
                ..
            } => (name, binding, mode),
            _ => panic!("tuple element lowers to PatKind::Binding"),
        };
        let (only_name, only_binding, _) = binding_parts(&singleton[0]);
        assert_eq!(symbols.resolve(only_name.sym).unwrap().as_ref(), "only");
        assert_eq!(only_binding.raw(), 0);

        let nested = tuple_elements(lowered[2]);
        assert_eq!(nested.len(), 3);
        let (first_name, first_binding, _) = binding_parts(&nested[0]);
        assert_eq!(symbols.resolve(first_name.sym).unwrap().as_ref(), "first");
        assert_eq!(first_binding.raw(), 1);
        let second = tuple_elements(&nested[1]);
        assert_eq!(second.len(), 1);
        let (second_name, second_binding, _) = binding_parts(&second[0]);
        assert_eq!(symbols.resolve(second_name.sym).unwrap().as_ref(), "second");
        assert_eq!(second_binding.raw(), 2);
        let (third_name, third_binding, third_mode) = binding_parts(&nested[2]);
        assert_eq!(symbols.resolve(third_name.sym).unwrap().as_ref(), "third");
        assert_eq!(third_binding.raw(), 3);
        assert_eq!(third_mode, HirBindingMode::Var);

        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(lowered[0].span.range, patterns[0].syntax().text_range());
        assert_eq!(
            singleton[0].span.range,
            patterns[1]
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::BINDING_PAT)
                .expect("singleton binding syntax")
                .text_range()
        );
        assert_eq!(
            second[0].hir_id.local_id.raw(),
            7,
            "nested tuple children receive owner-local IDs in source pre-order"
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 9);
        assert_eq!(map.binding_to_syntax().len(), 4);
    }

    #[test]
    fn lowers_slice_patterns_and_recovers_duplicate_rest_patterns() {
        let file = crate::FileId::new(69);
        let source = concat!(
            "fn f() { match value { ",
            "[] => (), ",
            "[first, .., last] => (), ",
            "[..borrow mut rest, after] => (), ",
            "[before, ..var tail] => (), ",
            "[.., ..] => (), ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::MultipleRestPatterns)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 5);
        let ast::Pattern::Slice(duplicate_rest) = &patterns[4] else {
            panic!("fifth pattern is a slice");
        };
        assert_eq!(duplicate_rest.rest_patterns().count(), 2);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..5]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("slice patterns lower");

        fn slice_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (
            &'hir [Pat<'hir>],
            Option<Option<&'hir Pat<'hir>>>,
            &'hir [Pat<'hir>],
        ) {
            match pattern.kind {
                PatKind::Slice {
                    before,
                    rest,
                    after,
                } => (before, rest, after),
                _ => panic!("slice pattern lowers to PatKind::Slice"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("slice element lowers to PatKind::Binding"),
            }
        }

        assert_eq!(slice_parts(lowered[0]), (&[][..], None, &[][..]));
        let (before, rest, after) = slice_parts(lowered[1]);
        assert_eq!(before.len(), 1);
        assert_eq!(rest, Some(None));
        assert_eq!(after.len(), 1);
        assert_eq!(
            symbols
                .resolve(binding_parts(&before[0]).0.sym)
                .unwrap()
                .as_ref(),
            "first"
        );
        let (_, first_binding, _) = binding_parts(&before[0]);
        assert_eq!(first_binding.raw(), 0);
        let (_, last_binding, _) = binding_parts(&after[0]);
        assert_eq!(last_binding.raw(), 1);
        assert_eq!(
            symbols
                .resolve(binding_parts(&after[0]).0.sym)
                .unwrap()
                .as_ref(),
            "last"
        );
        assert_eq!(before[0].hir_id.local_id.raw(), 3);
        assert_eq!(after[0].hir_id.local_id.raw(), 4);

        let (before, rest, after) = slice_parts(lowered[2]);
        assert!(before.is_empty());
        let rest = rest
            .expect("bound rest is present")
            .expect("rest binds a name");
        let (rest_name, rest_binding, rest_mode) = binding_parts(rest);
        assert_eq!(symbols.resolve(rest_name.sym).unwrap().as_ref(), "rest");
        assert_eq!(rest_binding.raw(), 2);
        assert_eq!(rest_mode, HirBindingMode::BorrowMut);
        assert_eq!(after.len(), 1);
        let (_, after_binding, _) = binding_parts(&after[0]);
        assert_eq!(after_binding.raw(), 3);
        assert_eq!(
            symbols
                .resolve(binding_parts(&after[0]).0.sym)
                .unwrap()
                .as_ref(),
            "after"
        );
        assert_eq!(rest.hir_id.local_id.raw(), 6);
        assert_eq!(after[0].hir_id.local_id.raw(), 7);

        let (before, rest, after) = slice_parts(lowered[3]);
        assert_eq!(before.len(), 1);
        let tail = rest
            .expect("bound rest is present")
            .expect("rest binds a name");
        let (tail_name, tail_binding, tail_mode) = binding_parts(tail);
        assert_eq!(symbols.resolve(tail_name.sym).unwrap().as_ref(), "tail");
        assert_eq!(tail_binding.raw(), 5);
        assert_eq!(tail_mode, HirBindingMode::Var);
        assert!(after.is_empty());
        let (_, before_binding, _) = binding_parts(&before[0]);
        assert_eq!(before_binding.raw(), 4);
        assert_eq!(
            symbols
                .resolve(binding_parts(&before[0]).0.sym)
                .unwrap()
                .as_ref(),
            "before"
        );
        assert_eq!(before[0].hir_id.local_id.raw(), 9);
        assert_eq!(tail.hir_id.local_id.raw(), 10);

        assert_eq!(lowered[4].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 5, 8, 11]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(lowered[1].span.range, patterns[1].syntax().text_range());
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 12);
        assert_eq!(map.binding_to_syntax().len(), 6);
    }

    #[test]
    fn lowers_implicit_member_patterns_and_preserves_argument_order() {
        let file = crate::FileId::new(70);
        let source = concat!(
            "fn f() { match value { ",
            ".Some => (), ",
            ".Some() => (), ",
            ".Some(..) => (), ",
            ".Some(field: var named, positional, ..) => (), ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 4);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..4]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("implicit member patterns lower");

        fn implicit_parts<'hir>(pattern: &Pat<'hir>) -> (Name, Option<&'hir [PatArg<'hir>]>, bool) {
            match pattern.kind {
                PatKind::ImplicitMember {
                    name,
                    args,
                    has_rest,
                } => (name, args, has_rest),
                _ => panic!("implicit member lowers to PatKind::ImplicitMember"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("argument pattern lowers to PatKind::Binding"),
            }
        }

        let (name, args, has_rest) = implicit_parts(lowered[0]);
        assert_eq!(symbols.resolve(name.sym).unwrap().as_ref(), "Some");
        assert!(args.is_none());
        assert!(!has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[1]);
        assert_eq!(args.expect("explicit empty args"), &[]);
        assert!(!has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[2]);
        assert_eq!(args.expect("explicit rest args"), &[]);
        assert!(has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[3]);
        let args = args.expect("explicit arguments");
        assert_eq!(args.len(), 2);
        assert!(has_rest);
        assert_eq!(
            symbols
                .resolve(args[0].name.expect("named argument").sym)
                .unwrap()
                .as_ref(),
            "field"
        );
        assert_eq!(args[1].name, None);
        let (named, named_binding, named_mode) = binding_parts(args[0].pat);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 0);
        assert_eq!(named_mode, HirBindingMode::Var);
        let (positional, positional_binding, positional_mode) = binding_parts(args[1].pat);
        assert_eq!(
            symbols.resolve(positional.sym).unwrap().as_ref(),
            "positional"
        );
        assert_eq!(positional_binding.raw(), 1);
        assert_eq!(positional_mode, HirBindingMode::Value);
        let ast::Pattern::ImplicitMember(mixed) = &patterns[3] else {
            panic!("fourth pattern is an implicit member");
        };
        let named_arg_syntax = mixed
            .args()
            .and_then(|args| args.named_args().next())
            .expect("named argument syntax");
        assert_eq!(
            args[0].name.expect("named argument").span.range,
            named_arg_syntax
                .name()
                .expect("named argument name")
                .syntax()
                .text_range()
        );
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(args[0].pat.hir_id.local_id.raw(), 5);
        assert_eq!(args[1].pat.hir_id.local_id.raw(), 6);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 7);
        assert_eq!(map.binding_to_syntax().len(), 2);
    }

    #[test]
    fn lowers_constructor_paths_arguments_and_source_mappings() {
        let file = crate::FileId::new(71);
        let source = concat!(
            "fn f() { match value { ",
            "Some => (), ",
            "Some() => (), ",
            "http.Status.Ok(code: var named, payload, ..) => (), ",
            "app.Result.Left(.Ready) => (), ",
            "r#Self() => () ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidRawIdent)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 5);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..4]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("constructor patterns lower");
        let invalid_raw = lower_pattern(&patterns[4], &arena, &symbols, &mut source_map)
            .expect("invalid raw constructor lowers to an error pattern");

        fn ctor_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (Path<'hir>, Option<&'hir [PatArg<'hir>]>, bool) {
            match pattern.kind {
                PatKind::Ctor {
                    path,
                    args,
                    has_rest,
                } => (path, args, has_rest),
                _ => panic!("constructor pattern lowers to PatKind::Ctor"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("constructor argument lowers to PatKind::Binding"),
            }
        }
        fn path_names<'hir>(path: &Path<'hir>, symbols: &Interner) -> Vec<String> {
            path.segments
                .iter()
                .map(|segment| {
                    symbols
                        .resolve(segment.name.sym)
                        .expect("path segment symbol resolves")
                        .to_string()
                })
                .collect()
        }

        let (first_path, args, has_rest) = ctor_parts(lowered[0]);
        assert_eq!(first_path.root, PathRoot::Plain);
        assert_eq!(path_names(&first_path, &symbols).join("."), "Some");
        assert!(args.is_none());
        assert!(!has_rest);

        let (path, args, has_rest) = ctor_parts(lowered[1]);
        assert_eq!(path_names(&path, &symbols).join("."), "Some");
        assert_eq!(args.expect("explicit empty args"), &[]);
        assert!(!has_rest);

        let (qualified_path, args, has_rest) = ctor_parts(lowered[2]);
        assert_eq!(
            path_names(&qualified_path, &symbols).join("."),
            "http.Status.Ok"
        );
        let args = args.expect("explicit constructor arguments");
        assert_eq!(args.len(), 2);
        assert!(has_rest);
        assert_eq!(
            symbols
                .resolve(args[0].name.expect("named argument").sym)
                .unwrap()
                .as_ref(),
            "code"
        );
        assert_eq!(args[1].name, None);
        let (named, named_binding, named_mode) = binding_parts(args[0].pat);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 0);
        assert_eq!(named_mode, HirBindingMode::Var);
        let (positional, positional_binding, positional_mode) = binding_parts(args[1].pat);
        assert_eq!(symbols.resolve(positional.sym).unwrap().as_ref(), "payload");
        assert_eq!(positional_binding.raw(), 1);
        assert_eq!(positional_mode, HirBindingMode::Value);

        let (nested_path, args, has_rest) = ctor_parts(lowered[3]);
        assert_eq!(
            path_names(&nested_path, &symbols).join("."),
            "app.Result.Left"
        );
        let args = args.expect("nested implicit-member argument");
        assert_eq!(args.len(), 1);
        assert!(!has_rest);
        assert!(matches!(
            args[0].pat.kind,
            PatKind::ImplicitMember { name, .. }
                if symbols.resolve(name.sym).unwrap().as_ref() == "Ready"
        ));
        assert_eq!(invalid_raw.kind, PatKind::Err);
        assert_eq!(
            invalid_raw.hir_id.local_id.raw(),
            20,
            "unsupported nested patterns are rejected before allocating the constructor"
        );

        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 4, 7, 14]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns[..4]) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        let ast::Pattern::Constructor(qualified_syntax) = &patterns[2] else {
            panic!("third arm is a constructor");
        };
        let syntax_path = qualified_syntax.path().expect("qualified constructor path");
        assert_eq!(qualified_path.span.range, syntax_path.syntax().text_range());
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 21);
        assert_eq!(map.binding_to_syntax().len(), 2);
        assert_eq!(
            map.syntax_for_node(qualified_path.hir_id)
                .expect("path source mapping")
                .resolve(file, &parsed.root)
                .expect("path syntax resolves")
                .text_range(),
            syntax_path.syntax().text_range()
        );
        assert_eq!(
            map.syntax_for_node(qualified_path.segments[0].hir_id)
                .expect("path segment source mapping")
                .resolve(file, &parsed.root)
                .expect("segment syntax resolves")
                .text_range(),
            syntax_path
                .segments()
                .next()
                .expect("first path segment")
                .syntax()
                .text_range()
        );
    }
}
