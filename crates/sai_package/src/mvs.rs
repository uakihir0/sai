//! Minimal Version Selection over preloaded package contracts.
//!
//! Contract fetching, path-source inspection, named-version calculation, and
//! source choice happen before/around this pure selection core. This module
//! takes their validated results and performs the deterministic MVS walk.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use crate::identity::PackageIdentity;
use crate::version::{CompatibilityLine, Version};
use crate::version_req::VersionReq;

/// A package identity and one concrete version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MvsPackageVersion {
    package: Box<str>,
    version: Version,
}

impl MvsPackageVersion {
    pub fn new(package: impl Into<Box<str>>, version: Version) -> Result<Self, MvsInputError> {
        let package = package.into();
        if PackageIdentity::parse(&package).is_err() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::InvalidPackageIdentity(package),
                None,
            ));
        }
        Ok(Self { package, version })
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub const fn version(&self) -> &Version {
        &self.version
    }

    pub fn node(&self) -> MvsNode {
        MvsNode {
            package: self.package.clone(),
            line: self.version.compatibility_line(),
        }
    }
}

/// A package node, identified by identity and compatibility line.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MvsNode {
    package: Box<str>,
    line: CompatibilityLine,
}

impl MvsNode {
    pub fn new(
        package: impl Into<Box<str>>,
        line: CompatibilityLine,
    ) -> Result<Self, MvsInputError> {
        let package = package.into();
        if PackageIdentity::parse(&package).is_err() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::InvalidPackageIdentity(package),
                None,
            ));
        }
        Ok(Self { package, line })
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub const fn line(&self) -> CompatibilityLine {
        self.line
    }
}

/// One validated requirement edge with the version named after source and pin
/// resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsEdge {
    package: Box<str>,
    requirement: VersionReq,
    named_version: Version,
    path_pin: bool,
    root_override_pin: bool,
    span: Range<usize>,
}

impl MvsEdge {
    /// Creates an ordinary edge, which names exactly the requirement minimum.
    pub fn new(
        package: impl Into<Box<str>>,
        requirement: VersionReq,
        named_version: Version,
        span: Range<usize>,
    ) -> Result<Self, MvsInputError> {
        Self::build(package, requirement, named_version, false, false, span)
    }

    /// Creates an edge whose source pins its node to the declared path version.
    pub fn new_path_pinned(
        package: impl Into<Box<str>>,
        requirement: VersionReq,
        declared_version: Version,
        span: Range<usize>,
    ) -> Result<Self, MvsInputError> {
        Self::build(package, requirement, declared_version, true, false, span)
    }

    /// Creates an edge transformed by a root `use path` override.
    ///
    /// Requirement satisfaction is checked by the selected-graph range check
    /// so the resolver can report `VERSION_CONFLICT` with the override.
    pub fn new_root_override_pinned(
        package: impl Into<Box<str>>,
        requirement: VersionReq,
        declared_version: Version,
        span: Range<usize>,
    ) -> Result<Self, MvsInputError> {
        Self::build(package, requirement, declared_version, false, true, span)
    }

    fn build(
        package: impl Into<Box<str>>,
        requirement: VersionReq,
        named_version: Version,
        path_pin: bool,
        root_override_pin: bool,
        span: Range<usize>,
    ) -> Result<Self, MvsInputError> {
        let package = package.into();
        if PackageIdentity::parse(&package).is_err() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::InvalidPackageIdentity(package),
                Some(span),
            ));
        }
        if named_version.compatibility_line() != requirement.compatibility_line() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::NamedVersionLineMismatch {
                    package,
                    requirement_line: requirement.compatibility_line(),
                    named_line: named_version.compatibility_line(),
                },
                Some(span),
            ));
        }
        if !root_override_pin && !requirement.matches_for_resolution(&named_version) {
            return Err(MvsInputError::new(
                MvsInputErrorKind::NamedVersionDoesNotSatisfyRequirement {
                    package,
                    named_version,
                },
                Some(span),
            ));
        }
        if !path_pin && !root_override_pin && named_version != *requirement.minimum() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::NamedVersionIsNotRequirementMinimum {
                    package,
                    named_version,
                    minimum: requirement.minimum().clone(),
                },
                Some(span),
            ));
        }
        Ok(Self {
            package,
            requirement,
            named_version,
            path_pin,
            root_override_pin,
            span,
        })
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub const fn requirement(&self) -> &VersionReq {
        &self.requirement
    }

    pub const fn named_version(&self) -> &Version {
        &self.named_version
    }

    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    pub(crate) const fn is_path_pin(&self) -> bool {
        self.path_pin
    }

    pub(crate) const fn is_root_override_pin(&self) -> bool {
        self.root_override_pin
    }

    pub(crate) fn target(&self) -> MvsPackageVersion {
        MvsPackageVersion {
            package: self.package.clone(),
            version: self.named_version.clone(),
        }
    }
}

/// A package version and its already-filtered, identity-sorted edges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsContract {
    package_version: MvsPackageVersion,
    edges: Vec<MvsEdge>,
}

impl MvsContract {
    pub fn new(
        package_version: MvsPackageVersion,
        mut edges: Vec<MvsEdge>,
    ) -> Result<Self, MvsInputError> {
        let mut required = HashSet::new();
        for edge in &edges {
            if edge.package == package_version.package {
                return Err(MvsInputError::new(
                    MvsInputErrorKind::SelfRequirement(edge.package.clone()),
                    Some(edge.span.clone()),
                ));
            }
            if !required.insert(edge.package.clone()) {
                return Err(MvsInputError::new(
                    MvsInputErrorKind::DuplicateRequirement(edge.package.clone()),
                    Some(edge.span.clone()),
                ));
            }
        }
        edges.sort_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
        Ok(Self {
            package_version,
            edges,
        })
    }

