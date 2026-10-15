//! Owner-local control-flow resolution.
//!
//! This module contains owner-local name/control-flow resolution helpers. They
//! return owned maps and diagnostics, so no HIR arena borrows escape a pass.

use sai_diagnostics::{
    Child, ChildKind, Code, Diagnostic, Location, Related, Severity, SourceRef,
    Span as DiagnosticSpan,
};
use sai_hir::body::Body;
use sai_hir::expressions::{
    Arg, Arm, Block, ClosureParam, Expr, ExprKind, FmtPiece, Label, Stmt, StmtKind,
};
use sai_hir::patterns::{BindingMode, Pat, PatKind, PatRangeEnd};
use sai_hir::source_map::HirSourceMap;
use sai_hir::types::{GenericArg, GenericArgs, PathRoot, Ty, TyKind};
use sai_hir::{
    BindingId, DesugaringKind, HirId, OwnerId, Safety, Span as HirSpan, SpanCtxt, Symbol,
};
use sai_intern::Interner;
use sai_vfs::{FileId, SourceFile, WorkspaceSnapshot};
use std::collections::{HashMap, HashSet};

use super::package_scopes::{PathRes, Phase1ModuleScopes, Res};

/// The class of a local binding retained for later type and lint analysis.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingKind {
    Let,
    Var,
    Param,
    VarParam,
    Pattern { var: bool },
    ClosureParam { var: bool },
    Synthetic,
}

/// Snapshot-relative metadata for one binding ID in an owner's HIR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalBinding {
    pub name: Option<Symbol>,
    pub span: Option<HirSpan>,
    pub kind: BindingKind,
}

/// An unqualified expression path that missed the lexical-local pass.
///
/// Candidates preserve source order and location for generic,
/// module, prelude, and diagnostic fallback passes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredPath {
    pub hir_id: HirId,
    pub name: Symbol,
    pub span: HirSpan,
}

/// The local portion of path resolution for one HIR owner.
///
/// A missing path is intentionally left for generic, module, and prelude
/// lookup. This result owns no HIR references. Its IDs and spans are tied to
/// the source map's owner file and the module aliases used during resolution.
#[derive(Clone, Debug)]
pub struct LocalResolution {
    pub owner: OwnerId,
    pub source_ref: SourceRef,
    pub source_file: FileId,
    pub paths: HashMap<HirId, PathRes>,
    pub bindings: HashMap<BindingId, LocalBinding>,
    pub diagnostics: Vec<Diagnostic>,
    pub deferred_paths: Vec<DeferredPath>,
    body_source_id: sai_hir::source_map::HirSourceId,
    module_aliases: HashSet<Symbol>,
}

impl PartialEq for LocalResolution {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.source_ref == other.source_ref
            && self.source_file == other.source_file
            && self.paths == other.paths
            && self.bindings == other.bindings
            && self.diagnostics == other.diagnostics
            && self.deferred_paths == other.deferred_paths
            && self.module_aliases == other.module_aliases
    }
}

impl Eq for LocalResolution {}

impl LocalResolution {
    fn new(
        owner: OwnerId,
        source_ref: SourceRef,
        source_file: FileId,
        body_source_id: sai_hir::source_map::HirSourceId,
        module_aliases: &HashSet<Symbol>,
    ) -> Self {
        Self {
            owner,
            source_ref,
            source_file,
            paths: HashMap::new(),
            bindings: HashMap::new(),
            diagnostics: Vec::new(),
            deferred_paths: Vec::new(),
            body_source_id,
            module_aliases: module_aliases.clone(),
        }
    }

    pub fn matches_source(
        &self,
        source_map: &HirSourceMap,
        snapshot: &WorkspaceSnapshot,
        module_aliases: &HashSet<Symbol>,
    ) -> bool {
        self.owner == source_map.owner()
            && self.source_file == source_map.file()
            && self.body_source_id == source_map.source_id()
            && self.source_ref == *source_map.source_ref()
            && source_map.matches_snapshot_file(snapshot)
            && self.module_aliases == *module_aliases
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalResolutionError {
    InvalidSourceSpan,
    MismatchedSourceFile,
    MismatchedSourceSnapshot,
    MismatchedOwner,
    MismatchedBodySource,
    MissingSourceFile,
    ForeignLocalSymbol,
    TooManyPathSegments,
}

#[derive(Clone, Copy)]
struct LocalBindingEntry {
    binding: BindingId,
    name: sai_hir::types::Name,
}

#[derive(Default)]
struct LocalRib {
    bindings: HashMap<Symbol, LocalBindingEntry>,
}

enum PatternBindingContext {
    Let { direct: bool },
    VarStatement,
    Pattern,
}

enum LocalWork<'hir> {
    Expr(&'hir Expr<'hir>),
    Block(&'hir Block<'hir>),
    Stmt(&'hir Stmt<'hir>),
    Arm(&'hir Arm<'hir>),
    BindPattern {
        pattern: &'hir Pat<'hir>,
        context: PatternBindingContext,
    },
    ExitScope,
}

/// Resolves local heads of plain value paths in one owner's body.
///
/// The walk covers the executable body, contracts, and invariant predicates.
/// A local binding may resolve the first segment of a qualified path so later
/// member lookup can continue from that value. Unbound one-segment paths are
/// deferred to generic, module, and prelude lookup; qualified module heads
/// remain available to their corresponding later pass.
pub fn resolve_owner_locals(
    body: &Body<'_>,
    source_map: &HirSourceMap,
    snapshot: &WorkspaceSnapshot,
    symbols: &Interner,
    module_aliases: &HashSet<Symbol>,
) -> Result<LocalResolution, LocalResolutionError> {
    if source_map.owner() != body.owner {
        return Err(LocalResolutionError::MismatchedOwner);
    }
    if !source_map.matches_body(body) {
        return Err(LocalResolutionError::MismatchedBodySource);
    }
    let snapshot_file = snapshot
        .file(source_map.file())
        .ok_or(LocalResolutionError::MissingSourceFile)?;
    if !source_map.matches_snapshot_file(snapshot) {
        return Err(LocalResolutionError::MismatchedSourceSnapshot);
    }
    let source = snapshot_file.source().as_ref();
    let source_ref = source_map.source_ref();
    for alias in module_aliases {
        validate_local_symbol(*alias, symbols)?;
    }
    let mut result = LocalResolution::new(
        body.owner,
        source_ref.clone(),
        source.id(),
        body.source_id,
        module_aliases,
    );
    let mut ribs = vec![LocalRib::default()];
    for param in body.params {
        let kind = if param.mutable {
            BindingKind::VarParam
        } else {
            BindingKind::Param
        };
        register_optional_binding(
            param.binding,
            param.name,
            kind,
            &mut ribs,
            &mut result,
            source_ref,
            source,
            symbols,
            module_aliases,
            true,
        )?;
    }

    let mut body_roots = body
        .invariant_preds
        .iter()
        .copied()
        .chain(body.contracts.iter().map(|contract| contract.expr))
        .chain([body.value])
        .collect::<Vec<_>>();
    body_roots.sort_by_key(|expr| expr.span.range.start());
    let mut work = body_roots
        .into_iter()
        .rev()
        .map(LocalWork::Expr)
        .collect::<Vec<_>>();

    while let Some(task) = work.pop() {
        match task {
            LocalWork::Expr(expr) => match expr.kind {
                ExprKind::Lit(_)
                | ExprKind::ImplicitMember(_)
                | ExprKind::Continue { .. }
                | ExprKind::ContractResult
                | ExprKind::Err => {}
                ExprKind::Break { value, .. } => {
                    if let Some(value) = value {
                        work.push(LocalWork::Expr(value));
                    }
                }
                ExprKind::Path(path) => {
                    if path.hir_id.owner != body.owner {
                        return Err(LocalResolutionError::MismatchedOwner);
                    }
                    if path.root == PathRoot::Plain && !path.segments.is_empty() {
                        let name = path.segments[0].name;
                        let segment_count = u32::try_from(path.segments.len())
                            .map_err(|_| LocalResolutionError::TooManyPathSegments)?;
                        if name.sym.is_error() {
                            result.paths.insert(
                                path.hir_id,
                                PathRes {
                                    base: Res::Err,
                                    resolved_segments: 1,
                                    segment_count,
                                },
                            );
                        } else {
                            validate_local_symbol(name.sym, symbols)?;
                            if let Some(binding) = lookup_local_binding(&ribs, name.sym) {
                                result.paths.insert(
                                    path.hir_id,
                                    PathRes {
                                        base: Res::Local(binding),
                                        resolved_segments: 1,
                                        segment_count,
                                    },
                                );
                            } else if path.segments.len() == 1 {
                                result.deferred_paths.push(DeferredPath {
                                    hir_id: path.hir_id,
                                    name: name.sym,
                                    span: name.span,
                                });
                            }
                        }
                    }
                }
                ExprKind::Field { base, .. }
                | ExprKind::TupleField { base, .. }
                | ExprKind::Unary(_, base)
                | ExprKind::Borrow(_, base)
                | ExprKind::Try(base)
                | ExprKind::Await(base) => work.push(LocalWork::Expr(base)),
                ExprKind::Call { callee, args, base } => {
                    if let Some(base) = base {
                        work.push(LocalWork::Expr(base));
                    }
                    for arg in args.iter().rev() {
                        work.push(LocalWork::Expr(arg.expr));
                    }
                    work.push(LocalWork::Expr(callee));
                }
                ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                    work.push(LocalWork::Expr(right));
                    work.push(LocalWork::Expr(left));
                }
                ExprKind::Range { start, end, .. } => {
                    if let Some(end) = end {
                        work.push(LocalWork::Expr(end));
                    }
                    if let Some(start) = start {
                        work.push(LocalWork::Expr(start));
                    }
                }
                ExprKind::Tuple(items) | ExprKind::Array(items) => {
                    for item in items.iter().rev() {
                        work.push(LocalWork::Expr(item));
                    }
                }
                ExprKind::Block(block) => work.push(LocalWork::Block(block)),
                ExprKind::AsyncBlock { block, .. } => work.push(LocalWork::Block(block)),
                ExprKind::If { cond, then, els } => {
                    if let Some(els) = els {
                        work.push(LocalWork::Expr(els));
                    }
                    work.push(LocalWork::Block(then));
                    work.push(LocalWork::Expr(cond));
                }
                ExprKind::Match {
                    scrutinee, arms, ..
                } => {
                    for arm in arms.iter().rev() {
                        work.push(LocalWork::Arm(arm));
                    }
                    work.push(LocalWork::Expr(scrutinee));
                }
                ExprKind::Loop { body, .. } => work.push(LocalWork::Block(body)),
                ExprKind::Return(value) => {
                    if let Some(value) = value {
                        work.push(LocalWork::Expr(value));
                    }
                }
                ExprKind::Closure(closure) => {
                    ribs.push(LocalRib::default());
                    for param in closure.params {
                        register_closure_param(
                            param,
                            &mut ribs,
                            &mut result,
                            source_ref,
                            source,
                            symbols,
                            module_aliases,
                        )?;
                    }
                    work.push(LocalWork::ExitScope);
                    work.push(LocalWork::Block(closure.body));
                }
                ExprKind::Region {
                    name,
                    binding,
                    block,
                } => {
                    ribs.push(LocalRib::default());
                    register_optional_binding(
                        binding,
                        Some(name),
                        BindingKind::Synthetic,
                        &mut ribs,
                        &mut result,
                        source_ref,
                        source,
                        symbols,
                        module_aliases,
                        false,
                    )?;
                    work.push(LocalWork::ExitScope);
                    work.push(LocalWork::Block(block));
                }
                ExprKind::FormatString(pieces) => {
                    for piece in pieces.iter().rev() {
                        if let FmtPiece::Arg { expr, .. } = piece {
                            work.push(LocalWork::Expr(expr));
                        }
                    }
                }
                ExprKind::ContractOld { args, base } => {
                    if let Some(base) = base {
                        work.push(LocalWork::Expr(base));
                    }
                    for arg in args.iter().rev() {
                        work.push(LocalWork::Expr(arg.expr));
                    }
                }
                ExprKind::AsCast { operand, .. } => work.push(LocalWork::Expr(operand)),
            },
            LocalWork::Block(block) => {
                ribs.push(LocalRib::default());
                work.push(LocalWork::ExitScope);
                if let Some(tail) = block.tail {
                    work.push(LocalWork::Expr(tail));
                }
                for stmt in block.stmts.iter().rev() {
                    work.push(LocalWork::Stmt(stmt));
                }
            }
            LocalWork::Stmt(stmt) => match stmt.kind {
                StmtKind::Let { pat, init, els, .. } => {
                    let context = if stmt.span.ctxt == SpanCtxt::Desugared(DesugaringKind::VarStmt)
                    {
                        PatternBindingContext::VarStatement
                    } else {
                        PatternBindingContext::Let {
                            direct: matches!(
                                pat.kind,
                                PatKind::Binding {
                                    mode: BindingMode::Value,
                                    sub: None,
                                    ..
                                }
                            ),
                        }
                    };
                    work.push(LocalWork::BindPattern {
                        pattern: pat,
                        context,
                    });
                    if let Some(els) = els {
                        work.push(LocalWork::Block(els));
                    }
                    work.push(LocalWork::Expr(init));
                }
                StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
                    work.push(LocalWork::Expr(rhs));
                    work.push(LocalWork::Expr(lhs));
                }
                StmtKind::Assert { cond, msg, .. } => {
                    if let Some(msg) = msg {
                        work.push(LocalWork::Expr(msg));
                    }
                    work.push(LocalWork::Expr(cond));
                }
                StmtKind::Expr(expr) => work.push(LocalWork::Expr(expr)),
            },
            LocalWork::Arm(arm) => {
                ribs.push(LocalRib::default());
                register_pattern_bindings(
                    arm.pat,
                    PatternBindingContext::Pattern,
                    &mut ribs,
                    &mut result,
                    source_ref,
                    source,
                    symbols,
                    module_aliases,
                    false,
                )?;
                work.push(LocalWork::ExitScope);
                work.push(LocalWork::Expr(arm.body));
                if let Some(guard) = arm.guard {
                    work.push(LocalWork::Expr(guard));
                }
            }
            LocalWork::BindPattern { pattern, context } => {
                register_pattern_bindings(
                    pattern,
                    context,
                    &mut ribs,
                    &mut result,
                    source_ref,
                    source,
                    symbols,
                    module_aliases,
                    true,
                )?;
            }
            LocalWork::ExitScope => {
                if ribs.len() <= 1 {
                    return Err(LocalResolutionError::MismatchedOwner);
                }
                ribs.pop();
            }
        }
    }

    sort_owner_diagnostics(&mut result.diagnostics);
    Ok(result)
}

