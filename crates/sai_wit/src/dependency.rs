//! Active package-graph projection and direct WIT reference resolution.
//!
//! The graph is built from already-selected package versions and active direct
//! edges. It intentionally does not inspect WIT source text or package
//! manifests; callers provide the visible edges for each unit.

use crate::{WitUnit, WitUnitKind};
use sai_package::mvs::{MvsNode, MvsPackageVersion};
use sai_package::version::Version;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// One WIT unit in the selected package graph.
///
/// Package units carry their selected MVS package version. Embedded WASI units
/// carry their fixed version directly because the `wasi` namespace is reserved
/// and is not a valid MVS package identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageNode {
    unit: WitUnit,
    identity: Box<str>,
    version: WitPackageNodeVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WitPackageNodeVersion {
    Package(MvsPackageVersion),
    Wasi(Version),
}

impl WitPackageNode {
    /// Creates a package node and checks that the unit names this exact
    /// selected package identity and version.
    pub fn new(unit: WitUnit, selected: MvsPackageVersion) -> Result<Self, WitPackageGraphError> {
        if matches!(unit.kind(), WitUnitKind::Wasi { .. }) {
            return Err(WitPackageGraphError::PackageNodeHasWasiUnit { unit, selected });
        }
        let identity = selected.package();
        let version = selected.version();
        let expected_name = format!("{identity}@{version}");
        if unit.canonical_name() != expected_name {
            if unit_package_identity(&unit).as_deref() != Some(identity) {
                return Err(WitPackageGraphError::NodeIdentityMismatch { unit, selected });
            }
            return Err(WitPackageGraphError::NodeVersionMismatch { unit, selected });
        }

        let expected_stable_key = match unit.kind() {
            WitUnitKind::Dir { .. } => {
                format!("wit:dir:{identity}#{}", version.compatibility_line())
            }
            WitUnitKind::Generated { .. } => {
                format!("wit:gen:{identity}#{}", version.compatibility_line())
            }
            WitUnitKind::Wasi { .. } => unreachable!("WASI nodes were rejected above"),
        };
        if unit.stable_key() != expected_stable_key {
            return Err(WitPackageGraphError::NodeIdentityMismatch { unit, selected });
        }

        Ok(Self {
            unit,
            identity: identity.into(),
            version: WitPackageNodeVersion::Package(selected),
        })
    }

    /// Creates a node for a validated embedded WASI unit.
    ///
    /// Its identity and selected version are read from the unit's canonical
    /// name, which is fixed by [`WitUnit::wasi`].
    pub fn wasi(unit: WitUnit) -> Result<Self, WitPackageGraphError> {
        let Some((identity, version)) = wasi_unit_identity_and_version(&unit) else {
            return Err(WitPackageGraphError::InvalidWasiNode { unit });
        };
        Ok(Self {
            unit,
            identity,
            version: WitPackageNodeVersion::Wasi(version),
        })
    }

    /// Returns the WIT unit represented by this node.
    pub fn unit(&self) -> &WitUnit {
        &self.unit
    }

    /// Returns the visible package identity of this node.
    pub fn package_identity(&self) -> &str {
        &self.identity
    }

    /// Returns the exact version selected for this unit.
    pub fn selected_version(&self) -> &Version {
        match &self.version {
            WitPackageNodeVersion::Package(selected) => selected.version(),
            WitPackageNodeVersion::Wasi(version) => version,
        }
    }

    /// Returns the MVS package version for a non-WASI package node.
    pub fn mvs_package_version(&self) -> Option<&MvsPackageVersion> {
        match &self.version {
            WitPackageNodeVersion::Package(selected) => Some(selected),
            WitPackageNodeVersion::Wasi(_) => None,
        }
    }

