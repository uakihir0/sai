//! Build-time activation from a complete resolver-contract set.
//!
//! This input is appropriate when the caller already has every selected
//! dependency contract, such as a fresh resolver-selection path. Mode A lock
//! builds must activate from recorded lock-edge metadata instead: they fetch
//! only active package contents and cannot require contracts for inactive
//! nodes. The lock-backed activation adapter is a separate required input to
//! production build integration.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::ops::Range;

use crate::manifest::PackageRole;
use crate::mvs::{MvsNode, MvsPackageVersion, MvsRequirer};
use crate::mvs_graph_validation::{
    MvsDependencyCycle, MvsSelectedEdge, MvsSelectedGraph, MvsVersionConflict,
};
use crate::resolver_contract::{ResolverContract, ResolverPinKind, ResolverRequirementResolution};
use crate::version_req::VersionReq;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ContractEdgeKey {
    requirer: MvsRequirer,
    package: Box<str>,
    span_start: usize,
    span_end: usize,
    requirement: VersionReq,
}

impl ContractEdgeKey {
    fn new(
        requirer: &MvsRequirer,
        package: &str,
        span: &Range<usize>,
        requirement: &VersionReq,
    ) -> Self {
        Self {
            requirer: requirer.clone(),
            package: package.into(),
            span_start: span.start,
            span_end: span.end,
            requirement: requirement.clone(),
        }
    }
}

/// The resolved packages and edges needed by one target and command.
///
/// `root` is separate from `packages`, which contains active dependency
/// versions only and is sorted by package identity and version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildActiveGraph {
    pub root: MvsPackageVersion,
    pub packages: Vec<MvsPackageVersion>,
    /// Active edges in the deterministic order established by the selected
    /// graph.
    pub edges: Vec<MvsSelectedEdge>,
}

/// Invalid or incomplete input to build-active graph construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildActiveGraphError {
    RootRoleMismatch(PackageRole),
    RootPackageMismatch {
        contract: Box<MvsPackageVersion>,
        graph: Box<MvsPackageVersion>,
    },
    RootMvsPackageVersionMismatch {
        manifest: Box<MvsPackageVersion>,
        mvs: Box<MvsPackageVersion>,
    },
    RootContractMetadataMismatch(MvsPackageVersion),
    TargetNotDeclaredByRoot(Box<str>),
    InvalidSelectedGraph {
        conflicts: Vec<MvsVersionConflict>,
        cycle: Option<MvsDependencyCycle>,
    },
    RootNodeSelectedAsDependency {
        root: Box<MvsPackageVersion>,
        dependency: Box<MvsPackageVersion>,
    },
    MultipleSelectedVersionsForNode {
        node: MvsNode,
        versions: Vec<MvsPackageVersion>,
    },
    DependencyContractsMismatch {
        missing: Vec<MvsPackageVersion>,
        unexpected: Vec<MvsPackageVersion>,
    },
    DependencyContractKeyMismatch {
        key: Box<MvsPackageVersion>,
        contract: Box<MvsPackageVersion>,
    },
    DependencyContractMetadataMismatch(MvsPackageVersion),
    DependencyTestRequirement {
        package: MvsPackageVersion,
        span: Range<usize>,
    },
    RootContextMismatch(MvsPackageVersion),
    SelectedGraphDependencySetMismatch {
        missing: Vec<MvsPackageVersion>,
        unexpected: Vec<MvsPackageVersion>,
    },
    RequirementEdgeMismatch {
        requirer: MvsRequirer,
        span: Range<usize>,
    },
    UnexpectedGraphEdge {
        requirer: MvsRequirer,
        span: Range<usize>,
    },
    UnreachableSelectedDependency(MvsPackageVersion),
    SelectedGraphContainsCycle(Vec<MvsPackageVersion>),
    TargetNotSupportedByDependency {
        package: MvsPackageVersion,
        target: Box<str>,
        /// The deterministic active dependency chain from the root through
        /// the unsupported package, including both endpoints.
        chain: Vec<MvsPackageVersion>,
    },
}