/// Module-scope resolutions for candidates not claimed by lexical locals.
///
/// Names present in `generic_parameters` remain deferred so module scope
/// cannot capture an in-scope generic. Misses also remain deferred for
/// prelude lookup and final unresolved-name diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleResolution {
    pub owner: OwnerId,
    pub paths: HashMap<HirId, PathRes>,
    pub deferred_paths: Vec<DeferredPath>,
}

/// Resolves deferred one-segment expression paths through the current module
/// scope after lexical locals and generic parameters.
///
/// `Res::TestOnly` is retained as an explicit unresolved binding marker for
/// the later owner-aware pass, which reports its build-context diagnostic.
/// Module-scope misses stay deferred; this partial pass does not perform
/// prelude lookup or emit unresolved-name diagnostics.
pub fn resolve_owner_module_paths(
    locals: &LocalResolution,
    module_scope: &Phase1ModuleScopes,
    generic_parameters: &HashSet<Symbol>,
) -> ModuleResolution {
    let mut resolution = ModuleResolution {
        owner: locals.owner,
        paths: HashMap::new(),
        deferred_paths: Vec::new(),
    };
    for candidate in &locals.deferred_paths {
        if generic_parameters.contains(&candidate.name) {
            resolution.deferred_paths.push(*candidate);
            continue;
        }
        let Some(binding) = module_scope.scope.get(&candidate.name) else {
            resolution.deferred_paths.push(*candidate);
            continue;
        };
        if matches!(binding.res, Res::Def(_, _) | Res::Err | Res::TestOnly(_)) {
            resolution.paths.insert(
                candidate.hir_id,
                PathRes {
                    base: binding.res,
                    resolved_segments: 1,
                    segment_count: 1,
                },
            );
        } else {
            resolution.deferred_paths.push(*candidate);
        }
    }
    resolution
}

fn lookup_local_binding(ribs: &[LocalRib], symbol: Symbol) -> Option<BindingId> {
    ribs.iter()
        .rev()
        .find_map(|rib| rib.bindings.get(&symbol).map(|entry| entry.binding))
}

fn register_closure_param(
    param: &ClosureParam<'_>,
    ribs: &mut [LocalRib],
    result: &mut LocalResolution,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
    module_aliases: &HashSet<Symbol>,
) -> Result<(), LocalResolutionError> {
    register_optional_binding(
        param.binding,
        param.name,
        BindingKind::ClosureParam { var: param.mutable },
        ribs,
        result,
        source_ref,
        source,
        symbols,
        module_aliases,
        true,
    )
}

fn register_optional_binding(
    binding: BindingId,
    name: Option<sai_hir::types::Name>,
    kind: BindingKind,
    ribs: &mut [LocalRib],
    result: &mut LocalResolution,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
    module_aliases: &HashSet<Symbol>,
    diagnose_duplicate: bool,
) -> Result<(), LocalResolutionError> {
    if let Some(name) = name {
        validate_local_symbol(name.sym, symbols)?;
    }
    result.bindings.entry(binding).or_insert(LocalBinding {
        name: name.map(|name| name.sym),
        span: name.map(|name| name.span),
        kind,
    });
    if let Some(name) = name.filter(|name| !name.sym.is_error()) {
        register_named_binding(
            binding,
            name,
            ribs,
            result,
            source_ref,
            source,
            symbols,
            module_aliases,
            diagnose_duplicate,
        )?;
    }
    Ok(())
}

fn register_pattern_bindings(
    pattern: &Pat<'_>,
    context: PatternBindingContext,
    ribs: &mut [LocalRib],
    result: &mut LocalResolution,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
    module_aliases: &HashSet<Symbol>,
    diagnose_duplicate: bool,
) -> Result<(), LocalResolutionError> {
    let mut patterns = vec![pattern];
    let mut pattern_symbols = HashSet::new();
    while let Some(pattern) = patterns.pop() {
        match pattern.kind {
            PatKind::Binding {
                mode,
                name,
                binding,
                sub,
            } => {
                validate_local_symbol(name.sym, symbols)?;
                let kind = if pattern.span.ctxt == SpanCtxt::Desugared(DesugaringKind::VarStmt) {
                    BindingKind::Var
                } else if pattern.span.ctxt != SpanCtxt::Root {
                    BindingKind::Synthetic
                } else {
                    match context {
                        PatternBindingContext::Let { direct: true } => BindingKind::Let,
                        PatternBindingContext::VarStatement => BindingKind::Var,
                        PatternBindingContext::Let { direct: false }
                        | PatternBindingContext::Pattern => BindingKind::Pattern {
                            var: mode == BindingMode::Var,
                        },
                    }
                };
                result.bindings.entry(binding).or_insert(LocalBinding {
                    name: Some(name.sym),
                    span: Some(name.span),
                    kind,
                });
                if !name.sym.is_error() && pattern_symbols.insert(name.sym) {
                    register_named_binding(
                        binding,
                        name,
                        ribs,
                        result,
                        source_ref,
                        source,
                        symbols,
                        module_aliases,
                        diagnose_duplicate,
                    )?;
                }
                if let Some(sub) = sub {
                    patterns.push(sub);
                }
            }
            PatKind::Tuple(items) | PatKind::Or(items) => {
                patterns.extend(items.iter().rev());
            }
            PatKind::Slice {
                before,
                rest,
                after,
            } => {
                patterns.extend(after.iter().rev());
                if let Some(Some(rest)) = rest {
                    patterns.push(rest);
                }
                patterns.extend(before.iter().rev());
            }
            PatKind::Ctor { args, .. } | PatKind::ImplicitMember { args, .. } => {
                if let Some(args) = args {
                    for arg in args.iter().rev() {
                        patterns.push(&arg.pat);
                    }
                }
            }
            PatKind::Wild | PatKind::Lit(_) | PatKind::Range { .. } | PatKind::Err => {}
        }
    }
    Ok(())
}

fn register_named_binding(
    binding: BindingId,
    name: sai_hir::types::Name,
    ribs: &mut [LocalRib],
    result: &mut LocalResolution,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
    module_aliases: &HashSet<Symbol>,
    diagnose_duplicate: bool,
) -> Result<(), LocalResolutionError> {
    if module_aliases.contains(&name.sym) {
        let spelling = symbols
            .resolve(name.sym)
            .map_err(|_| LocalResolutionError::ForeignLocalSymbol)?;
        let primary = local_source_location(name.span, source_ref, source)?;
        let mut diagnostic = Diagnostic::new(
            Code::ShadowsModule,
            Severity::Error,
            format!("`{spelling}` shadows a module or interface binding"),
            Some(primary),
        );
        diagnostic.primary_label = "shadows module binding".to_owned();
        result.diagnostics.push(diagnostic);
    }

    let rib = ribs
        .last_mut()
        .ok_or(LocalResolutionError::MismatchedOwner)?;
    if let Some(previous) = rib.bindings.get(&name.sym).copied() {
        if diagnose_duplicate && previous.binding != binding {
            let spelling = symbols
                .resolve(name.sym)
                .map_err(|_| LocalResolutionError::ForeignLocalSymbol)?;
            let primary = local_source_location(name.span, source_ref, source)?;
            let earlier = local_source_location(previous.name.span, source_ref, source)?;
            let mut diagnostic = Diagnostic::new(
                Code::DuplicateBinding,
                Severity::Error,
                format!("`{spelling}` is already bound in this scope"),
                Some(primary),
            );
            diagnostic.primary_label = "duplicate binding".to_owned();
            diagnostic
                .push_related(Related::DeclaredHere {
                    location: earlier,
                    what: "earlier binding".to_owned(),
                })
                .expect("unbound diagnostics accept related locations");
            result.diagnostics.push(diagnostic);
        }
    } else {
        rib.bindings
            .insert(name.sym, LocalBindingEntry { binding, name });
    }
    Ok(())
}

fn local_source_location(
    span: HirSpan,
    source_ref: &SourceRef,
    source: &SourceFile,
) -> Result<Location, LocalResolutionError> {
    if span.file != source.id() {
        return Err(LocalResolutionError::MismatchedSourceFile);
    }
    Location::from_decoded_source(
        source_ref.clone(),
        DiagnosticSpan::new(span.file, span.range),
        source,
    )
    .ok_or(LocalResolutionError::InvalidSourceSpan)
}

fn validate_local_symbol(symbol: Symbol, symbols: &Interner) -> Result<(), LocalResolutionError> {
    if !symbol.is_error() && symbol.session_id() != symbols.session_id() {
        return Err(LocalResolutionError::ForeignLocalSymbol);
    }
    Ok(())
}

/// An unused label discovered by loop resolution.
///
/// The later lint adapter owns lint-level application and the machine-applicable
/// source fix. Keeping the HIR span here avoids treating decoded offsets as
/// original-file offsets before a syntax fix planner is available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnusedLabel {
    pub loop_id: HirId,
    pub span: HirSpan,
}

/// The owner-local label resolutions and hard diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LabelResolution {
    /// Break/continue expression IDs mapped to their target loop, or `Err`
    /// when the jump has already received a diagnostic.
    pub labels: HashMap<HirId, Result<HirId, ()>>,
    pub diagnostics: Vec<Diagnostic>,
    /// Lint candidates whose diagnostic and source fix are attached by a later
    /// snapshot-aware stage.
    pub unused_labels: Vec<UnusedLabel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelResolutionError {
    InvalidSourceSpan,
    MismatchedSourceFile,
    ForeignLabelSymbol,
}

#[derive(Clone, Copy)]
struct LoopFrame {
    hir_id: HirId,
    label: Option<Label>,
    named_jump_used: bool,
}

enum Work<'hir> {
    Expr(&'hir Expr<'hir>),
    Block(&'hir Block<'hir>),
    Stmt(&'hir Stmt<'hir>),
    Pat(&'hir Pat<'hir>),
    Ty(&'hir Ty<'hir>),
    ExitLoop(HirId),
    ExitBoundary(Vec<LoopFrame>),
}

enum ContractWork<'hir> {
    Expr(&'hir Expr<'hir>),
    Block(&'hir Block<'hir>),
    Stmt(&'hir Stmt<'hir>),
}

/// Resolve labels and predicate restrictions for one HIR owner body.
///
/// Contract violations are diagnosed as `INVALID_CONTRACT_EXPRESSION`.
/// Contract jumps are deliberately excluded from the loop-label map and
/// ordinary outside-loop diagnostics. Closure and async-block bodies are
/// separate owner bodies and are left to their own resolver pass.
pub fn resolve_owner_control_flow(
    body: &Body<'_>,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
) -> Result<LabelResolution, LabelResolutionError> {
    let mut result = resolve_loop_labels(body.value, source_ref, source, symbols)?;
    for contract in body.contracts {
        resolve_predicate_restrictions(contract.expr, source_ref, source, &mut result.diagnostics)?;
    }
    for predicate in body.invariant_preds {
        resolve_predicate_restrictions(predicate, source_ref, source, &mut result.diagnostics)?;
    }
    sort_owner_diagnostics(&mut result.diagnostics);
    Ok(result)
}

/// Reports prohibited source constructs in one owner predicate expression.
///
/// This scans the predicate without treating its jumps as ordinary body
/// control flow, so `return`, `break`, and `continue` receive
/// `INVALID_CONTRACT_EXPRESSION` and do not produce label or
/// outside-function diagnostics. Closure and async-block bodies remain
/// separate owners.
pub fn resolve_predicate_expression(
    predicate: &Expr<'_>,
    source_ref: &SourceRef,
    source: &SourceFile,
) -> Result<Vec<Diagnostic>, LabelResolutionError> {
    let mut diagnostics = Vec::new();
    resolve_predicate_restrictions(predicate, source_ref, source, &mut diagnostics)?;
    sort_owner_diagnostics(&mut diagnostics);
    Ok(diagnostics)
}

fn sort_owner_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start().raw())
            .unwrap_or(u32::MAX)
    });
}

