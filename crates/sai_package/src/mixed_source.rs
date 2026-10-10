//! Source-aware MVS loading for local and Git package sources.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::git_path_requirements::{
    GitPathRequirementDiscoveryError, GitPathRequirementIndex, discover_git_path_requirements,
};
use crate::git_remote::{
    FetchedGitPackage, FetchedGitRelease, GitPackagePathError, GitPackageSourceIdentity,
    GitRemoteReleaseContractError, GitRemoteReleaseFetchError, SelectedGitReleaseFetchError,
    fetch_git_package_source_tree_at_commit, fetch_locked_git_path_package_tree,
    fetch_selected_git_release, matching_locked_git_path_package,
};
use crate::git_source_index::{GitPackageSourceIdentityIndex, GitPackageSourceIdentityIndexError};
use crate::local_source::{
    LocalPackageContractLoadError, LocalPackageDirectory, LocalPackageDirectoryIndex,
    LocalPackageDirectoryIndexError, LocalPackageError, LocalPackagePathIndexError,
    LocalPackagePathPinError, LocalPackageSourceResolveError,
};
use crate::lock::LockPackage;
use crate::manifest::PackageRole;
use crate::mvs::{MvsContractRequest, MvsNode, MvsPackageVersion, MvsRequirer};
use crate::package_path::{DependencyPackagePath, PackagePathError};
use crate::resolver_contract::{
    ResolverContractBuildError, ResolverContractContext, ResolverContractPathSourceRequest,
    build_resolver_contract as adapt_resolver_contract,
};
use crate::resolver_source::{
    ResolverPackageSource, ResolverPathPinSource, ResolverSourceProvenance, ResolverSourceReason,
    ResolverSourceSelection,
};
use crate::resolver_source_provider::{
    ResolverFinalContractAudit, ResolverFinalSourcePlan, ResolverReloadedSourceContract,
    ResolverSourceIdentityPlan, ResolverSourceIdentitySnapshot, ResolverSourceProviderError,
    ResolverSourceProviderIdentifiedOutput, ResolverSourceProviderIdentityLoadError,
    audit_resolver_reloaded_contracts, plan_resolver_final_sources, plan_resolver_source_reloads,
    select_with_source_provider_and_canonicalized_pins_and_identities,
};
use crate::validation::PackageValidationContext;
use crate::version::Version;

/// The exact local directory or Git tree that supplied a package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MixedPackageSourceIdentity {
    Local(LocalPackageDirectory),
    Git(GitPackageSourceIdentity),
}

/// A path pin's requirer has not been loaded from a source, or its source
/// could not canonicalize the pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MixedSourcePathPinError {
    MissingRequirer(MvsRequirer),
    Local(LocalPackagePathPinError),
    Git(GitPackageSourceIdentityIndexError),
}

type MixedSourceProviderError = ResolverSourceProviderIdentityLoadError<
    MixedSourceMvsLoadError,
    MixedPackageSourceIdentity,
    MixedSourcePathPinError,
    MixedPackageSourceIdentity,
>;

/// A local or Git MVS candidate could not be loaded.
#[derive(Debug)]
pub enum MixedSourceMvsLoadError {
    UnsupportedSource {
        package_version: Box<MvsPackageVersion>,
        package_source: Box<ResolverPackageSource>,
    },
    LocalSourceResolve(LocalPackageSourceResolveError),
    LocalPathRequirements(Box<LocalPackagePathIndexError>),
    GitPathRequirements(Box<GitPathRequirementDiscoveryError<GitRemoteReleaseFetchError>>),
    GitPathPackageNotIndexed {
        requirer: MvsRequirer,
        path: Box<str>,
    },
    GitReleaseFetch(Box<SelectedGitReleaseFetchError>),
    GitPackageContract(Box<GitRemoteReleaseContractError>),
    LocalPackageContract(Box<LocalPackageContractLoadError>),
    ResolverContext(Box<ResolverContractBuildError>),
    GitSourceIdentityIndex(Box<GitPackageSourceIdentityIndexError>),
    RequirerIdentityConflict(MvsRequirer),
}

/// Failure while loading one or more mixed-source MVS candidates.
///
/// The partial resolver output, evolving context, and source indexes are
/// retained so callers can report successful branches and inspect failures.
#[derive(Debug)]
pub struct MixedSourceMvsProviderFailure {
    error: Box<MixedSourceProviderError>,
    context: ResolverContractContext<'static>,
    local_directories: LocalPackageDirectoryIndex,
    git_identities: GitPackageSourceIdentityIndex,
    git_path_requirements: BTreeMap<MvsRequirer, GitPathRequirementIndex>,
}

impl MixedSourceMvsProviderFailure {
    pub const fn error(
        &self,
    ) -> &ResolverSourceProviderError<
        MixedSourceMvsLoadError,
        MixedPackageSourceIdentity,
        MixedSourcePathPinError,
    > {
        self.error.error()
    }

    pub const fn context(&self) -> &ResolverContractContext<'static> {
        &self.context
    }

    pub const fn local_directories(&self) -> &LocalPackageDirectoryIndex {
        &self.local_directories
    }

    pub const fn git_identities(&self) -> &GitPackageSourceIdentityIndex {
        &self.git_identities
    }

    pub fn git_path_requirements(&self) -> &BTreeMap<MvsRequirer, GitPathRequirementIndex> {
        &self.git_path_requirements
    }

    pub const fn source_identities(
        &self,
    ) -> &ResolverSourceIdentitySnapshot<MixedPackageSourceIdentity> {
        self.error.source_identities()
    }
}

