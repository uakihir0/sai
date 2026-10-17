//! Data-only resolution of `world.import_fn` field expressions.
//!
//! This pass consumes the lexical and module path results plus per-world
//! import maps supplied by its caller. It intentionally does not type-check
//! ordinary field accesses. If a world import map is incomplete, the pass
//! defers every lookup against that world: a known map entry might follow an
//! unresolved earlier include and therefore may not win first-name lookup.

use crate::owner::{LocalResolution, ModuleResolution};
use crate::package_scopes::{DefKind, Res};
use sai_diagnostics::{
    Child, ChildKind, Code, Diagnostic, Location, Severity, Span as DiagnosticSpan,
};
use sai_hir::body::Body;
use sai_hir::expressions::{Arm, Block, Expr, ExprKind, FmtPiece, Stmt, StmtKind};
use sai_hir::source_map::HirSourceMap;
use sai_hir::types::Name;
use sai_hir::{DefId, HirId, OwnerId};
use sai_intern::Interner;
use sai_vfs::WorkspaceSnapshot;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// How safely an import map can be used for world member lookup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorldImportLookupState {
    /// All reachable references were resolved and include-cycle edges were
    /// suppressed according to world elaboration.
    Complete,
    /// A reachable include is unresolved, so later declarations may not win.
    #[default]
    Incomplete,
    /// The supplied map may still contain imports contributed by an include
    /// edge that belongs to a cyclic component.
    CycleUnsafe,
}

/// The import functions known for one local package world.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorldImportFnLookup {
    /// Canonical SAI names mapped to the winning import-function definition.
    pub imports: BTreeMap<String, DefId>,
    /// Lookup state for this import map.
    ///
    /// Only `Complete` permits hits or misses. Callers must not mark a map
    /// complete until cyclic include edges have been removed: a collector
    /// that merely stops revisiting worlds can otherwise include functions
    /// that do not participate in elaboration.
    pub state: WorldImportLookupState,
    /// Canonical SAI names of this world's elaborated exported functions,
    /// including functions contributed by included worlds, for the
    /// explanatory note on a complete missing-name diagnostic.
    pub export_fns: BTreeSet<String>,
}

/// Caller-supplied inputs for resolving world member expressions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorldFieldResolutionInput {
    /// The complete set of worlds declared in the current package.
    ///
    /// Any resolved `World` DefId absent from this set is treated as an
    /// external-package world and produces `WRONG_NAME_KIND`.
    pub local_worlds: HashSet<DefId>,
    /// Import-function lookup information for local worlds.
    pub world_imports: HashMap<DefId, WorldImportFnLookup>,
}

/// Why a field expression was left for a later resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldMemberDeferral {
    /// The base is not a direct local-world path, so normal member resolution
    /// owns the expression.
    OrdinaryMember,
    /// No complete base-path resolution was supplied.
    BasePathUnresolved,
    /// A local world was recognized, but no import lookup was supplied.
    ImportLookupUnavailable,
    /// The supplied import map is partial and cannot establish the winner.
    ImportLookupIncomplete,
    /// The map may contain functions contributed by a cyclic include edge.
    CycleUnsafeImportLookup,
    /// An earlier world-member error already explains the field chain.
    SuppressedByWorldMemberError,
    /// An earlier path error already explains the field chain.
    SuppressedByPathError,
}

/// A world field left unresolved by this pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredWorldField {
    pub hir_id: HirId,
    pub reason: WorldMemberDeferral,
}

/// Resolutions and diagnostics produced for one HIR body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldFieldResolution {
    pub owner: OwnerId,
    /// Field-expression HIR IDs mapped to their resolved entities.
    pub fields: HashMap<HirId, Res>,
    pub diagnostics: Vec<Diagnostic>,
    pub deferred: Vec<DeferredWorldField>,
}

/// Errors encountered while validating the owner and source inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldFieldResolutionError {
    MismatchedOwner,
    MismatchedSource,
    MismatchedSnapshot,
    ForeignSymbol,
    InvalidSourceLocation,
}

#[derive(Clone, Copy)]
struct FieldCandidate<'hir> {
    hir_id: HirId,
    base: &'hir Expr<'hir>,
    name: Name,
}

enum Walk<'hir> {
    Expr(&'hir Expr<'hir>),
    Block(&'hir Block<'hir>),
    Stmt(&'hir Stmt<'hir>),
    Arm(&'hir Arm<'hir>),
}

