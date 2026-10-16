use super::{
    CurrentOwnerInputs, DetachedWorkspaceDiagnostic, InterfaceResourceMemberIds,
    OwnedPhase1ModuleInput, OwnerControlFlowResolutionError, Phase1ModuleScopes,
    Phase1PackageScopesQueryOutput, Prelude, PreparedPhase1PackageScopes,
    PrimitivePreludeBuildError, STAGED_PHASE1_PACKAGE_SCOPES, TypeOwnerPathResolution,
    append_interface_use_references, index_top_level_owner_descriptors_in_scopes,
    interface_generation_order, interface_member_kind, local_interface_type_member_owners,
    lower_interface_item_owner, lower_interface_resource_member_owner, lower_top_level_owner,
    module_scope_for_scopes, owner_test_only_path_candidates, owner_type_generic_resolutions,
    resolve_partial_type_owner_paths,
};
use sai_diagnostics::{
    Child, ChildKind, Diagnostic, Location, Related, Severity, SourceRef, Value,
};
use sai_hir::arena::HirArena;
use sai_hir::item_tree::ItemTreeKind;
use sai_hir::items::{HirOwner, InterfaceItemKind, ItemKind, MemberRole, OwnerNode, VariantShape};
use sai_hir::lower::HirLoweringContext;
use sai_hir::types::{GenericArg, Name, Ty, TyKind};
use sai_hir::{
    DefId, DefPathData, DefPathError, DefPathHash, DefPathInternerError, HirId, LocalDefId,
    OwnerId, Symbol,
};
use sai_intern::{Interner, PackageInterner};
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_resolve::module_member_lookup::ModuleMemberIndex;
use sai_resolve::package_scopes::{
    DefKind, PackageScopesError, PathRes, Phase1PackageScopes, Res, TopLevelOwnerKind,
};
use sai_resolve::{
    InterfaceUseReference, InterfaceUses, InterfaceUsesInput, WitNameCollision, WitNameDefinition,
    interface_uses, sai_identifier_to_wit_name, wit_name_collisions,
};
use sai_stable_hash::{Fingerprint, StableHash, StableHasher};
use sai_ty::boundary::{
    BoundaryAlias, BoundaryDefinition, BoundaryType, BoundaryTypeContext, BoundaryTypeOutcome,
    IncompatibleReason, Position, boundary_type_with_references,
};
use sai_ty::iface::{
    InterfaceCheckError, InterfaceCheckIssue, InterfaceCheckIssueKind, interface_item_check,
};
use sai_vfs::{FileId, PackageKey, Span, WorkspaceSnapshot};
use sai_wit::{WitViewDefKind, WitViewEntity, wit_name_to_sai};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

struct LocalBoundaryAliases<'hir> {
    definitions: HashMap<DefId, BoundaryAlias<'hir>>,
    path_resolutions: HashMap<sai_hir::HirId, PathRes>,
    interface_references: HashMap<DefId, Vec<DefId>>,
}

#[derive(Clone, Debug)]
struct BoundaryUseAliasName {
    target: String,
    emitted_name: String,
    collision_name: String,
    local_interface: Option<LocalDefId>,
}

#[derive(Clone, Debug)]
struct GeneratedUseAlias {
    target: String,
    emitted_name: String,
    collision_name: String,
    definition: DefId,
    owner: OwnerId,
    span: sai_hir::Span,
}

struct InterfaceNameCollisionInput {
    interface: LocalDefId,
    file: FileId,
    generated_use_aliases: Vec<GeneratedUseAlias>,
    local_definitions: Vec<WitNameDefinition>,
}

#[derive(Clone, Debug)]
enum InterfaceNameOrigin {
    Local(String),
    GeneratedUse { target: String, name: String },
}

#[derive(Clone, Debug)]
struct InterfaceNameDefinition {
    collision_name: String,
    origin: InterfaceNameOrigin,
    owner: OwnerId,
    span: sai_hir::Span,
}

#[derive(Default)]
struct FirstInterfaceName {
    span: Option<sai_hir::Span>,
    local_spellings: HashSet<String>,
    generated_uses: HashSet<(String, String)>,
}

#[derive(Clone, Copy)]
enum LocalAliasReference {
    InterfaceType(DefId),
    ModuleAlias(DefId),
}

/// A failure while deriving or rebinding `interface_uses(P)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterfaceUsesQueryError {
    Query(QueryError),
    Collection(OwnerControlFlowResolutionError),
    PackageScopes(PackageScopesError),
    DefPath(DefPathError),
    DefPathInterner(DefPathInternerError),
    DiagnosticTemplate(String),
}

#[derive(Clone, Debug)]
struct InterfaceUsesMemo {
    graph: InterfaceUses,
    diagnostics: Vec<DetachedWorkspaceDiagnostic>,
    interface_check_diagnostics: Vec<DetachedWorkspaceDiagnostic>,
}

impl PartialEq for InterfaceUsesMemo {
    fn eq(&self, other: &Self) -> bool {
        self.graph.uses == other.graph.uses
            && self.graph.cyclic == other.graph.cyclic
            && self.diagnostics == other.diagnostics
            && self.interface_check_diagnostics == other.interface_check_diagnostics
    }
}

impl Eq for InterfaceUsesMemo {}

#[derive(Clone)]
pub(super) struct InterfaceUsesQueryOutput {
    result: Result<InterfaceUsesMemo, InterfaceUsesQueryError>,
    result_fingerprint: Fingerprint,
}

impl PartialEq for InterfaceUsesQueryOutput {
    fn eq(&self, other: &Self) -> bool {
        self.result == other.result && self.result_fingerprint == other.result_fingerprint
    }
}

impl Eq for InterfaceUsesQueryOutput {}

impl StableHash for InterfaceUsesQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.result_fingerprint.stable_hash(hasher);
    }
}

pub(super) struct InterfaceUsesQueries {
    pub(super) query: DerivedQuery<PackageKey, InterfaceUsesQueryOutput>,
}

impl InterfaceUsesQueries {
    pub(super) fn new(
        package_scopes: DerivedQuery<PackageKey, Phase1PackageScopesQueryOutput>,
        packages: Arc<PackageInterner>,
        symbols: Arc<Interner>,
        primitive_prelude: Result<Prelude, PrimitivePreludeBuildError>,
    ) -> Result<Self, QueryError> {
        let query = DerivedQuery::new(
            "interface_uses",
            move |database: &mut QueryDatabase, key: &PackageKey| {
                let scopes_output = package_scopes.get(database, key.clone())?;
                let prepared = STAGED_PHASE1_PACKAGE_SCOPES.with(|staged| {
                    staged
                        .borrow()
                        .get(key)
                        .filter(|staged| {
                            staged.identity.as_ref() == scopes_output.identity.as_ref()
                        })
                        .map(|staged| Arc::clone(&staged.prepared))
                });
                let Some(prepared) = prepared else {
                    return Err(QueryError::MissingInput {
                        query: "interface_uses_prepared_phase1_inputs".to_owned(),
                    });
                };
                let scopes = scopes_output
                    .result
                    .as_ref()
                    .map_err(|_| QueryError::MissingInput {
                        query: "interface_uses_phase1_scopes".to_owned(),
                    })?
                    .scopes
                    .clone();
                let result = collect_interface_uses_input(
                    key,
                    &prepared,
                    &scopes,
                    &primitive_prelude,
                    &symbols,
                )
                .and_then(|(input, interface_check_diagnostics)| {
                    let graph = interface_uses(input.clone());
                    let result_fingerprint = stable_interface_uses_fingerprint(
                        key,
                        &input,
                        &graph,
                        &interface_check_diagnostics,
                        &prepared,
                        &packages,
                        &symbols,
                    )?;
                    let mut graph = graph;
                    let diagnostics = std::mem::take(&mut graph.diagnostics)
                        .into_iter()
                        .map(|diagnostic| {
                            let mut detached = DetachedWorkspaceDiagnostic::from_diagnostic(
                                &diagnostic,
                            )
                            .map_err(|error| {
                                InterfaceUsesQueryError::DiagnosticTemplate(format!("{error:?}"))
                            })?;
                            clear_document_versions(&mut detached);
                            Ok(detached)
                        })
                        .collect::<Result<Vec<_>, InterfaceUsesQueryError>>()?;
                    let interface_check_diagnostics = interface_check_diagnostics
                        .iter()
                        .map(|diagnostic| {
                            let mut detached = DetachedWorkspaceDiagnostic::from_diagnostic(
                                diagnostic,
                            )
                            .map_err(|error| {
                                InterfaceUsesQueryError::DiagnosticTemplate(format!("{error:?}"))
                            })?;
                            clear_document_versions(&mut detached);
                            Ok(detached)
                        })
                        .collect::<Result<Vec<_>, InterfaceUsesQueryError>>()?;
                    Ok((
                        InterfaceUsesMemo {
                            graph,
                            diagnostics,
                            interface_check_diagnostics,
                        },
                        result_fingerprint,
                    ))
                });
                let (result, result_fingerprint) = match result {
                    Ok((memo, fingerprint)) => (Ok(memo), fingerprint),
                    Err(error) => {
                        let fingerprint = interface_uses_error_fingerprint(&error);
                        (Err(error), fingerprint)
                    }
                };
                Ok(InterfaceUsesQueryOutput {
                    result,
                    result_fingerprint,
                })
            },
        )?;
        Ok(Self { query })
    }

    pub(super) fn get(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<(InterfaceUses, Fingerprint, Vec<Diagnostic>), InterfaceUsesQueryError> {
        let output = self
            .query
            .get(database, key)
            .map_err(InterfaceUsesQueryError::Query)?;
        let memo = output.result.as_ref().map_err(Clone::clone)?;
        let mut graph = memo.graph.clone();
        graph.diagnostics = memo
            .diagnostics
            .iter()
            .map(|diagnostic| bind_current_snapshot(diagnostic, snapshot))
            .collect::<Result<Vec<_>, _>>()
            .map_err(InterfaceUsesQueryError::PackageScopes)?;
        let interface_check_diagnostics = memo
            .interface_check_diagnostics
            .iter()
            .map(|diagnostic| bind_current_snapshot(diagnostic, snapshot))
            .collect::<Result<Vec<_>, _>>()
            .map_err(InterfaceUsesQueryError::PackageScopes)?;
        Ok((
            graph,
            output.result_fingerprint,
            interface_check_diagnostics,
        ))
    }
}

fn append_interface_item_diagnostics(
    issues: Vec<InterfaceCheckIssue>,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), InterfaceUsesQueryError> {
    for issue in issues {
        let code = issue.kind.code();
        let snapshot_file = snapshot.file(issue.span.file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::MissingSnapshotFile(issue.span.file),
            )
        })?;
        let location = Location::from_decoded_source(
            source_ref.clone(),
            sai_diagnostics::Span::new(issue.span.file, issue.span.range),
            source,
        )
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::InvalidSourceLocation(issue.owner),
            )
        })?;
        let mut diagnostic =
            Diagnostic::new(code, Severity::Error, issue.message(), Some(location));
        diagnostic.primary_label = issue.kind.primary_label().to_owned();
        diagnostic.package = snapshot_file.package().map(str::to_owned);
        if let Some(suggestion) = issue.suggestion {
            diagnostic
                .data
                .insert("suggestion".to_owned(), Value::String(suggestion.clone()));
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Help,
                    message: format!("try `{suggestion}`"),
                    location: None,
                })
                .expect("unbound diagnostics accept help children");
        }
        if let Some(earlier_span) = issue.related_span {
            let related_location = Location::from_decoded_source(
                source_ref.clone(),
                sai_diagnostics::Span::new(earlier_span.file, earlier_span.range),
                source,
            )
            .ok_or_else(|| {
                InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::InvalidSourceLocation(issue.owner),
                )
            })?;
            diagnostic
                .push_related(Related::DeclaredHere {
                    location: related_location,
                    what: "earlier declaration".to_owned(),
                })
                .expect("unbound diagnostics accept related locations");
        }
        diagnostics.push(diagnostic);
    }
    Ok(())
}