/// Failure to prepare a root contract or load a mixed-source MVS graph.
#[derive(Debug)]
pub enum MixedSourceMvsError {
    RootDiscovery(Box<LocalPackagePathIndexError>),
    RootContract(Box<ResolverContractBuildError>),
    Provider(Box<MixedSourceMvsProviderFailure>),
}

/// A source-aware MVS load that supports local and Git dependencies.
#[derive(Debug)]
pub struct MixedSourceMvsOutput {
    provider: ResolverSourceProviderIdentifiedOutput<MixedPackageSourceIdentity>,
    root_context: ResolverContractContext<'static>,
    context: ResolverContractContext<'static>,
    local_directories: LocalPackageDirectoryIndex,
    git_identities: GitPackageSourceIdentityIndex,
    git_path_requirements: BTreeMap<MvsRequirer, GitPathRequirementIndex>,
}

impl MixedSourceMvsOutput {
    pub const fn provider(
        &self,
    ) -> &ResolverSourceProviderIdentifiedOutput<MixedPackageSourceIdentity> {
        &self.provider
    }

    /// Root context extended with path packages found while loading candidates.
    pub const fn context(&self) -> &ResolverContractContext<'static> {
        &self.context
    }

    /// Root-only resolver context, before dependency path packages were added.
    pub const fn root_context(&self) -> &ResolverContractContext<'static> {
        &self.root_context
    }

    pub const fn local_directories(&self) -> &LocalPackageDirectoryIndex {
        &self.local_directories
    }

    pub const fn git_identities(&self) -> &GitPackageSourceIdentityIndex {
        &self.git_identities
    }

    pub fn git_path_requirements(&self) -> &BTreeMap<MvsRequirer, GitPathRequirementIndex> {
        &self.git_path_requirements
    }

    /// Plan final source choices after MVS range and cycle checking.
    pub fn plan_final_sources(
        &self,
        locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    ) -> Result<
        crate::resolver_source_provider::ResolverFinalSourcePlan,
        crate::resolver_source_provider::ResolverFinalSourcePlanError,
    > {
        plan_resolver_final_sources(self.provider.output(), &self.context, locked_sources)
    }

    /// Resolve final source identities, reload changed sources, and prepare
    /// the contract audit required after MVS range and cycle checks.
    pub fn plan_final_source_audit(
        &self,
        locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
        locked_packages: &HashMap<MvsPackageVersion, LockPackage>,
        validation_context: PackageValidationContext<'_>,
        mut fetch_git_release: impl FnMut(
            &ResolverSourceSelection,
            Option<&LockPackage>,
        )
            -> Result<FetchedGitRelease, SelectedGitReleaseFetchError>,
        mut fetch_git_sibling: impl FnMut(
            &GitPackageSourceIdentity,
            Option<&LockPackage>,
        )
            -> Result<FetchedGitPackage, GitRemoteReleaseFetchError>,
    ) -> Result<
        MixedSourceFinalAuditPlan,
        crate::resolver_source_provider::ResolverFinalSourcePlanError,
    > {
        let final_sources = self.plan_final_sources(locked_sources)?;
        let identities = self.provider.source_identities();
        let root_package = identities.root_package();
        let identity_context = MixedSourceFinalIdentityContext {
            root_package,
            read_identities: identities,
            locked_packages,
        };
        let mut final_snapshots = BTreeMap::new();
        let source_identities = plan_resolver_source_reloads(
            &final_sources,
            &self.provider,
            |package_version, selection, base_identity| {
                resolve_final_mixed_source_identity(
                    package_version,
                    selection,
                    base_identity,
                    &identity_context,
                    &mut fetch_git_release,
                    &mut final_snapshots,
                )
            },
        );

        let mut reload_failures = Vec::new();
        let mut local_directories = LocalPackageDirectoryIndex::default();
        let root_identity = identities
            .get(&MvsRequirer::Root(root_package.clone()))
            .and_then(|identity| match identity {
                MixedPackageSourceIdentity::Local(directory) => Some(directory),
                MixedPackageSourceIdentity::Git(_) => None,
            });
        if let Some(root_directory) = root_identity {
            if let Err(error) = local_directories.index_root_directory(root_directory) {
                reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: None,
                    error: MixedSourceFinalReloadError::LocalPathIndex(Box::new(error)),
                });
            }
        } else {
            reload_failures.push(MixedSourceFinalReloadFailure {
                package_version: None,
                error: MixedSourceFinalReloadError::MissingRootDirectory,
            });
        }

        for same in source_identities.same_identity() {
            if let MixedPackageSourceIdentity::Local(directory) = &same.identity
                && let Err(error) = local_directories.insert(
                    same.package_version.clone(),
                    directory.clone(),
                    PackageRole::Dependency,
                )
            {
                reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: Some(same.package_version.clone()),
                    error: MixedSourceFinalReloadError::LocalDirectoryIndex(Box::new(error)),
                });
            }
        }
        for reload in source_identities.reloads() {
            if let Some(MixedSourceFinalSnapshot::Local(directory)) =
                final_snapshots.get(&reload.package_version)
                && let Err(error) = local_directories.insert(
                    reload.package_version.clone(),
                    directory.clone(),
                    PackageRole::Dependency,
                )
            {
                reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: Some(reload.package_version.clone()),
                    error: MixedSourceFinalReloadError::LocalDirectoryIndex(Box::new(error)),
                });
            }
        }

        let mut final_path_packages = BTreeMap::new();
        let mut reloaded_git_path_packages = BTreeMap::new();
        for entry in final_sources.entries() {
            let package_version = entry.package_version();
            if source_identities
                .same_identity()
                .iter()
                .any(|same| same.package_version == *package_version)
            {
                append_read_time_path_packages(
                    package_version,
                    &self.context,
                    &self.git_path_requirements,
                    &mut final_path_packages,
                );
                continue;
            }
            if !source_identities
                .reloads()
                .iter()
                .any(|reload| reload.package_version == *package_version)
            {
                continue;
            }
            match final_snapshots.get(package_version) {
                Some(MixedSourceFinalSnapshot::Local(_)) => {
                    match local_directories
                        .index_dependency_path_requirements(package_version, &self.root_context)
                    {
                        Ok(additions) => final_path_packages.extend(additions),
                        Err(error) => reload_failures.push(MixedSourceFinalReloadFailure {
                            package_version: Some(package_version.clone()),
                            error: MixedSourceFinalReloadError::LocalPathIndex(Box::new(error)),
                        }),
                    }
                }
                Some(MixedSourceFinalSnapshot::GitRelease(release)) => {
                    append_final_git_path_packages(
                        release.as_ref(),
                        package_version,
                        &self.root_context,
                        locked_packages,
                        &mut fetch_git_sibling,
                        &mut final_path_packages,
                        &mut reload_failures,
                    );
                }
                Some(MixedSourceFinalSnapshot::GitPath(identity)) => {
                    let fetched = matching_locked_git_path_package(identity, locked_packages)
                        .and_then(|locked| {
                            fetch_git_sibling(identity, locked).and_then(|package| {
                                crate::git_remote::verify_git_path_package_identity(
                                    identity, &package,
                                )?;
                                Ok(package)
                            })
                        });
                    match fetched {
                        Ok(package) => {
                            append_final_git_path_packages(
                                &package,
                                package_version,
                                &self.root_context,
                                locked_packages,
                                &mut fetch_git_sibling,
                                &mut final_path_packages,
                                &mut reload_failures,
                            );
                            reloaded_git_path_packages.insert(package_version.clone(), package);
                        }
                        Err(error) => reload_failures.push(MixedSourceFinalReloadFailure {
                            package_version: Some(package_version.clone()),
                            error: MixedSourceFinalReloadError::GitPathFetch(Box::new(error)),
                        }),
                    }
                }
                None => reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: Some(package_version.clone()),
                    error: MixedSourceFinalReloadError::MissingFinalSourceSnapshot,
                }),
            }
        }

        let final_context = match self.root_context.with_path_packages(&final_path_packages) {
            Ok(context) => context,
            Err(error) => {
                reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: None,
                    error: MixedSourceFinalReloadError::ResolverContext(Box::new(error)),
                });
                self.root_context.clone()
            }
        };
        let mut reloaded_contracts = BTreeMap::new();
        for reload in source_identities.reloads() {
            if let Some(package) = reloaded_git_path_packages.get(&reload.package_version) {
                match package.build_dependency_contract(
                    &reload.package_version,
                    &final_context,
                    validation_context,
                ) {
                    Ok(contract) => {
                        reloaded_contracts.insert(
                            reload.package_version.clone(),
                            ResolverReloadedSourceContract::new(
                                contract,
                                final_context.clone(),
                                reload.final_identity.clone(),
                            ),
                        );
                    }
                    Err(error) => reload_failures.push(MixedSourceFinalReloadFailure {
                        package_version: Some(reload.package_version.clone()),
                        error: MixedSourceFinalReloadError::GitContract(Box::new(error)),
                    }),
                }
                continue;
            }
            let Some(snapshot) = final_snapshots.get(&reload.package_version) else {
                continue;
            };
            let contract = match snapshot {
                MixedSourceFinalSnapshot::Local(directory) => directory
                    .build_dependency_contract(
                        &reload.package_version,
                        &final_context,
                        validation_context,
                    )
                    .map_err(|error| MixedSourceFinalReloadError::LocalContract(Box::new(error))),
                MixedSourceFinalSnapshot::GitRelease(release) => release
                    .build_dependency_contract(
                        &reload.package_version,
                        &final_context,
                        validation_context,
                    )
                    .map_err(|error| MixedSourceFinalReloadError::GitContract(Box::new(error))),
                MixedSourceFinalSnapshot::GitPath(_) => {
                    Err(MixedSourceFinalReloadError::MissingFinalSourceSnapshot)
                }
            };
            match contract {
                Ok(contract) => {
                    reloaded_contracts.insert(
                        reload.package_version.clone(),
                        ResolverReloadedSourceContract::new(
                            contract,
                            final_context.clone(),
                            reload.final_identity.clone(),
                        ),
                    );
                }
                Err(error) => reload_failures.push(MixedSourceFinalReloadFailure {
                    package_version: Some(reload.package_version.clone()),
                    error,
                }),
            }
        }

        Ok(MixedSourceFinalAuditPlan {
            final_sources,
            source_identities,
            read_context: self.context.clone(),
            final_context,
            reloaded_contracts,
            reload_failures,
        })
    }
}

