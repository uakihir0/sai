use super::{
    OwnerControlFlowResolutionError, Phase1ActiveSourceMemberScopes, Phase1ActiveWitViewScopes,
    Phase1PackageScopesCollection, RootDatabase, WorldReferencePathResolution,
    WorldReferencePathResult, WorldReferencePosition, WorldReferenceResolutions,
    WorldSourceOrderItem, WorldSourceOrderKind,
};
use sai_diagnostics::{Location, Span as DiagnosticSpan};
use sai_hir::DefId;
use sai_resolve::package_scopes::{DefKind, Res};
use sai_resolve::{
    ElabItem, ElabKind, ElabOrigin, ExternalWorld, ExternalWorldItem, WorldDirection,
    WorldElaborationInput, WorldElaborationLocalWorld, WorldElaborationSourceItem,
};
use sai_vfs::{FileId, WorkspaceSnapshot};
use sai_wit::{
    WitView, WitViewEntity, WitWorldIdx, WitWorldItem, WitWorldItemDirection, WitWorldItemOrder,
    WitWorldRef,
};
use std::collections::{HashMap, HashSet};

#[allow(clippy::too_many_arguments)]
pub(super) fn build(
    database: &mut RootDatabase,
    collection: &Phase1PackageScopesCollection,
    module_tree: &super::ModuleTree,
    snapshot: &WorkspaceSnapshot,
    references: &WorldReferenceResolutions,
    member_scopes: &Phase1ActiveSourceMemberScopes,
    wit_view_scopes: &Phase1ActiveWitViewScopes,
) -> Result<WorldElaborationInput, OwnerControlFlowResolutionError> {
    if collection.session_token != database.session_token {
        return Err(OwnerControlFlowResolutionError::SessionMismatch);
    }
    if collection.snapshot_id != snapshot.id() || !references.matches_snapshot(snapshot) {
        return Err(OwnerControlFlowResolutionError::SnapshotMismatch);
    }
    if collection.module_tree != *module_tree {
        return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
    }
    if !member_scopes.matches_collection(collection)
        || !references.matches_member_scopes(collection, member_scopes)
    {
        return Err(OwnerControlFlowResolutionError::ModuleMemberScopes(
            super::Phase1ModuleMemberScopeError::PackageGraphMismatch(
                collection.package_key.clone(),
            ),
        ));
    }
    if !wit_view_scopes.matches_collection(collection)
        || !references.matches_wit_view_scopes(collection, wit_view_scopes)
    {
        return Err(OwnerControlFlowResolutionError::ModuleMemberScopes(
            super::Phase1ModuleMemberScopeError::WitViewScopeMismatch(
                collection.package_key.clone(),
            ),
        ));
    }
    if !references.matches_collection(collection) {
        return Err(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch);
    }
    database.validate_active_wit_view_scopes(collection, snapshot, wit_view_scopes)?;

    let descriptors = super::interface_generation_order(
        module_tree,
        collection
            .scopes
            .top_level_owners
            .iter()
            .copied()
            .filter(|descriptor| descriptor.kind == super::TopLevelOwnerKind::World)
            .collect(),
        &database.symbols,
    )?;
    let mut owners_by_id = HashMap::with_capacity(references.owners.len());
    for owner in &references.owners {
        if owners_by_id.insert(owner.owner, owner).is_some() {
            return Err(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch);
        }
    }
    if owners_by_id.len() != descriptors.len()
        || descriptors
            .iter()
            .any(|descriptor| !owners_by_id.contains_key(&descriptor.owner))
    {
        return Err(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch);
    }

    let interface_keys = collection
        .world_interface_uses
        .interfaces()
        .iter()
        .map(|interface| (interface.interface, interface.key.to_string()))
        .collect::<HashMap<_, _>>();
    let external_world_data = collect_external_worlds(wit_view_scopes, &interface_keys)?;
    let local_world_ids = descriptors
        .iter()
        .map(|descriptor| descriptor.owner.def_id.to_def_id())
        .collect::<HashSet<_>>();
    let external_world_ids = external_world_data
        .worlds
        .iter()
        .map(|world| world.world)
        .collect::<HashSet<_>>();

    let includes = database.world_includes_from_references(
        collection,
        module_tree,
        snapshot,
        references,
        false,
    )?;
    let mut local_worlds = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let owner = owners_by_id
            .get(&descriptor.owner)
            .copied()
            .ok_or(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch)?;
        let mut complete = owner.error.is_none() && owner.item_errors.is_empty();
        let mut references_by_owner = HashMap::with_capacity(owner.references.len());
        for reference in &owner.references {
            if reference.world_owner != descriptor.owner
                || references_by_owner
                    .insert(reference.owner, reference)
                    .is_some()
            {
                return Err(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch);
            }
        }
        let inputs =
            database.current_owner_inputs(snapshot, descriptor.file, &collection.target_cfg)?;
        let mut consumed_references = HashSet::with_capacity(owner.references.len());
        let mut items = Vec::with_capacity(owner.source_order.len());

        for source in &owner.source_order {
            match source.kind() {
                WorldSourceOrderKind::Import
                | WorldSourceOrderKind::Export
                | WorldSourceOrderKind::Include => {
                    let expected_position = match source.kind() {
                        WorldSourceOrderKind::Import => WorldReferencePosition::Import,
                        WorldSourceOrderKind::Export => WorldReferencePosition::Export,
                        WorldSourceOrderKind::Include => WorldReferencePosition::Include,
                        WorldSourceOrderKind::ImportFunction
                        | WorldSourceOrderKind::ExportFunction => unreachable!(),
                    };
                    let Some(reference) = references_by_owner.get(&source.owner()).copied() else {
                        complete = false;
                        continue;
                    };
                    consumed_references.insert(source.owner());
                    if reference.position != expected_position
                        || reference.hir_id != source.hir_id()
                        || reference.span != source.span()
                    {
                        complete = false;
                        continue;
                    }
                    let Some(base) = resolved_world_reference(reference) else {
                        complete = false;
                        continue;
                    };
                    let location = source_location(source, descriptor.file, &inputs)?.ok_or(
                        OwnerControlFlowResolutionError::InvalidSourceLocation(source.owner()),
                    )?;
                    match source.kind() {
                        WorldSourceOrderKind::Import | WorldSourceOrderKind::Export => {
                            let Res::Def(DefKind::Interface, interface) = base else {
                                complete = false;
                                continue;
                            };
                            let Some(key) = interface_keys.get(&interface) else {
                                complete = false;
                                continue;
                            };
                            let item = match source.kind() {
                                WorldSourceOrderKind::Import => {
                                    WorldElaborationSourceItem::ImportInterface {
                                        key: key.as_str().into(),
                                        interface,
                                        owner: source.hir_id(),
                                        location,
                                    }
                                }
                                WorldSourceOrderKind::Export => {
                                    WorldElaborationSourceItem::ExportInterface {
                                        key: key.as_str().into(),
                                        interface,
                                        owner: source.hir_id(),
                                        location,
                                    }
                                }
                                _ => unreachable!(),
                            };
                            items.push(item);
                        }
                        WorldSourceOrderKind::Include => {
                            let Res::Def(DefKind::World, target) = base else {
                                complete = false;
                                continue;
                            };
                            if !local_world_ids.contains(&target)
                                && !external_world_ids.contains(&target)
                            {
                                complete = false;
                                continue;
                            }
                            items.push(WorldElaborationSourceItem::Include {
                                target,
                                owner: source.hir_id(),
                                location,
                            });
                        }
                        WorldSourceOrderKind::ImportFunction
                        | WorldSourceOrderKind::ExportFunction => unreachable!(),
                    }
                }
                WorldSourceOrderKind::ImportFunction | WorldSourceOrderKind::ExportFunction => {
                    let Some(name) = source.name() else {
                        complete = false;
                        continue;
                    };
                    let Some(key) = sai_resolve::sai_identifier_to_wit_name(name) else {
                        complete = false;
                        continue;
                    };
                    let location = source_location(source, descriptor.file, &inputs)?.ok_or(
                        OwnerControlFlowResolutionError::InvalidSourceLocation(source.owner()),
                    )?;
                    let function = source.owner().def_id.to_def_id();
                    items.push(match source.kind() {
                        WorldSourceOrderKind::ImportFunction => {
                            WorldElaborationSourceItem::ImportFunction {
                                key: key.into(),
                                function,
                                owner: source.hir_id(),
                                location,
                            }
                        }
                        WorldSourceOrderKind::ExportFunction => {
                            WorldElaborationSourceItem::ExportFunction {
                                key: key.into(),
                                function,
                                owner: source.hir_id(),
                                location,
                            }
                        }
                        _ => unreachable!(),
                    });
                }
            }
        }
        if consumed_references.len() != references_by_owner.len() {
            complete = false;
        }
        local_worlds.push(WorldElaborationLocalWorld {
            world: descriptor.owner.def_id.to_def_id(),
            complete,
            items,
        });
    }

    Ok(WorldElaborationInput {
        package: collection.world_interface_uses.package().to_owned(),
        local_worlds,
        external_worlds: external_world_data.worlds,
        interfaces: collection.world_interface_uses.interfaces().to_vec(),
        interface_uses: collection.world_interface_uses.edges().to_vec(),
        cyclic_worlds: includes.cyclic.into_iter().collect(),
    })
}

