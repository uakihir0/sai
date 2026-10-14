//! Stable SAI package graph values prepared by the driver after resolution.
//!
//! This model deliberately contains stable package keys and strings rather
//! than session-local `PackageId`s. The driver can attach interner IDs later,
//! at the boundary where query-facing package identities are needed.
//! Implicit `sai:core` and `sai:alloc` packages are toolchain inputs, not graph
//! nodes; WIT view packages are modeled by the later WIT package-scope layer.

use sai_package::identity::PackageIdentity;
use sai_package::lock_active_graph::{LockActiveGraph, LockActiveNode};
use sai_package::mvs::MvsPackageVersion;
use sai_package::version::Version;
use sai_stable_hash::{StableHash, StableHasher};
use sai_vfs::PackageKey;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Whether an active package is compiled from source or consumed as an artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageForm {
    Source,
    Artifact,
}

/// The package's place in the resolved build graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageGraphRole {
    Root,
    WorkspaceMember,
    Dependency,
}

/// One active dependency edge, identified without session-local IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageGraphEdge {
    target: PackageKey,
    visible_identity: String,
}

impl PackageGraphEdge {
    /// Creates an edge to a stable package key.
    ///
    /// `visible_identity` is the package identity that source code names in
    /// `use` declarations. Graph construction verifies it against the target
    /// node's declared identity.
    pub fn new(
        target: PackageKey,
        visible_identity: impl Into<String>,
    ) -> Result<Self, PackageGraphError> {
        let visible_identity = visible_identity.into();
        if !is_package_identity(&visible_identity) {
            return Err(PackageGraphError::InvalidVisibleIdentity(visible_identity));
        }
        Ok(Self {
            target,
            visible_identity,
        })
    }

    pub fn target(&self) -> &PackageKey {
        &self.target
    }

    pub fn visible_identity(&self) -> &str {
        &self.visible_identity
    }
}

impl StableHash for PackageGraphEdge {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE2);
        self.target.stable_hash(hasher);
        self.visible_identity.stable_hash(hasher);
    }
}

/// One package unit selected for the current build.
///
/// `version` is the exact version from the package contract, even though
/// ordinary `PackageKey`s identify a compatibility line. The implicit
/// `sai:core` and `sai:alloc` packages are stored separately on the graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageGraphNode {
    key: PackageKey,
    identity: String,
    version: Version,
    form: PackageForm,
    role: PackageGraphRole,
    active_edges: Vec<PackageGraphEdge>,
}

impl PackageGraphNode {
    /// Creates a package graph node and validates its key, identity, version,
    /// role, form, and outgoing edge uniqueness.
    pub fn new(
        key: PackageKey,
        identity: impl Into<String>,
        version: Version,
        form: PackageForm,
        role: PackageGraphRole,
        active_edges: impl IntoIterator<Item = PackageGraphEdge>,
    ) -> Result<Self, PackageGraphError> {
        let identity = identity.into();
        validate_node_identity(&key, &identity, &version, form, role)?;

        let mut active_edges = active_edges.into_iter().collect::<Vec<_>>();
        active_edges.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.visible_identity.cmp(&right.visible_identity))
        });
        let mut visible_identities = BTreeSet::new();
        for pair in active_edges.windows(2) {
            if pair[0].target == pair[1].target {
                return Err(PackageGraphError::DuplicateEdgeTarget {
                    source: key,
                    target: pair[0].target.clone(),
                });
            }
        }
        for edge in &active_edges {
            if edge.visible_identity == identity {
                return Err(PackageGraphError::SelfDependency {
                    source: key,
                    identity,
                });
            }
            if !visible_identities.insert(edge.visible_identity.clone()) {
                return Err(PackageGraphError::DuplicateEdgeIdentity {
                    source: key,
                    identity: edge.visible_identity.clone(),
                });
            }
        }

        Ok(Self {
            key,
            identity,
            version,
            form,
            role,
            active_edges,
        })
    }

    pub fn key(&self) -> &PackageKey {
        &self.key
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub const fn form(&self) -> PackageForm {
        self.form
    }

    pub const fn role(&self) -> PackageGraphRole {
        self.role
    }

    pub fn active_edges(&self) -> &[PackageGraphEdge] {
        &self.active_edges
    }
}

impl StableHash for PackageGraphNode {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE1);
        self.key.stable_hash(hasher);
        self.identity.stable_hash(hasher);
        // `Version` is canonical and its Display form includes every exact
        // version component, including pre-release identifiers.
        self.version.to_string().stable_hash(hasher);
        self.form.stable_hash(hasher);
        self.role.stable_hash(hasher);
        self.active_edges.stable_hash(hasher);
    }
}

impl StableHash for PackageForm {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE3);
        hasher.write_tag(match self {
            Self::Source => 0,
            Self::Artifact => 1,
        });
    }
}

impl StableHash for PackageGraphRole {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE4);
        hasher.write_tag(match self {
            Self::Root => 0,
            Self::WorkspaceMember => 1,
            Self::Dependency => 2,
        });
    }
}

/// A validated package graph whose node and edge order is canonical.
///
/// The toolchain version is stored separately from `nodes`: implicit
/// `sai:core` and `sai:alloc` package units have identity-only session keys,
/// but do not participate in resolver graph topology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageGraph {
    root: PackageKey,
    toolchain_version: Version,
    nodes: Vec<PackageGraphNode>,
}

/// Source/artifact metadata not retained by a lock-backed active graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockActivePackageMetadata {
    pub form: PackageForm,
    pub role: PackageGraphRole,
}