/// A final source could not be converted to its complete filesystem or Git identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MixedSourceFinalIdentityError {
    InvalidProvenance(MvsPackageVersion),
    MissingRequirer(MvsRequirer),
    LocalPathSyntax(PackagePathError),
    LocalPath(LocalPackageError),
    GitDirectory(PackagePathError),
    GitPath(GitPackagePathError),
    GitRelease(Box<SelectedGitReleaseFetchError>),
    GitReleaseIdentityMismatch {
        expected: Box<ResolverPackageSource>,
        found: Box<GitPackageSourceIdentity>,
    },
}

/// Why one changed final source could not be reloaded for contract comparison.
#[derive(Debug, Eq, PartialEq)]
pub enum MixedSourceFinalReloadError {
    MissingRootDirectory,
    MissingFinalSourceSnapshot,
    LocalPathIndex(Box<LocalPackagePathIndexError>),
    LocalDirectoryIndex(Box<LocalPackageDirectoryIndexError>),
    GitPathFetch(Box<GitRemoteReleaseFetchError>),
    GitPathDiscovery(Box<GitPathRequirementDiscoveryError<GitRemoteReleaseFetchError>>),
    LocalContract(Box<LocalPackageContractLoadError>),
    GitContract(Box<GitRemoteReleaseContractError>),
    ResolverContext(Box<ResolverContractBuildError>),
}

