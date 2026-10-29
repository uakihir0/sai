//! Conservative owner-local unused binding analysis, without lint emission.

use std::collections::HashSet;

use sai_diagnostics::Diagnostic;
use sai_hir::body::Body;
use sai_hir::expressions::{Block, Expr, ExprKind, Stmt, StmtKind};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, PatKind};
use sai_hir::source_map::HirSourceMap;
use sai_hir::types::PathRoot;
use sai_hir::{BindingId, OwnerId, Span, SpanCtxt, Symbol};
use sai_intern::Interner;
use sai_vfs::WorkspaceSnapshot;

use crate::owner::{BindingKind, resolve_owner_locals};
use crate::package_scopes::Res;

/// A candidate is not a diagnostic: callers must still resolve lint levels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnusedBinding {
    pub binding: BindingId,
    pub name: Symbol,
    pub span: Span,
}

/// Incomplete analyses never expose candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnusedBindingAnalysis {
    Complete {
        owner: OwnerId,
        unused: Vec<UnusedBinding>,
    },
    Incomplete,
}

/// The module/interface aliases visible at the analyzed owner.
///
/// `Complete` is a caller assertion: the set must contain every alias that
/// owner-local resolution would check for `SHADOWS_MODULE`, from the fully
/// resolved module scope of this owner, snapshot, build context, and symbol
/// interner. This includes applicable test-only aliases, including those
/// retained after resolution errors. An empty set is complete only when that
/// scope has no such aliases. A partial, stale, or different-owner set is not
/// complete; its absence cannot be detected from the set itself. When scope
/// construction or its completeness is unknown, pass `Unavailable` instead.
#[derive(Clone, Copy, Debug)]
pub enum ModuleAliases<'a> {
    Complete(&'a HashSet<Symbol>),
    Unavailable,
}

enum Work<'hir> {
    Expr(&'hir Expr<'hir>),
    Block(&'hir Block<'hir>),
    Stmt(&'hir Stmt<'hir>),
}

