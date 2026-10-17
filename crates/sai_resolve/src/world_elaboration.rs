//! Data-only elaboration of one local or external world.
//!
//! The caller supplies local worlds in package generation order, each with
//! all direct items in source order, and resolves their references before
//! invoking this helper. External worlds are supplied as already-elaborated
//! items in WIT order. This module expands includes, appends implicit imports
//! from interface-use edges, and reports local duplicate items without
//! inspecting HIR, WIT models, or package/query state.

use sai_diagnostics::{Child, ChildKind, Code, Diagnostic, Location, Severity};
use sai_hir::{DefId, HirId};
use smol_str::SmolStr;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(test)]
std::thread_local! {
    static WORLD_NORMALIZED_ITEM_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_normalized_item_count(count: usize) {
    WORLD_NORMALIZED_ITEM_COUNT.with(|total| total.set(total.get() + count));
}

/// Whether an elaborated item is imported or exported by a world.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorldDirection {
    Import,
    Export,
}

/// The identity of an item in an elaborated world.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ElabKind {
    Interface(DefId),
    Func(DefId),
}

/// The origin of an item in an elaborated world.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ElabOrigin {
    /// An item directly written in this world.
    Written(HirId),
    /// An item reached through this world's include item.
    Included { include: HirId },
    /// An interface added because another interface uses it.
    Implicit { cause: HirId, used_by: DefId },
    /// An item in an external WIT world's elaboration.
    External,
}

/// One interface or function in the elaborated world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElabItem {
    /// The item's WIT name in this world.
    pub key: SmolStr,
    pub kind: ElabKind,
    pub origin: ElabOrigin,
}

/// The direction and position of an item in the combined world order.
///
/// `index` addresses `imports` or `exports` according to `direction`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ElabItemOrder {
    pub direction: WorldDirection,
    pub index: usize,
}

/// The ordered, fully elaborated result for one world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElaboratedWorld {
    pub world: DefId,
    pub imports: Vec<ElabItem>,
    pub exports: Vec<ElabItem>,
    /// Cross-direction item order after include expansion and implicit
    /// imports. This keeps source order available without duplicating items.
    pub item_order: Vec<ElabItemOrder>,
    /// Duplicate diagnostics for this local world only. External worlds are
    /// already validated by their WIT loader and have no diagnostics here.
    pub diagnostics: Vec<Diagnostic>,
    /// False when a supplied local or included-world input was incomplete.
    ///
    /// Partial items are retained for recovery, but diagnostics that depend
    /// on a complete item list are suppressed.
    pub complete: bool,
}

/// One direct item of a local world, in source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorldElaborationSourceItem {
    ImportInterface {
        key: SmolStr,
        interface: DefId,
        owner: HirId,
        location: Location,
    },
    ExportInterface {
        key: SmolStr,
        interface: DefId,
        owner: HirId,
        location: Location,
    },
    ImportFunction {
        key: SmolStr,
        function: DefId,
        owner: HirId,
        location: Location,
    },
    ExportFunction {
        key: SmolStr,
        function: DefId,
        owner: HirId,
        location: Location,
    },
    Include {
        target: DefId,
        owner: HirId,
        location: Location,
    },
}

/// Ordered direct items of one local world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldElaborationLocalWorld {
    pub world: DefId,
    /// A complete world has a resolved entry for every direct item and every
    /// interface-use edge reachable from those items. Callers should set this
    /// false when path resolution, HIR lowering, or interface-use collection
    /// deferred any part of the input.
    pub complete: bool,
    pub items: Vec<WorldElaborationSourceItem>,
}

/// An external world's item, already elaborated by the WIT loader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalWorldItem {
    pub direction: WorldDirection,
    pub item: ElabItem,
}

/// One external world's already-elaborated item sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalWorld {
    pub world: DefId,
    /// Items are supplied in the external WIT world's elaboration order.
    pub items: Vec<ExternalWorldItem>,
    /// False when the active WIT model omitted unsupported or unresolved
    /// items. Including such a world cannot establish complete local
    /// duplicate checks.
    pub complete: bool,
}

/// Identity and use-collection state for a local or external interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldElaborationInterface {
    pub interface: DefId,
    /// The canonical WIT name used when the interface is implicitly imported.
    pub key: SmolStr,
    /// False if an unsupported or unresolved boundary type prevented a
    /// complete interface-use graph from being collected.
    pub uses_complete: bool,
}

/// An ordered edge from one interface to another interface it uses.
///
/// Callers provide edges in interface generation order, then first-reference
/// order within each interface. Edges outside the supplied interface set
/// make affected local worlds incomplete instead of producing invented
/// imports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceUse {
    pub from: DefId,
    pub to: DefId,
}

/// All resolved input needed to elaborate a world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldElaborationInput {
    /// Canonical package identity used in duplicate diagnostics.
    pub package: String,
    /// Local worlds in package generation order.
    pub local_worlds: Vec<WorldElaborationLocalWorld>,
    /// External worlds in active WIT-view order.
    pub external_worlds: Vec<ExternalWorld>,
    /// Local and external interface identity/name data.
    pub interfaces: Vec<WorldElaborationInterface>,
    /// Interface-use edges in generation and first-reference order.
    pub interface_uses: Vec<InterfaceUse>,
    /// Local worlds belonging to include-cycle SCCs. Includes targeting any
    /// such world contribute nothing.
    pub cyclic_worlds: HashSet<DefId>,
}

/// The requested world is not present in either supplied world set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldElaborationError {
    pub world: DefId,
}

#[derive(Clone, Debug)]
struct Occurrence {
    direction: WorldDirection,
    item: ElabItem,
    location: Option<Location>,
}

#[derive(Clone, Debug)]
struct SegmentView {
    sequence: Arc<SegmentSequence>,
    /// An outer include overrides the provenance of every item in this view.
    rebase: Option<Arc<(HirId, Location)>>,
}

#[derive(Clone, Debug)]
enum SegmentSequence {
    Empty,
    Leaf(Arc<Vec<Occurrence>>),
    Concat {
        left: SegmentView,
        right: SegmentView,
        height: u32,
    },
}

#[derive(Clone, Debug)]
struct LocalResult {
    /// An AVL rope of ordered item segments. Include-only worlds share the
    /// whole rope; function-only additions path-copy just the changed spine.
    segments: SegmentView,
    /// Persistent direction/name index used to validate fast-path additions.
    key_index: PersistentWorldKeys,
    diagnostics: Vec<Diagnostic>,
    complete: bool,
}