fn resolve_predicate_restrictions(
    root: &Expr<'_>,
    source_ref: &SourceRef,
    source: &SourceFile,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), LabelResolutionError> {
    let mut work = vec![ContractWork::Expr(root)];
    while let Some(task) = work.pop() {
        match task {
            ContractWork::Expr(expr) => match expr.kind {
                ExprKind::Break { value, .. } => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`break`",
                            "control transfer in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    if let Some(value) = value {
                        work.push(ContractWork::Expr(value));
                    }
                }
                ExprKind::Continue { .. } => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`continue`",
                            "control transfer in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                }
                ExprKind::Return(value) => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`return`",
                            "control transfer in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    if let Some(value) = value {
                        work.push(ContractWork::Expr(value));
                    }
                }
                ExprKind::Lit(_)
                | ExprKind::Path(_)
                | ExprKind::ImplicitMember(_)
                | ExprKind::ContractResult
                | ExprKind::AsyncBlock { .. }
                | ExprKind::Err => {}
                ExprKind::Closure(_) => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "closure",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                }
                ExprKind::Field { base, .. }
                | ExprKind::TupleField { base, .. }
                | ExprKind::Unary(_, base)
                | ExprKind::Borrow(_, base) => work.push(ContractWork::Expr(base)),
                ExprKind::Try(base) => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`?`",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Expr(base));
                }
                ExprKind::Await(base) => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`.await`",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Expr(base));
                }
                ExprKind::Call { callee, args, base } => {
                    if let Some(base) = base {
                        work.push(ContractWork::Expr(base));
                    }
                    for arg in args.iter().rev() {
                        work.push(ContractWork::Expr(arg.expr));
                    }
                    work.push(ContractWork::Expr(callee));
                }
                ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                    work.push(ContractWork::Expr(right));
                    work.push(ContractWork::Expr(left));
                }
                ExprKind::Range { start, end, .. } => {
                    if let Some(end) = end {
                        work.push(ContractWork::Expr(end));
                    }
                    if let Some(start) = start {
                        work.push(ContractWork::Expr(start));
                    }
                }
                ExprKind::Tuple(items) | ExprKind::Array(items) => {
                    for item in items.iter().rev() {
                        work.push(ContractWork::Expr(item));
                    }
                }
                ExprKind::Block(block) => {
                    if block.safety == Safety::Unsafe && expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`unsafe` block",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Block(block));
                }
                ExprKind::Region { block, .. } => {
                    if expr.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            expr.span,
                            "`region` block",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Block(block));
                }
                ExprKind::If { cond, then, els } => {
                    if let Some(els) = els {
                        work.push(ContractWork::Expr(els));
                    }
                    work.push(ContractWork::Block(then));
                    work.push(ContractWork::Expr(cond));
                }
                ExprKind::Match {
                    scrutinee, arms, ..
                } => {
                    for arm in arms.iter().rev() {
                        work.push(ContractWork::Expr(arm.body));
                        if let Some(guard) = arm.guard {
                            work.push(ContractWork::Expr(guard));
                        }
                    }
                    work.push(ContractWork::Expr(scrutinee));
                }
                ExprKind::Loop { body, .. } => work.push(ContractWork::Block(body)),
                ExprKind::FormatString(pieces) => {
                    for piece in pieces.iter().rev() {
                        if let FmtPiece::Arg { expr, .. } = piece {
                            work.push(ContractWork::Expr(expr));
                        }
                    }
                }
                ExprKind::ContractOld { args, base } => {
                    if let Some(base) = base {
                        work.push(ContractWork::Expr(base));
                    }
                    for arg in args.iter().rev() {
                        work.push(ContractWork::Expr(arg.expr));
                    }
                }
                ExprKind::AsCast { operand, .. } => {
                    work.push(ContractWork::Expr(operand));
                }
            },
            ContractWork::Block(block) => {
                if let Some(tail) = block.tail {
                    work.push(ContractWork::Expr(tail));
                }
                for stmt in block.stmts.iter().rev() {
                    work.push(ContractWork::Stmt(stmt));
                }
            }
            ContractWork::Stmt(stmt) => match stmt.kind {
                StmtKind::Let { pat, init, els, .. } => {
                    if pat.span.ctxt == SpanCtxt::Desugared(DesugaringKind::VarStmt) {
                        diagnostics.push(invalid_contract_expression(
                            pat.span,
                            "`var` statement",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    if let Some(els) = els {
                        work.push(ContractWork::Block(els));
                    }
                    work.push(ContractWork::Expr(init));
                }
                StmtKind::Assign { lhs, rhs } => {
                    if stmt.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            stmt.span,
                            "assignment",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Expr(rhs));
                    work.push(ContractWork::Expr(lhs));
                }
                StmtKind::AssignOp { lhs, rhs, .. } => {
                    if stmt.span.ctxt == SpanCtxt::Root {
                        diagnostics.push(invalid_contract_expression(
                            stmt.span,
                            "compound assignment",
                            "forbidden construct in predicate",
                            source_ref,
                            source,
                        )?);
                    }
                    work.push(ContractWork::Expr(rhs));
                    work.push(ContractWork::Expr(lhs));
                }
                StmtKind::Assert { cond, msg, .. } => {
                    if let Some(msg) = msg {
                        work.push(ContractWork::Expr(msg));
                    }
                    work.push(ContractWork::Expr(cond));
                }
                StmtKind::Expr(expr) => work.push(ContractWork::Expr(expr)),
            },
        }
    }
    Ok(())
}

fn invalid_contract_expression(
    span: HirSpan,
    construct: &str,
    primary_label: &str,
    source_ref: &SourceRef,
    source: &SourceFile,
) -> Result<Diagnostic, LabelResolutionError> {
    let primary = source_location(span, source_ref, source)?;
    let mut diagnostic = Diagnostic::new(
        Code::InvalidContractExpression,
        Severity::Error,
        format!("{construct} is not allowed in a predicate expression"),
        Some(primary),
    );
    diagnostic.primary_label = primary_label.to_owned();
    Ok(diagnostic)
}

/// Resolve loop and jump labels in one owner body expression.
///
/// `source_ref` identifies the source file for diagnostics; HIR spans use
/// decoded-text offsets and are converted to original-file offsets here.
/// Predicate expressions are supplied separately by
/// `resolve_owner_control_flow` because their forbidden control-flow and
/// syntax constructs are `INVALID_CONTRACT_EXPRESSION`, not loop-target
/// errors.
pub fn resolve_loop_labels(
    root: &Expr<'_>,
    source_ref: &SourceRef,
    source: &SourceFile,
    symbols: &Interner,
) -> Result<LabelResolution, LabelResolutionError> {
    let mut result = LabelResolution::default();
    let mut work = vec![Work::Expr(root)];
    let mut loops = Vec::<LoopFrame>::new();
    let mut boundaries = Vec::<HirSpan>::new();
    let mut error_ancestors = Vec::<HirId>::new();
    let mut error_loops = HashSet::<HirId>::new();

    while let Some(task) = work.pop() {
        match task {
            Work::Expr(expr) => match expr.kind {
                ExprKind::Loop { body, label, .. } => {
                    if let Some(label) = label.filter(|label| !label.name.is_error()) {
                        if let Some(previous) = loops
                            .iter()
                            .rev()
                            .filter_map(|frame| frame.label)
                            .find(|previous| previous.name == label.name)
                        {
                            let primary = source_location(label.span, source_ref, source)?;
                            let previous = source_location(previous.span, source_ref, source)?;
                            let mut diagnostic = Diagnostic::new(
                                Code::LabelShadowed,
                                Severity::Error,
                                "loop label shadows an enclosing loop label",
                                Some(primary),
                            );
                            diagnostic.primary_label = "shadowed label".to_owned();
                            diagnostic
                                .push_related(Related::DeclaredHere {
                                    location: previous,
                                    what: "enclosing loop label".to_owned(),
                                })
                                .expect("unbound diagnostics accept related locations");
                            result.diagnostics.push(diagnostic);
                        }
                    }

                    loops.push(LoopFrame {
                        hir_id: expr.hir_id,
                        label,
                        named_jump_used: false,
                    });
                    error_ancestors.push(expr.hir_id);
                    work.push(Work::ExitLoop(expr.hir_id));
                    work.push(Work::Block(body));
                }
                ExprKind::Break { label, value } => {
                    resolve_jump(
                        expr.hir_id,
                        expr.span,
                        label,
                        true,
                        &mut loops,
                        &boundaries,
                        source_ref,
                        source,
                        &mut result,
                        symbols,
                    )?;
                    if let Some(value) = value {
                        work.push(Work::Expr(value));
                    }
                }
                ExprKind::Continue { label } => resolve_jump(
                    expr.hir_id,
                    expr.span,
                    label,
                    false,
                    &mut loops,
                    &boundaries,
                    source_ref,
                    source,
                    &mut result,
                    symbols,
                )?,
                ExprKind::Err => {
                    for loop_id in &error_ancestors {
                        error_loops.insert(*loop_id);
                    }
                }
                ExprKind::Lit(_) | ExprKind::ImplicitMember(_) | ExprKind::ContractResult => {}
                ExprKind::Path(path) => {
                    push_path_args(&mut work, path);
                }
                ExprKind::Field { base, args, .. } => {
                    if let Some(args) = args {
                        push_generic_args(&mut work, args);
                    }
                    work.push(Work::Expr(base));
                }
                ExprKind::TupleField { base, .. }
                | ExprKind::Unary(_, base)
                | ExprKind::Borrow(_, base)
                | ExprKind::Try(base)
                | ExprKind::Await(base) => work.push(Work::Expr(base)),
                ExprKind::Call { callee, args, base } => {
                    if let Some(base) = base {
                        work.push(Work::Expr(base));
                    }
                    push_args(&mut work, args);
                    work.push(Work::Expr(callee));
                }
                ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                    work.push(Work::Expr(right));
                    work.push(Work::Expr(left));
                }
                ExprKind::Range { start, end, .. } => {
                    if let Some(end) = end {
                        work.push(Work::Expr(end));
                    }
                    if let Some(start) = start {
                        work.push(Work::Expr(start));
                    }
                }
                ExprKind::Tuple(items) | ExprKind::Array(items) => {
                    push_exprs(&mut work, items);
                }
                ExprKind::Block(block) => work.push(Work::Block(block)),
                ExprKind::AsyncBlock { block, .. } => {
                    let previous = std::mem::take(&mut loops);
                    boundaries.push(expr.span);
                    work.push(Work::ExitBoundary(previous));
                    work.push(Work::Block(block));
                }
                ExprKind::If { cond, then, els } => {
                    if let Some(els) = els {
                        work.push(Work::Expr(els));
                    }
                    work.push(Work::Block(then));
                    work.push(Work::Expr(cond));
                }
                ExprKind::Match {
                    scrutinee, arms, ..
                } => {
                    for arm in arms.iter().rev() {
                        work.push(Work::Expr(&arm.body));
                        if let Some(guard) = arm.guard {
                            work.push(Work::Expr(guard));
                        }
                        work.push(Work::Pat(arm.pat));
                    }
                    work.push(Work::Expr(scrutinee));
                }
                ExprKind::Return(value) => {
                    if let Some(value) = value {
                        work.push(Work::Expr(value));
                    }
                }
                ExprKind::Closure(closure) => {
                    let previous = std::mem::take(&mut loops);
                    boundaries.push(expr.span);
                    work.push(Work::ExitBoundary(previous));
                    work.push(Work::Block(closure.body));
                    if let Some(ret) = closure.ret {
                        work.push(Work::Ty(ret));
                    }
                    for param in closure.params.iter().rev() {
                        if let Some(ty) = param.ty {
                            work.push(Work::Ty(ty));
                        }
                    }
                }
                ExprKind::Region { block, .. } => work.push(Work::Block(block)),
                ExprKind::FormatString(pieces) => {
                    for piece in pieces.iter().rev() {
                        if let FmtPiece::Arg { expr, .. } = piece {
                            work.push(Work::Expr(expr));
                        }
                    }
                }
                ExprKind::ContractOld { args, base } => {
                    if let Some(base) = base {
                        work.push(Work::Expr(base));
                    }
                    push_args(&mut work, args);
                }
                ExprKind::AsCast { operand, ty } => {
                    work.push(Work::Ty(ty));
                    work.push(Work::Expr(operand));
                }
            },
            Work::Block(block) => {
                if let Some(tail) = block.tail {
                    work.push(Work::Expr(tail));
                }
                for stmt in block.stmts.iter().rev() {
                    work.push(Work::Stmt(stmt));
                }
            }
            Work::Stmt(stmt) => match stmt.kind {
                StmtKind::Let { pat, ty, init, els } => {
                    if let Some(els) = els {
                        work.push(Work::Block(els));
                    }
                    work.push(Work::Expr(init));
                    if let Some(ty) = ty {
                        work.push(Work::Ty(ty));
                    }
                    work.push(Work::Pat(pat));
                }
                StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
                    work.push(Work::Expr(rhs));
                    work.push(Work::Expr(lhs));
                }
                StmtKind::Assert { cond, msg, .. } => {
                    if let Some(msg) = msg {
                        work.push(Work::Expr(msg));
                    }
                    work.push(Work::Expr(cond));
                }
                StmtKind::Expr(expr) => work.push(Work::Expr(expr)),
            },
            Work::Pat(pat) => match pat.kind {
                PatKind::Err => {
                    for loop_id in &error_ancestors {
                        error_loops.insert(*loop_id);
                    }
                }
                PatKind::Wild | PatKind::Lit(_) => {}
                PatKind::ImplicitMember { args, .. } => {
                    if let Some(args) = args {
                        for arg in args.iter().rev() {
                            work.push(Work::Pat(arg.pat));
                        }
                    }
                }
                PatKind::Binding { sub, .. } => {
                    if let Some(sub) = sub {
                        work.push(Work::Pat(sub));
                    }
                }
                PatKind::Range { start, end, .. } => {
                    if let Some(PatRangeEnd::Path(path)) = end {
                        for segment in path.segments.iter().rev() {
                            if let Some(args) = segment.args {
                                push_generic_args(&mut work, args);
                            }
                        }
                    }
                    if let Some(PatRangeEnd::Path(path)) = start {
                        for segment in path.segments.iter().rev() {
                            if let Some(args) = segment.args {
                                push_generic_args(&mut work, args);
                            }
                        }
                    }
                }
                PatKind::Tuple(pats) | PatKind::Or(pats) => {
                    for pat in pats.iter().rev() {
                        work.push(Work::Pat(pat));
                    }
                }
                PatKind::Slice {
                    before,
                    rest,
                    after,
                } => {
                    for pat in after.iter().rev() {
                        work.push(Work::Pat(pat));
                    }
                    if let Some(Some(rest)) = rest {
                        work.push(Work::Pat(rest));
                    }
                    for pat in before.iter().rev() {
                        work.push(Work::Pat(pat));
                    }
                }
                PatKind::Ctor { path, args, .. } => {
                    if let Some(args) = args {
                        for arg in args.iter().rev() {
                            work.push(Work::Pat(arg.pat));
                        }
                    }
                    for segment in path.segments.iter().rev() {
                        if let Some(args) = segment.args {
                            push_generic_args(&mut work, args);
                        }
                    }
                }
            },
            Work::Ty(ty) => match ty.kind {
                TyKind::Err => {
                    for loop_id in &error_ancestors {
                        error_loops.insert(*loop_id);
                    }
                }
                TyKind::Infer => {}
                TyKind::Borrow(_, ty) => work.push(Work::Ty(ty)),
                TyKind::Tuple(tys) => {
                    for ty in tys.iter().rev() {
                        work.push(Work::Ty(ty));
                    }
                }
                TyKind::FnPtr { params, ret, .. } => {
                    if let Some(ret) = ret {
                        work.push(Work::Ty(ret));
                    }
                    for ty in params.iter().rev() {
                        work.push(Work::Ty(ty));
                    }
                }
                TyKind::Path(path) => {
                    for segment in path.segments.iter().rev() {
                        if let Some(args) = segment.args {
                            push_generic_args(&mut work, args);
                        }
                    }
                }
            },
            Work::ExitLoop(loop_id) => {
                let frame = loops.pop().expect("loop exit has an active loop");
                assert_eq!(frame.hir_id, loop_id, "loop stack remains properly nested");
                assert_eq!(
                    error_ancestors.pop(),
                    Some(loop_id),
                    "error-suppression loop stack remains properly nested"
                );
                let has_error_node = error_loops.remove(&loop_id);
                if let Some(label) = frame.label
                    && !label.name.is_error()
                    && !frame.named_jump_used
                    && !has_error_node
                {
                    result.unused_labels.push(UnusedLabel {
                        loop_id,
                        span: label.span,
                    });
                }
            }
            Work::ExitBoundary(previous) => {
                loops.clear();
                loops = previous;
                boundaries
                    .pop()
                    .expect("boundary exit has an active boundary");
            }
        }
    }

    result.unused_labels.sort_by_key(|unused| {
        (
            unused.span.file.raw(),
            unused.span.range.start().raw(),
            unused.span.range.end().raw(),
        )
    });
    result.diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start().raw())
            .unwrap_or(u32::MAX)
    });
    Ok(result)
}