/// A changed final source whose path discovery or contract reload failed.
#[derive(Debug, Eq, PartialEq)]
pub struct MixedSourceFinalReloadFailure {
    pub package_version: Option<MvsPackageVersion>,
    pub error: MixedSourceFinalReloadError,
}

/// A package path could not be resolved against its supplying source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MixedSourceFinalPathResolutionError {
    LocalPathSyntax(PackagePathError),
    LocalPath(LocalPackageError),
    GitPath(GitPackagePathError),
}

/// Source data retained while planning and loading final-source reloads.
#[derive(Debug)]
enum MixedSourceFinalSnapshot {
    Local(LocalPackageDirectory),
    GitRelease(Box<FetchedGitRelease>),
    GitPath(GitPackageSourceIdentity),
}

struct MixedSourceFinalIdentityContext<'a> {
    root_package: &'a MvsPackageVersion,
    read_identities: &'a ResolverSourceIdentitySnapshot<MixedPackageSourceIdentity>,
    locked_packages: &'a HashMap<MvsPackageVersion, LockPackage>,
}

/// Source identities, reloaded contracts, and comparison context after MVS.
#[derive(Debug)]
pub struct MixedSourceFinalAuditPlan {
    final_sources: ResolverFinalSourcePlan,
    source_identities:
        ResolverSourceIdentityPlan<MixedPackageSourceIdentity, MixedSourceFinalIdentityError>,
    read_context: ResolverContractContext<'static>,
    final_context: ResolverContractContext<'static>,
    reloaded_contracts: BTreeMap<
        MvsPackageVersion,
        ResolverReloadedSourceContract<'static, MixedPackageSourceIdentity>,
    >,
    reload_failures: Vec<MixedSourceFinalReloadFailure>,
}

impl MixedSourceFinalAuditPlan {
    pub const fn final_sources(&self) -> &ResolverFinalSourcePlan {
        &self.final_sources
    }

    pub const fn source_identities(
        &self,
    ) -> &ResolverSourceIdentityPlan<MixedPackageSourceIdentity, MixedSourceFinalIdentityError>
    {
        &self.source_identities
    }

    pub const fn final_context(&self) -> &ResolverContractContext<'static> {
        &self.final_context
    }

    pub fn reload_failures(&self) -> &[MixedSourceFinalReloadFailure] {
        &self.reload_failures
    }

    /// Compare every reloaded contract with the contract read during MVS.
    pub fn audit(
        &self,
    ) -> ResolverFinalContractAudit<
        '_,
        MixedPackageSourceIdentity,
        MixedSourceFinalIdentityError,
        MixedSourceFinalPathResolutionError,
    > {
        audit_resolver_reloaded_contracts(
            &self.final_sources,
            &self.source_identities,
            &self.read_context,
            &self.reloaded_contracts,
            |_, request, base_identity| resolve_mixed_source_path_request(request, base_identity),
        )
    }

    /// True only when identity planning, every required reload, and all edge
    /// and source-bound path comparisons completed without divergence.
    pub fn is_equivalent(&self) -> bool {
        self.final_sources.failures().is_empty()
            && self.final_sources.root_cycles().is_empty()
            && self.reload_failures.is_empty()
            && self.source_identities.failures().is_empty()
            && self.audit().is_equivalent()
    }
}

fn resolve_final_mixed_source_identity(
    package_version: &MvsPackageVersion,
    selection: &ResolverSourceSelection,
    base_identity: Option<&MixedPackageSourceIdentity>,
    context: &MixedSourceFinalIdentityContext<'_>,
    fetch_git_release: &mut impl FnMut(
        &ResolverSourceSelection,
        Option<&LockPackage>,
    ) -> Result<FetchedGitRelease, SelectedGitReleaseFetchError>,
    snapshots: &mut BTreeMap<MvsPackageVersion, MixedSourceFinalSnapshot>,
) -> Result<MixedPackageSourceIdentity, MixedSourceFinalIdentityError> {
    match selection.source() {
        ResolverPackageSource::Git { url, directory } => {
            let selected_directory = crate::package_path::RepositoryPackageDirectory::parse(
                directory.as_deref().unwrap_or("."),
            )
            .map_err(MixedSourceFinalIdentityError::GitDirectory)?;
            if let Some(MixedPackageSourceIdentity::Git(identity)) = context
                .read_identities
                .get(&MvsRequirer::Package(package_version.clone()))
                && identity.canonical_url() == url.as_ref()
                && identity.directory() == &selected_directory
            {
                return Ok(MixedPackageSourceIdentity::Git(identity.clone()));
            }
            let fetched =
                fetch_git_release(selection, context.locked_packages.get(package_version))
                    .map_err(|error| MixedSourceFinalIdentityError::GitRelease(Box::new(error)))?;
            if fetched.source_identity().canonical_url() != url.as_ref()
                || fetched.source_identity().directory() != &selected_directory
            {
                return Err(MixedSourceFinalIdentityError::GitReleaseIdentityMismatch {
                    expected: Box::new(selection.source().clone()),
                    found: Box::new(fetched.source_identity().clone()),
                });
            }
            let identity = MixedPackageSourceIdentity::Git(fetched.source_identity().clone());
            snapshots.insert(
                package_version.clone(),
                MixedSourceFinalSnapshot::GitRelease(Box::new(fetched)),
            );
            Ok(identity)
        }
        ResolverPackageSource::Path { relative_to, path } => {
            let requirer = source_requirer(selection, context.root_package).ok_or_else(|| {
                MixedSourceFinalIdentityError::InvalidProvenance(package_version.clone())
            })?;
            if requirer.package_version() != relative_to {
                return Err(MixedSourceFinalIdentityError::InvalidProvenance(
                    package_version.clone(),
                ));
            }
            let base_identity = base_identity
                .or_else(|| context.read_identities.get(&requirer))
                .ok_or_else(|| MixedSourceFinalIdentityError::MissingRequirer(requirer.clone()))?;
            match base_identity {
                MixedPackageSourceIdentity::Local(directory) => {
                    let path = DependencyPackagePath::parse(path)
                        .map_err(MixedSourceFinalIdentityError::LocalPathSyntax)?;
                    let directory = directory
                        .resolve_dependency(&path)
                        .map_err(MixedSourceFinalIdentityError::LocalPath)?;
                    let identity = MixedPackageSourceIdentity::Local(directory.clone());
                    snapshots.insert(
                        package_version.clone(),
                        MixedSourceFinalSnapshot::Local(directory),
                    );
                    Ok(identity)
                }
                MixedPackageSourceIdentity::Git(identity) => {
                    let identity = identity
                        .resolve_dependency_path(path)
                        .map_err(MixedSourceFinalIdentityError::GitPath)?;
                    snapshots.insert(
                        package_version.clone(),
                        MixedSourceFinalSnapshot::GitPath(identity.clone()),
                    );
                    Ok(MixedPackageSourceIdentity::Git(identity))
                }
            }
        }
    }
}

