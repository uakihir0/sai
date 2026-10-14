//! Stable SAI package graph values prepared by the driver after resolution.
//!
//! This model deliberately contains stable package keys and strings rather
//! than session-local `PackageId`s. The driver can attach interner IDs later,
//! at the boundary where query-facing package identities are needed.
//! Implicit `sai:core` and `sai:alloc` packages are toolchain inputs, not graph
//! nodes; WIT view packages are modeled by the later WIT package-scope layer.

use sai_package::identity::PackageIdentity;
use sai_package::version::Version;
use sai_stable_hash::{StableHash, StableHasher};
use sai_vfs::PackageKey;
use std::{collections::BTreeSet, fmt};

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
        PackageForm, PackageGraph, PackageGraphEdge, PackageGraphError, PackageGraphNode,
        PackageGraphRole,
    };
    use sai_package::version::Version;
    use sai_stable_hash::fingerprint;
    use sai_vfs::PackageKey;

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