    fn validate(&self) -> Result<(), WitPackageGraphError> {
        match &self.version {
            WitPackageNodeVersion::Package(selected) => {
                let expected_name = format!("{}@{}", selected.package(), selected.version());
                if self.identity.as_ref() != selected.package()
                    || unit_package_identity(&self.unit).as_deref() != Some(selected.package())
                {
                    return Err(WitPackageGraphError::NodeIdentityMismatch {
                        unit: self.unit.clone(),
                        selected: selected.clone(),
                    });
                }
                if self.unit.canonical_name() != expected_name {
                    return Err(WitPackageGraphError::NodeVersionMismatch {
                        unit: self.unit.clone(),
                        selected: selected.clone(),
                    });
                }
                let expected_stable_key = match self.unit.kind() {
                    WitUnitKind::Dir { .. } => format!(
                        "wit:dir:{}#{}",
                        selected.package(),
                        selected.version().compatibility_line()
                    ),
                    WitUnitKind::Generated { .. } => format!(
                        "wit:gen:{}#{}",
                        selected.package(),
                        selected.version().compatibility_line()
                    ),
                    WitUnitKind::Wasi { .. } => {
                        return Err(WitPackageGraphError::PackageNodeHasWasiUnit {
                            unit: self.unit.clone(),
                            selected: selected.clone(),
                        });
                    }
                };
                if self.unit.stable_key() != expected_stable_key {
                    return Err(WitPackageGraphError::NodeIdentityMismatch {
                        unit: self.unit.clone(),
                        selected: selected.clone(),
                    });
                }
            }
            WitPackageNodeVersion::Wasi(version) => {
                let Some((identity, unit_version)) = wasi_unit_identity_and_version(&self.unit)
                else {
                    return Err(WitPackageGraphError::InvalidWasiNode {
                        unit: self.unit.clone(),
                    });
                };
                if self.identity != identity {
                    return Err(WitPackageGraphError::InvalidWasiNode {
                        unit: self.unit.clone(),
                    });
                }
                if version != &unit_version {
                    return Err(WitPackageGraphError::WasiNodeVersionMismatch {
                        unit: self.unit.clone(),
                        selected: version.clone(),
                        unit_version,
                    });
                }
            }
        }
        Ok(())
    }
}

/// One active direct package edge visible from a WIT unit.
///
/// `target_unit: None` represents an active package that does not have a WIT
/// unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageEdge {
    source: WitUnit,
    package_identity: Box<str>,
    selected: MvsPackageVersion,
    target_unit: Option<WitUnit>,
}

impl WitPackageEdge {
    /// Creates an edge. Its identity, source, and target are validated when
    /// constructing [`WitPackageGraph`].
    pub fn new(
        source: WitUnit,
        package_identity: impl Into<Box<str>>,
        selected: MvsPackageVersion,
        target_unit: Option<WitUnit>,
    ) -> Self {
        Self {
            source,
            package_identity: package_identity.into(),
            selected,
            target_unit,
        }
    }

    /// Returns the source WIT unit.
    pub fn source(&self) -> &WitUnit {
        &self.source
    }

    /// Returns the package identity visible from the source.
    pub fn package_identity(&self) -> &str {
        &self.package_identity
    }

    /// Returns the selected active package version.
    pub fn selected_package_version(&self) -> &MvsPackageVersion {
        &self.selected
    }

    /// Returns the WIT unit for the active package, if one exists.
    pub fn target_unit(&self) -> Option<&WitUnit> {
        self.target_unit.as_ref()
    }
}

/// A validated projection of the active package graph used for WIT resolution.
#[derive(Clone, Debug)]
pub struct WitPackageGraph {
    root: WitUnit,
    nodes: Vec<WitPackageNode>,
    node_indices: HashMap<WitUnit, usize>,
    edges: HashMap<WitUnit, HashMap<Box<str>, WitPackageEdge>>,
}

