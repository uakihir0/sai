//! Deterministic requirement chains for MVS results and graph diagnostics.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ops::Range;

use crate::mvs::{
    MvsContract, MvsEdge, MvsNamedEdge, MvsNode, MvsPackageVersion, MvsRequirer, MvsSelectionError,
    MvsSelectionOutput,
};
use crate::mvs_graph_validation::{MvsDependencyCycle, MvsVersionConflict};
use crate::version::Version;
use crate::version_req::VersionReq;

/// Where an explanation chain begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsChainOrigin {
    Root(MvsPackageVersion),
    LockFloor(MvsPackageVersion),
}

/// One requirement edge in an explanation chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsChainLink {
    pub requirer: MvsRequirer,
    /// Whether the requirer's concrete package version is selected.
    pub requirer_selected: bool,
    /// Concrete version named by this edge, which may not be selected.
    pub named_target: MvsPackageVersion,
    pub requirement: VersionReq,
    pub span: Range<usize>,
}

/// A shortest chain from the root or a lock floor to a requirement edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsRequirementChain {
    pub origin: MvsChainOrigin,
    /// The final element is the edge being explained.
    pub links: Vec<MvsChainLink>,
}

/// Why the selected version of one node was chosen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsSelectionReason {
    RootOverridePin { chain: MvsRequirementChain },
    PathPin { chain: MvsRequirementChain },
    RequirementEdge { chain: MvsRequirementChain },
    LockFloor { package_version: MvsPackageVersion },
}

/// Invalid selection provenance supplied to chain construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsExplanationError {
    SelectionIncomplete(Vec<MvsSelectionError>),
    RequirerNotReached(Box<MvsRequirer>),
    EdgeNotFound {
        requirer: Box<MvsRequirer>,
        named_target: Box<MvsPackageVersion>,
        span: Range<usize>,
    },
    VersionNotSelected(MvsPackageVersion),
    SelectionReasonNotFound(MvsPackageVersion),
    SelectedVersionNotFound(MvsNode),
    SelectedContractNotFound(MvsPackageVersion),
    ConflictNotInSelection(Box<MvsVersionConflict>),
    MalformedCycle(Box<MvsDependencyCycle>),
    CycleEdgeIndexOutOfBounds(usize),
    CycleEdgeNotFound {
        requirer: Box<MvsRequirer>,
        target_node: Box<MvsNode>,
        span: Range<usize>,
    },
}

#[derive(Clone, Debug)]
struct Predecessor {
    previous: MvsPackageVersion,
    link: MvsChainLink,
}

/// Precomputed breadth-first provenance over every visited MVS contract.
#[derive(Debug)]
pub struct MvsExplanationGraph<'a> {
    selection: &'a MvsSelectionOutput,
    predecessors: HashMap<MvsPackageVersion, Predecessor>,
    origins: HashMap<MvsPackageVersion, MvsChainOrigin>,
    selected_graph_nodes: BTreeSet<MvsPackageVersion>,
}

impl<'a> MvsExplanationGraph<'a> {
    /// Builds shortest-path provenance for a successful selection.
    ///
    /// Root edges and every visited version's edges are expanded in the
    /// identity order established by `MvsContract`. Root-reachable paths take
    /// precedence; floor-only candidate branches start at their lock entry.
    pub fn new(selection: &'a MvsSelectionOutput) -> Result<Self, MvsExplanationError> {
        if !selection.is_complete() {
            return Err(MvsExplanationError::SelectionIncomplete(
                selection.errors().to_vec(),
            ));
        }

        let root = selection.root().clone();
        let selected_graph_nodes = collect_selected_graph_nodes(selection)?;
        let mut graph = Self {
            selection,
            predecessors: HashMap::new(),
            origins: HashMap::from([(root.clone(), MvsChainOrigin::Root(root.clone()))]),
            selected_graph_nodes,
        };
        let mut pending = VecDeque::from([root]);
        graph.expand(&mut pending);

        let mut floor_pending = VecDeque::new();
        for (node, version) in selection.reached_floors() {
            let package_version = package_version_for_node(node, version);
            if graph.origins.contains_key(&package_version) {
                continue;
            }
            graph.origins.insert(
                package_version.clone(),
                MvsChainOrigin::LockFloor(package_version.clone()),
            );
            floor_pending.push_back(package_version);
        }
        graph.expand(&mut floor_pending);
        Ok(graph)
    }