fn resolved_world_reference(reference: &WorldReferencePathResolution) -> Option<Res> {
    let path = match reference.path_result() {
        WorldReferencePathResult::Mod(path) | WorldReferencePathResult::ResolvedPkg(path)
            if path.resolved_segments == path.segment_count =>
        {
            path
        }
        WorldReferencePathResult::Mod(_)
        | WorldReferencePathResult::DeferredMod(_)
        | WorldReferencePathResult::Pkg(_)
        | WorldReferencePathResult::ResolvedPkg(_) => return None,
    };
    Some(path.base)
}

fn source_location(
    source: &WorldSourceOrderItem,
    file: FileId,
    inputs: &super::CurrentOwnerInputs,
) -> Result<Option<Location>, OwnerControlFlowResolutionError> {
    if source.span().file != file {
        return Err(OwnerControlFlowResolutionError::InvalidSourceLocation(
            source.owner(),
        ));
    }
    Ok(Location::from_decoded_source(
        inputs.source_ref.clone(),
        DiagnosticSpan::new(source.span().file, source.span().range),
        &inputs.source,
    ))
}

struct ExternalWorldData {
    worlds: Vec<ExternalWorld>,
}

#[derive(Clone, Copy)]
struct ExternalWitWorld<'a> {
    view: &'a WitView,
    index: WitWorldIdx,
    definition: DefId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectedWitItem {
    direction: WorldDirection,
    item: ElabItem,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ExternalCandidateKey {
    Function(String),
    Interface(DefId),
}

#[derive(Clone)]
struct ExternalWorldFrame<'a> {
    world: ExternalWitWorld<'a>,
    order: Vec<WitWorldItemOrder>,
    next: usize,
    consumed_imports: Vec<bool>,
    consumed_exports: Vec<bool>,
    import_candidates: HashMap<ExternalCandidateKey, Vec<usize>>,
    export_candidates: HashMap<ExternalCandidateKey, Vec<usize>>,
    seen_items: HashSet<(WorldDirection, String, ElabKind)>,
    complete: bool,
    items: Vec<ProjectedWitItem>,
    implicit_imports_insert_at: Option<usize>,
    pending_include: Option<WitWorldRef>,
}