fn wit_name_definition(
    name: Name,
    owner: OwnerId,
    symbols: &Interner,
) -> Result<Option<WitNameDefinition>, InterfaceUsesQueryError> {
    if name.is_error() {
        return Ok(None);
    }
    let source_name = symbols
        .resolve(name.sym)
        .map_err(OwnerControlFlowResolutionError::SymbolLookup)
        .map_err(InterfaceUsesQueryError::Collection)?;
    Ok(Some(WitNameDefinition {
        source_name: source_name.to_string(),
        owner,
        span: name.span,
    }))
}

fn append_wit_name_collision_diagnostics(
    definitions: impl IntoIterator<Item = WitNameDefinition>,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<bool, InterfaceUsesQueryError> {
    let collisions = wit_name_collisions(definitions);
    let has_collisions = !collisions.is_empty();
    append_wit_name_collisions(collisions, source_ref, source, snapshot, diagnostics)?;
    Ok(has_collisions)
}

fn append_wit_name_collisions(
    collisions: impl IntoIterator<Item = WitNameCollision>,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), InterfaceUsesQueryError> {
    for collision in collisions {
        let snapshot_file = snapshot.file(collision.later_span.file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::MissingSnapshotFile(collision.later_span.file),
            )
        })?;
        let location = Location::from_decoded_source(
            source_ref.clone(),
            sai_diagnostics::Span::new(collision.later_span.file, collision.later_span.range),
            source,
        )
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::InvalidSourceLocation(collision.later_owner),
            )
        })?;
        let earlier_location = Location::from_decoded_source(
            source_ref.clone(),
            sai_diagnostics::Span::new(collision.earlier_span.file, collision.earlier_span.range),
            source,
        )
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::InvalidSourceLocation(collision.later_owner),
            )
        })?;
        let mut diagnostic = Diagnostic::new(
            sai_diagnostics::Code::WitNameCollision,
            Severity::Error,
            format!(
                "multiple declarations map to WIT name `{}`",
                collision.wit_name
            ),
            Some(location),
        );
        diagnostic.primary_label = "colliding WIT name".to_owned();
        diagnostic.package = snapshot_file.package().map(str::to_owned);
        diagnostic
            .push_related(Related::DeclaredHere {
                location: earlier_location,
                what: "earlier declaration".to_owned(),
            })
            .expect("unbound diagnostics accept related locations");
        diagnostics.push(diagnostic);
    }
    Ok(())
}

fn interface_boundary_use_alias_names(
    local_interface_type_members: &HashMap<DefId, LocalDefId>,
    interface_boundary_types: &HashMap<DefId, BoundaryDefinition>,
    interface_descriptors: &[super::TopLevelOwnerDescriptor],
    scopes: &Phase1PackageScopes,
    prepared: &PreparedPhase1PackageScopes,
    symbols: &Interner,
    ineligible_interfaces: &mut HashSet<LocalDefId>,
) -> Result<HashMap<DefId, BoundaryUseAliasName>, InterfaceUsesQueryError> {
    let mut names = HashMap::new();
    let mut local_interface_names = HashMap::new();
    let mut seen_interface_names = HashSet::new();
    for descriptor in interface_descriptors {
        let owner = descriptor.owner.def_id;
        let Some(interface_name) = local_interface_wit_name(owner, prepared, symbols)? else {
            ineligible_interfaces.insert(owner);
            continue;
        };
        if seen_interface_names.insert(interface_name.clone()) {
            local_interface_names.insert(owner, interface_name);
        } else {
            ineligible_interfaces.insert(owner);
        }
    }
    for (interface, scope) in &scopes.interfaces {
        for (symbol, binding) in &scope.members {
            let Res::Def(_, definition) = binding.res else {
                continue;
            };
            let Some(&owner) = local_interface_type_members.get(&definition) else {
                continue;
            };
            if owner.to_def_id() != *interface
                || !interface_boundary_types.contains_key(&definition)
            {
                continue;
            }
            let source_name = symbols
                .resolve(*symbol)
                .map_err(OwnerControlFlowResolutionError::SymbolLookup)
                .map_err(InterfaceUsesQueryError::Collection)?;
            let Some(collision_name) = sai_identifier_to_wit_name(&source_name) else {
                continue;
            };
            let Some(interface_name) = local_interface_names.get(&owner) else {
                continue;
            };
            names.insert(
                definition,
                BoundaryUseAliasName {
                    target: render_wit_name(interface_name),
                    emitted_name: render_wit_name(&collision_name),
                    collision_name,
                    local_interface: Some(owner),
                },
            );
        }
    }

    for foreign in &prepared.foreign_wit_views {
        for view_interface in foreign.view.interfaces.values() {
            for member in view_interface.members.values() {
                if !interface_boundary_types.contains_key(&member.def)
                    || !matches!(
                        member.kind,
                        WitViewDefKind::Struct
                            | WitViewDefKind::Enum
                            | WitViewDefKind::Resource
                            | WitViewDefKind::Alias
                    )
                {
                    continue;
                }
                let Some(definition) = foreign.view.definition(member.def) else {
                    continue;
                };
                let WitViewEntity::Item { interface, item } = definition.entity else {
                    continue;
                };
                let Some(wit_interface) = foreign.view.model.interfaces.get(interface) else {
                    continue;
                };
                let Some(wit_item) = wit_interface.items.get(item) else {
                    continue;
                };
                let Ok(source_name) = wit_name_to_sai(&wit_item.name, true) else {
                    continue;
                };
                let Some(collision_name) = sai_identifier_to_wit_name(&source_name) else {
                    continue;
                };
                names.insert(
                    member.def,
                    BoundaryUseAliasName {
                        target: external_wit_interface_use_target(
                            &foreign.view.model.name.namespace,
                            &foreign.view.model.name.name,
                            &wit_interface.name,
                            foreign.view.model.name.version.as_ref(),
                        ),
                        emitted_name: render_wit_name(&wit_item.name),
                        collision_name,
                        local_interface: None,
                    },
                );
            }
        }
    }
    Ok(names)
}

fn local_interface_wit_name(
    interface: LocalDefId,
    prepared: &PreparedPhase1PackageScopes,
    symbols: &Interner,
) -> Result<Option<String>, InterfaceUsesQueryError> {
    let path = prepared
        .def_paths
        .resolve_def_id(interface.to_def_id())
        .map_err(OwnerControlFlowResolutionError::DefPathInterner)
        .map_err(InterfaceUsesQueryError::Collection)?
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::MissingDefPath(
                OwnerId { def_id: interface },
            ))
        })?;
    let Some(DefPathData::Interface(name)) = path.data().last().map(|part| &part.data) else {
        return Ok(None);
    };
    let source_name = symbols
        .resolve(*name)
        .map_err(OwnerControlFlowResolutionError::SymbolLookup)
        .map_err(InterfaceUsesQueryError::Collection)?;
    Ok(sai_identifier_to_wit_name(&source_name))
}

fn external_wit_interface_use_target(
    namespace: &str,
    package: &str,
    interface: &str,
    version: Option<&sai_package::version::Version>,
) -> String {
    let mut target = format!(
        "{}:{}/{}",
        render_wit_name(namespace),
        render_wit_name(package),
        render_wit_name(interface)
    );
    if let Some(version) = version {
        target.push('@');
        target.push_str(&version.to_string());
    }
    target
}

fn render_wit_name(name: &str) -> String {
    let name = name.strip_prefix('%').unwrap_or(name);
    sai_wit::wit_identifier(name)
}

fn collect_interface_name_collisions(
    mut generated_use_aliases: Vec<GeneratedUseAlias>,
    local_definitions: impl IntoIterator<Item = WitNameDefinition>,
) -> Vec<WitNameCollision> {
    generated_use_aliases.sort_by(|left, right| {
        left.target
            .as_bytes()
            .cmp(right.target.as_bytes())
            .then_with(|| {
                left.emitted_name
                    .as_bytes()
                    .cmp(right.emitted_name.as_bytes())
            })
            .then_with(|| left.span.range.start().cmp(&right.span.range.start()))
    });
    generated_use_aliases.dedup_by(|later, earlier| {
        later.target == earlier.target && later.emitted_name == earlier.emitted_name
    });

    let mut first_by_wit_name = HashMap::<String, FirstInterfaceName>::new();
    let mut collisions = Vec::new();
    for alias in generated_use_aliases {
        record_interface_name_definition(
            InterfaceNameDefinition {
                collision_name: alias.collision_name,
                origin: InterfaceNameOrigin::GeneratedUse {
                    target: alias.target,
                    name: alias.emitted_name,
                },
                owner: alias.owner,
                span: alias.span,
            },
            &mut first_by_wit_name,
            &mut collisions,
        );
    }
    for definition in local_definitions {
        let Some(collision_name) = sai_identifier_to_wit_name(&definition.source_name) else {
            continue;
        };
        record_interface_name_definition(
            InterfaceNameDefinition {
                collision_name,
                origin: InterfaceNameOrigin::Local(definition.source_name),
                owner: definition.owner,
                span: definition.span,
            },
            &mut first_by_wit_name,
            &mut collisions,
        );
    }
    collisions
}

fn record_interface_name_definition(
    definition: InterfaceNameDefinition,
    first_by_wit_name: &mut HashMap<String, FirstInterfaceName>,
    collisions: &mut Vec<WitNameCollision>,
) {
    let first = first_by_wit_name
        .entry(definition.collision_name.clone())
        .or_default();
    let Some(earlier_span) = first.span else {
        first.span = Some(definition.span);
        remember_interface_name_origin(first, &definition.origin);
        return;
    };
    let first_has_generated_use = !first.generated_uses.is_empty();
    let should_report =
        !matches!(&definition.origin, InterfaceNameOrigin::Local(_)) || first_has_generated_use;
    if remember_interface_name_origin(first, &definition.origin) && should_report {
        collisions.push(WitNameCollision {
            wit_name: definition.collision_name,
            earlier_span,
            later_owner: definition.owner,
            later_span: definition.span,
        });
    }
}