#[derive(Clone, Debug)]
struct ExternalSequence {
    segments: SegmentView,
    key_index: PersistentWorldKeys,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct WorldItemKey {
    direction: WorldDirection,
    key: SmolStr,
}

#[derive(Clone, Debug, Default)]
struct PersistentWorldKeys {
    root: Option<Arc<WorldItemKeyNode>>,
    len: usize,
}

#[derive(Clone, Debug)]
struct WorldItemKeyNode {
    key: WorldItemKey,
    left: Option<Arc<WorldItemKeyNode>>,
    right: Option<Arc<WorldItemKeyNode>>,
    height: u32,
}

impl PersistentWorldKeys {
    fn contains(&self, key: &WorldItemKey) -> bool {
        let mut node = self.root.as_deref();
        while let Some(current) = node {
            match key.cmp(&current.key) {
                std::cmp::Ordering::Less => node = current.left.as_deref(),
                std::cmp::Ordering::Greater => node = current.right.as_deref(),
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    fn insert(&mut self, key: WorldItemKey) {
        let (root, inserted) = insert_world_item_key(self.root.as_ref(), key);
        self.root = root;
        self.len += usize::from(inserted);
    }
}

fn insert_world_item_key(
    root: Option<&Arc<WorldItemKeyNode>>,
    key: WorldItemKey,
) -> (Option<Arc<WorldItemKeyNode>>, bool) {
    let Some(node) = root else {
        return (
            Some(Arc::new(WorldItemKeyNode {
                key,
                left: None,
                right: None,
                height: 1,
            })),
            true,
        );
    };
    match key.cmp(&node.key) {
        std::cmp::Ordering::Equal => (Some(Arc::clone(node)), false),
        std::cmp::Ordering::Less => {
            let (left, inserted) = insert_world_item_key(node.left.as_ref(), key);
            if !inserted {
                return (Some(Arc::clone(node)), false);
            }
            (
                Some(balance_world_item_key(
                    node.key.clone(),
                    left,
                    node.right.clone(),
                )),
                true,
            )
        }
        std::cmp::Ordering::Greater => {
            let (right, inserted) = insert_world_item_key(node.right.as_ref(), key);
            if !inserted {
                return (Some(Arc::clone(node)), false);
            }
            (
                Some(balance_world_item_key(
                    node.key.clone(),
                    node.left.clone(),
                    right,
                )),
                true,
            )
        }
    }
}

fn key_node_height(node: &Option<Arc<WorldItemKeyNode>>) -> u32 {
    node.as_ref().map_or(0, |node| node.height)
}

fn make_world_item_key_node(
    key: WorldItemKey,
    left: Option<Arc<WorldItemKeyNode>>,
    right: Option<Arc<WorldItemKeyNode>>,
) -> Arc<WorldItemKeyNode> {
    Arc::new(WorldItemKeyNode {
        key,
        height: 1 + key_node_height(&left).max(key_node_height(&right)),
        left,
        right,
    })
}

fn balance_world_item_key(
    key: WorldItemKey,
    left: Option<Arc<WorldItemKeyNode>>,
    right: Option<Arc<WorldItemKeyNode>>,
) -> Arc<WorldItemKeyNode> {
    let left_height = key_node_height(&left);
    let right_height = key_node_height(&right);
    if left_height > right_height + 1 {
        let left_node = left.as_ref().expect("left subtree is taller");
        if key_node_height(&left_node.left) >= key_node_height(&left_node.right) {
            let new_right = make_world_item_key_node(key, left_node.right.clone(), right);
            return make_world_item_key_node(
                left_node.key.clone(),
                left_node.left.clone(),
                Some(new_right),
            );
        }
        let pivot = left_node
            .right
            .as_ref()
            .expect("left-right subtree is taller");
        let new_left = make_world_item_key_node(
            left_node.key.clone(),
            left_node.left.clone(),
            pivot.left.clone(),
        );
        let new_right = make_world_item_key_node(key, pivot.right.clone(), right);
        return make_world_item_key_node(pivot.key.clone(), Some(new_left), Some(new_right));
    }
    if right_height > left_height + 1 {
        let right_node = right.as_ref().expect("right subtree is taller");
        if key_node_height(&right_node.right) >= key_node_height(&right_node.left) {
            let new_left = make_world_item_key_node(key, left, right_node.left.clone());
            return make_world_item_key_node(
                right_node.key.clone(),
                Some(new_left),
                right_node.right.clone(),
            );
        }
        let pivot = right_node
            .left
            .as_ref()
            .expect("right-left subtree is taller");
        let new_left = make_world_item_key_node(key, left, pivot.left.clone());
        let new_right = make_world_item_key_node(
            right_node.key.clone(),
            pivot.right.clone(),
            right_node.right.clone(),
        );
        return make_world_item_key_node(pivot.key.clone(), Some(new_left), Some(new_right));
    }
    make_world_item_key_node(key, left, right)
}

fn segment_height(sequence: &SegmentView) -> u32 {
    match sequence.sequence.as_ref() {
        SegmentSequence::Empty => 0,
        SegmentSequence::Leaf(_) => 1,
        SegmentSequence::Concat { height, .. } => *height,
    }
}

fn concat_segments(left: SegmentView, right: SegmentView) -> SegmentView {
    if matches!(left.sequence.as_ref(), SegmentSequence::Empty) {
        return right;
    }
    if matches!(right.sequence.as_ref(), SegmentSequence::Empty) {
        return left;
    }
    let left_height = segment_height(&left);
    let right_height = segment_height(&right);
    if left_height > right_height + 1
        && let SegmentSequence::Concat {
            left: left_child,
            right: right_child,
            ..
        } = left.sequence.as_ref()
    {
        let left_left = apply_segment_rebase(&left.rebase, left_child.clone());
        let left_right = apply_segment_rebase(&left.rebase, right_child.clone());
        let joined = concat_segments(left_right, right);
        return balance_segments(left_left, joined);
    }
    if right_height > left_height + 1
        && let SegmentSequence::Concat {
            left: right_left,
            right: right_right,
            ..
        } = right.sequence.as_ref()
    {
        let right_left = apply_segment_rebase(&right.rebase, right_left.clone());
        let right_right = apply_segment_rebase(&right.rebase, right_right.clone());
        let joined = concat_segments(left, right_left);
        return balance_segments(joined, right_right);
    }
    make_segment_node(left, right)
}

fn apply_segment_rebase(
    outer: &Option<Arc<(HirId, Location)>>,
    mut inner: SegmentView,
) -> SegmentView {
    if let Some(outer) = outer {
        inner.rebase = Some(Arc::clone(outer));
    }
    inner
}

fn balance_segments(left: SegmentView, right: SegmentView) -> SegmentView {
    let left_height = segment_height(&left);
    let right_height = segment_height(&right);
    if left_height > right_height + 1
        && let SegmentSequence::Concat {
            left: stored_left,
            right: stored_right,
            ..
        } = left.sequence.as_ref()
    {
        let left_left = apply_segment_rebase(&left.rebase, stored_left.clone());
        let left_right = apply_segment_rebase(&left.rebase, stored_right.clone());
        if segment_height(&left_left) >= segment_height(&left_right) {
            let new_right = make_segment_node(left_right, right);
            return make_segment_node(left_left, new_right);
        }
        if let SegmentSequence::Concat {
            left: pivot_left,
            right: pivot_right,
            ..
        } = left_right.sequence.as_ref()
        {
            let pivot_left = apply_segment_rebase(&left_right.rebase, pivot_left.clone());
            let pivot_right = apply_segment_rebase(&left_right.rebase, pivot_right.clone());
            let new_left = make_segment_node(left_left, pivot_left);
            let new_right = make_segment_node(pivot_right, right);
            return make_segment_node(new_left, new_right);
        }
    }
    if right_height > left_height + 1
        && let SegmentSequence::Concat {
            left: stored_left,
            right: stored_right,
            ..
        } = right.sequence.as_ref()
    {
        let right_left = apply_segment_rebase(&right.rebase, stored_left.clone());
        let right_right = apply_segment_rebase(&right.rebase, stored_right.clone());
        if segment_height(&right_right) >= segment_height(&right_left) {
            let new_left = make_segment_node(left, right_left);
            return make_segment_node(new_left, right_right);
        }
        if let SegmentSequence::Concat {
            left: pivot_left,
            right: pivot_right,
            ..
        } = right_left.sequence.as_ref()
        {
            let pivot_left = apply_segment_rebase(&right_left.rebase, pivot_left.clone());
            let pivot_right = apply_segment_rebase(&right_left.rebase, pivot_right.clone());
            let new_left = make_segment_node(left, pivot_left);
            let new_right = make_segment_node(pivot_right, right_right);
            return make_segment_node(new_left, new_right);
        }
    }
    make_segment_node(left, right)
}

fn make_segment_node(left: SegmentView, right: SegmentView) -> SegmentView {
    SegmentView {
        sequence: Arc::new(SegmentSequence::Concat {
            height: 1 + segment_height(&left).max(segment_height(&right)),
            left,
            right,
        }),
        rebase: None,
    }
}

fn walk_segments(
    sequence: &SegmentView,
    outer_rebase: Option<Arc<(HirId, Location)>>,
    mut visit: impl FnMut(&Arc<Vec<Occurrence>>, Option<&Arc<(HirId, Location)>>),
) {
    let initial_rebase = outer_rebase.or_else(|| sequence.rebase.clone());
    let mut pending = vec![(Arc::clone(&sequence.sequence), initial_rebase)];
    while let Some((current, rebase)) = pending.pop() {
        match current.as_ref() {
            SegmentSequence::Empty => {}
            SegmentSequence::Leaf(occurrences) => visit(occurrences, rebase.as_ref()),
            SegmentSequence::Concat { left, right, .. } => {
                pending.push((
                    Arc::clone(&right.sequence),
                    rebase.clone().or_else(|| right.rebase.clone()),
                ));
                pending.push((
                    Arc::clone(&left.sequence),
                    rebase.or_else(|| left.rebase.clone()),
                ));
            }
        }
    }
}

#[cfg(test)]
fn assert_segment_invariants(sequence: &SegmentView) {
    fn verify(view: &SegmentView) -> u32 {
        match view.sequence.as_ref() {
            SegmentSequence::Empty => 0,
            SegmentSequence::Leaf(_) => 1,
            SegmentSequence::Concat {
                left,
                right,
                height,
            } => {
                let left_height = verify(left);
                let right_height = verify(right);
                let expected_height = 1 + left_height.max(right_height);
                assert_eq!(
                    *height, expected_height,
                    "stored rope height must equal one plus the taller child"
                );
                assert!(
                    left_height.abs_diff(right_height) <= 1,
                    "rope child heights must differ by at most one"
                );
                expected_height
            }
        }
    }

    let verified_height = verify(sequence);
    assert_eq!(segment_height(sequence), verified_height);
}

#[derive(Default)]
struct SameIdentityOccurrences {
    seen: bool,
    first_origin_is_merge_eligible: bool,
    first_direct_index: Option<usize>,
}

/// Elaborates one local world or returns an external world's WIT elaboration.
///
/// For a local world, include expansion is depth-first and replaces the
/// include at its source position. Local worlds in cyclic SCCs are not
/// traversed through include items. Interface dependencies of exports are
/// appended as imports before dependencies of imports, each in depth-first
/// first-reference order.
///
/// Only the requested local world's duplicate diagnostics are returned.
/// Included local worlds are independently checked when requested as roots;
/// their already-filtered item sequences are used during this world's
/// expansion.
pub fn elaborated_world(
    world: DefId,
    input: WorldElaborationInput,
) -> Result<ElaboratedWorld, WorldElaborationError> {
    let local_indices = first_local_indices(&input.local_worlds);
    let external_indices = first_external_indices(&input.external_worlds);
    if let Some(&external_index) = external_indices.get(&world) {
        return Ok(external_result(&input.external_worlds[external_index]));
    }

    let Some(&root_index) = local_indices.get(&world) else {
        return Err(WorldElaborationError { world });
    };

    let (order, missing_cycle_edges) = local_postorder(
        root_index,
        &input.local_worlds,
        &local_indices,
        &input.cyclic_worlds,
    );
    let missing_cycle_sources = missing_cycle_edges
        .iter()
        .map(|(source, _)| *source)
        .collect::<HashSet<_>>();
    let interface_indices = first_interface_indices(&input.interfaces);
    let interface_uses = interface_use_adjacency(&input.interface_uses);
    let mut results = HashMap::<DefId, LocalResult>::with_capacity(order.len());
    let mut external_sequences = HashMap::<usize, ExternalSequence>::new();

    for index in order {
        let local_world = &input.local_worlds[index];
        let world_id = local_world.world;
        if let Some(result) = fast_include_with_functions(
            local_world,
            world_id,
            &input,
            &local_indices,
            &external_indices,
            &missing_cycle_edges,
            &missing_cycle_sources,
            &results,
            &mut external_sequences,
        ) {
            results.insert(world_id, result);
            continue;
        }

        let mut ordered_items = Vec::new();
        let mut mergeable_identities = HashSet::new();
        let mut complete = local_world.complete && !missing_cycle_sources.contains(&world_id);
        let mut expanded_include_targets = HashSet::new();
        let mut expanded_segments = HashSet::new();

        for source_item in &local_world.items {
            match source_item {
                WorldElaborationSourceItem::ImportInterface {
                    key,
                    interface,
                    owner,
                    location,
                } => push_occurrence(
                    &mut ordered_items,
                    &mut mergeable_identities,
                    written_occurrence(
                        WorldDirection::Import,
                        key,
                        ElabKind::Interface(*interface),
                        *owner,
                        location.clone(),
                    ),
                ),
                WorldElaborationSourceItem::ExportInterface {
                    key,
                    interface,
                    owner,
                    location,
                } => push_occurrence(
                    &mut ordered_items,
                    &mut mergeable_identities,
                    written_occurrence(
                        WorldDirection::Export,
                        key,
                        ElabKind::Interface(*interface),
                        *owner,
                        location.clone(),
                    ),
                ),
                WorldElaborationSourceItem::ImportFunction {
                    key,
                    function,
                    owner,
                    location,
                } => push_occurrence(
                    &mut ordered_items,
                    &mut mergeable_identities,
                    written_occurrence(
                        WorldDirection::Import,
                        key,
                        ElabKind::Func(*function),
                        *owner,
                        location.clone(),
                    ),
                ),
                WorldElaborationSourceItem::ExportFunction {
                    key,
                    function,
                    owner,
                    location,
                } => push_occurrence(
                    &mut ordered_items,
                    &mut mergeable_identities,
                    written_occurrence(
                        WorldDirection::Export,
                        key,
                        ElabKind::Func(*function),
                        *owner,
                        location.clone(),
                    ),
                ),
                WorldElaborationSourceItem::Include {
                    target,
                    owner,
                    location,
                } => {
                    if local_indices.contains_key(target) && input.cyclic_worlds.contains(target) {
                        continue;
                    }
                    if missing_cycle_edges.contains(&(world_id, *target)) {
                        continue;
                    }
                    // Repeating the same include contributes no new item
                    // identity; retain provenance from the first include.
                    if !expanded_include_targets.insert(*target) {
                        continue;
                    }
                    if let Some(&included_index) = local_indices.get(target) {
                        let included = results
                            .get(&input.local_worlds[included_index].world)
                            .expect("local include target precedes its parent in postorder");
                        complete &= included.complete;
                        let rebase = Arc::new((*owner, location.clone()));
                        walk_segments(&included.segments, Some(rebase), |occurrences, _| {
                            if !expanded_segments.insert(Arc::as_ptr(occurrences) as usize) {
                                return;
                            }
                            for occurrence in occurrences.iter() {
                                append_included_occurrence(
                                    &mut ordered_items,
                                    &mut mergeable_identities,
                                    occurrence,
                                    *owner,
                                    location,
                                );
                            }
                        });
                    } else if let Some(&included_index) = external_indices.get(target) {
                        let external = &input.external_worlds[included_index];
                        complete &= external.complete;
                        let sequence = external_sequences
                            .entry(included_index)
                            .or_insert_with(|| external_sequence(external));
                        let rebase = Arc::new((*owner, location.clone()));
                        walk_segments(&sequence.segments, Some(rebase), |occurrences, _| {
                            if !expanded_segments.insert(Arc::as_ptr(occurrences) as usize) {
                                return;
                            }
                            for occurrence in occurrences.iter() {
                                append_included_occurrence(
                                    &mut ordered_items,
                                    &mut mergeable_identities,
                                    occurrence,
                                    *owner,
                                    location,
                                );
                            }
                        });
                    } else {
                        complete = false;
                    }
                }
            }
        }

        let reporting_locations = reporting_locations(&ordered_items);
        let mut export_expanded_interfaces = HashSet::new();
        append_implicit_imports(
            &mut ordered_items,
            &mut mergeable_identities,
            &input.interfaces,
            &interface_indices,
            &interface_uses,
            &reporting_locations,
            &mut export_expanded_interfaces,
            &mut complete,
            true,
        );
        let mut import_expanded_interfaces = HashSet::new();
        append_implicit_imports(
            &mut ordered_items,
            &mut mergeable_identities,
            &input.interfaces,
            &interface_indices,
            &interface_uses,
            &reporting_locations,
            &mut import_expanded_interfaces,
            &mut complete,
            false,
        );

        #[cfg(test)]
        note_normalized_item_count(ordered_items.len());
        let (normalized, diagnostics) = normalize_items(
            &input.package,
            ordered_items,
            complete,
            &input.interfaces,
            &interface_indices,
        );
        let mut key_index = PersistentWorldKeys::default();
        for occurrence in &normalized {
            key_index.insert(WorldItemKey {
                direction: occurrence.direction,
                key: occurrence.item.key.clone(),
            });
        }
        let segments = occurrence_segment(normalized);
        results.insert(
            world_id,
            LocalResult {
                segments,
                key_index,
                diagnostics,
                complete,
            },
        );
    }

    results
        .remove(&world)
        .map(|result| {
            let mut ordered_items = Vec::new();
            walk_segments(&result.segments, None, |occurrences, rebase| match rebase {
                Some(rebase) => ordered_items.extend(
                    occurrences
                        .iter()
                        .map(|occurrence| included_occurrence(occurrence, rebase.0, &rebase.1)),
                ),
                None => ordered_items.extend(occurrences.iter().cloned()),
            });
            build_output(world, ordered_items, result.diagnostics, result.complete)
        })
        .ok_or(WorldElaborationError { world })
}

fn fast_include_with_functions(
    local_world: &WorldElaborationLocalWorld,
    world_id: DefId,
    input: &WorldElaborationInput,
    local_indices: &HashMap<DefId, usize>,
    external_indices: &HashMap<DefId, usize>,
    missing_cycle_edges: &HashSet<(DefId, DefId)>,
    missing_cycle_sources: &HashSet<DefId>,
    results: &HashMap<DefId, LocalResult>,
    external_sequences: &mut HashMap<usize, ExternalSequence>,
) -> Option<LocalResult> {
    if !local_world.complete || missing_cycle_sources.contains(&world_id) {
        return None;
    }

    let mut include = None;
    let mut direct_keys = HashSet::new();
    for item in &local_world.items {
        match item {
            WorldElaborationSourceItem::Include {
                target,
                owner,
                location,
            } if include.is_none() => {
                include = Some((*target, *owner, location.clone()));
            }
            WorldElaborationSourceItem::ImportFunction { key, .. } => {
                if !direct_keys.insert(WorldItemKey {
                    direction: WorldDirection::Import,
                    key: key.clone(),
                }) {
                    return None;
                }
            }
            WorldElaborationSourceItem::ExportFunction { key, .. } => {
                if !direct_keys.insert(WorldItemKey {
                    direction: WorldDirection::Export,
                    key: key.clone(),
                }) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let (target, owner, location) = include?;
    if local_indices.contains_key(&target) && input.cyclic_worlds.contains(&target)
        || missing_cycle_edges.contains(&(world_id, target))
    {
        return None;
    }

    let (included_segments, mut key_index, included_complete) =
        if let Some(&included_index) = local_indices.get(&target) {
            let included = results.get(&input.local_worlds[included_index].world)?;
            (
                included.segments.clone(),
                included.key_index.clone(),
                included.complete,
            )
        } else if let Some(&included_index) = external_indices.get(&target) {
            let external = &input.external_worlds[included_index];
            let sequence = external_sequences
                .entry(included_index)
                .or_insert_with(|| external_sequence(external));
            (
                sequence.segments.clone(),
                sequence.key_index.clone(),
                external.complete,
            )
        } else {
            return None;
        };
    if !included_complete {
        return None;
    }

    if direct_keys.iter().any(|key| key_index.contains(key)) {
        return None;
    }

    if direct_keys.is_empty() {
        let segments = apply_segment_rebase(&Some(Arc::new((owner, location))), included_segments);
        #[cfg(test)]
        assert_segment_invariants(&segments);
        return Some(LocalResult {
            segments,
            key_index,
            diagnostics: Vec::new(),
            complete: true,
        });
    }

    for key in &direct_keys {
        key_index.insert(key.clone());
    }
    let include_view = SegmentView {
        sequence: Arc::clone(&included_segments.sequence),
        rebase: Some(Arc::new((owner, location.clone()))),
    };
    let mut source_ordered_segments = empty_segment();
    let mut pending_direct_items = Vec::new();
    for item in &local_world.items {
        match item {
            WorldElaborationSourceItem::ImportFunction {
                key,
                function,
                owner,
                location,
            } => pending_direct_items.push(written_occurrence(
                WorldDirection::Import,
                key,
                ElabKind::Func(*function),
                *owner,
                location.clone(),
            )),
            WorldElaborationSourceItem::ExportFunction {
                key,
                function,
                owner,
                location,
            } => pending_direct_items.push(written_occurrence(
                WorldDirection::Export,
                key,
                ElabKind::Func(*function),
                *owner,
                location.clone(),
            )),
            WorldElaborationSourceItem::Include { .. } => {
                if !pending_direct_items.is_empty() {
                    source_ordered_segments = concat_segments(
                        source_ordered_segments,
                        occurrence_segment(std::mem::take(&mut pending_direct_items)),
                    );
                }
                source_ordered_segments =
                    concat_segments(source_ordered_segments, include_view.clone());
            }
            _ => unreachable!("the fast-path validation accepts only functions and one include"),
        }
    }
    if !pending_direct_items.is_empty() {
        source_ordered_segments = concat_segments(
            source_ordered_segments,
            occurrence_segment(pending_direct_items),
        );
    }
    #[cfg(test)]
    assert_segment_invariants(&source_ordered_segments);
    Some(LocalResult {
        segments: source_ordered_segments,
        key_index,
        diagnostics: Vec::new(),
        complete: true,
    })
}

fn empty_segment() -> SegmentView {
    SegmentView {
        sequence: Arc::new(SegmentSequence::Empty),
        rebase: None,
    }
}

fn occurrence_segment(occurrences: Vec<Occurrence>) -> SegmentView {
    if occurrences.is_empty() {
        empty_segment()
    } else {
        SegmentView {
            sequence: Arc::new(SegmentSequence::Leaf(Arc::new(occurrences))),
            rebase: None,
        }
    }
}

fn first_local_indices(worlds: &[WorldElaborationLocalWorld]) -> HashMap<DefId, usize> {
    let mut indices = HashMap::with_capacity(worlds.len());
    for (index, world) in worlds.iter().enumerate() {
        indices.entry(world.world).or_insert(index);
    }
    indices
}

fn first_external_indices(worlds: &[ExternalWorld]) -> HashMap<DefId, usize> {
    let mut indices = HashMap::with_capacity(worlds.len());
    for (index, world) in worlds.iter().enumerate() {
        indices.entry(world.world).or_insert(index);
    }
    indices
}

fn first_interface_indices(interfaces: &[WorldElaborationInterface]) -> HashMap<DefId, usize> {
    let mut indices = HashMap::with_capacity(interfaces.len());
    for (index, interface) in interfaces.iter().enumerate() {
        indices.entry(interface.interface).or_insert(index);
    }
    indices
}

fn external_sequence(world: &ExternalWorld) -> ExternalSequence {
    let occurrences = world
        .items
        .iter()
        .map(|external_item| {
            let mut item = external_item.item.clone();
            item.origin = ElabOrigin::External;
            Occurrence {
                direction: external_item.direction,
                item,
                location: None,
            }
        })
        .collect::<Vec<_>>();
    let mut key_index = PersistentWorldKeys::default();
    for occurrence in &occurrences {
        key_index.insert(WorldItemKey {
            direction: occurrence.direction,
            key: occurrence.item.key.clone(),
        });
    }
    ExternalSequence {
        segments: occurrence_segment(occurrences),
        key_index,
    }
}

fn interface_use_adjacency(uses: &[InterfaceUse]) -> HashMap<DefId, Vec<DefId>> {
    let mut adjacency = HashMap::<DefId, Vec<DefId>>::new();
    let mut seen = HashMap::<DefId, HashSet<DefId>>::new();
    for use_edge in uses {
        if seen.entry(use_edge.from).or_default().insert(use_edge.to) {
            adjacency
                .entry(use_edge.from)
                .or_default()
                .push(use_edge.to);
        }
    }
    adjacency
}

fn local_postorder(
    root_index: usize,
    local_worlds: &[WorldElaborationLocalWorld],
    local_indices: &HashMap<DefId, usize>,
    cyclic_worlds: &HashSet<DefId>,
) -> (Vec<usize>, HashSet<(DefId, DefId)>) {
    let mut state = vec![0u8; local_worlds.len()];
    let mut postorder = Vec::new();
    let mut frames = vec![(root_index, 0usize)];
    let mut missing_cycle_edges = HashSet::new();
    state[root_index] = 1;

    loop {
        let Some(&(current_index, next_item)) = frames.last() else {
            break;
        };
        let current = &local_worlds[current_index];
        let mut next_local_child = None;
        for item_index in next_item..current.items.len() {
            let WorldElaborationSourceItem::Include { target, .. } = &current.items[item_index]
            else {
                continue;
            };
            if cyclic_worlds.contains(target) && local_indices.contains_key(target) {
                continue;
            }
            let Some(&target_index) = local_indices.get(target) else {
                continue;
            };
            match state[target_index] {
                0 => {
                    next_local_child = Some((target_index, item_index + 1));
                    break;
                }
                1 => {
                    // Correct callers supply the SCC result. This guard keeps
                    // a stale/malformed cycle set finite and defers the
                    // affected local world's duplicate diagnostics.
                    missing_cycle_edges.insert((current.world, local_worlds[target_index].world));
                }
                _ => {}
            }
        }
        if let Some((child_index, parent_next_item)) = next_local_child {
            if let Some(frame) = frames.last_mut() {
                frame.1 = parent_next_item;
            }
            state[child_index] = 1;
            frames.push((child_index, 0));
            continue;
        }
        let (finished_index, _) = frames.pop().expect("a frame was just inspected");
        state[finished_index] = 2;
        postorder.push(finished_index);
    }
    (postorder, missing_cycle_edges)
}

fn written_occurrence(
    direction: WorldDirection,
    key: &SmolStr,
    kind: ElabKind,
    owner: HirId,
    location: Location,
) -> Occurrence {
    Occurrence {
        direction,
        item: ElabItem {
            key: key.clone(),
            kind,
            origin: ElabOrigin::Written(owner),
        },
        location: Some(location),
    }
}

fn included_occurrence(occurrence: &Occurrence, include: HirId, location: &Location) -> Occurrence {
    Occurrence {
        direction: occurrence.direction,
        item: ElabItem {
            key: occurrence.item.key.clone(),
            kind: occurrence.item.kind,
            origin: ElabOrigin::Included { include },
        },
        location: Some(location.clone()),
    }
}

fn occurrence_identity_key(
    direction: WorldDirection,
    item: &ElabItem,
) -> (WorldDirection, SmolStr, ElabKind) {
    (direction, item.key.clone(), item.kind)
}

fn push_occurrence(
    ordered_items: &mut Vec<Occurrence>,
    mergeable_identities: &mut HashSet<(WorldDirection, SmolStr, ElabKind)>,
    occurrence: Occurrence,
) {
    let first = mergeable_identities.insert(occurrence_identity_key(
        occurrence.direction,
        &occurrence.item,
    ));
    if first || !is_include_or_implicit(occurrence.item.origin) {
        ordered_items.push(occurrence);
    }
}

fn append_included_occurrence(
    ordered_items: &mut Vec<Occurrence>,
    mergeable_identities: &mut HashSet<(WorldDirection, SmolStr, ElabKind)>,
    occurrence: &Occurrence,
    include: HirId,
    location: &Location,
) {
    if !mergeable_identities.insert(occurrence_identity_key(
        occurrence.direction,
        &occurrence.item,
    )) {
        return;
    }
    ordered_items.push(included_occurrence(occurrence, include, location));
}

fn append_implicit_imports(
    ordered_items: &mut Vec<Occurrence>,
    mergeable_identities: &mut HashSet<(WorldDirection, SmolStr, ElabKind)>,
    interfaces: &[WorldElaborationInterface],
    interface_indices: &HashMap<DefId, usize>,
    adjacency: &HashMap<DefId, Vec<DefId>>,
    reporting_locations: &HashMap<HirId, Option<Location>>,
    expanded_interfaces: &mut HashSet<DefId>,
    complete: &mut bool,
    export_phase: bool,
) {
    // The caller creates a fresh set for each phase and shares it across that
    // phase's ordered roots, so common suffixes keep their first provenance.
    let exported = ordered_items
        .iter()
        .filter_map(|occurrence| {
            (occurrence.direction == WorldDirection::Export).then_some(occurrence.item.kind)
        })
        .filter_map(|kind| match kind {
            ElabKind::Interface(interface) => Some(interface),
            ElabKind::Func(_) => None,
        })
        .collect::<HashSet<_>>();

    let roots = ordered_items
        .iter()
        .filter(|occurrence| {
            occurrence.direction
                == if export_phase {
                    WorldDirection::Export
                } else {
                    WorldDirection::Import
                }
        })
        .filter_map(|occurrence| {
            let ElabKind::Interface(interface) = occurrence.item.kind else {
                return None;
            };
            let cause = match occurrence.item.origin {
                ElabOrigin::Written(owner) => owner,
                ElabOrigin::Included { include } => include,
                ElabOrigin::Implicit { cause, .. } => cause,
                ElabOrigin::External => return None,
            };
            Some((interface, cause))
        })
        .collect::<Vec<_>>();

    for (root_interface, cause) in roots {
        if !expanded_interfaces.insert(root_interface) {
            continue;
        }
        let Some(&root_metadata_index) = interface_indices.get(&root_interface) else {
            *complete = false;
            continue;
        };
        if !interfaces[root_metadata_index].uses_complete {
            *complete = false;
        }

        let mut seen = HashSet::from([root_interface]);
        let mut stack = adjacency
            .get(&root_interface)
            .into_iter()
            .flatten()
            .rev()
            .map(|target| (*target, root_interface))
            .collect::<Vec<_>>();

        while let Some((interface, used_by)) = stack.pop() {
            if !seen.insert(interface) || !expanded_interfaces.insert(interface) {
                continue;
            }
            let Some(&metadata_index) = interface_indices.get(&interface) else {
                *complete = false;
                continue;
            };
            let metadata = &interfaces[metadata_index];
            if !metadata.uses_complete {
                *complete = false;
            }

            if !export_phase || !exported.contains(&interface) {
                let location = reporting_locations.get(&cause).cloned().flatten();
                push_occurrence(
                    ordered_items,
                    mergeable_identities,
                    Occurrence {
                        direction: WorldDirection::Import,
                        item: ElabItem {
                            key: metadata.key.clone(),
                            kind: ElabKind::Interface(interface),
                            origin: ElabOrigin::Implicit { cause, used_by },
                        },
                        location,
                    },
                );
            }

            let Some(targets) = adjacency.get(&interface) else {
                continue;
            };
            stack.extend(targets.iter().rev().map(|target| (*target, interface)));
        }
    }
}

fn reporting_locations(ordered_items: &[Occurrence]) -> HashMap<HirId, Option<Location>> {
    let mut locations = HashMap::new();
    for occurrence in ordered_items {
        let cause = match occurrence.item.origin {
            ElabOrigin::Written(owner) => Some(owner),
            ElabOrigin::Included { include } => Some(include),
            ElabOrigin::Implicit { cause, .. } => Some(cause),
            ElabOrigin::External => None,
        };
        if let Some(cause) = cause {
            locations
                .entry(cause)
                .or_insert_with(|| occurrence.location.clone());
        }
    }
    locations
}

fn normalize_items(
    package: &str,
    ordered_items: Vec<Occurrence>,
    complete: bool,
    interfaces: &[WorldElaborationInterface],
    interface_indices: &HashMap<DefId, usize>,
) -> (Vec<Occurrence>, Vec<Diagnostic>) {
    if !complete {
        return (ordered_items, Vec::new());
    }

    let mut first_by_direction = HashMap::<(WorldDirection, SmolStr), usize>::new();
    let mut same_identity_occurrences =
        HashMap::<(WorldDirection, SmolStr, ElabKind), SameIdentityOccurrences>::new();
    let mut accepted_interfaces = HashMap::<DefId, (WorldDirection, usize)>::new();
    let mut accepted = vec![false; ordered_items.len()];
    let mut diagnostics = Vec::new();

    for (index, occurrence) in ordered_items.iter().enumerate() {
        let key = (occurrence.direction, occurrence.item.key.clone());
        let identity_key = (
            occurrence.direction,
            occurrence.item.key.clone(),
            occurrence.item.kind,
        );
        let merge_eligible = is_include_or_implicit(occurrence.item.origin);
        let identity_state = same_identity_occurrences.entry(identity_key).or_default();
        let direct_duplicate = !merge_eligible && identity_state.first_direct_index.is_some();
        let first_direct_index = identity_state.first_direct_index;
        let merge_same_identity = identity_state.seen
            && (merge_eligible || identity_state.first_origin_is_merge_eligible);
        if !identity_state.seen {
            identity_state.first_origin_is_merge_eligible = merge_eligible;
        }
        identity_state.seen = true;
        if !merge_eligible && identity_state.first_direct_index.is_none() {
            identity_state.first_direct_index = Some(index);
        }
        if direct_duplicate {
            let first_direct =
                &ordered_items[first_direct_index.expect("direct duplicate has a prior direct")];
            diagnostics.push(duplicate_diagnostic(
                package,
                &occurrence.item.key,
                occurrence.location.clone(),
                first_direct.location.clone(),
                None,
            ));
            continue;
        }
        if merge_same_identity {
            continue;
        }

        let opposite_direction_occurrence = match occurrence.item.kind {
            ElabKind::Interface(interface) => accepted_interfaces
                .get(&interface)
                .filter(|(direction, _)| *direction != occurrence.direction)
                .copied(),
            ElabKind::Func(_) => None,
        };
        if let Some(&first_index) = first_by_direction.get(&key) {
            let first = &ordered_items[first_index];
            diagnostics.push(duplicate_diagnostic(
                package,
                &occurrence.item.key,
                occurrence.location.clone(),
                first.location.clone(),
                opposite_direction_occurrence.and_then(|_| {
                    implicit_used_by_note(&occurrence.item, interfaces, interface_indices)
                }),
            ));
            continue;
        }
        first_by_direction.insert(key, index);

        if let ElabKind::Interface(interface) = occurrence.item.kind {
            if let Some(&(earlier_direction, opposite_index)) = accepted_interfaces.get(&interface)
                && earlier_direction != occurrence.direction
            {
                let earlier = &ordered_items[opposite_index];
                diagnostics.push(duplicate_diagnostic(
                    package,
                    &occurrence.item.key,
                    occurrence.location.clone(),
                    earlier.location.clone(),
                    implicit_used_by_note(&occurrence.item, interfaces, interface_indices),
                ));
                continue;
            }
            accepted_interfaces
                .entry(interface)
                .or_insert((occurrence.direction, index));
        }
        accepted[index] = true;
    }

    let normalized = ordered_items
        .into_iter()
        .zip(accepted)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect();
    (normalized, diagnostics)
}

fn is_include_or_implicit(origin: ElabOrigin) -> bool {
    matches!(
        origin,
        ElabOrigin::Included { .. } | ElabOrigin::Implicit { .. }
    )
}

fn implicit_used_by_note(
    item: &ElabItem,
    interfaces: &[WorldElaborationInterface],
    interface_indices: &HashMap<DefId, usize>,
) -> Option<String> {
    let ElabOrigin::Implicit { used_by, .. } = item.origin else {
        return None;
    };
    let name = interface_indices
        .get(&used_by)
        .map(|index| interfaces[*index].key.as_str())
        .unwrap_or("an imported interface");
    Some(format!(
        "imported interface `{name}` uses this exported interface"
    ))
}

fn duplicate_diagnostic(
    package: &str,
    key: &str,
    primary: Option<Location>,
    earlier: Option<Location>,
    extra_note: Option<String>,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::WorldDuplicateItem,
        Severity::Error,
        format!("world item `{key}` is duplicated"),
        primary,
    );
    diagnostic.primary_label = "duplicate world item".to_owned();
    diagnostic.package = Some(package.to_owned());
    if let Some(location) = earlier {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "the earlier item is here".to_owned(),
                location: Some(location),
            })
            .expect("an unbound diagnostic accepts a note");
    }
    if let Some(message) = extra_note {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message,
                location: None,
            })
            .expect("an unbound diagnostic accepts a note");
    }
    diagnostic
}

fn build_output(
    world: DefId,
    ordered_items: Vec<Occurrence>,
    diagnostics: Vec<Diagnostic>,
    complete: bool,
) -> ElaboratedWorld {
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut item_order = Vec::with_capacity(ordered_items.len());
    for occurrence in ordered_items {
        match occurrence.direction {
            WorldDirection::Import => {
                let index = imports.len();
                imports.push(occurrence.item);
                item_order.push(ElabItemOrder {
                    direction: WorldDirection::Import,
                    index,
                });
            }
            WorldDirection::Export => {
                let index = exports.len();
                exports.push(occurrence.item);
                item_order.push(ElabItemOrder {
                    direction: WorldDirection::Export,
                    index,
                });
            }
        }
    }
    ElaboratedWorld {
        world,
        imports,
        exports,
        item_order,
        diagnostics,
        complete,
    }
}

fn external_result(world: &ExternalWorld) -> ElaboratedWorld {
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut item_order = Vec::with_capacity(world.items.len());
    for external_item in &world.items {
        let mut item = external_item.item.clone();
        item.origin = ElabOrigin::External;
        match external_item.direction {
            WorldDirection::Import => {
                let index = imports.len();
                imports.push(item);
                item_order.push(ElabItemOrder {
                    direction: WorldDirection::Import,
                    index,
                });
            }
            WorldDirection::Export => {
                let index = exports.len();
                exports.push(item);
                item_order.push(ElabItemOrder {
                    direction: WorldDirection::Export,
                    index,
                });
            }
        }
    }
    ElaboratedWorld {
        world: world.world,
        imports,
        exports,
        item_order,
        diagnostics: Vec::new(),
        complete: world.complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::{
        DefIndex, DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, ItemLocalId,
        OwnerId,
    };
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_vfs::{FileId, Span, TextRange, TextSize};

    struct Fixture {
        packages: PackageInterner,
        symbols: Interner,
        paths: DefPathInterner,
        package: sai_hir::PackageId,
    }

    impl Fixture {
        fn new() -> Self {
            let packages = PackageInterner::new();
            let package = packages
                .intern("acme:app#1.0")
                .expect("test package interns");
            let symbols = Interner::new();
            let paths = DefPathInterner::new(package, &packages, &symbols)
                .expect("definition paths initialize");
            Self {
                packages,
                symbols,
                paths,
                package,
            }
        }

        fn def(&self, index: u32) -> DefId {
            DefId::from_foreign(self.package, DefIndex::from_raw(index))
        }

        fn owner(&self, index: u32) -> OwnerId {
            let spelling = format!("world_item_{index}");
            let symbol = self.symbols.intern(&spelling).expect("owner name interns");
            let path = DefPath::new(
                self.package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::ValueNs(symbol),
                    disambiguator: index,
                }],
                &self.packages,
                &self.symbols,
            )
            .expect("owner path is valid");
            OwnerId {
                def_id: self.paths.intern(path).expect("owner path interns"),
            }
        }

        fn hir(&self, index: u32, local_id: u32) -> HirId {
            HirId {
                owner: self.owner(index),
                local_id: ItemLocalId::from_raw(local_id),
            }
        }

        fn location(&self, offset: u32) -> Location {
            Location::new(
                "src/lib.sai",
                Span::new(
                    FileId::new(1),
                    TextRange::new(TextSize::new(offset), TextSize::new(offset + 1))
                        .expect("test location range is ordered"),
                ),
            )
        }

        fn local_world(
            &self,
            world: u32,
            items: Vec<WorldElaborationSourceItem>,
        ) -> WorldElaborationLocalWorld {
            WorldElaborationLocalWorld {
                world: self.def(world),
                complete: true,
                items,
            }
        }

        fn input(
            &self,
            local_worlds: Vec<WorldElaborationLocalWorld>,
            external_worlds: Vec<ExternalWorld>,
            interfaces: Vec<WorldElaborationInterface>,
            interface_uses: Vec<InterfaceUse>,
            cyclic_worlds: impl IntoIterator<Item = DefId>,
        ) -> WorldElaborationInput {
            WorldElaborationInput {
                package: "acme:app@1.0.0".to_owned(),
                local_worlds,
                external_worlds,
                interfaces,
                interface_uses,
                cyclic_worlds: cyclic_worlds.into_iter().collect(),
            }
        }

        fn function(
            &self,
            direction: WorldDirection,
            key: &str,
            function: u32,
            owner: u32,
            offset: u32,
        ) -> WorldElaborationSourceItem {
            let key = SmolStr::new(key);
            let owner = self.hir(owner, 0);
            let location = self.location(offset);
            match direction {
                WorldDirection::Import => WorldElaborationSourceItem::ImportFunction {
                    key,
                    function: self.def(function),
                    owner,
                    location,
                },
                WorldDirection::Export => WorldElaborationSourceItem::ExportFunction {
                    key,
                    function: self.def(function),
                    owner,
                    location,
                },
            }
        }

        fn interface(
            &self,
            direction: WorldDirection,
            key: &str,
            interface: u32,
            owner: u32,
            offset: u32,
        ) -> WorldElaborationSourceItem {
            let key = SmolStr::new(key);
            let owner = self.hir(owner, 0);
            let location = self.location(offset);
            match direction {
                WorldDirection::Import => WorldElaborationSourceItem::ImportInterface {
                    key,
                    interface: self.def(interface),
                    owner,
                    location,
                },
                WorldDirection::Export => WorldElaborationSourceItem::ExportInterface {
                    key,
                    interface: self.def(interface),
                    owner,
                    location,
                },
            }
        }

        fn include(&self, target: u32, owner: u32, offset: u32) -> WorldElaborationSourceItem {
            WorldElaborationSourceItem::Include {
                target: self.def(target),
                owner: self.hir(owner, 0),
                location: self.location(offset),
            }
        }

        fn interface_metadata(&self, interface: u32, key: &str) -> WorldElaborationInterface {
            WorldElaborationInterface {
                interface: self.def(interface),
                key: SmolStr::new(key),
                uses_complete: true,
            }
        }
    }