fn resolve_jump(
    hir_id: HirId,
    expression_span: HirSpan,
    label: Option<Label>,
    is_break: bool,
    loops: &mut [LoopFrame],
    boundaries: &[HirSpan],
    source_ref: &SourceRef,
    source: &SourceFile,
    result: &mut LabelResolution,
    symbols: &Interner,
) -> Result<(), LabelResolutionError> {
    if label.is_some_and(|label| label.name.is_error()) {
        result.labels.insert(hir_id, Err(()));
        return Ok(());
    }

    let target = if let Some(label) = label {
        loops.iter().rposition(|frame| {
            frame
                .label
                .is_some_and(|candidate| candidate.name == label.name)
        })
    } else {
        loops.len().checked_sub(1)
    };

    let Some(target) = target else {
        result.labels.insert(hir_id, Err(()));
        let span = label.map_or(expression_span, |label| label.span);
        let primary = source_location(span, source_ref, source)?;
        let (code, message, label_text) = if label.is_some() {
            (
                Code::UndeclaredLabel,
                "no enclosing loop has the requested label",
                "undeclared label",
            )
        } else if is_break {
            (
                Code::BreakOutsideLoop,
                "`break` appears outside a loop",
                "break outside loop",
            )
        } else {
            (
                Code::ContinueOutsideLoop,
                "`continue` appears outside a loop",
                "continue outside loop",
            )
        };
        let mut diagnostic = Diagnostic::new(code, Severity::Error, message, Some(primary));
        diagnostic.primary_label = label_text.to_owned();
        if let Some(label) = label {
            add_label_suggestions(&mut diagnostic, label.name, loops, symbols)?;
        }
        if let Some(boundary) = boundaries.last() {
            let location = source_location(*boundary, source_ref, source)?;
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "loop labels do not cross a closure or async-block boundary"
                        .to_owned(),
                    location: Some(location),
                })
                .expect("unbound diagnostics accept child locations");
        }
        result.diagnostics.push(diagnostic);
        return Ok(());
    };

    let target_loop = loops[target].hir_id;
    if label.is_some() {
        loops[target].named_jump_used = true;
    }
    result.labels.insert(hir_id, Ok(target_loop));
    Ok(())
}

fn add_label_suggestions(
    diagnostic: &mut Diagnostic,
    missing: sai_hir::Symbol,
    loops: &[LoopFrame],
    symbols: &Interner,
) -> Result<(), LabelResolutionError> {
    let missing = symbols
        .resolve(missing)
        .map_err(|_| LabelResolutionError::ForeignLabelSymbol)?;
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for frame in loops {
        let Some(label) = frame.label else {
            continue;
        };
        if label.name.is_error() {
            continue;
        }
        if !seen.insert(label.name) {
            continue;
        }
        let spelling = symbols
            .resolve(label.name)
            .map_err(|_| LabelResolutionError::ForeignLabelSymbol)?;
        if let Some(distance) = edit_distance_at_most_two(&missing, &spelling) {
            candidates.push((
                distance,
                label.span.range.start().raw(),
                spelling.to_string(),
            ));
        }
    }
    candidates.sort_by(|left, right| {
        (left.0, left.1, left.2.as_bytes()).cmp(&(right.0, right.1, right.2.as_bytes()))
    });
    candidates.truncate(3);
    if !candidates.is_empty() {
        let suggestions = candidates
            .into_iter()
            .map(|(_, _, spelling)| format!("'{spelling}"))
            .collect::<Vec<_>>()
            .join(", ");
        diagnostic
            .push_child(Child {
                kind: ChildKind::Help,
                message: format!("similar labels: {suggestions}"),
                location: None,
            })
            .expect("unbound diagnostics accept help children");
    }
    Ok(())
}

fn edit_distance_at_most_two(left: &str, right: &str) -> Option<usize> {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left.len().abs_diff(right.len()) > 2 {
        return None;
    }

    const TOO_FAR: usize = 3;
    const BAND_RADIUS: usize = 2;
    let mut distances = vec![[TOO_FAR; BAND_RADIUS * 2 + 1]; left.len() + 1];
    for right_count in 0..=right.len().min(BAND_RADIUS) {
        distances[0][BAND_RADIUS + right_count] = right_count;
    }
    for left_count in 1..=left.len().min(BAND_RADIUS) {
        distances[left_count][BAND_RADIUS - left_count] = left_count;
    }

    let mut last_seen = HashMap::<char, usize>::new();
    for (left_index, left_char) in left.iter().enumerate() {
        let left_count = left_index + 1;
        let start = left_count.saturating_sub(BAND_RADIUS).max(1);
        let end = right.len().min(left_count.saturating_add(BAND_RADIUS));
        let mut last_match = 0usize;
        if start <= end {
            for right_count in start..=end {
                let previous_left = last_seen.get(&right[right_count - 1]).copied().unwrap_or(0);
                let previous_right = last_match;
                let substitution_cost = if *left_char == right[right_count - 1] {
                    last_match = right_count;
                    0
                } else {
                    1
                };

                let transposition = if previous_left > 0 && previous_right > 0 {
                    let skipped =
                        (left_count - previous_left - 1) + (right_count - previous_right - 1);
                    bounded_distance_at(&distances, previous_left - 1, previous_right - 1)
                        .saturating_add(skipped)
                        .saturating_add(1)
                        .min(TOO_FAR)
                } else {
                    TOO_FAR
                };
                let distance = (bounded_distance_at(&distances, left_count - 1, right_count) + 1)
                    .min(bounded_distance_at(&distances, left_count, right_count - 1) + 1)
                    .min(
                        bounded_distance_at(&distances, left_count - 1, right_count - 1)
                            + substitution_cost,
                    )
                    .min(transposition)
                    .min(TOO_FAR);
                let band_index = bounded_distance_band_index(left_count, right_count)
                    .expect("computed edit-distance cells stay in the bounded band");
                distances[left_count][band_index] = distance;
            }
        }
        last_seen.insert(*left_char, left_count);
    }
    let distance = bounded_distance_at(&distances, left.len(), right.len());
    (distance <= 2).then_some(distance)
}

fn bounded_distance_at(distances: &[[usize; 5]], left_count: usize, right_count: usize) -> usize {
    let Some(band_index) = bounded_distance_band_index(left_count, right_count) else {
        return 3;
    };
    distances.get(left_count).map_or(3, |row| row[band_index])
}

fn bounded_distance_band_index(left_count: usize, right_count: usize) -> Option<usize> {
    if right_count >= left_count {
        let difference = right_count - left_count;
        (difference <= 2).then_some(difference + 2)
    } else {
        let difference = left_count - right_count;
        if difference <= 2 {
            Some(2 - difference)
        } else {
            None
        }
    }
}

fn source_location(
    span: HirSpan,
    source_ref: &SourceRef,
    source: &SourceFile,
) -> Result<Location, LabelResolutionError> {
    if span.file != source.id() {
        return Err(LabelResolutionError::MismatchedSourceFile);
    }
    Location::from_decoded_source(
        source_ref.clone(),
        DiagnosticSpan::new(span.file, span.range),
        source,
    )
    .ok_or(LabelResolutionError::InvalidSourceSpan)
}

fn push_exprs<'hir>(work: &mut Vec<Work<'hir>>, expressions: &'hir [Expr<'hir>]) {
    for expr in expressions.iter().rev() {
        work.push(Work::Expr(expr));
    }
}

fn push_args<'hir>(work: &mut Vec<Work<'hir>>, args: &'hir [Arg<'hir>]) {
    for arg in args.iter().rev() {
        work.push(Work::Expr(arg.expr));
    }
}

fn push_generic_args<'hir>(work: &mut Vec<Work<'hir>>, args: &'hir GenericArgs<'hir>) {
    for arg in args.args.iter().rev() {
        match arg {
            GenericArg::Type(ty) | GenericArg::Binding { ty, .. } => work.push(Work::Ty(ty)),
            GenericArg::Const(_) => {}
        }
    }
}