fn remember_interface_name_origin(
    first: &mut FirstInterfaceName,
    origin: &InterfaceNameOrigin,
) -> bool {
    match origin {
        InterfaceNameOrigin::Local(source_name) => {
            first.local_spellings.insert(source_name.clone())
        }
        InterfaceNameOrigin::GeneratedUse { target, name } => {
            first.generated_uses.insert((target.clone(), name.clone()))
        }
    }
}

fn collect_interface_item_issues(
    owner: &HirOwner<'_>,
    flags_marker_accepted: bool,
    symbols: &Interner,
) -> Result<Vec<InterfaceCheckIssue>, InterfaceUsesQueryError> {
    interface_item_check(owner, flags_marker_accepted, symbols)
        .map_err(|error| match error {
            InterfaceCheckError::SymbolLookup(error) => {
                OwnerControlFlowResolutionError::SymbolLookup(error)
            }
        })
        .map_err(InterfaceUsesQueryError::Collection)
}

fn collect_interface_uses_input(
    package_key: &PackageKey,
    prepared: &PreparedPhase1PackageScopes,
    scopes: &Phase1PackageScopes,
    primitive_prelude: &Result<Prelude, PrimitivePreludeBuildError>,
    symbols: &Interner,
) -> Result<(InterfaceUsesInput, Vec<Diagnostic>), InterfaceUsesQueryError> {
    let descriptors = interface_generation_order(
        &prepared.module_tree,
        scopes
            .top_level_owners
            .iter()
            .copied()
            .filter(|descriptor| descriptor.kind == TopLevelOwnerKind::Interface)
            .collect(),
        symbols,
    )
    .map_err(InterfaceUsesQueryError::Collection)?;
    let interface_type_members = local_interface_type_member_owners(scopes, &descriptors);
    let indexed_scopes = std::iter::once(scopes).chain(
        prepared
            .foreign_packages
            .iter()
            .map(|package| package.scopes.as_ref()),
    );
    let module_members = ModuleMemberIndex::new_with_wit_views(
        indexed_scopes,
        prepared
            .foreign_wit_views
            .iter()
            .map(|foreign| foreign.view.as_ref()),
        symbols,
    );
    let source_graph_packages = std::iter::once(prepared.module_tree.package)
        .chain(
            prepared
                .foreign_packages
                .iter()
                .map(|package| package.module_tree.package),
        )
        .collect::<HashSet<_>>();
    let interface_boundary_types =
        interface_boundary_type_definitions(&interface_type_members, scopes, prepared);
    let mut ineligible_interfaces = HashSet::new();
    let boundary_use_alias_names = interface_boundary_use_alias_names(
        &interface_type_members,
        &interface_boundary_types,
        &descriptors,
        scopes,
        prepared,
        symbols,
        &mut ineligible_interfaces,
    )?;
    let mut module_inputs =
        HashMap::<FileId, CurrentOwnerInputs>::with_capacity(prepared.modules.len());
    for input in &prepared.modules {
        let file = input.filtered.item_tree.file;
        if module_inputs
            .insert(file, current_owner_inputs_from_prepared(input))
            .is_some()
        {
            return Err(InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::ModuleTreeMismatch,
            ));
        }
    }
    if module_inputs.len() != prepared.module_tree.modules.len() {
        return Err(InterfaceUsesQueryError::Collection(
            OwnerControlFlowResolutionError::ModuleTreeMismatch,
        ));
    }
    let alias_arena = HirArena::new();
    let local_boundary_aliases = collect_local_boundary_aliases(
        prepared,
        scopes,
        &module_inputs,
        &module_members,
        &source_graph_packages,
        primitive_prelude,
        symbols,
        &alias_arena,
        &interface_type_members,
    )?;

    let mut package_identity = None;
    let mut references = Vec::<InterfaceUseReference>::new();
    let mut interface_check_diagnostics = Vec::new();
    let mut collision_inputs = Vec::with_capacity(descriptors.len());
    let mut poisoned_owners = HashSet::<OwnerId>::new();
    let mut owner_dependencies = HashMap::<OwnerId, HashSet<DefId>>::new();
    let mut resource_method_parents = HashMap::<OwnerId, OwnerId>::new();
    let mut local_type_definitions_by_owner = HashMap::<OwnerId, DefId>::new();
    let interface_ids = descriptors
        .iter()
        .map(|descriptor| descriptor.owner.def_id)
        .collect::<Vec<_>>();
    let owner_index = index_top_level_owner_descriptors_in_scopes(scopes);

    for descriptor in descriptors {
        let inputs = module_inputs.get(&descriptor.file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::ModuleTreeMismatch)
        })?;
        package_identity = package_identity.or_else(|| inputs.source_ref.package.clone());
        let parent_descriptor = super::find_top_level_owner_descriptor(
            &owner_index,
            descriptor.owner,
            descriptor.file,
            descriptor.item_id,
            TopLevelOwnerKind::Interface,
        )
        .map_err(InterfaceUsesQueryError::Collection)?;
        if inputs.retained_item_kinds.get(&descriptor.item_id) != Some(&ItemTreeKind::Interface) {
            return Err(InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::DescriptorNotInCurrentTarget {
                    owner: descriptor.owner,
                    file: descriptor.file,
                },
            ));
        }
        let parent_entry = inputs
            .filtered_item_tree
            .items
            .iter()
            .find(|entry| {
                entry.ast_id == descriptor.item_id && entry.kind == ItemTreeKind::Interface
            })
            .ok_or_else(|| {
                InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::DescriptorNotInCurrentTarget {
                        owner: descriptor.owner,
                        file: descriptor.file,
                    },
                )
            })?;
        let member_entries = parent_entry
            .children
            .iter()
            .filter(|entry| interface_member_kind(entry.kind).is_some())
            .collect::<Vec<_>>();
        let def_paths = &prepared.def_paths;
        let expected_parent_path = def_paths
            .resolve_def_id(descriptor.owner.def_id.to_def_id())
            .map_err(OwnerControlFlowResolutionError::DefPathInterner)
            .map_err(InterfaceUsesQueryError::Collection)?
            .ok_or_else(|| {
                InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::MissingDefPath(descriptor.owner),
                )
            })?;
        let arena = HirArena::new();
        let context = sai_hir::lower::HirLoweringContext::new(
            &inputs.ast_ids,
            &prepared.snapshot,
            &inputs.root,
            &arena,
            symbols,
            def_paths,
        );
        let (parent_hir, _) = lower_top_level_owner(
            parent_descriptor,
            &expected_parent_path,
            &inputs.filtered_item_tree,
            &inputs.retained_item_kinds,
            &context,
        )
        .map_err(InterfaceUsesQueryError::Collection)?;
        let parent_members = match parent_hir.node {
            OwnerNode::Item(item) => match item.kind {
                ItemKind::Interface { items } => items,
                _ => {
                    return Err(InterfaceUsesQueryError::Collection(
                        OwnerControlFlowResolutionError::TopLevelOwnerKindMismatch {
                            owner: descriptor.owner,
                            file: descriptor.file,
                            descriptor: TopLevelOwnerKind::Interface,
                            current: ItemTreeKind::Interface,
                        },
                    ));
                }
            },
            _ => {
                return Err(InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::TopLevelOwnerDescriptorMismatch {
                        owner: descriptor.owner,
                        file: descriptor.file,
                    },
                ));
            }
        };
        if member_entries.len() != parent_members.len() {
            return Err(InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::Lowering(
                    sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                ),
            ));
        }
        let module_scope = module_scope_for_scopes(scopes, &prepared.module_tree, descriptor.file)
            .map_err(InterfaceUsesQueryError::Collection)?;
        let interface_module_scope =
            module_scope_with_interface_members(module_scope, scopes, descriptor.owner)
                .map_err(InterfaceUsesQueryError::Collection)?;
        let mut owner_references = Vec::new();
        let mut interface_item_names = Vec::with_capacity(member_entries.len());
        let mut generated_use_aliases = Vec::<GeneratedUseAlias>::new();
        for (entry, member_owner) in member_entries
            .into_iter()
            .zip(parent_members.iter().copied())
        {
            let expected_member_path = def_paths
                .resolve_def_id(member_owner.def_id.to_def_id())
                .map_err(OwnerControlFlowResolutionError::DefPathInterner)
                .map_err(InterfaceUsesQueryError::Collection)?
                .ok_or_else(|| {
                    InterfaceUsesQueryError::Collection(
                        OwnerControlFlowResolutionError::MissingDefPath(member_owner),
                    )
                })?;
            let (member_hir, _) = lower_interface_item_owner(
                member_owner,
                &expected_member_path,
                &parent_hir,
                descriptor.item_id,
                entry.ast_id,
                &inputs.filtered_item_tree,
                &context,
            )
            .map_err(OwnerControlFlowResolutionError::Lowering)
            .map_err(InterfaceUsesQueryError::Collection)?;
            let flags_marker_accepted = inputs.accepted_flags_attributes.contains(&entry.ast_id);
            let member_issues =
                collect_interface_item_issues(&member_hir, flags_marker_accepted, symbols)?;
            if !member_issues.is_empty() {
                poisoned_owners.insert(member_owner);
            }
            append_interface_item_diagnostics(
                member_issues,
                &inputs.source_ref,
                &inputs.source,
                &prepared.snapshot,
                &mut interface_check_diagnostics,
            )?;
            let kind = interface_member_kind(entry.kind).ok_or_else(|| {
                InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::Lowering(
                    sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                ))
            })?;
            if matches!(
                kind,
                super::InterfaceMemberKind::Struct
                    | super::InterfaceMemberKind::Alias
                    | super::InterfaceMemberKind::Refined
                    | super::InterfaceMemberKind::Enum
                    | super::InterfaceMemberKind::Resource
            ) {
                local_type_definitions_by_owner
                    .insert(member_owner, member_owner.def_id.to_def_id());
            }
            let OwnerNode::InterfaceItem(item) = member_hir.node else {
                return Err(InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::Lowering(
                        sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                    ),
                ));
            };
            if let Some(definition) = wit_name_definition(item.name, member_owner, symbols)? {
                interface_item_names.push(definition);
            }
            let resolution = collect_owner_references(
                &mut owner_references,
                descriptor.owner.def_id,
                member_owner,
                &member_hir,
                (kind == super::InterfaceMemberKind::Refined)
                    .then(|| super::item_tree_entry_generic_parameters(entry))
                    .unwrap_or_default(),
                &interface_module_scope,
                prepared.module_tree.package,
                &module_members,
                &source_graph_packages,
                primitive_prelude,
                &interface_type_members,
                inputs,
            )?;
            if append_interface_boundary_diagnostics(
                &member_hir,
                &resolution,
                &interface_boundary_types,
                &boundary_use_alias_names,
                &local_boundary_aliases,
                descriptor.owner.def_id,
                &interface_type_members,
                &mut owner_references,
                &module_inputs,
                &inputs.source_ref,
                &inputs.source,
                &prepared.snapshot,
                &mut generated_use_aliases,
                &mut owner_dependencies,
                &mut interface_check_diagnostics,
            )? {
                poisoned_owners.insert(member_owner);
            }
            if kind != super::InterfaceMemberKind::Resource {
                continue;
            }
            let resource_members = match member_hir.node {
                OwnerNode::InterfaceItem(item) => match item.kind {
                    InterfaceItemKind::Resource { members } => members,
                    _ => {
                        return Err(InterfaceUsesQueryError::Collection(
                            OwnerControlFlowResolutionError::Lowering(
                                sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                            ),
                        ));
                    }
                },
                _ => {
                    return Err(InterfaceUsesQueryError::Collection(
                        OwnerControlFlowResolutionError::Lowering(
                            sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                        ),
                    ));
                }
            };
            let method_entries = entry
                .children
                .iter()
                .filter(|child| child.kind == ItemTreeKind::AssocFn)
                .collect::<Vec<_>>();
            if method_entries.len() != resource_members.len() {
                return Err(InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::Lowering(
                        sai_hir::lower::HirLoweringError::ItemTreeEntryMismatch,
                    ),
                ));
            }
            let mut resource_member_names = Vec::with_capacity(method_entries.len());
            for (method_entry, method_owner) in method_entries
                .into_iter()
                .zip(resource_members.iter().copied())
            {
                resource_method_parents.insert(method_owner, member_owner);
                let expected_method_path = def_paths
                    .resolve_def_id(method_owner.def_id.to_def_id())
                    .map_err(OwnerControlFlowResolutionError::DefPathInterner)
                    .map_err(InterfaceUsesQueryError::Collection)?
                    .ok_or_else(|| {
                        InterfaceUsesQueryError::Collection(
                            OwnerControlFlowResolutionError::MissingDefPath(method_owner),
                        )
                    })?;
                let (method_hir, _) = lower_interface_resource_member_owner(
                    method_owner,
                    &expected_method_path,
                    &parent_hir,
                    &member_hir,
                    InterfaceResourceMemberIds {
                        interface: descriptor.item_id,
                        resource: entry.ast_id,
                        member: method_entry.ast_id,
                    },
                    &inputs.filtered_item_tree,
                    &context,
                )
                .map_err(OwnerControlFlowResolutionError::Lowering)
                .map_err(InterfaceUsesQueryError::Collection)?;
                let method_issues = collect_interface_item_issues(&method_hir, false, symbols)?;
                if !method_issues.is_empty() {
                    poisoned_owners.insert(method_owner);
                }
                append_interface_item_diagnostics(
                    method_issues,
                    &inputs.source_ref,
                    &inputs.source,
                    &prepared.snapshot,
                    &mut interface_check_diagnostics,
                )?;
                if let OwnerNode::InterfaceItem(item) = method_hir.node
                    && let InterfaceItemKind::ResourceMember { role, .. } = item.kind
                    && role != MemberRole::Constructor
                    && let Some(definition) = wit_name_definition(item.name, method_owner, symbols)?
                {
                    resource_member_names.push(definition);
                }
                let resolution = collect_owner_references(
                    &mut owner_references,
                    descriptor.owner.def_id,
                    method_owner,
                    &method_hir,
                    HashSet::new(),
                    &interface_module_scope,
                    prepared.module_tree.package,
                    &module_members,
                    &source_graph_packages,
                    primitive_prelude,
                    &interface_type_members,
                    inputs,
                )?;
                if append_interface_boundary_diagnostics(
                    &method_hir,
                    &resolution,
                    &interface_boundary_types,
                    &boundary_use_alias_names,
                    &local_boundary_aliases,
                    descriptor.owner.def_id,
                    &interface_type_members,
                    &mut owner_references,
                    &module_inputs,
                    &inputs.source_ref,
                    &inputs.source,
                    &prepared.snapshot,
                    &mut generated_use_aliases,
                    &mut owner_dependencies,
                    &mut interface_check_diagnostics,
                )? {
                    poisoned_owners.insert(method_owner);
                }
            }
            if append_wit_name_collision_diagnostics(
                resource_member_names,
                &inputs.source_ref,
                &inputs.source,
                &prepared.snapshot,
                &mut interface_check_diagnostics,
            )? {
                poisoned_owners.insert(member_owner);
            }
        }
        collision_inputs.push(InterfaceNameCollisionInput {
            interface: descriptor.owner.def_id,
            file: descriptor.file,
            generated_use_aliases,
            local_definitions: interface_item_names,
        });
        // Direct path resolution and alias expansion are collected in
        // separate passes for each owner. Re-establish source order before
        // graph deduplication so the retained edge (and its diagnostic span)
        // points to the first WIT reference written by the user.
        owner_references.sort_by_key(|reference| reference.location.span.range.start());
        references.extend(owner_references);
    }

    let package = package_identity.unwrap_or_else(|| {
        package_key
            .as_str()
            .split_once('#')
            .map_or(package_key.as_str(), |(identity, _)| identity)
            .to_owned()
    });
    for reference in &mut references {
        reference.location.file.snapshot = None;
    }
    let input = InterfaceUsesInput {
        package,
        interfaces: interface_ids,
        references,
    };
    append_generation_eligible_interface_name_collisions(
        input.clone(),
        collision_inputs,
        &interface_type_members,
        &local_type_definitions_by_owner,
        &ineligible_interfaces,
        &poisoned_owners,
        &owner_dependencies,
        &resource_method_parents,
        &module_inputs,
        &prepared.snapshot,
        &mut interface_check_diagnostics,
    )?;
    Ok((input, interface_check_diagnostics))
}