fn source_requirer(
    selection: &ResolverSourceSelection,
    root_package: &MvsPackageVersion,
) -> Option<MvsRequirer> {
    match selection.provenance() {
        ResolverSourceProvenance::RootPathOverride { root_package, .. }
        | ResolverSourceProvenance::RootGitOverride { root_package, .. } => {
            Some(MvsRequirer::Root(root_package.clone()))
        }
        ResolverSourceProvenance::PathPin { pin } => Some(pin.requirer.clone()),
        ResolverSourceProvenance::NamedRequirement { edge } => Some(edge.requirer.clone()),
        ResolverSourceProvenance::LockFloor { .. } => Some(MvsRequirer::Root(root_package.clone())),
    }
}

fn append_read_time_path_packages(
    package_version: &MvsPackageVersion,
    context: &ResolverContractContext<'_>,
    git_path_requirements: &BTreeMap<MvsRequirer, GitPathRequirementIndex>,
    destination: &mut BTreeMap<crate::resolver_contract::ResolverPathKey, MvsPackageVersion>,
) {
    for (key, dependency) in context.path_packages() {
        if key.requirer == MvsRequirer::Package(package_version.clone()) {
            destination.insert(key.clone(), dependency.clone());
        }
    }
    if let Some(index) = git_path_requirements.get(&MvsRequirer::Package(package_version.clone())) {
        destination.extend(index.path_packages().clone());
    }
}

fn append_final_git_path_packages(
    source: &impl crate::git_remote::GitPackageSourceView,
    package_version: &MvsPackageVersion,
    root_context: &ResolverContractContext<'_>,
    locked_packages: &HashMap<MvsPackageVersion, LockPackage>,
    fetch_git_sibling: &mut impl FnMut(
        &GitPackageSourceIdentity,
        Option<&LockPackage>,
    ) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError>,
    destination: &mut BTreeMap<crate::resolver_contract::ResolverPathKey, MvsPackageVersion>,
    failures: &mut Vec<MixedSourceFinalReloadFailure>,
) {
    let requirer = MvsRequirer::Package(package_version.clone());
    let discovered = discover_git_path_requirements(source, &requirer, root_context, |identity| {
        let locked = matching_locked_git_path_package(identity, locked_packages)?;
        fetch_git_sibling(identity, locked)
    });
    match discovered {
        Ok(discovered) => destination.extend(discovered.path_packages().clone()),
        Err(error) => failures.push(MixedSourceFinalReloadFailure {
            package_version: Some(package_version.clone()),
            error: MixedSourceFinalReloadError::GitPathDiscovery(Box::new(error)),
        }),
    }
}

fn resolve_mixed_source_path_request(
    request: &ResolverContractPathSourceRequest,
    base_identity: &MixedPackageSourceIdentity,
) -> Result<MixedPackageSourceIdentity, MixedSourceFinalPathResolutionError> {
    match base_identity {
        MixedPackageSourceIdentity::Local(directory) => {
            let path = DependencyPackagePath::parse(&request.path)
                .map_err(MixedSourceFinalPathResolutionError::LocalPathSyntax)?;
            directory
                .resolve_dependency(&path)
                .map(MixedPackageSourceIdentity::Local)
                .map_err(MixedSourceFinalPathResolutionError::LocalPath)
        }
        MixedPackageSourceIdentity::Git(identity) => identity
            .resolve_dependency_path(&request.path)
            .map(MixedPackageSourceIdentity::Git)
            .map_err(MixedSourceFinalPathResolutionError::GitPath),
    }
}

struct MixedSourceLoadState {
    context: ResolverContractContext<'static>,
    local_directories: LocalPackageDirectoryIndex,
    git_identities: GitPackageSourceIdentityIndex,
    git_path_requirements: BTreeMap<MvsRequirer, GitPathRequirementIndex>,
    loaded_sources: BTreeMap<MvsRequirer, MixedPackageSourceIdentity>,
}