impl WitPackageGraph {
    /// Builds a graph from explicit package-unit nodes and active direct edges.
    ///
    /// Edge identity, not input order, determines lookup. A package identity
    /// can occur at most once per source unit.
    pub fn new(
        root: WitUnit,
        node_values: impl IntoIterator<Item = WitPackageNode>,
        edges: impl IntoIterator<Item = WitPackageEdge>,
    ) -> Result<Self, WitPackageGraphError> {
        let mut nodes: Vec<WitPackageNode> = Vec::new();
        let mut node_indices = HashMap::new();
        let mut package_line_versions = HashMap::<MvsNode, usize>::new();
        let mut wasi_identities = HashSet::<Box<str>>::new();

        for node in node_values {
            node.validate()?;
            match &node.version {
                WitPackageNodeVersion::Package(selected) => {
                    let key = selected.node();
                    if let Some(&existing_index) = package_line_versions.get(&key) {
                        let existing = &nodes[existing_index];
                        let existing_selected = existing
                            .mvs_package_version()
                            .expect("package-line index only contains package nodes");
                        if existing_selected.version() != selected.version() {
                            return Err(WitPackageGraphError::PackageLineVersionMismatch {
                                existing: existing.clone(),
                                duplicate: node,
                            });
                        }
                    } else {
                        package_line_versions.insert(key, nodes.len());
                    }
                }
                WitPackageNodeVersion::Wasi(_) => {
                    if !wasi_identities.insert(node.identity.clone()) {
                        return Err(WitPackageGraphError::DuplicateWasiUnit {
                            package_identity: node.identity.clone(),
                        });
                    }
                }
            }
            if node_indices.contains_key(&node.unit) {
                return Err(WitPackageGraphError::DuplicateUnit {
                    unit: node.unit.clone(),
                });
            }
            let index = nodes.len();
            node_indices.insert(node.unit.clone(), index);
            nodes.push(node);
        }

        let Some(&root_index) = node_indices.get(&root) else {
            return Err(WitPackageGraphError::MissingRootUnit { root });
        };
        if !same_exact_unit(&root, &nodes[root_index].unit) {
            return Err(WitPackageGraphError::RootUnitVersionMismatch {
                root,
                selected: nodes[root_index].clone(),
            });
        }

        let mut edges_by_source = HashMap::<WitUnit, HashMap<Box<str>, WitPackageEdge>>::new();
        for edge in edges {
            let Some(&source_index) = node_indices.get(&edge.source) else {
                return Err(WitPackageGraphError::MissingEdgeSource {
                    source: edge.source,
                    package_identity: edge.package_identity,
                });
            };
            if !same_exact_unit(&edge.source, &nodes[source_index].unit) {
                return Err(WitPackageGraphError::EdgeSourceUnitMismatch {
                    source: edge.source,
                    selected: nodes[source_index].clone(),
                });
            }
            if edge.package_identity.as_ref() != edge.selected.package() {
                return Err(WitPackageGraphError::EdgeIdentityMismatch {
                    package_identity: edge.package_identity,
                    selected: edge.selected,
                });
            }
            if matches!(nodes[source_index].unit.kind(), WitUnitKind::Wasi { .. }) {
                return Err(WitPackageGraphError::WasiUnitHasPackageEdge {
                    source: edge.source,
                    package_identity: edge.package_identity,
                });
            }
            let source_edges = edges_by_source.entry(edge.source.clone()).or_default();
            if source_edges.contains_key(&edge.package_identity) {
                return Err(WitPackageGraphError::DuplicateEdge {
                    source: edge.source,
                    package_identity: edge.package_identity,
                });
            }

            if let Some(target_unit) = &edge.target_unit {
                let Some(&target_index) = node_indices.get(target_unit) else {
                    return Err(WitPackageGraphError::MissingTargetUnit {
                        package_identity: edge.package_identity,
                        selected: edge.selected,
                        target_unit: target_unit.clone(),
                    });
                };
                let target = &nodes[target_index];
                if !same_exact_unit(target_unit, &target.unit) {
                    return Err(WitPackageGraphError::EdgeTargetUnitMismatch {
                        selected: edge.selected,
                        target_unit: target_unit.clone(),
                        graph_target: target.clone(),
                    });
                }
                let Some(target_version) = target.mvs_package_version() else {
                    return Err(WitPackageGraphError::TargetUnitIsNotMvsPackage {
                        package_identity: edge.package_identity,
                        selected: edge.selected,
                        target_unit: target_unit.clone(),
                    });
                };
                if target_version.package() != edge.selected.package() {
                    return Err(WitPackageGraphError::EdgeTargetIdentityMismatch {
                        selected: edge.selected,
                        target: target_version.clone(),
                    });
                }
                if target_version != &edge.selected {
                    return Err(WitPackageGraphError::EdgeTargetVersionMismatch {
                        selected: edge.selected,
                        target: target_version.clone(),
                    });
                }
            }

            source_edges.insert(edge.package_identity.clone(), edge);
        }

        Ok(Self {
            root,
            nodes,
            node_indices,
            edges: edges_by_source,
        })
    }

    /// Returns the root WIT unit.
    pub fn root(&self) -> &WitUnit {
        &self.root
    }

    /// Returns the graph's package-unit nodes in construction order.
    pub fn nodes(&self) -> &[WitPackageNode] {
        &self.nodes
    }

    /// Resolves one direct WIT package reference from `source`.
    ///
    /// A self-reference resolves to `source`, an active edge resolves to its
    /// target unit, and a `wasi:*` reference resolves by searching the graph's
    /// WASI nodes. An active package without a WIT unit and an undeclared
    /// package are reported separately.
    pub fn resolve_direct_reference(
        &self,
        source: &WitUnit,
        package_identity: &str,
        requested_version: Option<&Version>,
    ) -> Result<&WitPackageNode, WitPackageReferenceError> {
        let Some(&source_index) = self.node_indices.get(source) else {
            return Err(WitPackageReferenceError::UnknownSourceUnit {
                source: source.clone(),
            });
        };
        let source_node = &self.nodes[source_index];
        if !same_exact_unit(source, &source_node.unit) {
            return Err(WitPackageReferenceError::SourceUnitVersionMismatch {
                source: source.clone(),
                selected: source_node.clone(),
            });
        }

        if matches!(package_identity, "sai:core" | "sai:alloc") {
            return Err(WitPackageReferenceError::ImplicitPackageHasNoWitUnit {
                package_identity: package_identity.into(),
            });
        }

        let resolved = if package_identity.starts_with("wasi:") {
            self.nodes
                .iter()
                .find(|node| {
                    matches!(&node.version, WitPackageNodeVersion::Wasi(_))
                        && node.package_identity() == package_identity
                })
                .ok_or_else(|| WitPackageReferenceError::UnknownWasiPackage {
                    package_identity: package_identity.into(),
                })?
        } else if matches!(source_node.unit.kind(), WitUnitKind::Dir { .. })
            && source_node.package_identity() == package_identity
        {
            source_node
        } else {
            let edge = self
                .edges
                .get(source)
                .and_then(|source_edges| source_edges.get(package_identity))
                .ok_or_else(|| WitPackageReferenceError::UndeclaredPackage {
                    source: source.clone(),
                    package_identity: package_identity.into(),
                })?;
            let Some(target_unit) = edge.target_unit() else {
                return Err(WitPackageReferenceError::ActivePackageHasNoWitUnit {
                    package_identity: edge.package_identity.clone(),
                    selected: edge.selected.clone(),
                });
            };
            let target_index = self.node_indices[target_unit];
            &self.nodes[target_index]
        };

        if let Some(requested) = requested_version {
            validate_requested_version(package_identity, resolved.selected_version(), requested)?;
        }
        Ok(resolved)
    }
}