fn collect_external_worlds(
    scopes: &Phase1ActiveWitViewScopes,
    interface_keys: &HashMap<DefId, String>,
) -> Result<ExternalWorldData, OwnerControlFlowResolutionError> {
    let views = scopes
        .views()
        .map(|foreign| foreign.view.as_ref())
        .collect::<Vec<_>>();
    let mut worlds_by_def = HashMap::<DefId, ExternalWitWorld<'_>>::new();
    let mut worlds_by_address = HashMap::<(String, String, u32), DefId>::new();
    let mut ordered_definitions = Vec::new();
    let mut interfaces_by_address = HashMap::<(String, String, u32), DefId>::new();

    for view in &views {
        let unit_identity = (
            view.unit.canonical_name().to_owned(),
            view.unit.stable_key().to_owned(),
        );
        for declaration in view.model.declarations_in_order() {
            match declaration {
                sai_wit::WitPackageDeclaration::Interface(index) => {
                    let entity = WitViewEntity::Interface { interface: index };
                    if let Some(definition) = view.definition_for_entity(entity) {
                        let address = (
                            unit_identity.0.clone(),
                            unit_identity.1.clone(),
                            index.raw(),
                        );
                        if interfaces_by_address
                            .insert(address, definition)
                            .is_some_and(|previous| previous != definition)
                        {
                            return Err(
                                OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch,
                            );
                        }
                    }
                }
                sai_wit::WitPackageDeclaration::World(index) => {
                    let entity = WitViewEntity::World { world: index };
                    let Some(definition) = view.definition_for_entity(entity) else {
                        continue;
                    };
                    let address = (
                        unit_identity.0.clone(),
                        unit_identity.1.clone(),
                        index.raw(),
                    );
                    if worlds_by_address
                        .insert(address, definition)
                        .is_some_and(|previous| previous != definition)
                    {
                        return Err(
                            OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch,
                        );
                    }
                    if worlds_by_def
                        .insert(
                            definition,
                            ExternalWitWorld {
                                view,
                                index,
                                definition,
                            },
                        )
                        .is_some()
                    {
                        return Err(
                            OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch,
                        );
                    }
                    ordered_definitions.push(definition);
                }
            }
        }
    }

    let mut cache = HashMap::<DefId, (Vec<ProjectedWitItem>, bool)>::new();
    let mut output = Vec::with_capacity(ordered_definitions.len());
    for definition in ordered_definitions {
        let Some(world) = worlds_by_def.get(&definition).copied() else {
            return Err(OwnerControlFlowResolutionError::WorldReferenceCollectionMismatch);
        };
        let mut active = HashSet::new();
        let (items, complete) = expand_external_world(
            world,
            &worlds_by_def,
            &worlds_by_address,
            &interfaces_by_address,
            interface_keys,
            &mut cache,
            &mut active,
        );
        output.push(ExternalWorld {
            world: definition,
            items: items
                .into_iter()
                .map(|projected| ExternalWorldItem {
                    direction: projected.direction,
                    item: projected.item,
                })
                .collect(),
            complete,
        });
    }
    Ok(ExternalWorldData { worlds: output })
}