impl MixedSourceLoadState {
    fn canonicalize_path_pin(
        &self,
        pin: &ResolverPathPinSource,
    ) -> Result<MixedPackageSourceIdentity, MixedSourcePathPinError> {
        let source_identity = self
            .loaded_sources
            .get(&pin.requirer)
            .ok_or_else(|| MixedSourcePathPinError::MissingRequirer(pin.requirer.clone()))?;
        match source_identity {
            MixedPackageSourceIdentity::Local(_) => self
                .local_directories
                .canonicalize_path_pin(pin)
                .map(MixedPackageSourceIdentity::Local)
                .map_err(MixedSourcePathPinError::Local),
            MixedPackageSourceIdentity::Git(_) => self
                .git_identities
                .canonicalize_path_pin(pin)
                .map(MixedPackageSourceIdentity::Git)
                .map_err(MixedSourcePathPinError::Git),
        }
    }
}

/// Resolve a local root with Git and local path dependencies.
///
/// `lock_floors`, `locked_sources`, and `locked_packages` must come from a
/// validated lock snapshot. Pass empty maps to resolve without lock floors.
/// Selected Git releases are fetched by their exact canonical tag unless a
/// matching locked package supplies the verified commit and checksums.
pub fn resolve_mixed_source_packages(
    root_directory: LocalPackageDirectory,
    lock_floors: &HashMap<MvsNode, Version>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    locked_packages: &HashMap<MvsPackageVersion, LockPackage>,
    validation_context: PackageValidationContext<'_>,
) -> Result<MixedSourceMvsOutput, MixedSourceMvsError> {
    resolve_mixed_source_packages_with_fetchers(
        root_directory,
        lock_floors,
        locked_sources,
        locked_packages,
        validation_context,
        fetch_selected_git_release,
        |identity, locked_package| {
            if let Some(locked_package) = locked_package {
                fetch_locked_git_path_package_tree(locked_package, identity)
            } else {
                fetch_git_package_source_tree_at_commit(
                    identity.canonical_url(),
                    identity.commit_id(),
                    identity.directory(),
                )
            }
        },
    )
}