    /// Returns a chain to `requirer` followed by the specified edge.
    pub fn chain_to_edge(
        &self,
        requirer: &MvsRequirer,
        named_target: &MvsPackageVersion,
        span: Range<usize>,
    ) -> Result<MvsRequirementChain, MvsExplanationError> {
        if !self.edge_exists(requirer, named_target, &span) {
            return Err(MvsExplanationError::EdgeNotFound {
                requirer: Box::new(requirer.clone()),
                named_target: Box::new(named_target.clone()),
                span,
            });
        }
        let (origin, mut links) = self.path_to_requirer(requirer)?;
        let edge = self
            .find_edge(requirer, named_target, &span)
            .expect("edge existence was checked before constructing its explanation chain");
        links.push(chain_link(
            requirer.clone(),
            self.is_selected_requirer(requirer),
            edge,
        ));
        Ok(MvsRequirementChain { origin, links })
    }

    /// Returns why MVS selected `selected_version` for its node.
    pub fn why_selected(
        &self,
        selected_version: &MvsPackageVersion,
    ) -> Result<MvsSelectionReason, MvsExplanationError> {
        let node = selected_version.node();
        if self.selection.selected().get(&node) != Some(selected_version.version()) {
            return Err(MvsExplanationError::VersionNotSelected(
                selected_version.clone(),
            ));
        }

        let named_edges = self
            .selection
            .named_by()
            .get(selected_version)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let is_pinned = self
            .selection
            .reached_pins()
            .get(&node)
            .is_some_and(|versions| versions.contains(selected_version.version()));

        if is_pinned {
            if let Some(edge) = named_edges.iter().find(|edge| edge.root_override_pin) {
                return Ok(MvsSelectionReason::RootOverridePin {
                    chain: self.chain_for_named_edge(selected_version, edge)?,
                });
            }
            if let Some(edge) = named_edges.iter().find(|edge| edge.path_pin) {
                return Ok(MvsSelectionReason::PathPin {
                    chain: self.chain_for_named_edge(selected_version, edge)?,
                });
            }
            return Err(MvsExplanationError::SelectionReasonNotFound(
                selected_version.clone(),
            ));
        }

        if let Some(edge) = named_edges.first() {
            return Ok(MvsSelectionReason::RequirementEdge {
                chain: self.chain_for_named_edge(selected_version, edge)?,
            });
        }

        if self.selection.reached_floors().get(&node) == Some(selected_version.version()) {
            return Ok(MvsSelectionReason::LockFloor {
                package_version: selected_version.clone(),
            });
        }

        Err(MvsExplanationError::SelectionReasonNotFound(
            selected_version.clone(),
        ))
    }

    /// Explains both why a conflict's selected version was chosen and why its
    /// rejecting requirement exists.
    pub fn explain_conflict(
        &self,
        conflict: &MvsVersionConflict,
    ) -> Result<MvsConflictExplanation, MvsExplanationError> {
        let edge = self.find_edge(&conflict.requirer, &conflict.named_target, &conflict.span);
        let target_node = conflict.target.node();
        let conflict_is_valid = self
            .selected_graph_nodes
            .contains(conflict.requirer.package_version())
            && edge.is_some_and(|edge| {
                conflict.named_target.node() == target_node
                    && self.selection.selected().get(&target_node)
                        == Some(conflict.target.version())
                    && edge.requirement() == &conflict.requirement
                    && !conflict
                        .requirement
                        .matches_for_resolution(conflict.target.version())
            });
        if !conflict_is_valid {
            return Err(MvsExplanationError::ConflictNotInSelection(Box::new(
                conflict.clone(),
            )));
        }

        Ok(MvsConflictExplanation {
            selected_reason: self.why_selected(&conflict.target)?,
            requirement_chain: self.chain_to_edge(
                &conflict.requirer,
                &conflict.named_target,
                conflict.span.clone(),
            )?,
        })
    }