fn same_exact_unit(left: &WitUnit, right: &WitUnit) -> bool {
    left == right && left.canonical_name() == right.canonical_name()
}

fn validate_requested_version(
    package_identity: &str,
    selected: &Version,
    requested: &Version,
) -> Result<(), WitPackageReferenceError> {
    if selected.compatibility_line() != requested.compatibility_line() {
        return Err(WitPackageReferenceError::IncompatibleRequestedVersion {
            package_identity: package_identity.into(),
            selected: selected.clone(),
            requested: requested.clone(),
        });
    }
    if requested > selected {
        return Err(WitPackageReferenceError::RequestedVersionIsNewer {
            package_identity: package_identity.into(),
            selected: selected.clone(),
            requested: requested.clone(),
        });
    }
    Ok(())
}

fn unit_package_identity(unit: &WitUnit) -> Option<Box<str>> {
    match unit.kind() {
        WitUnitKind::Wasi { .. } => {
            let canonical = unit.canonical_name().strip_prefix("wasi:")?;
            let (package, _) = canonical.rsplit_once('@')?;
            Some(format!("wasi:{package}").into())
        }
        WitUnitKind::Dir { .. } | WitUnitKind::Generated { .. } => {
            let (identity, _) = unit.canonical_name().rsplit_once('@')?;
            Some(identity.into())
        }
    }
}

fn wasi_unit_identity_and_version(unit: &WitUnit) -> Option<(Box<str>, Version)> {
    if !matches!(unit.kind(), WitUnitKind::Wasi { .. }) {
        return None;
    }
    let canonical = unit.canonical_name().strip_prefix("wasi:")?;
    let (package, version) = canonical.rsplit_once('@')?;
    if package.is_empty() {
        return None;
    }
    let version = Version::parse(version).ok()?;
    let identity: Box<str> = format!("wasi:{package}").into();
    let expected_stable_key = format!("wit:wasi:{package}@{version}");
    if unit.stable_key() != expected_stable_key {
        return None;
    }
    Some((identity, version))
}

/// An invalid or inconsistent input to [`WitPackageGraph::new`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageGraphError {
    NodeIdentityMismatch {
        unit: WitUnit,
        selected: MvsPackageVersion,
    },
    NodeVersionMismatch {
        unit: WitUnit,
        selected: MvsPackageVersion,
    },
    PackageNodeHasWasiUnit {
        unit: WitUnit,
        selected: MvsPackageVersion,
    },
    InvalidWasiNode {
        unit: WitUnit,
    },
    WasiNodeVersionMismatch {
        unit: WitUnit,
        selected: Version,
        unit_version: Version,
    },
    WasiUnitHasPackageEdge {
        source: WitUnit,
        package_identity: Box<str>,
    },
    DuplicateUnit {
        unit: WitUnit,
    },
    PackageLineVersionMismatch {
        existing: WitPackageNode,
        duplicate: WitPackageNode,
    },
    DuplicateWasiUnit {
        package_identity: Box<str>,
    },
    MissingRootUnit {
        root: WitUnit,
    },
    RootUnitVersionMismatch {
        root: WitUnit,
        selected: WitPackageNode,
    },
    MissingEdgeSource {
        source: WitUnit,
        package_identity: Box<str>,
    },
    EdgeSourceUnitMismatch {
        source: WitUnit,
        selected: WitPackageNode,
    },
    EdgeIdentityMismatch {
        package_identity: Box<str>,
        selected: MvsPackageVersion,
    },
    DuplicateEdge {
        source: WitUnit,
        package_identity: Box<str>,
    },
    MissingTargetUnit {
        package_identity: Box<str>,
        selected: MvsPackageVersion,
        target_unit: WitUnit,
    },
    EdgeTargetUnitMismatch {
        selected: MvsPackageVersion,
        target_unit: WitUnit,
        graph_target: WitPackageNode,
    },
    TargetUnitIsNotMvsPackage {
        package_identity: Box<str>,
        selected: MvsPackageVersion,
        target_unit: WitUnit,
    },
    EdgeTargetIdentityMismatch {
        selected: MvsPackageVersion,
        target: MvsPackageVersion,
    },
    EdgeTargetVersionMismatch {
        selected: MvsPackageVersion,
        target: MvsPackageVersion,
    },
}