pub(crate) fn resolve_mixed_source_packages_with_fetchers(
    root_directory: LocalPackageDirectory,
    lock_floors: &HashMap<MvsNode, Version>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    locked_packages: &HashMap<MvsPackageVersion, LockPackage>,
    validation_context: PackageValidationContext<'_>,
    mut fetch_git_release: impl FnMut(
        &ResolverSourceSelection,
        Option<&LockPackage>,
    ) -> Result<FetchedGitRelease, SelectedGitReleaseFetchError>,
    mut fetch_git_sibling: impl FnMut(
        &GitPackageSourceIdentity,
        Option<&LockPackage>,
    ) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError>,
) -> Result<MixedSourceMvsOutput, MixedSourceMvsError> {
    let mut local_directories = LocalPackageDirectoryIndex::default();
    local_directories
        .index_root_directory(&root_directory)
        .map_err(|error| MixedSourceMvsError::RootDiscovery(Box::new(error)))?;

    let root_manifest = root_directory.parse_manifest(PackageRole::Root);
    let root_path_overrides = local_directories
        .index_root_path_overrides(&root_directory)
        .map_err(|error| MixedSourceMvsError::RootDiscovery(Box::new(error)))?;
    let root_path_packages = local_directories
        .index_root_path_requirements(&root_directory, &root_path_overrides)
        .map_err(|error| MixedSourceMvsError::RootDiscovery(Box::new(error)))?;
    let root_context = ResolverContractContext::from_root_manifest(
        &root_manifest,
        &root_path_packages,
        &root_path_overrides,
        validation_context,
    )
    .map_err(|error| MixedSourceMvsError::RootContract(Box::new(error)))?
    .into_owned();
    let root_contract = adapt_resolver_contract(&root_manifest, &root_context, validation_context)
        .map_err(|error| MixedSourceMvsError::RootContract(Box::new(error)))?;

    let mut loaded_sources = BTreeMap::new();
    let root_requirer = MvsRequirer::Root(root_context.root_package().clone());
    loaded_sources.insert(
        root_requirer,
        MixedPackageSourceIdentity::Local(root_directory.clone()),
    );
    let state = RefCell::new(MixedSourceLoadState {
        context: root_context.clone(),
        local_directories,
        git_identities: GitPackageSourceIdentityIndex::default(),
        git_path_requirements: BTreeMap::new(),
        loaded_sources,
    });

    let provider_result = select_with_source_provider_and_canonicalized_pins_and_identities(
        crate::resolver_source_provider::ResolverLoadedRootContract::new(
            root_contract,
            MixedPackageSourceIdentity::Local(root_directory),
        ),
        &root_context,
        lock_floors,
        locked_sources,
        |pin| state.borrow().canonicalize_path_pin(pin),
        |request, selection, canonicalized_path_pins| {
            load_mixed_source_candidate(
                request,
                selection,
                canonicalized_path_pins,
                &state,
                locked_packages,
                validation_context,
                &mut fetch_git_release,
                &mut fetch_git_sibling,
            )
        },
    );

    let state = state.into_inner();
    match provider_result {
        Ok(provider) => Ok(MixedSourceMvsOutput {
            provider,
            root_context,
            context: state.context,
            local_directories: state.local_directories,
            git_identities: state.git_identities,
            git_path_requirements: state.git_path_requirements,
        }),
        Err(error) => Err(MixedSourceMvsError::Provider(Box::new(
            MixedSourceMvsProviderFailure {
                error,
                context: state.context,
                local_directories: state.local_directories,
                git_identities: state.git_identities,
                git_path_requirements: state.git_path_requirements,
            },
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn load_mixed_source_candidate(
    request: &MvsContractRequest,
    selection: &ResolverSourceSelection,
    canonicalized_path_pins: &[MixedPackageSourceIdentity],
    state: &RefCell<MixedSourceLoadState>,
    locked_packages: &HashMap<MvsPackageVersion, LockPackage>,
    validation_context: PackageValidationContext<'_>,
    fetch_git_release: &mut impl FnMut(
        &ResolverSourceSelection,
        Option<&LockPackage>,
    ) -> Result<FetchedGitRelease, SelectedGitReleaseFetchError>,
    fetch_git_sibling: &mut impl FnMut(
        &GitPackageSourceIdentity,
        Option<&LockPackage>,
    ) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError>,
) -> Result<
    crate::resolver_source_provider::ResolverLoadedContract<MixedPackageSourceIdentity>,
    MixedSourceMvsLoadError,
> {
    let mut state = state.borrow_mut();
    let current_context = state.context.clone();
    let mut local_directories = state.local_directories.clone();
    let mut git_identities = state.git_identities.clone();
    let requirer = MvsRequirer::Package(request.package_version.clone());
    let mut discovered_git_paths = None;

    let (contract, source_identity, next_context) = match (selection.reason(), selection.source()) {
        (ResolverSourceReason::PathPin, ResolverPackageSource::Path { .. }) => {
            match canonicalized_path_pins.first() {
                Some(MixedPackageSourceIdentity::Local(_)) => {
                    let local_pins = canonicalized_path_pins
                        .iter()
                        .map(|identity| match identity {
                            MixedPackageSourceIdentity::Local(directory) => Ok(directory.clone()),
                            MixedPackageSourceIdentity::Git(_) => {
                                Err(MixedSourceMvsLoadError::UnsupportedSource {
                                    package_version: Box::new(request.package_version.clone()),
                                    package_source: Box::new(selection.source().clone()),
                                })
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let directory = local_directories
                        .resolve_selected_path_source(
                            selection,
                            current_context.root_package(),
                            &local_pins,
                        )
                        .map_err(MixedSourceMvsLoadError::LocalSourceResolve)?
                        .ok_or_else(|| MixedSourceMvsLoadError::UnsupportedSource {
                            package_version: Box::new(request.package_version.clone()),
                            package_source: Box::new(selection.source().clone()),
                        })?;
                    let additions = local_directories
                        .index_dependency_path_requirements(
                            &request.package_version,
                            &current_context,
                        )
                        .map_err(|error| {
                            MixedSourceMvsLoadError::LocalPathRequirements(Box::new(error))
                        })?;
                    let next_context =
                        current_context
                            .with_path_packages(&additions)
                            .map_err(|error| {
                                MixedSourceMvsLoadError::ResolverContext(Box::new(error))
                            })?;
                    let contract = directory
                        .build_dependency_contract(
                            &request.package_version,
                            &next_context,
                            validation_context,
                        )
                        .map_err(|error| {
                            MixedSourceMvsLoadError::LocalPackageContract(Box::new(error))
                        })?;
                    (
                        contract,
                        MixedPackageSourceIdentity::Local(directory),
                        next_context,
                    )
                }
                Some(MixedPackageSourceIdentity::Git(_)) => {
                    let pin = selection.path_pin_sources().first().ok_or_else(|| {
                        MixedSourceMvsLoadError::UnsupportedSource {
                            package_version: Box::new(request.package_version.clone()),
                            package_source: Box::new(selection.source().clone()),
                        }
                    })?;
                    let (path_requirer, path) = path_source_parts(pin)?;
                    let key = crate::resolver_contract::ResolverPathKey::new(
                        path_requirer.clone(),
                        path.to_owned().into_boxed_str(),
                    );
                    let package = state
                        .git_path_requirements
                        .get(path_requirer)
                        .and_then(|index| index.package(&key))
                        .ok_or_else(|| MixedSourceMvsLoadError::GitPathPackageNotIndexed {
                            requirer: path_requirer.clone(),
                            path: path.into(),
                        })?;
                    let identity =
                        MixedPackageSourceIdentity::Git(package.source_identity().clone());
                    if canonicalized_path_pins
                        .iter()
                        .any(|canonicalized| canonicalized != &identity)
                    {
                        return Err(MixedSourceMvsLoadError::UnsupportedSource {
                            package_version: Box::new(request.package_version.clone()),
                            package_source: Box::new(selection.source().clone()),
                        });
                    }
                    let discovered = discover_git_path_requirements(
                        package,
                        &requirer,
                        &current_context,
                        |sibling_identity| {
                            let locked_package = matching_locked_git_path_package(
                                sibling_identity,
                                locked_packages,
                            )?;
                            fetch_git_sibling(sibling_identity, locked_package)
                        },
                    )
                    .map_err(|error| {
                        MixedSourceMvsLoadError::GitPathRequirements(Box::new(error))
                    })?;
                    let next_context = current_context
                        .with_path_packages(discovered.path_packages())
                        .map_err(|error| {
                            MixedSourceMvsLoadError::ResolverContext(Box::new(error))
                        })?;
                    let contract = package
                        .build_dependency_contract(
                            &request.package_version,
                            &next_context,
                            validation_context,
                        )
                        .map_err(|error| {
                            MixedSourceMvsLoadError::GitPackageContract(Box::new(error))
                        })?;
                    discovered_git_paths = Some(discovered);
                    (contract, identity, next_context)
                }
                None => {
                    return Err(MixedSourceMvsLoadError::UnsupportedSource {
                        package_version: Box::new(request.package_version.clone()),
                        package_source: Box::new(selection.source().clone()),
                    });
                }
            }
        }
        (ResolverSourceReason::RootPathOverride, ResolverPackageSource::Path { .. }) => {
            let local_pins = canonicalized_path_pins
                .iter()
                .map(|identity| match identity {
                    MixedPackageSourceIdentity::Local(directory) => Ok(directory.clone()),
                    MixedPackageSourceIdentity::Git(_) => {
                        Err(MixedSourceMvsLoadError::UnsupportedSource {
                            package_version: Box::new(request.package_version.clone()),
                            package_source: Box::new(selection.source().clone()),
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let directory = local_directories
                .resolve_selected_path_source(
                    selection,
                    current_context.root_package(),
                    &local_pins,
                )
                .map_err(MixedSourceMvsLoadError::LocalSourceResolve)?
                .ok_or_else(|| MixedSourceMvsLoadError::UnsupportedSource {
                    package_version: Box::new(request.package_version.clone()),
                    package_source: Box::new(selection.source().clone()),
                })?;
            let additions = local_directories
                .index_dependency_path_requirements(&request.package_version, &current_context)
                .map_err(|error| MixedSourceMvsLoadError::LocalPathRequirements(Box::new(error)))?;
            let next_context = current_context
                .with_path_packages(&additions)
                .map_err(|error| MixedSourceMvsLoadError::ResolverContext(Box::new(error)))?;
            let contract = directory
                .build_dependency_contract(
                    &request.package_version,
                    &next_context,
                    validation_context,
                )
                .map_err(|error| MixedSourceMvsLoadError::LocalPackageContract(Box::new(error)))?;
            (
                contract,
                MixedPackageSourceIdentity::Local(directory),
                next_context,
            )
        }
        (_, ResolverPackageSource::Git { .. }) => {
            let locked_package = locked_packages.get(&request.package_version);
            let fetched = fetch_git_release(selection, locked_package)
                .map_err(|error| MixedSourceMvsLoadError::GitReleaseFetch(Box::new(error)))?;
            let identity = MixedPackageSourceIdentity::Git(fetched.source_identity().clone());
            let discovered = discover_git_path_requirements(
                &fetched,
                &requirer,
                &current_context,
                |sibling_identity| {
                    let locked_package =
                        matching_locked_git_path_package(sibling_identity, locked_packages)?;
                    fetch_git_sibling(sibling_identity, locked_package)
                },
            )
            .map_err(|error| MixedSourceMvsLoadError::GitPathRequirements(Box::new(error)))?;
            let next_context = current_context
                .with_path_packages(discovered.path_packages())
                .map_err(|error| MixedSourceMvsLoadError::ResolverContext(Box::new(error)))?;
            let contract = fetched
                .build_dependency_contract(
                    &request.package_version,
                    &next_context,
                    validation_context,
                )
                .map_err(|error| MixedSourceMvsLoadError::GitPackageContract(Box::new(error)))?;
            discovered_git_paths = Some(discovered);
            (contract, identity, next_context)
        }
        _ => {
            return Err(MixedSourceMvsLoadError::UnsupportedSource {
                package_version: Box::new(request.package_version.clone()),
                package_source: Box::new(selection.source().clone()),
            });
        }
    };

    if let MixedPackageSourceIdentity::Git(identity) = &source_identity {
        git_identities
            .insert(requirer.clone(), identity.clone())
            .map_err(|error| MixedSourceMvsLoadError::GitSourceIdentityIndex(Box::new(error)))?;
    }
    if state
        .loaded_sources
        .get(&requirer)
        .is_some_and(|existing| existing != &source_identity)
    {
        return Err(MixedSourceMvsLoadError::RequirerIdentityConflict(requirer));
    }
    state
        .loaded_sources
        .insert(requirer.clone(), source_identity.clone());
    state.context = next_context;
    state.local_directories = local_directories;
    state.git_identities = git_identities;
    if let Some(discovered) = discovered_git_paths {
        state.git_path_requirements.insert(requirer, discovered);
    }

    Ok(crate::resolver_source_provider::ResolverLoadedContract::new(contract, source_identity))
}

fn path_source_parts(
    pin: &ResolverPathPinSource,
) -> Result<(&MvsRequirer, &str), MixedSourceMvsLoadError> {
    match &pin.source {
        ResolverPackageSource::Path { path, .. } => Ok((&pin.requirer, path)),
        ResolverPackageSource::Git { .. } => Err(MixedSourceMvsLoadError::UnsupportedSource {
            package_version: Box::new(pin.requirer.package_version().clone()),
            package_source: Box::new(pin.source.clone()),
        }),
    }
}

impl fmt::Display for MixedSourceMvsLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSource {
                package_version, ..
            } => write!(
                formatter,
                "unsupported package source for {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::LocalSourceResolve(error) => fmt::Display::fmt(error, formatter),
            Self::LocalPathRequirements(error) => fmt::Display::fmt(error, formatter),
            Self::GitPathRequirements(error) => write!(formatter, "{error:?}"),
            Self::GitPathPackageNotIndexed { requirer, path } => write!(
                formatter,
                "Git path `{path}` from {}@{} was not indexed",
                requirer.package_version().package(),
                requirer.package_version().version()
            ),
            Self::GitReleaseFetch(error) => fmt::Display::fmt(error, formatter),
            Self::GitPackageContract(error) => fmt::Display::fmt(error, formatter),
            Self::LocalPackageContract(error) => write!(formatter, "{error:?}"),
            Self::ResolverContext(error) => fmt::Display::fmt(error, formatter),
            Self::GitSourceIdentityIndex(error) => write!(formatter, "{error:?}"),
            Self::RequirerIdentityConflict(requirer) => write!(
                formatter,
                "source identity changed while loading {}@{}",
                requirer.package_version().package(),
                requirer.package_version().version()
            ),
        }
    }
}

impl std::error::Error for MixedSourceMvsLoadError {}