/// Invalid lock-active-graph input while adapting it to the driver's graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockActiveGraphAdapterError {
    InvalidPackageIdentity(String),
    NodeRoleMismatch {
        node: LockActiveNode,
        role: PackageGraphRole,
    },
    DuplicateActiveNode(MvsPackageVersion),
    MissingDependencyMetadata(MvsPackageVersion),
    UnexpectedDependencyMetadata(MvsPackageVersion),
    EdgeEndpointMissing {
        endpoint: LockActiveNode,
    },
    EdgeEndpointMismatch {
        endpoint: LockActiveNode,
        expected: LockActiveNode,
    },
    InvalidPackageKey(sai_vfs::PackageKeyError),
    PackageGraph(PackageGraphError),
}

impl fmt::Display for LockActiveGraphAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not adapt lock active package graph: {self:?}"
        )
    }
}

impl std::error::Error for LockActiveGraphAdapterError {}

impl From<PackageGraphError> for LockActiveGraphAdapterError {
    fn from(error: PackageGraphError) -> Self {
        Self::PackageGraph(error)
    }
}

impl PackageGraph {
    /// Builds the active resolved graph, rejecting duplicate nodes, dangling
    /// or mismatched edges, nodes unreachable from the root, cycles, and
    /// graphs without exactly one root.
    ///
    /// `toolchain_version` records the version of the implicit toolchain
    /// packages. Those packages are available to every graph but are not
    /// resolver graph nodes.
    pub fn new(
        toolchain_version: Version,
        nodes: impl IntoIterator<Item = PackageGraphNode>,
    ) -> Result<Self, PackageGraphError> {
        let mut nodes = nodes.into_iter().collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.key.cmp(&right.key));

        for pair in nodes.windows(2) {
            if pair[0].key == pair[1].key {
                return Err(PackageGraphError::DuplicateNode(pair[0].key.clone()));
            }
        }
        let roots = nodes
            .iter()
            .filter(|node| node.role == PackageGraphRole::Root)
            .collect::<Vec<_>>();
        if roots.len() != 1 {
            return Err(PackageGraphError::InvalidRootCardinality(roots.len()));
        }
        let root = roots[0].key.clone();

        for node in &nodes {
            for edge in &node.active_edges {
                let target = nodes
                    .binary_search_by(|candidate| candidate.key.cmp(&edge.target))
                    .ok()
                    .map(|index| &nodes[index])
                    .ok_or_else(|| PackageGraphError::MissingEdgeTarget {
                        source: node.key.clone(),
                        target: edge.target.clone(),
                    })?;
                if node.key == target.key {
                    return Err(PackageGraphError::SelfDependency {
                        source: node.key.clone(),
                        identity: node.identity.clone(),
                    });
                }
                if target.identity != edge.visible_identity {
                    return Err(PackageGraphError::VisibleIdentityMismatch {
                        source: node.key.clone(),
                        target: edge.target.clone(),
                        expected: target.identity.clone(),
                        found: edge.visible_identity.clone(),
                    });
                }
            }
        }
        validate_reachable(&nodes)?;
        validate_acyclic(&nodes)?;