impl fmt::Display for BuildActiveGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not activate selected package graph: {self:?}"
        )
    }
}

impl std::error::Error for BuildActiveGraphError {}

/// Derives the active graph for `target` from complete resolver contracts.
///
/// `dependency_contracts` must contain exactly one resolver contract for each
/// selected dependency version, including dependencies that are inactive for
/// this target. Dependency contracts are validated against the root context
/// and selected graph before activation. Their test requirements are rejected
/// because only root test requirements participate in test analysis.
///
/// This function is not a Mode A lock-build input. Mode A must derive edge
/// activation from the complete lock graph and load contracts only for active
/// packages.
pub fn build_active_graph_from_resolver_contracts(
    root_contract: &ResolverContract,
    graph: &MvsSelectedGraph,
    dependency_contracts: &BTreeMap<MvsPackageVersion, ResolverContract>,
    target: &str,
    test_analysis: bool,
) -> Result<BuildActiveGraph, BuildActiveGraphError> {
    if root_contract.role() != PackageRole::Root {
        return Err(BuildActiveGraphError::RootRoleMismatch(
            root_contract.role(),
        ));
    }

    let root_version = root_contract.manifest_package_version();
    let root_mvs_version = root_contract.mvs.package_version();
    if root_mvs_version != root_version {
        return Err(BuildActiveGraphError::RootMvsPackageVersionMismatch {
            manifest: Box::new(root_version.clone()),
            mvs: Box::new(root_mvs_version.clone()),
        });
    }
    if root_version != &graph.root {
        return Err(BuildActiveGraphError::RootPackageMismatch {
            contract: Box::new(root_version.clone()),
            graph: Box::new(graph.root.clone()),
        });
    }
    if !root_contract.mvs_metadata_is_consistent() {
        return Err(BuildActiveGraphError::RootContractMetadataMismatch(
            root_version.clone(),
        ));
    }
    if !root_contract
        .root_targets()
        .iter()
        .any(|root_target| root_target.as_ref() == target)
    {
        return Err(BuildActiveGraphError::TargetNotDeclaredByRoot(
            target.into(),
        ));
    }
    if !graph.is_valid() {
        return Err(BuildActiveGraphError::InvalidSelectedGraph {
            conflicts: graph.conflicts.clone(),
            cycle: graph.cycle.clone(),
        });
    }
    validate_selected_node_versions(graph)?;

    let contract_versions = dependency_contracts
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if contract_versions != graph.dependencies {
        return Err(BuildActiveGraphError::DependencyContractsMismatch {
            missing: graph
                .dependencies
                .difference(&contract_versions)
                .cloned()
                .collect(),
            unexpected: contract_versions
                .difference(&graph.dependencies)
                .cloned()
                .collect(),
        });
    }

    for (key, contract) in dependency_contracts {
        if contract.role() != PackageRole::Dependency
            || contract.manifest_package_version() != key
            || contract.mvs.package_version() != key
        {
            return Err(BuildActiveGraphError::DependencyContractKeyMismatch {
                key: Box::new(key.clone()),
                contract: Box::new(contract.mvs.package_version().clone()),
            });
        }
        if contract.root_targets() != root_contract.root_targets()
            || !contract.mvs_metadata_is_consistent()
        {
            return Err(BuildActiveGraphError::DependencyContractMetadataMismatch(
                key.clone(),
            ));
        }
        if !root_contract.shares_root_context_with(contract) {
            return Err(BuildActiveGraphError::RootContextMismatch(key.clone()));
        }
        if let Some(requirement) = contract
            .requirements()
            .iter()
            .find(|requirement| requirement.test)
        {
            return Err(BuildActiveGraphError::DependencyTestRequirement {
                package: key.clone(),
                span: requirement.span.clone(),
            });
        }
    }

    let graph_target_versions = graph
        .edges
        .iter()
        .map(|edge| edge.target.clone())
        .collect::<BTreeSet<_>>();
    if graph_target_versions != graph.dependencies {
        return Err(BuildActiveGraphError::SelectedGraphDependencySetMismatch {
            missing: graph
                .dependencies
                .difference(&graph_target_versions)
                .cloned()
                .collect(),
            unexpected: graph_target_versions
                .difference(&graph.dependencies)
                .cloned()
                .collect(),
        });
    }

    let mut edge_active = vec![false; graph.edges.len()];
    let mut consumed_edges = vec![false; graph.edges.len()];
    let contract_edge_index = build_contract_edge_index(graph);
    {
        let mut edge_validation = ContractEdgeValidation {
            graph,
            edge_index: &contract_edge_index,
            target,
            test_analysis,
            consumed_edges: &mut consumed_edges,
            edge_active: &mut edge_active,
        };
        validate_contract_edges(
            root_contract,
            &MvsRequirer::Root(root_version.clone()),
            &mut edge_validation,
        )?;
        for (package_version, contract) in dependency_contracts {
            validate_contract_edges(
                contract,
                &MvsRequirer::Package(package_version.clone()),
                &mut edge_validation,
            )?;
        }
    }

    if let Some((_, edge)) = graph
        .edges
        .iter()
        .enumerate()
        .find(|(index, _)| !consumed_edges[*index])
    {
        return Err(BuildActiveGraphError::UnexpectedGraphEdge {
            requirer: edge.requirer.clone(),
            span: edge.span.clone(),
        });
    }

    let mut adjacency = BTreeMap::<MvsPackageVersion, Vec<usize>>::new();
    for (index, edge) in graph.edges.iter().enumerate() {
        adjacency
            .entry(edge.requirer.package_version().clone())
            .or_default()
            .push(index);
    }
    validate_reachability_and_acyclicity(graph, &adjacency)?;

    let mut active_packages = BTreeSet::from([graph.root.clone()]);
    let mut active_parents = BTreeMap::<MvsPackageVersion, MvsPackageVersion>::new();
    let mut pending = VecDeque::from([graph.root.clone()]);
    while let Some(source) = pending.pop_front() {
        let Some(edge_indices) = adjacency.get(&source) else {
            continue;
        };
        for &index in edge_indices {
            if !edge_active[index] {
                continue;
            }
            let target_version = graph.edges[index].target.clone();
            if active_packages.insert(target_version.clone()) {
                active_parents.insert(target_version.clone(), source.clone());
                let contract = dependency_contracts
                    .get(&target_version)
                    .expect("selected dependency has a validated contract");
                if !contract
                    .package_targets()
                    .iter()
                    .any(|supported| supported.as_ref() == target)
                {
                    return Err(BuildActiveGraphError::TargetNotSupportedByDependency {
                        chain: active_dependency_chain(
                            &graph.root,
                            &target_version,
                            &active_parents,
                        ),
                        package: target_version,
                        target: target.into(),
                    });
                }
                pending.push_back(target_version);
            }
        }
    }

    let edges = graph
        .edges
        .iter()
        .zip(edge_active)
        .filter(|(edge, active)| {
            *active && active_packages.contains(edge.requirer.package_version())
        })
        .map(|(edge, _)| edge.clone())
        .collect();
    let packages = graph
        .dependencies
        .iter()
        .filter(|package| active_packages.contains(*package))
        .cloned()
        .collect();

    Ok(BuildActiveGraph {
        root: graph.root.clone(),
        packages,
        edges,
    })
}