impl fmt::Display for WitPackageGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid WIT package graph: {self:?}")
    }
}

impl std::error::Error for WitPackageGraphError {}

/// A failed direct reference lookup or requested-version check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageReferenceError {
    UnknownSourceUnit {
        source: WitUnit,
    },
    SourceUnitVersionMismatch {
        source: WitUnit,
        selected: WitPackageNode,
    },
    UnknownWasiPackage {
        package_identity: Box<str>,
    },
    UndeclaredPackage {
        source: WitUnit,
        package_identity: Box<str>,
    },
    ImplicitPackageHasNoWitUnit {
        package_identity: Box<str>,
    },
    ActivePackageHasNoWitUnit {
        package_identity: Box<str>,
        selected: MvsPackageVersion,
    },
    IncompatibleRequestedVersion {
        package_identity: Box<str>,
        selected: Version,
        requested: Version,
    },
    RequestedVersionIsNewer {
        package_identity: Box<str>,
        selected: Version,
        requested: Version,
    },
}

impl fmt::Display for WitPackageReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSourceUnit { source } => {
                write!(
                    formatter,
                    "WIT source unit is not in the package graph: {source:?}"
                )
            }
            Self::SourceUnitVersionMismatch { source, selected } => write!(
                formatter,
                "WIT source unit `{}` does not match the selected `{}`",
                source.canonical_name(),
                selected.unit().canonical_name()
            ),
            Self::UnknownWasiPackage { package_identity } => {
                write!(formatter, "unknown WIT package `{package_identity}`")
            }
            Self::UndeclaredPackage {
                package_identity, ..
            } => write!(
                formatter,
                "WIT package `{package_identity}` is not an active direct dependency; add a dependency on `{package_identity}` to `sai.package`"
            ),
            Self::ImplicitPackageHasNoWitUnit { package_identity } => write!(
                formatter,
                "implicit package `{package_identity}` has no WIT unit"
            ),
            Self::ActivePackageHasNoWitUnit {
                package_identity,
                selected,
            } => write!(
                formatter,
                "active package `{package_identity}@{}` has no WIT unit",
                selected.version()
            ),
            Self::IncompatibleRequestedVersion {
                package_identity,
                selected,
                requested,
            } => write!(
                formatter,
                "WIT package `{package_identity}` requests `{requested}` but the build provides `{selected}` from another compatibility line"
            ),
            Self::RequestedVersionIsNewer {
                package_identity,
                selected,
                requested,
            } => write!(
                formatter,
                "WIT package `{package_identity}` requests `{requested}`, newer than the build-provided `{selected}`"
            ),
        }
    }
}

impl std::error::Error for WitPackageReferenceError {}

#[cfg(test)]
mod tests {
    use super::{
        WitPackageEdge, WitPackageGraph, WitPackageGraphError, WitPackageNode,
        WitPackageReferenceError,
    };
    use crate::WitUnit;
    use sai_intern::{Interner, PackageInterner};
    use sai_package::mvs::MvsPackageVersion;
    use sai_package::version::Version;

    fn version(source: &str) -> Version {
        Version::parse(source).expect("valid test version")
    }

    fn package_version(identity: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(identity, self::version(version)).expect("valid package version")
    }

    fn package_unit(identity: &str, version: &str) -> WitUnit {
        let selected = package_version(identity, version);
        let packages = PackageInterner::new();
        let package_key = format!(
            "{}#{}",
            selected.package(),
            selected.version().compatibility_line()
        );
        let package_id = packages
            .intern(&package_key)
            .expect("valid test package key");
        WitUnit::dir(package_id, &packages, &selected).expect("unit matches package version")
    }

    fn generated_unit(identity: &str, version: &str) -> WitUnit {
        let selected = package_version(identity, version);
        let packages = PackageInterner::new();
        let package_key = format!(
            "{}#{}",
            selected.package(),
            selected.version().compatibility_line()
        );
        let package_id = packages
            .intern(&package_key)
            .expect("valid test package key");
        WitUnit::generated(package_id, &packages, &selected).expect("unit matches package version")
    }