        Ok(Self {
            root,
            toolchain_version,
            nodes,
        })
    }

    /// Converts a lock-backed active graph into the driver's stable graph.
    ///
    /// The lock graph does not retain source/artifact form or package role for
    /// dependencies, so `dependency_metadata` must describe every active
    /// dependency and no other package. The root is always a source package
    /// with the root role. `toolchain_version` is supplied separately because
    /// implicit toolchain packages are not nodes in either graph.
    pub fn from_lock_active_graph(
        lock_graph: &LockActiveGraph,
        dependency_metadata: &BTreeMap<MvsPackageVersion, LockActivePackageMetadata>,
        toolchain_version: Version,
    ) -> Result<Self, LockActiveGraphAdapterError> {
        let root_version = lock_node_version(&lock_graph.root)?;
        if !lock_graph.root.root {
            return Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                node: lock_graph.root.clone(),
                role: PackageGraphRole::Root,
            });
        }

        let mut dependencies = lock_graph.packages.iter().collect::<Vec<_>>();
        dependencies.sort();

        let mut expected_metadata = BTreeSet::new();
        let mut active_nodes = BTreeMap::new();
        let mut stable_keys = BTreeMap::new();
        let mut graph_nodes = Vec::with_capacity(dependencies.len() + 1);

        let root_key = lock_node_key(&lock_graph.root)?;
        active_nodes.insert(root_version.clone(), lock_graph.root.clone());
        stable_keys.insert(root_version.clone(), root_key.clone());
        let root_edges = Vec::<PackageGraphEdge>::new();
        graph_nodes.push((
            root_version.clone(),
            lock_graph.root.package.to_string(),
            lock_graph.root.version.clone(),
            PackageForm::Source,
            PackageGraphRole::Root,
            root_edges,
        ));

        for dependency in dependencies {
            let package_version = lock_node_version(dependency)?;
            if active_nodes
                .insert(package_version.clone(), dependency.clone())
                .is_some()
            {
                return Err(LockActiveGraphAdapterError::DuplicateActiveNode(
                    package_version,
                ));
            }
            if dependency.root {
                let role = dependency_metadata
                    .get(&package_version)
                    .map_or(PackageGraphRole::Dependency, |metadata| metadata.role);
                return Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                    node: dependency.clone(),
                    role,
                });
            }
            expected_metadata.insert(package_version.clone());
            let Some(metadata) = dependency_metadata.get(&package_version) else {
                return Err(LockActiveGraphAdapterError::MissingDependencyMetadata(
                    package_version,
                ));
            };
            if metadata.role == PackageGraphRole::Root {
                return Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                    node: dependency.clone(),
                    role: metadata.role,
                });
            }

            let key = lock_node_key(dependency)?;
            stable_keys.insert(package_version.clone(), key);
            graph_nodes.push((
                package_version,
                dependency.package.to_string(),
                dependency.version.clone(),
                metadata.form,
                metadata.role,
                Vec::new(),
            ));
        }

        if let Some(extra) = dependency_metadata
            .keys()
            .find(|package| !expected_metadata.contains(*package))
        {
            return Err(LockActiveGraphAdapterError::UnexpectedDependencyMetadata(
                extra.clone(),
            ));
        }

        let mut outgoing = BTreeMap::<MvsPackageVersion, Vec<PackageGraphEdge>>::new();
        let mut outgoing_targets = BTreeMap::<MvsPackageVersion, BTreeSet<PackageKey>>::new();
        for package_version in active_nodes.keys() {
            outgoing.insert(package_version.clone(), Vec::new());
            outgoing_targets.insert(package_version.clone(), BTreeSet::new());
        }

        let mut edges = lock_graph.edges.iter().collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            left.from
                .cmp(&right.from)
                .then_with(|| left.to.cmp(&right.to))
                .then_with(|| {
                    left.requirement
                        .to_string()
                        .cmp(&right.requirement.to_string())
                })
                .then_with(|| left.test.cmp(&right.test))
                .then_with(|| left.only.cmp(&right.only))
        });

        for edge in edges {
            let source_version = lock_node_version(&edge.from)?;
            let Some(expected_source) = active_nodes.get(&source_version) else {
                return Err(LockActiveGraphAdapterError::EdgeEndpointMissing {
                    endpoint: edge.from.clone(),
                });
            };
            if expected_source != &edge.from {
                return Err(LockActiveGraphAdapterError::EdgeEndpointMismatch {
                    endpoint: edge.from.clone(),
                    expected: expected_source.clone(),
                });
            }

            let target_version = lock_node_version(&edge.to)?;
            let Some(expected_target) = active_nodes.get(&target_version) else {
                return Err(LockActiveGraphAdapterError::EdgeEndpointMissing {
                    endpoint: edge.to.clone(),
                });
            };
            if expected_target != &edge.to {
                return Err(LockActiveGraphAdapterError::EdgeEndpointMismatch {
                    endpoint: edge.to.clone(),
                    expected: expected_target.clone(),
                });
            }

            let target_key = stable_keys
                .get(&target_version)
                .expect("every validated active node has a stable key");
            let package_edge =
                PackageGraphEdge::new(target_key.clone(), edge.to.package.to_string())?;
            // A malformed or externally constructed active graph can repeat
            // an edge to one package node. The driver graph models topology,
            // not lock requirement multiplicity, so retain it only once.
            if !outgoing_targets
                .get_mut(&source_version)
                .expect("every validated active node has an outgoing-edge entry")
                .insert(target_key.clone())
            {
                continue;
            }
            outgoing
                .get_mut(&source_version)
                .expect("every validated active node has an outgoing-edge entry")
                .push(package_edge);
        }

        let nodes = graph_nodes
            .into_iter()
            .map(|(package_version, identity, version, form, role, _)| {
                let key = stable_keys
                    .get(&package_version)
                    .expect("every graph node has a stable key")
                    .clone();
                let edges = outgoing
                    .remove(&package_version)
                    .expect("every graph node has an outgoing-edge entry");
                PackageGraphNode::new(key, identity, version, form, role, edges)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::new(toolchain_version, nodes)?)
    }

    pub fn root_key(&self) -> &PackageKey {
        &self.root
    }

    pub fn toolchain_version(&self) -> &Version {
        &self.toolchain_version
    }

    /// Returns the root node. Construction guarantees that it exists.
    pub fn root(&self) -> &PackageGraphNode {
        let index = self
            .nodes
            .binary_search_by(|node| node.key.cmp(&self.root))
            .expect("validated package graph retains its root");
        &self.nodes[index]
    }

    pub fn nodes(&self) -> &[PackageGraphNode] {
        &self.nodes
    }

    pub fn node(&self, key: &PackageKey) -> Option<&PackageGraphNode> {
        self.nodes
            .binary_search_by(|node| node.key.cmp(key))
            .ok()
            .map(|index| &self.nodes[index])
    }
}

fn lock_node_version(
    node: &LockActiveNode,
) -> Result<MvsPackageVersion, LockActiveGraphAdapterError> {
    MvsPackageVersion::new(node.package.clone(), node.version.clone())
        .map_err(|_| LockActiveGraphAdapterError::InvalidPackageIdentity(node.package.to_string()))
}

fn lock_node_key(node: &LockActiveNode) -> Result<PackageKey, LockActiveGraphAdapterError> {
    PackageKey::new(format!(
        "{}#{}",
        node.package,
        node.version.compatibility_line()
    ))
    .map_err(LockActiveGraphAdapterError::InvalidPackageKey)
}

impl StableHash for PackageGraph {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE0);
        self.root.stable_hash(hasher);
        self.toolchain_version.to_string().stable_hash(hasher);
        self.nodes.stable_hash(hasher);
    }
}