fn build_contract_edge_index(graph: &MvsSelectedGraph) -> HashMap<ContractEdgeKey, Vec<usize>> {
    let mut index = HashMap::<ContractEdgeKey, Vec<usize>>::with_capacity(graph.edges.len());
    for (edge_index, edge) in graph.edges.iter().enumerate() {
        index
            .entry(ContractEdgeKey::new(
                &edge.requirer,
                edge.named_target.package(),
                &edge.span,
                &edge.requirement,
            ))
            .or_default()
            .push(edge_index);
    }
    index
}

struct ContractEdgeValidation<'a> {
    graph: &'a MvsSelectedGraph,
    edge_index: &'a HashMap<ContractEdgeKey, Vec<usize>>,
    target: &'a str,
    test_analysis: bool,
    consumed_edges: &'a mut [bool],
    edge_active: &'a mut [bool],
}

fn active_dependency_chain(
    root: &MvsPackageVersion,
    package: &MvsPackageVersion,
    parents: &BTreeMap<MvsPackageVersion, MvsPackageVersion>,
) -> Vec<MvsPackageVersion> {
    let mut chain = vec![package.clone()];
    let mut current = package;
    while current != root {
        current = parents
            .get(current)
            .expect("active dependency has a parent path to the root");
        chain.push(current.clone());
    }
    chain.reverse();
    chain
}

