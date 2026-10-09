//! Drives MVS contract loading through manifest-aware source selection.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::manifest::PackageRole;
use crate::mvs::{
    MvsContractProviderFailure, MvsContractRequest, MvsInputError, MvsNode, MvsPackageVersion,
    MvsSelectionError, MvsSelectionOutput, MvsSelectionProviderError, select_with_provider,
};
use crate::mvs_graph_validation::{
    MvsSelectedGraph, MvsSelectedGraphInputError, check_selected_graph_with_root_cycles,
};
use crate::resolver_contract::{ResolverContract, ResolverContractContext};
use crate::resolver_source::{
    ResolverPackageSource, ResolverPathPinSource, ResolverPathPinValidationError,
    ResolverSourceSelection, ResolverSourceSelectionError, select_resolver_source, source_for_edge,
    validate_resolver_path_pins,
};
use crate::version::Version;

struct ResolverPathPinCanonicalizationCache<K> {
    canonical_sources: Vec<(ResolverPathPinSource, K)>,
    failed_pins: Vec<ResolverPathPinSource>,
    reported_conflicts: Vec<(ResolverPathPinSource, ResolverPathPinSource)>,
}

impl<K> ResolverPathPinCanonicalizationCache<K> {
    fn new() -> Self {
        Self {
            canonical_sources: Vec::new(),
            failed_pins: Vec::new(),
            reported_conflicts: Vec::new(),
        }
    }
}

fn canonicalize_path_pin_with_cache<K: Clone, C>(
    pin: &ResolverPathPinSource,
    cache: &mut ResolverPathPinCanonicalizationCache<K>,
    canonicalize: &mut impl FnMut(&ResolverPathPinSource) -> Result<K, C>,
) -> Result<K, C> {
    if let Some((_, key)) = cache
        .canonical_sources
        .iter()
        .find(|(known_pin, _)| known_pin == pin)
    {
        return Ok(key.clone());
    }
    match canonicalize(pin) {
        Ok(key) => {
            cache.canonical_sources.push((pin.clone(), key.clone()));
            Ok(key)
        }
        Err(error) => {
            if !cache.failed_pins.contains(pin) {
                cache.failed_pins.push(pin.clone());
            }
            Err(error)
        }
    }
}

/// The completed or partial result of source-aware MVS contract loading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceProviderOutput {
    pub root_contract: ResolverContract,
    pub selection: MvsSelectionOutput,
    /// Successfully loaded dependency contracts, including unselected MVS
    /// candidates whose edges contributed to selection.
    pub dependency_contracts: BTreeMap<MvsPackageVersion, ResolverContract>,
    /// Source chosen when each concrete version left the MVS queue.
    pub sources_used: BTreeMap<MvsPackageVersion, ResolverSourceSelection>,
}

/// One resolved dependency's read-time and post-Check source choices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverFinalSourcePlanEntry {
    package_version: MvsPackageVersion,
    read_request: MvsContractRequest,
    final_request: MvsContractRequest,
    read_source: ResolverSourceSelection,
    final_source: ResolverSourceSelection,
}

impl ResolverFinalSourcePlanEntry {
    pub const fn package_version(&self) -> &MvsPackageVersion {
        &self.package_version
    }

    pub const fn read_request(&self) -> &MvsContractRequest {
        &self.read_request
    }

    pub const fn final_request(&self) -> &MvsContractRequest {
        &self.final_request
    }

    pub const fn read_source(&self) -> &ResolverSourceSelection {
        &self.read_source
    }

    pub const fn final_source(&self) -> &ResolverSourceSelection {
        &self.final_source
    }
}

/// A selected node whose final source could not be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverFinalSourcePlanFailure {
    package_version: MvsPackageVersion,
    read_request: MvsContractRequest,
    read_source: ResolverSourceSelection,
    error: ResolverSourceSelectionError,
}

impl ResolverFinalSourcePlanFailure {
    pub const fn package_version(&self) -> &MvsPackageVersion {
        &self.package_version
    }

    pub const fn read_request(&self) -> &MvsContractRequest {
        &self.read_request
    }

    pub const fn read_source(&self) -> &ResolverSourceSelection {
        &self.read_source
    }

    pub const fn error(&self) -> &ResolverSourceSelectionError {
        &self.error
    }
}

/// Which retained source needed identity resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverSourceIdentitySide {
    ReadTime,
    Final,
}

/// Why a source identity could not be recorded for a plan entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverSourceIdentityFailureKind<E> {
    MissingReadTimeIdentity(Box<MvsPackageVersion>),
    MissingBaseSourceIdentity(Box<MvsPackageVersion>),
    FinalBaseSourceIdentityUnavailable(Box<MvsPackageVersion>),
    FinalSourceSelection(Box<ResolverSourceSelectionError>),
    PackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    Canonicalization(Box<E>),
}

/// A read-time or final source whose complete identity could not be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceIdentityFailure<E> {
    pub package_version: MvsPackageVersion,
    pub side: ResolverSourceIdentitySide,
    /// Present when an actual source selection exists for this side.
    pub source: Option<ResolverSourceSelection>,
    pub error: ResolverSourceIdentityFailureKind<E>,
}

/// A package version whose two source selections resolve to one identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSameSourceIdentity<K> {
    pub package_version: MvsPackageVersion,
    pub read_source: ResolverSourceSelection,
    pub final_source: ResolverSourceSelection,
    pub identity: K,
}

/// A package version that must be reloaded from its final source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceReload<K> {
    pub package_version: MvsPackageVersion,
    pub read_source: ResolverSourceSelection,
    pub final_source: ResolverSourceSelection,
    pub read_identity: K,
    pub final_identity: K,
}

/// Per-node identity results for the post-`Check` source audit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceIdentityPlan<K, E> {
    /// Nodes whose read-time and final sources identify the same source tree.
    pub same_identity: Vec<ResolverSameSourceIdentity<K>>,
    /// Nodes that must be fetched and revalidated from the final source.
    pub reloads: Vec<ResolverSourceReload<K>>,
    /// Final-source selection and identity errors, in package-version/side order.
    pub failures: Vec<ResolverSourceIdentityFailure<E>>,
}

/// Read-time and final-source selections for the selected graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverFinalSourcePlan {
    /// Includes range conflicts and dependency-to-dependency cycles.
    graph: MvsSelectedGraph,
    /// Selection-time edges back to the root, which are not dependency nodes.
    root_cycles: Vec<MvsSelectionError>,
    /// Entries are sorted by package identity and version.
    entries: Vec<ResolverFinalSourcePlanEntry>,
    /// Independent final-source selection errors, in package-version order.
    failures: Vec<ResolverFinalSourcePlanFailure>,
}

impl ResolverFinalSourcePlan {
    pub const fn graph(&self) -> &MvsSelectedGraph {
        &self.graph
    }

    pub fn root_cycles(&self) -> &[MvsSelectionError] {
        &self.root_cycles
    }

    pub fn entries(&self) -> &[ResolverFinalSourcePlanEntry] {
        &self.entries
    }

    pub fn failures(&self) -> &[ResolverFinalSourcePlanFailure] {
        &self.failures
    }
}

/// A provider result could not be safely paired with its MVS selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverFinalSourcePlanError {
    SelectedGraph(Box<MvsSelectedGraphInputError>),
    ProviderOutputMismatch(Box<MvsPackageVersion>),
}

/// A contract-loading failure associated with its exact MVS request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceProviderFailure<E, K, C> {
    pub request: MvsContractRequest,
    pub error: ResolverPackageLoadError<E, K, C>,
}

/// A path-pin validation failure found in the final accumulated named edges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverPathPinProviderFailure<K, C> {
    pub node: MvsNode,
    pub error: ResolverPathPinProviderFailureKind<K, C>,
}

/// The source metadata or canonical identities for a node's path pins disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverPathPinProviderFailureKind<K, C> {
    SourceSelection(ResolverSourceSelectionError),
    Validation(Box<ResolverPathPinValidationError<K, C>>),
}

