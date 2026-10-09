//! Build and validate the graph induced by MVS-selected package versions.
//!
//! This phase follows version selection. It checks requirements only on edges
//! from the root and selected dependency versions; contracts for other
//! versions in the MVS candidate closure do not contribute to the build graph.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use crate::mvs::{
    MvsContract, MvsEdge, MvsNode, MvsPackageVersion, MvsRequirer, MvsSelectionError,
    MvsSelectionOutput,
};
use crate::version::Version;
use crate::version_req::VersionReq;

/// One edge included in the graph of selected package versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsSelectedEdge {
    pub requirer: MvsRequirer,
    pub target: MvsPackageVersion,
    pub requirement: VersionReq,
    pub span: Range<usize>,
}

impl MvsSelectedEdge {
    fn from_edge(
        requirer: MvsRequirer,
        edge: &MvsEdge,
        selection: &MvsSelectionOutput,
    ) -> Result<Self, MvsSelectedGraphInputError> {
        let node = edge.target().node();
        let Some(version) = selection.selected().get(&node) else {
            return Err(MvsSelectedGraphInputError::MissingSelectedVersion {
                node: Box::new(node),
                requirer: Box::new(requirer),
                span: edge.span(),
            });
        };
        if version.compatibility_line() != node.line() {
            return Err(MvsSelectedGraphInputError::SelectedVersionLineMismatch {
                node,
                version: version.clone(),
            });
        }

        let target = MvsPackageVersion::new(node.package().to_owned(), version.clone())
            .expect("selected node identities were validated by MVS input construction");
        Ok(Self {
            requirer,
            target,
            requirement: edge.requirement().clone(),
            span: edge.span(),
        })
    }

    fn source(&self) -> &MvsPackageVersion {
        self.requirer.package_version()
    }
}

/// A requirement edge in the selected graph that rejects its selected version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsVersionConflict {
    pub requirer: MvsRequirer,
    pub target: MvsPackageVersion,
    pub requirement: VersionReq,
    pub span: Range<usize>,
}

/// One cycle in the selected dependency graph.
///
/// `nodes` has its first package version repeated at the end. `spans[i]` is the
/// source range for the edge from `nodes[i]` to `nodes[i + 1]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsDependencyCycle {
    pub nodes: Vec<MvsPackageVersion>,
    pub spans: Vec<Range<usize>>,
}

/// The graph reachable from the root through selected versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsSelectedGraph {
    pub root: MvsPackageVersion,
    /// Selected dependency package versions in the graph. The root is kept
    /// separately because it is not a package entry in `sai.lock`.
    pub dependencies: BTreeSet<MvsPackageVersion>,
    /// Edges from the root and selected dependency versions, sorted by source
    /// package version and then target identity.
    pub edges: Vec<MvsSelectedEdge>,
    /// Every selected-graph edge whose requirement rejects its target version.
    /// Conflicts are sorted by target node, requirer, and source range.
    pub conflicts: Vec<MvsVersionConflict>,
    /// The first deterministic dependency cycle, if one exists.
    pub cycle: Option<MvsDependencyCycle>,
}

impl MvsSelectedGraph {
    pub fn is_valid(&self) -> bool {
        self.conflicts.is_empty() && self.cycle.is_none()
    }
}

/// Invalid or incomplete input to selected-graph construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsSelectedGraphInputError {
    SelectionIncomplete(Vec<MvsSelectionError>),
    RootContractMismatch {
        provided: Box<MvsContract>,
        selected: Box<MvsContract>,
    },
    MissingSelectedVersion {
        node: Box<MvsNode>,
        requirer: Box<MvsRequirer>,
        span: Range<usize>,
    },
    SelectedVersionLineMismatch {
        node: MvsNode,
        version: Version,
    },
}

#[derive(Clone, Debug)]
struct DfsFrame {
    node: MvsPackageVersion,
    next_edge: usize,
}