fn expand_external_world<'a>(
    root: ExternalWitWorld<'a>,
    worlds_by_def: &HashMap<DefId, ExternalWitWorld<'a>>,
    worlds_by_address: &HashMap<(String, String, u32), DefId>,
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
    interface_keys: &HashMap<DefId, String>,
    cache: &mut HashMap<DefId, (Vec<ProjectedWitItem>, bool)>,
    active: &mut HashSet<DefId>,
) -> (Vec<ProjectedWitItem>, bool) {
    if let Some(cached) = cache.get(&root.definition) {
        return cached.clone();
    }
    if !active.insert(root.definition) {
        return (Vec::new(), false);
    }

    let mut stack = vec![new_external_world_frame(root, interfaces_by_address)];
    loop {
        let Some(frame) = stack.last_mut() else {
            active.remove(&root.definition);
            return (Vec::new(), false);
        };
        let world = &frame.world.view.model.worlds[frame.world.index];
        if frame.next == frame.order.len() {
            insert_implicit_external_imports(frame, interfaces_by_address, interface_keys);
            if frame.consumed_imports.iter().any(|consumed| !consumed)
                || frame.consumed_exports.iter().any(|consumed| !consumed)
            {
                frame.complete = false;
            }
            let finished = stack.pop().expect("the current frame exists");
            active.remove(&finished.world.definition);
            let completed = (finished.items, finished.complete);
            cache.insert(finished.world.definition, completed.clone());
            if let Some(parent) = stack.last_mut() {
                let Some(include) = parent.pending_include.take() else {
                    parent.complete = false;
                    continue;
                };
                parent.complete &= completed.1;
                append_included_external_items(
                    parent,
                    &completed.0,
                    &include,
                    interfaces_by_address,
                    interface_keys,
                );
                continue;
            }
            return completed;
        }

        let order = frame.order[frame.next];
        frame.next += 1;
        match order.direction {
            WitWorldItemDirection::Import | WitWorldItemDirection::Export => {
                let imported = order.direction == WitWorldItemDirection::Import;
                if !imported && frame.implicit_imports_insert_at.is_none() {
                    frame.implicit_imports_insert_at = Some(frame.items.len());
                }
                let index = order.index as usize;
                let items = if imported {
                    &world.imports
                } else {
                    &world.exports
                };
                let Some(item) = items.get(index) else {
                    frame.complete = false;
                    continue;
                };
                let consumed = if imported {
                    &mut frame.consumed_imports
                } else {
                    &mut frame.consumed_exports
                };
                if let Some(slot) = consumed.get_mut(index) {
                    let was_consumed = std::mem::replace(slot, true);
                    if was_consumed && !matches!(item, WitWorldItem::Interface { .. }) {
                        frame.complete = false;
                        continue;
                    }
                } else {
                    frame.complete = false;
                    continue;
                }
                let Some(projected) = project_external_item(
                    frame.world,
                    order.direction,
                    order.index,
                    item,
                    interfaces_by_address,
                    interface_keys,
                ) else {
                    frame.complete = false;
                    continue;
                };
                push_external_item(frame, projected);
            }
            WitWorldItemDirection::Include => {
                let Some(include) = world.includes.get(order.index as usize).cloned() else {
                    frame.complete = false;
                    continue;
                };
                let address = (
                    include.unit.canonical_name().to_owned(),
                    include.unit.stable_key().to_owned(),
                    include.world.raw(),
                );
                let Some(target) = worlds_by_address.get(&address).copied() else {
                    frame.complete = false;
                    continue;
                };
                if let Some(completed) = cache.get(&target).cloned() {
                    frame.complete &= completed.1;
                    append_included_external_items(
                        frame,
                        &completed.0,
                        &include,
                        interfaces_by_address,
                        interface_keys,
                    );
                    continue;
                }
                if active.contains(&target) {
                    frame.complete = false;
                    continue;
                }
                let Some(child) = worlds_by_def.get(&target).copied() else {
                    frame.complete = false;
                    continue;
                };
                frame.pending_include = Some(include);
                active.insert(target);
                stack.push(new_external_world_frame(child, interfaces_by_address));
            }
        }
    }
}