/// Validation failure while constructing a package graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageGraphError {
    InvalidPackageIdentity(String),
    InvalidVisibleIdentity(String),
    PackageKeyMismatch {
        key: PackageKey,
        expected: String,
    },
    ImplicitPackageIsNotGraphNode(String),
    SourceRoleMustUseSource(PackageGraphRole),
    DuplicateEdgeTarget {
        source: PackageKey,
        target: PackageKey,
    },
    DuplicateEdgeIdentity {
        source: PackageKey,
        identity: String,
    },
    SelfDependency {
        source: PackageKey,
        identity: String,
    },
    DuplicateNode(PackageKey),
    MissingEdgeTarget {
        source: PackageKey,
        target: PackageKey,
    },
    UnreachableNode(PackageKey),
    VisibleIdentityMismatch {
        source: PackageKey,
        target: PackageKey,
        expected: String,
        found: String,
    },
    InvalidRootCardinality(usize),
    DependencyCycle,
}

impl fmt::Display for PackageGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackageIdentity(identity) => {
                write!(formatter, "invalid package identity `{identity}`")
            }
            Self::InvalidVisibleIdentity(identity) => {
                write!(formatter, "invalid visible package identity `{identity}`")
            }
            Self::PackageKeyMismatch { key, expected } => write!(
                formatter,
                "package key `{}` does not match the identity and version; expected `{expected}`",
                key.as_str()
            ),
            Self::ImplicitPackageIsNotGraphNode(identity) => write!(
                formatter,
                "implicit package `{identity}` cannot be a resolved graph node"
            ),
            Self::SourceRoleMustUseSource(role) => {
                write!(formatter, "package with role {role:?} must use source form")
            }
            Self::DuplicateEdgeTarget { source, target } => write!(
                formatter,
                "package `{}` has more than one active edge to `{}`",
                source.as_str(),
                target.as_str()
            ),
            Self::DuplicateEdgeIdentity { source, identity } => write!(
                formatter,
                "package `{}` has more than one active edge for identity `{identity}`",
                source.as_str()
            ),
            Self::SelfDependency { source, identity } => write!(
                formatter,
                "package `{}` has an active dependency on its own identity `{identity}`",
                source.as_str()
            ),
            Self::DuplicateNode(key) => {
                write!(
                    formatter,
                    "package graph contains duplicate node `{}`",
                    key.as_str()
                )
            }
            Self::MissingEdgeTarget { source, target } => write!(
                formatter,
                "active edge from `{}` targets missing package `{}`",
                source.as_str(),
                target.as_str()
            ),
            Self::UnreachableNode(key) => write!(
                formatter,
                "active package `{}` is not reachable from the root",
                key.as_str()
            ),
            Self::VisibleIdentityMismatch {
                source,
                target,
                expected,
                found,
            } => write!(
                formatter,
                "active edge from `{}` to `{}` exposes `{found}`, expected `{expected}`",
                source.as_str(),
                target.as_str()
            ),
            Self::InvalidRootCardinality(found) => {
                write!(
                    formatter,
                    "package graph must contain exactly one root, found {found}"
                )
            }
            Self::DependencyCycle => {
                write!(
                    formatter,
                    "package graph contains an active dependency cycle"
                )
            }
        }
    }
}

impl std::error::Error for PackageGraphError {}

fn validate_node_identity(
    key: &PackageKey,
    identity: &str,
    version: &Version,
    form: PackageForm,
    role: PackageGraphRole,
) -> Result<(), PackageGraphError> {
    if matches!(identity, "sai:core" | "sai:alloc") {
        return Err(PackageGraphError::ImplicitPackageIsNotGraphNode(
            identity.to_owned(),
        ));
    }
    if PackageIdentity::parse(identity).is_err() {
        return Err(PackageGraphError::InvalidPackageIdentity(
            identity.to_owned(),
        ));
    }
    if matches!(
        role,
        PackageGraphRole::Root | PackageGraphRole::WorkspaceMember
    ) && form != PackageForm::Source
    {
        return Err(PackageGraphError::SourceRoleMustUseSource(role));
    }

    let expected = format!("{identity}#{}", version.compatibility_line());
    if key.as_str() != expected {
        return Err(PackageGraphError::PackageKeyMismatch {
            key: key.clone(),
            expected,
        });
    }
    Ok(())
}

fn is_package_identity(identity: &str) -> bool {
    PackageIdentity::parse(identity).is_ok()
}

fn validate_acyclic(nodes: &[PackageGraphNode]) -> Result<(), PackageGraphError> {
    let mut outgoing = vec![Vec::new(); nodes.len()];
    let mut indegree = vec![0usize; nodes.len()];

    for (source_index, node) in nodes.iter().enumerate() {
        for edge in &node.active_edges {
            let target_index = nodes
                .binary_search_by(|candidate| candidate.key.cmp(&edge.target))
                .expect("edge targets were validated before cycle checking");
            outgoing[source_index].push(target_index);
            indegree[target_index] += 1;
        }
    }

    let mut ready = BTreeSet::new();
    for (index, incoming) in indegree.iter().enumerate() {
        if *incoming == 0 {
            ready.insert(index);
        }
    }

    let mut visited = 0usize;
    while let Some(index) = ready.pop_first() {
        visited += 1;
        for target_index in &outgoing[index] {
            indegree[*target_index] -= 1;
            if indegree[*target_index] == 0 {
                ready.insert(*target_index);
            }
        }
    }

    if visited == nodes.len() {
        Ok(())
    } else {
        Err(PackageGraphError::DependencyCycle)
    }
}