/// Builds the graph induced by `selection` and checks its selected edges.
///
/// The selection must be a successful result from [`crate::mvs::select`] for
/// `root`. It uses the exact dependency contracts captured by MVS, so a
/// caller cannot accidentally validate with a different candidate catalog.
pub fn check_selected_graph(
    root: &MvsContract,
    selection: &MvsSelectionOutput,
) -> Result<MvsSelectedGraph, MvsSelectedGraphInputError> {
    if !selection.is_complete() {
        return Err(MvsSelectedGraphInputError::SelectionIncomplete(
            selection.errors().to_vec(),
        ));
    }
    if selection.root_contract() != root {
        return Err(MvsSelectedGraphInputError::RootContractMismatch {
            provided: Box::new(root.clone()),
            selected: Box::new(selection.root_contract().clone()),
        });
    }

    let root_version = root.package_version().clone();
    let mut graph = MvsSelectedGraph {
        root: root_version.clone(),
        dependencies: BTreeSet::new(),
        edges: Vec::new(),
        conflicts: Vec::new(),
        cycle: None,
    };
    let mut edges_by_source = BTreeMap::<MvsPackageVersion, Vec<MvsSelectedEdge>>::new();
    let mut pending = BTreeSet::<MvsPackageVersion>::new();
    let mut read = HashSet::from([root_version.clone()]);

    collect_selected_edges(
        &root_version,
        root.edges(),
        selection,
        &mut graph,
        &mut edges_by_source,
        &mut pending,
    )?;

    while let Some(package_version) = pending.pop_first() {
        if !read.insert(package_version.clone()) {
            continue;
        }
        let contract = selection
            .read_contracts()
            .get(&package_version)
            .expect("complete MVS selection retains every contract it read");
        collect_selected_edges(
            &package_version,
            contract.edges(),
            selection,
            &mut graph,
            &mut edges_by_source,
            &mut pending,
        )?;
    }

    graph.edges = edges_by_source
        .values()
        .flat_map(|edges| edges.iter().cloned())
        .collect();
    graph.conflicts.sort_by(|left, right| {
        left.target
            .node()
            .cmp(&right.target.node())
            .then_with(|| {
                left.requirer
                    .package_version()
                    .cmp(right.requirer.package_version())
            })
            .then_with(|| left.span.start.cmp(&right.span.start))
            .then_with(|| left.span.end.cmp(&right.span.end))
    });
    graph.cycle = find_cycle(&graph.root, &graph.dependencies, &edges_by_source);
    Ok(graph)
}

fn collect_selected_edges(
    source: &MvsPackageVersion,
    edges: &[MvsEdge],
    selection: &MvsSelectionOutput,
    graph: &mut MvsSelectedGraph,
    edges_by_source: &mut BTreeMap<MvsPackageVersion, Vec<MvsSelectedEdge>>,
    pending: &mut BTreeSet<MvsPackageVersion>,
) -> Result<(), MvsSelectedGraphInputError> {
    let requirer = if source == &graph.root {
        MvsRequirer::Root(source.clone())
    } else {
        MvsRequirer::Package(source.clone())
    };
    let selected_edges = edges
        .iter()
        .map(|edge| MvsSelectedEdge::from_edge(requirer.clone(), edge, selection))
        .collect::<Result<Vec<_>, _>>()?;

    for selected_edge in &selected_edges {
        graph.dependencies.insert(selected_edge.target.clone());
        if !selected_edge
            .requirement
            .matches_for_resolution(selected_edge.target.version())
        {
            graph.conflicts.push(MvsVersionConflict {
                requirer: selected_edge.requirer.clone(),
                target: selected_edge.target.clone(),
                requirement: selected_edge.requirement.clone(),
                span: selected_edge.span.clone(),
            });
        }
        pending.insert(selected_edge.target.clone());
    }
    edges_by_source.insert(source.clone(), selected_edges);
    Ok(())
}