fn new_external_world_frame<'a>(
    world: ExternalWitWorld<'a>,
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
) -> ExternalWorldFrame<'a> {
    let model = &world.view.model.worlds[world.index];
    let order = if model.source_order.is_empty() {
        let mut fallback = (0..model.imports.len())
            .filter_map(|index| u32::try_from(index).ok())
            .map(WitWorldItemOrder::import)
            .collect::<Vec<_>>();
        fallback.extend(
            (0..model.exports.len())
                .filter_map(|index| u32::try_from(index).ok())
                .map(WitWorldItemOrder::export),
        );
        fallback
    } else {
        model.source_order.clone()
    };
    let import_candidates = external_item_candidates(&model.imports, interfaces_by_address);
    let export_candidates = external_item_candidates(&model.exports, interfaces_by_address);
    ExternalWorldFrame {
        world,
        order,
        next: 0,
        consumed_imports: vec![false; model.imports.len()],
        consumed_exports: vec![false; model.exports.len()],
        import_candidates,
        export_candidates,
        seen_items: HashSet::new(),
        complete: model.unsupported.is_none()
            && (!model.source_order.is_empty() || model.includes.is_empty()),
        items: Vec::new(),
        implicit_imports_insert_at: None,
        pending_include: None,
    }
}

fn external_item_candidates(
    items: &[WitWorldItem],
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
) -> HashMap<ExternalCandidateKey, Vec<usize>> {
    let mut candidates = HashMap::<ExternalCandidateKey, Vec<usize>>::new();
    for (index, item) in items.iter().enumerate() {
        let key = match item {
            WitWorldItem::Func { name, .. } => ExternalCandidateKey::Function(name.clone()),
            WitWorldItem::Interface { reference, .. } => {
                let address = (
                    reference.unit.canonical_name().to_owned(),
                    reference.unit.stable_key().to_owned(),
                    reference.interface.raw(),
                );
                let Some(interface) = interfaces_by_address.get(&address).copied() else {
                    continue;
                };
                ExternalCandidateKey::Interface(interface)
            }
        };
        candidates.entry(key).or_default().push(index);
    }
    candidates
}

fn project_external_item(
    world: ExternalWitWorld<'_>,
    direction: WitWorldItemDirection,
    index: u32,
    item: &WitWorldItem,
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
    interface_keys: &HashMap<DefId, String>,
) -> Option<ProjectedWitItem> {
    let imported = direction == WitWorldItemDirection::Import;
    let direction = if imported {
        WorldDirection::Import
    } else {
        WorldDirection::Export
    };
    let item = match item {
        WitWorldItem::Interface { reference, .. } => {
            let address = (
                reference.unit.canonical_name().to_owned(),
                reference.unit.stable_key().to_owned(),
                reference.interface.raw(),
            );
            let interface = interfaces_by_address.get(&address).copied()?;
            let key = interface_keys.get(&interface)?.as_str().into();
            ElabItem {
                key,
                kind: ElabKind::Interface(interface),
                origin: ElabOrigin::External,
            }
        }
        WitWorldItem::Func { name, .. } => {
            let function = world
                .view
                .definition_for_entity(WitViewEntity::WorldFunction {
                    world: world.index,
                    import: imported,
                    item: index,
                })?;
            ElabItem {
                key: name.as_str().into(),
                kind: ElabKind::Func(function),
                origin: ElabOrigin::External,
            }
        }
    };
    Some(ProjectedWitItem { direction, item })
}