    #[test]
    fn expands_nested_includes_depth_first_at_the_include_position() {
        let fixture = Fixture::new();
        let root_include = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "root_first", 101, 11, 1),
                fixture.include(2, 10, 10),
                fixture.function(WorldDirection::Export, "root_last", 102, 12, 20),
            ],
        );
        let child = fixture.local_world(
            2,
            vec![
                fixture.function(WorldDirection::Import, "child_first", 201, 21, 30),
                fixture.include(3, 20, 40),
                fixture.function(WorldDirection::Export, "child_last", 202, 22, 50),
            ],
        );
        let grandchild = fixture.local_world(
            3,
            vec![fixture.function(WorldDirection::Import, "grandchild", 301, 31, 60)],
        );
        let input = fixture.input(vec![root, child, grandchild], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["root_first", "child_first", "grandchild"]
        );
        assert_eq!(
            result
                .exports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["child_last", "root_last"]
        );
        assert_eq!(
            result.item_order,
            [
                ElabItemOrder {
                    direction: WorldDirection::Import,
                    index: 0,
                },
                ElabItemOrder {
                    direction: WorldDirection::Import,
                    index: 1,
                },
                ElabItemOrder {
                    direction: WorldDirection::Import,
                    index: 2,
                },
                ElabItemOrder {
                    direction: WorldDirection::Export,
                    index: 0,
                },
                ElabItemOrder {
                    direction: WorldDirection::Export,
                    index: 1,
                },
            ]
        );
        assert_eq!(
            result.imports[1].origin,
            ElabOrigin::Included {
                include: root_include
            }
        );
        assert_eq!(
            result.imports[2].origin,
            ElabOrigin::Included {
                include: root_include
            },
            "nested include provenance is reported at the outer include"
        );
        assert!(result.diagnostics.is_empty());
        assert!(result.complete);
    }

    #[test]
    fn implicit_imports_follow_export_then_import_dfs_first_reference_order() {
        let fixture = Fixture::new();
        let export_cause = fixture.hir(10, 0);
        let import_cause = fixture.hir(11, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.interface(WorldDirection::Export, "exported", 1, 10, 1),
                fixture.interface(WorldDirection::Import, "imported", 2, 11, 2),
            ],
        );
        let interfaces = vec![
            fixture.interface_metadata(1, "exported"),
            fixture.interface_metadata(2, "imported"),
            fixture.interface_metadata(3, "first-use"),
            fixture.interface_metadata(4, "nested-use"),
            fixture.interface_metadata(5, "second-use"),
            fixture.interface_metadata(6, "import-use"),
        ];
        let uses = vec![
            InterfaceUse {
                from: fixture.def(1),
                to: fixture.def(3),
            },
            InterfaceUse {
                from: fixture.def(1),
                to: fixture.def(5),
            },
            InterfaceUse {
                from: fixture.def(3),
                to: fixture.def(4),
            },
            InterfaceUse {
                from: fixture.def(2),
                to: fixture.def(6),
            },
        ];
        let input = fixture.input(vec![root], vec![], interfaces, uses, []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            [
                "imported",
                "first-use",
                "nested-use",
                "second-use",
                "import-use"
            ]
        );
        assert_eq!(
            result.imports[1].origin,
            ElabOrigin::Implicit {
                cause: export_cause,
                used_by: fixture.def(1),
            }
        );
        assert_eq!(
            result.imports[2].origin,
            ElabOrigin::Implicit {
                cause: export_cause,
                used_by: fixture.def(3),
            }
        );
        assert_eq!(
            result.imports[4].origin,
            ElabOrigin::Implicit {
                cause: import_cause,
                used_by: fixture.def(2),
            }
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn repeated_implicit_use_closures_report_a_conflicting_identity_once() {
        let fixture = Fixture::new();
        let root = fixture.local_world(
            1,
            vec![
                fixture.interface(WorldDirection::Export, "x", 1, 10, 1),
                fixture.function(WorldDirection::Import, "b", 20, 11, 2),
            ],
        );
        let interfaces = vec![
            fixture.interface_metadata(1, "x"),
            fixture.interface_metadata(2, "a"),
            fixture.interface_metadata(3, "b"),
        ];
        let uses = vec![
            InterfaceUse {
                from: fixture.def(1),
                to: fixture.def(2),
            },
            InterfaceUse {
                from: fixture.def(2),
                to: fixture.def(3),
            },
        ];
        let input = fixture.input(vec![root], vec![], interfaces, uses, []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.primary(), Some(&fixture.location(1)));
        assert_eq!(
            diagnostic.children()[0].location.as_ref(),
            Some(&fixture.location(2))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .all(|child| !child.message.contains("uses this exported interface"))
        );
    }

    #[test]
    fn same_direction_implicit_name_conflicts_do_not_claim_a_cross_direction_use() {
        let fixture = Fixture::new();
        let import_cause = fixture.hir(11, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "b", 20, 10, 1),
                fixture.interface(WorldDirection::Import, "a", 1, 11, 11),
            ],
        );
        let interfaces = vec![
            fixture.interface_metadata(1, "a"),
            fixture.interface_metadata(2, "b"),
        ];
        let input = fixture.input(
            vec![root],
            vec![],
            interfaces,
            vec![InterfaceUse {
                from: fixture.def(1),
                to: fixture.def(2),
            }],
            [],
        );

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.primary(), Some(&fixture.location(11)));
        assert_eq!(
            diagnostic.children()[0].location.as_ref(),
            Some(&fixture.location(1))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .all(|child| !child.message.contains("uses this exported interface"))
        );
        assert_eq!(result.imports[1].origin, ElabOrigin::Written(import_cause));
    }

    #[test]
    fn long_implicit_use_chains_expand_each_interface_only_once() {
        let fixture = Fixture::new();
        let chain_length = 128u32;
        let interfaces = (0..=chain_length)
            .map(|index| fixture.interface_metadata(index, &format!("iface-{index}")))
            .collect::<Vec<_>>();
        let uses = (0..chain_length)
            .map(|index| InterfaceUse {
                from: fixture.def(index),
                to: fixture.def(index + 1),
            })
            .collect::<Vec<_>>();
        let indices = first_interface_indices(&interfaces);
        let adjacency = interface_use_adjacency(&uses);
        let mut ordered_items = vec![Occurrence {
            direction: WorldDirection::Export,
            item: ElabItem {
                key: SmolStr::new("iface-0"),
                kind: ElabKind::Interface(fixture.def(0)),
                origin: ElabOrigin::Written(fixture.hir(10, 0)),
            },
            location: Some(fixture.location(1)),
        }];
        let location_index = reporting_locations(&ordered_items);
        let mut export_expanded_interfaces = HashSet::new();
        let mut import_expanded_interfaces = HashSet::new();
        let mut mergeable_identities = HashSet::new();
        let mut complete = true;

        append_implicit_imports(
            &mut ordered_items,
            &mut mergeable_identities,
            &interfaces,
            &indices,
            &adjacency,
            &location_index,
            &mut export_expanded_interfaces,
            &mut complete,
            true,
        );
        append_implicit_imports(
            &mut ordered_items,
            &mut mergeable_identities,
            &interfaces,
            &indices,
            &adjacency,
            &location_index,
            &mut import_expanded_interfaces,
            &mut complete,
            false,
        );

        assert!(complete);
        assert_eq!(export_expanded_interfaces.len(), chain_length as usize + 1);
        assert_eq!(import_expanded_interfaces.len(), chain_length as usize);
        assert_eq!(ordered_items.len(), chain_length as usize + 1);
        assert_eq!(
            ordered_items[1..]
                .iter()
                .map(|occurrence| occurrence.item.key.to_string())
                .collect::<Vec<_>>(),
            (1..=chain_length)
                .map(|index| format!("iface-{index}"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sparse_export_roots_keep_linear_output_and_each_cause_location() {
        let fixture = Fixture::new();
        let root_count = 512u32;
        let mut items = Vec::with_capacity(root_count as usize);
        let mut interfaces = Vec::with_capacity(root_count as usize * 2);
        let mut uses = Vec::with_capacity(root_count as usize);

        for index in 0..root_count {
            let root_key = format!("root-{index}");
            let target_key = format!("target-{index}");
            items.push(fixture.interface(
                WorldDirection::Export,
                &root_key,
                index,
                1000 + index,
                index,
            ));
            interfaces.push(fixture.interface_metadata(index, &root_key));
            interfaces.push(fixture.interface_metadata(root_count + index, &target_key));
            uses.push(InterfaceUse {
                from: fixture.def(index),
                to: fixture.def(root_count + index),
            });
        }
        let root = fixture.local_world(1, items);
        let input = fixture.input(vec![root], vec![], interfaces, uses, []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.exports.len(), root_count as usize);
        assert_eq!(result.imports.len(), root_count as usize);
        assert_eq!(result.item_order.len(), root_count as usize * 2);
        for (index, item) in result.imports.iter().enumerate() {
            assert_eq!(item.key, format!("target-{index}"));
            assert_eq!(
                item.origin,
                ElabOrigin::Implicit {
                    cause: fixture.hir(1000 + index as u32, 0),
                    used_by: fixture.def(index as u32),
                }
            );
        }
    }

    #[test]
    fn fanout_include_coalescing_retains_only_first_shared_descendant_items() {
        let fixture = Fixture::new();
        let fanout = 128u32;
        let item_count = 128u32;
        let leaf_items = (0..item_count)
            .map(|index| {
                written_occurrence(
                    WorldDirection::Import,
                    &SmolStr::new(format!("leaf-{index}")),
                    ElabKind::Func(fixture.def(10_000 + index)),
                    fixture.hir(20_000 + index, 0),
                    fixture.location(index),
                )
            })
            .collect::<Vec<_>>();
        let mut included_items = Vec::new();
        let mut mergeable_identities = HashSet::new();

        for wrapper in 0..fanout {
            let include = fixture.hir(30_000 + wrapper, 0);
            let location = fixture.location(30_000 + wrapper);
            for item in &leaf_items {
                append_included_occurrence(
                    &mut included_items,
                    &mut mergeable_identities,
                    item,
                    include,
                    &location,
                );
            }
        }

        assert_eq!(included_items.len(), item_count as usize);
        assert_eq!(mergeable_identities.len(), item_count as usize);
        assert_eq!(
            included_items[0].item.origin,
            ElabOrigin::Included {
                include: fixture.hir(30_000, 0)
            }
        );
        assert_eq!(included_items[0].location, Some(fixture.location(30_000)));
    }

    #[test]
    fn merges_the_same_identity_when_an_include_contributes_it() {
        let fixture = Fixture::new();
        let direct_owner = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "run", 100, 10, 1),
                fixture.include(2, 11, 2),
            ],
        );
        let included = fixture.local_world(
            2,
            vec![fixture.function(WorldDirection::Import, "run", 100, 20, 20)],
        );
        let input = fixture.input(vec![root, included], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.imports.len(), 1);
        assert_eq!(
            result.imports[0].origin,
            ElabOrigin::Written(direct_owner),
            "the first position and origin are retained"
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn reports_a_different_identity_duplicate_at_the_include_position() {
        let fixture = Fixture::new();
        let root_include = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.include(2, 10, 10),
                fixture.function(WorldDirection::Import, "run", 101, 11, 20),
            ],
        );
        let included = fixture.local_world(
            2,
            vec![fixture.function(WorldDirection::Import, "run", 102, 20, 30)],
        );
        let input = fixture.input(vec![root, included], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::WorldDuplicateItem);
        assert_eq!(diagnostic.primary(), Some(&fixture.location(20)));
        assert_eq!(
            diagnostic.children()[0].location.as_ref(),
            Some(&fixture.location(10))
        );
        assert_eq!(
            result.imports[0].origin,
            ElabOrigin::Included {
                include: root_include
            }
        );
    }

    #[test]
    fn reports_later_direct_duplicate_and_keeps_first_item() {
        let fixture = Fixture::new();
        let first_owner = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "run", 100, 10, 1),
                fixture.function(WorldDirection::Import, "run", 100, 11, 5),
            ],
        );
        let input = fixture.input(vec![root], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].origin, ElabOrigin::Written(first_owner));
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, Code::WorldDuplicateItem);
        assert_eq!(result.diagnostics[0].primary(), Some(&fixture.location(5)));
        assert_eq!(
            result.diagnostics[0].children()[0].location.as_ref(),
            Some(&fixture.location(1))
        );
    }

    #[test]
    fn direct_duplicates_still_report_after_a_merge_eligible_occurrence() {
        let fixture = Fixture::new();
        let first_owner = fixture.hir(10, 0);
        let middle_origins = [
            ElabOrigin::Included {
                include: fixture.hir(11, 0),
            },
            ElabOrigin::Implicit {
                cause: fixture.hir(12, 0),
                used_by: fixture.def(12),
            },
        ];

        for middle_origin in middle_origins {
            let occurrences = vec![
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: ElabOrigin::Written(first_owner),
                    },
                    location: Some(fixture.location(0)),
                },
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: middle_origin,
                    },
                    location: Some(fixture.location(1)),
                },
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: ElabOrigin::Written(fixture.hir(13, 0)),
                    },
                    location: Some(fixture.location(2)),
                },
            ];
            let (normalized, diagnostics) =
                normalize_items("acme:app@1.0.0", occurrences, true, &[], &HashMap::new());

            assert_eq!(normalized.len(), 1);
            assert_eq!(normalized[0].item.origin, ElabOrigin::Written(first_owner));
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].primary(), Some(&fixture.location(2)));
        }
    }

    #[test]
    fn direct_duplicates_report_after_initial_included_or_implicit_identity() {
        let fixture = Fixture::new();
        let special_origins = [
            ElabOrigin::Included {
                include: fixture.hir(11, 0),
            },
            ElabOrigin::Implicit {
                cause: fixture.hir(12, 0),
                used_by: fixture.def(12),
            },
        ];

        for special_origin in special_origins {
            let occurrences = vec![
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: special_origin,
                    },
                    location: Some(fixture.location(0)),
                },
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: ElabOrigin::Written(fixture.hir(13, 0)),
                    },
                    location: Some(fixture.location(1)),
                },
                Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("shared"),
                        kind: ElabKind::Interface(fixture.def(100)),
                        origin: ElabOrigin::Written(fixture.hir(14, 0)),
                    },
                    location: Some(fixture.location(2)),
                },
            ];
            let (normalized, diagnostics) =
                normalize_items("acme:app@1.0.0", occurrences, true, &[], &HashMap::new());

            assert_eq!(normalized.len(), 1);
            assert_eq!(normalized[0].item.origin, special_origin);
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].primary(), Some(&fixture.location(2)));
            assert_eq!(
                diagnostics[0].children()[0].location.as_ref(),
                Some(&fixture.location(1))
            );
        }
    }

    #[test]
    fn large_direct_duplicate_run_keeps_one_item_and_one_diagnostic_per_repeat() {
        let fixture = Fixture::new();
        let duplicate_count = 4096u32;
        let key = SmolStr::new("run");
        let occurrences = (0..duplicate_count)
            .map(|index| {
                written_occurrence(
                    WorldDirection::Import,
                    &key,
                    ElabKind::Func(fixture.def(100)),
                    fixture.hir(1000 + index, 0),
                    fixture.location(index),
                )
            })
            .collect();

        let (normalized, diagnostics) =
            normalize_items("acme:app@1.0.0", occurrences, true, &[], &HashMap::new());

        assert_eq!(normalized.len(), 1);
        assert_eq!(diagnostics.len(), duplicate_count as usize - 1);
        assert_eq!(diagnostics[0].primary(), Some(&fixture.location(1)));
        assert_eq!(
            diagnostics[0].children()[0].location.as_ref(),
            Some(&fixture.location(0))
        );
        assert_eq!(
            diagnostics.last().and_then(Diagnostic::primary),
            Some(&fixture.location(duplicate_count - 1))
        );
    }

    #[test]
    fn implicit_import_conflicting_with_export_reports_the_import_cause() {
        let fixture = Fixture::new();
        let import_cause = fixture.hir(11, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.interface(WorldDirection::Export, "exported", 1, 10, 1),
                fixture.interface(WorldDirection::Import, "importer", 2, 11, 11),
            ],
        );
        let interfaces = vec![
            fixture.interface_metadata(1, "exported"),
            fixture.interface_metadata(2, "importer"),
        ];
        let input = fixture.input(
            vec![root],
            vec![],
            interfaces,
            vec![InterfaceUse {
                from: fixture.def(2),
                to: fixture.def(1),
            }],
            [],
        );

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.exports.len(), 1);
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::WorldDuplicateItem);
        assert_eq!(diagnostic.primary(), Some(&fixture.location(11)));
        assert_eq!(
            diagnostic.children()[0].location.as_ref(),
            Some(&fixture.location(1))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message.contains("importer"))
        );
        assert_eq!(result.imports[0].origin, ElabOrigin::Written(import_cause));
    }

    #[test]
    fn cyclic_targets_are_omitted_and_diamond_includes_merge_first_dfs_item() {
        let fixture = Fixture::new();
        let a_include = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "keep", 101, 9, 1),
                fixture.include(2, 10, 2),
                fixture.include(3, 11, 3),
                fixture.include(5, 12, 4),
            ],
        );
        let a = fixture.local_world(2, vec![fixture.include(4, 20, 20)]);
        let b = fixture.local_world(3, vec![fixture.include(4, 30, 30)]);
        let shared = fixture.local_world(
            4,
            vec![fixture.function(WorldDirection::Import, "shared", 404, 40, 40)],
        );
        let cyclic = fixture.local_world(
            5,
            vec![
                fixture.include(1, 50, 50),
                fixture.function(WorldDirection::Import, "cycle_only", 505, 51, 51),
            ],
        );
        let input = fixture.input(
            vec![root, a, b, shared, cyclic],
            vec![],
            vec![],
            vec![],
            [fixture.def(1), fixture.def(5)],
        );

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["keep", "shared"]
        );
        assert_eq!(
            result.imports[1].origin,
            ElabOrigin::Included { include: a_include }
        );
        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn an_unreported_include_cycle_recovers_without_following_the_back_edge() {
        let fixture = Fixture::new();
        let root = fixture.local_world(
            1,
            vec![
                fixture.include(2, 10, 10),
                fixture.function(WorldDirection::Export, "root", 101, 11, 11),
            ],
        );
        let child = fixture.local_world(
            2,
            vec![
                fixture.include(1, 20, 20),
                fixture.function(WorldDirection::Import, "child", 201, 21, 21),
            ],
        );
        let input = fixture.input(vec![root, child], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["child"]
        );
        assert_eq!(
            result
                .exports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["root"]
        );
        assert!(!result.complete);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn repeated_includes_expand_one_copy_at_the_first_include_location() {
        let fixture = Fixture::new();
        let include_count = 512u32;
        let item_count = 512u32;
        let first_include = fixture.hir(10_000, 0);
        let root = fixture.local_world(
            1,
            (0..include_count)
                .map(|index| fixture.include(2, 10_000 + index, index))
                .collect(),
        );
        let child = fixture.local_world(
            2,
            (0..item_count)
                .map(|index| {
                    fixture.function(
                        WorldDirection::Import,
                        &format!("child-{index}"),
                        1000 + index,
                        20_000 + index,
                        1000 + index,
                    )
                })
                .collect(),
        );
        let input = fixture.input(vec![root, child], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.imports.len(), item_count as usize);
        assert_eq!(result.item_order.len(), item_count as usize);
        assert_eq!(
            result.imports[0].origin,
            ElabOrigin::Included {
                include: first_include
            }
        );
        assert_eq!(
            result.imports.last().map(|item| item.key.as_str()),
            Some("child-511")
        );
    }

    #[test]
    fn shared_leaf_fanout_keeps_first_order_and_include_provenance() {
        let fixture = Fixture::new();
        let fanout = 64u32;
        let item_count = 64u32;
        let leaf_world = 1000;
        let first_wrapper_include = fixture.hir(2000, 0);
        let mut worlds = vec![
            fixture.local_world(
                1,
                (0..fanout)
                    .map(|index| fixture.include(index + 2, 2000 + index, index))
                    .collect(),
            ),
        ];
        for index in 0..fanout {
            worlds.push(fixture.local_world(
                index + 2,
                vec![
                    fixture.function(
                        WorldDirection::Import,
                        &format!("wrapper-{index}"),
                        15_000 + index,
                        16_000 + index,
                        16_000 + index,
                    ),
                    fixture.include(leaf_world, 10_000 + index, 10_000 + index),
                ],
            ));
        }
        worlds.push(
            fixture.local_world(
                leaf_world,
                (0..item_count)
                    .map(|index| {
                        fixture.function(
                            WorldDirection::Import,
                            &format!("leaf-{index}"),
                            20_000 + index,
                            30_000 + index,
                            30_000 + index,
                        )
                    })
                    .collect(),
            ),
        );
        let input = fixture.input(worlds, vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.imports.len(), (item_count + fanout) as usize);
        let mut expected_keys = Vec::new();
        for wrapper in 0..fanout {
            expected_keys.push(format!("wrapper-{wrapper}"));
            if wrapper == 0 {
                expected_keys.extend((0..item_count).map(|index| format!("leaf-{index}")));
            }
        }
        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            expected_keys
        );
        assert!(
            result.imports[..=item_count as usize]
                .iter()
                .all(|item| item.origin
                    == ElabOrigin::Included {
                        include: first_wrapper_include
                    })
        );
        for wrapper in 1..fanout {
            assert_eq!(
                result.imports[item_count as usize + wrapper as usize].origin,
                ElabOrigin::Included {
                    include: fixture.hir(2000 + wrapper, 0)
                }
            );
        }
    }

    #[test]
    fn shared_external_fanout_keeps_wrapper_items_and_outer_provenance() {
        let fixture = Fixture::new();
        let fanout = 32u32;
        let item_count = 32u32;
        let external_world = ExternalWorld {
            world: fixture.def(1000),
            items: (0..item_count)
                .map(|index| ExternalWorldItem {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new(format!("external-{index}")),
                        kind: ElabKind::Func(fixture.def(20_000 + index)),
                        origin: ElabOrigin::External,
                    },
                })
                .collect(),
            complete: true,
        };
        let mut worlds = vec![
            fixture.local_world(
                1,
                (0..fanout)
                    .map(|index| fixture.include(index + 2, 2000 + index, index))
                    .collect(),
            ),
        ];
        for index in 0..fanout {
            worlds.push(fixture.local_world(
                index + 2,
                vec![
                    fixture.function(
                        WorldDirection::Import,
                        &format!("wrapper-{index}"),
                        15_000 + index,
                        16_000 + index,
                        16_000 + index,
                    ),
                    fixture.include(1000, 10_000 + index, 10_000 + index),
                ],
            ));
        }
        let input = fixture.input(worlds, vec![external_world], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.imports.len(), (fanout + item_count) as usize);
        let mut expected_keys = Vec::new();
        for wrapper in 0..fanout {
            expected_keys.push(format!("wrapper-{wrapper}"));
            if wrapper == 0 {
                expected_keys.extend((0..item_count).map(|index| format!("external-{index}")));
            }
        }
        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            expected_keys
        );
        for index in 0..=item_count as usize {
            assert_eq!(
                result.imports[index].origin,
                ElabOrigin::Included {
                    include: fixture.hir(2000, 0)
                }
            );
        }
        for wrapper in 1..fanout {
            assert_eq!(
                result.imports[item_count as usize + wrapper as usize].origin,
                ElabOrigin::Included {
                    include: fixture.hir(2000 + wrapper, 0)
                }
            );
        }
    }

    #[test]
    fn shared_descendant_chain_uses_bounded_normalization_and_retains_order() {
        let fixture = Fixture::new();
        let depth = 256u32;
        let item_count = 256u32;
        let leaf_world = depth + 1;
        let root_include = fixture.hir(10_000, 0);
        let mut worlds = Vec::with_capacity((depth + 1) as usize);
        for index in 0..depth {
            worlds.push(fixture.local_world(
                index + 1,
                vec![
                    fixture.function(
                        WorldDirection::Import,
                        &format!("wrapper-{index}"),
                        20_000 + index,
                        30_000 + index,
                        30_000 + index,
                    ),
                    fixture.include(index + 2, 10_000 + index, 10_000 + index),
                ],
            ));
        }
        worlds.push(
            fixture.local_world(
                leaf_world,
                (0..item_count)
                    .map(|index| {
                        fixture.function(
                            WorldDirection::Import,
                            &format!("leaf-{index}"),
                            40_000 + index,
                            50_000 + index,
                            50_000 + index,
                        )
                    })
                    .collect(),
            ),
        );
        let input = fixture.input(worlds, vec![], vec![], vec![], []);

        WORLD_NORMALIZED_ITEM_COUNT.with(|total| total.set(0));
        let result = elaborated_world(fixture.def(1), input).expect("root exists");
        let normalized_item_count = WORLD_NORMALIZED_ITEM_COUNT.with(std::cell::Cell::get);

        assert!(result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(normalized_item_count, item_count as usize);
        assert_eq!(result.imports.len(), (depth + item_count) as usize);
        assert_eq!(
            result
                .imports
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            (0..depth)
                .map(|index| format!("wrapper-{index}"))
                .chain((0..item_count).map(|index| format!("leaf-{index}")))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            result.imports[0].origin,
            ElabOrigin::Written(fixture.hir(30_000, 0))
        );
        assert!(result.imports[1..].iter().all(|item| {
            item.origin
                == ElabOrigin::Included {
                    include: root_include,
                }
        }));
    }

    #[test]
    fn many_unreported_cycle_back_edges_use_incomplete_recovery() {
        let fixture = Fixture::new();
        let child_count = 2048u32;
        let mut worlds = vec![
            fixture.local_world(
                1,
                (0..child_count)
                    .map(|index| fixture.include(index + 2, 10_000 + index, index))
                    .collect(),
            ),
        ];
        for index in 0..child_count {
            let world = index + 2;
            worlds.push(fixture.local_world(
                world,
                vec![
                    fixture.include(world, 20_000 + index, 20_000 + index),
                    fixture.function(
                        WorldDirection::Import,
                        &format!("child-{index}"),
                        30_000 + index,
                        40_000 + index,
                        40_000 + index,
                    ),
                ],
            ));
        }
        let input = fixture.input(worlds, vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(!result.complete);
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.imports.len(), child_count as usize);
    }

    #[test]
    fn external_order_is_preserved_and_origins_are_rebased_when_included() {
        let fixture = Fixture::new();
        let include = fixture.hir(10, 0);
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Export, "before", 101, 11, 1),
                fixture.include(2, 10, 10),
                fixture.function(WorldDirection::Import, "after", 102, 12, 20),
            ],
        );
        let external = ExternalWorld {
            world: fixture.def(2),
            complete: true,
            items: vec![
                ExternalWorldItem {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("external-in"),
                        kind: ElabKind::Func(fixture.def(201)),
                        origin: ElabOrigin::External,
                    },
                },
                ExternalWorldItem {
                    direction: WorldDirection::Export,
                    item: ElabItem {
                        key: SmolStr::new("external-out"),
                        kind: ElabKind::Interface(fixture.def(202)),
                        origin: ElabOrigin::External,
                    },
                },
                ExternalWorldItem {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new("external-in-2"),
                        kind: ElabKind::Func(fixture.def(203)),
                        origin: ElabOrigin::External,
                    },
                },
            ],
        };
        let input = fixture.input(vec![root], vec![external.clone()], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(
            result
                .item_order
                .iter()
                .map(|entry| (entry.direction, entry.index))
                .collect::<Vec<_>>(),
            [
                (WorldDirection::Export, 0),
                (WorldDirection::Import, 0),
                (WorldDirection::Export, 1),
                (WorldDirection::Import, 1),
                (WorldDirection::Import, 2),
            ]
        );
        assert_eq!(result.imports[0].origin, ElabOrigin::Included { include });
        assert_eq!(result.exports[1].origin, ElabOrigin::Included { include });

        let external_root = elaborated_world(
            fixture.def(2),
            fixture.input(vec![], vec![external], vec![], vec![], []),
        )
        .expect("external root exists");
        assert!(
            external_root
                .imports
                .iter()
                .all(|item| item.origin == ElabOrigin::External)
        );
        assert!(
            external_root
                .exports
                .iter()
                .all(|item| item.origin == ElabOrigin::External)
        );
        assert_eq!(
            external_root.item_order,
            [
                ElabItemOrder {
                    direction: WorldDirection::Import,
                    index: 0,
                },
                ElabItemOrder {
                    direction: WorldDirection::Export,
                    index: 0,
                },
                ElabItemOrder {
                    direction: WorldDirection::Import,
                    index: 1,
                },
            ]
        );
    }

    #[test]
    fn incomplete_world_inputs_suppress_diagnostics_that_need_a_complete_list() {
        let fixture = Fixture::new();
        let mut root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "run", 100, 10, 1),
                fixture.function(WorldDirection::Import, "run", 100, 11, 2),
            ],
        );
        root.complete = false;
        let input = fixture.input(vec![root], vec![], vec![], vec![], []);

        let result = elaborated_world(fixture.def(1), input).expect("root exists");

        assert!(!result.complete);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn repeated_elaboration_is_deterministic() {
        let fixture = Fixture::new();
        let root = fixture.local_world(
            1,
            vec![
                fixture.function(WorldDirection::Import, "first", 101, 10, 1),
                fixture.include(2, 11, 2),
                fixture.function(WorldDirection::Export, "last", 102, 12, 3),
            ],
        );
        let included = fixture.local_world(
            2,
            vec![fixture.function(WorldDirection::Import, "included", 201, 20, 20)],
        );
        let input = fixture.input(vec![root, included], vec![], vec![], vec![], []);

        let first = elaborated_world(fixture.def(1), input.clone()).expect("root exists");
        let second = elaborated_world(fixture.def(1), input).expect("root exists");

        assert_eq!(first, second);
    }

    #[test]
    fn segment_rope_stays_balanced_and_preserves_long_append_order() {
        let fixture = Fixture::new();
        let item_count = 10_000u32;
        let mut sequence = empty_segment();

        for index in 0..item_count {
            sequence = concat_segments(
                sequence,
                occurrence_segment(vec![Occurrence {
                    direction: WorldDirection::Import,
                    item: ElabItem {
                        key: SmolStr::new(format!("item-{index}")),
                        kind: ElabKind::Func(fixture.def(index)),
                        origin: ElabOrigin::External,
                    },
                    location: None,
                }]),
            );
        }

        assert_segment_invariants(&sequence);
        let height_limit = 2 * (u32::BITS - item_count.leading_zeros()) + 1;
        assert!(
            segment_height(&sequence) <= height_limit,
            "10,000 appended leaves produced rope height {}, above logarithmic bound {height_limit}",
            segment_height(&sequence)
        );

        let mut keys = Vec::new();
        walk_segments(&sequence, None, |occurrences, _| {
            keys.extend(
                occurrences
                    .iter()
                    .map(|occurrence| occurrence.item.key.to_string()),
            );
        });
        assert_eq!(keys.len(), item_count as usize);
        assert_eq!(
            keys,
            (0..item_count)
                .map(|index| format!("item-{index}"))
                .collect::<Vec<_>>()
        );
    }
}