fn push_path_args<'hir>(work: &mut Vec<Work<'hir>>, path: sai_hir::types::Path<'hir>) {
    for segment in path.segments.iter().rev() {
        if let Some(args) = segment.args {
            push_generic_args(work, args);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::arena::HirArena;
    use sai_hir::body::{BodyParam, Contract, ContractKind};
    use sai_hir::expressions::{CaptureBy, Closure, LoopSource, MatchSource};
    use sai_hir::patterns::{BindingMode, Pat};
    use sai_hir::source_map::HirSourceMapBuilder;
    use sai_hir::types::{Name, Path, PathRoot, PathSegment};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, OwnerId,
        Span as HirSpan, SpanCtxt,
    };
    use sai_intern::{Idx, Interner, ItemLocalId, PackageInterner};
    use sai_syntax::ast::AstIdMap;
    use sai_vfs::{FileId, SnapshotFile, SourceFile, TextRange, TextSize, WorkspaceSnapshot};
    use std::sync::Arc;

    fn owner_id() -> OwnerId {
        owner_id_with_local().0
    }

    fn owner_id_with_local() -> (OwnerId, sai_hir::LocalDefId) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("paths create");
        let symbol = symbols.intern("test_owner").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("path is valid");
        let local = paths.intern(path).expect("owner path interns");
        (OwnerId { def_id: local }, local)
    }

    fn hir_id(owner: OwnerId, local: u32) -> HirId {
        HirId {
            owner,
            local_id: ItemLocalId::from_raw(local),
        }
    }

    fn hir_span(start: u32, end: u32) -> HirSpan {
        HirSpan {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("span range is valid"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn label(symbol: sai_hir::Symbol, start: u32, end: u32) -> Label {
        Label {
            name: symbol,
            span: hir_span(start, end),
        }
    }

    fn source() -> (SourceRef, SourceFile) {
        let file = FileId::new(1);
        (
            SourceRef {
                path: "src/lib.sai".to_owned(),
                package: Some("acme:app#1.0".to_owned()),
                snapshot: None,
            },
            SourceFile::new(file, " ".repeat(128), false).expect("source creates"),
        )
    }

    fn local_source_context(
        owner: OwnerId,
    ) -> (WorkspaceSnapshot, sai_hir::source_map::HirSourceMap) {
        local_source_context_with(owner, &" ".repeat(128), None)
    }

    fn local_source_context_with(
        owner: OwnerId,
        text: &str,
        document_version: Option<u64>,
    ) -> (WorkspaceSnapshot, sai_hir::source_map::HirSourceMap) {
        let file = FileId::new(1);
        let source = Arc::new(SourceFile::new(file, text, false).expect("source creates"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/lib.sai",
            None,
            document_version,
        )])
        .expect("snapshot creates");
        let parsed = sai_syntax::parse(&snapshot, file).expect("source parses");
        let ast_ids = AstIdMap::new(file, &parsed.root).expect("AST IDs");
        let source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &parsed.root)
                .expect("source map starts")
                .finish();
        (snapshot, source_map)
    }

    fn local_name(symbol: sai_hir::Symbol, start: u32, end: u32) -> Name {
        Name {
            sym: symbol,
            span: hir_span(start, end),
        }
    }

    fn local_path_expr<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        start: u32,
        end: u32,
        symbol: sai_hir::Symbol,
        root: PathRoot,
    ) -> &'hir Expr<'hir> {
        let segment = PathSegment {
            hir_id: hir_id(owner, local + 2),
            name: local_name(symbol, start, end),
            args: None,
        };
        let path = Path {
            hir_id: hir_id(owner, local + 1),
            root,
            segments: arena.alloc_path_segments(&[segment]),
            span: hir_span(start, end),
        };
        expr(arena, owner, local, start, end, ExprKind::Path(path))
    }

    fn local_multi_segment_path_expr<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        first: (sai_hir::Symbol, u32, u32),
        second: (sai_hir::Symbol, u32, u32),
    ) -> &'hir Expr<'hir> {
        let segments = [
            PathSegment {
                hir_id: hir_id(owner, local + 2),
                name: local_name(first.0, first.1, first.2),
                args: None,
            },
            PathSegment {
                hir_id: hir_id(owner, local + 3),
                name: local_name(second.0, second.1, second.2),
                args: None,
            },
        ];
        let path = Path {
            hir_id: hir_id(owner, local + 1),
            root: PathRoot::Plain,
            segments: arena.alloc_path_segments(&segments),
            span: hir_span(first.1, second.2),
        };
        expr(arena, owner, local, first.1, second.2, ExprKind::Path(path))
    }

    fn local_binding_pat<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        name: Name,
        binding: BindingId,
        mode: BindingMode,
    ) -> &'hir Pat<'hir> {
        arena.alloc_pat(Pat {
            hir_id: hir_id(owner, local),
            kind: PatKind::Binding {
                mode,
                name,
                binding,
                sub: None,
            },
            span: name.span,
        })
    }

    fn block_with_stmts<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        stmts: &[Stmt<'hir>],
        tail: Option<&'hir Expr<'hir>>,
    ) -> &'hir Block<'hir> {
        arena.alloc_block(Block {
            hir_id: hir_id(owner, local),
            stmts: arena.alloc_stmts(stmts),
            tail,
            safety: sai_hir::Safety::Safe,
            span: hir_span(0, 120),
        })
    }

    fn body<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        source_id: sai_hir::source_map::HirSourceId,
        params: &'hir [BodyParam],
        value: &'hir Expr<'hir>,
        contracts: &'hir [Contract<'hir>],
    ) -> &'hir Body<'hir> {
        arena.alloc_body(sai_hir::body::Body {
            owner,
            source_id,
            params,
            value,
            contracts,
            invariant_preds: &[],
        })
    }

    #[test]
    fn local_paths_respect_parameter_and_initializer_binding_order() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let x = symbols.intern("x").expect("symbol interns");
        let parameter = BodyParam {
            hir_id: hir_id(owner, 1),
            binding: BindingId::from_raw(0),
            name: Some(local_name(x, 0, 1)),
            mutable: false,
        };
        let params = [parameter];
        let initializer_use = local_path_expr(&arena, owner, 10, 20, 21, x, PathRoot::Plain);
        let pattern = local_binding_pat(
            &arena,
            owner,
            20,
            local_name(x, 25, 26),
            BindingId::from_raw(1),
            BindingMode::Value,
        );
        let statement = Stmt {
            hir_id: hir_id(owner, 22),
            kind: StmtKind::Let {
                pat: pattern,
                ty: None,
                init: initializer_use,
                els: None,
            },
            span: hir_span(25, 32),
        };
        let later_use = local_path_expr(&arena, owner, 30, 40, 41, x, PathRoot::Plain);
        let field_a_name = symbols.intern("a").expect("symbol interns");
        let field_b_name = symbols.intern("b").expect("symbol interns");
        let field_chain_root = local_path_expr(&arena, owner, 50, 42, 43, x, PathRoot::Plain);
        let field_a = expr(
            &arena,
            owner,
            53,
            42,
            44,
            ExprKind::Field {
                base: field_chain_root,
                name: local_name(field_a_name, 43, 44),
                args: None,
            },
        );
        let field_b = expr(
            &arena,
            owner,
            54,
            42,
            46,
            ExprKind::Field {
                base: field_a,
                name: local_name(field_b_name, 45, 46),
                args: None,
            },
        );
        let tail = expr(
            &arena,
            owner,
            55,
            40,
            46,
            ExprKind::Tuple(arena.alloc_exprs(&[*later_use, *field_b])),
        );
        let value = expr(
            &arena,
            owner,
            40,
            0,
            50,
            ExprKind::Block(block_with_stmts(
                &arena,
                owner,
                41,
                &[statement],
                Some(tail),
            )),
        );
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(&arena, owner, source_map.source_id(), &params, value, &[]);

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &HashSet::new())
            .expect("local paths resolve");
        assert!(result.matches_source(&source_map, &snapshot, &HashSet::new()));
        let (same_file_snapshot, _) = local_source_context(owner);
        assert!(source_map.matches_snapshot_file(&same_file_snapshot));
        let same_file_result = resolve_owner_locals(
            body,
            &source_map,
            &same_file_snapshot,
            &symbols,
            &HashSet::new(),
        )
        .expect("unchanged owner source remains valid");
        assert!(same_file_result.matches_source(&source_map, &same_file_snapshot, &HashSet::new()));
        let unchanged_file = Arc::new(
            SourceFile::new(FileId::new(1), " ".repeat(128), false).expect("owner source creates"),
        );
        let unrelated_file = Arc::new(
            SourceFile::new(FileId::new(2), "fn other() {}", false)
                .expect("unrelated source creates"),
        );
        let changed_workspace = WorkspaceSnapshot::new([
            SnapshotFile::new(unchanged_file, "src/lib.sai", None, None),
            SnapshotFile::new(unrelated_file, "src/other.sai", None, None),
        ])
        .expect("workspace with unrelated source creates");
        assert!(source_map.matches_snapshot_file(&changed_workspace));
        assert!(result.matches_source(&source_map, &changed_workspace, &HashSet::new()));
        let (different_version, _) = local_source_context_with(owner, &" ".repeat(128), Some(2));
        assert!(!source_map.matches_snapshot_file(&different_version));
        assert!(!result.matches_source(&source_map, &different_version, &HashSet::new()));
        let (stale_snapshot, _) =
            local_source_context_with(owner, &format!("x{}", " ".repeat(127)), None);
        assert!(!source_map.matches_snapshot_file(&stale_snapshot));
        assert!(!result.matches_source(&source_map, &stale_snapshot, &HashSet::new()));
        let (_, another_source_map) = local_source_context(owner);
        assert_ne!(source_map.source_id(), another_source_map.source_id());
        assert_eq!(
            resolve_owner_locals(
                body,
                &another_source_map,
                &snapshot,
                &symbols,
                &HashSet::new(),
            ),
            Err(LocalResolutionError::MismatchedBodySource)
        );
        assert_eq!(
            resolve_owner_locals(
                body,
                &source_map,
                &stale_snapshot,
                &symbols,
                &HashSet::new()
            )
            .unwrap_err(),
            LocalResolutionError::MismatchedSourceSnapshot
        );

        assert_eq!(
            result.paths.get(&match initializer_use.kind {
                ExprKind::Path(path) => path.hir_id,
                _ => unreachable!(),
            }),
            Some(&PathRes {
                base: Res::Local(BindingId::from_raw(0)),
                resolved_segments: 1,
                segment_count: 1,
            })
        );
        let ExprKind::Path(later_path) = later_use.kind else {
            unreachable!()
        };
        assert_eq!(
            result
                .paths
                .get(&later_path.hir_id)
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1)))
        );
        let ExprKind::Path(field_chain_path) = field_chain_root.kind else {
            unreachable!()
        };
        assert_eq!(
            result.paths.get(&field_chain_path.hir_id),
            Some(&PathRes {
                base: Res::Local(BindingId::from_raw(1)),
                resolved_segments: 1,
                segment_count: 1,
            })
        );
        assert_eq!(
            result
                .bindings
                .get(&BindingId::from_raw(0))
                .map(|binding| binding.kind),
            Some(BindingKind::Param)
        );
        assert_eq!(
            result
                .bindings
                .get(&BindingId::from_raw(1))
                .map(|binding| binding.kind),
            Some(BindingKind::Let)
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn local_binding_resolves_the_head_of_a_multi_segment_plain_path() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let receiver = symbols.intern("receiver").expect("symbol interns");
        let member = symbols.intern("member").expect("symbol interns");
        let params = [BodyParam {
            hir_id: hir_id(owner, 1),
            binding: BindingId::from_raw(0),
            name: Some(local_name(receiver, 0, 8)),
            mutable: false,
        }];
        let path_expr =
            local_multi_segment_path_expr(&arena, owner, 10, (receiver, 20, 28), (member, 29, 35));
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(
            &arena,
            owner,
            source_map.source_id(),
            &params,
            path_expr,
            &[],
        );

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &HashSet::new())
            .expect("local path head resolves");
        let ExprKind::Path(path) = path_expr.kind else {
            unreachable!()
        };
        assert_eq!(
            result.paths.get(&path.hir_id),
            Some(&PathRes {
                base: Res::Local(BindingId::from_raw(0)),
                resolved_segments: 1,
                segment_count: 2,
            })
        );
        assert!(result.deferred_paths.is_empty());
    }

    #[test]
    fn local_bindings_report_module_alias_shadowing() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let alias = symbols.intern("alias").expect("symbol interns");
        let module_aliases = HashSet::from([alias]);
        let params = [BodyParam {
            hir_id: hir_id(owner, 1),
            binding: BindingId::from_raw(0),
            name: Some(local_name(alias, 0, 1)),
            mutable: false,
        }];

        let let_pattern = local_binding_pat(
            &arena,
            owner,
            2,
            local_name(alias, 10, 11),
            BindingId::from_raw(1),
            BindingMode::Value,
        );
        let let_init = expr(
            &arena,
            owner,
            3,
            12,
            13,
            ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
        );
        let let_statement = Stmt {
            hir_id: hir_id(owner, 4),
            kind: StmtKind::Let {
                pat: let_pattern,
                ty: None,
                init: let_init,
                els: None,
            },
            span: hir_span(10, 13),
        };
        let let_block = block_with_stmts(&arena, owner, 5, &[let_statement], None);
        let let_expr = expr(&arena, owner, 6, 9, 14, ExprKind::Block(let_block));

        let arm_pattern = local_binding_pat(
            &arena,
            owner,
            7,
            local_name(alias, 20, 21),
            BindingId::from_raw(2),
            BindingMode::Value,
        );
        let arm_body = expr(
            &arena,
            owner,
            8,
            22,
            23,
            ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
        );
        let arm = sai_hir::expressions::Arm {
            hir_id: hir_id(owner, 9),
            pat: arm_pattern,
            guard: None,
            body: arm_body,
            span: hir_span(20, 23),
            synthesized: false,
        };
        let match_expr = expr(
            &arena,
            owner,
            10,
            19,
            24,
            ExprKind::Match {
                scrutinee: expr(
                    &arena,
                    owner,
                    11,
                    18,
                    19,
                    ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
                ),
                arms: arena.alloc_arms(&[arm]),
                source: MatchSource::Normal,
            },
        );

        let closure_param = sai_hir::expressions::ClosureParam {
            hir_id: hir_id(owner, 12),
            binding: BindingId::from_raw(3),
            name: Some(local_name(alias, 30, 31)),
            mutable: false,
            ty: None,
        };
        let closure_params = [closure_param];
        let closure_body_expr = expr(
            &arena,
            owner,
            13,
            32,
            33,
            ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
        );
        let closure = arena.alloc_closure(Closure {
            def_id: owner.def_id,
            capture: CaptureBy::Infer,
            params: &closure_params,
            ret: None,
            body: block(&arena, owner, 14, Some(closure_body_expr)),
        });
        let closure_expr = expr(&arena, owner, 15, 29, 34, ExprKind::Closure(closure));
        let value = expr(
            &arena,
            owner,
            16,
            9,
            35,
            ExprKind::Tuple(arena.alloc_exprs(&[*let_expr, *match_expr, *closure_expr])),
        );
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(&arena, owner, source_map.source_id(), &params, value, &[]);

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &module_aliases)
            .expect("local paths resolve");
        assert!(result.matches_source(&source_map, &snapshot, &module_aliases));
        assert!(!result.matches_source(&source_map, &snapshot, &HashSet::new()));
        let shadow_diagnostics = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ShadowsModule)
            .collect::<Vec<_>>();

        assert_eq!(shadow_diagnostics.len(), 4);
        assert!(
            shadow_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == Severity::Error)
        );
        assert!(
            shadow_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.primary().is_some())
        );
    }

    #[test]
    fn local_scopes_preserve_first_duplicate_and_do_not_leak_shadowing() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let x = symbols.intern("x").expect("symbol interns");
        let missing = symbols.intern("missing").expect("symbol interns");
        let first = local_binding_pat(
            &arena,
            owner,
            1,
            local_name(x, 1, 2),
            BindingId::from_raw(1),
            BindingMode::Value,
        );
        let duplicate = local_binding_pat(
            &arena,
            owner,
            2,
            local_name(x, 11, 12),
            BindingId::from_raw(2),
            BindingMode::Value,
        );
        let first_stmt = Stmt {
            hir_id: hir_id(owner, 3),
            kind: StmtKind::Let {
                pat: first,
                ty: None,
                init: expr(
                    &arena,
                    owner,
                    4,
                    3,
                    4,
                    ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
                ),
                els: None,
            },
            span: hir_span(0, 5),
        };
        let duplicate_stmt = Stmt {
            hir_id: hir_id(owner, 5),
            kind: StmtKind::Let {
                pat: duplicate,
                ty: None,
                init: expr(
                    &arena,
                    owner,
                    6,
                    13,
                    14,
                    ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
                ),
                els: None,
            },
            span: hir_span(10, 15),
        };
        let inner_binding = local_binding_pat(
            &arena,
            owner,
            7,
            local_name(x, 21, 22),
            BindingId::from_raw(3),
            BindingMode::Value,
        );
        let inner_stmt = Stmt {
            hir_id: hir_id(owner, 8),
            kind: StmtKind::Let {
                pat: inner_binding,
                ty: None,
                init: expr(
                    &arena,
                    owner,
                    9,
                    23,
                    24,
                    ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
                ),
                els: None,
            },
            span: hir_span(20, 25),
        };
        let inner_use = local_path_expr(&arena, owner, 10, 30, 31, x, PathRoot::Plain);
        let inner_expr = expr(
            &arena,
            owner,
            12,
            19,
            35,
            ExprKind::Block(block_with_stmts(
                &arena,
                owner,
                13,
                &[inner_stmt],
                Some(inner_use),
            )),
        );
        let outer_use = local_path_expr(&arena, owner, 14, 40, 41, x, PathRoot::Plain);
        let missing_use = local_path_expr(&arena, owner, 16, 45, 52, missing, PathRoot::Plain);
        let rooted_use = local_path_expr(&arena, owner, 18, 55, 56, x, PathRoot::SelfValue);
        let tuple = expr(
            &arena,
            owner,
            20,
            19,
            58,
            ExprKind::Tuple(arena.alloc_exprs(&[
                *inner_expr,
                *outer_use,
                *missing_use,
                *rooted_use,
            ])),
        );
        let value = expr(
            &arena,
            owner,
            21,
            0,
            60,
            ExprKind::Block(block_with_stmts(
                &arena,
                owner,
                22,
                &[first_stmt, duplicate_stmt],
                Some(tuple),
            )),
        );
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(&arena, owner, source_map.source_id(), &[], value, &[]);

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &HashSet::new())
            .expect("local paths resolve");

        let ExprKind::Path(inner_path) = inner_use.kind else {
            unreachable!()
        };
        let ExprKind::Path(outer_path) = outer_use.kind else {
            unreachable!()
        };
        let ExprKind::Path(missing_path) = missing_use.kind else {
            unreachable!()
        };
        let ExprKind::Path(rooted_path) = rooted_use.kind else {
            unreachable!()
        };
        assert_eq!(
            result
                .paths
                .get(&inner_path.hir_id)
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(3)))
        );
        assert_eq!(
            result
                .paths
                .get(&outer_path.hir_id)
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1)))
        );
        assert!(!result.paths.contains_key(&missing_path.hir_id));
        assert!(!result.paths.contains_key(&rooted_path.hir_id));
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::DuplicateBinding]
        );
        assert_eq!(
            result.diagnostics[0]
                .primary()
                .expect("duplicate has a primary span")
                .span
                .range
                .start()
                .raw(),
            11
        );
        let Related::DeclaredHere { location, .. } = &result.diagnostics[0].related()[0] else {
            panic!("duplicate binding points to its earlier declaration");
        };
        assert_eq!(location.span.range.start().raw(), 1);
        assert_eq!(
            result
                .paths
                .get(&outer_path.hir_id)
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1))),
            "the earlier same-scope binding remains the recovery target"
        );
    }

    #[test]
    fn let_else_region_and_generated_loop_bindings_have_their_own_scopes() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let x = symbols.intern("x").expect("symbol interns");
        let region = symbols.intern("region").expect("symbol interns");
        let iter = symbols.intern("%iter").expect("symbol interns");
        let parameter = BodyParam {
            hir_id: hir_id(owner, 1),
            binding: BindingId::from_raw(0),
            name: Some(local_name(x, 0, 1)),
            mutable: false,
        };
        let params = [parameter];
        let initializer_use = local_path_expr(&arena, owner, 10, 5, 6, x, PathRoot::Plain);
        let else_use = local_path_expr(&arena, owner, 12, 8, 9, x, PathRoot::Plain);
        let else_block = block(&arena, owner, 14, Some(else_use));
        let let_pattern = local_binding_pat(
            &arena,
            owner,
            15,
            local_name(x, 10, 11),
            BindingId::from_raw(1),
            BindingMode::Value,
        );
        let let_stmt = Stmt {
            hir_id: hir_id(owner, 16),
            kind: StmtKind::Let {
                pat: let_pattern,
                ty: None,
                init: initializer_use,
                els: Some(else_block),
            },
            span: hir_span(5, 15),
        };
        let after_let = local_path_expr(&arena, owner, 17, 16, 17, x, PathRoot::Plain);

        let region_name = Name {
            sym: region,
            span: HirSpan {
                ctxt: SpanCtxt::Root,
                ..hir_span(27, 33)
            },
        };
        let region_use = local_path_expr(&arena, owner, 18, 29, 35, region, PathRoot::Plain);
        let region_body = block(&arena, owner, 19, Some(region_use));
        let region_expr = expr(
            &arena,
            owner,
            20,
            20,
            36,
            ExprKind::Region {
                name: region_name,
                binding: BindingId::from_raw(2),
                block: region_body,
            },
        );

        let generated_span = HirSpan {
            ctxt: SpanCtxt::Desugared(DesugaringKind::ForLoop),
            ..hir_span(40, 45)
        };
        let generated_pat = arena.alloc_pat(Pat {
            hir_id: hir_id(owner, 21),
            kind: PatKind::Binding {
                mode: BindingMode::Var,
                name: Name {
                    sym: iter,
                    span: generated_span,
                },
                binding: BindingId::from_raw(3),
                sub: None,
            },
            span: generated_span,
        });
        let generated_use = local_path_expr(&arena, owner, 22, 46, 51, iter, PathRoot::Plain);
        let generated_arm = sai_hir::expressions::Arm {
            hir_id: hir_id(owner, 23),
            pat: generated_pat,
            guard: None,
            body: generated_use,
            span: generated_span,
            synthesized: true,
        };
        let for_match = expr(
            &arena,
            owner,
            24,
            40,
            52,
            ExprKind::Match {
                scrutinee: expr(
                    &arena,
                    owner,
                    25,
                    38,
                    39,
                    ExprKind::Lit(sai_hir::literals::Lit::Bool(true)),
                ),
                arms: arena.alloc_arms(&[generated_arm]),
                source: MatchSource::ForLoop,
            },
        );
        let after_arm = local_path_expr(&arena, owner, 26, 53, 58, iter, PathRoot::Plain);
        let error_name = local_path_expr(&arena, owner, 27, 59, 60, Symbol::ERROR, PathRoot::Plain);
        let tail = expr(
            &arena,
            owner,
            28,
            16,
            61,
            ExprKind::Tuple(arena.alloc_exprs(&[
                *after_let,
                *region_expr,
                *for_match,
                *after_arm,
                *error_name,
            ])),
        );
        let value = expr(
            &arena,
            owner,
            29,
            0,
            62,
            ExprKind::Block(block_with_stmts(&arena, owner, 30, &[let_stmt], Some(tail))),
        );
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(&arena, owner, source_map.source_id(), &params, value, &[]);

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &HashSet::new())
            .expect("local paths resolve");
        let path_id = |expr: &Expr<'_>| match expr.kind {
            ExprKind::Path(path) => path.hir_id,
            _ => unreachable!(),
        };

        assert_eq!(
            result
                .paths
                .get(&path_id(initializer_use))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(0)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(else_use))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(0)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(after_let))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(region_use))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(2)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(generated_use))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(3)))
        );
        assert!(!result.paths.contains_key(&path_id(after_arm)));
        assert_eq!(
            result
                .paths
                .get(&path_id(error_name))
                .map(|resolution| resolution.base),
            Some(Res::Err)
        );
        assert_eq!(
            result
                .bindings
                .get(&BindingId::from_raw(2))
                .map(|binding| binding.kind),
            Some(BindingKind::Synthetic)
        );
        assert_eq!(
            result
                .bindings
                .get(&BindingId::from_raw(3))
                .map(|binding| binding.kind),
            Some(BindingKind::Synthetic)
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn local_paths_cover_match_closure_async_and_contract_scopes() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let x = symbols.intern("x").expect("symbol interns");
        let y = symbols.intern("y").expect("symbol interns");
        let params = [
            BodyParam {
                hir_id: hir_id(owner, 1),
                binding: BindingId::from_raw(0),
                name: Some(local_name(x, 0, 1)),
                mutable: false,
            },
            BodyParam {
                hir_id: hir_id(owner, 2),
                binding: BindingId::from_raw(1),
                name: Some(local_name(y, 2, 3)),
                mutable: false,
            },
        ];
        let scrutinee = local_path_expr(&arena, owner, 10, 10, 11, x, PathRoot::Plain);
        let first_arm_pat = local_binding_pat(
            &arena,
            owner,
            20,
            local_name(y, 12, 13),
            BindingId::from_raw(2),
            BindingMode::Value,
        );
        let first_guard = local_path_expr(&arena, owner, 21, 14, 15, y, PathRoot::Plain);
        let first_body = local_path_expr(&arena, owner, 22, 16, 17, y, PathRoot::Plain);
        let second_arm_pat = local_binding_pat(
            &arena,
            owner,
            23,
            local_name(y, 18, 19),
            BindingId::from_raw(3),
            BindingMode::Value,
        );
        let second_body = local_path_expr(&arena, owner, 24, 20, 21, y, PathRoot::Plain);
        let arms = [
            sai_hir::expressions::Arm {
                hir_id: hir_id(owner, 25),
                pat: first_arm_pat,
                guard: Some(first_guard),
                body: first_body,
                span: hir_span(12, 17),
                synthesized: false,
            },
            sai_hir::expressions::Arm {
                hir_id: hir_id(owner, 26),
                pat: second_arm_pat,
                guard: None,
                body: second_body,
                span: hir_span(18, 22),
                synthesized: false,
            },
        ];
        let matched = expr(
            &arena,
            owner,
            27,
            10,
            23,
            ExprKind::Match {
                scrutinee,
                arms: arena.alloc_arms(&arms),
                source: MatchSource::Normal,
            },
        );
        let closure_param = sai_hir::expressions::ClosureParam {
            hir_id: hir_id(owner, 28),
            binding: BindingId::from_raw(4),
            name: Some(local_name(x, 30, 31)),
            mutable: true,
            ty: None,
        };
        let closure_params = [closure_param];
        let closure_x = local_path_expr(&arena, owner, 29, 32, 33, x, PathRoot::Plain);
        let closure_y = local_path_expr(&arena, owner, 31, 34, 35, y, PathRoot::Plain);
        let closure_tuple = expr(
            &arena,
            owner,
            33,
            32,
            36,
            ExprKind::Tuple(arena.alloc_exprs(&[*closure_x, *closure_y])),
        );
        let closure_body = block(&arena, owner, 34, Some(closure_tuple));
        let closure = arena.alloc_closure(Closure {
            def_id: owner.def_id,
            capture: CaptureBy::Infer,
            params: &closure_params,
            ret: None,
            body: closure_body,
        });
        let closure_expr = expr(&arena, owner, 35, 30, 37, ExprKind::Closure(closure));
        let async_y = local_path_expr(&arena, owner, 36, 38, 39, y, PathRoot::Plain);
        let async_body = block(&arena, owner, 37, Some(async_y));
        let async_expr = expr(
            &arena,
            owner,
            38,
            37,
            40,
            ExprKind::AsyncBlock {
                def_id: owner.def_id,
                capture: CaptureBy::Infer,
                block: async_body,
            },
        );
        let break_value = local_path_expr(&arena, owner, 42, 42, 43, x, PathRoot::Plain);
        let break_expr = expr(
            &arena,
            owner,
            43,
            41,
            44,
            ExprKind::Break {
                label: None,
                value: Some(break_value),
            },
        );
        let value = expr(
            &arena,
            owner,
            39,
            10,
            44,
            ExprKind::Tuple(arena.alloc_exprs(&[
                *matched,
                *closure_expr,
                *async_expr,
                *break_expr,
            ])),
        );
        let contract_use = local_path_expr(&arena, owner, 40, 50, 51, x, PathRoot::Plain);
        let contract = Contract {
            hir_id: hir_id(owner, 41),
            kind: ContractKind::Requires,
            expr: contract_use,
        };
        let contracts = [contract];
        let (snapshot, source_map) = local_source_context(owner);
        let body = body(
            &arena,
            owner,
            source_map.source_id(),
            &params,
            value,
            &contracts,
        );

        let result = resolve_owner_locals(body, &source_map, &snapshot, &symbols, &HashSet::new())
            .expect("local paths resolve");
        let path_id = |expr: &Expr<'_>| match expr.kind {
            ExprKind::Path(path) => path.hir_id,
            _ => unreachable!(),
        };

        assert_eq!(
            result
                .paths
                .get(&path_id(scrutinee))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(0)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(first_guard))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(2)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(first_body))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(2)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(second_body))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(3)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(closure_x))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(4)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(closure_y))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(async_y))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(1)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(contract_use))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(0)))
        );
        assert_eq!(
            result
                .paths
                .get(&path_id(break_value))
                .map(|resolution| resolution.base),
            Some(Res::Local(BindingId::from_raw(0)))
        );
        assert_eq!(
            result
                .bindings
                .get(&BindingId::from_raw(4))
                .map(|binding| binding.kind),
            Some(BindingKind::ClosureParam { var: true })
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn function_contract_forbidden_expressions_are_not_resolved_as_loop_jumps() {
        let arena = HirArena::new();
        let (owner, nested_def_id) = owner_id_with_local();
        let symbols = Interner::new();
        let missing_label = symbols.intern("missing").expect("symbol interns");
        let contract_break = expr(
            &arena,
            owner,
            1,
            10,
            22,
            ExprKind::Break {
                label: Some(label(missing_label, 16, 24)),
                value: None,
            },
        );
        let contract_return = expr(&arena, owner, 2, 30, 40, ExprKind::Return(None));
        let contract_continue = expr(&arena, owner, 3, 50, 60, ExprKind::Continue { label: None });
        let synthetic_break = arena.alloc_expr(Expr {
            hir_id: hir_id(owner, 4),
            kind: ExprKind::Break {
                label: None,
                value: None,
            },
            span: HirSpan {
                ctxt: SpanCtxt::Desugared(sai_hir::DesugaringKind::WhileLoop),
                ..hir_span(70, 76)
            },
        });
        let contract_while = expr(
            &arena,
            owner,
            5,
            65,
            80,
            ExprKind::Loop {
                body: block(&arena, owner, 6, Some(synthetic_break)),
                label: None,
                source: LoopSource::While,
            },
        );
        let closure_return = expr(&arena, owner, 14, 80, 88, ExprKind::Return(None));
        let closure = arena.alloc_closure(Closure {
            def_id: nested_def_id,
            capture: CaptureBy::Infer,
            params: &[],
            ret: None,
            body: block(&arena, owner, 15, Some(closure_return)),
        });
        let closure_contract = expr(&arena, owner, 16, 80, 90, ExprKind::Closure(closure));
        let async_return = expr(&arena, owner, 17, 90, 98, ExprKind::Return(None));
        let async_contract = expr(
            &arena,
            owner,
            18,
            90,
            100,
            ExprKind::AsyncBlock {
                def_id: nested_def_id,
                capture: CaptureBy::Infer,
                block: block(&arena, owner, 19, Some(async_return)),
            },
        );
        let function_break = expr(
            &arena,
            owner,
            8,
            100,
            110,
            ExprKind::Break {
                label: None,
                value: None,
            },
        );
        let function_loop = expr(
            &arena,
            owner,
            7,
            90,
            120,
            ExprKind::Loop {
                body: block(&arena, owner, 9, Some(function_break)),
                label: None,
                source: LoopSource::Loop,
            },
        );
        let contracts = [
            Contract {
                hir_id: hir_id(owner, 10),
                kind: ContractKind::Requires,
                expr: contract_break,
            },
            Contract {
                hir_id: hir_id(owner, 11),
                kind: ContractKind::Ensures,
                expr: contract_return,
            },
            Contract {
                hir_id: hir_id(owner, 12),
                kind: ContractKind::Requires,
                expr: contract_continue,
            },
            Contract {
                hir_id: hir_id(owner, 13),
                kind: ContractKind::Requires,
                expr: contract_while,
            },
            Contract {
                hir_id: hir_id(owner, 20),
                kind: ContractKind::Requires,
                expr: closure_contract,
            },
            Contract {
                hir_id: hir_id(owner, 21),
                kind: ContractKind::Requires,
                expr: async_contract,
            },
        ];
        let body = Body {
            owner,
            source_id: sai_hir::source_map::HirSourceId::default(),
            params: &[],
            value: function_loop,
            contracts: &contracts,
            invariant_preds: &[],
        };
        let (source_ref, source) = source();

        let result = resolve_owner_control_flow(&body, &source_ref, &source, &symbols)
            .expect("function control flow resolves");

        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                Code::InvalidContractExpression,
                Code::InvalidContractExpression,
                Code::InvalidContractExpression,
                Code::InvalidContractExpression,
            ]
        );
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    diagnostic
                        .primary()
                        .expect("diagnostic has a primary location")
                        .span
                        .range
                        .start()
                        .raw()
                })
                .collect::<Vec<_>>(),
            [10, 30, 50, 80]
        );
        assert!(!result.labels.contains_key(&contract_break.hir_id));
        assert!(!result.labels.contains_key(&contract_continue.hir_id));
        assert_eq!(
            result.labels.get(&function_break.hir_id),
            Some(&Ok(function_loop.hir_id))
        );
    }

    #[test]
    fn function_contract_rejects_remaining_forbidden_constructs() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let operand = expr(&arena, owner, 1, 11, 12, ExprKind::ContractResult);
        let try_expr = expr(&arena, owner, 2, 10, 13, ExprKind::Try(operand));
        let await_expr = expr(&arena, owner, 3, 20, 30, ExprKind::Await(operand));
        let unsafe_block = arena.alloc_block(Block {
            hir_id: hir_id(owner, 4),
            stmts: &[],
            tail: None,
            safety: Safety::Unsafe,
            span: hir_span(40, 50),
        });
        let unsafe_expr = expr(&arena, owner, 5, 40, 50, ExprKind::Block(unsafe_block));
        let region_block = block(&arena, owner, 6, None);
        let region_expr = expr(
            &arena,
            owner,
            7,
            60,
            70,
            ExprKind::Region {
                name: sai_hir::types::Name {
                    sym: symbols.intern("r").expect("symbol interns"),
                    span: hir_span(67, 68),
                },
                binding: sai_hir::BindingId::default(),
                block: region_block,
            },
        );
        let lhs = expr(&arena, owner, 8, 81, 82, ExprKind::ContractResult);
        let rhs = expr(&arena, owner, 9, 83, 84, ExprKind::ContractResult);
        let assignment = Stmt {
            hir_id: hir_id(owner, 10),
            kind: StmtKind::Assign { lhs, rhs },
            span: hir_span(80, 85),
        };
        let compound_assignment = Stmt {
            hir_id: hir_id(owner, 11),
            kind: StmtKind::AssignOp {
                op: sai_hir::expressions::BinOp::Add,
                lhs,
                rhs,
            },
            span: hir_span(90, 96),
        };
        let generated_assignment = Stmt {
            hir_id: hir_id(owner, 24),
            kind: StmtKind::Assign { lhs, rhs },
            span: HirSpan {
                ctxt: SpanCtxt::Desugared(DesugaringKind::ForLoop),
                ..hir_span(96, 100)
            },
        };
        let var_pat = arena.alloc_pat(sai_hir::patterns::Pat {
            hir_id: hir_id(owner, 12),
            kind: PatKind::Binding {
                mode: sai_hir::patterns::BindingMode::Var,
                name: sai_hir::types::Name {
                    sym: symbols.intern("mutable").expect("symbol interns"),
                    span: HirSpan {
                        ctxt: SpanCtxt::Desugared(DesugaringKind::VarStmt),
                        ..hir_span(100, 110)
                    },
                },
                binding: sai_hir::BindingId::default(),
                sub: None,
            },
            span: HirSpan {
                ctxt: SpanCtxt::Desugared(DesugaringKind::VarStmt),
                ..hir_span(100, 110)
            },
        });
        let var_statement = Stmt {
            hir_id: hir_id(owner, 13),
            kind: StmtKind::Let {
                pat: var_pat,
                ty: None,
                init: operand,
                els: None,
            },
            span: hir_span(100, 110),
        };
        let let_pat = arena.alloc_pat(sai_hir::patterns::Pat {
            hir_id: hir_id(owner, 14),
            kind: PatKind::Binding {
                mode: sai_hir::patterns::BindingMode::Var,
                name: sai_hir::types::Name {
                    sym: symbols.intern("mutable").expect("symbol interns"),
                    span: hir_span(110, 120),
                },
                binding: sai_hir::BindingId::default(),
                sub: None,
            },
            span: hir_span(110, 120),
        });
        let let_statement = Stmt {
            hir_id: hir_id(owner, 15),
            kind: StmtKind::Let {
                pat: let_pat,
                ty: None,
                init: operand,
                els: None,
            },
            span: hir_span(110, 120),
        };
        let statements = arena.alloc_stmts(&[
            Stmt {
                hir_id: hir_id(owner, 16),
                kind: StmtKind::Expr(try_expr),
                span: hir_span(10, 13),
            },
            Stmt {
                hir_id: hir_id(owner, 17),
                kind: StmtKind::Expr(await_expr),
                span: hir_span(20, 30),
            },
            Stmt {
                hir_id: hir_id(owner, 18),
                kind: StmtKind::Expr(unsafe_expr),
                span: hir_span(40, 50),
            },
            Stmt {
                hir_id: hir_id(owner, 19),
                kind: StmtKind::Expr(region_expr),
                span: hir_span(60, 70),
            },
            assignment,
            compound_assignment,
            generated_assignment,
            var_statement,
            let_statement,
        ]);
        let predicate_block = arena.alloc_block(Block {
            hir_id: hir_id(owner, 20),
            stmts: statements,
            tail: None,
            safety: Safety::Safe,
            span: hir_span(0, 128),
        });
        let predicate = expr(&arena, owner, 21, 0, 128, ExprKind::Block(predicate_block));
        let contracts = [Contract {
            hir_id: hir_id(owner, 22),
            kind: ContractKind::Requires,
            expr: predicate,
        }];
        let body_value = expr(&arena, owner, 23, 0, 1, ExprKind::Err);
        let body = Body {
            owner,
            source_id: sai_hir::source_map::HirSourceId::default(),
            params: &[],
            value: body_value,
            contracts: &contracts,
            invariant_preds: &[],
        };
        let (source_ref, source) = source();

        let result = resolve_owner_control_flow(&body, &source_ref, &source, &symbols)
            .expect("function contract restrictions resolve");

        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidContractExpression; 7]
        );
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    diagnostic
                        .primary()
                        .expect("diagnostic has a primary location")
                        .span
                        .range
                        .start()
                        .raw()
                })
                .collect::<Vec<_>>(),
            [10, 20, 40, 60, 80, 90, 100]
        );
        assert!(result.labels.is_empty());
        assert!(result.unused_labels.is_empty());

        let invariant_return = expr(&arena, owner, 25, 120, 126, ExprKind::Return(None));
        let invariant_predicates = [predicate, invariant_return];
        let invariant_body = Body {
            owner,
            source_id: sai_hir::source_map::HirSourceId::default(),
            params: &[],
            value: body_value,
            contracts: &[],
            invariant_preds: &invariant_predicates,
        };
        let invariant_result =
            resolve_owner_control_flow(&invariant_body, &source_ref, &source, &symbols)
                .expect("invariant predicate restrictions resolve");
        assert_eq!(
            invariant_result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidContractExpression; 8]
        );
        assert_eq!(
            invariant_result
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    diagnostic
                        .primary()
                        .expect("diagnostic has a primary location")
                        .span
                        .range
                        .start()
                        .raw()
                })
                .collect::<Vec<_>>(),
            [10, 20, 40, 60, 80, 90, 100, 120]
        );
        assert!(invariant_result.labels.is_empty());
    }

    #[test]
    fn predicate_expression_jumps_do_not_enter_loop_label_resolution() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let missing_label = symbols.intern("missing").expect("symbol interns");
        let jump = expr(
            &arena,
            owner,
            1,
            10,
            20,
            ExprKind::Break {
                label: Some(label(missing_label, 15, 22)),
                value: None,
            },
        );
        let loop_body = block(&arena, owner, 2, Some(jump));
        let predicate = expr(
            &arena,
            owner,
            0,
            0,
            30,
            ExprKind::Loop {
                body: loop_body,
                label: None,
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let diagnostics = resolve_predicate_expression(&predicate, &source_ref, &source)
            .expect("predicate restrictions resolve");

        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidContractExpression]
        );
        assert_eq!(
            diagnostics[0]
                .primary()
                .expect("diagnostic has a primary location")
                .span
                .range
                .start()
                .raw(),
            10
        );
    }

    fn expr<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        start: u32,
        end: u32,
        kind: ExprKind<'hir>,
    ) -> &'hir Expr<'hir> {
        arena.alloc_expr(Expr {
            hir_id: hir_id(owner, local),
            kind,
            span: hir_span(start, end),
        })
    }

    fn block<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: OwnerId,
        local: u32,
        tail: Option<&'hir Expr<'hir>>,
    ) -> &'hir Block<'hir> {
        arena.alloc_block(Block {
            hir_id: hir_id(owner, local),
            stmts: &[],
            tail,
            safety: sai_hir::Safety::Safe,
            span: hir_span(0, 100),
        })
    }

    #[test]
    fn labeled_jump_resolves_to_matching_outer_loop() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let outer_name = symbols.intern("outer").expect("symbol interns");
        let jump = expr(
            &arena,
            owner,
            3,
            30,
            40,
            ExprKind::Break {
                label: Some(label(outer_name, 37, 42)),
                value: None,
            },
        );
        let inner_body = block(&arena, owner, 2, Some(jump));
        let inner = expr(
            &arena,
            owner,
            1,
            20,
            50,
            ExprKind::Loop {
                body: inner_body,
                label: None,
                source: LoopSource::Loop,
            },
        );
        let outer_body = block(&arena, owner, 4, Some(inner));
        let outer = expr(
            &arena,
            owner,
            0,
            0,
            60,
            ExprKind::Loop {
                body: outer_body,
                label: Some(label(outer_name, 1, 6)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result = resolve_loop_labels(outer, &source_ref, &source, &symbols).expect("resolves");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Ok(outer.hir_id)));
        assert!(result.diagnostics.is_empty());
        assert!(result.unused_labels.is_empty());
    }

    #[test]
    fn closure_and_async_boundaries_hide_outer_loop_labels() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let outer_name = symbols.intern("outer").expect("symbol interns");

        let closure_jump = expr(
            &arena,
            owner,
            4,
            25,
            35,
            ExprKind::Break {
                label: Some(label(outer_name, 31, 36)),
                value: None,
            },
        );
        let closure_body = block(&arena, owner, 3, Some(closure_jump));
        let closure = arena.alloc_closure(Closure {
            def_id: owner.def_id,
            capture: CaptureBy::Infer,
            params: &[],
            ret: None,
            body: closure_body,
        });
        let closure_expr = expr(&arena, owner, 2, 20, 40, ExprKind::Closure(closure));

        let async_jump = expr(
            &arena,
            owner,
            7,
            55,
            65,
            ExprKind::Continue {
                label: Some(label(outer_name, 61, 66)),
            },
        );
        let async_body = block(&arena, owner, 6, Some(async_jump));
        let async_expr = expr(
            &arena,
            owner,
            5,
            50,
            70,
            ExprKind::AsyncBlock {
                def_id: owner.def_id,
                capture: CaptureBy::Infer,
                block: async_body,
            },
        );
        let expressions = arena.alloc_exprs(&[*closure_expr, *async_expr]);
        let tuple = expr(&arena, owner, 8, 20, 70, ExprKind::Tuple(expressions));
        let outer_body = block(&arena, owner, 9, Some(tuple));
        let outer = expr(
            &arena,
            owner,
            0,
            0,
            80,
            ExprKind::Loop {
                body: outer_body,
                label: Some(label(outer_name, 1, 6)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result = resolve_loop_labels(outer, &source_ref, &source, &symbols).expect("resolves");

        assert_eq!(result.labels.get(&closure_jump.hir_id), Some(&Err(())));
        assert_eq!(result.labels.get(&async_jump.hir_id), Some(&Err(())));
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::UndeclaredLabel, Code::UndeclaredLabel]
        );
        assert!(
            result
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.children().len() == 1)
        );
        assert_eq!(result.unused_labels.len(), 1);
        assert_eq!(result.unused_labels[0].loop_id, outer.hir_id);
    }

    #[test]
    fn nested_duplicate_labels_are_reported_and_unlabeled_jumps_do_not_use_labels() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let same_name = symbols.intern("same").expect("symbol interns");
        let jump = expr(
            &arena,
            owner,
            3,
            40,
            45,
            ExprKind::Break {
                label: None,
                value: None,
            },
        );
        let inner_body = block(&arena, owner, 2, Some(jump));
        let inner = expr(
            &arena,
            owner,
            1,
            20,
            50,
            ExprKind::Loop {
                body: inner_body,
                label: Some(label(same_name, 21, 26)),
                source: LoopSource::Loop,
            },
        );
        let outer_body = block(&arena, owner, 4, Some(inner));
        let outer = expr(
            &arena,
            owner,
            0,
            0,
            60,
            ExprKind::Loop {
                body: outer_body,
                label: Some(label(same_name, 1, 6)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result = resolve_loop_labels(outer, &source_ref, &source, &symbols).expect("resolves");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Ok(inner.hir_id)));
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::LabelShadowed]
        );
        assert_eq!(result.diagnostics[0].related().len(), 1);
        assert_eq!(result.unused_labels.len(), 2);
        assert_eq!(result.unused_labels[0].loop_id, outer.hir_id);
        assert_eq!(result.unused_labels[1].loop_id, inner.hir_id);
    }

    #[test]
    fn error_nodes_suppress_unused_label_candidates() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let name = symbols.intern("loop_label").expect("symbol interns");
        let error = expr(&arena, owner, 2, 10, 20, ExprKind::Err);
        let body = block(&arena, owner, 1, Some(error));
        let loop_expr = expr(
            &arena,
            owner,
            0,
            0,
            30,
            ExprKind::Loop {
                body,
                label: Some(label(name, 1, 11)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result =
            resolve_loop_labels(loop_expr, &source_ref, &source, &symbols).expect("resolves");

        assert!(result.unused_labels.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn diagnostics_convert_decoded_spans_to_original_source_offsets() {
        let arena = HirArena::new();
        let owner = owner_id();
        let jump = expr(
            &arena,
            owner,
            0,
            0,
            5,
            ExprKind::Break {
                label: None,
                value: None,
            },
        );
        let source_file = FileId::new(1);
        let source = SourceFile::new(source_file, " ".repeat(32), true).expect("source creates");
        let symbols = Interner::new();
        let source_ref = SourceRef {
            path: "src/lib.sai".to_owned(),
            package: Some("acme:app#1.0".to_owned()),
            snapshot: None,
        };

        let result = resolve_loop_labels(jump, &source_ref, &source, &symbols).expect("resolves");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Err(())));
        assert_eq!(
            result.diagnostics[0]
                .primary()
                .expect("primary")
                .span
                .range
                .start(),
            TextSize::new(3),
            "BOM occupies three original-file bytes"
        );
    }

    #[test]
    fn undeclared_labels_suggest_nearby_visible_labels() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let expected = symbols.intern("abcd").expect("symbol interns");
        let misspelled = symbols.intern("badc").expect("symbol interns");
        let jump = expr(
            &arena,
            owner,
            2,
            30,
            40,
            ExprKind::Break {
                label: Some(label(misspelled, 36, 41)),
                value: None,
            },
        );
        let body = block(&arena, owner, 1, Some(jump));
        let loop_expr = expr(
            &arena,
            owner,
            0,
            0,
            50,
            ExprKind::Loop {
                body,
                label: Some(label(expected, 1, 6)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result =
            resolve_loop_labels(loop_expr, &source_ref, &source, &symbols).expect("resolves");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Err(())));
        assert_eq!(result.diagnostics[0].code, Code::UndeclaredLabel);
        assert_eq!(result.diagnostics[0].children().len(), 1);
        assert_eq!(
            result.diagnostics[0].children()[0].message,
            "similar labels: 'abcd"
        );
        assert_eq!(
            edit_distance_at_most_two("outr", "outer"),
            Some(1),
            "insertion candidates remain inside the bounded edit-distance band"
        );
        assert_eq!(
            edit_distance_at_most_two("abcd", "badc"),
            Some(2),
            "two adjacent transpositions count as two edits"
        );
        assert_eq!(
            edit_distance_at_most_two("CA", "ABC"),
            Some(2),
            "unrestricted Damerau-Levenshtein permits a transposition followed by insertion"
        );
        assert_eq!(
            edit_distance_at_most_two("é猫", "猫é"),
            Some(1),
            "distance is measured over Unicode scalar values"
        );
        assert_eq!(edit_distance_at_most_two("unrelated", "outer"), None);
    }

    #[test]
    fn bounded_damerau_distance_matches_full_distance_for_short_names() {
        fn collect_words(prefix: &mut String, remaining: usize, words: &mut Vec<String>) {
            words.push(prefix.clone());
            if remaining == 0 {
                return;
            }
            for character in ['a', 'b', 'c'] {
                prefix.push(character);
                collect_words(prefix, remaining - 1, words);
                prefix.pop();
            }
        }

        fn full_damerau_distance(left: &str, right: &str) -> usize {
            let left = left.chars().collect::<Vec<_>>();
            let right = right.chars().collect::<Vec<_>>();
            let maximum = left.len() + right.len();
            let mut distances = vec![vec![0; right.len() + 2]; left.len() + 2];
            distances[0][0] = maximum;
            for index in 0..=left.len() {
                distances[index + 1][0] = maximum;
                distances[index + 1][1] = index;
            }
            for index in 0..=right.len() {
                distances[0][index + 1] = maximum;
                distances[1][index + 1] = index;
            }

            let mut last_seen = HashMap::<char, usize>::new();
            for left_index in 1..=left.len() {
                let mut last_match = 0usize;
                for right_index in 1..=right.len() {
                    let previous_left =
                        last_seen.get(&right[right_index - 1]).copied().unwrap_or(0);
                    let previous_right = last_match;
                    let substitution_cost = if left[left_index - 1] == right[right_index - 1] {
                        last_match = right_index;
                        0
                    } else {
                        1
                    };
                    distances[left_index + 1][right_index + 1] = [
                        distances[left_index][right_index] + substitution_cost,
                        distances[left_index + 1][right_index] + 1,
                        distances[left_index][right_index + 1] + 1,
                        distances[previous_left][previous_right]
                            + (left_index - previous_left - 1)
                            + 1
                            + (right_index - previous_right - 1),
                    ]
                    .into_iter()
                    .min()
                    .expect("distance candidates exist");
                }
                last_seen.insert(left[left_index - 1], left_index);
            }
            distances[left.len() + 1][right.len() + 1]
        }

        let mut words = Vec::new();
        collect_words(&mut String::new(), 4, &mut words);
        for left in &words {
            for right in &words {
                let full_distance = full_damerau_distance(left, right);
                let expected = (full_distance <= 2).then_some(full_distance);
                assert_eq!(
                    edit_distance_at_most_two(left, right),
                    expected,
                    "bounded distance differs for {left:?} and {right:?}"
                );
            }
        }
    }

    #[test]
    fn error_inside_nested_closure_suppresses_outer_unused_label_lint_candidate() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let label_name = symbols.intern("outer").expect("symbol interns");
        let error = expr(&arena, owner, 3, 25, 30, ExprKind::Err);
        let closure_body = block(&arena, owner, 2, Some(error));
        let closure = arena.alloc_closure(Closure {
            def_id: owner.def_id,
            capture: CaptureBy::Infer,
            params: &[],
            ret: None,
            body: closure_body,
        });
        let closure_expr = expr(&arena, owner, 1, 20, 40, ExprKind::Closure(closure));
        let body = block(&arena, owner, 4, Some(closure_expr));
        let loop_expr = expr(
            &arena,
            owner,
            0,
            0,
            50,
            ExprKind::Loop {
                body,
                label: Some(label(label_name, 1, 6)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result =
            resolve_loop_labels(loop_expr, &source_ref, &source, &symbols).expect("resolves");

        assert!(result.unused_labels.is_empty());
    }

    #[test]
    fn error_named_jump_does_not_cascade_to_undeclared_label() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let jump = expr(
            &arena,
            owner,
            0,
            0,
            5,
            ExprKind::Break {
                label: Some(label(sai_hir::Symbol::ERROR, 2, 3)),
                value: None,
            },
        );
        let (source_ref, source) = source();

        let result = resolve_loop_labels(jump, &source_ref, &source, &symbols)
            .expect("error names are already diagnosed");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Err(())));
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn error_named_loop_label_does_not_abort_undeclared_label_resolution() {
        let arena = HirArena::new();
        let owner = owner_id();
        let symbols = Interner::new();
        let missing = symbols.intern("missing").expect("symbol interns");
        let jump = expr(
            &arena,
            owner,
            2,
            20,
            30,
            ExprKind::Break {
                label: Some(label(missing, 26, 33)),
                value: None,
            },
        );
        let inner_body = block(&arena, owner, 1, Some(jump));
        let inner = expr(
            &arena,
            owner,
            0,
            10,
            40,
            ExprKind::Loop {
                body: inner_body,
                label: Some(label(sai_hir::Symbol::ERROR, 11, 12)),
                source: LoopSource::Loop,
            },
        );
        let (source_ref, source) = source();

        let result = resolve_loop_labels(inner, &source_ref, &source, &symbols)
            .expect("error labels are ignored as suggestions");

        assert_eq!(result.labels.get(&jump.hir_id), Some(&Err(())));
        assert_eq!(result.diagnostics[0].code, Code::UndeclaredLabel);
        assert!(result.diagnostics[0].children().is_empty());
    }
}