fn find_cycle(
    root: &MvsPackageVersion,
    dependencies: &BTreeSet<MvsPackageVersion>,
    edges_by_source: &BTreeMap<MvsPackageVersion, Vec<MvsSelectedEdge>>,
) -> Option<MvsDependencyCycle> {
    let mut states = HashMap::<MvsPackageVersion, u8>::new();
    let mut active_positions = HashMap::<MvsPackageVersion, usize>::new();
    let mut vertices = dependencies.clone();
    vertices.insert(root.clone());

    for start in &vertices {
        if states.get(start).copied().unwrap_or(0) != 0 {
            continue;
        }
        states.insert(start.clone(), 1);
        active_positions.insert(start.clone(), 0);
        let mut stack = vec![DfsFrame {
            node: start.clone(),
            next_edge: 0,
        }];
        let mut incoming_edges = Vec::<MvsSelectedEdge>::new();

        while !stack.is_empty() {
            let frame = stack.last_mut().expect("stack is non-empty");
            let Some(edges) = edges_by_source.get(&frame.node) else {
                finish_dfs_frame(&mut stack, &mut states, &mut active_positions);
                if !incoming_edges.is_empty() {
                    incoming_edges.pop();
                }
                continue;
            };
            if frame.next_edge == edges.len() {
                finish_dfs_frame(&mut stack, &mut states, &mut active_positions);
                if !incoming_edges.is_empty() {
                    incoming_edges.pop();
                }
                continue;
            }

            let edge = &edges[frame.next_edge];
            frame.next_edge += 1;
            match states.get(&edge.target).copied().unwrap_or(0) {
                0 => {
                    states.insert(edge.target.clone(), 1);
                    active_positions.insert(edge.target.clone(), stack.len());
                    stack.push(DfsFrame {
                        node: edge.target.clone(),
                        next_edge: 0,
                    });
                    incoming_edges.push(edge.clone());
                }
                1 => {
                    let cycle_start = active_positions[&edge.target];
                    let mut cycle_edges = incoming_edges[cycle_start..].to_vec();
                    cycle_edges.push(edge.clone());
                    rotate_cycle_to_least_package(&mut cycle_edges);
                    let mut nodes = cycle_edges
                        .iter()
                        .map(|cycle_edge| cycle_edge.source().clone())
                        .collect::<Vec<_>>();
                    nodes.push(
                        cycle_edges
                            .first()
                            .expect("a cycle has at least one edge")
                            .source()
                            .clone(),
                    );
                    let spans = cycle_edges
                        .into_iter()
                        .map(|cycle_edge| cycle_edge.span)
                        .collect();
                    return Some(MvsDependencyCycle { nodes, spans });
                }
                2 => {}
                _ => unreachable!(),
            }
        }
    }
    None
}

fn rotate_cycle_to_least_package(edges: &mut [MvsSelectedEdge]) {
    let first = edges
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| left.source().cmp(right.source()))
        .map(|(index, _)| index)
        .expect("a cycle has at least one edge");
    edges.rotate_left(first);
}