/// Why a contract request could not be resolved to a validated dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverPackageLoadError<E, K, C> {
    SourceSelection(ResolverSourceSelectionError),
    PathPinValidation(Box<ResolverPathPinValidationError<K, C>>),
    Provider(Box<E>),
    PackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    WrongContractRole {
        package_version: Box<MvsPackageVersion>,
        found: PackageRole,
    },
    StaleRootContext(Box<MvsPackageVersion>),
}

/// Failure to run source-aware MVS selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverSourceProviderError<E, K, C> {
    RootContract(ResolverSourceSelectionError),
    Input(Vec<MvsInputError>),
    Resolution {
        failures: Vec<ResolverSourceProviderFailure<E, K, C>>,
        path_pin_failures: Vec<ResolverPathPinProviderFailure<K, C>>,
        /// Completed or partial MVS state and every successfully loaded
        /// source-bound contract.
        output: Box<ResolverSourceProviderOutput>,
    },
}

/// Loads contracts as deterministic MVS requests leave the priority queue.
///
/// The canonicalizer resolves each path-pin candidate using the appropriate
/// local-filesystem or Git-tree rules. This function compares the resulting
/// complete source keys before invoking the loader, so a path-pin selection
/// cannot be loaded without agreement. The loader receives the selected
/// source and candidates after that check; it is responsible for reading,
/// parsing, and semantically validating the source, then returning a
/// dependency-role contract bound to `root_context`.
///
/// Root path pins come from `root_context`; `lock_floors` and `locked_sources`
/// must come from a validated lock. The result retains the source used for
/// every successfully loaded MVS candidate, not only the final selected
/// versions. After the queue is drained, all path pins in the accumulated
/// named-edge set are checked again, because a later contract can add a pin
/// for a version whose contract has already been read. On partial selections,
/// that set contains only edges from contracts that were successfully read.
pub fn select_with_source_provider<E, K, C>(
    root_contract: &ResolverContract,
    root_context: &ResolverContractContext<'_>,
    lock_floors: &HashMap<MvsNode, Version>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    mut canonicalize_path_pin: impl FnMut(&ResolverPathPinSource) -> Result<K, C>,
    mut load_contract: impl FnMut(
        &MvsContractRequest,
        &ResolverSourceSelection,
    ) -> Result<ResolverContract, E>,
) -> Result<ResolverSourceProviderOutput, ResolverSourceProviderError<E, K, C>>
where
    K: Eq + Clone,
{
    if root_contract.role() != PackageRole::Root {
        return Err(ResolverSourceProviderError::RootContract(
            ResolverSourceSelectionError::RootContractRoleMismatch(root_contract.role()),
        ));
    }
    if root_contract.mvs.package_version() != root_context.root_package() {
        return Err(ResolverSourceProviderError::RootContract(
            ResolverSourceSelectionError::RootContractMismatch {
                contract: Box::new(root_contract.mvs.package_version().clone()),
                context: Box::new(root_context.root_package().clone()),
            },
        ));
    }
    if !root_context.is_bound_to_root_contract(root_contract) {
        return Err(ResolverSourceProviderError::RootContract(
            ResolverSourceSelectionError::StaleRootContractContext,
        ));
    }

    let mut loaded_contracts = HashMap::new();
    let mut sources_used = BTreeMap::new();
    let mut path_pin_cache = ResolverPathPinCanonicalizationCache::new();
    let pins = root_context.root_override_pins();
    let selection = select_with_provider(&root_contract.mvs, lock_floors, &pins, |request| {
        let source = select_resolver_source(
            request,
            root_contract,
            root_context,
            &loaded_contracts,
            locked_sources,
        )
        .map_err(ResolverPackageLoadError::SourceSelection)?;
        if let Err(error) = validate_resolver_path_pins(&source, |pin| {
            canonicalize_path_pin_with_cache(pin, &mut path_pin_cache, &mut canonicalize_path_pin)
        }) {
            if let ResolverPathPinValidationError::ConflictingSources {
                first, conflicting, ..
            } = &error
            {
                path_pin_cache
                    .reported_conflicts
                    .push((first.as_ref().clone(), conflicting.as_ref().clone()));
            }
            return Err(ResolverPackageLoadError::PathPinValidation(Box::new(error)));
        }
        let contract = load_contract(request, &source)
            .map_err(|error| ResolverPackageLoadError::Provider(Box::new(error)))?;
        let found = contract.mvs.package_version();
        if found != &request.package_version {
            return Err(ResolverPackageLoadError::PackageVersionMismatch {
                expected: Box::new(request.package_version.clone()),
                found: Box::new(found.clone()),
            });
        }
        if contract.role() != PackageRole::Dependency {
            return Err(ResolverPackageLoadError::WrongContractRole {
                package_version: Box::new(request.package_version.clone()),
                found: contract.role(),
            });
        }
        if !root_context.is_bound_to_root_contract(&contract) {
            return Err(ResolverPackageLoadError::StaleRootContext(Box::new(
                request.package_version.clone(),
            )));
        }

        let package_version = request.package_version.clone();
        sources_used.insert(package_version.clone(), source);
        loaded_contracts.insert(package_version, contract.clone());
        Ok(contract.mvs.clone())
    });

    let output = |selection| ResolverSourceProviderOutput {
        root_contract: root_contract.clone(),
        selection,
        dependency_contracts: loaded_contracts
            .iter()
            .map(|(version, contract)| (version.clone(), contract.clone()))
            .collect(),
        sources_used: sources_used.clone(),
    };

    let (selection, failures) = match selection {
        Ok(selection) => (selection, Vec::new()),
        Err(MvsSelectionProviderError::Input(errors)) => {
            return Err(ResolverSourceProviderError::Input(errors));
        }
        Err(MvsSelectionProviderError::ContractProvider {
            failures,
            partial_selection,
        }) => (
            *partial_selection,
            failures
                .into_iter()
                .map(|MvsContractProviderFailure { request, error }| {
                    ResolverSourceProviderFailure { request, error }
                })
                .collect(),
        ),
    };
    let partial_selection = output(selection);
    let mut path_pin_failures = collect_final_path_pin_failures(
        &partial_selection.selection,
        root_contract,
        root_context,
        &loaded_contracts,
        &mut path_pin_cache,
        &mut canonicalize_path_pin,
    );
    path_pin_failures.retain(|path_pin_failure| {
        !failures.iter().any(|failure| {
            failure.request.package_version.node() == path_pin_failure.node
                && match (&failure.error, &path_pin_failure.error) {
                    (
                        ResolverPackageLoadError::SourceSelection(previous),
                        ResolverPathPinProviderFailureKind::SourceSelection(current),
                    ) => previous == current,
                    _ => false,
                }
        })
    });
    if failures.is_empty() && path_pin_failures.is_empty() {
        Ok(partial_selection)
    } else {
        Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            output: Box::new(partial_selection),
        })
    }
}