fn validate_selected_node_versions(graph: &MvsSelectedGraph) -> Result<(), BuildActiveGraphError> {
    let root_node = graph.root.node();
    let mut selected_by_node = BTreeMap::<MvsNode, MvsPackageVersion>::new();
    selected_by_node.insert(root_node.clone(), graph.root.clone());
    for dependency in &graph.dependencies {
        if dependency.node() == root_node {
            return Err(BuildActiveGraphError::RootNodeSelectedAsDependency {
                root: Box::new(graph.root.clone()),
                dependency: Box::new(dependency.clone()),
            });
        }
        let node = dependency.node();
        if let Some(previous) = selected_by_node.get(&node) {
            if previous.version() != dependency.version() {
                let mut versions = vec![previous.clone(), dependency.clone()];
                versions.sort();
                return Err(BuildActiveGraphError::MultipleSelectedVersionsForNode {
                    node,
                    versions,
                });
            }
        } else {
            selected_by_node.insert(node, dependency.clone());
        }
    }
    Ok(())
}

fn validate_contract_edges(
    contract: &ResolverContract,
    requirer: &MvsRequirer,
    validation: &mut ContractEdgeValidation<'_>,
) -> Result<(), BuildActiveGraphError> {
    for requirement in contract.requirements() {
        let key = ContractEdgeKey::new(
            requirer,
            &requirement.package,
            &requirement.span,
            &requirement.version,
        );
        let matches = validation
            .edge_index
            .get(&key)
            .map_or(&[][..], Vec::as_slice);

        match &requirement.resolution {
            ResolverRequirementResolution::ExcludedByRootTargets => {
                if !matches.is_empty() {
                    return Err(BuildActiveGraphError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                }
            }
            ResolverRequirementResolution::Included { named_version, pin } => {
                let [index] = matches else {
                    return Err(BuildActiveGraphError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                };
                let index = *index;
                let edge = &validation.graph.edges[index];
                if validation.consumed_edges[index]
                    || edge.target.package() != requirement.package.as_ref()
                    || edge.named_target.version() != named_version
                    || edge.target.node() != edge.named_target.node()
                    || !requirement
                        .version
                        .matches_for_resolution(edge.target.version())
                    || !validation.graph.dependencies.contains(&edge.target)
                    || match pin {
                        ResolverPinKind::Minimum => edge.target.version() < named_version,
                        ResolverPinKind::Path | ResolverPinKind::RootPathOverride => {
                            edge.target.version() != named_version
                        }
                    }
                {
                    return Err(BuildActiveGraphError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                }
                validation.consumed_edges[index] = true;
                validation.edge_active[index] = requirement.only.as_ref().is_none_or(|only| {
                    only.iter()
                        .any(|only_target| only_target.as_ref() == validation.target)
                }) && (!requirement.test
                    || contract.role() == PackageRole::Root && validation.test_analysis);
            }
        }
    }
    Ok(())
}

fn validate_reachability_and_acyclicity(
    graph: &MvsSelectedGraph,
    adjacency: &BTreeMap<MvsPackageVersion, Vec<usize>>,
) -> Result<(), BuildActiveGraphError> {
    let mut reachable = BTreeSet::from([graph.root.clone()]);
    let mut pending = VecDeque::from([graph.root.clone()]);
    while let Some(source) = pending.pop_front() {
        if let Some(edge_indices) = adjacency.get(&source) {
            for &index in edge_indices {
                let target = graph.edges[index].target.clone();
                if reachable.insert(target.clone()) {
                    pending.push_back(target);
                }
            }
        }
    }
    if let Some(package) = graph
        .dependencies
        .iter()
        .find(|package| !reachable.contains(*package))
    {
        return Err(BuildActiveGraphError::UnreachableSelectedDependency(
            package.clone(),
        ));
    }

    let mut vertices = graph.dependencies.clone();
    vertices.insert(graph.root.clone());
    let mut in_degree = vertices
        .iter()
        .cloned()
        .map(|vertex| (vertex, 0usize))
        .collect::<BTreeMap<_, _>>();
    for edge in &graph.edges {
        let Some(in_degree) = in_degree.get_mut(&edge.target) else {
            return Err(BuildActiveGraphError::SelectedGraphDependencySetMismatch {
                missing: Vec::new(),
                unexpected: vec![edge.target.clone()],
            });
        };
        *in_degree += 1;
    }

    let mut ready = in_degree
        .iter()
        .filter_map(|(package, degree)| (*degree == 0).then_some(package.clone()))
        .collect::<BTreeSet<_>>();
    let mut visited = 0usize;
    while let Some(source) = ready.pop_first() {
        visited += 1;
        if let Some(edge_indices) = adjacency.get(&source) {
            for &index in edge_indices {
                let target = &graph.edges[index].target;
                let degree = in_degree
                    .get_mut(target)
                    .expect("selected graph target was checked above");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(target.clone());
                }
            }
        }
    }
    if visited != vertices.len() {
        return Err(BuildActiveGraphError::SelectedGraphContainsCycle(
            in_degree
                .into_iter()
                .filter_map(|(package, degree)| (degree > 0).then_some(package))
                .collect(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        BuildActiveGraphError, ContractEdgeKey, build_active_graph_from_resolver_contracts,
        build_contract_edge_index, validate_reachability_and_acyclicity,
    };
    use crate::manifest::{PackageParseOutput, PackageRole, parse, parse_for_role};
    use crate::mvs::{MvsPackageVersion, MvsRequirer, select};
    use crate::mvs_graph_validation::{MvsDependencyCycle, check_selected_graph};
    use crate::resolver_contract::{
        ResolverContract, ResolverContractContext, build_resolver_contract,
    };
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    struct Fixture {
        root: ResolverContract,
        graph: crate::mvs_graph_validation::MvsSelectedGraph,
        contracts: BTreeMap<MvsPackageVersion, ResolverContract>,
    }

    fn parse_dependency(source: &[u8]) -> PackageParseOutput {
        parse_for_role(source, PackageRole::Dependency)
    }

    fn fixture() -> Fixture {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:base @ ^1 from git "https://example.test/base"
    acme:wasi-only @ ^1 from git "https://example.test/wasi-only" only { wasm32-wasi }
}
requires test {
    acme:test-dep @ ^1 from git "https://example.test/test-dep"
    acme:test-wasi-only @ ^1 from git "https://example.test/test-wasi-only" only { wasm32-wasi }
}
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("valid root contract");

        let manifests = [
            br#"package acme:base @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi, wasm64-wasi }
requires {
    acme:transitive @ ^1 from git "https://example.test/transitive" only { wasm32-core }
    acme:excluded-by-root @ ^1 from git "https://example.test/excluded" only { wasm64-wasi }
}
requires test { acme:dependency-test @ ^1 from git "https://example.test/dependency-test" }
"#
            .as_slice(),
            br#"package acme:wasi-only @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires { acme:orphan @ ^1 from git "https://example.test/orphan" }
"#,
            br#"package acme:test-dep @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
"#,
            br#"package acme:test-wasi-only @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
"#,
            br#"package acme:transitive @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
"#,
            br#"package acme:orphan @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
"#,
        ];
        let mut contracts = BTreeMap::new();
        for manifest in manifests {
            let parsed = parse_dependency(manifest);
            let contract =
                build_resolver_contract(&parsed, &context, PackageValidationContext::default())
                    .expect("valid dependency contract");
            contracts.insert(contract.mvs.package_version().clone(), contract);
        }

        let catalog = contracts
            .iter()
            .map(|(package_version, contract)| (package_version.clone(), contract.mvs.clone()))
            .collect::<HashMap<_, _>>();
        let selection = select(
            &root.mvs,
            &catalog,
            &HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("complete MVS selection");
        let graph = check_selected_graph(&root.mvs, &selection).expect("valid selected graph");
        Fixture {
            root,
            graph,
            contracts,
        }
    }

    fn target_support_fixture() -> Fixture {
        let root_manifest = parse(
            br#"package acme:target-root @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:inactive @ ^1 from git "https://example.test/inactive" only { wasm32-wasi }
}
requires test {
    acme:test-mid @ ^1 from git "https://example.test/test-mid"
}
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("valid root contract");

        let manifests = [
            br#"package acme:inactive @ 1.0.0
source "src"
targets { wasm32-core }
"#
            .as_slice(),
            br#"package acme:test-mid @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires { acme:test-only @ ^1 from git "https://example.test/test-only" }
"#
            .as_slice(),
            br#"package acme:test-only @ 1.0.0
source "src"
targets { wasm32-wasi }
"#
            .as_slice(),
        ];
        let mut contracts = BTreeMap::new();
        for manifest in manifests {
            let parsed = parse_dependency(manifest);
            let contract =
                build_resolver_contract(&parsed, &context, PackageValidationContext::default())
                    .expect("valid dependency contract");
            contracts.insert(contract.mvs.package_version().clone(), contract);
        }
        let catalog = contracts
            .iter()
            .map(|(package_version, contract)| (package_version.clone(), contract.mvs.clone()))
            .collect::<HashMap<_, _>>();
        let selection = select(
            &root.mvs,
            &catalog,
            &HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("complete MVS selection");
        let graph = check_selected_graph(&root.mvs, &selection).expect("valid selected graph");
        Fixture {
            root,
            graph,
            contracts,
        }
    }

    #[test]
    fn ordinary_build_filters_test_and_only_edges_and_keeps_only_reachable_packages() {
        let fixture = fixture();
        let active = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-core",
            false,
        )
        .expect("active graph is valid");
        let packages = active
            .packages
            .iter()
            .map(|package| package.package())
            .collect::<Vec<_>>();
        assert_eq!(packages, ["acme:base", "acme:transitive"]);
        assert_eq!(active.edges.len(), 2);
        assert!(active.edges.iter().all(|edge| {
            edge.requirer.package_version().package() != "acme:wasi-only"
                && edge.requirer.package_version().package() != "acme:test-dep"
                && edge.requirer.package_version().package() != "acme:test-wasi-only"
                && edge.target.package() != "acme:orphan"
        }));
    }

    #[test]
    fn contract_edge_index_indexes_each_selected_edge_once() {
        let fixture = fixture();
        let index = build_contract_edge_index(&fixture.graph);
        assert_eq!(
            index.values().map(Vec::len).sum::<usize>(),
            fixture.graph.edges.len()
        );
        for (edge_index, edge) in fixture.graph.edges.iter().enumerate() {
            let key = ContractEdgeKey::new(
                &edge.requirer,
                edge.named_target.package(),
                &edge.span,
                &edge.requirement,
            );
            assert!(
                index
                    .get(&key)
                    .is_some_and(|indices| indices.contains(&edge_index))
            );
        }
    }

    #[test]
    fn duplicate_contract_edge_matches_are_rejected() {
        let fixture = fixture();
        let mut duplicate_graph = fixture.graph.clone();
        let root_edge = duplicate_graph
            .edges
            .iter()
            .find(|edge| matches!(&edge.requirer, MvsRequirer::Root(_)))
            .expect("fixture has a root edge")
            .clone();
        duplicate_graph.edges.push(root_edge);

        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &duplicate_graph,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::RequirementEdgeMismatch { .. })
        ));
    }

    #[test]
    fn test_analysis_activates_root_test_edges_but_never_dependency_test_edges() {
        let fixture = fixture();
        let active = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-core",
            true,
        )
        .expect("active graph is valid");
        let packages = active
            .packages
            .iter()
            .map(|package| package.package())
            .collect::<Vec<_>>();
        assert_eq!(packages, ["acme:base", "acme:test-dep", "acme:transitive"]);
        assert!(!packages.contains(&"acme:dependency-test"));
        assert!(
            !active
                .edges
                .iter()
                .any(|edge| edge.target.package() == "acme:dependency-test")
        );
    }

    #[test]
    fn only_edges_are_selected_for_the_requested_root_target() {
        let fixture = fixture();
        let active = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-wasi",
            true,
        )
        .expect("active graph is valid");
        let packages = active
            .packages
            .iter()
            .map(|package| package.package())
            .collect::<Vec<_>>();
        assert_eq!(
            packages,
            [
                "acme:base",
                "acme:orphan",
                "acme:test-dep",
                "acme:test-wasi-only",
                "acme:wasi-only",
            ]
        );
    }

    #[test]
    fn incomplete_graph_and_missing_or_mismatched_contracts_are_rejected() {
        let fixture = fixture();
        let mut incomplete = fixture.graph.clone();
        incomplete.edges.pop();
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &incomplete,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::SelectedGraphDependencySetMismatch { .. })
                | Err(BuildActiveGraphError::RequirementEdgeMismatch { .. })
        ));

        let empty_contracts = BTreeMap::new();
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &fixture.graph,
                &empty_contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::DependencyContractsMismatch { .. })
        ));

        let mut mismatched_contracts = fixture.contracts.clone();
        let keys = mismatched_contracts.keys().cloned().collect::<Vec<_>>();
        let first = keys.first().expect("fixture has dependencies").clone();
        let second = keys.get(1).expect("fixture has two dependencies").clone();
        let first_contract = mismatched_contracts[&first].clone();
        let second_contract = mismatched_contracts[&second].clone();
        mismatched_contracts.insert(first, second_contract);
        mismatched_contracts.insert(second, first_contract);
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &fixture.graph,
                &mismatched_contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::DependencyContractKeyMismatch { .. })
        ));
    }

    #[test]
    fn invalid_selected_graph_is_rejected() {
        let fixture = fixture();
        let mut invalid = fixture.graph.clone();
        invalid.cycle = Some(MvsDependencyCycle {
            nodes: vec![invalid.root.clone(), invalid.root.clone()],
            spans: std::iter::once(0..1).collect(),
        });
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &invalid,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::InvalidSelectedGraph { .. })
        ));
    }

    #[test]
    fn dependency_edge_excluded_by_root_targets_is_not_an_active_edge() {
        let fixture = fixture();
        assert!(!fixture.graph.edges.iter().any(|edge| {
            edge.target.package() == "acme:excluded-by-root"
                || edge.named_target.package() == "acme:excluded-by-root"
        }));
        let active = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-core",
            false,
        )
        .expect("root-excluded requirement is omitted");
        assert!(
            active
                .packages
                .iter()
                .all(|package| package.package() != "acme:excluded-by-root")
        );
    }

    #[test]
    fn target_support_is_checked_only_for_dependencies_active_in_this_build() {
        let fixture = target_support_fixture();
        let ordinary = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-core",
            false,
        )
        .expect("only and test edges are inactive in an ordinary build");
        assert!(ordinary.packages.is_empty());

        let error = build_active_graph_from_resolver_contracts(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            "wasm32-core",
            true,
        )
        .expect_err("active test dependency does not support the target");
        match error {
            BuildActiveGraphError::TargetNotSupportedByDependency {
                package,
                target,
                chain,
            } => {
                assert_eq!(package.package(), "acme:test-only");
                assert_eq!(target.as_ref(), "wasm32-core");
                assert_eq!(
                    chain
                        .iter()
                        .map(MvsPackageVersion::package)
                        .collect::<Vec<_>>(),
                    ["acme:target-root", "acme:test-mid", "acme:test-only"]
                );
                assert_eq!(chain.first(), Some(&fixture.graph.root));
                assert_eq!(chain.last(), Some(&package));
            }
            other => panic!("expected target support error, got {other:?}"),
        }
    }

    #[test]
    fn selected_graph_rejects_multiple_versions_for_one_node_and_the_root_node() {
        let fixture = fixture();
        let mut duplicate_line = fixture.graph.clone();
        duplicate_line.dependencies.insert(
            MvsPackageVersion::new("acme:base", Version::parse("1.1.0").expect("valid version"))
                .expect("valid package version"),
        );
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &duplicate_line,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::MultipleSelectedVersionsForNode { .. })
        ));

        let mut root_node_dependency = fixture.graph.clone();
        root_node_dependency.dependencies.insert(
            MvsPackageVersion::new(
                fixture.graph.root.package().to_owned(),
                Version::parse("1.1.0").expect("valid version"),
            )
            .expect("valid package version"),
        );
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &root_node_dependency,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::RootNodeSelectedAsDependency { .. })
        ));
    }

    #[test]
    fn reachability_check_rejects_unreachable_selected_dependencies() {
        let fixture = fixture();
        let mut disconnected = fixture.graph.clone();
        disconnected
            .edges
            .retain(|edge| edge.target.package() != "acme:orphan");
        let adjacency = adjacency(&disconnected);
        assert!(matches!(
            validate_reachability_and_acyclicity(&disconnected, &adjacency),
            Err(BuildActiveGraphError::UnreachableSelectedDependency(_))
        ));
    }

    #[test]
    fn acyclicity_check_finds_a_cycle_even_without_a_recorded_cycle() {
        let fixture = fixture();
        let mut graph = fixture.graph.clone();
        let mut back_edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.requirer.package_version().package() == "acme:base"
                    && edge.target.package() == "acme:transitive"
            })
            .expect("fixture has a base-to-transitive edge")
            .clone();
        back_edge.requirer = MvsRequirer::Package(
            MvsPackageVersion::new(
                "acme:transitive",
                Version::parse("1.0.0").expect("valid version"),
            )
            .expect("valid package version"),
        );
        back_edge.target =
            MvsPackageVersion::new("acme:base", Version::parse("1.0.0").expect("valid version"))
                .expect("valid package version");
        graph.edges.push(back_edge);
        let adjacency = adjacency(&graph);
        assert!(matches!(
            validate_reachability_and_acyclicity(&graph, &adjacency),
            Err(BuildActiveGraphError::SelectedGraphContainsCycle(_))
        ));
    }

    #[test]
    fn requirement_mismatches_and_unexpected_edges_are_rejected() {
        let fixture = fixture();
        let mut mismatched = fixture.graph.clone();
        mismatched.edges[0].span = 100..101;
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &mismatched,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::RequirementEdgeMismatch { .. })
        ));

        let mut unexpected = fixture.graph.clone();
        let mut extra = unexpected
            .edges
            .iter()
            .find(|edge| {
                matches!(edge.requirer, MvsRequirer::Root(_))
                    && edge.target.package() == "acme:base"
            })
            .expect("fixture has a root-to-base edge")
            .clone();
        extra.requirer = MvsRequirer::Package(
            MvsPackageVersion::new("acme:base", Version::parse("1.0.0").expect("valid version"))
                .expect("valid package version"),
        );
        unexpected.edges.push(extra);
        assert!(matches!(
            build_active_graph_from_resolver_contracts(
                &fixture.root,
                &unexpected,
                &fixture.contracts,
                "wasm32-core",
                false,
            ),
            Err(BuildActiveGraphError::UnexpectedGraphEdge { .. })
        ));
    }

    fn adjacency(
        graph: &crate::mvs_graph_validation::MvsSelectedGraph,
    ) -> BTreeMap<MvsPackageVersion, Vec<usize>> {
        let mut adjacency = BTreeMap::<MvsPackageVersion, Vec<usize>>::new();
        for (index, edge) in graph.edges.iter().enumerate() {
            adjacency
                .entry(edge.requirer.package_version().clone())
                .or_default()
                .push(index);
        }
        adjacency
    }
}
