use crate::SpanCtxt;
use crate::arena::HirArena;
use crate::expressions::{Expr, ExprKind};
use crate::literals::Lit;
use crate::patterns::{Pat, PatKind, PatRangeEnd};
use crate::source_map::{HirSourceMapBuilder, HirSourceMapError};
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

/// Lower the pattern forms that do not require recursive child construction
/// beyond parenthesis stripping.
///
/// Unsupported, otherwise-valid pattern kinds are returned explicitly so a
/// caller cannot mistake an unfinished lowering case for a source error.
pub fn lower_pattern<'hir>(
    pattern: &ast::Pattern,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
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
            "fn f() { match value { _ => (), (true) => (), ((_)) => (), (left, right) => () } }\n",
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
            HirLoweringError::UnsupportedSyntax(SyntaxKind::TUPLE_PAT)
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
}