#[allow(clippy::too_many_arguments)]
fn append_generation_eligible_interface_name_collisions(
    input: InterfaceUsesInput,
    collision_inputs: Vec<InterfaceNameCollisionInput>,
    local_interface_type_members: &HashMap<DefId, LocalDefId>,
    type_definitions_by_owner: &HashMap<OwnerId, DefId>,
    ineligible_interfaces: &HashSet<LocalDefId>,
    directly_poisoned_owners: &HashSet<OwnerId>,
    owner_dependencies: &HashMap<OwnerId, HashSet<DefId>>,
    resource_method_parents: &HashMap<OwnerId, OwnerId>,
    module_inputs: &HashMap<FileId, CurrentOwnerInputs>,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), InterfaceUsesQueryError> {
    let cyclic_interfaces = interface_uses(input).cyclic;
    let mut omitted_owners = directly_poisoned_owners.clone();
    let mut omitted_local_types = local_interface_type_members
        .iter()
        .filter_map(|(definition, owner)| {
            (ineligible_interfaces.contains(owner) || cyclic_interfaces.contains(owner))
                .then_some(*definition)
        })
        .collect::<HashSet<_>>();
    for owner in directly_poisoned_owners {
        if let Some(definition) = type_definitions_by_owner.get(owner) {
            omitted_local_types.insert(*definition);
        }
    }

    let mut local_name_collision_owners = HashSet::new();
    let mut local_name_collisions = Vec::<(FileId, Vec<WitNameCollision>)>::new();
    loop {
        let mut changed = propagate_omitted_interface_owners(
            &mut omitted_owners,
            &mut omitted_local_types,
            owner_dependencies,
            resource_method_parents,
            type_definitions_by_owner,
        );

        for collision_input in &collision_inputs {
            if ineligible_interfaces.contains(&collision_input.interface)
                || cyclic_interfaces.contains(&collision_input.interface)
            {
                continue;
            }
            let eligible_local_definitions = collision_input
                .local_definitions
                .iter()
                .filter(|definition| !omitted_owners.contains(&definition.owner))
                .cloned()
                .collect::<Vec<_>>();
            let collisions = wit_name_collisions(eligible_local_definitions);
            let new_collisions = collisions
                .into_iter()
                .filter(|collision| local_name_collision_owners.insert(collision.later_owner))
                .collect::<Vec<_>>();
            if !new_collisions.is_empty() {
                for collision in &new_collisions {
                    omitted_owners.insert(collision.later_owner);
                    if let Some(definition) = type_definitions_by_owner.get(&collision.later_owner)
                    {
                        omitted_local_types.insert(*definition);
                    }
                }
                local_name_collisions.push((collision_input.file, new_collisions));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for (file, collisions) in local_name_collisions {
        let inputs = module_inputs.get(&file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::ModuleTreeMismatch)
        })?;
        append_wit_name_collisions(
            collisions,
            &inputs.source_ref,
            &inputs.source,
            snapshot,
            diagnostics,
        )?;
    }

    let mut generated_name_collisions = Vec::<(FileId, WitNameCollision)>::new();
    loop {
        let next_collision = collision_inputs.iter().find_map(|collision_input| {
            if ineligible_interfaces.contains(&collision_input.interface)
                || cyclic_interfaces.contains(&collision_input.interface)
            {
                return None;
            }
            let generated_use_aliases = collision_input
                .generated_use_aliases
                .iter()
                .filter(|alias| {
                    !omitted_owners.contains(&alias.owner)
                        && !omitted_local_types.contains(&alias.definition)
                })
                .cloned()
                .collect();
            let local_definitions = collision_input
                .local_definitions
                .iter()
                .filter(|definition| !omitted_owners.contains(&definition.owner))
                .cloned()
                .collect::<Vec<_>>();
            collect_interface_name_collisions(generated_use_aliases, local_definitions)
                .into_iter()
                .find(|collision| !omitted_owners.contains(&collision.later_owner))
                .map(|collision| (collision_input.file, collision))
        });
        let Some((file, collision)) = next_collision else {
            break;
        };

        omitted_owners.insert(collision.later_owner);
        if let Some(definition) = type_definitions_by_owner.get(&collision.later_owner) {
            omitted_local_types.insert(*definition);
        }
        // Collision poisoning is monotonic and follows emitted-name order.
        // Once reported, a collision remains a valid finding from the
        // candidate scope in which it was discovered, even if poisoning its
        // later owner subsequently removes an earlier alias that supported a
        // different finding. Recomputations suppress only later cascades.
        generated_name_collisions.push((file, collision));
        propagate_omitted_interface_owners(
            &mut omitted_owners,
            &mut omitted_local_types,
            owner_dependencies,
            resource_method_parents,
            type_definitions_by_owner,
        );
    }

    for (file, collision) in generated_name_collisions {
        let inputs = module_inputs.get(&file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::ModuleTreeMismatch)
        })?;
        append_wit_name_collisions(
            [collision],
            &inputs.source_ref,
            &inputs.source,
            snapshot,
            diagnostics,
        )?;
    }
    Ok(())
}

fn propagate_omitted_interface_owners(
    omitted_owners: &mut HashSet<OwnerId>,
    omitted_local_types: &mut HashSet<DefId>,
    owner_dependencies: &HashMap<OwnerId, HashSet<DefId>>,
    resource_method_parents: &HashMap<OwnerId, OwnerId>,
    type_definitions_by_owner: &HashMap<OwnerId, DefId>,
) -> bool {
    let mut any_changed = false;
    loop {
        let mut changed = false;
        for (owner, dependencies) in owner_dependencies {
            if dependencies
                .iter()
                .any(|definition| omitted_local_types.contains(definition))
                && omitted_owners.insert(*owner)
            {
                changed = true;
            }
        }
        for (method, resource) in resource_method_parents {
            if omitted_owners.contains(method) && omitted_owners.insert(*resource) {
                changed = true;
            }
        }
        for (owner, definition) in type_definitions_by_owner {
            if omitted_owners.contains(owner) && omitted_local_types.insert(*definition) {
                changed = true;
            }
        }
        if !changed {
            return any_changed;
        }
        any_changed = true;
    }
}