    /// Returns an explanation chain for one edge in a selected dependency
    /// cycle. Cycle indexes follow `MvsDependencyCycle::spans`.
    pub fn chain_for_cycle_edge(
        &self,
        cycle: &MvsDependencyCycle,
        edge_index: usize,
    ) -> Result<MvsRequirementChain, MvsExplanationError> {
        if cycle.nodes.len() < 2
            || cycle.nodes.first() != cycle.nodes.last()
            || cycle.spans.len() != cycle.nodes.len() - 1
        {
            return Err(MvsExplanationError::MalformedCycle(Box::new(cycle.clone())));
        }

        let unique_node_count = cycle.nodes.len() - 1;
        if cycle.nodes[..unique_node_count]
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != unique_node_count
        {
            return Err(MvsExplanationError::MalformedCycle(Box::new(cycle.clone())));
        }

        let mut selected_edges = Vec::with_capacity(cycle.spans.len());
        for index in 0..cycle.spans.len() {
            let source = &cycle.nodes[index];
            let target = &cycle.nodes[index + 1];
            let span = &cycle.spans[index];
            let requirer = if source == self.selection.root() {
                MvsRequirer::Root(source.clone())
            } else {
                MvsRequirer::Package(source.clone())
            };
            let matching_edge = self.edges_for_requirer(&requirer).and_then(|edges| {
                edges
                    .iter()
                    .find(|edge| edge.span() == *span && edge.target().node() == target.node())
            });
            let Some(edge) = matching_edge else {
                return Err(MvsExplanationError::CycleEdgeNotFound {
                    requirer: Box::new(requirer),
                    target_node: Box::new(target.node()),
                    span: span.clone(),
                });
            };
            if !self.selected_graph_nodes.contains(source)
                || !self.selected_graph_nodes.contains(target)
            {
                return Err(MvsExplanationError::CycleEdgeNotFound {
                    requirer: Box::new(requirer),
                    target_node: Box::new(target.node()),
                    span: span.clone(),
                });
            }
            selected_edges.push((requirer, edge.target()));
        }

        let Some((requirer, named_target)) = selected_edges.get(edge_index) else {
            return Err(MvsExplanationError::CycleEdgeIndexOutOfBounds(edge_index));
        };
        self.chain_to_edge(requirer, named_target, cycle.spans[edge_index].clone())
    }

    fn expand(&mut self, pending: &mut VecDeque<MvsPackageVersion>) {
        while let Some(source) = pending.pop_front() {
            let source_origin = self.origins[&source].clone();
            let requirer = if &source == self.selection.root() {
                MvsRequirer::Root(source.clone())
            } else {
                MvsRequirer::Package(source.clone())
            };
            let requirer_selected = self.is_selected_requirer(&requirer);
            let links = self
                .edges_for_package(&source)
                .iter()
                .map(|edge| chain_link(requirer.clone(), requirer_selected, edge))
                .collect::<Vec<_>>();
            for link in links {
                let named_target = link.named_target.clone();
                if self.origins.contains_key(&named_target) {
                    continue;
                }
                self.origins
                    .insert(named_target.clone(), source_origin.clone());
                self.predecessors.insert(
                    named_target.clone(),
                    Predecessor {
                        previous: source.clone(),
                        link,
                    },
                );
                pending.push_back(named_target);
            }
        }
    }

