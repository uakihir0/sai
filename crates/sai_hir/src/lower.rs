use crate::SpanCtxt;
use crate::Symbol;
use crate::arena::HirArena;
use crate::expressions::{Expr, ExprKind};
use crate::literals::Lit;
use crate::patterns::{BindingMode as HirBindingMode, Pat, PatArg, PatKind, PatRangeEnd};
use crate::source_map::{HirSourceMapBuilder, HirSourceMapError};
use crate::types::{Name, Path, PathRoot, PathSegment};
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
    let (hir_id, span) = source_map.add_node_with_span(literal.syntax(), SpanCtxt::Root)?;
    let kind = if contains_error_node(literal.syntax()) {
        ExprKind::Err
    } else {
        literal
            .token()
            .map(|token| decode_literal(token, arena, symbols, true, false))
            .transpose()?
            .flatten()
            .map(ExprKind::Lit)
            .unwrap_or(ExprKind::Err)
    };
    Ok(arena.alloc_expr(Expr { hir_id, kind, span }))
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

/// Lower a range pattern with literal endpoints.
///
/// Constant-path endpoints are rejected explicitly until path lowering is
/// available. Invalid endpoint syntax becomes `PatKind::Err`.
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
    let has_invalid_raw_path = [start_syntax.as_ref(), end_syntax.as_ref()]
        .into_iter()
        .flatten()
        .any(|endpoint| match endpoint {
            ast::RangePatEndpoint::ConstantPath(path) => {
                const_path_has_invalid_raw_identifier(path.clone())
            }
            ast::RangePatEndpoint::Literal(_) => false,
        });
    if has_invalid_raw_path {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }
    if start_syntax
        .as_ref()
        .is_some_and(|endpoint| matches!(endpoint, ast::RangePatEndpoint::ConstantPath(_)))
        || end_syntax
            .as_ref()
            .is_some_and(|endpoint| matches!(endpoint, ast::RangePatEndpoint::ConstantPath(_)))
    {
        return Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::CONST_PATH));
    }

    let start = start_syntax
        .map(|endpoint| lower_range_endpoint(endpoint, arena, symbols))
        .transpose()?;
    let end = end_syntax
        .map(|endpoint| lower_range_endpoint(endpoint, arena, symbols))
        .transpose()?;
    let inclusive = range.syntax().children_with_tokens().any(|element| {
        matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2EQ)
    });
    let invalid_shape = if inclusive {
        end.is_none()
    } else {
        start.is_none()
    };
    let invalid_endpoint = start.is_some_and(|endpoint| endpoint.is_none())
        || end.is_some_and(|endpoint| endpoint.is_none());
    let (hir_id, span) = source_map.add_node_with_span(range.syntax(), SpanCtxt::Root)?;
    let kind = if invalid_shape || invalid_endpoint {
        PatKind::Err
    } else {
        PatKind::Range {
            start: start.flatten(),
            end: end.flatten(),
            inclusive,
        }
    };
    Ok(arena.alloc_pat(Pat { hir_id, kind, span }))
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
            if constructor
                .path()
                .is_some_and(const_path_has_invalid_raw_identifier) =>
        {
            lower_error_pattern(constructor.syntax(), arena, source_map)
        }
        ast::Pattern::Constructor(constructor) => {
            lower_constructor_pattern(constructor, arena, symbols, source_map)
        }
        ast::Pattern::ImplicitMember(member) => {
            lower_implicit_member_pattern(member, arena, symbols, source_map)
        }
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => lower_pattern(&inner, arena, symbols, source_map),
            None => lower_error_pattern(paren.syntax(), arena, source_map),
        },
        _ => Err(HirLoweringError::UnsupportedSyntax(pattern.syntax().kind())),
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

fn lower_tuple_pattern<'hir>(
    tuple: &ast::TuplePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
    let elements = tuple
        .elements()
        .map(|element| lower_pattern(&element, arena, symbols, source_map).copied())
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
        .map(|element| lower_pattern(element, arena, symbols, source_map).copied())
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
        .map(|element| lower_pattern(element, arena, symbols, source_map).copied())
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