fn collect_owner_references(
    references: &mut Vec<InterfaceUseReference>,
    interface: LocalDefId,
    owner: OwnerId,
    hir_owner: &HirOwner<'_>,
    additional_generic_parameters: HashSet<Symbol>,
    module_scope: &super::Phase1ModuleScopes,
    requesting_package: sai_hir::PackageId,
    module_members: &ModuleMemberIndex<'_>,
    source_graph_packages: &HashSet<sai_hir::PackageId>,
    primitive_prelude: &Result<Prelude, PrimitivePreludeBuildError>,
    interface_type_members: &HashMap<sai_hir::DefId, LocalDefId>,
    inputs: &CurrentOwnerInputs,
) -> Result<TypeOwnerPathResolution, InterfaceUsesQueryError> {
    let candidates = owner_test_only_path_candidates(hir_owner);
    let interface_use_candidates = interface_use_path_candidates(hir_owner, &candidates);
    let mut generic_resolutions = owner_type_generic_resolutions(hir_owner);
    for parameter in additional_generic_parameters {
        generic_resolutions.entry(parameter).or_insert(Res::Err);
    }
    let resolution = resolve_partial_type_owner_paths(
        owner,
        &candidates,
        &generic_resolutions,
        primitive_prelude,
        module_scope,
        requesting_package,
        Some(module_members),
        Some(source_graph_packages),
    )
    .map_err(InterfaceUsesQueryError::Collection)?;
    append_interface_use_references(
        references,
        interface,
        &interface_use_candidates,
        &resolution,
        interface_type_members,
        inputs,
    )
    .map_err(InterfaceUsesQueryError::Collection)?;
    Ok(resolution)
}

fn interface_use_path_candidates<'hir>(
    owner: &HirOwner<'hir>,
    candidates: &[super::OwnerPathHead<'hir>],
) -> Vec<super::OwnerPathHead<'hir>> {
    let roots = interface_use_type_roots(owner);
    type_path_candidates_in_roots(candidates, &roots)
}

fn type_path_candidates_in_roots<'hir>(
    candidates: &[super::OwnerPathHead<'hir>],
    roots: &[&'hir Ty<'hir>],
) -> Vec<super::OwnerPathHead<'hir>> {
    let mut path_ids = HashSet::<HirId>::new();
    for root in roots {
        collect_type_path_ids(root, &mut path_ids);
    }
    candidates
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.kind == super::OwnerPathKind::Type && path_ids.contains(&candidate.hir_id)
        })
        .collect()
}

fn collect_type_path_ids(ty: &Ty<'_>, path_ids: &mut HashSet<HirId>) {
    match ty.kind {
        TyKind::Path(path) => {
            path_ids.insert(path.hir_id);
            for segment in path.segments {
                let Some(arguments) = segment.args else {
                    continue;
                };
                for argument in arguments.args {
                    match *argument {
                        GenericArg::Type(ty) | GenericArg::Binding { ty, .. } => {
                            collect_type_path_ids(ty, path_ids);
                        }
                        GenericArg::Const(_) => {}
                    }
                }
            }
        }
        TyKind::Borrow(_, inner) => collect_type_path_ids(inner, path_ids),
        TyKind::Tuple(elements) => {
            for element in elements {
                collect_type_path_ids(element, path_ids);
            }
        }
        TyKind::FnPtr { params, ret, .. } => {
            for parameter in params {
                collect_type_path_ids(parameter, path_ids);
            }
            if let Some(ret) = ret {
                collect_type_path_ids(ret, path_ids);
            }
        }
        TyKind::Infer | TyKind::Err => {}
    }
}

fn interface_use_type_roots<'hir>(owner: &HirOwner<'hir>) -> Vec<&'hir Ty<'hir>> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    match item.kind {
        InterfaceItemKind::Struct { fields, .. } => {
            roots.extend(fields.iter().map(|field| field.ty));
        }
        InterfaceItemKind::Alias { ty, .. } | InterfaceItemKind::Refined { base: ty, .. } => {
            roots.push(ty);
        }
        InterfaceItemKind::Enum { variants, .. } => {
            for variant in variants {
                match variant.shape {
                    VariantShape::Unit => {}
                    VariantShape::Tuple(types) => roots.extend(types.iter()),
                    VariantShape::Record(fields) => {
                        roots.extend(fields.iter().map(|field| field.ty));
                    }
                }
            }
        }
        InterfaceItemKind::Fn { sig, .. } | InterfaceItemKind::ResourceMember { sig, .. } => {
            roots.extend(
                sig.params
                    .iter()
                    .filter(|param| !param.is_self)
                    .map(|param| param.ty),
            );
            roots.extend(sig.ret);
        }
        InterfaceItemKind::Resource { .. } => {}
    }
    roots
}

fn collect_local_boundary_aliases<'hir>(
    prepared: &PreparedPhase1PackageScopes,
    scopes: &Phase1PackageScopes,
    module_inputs: &'hir HashMap<FileId, CurrentOwnerInputs>,
    module_members: &ModuleMemberIndex<'_>,
    source_graph_packages: &HashSet<sai_hir::PackageId>,
    primitive_prelude: &Result<Prelude, PrimitivePreludeBuildError>,
    symbols: &Interner,
    arena: &'hir HirArena<'hir>,
    interface_type_members: &HashMap<DefId, LocalDefId>,
) -> Result<LocalBoundaryAliases<'hir>, InterfaceUsesQueryError> {
    let mut aliases = HashMap::new();
    let mut path_resolutions = HashMap::new();
    let mut alias_references = HashMap::new();
    for descriptor in scopes
        .top_level_owners
        .iter()
        .copied()
        .filter(|descriptor| descriptor.kind == TopLevelOwnerKind::Alias)
    {
        let Some(inputs) = module_inputs.get(&descriptor.file) else {
            continue;
        };
        let expected_path = prepared
            .def_paths
            .resolve_def_id(descriptor.owner.def_id.to_def_id())
            .map_err(OwnerControlFlowResolutionError::DefPathInterner)
            .map_err(InterfaceUsesQueryError::Collection)?
            .ok_or_else(|| OwnerControlFlowResolutionError::MissingDefPath(descriptor.owner))
            .map_err(InterfaceUsesQueryError::Collection)?;
        let context = HirLoweringContext::new(
            &inputs.ast_ids,
            &prepared.snapshot,
            &inputs.root,
            arena,
            symbols,
            &prepared.def_paths,
        );
        let Ok((hir_owner, _)) = lower_top_level_owner(
            descriptor,
            &expected_path,
            &inputs.filtered_item_tree,
            &inputs.retained_item_kinds,
            &context,
        ) else {
            // Existing lowering diagnostics own malformed or unsupported alias
            // declarations. They cannot provide a sound expansion target.
            continue;
        };
        let OwnerNode::Item(item) = hir_owner.node else {
            continue;
        };
        let ItemKind::Alias { generics, ty } = item.kind else {
            continue;
        };
        let candidates = owner_test_only_path_candidates(&hir_owner);
        let candidates = type_path_candidates_in_roots(&candidates, &[ty]);
        let mut candidates = candidates;
        candidates.sort_by_key(|candidate| candidate.span.range.start());
        let generic_resolutions = owner_type_generic_resolutions(&hir_owner);
        let module_scope = module_scope_for_scopes(scopes, &prepared.module_tree, descriptor.file)
            .map_err(InterfaceUsesQueryError::Collection)?;
        let resolution = resolve_partial_type_owner_paths(
            descriptor.owner,
            &candidates,
            &generic_resolutions,
            primitive_prelude,
            module_scope,
            prepared.module_tree.package,
            Some(module_members),
            Some(source_graph_packages),
        )
        .map_err(InterfaceUsesQueryError::Collection)?;
        let has_generic_params = !generics.params.is_empty();
        let references = candidates
            .iter()
            .filter_map(|candidate| {
                let PathRes {
                    base: Res::Def(kind, target),
                    ..
                } = resolution.paths().get(&candidate.hir_id).copied()?
                else {
                    return None;
                };
                if interface_type_members.contains_key(&target) {
                    Some(LocalAliasReference::InterfaceType(target))
                } else if kind == DefKind::Alias {
                    Some(LocalAliasReference::ModuleAlias(target))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        aliases.insert(
            descriptor.owner.def_id.to_def_id(),
            BoundaryAlias {
                owner: descriptor.owner,
                ty,
                has_generic_params,
            },
        );
        alias_references.insert(descriptor.owner.def_id.to_def_id(), references);
        path_resolutions.extend(
            resolution
                .paths()
                .iter()
                .map(|(hir_id, resolution)| (*hir_id, *resolution)),
        );
    }
    let mut expanded_references = HashMap::with_capacity(aliases.len());
    for alias in aliases.keys().copied() {
        let mut references = Vec::new();
        expand_local_alias_interface_references(
            alias,
            &alias_references,
            &aliases,
            &mut HashSet::new(),
            &mut references,
        );
        expanded_references.insert(alias, references);
    }
    Ok(LocalBoundaryAliases {
        definitions: aliases,
        path_resolutions,
        interface_references: expanded_references,
    })
}

fn expand_local_alias_interface_references(
    alias: DefId,
    alias_references: &HashMap<DefId, Vec<LocalAliasReference>>,
    aliases: &HashMap<DefId, BoundaryAlias<'_>>,
    visiting: &mut HashSet<DefId>,
    output: &mut Vec<DefId>,
) {
    if !aliases.contains_key(&alias) || !visiting.insert(alias) {
        return;
    }
    for reference in alias_references.get(&alias).into_iter().flatten() {
        match reference {
            LocalAliasReference::InterfaceType(definition) => output.push(*definition),
            LocalAliasReference::ModuleAlias(nested) => expand_local_alias_interface_references(
                *nested,
                alias_references,
                aliases,
                visiting,
                output,
            ),
        }
    }
    visiting.remove(&alias);
}

fn append_local_alias_interface_use_references(
    owner: &HirOwner<'_>,
    resolution: &TypeOwnerPathResolution,
    local_boundary_aliases: &LocalBoundaryAliases<'_>,
    interface: LocalDefId,
    interface_type_members: &HashMap<DefId, LocalDefId>,
    references: &mut Vec<InterfaceUseReference>,
    module_inputs: &HashMap<FileId, CurrentOwnerInputs>,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
) -> Result<(), InterfaceUsesQueryError> {
    let owner_id = owner.id;
    let candidates = owner_test_only_path_candidates(owner);
    let mut candidates = interface_use_path_candidates(owner, &candidates);
    candidates.sort_by_key(|candidate| candidate.span.range.start());
    for candidate in candidates {
        let Some(PathRes {
            base: Res::Def(DefKind::Alias, alias),
            ..
        }) = resolution.paths().get(&candidate.hir_id)
        else {
            continue;
        };
        if interface_type_members.contains_key(alias) {
            continue;
        }
        let Some(hidden_references) = local_boundary_aliases.interface_references.get(alias) else {
            continue;
        };
        let (reference_source_ref, reference_source) = module_inputs
            .get(&candidate.span.file)
            .map(|inputs| (inputs.source_ref.clone(), inputs.source.as_ref()))
            .unwrap_or_else(|| (source_ref.clone(), source));
        let location = Location::from_decoded_source(
            reference_source_ref,
            sai_diagnostics::Span::new(candidate.span.file, candidate.span.range),
            reference_source,
        )
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::InvalidSourceLocation(owner_id),
            )
        })?;
        for hidden_reference in hidden_references {
            let Some(&to) = interface_type_members.get(hidden_reference) else {
                continue;
            };
            references.push(InterfaceUseReference {
                from: interface,
                to,
                location: location.clone(),
            });
        }
    }
    Ok(())
}