    pub const fn package_version(&self) -> &MvsPackageVersion {
        &self.package_version
    }

    pub fn edges(&self) -> &[MvsEdge] {
        &self.edges
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsInputErrorKind {
    InvalidPackageIdentity(Box<str>),
    SelfRequirement(Box<str>),
    DuplicateRequirement(Box<str>),
    NamedVersionLineMismatch {
        package: Box<str>,
        requirement_line: CompatibilityLine,
        named_line: CompatibilityLine,
    },
    NamedVersionDoesNotSatisfyRequirement {
        package: Box<str>,
        named_version: Version,
    },
    NamedVersionIsNotRequirementMinimum {
        package: Box<str>,
        named_version: Version,
        minimum: Version,
    },
    ContractKeyMismatch {
        key: MvsPackageVersion,
        contract: MvsPackageVersion,
    },
    FloorVersionLineMismatch {
        node: MvsNode,
        version: Version,
    },
    PinVersionLineMismatch {
        node: MvsNode,
        version: Version,
    },
    NamedVersionDoesNotMatchRootPathPin {
        node: MvsNode,
        named_version: Version,
        pinned_version: Version,
    },
    MissingRootPathPin(MvsNode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsInputError {
    pub kind: Box<MvsInputErrorKind>,
    pub span: Option<Range<usize>>,
}

impl MvsInputError {
    fn new(kind: MvsInputErrorKind, span: Option<Range<usize>>) -> Self {
        Self {
            kind: Box::new(kind),
            span,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MvsRequirer {
    Root(MvsPackageVersion),
    Package(MvsPackageVersion),
}

impl MvsRequirer {
    pub const fn package_version(&self) -> &MvsPackageVersion {
        match self {
            Self::Root(package_version) | Self::Package(package_version) => package_version,
        }
    }
}

/// An edge that named a concrete package version, excluding lock floors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsNamedEdge {
    pub requirer: MvsRequirer,
    pub span: Range<usize>,
    pub path_pin: bool,
    pub root_override_pin: bool,
}

/// Source-selection inputs captured when a concrete contract leaves the queue.
///
/// The snapshot is recorded even when the caller has not supplied the
/// contract. A loader can therefore choose the source using exactly the edges
/// known at the deterministic dequeue point, without later edges changing
/// that choice. Edge source clauses remain in the corresponding manifest
/// metadata and can be matched by requiring package version and source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsContractRequest {
    pub package_version: MvsPackageVersion,
    pub named_by: Vec<MvsNamedEdge>,
    /// Whether this version is also the node's lock floor. It is the source
    /// fallback only when `named_by` is empty.
    pub lock_floor: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsSelectionError {
    pub kind: MvsSelectionErrorKind,
    pub span: Option<Range<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsSelectionErrorKind {
    DependencyCycleToRoot {
        requirer: MvsRequirer,
        root: MvsPackageVersion,
    },
    ConflictingPinVersions {
        node: MvsNode,
        versions: Vec<Version>,
    },
    ContractNotProvided(MvsPackageVersion),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsSelectionOutput {
    root_contract: MvsContract,
    read_contracts: BTreeMap<MvsPackageVersion, MvsContract>,
    /// Selected version per reached identity and compatibility line: the
    /// maximum named version for unpinned nodes, or the exact path pin.
    selected: BTreeMap<MvsNode, Version>,
    /// Package versions whose contracts were read, in deterministic queue
    /// order.
    read_order: Vec<MvsPackageVersion>,
    /// Every concrete version whose contract was requested, including
    /// unavailable contracts, in deterministic queue order.
    contract_requests: Vec<MvsContractRequest>,
    /// All real edges that named each version. Lock floors are excluded.
    named_by: BTreeMap<MvsPackageVersion, Vec<MvsNamedEdge>>,
    /// The `named_by` snapshot available when each contract left the queue.
    read_named_by: BTreeMap<MvsPackageVersion, Vec<MvsNamedEdge>>,
    /// Pin versions from root overrides and path edges that were reached.
    reached_pins: BTreeMap<MvsNode, BTreeSet<Version>>,
    /// Lock floor versions added for reached nodes.
    reached_floors: BTreeMap<MvsNode, Version>,
    errors: Vec<MvsSelectionError>,
}

/// Failure to select with a contract provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsSelectionProviderError<E> {
    Input(Vec<MvsInputError>),
    ContractProvider {
        failures: Vec<MvsContractProviderFailure<E>>,
        /// Partial selection retains graph errors found on independent
        /// branches, including a known cycle back to the root.
        partial_selection: Box<MvsSelectionOutput>,
    },
}

/// One package contract that a provider could not load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsContractProviderFailure<E> {
    pub request: MvsContractRequest,
    pub error: E,
}

impl MvsSelectionOutput {
    fn new(root_contract: MvsContract) -> Self {
        Self {
            root_contract,
            read_contracts: BTreeMap::new(),
            selected: BTreeMap::new(),
            read_order: Vec::new(),
            contract_requests: Vec::new(),
            named_by: BTreeMap::new(),
            read_named_by: BTreeMap::new(),
            reached_pins: BTreeMap::new(),
            reached_floors: BTreeMap::new(),
            errors: Vec::new(),
        }
    }

    pub const fn root(&self) -> &MvsPackageVersion {
        self.root_contract.package_version()
    }

    pub const fn root_contract(&self) -> &MvsContract {
        &self.root_contract
    }

    pub fn read_contracts(&self) -> &BTreeMap<MvsPackageVersion, MvsContract> {
        &self.read_contracts
    }

    pub fn selected(&self) -> &BTreeMap<MvsNode, Version> {
        &self.selected
    }

    pub fn read_order(&self) -> &[MvsPackageVersion] {
        &self.read_order
    }

    pub fn contract_requests(&self) -> &[MvsContractRequest] {
        &self.contract_requests
    }

    pub fn named_by(&self) -> &BTreeMap<MvsPackageVersion, Vec<MvsNamedEdge>> {
        &self.named_by
    }

    pub fn read_named_by(&self) -> &BTreeMap<MvsPackageVersion, Vec<MvsNamedEdge>> {
        &self.read_named_by
    }

    pub fn reached_pins(&self) -> &BTreeMap<MvsNode, BTreeSet<Version>> {
        &self.reached_pins
    }

    pub fn reached_floors(&self) -> &BTreeMap<MvsNode, Version> {
        &self.reached_floors
    }

    pub fn errors(&self) -> &[MvsSelectionError] {
        &self.errors
    }

    pub fn is_complete(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Runs the deterministic MVS selection phase over preloaded contracts.
///
/// `contracts` must contain every reachable concrete version's already
/// validated contract. Unreached entries are not inspected. `floors` and
/// root-override `pins` are keyed by identity and line; their values are
/// checked only when their node is reached. Edges affected by a root path
/// override must already name the pinned version. A floor contract is added to
/// the candidate closure when its node is first reached unless a root override
/// already pins that node. Path pins are carried by their edges and take effect
/// only when those edges are reached. A reached path pin determines the final
/// version for its node, while the floor contract can remain in the candidate
/// closure and contribute edges for other nodes. The root contract's edges
/// must already include test requirements, while dependency contracts must
/// omit their test requirements and include only edges selected by target
/// filtering.
///
/// The function computes selected versions and records source-selection
/// provenance, including the `named_by` snapshot at each contract's queue
/// dequeue. It does not fetch contracts, inspect path packages, choose
/// sources, or perform the selected-graph range/cycle checks of §2.4.3–4.
pub fn select(
    root: &MvsContract,
    contracts: &HashMap<MvsPackageVersion, MvsContract>,
    floors: &HashMap<MvsNode, Version>,
    pins: &HashMap<MvsNode, Version>,
) -> Result<MvsSelectionOutput, Vec<MvsInputError>> {
    select_impl(root, floors, pins, |request| {
        contracts.get(&request.package_version).cloned()
    })
}

/// Runs deterministic MVS selection while loading each contract when its
/// concrete package version leaves the queue.
///
/// The provider receives the exact `named_by` snapshot at that dequeue point,
/// including when only a lock floor introduced the version. It can use the
/// requiring package versions and spans to find source clauses in the
/// corresponding manifests. Before returning a contract, it is responsible
/// for verifying the source identity and version and parsing and semantically
/// validating the manifest. Each concrete version is requested at most once.
/// The result contains the selection and every loaded contract. Provider
/// failures are collected while other branches continue, so the partial
/// selection retains independent errors such as a known cycle to the root.
/// Graph range checks and selected-version cycle checks remain separate.
pub fn select_with_provider<E>(
    root: &MvsContract,
    floors: &HashMap<MvsNode, Version>,
    pins: &HashMap<MvsNode, Version>,
    mut load_contract: impl FnMut(&MvsContractRequest) -> Result<MvsContract, E>,
) -> Result<MvsSelectionOutput, MvsSelectionProviderError<E>> {
    let mut failures = Vec::new();
    let selection = select_impl(root, floors, pins, |request| match load_contract(request) {
        Ok(contract) => Some(contract),
        Err(error) => {
            failures.push(MvsContractProviderFailure {
                request: request.clone(),
                error,
            });
            None
        }
    })
    .map_err(MvsSelectionProviderError::Input)?;
    if failures.is_empty() {
        Ok(selection)
    } else {
        Err(MvsSelectionProviderError::ContractProvider {
            failures,
            partial_selection: Box::new(selection),
        })
    }
}

fn select_impl(
    root: &MvsContract,
    floors: &HashMap<MvsNode, Version>,
    pins: &HashMap<MvsNode, Version>,
    mut load_contract: impl FnMut(&MvsContractRequest) -> Option<MvsContract>,
) -> Result<MvsSelectionOutput, Vec<MvsInputError>> {
    let mut state = MvsSelectionState::new(root.clone(), floors, pins);

    for edge in &root.edges {
        state
            .name_edge(edge, MvsRequirer::Root(state.root_version.clone()))
            .map_err(|error| vec![error])?;
    }

    while let Some(package_version) = state.queue.pop_first() {
        if !state.read.insert(package_version.clone()) {
            continue;
        }
        let mut named_by = state
            .output
            .named_by
            .get(&package_version)
            .cloned()
            .unwrap_or_default();
        sort_named_edge_list(&mut named_by);
        let node = package_version.node();
        let lock_floor = state.output.reached_floors.get(&node) == Some(package_version.version());
        let request = MvsContractRequest {
            package_version: package_version.clone(),
            named_by: named_by.clone(),
            lock_floor,
        };
        state.output.contract_requests.push(request.clone());
        let contract = load_contract(&request);
        let Some(contract) = contract else {
            state.output.errors.push(MvsSelectionError {
                kind: MvsSelectionErrorKind::ContractNotProvided(package_version),
                span: None,
            });
            continue;
        };
        if contract.package_version != package_version {
            return Err(vec![MvsInputError::new(
                MvsInputErrorKind::ContractKeyMismatch {
                    key: package_version,
                    contract: contract.package_version.clone(),
                },
                None,
            )]);
        }

        state
            .output
            .read_contracts
            .insert(package_version.clone(), contract.clone());
        state.output.read_order.push(package_version.clone());
        state
            .output
            .read_named_by
            .insert(package_version.clone(), named_by);
        for edge in &contract.edges {
            state
                .name_edge(edge, MvsRequirer::Package(package_version.clone()))
                .map_err(|error| vec![error])?;
        }
    }

    for (node, versions) in &state.output.reached_pins {
        let pinned_version = versions.last().expect("a pin set is non-empty").clone();
        state.output.selected.insert(node.clone(), pinned_version);
        if versions.len() > 1 {
            state.output.errors.push(MvsSelectionError {
                kind: MvsSelectionErrorKind::ConflictingPinVersions {
                    node: node.clone(),
                    versions: versions.iter().cloned().collect(),
                },
                span: None,
            });
        }
    }
    sort_named_edges(&mut state.output.named_by);
    sort_named_edges(&mut state.output.read_named_by);
    for request in &mut state.output.contract_requests {
        sort_named_edge_list(&mut request.named_by);
    }
    Ok(state.output)
}

struct MvsSelectionState<'a> {
    root_version: MvsPackageVersion,
    root_node: MvsNode,
    floors: &'a HashMap<MvsNode, Version>,
    pins: &'a HashMap<MvsNode, Version>,
    touched: HashSet<MvsNode>,
    read: HashSet<MvsPackageVersion>,
    queue: BTreeSet<MvsPackageVersion>,
    output: MvsSelectionOutput,
}

impl<'a> MvsSelectionState<'a> {
    fn new(
        root_contract: MvsContract,
        floors: &'a HashMap<MvsNode, Version>,
        pins: &'a HashMap<MvsNode, Version>,
    ) -> Self {
        let root_version = root_contract.package_version.clone();
        let output = MvsSelectionOutput::new(root_contract);
        Self {
            root_node: root_version.node(),
            root_version,
            floors,
            pins,
            touched: HashSet::new(),
            read: HashSet::new(),
            queue: BTreeSet::new(),
            output,
        }
    }

    fn name_edge(&mut self, edge: &MvsEdge, requirer: MvsRequirer) -> Result<(), MvsInputError> {
        let package_version = edge.target();
        let node = package_version.node();
        if node == self.root_node {
            self.output.errors.push(MvsSelectionError {
                kind: MvsSelectionErrorKind::DependencyCycleToRoot {
                    requirer,
                    root: self.root_version.clone(),
                },
                span: Some(edge.span.clone()),
            });
            return Ok(());
        }

        if let Some(pin) = self.pins.get(&node) {
            if pin.compatibility_line() != node.line {
                return Err(MvsInputError::new(
                    MvsInputErrorKind::PinVersionLineMismatch {
                        node,
                        version: pin.clone(),
                    },
                    None,
                ));
            }
            if pin != &edge.named_version {
                return Err(MvsInputError::new(
                    MvsInputErrorKind::NamedVersionDoesNotMatchRootPathPin {
                        node,
                        named_version: edge.named_version.clone(),
                        pinned_version: pin.clone(),
                    },
                    Some(edge.span.clone()),
                ));
            }
            self.output
                .reached_pins
                .entry(node.clone())
                .or_default()
                .insert(pin.clone());
        } else if edge.is_root_override_pin() {
            return Err(MvsInputError::new(
                MvsInputErrorKind::MissingRootPathPin(node),
                Some(edge.span.clone()),
            ));
        }
        if edge.is_path_pin() {
            self.output
                .reached_pins
                .entry(node.clone())
                .or_default()
                .insert(edge.named_version.clone());
        }

        if self.touched.insert(node.clone()) && !self.pins.contains_key(&node) {
            if let Some(floor) = self.floors.get(&node) {
                if floor.compatibility_line() != node.line {
                    return Err(MvsInputError::new(
                        MvsInputErrorKind::FloorVersionLineMismatch {
                            node: node.clone(),
                            version: floor.clone(),
                        },
                        None,
                    ));
                }
                self.record_named_version(
                    MvsPackageVersion {
                        package: node.package.clone(),
                        version: floor.clone(),
                    },
                    None,
                );
                self.output
                    .reached_floors
                    .insert(node.clone(), floor.clone());
            }
        }

        self.record_named_version(
            package_version,
            Some(MvsNamedEdge {
                requirer,
                span: edge.span.clone(),
                path_pin: edge.is_path_pin(),
                root_override_pin: edge.is_root_override_pin(),
            }),
        );
        Ok(())
    }

    fn record_named_version(
        &mut self,
        package_version: MvsPackageVersion,
        named_edge: Option<MvsNamedEdge>,
    ) {
        if let Some(named_edge) = named_edge {
            self.output
                .named_by
                .entry(package_version.clone())
                .or_default()
                .push(named_edge);
        }

        let node = package_version.node();
        self.output
            .selected
            .entry(node)
            .and_modify(|selected| {
                if &package_version.version > selected {
                    *selected = package_version.version.clone();
                }
            })
            .or_insert_with(|| package_version.version.clone());

        if !self.read.contains(&package_version) {
            self.queue.insert(package_version);
        }
    }
}

fn sort_named_edges(named_by: &mut BTreeMap<MvsPackageVersion, Vec<MvsNamedEdge>>) {
    for edges in named_by.values_mut() {
        sort_named_edge_list(edges);
    }
}

fn sort_named_edge_list(edges: &mut [MvsNamedEdge]) {
    edges.sort_by(|left, right| {
        left.requirer
            .package_version()
            .cmp(right.requirer.package_version())
            .then_with(|| left.span.start.cmp(&right.span.start))
            .then_with(|| left.span.end.cmp(&right.span.end))
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        MvsContract, MvsEdge, MvsInputErrorKind, MvsNamedEdge, MvsNode, MvsPackageVersion,
        MvsRequirer, MvsSelectionErrorKind, MvsSelectionProviderError, select,
        select_with_provider,
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
            VersionReq::parse(requirement).expect("valid test requirement"),
            version(named_version),
            start..start + 1,
        )
        .expect("valid MVS edge")
    }

    fn contract(package: &str, version_: &str, edges: Vec<MvsEdge>) -> MvsContract {
        MvsContract::new(package_version(package, version_), edges).expect("valid test contract")
    }

    fn catalog(contracts: Vec<MvsContract>) -> HashMap<MvsPackageVersion, MvsContract> {
        contracts
            .into_iter()
            .map(|contract| (contract.package_version().clone(), contract))
            .collect()
    }

    fn node(package: &str, line_version: &str) -> MvsNode {
        package_version(package, line_version).node()
    }

    #[test]
    fn selects_the_maximum_of_named_versions_across_the_visited_closure() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![
                    edge("acme:b", "^1.2", "1.2.0", 10),
                    edge("acme:c", "^1", "1.0.0", 20),
                ],
            ),
            contract("acme:c", "1.0.0", vec![edge("acme:b", "^1.5", "1.5.0", 30)]),
            contract("acme:b", "1.2.0", vec![]),
            contract("acme:b", "1.5.0", vec![]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");

        assert!(output.is_complete(), "{:?}", output.errors);
        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.5.0"));
        assert!(
            output
                .read_order
                .contains(&package_version("acme:b", "1.2.0"))
        );
        assert!(
            output
                .read_order
                .contains(&package_version("acme:b", "1.5.0"))
        );
    }

    #[test]
    fn requirements_from_unselected_versions_still_raise_the_selection() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![
                    edge("acme:b", "^1", "1.0.0", 10),
                    edge("acme:c", "^1", "1.0.0", 20),
                ],
            ),
            contract("acme:c", "1.0.0", vec![edge("acme:b", "^2", "2.0.0", 30)]),
            contract("acme:b", "1.0.0", vec![edge("acme:d", "^1", "1.0.0", 40)]),
            contract("acme:b", "2.0.0", vec![]),
            contract("acme:d", "1.0.0", vec![]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");

        assert_eq!(output.selected[&node("acme:b", "2.0.0")], version("2.0.0"));
        assert!(
            output
                .read_order
                .contains(&package_version("acme:b", "1.0.0"))
        );
        assert!(output.selected.contains_key(&node("acme:d", "1.0.0")));
    }

    #[test]
    fn applies_lock_floors_only_after_the_node_is_reached() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let orphan_contract = contract("acme:orphan", "1.0.0", vec![]);
        let contracts = HashMap::from([
            (
                package_version("acme:a", "1.0.0"),
                contract("acme:a", "1.0.0", vec![]),
            ),
            (package_version("acme:missing", "1.0.0"), orphan_contract),
        ]);
        let floors = HashMap::from([(node("acme:orphan", "2.0.0"), version("1.0.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS inputs");

        assert!(!output.selected.contains_key(&node("acme:orphan", "2.0.0")));
        assert!(output.is_complete(), "{:?}", output.errors);
    }

    #[test]
    fn names_a_reached_floor_without_claiming_a_requirement_edge() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 10)]),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.8.0", vec![]),
        ]);
        let floors = HashMap::from([(node("acme:b", "1.0.0"), version("1.8.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS inputs");

        let floor = package_version("acme:b", "1.8.0");
        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.8.0"));
        assert!(!output.named_by.contains_key(&floor));
        assert!(output.read_named_by[&floor].is_empty());
    }

    #[test]
    fn contract_requests_capture_named_edges_at_the_dequeue_point() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:zz", "^1", "1.0.0", 1),
            ],
        );
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 10)]),
            contract("acme:zz", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 20)]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");
        let z = package_version("acme:z", "1.0.0");
        let request = output
            .contract_requests()
            .iter()
            .find(|request| request.package_version == z)
            .expect("the missing contract was requested");

        assert_eq!(
            request
                .named_by
                .iter()
                .map(|edge| edge.requirer.package_version().package())
                .collect::<Vec<_>>(),
            ["acme:a"]
        );
        assert!(!request.lock_floor);
        assert_eq!(output.named_by()[&z].len(), 2);
        assert!(!output.read_named_by().contains_key(&z));
        assert!(output.errors().iter().any(|error| {
            error.kind == super::MvsSelectionErrorKind::ContractNotProvided(z.clone())
        }));
    }

    #[test]
    fn contract_requests_identify_lock_floor_only_versions() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![]),
            contract("acme:a", "1.8.0", vec![]),
        ]);
        let floors = HashMap::from([(node("acme:a", "1.0.0"), version("1.8.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new())
            .expect("valid MVS inputs with a floor");
        let request = output
            .contract_requests()
            .iter()
            .find(|request| request.package_version == package_version("acme:a", "1.8.0"))
            .expect("the floor contract was requested");

        assert!(request.named_by.is_empty());
        assert!(request.lock_floor);
    }

    #[test]
    fn provider_loads_contracts_in_queue_order_and_keeps_dequeue_source_snapshot() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:zz", "^1", "1.0.0", 1),
            ],
        );
        let mut requests = Vec::new();
        let output = select_with_provider(&root, &HashMap::new(), &HashMap::new(), |request| {
            requests.push(request.clone());
            Ok::<_, &'static str>(match request.package_version.package() {
                "acme:a" => contract("acme:a", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 10)]),
                "acme:z" => contract("acme:z", "1.0.0", vec![]),
                "acme:zz" => contract("acme:zz", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 20)]),
                other => panic!("unexpected package request: {other}"),
            })
        })
        .expect("provider supplies every contract");

        assert_eq!(
            requests
                .iter()
                .map(|request| request.package_version.package())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:z", "acme:zz"]
        );
        let z_request = requests
            .iter()
            .find(|request| request.package_version.package() == "acme:z")
            .expect("z was requested");
        assert_eq!(
            z_request
                .named_by
                .iter()
                .map(|edge| edge.requirer.package_version().package())
                .collect::<Vec<_>>(),
            ["acme:a"]
        );
        let z = package_version("acme:z", "1.0.0");
        assert_eq!(output.named_by()[&z].len(), 2);
        assert_eq!(output.read_named_by()[&z].len(), 1);
        assert!(output.is_complete());
    }

    #[test]
    fn provider_failures_preserve_independent_root_cycles() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:b", "^1", "1.0.0", 1),
            ],
        );
        let error =
            select_with_provider(
                &root,
                &HashMap::new(),
                &HashMap::new(),
                |request| match request.package_version.package() {
                    "acme:a" => Err("offline"),
                    "acme:b" => Ok(contract(
                        "acme:b",
                        "1.0.0",
                        vec![edge("acme:app", "^1", "1.0.0", 10)],
                    )),
                    other => panic!("unexpected package request: {other}"),
                },
            )
            .expect_err("provider failure is returned");

        assert!(matches!(
            error,
            MvsSelectionProviderError::ContractProvider {
                failures,
                partial_selection,
            } if failures.len() == 1
                && failures[0].request.package_version == package_version("acme:a", "1.0.0")
                && failures[0].request.named_by.len() == 1
                && failures[0].error == "offline"
                && partial_selection.errors().iter().any(|error| matches!(
                    error.kind,
                    MvsSelectionErrorKind::DependencyCycleToRoot { .. }
                ))
        ));
    }

    #[test]
    fn provider_cannot_supply_a_contract_for_another_package_version() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let error = select_with_provider(&root, &HashMap::new(), &HashMap::new(), |_| {
            Ok::<_, &'static str>(contract("acme:b", "1.0.0", vec![]))
        })
        .expect_err("mismatched provider result is rejected");

        assert!(matches!(
            error,
            MvsSelectionProviderError::Input(errors)
                if matches!(
                    errors[0].kind.as_ref(),
                    MvsInputErrorKind::ContractKeyMismatch { .. }
                )
        ));
    }

    #[test]
    fn a_pin_prevents_a_lock_floor_from_being_visited() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 10)]),
            contract("acme:b", "1.0.0", vec![]),
        ]);
        let floors = HashMap::from([(node("acme:b", "1.0.0"), version("1.8.0"))]);
        let pins = HashMap::from([(node("acme:b", "1.0.0"), version("1.0.0"))]);
        let output = select(&root, &contracts, &floors, &pins).expect("valid MVS inputs");

        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.0.0"));
        assert!(
            !output
                .read_order
                .contains(&package_version("acme:b", "1.8.0"))
        );
    }

    #[test]
    fn root_path_override_names_its_pinned_version_on_each_edge() {
        let requirement = VersionReq::parse("^1").expect("valid requirement");
        let unmodified_root = contract(
            "acme:app",
            "1.0.0",
            vec![
                MvsEdge::new("acme:b", requirement.clone(), version("1.0.0"), 0..1)
                    .expect("valid ordinary edge"),
            ],
        );
        let contracts = catalog(vec![contract("acme:b", "1.5.0", vec![])]);
        let pins = HashMap::from([(node("acme:b", "1.0.0"), version("1.5.0"))]);
        let error = select(&unmodified_root, &contracts, &HashMap::new(), &pins)
            .expect_err("root path override must rewrite the edge's named version");
        assert!(matches!(
            error[0].kind.as_ref(),
            MvsInputErrorKind::NamedVersionDoesNotMatchRootPathPin { .. }
        ));

        let overridden_root = contract(
            "acme:app",
            "1.0.0",
            vec![
                MvsEdge::new_root_override_pinned("acme:b", requirement, version("1.5.0"), 0..1)
                    .expect("valid overridden path edge"),
            ],
        );
        let output = select(&overridden_root, &contracts, &HashMap::new(), &pins)
            .expect("matching root path override");
        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.5.0"));
        assert_eq!(
            output.reached_pins[&node("acme:b", "1.0.0")],
            [version("1.5.0")].into_iter().collect()
        );
    }

    #[test]
    fn root_path_override_range_mismatch_is_left_for_graph_checking() {
        let requirement = VersionReq::parse("=1.0.0").expect("valid requirement");
        let edge = MvsEdge::new_root_override_pinned("acme:b", requirement, version("1.5.0"), 0..1)
            .expect("root override range conflicts belong to the graph check");
        let root = contract("acme:app", "1.0.0", vec![edge]);
        let contracts = catalog(vec![contract("acme:b", "1.5.0", vec![])]);
        let pins = HashMap::from([(node("acme:b", "1.0.0"), version("1.5.0"))]);
        let output = select(&root, &contracts, &HashMap::new(), &pins)
            .expect("selection leaves root override range checking to phase two");

        assert!(output.is_complete());
        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.5.0"));
        assert!(
            !root.edges[0]
                .requirement
                .matches_for_resolution(output.selected.get(&node("acme:b", "1.0.0")).unwrap())
        );
    }

    #[test]
    fn reports_distinct_versions_pinning_the_same_node() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:c", "^1", "1.0.0", 1),
            ],
        );
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![
                    MvsEdge::new_path_pinned(
                        "acme:b",
                        VersionReq::parse("^1").expect("valid requirement"),
                        version("1.0.0"),
                        10..11,
                    )
                    .expect("valid path pin"),
                ],
            ),
            contract(
                "acme:c",
                "1.0.0",
                vec![
                    MvsEdge::new_path_pinned(
                        "acme:b",
                        VersionReq::parse("^1").expect("valid requirement"),
                        version("1.5.0"),
                        20..21,
                    )
                    .expect("valid path pin"),
                ],
            ),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:b", "1.5.0", vec![]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");

        assert!(matches!(
            &output.errors[0].kind,
            MvsSelectionErrorKind::ConflictingPinVersions {
                node: pinned_node,
                versions,
            } if pinned_node == &node("acme:b", "1.0.0")
                    && versions == &[version("1.0.0"), version("1.5.0")]
        ));
        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.5.0"));
    }

    #[test]
    fn path_pins_apply_only_when_their_edges_are_reached() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:x", "^1", "1.0.0", 10)]),
            contract("acme:x", "1.0.0", vec![]),
            contract("acme:x", "1.8.0", vec![]),
            contract(
                "acme:orphan",
                "1.0.0",
                vec![
                    MvsEdge::new_path_pinned(
                        "acme:x",
                        VersionReq::parse("^1").expect("valid requirement"),
                        version("1.0.0"),
                        20..21,
                    )
                    .expect("valid path pin"),
                ],
            ),
        ]);
        let floors = HashMap::from([(node("acme:x", "1.0.0"), version("1.8.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS inputs");

        assert_eq!(output.selected[&node("acme:x", "1.0.0")], version("1.8.0"));
        assert!(!output.reached_pins.contains_key(&node("acme:x", "1.0.0")));
    }

    #[test]
    fn a_reached_path_pin_selects_its_version_but_keeps_floor_in_candidate_closure() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:b", "^1", "1.0.0", 1),
            ],
        );
        let path_pin = MvsEdge::new_path_pinned(
            "acme:z",
            VersionReq::parse("^1").expect("valid requirement"),
            version("1.0.0"),
            10..11,
        )
        .expect("valid path pin");
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:z", "^1", "1.0.0", 10)]),
            contract("acme:b", "1.0.0", vec![path_pin]),
            contract("acme:z", "1.0.0", vec![]),
            contract("acme:z", "1.8.0", vec![]),
        ]);
        let floors = HashMap::from([(node("acme:z", "1.0.0"), version("1.8.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS inputs");

        assert_eq!(output.selected[&node("acme:z", "1.0.0")], version("1.0.0"));
        assert!(
            output
                .read_order
                .contains(&package_version("acme:z", "1.8.0"))
        );
        assert_eq!(
            output.reached_pins[&node("acme:z", "1.0.0")],
            [version("1.0.0")].into_iter().collect()
        );
    }

    #[test]
    fn a_late_path_pin_keeps_the_candidate_floor_edges_in_the_closure() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:a", "^1", "1.0.0", 0),
                edge("acme:y", "=1.0.0", "1.0.0", 1),
                edge("acme:z", "^1", "1.0.0", 2),
            ],
        );
        let contracts = catalog(vec![
            contract("acme:a", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 10)]),
            contract(
                "acme:z",
                "1.0.0",
                vec![
                    MvsEdge::new_path_pinned(
                        "acme:b",
                        VersionReq::parse("^1").expect("valid requirement"),
                        version("1.0.0"),
                        20..21,
                    )
                    .expect("valid path pin"),
                ],
            ),
            contract("acme:b", "1.0.0", vec![]),
            contract(
                "acme:b",
                "1.8.0",
                vec![edge("acme:y", "=1.8.0", "1.8.0", 30)],
            ),
            contract("acme:y", "1.0.0", vec![]),
            contract("acme:y", "1.8.0", vec![]),
        ]);
        let floors = HashMap::from([(node("acme:b", "1.0.0"), version("1.8.0"))]);
        let output = select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS inputs");

        assert_eq!(output.selected[&node("acme:b", "1.0.0")], version("1.0.0"));
        assert_eq!(output.selected[&node("acme:y", "1.0.0")], version("1.8.0"));
        assert!(
            output
                .read_order
                .contains(&package_version("acme:b", "1.8.0"))
        );
    }

    #[test]
    fn detects_an_indirect_edge_back_to_the_roots_compatibility_line() {
        let root = contract("acme:app", "1.4.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![contract(
            "acme:a",
            "1.0.0",
            vec![edge("acme:app", "^1", "1.0.0", 10)],
        )]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");

        assert!(output.errors.iter().any(|error| {
            matches!(
                &error.kind,
                MvsSelectionErrorKind::DependencyCycleToRoot { root, .. }
                    if root == &package_version("acme:app", "1.4.0")
            )
        }));
        assert!(
            !output
                .read_order
                .contains(&package_version("acme:app", "1.0.0"))
        );
        assert!(!output.errors.iter().any(|error| {
            matches!(
                &error.kind,
                MvsSelectionErrorKind::ContractNotProvided(package_version)
                    if package_version.package() == "acme:app"
            )
        }));
    }

    #[test]
    fn records_queue_order_and_source_provenance_deterministically() {
        let root = contract(
            "acme:app",
            "1.0.0",
            vec![
                edge("acme:z", "^1", "1.0.0", 0),
                edge("acme:a", "^2", "2.0.0", 1),
            ],
        );
        let contracts = catalog(vec![
            contract("acme:z", "1.0.0", vec![]),
            contract("acme:a", "2.0.0", vec![]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");

        assert_eq!(
            output.read_order,
            [
                package_version("acme:a", "2.0.0"),
                package_version("acme:z", "1.0.0"),
            ]
        );
        assert_eq!(
            output.named_by[&package_version("acme:a", "2.0.0")],
            [MvsNamedEdge {
                requirer: MvsRequirer::Root(package_version("acme:app", "1.0.0")),
                span: 1..2,
                path_pin: false,
                root_override_pin: false,
            }]
        );
        assert_eq!(
            output.read_named_by[&package_version("acme:z", "1.0.0")],
            [MvsNamedEdge {
                requirer: MvsRequirer::Root(package_version("acme:app", "1.0.0")),
                span: 0..1,
                path_pin: false,
                root_override_pin: false,
            }]
        );
    }

    #[test]
    fn read_provenance_is_a_queue_time_snapshot() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let contracts = catalog(vec![
            contract(
                "acme:a",
                "1.0.0",
                vec![
                    edge("acme:b", "^1", "1.0.0", 10),
                    edge("acme:c", "^1", "1.0.0", 20),
                ],
            ),
            contract("acme:b", "1.0.0", vec![]),
            contract("acme:c", "1.0.0", vec![edge("acme:b", "^1", "1.0.0", 30)]),
        ]);
        let output =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("valid MVS inputs");
        let b = package_version("acme:b", "1.0.0");
        let a_requirer = MvsRequirer::Package(package_version("acme:a", "1.0.0"));
        let c_requirer = MvsRequirer::Package(package_version("acme:c", "1.0.0"));

        assert_eq!(
            output.read_named_by[&b]
                .iter()
                .map(|edge| &edge.requirer)
                .collect::<Vec<_>>(),
            [&a_requirer]
        );
        assert_eq!(
            output.named_by[&b]
                .iter()
                .map(|edge| &edge.requirer)
                .collect::<Vec<_>>(),
            [&a_requirer, &c_requirer]
        );
    }

    #[test]
    fn reports_contracts_missing_from_the_preloaded_catalog() {
        let root = contract("acme:app", "1.0.0", vec![edge("acme:a", "^1", "1.0.0", 0)]);
        let output = select(&root, &HashMap::new(), &HashMap::new(), &HashMap::new())
            .expect("valid MVS input shape");

        assert_eq!(
            output.errors[0].kind,
            MvsSelectionErrorKind::ContractNotProvided(package_version("acme:a", "1.0.0"))
        );
    }

    #[test]
    fn rejects_invalid_named_version_lines_and_duplicate_edges() {
        let requirement = VersionReq::parse("^0.4").expect("valid requirement");
        let mismatch = MvsEdge::new("acme:a", requirement, version("1.0.0"), 4..5)
            .expect_err("named version must stay on the minimum's line");
        assert!(matches!(
            mismatch.kind.as_ref(),
            MvsInputErrorKind::NamedVersionLineMismatch { .. }
        ));

        let requirement = VersionReq::parse("^1.4").expect("valid requirement");
        let out_of_range = MvsEdge::new("acme:a", requirement, version("1.2.0"), 6..7)
            .expect_err("named version must satisfy the requirement");
        assert!(matches!(
            out_of_range.kind.as_ref(),
            MvsInputErrorKind::NamedVersionDoesNotSatisfyRequirement { .. }
        ));
        let requirement = VersionReq::parse("^1.4").expect("valid requirement");
        let not_minimum = MvsEdge::new("acme:a", requirement, version("1.5.0"), 8..9)
            .expect_err("ordinary edges name the requirement minimum");
        assert!(matches!(
            not_minimum.kind.as_ref(),
            MvsInputErrorKind::NamedVersionIsNotRequirementMinimum { .. }
        ));

        let first = edge("acme:a", "^1", "1.0.0", 0);
        let second = edge("acme:a", "^2", "2.0.0", 1);
        let duplicate =
            MvsContract::new(package_version("acme:root", "1.0.0"), vec![first, second])
                .expect_err("one identity may occur once per contract");
        assert_eq!(
            duplicate.kind.as_ref(),
            &MvsInputErrorKind::DuplicateRequirement("acme:a".into())
        );
    }
}