fn ensure_supported_pattern(pattern: &ast::Pattern) -> Result<(), HirLoweringError> {
    if contains_error_node(pattern.syntax()) {
        return Ok(());
    }

    match pattern {
        ast::Pattern::Wildcard(_) | ast::Pattern::Literal(_) | ast::Pattern::Binding(_) => Ok(()),
        ast::Pattern::Range(range) => {
            let endpoints = [range.start(), range.end()];
            let has_invalid_raw_path = endpoints.iter().flatten().any(|endpoint| match endpoint {
                ast::RangePatEndpoint::ConstantPath(path) => {
                    const_path_has_invalid_raw_identifier(path.clone())
                }
                ast::RangePatEndpoint::Literal(_) => false,
            });
            if has_invalid_raw_path {
                return Ok(());
            }
            for endpoint in endpoints.into_iter().flatten() {
                if let ast::RangePatEndpoint::ConstantPath(path) = endpoint {
                    return Err(HirLoweringError::UnsupportedSyntax(path.syntax().kind()));
                }
            }
            Ok(())
        }
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
            if constructor
                .path()
                .is_some_and(const_path_has_invalid_raw_identifier) =>
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
        _ => Err(HirLoweringError::UnsupportedSyntax(pattern.syntax().kind())),
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
                let pattern = lower_pattern(pattern, arena, symbols, source_map)?;
                lowered.push(PatArg {
                    name: None,
                    pat: pattern,
                });
            }
            CtorPatArgSyntax::Named { name, pattern } => {
                let name = lower_name_ref(name, symbols, source_map)?;
                let pattern = lower_pattern(pattern, arena, symbols, source_map)?;
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
    let (binding, _) = source_map.add_binding_with_span(binding_pat.syntax(), SpanCtxt::Root)?;
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
    let (binding, _) = source_map.add_binding_with_span(binding_pat.syntax(), SpanCtxt::Root)?;
    let sub = lower_pattern(&inner, arena, symbols, source_map)?;
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

fn lower_range_endpoint<'hir>(
    endpoint: ast::RangePatEndpoint,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
) -> Result<Option<PatRangeEnd<'hir>>, HirLoweringError> {
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
            Ok(decoded.map(PatRangeEnd::Lit))
        }
        ast::RangePatEndpoint::ConstantPath(path) => {
            Err(HirLoweringError::UnsupportedSyntax(path.syntax().kind()))
        }
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
        ast::LiteralToken::Float(token)
            if allow_float && !negated && !token.syntax().has_error() =>
        {
            let text = token.syntax().text().replace('_', "");
            Some(Lit::Float {
                text: symbols.intern(&text)?,
                negated: false,
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
            "fn f() { match value { _ => (), (true) => (), ((_)) => (), [left, first | second] => () } }\n",
        );
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
        assert_eq!(
            lower_pattern(&patterns[3], &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::OR_PAT)
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 4);
    }

    #[test]
    fn lowers_open_inclusive_and_literal_range_patterns() {
        let file = crate::FileId::new(66);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match value { 1..=4 => (), ..=8 => (), 9.. => (), 'a'..'z' => (), b'A'..=b'Z' => (), 1..2 => (), -2..=-1 => (), 1..=2.5 => (), Low..=High => () } }\n",
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
        let patterns = ranges[..8]
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
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        for (pattern, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(pattern.span.range, syntax.syntax().text_range());
            assert_eq!(pattern.span.file, file);
            assert_eq!(pattern.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(
            lower_pattern(
                &ast::Pattern::cast(ranges[8].syntax().clone()).expect("const range pattern"),
                &arena,
                &symbols,
                &mut source_map,
            )
            .unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::CONST_PATH)
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 9);
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
    fn lowers_tuple_patterns_and_preflights_unsupported_children() {
        let file = crate::FileId::new(68);
        let source = "fn f() { match value { () => (), (only,) => (), (first, (second,), var third,) => (), (before, left | right) => () } }";
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

        assert_eq!(
            lower_pattern(&patterns[3], &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::OR_PAT)
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
            "[before, left | right] => () ",
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
        assert_eq!(patterns.len(), 6);
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
        assert_eq!(
            lower_pattern(&patterns[5], &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::OR_PAT)
        );

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
            ".Some(before, left | right) => () ",
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
        assert!(
            matches!(
                lower_pattern(&patterns[4], &arena, &symbols, &mut source_map),
                Err(HirLoweringError::UnsupportedSyntax(SyntaxKind::OR_PAT))
            ),
            "unsupported nested or-patterns are rejected before allocation"
        );

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
            "app.Result.Left(first | second) => (), ",
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
        assert_eq!(patterns.len(), 6);

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
        assert_eq!(
            lower_pattern(&patterns[4], &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::OR_PAT)
        );
        let invalid_raw = lower_pattern(&patterns[5], &arena, &symbols, &mut source_map)
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