fn append_interface_boundary_diagnostics(
    owner: &HirOwner<'_>,
    resolution: &TypeOwnerPathResolution,
    interface_boundary_types: &HashMap<DefId, BoundaryDefinition>,
    boundary_use_alias_names: &HashMap<DefId, BoundaryUseAliasName>,
    local_boundary_aliases: &LocalBoundaryAliases<'_>,
    interface: LocalDefId,
    interface_type_members: &HashMap<DefId, LocalDefId>,
    references: &mut Vec<InterfaceUseReference>,
    module_inputs: &HashMap<FileId, CurrentOwnerInputs>,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
    snapshot: &WorkspaceSnapshot,
    generated_use_aliases: &mut Vec<GeneratedUseAlias>,
    owner_dependencies: &mut HashMap<OwnerId, HashSet<DefId>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<bool, InterfaceUsesQueryError> {
    let owner_id = owner.id;
    let self_resource = match owner.node {
        OwnerNode::InterfaceItem(item) => match item.kind {
            InterfaceItemKind::ResourceMember { resource, .. } => Some(resource.to_def_id()),
            _ => None,
        },
        _ => None,
    };
    let mut path_resolutions = resolution.paths().clone();
    path_resolutions.extend(
        local_boundary_aliases
            .path_resolutions
            .iter()
            .map(|(hir_id, resolution)| (*hir_id, *resolution)),
    );
    append_local_alias_interface_use_references(
        owner,
        resolution,
        local_boundary_aliases,
        interface,
        interface_type_members,
        references,
        module_inputs,
        source_ref,
        source,
    )?;
    let mut definitions = HashMap::new();
    for path_resolution in path_resolutions.values() {
        let Res::Def(kind, def_id) = path_resolution.base else {
            continue;
        };
        let definition = if let Some(&definition) = interface_boundary_types.get(&def_id) {
            Some(definition)
        } else {
            match kind {
                DefKind::Alias => Some(BoundaryDefinition::ModuleAlias),
                DefKind::Struct
                | DefKind::Enum
                | DefKind::Distinct
                | DefKind::Refined
                | DefKind::Resource => Some(BoundaryDefinition::OutsideInterfaceNominal),
                _ => None,
            }
        };
        if let Some(definition) = definition {
            definitions.insert(def_id, definition);
        }
    }
    let canonical_types = HashMap::new();
    let boundary_context = BoundaryTypeContext {
        path_resolutions: &path_resolutions,
        canonical_types: &canonical_types,
        definitions: &definitions,
        resource_containment: None,
        aliases: &local_boundary_aliases.definitions,
        self_resource,
    };
    let owner_path_candidates = owner_test_only_path_candidates(owner);
    let mut has_incompatible_boundary = false;

    // The receiver is not emitted as a boundary parameter, but its type must
    // still be the enclosing resource. The syntax-only interface check owns
    // non-borrow receivers; resolve borrowed receivers here so `Self`, a
    // resource path, and a known module alias compare by definition identity.
    let resource_receiver = match owner.node {
        OwnerNode::InterfaceItem(item) => match item.kind {
            InterfaceItemKind::ResourceMember {
                resource,
                role: MemberRole::Method,
                sig,
                ..
            } => sig
                .params
                .first()
                .filter(|parameter| parameter.is_self)
                .map(|parameter| (resource.to_def_id(), parameter.ty)),
            _ => None,
        },
        _ => None,
    };
    if let Some((expected_resource, receiver_ty)) = resource_receiver
        && matches!(receiver_ty.kind, TyKind::Borrow(_, _))
    {
        let invalid_receiver = match boundary_type_with_references(
            &boundary_context,
            owner_id,
            receiver_ty,
            Position::Param,
        )
        .outcome
        {
            BoundaryTypeOutcome::Mapped(BoundaryType::Borrow(resource)) => {
                resource != expected_resource
            }
            BoundaryTypeOutcome::Mapped(_) | BoundaryTypeOutcome::Incompatible(_) => true,
            BoundaryTypeOutcome::Suppressed | BoundaryTypeOutcome::Deferred => false,
        };
        if invalid_receiver {
            let issue = InterfaceCheckIssue {
                owner: owner_id,
                kind: InterfaceCheckIssueKind::InvalidResourceReceiver,
                span: receiver_ty.span,
                name: None,
                suggestion: None,
                related_span: None,
            };
            append_interface_item_diagnostics(
                vec![issue],
                source_ref,
                source,
                snapshot,
                diagnostics,
            )?;
            has_incompatible_boundary = true;
        }
    }

    for (ty, position) in interface_boundary_roots(owner) {
        let conversion = boundary_type_with_references(&boundary_context, owner_id, ty, position);
        let incompatible = match conversion.outcome {
            BoundaryTypeOutcome::Mapped(mapped) => {
                let mut mapped_definitions = HashSet::new();
                collect_boundary_type_definitions(&mapped, &mut mapped_definitions);
                owner_dependencies
                    .entry(owner_id)
                    .or_default()
                    .extend(mapped_definitions.iter().copied());
                collect_generated_use_aliases(
                    owner_id,
                    interface,
                    &mapped_definitions,
                    &conversion.references,
                    ty,
                    &owner_path_candidates,
                    resolution,
                    boundary_use_alias_names,
                    generated_use_aliases,
                );
                continue;
            }
            BoundaryTypeOutcome::Incompatible(incompatible) => incompatible,
            BoundaryTypeOutcome::Suppressed | BoundaryTypeOutcome::Deferred => continue,
        };
        has_incompatible_boundary = true;
        let snapshot_file = snapshot.file(incompatible.span.file).ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::MissingSnapshotFile(incompatible.span.file),
            )
        })?;
        let (diagnostic_source_ref, diagnostic_source) = module_inputs
            .get(&incompatible.span.file)
            .map(|inputs| (inputs.source_ref.clone(), inputs.source.as_ref()))
            .unwrap_or_else(|| (source_ref.clone(), source));
        let location = Location::from_decoded_source(
            diagnostic_source_ref,
            sai_diagnostics::Span::new(incompatible.span.file, incompatible.span.range),
            diagnostic_source,
        )
        .ok_or_else(|| {
            InterfaceUsesQueryError::Collection(
                OwnerControlFlowResolutionError::InvalidSourceLocation(owner_id),
            )
        })?;
        let position_name = boundary_position_name(position);
        let (reason_name, reason_message) = boundary_incompatibility_message(incompatible.reason);
        let mut diagnostic = Diagnostic::new(
            sai_diagnostics::Code::WitIncompatibleType,
            Severity::Error,
            format!("{reason_message} in {position_name} position"),
            Some(location),
        );
        diagnostic.primary_label = "incompatible WIT boundary type".to_owned();
        diagnostic.package = snapshot_file.package().map(str::to_owned);
        diagnostic.data.insert(
            "wit_position".to_owned(),
            Value::String(position_name.to_owned()),
        );
        diagnostic.data.insert(
            "wit_incompatibility".to_owned(),
            Value::String(reason_name.to_owned()),
        );
        if let Some(help) = boundary_incompatibility_help(incompatible.reason) {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Help,
                    message: help.to_owned(),
                    location: None,
                })
                .expect("unbound diagnostics accept help children");
        }
        diagnostics.push(diagnostic);
    }
    Ok(has_incompatible_boundary)
}