/// Analyze the deliberately narrow, non-desugared body subset.
///
/// `lowering_diagnostics` must be the complete diagnostics for this exact
/// lowered owner. The caller must supply a real body, not a body synthesized
/// for a bodyless declaration. An unsupported or recovered form, stale input,
/// or unresolved value path makes the entire result incomplete. This subset
/// does not yet handle receivers, contracts, patterns, closures or format
/// strings; it cannot be used to claim general lint coverage.
///
/// `Complete` is conditional on the caller upholding the completeness
/// contract of `module_aliases`; the input cannot prove that an alias was not
/// omitted. `ModuleAliases::Unavailable` always yields `Incomplete`.
pub fn analyze_unused_bindings(
    body: &Body<'_>,
    source_map: &HirSourceMap,
    snapshot: &WorkspaceSnapshot,
    symbols: &Interner,
    module_aliases: ModuleAliases<'_>,
    lowering_diagnostics: &[Diagnostic],
) -> UnusedBindingAnalysis {
    let ModuleAliases::Complete(module_aliases) = module_aliases else {
        return UnusedBindingAnalysis::Incomplete;
    };
    if !lowering_diagnostics.is_empty()
        || !source_map.matches_body(body)
        || !source_map.matches_snapshot_file(snapshot)
        || !body.contracts.is_empty()
        || !body.invariant_preds.is_empty()
    {
        return UnusedBindingAnalysis::Incomplete;
    }
    let Ok(parsed) = sai_syntax::parse(snapshot, source_map.file()) else {
        return UnusedBindingAnalysis::Incomplete;
    };
    if !parsed.errors.is_empty() {
        return UnusedBindingAnalysis::Incomplete;
    }

    let mut declarations = HashSet::new();
    for param in body.params {
        if !declarations.insert(param.binding) {
            return UnusedBindingAnalysis::Incomplete;
        }
        if let Some(name) = param.name {
            let Ok(spelling) = symbols.resolve(name.sym) else {
                return UnusedBindingAnalysis::Incomplete;
            };
            if spelling.as_ref() == "self" || name.span.ctxt != SpanCtxt::Root {
                return UnusedBindingAnalysis::Incomplete;
            }
        }
    }

    let mut work = vec![Work::Expr(body.value)];
    while let Some(task) = work.pop() {
        match task {
            Work::Expr(expr) if expr.span.ctxt != SpanCtxt::Root => {
                return UnusedBindingAnalysis::Incomplete;
            }
            Work::Expr(expr) => match expr.kind {
                ExprKind::Lit(Lit::Int { value: None, .. }) | ExprKind::Err => {
                    return UnusedBindingAnalysis::Incomplete;
                }
                ExprKind::Lit(_) => {}
                ExprKind::Path(path)
                    if path.root == PathRoot::Plain
                        && path.segments.len() == 1
                        && path.segments[0].args.is_none() => {}
                ExprKind::Unary(_, operand) | ExprKind::Borrow(_, operand) => {
                    work.push(Work::Expr(operand));
                }
                ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                    work.push(Work::Expr(left));
                    work.push(Work::Expr(right));
                }
                ExprKind::Tuple(items) | ExprKind::Array(items) => {
                    work.extend(items.iter().map(Work::Expr));
                }
                ExprKind::Block(block) => work.push(Work::Block(block)),
                ExprKind::If { cond, then, els } => {
                    work.push(Work::Expr(cond));
                    work.push(Work::Block(then));
                    if let Some(els) = els {
                        work.push(Work::Expr(els));
                    }
                }
                ExprKind::Return(value) => {
                    if let Some(value) = value {
                        work.push(Work::Expr(value));
                    }
                }
                _ => return UnusedBindingAnalysis::Incomplete,
            },
            Work::Block(block) => {
                if block.span.ctxt != SpanCtxt::Root {
                    return UnusedBindingAnalysis::Incomplete;
                }
                work.extend(block.stmts.iter().map(Work::Stmt));
                if let Some(tail) = block.tail {
                    work.push(Work::Expr(tail));
                }
            }
            Work::Stmt(stmt) => {
                if stmt.span.ctxt != SpanCtxt::Root {
                    return UnusedBindingAnalysis::Incomplete;
                }
                match stmt.kind {
                    StmtKind::Let {
                        pat,
                        ty: None,
                        init,
                        els: None,
                    } => {
                        match pat.kind {
                            PatKind::Wild => {}
                            PatKind::Binding {
                                mode: BindingMode::Value,
                                name,
                                binding,
                                sub: None,
                            } if pat.span.ctxt == SpanCtxt::Root
                                && name.span.ctxt == SpanCtxt::Root
                                && declarations.insert(binding) => {}
                            _ => return UnusedBindingAnalysis::Incomplete,
                        }
                        work.push(Work::Expr(init));
                    }
                    StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
                        work.push(Work::Expr(lhs));
                        work.push(Work::Expr(rhs));
                    }
                    StmtKind::Expr(expr) => work.push(Work::Expr(expr)),
                    _ => return UnusedBindingAnalysis::Incomplete,
                }
            }
        }
    }

    let Ok(locals) = resolve_owner_locals(body, source_map, snapshot, symbols, module_aliases)
    else {
        return UnusedBindingAnalysis::Incomplete;
    };
    if !locals.diagnostics.is_empty() || !locals.deferred_paths.is_empty() {
        return UnusedBindingAnalysis::Incomplete;
    }
    let mut used = HashSet::new();
    for resolution in locals.paths.values() {
        match resolution.base {
            Res::Local(binding)
                if resolution.resolved_segments == 1
                    && resolution.segment_count == 1
                    && declarations.contains(&binding) =>
            {
                used.insert(binding);
            }
            _ => return UnusedBindingAnalysis::Incomplete,
        }
    }
    if locals.bindings.len() != declarations.len() {
        return UnusedBindingAnalysis::Incomplete;
    }
    let mut unused = Vec::new();
    for (binding, local) in &locals.bindings {
        if !declarations.contains(binding)
            || !matches!(
                local.kind,
                BindingKind::Let | BindingKind::Param | BindingKind::VarParam
            )
        {
            return UnusedBindingAnalysis::Incomplete;
        }
        let (Some(name), Some(span)) = (local.name, local.span) else {
            continue;
        };
        let Ok(spelling) = symbols.resolve(name) else {
            return UnusedBindingAnalysis::Incomplete;
        };
        if !spelling.starts_with('_') && !used.contains(binding) {
            unused.push(UnusedBinding {
                binding: *binding,
                name,
                span,
            });
        }
    }
    unused.sort_by_key(|candidate| (candidate.span.range.start(), candidate.span.range.end()));
    UnusedBindingAnalysis::Complete {
        owner: body.owner,
        unused,
    }
}