fn finish_dfs_frame(
    stack: &mut Vec<DfsFrame>,
    states: &mut HashMap<MvsPackageVersion, u8>,
    active_positions: &mut HashMap<MvsPackageVersion, usize>,
) {
    let finished = stack.pop().expect("a DFS frame is active");
    states.insert(finished.node.clone(), 2);
    active_positions.remove(&finished.node);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{MvsSelectedGraphInputError, MvsVersionConflict, check_selected_graph};
    use crate::mvs::{
        MvsContract, MvsEdge, MvsNode, MvsPackageVersion, MvsRequirer, MvsSelectionErrorKind,
        select,
    };
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
            VersionReq::parse(requirement).expect("valid requirement"),
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
    fn validates_requirements_on_edges_from_selected_versions() {
        let root = root(vec![
            edge("acme:a", "^1", "1.0.0", 0),
            edge("acme:z", "^1", "1.0.0", 10),
        ]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:c", "^1", "1.0.0", 20)]),
            contract("acme:a", "1.8.0", vec![edge("acme:d", "^1", "1.0.0", 30)]),
            contract("acme:z", "1.0.0", vec![edge("acme:a", "^1.8", "1.8.0", 40)]),
            contract("acme:c", "1.0.0", vec![]),
            contract("acme:d", "1.0.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS input");
        let graph = check_selected_graph(&root, &selection).expect("valid graph input");

        assert!(graph.is_valid(), "{:?}", graph.conflicts);
        assert!(
            graph
                .dependencies
                .contains(&package_version("acme:d", "1.0.0"))
        );
        assert!(
            !graph
                .dependencies
                .contains(&package_version("acme:c", "1.0.0"))
        );
        assert!(
            graph
                .edges
                .iter()
                .all(|selected_edge| selected_edge.source() != &package_version("acme:a", "1.0.0"))
        );
    }

    #[test]
    fn reports_the_edge_rejecting_the_selected_version() {
        let root = root(vec![
            edge("acme:a", ">=1, <1.4", "1.0.0", 0),
            edge("acme:b", ">=1, <1.4", "1.0.0", 10),
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
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS input");
        let graph = check_selected_graph(&root, &selection).expect("valid graph input");

        assert_eq!(graph.conflicts.len(), 1);
        assert_eq!(
            graph.conflicts[0],
            MvsVersionConflict {
                requirer: MvsRequirer::Root(root.package_version().clone()),
                target: package_version("acme:b", "1.4.0"),
                requirement: VersionReq::parse(">=1, <1.4").expect("valid requirement"),
                span: 10..11,
            }
        );
    }

    #[test]
    fn reports_requirement_conflicts_in_target_order() {
        let root = root(vec![
            edge("acme:z", ">=1, <1.4", "1.0.0", 0),
            edge("acme:a", ">=1, <1.4", "1.0.0", 10),
            edge("acme:b", "^1", "1.0.0", 20),
            edge("acme:c", "^1", "1.0.0", 30),
        ]);
        let contracts = catalog(vec![
            contract(
                "acme:b",
                "1.0.0",
                vec![edge("acme:a", ">=1.4", "1.4.0", 20)],
            ),
            contract("acme:a", "1.0.0", vec![]),
            contract("acme:a", "1.4.0", vec![]),
            contract(
                "acme:c",
                "1.0.0",
                vec![edge("acme:z", ">=1.4", "1.4.0", 40)],
            ),
            contract("acme:z", "1.0.0", vec![]),
            contract("acme:z", "1.4.0", vec![]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS input");
        let graph = check_selected_graph(&root, &selection).expect("valid graph input");

        assert_eq!(
            graph
                .conflicts
                .iter()
                .map(|conflict| conflict.target.package().to_owned())
                .collect::<Vec<_>>(),
            vec!["acme:a", "acme:z"]
        );
    }

    #[test]
    fn reports_a_root_path_override_range_failure_as_a_selected_conflict() {
        let root_edge = MvsEdge::new_root_override_pinned(
            "acme:a",
            VersionReq::parse(">=1, <1.8").expect("valid requirement"),
            version("1.8.0"),
            0..1,
        )
        .expect("root override edge defers range validation");
        let root = root(vec![root_edge]);
        let pin_node =
            MvsNode::new("acme:a", version("1.8.0").compatibility_line()).expect("valid node");
        let pins = HashMap::from([(pin_node, version("1.8.0"))]);
        let contracts = catalog(vec![contract("acme:a", "1.8.0", vec![])]);
        let selection = select(&root, &contracts, &HashMap::new(), &pins)
            .expect("root path version is valid MVS input");
        let graph = check_selected_graph(&root, &selection).expect("valid graph input");

        assert_eq!(graph.conflicts.len(), 1);
        assert_eq!(
            graph.conflicts[0].target,
            package_version("acme:a", "1.8.0")
        );
    }

    #[test]
    fn returns_one_cycle_rotated_to_its_least_package_version() {
        let root = root(vec![edge("acme:z", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:z", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 10)]),
            contract("acme:b", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 20)]),
            contract("acme:a", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 30)]),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS input");
        let graph = check_selected_graph(&root, &selection).expect("valid graph input");
        let cycle = graph.cycle.expect("cycle should be reported");

        assert_eq!(
            cycle.nodes,
            vec![
                package_version("acme:a", "1.0.0"),
                package_version("acme:z", "1.0.0"),
                package_version("acme:b", "1.0.0"),
                package_version("acme:a", "1.0.0"),
            ]
        );
        assert_eq!(cycle.spans, vec![30..31, 10..11, 20..21]);
    }

    #[test]
    fn rejects_selection_from_a_root_with_different_edges() {
        let root = root(vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let other_root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![edge("acme:a", "^2", "2.0.0", 0)],
        )
        .expect("valid root with different edges");
        let contracts = catalog(vec![contract("acme:a", "2.0.0", vec![])]);
        let selection = select(&other_root, &contracts, &HashMap::new(), &HashMap::new())
            .expect("valid MVS input");

        assert_eq!(
            check_selected_graph(&root, &selection).unwrap_err(),
            MvsSelectedGraphInputError::RootContractMismatch {
                provided: Box::new(root.clone()),
                selected: Box::new(other_root),
            }
        );
    }

    #[test]
    fn rejects_incomplete_selection_before_building_the_graph() {
        let root = root(vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let selection = select(&root, &HashMap::new(), &HashMap::new(), &HashMap::new())
            .expect("selection reports missing contracts as a diagnostic");

        assert!(matches!(
            check_selected_graph(&root, &selection),
            Err(MvsSelectedGraphInputError::SelectionIncomplete(errors))
                if matches!(
                errors.first().map(|error| &error.kind),
                Some(MvsSelectionErrorKind::ContractNotProvided(missing_package))
                    if missing_package == &package_version("acme:a", "1.0.0")
                )
        ));
    }
}