fn collect_generated_use_aliases(
    owner: OwnerId,
    current_interface: LocalDefId,
    mapped_definitions: &HashSet<DefId>,
    hidden_references: &[sai_ty::boundary::BoundaryTypeReference],
    root: &Ty<'_>,
    owner_path_candidates: &[super::OwnerPathHead<'_>],
    resolution: &TypeOwnerPathResolution,
    alias_names: &HashMap<DefId, BoundaryUseAliasName>,
    generated_use_aliases: &mut Vec<GeneratedUseAlias>,
) {
    let mut references = Vec::<(DefId, sai_hir::Span)>::new();
    let mut path_spans = HashMap::new();
    collect_type_path_spans(root, &mut path_spans);
    let mut direct_candidates = type_path_candidates_in_roots(owner_path_candidates, &[root]);
    direct_candidates.sort_by_key(|candidate| candidate.span.range.start());
    for candidate in direct_candidates {
        let Some(PathRes {
            base: Res::Def(_, definition),
            ..
        }) = resolution.paths().get(&candidate.hir_id)
        else {
            continue;
        };
        if mapped_definitions.contains(definition) {
            references.push((
                *definition,
                path_spans
                    .get(&candidate.hir_id)
                    .copied()
                    .unwrap_or(candidate.span),
            ));
        }
    }
    references.extend(
        hidden_references
            .iter()
            .filter(|reference| mapped_definitions.contains(&reference.definition))
            .map(|reference| (reference.definition, reference.span)),
    );
    references.sort_by_key(|(_, span)| span.range.start());

    for (definition, span) in references {
        let Some(alias_name) = alias_names.get(&definition) else {
            continue;
        };
        if alias_name.local_interface == Some(current_interface) {
            continue;
        }
        generated_use_aliases.push(GeneratedUseAlias {
            target: alias_name.target.clone(),
            emitted_name: alias_name.emitted_name.clone(),
            collision_name: alias_name.collision_name.clone(),
            definition,
            owner,
            span,
        });
    }
}

fn collect_boundary_type_definitions(
    boundary_type: &BoundaryType,
    definitions: &mut HashSet<DefId>,
) {
    match boundary_type {
        BoundaryType::List(inner) | BoundaryType::Option(inner) => {
            collect_boundary_type_definitions(inner, definitions)
        }
        BoundaryType::Result { ok, err } => {
            if let Some(ok) = ok {
                collect_boundary_type_definitions(ok, definitions);
            }
            if let Some(err) = err {
                collect_boundary_type_definitions(err, definitions);
            }
        }
        BoundaryType::Tuple(elements) => {
            for element in elements {
                collect_boundary_type_definitions(element, definitions);
            }
        }
        BoundaryType::Named(definition)
        | BoundaryType::Own(definition)
        | BoundaryType::Borrow(definition) => {
            definitions.insert(*definition);
        }
        BoundaryType::Unit
        | BoundaryType::Bool
        | BoundaryType::S8
        | BoundaryType::S16
        | BoundaryType::S32
        | BoundaryType::S64
        | BoundaryType::U8
        | BoundaryType::U16
        | BoundaryType::U32
        | BoundaryType::U64
        | BoundaryType::F32
        | BoundaryType::F64
        | BoundaryType::Char
        | BoundaryType::String => {}
    }
}

fn collect_type_path_spans(ty: &Ty<'_>, path_spans: &mut HashMap<HirId, sai_hir::Span>) {
    match ty.kind {
        TyKind::Path(path) => {
            path_spans.insert(path.hir_id, path.span);
            for segment in path.segments {
                let Some(arguments) = segment.args else {
                    continue;
                };
                for argument in arguments.args {
                    match *argument {
                        GenericArg::Type(ty) | GenericArg::Binding { ty, .. } => {
                            collect_type_path_spans(ty, path_spans);
                        }
                        GenericArg::Const(_) => {}
                    }
                }
            }
        }
        TyKind::Borrow(_, inner) => collect_type_path_spans(inner, path_spans),
        TyKind::Tuple(elements) => {
            for element in elements {
                collect_type_path_spans(element, path_spans);
            }
        }
        TyKind::FnPtr { params, ret, .. } => {
            for parameter in params {
                collect_type_path_spans(parameter, path_spans);
            }
            if let Some(ret) = ret {
                collect_type_path_spans(ret, path_spans);
            }
        }
        TyKind::Infer | TyKind::Err => {}
    }
}

fn interface_boundary_type_definitions(
    local_interface_type_members: &HashMap<DefId, LocalDefId>,
    scopes: &Phase1PackageScopes,
    prepared: &PreparedPhase1PackageScopes,
) -> HashMap<DefId, BoundaryDefinition> {
    let mut definitions = HashMap::new();
    for scope in scopes.interfaces.values() {
        for binding in scope.members.values() {
            let Res::Def(kind, def_id) = binding.res else {
                continue;
            };
            if !local_interface_type_members.contains_key(&def_id) {
                continue;
            }
            let definition = match kind {
                DefKind::Resource => Some(BoundaryDefinition::Resource),
                DefKind::Struct
                | DefKind::Enum
                | DefKind::Alias
                | DefKind::Distinct
                | DefKind::Refined => Some(BoundaryDefinition::InterfaceType),
                _ => None,
            };
            if let Some(definition) = definition {
                definitions.insert(def_id, definition);
            }
        }
    }
    for foreign in &prepared.foreign_wit_views {
        for interface in foreign.view.interfaces.values() {
            for member in interface.members.values() {
                let definition = match member.kind {
                    WitViewDefKind::Resource => Some(BoundaryDefinition::Resource),
                    WitViewDefKind::Struct | WitViewDefKind::Enum | WitViewDefKind::Alias => {
                        Some(BoundaryDefinition::InterfaceType)
                    }
                    WitViewDefKind::Interface
                    | WitViewDefKind::World
                    | WitViewDefKind::Fn
                    | WitViewDefKind::Field
                    | WitViewDefKind::Variant
                    | WitViewDefKind::AssocFn
                    | WitViewDefKind::WorldFn => None,
                };
                if let Some(definition) = definition {
                    definitions.insert(member.def, definition);
                }
            }
        }
    }
    definitions
}

fn interface_boundary_roots<'hir>(owner: &HirOwner<'hir>) -> Vec<(&'hir Ty<'hir>, Position)> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    match item.kind {
        InterfaceItemKind::Struct { fields, .. } => {
            roots.extend(fields.iter().map(|field| (field.ty, Position::Nested)));
        }
        InterfaceItemKind::Alias { ty, .. } => roots.push((ty, Position::Nested)),
        InterfaceItemKind::Enum { variants, .. } => {
            for variant in variants {
                if let VariantShape::Tuple([ty]) = variant.shape {
                    roots.push((ty, Position::Nested));
                }
            }
        }
        InterfaceItemKind::Fn { sig, .. } | InterfaceItemKind::ResourceMember { sig, .. } => {
            roots.extend(
                sig.params
                    .iter()
                    .filter(|param| !param.is_self)
                    .map(|param| (param.ty, Position::Param)),
            );
            roots.extend(sig.ret.map(|ty| (ty, Position::Result)));
        }
        InterfaceItemKind::Refined { .. } | InterfaceItemKind::Resource { .. } => {}
    }
    roots
}

fn boundary_position_name(position: Position) -> &'static str {
    match position {
        Position::Param => "parameter",
        Position::Result => "result",
        Position::Nested => "nested type",
    }
}

fn boundary_incompatibility_message(reason: IncompatibleReason) -> (&'static str, &'static str) {
    match reason {
        IncompatibleReason::PointerSizedInteger => (
            "pointer_sized_integer",
            "pointer-sized integers have no WIT mapping",
        ),
        IncompatibleReason::Never => ("never", "`Never` has no WIT mapping"),
        IncompatibleReason::FixedLengthArray => (
            "fixed_length_array",
            "fixed-length arrays have no WIT mapping",
        ),
        IncompatibleReason::FunctionPointer => (
            "function_pointer",
            "function pointer types have no WIT mapping",
        ),
        IncompatibleReason::TypeParameter => {
            ("type_parameter", "type parameters have no WIT mapping")
        }
        IncompatibleReason::Box => ("box", "`Box<T>` has no WIT mapping"),
        IncompatibleReason::BorrowOfNonResource => (
            "borrow_of_non_resource",
            "only resource types may be borrowed across the WIT boundary",
        ),
        IncompatibleReason::BorrowOutsideParameter => (
            "borrow_outside_parameter",
            "resource borrows are valid only in parameter position",
        ),
        IncompatibleReason::StrOutsideParameter => (
            "str_outside_parameter",
            "`Str` is valid only in parameter position",
        ),
        IncompatibleReason::SliceOutsideParameter => (
            "slice_outside_parameter",
            "`Slice<T>` is valid only in parameter position",
        ),
        IncompatibleReason::SliceContainsResource => (
            "slice_contains_resource",
            "`Slice<T>` cannot contain a resource type",
        ),
        IncompatibleReason::UnitOutsideResult => (
            "unit_outside_result",
            "`()` is valid only as a result or a `Result` arm",
        ),
        IncompatibleReason::OptionUnit => ("option_unit", "`Option<()>` has no WIT mapping"),
        IncompatibleReason::OutsideInterfaceNominal => (
            "outside_interface_nominal",
            "nominal types crossing the boundary must be declared in an interface",
        ),
        IncompatibleReason::DistinctType => ("distinct_type", "distinct types have no WIT mapping"),
        IncompatibleReason::RefinedType => ("refined_type", "refined types have no WIT mapping"),
    }
}

fn boundary_incompatibility_help(reason: IncompatibleReason) -> Option<&'static str> {
    match reason {
        IncompatibleReason::PointerSizedInteger => {
            Some("use a fixed-width integer such as `I32`, `I64`, `U32` or `U64`")
        }
        IncompatibleReason::OutsideInterfaceNominal => Some("declare the type in an interface"),
        IncompatibleReason::SliceContainsResource => {
            Some("use `Vec<T>` for a list whose elements contain resource handles")
        }
        _ => None,
    }
}

fn current_owner_inputs_from_prepared(input: &OwnedPhase1ModuleInput) -> CurrentOwnerInputs {
    let filtered_item_tree = input.filtered.item_tree.clone();
    let accepted_flags_attributes = input
        .filtered
        .validated_attrs
        .iter()
        .filter_map(|(ast_id, attrs)| attrs.flags.map(|_| *ast_id))
        .collect();
    let retained_item_kinds = filtered_item_tree
        .items
        .iter()
        .flat_map(|entry| {
            std::iter::once((entry.ast_id, entry.kind)).chain(
                entry
                    .children
                    .iter()
                    .map(|child| (child.ast_id, child.kind)),
            )
        })
        .collect();
    CurrentOwnerInputs {
        source: Arc::clone(&input.source),
        root: input.root.clone(),
        ast_ids: input.ast_ids.clone(),
        filtered_item_tree,
        retained_item_kinds,
        accepted_flags_attributes,
        source_ref: input.source_ref.clone(),
    }
}

pub(super) fn module_scope_with_interface_members(
    module_scope: &Phase1ModuleScopes,
    scopes: &Phase1PackageScopes,
    interface_owner: OwnerId,
) -> Result<Phase1ModuleScopes, OwnerControlFlowResolutionError> {
    let interface_scope = scopes
        .interfaces
        .get(&interface_owner.def_id.to_def_id())
        .ok_or(OwnerControlFlowResolutionError::MissingInterfaceScope(
            interface_owner,
        ))?;
    let mut module_scope = module_scope.clone();
    for (name, binding) in &interface_scope.members {
        module_scope.scope.insert(*name, binding.clone());
    }
    Ok(module_scope)
}