/// Resolves direct local-world field expressions such as `w.f`.
///
/// Lexical path results take precedence over module path results. A direct
/// field on a local world resolves only when its complete import lookup is
/// available. A `Field` over a resolved `WorldFn` is a wrong-kind chain;
/// calls of that function produce ordinary values, so a following field on a
/// `Call` is left to normal member resolution.
pub fn resolve_owner_world_fields(
    body: &Body<'_>,
    source_map: &HirSourceMap,
    snapshot: &WorkspaceSnapshot,
    symbols: &Interner,
    locals: &LocalResolution,
    module_paths: &ModuleResolution,
    input: &WorldFieldResolutionInput,
) -> Result<WorldFieldResolution, WorldFieldResolutionError> {
    if locals.owner != body.owner || module_paths.owner != body.owner {
        return Err(WorldFieldResolutionError::MismatchedOwner);
    }
    if !locals.matches_body_source(body, source_map, snapshot) {
        return Err(WorldFieldResolutionError::MismatchedSource);
    }
    if !source_map.matches_snapshot_file(snapshot) {
        return Err(WorldFieldResolutionError::MismatchedSnapshot);
    }

    let mut fields = Vec::new();
    collect_fields(body, &mut fields)?;
    let mut result = WorldFieldResolution {
        owner: body.owner,
        fields: HashMap::new(),
        diagnostics: Vec::new(),
        deferred: Vec::new(),
    };
    let mut failed_world_fields = HashSet::new();
    let mut failed_path_fields = HashSet::new();

    // The walk records a field before its base. Reversing the candidates
    // resolves inner world fields before diagnosing an additional segment.
    for field in fields.into_iter().rev() {
        if field.hir_id.owner != body.owner {
            return Err(WorldFieldResolutionError::MismatchedOwner);
        }

        let deferred = |reason| DeferredWorldField {
            hir_id: field.hir_id,
            reason,
        };

        match field.base.kind {
            ExprKind::Path(path) => {
                if path.hir_id.owner != body.owner {
                    return Err(WorldFieldResolutionError::MismatchedOwner);
                }
                let path_res = locals
                    .paths
                    .get(&path.hir_id)
                    .or_else(|| module_paths.paths.get(&path.hir_id))
                    .copied();
                let Some(path_res) =
                    path_res.filter(|res| res.resolved_segments == res.segment_count)
                else {
                    result
                        .deferred
                        .push(deferred(WorldMemberDeferral::BasePathUnresolved));
                    continue;
                };

                match path_res.base {
                    Res::Err | Res::TestOnly(_) => {
                        failed_path_fields.insert(field.hir_id);
                        result
                            .deferred
                            .push(deferred(WorldMemberDeferral::SuppressedByPathError));
                    }
                    Res::Def(DefKind::World, world) if input.local_worlds.contains(&world) => {
                        let Some(lookup) = input.world_imports.get(&world) else {
                            failed_world_fields.insert(field.hir_id);
                            result
                                .deferred
                                .push(deferred(WorldMemberDeferral::ImportLookupUnavailable));
                            continue;
                        };
                        if lookup.state != WorldImportLookupState::Complete {
                            failed_world_fields.insert(field.hir_id);
                            let reason = match lookup.state {
                                WorldImportLookupState::Complete => unreachable!(),
                                WorldImportLookupState::Incomplete => {
                                    WorldMemberDeferral::ImportLookupIncomplete
                                }
                                WorldImportLookupState::CycleUnsafe => {
                                    WorldMemberDeferral::CycleUnsafeImportLookup
                                }
                            };
                            result.deferred.push(deferred(reason));
                            continue;
                        }
                        let Some(spelling) = resolve_field_name(field.name, symbols)? else {
                            failed_path_fields.insert(field.hir_id);
                            result
                                .deferred
                                .push(deferred(WorldMemberDeferral::SuppressedByPathError));
                            continue;
                        };
                        if let Some(def_id) = lookup.imports.get(&spelling).copied() {
                            result
                                .fields
                                .insert(field.hir_id, Res::Def(DefKind::WorldFn, def_id));
                        } else {
                            failed_world_fields.insert(field.hir_id);
                            result.diagnostics.push(make_field_diagnostic(
                                Code::UnresolvedName,
                                "unresolved world function",
                                format!("no imported world function `{spelling}`"),
                                field.name,
                                source_map,
                                snapshot,
                                lookup.export_fns.contains(&spelling),
                            )?);
                        }
                    }
                    Res::Def(DefKind::World, _) => {
                        let Some(spelling) = resolve_field_name(field.name, symbols)? else {
                            failed_path_fields.insert(field.hir_id);
                            result
                                .deferred
                                .push(deferred(WorldMemberDeferral::SuppressedByPathError));
                            continue;
                        };
                        failed_world_fields.insert(field.hir_id);
                        result.diagnostics.push(make_field_diagnostic(
                            Code::WrongNameKind,
                            "wrong name kind",
                            format!(
                                "world member access is only available for worlds in the current package; `{spelling}` is not an imported function here"
                            ),
                            field.name,
                            source_map,
                            snapshot,
                            false,
                        )?);
                    }
                    _ => result
                        .deferred
                        .push(deferred(WorldMemberDeferral::OrdinaryMember)),
                }
            }
            ExprKind::Field { .. }
                if matches!(
                    result.fields.get(&field.base.hir_id),
                    Some(Res::Def(DefKind::WorldFn, _))
                ) =>
            {
                let Some(spelling) = resolve_field_name(field.name, symbols)? else {
                    failed_path_fields.insert(field.hir_id);
                    result
                        .deferred
                        .push(deferred(WorldMemberDeferral::SuppressedByPathError));
                    continue;
                };
                failed_world_fields.insert(field.hir_id);
                result.diagnostics.push(make_field_diagnostic(
                    Code::WrongNameKind,
                    "wrong name kind",
                    format!("world function has no member `{spelling}`"),
                    field.name,
                    source_map,
                    snapshot,
                    false,
                )?);
            }
            ExprKind::Field { .. } if failed_world_fields.contains(&field.base.hir_id) => {
                failed_world_fields.insert(field.hir_id);
                result
                    .deferred
                    .push(deferred(WorldMemberDeferral::SuppressedByWorldMemberError));
            }
            ExprKind::Field { .. } if failed_path_fields.contains(&field.base.hir_id) => {
                failed_path_fields.insert(field.hir_id);
                result
                    .deferred
                    .push(deferred(WorldMemberDeferral::SuppressedByPathError));
            }
            _ => result
                .deferred
                .push(deferred(WorldMemberDeferral::OrdinaryMember)),
        }
    }

    result.diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start().raw())
            .unwrap_or(u32::MAX)
    });
    result.deferred.sort_by_key(|field| {
        // HIR local IDs are stable within this owner and serve only as a
        // deterministic tie-breaker for equal-source-position expressions.
        field.hir_id.local_id
    });
    Ok(result)
}