    fn package_node(identity: &str, version: &str) -> WitPackageNode {
        WitPackageNode::new(
            package_unit(identity, version),
            package_version(identity, version),
        )
        .expect("node matches package version")
    }

    fn wasi_unit(package: &str) -> WitUnit {
        let symbols = Interner::new();
        let package = symbols.intern(package).expect("valid test WASI symbol");
        WitUnit::wasi(package, &symbols, &version("0.2.3")).expect("embedded WASI unit")
    }

    fn graph_with_active_dependency() -> (WitUnit, WitUnit, WitPackageGraph) {
        let root = package_unit("acme:root", "1.0.0");
        let dep = package_unit("acme:dep", "1.4.0");
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                package_node("acme:root", "1.0.0"),
                package_node("acme:dep", "1.4.0"),
            ],
            [WitPackageEdge::new(
                root.clone(),
                "acme:dep",
                package_version("acme:dep", "1.4.0"),
                Some(dep.clone()),
            )],
        )
        .expect("valid package graph");
        (root, dep, graph)
    }

    #[test]
    fn self_reference_resolves_to_the_source_node() {
        let (root, _, graph) = graph_with_active_dependency();

        let resolved = graph
            .resolve_direct_reference(&root, "acme:root", None)
            .expect("self-reference resolves");

        assert_eq!(resolved.unit(), &root);
        assert_eq!(resolved.selected_version(), &version("1.0.0"));
    }

    #[test]
    fn generated_self_reference_is_undeclared() {
        let generated = generated_unit("acme:generated", "1.0.0");
        let graph = WitPackageGraph::new(
            generated.clone(),
            [WitPackageNode::new(
                generated.clone(),
                package_version("acme:generated", "1.0.0"),
            )
            .expect("valid generated package node")],
            [],
        )
        .expect("valid package graph");

        assert!(matches!(
            graph.resolve_direct_reference(&generated, "acme:generated", None),
            Err(WitPackageReferenceError::UndeclaredPackage { .. })
        ));
    }

    #[test]
    fn active_undeclared_and_active_without_wit_unit_are_distinct() {
        let root = package_unit("acme:root", "1.0.0");
        let dep = package_unit("acme:dep", "1.2.0");
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                package_node("acme:root", "1.0.0"),
                package_node("acme:dep", "1.2.0"),
            ],
            [
                WitPackageEdge::new(
                    root.clone(),
                    "acme:dep",
                    package_version("acme:dep", "1.2.0"),
                    Some(dep.clone()),
                ),
                WitPackageEdge::new(
                    root.clone(),
                    "acme:without-wit",
                    package_version("acme:without-wit", "1.0.0"),
                    None,
                ),
            ],
        )
        .expect("valid package graph");

        assert_eq!(
            graph
                .resolve_direct_reference(&root, "acme:dep", None)
                .expect("active WIT dependency resolves")
                .unit(),
            &dep
        );
        assert!(matches!(
            graph.resolve_direct_reference(&root, "acme:missing", None),
            Err(WitPackageReferenceError::UndeclaredPackage { .. })
        ));
        assert!(matches!(
            graph.resolve_direct_reference(&root, "acme:without-wit", None),
            Err(WitPackageReferenceError::ActivePackageHasNoWitUnit { .. })
        ));
        for identity in ["sai:core", "sai:alloc"] {
            let error = graph
                .resolve_direct_reference(&root, identity, None)
                .expect_err("implicit packages have no WIT unit");
            assert!(matches!(
                error,
                WitPackageReferenceError::ImplicitPackageHasNoWitUnit { .. }
            ));
            assert!(
                !error.to_string().contains("add a dependency"),
                "implicit packages cannot be added as explicit dependencies"
            );
        }
    }

    #[test]
    fn wasi_reference_uses_a_graph_node_and_unknown_wasi_is_rejected() {
        let root = package_unit("acme:root", "1.0.0");
        let wasi = wasi_unit("io");
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                package_node("acme:root", "1.0.0"),
                WitPackageNode::wasi(wasi.clone()).expect("valid WASI node"),
            ],
            [],
        )
        .expect("valid package graph");

        assert_eq!(
            graph
                .resolve_direct_reference(&root, "wasi:io", None)
                .expect("known WASI package resolves")
                .unit(),
            &wasi
        );
        assert_eq!(
            graph
                .resolve_direct_reference(&wasi, "wasi:io", None)
                .expect("WASI unit resolves its own identity through the WASI lookup")
                .unit(),
            &wasi
        );
        assert!(matches!(
            graph.resolve_direct_reference(&root, "wasi:unknown", None),
            Err(WitPackageReferenceError::UnknownWasiPackage { .. })
        ));
    }

    #[test]
    fn version_requests_accept_unversioned_and_older_same_line_references() {
        let (root, _, graph) = graph_with_active_dependency();

        let unversioned = graph
            .resolve_direct_reference(&root, "acme:dep", None)
            .expect("unversioned references are accepted");
        assert_eq!(unversioned.selected_version(), &version("1.4.0"));

        let older = graph
            .resolve_direct_reference(&root, "acme:dep", Some(&version("1.0.0")))
            .expect("older same-line reference is accepted");
        assert_eq!(older.selected_version(), &version("1.4.0"));
    }

    #[test]
    fn version_requests_reject_newer_same_line_and_incompatible_versions() {
        let (root, _, graph) = graph_with_active_dependency();

        assert_eq!(
            graph
                .resolve_direct_reference(&root, "acme:dep", Some(&version("1.5.0")))
                .unwrap_err(),
            WitPackageReferenceError::RequestedVersionIsNewer {
                package_identity: "acme:dep".into(),
                selected: version("1.4.0"),
                requested: version("1.5.0"),
            }
        );
        assert_eq!(
            graph
                .resolve_direct_reference(&root, "acme:dep", Some(&version("2.0.0")))
                .unwrap_err(),
            WitPackageReferenceError::IncompatibleRequestedVersion {
                package_identity: "acme:dep".into(),
                selected: version("1.4.0"),
                requested: version("2.0.0"),
            }
        );
    }

    #[test]
    fn rejects_malformed_nodes_roots_and_edges() {
        let root = package_unit("acme:root", "1.0.0");
        let wrong_identity = WitPackageNode::new(
            package_unit("acme:other", "1.0.0"),
            package_version("acme:dep", "1.0.0"),
        )
        .expect_err("node identity must match its selected package");
        assert!(matches!(
            wrong_identity,
            WitPackageGraphError::NodeIdentityMismatch { .. }
        ));

        let wrong_version = WitPackageNode::new(
            package_unit("acme:dep", "1.2.0"),
            package_version("acme:dep", "1.3.0"),
        )
        .expect_err("node version must match exactly");
        assert!(matches!(
            wrong_version,
            WitPackageGraphError::NodeVersionMismatch { .. }
        ));

        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                [package_node("acme:root", "1.0.0")],
                [WitPackageEdge::new(
                    root.clone(),
                    "acme:other",
                    package_version("acme:dep", "1.0.0"),
                    None,
                )],
            ),
            Err(WitPackageGraphError::EdgeIdentityMismatch { .. })
        ));
        assert!(matches!(
            WitPackageGraph::new(root.clone(), [package_node("acme:root", "1.0.0")], [],),
            Ok(_)
        ));
        let absent = package_unit("acme:absent", "1.0.0");
        assert!(matches!(
            WitPackageGraph::new(absent.clone(), [package_node("acme:root", "1.0.0")], [],),
            Err(WitPackageGraphError::MissingRootUnit { .. })
        ));
        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                [package_node("acme:root", "1.0.0")],
                [WitPackageEdge::new(
                    root,
                    "acme:dep",
                    package_version("acme:dep", "1.0.0"),
                    Some(package_unit("acme:dep", "1.0.0")),
                )],
            ),
            Err(WitPackageGraphError::MissingTargetUnit { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_edges_and_target_version_mismatches() {
        let root = package_unit("acme:root", "1.0.0");
        let dep = package_unit("acme:dep", "1.4.0");
        let edge = || {
            WitPackageEdge::new(
                root.clone(),
                "acme:dep",
                package_version("acme:dep", "1.4.0"),
                Some(dep.clone()),
            )
        };
        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                [
                    package_node("acme:root", "1.0.0"),
                    package_node("acme:dep", "1.4.0"),
                ],
                [edge(), edge()],
            ),
            Err(WitPackageGraphError::DuplicateEdge { .. })
        ));

        let changed_edge = WitPackageEdge::new(
            root.clone(),
            "acme:dep",
            package_version("acme:dep", "1.3.0"),
            Some(dep),
        );
        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                [
                    package_node("acme:root", "1.0.0"),
                    package_node("acme:dep", "1.4.0"),
                ],
                [changed_edge],
            ),
            Err(WitPackageGraphError::EdgeTargetVersionMismatch { .. })
        ));
    }

    #[test]
    fn unit_lookups_require_the_exact_selected_version() {
        let root = package_unit("acme:root", "1.0.0");
        let same_line_root = package_unit("acme:root", "1.1.0");
        let dep = package_unit("acme:dep", "1.4.0");
        let same_line_dep = package_unit("acme:dep", "1.2.0");
        let nodes = [
            package_node("acme:root", "1.0.0"),
            package_node("acme:dep", "1.4.0"),
        ];

        assert!(matches!(
            WitPackageGraph::new(same_line_root.clone(), nodes.clone(), []),
            Err(WitPackageGraphError::RootUnitVersionMismatch { .. })
        ));

        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                nodes.clone(),
                [WitPackageEdge::new(
                    same_line_root.clone(),
                    "acme:without-wit",
                    package_version("acme:without-wit", "1.0.0"),
                    None,
                )],
            ),
            Err(WitPackageGraphError::EdgeSourceUnitMismatch { .. })
        ));

        assert!(matches!(
            WitPackageGraph::new(
                root.clone(),
                nodes,
                [WitPackageEdge::new(
                    root.clone(),
                    "acme:dep",
                    package_version("acme:dep", "1.4.0"),
                    Some(same_line_dep),
                )],
            ),
            Err(WitPackageGraphError::EdgeTargetUnitMismatch { .. })
        ));

        let graph = WitPackageGraph::new(
            root,
            [
                package_node("acme:root", "1.0.0"),
                package_node("acme:dep", "1.4.0"),
            ],
            [WitPackageEdge::new(
                package_unit("acme:root", "1.0.0"),
                "acme:dep",
                package_version("acme:dep", "1.4.0"),
                Some(dep),
            )],
        )
        .expect("valid package graph");
        assert!(matches!(
            graph.resolve_direct_reference(&same_line_root, "acme:dep", None,),
            Err(WitPackageReferenceError::SourceUnitVersionMismatch { .. })
        ));
    }

    #[test]
    fn graph_nodes_reject_wasi_as_an_mvs_package_node() {
        let unit = wasi_unit("io");
        assert!(matches!(
            WitPackageNode::new(unit, package_version("acme:io", "1.0.0")),
            Err(WitPackageGraphError::PackageNodeHasWasiUnit { .. })
        ));
    }

    #[test]
    fn dir_and_generated_views_can_share_a_selected_package_version() {
        let selected = package_version("acme:shared", "1.4.0");
        let packages = PackageInterner::new();
        let package_key = format!(
            "{}#{}",
            selected.package(),
            selected.version().compatibility_line()
        );
        let package_id = packages
            .intern(&package_key)
            .expect("valid test package key");
        let dir = WitUnit::dir(package_id, &packages, &selected).expect("matching directory unit");
        let generated =
            WitUnit::generated(package_id, &packages, &selected).expect("matching generated unit");
        assert_ne!(dir, generated);

        WitPackageGraph::new(
            dir.clone(),
            [
                WitPackageNode::new(dir.clone(), selected.clone()).expect("directory node"),
                WitPackageNode::new(generated, selected.clone()).expect("generated node"),
            ],
            [],
        )
        .expect("same selected package version can have both WIT views");

        assert!(matches!(
            WitPackageGraph::new(
                dir.clone(),
                [
                    WitPackageNode::new(dir.clone(), selected.clone()).expect("directory node"),
                    WitPackageNode::new(dir.clone(), selected.clone()).expect("duplicate node"),
                ],
                [],
            ),
            Err(WitPackageGraphError::DuplicateUnit { .. })
        ));

        let other_selected = package_version("acme:shared", "1.5.0");
        let other_generated = WitUnit::generated(package_id, &packages, &other_selected)
            .expect("generated unit has the same compatibility-line identity");
        assert!(matches!(
            WitPackageGraph::new(
                dir.clone(),
                [
                    WitPackageNode::new(dir, selected).expect("directory node"),
                    WitPackageNode::new(other_generated, other_selected).expect("generated node"),
                ],
                [],
            ),
            Err(WitPackageGraphError::PackageLineVersionMismatch { .. })
        ));
    }

    #[test]
    fn wasi_nodes_reject_non_wasi_units() {
        assert!(matches!(
            WitPackageNode::wasi(package_unit("acme:root", "1.0.0")),
            Err(WitPackageGraphError::InvalidWasiNode { .. })
        ));
    }

    #[test]
    fn wasi_units_cannot_have_edges_to_non_wasi_packages() {
        let root = package_unit("acme:root", "1.0.0");
        let wasi = wasi_unit("io");
        let dependency = package_unit("acme:dep", "1.0.0");
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                package_node("acme:root", "1.0.0"),
                WitPackageNode::wasi(wasi.clone()).expect("valid WASI node"),
                package_node("acme:dep", "1.0.0"),
            ],
            [WitPackageEdge::new(
                wasi.clone(),
                "acme:dep",
                package_version("acme:dep", "1.0.0"),
                Some(dependency),
            )],
        );

        assert!(matches!(
            graph,
            Err(WitPackageGraphError::WasiUnitHasPackageEdge {
                source,
                package_identity,
            }) if source == wasi && package_identity.as_ref() == "acme:dep"
        ));
    }
}