fn validate_reachable(nodes: &[PackageGraphNode]) -> Result<(), PackageGraphError> {
    let mut reachable = vec![false; nodes.len()];
    let root_index = nodes
        .iter()
        .position(|node| node.role == PackageGraphRole::Root)
        .expect("root cardinality was validated before reachability checking");
    let mut pending = vec![root_index];

    while let Some(index) = pending.pop() {
        if reachable[index] {
            continue;
        }
        reachable[index] = true;
        for edge in &nodes[index].active_edges {
            let target_index = nodes
                .binary_search_by(|candidate| candidate.key.cmp(&edge.target))
                .expect("edge targets were validated before reachability checking");
            pending.push(target_index);
        }
    }

    if let Some((_, node)) = nodes
        .iter()
        .enumerate()
        .find(|(index, _)| !reachable[*index])
    {
        return Err(PackageGraphError::UnreachableNode(node.key.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        LockActiveGraphAdapterError, LockActivePackageMetadata, PackageForm, PackageGraph,
        PackageGraphEdge, PackageGraphError, PackageGraphNode, PackageGraphRole,
    };
    use sai_package::lock_active_graph::{LockActiveEdge, LockActiveGraph, LockActiveNode};
    use sai_package::mvs::MvsPackageVersion;
    use sai_package::version::Version;
    use sai_package::version_req::VersionReq;
    use sai_stable_hash::fingerprint;
    use sai_vfs::PackageKey;
    use std::collections::BTreeMap;

    const TOOLCHAIN_VERSION: &str = "1.8.2";

    fn version(source: &str) -> Version {
        Version::parse(source).expect("test version is valid")
    }

    fn key(source: &str) -> PackageKey {
        PackageKey::new(source).expect("test package key is valid")
    }

    fn node(
        package_key: &str,
        identity: &str,
        version_text: &str,
        role: PackageGraphRole,
        edges: impl IntoIterator<Item = PackageGraphEdge>,
    ) -> PackageGraphNode {
        PackageGraphNode::new(
            key(package_key),
            identity,
            version(version_text),
            PackageForm::Source,
            role,
            edges,
        )
        .expect("test package node is valid")
    }

    fn edge(target_key: &str, identity: &str) -> PackageGraphEdge {
        PackageGraphEdge::new(key(target_key), identity).expect("test edge is valid")
    }

    fn resolved_graph(
        nodes: impl IntoIterator<Item = PackageGraphNode>,
    ) -> Result<PackageGraph, PackageGraphError> {
        PackageGraph::new(version(TOOLCHAIN_VERSION), nodes)
    }

    fn mvs_package_version(package: &str, version_text: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, version(version_text))
            .expect("test package version is valid")
    }

    fn lock_node(package: &str, version_text: &str, root: bool) -> LockActiveNode {
        LockActiveNode {
            package: package.into(),
            version: version(version_text),
            root,
        }
    }

    fn lock_edge(from: LockActiveNode, to: LockActiveNode, requirement: &str) -> LockActiveEdge {
        LockActiveEdge {
            from,
            to,
            requirement: VersionReq::parse(requirement).expect("test requirement is valid"),
            test: false,
            only: None,
            span: 0..1,
        }
    }

    fn lock_active_graph() -> LockActiveGraph {
        let root = lock_node("acme:app", "1.0.0", true);
        let dependency = lock_node("acme:dep", "1.4.2", false);
        let leaf = lock_node("acme:leaf", "0.4.5", false);
        LockActiveGraph {
            root: root.clone(),
            // Deliberately noncanonical input order: the adapter must not
            // depend on the order in which lock packages or edges arrive.
            packages: vec![leaf.clone(), dependency.clone()],
            edges: vec![
                lock_edge(dependency.clone(), leaf, "^0.4"),
                lock_edge(root, dependency, "^1"),
            ],
        }
    }

    fn lock_active_metadata() -> BTreeMap<MvsPackageVersion, LockActivePackageMetadata> {
        BTreeMap::from([
            (
                mvs_package_version("acme:dep", "1.4.2"),
                LockActivePackageMetadata {
                    form: PackageForm::Artifact,
                    role: PackageGraphRole::Dependency,
                },
            ),
            (
                mvs_package_version("acme:leaf", "0.4.5"),
                LockActivePackageMetadata {
                    form: PackageForm::Source,
                    role: PackageGraphRole::Dependency,
                },
            ),
        ])
    }

    #[test]
    fn adapts_lock_active_graph_with_required_package_metadata() {
        let graph = PackageGraph::from_lock_active_graph(
            &lock_active_graph(),
            &lock_active_metadata(),
            version(TOOLCHAIN_VERSION),
        )
        .expect("complete lock graph metadata adapts");

        assert_eq!(graph.root().key().as_str(), "acme:app#1");
        assert_eq!(graph.root().form(), PackageForm::Source);
        assert_eq!(graph.root().role(), PackageGraphRole::Root);
        assert_eq!(graph.root().active_edges().len(), 1);

        let dependency = graph
            .node(&key("acme:dep#1"))
            .expect("dependency is present");
        assert_eq!(dependency.version().to_string(), "1.4.2");
        assert_eq!(dependency.form(), PackageForm::Artifact);
        assert_eq!(dependency.role(), PackageGraphRole::Dependency);
        assert_eq!(dependency.active_edges().len(), 1);
        assert_eq!(dependency.active_edges()[0].visible_identity(), "acme:leaf");

        let leaf = graph.node(&key("acme:leaf#0.4")).expect("leaf is present");
        assert_eq!(leaf.version().to_string(), "0.4.5");
        assert_eq!(leaf.form(), PackageForm::Source);
        assert_eq!(graph.toolchain_version().to_string(), TOOLCHAIN_VERSION);
    }

    #[test]
    fn lock_adapter_collapses_repeated_edges_to_the_same_target_key() {
        let mut lock_graph = lock_active_graph();
        let repeated_edge = lock_graph
            .edges
            .iter()
            .find(|edge| edge.from.root)
            .expect("root has an active edge")
            .clone();
        // The validated lock builder rejects repeated requirement identities;
        // this exercises the adapter's defensive topology projection.
        lock_graph.edges.push(repeated_edge);

        let graph = PackageGraph::from_lock_active_graph(
            &lock_graph,
            &lock_active_metadata(),
            version(TOOLCHAIN_VERSION),
        )
        .expect("multiple active requirements for one identity share one graph edge");

        assert_eq!(graph.root().active_edges().len(), 1);
        assert_eq!(
            graph.root().active_edges()[0].visible_identity(),
            "acme:dep"
        );
    }

    #[test]
    fn lock_adapter_output_is_independent_of_package_and_edge_order() {
        let first_input = lock_active_graph();
        let mut second_input = first_input.clone();
        second_input.packages.reverse();
        second_input.edges.reverse();

        let first = PackageGraph::from_lock_active_graph(
            &first_input,
            &lock_active_metadata(),
            version(TOOLCHAIN_VERSION),
        )
        .expect("first input adapts");
        let second = PackageGraph::from_lock_active_graph(
            &second_input,
            &lock_active_metadata(),
            version(TOOLCHAIN_VERSION),
        )
        .expect("reordered input adapts");

        assert_eq!(first, second);
        assert_eq!(fingerprint(&first), fingerprint(&second));
    }

    #[test]
    fn lock_adapter_rejects_missing_and_extra_dependency_metadata() {
        let active = lock_active_graph();
        let complete = lock_active_metadata();
        let missing = BTreeMap::from([(
            mvs_package_version("acme:dep", "1.4.2"),
            complete[&mvs_package_version("acme:dep", "1.4.2")],
        )]);
        assert_eq!(
            PackageGraph::from_lock_active_graph(&active, &missing, version(TOOLCHAIN_VERSION))
                .unwrap_err(),
            LockActiveGraphAdapterError::MissingDependencyMetadata(mvs_package_version(
                "acme:leaf",
                "0.4.5"
            ))
        );

        let mut extra = complete;
        extra.insert(
            mvs_package_version("acme:unused", "2.0.0"),
            LockActivePackageMetadata {
                form: PackageForm::Source,
                role: PackageGraphRole::Dependency,
            },
        );
        assert_eq!(
            PackageGraph::from_lock_active_graph(&active, &extra, version(TOOLCHAIN_VERSION))
                .unwrap_err(),
            LockActiveGraphAdapterError::UnexpectedDependencyMetadata(mvs_package_version(
                "acme:unused",
                "2.0.0"
            ))
        );
    }

    #[test]
    fn lock_adapter_rejects_root_flag_and_metadata_role_mismatches() {
        let mut root_mismatch = lock_active_graph();
        root_mismatch.root.root = false;
        assert!(matches!(
            PackageGraph::from_lock_active_graph(
                &root_mismatch,
                &lock_active_metadata(),
                version(TOOLCHAIN_VERSION)
            ),
            Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                role: PackageGraphRole::Root,
                ..
            })
        ));

        let mut dependency_mismatch = lock_active_graph();
        dependency_mismatch.packages[1].root = true;
        assert!(matches!(
            PackageGraph::from_lock_active_graph(
                &dependency_mismatch,
                &lock_active_metadata(),
                version(TOOLCHAIN_VERSION)
            ),
            Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                role: PackageGraphRole::Dependency,
                ..
            })
        ));

        let mut metadata = lock_active_metadata();
        metadata.insert(
            mvs_package_version("acme:dep", "1.4.2"),
            LockActivePackageMetadata {
                form: PackageForm::Source,
                role: PackageGraphRole::Root,
            },
        );
        assert!(matches!(
            PackageGraph::from_lock_active_graph(
                &lock_active_graph(),
                &metadata,
                version(TOOLCHAIN_VERSION)
            ),
            Err(LockActiveGraphAdapterError::NodeRoleMismatch {
                role: PackageGraphRole::Root,
                ..
            })
        ));
    }

    #[test]
    fn lock_adapter_rejects_edges_with_missing_or_mismatched_endpoints() {
        let mut missing_endpoint = lock_active_graph();
        missing_endpoint.edges.push(lock_edge(
            missing_endpoint.root.clone(),
            lock_node("acme:missing", "1.0.0", false),
            "^1",
        ));
        assert!(matches!(
            PackageGraph::from_lock_active_graph(
                &missing_endpoint,
                &lock_active_metadata(),
                version(TOOLCHAIN_VERSION)
            ),
            Err(LockActiveGraphAdapterError::EdgeEndpointMissing { endpoint })
                if endpoint.package.as_ref() == "acme:missing"
        ));

        let mut mismatched_root_flag = lock_active_graph();
        let edge = &mut mismatched_root_flag.edges[0];
        edge.to.root = true;
        assert!(matches!(
            PackageGraph::from_lock_active_graph(
                &mismatched_root_flag,
                &lock_active_metadata(),
                version(TOOLCHAIN_VERSION)
            ),
            Err(LockActiveGraphAdapterError::EdgeEndpointMismatch {
                endpoint,
                expected,
            }) if endpoint.root && !expected.root
        ));
    }

    #[test]
    fn validates_ordinary_key_identity_and_exact_version_line() {
        let root = node(
            "acme:app#1",
            "acme:app",
            "1.4.2",
            PackageGraphRole::Root,
            [edge("acme:json#0.4", "acme:json")],
        );
        let dependency = node(
            "acme:json#0.4",
            "acme:json",
            "0.4.7-rc.1",
            PackageGraphRole::Dependency,
            [],
        );

        let graph = resolved_graph([dependency, root]).expect("valid graph");
        assert_eq!(graph.root().identity(), "acme:app");
        assert_eq!(graph.root().version().to_string(), "1.4.2");
        assert_eq!(
            graph.node(&key("acme:json#0.4")).unwrap().identity(),
            "acme:json"
        );
    }

    #[test]
    fn keeps_implicit_toolchain_packages_out_of_resolved_nodes() {
        let root = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [],
        );

        let graph = resolved_graph([root]).expect("valid graph");
        assert_eq!(graph.nodes().len(), 1);
        assert_eq!(graph.toolchain_version().to_string(), TOOLCHAIN_VERSION);
        assert!(graph.node(&key("sai:core")).is_none());
        assert!(graph.node(&key("sai:alloc")).is_none());

        let different_toolchain = PackageGraph::new(version("1.8.3"), graph.nodes().to_vec())
            .expect("same package nodes with another toolchain");
        assert_ne!(fingerprint(&graph), fingerprint(&different_toolchain));
    }

    #[test]
    fn canonicalizes_nodes_and_edges_for_stable_hashing() {
        let root_left = node(
            "acme:app#1",
            "acme:app",
            "1.4.2",
            PackageGraphRole::Root,
            [edge("acme:z#2", "acme:z"), edge("acme:a#0.4", "acme:a")],
        );
        let root_right = node(
            "acme:app#1",
            "acme:app",
            "1.4.2",
            PackageGraphRole::Root,
            [edge("acme:a#0.4", "acme:a"), edge("acme:z#2", "acme:z")],
        );
        let a = node(
            "acme:a#0.4",
            "acme:a",
            "0.4.8",
            PackageGraphRole::Dependency,
            [],
        );
        let z = node(
            "acme:z#2",
            "acme:z",
            "2.0.0",
            PackageGraphRole::Dependency,
            [],
        );

        let left = resolved_graph([z.clone(), root_left, a.clone()]).expect("left graph");
        let right = resolved_graph([a, root_right, z]).expect("right graph");
        assert_eq!(left, right);
        assert_eq!(fingerprint(&left), fingerprint(&right));
    }

    #[test]
    fn hashes_exact_version_form_role_and_edges() {
        let original = node(
            "acme:dep#1",
            "acme:dep",
            "1.2.0",
            PackageGraphRole::Dependency,
            [],
        );
        let changed_version = node(
            "acme:dep#1",
            "acme:dep",
            "1.3.0",
            PackageGraphRole::Dependency,
            [],
        );
        assert_ne!(fingerprint(&original), fingerprint(&changed_version));
        assert_ne!(
            fingerprint(&original),
            fingerprint(
                &PackageGraphNode::new(
                    key("acme:dep#1"),
                    "acme:dep",
                    version("1.2.0"),
                    PackageForm::Artifact,
                    PackageGraphRole::Dependency,
                    [],
                )
                .unwrap()
            )
        );
        assert_ne!(
            fingerprint(&original),
            fingerprint(
                &PackageGraphNode::new(
                    key("acme:dep#1"),
                    "acme:dep",
                    version("1.2.0"),
                    PackageForm::Source,
                    PackageGraphRole::WorkspaceMember,
                    [],
                )
                .unwrap()
            )
        );
        assert_ne!(
            fingerprint(&original),
            fingerprint(&node(
                "acme:dep#1",
                "acme:dep",
                "1.2.0",
                PackageGraphRole::Dependency,
                [edge("acme:next#0.1", "acme:next")],
            ))
        );
    }

    #[test]
    fn rejects_key_identity_version_and_special_role_mismatches() {
        assert_eq!(
            PackageGraphNode::new(
                key("acme:dep#1"),
                "acme:other",
                version("1.2.0"),
                PackageForm::Source,
                PackageGraphRole::Dependency,
                [],
            )
            .unwrap_err(),
            PackageGraphError::PackageKeyMismatch {
                key: key("acme:dep#1"),
                expected: "acme:other#1".to_owned(),
            }
        );
        assert_eq!(
            PackageGraphNode::new(
                key("acme:dep#1"),
                "acme:dep",
                version("2.0.0"),
                PackageForm::Source,
                PackageGraphRole::Dependency,
                [],
            )
            .unwrap_err(),
            PackageGraphError::PackageKeyMismatch {
                key: key("acme:dep#1"),
                expected: "acme:dep#2".to_owned(),
            }
        );
        for identity in ["sai:core", "sai:alloc"] {
            assert_eq!(
                PackageGraphNode::new(
                    key(identity),
                    identity,
                    version("1.0.0"),
                    PackageForm::Source,
                    PackageGraphRole::Dependency,
                    [],
                )
                .unwrap_err(),
                PackageGraphError::ImplicitPackageIsNotGraphNode(identity.to_owned())
            );
        }
        for role in [PackageGraphRole::Root, PackageGraphRole::WorkspaceMember] {
            assert_eq!(
                PackageGraphNode::new(
                    key("acme:app#1"),
                    "acme:app",
                    version("1.0.0"),
                    PackageForm::Artifact,
                    role,
                    [],
                )
                .unwrap_err(),
                PackageGraphError::SourceRoleMustUseSource(role)
            );
        }
    }

    #[test]
    fn keeps_distinct_compatibility_lines_and_rejects_own_identity_requirements() {
        let root = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:lib#1", "acme:lib")],
        );
        let library = node(
            "acme:lib#1",
            "acme:lib",
            "1.0.0",
            PackageGraphRole::Dependency,
            [edge("acme:app#2", "acme:app")],
        );
        let other_line = node(
            "acme:app#2",
            "acme:app",
            "2.0.0",
            PackageGraphRole::Dependency,
            [],
        );
        let graph = resolved_graph([root, library, other_line])
            .expect("different packages may use different lines");
        assert_eq!(graph.nodes().len(), 3);

        assert!(matches!(
            PackageGraphNode::new(
                key("acme:app#1"),
                "acme:app",
                version("1.0.0"),
                PackageForm::Source,
                PackageGraphRole::Root,
                [edge("acme:app#2", "acme:app")],
            ),
            Err(PackageGraphError::SelfDependency { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_visible_dependency_identity() {
        assert!(matches!(
            PackageGraphNode::new(
                key("acme:app#1"),
                "acme:app",
                version("1.0.0"),
                PackageForm::Source,
                PackageGraphRole::Root,
                [
                    edge("acme:dep#1", "acme:dep"),
                    edge("acme:dep#2", "acme:dep")
                ],
            ),
            Err(PackageGraphError::DuplicateEdgeIdentity { .. })
        ));
    }

    #[test]
    fn rejects_dependency_cycles() {
        let root = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:dep#1", "acme:dep")],
        );
        let dependency = node(
            "acme:dep#1",
            "acme:dep",
            "1.2.0",
            PackageGraphRole::Dependency,
            [edge("acme:app#1", "acme:app")],
        );

        assert_eq!(
            resolved_graph([root, dependency]).unwrap_err(),
            PackageGraphError::DependencyCycle
        );
    }

    #[test]
    fn rejects_duplicate_nodes_edges_dangling_edges_and_wrong_visible_identity() {
        let root = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [],
        );
        let duplicate = node(
            "acme:app#1",
            "acme:app",
            "1.2.0",
            PackageGraphRole::Root,
            [],
        );
        assert!(matches!(
            resolved_graph([root.clone(), duplicate]),
            Err(PackageGraphError::DuplicateNode(_))
        ));

        assert!(matches!(
            PackageGraphNode::new(
                key("acme:app#1"),
                "acme:app",
                version("1.0.0"),
                PackageForm::Source,
                PackageGraphRole::Root,
                [
                    edge("acme:dep#1", "acme:dep"),
                    edge("acme:dep#1", "acme:dep")
                ],
            ),
            Err(PackageGraphError::DuplicateEdgeTarget { .. })
        ));

        let dangling = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:missing#1", "acme:missing")],
        );
        assert!(matches!(
            resolved_graph([dangling]),
            Err(PackageGraphError::MissingEdgeTarget { .. })
        ));

        let mismatched = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:dep#1", "acme:wrong")],
        );
        let dependency = node(
            "acme:dep#1",
            "acme:dep",
            "1.2.0",
            PackageGraphRole::Dependency,
            [],
        );
        assert!(matches!(
            resolved_graph([mismatched, dependency]),
            Err(PackageGraphError::VisibleIdentityMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unreachable_nodes_but_keeps_active_workspace_members() {
        let root = node(
            "acme:root#1",
            "acme:root",
            "1.0.0",
            PackageGraphRole::Root,
            [],
        );
        let orphan_dependency = node(
            "acme:orphan#1",
            "acme:orphan",
            "1.0.0",
            PackageGraphRole::Dependency,
            [],
        );
        assert_eq!(
            resolved_graph([root.clone(), orphan_dependency]).unwrap_err(),
            PackageGraphError::UnreachableNode(key("acme:orphan#1"))
        );

        let member = node(
            "acme:member#1",
            "acme:member",
            "1.0.0",
            PackageGraphRole::WorkspaceMember,
            [edge("acme:member-dep#1", "acme:member-dep")],
        );
        let root_with_member = node(
            "acme:root#1",
            "acme:root",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:member#1", "acme:member")],
        );
        let member_dependency = node(
            "acme:member-dep#1",
            "acme:member-dep",
            "1.0.0",
            PackageGraphRole::Dependency,
            [],
        );
        let graph = resolved_graph([root_with_member, member, member_dependency])
            .expect("workspace member is reachable through the active root graph");
        assert_eq!(graph.nodes().len(), 3);

        assert!(matches!(
            resolved_graph([
                root,
                node(
                    "acme:member#1",
                    "acme:member",
                    "1.0.0",
                    PackageGraphRole::WorkspaceMember,
                    [],
                ),
            ]),
            Err(PackageGraphError::UnreachableNode(_))
        ));
    }

    #[test]
    fn requires_exactly_one_root_and_resolves_edge_targets_by_key() {
        assert_eq!(
            PackageGraph::new(version(TOOLCHAIN_VERSION), []).unwrap_err(),
            PackageGraphError::InvalidRootCardinality(0)
        );

        let first = node(
            "acme:app#1",
            "acme:app",
            "1.0.0",
            PackageGraphRole::Root,
            [edge("acme:dep#0.1", "acme:dep")],
        );
        let dependency = node(
            "acme:dep#0.1",
            "acme:dep",
            "0.1.7",
            PackageGraphRole::Dependency,
            [],
        );
        let graph = resolved_graph([dependency, first]).expect("complete graph");
        let edge = &graph.root().active_edges()[0];
        assert_eq!(
            graph.node(edge.target()).unwrap().identity(),
            edge.visible_identity()
        );

        let two_roots = [
            node("acme:a#1", "acme:a", "1.0.0", PackageGraphRole::Root, []),
            node("acme:b#1", "acme:b", "1.0.0", PackageGraphRole::Root, []),
        ];
        assert_eq!(
            resolved_graph(two_roots).unwrap_err(),
            PackageGraphError::InvalidRootCardinality(2)
        );
    }
}