fn collect_fields<'hir>(
    body: &Body<'hir>,
    fields: &mut Vec<FieldCandidate<'hir>>,
) -> Result<(), WorldFieldResolutionError> {
    let roots = body
        .invariant_preds
        .iter()
        .copied()
        .chain(body.contracts.iter().map(|contract| contract.expr))
        .chain([body.value])
        .collect::<Vec<_>>();
    let mut work = roots.into_iter().rev().map(Walk::Expr).collect::<Vec<_>>();

    while let Some(task) = work.pop() {
        match task {
            Walk::Expr(expr) => {
                if expr.hir_id.owner != body.owner {
                    return Err(WorldFieldResolutionError::MismatchedOwner);
                }
                match expr.kind {
                    ExprKind::Lit(_)
                    | ExprKind::ImplicitMember(_)
                    | ExprKind::ContractResult
                    | ExprKind::Continue { .. }
                    | ExprKind::Err => {}
                    ExprKind::Path(_) => {}
                    ExprKind::Field { base, name, .. } => {
                        fields.push(FieldCandidate {
                            hir_id: expr.hir_id,
                            base,
                            name,
                        });
                        work.push(Walk::Expr(base));
                    }
                    ExprKind::TupleField { base, .. }
                    | ExprKind::Unary(_, base)
                    | ExprKind::Borrow(_, base)
                    | ExprKind::Try(base)
                    | ExprKind::Await(base) => work.push(Walk::Expr(base)),
                    ExprKind::Call { callee, args, base } => {
                        if let Some(base) = base {
                            work.push(Walk::Expr(base));
                        }
                        work.extend(args.iter().rev().map(|arg| Walk::Expr(arg.expr)));
                        work.push(Walk::Expr(callee));
                    }
                    ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                        work.push(Walk::Expr(right));
                        work.push(Walk::Expr(left));
                    }
                    ExprKind::Range { start, end, .. } => {
                        if let Some(end) = end {
                            work.push(Walk::Expr(end));
                        }
                        if let Some(start) = start {
                            work.push(Walk::Expr(start));
                        }
                    }
                    ExprKind::Tuple(items) | ExprKind::Array(items) => {
                        work.extend(items.iter().rev().map(Walk::Expr));
                    }
                    ExprKind::Block(block) | ExprKind::AsyncBlock { block, .. } => {
                        work.push(Walk::Block(block));
                    }
                    ExprKind::If { cond, then, els } => {
                        if let Some(els) = els {
                            work.push(Walk::Expr(els));
                        }
                        work.push(Walk::Block(then));
                        work.push(Walk::Expr(cond));
                    }
                    ExprKind::Match {
                        scrutinee, arms, ..
                    } => {
                        work.extend(arms.iter().rev().map(Walk::Arm));
                        work.push(Walk::Expr(scrutinee));
                    }
                    ExprKind::Loop { body, .. } => work.push(Walk::Block(body)),
                    ExprKind::Break { value, .. } | ExprKind::Return(value) => {
                        if let Some(value) = value {
                            work.push(Walk::Expr(value));
                        }
                    }
                    ExprKind::Closure(closure) => work.push(Walk::Block(closure.body)),
                    ExprKind::Region { block, .. } => work.push(Walk::Block(block)),
                    ExprKind::FormatString(pieces) => {
                        for piece in pieces.iter().rev() {
                            if let FmtPiece::Arg { expr, .. } = piece {
                                work.push(Walk::Expr(expr));
                            }
                        }
                    }
                    ExprKind::ContractOld { args, base } => {
                        if let Some(base) = base {
                            work.push(Walk::Expr(base));
                        }
                        work.extend(args.iter().rev().map(|arg| Walk::Expr(arg.expr)));
                    }
                    ExprKind::AsCast { operand, .. } => work.push(Walk::Expr(operand)),
                }
            }
            Walk::Block(block) => {
                if block.hir_id.owner != body.owner {
                    return Err(WorldFieldResolutionError::MismatchedOwner);
                }
                if let Some(tail) = block.tail {
                    work.push(Walk::Expr(tail));
                }
                work.extend(block.stmts.iter().rev().map(Walk::Stmt));
            }
            Walk::Stmt(stmt) => {
                if stmt.hir_id.owner != body.owner {
                    return Err(WorldFieldResolutionError::MismatchedOwner);
                }
                match stmt.kind {
                    StmtKind::Let { init, els, .. } => {
                        if let Some(els) = els {
                            work.push(Walk::Block(els));
                        }
                        work.push(Walk::Expr(init));
                    }
                    StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
                        work.push(Walk::Expr(rhs));
                        work.push(Walk::Expr(lhs));
                    }
                    StmtKind::Assert { cond, msg, .. } => {
                        if let Some(msg) = msg {
                            work.push(Walk::Expr(msg));
                        }
                        work.push(Walk::Expr(cond));
                    }
                    StmtKind::Expr(expr) => work.push(Walk::Expr(expr)),
                }
            }
            Walk::Arm(arm) => {
                if arm.hir_id.owner != body.owner {
                    return Err(WorldFieldResolutionError::MismatchedOwner);
                }
                work.push(Walk::Expr(arm.body));
                if let Some(guard) = arm.guard {
                    work.push(Walk::Expr(guard));
                }
            }
        }
    }
    Ok(())
}