    fn is_selected_requirer(&self, requirer: &MvsRequirer) -> bool {
        match requirer {
            MvsRequirer::Root(root) => root == self.selection.root(),
            MvsRequirer::Package(package_version) => {
                self.selection.selected().get(&package_version.node())
                    == Some(package_version.version())
            }
        }
    }

    fn edges_for_package(&self, package_version: &MvsPackageVersion) -> &[MvsEdge] {
        if package_version == self.selection.root() {
            return self.selection.root_contract().edges();
        }
        self.selection.read_contracts()[package_version].edges()
    }

    fn edge_exists(
        &self,
        requirer: &MvsRequirer,
        named_target: &MvsPackageVersion,
        span: &Range<usize>,
    ) -> bool {
        self.find_edge(requirer, named_target, span).is_some()
    }

    fn find_edge(
        &self,
        requirer: &MvsRequirer,
        named_target: &MvsPackageVersion,
        span: &Range<usize>,
    ) -> Option<&MvsEdge> {
        self.edges_for_requirer(requirer)?
            .iter()
            .find(|edge| edge.target() == *named_target && edge.span() == *span)
    }

    fn edges_for_requirer(&self, requirer: &MvsRequirer) -> Option<&[MvsEdge]> {
        match requirer {
            MvsRequirer::Root(root) if root == self.selection.root() => {
                Some(self.selection.root_contract().edges())
            }
            MvsRequirer::Package(package_version) => self
                .selection
                .read_contracts()
                .get(package_version)
                .map(MvsContract::edges),
            MvsRequirer::Root(_) => None,
        }
    }

    fn path_to_requirer(
        &self,
        requirer: &MvsRequirer,
    ) -> Result<(MvsChainOrigin, Vec<MvsChainLink>), MvsExplanationError> {
        let mut current = requirer.package_version().clone();
        if matches!(requirer, MvsRequirer::Root(root) if root != self.selection.root())
            || !self.origins.contains_key(&current)
        {
            return Err(MvsExplanationError::RequirerNotReached(Box::new(
                requirer.clone(),
            )));
        }

        let mut links = Vec::new();
        while let Some(predecessor) = self.predecessors.get(&current) {
            links.push(predecessor.link.clone());
            current = predecessor.previous.clone();
        }
        links.reverse();
        let origin = self.origins[&current].clone();
        Ok((origin, links))
    }

    fn chain_for_named_edge(
        &self,
        named_version: &MvsPackageVersion,
        edge: &MvsNamedEdge,
    ) -> Result<MvsRequirementChain, MvsExplanationError> {
        self.chain_to_edge(&edge.requirer, named_version, edge.span.clone())
    }
}

/// Both chains required to explain a `VERSION_CONFLICT`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsConflictExplanation {
    pub selected_reason: MvsSelectionReason,
    pub requirement_chain: MvsRequirementChain,
}

fn package_version_for_node(node: &MvsNode, version: &Version) -> MvsPackageVersion {
    MvsPackageVersion::new(node.package().to_owned(), version.clone())
        .expect("floor node identities were validated by MVS input construction")
}

fn collect_selected_graph_nodes(
    selection: &MvsSelectionOutput,
) -> Result<BTreeSet<MvsPackageVersion>, MvsExplanationError> {
    let root = selection.root().clone();
    let mut nodes = BTreeSet::from([root.clone()]);
    let mut pending = VecDeque::from([root.clone()]);

    while let Some(source) = pending.pop_front() {
        let edges = if source == root {
            selection.root_contract().edges()
        } else {
            selection
                .read_contracts()
                .get(&source)
                .ok_or_else(|| MvsExplanationError::SelectedContractNotFound(source.clone()))?
                .edges()
        };

        for edge in edges {
            let node = edge.target().node();
            let version = selection
                .selected()
                .get(&node)
                .ok_or_else(|| MvsExplanationError::SelectedVersionNotFound(node.clone()))?;
            let target = package_version_for_node(&node, version);
            if nodes.insert(target.clone()) {
                pending.push_back(target);
            }
        }
    }

    Ok(nodes)
}