fn stable_interface_uses_fingerprint(
    package_key: &PackageKey,
    input: &InterfaceUsesInput,
    graph: &InterfaceUses,
    interface_check_diagnostics: &[Diagnostic],
    prepared: &PreparedPhase1PackageScopes,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<Fingerprint, InterfaceUsesQueryError> {
    let mut canonical = HashMap::with_capacity(input.interfaces.len());
    for interface in &input.interfaces {
        let path = prepared
            .def_paths
            .resolve(*interface)
            .map_err(InterfaceUsesQueryError::DefPathInterner)?
            .ok_or_else(|| {
                InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::MissingDefPath(OwnerId { def_id: *interface }),
                )
            })?;
        let hash = DefPathHash::compute(&path, packages, symbols)
            .map_err(InterfaceUsesQueryError::DefPath)?;
        canonical.insert(*interface, hash.raw());
    }
    let sources = prepared
        .modules
        .iter()
        .map(|module| {
            (
                module.source.id(),
                (
                    module.source_ref.path.as_str(),
                    module.source_ref.package.as_deref().unwrap_or(""),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut hasher = StableHasher::new();
    hasher.write_tag(0xEA);
    package_key.as_str().stable_hash(&mut hasher);
    input.package.stable_hash(&mut hasher);
    hasher.write_len(u64::try_from(input.interfaces.len()).unwrap_or(u64::MAX));
    for (generation_order, interface) in input.interfaces.iter().enumerate() {
        hasher.write_len(u64::try_from(generation_order).unwrap_or(u64::MAX));
        hasher.write_u128(canonical[interface]);
        hasher.write_u8(u8::from(graph.cyclic.contains(interface)));
        let edges = graph.uses.get(interface).map(Vec::as_slice).unwrap_or(&[]);
        hasher.write_len(u64::try_from(edges.len()).unwrap_or(u64::MAX));
        for (target, span) in edges {
            hasher.write_u128(canonical[target]);
            let (path, package) = sources.get(&span.file).ok_or_else(|| {
                InterfaceUsesQueryError::Collection(
                    OwnerControlFlowResolutionError::MissingSnapshotFile(span.file),
                )
            })?;
            path.stable_hash(&mut hasher);
            package.stable_hash(&mut hasher);
            hash_range(span, &mut hasher);
        }
    }
    hasher.write_len(u64::try_from(graph.diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in &graph.diagnostics {
        diagnostic.code.as_str().stable_hash(&mut hasher);
        diagnostic.message.stable_hash(&mut hasher);
        hash_optional_location(diagnostic.primary(), &sources, &mut hasher)?;
        hasher.write_len(u64::try_from(diagnostic.children().len()).unwrap_or(u64::MAX));
        for child in diagnostic.children() {
            child.message.stable_hash(&mut hasher);
            hash_optional_location(child.location.as_ref(), &sources, &mut hasher)?;
        }
    }
    hasher.write_len(u64::try_from(interface_check_diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in interface_check_diagnostics {
        diagnostic.code.as_str().stable_hash(&mut hasher);
        diagnostic.message.stable_hash(&mut hasher);
        hash_optional_location(diagnostic.primary(), &sources, &mut hasher)?;
        hasher.write_len(u64::try_from(diagnostic.related().len()).unwrap_or(u64::MAX));
        for related in diagnostic.related() {
            match related {
                Related::DeclaredHere { location, what } => {
                    hasher.write_tag(1);
                    what.stable_hash(&mut hasher);
                    hash_optional_location(Some(location), &sources, &mut hasher)?;
                }
                _ => {
                    return Err(InterfaceUsesQueryError::DiagnosticTemplate(
                        "unsupported related interface-check diagnostic".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(hasher.finish())
}

fn hash_optional_location(
    location: Option<&Location>,
    sources: &HashMap<FileId, (&str, &str)>,
    hasher: &mut StableHasher,
) -> Result<(), InterfaceUsesQueryError> {
    let Some(location) = location else {
        hasher.write_tag(0);
        return Ok(());
    };
    hasher.write_tag(1);
    let (path, package) = sources.get(&location.span.file).ok_or_else(|| {
        InterfaceUsesQueryError::Collection(OwnerControlFlowResolutionError::MissingSnapshotFile(
            location.span.file,
        ))
    })?;
    path.stable_hash(hasher);
    package.stable_hash(hasher);
    hash_range(&location.span, hasher);
    Ok(())
}

fn hash_range(span: &Span, hasher: &mut StableHasher) {
    hasher.write_len(u64::try_from(span.range.start().to_usize()).unwrap_or(u64::MAX));
    hasher.write_len(u64::try_from(span.range.end().to_usize()).unwrap_or(u64::MAX));
}

fn interface_uses_error_fingerprint(error: &InterfaceUsesQueryError) -> Fingerprint {
    let mut hasher = StableHasher::new();
    hasher.write_tag(0xEB);
    match error {
        InterfaceUsesQueryError::Query(error) => {
            hasher.write_tag(0);
            error.to_string().stable_hash(&mut hasher);
        }
        InterfaceUsesQueryError::Collection(error) => {
            hasher.write_tag(1);
            hasher.write_tag(owner_resolution_error_tag(error));
        }
        InterfaceUsesQueryError::PackageScopes(error) => {
            hasher.write_tag(2);
            hasher.write_tag(package_scopes_error_tag(error));
        }
        InterfaceUsesQueryError::DefPath(error) => {
            hasher.write_tag(3);
            hasher.write_tag(def_path_error_tag(error));
        }
        InterfaceUsesQueryError::DefPathInterner(error) => {
            hasher.write_tag(4);
            hasher.write_tag(def_path_interner_error_tag(error));
        }
        InterfaceUsesQueryError::DiagnosticTemplate(error) => {
            hasher.write_tag(5);
            error.stable_hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn owner_resolution_error_tag(error: &OwnerControlFlowResolutionError) -> u8 {
    match error {
        OwnerControlFlowResolutionError::Query(_) => 0,
        OwnerControlFlowResolutionError::PrimitivePrelude(_) => 1,
        OwnerControlFlowResolutionError::SessionMismatch => 33,
        OwnerControlFlowResolutionError::SnapshotMismatch => 2,
        OwnerControlFlowResolutionError::ModuleTreeMismatch => 3,
        OwnerControlFlowResolutionError::ModuleMemberScopes(_) => 4,
        OwnerControlFlowResolutionError::TestAnalysisDisabled => 5,
        OwnerControlFlowResolutionError::MissingSnapshotFile(_) => 6,
        OwnerControlFlowResolutionError::Parse(_) => 7,
        OwnerControlFlowResolutionError::ItemTree(_) => 8,
        OwnerControlFlowResolutionError::TargetFilter(_) => 9,
        OwnerControlFlowResolutionError::DefPathInterner(_) => 10,
        OwnerControlFlowResolutionError::SymbolLookup(_) => 11,
        OwnerControlFlowResolutionError::PackageId(_) => 12,
        OwnerControlFlowResolutionError::MissingDefPath(_) => 13,
        OwnerControlFlowResolutionError::MissingTargetDefPath(_) => 14,
        OwnerControlFlowResolutionError::DescriptorNotInCurrentTarget { .. } => 15,
        OwnerControlFlowResolutionError::MissingSourcePackageIdentity { .. } => 16,
        OwnerControlFlowResolutionError::TopLevelOwnerDescriptorMismatch { .. } => 17,
        OwnerControlFlowResolutionError::TopLevelOwnerKindMismatch { .. } => 18,
        OwnerControlFlowResolutionError::MissingModuleScope(_) => 19,
        OwnerControlFlowResolutionError::MissingImportBinding { .. } => 20,
        OwnerControlFlowResolutionError::MissingTestOnlyUseSyntax { .. } => 21,
        OwnerControlFlowResolutionError::MissingEffectDeclarationSyntax(_) => 22,
        OwnerControlFlowResolutionError::MissingWorldItemSyntax { .. } => 23,
        OwnerControlFlowResolutionError::MissingRefinedTypeSyntax(_) => 24,
        OwnerControlFlowResolutionError::MissingInterfaceFunctionSyntax(_) => 25,
        OwnerControlFlowResolutionError::InvalidSourceLocation(_) => 26,
        OwnerControlFlowResolutionError::Lowering(_) => 27,
        OwnerControlFlowResolutionError::Labels(_) => 28,
        OwnerControlFlowResolutionError::Locals(_) => 29,
        OwnerControlFlowResolutionError::MissingInterfaceScope(_) => 30,
        OwnerControlFlowResolutionError::MissingInterfaceImplTarget(_) => 31,
        OwnerControlFlowResolutionError::InterfaceImplDiagnosticSourceMismatch { .. } => 32,
    }
}

fn package_scopes_error_tag(error: &PackageScopesError) -> u8 {
    match error {
        PackageScopesError::PackageMismatch => 0,
        PackageScopesError::MissingModuleInput(_) => 1,
        PackageScopesError::UnexpectedModuleInput(_) => 2,
        PackageScopesError::InvalidModuleInput(_) => 3,
        PackageScopesError::TooManyModules => 4,
        PackageScopesError::MissingAstNode => 5,
        PackageScopesError::MissingName => 6,
        PackageScopesError::NameMismatch => 7,
        PackageScopesError::InvalidSourceRange => 8,
        PackageScopesError::Attribute(_) => 9,
        PackageScopesError::TargetFilter(_) => 10,
        PackageScopesError::Package(_) => 11,
        PackageScopesError::PackageKey(_) => 12,
        PackageScopesError::DefPathValidation(_) => 13,
        PackageScopesError::Interner(_) => 14,
        PackageScopesError::Symbol(_) => 15,
        PackageScopesError::InvalidModuleTree => 16,
        PackageScopesError::InvalidCurrentModule => 17,
        PackageScopesError::UnsupportedPackageKey => 18,
        PackageScopesError::ForeignPackageScopeMismatch => 19,
        PackageScopesError::DefPath(_) => 20,
        PackageScopesError::TooManyDefinitions => 21,
    }
}

fn def_path_error_tag(error: &DefPathError) -> u8 {
    match error {
        DefPathError::Package(_) => 0,
        DefPathError::Symbol(_) => 1,
        DefPathError::InternerSessionMismatch => 2,
    }
}

fn def_path_interner_error_tag(error: &DefPathInternerError) -> u8 {
    match error {
        DefPathInternerError::Package(_) => 0,
        DefPathInternerError::InternerSessionMismatch => 1,
        DefPathInternerError::ForeignDefinitionId => 2,
        DefPathInternerError::ParentDefinitionNotFound => 3,
        DefPathInternerError::InvalidNestedComponent => 4,
        DefPathInternerError::InvalidAssociatedItem => 5,
        DefPathInternerError::WrongPackage => 6,
        DefPathInternerError::TooManyInternerInstances => 7,
        DefPathInternerError::TooManyDefinitions => 8,
        DefPathInternerError::LockPoisoned => 9,
    }
}

fn clear_document_versions(diagnostic: &mut DetachedWorkspaceDiagnostic) {
    if let Some(location) = &mut diagnostic.primary {
        location.file.snapshot = None;
    }
    for label in &mut diagnostic.labels {
        label.location.file.snapshot = None;
    }
    for child in &mut diagnostic.children {
        if let Some(location) = &mut child.location {
            location.file.snapshot = None;
        }
    }
    for related in &mut diagnostic.related {
        if let sai_diagnostics::Related::DeclaredHere { location, .. } = related {
            location.file.snapshot = None;
        }
    }
    if let Some(level) = &mut diagnostic.level {
        if let Some(location) = &mut level.location {
            location.file.snapshot = None;
        }
    }
}

fn bind_current_snapshot(
    diagnostic: &DetachedWorkspaceDiagnostic,
    snapshot: &WorkspaceSnapshot,
) -> Result<Diagnostic, PackageScopesError> {
    let mut diagnostic = diagnostic.clone();
    if let Some(location) = &mut diagnostic.primary {
        refresh_location_reference(location, snapshot)?;
    }
    for label in &mut diagnostic.labels {
        refresh_location_reference(&mut label.location, snapshot)?;
    }
    for child in &mut diagnostic.children {
        if let Some(location) = &mut child.location {
            refresh_location_reference(location, snapshot)?;
        }
    }
    for related in &mut diagnostic.related {
        if let sai_diagnostics::Related::DeclaredHere { location, .. } = related {
            refresh_location_reference(location, snapshot)?;
        }
    }
    if let Some(level) = &mut diagnostic.level {
        if let Some(location) = &mut level.location {
            refresh_location_reference(location, snapshot)?;
        }
    }
    diagnostic.bind(snapshot)
}

fn refresh_location_reference(
    location: &mut Location,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), PackageScopesError> {
    let file = snapshot
        .file(location.span.file)
        .ok_or(PackageScopesError::InvalidSourceRange)?;
    location.file = SourceRef {
        path: file.path().to_owned(),
        package: file.package().map(str::to_owned),
        snapshot: file.document_version(),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InterfaceUsesQueryError, interface_uses_error_fingerprint};
    use crate::OwnerControlFlowResolutionError;
    use sai_vfs::FileId;

    #[test]
    fn error_fingerprint_excludes_session_local_file_ids() {
        let first = InterfaceUsesQueryError::Collection(
            OwnerControlFlowResolutionError::MissingSnapshotFile(FileId::new(1)),
        );
        let second = InterfaceUsesQueryError::Collection(
            OwnerControlFlowResolutionError::MissingSnapshotFile(FileId::new(99)),
        );

        assert_eq!(
            interface_uses_error_fingerprint(&first),
            interface_uses_error_fingerprint(&second)
        );
    }
}
