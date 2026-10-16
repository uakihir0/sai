use super::{
    CurrentOwnerInputs, DetachedWorkspaceDiagnostic, InterfaceResourceMemberIds,
    OwnedPhase1ModuleInput, OwnerControlFlowResolutionError, Phase1ModuleScopes,
    Phase1PackageScopesQueryOutput, Prelude, PreparedPhase1PackageScopes,
    PrimitivePreludeBuildError, STAGED_PHASE1_PACKAGE_SCOPES, append_interface_use_references,
    index_top_level_owner_descriptors_in_scopes, interface_generation_order, interface_member_kind,
    local_interface_type_member_owners, lower_interface_item_owner,
    lower_interface_resource_member_owner, lower_top_level_owner, module_scope_for_scopes,
    owner_test_only_path_candidates, owner_type_generic_resolutions,
    resolve_partial_type_owner_paths,
};
use sai_diagnostics::{Child, ChildKind, Diagnostic, Location, Severity, SourceRef, Value};
use sai_hir::arena::HirArena;
use sai_hir::item_tree::ItemTreeKind;
use sai_hir::items::{HirOwner, InterfaceItemKind, ItemKind, OwnerNode};
use sai_hir::{DefPathError, DefPathHash, DefPathInternerError, LocalDefId, OwnerId, Symbol};
use sai_intern::{Interner, PackageInterner};
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_resolve::module_member_lookup::ModuleMemberIndex;
use sai_resolve::package_scopes::{
    PackageScopesError, Phase1PackageScopes, Res, TopLevelOwnerKind,
};
use sai_resolve::{InterfaceUseReference, InterfaceUses, InterfaceUsesInput, interface_uses};
use sai_stable_hash::{Fingerprint, StableHash, StableHasher};
use sai_ty::iface::{InterfaceCheckError, InterfaceCheckIssue, interface_item_check};
use sai_vfs::{FileId, PackageKey, Span, WorkspaceSnapshot};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
        diagnostics.push(diagnostic);
    }
    Ok(())
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
    let indexed_scopes = [scopes];
    let module_members = ModuleMemberIndex::new(indexed_scopes);
    let source_graph_packages = HashSet::from([prepared.module_tree.package]);
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

    let mut package_identity = None;
    let mut references = Vec::<InterfaceUseReference>::new();
    let mut interface_check_diagnostics = Vec::new();
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
            append_interface_item_diagnostics(
                collect_interface_item_issues(&member_hir, flags_marker_accepted, symbols)?,
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
            collect_owner_references(
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
            for (method_entry, method_owner) in method_entries
                .into_iter()
                .zip(resource_members.iter().copied())
            {
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
                append_interface_item_diagnostics(
                    collect_interface_item_issues(&method_hir, false, symbols)?,
                    &inputs.source_ref,
                    &inputs.source,
                    &prepared.snapshot,
                    &mut interface_check_diagnostics,
                )?;
                collect_owner_references(
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
            }
        }
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
    Ok((
        InterfaceUsesInput {
            package,
            interfaces: interface_ids,
            references,
        },
        interface_check_diagnostics,
    ))
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
) -> Result<(), InterfaceUsesQueryError> {
    let candidates = owner_test_only_path_candidates(hir_owner);
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
        &candidates,
        &resolution,
        interface_type_members,
        inputs,
    )
    .map_err(InterfaceUsesQueryError::Collection)
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