/// Recomputes source choices after the selected graph has been checked.
///
/// The plan includes only dependencies reachable through selected versions.
/// For each such dependency it retains the request and source used when the
/// contract was read, then selects again using every edge accumulated by MVS.
/// Range conflicts and cycles are retained alongside the plan and do not
/// suppress source planning. Final source-selection failures are collected per
/// node.
pub fn plan_resolver_final_sources(
    output: &ResolverSourceProviderOutput,
    root_context: &ResolverContractContext<'_>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
) -> Result<ResolverFinalSourcePlan, ResolverFinalSourcePlanError> {
    let root_version = output.root_contract.mvs.package_version().clone();
    let (graph, root_cycles) =
        check_selected_graph_with_root_cycles(&output.root_contract.mvs, &output.selection)
            .map_err(|error| ResolverFinalSourcePlanError::SelectedGraph(Box::new(error)))?;

    if output.root_contract.role() != PackageRole::Root
        || !root_context.is_bound_to_root_contract(&output.root_contract)
    {
        return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
            Box::new(root_version),
        ));
    }

    let mut read_requests = BTreeMap::new();
    for request in output.selection.contract_requests() {
        let package_version = &request.package_version;
        if read_requests
            .insert(package_version.clone(), request)
            .is_some()
            || output.selection.read_named_by().get(package_version) != Some(&request.named_by)
            || request.lock_floor
                != (output
                    .selection
                    .reached_floors()
                    .get(&package_version.node())
                    == Some(package_version.version()))
        {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }

    for package_version in output.selection.read_contracts().keys() {
        if !read_requests.contains_key(package_version) {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }
    for package_version in read_requests.keys() {
        if !output
            .selection
            .read_contracts()
            .contains_key(package_version)
        {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }

    for (package_version, mvs_contract) in output.selection.read_contracts() {
        let Some(contract) = output.dependency_contracts.get(package_version) else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        if contract.mvs != *mvs_contract
            || contract.role() != PackageRole::Dependency
            || !contract.mvs_metadata_is_consistent()
            || !root_context.is_bound_to_root_contract(contract)
        {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
        let Some(source) = output.sources_used.get(package_version) else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        if source.package_version() != package_version {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }
    for package_version in output.dependency_contracts.keys() {
        if !output
            .selection
            .read_contracts()
            .contains_key(package_version)
        {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }
    for package_version in output.sources_used.keys() {
        if !output
            .selection
            .read_contracts()
            .contains_key(package_version)
        {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
    }

    let mut read_time_contracts = HashMap::new();
    for package_version in output.selection.read_order() {
        let Some(request) = read_requests.get(package_version).copied() else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        let Some(read_source) = output.sources_used.get(package_version) else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        let recomputed_source = select_resolver_source(
            request,
            &output.root_contract,
            root_context,
            &read_time_contracts,
            locked_sources,
        )
        .map_err(|_| {
            ResolverFinalSourcePlanError::ProviderOutputMismatch(Box::new(package_version.clone()))
        })?;
        if &recomputed_source != read_source {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        }
        let contract = output
            .dependency_contracts
            .get(package_version)
            .expect("provider output was checked against the read contracts");
        read_time_contracts.insert(package_version.clone(), contract.clone());
    }
    if read_time_contracts.len() != read_requests.len() {
        let package_version = read_requests
            .keys()
            .find(|package_version| !read_time_contracts.contains_key(*package_version))
            .expect("different sizes imply a missing read contract");
        return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
            Box::new(package_version.clone()),
        ));
    }

    let dependency_contracts = output
        .dependency_contracts
        .iter()
        .map(|(package_version, contract)| (package_version.clone(), contract.clone()))
        .collect::<HashMap<_, _>>();
    let mut entries = Vec::new();
    let mut failures = Vec::new();

    for package_version in &graph.dependencies {
        let Some(read_request) = read_requests.get(package_version).copied() else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        let Some(read_source) = output.sources_used.get(package_version) else {
            return Err(ResolverFinalSourcePlanError::ProviderOutputMismatch(
                Box::new(package_version.clone()),
            ));
        };
        let final_request = MvsContractRequest {
            package_version: package_version.clone(),
            named_by: output
                .selection
                .named_by()
                .get(package_version)
                .cloned()
                .unwrap_or_default(),
            lock_floor: output
                .selection
                .reached_floors()
                .get(&package_version.node())
                == Some(package_version.version()),
        };
        match select_resolver_source(
            &final_request,
            &output.root_contract,
            root_context,
            &dependency_contracts,
            locked_sources,
        ) {
            Ok(final_source) => entries.push(ResolverFinalSourcePlanEntry {
                package_version: package_version.clone(),
                read_request: read_request.clone(),
                final_request,
                read_source: read_source.clone(),
                final_source,
            }),
            Err(error) => failures.push(ResolverFinalSourcePlanFailure {
                package_version: package_version.clone(),
                read_request: read_request.clone(),
                read_source: read_source.clone(),
                error,
            }),
        }
    }

    Ok(ResolverFinalSourcePlan {
        graph,
        root_cycles,
        entries,
        failures,
    })
}

/// Resolves final source identities and plans conditional reloads.
///
/// `read_source_identities` must contain the identities captured when each
/// source was actually read, including the root and any unselected packages
/// whose contracts contributed source clauses. The callback resolves only
/// the final selection. For a Git source its `base_identity` is `None`; for
/// a path source relative to a selected dependency, it is that dependency's
/// final identity. For a path source relative to the root or an unselected
/// package, it is the captured read-time identity of `relative_to`.
///
/// The caller owns source identity resolution because it depends on the host
/// filesystem and Git backend. The returned identity must be complete: local
/// paths should include lexical normalization and symlink resolution; Git
/// sources should include the canonical repository URL, resolved commit, and
/// normalized directory; a Git-relative path should include the requiring
/// package's source identity and normalized tree path. If a path is relative
/// to another selected dependency, that dependency's final identity is
/// resolved first. A failed or cyclic selected base is an error, never a
/// reason to fall back to its read-time identity. Both the captured read
/// identity and final identity are checked for every entry so independent
/// missing-identity and canonicalization errors are retained. Final-source
/// selection failures from the input plan are included in the returned
/// failures so a caller receives a complete audit result.
pub fn plan_resolver_source_reloads<K, E>(
    plan: &ResolverFinalSourcePlan,
    read_source_identities: &BTreeMap<MvsPackageVersion, K>,
    mut resolve_final_identity: impl FnMut(
        &MvsPackageVersion,
        &ResolverSourceSelection,
        Option<&K>,
    ) -> Result<K, E>,
) -> ResolverSourceIdentityPlan<K, E>
where
    K: Clone + Eq,
{
    let mut same_identity = Vec::new();
    let mut reloads = Vec::new();
    let mut failures = Vec::new();

    let entries_by_version = plan
        .entries
        .iter()
        .map(|entry| (entry.package_version.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let selected_versions = plan
        .graph
        .dependencies
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut dependents = BTreeMap::<MvsPackageVersion, Vec<MvsPackageVersion>>::new();
    let mut base_dependencies = BTreeMap::<MvsPackageVersion, MvsPackageVersion>::new();
    let mut dependency_counts = BTreeMap::new();
    let mut ready = BTreeSet::new();
    let mut final_identities = BTreeMap::new();
    let mut final_identity_failures = BTreeMap::new();

    for entry in &plan.entries {
        dependency_counts.insert(entry.package_version.clone(), 0);
        match entry.final_source.source() {
            ResolverPackageSource::Git { .. } => {
                ready.insert(entry.package_version.clone());
            }
            ResolverPackageSource::Path { relative_to, .. } => {
                if selected_versions.contains(relative_to) {
                    if entries_by_version.contains_key(relative_to) {
                        base_dependencies
                            .insert(entry.package_version.clone(), relative_to.clone());
                        dependents
                            .entry(relative_to.clone())
                            .or_default()
                            .push(entry.package_version.clone());
                        dependency_counts.insert(entry.package_version.clone(), 1);
                    } else {
                        final_identity_failures.insert(
                            entry.package_version.clone(),
                            ResolverSourceIdentityFailureKind::FinalBaseSourceIdentityUnavailable(
                                Box::new(relative_to.clone()),
                            ),
                        );
                        ready.insert(entry.package_version.clone());
                    }
                } else if !read_source_identities.contains_key(relative_to) {
                    final_identity_failures.insert(
                        entry.package_version.clone(),
                        ResolverSourceIdentityFailureKind::MissingBaseSourceIdentity(Box::new(
                            relative_to.clone(),
                        )),
                    );
                    ready.insert(entry.package_version.clone());
                } else {
                    ready.insert(entry.package_version.clone());
                }
            }
        }
    }

    while let Some(package_version) = ready.pop_first() {
        let entry = entries_by_version
            .get(&package_version)
            .expect("every ready identity has a source-plan entry");
        if !final_identity_failures.contains_key(&package_version) {
            let result = if entry.final_source.package_version() != &entry.package_version {
                Err(ResolverSourceIdentityFailureKind::PackageVersionMismatch {
                    expected: Box::new(entry.package_version.clone()),
                    found: Box::new(entry.final_source.package_version().clone()),
                })
            } else {
                match entry.final_source.source() {
                    ResolverPackageSource::Git { .. } => {
                        resolve_final_identity(&entry.package_version, &entry.final_source, None)
                            .map_err(|error| {
                                ResolverSourceIdentityFailureKind::Canonicalization(Box::new(error))
                            })
                    }
                    ResolverPackageSource::Path { relative_to, .. } => {
                        let base_identity = if selected_versions.contains(relative_to) {
                            final_identities.get(relative_to)
                        } else {
                            read_source_identities.get(relative_to)
                        };
                        match base_identity {
                            Some(identity) => resolve_final_identity(
                                &entry.package_version,
                                &entry.final_source,
                                Some(identity),
                            )
                            .map_err(|error| {
                                ResolverSourceIdentityFailureKind::Canonicalization(Box::new(
                                    error,
                                ))
                            }),
                            None if selected_versions.contains(relative_to) => Err(
                                ResolverSourceIdentityFailureKind::FinalBaseSourceIdentityUnavailable(
                                    Box::new(relative_to.clone()),
                                ),
                            ),
                            None => Err(
                                ResolverSourceIdentityFailureKind::MissingBaseSourceIdentity(
                                    Box::new(relative_to.clone()),
                                ),
                            ),
                        }
                    }
                }
            };
            match result {
                Ok(identity) => {
                    final_identities.insert(package_version.clone(), identity);
                }
                Err(error) => {
                    final_identity_failures.insert(package_version.clone(), error);
                }
            }
        }

        if let Some(dependent_versions) = dependents.get(&package_version) {
            for dependent_version in dependent_versions {
                let count = dependency_counts
                    .get_mut(dependent_version)
                    .expect("every selected path dependent has a count");
                *count -= 1;
                if *count == 0 {
                    ready.insert(dependent_version.clone());
                }
            }
        }
    }

    for (package_version, dependency_count) in dependency_counts {
        if dependency_count > 0 {
            let relative_to = base_dependencies
                .get(&package_version)
                .expect("every unresolved identity depends on a selected base");
            final_identity_failures.insert(
                package_version,
                ResolverSourceIdentityFailureKind::FinalBaseSourceIdentityUnavailable(Box::new(
                    relative_to.clone(),
                )),
            );
        }
    }

    let selection_failures = plan
        .failures
        .iter()
        .map(|failure| (failure.package_version.clone(), failure))
        .collect::<BTreeMap<_, _>>();
    for package_version in &plan.graph.dependencies {
        if let Some(selection_failure) = selection_failures.get(package_version) {
            let read_source = &selection_failure.read_source;
            if read_source.package_version() != package_version {
                failures.push(ResolverSourceIdentityFailure {
                    package_version: package_version.clone(),
                    side: ResolverSourceIdentitySide::ReadTime,
                    source: Some(read_source.clone()),
                    error: ResolverSourceIdentityFailureKind::PackageVersionMismatch {
                        expected: Box::new(package_version.clone()),
                        found: Box::new(read_source.package_version().clone()),
                    },
                });
            } else if !read_source_identities.contains_key(package_version) {
                failures.push(ResolverSourceIdentityFailure {
                    package_version: package_version.clone(),
                    side: ResolverSourceIdentitySide::ReadTime,
                    source: Some(read_source.clone()),
                    error: ResolverSourceIdentityFailureKind::MissingReadTimeIdentity(Box::new(
                        package_version.clone(),
                    )),
                });
            }
            failures.push(ResolverSourceIdentityFailure {
                package_version: package_version.clone(),
                side: ResolverSourceIdentitySide::Final,
                source: None,
                error: ResolverSourceIdentityFailureKind::FinalSourceSelection(Box::new(
                    selection_failure.error.clone(),
                )),
            });
            continue;
        }

        let entry = entries_by_version
            .get(package_version)
            .expect("every selected node has a source entry or a source-selection failure");
        let read_identity = if entry.read_source.package_version() != package_version {
            failures.push(ResolverSourceIdentityFailure {
                package_version: package_version.clone(),
                side: ResolverSourceIdentitySide::ReadTime,
                source: Some(entry.read_source.clone()),
                error: ResolverSourceIdentityFailureKind::PackageVersionMismatch {
                    expected: Box::new(package_version.clone()),
                    found: Box::new(entry.read_source.package_version().clone()),
                },
            });
            None
        } else if let Some(identity) = read_source_identities.get(package_version) {
            Some(identity.clone())
        } else {
            failures.push(ResolverSourceIdentityFailure {
                package_version: package_version.clone(),
                side: ResolverSourceIdentitySide::ReadTime,
                source: Some(entry.read_source.clone()),
                error: ResolverSourceIdentityFailureKind::MissingReadTimeIdentity(Box::new(
                    package_version.clone(),
                )),
            });
            None
        };
        let final_identity = if let Some(identity) = final_identities.remove(package_version) {
            Some(identity)
        } else {
            if let Some(error) = final_identity_failures.remove(package_version) {
                failures.push(ResolverSourceIdentityFailure {
                    package_version: package_version.clone(),
                    side: ResolverSourceIdentitySide::Final,
                    source: Some(entry.final_source.clone()),
                    error,
                });
            }
            None
        };

        let (Some(read_identity), Some(final_identity)) = (read_identity, final_identity) else {
            continue;
        };
        if read_identity == final_identity {
            same_identity.push(ResolverSameSourceIdentity {
                package_version: package_version.clone(),
                read_source: entry.read_source.clone(),
                final_source: entry.final_source.clone(),
                identity: read_identity,
            });
        } else {
            reloads.push(ResolverSourceReload {
                package_version: package_version.clone(),
                read_source: entry.read_source.clone(),
                final_source: entry.final_source.clone(),
                read_identity,
                final_identity,
            });
        }
    }

    ResolverSourceIdentityPlan {
        same_identity,
        reloads,
        failures,
    }
}

fn collect_final_path_pin_failures<K, C>(
    selection: &MvsSelectionOutput,
    root_contract: &ResolverContract,
    root_context: &ResolverContractContext<'_>,
    dependency_contracts: &HashMap<MvsPackageVersion, ResolverContract>,
    pin_cache: &mut ResolverPathPinCanonicalizationCache<K>,
    canonicalize_path_pin: &mut impl FnMut(&ResolverPathPinSource) -> Result<K, C>,
) -> Vec<ResolverPathPinProviderFailure<K, C>>
where
    K: Eq + Clone,
{
    let mut path_pins_by_node = BTreeMap::<MvsNode, Vec<ResolverPathPinSource>>::new();
    let mut failures = Vec::new();
    for (package_version, named_by) in selection.named_by() {
        for edge in named_by.iter().filter(|edge| edge.path_pin) {
            let node = package_version.node();
            match source_for_edge(
                edge,
                package_version,
                root_contract,
                root_context,
                dependency_contracts,
            ) {
                Ok(source) => {
                    path_pins_by_node
                        .entry(node)
                        .or_default()
                        .push(ResolverPathPinSource {
                            source,
                            requirer: edge.requirer.clone(),
                            span: edge.span.clone(),
                        });
                }
                Err(error) => failures.push(ResolverPathPinProviderFailure {
                    node,
                    error: ResolverPathPinProviderFailureKind::SourceSelection(error),
                }),
            }
        }
    }
    for (node, path_pin_sources) in path_pins_by_node {
        let mut canonical_sources = Vec::new();
        for pin in path_pin_sources {
            if !matches!(pin.source, ResolverPackageSource::Path { .. }) {
                failures.push(ResolverPathPinProviderFailure {
                    node: node.clone(),
                    error: ResolverPathPinProviderFailureKind::SourceSelection(
                        ResolverSourceSelectionError::PathPinWithoutPathSource {
                            requirer: pin.requirer.package_version().clone(),
                            package: node.package().into(),
                            span: pin.span.clone(),
                        },
                    ),
                });
                continue;
            }
            if pin_cache.failed_pins.contains(&pin) {
                continue;
            }
            match canonicalize_path_pin_with_cache(&pin, pin_cache, canonicalize_path_pin) {
                Ok(key) => canonical_sources.push((pin, key)),
                Err(error) => failures.push(ResolverPathPinProviderFailure {
                    node: node.clone(),
                    error: ResolverPathPinProviderFailureKind::Validation(Box::new(
                        ResolverPathPinValidationError::Canonicalization {
                            pin: Box::new(pin),
                            error: Box::new(error),
                        },
                    )),
                }),
            }
        }
        if canonical_sources.is_empty() {
            continue;
        }
        let unreported_conflict = (0..canonical_sources.len()).find_map(|first_index| {
            ((first_index + 1)..canonical_sources.len()).find_map(|conflicting_index| {
                let (first, first_key) = &canonical_sources[first_index];
                let (conflicting, conflicting_key) = &canonical_sources[conflicting_index];
                (first_key != conflicting_key
                    && !pin_cache.reported_conflicts.iter().any(
                        |(reported_first, reported_conflicting)| {
                            (reported_first == first && reported_conflicting == conflicting)
                                || (reported_first == conflicting && reported_conflicting == first)
                        },
                    ))
                .then_some((first_index, conflicting_index))
            })
        });
        if let Some((first_index, conflicting_index)) = unreported_conflict {
            let (first, first_key) = canonical_sources.remove(first_index);
            let (conflicting, conflicting_key) = canonical_sources.remove(conflicting_index - 1);
            failures.push(ResolverPathPinProviderFailure {
                node,
                error: ResolverPathPinProviderFailureKind::Validation(Box::new(
                    ResolverPathPinValidationError::ConflictingSources {
                        first: Box::new(first),
                        conflicting: Box::new(conflicting),
                        first_key: Box::new(first_key),
                        conflicting_key: Box::new(conflicting_key),
                    },
                )),
            });
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        ResolverPackageLoadError, ResolverPathPinProviderFailureKind, ResolverSourceIdentitySide,
        ResolverSourceProviderError, plan_resolver_final_sources, plan_resolver_source_reloads,
        select_with_source_provider,
    };
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::mvs::{MvsContractRequest, MvsNamedEdge, MvsPackageVersion, MvsRequirer};
    use crate::resolver_contract::{
        ResolverContract, ResolverContractContext, ResolverPathKey, build_resolver_contract,
    };
    use crate::resolver_source::{
        ResolverPackageSource, ResolverPathPinSource, ResolverPathPinValidationError,
        ResolverSourceReason, ResolverSourceSelectionError, select_resolver_source,
    };
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    fn package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, Version::parse(version).expect("valid version"))
            .expect("valid package version")
    }

    #[test]
    fn selects_sources_at_dequeue_and_retains_loaded_contract_metadata() {
        let root = parse(
            br#"package zz:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:alpha @ ^1 from git "https://example.test/alpha"
    acme:dep @ ^1 from git "https://example.test/root-dep"
}
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut loaded = Vec::new();

        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, source| {
                loaded.push((request.package_version.clone(), source.clone()));
                let manifest = match request.package_version.package() {
                    "acme:alpha" => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/alpha-dep" }
"#,
                        PackageRole::Dependency,
                    ),
                    "acme:dep" => parse_for_role(
                        br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load");

        assert_eq!(
            loaded
                .iter()
                .map(|(version, _)| version.package())
                .collect::<Vec<_>>(),
            ["acme:alpha", "acme:dep"]
        );
        let dep = package_version("acme:dep", "1.0.0");
        assert_eq!(
            output.sources_used[&dep].reason(),
            ResolverSourceReason::NamedRequirement
        );
        assert_eq!(
            output.sources_used[&dep].source(),
            &ResolverPackageSource::Git {
                url: "https://example.test/alpha-dep".into(),
                directory: None,
            }
        );
        assert_eq!(output.dependency_contracts.len(), 2);
        assert!(output.selection.is_complete());
    }

    #[test]
    fn rejects_path_pin_resolution_errors_before_loading_a_contract() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let pin_span = root
            .file()
            .declarations
            .iter()
            .find_map(|declaration| match &declaration.value {
                crate::manifest::PackageDeclaration::Requires { entries, .. } => {
                    entries.first().map(|entry| entry.span.clone())
                }
                _ => None,
            })
            .expect("root path requirement exists");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version.clone(), "../dep"),
            package_version("acme:dep", "1.5.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut loader_called = false;

        let result = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Err::<&str, _>("path leaves repository"),
            |_, _| -> Result<ResolverContract, ()> {
                loader_called = true;
                panic!("the loader must not run when path-pin validation fails")
            },
        );

        assert!(!loader_called);
        let Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            output: partial_selection,
        }) = result
        else {
            panic!("path-pin validation failure should be retained as a provider failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(path_pin_failures.is_empty());
        assert_eq!(
            failures[0].error,
            ResolverPackageLoadError::PathPinValidation(Box::new(
                ResolverPathPinValidationError::Canonicalization {
                    pin: Box::new(ResolverPathPinSource {
                        source: ResolverPackageSource::Path {
                            relative_to: root_version.clone(),
                            path: "../dep".into(),
                        },
                        requirer: MvsRequirer::Root(root_version),
                        span: pin_span,
                    }),
                    error: Box::new("path leaves repository"),
                }
            ))
        );
        assert!(!partial_selection.selection.is_complete());
        assert!(partial_selection.dependency_contracts.is_empty());
    }

    #[test]
    fn reports_path_pin_metadata_failures_only_once() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let dep_version = package_version("acme:dep", "1.5.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version.clone(), "../dep"),
            dep_version,
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let mut root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let pin_span = root_contract.requirements[0].span.clone();
        root_contract.requirements[0].source = None;

        let result = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |_, _| -> Result<ResolverContract, ()> {
                panic!("a path pin with inconsistent metadata must not be loaded")
            },
        );

        let Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            output: _,
        }) = result
        else {
            panic!("invalid path-pin metadata should fail source-aware resolution");
        };
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].error,
            ResolverPackageLoadError::SourceSelection(
                ResolverSourceSelectionError::MissingRequirementSource {
                    requirer: root_version,
                    package: "acme:dep".into(),
                    span: pin_span,
                }
            )
        );
        assert!(path_pin_failures.is_empty());
    }

    #[test]
    fn reports_a_late_pin_error_after_an_earlier_pin_failed_canonicalization() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:alpha @ ^1 from git "https://example.test/alpha"
    zz:beta @ ^1 from git "https://example.test/beta"
}
"#,
        );
        let alpha_version = package_version("acme:alpha", "1.0.0");
        let beta_version = package_version("zz:beta", "1.0.0");
        let dep_version = package_version("acme:dep", "1.5.0");
        let path_packages = BTreeMap::from([
            (
                ResolverPathKey::new(alpha_version, "../dep-a"),
                dep_version.clone(),
            ),
            (
                ResolverPathKey::new(beta_version, "../dep-b"),
                dep_version.clone(),
            ),
        ]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut canonicalized_paths = Vec::new();
        let mut loaded_packages = Vec::new();

        let result = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |pin| {
                let ResolverPackageSource::Path { path, .. } = &pin.source else {
                    panic!("test fixture contains only path pins")
                };
                canonicalized_paths.push(path.to_string());
                Err::<&str, _>(if path.as_ref() == "../dep-a" {
                    "first pin could not be canonicalized"
                } else {
                    "late pin escapes repository"
                })
            },
            |request, _source| {
                let package = request.package_version.package();
                loaded_packages.push(package.to_owned());
                let manifest = match package {
                    "acme:alpha" => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep-a" }
"#,
                        PackageRole::Dependency,
                    ),
                    "zz:beta" => parse_for_role(
                        br#"package zz:beta @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep-b" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        );

        assert_eq!(loaded_packages, ["acme:alpha", "zz:beta"]);
        assert_eq!(
            canonicalized_paths
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["../dep-a", "../dep-b"]
        );
        let Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            ..
        }) = result
        else {
            panic!("path canonicalization failures should reject resolution");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0].error,
            ResolverPackageLoadError::PathPinValidation(error)
                if matches!(
                    &**error,
                    ResolverPathPinValidationError::Canonicalization { pin, .. }
                        if matches!(
                            &pin.source,
                            ResolverPackageSource::Path { path, .. }
                                if path.as_ref() == "../dep-a"
                        )
                )
        ));
        assert_eq!(path_pin_failures.len(), 1);
        assert_eq!(path_pin_failures[0].node, dep_version.node());
        assert!(matches!(
            &path_pin_failures[0].error,
            ResolverPathPinProviderFailureKind::Validation(error)
                if matches!(
                    &**error,
                    ResolverPathPinValidationError::Canonicalization { pin, error }
                        if matches!(
                            &pin.source,
                            ResolverPackageSource::Path { path, .. }
                                if path.as_ref() == "../dep-b"
                        ) && **error == "late pin escapes repository"
                )
        ));
    }

    #[test]
    fn catches_path_pins_discovered_after_the_target_contract_was_read() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:alpha @ ^1 from git "https://example.test/alpha"
    zz:beta @ ^1 from git "https://example.test/beta"
}
"#,
        );
        let alpha_version = package_version("acme:alpha", "1.0.0");
        let beta_version = package_version("zz:beta", "1.0.0");
        let dep_version = package_version("acme:dep", "1.5.0");
        let path_packages = BTreeMap::from([
            (
                ResolverPathKey::new(alpha_version.clone(), "../dep-a"),
                dep_version.clone(),
            ),
            (
                ResolverPathKey::new(beta_version, "../dep-b"),
                dep_version.clone(),
            ),
        ]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut loaded_packages = Vec::new();

        let result = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |pin| match &pin.source {
                ResolverPackageSource::Path { path, .. } if path.as_ref() == "../dep-a" => {
                    Ok::<_, ()>("canonical-dep-a")
                }
                ResolverPackageSource::Path { path, .. } if path.as_ref() == "../dep-b" => {
                    Ok::<_, ()>("canonical-dep-b")
                }
                source => panic!("unexpected pin source: {source:?}"),
            },
            |request, _source| {
                let package = request.package_version.package();
                loaded_packages.push(package.to_owned());
                let manifest = match package {
                    "acme:alpha" => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep-a" }
"#,
                        PackageRole::Dependency,
                    ),
                    "acme:dep" => parse_for_role(
                        br#"package acme:dep @ 1.5.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    "zz:beta" => parse_for_role(
                        br#"package zz:beta @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep-b" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        );

        assert_eq!(loaded_packages, ["acme:alpha", "acme:dep", "zz:beta"]);
        let Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            output: partial_selection,
        }) = result
        else {
            panic!("late path-pin disagreement must fail source-aware resolution");
        };
        assert!(failures.is_empty());
        assert_eq!(path_pin_failures.len(), 1);
        assert_eq!(path_pin_failures[0].node, dep_version.node());
        let ResolverPathPinProviderFailureKind::Validation(error) = &path_pin_failures[0].error
        else {
            panic!("both path source candidates should map to validated requirements");
        };
        let ResolverPathPinValidationError::ConflictingSources {
            first,
            conflicting,
            first_key,
            conflicting_key,
        } = &**error
        else {
            panic!("the late path pin should conflict with the first pin");
        };
        assert!(matches!(
            &first.source,
            ResolverPackageSource::Path { path, relative_to }
                if path.as_ref() == "../dep-a" && relative_to == &alpha_version
        ));
        assert!(matches!(
            &conflicting.source,
            ResolverPackageSource::Path { path, .. } if path.as_ref() == "../dep-b"
        ));
        assert_eq!(**first_key, "canonical-dep-a");
        assert_eq!(**conflicting_key, "canonical-dep-b");
        assert_eq!(
            partial_selection.selection.selected()[&dep_version.node()],
            *dep_version.version()
        );
    }

    #[test]
    fn provider_failures_keep_independent_contracts_and_source_choices() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:a @ ^1 from git "https://example.test/a"
    acme:b @ ^1 from git "https://example.test/b"
}
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");

        let result = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                if request.package_version.package() == "acme:a" {
                    return Err("offline");
                }
                let manifest = parse_for_role(
                    br#"package acme:b @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                    PackageRole::Dependency,
                );
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        );

        let Err(ResolverSourceProviderError::Resolution {
            failures,
            path_pin_failures,
            output: partial_selection,
        }) = result
        else {
            panic!("one failed branch should preserve a partial selection");
        };
        assert_eq!(failures.len(), 1);
        assert!(path_pin_failures.is_empty());
        assert_eq!(
            failures[0].request.package_version,
            package_version("acme:a", "1.0.0")
        );
        assert_eq!(
            failures[0].error,
            ResolverPackageLoadError::Provider(Box::new("offline"))
        );
        assert!(
            partial_selection
                .selection
                .read_contracts()
                .contains_key(&package_version("acme:b", "1.0.0"))
        );
        assert!(
            partial_selection
                .sources_used
                .contains_key(&package_version("acme:b", "1.0.0"))
        );
        assert!(
            !partial_selection
                .sources_used
                .contains_key(&package_version("acme:a", "1.0.0"))
        );
    }

    #[test]
    fn final_source_plan_retains_a_late_edge_that_changes_the_winner() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from git "https://example.test/root-dep"
    z:late @ ^1 from git "https://example.test/late"
}
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                let manifest = match request.package_version.package() {
                    "acme:dep" => parse_for_role(
                        br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    "z:late" => parse_for_role(
                        br#"package z:late @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/late-dep" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load");

        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("final source plan is consistent");
        let dep_version = package_version("acme:dep", "1.0.0");
        assert_eq!(plan.graph.dependencies.len(), 2);
        assert_eq!(plan.entries.len(), 2);
        let dep = plan
            .entries
            .iter()
            .find(|entry| entry.package_version == dep_version)
            .expect("selected dependency has a plan entry");
        assert_eq!(dep.read_request.named_by.len(), 1);
        assert_eq!(dep.final_request.named_by.len(), 2);
        assert_eq!(
            dep.read_source.source(),
            &ResolverPackageSource::Git {
                url: "https://example.test/root-dep".into(),
                directory: None,
            }
        );
        assert_eq!(
            dep.final_source.source(),
            &ResolverPackageSource::Git {
                url: "https://example.test/late-dep".into(),
                directory: None,
            }
        );
        assert_eq!(
            dep.final_source.reason(),
            ResolverSourceReason::NamedRequirement
        );
        assert!(plan.failures.is_empty());

        let read_source_identities = BTreeMap::from([
            (dep_version.clone(), "canonical-dep-tree"),
            (package_version("z:late", "1.0.0"), "late-package-tree"),
        ]);
        let same_identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |package_version, _source, _base| {
                Ok::<_, ()>(if package_version.package() == "acme:dep" {
                    "canonical-dep-tree"
                } else {
                    "late-package-tree"
                })
            },
        );
        assert_eq!(same_identity_plan.same_identity.len(), 2);
        assert!(same_identity_plan.reloads.is_empty());
        assert!(same_identity_plan.failures.is_empty());

        let changed_identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |package_version, source, _base| {
                if package_version.package() == "z:late" {
                    return Err("source identity is unavailable");
                }
                let ResolverPackageSource::Git { url, .. } = source.source() else {
                    panic!("test sources are Git sources")
                };
                Ok(if url.as_ref() == "https://example.test/root-dep" {
                    "root-dep-tree"
                } else {
                    "late-dep-tree"
                })
            },
        );
        assert_eq!(changed_identity_plan.same_identity.len(), 0);
        assert_eq!(changed_identity_plan.reloads.len(), 1);
        assert_eq!(
            changed_identity_plan.reloads[0].package_version,
            dep_version
        );
        assert_eq!(changed_identity_plan.failures.len(), 1);
        assert_eq!(
            changed_identity_plan.failures[0].side,
            ResolverSourceIdentitySide::Final
        );
        let missing_read_identities =
            plan_resolver_source_reloads(&plan, &BTreeMap::new(), |_, _, _| {
                Ok::<_, ()>("final-tree")
            });
        assert!(missing_read_identities.reloads.is_empty());
        assert_eq!(missing_read_identities.failures.len(), 2);
        assert!(
            missing_read_identities
                .failures
                .iter()
                .all(|failure| failure.side == ResolverSourceIdentitySide::ReadTime)
        );

        let alternative_root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/other-dep" }
"#,
        );
        let alternative_context = ResolverContractContext::from_root_manifest(
            &alternative_root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("alternative root resolver context is valid");
        let alternative_contract = build_resolver_contract(
            &alternative_root,
            &alternative_context,
            PackageValidationContext::default(),
        )
        .expect("alternative root resolver contract is valid");
        let alternative_requirement = &alternative_contract.requirements()[0];
        let alternative_source = select_resolver_source(
            &MvsContractRequest {
                package_version: dep_version.clone(),
                named_by: vec![MvsNamedEdge {
                    requirer: MvsRequirer::Root(alternative_contract.mvs.package_version().clone()),
                    span: alternative_requirement.span.clone(),
                    path_pin: false,
                    root_override_pin: false,
                }],
                lock_floor: false,
            },
            &alternative_contract,
            &alternative_context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("alternative root names a valid source for the same version");
        output
            .sources_used
            .insert(dep_version.clone(), alternative_source);
        assert!(matches!(
            plan_resolver_final_sources(&output, &context, &HashMap::new()),
            Err(super::ResolverFinalSourcePlanError::ProviderOutputMismatch(version))
                if *version == dep_version
        ));
    }

    #[test]
    fn final_path_identity_uses_the_captured_source_of_its_requirer() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let dep_version = package_version("acme:dep", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version.clone(), "../dep"),
            dep_version.clone(),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("canonical-dependency-path"),
            |_, _| {
                let manifest = parse_for_role(
                    br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                    PackageRole::Dependency,
                );
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load");
        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("final source plan is consistent");
        let read_source_identities = BTreeMap::from([
            (root_version.clone(), "root-read-tree"),
            (dep_version.clone(), "dependency-read-tree"),
        ]);

        let identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |package_version, source, base_identity| {
                assert_eq!(package_version, &dep_version);
                let ResolverPackageSource::Path { relative_to, path } = source.source() else {
                    panic!("fixture's final source must be a path")
                };
                assert_eq!(relative_to, &root_version);
                assert_eq!(path.as_ref(), "../dep");
                assert_eq!(base_identity, Some(&"root-read-tree"));
                Ok::<_, ()>("dependency-from-final-path")
            },
        );
        assert!(identity_plan.same_identity.is_empty());
        assert_eq!(identity_plan.reloads.len(), 1);
        assert_eq!(
            identity_plan.reloads[0].read_identity,
            "dependency-read-tree"
        );
        assert_eq!(
            identity_plan.reloads[0].final_identity,
            "dependency-from-final-path"
        );
        assert!(identity_plan.failures.is_empty());

        let missing_base_plan = plan_resolver_source_reloads(
            &plan,
            &BTreeMap::from([(dep_version.clone(), "dependency-read-tree")]),
            |_, _, _| -> Result<&str, ()> {
                panic!("a path with no captured base identity must not be resolved")
            },
        );
        assert!(missing_base_plan.reloads.is_empty());
        assert_eq!(missing_base_plan.failures.len(), 1);
        assert_eq!(
            missing_base_plan.failures[0].side,
            ResolverSourceIdentitySide::Final
        );
        assert!(matches!(
            &missing_base_plan.failures[0].error,
            super::ResolverSourceIdentityFailureKind::MissingBaseSourceIdentity(package)
                if **package == root_version
        ));
    }

    #[test]
    fn selected_path_base_uses_its_final_identity_before_resolving_dependents() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    zz:base @ ^1 from git "https://example.test/root-base"
    zz:late @ ^1 from git "https://example.test/late"
}
"#,
        );
        let root_version = package_version("zz:root", "1.0.0");
        let base_version = package_version("zz:base", "1.0.0");
        let dep_version = package_version("acme:dep", "1.0.0");
        let late_version = package_version("zz:late", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(base_version.clone(), "../dep"),
            dep_version.clone(),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("canonical-dependency-path"),
            |request, _| {
                let manifest = match request.package_version.package() {
                    "zz:base" => parse_for_role(
                        br#"package zz:base @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
                        PackageRole::Dependency,
                    ),
                    "acme:dep" => parse_for_role(
                        br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    "zz:late" => parse_for_role(
                        br#"package zz:late @ 1.0.0
source "src"
targets { wasm32-core }
requires { zz:base @ ^1 from git "https://example.test/late-base" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load");
        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("final source plan is consistent");
        let read_source_identities = BTreeMap::from([
            (root_version, "root-tree"),
            (base_version.clone(), "base-read"),
            (dep_version.clone(), "dep-read"),
            (late_version, "late-read"),
        ]);
        let mut final_path_base = None;

        let identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |package_version, source, base_identity| match package_version.package() {
                "acme:dep" => {
                    assert_eq!(base_identity, Some(&"base-final"));
                    final_path_base = base_identity.copied().map(str::to_owned);
                    Ok::<_, ()>("dep-final")
                }
                "zz:base" => {
                    assert_eq!(base_identity, None);
                    assert_eq!(
                        source.source(),
                        &ResolverPackageSource::Git {
                            url: "https://example.test/late-base".into(),
                            directory: None,
                        }
                    );
                    Ok("base-final")
                }
                "zz:late" => Ok("late-read"),
                package => panic!("unexpected selected package {package}"),
            },
        );
        assert_eq!(final_path_base.as_deref(), Some("base-final"));
        assert_eq!(identity_plan.reloads.len(), 2);
        assert_eq!(identity_plan.reloads[0].package_version, dep_version);
        assert_eq!(identity_plan.reloads[0].read_identity, "dep-read");
        assert_eq!(identity_plan.reloads[0].final_identity, "dep-final");
        assert_eq!(identity_plan.reloads[1].package_version, base_version);
        assert_eq!(identity_plan.reloads[1].read_identity, "base-read");
        assert_eq!(identity_plan.reloads[1].final_identity, "base-final");
        assert_eq!(identity_plan.same_identity.len(), 1);
        assert!(identity_plan.failures.is_empty());
    }

    #[test]
    fn cyclic_selected_path_bases_fail_without_read_time_fallback() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:a @ ^1 from git "https://example.test/a" }
"#,
        );
        let root_version = package_version("zz:root", "1.0.0");
        let a_version = package_version("acme:a", "1.0.0");
        let b_version = package_version("acme:b", "1.0.0");
        let path_packages = BTreeMap::from([
            (
                ResolverPathKey::new(a_version.clone(), "../b"),
                b_version.clone(),
            ),
            (
                ResolverPathKey::new(b_version.clone(), "../a"),
                a_version.clone(),
            ),
        ]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("canonical-path"),
            |request, _| {
                let manifest = match request.package_version.package() {
                    "acme:a" => parse_for_role(
                        br#"package acme:a @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:b @ ^1 from path "../b" }
"#,
                        PackageRole::Dependency,
                    ),
                    "acme:b" => parse_for_role(
                        br#"package acme:b @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:a @ ^1 from path "../a" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("the path cycle is retained for graph validation");
        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("final source plan retains dependency cycles");
        let read_source_identities = BTreeMap::from([
            (root_version, "root-read"),
            (a_version.clone(), "a-read"),
            (b_version.clone(), "b-read"),
        ]);
        let identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |_, _, _| -> Result<&str, ()> {
                panic!("cyclic final path bases must not be resolved")
            },
        );

        assert!(identity_plan.same_identity.is_empty());
        assert!(identity_plan.reloads.is_empty());
        assert_eq!(identity_plan.failures.len(), 2);
        assert!(
            identity_plan
                .failures
                .iter()
                .all(|failure| failure.side == ResolverSourceIdentitySide::Final)
        );
        assert!(matches!(
            &identity_plan.failures[0].error,
            super::ResolverSourceIdentityFailureKind::FinalBaseSourceIdentityUnavailable(_)
        ));
        assert!(matches!(
            &identity_plan.failures[1].error,
            super::ResolverSourceIdentityFailureKind::FinalBaseSourceIdentityUnavailable(_)
        ));
    }

    #[test]
    fn final_source_plan_only_includes_selected_graph_versions() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:alpha @ ^1 from git "https://example.test/alpha"
    acme:dep @ ^1 from git "https://example.test/root-dep"
}
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                let manifest = match (
                    request.package_version.package(),
                    request.package_version.version().to_string().as_str(),
                ) {
                    ("acme:alpha", _) => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1.2 from git "https://example.test/new-dep" }
"#,
                        PackageRole::Dependency,
                    ),
                    ("acme:dep", "1.0.0") => parse_for_role(
                        br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:orphan @ ^1 from git "https://example.test/orphan" }
"#,
                        PackageRole::Dependency,
                    ),
                    ("acme:dep", "1.2.0") => parse_for_role(
                        br#"package acme:dep @ 1.2.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    ("acme:orphan", _) => parse_for_role(
                        br#"package acme:orphan @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    (package, _) => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS candidates load");

        assert!(
            output
                .dependency_contracts
                .contains_key(&package_version("acme:dep", "1.0.0"))
        );
        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("final source plan is consistent");
        let planned_versions = plan
            .entries
            .iter()
            .map(|entry| entry.package_version.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            planned_versions,
            [
                package_version("acme:alpha", "1.0.0"),
                package_version("acme:dep", "1.2.0"),
            ]
        );
        assert!(!planned_versions.contains(&package_version("acme:dep", "1.0.0")));
        assert!(!planned_versions.contains(&package_version("acme:orphan", "1.0.0")));
    }

    #[test]
    fn final_source_plan_keeps_independent_results_when_the_graph_has_a_cycle() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:alpha @ ^1 from git "https://example.test/alpha"
    acme:b @ ^1 from git "https://example.test/b"
    z:late @ ^1 from git "https://example.test/late"
}
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let mut output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                let manifest = match request.package_version.package() {
                    "acme:alpha" => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    "acme:b" => parse_for_role(
                        br#"package acme:b @ 1.0.0
source "src"
targets { wasm32-core }
requires { z:late @ ^1 from git "https://example.test/late-from-b" }
"#,
                        PackageRole::Dependency,
                    ),
                    "z:late" => parse_for_role(
                        br#"package z:late @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:b @ ^1 from git "https://example.test/b-from-late" }
"#,
                        PackageRole::Dependency,
                    ),
                    package => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load at their read-time source");

        let late = package_version("z:late", "1.0.0");
        output
            .dependency_contracts
            .get_mut(&late)
            .expect("late contract was loaded")
            .requirements
            .iter_mut()
            .find(|requirement| requirement.package.as_ref() == "acme:b")
            .expect("late edge metadata exists")
            .source = None;

        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("a selected-graph cycle does not suppress source planning");
        assert!(plan.graph.cycle.is_some());
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.failures.len(), 1);
        assert_eq!(
            plan.failures[0].package_version,
            package_version("acme:b", "1.0.0")
        );
        assert!(matches!(
            plan.failures[0].error,
            ResolverSourceSelectionError::MissingRequirementSource { .. }
        ));
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.package_version.package())
                .collect::<Vec<_>>(),
            ["acme:alpha", "z:late"]
        );

        let root_version = package_version("zz:root", "1.0.0");
        let alpha_version = package_version("acme:alpha", "1.0.0");
        let b_version = package_version("acme:b", "1.0.0");
        let late_version = package_version("z:late", "1.0.0");
        let read_source_identities = BTreeMap::from([
            (root_version, "root".to_owned()),
            (alpha_version, "acme:alpha".to_owned()),
            (b_version.clone(), "acme:b".to_owned()),
            (late_version, "z:late".to_owned()),
        ]);
        let identity_plan = plan_resolver_source_reloads(
            &plan,
            &read_source_identities,
            |package_version, _, base_identity| {
                assert_eq!(base_identity, None);
                Ok::<_, ()>(package_version.package().to_owned())
            },
        );
        assert_eq!(identity_plan.same_identity.len(), 2);
        assert!(identity_plan.reloads.is_empty());
        assert_eq!(identity_plan.failures.len(), 1);
        assert_eq!(identity_plan.failures[0].package_version, b_version);
        assert_eq!(
            identity_plan.failures[0].side,
            ResolverSourceIdentitySide::Final
        );
        assert!(identity_plan.failures[0].source.is_none());
        assert!(matches!(
            &identity_plan.failures[0].error,
            super::ResolverSourceIdentityFailureKind::FinalSourceSelection(error)
                if matches!(
                    &**error,
                    ResolverSourceSelectionError::MissingRequirementSource { .. }
                )
        ));
    }

    #[test]
    fn final_source_plan_retains_a_cycle_back_to_the_root() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:alpha @ ^1 from git "https://example.test/alpha" }
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                let manifest = parse_for_role(
                    br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { zz:root @ ^1 from git "https://example.test/root" }
"#,
                    PackageRole::Dependency,
                );
                assert_eq!(
                    request.package_version,
                    package_version("acme:alpha", "1.0.0")
                );
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("root cycle findings do not fail source loading");

        let plan = plan_resolver_final_sources(&output, &context, &HashMap::new())
            .expect("a resolved root cycle does not suppress independent source planning");
        assert_eq!(plan.root_cycles.len(), 1);
        assert!(plan.graph.cycle.is_none());
        assert_eq!(
            plan.graph.dependencies,
            [package_version("acme:alpha", "1.0.0")]
                .into_iter()
                .collect()
        );
        assert_eq!(plan.entries.len(), 1);
        assert!(plan.failures.is_empty());
    }

    #[test]
    fn final_source_plan_rejects_a_changed_lock_floor_source_map() {
        let root = parse(
            br#"package zz:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:alpha @ ^1 from git "https://example.test/alpha" }
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let floor = package_version("acme:b", "1.5.0");
        let lock_floors = HashMap::from([(floor.node(), floor.version().clone())]);
        let locked_sources = HashMap::from([(
            floor.clone(),
            ResolverPackageSource::Git {
                url: "https://example.test/locked-b".into(),
                directory: None,
            },
        )]);
        let output = select_with_source_provider(
            &root_contract,
            &context,
            &lock_floors,
            &locked_sources,
            |_| Ok::<_, ()>("unused"),
            |request, _source| {
                let manifest = match (
                    request.package_version.package(),
                    request.package_version.version().to_string().as_str(),
                ) {
                    ("acme:alpha", _) => parse_for_role(
                        br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:b @ ^1 from git "https://example.test/alpha-b" }
"#,
                        PackageRole::Dependency,
                    ),
                    ("acme:b", "1.0.0") => parse_for_role(
                        br#"package acme:b @ 1.0.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    ("acme:b", "1.5.0") => parse_for_role(
                        br#"package acme:b @ 1.5.0
source "src"
targets { wasm32-core }
"#,
                        PackageRole::Dependency,
                    ),
                    (package, _) => panic!("unexpected package request: {package}"),
                };
                build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                    .map_err(|_| "loaded manifest failed validation")
            },
        )
        .expect("all MVS contracts load at their read-time source");

        assert!(matches!(
            plan_resolver_final_sources(&output, &context, &HashMap::new()),
            Err(super::ResolverFinalSourcePlanError::ProviderOutputMismatch(version))
                if *version == floor
        ));
    }

    #[test]
    fn rejects_a_root_role_contract_in_the_dependency_catalog() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/dep" }
"#,
        );
        let path_packages = BTreeMap::<ResolverPathKey, MvsPackageVersion>::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root resolver context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        let root_version = root_contract.mvs.package_version().clone();
        let requirement = root_contract
            .requirements
            .iter()
            .find(|requirement| requirement.package.as_ref() == "acme:dep")
            .expect("root dependency metadata exists");
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![MvsNamedEdge {
                requirer: MvsRequirer::Package(root_version.clone()),
                span: requirement.span.clone(),
                path_pin: false,
                root_override_pin: false,
            }],
            lock_floor: false,
        };
        let dependencies = HashMap::from([(root_version.clone(), root_contract.clone())]);

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &context,
                &dependencies,
                &HashMap::new(),
            ),
            Err(
                crate::resolver_source::ResolverSourceSelectionError::RequirerContractRoleMismatch {
                requirer: root_version,
                expected: PackageRole::Dependency,
                found: PackageRole::Root,
            }
            )
        );
    }
}