fn append_included_external_items(
    parent: &mut ExternalWorldFrame<'_>,
    child_items: &[ProjectedWitItem],
    include: &WitWorldRef,
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
    interface_keys: &HashMap<DefId, String>,
) {
    let mut names = HashMap::<&str, &str>::with_capacity(include.names.len());
    for (original, renamed) in &include.names {
        names.entry(original.as_str()).or_insert(renamed.as_str());
    }
    for child in child_items {
        let mut key = child.item.key.clone();
        if matches!(child.item.kind, ElabKind::Func(_))
            && let Some(renamed) = names.get(key.as_str())
        {
            key = (*renamed).into();
        }
        let candidates = match child.direction {
            WorldDirection::Import => &parent.import_candidates,
            WorldDirection::Export => &parent.export_candidates,
        };
        let candidate_key = match child.item.kind {
            ElabKind::Func(_) => ExternalCandidateKey::Function(key.to_string()),
            ElabKind::Interface(interface) => ExternalCandidateKey::Interface(interface),
        };
        let Some(indices) = candidates.get(&candidate_key) else {
            parent.complete = false;
            continue;
        };
        if indices.len() != 1 {
            parent.complete = false;
        }
        let Some(index) = indices.first().copied() else {
            parent.complete = false;
            continue;
        };
        let direction = match child.direction {
            WorldDirection::Import => WitWorldItemDirection::Import,
            WorldDirection::Export => WitWorldItemDirection::Export,
        };
        let model = &parent.world.view.model.worlds[parent.world.index];
        let items = if direction == WitWorldItemDirection::Import {
            &model.imports
        } else {
            &model.exports
        };
        let consumed = if direction == WitWorldItemDirection::Import {
            &mut parent.consumed_imports
        } else {
            &mut parent.consumed_exports
        };
        if let Some(slot) = consumed.get_mut(index) {
            *slot = true;
        } else {
            parent.complete = false;
            continue;
        }
        let Some(candidate) = items.get(index) else {
            parent.complete = false;
            continue;
        };
        let Some(mut projected) = project_external_item(
            parent.world,
            direction,
            index as u32,
            candidate,
            interfaces_by_address,
            interface_keys,
        ) else {
            parent.complete = false;
            continue;
        };
        if projected.item.key != key && matches!(projected.item.kind, ElabKind::Func(_)) {
            parent.complete = false;
            continue;
        }
        if matches!(projected.item.kind, ElabKind::Func(_)) {
            projected.item.key = key;
        }
        push_external_item(parent, projected);
    }
}

fn insert_implicit_external_imports(
    frame: &mut ExternalWorldFrame<'_>,
    interfaces_by_address: &HashMap<(String, String, u32), DefId>,
    interface_keys: &HashMap<DefId, String>,
) {
    // wit-parser can add interface dependency imports during world elaboration.
    // They have no direct source-order slot, so place them before the first
    // direct export (or at the end if the world has no direct export).
    let world = &frame.world.view.model.worlds[frame.world.index];
    let mut implicit = Vec::new();
    for (index, item) in world.imports.iter().enumerate() {
        if frame.consumed_imports.get(index).copied().unwrap_or(false)
            || !matches!(item, WitWorldItem::Interface { .. })
        {
            continue;
        }
        let Ok(index_u32) = u32::try_from(index) else {
            frame.complete = false;
            continue;
        };
        let Some(slot) = frame.consumed_imports.get_mut(index) else {
            frame.complete = false;
            continue;
        };
        *slot = true;
        if let Some(projected) = project_external_item(
            frame.world,
            WitWorldItemDirection::Import,
            index_u32,
            item,
            interfaces_by_address,
            interface_keys,
        ) {
            if frame.seen_items.insert((
                projected.direction,
                projected.item.key.to_string(),
                projected.item.kind,
            )) {
                implicit.push(projected);
            }
        } else {
            frame.complete = false;
        }
    }
    let index = frame
        .implicit_imports_insert_at
        .unwrap_or(frame.items.len())
        .min(frame.items.len());
    frame.items.splice(index..index, implicit);
}

fn push_external_item(parent: &mut ExternalWorldFrame<'_>, item: ProjectedWitItem) {
    if !parent
        .seen_items
        .insert((item.direction, item.item.key.to_string(), item.item.kind))
    {
        return;
    }
    parent.items.push(item);
}