fn resolve_field_name(
    name: Name,
    symbols: &Interner,
) -> Result<Option<String>, WorldFieldResolutionError> {
    if name.sym.is_error() {
        return Ok(None);
    }
    symbols
        .resolve(name.sym)
        .map(|spelling| Some(spelling.to_string()))
        .map_err(|_| WorldFieldResolutionError::ForeignSymbol)
}

fn make_field_diagnostic(
    code: Code,
    primary_label: &str,
    message: String,
    name: Name,
    source_map: &HirSourceMap,
    snapshot: &WorkspaceSnapshot,
    has_export_fn: bool,
) -> Result<Diagnostic, WorldFieldResolutionError> {
    if name.span.file != source_map.file() {
        return Err(WorldFieldResolutionError::MismatchedSource);
    }
    let source_file = snapshot
        .file(source_map.file())
        .ok_or(WorldFieldResolutionError::MismatchedSnapshot)?;
    let primary = Location::from_decoded_source(
        source_map.source_ref().clone(),
        DiagnosticSpan::new(name.span.file, name.span.range),
        source_file.source(),
    )
    .ok_or(WorldFieldResolutionError::InvalidSourceLocation)?;
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message, Some(primary));
    diagnostic.primary_label = primary_label.to_owned();
    diagnostic.package = source_map.source_ref().package.clone();
    if has_export_fn {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "an exported world function is called through its implementation."
                    .to_owned(),
                location: None,
            })
            .map_err(|_| WorldFieldResolutionError::InvalidSourceLocation)?;
    }
    diagnostic
        .with_workspace_snapshot(snapshot)
        .map_err(|_| WorldFieldResolutionError::InvalidSourceLocation)
}