fn chain_link(requirer: MvsRequirer, requirer_selected: bool, edge: &MvsEdge) -> MvsChainLink {
    MvsChainLink {
        requirer,
        requirer_selected,
        named_target: edge.target(),
        requirement: edge.requirement().clone(),
        span: edge.span(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{MvsChainOrigin, MvsExplanationError, MvsExplanationGraph, MvsSelectionReason};
    use crate::mvs::{MvsContract, MvsEdge, MvsNode, MvsPackageVersion, MvsRequirer, select};
    use crate::mvs_graph_validation::{MvsDependencyCycle, check_selected_graph};
    use crate::version::Version;
    use crate::version_req::VersionReq;

    fn version(source: &str) -> Version {
        Version::parse(source).expect("valid test version")
    }

    fn package_version(package: &str, version_: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, version(version_)).expect("valid package version")
    }

    fn edge(package: &str, requirement: &str, named_version: &str, start: usize) -> MvsEdge {
        MvsEdge::new(
            package,
            VersionReq::parse(requirement).expect("valid test requirement"),
            version(named_version),
            start..start + 1,
        )
        .expect("valid edge")
    }

    fn root(edges: Vec<MvsEdge>) -> MvsContract {
        MvsContract::new(package_version("acme:app", "1.0.0"), edges).expect("valid root contract")
    }

    fn contract(
        package: &str,
        version_: &str,
        edges: Vec<MvsEdge>,
    ) -> (MvsPackageVersion, MvsContract) {
        let contract = MvsContract::new(package_version(package, version_), edges)
            .expect("valid package contract");
        (contract.package_version().clone(), contract)
    }

    fn catalog(
        contracts: Vec<(MvsPackageVersion, MvsContract)>,
    ) -> HashMap<MvsPackageVersion, MvsContract> {
        contracts.into_iter().collect()
    }

    #[test]
    fn chooses_the_shortest_path_and_uses_identity_order_for_ties() {
        let root = root(vec![
            edge("acme:z", "^1", "1.0.0", 10),
            edge("acme:a", "^1", "1.0.0", 0),
        ]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 20)]),
            contract("acme:z", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 30)]),
            contract(
                "acme:b",
                "1.0.0",
                vec![edge("acme:target", "^1", "1.0.0", 40)],
            ),
            contract(
                "acme:target",
                "1.0.0",
                vec![edge("acme:leaf", "^1", "1.0.0", 50)],
            ),
            contract("acme:leaf", "1.0.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let target = package_version("acme:leaf", "1.0.0");
        let chain = explanations
            .chain_to_edge(
                &MvsRequirer::Package(package_version("acme:target", "1.0.0")),
                &target,
                50..51,
            )
            .expect("requirement chain exists");

        assert_eq!(
            chain.origin,
            MvsChainOrigin::Root(root.package_version().clone())
        );
        assert_eq!(
            chain
                .links
                .iter()
                .map(|link| link.requirer.package_version().package())
                .collect::<Vec<_>>(),
            vec!["acme:app", "acme:a", "acme:b", "acme:target"]
        );
    }

    #[test]
    fn prefers_a_shorter_path_over_an_earlier_breadth_first_branch() {
        let root = root(vec![
            edge("acme:z", "^1", "1.0.0", 10),
            edge("acme:a", "^1", "1.0.0", 0),
        ]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 20)]),
            contract(
                "acme:b",
                "1.0.0",
                vec![edge("acme:target", "^1", "1.0.0", 30)],
            ),
            contract(
                "acme:z",
                "1.0.0",
                vec![edge("acme:target", "^1", "1.0.0", 40)],
            ),
            contract(
                "acme:target",
                "1.0.0",
                vec![edge("acme:leaf", "^1", "1.0.0", 50)],
            ),
            contract("acme:leaf", "1.0.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let chain = explanations
            .chain_to_edge(
                &MvsRequirer::Package(package_version("acme:target", "1.0.0")),
                &package_version("acme:leaf", "1.0.0"),
                50..51,
            )
            .expect("requirement chain exists");

        assert_eq!(
            chain
                .links
                .iter()
                .map(|link| link.requirer.package_version().package())
                .collect::<Vec<_>>(),
            vec!["acme:app", "acme:z", "acme:target"]
        );
    }

    #[test]
    fn marks_candidate_versions_that_are_not_selected() {
        let root = root(vec![
            edge("acme:b", "^1", "1.0.0", 0),
            edge("acme:a", "^1", "1.0.0", 10),
        ]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![edge("acme:b", ">=1.4", "1.4.0", 20)],
            ),
            contract(
                "acme:b",
                "1.0.0",
                vec![edge("acme:leaf", "^1", "1.0.0", 30)],
            ),
            contract("acme:b", "1.4.0", vec![]),
            contract("acme:leaf", "1.0.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let chain = explanations
            .chain_to_edge(
                &MvsRequirer::Package(package_version("acme:b", "1.0.0")),
                &package_version("acme:leaf", "1.0.0"),
                30..31,
            )
            .expect("candidate contract edge has a chain");

        assert_eq!(
            chain
                .links
                .iter()
                .map(|link| link.requirer_selected)
                .collect::<Vec<_>>(),
            vec![true, false]
        );
    }

    #[test]
    fn starts_floor_only_explanations_at_the_lock_entry() {
        let root = root(vec![edge("acme:b", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.8.0", vec![edge("acme:c", "^1", "1.0.0", 10)]),
            contract("acme:c", "1.0.0", vec![]),
        ]);
        let floors = HashMap::from([(package_version("acme:b", "1.0.0").node(), version("1.8.0"))]);
        let selection =
            select(&root, &contracts, &floors, &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");

        assert_eq!(
            explanations
                .why_selected(&package_version("acme:b", "1.8.0"))
                .expect("floor reason"),
            MvsSelectionReason::LockFloor {
                package_version: package_version("acme:b", "1.8.0"),
            }
        );
        let chain = explanations
            .chain_to_edge(
                &MvsRequirer::Package(package_version("acme:b", "1.8.0")),
                &package_version("acme:c", "1.0.0"),
                10..11,
            )
            .expect("floor descendant chain");
        assert_eq!(
            chain.origin,
            MvsChainOrigin::LockFloor(package_version("acme:b", "1.8.0"))
        );
        assert_eq!(chain.links.len(), 1);
    }

    #[test]
    fn explains_both_sides_of_a_version_conflict() {
        let root = root(vec![
            edge("acme:b", ">=1, <1.4", "1.0.0", 0),
            edge("acme:a", "^1", "1.0.0", 10),
        ]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![edge("acme:b", ">=1.4", "1.4.0", 20)],
            ),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.4.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let graph = check_selected_graph(&root, &selection).expect("selected graph");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let explanation = explanations
            .explain_conflict(&graph.conflicts[0])
            .expect("conflict explanation");

        let MvsSelectionReason::RequirementEdge { chain } = explanation.selected_reason else {
            panic!("the selected version is named by a requirement edge");
        };
        assert_eq!(
            chain
                .links
                .iter()
                .map(|link| link.requirer.package_version().package())
                .collect::<Vec<_>>(),
            vec!["acme:app", "acme:a"]
        );
        assert_eq!(
            chain.links.last().unwrap().named_target,
            package_version("acme:b", "1.4.0")
        );
        assert_eq!(
            explanation
                .requirement_chain
                .links
                .iter()
                .map(|link| link.requirer.package_version().package())
                .collect::<Vec<_>>(),
            vec!["acme:app"]
        );
        assert_eq!(
            explanation.requirement_chain.links[0].named_target,
            package_version("acme:b", "1.0.0")
        );
    }

    #[test]
    fn rejects_conflicts_that_do_not_match_the_captured_requirement() {
        let root = root(vec![
            edge("acme:b", ">=1, <1.4", "1.0.0", 0),
            edge("acme:a", "^1", "1.0.0", 10),
        ]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![edge("acme:b", ">=1.4", "1.4.0", 20)],
            ),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.4.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let graph = check_selected_graph(&root, &selection).expect("selected graph");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let mut conflict = graph.conflicts[0].clone();
        conflict.requirement = VersionReq::parse("^1").expect("valid requirement");

        assert_eq!(
            explanations.explain_conflict(&conflict),
            Err(MvsExplanationError::ConflictNotInSelection(Box::new(
                conflict
            )))
        );
    }

    #[test]
    fn rejects_conflicts_from_unselected_candidate_contracts() {
        let root = root(vec![
            edge("acme:b", ">=1, <1.4", "1.0.0", 0),
            edge("acme:a", "^1", "1.0.0", 10),
            edge("acme:d", "^1", "1.0.0", 20),
        ]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:c", "^1", "1.0.0", 30)]),
            contract("acme:a", "1.5.0", vec![]),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.4.0", vec![]),
            contract(
                "acme:c",
                "1.0.0",
                vec![edge("acme:b", ">=1, <1.5", "1.0.0", 50)],
            ),
            contract(
                "acme:d",
                "1.0.0",
                vec![
                    edge("acme:a", ">=1.5", "1.5.0", 40),
                    edge("acme:b", ">=1.4", "1.4.0", 60),
                ],
            ),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let graph = check_selected_graph(&root, &selection).expect("selected graph");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let mut conflict = graph.conflicts[0].clone();
        conflict.requirer = MvsRequirer::Package(package_version("acme:c", "1.0.0"));
        conflict.named_target = package_version("acme:b", "1.0.0");
        conflict.requirement = VersionReq::parse(">=1, <1.5").expect("valid requirement");
        conflict.span = 50..51;

        assert_eq!(
            explanations.explain_conflict(&conflict),
            Err(MvsExplanationError::ConflictNotInSelection(Box::new(
                conflict
            )))
        );
    }

    #[test]
    fn explains_an_edge_that_closes_a_selected_dependency_cycle() {
        let root = root(vec![edge("acme:b", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:b", "1.0.0", vec![edge("acme:c", "^1", "1.0.0", 10)]),
            contract("acme:c", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 20)]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let graph = check_selected_graph(&root, &selection).expect("selected graph");
        let cycle = graph.cycle.expect("selected graph contains a cycle");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");

        let chain = explanations
            .chain_for_cycle_edge(&cycle, 1)
            .expect("cycle edge has an explanation");
        assert_eq!(
            chain.origin,
            MvsChainOrigin::Root(root.package_version().clone())
        );
        assert_eq!(
            chain
                .links
                .iter()
                .map(|link| link.requirer.package_version().package())
                .collect::<Vec<_>>(),
            vec!["acme:app", "acme:b", "acme:c"]
        );
        assert_eq!(chain.links.last().expect("cycle edge").span, cycle.spans[1]);
        assert_eq!(
            explanations.chain_for_cycle_edge(&cycle, 2),
            Err(MvsExplanationError::CycleEdgeIndexOutOfBounds(2))
        );
    }

    #[test]
    fn rejects_a_nonclosing_cycle_even_when_its_edge_exists() {
        let root = root(vec![edge("acme:b", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![contract("acme:b", "1.0.0", vec![])]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let cycle = MvsDependencyCycle {
            nodes: vec![
                root.package_version().clone(),
                package_version("acme:b", "1.0.0"),
            ],
            spans: std::iter::once(0..1).collect(),
        };

        assert_eq!(
            explanations.chain_for_cycle_edge(&cycle, 0),
            Err(MvsExplanationError::MalformedCycle(Box::new(cycle)))
        );
    }

    #[test]
    fn rejects_cycles_in_candidates_outside_the_selected_graph() {
        let root = root(vec![
            edge("acme:a", "^1", "1.0.0", 0),
            edge("acme:d", "^1", "1.0.0", 1),
        ]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:c", "^1", "1.0.0", 10)]),
            contract("acme:a", "1.5.0", vec![]),
            contract("acme:b", "1.0.0", vec![edge("acme:c", "^1", "1.0.0", 40)]),
            contract("acme:c", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 30)]),
            contract(
                "acme:d",
                "1.0.0",
                vec![edge("acme:a", ">=1.5", "1.5.0", 20)],
            ),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        assert_eq!(
            selection
                .selected()
                .get(&package_version("acme:c", "1.0.0").node()),
            Some(&version("1.0.0"))
        );
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");
        let cycle = MvsDependencyCycle {
            nodes: vec![
                package_version("acme:c", "1.0.0"),
                package_version("acme:b", "1.0.0"),
                package_version("acme:c", "1.0.0"),
            ],
            spans: vec![30..31, 40..41],
        };

        assert_eq!(
            explanations.chain_for_cycle_edge(&cycle, 0),
            Err(MvsExplanationError::CycleEdgeNotFound {
                requirer: Box::new(MvsRequirer::Package(package_version("acme:c", "1.0.0"))),
                target_node: Box::new(package_version("acme:b", "1.0.0").node()),
                span: 30..31,
            })
        );
    }

    #[test]
    fn explains_a_reached_from_path_pin_by_its_edge() {
        let root = root(vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let path_edge = MvsEdge::new_path_pinned(
            "acme:b",
            VersionReq::parse("^1").expect("valid requirement"),
            version("1.2.0"),
            10..11,
        )
        .expect("valid path pin");
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![path_edge]),
            contract("acme:b", "1.2.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");

        let MvsSelectionReason::PathPin { chain } = explanations
            .why_selected(&package_version("acme:b", "1.2.0"))
            .expect("path pin reason")
        else {
            panic!("the selected version is pinned by a path edge");
        };
        assert_eq!(chain.links.len(), 2);
        assert_eq!(chain.links[1].span, 10..11);
    }

    #[test]
    fn explains_a_root_path_override_pin_by_its_root_edge() {
        let root_edge = MvsEdge::new_root_override_pinned(
            "acme:b",
            VersionReq::parse("^1").expect("valid requirement"),
            version("1.8.0"),
            0..1,
        )
        .expect("valid root override edge");
        let root = root(vec![root_edge]);
        let node = MvsNode::new("acme:b", version("1.0.0").compatibility_line())
            .expect("valid package node");
        let pins = HashMap::from([(node, version("1.8.0"))]);
        let contracts = catalog(vec![contract("acme:b", "1.8.0", vec![])]);
        let selection = select(&root, &contracts, &HashMap::new(), &pins).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");

        let MvsSelectionReason::RootOverridePin { chain } = explanations
            .why_selected(&package_version("acme:b", "1.8.0"))
            .expect("root override reason")
        else {
            panic!("the selected version is pinned by a root override");
        };
        assert_eq!(chain.links.len(), 1);
        assert_eq!(
            chain.links[0].requirer,
            MvsRequirer::Root(root.package_version().clone())
        );
    }

    #[test]
    fn rejects_edges_absent_from_the_selection_provenance() {
        let root = root(vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![contract("acme:a", "1.0.0", vec![])]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid selection");
        let explanations =
            MvsExplanationGraph::new(&selection).expect("complete explanation graph");

        assert!(matches!(
            explanations.chain_to_edge(
                &MvsRequirer::Package(package_version("acme:a", "1.0.0")),
                &package_version("acme:missing", "1.0.0"),
                99..100,
            ),
            Err(MvsExplanationError::EdgeNotFound { .. })
        ));
    }
}
