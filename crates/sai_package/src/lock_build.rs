//! Construction of `sai.lock` from a validated final resolver graph.
//!
//! Content-derived fields are supplied by the caller after source selection
//! and final-source auditing. Requirements are derived from the resolver
//! contracts and checked against the selected graph, so callers cannot persist
//! edges from a different resolution.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::ops::Range;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::lock::{
    LockFile, LockPackage, LockPackageField, LockParseOutput, LockRequirement, LockRoot,
    LockRootField,
};
use crate::lock_graph_validation::{
    LockGraphValidationInputError, LockGraphValidationOutput, validate as validate_lock_graph,
};
use crate::lock_validation::{
    LockValidationError, SUPPORTED_LOCK_FORMAT_VERSION, canonical_git_url,
    validate as validate_lock,
};
use crate::lock_writer::{
    LockWriteOutcome, WorkspaceLockWriteError, create_workspace_lock_atomically,
};
use crate::manifest::{MAX_PACKAGE_MANIFEST_BYTES, PackageRole, RequirementSource, Spanned};
use crate::mvs::{MvsInputError, MvsPackageVersion, MvsRequirer, select};
use crate::mvs_graph_validation::{
    MvsDependencyCycle, MvsSelectedGraph, MvsSelectedGraphInputError, MvsVersionConflict,
    check_selected_graph,
};
use crate::resolver_contract::{ResolverContract, ResolverPinKind, ResolverRequirementResolution};
use crate::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};

/// Content-derived lock fields for one selected package version.
///
/// `Requires` fields are deliberately excluded: the builder derives those
/// from the resolver contracts and selected graph. Git/path source identity,
/// hashes, artifact records, compiler requirements, and capability metadata
/// come from the audited package contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LockPackageMetadata {
    fields: Vec<LockPackageField>,
}

/// Failure to construct a lock that represents the supplied final graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockBuildError {
    RootRoleMismatch(PackageRole),
    RootPackageMismatch {
        contract: Box<MvsPackageVersion>,
        graph: Box<MvsPackageVersion>,
    },
    MvsPackageVersionMismatch {
        manifest: Box<MvsPackageVersion>,
        mvs: Box<MvsPackageVersion>,
    },
    InconsistentRootContract(MvsPackageVersion),
    InvalidSelectedGraph {
        conflicts: Vec<MvsVersionConflict>,
        cycle: Option<MvsDependencyCycle>,
    },
    DependencyContractsMismatch {
        missing: Vec<MvsPackageVersion>,
        unexpected: Vec<MvsPackageVersion>,
    },
    PackageMetadataMismatch {
        missing: Vec<MvsPackageVersion>,
        unexpected: Vec<MvsPackageVersion>,
    },
    DependencyContractMismatch(MvsPackageVersion),
    RootContextMismatch(MvsPackageVersion),
    RequirementEdgeMismatch {
        requirer: MvsRequirer,
        span: Range<usize>,
    },
    MissingRequirementSource {
        requirer: MvsRequirer,
        package: Box<str>,
        span: Range<usize>,
    },
    InvalidRequirementSourceUrl {
        requirer: MvsRequirer,
        package: Box<str>,
        span: Range<usize>,
    },
    UnexpectedGraphEdge {
        requirer: MvsRequirer,
        span: Range<usize>,
    },
    MetadataContainsRequirements(MvsPackageVersion),
    InvalidLock(Vec<LockValidationError>),
    LockGraphValidationInput(LockGraphValidationInputError),
    InvalidLockGraph(LockGraphValidationOutput),
}

impl fmt::Display for LockBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "could not build sai.lock: {self:?}")
    }
}

impl std::error::Error for LockBuildError {}

/// Failure while initializing a lock for a root package with no active
/// requirements.
#[derive(Debug)]
pub enum RootOnlyLockCreationError {
    RootHasRequirements(MvsPackageVersion),
    RootManifestRead {
        path: PathBuf,
        source: io::Error,
    },
    RootManifestNotRegularFile(PathBuf),
    RootManifestTooLarge(PathBuf),
    RootManifestMismatch {
        expected_digest: [u8; 32],
        actual_digest: [u8; 32],
    },
    Selection(Vec<MvsInputError>),
    SelectedGraph(MvsSelectedGraphInputError),
    Build(LockBuildError),
    Write(WorkspaceLockWriteError),
}

impl fmt::Display for RootOnlyLockCreationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootHasRequirements(root) => write!(
                formatter,
                "cannot create a root-only lock for {root:?}: it has active requirements"
            ),
            Self::RootManifestRead { path, source } => {
                write!(formatter, "could not read root manifest {path:?}: {source}")
            }
            Self::RootManifestNotRegularFile(path) => {
                write!(formatter, "root manifest is not a regular file: {path:?}")
            }
            Self::RootManifestTooLarge(path) => write!(
                formatter,
                "root manifest exceeds the {MAX_PACKAGE_MANIFEST_BYTES}-byte limit: {path:?}"
            ),
            Self::RootManifestMismatch { .. } => {
                formatter.write_str("root contract does not match the workspace sai.package")
            }
            Self::Selection(errors) => {
                write!(formatter, "could not select the root graph: {errors:?}")
            }
            Self::SelectedGraph(error) => {
                write!(
                    formatter,
                    "could not construct the selected root graph: {error:?}"
                )
            }
            Self::Build(error) => error.fmt(formatter),
            Self::Write(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RootOnlyLockCreationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RootManifestRead { source, .. } => Some(source),
            Self::Build(error) => Some(error),
            Self::Write(error) => Some(error),
            Self::RootHasRequirements(_)
            | Self::RootManifestNotRegularFile(_)
            | Self::RootManifestTooLarge(_)
            | Self::RootManifestMismatch { .. }
            | Self::Selection(_)
            | Self::SelectedGraph(_) => None,
        }
    }
}

/// Build and atomically create a root-only `sai.lock`.
///
/// The root contract must describe the validated `sai.package` in the guarded
/// package directory, with no active requirements. This function rereads the
/// manifest while the guard is held and compares its SHA-256 digest to the
/// contract. `guard` must hold the package directory's exclusive workspace
/// lock. The operation refuses to replace any existing `sai.lock` directory
/// entry, including a symlink or a non-regular file.
pub fn create_root_only_lock_if_missing(
    guard: &WorkspaceLockGuard,
    root_contract: &ResolverContract,
) -> Result<LockWriteOutcome, RootOnlyLockCreationError> {
    if guard.mode() != WorkspaceLockMode::Exclusive {
        return Err(RootOnlyLockCreationError::Write(
            WorkspaceLockWriteError::WorkspaceLockNotExclusive(guard.mode()),
        ));
    }
    if !root_contract.mvs.edges().is_empty() {
        return Err(RootOnlyLockCreationError::RootHasRequirements(
            root_contract.manifest_package_version().clone(),
        ));
    }

    let manifest_bytes = read_guarded_root_manifest(guard)?;
    let actual_digest: [u8; 32] = Sha256::digest(&manifest_bytes).into();
    let expected_digest = *root_contract.manifest_digest();
    if actual_digest != expected_digest {
        return Err(RootOnlyLockCreationError::RootManifestMismatch {
            expected_digest,
            actual_digest,
        });
    }

    let selection = select(
        &root_contract.mvs,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    )
    .map_err(RootOnlyLockCreationError::Selection)?;
    let graph = check_selected_graph(&root_contract.mvs, &selection)
        .map_err(RootOnlyLockCreationError::SelectedGraph)?;
    let parsed = build_resolved_lock(root_contract, &graph, &BTreeMap::new(), &BTreeMap::new())
        .map_err(RootOnlyLockCreationError::Build)?;
    create_workspace_lock_atomically(guard, &parsed).map_err(RootOnlyLockCreationError::Write)
}

fn read_guarded_root_manifest(
    guard: &WorkspaceLockGuard,
) -> Result<Vec<u8>, RootOnlyLockCreationError> {
    let path = guard.package_directory().join("sai.package");
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let mut file =
        options
            .open(&path)
            .map_err(|source| RootOnlyLockCreationError::RootManifestRead {
                path: path.clone(),
                source,
            })?;
    let metadata =
        file.metadata()
            .map_err(|source| RootOnlyLockCreationError::RootManifestRead {
                path: path.clone(),
                source,
            })?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(RootOnlyLockCreationError::RootManifestNotRegularFile(path));
        }
    }
    if !metadata.is_file() {
        return Err(RootOnlyLockCreationError::RootManifestNotRegularFile(path));
    }
    if metadata.len() > MAX_PACKAGE_MANIFEST_BYTES as u64 {
        return Err(RootOnlyLockCreationError::RootManifestTooLarge(path));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| RootOnlyLockCreationError::RootManifestRead {
            path: path.clone(),
            source,
        })?;
    if bytes.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(RootOnlyLockCreationError::RootManifestTooLarge(path));
    }
    Ok(bytes)
}

/// Builds a lock AST for the final resolved graph.
///
/// `root_contract` and every dependency contract must come from the same
/// resolver root context. `dependency_contracts` and `package_metadata` must
/// each contain exactly one record for every selected dependency version.
/// Package metadata must describe the final audited source selected for that
/// version; in particular a path record must use a root-relative normalized
/// path, while Git records must include the content fields required by the
/// lock format. This assembly boundary validates field shape, but does not
/// independently prove source selection or recompute content checksums; those
/// facts must come from the caller's completed final-source audit and fetch.
///
/// This function checks lock-local semantics and graph consistency, but does
/// not normalize or serialize the result. Callers must first validate
/// graph-dependent capability names in the returned AST, then call
/// `lock_serialization::serialize_canonical` or the atomic writer. This order
/// preserves capability names that canonical serialization may remove as
/// covered by another capability.
pub(crate) fn build_resolved_lock(
    root_contract: &ResolverContract,
    graph: &MvsSelectedGraph,
    dependency_contracts: &BTreeMap<MvsPackageVersion, ResolverContract>,
    package_metadata: &BTreeMap<MvsPackageVersion, LockPackageMetadata>,
) -> Result<LockParseOutput, LockBuildError> {
    if root_contract.role() != PackageRole::Root {
        return Err(LockBuildError::RootRoleMismatch(root_contract.role()));
    }
    let root_version = root_contract.manifest_package_version();
    let mvs_root_version = root_contract.mvs.package_version();
    if mvs_root_version != root_version {
        return Err(LockBuildError::MvsPackageVersionMismatch {
            manifest: Box::new(root_version.clone()),
            mvs: Box::new(mvs_root_version.clone()),
        });
    }
    if root_version != &graph.root {
        return Err(LockBuildError::RootPackageMismatch {
            contract: Box::new(root_version.clone()),
            graph: Box::new(graph.root.clone()),
        });
    }
    if !root_contract.mvs_metadata_is_consistent() {
        return Err(LockBuildError::InconsistentRootContract(
            root_version.clone(),
        ));
    }
    if !graph.is_valid() {
        return Err(LockBuildError::InvalidSelectedGraph {
            conflicts: graph.conflicts.clone(),
            cycle: graph.cycle.clone(),
        });
    }

    let graph_dependencies = &graph.dependencies;
    let contract_versions = dependency_contracts.keys().cloned().collect();
    if &contract_versions != graph_dependencies {
        return Err(LockBuildError::DependencyContractsMismatch {
            missing: graph_dependencies
                .difference(&contract_versions)
                .cloned()
                .collect(),
            unexpected: contract_versions
                .difference(graph_dependencies)
                .cloned()
                .collect(),
        });
    }
    let metadata_versions = package_metadata.keys().cloned().collect();
    if &metadata_versions != graph_dependencies {
        return Err(LockBuildError::PackageMetadataMismatch {
            missing: graph_dependencies
                .difference(&metadata_versions)
                .cloned()
                .collect(),
            unexpected: metadata_versions
                .difference(graph_dependencies)
                .cloned()
                .collect(),
        });
    }

    for (package_version, contract) in dependency_contracts {
        if contract.role() != PackageRole::Dependency
            || contract.manifest_package_version() != package_version
            || contract.mvs.package_version() != package_version
            || contract.root_targets() != root_contract.root_targets()
            || !contract.mvs_metadata_is_consistent()
        {
            return Err(LockBuildError::DependencyContractMismatch(
                package_version.clone(),
            ));
        }
        if !root_contract.shares_root_context_with(contract) {
            return Err(LockBuildError::RootContextMismatch(package_version.clone()));
        }
    }

    let root_requirer = MvsRequirer::Root(root_version.clone());
    let mut consumed_edges = vec![false; graph.edges.len()];
    let mut root_fields = vec![Spanned {
        value: LockRootField::Targets(
            root_contract
                .root_targets()
                .iter()
                .map(|target| Spanned {
                    value: target.clone(),
                    span: 0..0,
                })
                .collect(),
        ),
        span: 0..0,
    }];
    append_contract_requirements(
        root_contract,
        &root_requirer,
        graph,
        &mut consumed_edges,
        &mut root_fields,
    )?;

    let mut packages = Vec::with_capacity(graph_dependencies.len());
    for package_version in graph_dependencies {
        let contract = &dependency_contracts[package_version];
        let requirer = MvsRequirer::Package(package_version.clone());
        let metadata = &package_metadata[package_version];
        if metadata
            .fields
            .iter()
            .any(|field| matches!(field, LockPackageField::Requires(_)))
        {
            return Err(LockBuildError::MetadataContainsRequirements(
                package_version.clone(),
            ));
        }
        let mut fields = metadata
            .fields
            .iter()
            .cloned()
            .map(|value| Spanned { value, span: 0..0 })
            .collect::<Vec<_>>();
        append_contract_requirements(contract, &requirer, graph, &mut consumed_edges, &mut fields)?;
        packages.push(Spanned {
            value: LockPackage {
                package: package_version.package().into(),
                version: package_version.version().clone(),
                fields,
            },
            span: 0..0,
        });
    }

    if let Some((_, edge)) = graph
        .edges
        .iter()
        .enumerate()
        .find(|(index, _)| !consumed_edges[*index])
    {
        return Err(LockBuildError::UnexpectedGraphEdge {
            requirer: edge.requirer.clone(),
            span: edge.span.clone(),
        });
    }

    let parsed = LockParseOutput {
        file: LockFile {
            format_version: Some(SUPPORTED_LOCK_FORMAT_VERSION),
            format_version_span: Some(0..0),
            root: Some(Spanned {
                value: LockRoot {
                    package: root_version.package().into(),
                    version: root_version.version().clone(),
                    fields: root_fields,
                },
                span: 0..0,
            }),
            packages,
            trivia: Vec::new(),
        },
        lexical_errors: Vec::new(),
        errors: Vec::new(),
    };

    let lock_validation = validate_lock(&parsed);
    if !lock_validation.is_valid() {
        return Err(LockBuildError::InvalidLock(lock_validation.errors));
    }
    let graph_validation =
        validate_lock_graph(&parsed).map_err(LockBuildError::LockGraphValidationInput)?;
    if !graph_validation.is_consistent() {
        return Err(LockBuildError::InvalidLockGraph(graph_validation));
    }

    Ok(parsed)
}

fn append_contract_requirements<F: LockRequirementField>(
    contract: &ResolverContract,
    requirer: &MvsRequirer,
    graph: &MvsSelectedGraph,
    consumed_edges: &mut [bool],
    output: &mut Vec<Spanned<F>>,
) -> Result<(), LockBuildError> {
    for requirement in contract.requirements() {
        let matching_edges = graph
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.requirer == *requirer && edge.span == requirement.span)
            .collect::<Vec<_>>();

        match &requirement.resolution {
            ResolverRequirementResolution::ExcludedByRootTargets => {
                if !matching_edges.is_empty() {
                    return Err(LockBuildError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                }
            }
            ResolverRequirementResolution::Included { named_version, pin } => {
                let [(index, edge)] = matching_edges.as_slice() else {
                    return Err(LockBuildError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                };
                if consumed_edges[*index]
                    || edge.named_target.package() != requirement.package.as_ref()
                    || edge.named_target.version() != named_version
                    || edge.target.package() != requirement.package.as_ref()
                    || edge.target.node() != edge.named_target.node()
                    || edge.requirement != requirement.version
                    || (matches!(
                        pin,
                        ResolverPinKind::Path | ResolverPinKind::RootPathOverride
                    ) && edge.target.version() != named_version)
                    || !requirement
                        .version
                        .matches_for_resolution(edge.target.version())
                    || !graph.dependencies.contains(&edge.target)
                {
                    return Err(LockBuildError::RequirementEdgeMismatch {
                        requirer: requirer.clone(),
                        span: requirement.span.clone(),
                    });
                }
                consumed_edges[*index] = true;
                let source = requirement.source.clone().ok_or_else(|| {
                    LockBuildError::MissingRequirementSource {
                        requirer: requirer.clone(),
                        package: requirement.package.clone(),
                        span: requirement.span.clone(),
                    }
                })?;
                let source_value = match source.value {
                    RequirementSource::Git { url, directory } => {
                        let Ok(url) = canonical_git_url(&url) else {
                            return Err(LockBuildError::InvalidRequirementSourceUrl {
                                requirer: requirer.clone(),
                                package: requirement.package.clone(),
                                span: requirement.span.clone(),
                            });
                        };
                        RequirementSource::Git {
                            url: url.into(),
                            directory: directory.filter(|directory| directory.as_ref() != "."),
                        }
                    }
                    RequirementSource::Path(path) => RequirementSource::Path(path),
                };
                let source = Some(Spanned {
                    value: source_value,
                    span: 0..0,
                });
                let only = requirement.only.as_ref().map(|targets| {
                    targets
                        .iter()
                        .map(|target| Spanned {
                            value: target.clone(),
                            span: 0..0,
                        })
                        .collect()
                });
                let grant = requirement.grant.as_ref().map(|capabilities| {
                    capabilities
                        .iter()
                        .map(|capability| Spanned {
                            value: capability.clone(),
                            span: 0..0,
                        })
                        .collect()
                });
                output.push(Spanned {
                    value: F::from_requirement(LockRequirement {
                        test: requirement.test,
                        package: requirement.package.clone(),
                        requirement: requirement.version.clone(),
                        selected: edge.target.version().clone(),
                        source,
                        only,
                        grant,
                    }),
                    span: requirement.span.clone(),
                });
            }
        }
    }
    Ok(())
}

trait LockRequirementField {
    fn from_requirement(requirement: LockRequirement) -> Self;
}

impl LockRequirementField for LockRootField {
    fn from_requirement(requirement: LockRequirement) -> Self {
        Self::Requires(Box::new(requirement))
    }
}

impl LockRequirementField for LockPackageField {
    fn from_requirement(requirement: LockRequirement) -> Self {
        Self::Requires(Box::new(requirement))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        LockBuildError, LockPackageMetadata, RootOnlyLockCreationError, build_resolved_lock,
        create_root_only_lock_if_missing,
    };
    use crate::lock::{LockPackageField, LockRootField, LockSource};
    use crate::lock_graph_validation::validate as validate_lock_graph;
    use crate::lock_serialization::serialize_canonical;
    use crate::lock_writer::{LockWriteOutcome, WorkspaceLockWriteError};
    use crate::manifest::MAX_PACKAGE_MANIFEST_BYTES;
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::mvs::{MvsPackageVersion, select};
    use crate::mvs_graph_validation::{
        check_selected_graph, check_selected_graph_with_root_cycles,
    };
    use crate::resolver_contract::{
        ResolverContract, ResolverContractContext, ResolverPathKey, build_resolver_contract,
    };
    use crate::validation::PackageValidationContext;
    use crate::version::Version;
    use crate::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    const ROOT_MANIFEST_BYTES: &[u8] = b"package acme:root @ 1.0.0\n\
                                         source \"src\"\n\
                                         targets { wasm32-core }\n";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-lock-build-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        root: ResolverContract,
        graph: crate::mvs_graph_validation::MvsSelectedGraph,
        contracts: BTreeMap<MvsPackageVersion, ResolverContract>,
        metadata: BTreeMap<MvsPackageVersion, LockPackageMetadata>,
        context: ResolverContractContext<'static>,
    }

    fn package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(
            package,
            Version::parse(version).expect("test version is valid"),
        )
        .expect("test package identity is valid")
    }

    fn root_only_contract() -> ResolverContract {
        let manifest = parse(ROOT_MANIFEST_BYTES);
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid");
        build_resolver_contract(&manifest, &context, PackageValidationContext::default())
            .expect("root contract is valid")
    }

    fn fixture() -> Fixture {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from path "../dep" only { wasm32-core } grant { }
}
requires test {
    acme:testdep @ ^1 from path "../testdep" only { wasm32-core }
}
"#,
        );
        let root_version = package_version("acme:root", "1.0.0");
        let dep_version = package_version("acme:dep", "1.2.0");
        let testdep_version = package_version("acme:testdep", "1.1.0");
        let path_packages = BTreeMap::from([
            (
                ResolverPathKey::root(root_version.clone(), "../dep"),
                dep_version.clone(),
            ),
            (
                ResolverPathKey::root(root_version, "../testdep"),
                testdep_version.clone(),
            ),
        ]);
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");

        let dep_manifest = parse_for_role(
            br#"package acme:dep @ 1.2.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:excluded @ ^1 from path "../excluded" only { wasm32-wasi }
}
"#,
            PackageRole::Dependency,
        );
        let testdep_manifest = parse_for_role(
            b"package acme:testdep @ 1.1.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
            PackageRole::Dependency,
        );
        let dep =
            build_resolver_contract(&dep_manifest, &context, PackageValidationContext::default())
                .expect("dependency contract is valid");
        let testdep = build_resolver_contract(
            &testdep_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("test dependency contract is valid");
        let contract_catalog = HashMap::from([
            (dep_version.clone(), dep.mvs.clone()),
            (testdep_version.clone(), testdep.mvs.clone()),
        ]);
        let selection = select(
            &root.mvs,
            &contract_catalog,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("MVS selection is complete");
        let graph =
            check_selected_graph(&root.mvs, &selection).expect("selected graph input is valid");
        let contracts = BTreeMap::from([
            (dep_version.clone(), dep),
            (testdep_version.clone(), testdep),
        ]);
        let metadata = BTreeMap::from([
            (
                dep_version,
                LockPackageMetadata {
                    fields: vec![LockPackageField::Source(LockSource::Path("../dep".into()))],
                },
            ),
            (
                testdep_version,
                LockPackageMetadata {
                    fields: vec![LockPackageField::Source(LockSource::Path(
                        "../testdep".into(),
                    ))],
                },
            ),
        ]);

        Fixture {
            root,
            graph,
            contracts,
            metadata,
            context: context.into_owned(),
        }
    }

    #[test]
    fn builds_lock_from_final_graph_and_content_metadata() {
        let fixture = fixture();
        let lock = build_resolved_lock(
            &fixture.root,
            &fixture.graph,
            &fixture.contracts,
            &fixture.metadata,
        )
        .expect("lock is valid");
        assert!(
            validate_lock_graph(&lock)
                .expect("lock-local validation passed")
                .is_consistent()
        );

        let canonical = serialize_canonical(&lock).expect("lock is serializable");
        assert_eq!(
            canonical,
            concat!(
                "lock 2\n\n",
                "root acme:root @ 1.0.0\n",
                "    targets { wasm32-core }\n",
                "    requires acme:dep @ ^1 -> 1.2.0 from path \"../dep\" only { wasm32-core } grant { }\n",
                "    requires test acme:testdep @ ^1 -> 1.1.0 from path \"../testdep\" only { wasm32-core }\n",
                "\n",
                "package acme:dep @ 1.2.0\n",
                "    source path \"../dep\"\n",
                "\n",
                "package acme:testdep @ 1.1.0\n",
                "    source path \"../testdep\"\n",
            )
        );
        assert!(
            !canonical.contains("acme:excluded"),
            "requirements excluded by root targets are not recorded"
        );
    }

    #[test]
    fn fails_closed_when_an_included_requirement_has_no_source_clause() {
        let mut fixture = fixture();
        fixture.root.requirements_mut_for_test()[0].source = None;

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::MissingRequirementSource {
                package,
                ..
            }) if package.as_ref() == "acme:dep"
        ));
    }

    #[test]
    fn rejects_an_invalid_git_url_in_a_forged_resolver_contract() {
        let mut fixture = fixture();
        fixture.root.requirements_mut_for_test()[0].source = Some(crate::manifest::Spanned {
            value: crate::manifest::RequirementSource::Git {
                url: "file:///tmp/dep".into(),
                directory: None,
            },
            span: 0..0,
        });

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::InvalidRequirementSourceUrl { .. })
        ));
    }

    #[test]
    fn persists_the_original_requirement_source_before_a_root_override() {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "HTTPS://Example.test:443/original/" }
overrides { acme:dep use path "../dep" }
"#,
        );
        let dep_version = package_version("acme:dep", "1.2.0");
        let root_path_overrides = BTreeMap::from([("acme:dep".into(), dep_version.clone())]);
        let path_packages = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root override contract is valid");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");
        let dep_manifest = parse_for_role(
            b"package acme:dep @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
            PackageRole::Dependency,
        );
        let dep =
            build_resolver_contract(&dep_manifest, &context, PackageValidationContext::default())
                .expect("dependency contract is valid");
        let graph = check_selected_graph(
            &root.mvs,
            &select(
                &root.mvs,
                &HashMap::from([(dep_version.clone(), dep.mvs.clone())]),
                &HashMap::new(),
                &context.root_override_pins(),
            )
            .expect("root override selection is valid"),
        )
        .expect("selected graph is valid");
        let contracts = BTreeMap::from([(dep_version.clone(), dep)]);
        let metadata = BTreeMap::from([(
            dep_version,
            LockPackageMetadata {
                fields: vec![LockPackageField::Source(LockSource::Path("../dep".into()))],
            },
        )]);

        let lock = build_resolved_lock(&root, &graph, &contracts, &metadata)
            .expect("lock preserves the original requirement source");
        let requirement = lock
            .file()
            .root
            .as_ref()
            .unwrap()
            .value
            .fields
            .iter()
            .find_map(|field| match &field.value {
                LockRootField::Requires(requirement) => Some(requirement),
                LockRootField::Targets(_) => None,
            })
            .unwrap();
        assert_eq!(
            requirement.source.as_ref().unwrap().value,
            crate::manifest::RequirementSource::Git {
                url: "https://example.test/original".into(),
                directory: None,
            }
        );
        assert!(
            serialize_canonical(&lock)
                .unwrap()
                .contains("from git \"https://example.test/original\"")
        );
    }

    #[test]
    fn creates_a_canonical_lock_for_a_root_without_active_requirements() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("sai.package"), ROOT_MANIFEST_BYTES).unwrap();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        let outcome = create_root_only_lock_if_missing(&guard, &root_only_contract())
            .expect("root-only lock is created");
        let expected = b"lock 2\n\nroot acme:root @ 1.0.0\n    targets { wasm32-core }\n";
        assert_eq!(
            outcome,
            LockWriteOutcome::Written {
                bytes: expected.len()
            }
        );
        assert_eq!(fs::read(directory.0.join("sai.lock")).unwrap(), expected);
    }

    #[test]
    fn root_only_lock_creation_rejects_active_requirements_without_writing() {
        let directory = TestDirectory::new();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let root = fixture().root;

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root),
            Err(RootOnlyLockCreationError::RootHasRequirements(_))
        ));
        assert!(!directory.0.join("sai.lock").exists());
    }

    #[test]
    fn root_only_lock_creation_rejects_a_shared_guard() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("sai.package"), ROOT_MANIFEST_BYTES).unwrap();
        let guard = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root_only_contract()),
            Err(RootOnlyLockCreationError::Write(
                WorkspaceLockWriteError::WorkspaceLockNotExclusive(WorkspaceLockMode::Shared)
            ))
        ));
        assert!(!directory.0.join("sai.lock").exists());
    }

    #[test]
    fn root_only_lock_creation_does_not_replace_an_existing_lock() {
        let directory = TestDirectory::new();
        let path = directory.0.join("sai.lock");
        fs::write(directory.0.join("sai.package"), ROOT_MANIFEST_BYTES).unwrap();
        fs::write(&path, b"preserve existing lock").unwrap();
        let canonical_path = fs::canonicalize(&directory.0).unwrap().join("sai.lock");
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root_only_contract()),
            Err(RootOnlyLockCreationError::Write(
                WorkspaceLockWriteError::DestinationAlreadyExists(existing)
            )) if existing == canonical_path
        ));
        assert_eq!(fs::read(path).unwrap(), b"preserve existing lock");
    }

    #[test]
    fn root_only_lock_creation_rejects_a_contract_for_another_workspace() {
        let directory = TestDirectory::new();
        fs::write(
            directory.0.join("sai.package"),
            b"package acme:other @ 9.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .unwrap();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root_only_contract()),
            Err(RootOnlyLockCreationError::RootManifestMismatch { .. })
        ));
        assert!(!directory.0.join("sai.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn root_only_lock_creation_does_not_follow_a_manifest_symlink() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.0.join("manifest-target");
        fs::write(&target, ROOT_MANIFEST_BYTES).unwrap();
        symlink(&target, directory.0.join("sai.package")).unwrap();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root_only_contract()),
            Err(RootOnlyLockCreationError::RootManifestRead { .. })
        ));
        assert!(!directory.0.join("sai.lock").exists());
    }

    #[test]
    fn root_only_lock_creation_enforces_the_manifest_size_limit() {
        let directory = TestDirectory::new();
        fs::write(
            directory.0.join("sai.package"),
            vec![b' '; MAX_PACKAGE_MANIFEST_BYTES + 1],
        )
        .unwrap();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            create_root_only_lock_if_missing(&guard, &root_only_contract()),
            Err(RootOnlyLockCreationError::RootManifestTooLarge(_))
        ));
        assert!(!directory.0.join("sai.lock").exists());
    }

    #[test]
    fn rejects_missing_dependency_metadata() {
        let mut fixture = fixture();
        fixture.metadata.pop_first();

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::PackageMetadataMismatch { .. })
        ));
    }

    #[test]
    fn rejects_requirement_fields_in_content_metadata() {
        let mut fixture = fixture();
        let package = fixture.metadata.keys().next().unwrap().clone();
        fixture
            .metadata
            .get_mut(&package)
            .unwrap()
            .fields
            .push(LockPackageField::Requires(Box::new(
                crate::lock::LockRequirement {
                    test: false,
                    package: "acme:extra".into(),
                    requirement: crate::version_req::VersionReq::parse("^1")
                        .expect("test requirement is valid"),
                    selected: Version::parse("1.0.0").expect("test version is valid"),
                    source: None,
                    only: None,
                    grant: None,
                },
            )));

        assert_eq!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::MetadataContainsRequirements(package))
        );
    }

    #[test]
    fn rejects_graph_edges_that_do_not_match_contract_requirements() {
        let mut fixture = fixture();
        fixture.graph.edges[0].target = package_version("acme:dep", "1.3.0");

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::RequirementEdgeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_path_pin_changed_to_another_satisfying_version() {
        let mut fixture = fixture();
        let pinned_version = package_version("acme:dep", "1.2.0");
        let forged_version = package_version("acme:dep", "1.3.0");
        let forged_manifest = parse_for_role(
            br#"package acme:dep @ 1.3.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:excluded @ ^1 from path "../excluded" only { wasm32-wasi }
}
"#,
            PackageRole::Dependency,
        );
        let forged_contract = build_resolver_contract(
            &forged_manifest,
            &fixture.context,
            PackageValidationContext::default(),
        )
        .expect("forged graph's dependency contract is otherwise valid");

        fixture.contracts.remove(&pinned_version);
        fixture
            .contracts
            .insert(forged_version.clone(), forged_contract);
        let metadata = fixture.metadata.remove(&pinned_version).unwrap();
        fixture.metadata.insert(forged_version.clone(), metadata);
        fixture.graph.dependencies.remove(&pinned_version);
        fixture.graph.dependencies.insert(forged_version.clone());
        let edge = fixture
            .graph
            .edges
            .iter_mut()
            .find(|edge| edge.target.package() == "acme:dep")
            .expect("graph contains the pinned dependency edge");
        edge.target = forged_version;

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::RequirementEdgeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_root_path_override_changed_to_another_satisfying_version() {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/original" }
overrides { acme:dep use path "../dep" }
"#,
        );
        let dep_version = package_version("acme:dep", "1.2.0");
        let root_path_overrides = BTreeMap::from([("acme:dep".into(), dep_version.clone())]);
        let empty_paths = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &empty_paths,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root path override is valid");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");
        let dep_manifest = parse_for_role(
            br#"package acme:dep @ 1.2.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let dep =
            build_resolver_contract(&dep_manifest, &context, PackageValidationContext::default())
                .expect("dependency contract is valid");
        let selection = select(
            &root.mvs,
            &HashMap::from([(dep_version.clone(), dep.mvs.clone())]),
            &HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("root path override selects the pinned version");
        let mut graph =
            check_selected_graph(&root.mvs, &selection).expect("selected graph is valid");

        let forged_version = package_version("acme:dep", "1.3.0");
        let forged_manifest = parse_for_role(
            br#"package acme:dep @ 1.3.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let forged_contract = build_resolver_contract(
            &forged_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("forged graph's dependency contract is otherwise valid");
        graph.dependencies.remove(&dep_version);
        graph.dependencies.insert(forged_version.clone());
        graph.edges[0].target = forged_version.clone();
        let contracts = BTreeMap::from([(forged_version.clone(), forged_contract)]);
        let metadata = BTreeMap::from([(
            forged_version,
            LockPackageMetadata {
                fields: vec![LockPackageField::Source(LockSource::Path("../dep".into()))],
            },
        )]);

        assert!(matches!(
            build_resolved_lock(&root, &graph, &contracts, &metadata),
            Err(LockBuildError::RequirementEdgeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_path_pin_metadata_changed_to_an_unpinned_edge() {
        let mut fixture = fixture();
        let root_version = fixture.root.manifest_package_version().clone();
        let pinned_version = package_version("acme:dep", "1.2.0");
        let satisfying_version = package_version("acme:dep", "1.3.0");
        let requirement = &mut fixture.root.requirements_mut_for_test()[0];
        requirement.resolution =
            crate::resolver_contract::ResolverRequirementResolution::Included {
                named_version: pinned_version.version().clone(),
                pin: crate::resolver_contract::ResolverPinKind::Minimum,
            };
        fixture.graph.dependencies.remove(&pinned_version);
        fixture
            .graph
            .dependencies
            .insert(satisfying_version.clone());
        fixture.graph.edges[0].target = satisfying_version;

        assert_eq!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::InconsistentRootContract(root_version))
        );
    }

    #[test]
    fn rejects_contracts_built_for_a_different_root_context() {
        let mut fixture = fixture();
        let independent_root = parse(
            b"package acme:other-root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let empty_paths = BTreeMap::new();
        let empty_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &independent_root,
            &empty_paths,
            &empty_overrides,
            PackageValidationContext::default(),
        )
        .expect("other root manifest is valid");
        let package = fixture.contracts.keys().next().unwrap().clone();
        let manifest = parse_for_role(
            b"package acme:dep @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
            PackageRole::Dependency,
        );
        fixture.contracts.insert(
            package.clone(),
            build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                .expect("dependency contract is valid"),
        );

        assert!(matches!(
            build_resolved_lock(
                &fixture.root,
                &fixture.graph,
                &fixture.contracts,
                &fixture.metadata,
            ),
            Err(LockBuildError::DependencyContractMismatch(_))
                | Err(LockBuildError::RootContextMismatch(_))
        ));
    }

    #[test]
    fn rejects_an_mvs_root_replaced_after_manifest_adaptation() {
        let root_manifest = parse(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let empty_paths = BTreeMap::new();
        let empty_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &empty_paths,
            &empty_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid");
        let mut root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");
        let forged_version = package_version("acme:forged", "1.0.0");
        root.mvs = crate::mvs::MvsContract::new(forged_version.clone(), Vec::new())
            .expect("empty forged MVS contract is structurally valid");
        let selection = select(&root.mvs, &HashMap::new(), &HashMap::new(), &HashMap::new())
            .expect("forged selection is complete");
        let graph = check_selected_graph(&root.mvs, &selection)
            .expect("forged graph input is structurally valid");

        assert_eq!(
            build_resolved_lock(&root, &graph, &BTreeMap::new(), &BTreeMap::new(),),
            Err(LockBuildError::MvsPackageVersionMismatch {
                manifest: Box::new(root_version),
                mvs: Box::new(forged_version),
            })
        );
    }

    #[test]
    fn rejects_root_cycle_edges_even_if_the_cycle_finding_was_discarded() {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
        );
        let root_version = package_version("acme:root", "1.0.0");
        let dep_version = package_version("acme:dep", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::root(root_version.clone(), "../dep"),
            dep_version.clone(),
        )]);
        let root_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");
        let dep_manifest = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:root @ ^1 from path "../root" }
"#,
            PackageRole::Dependency,
        );
        let dependency_paths = BTreeMap::from([(
            ResolverPathKey::package(dep_version.clone(), "../root"),
            root_version,
        )]);
        let dependency_context = context
            .with_path_packages(&dependency_paths)
            .expect("dependency root path is part of the shared context");
        let dep = build_resolver_contract(
            &dep_manifest,
            &dependency_context,
            PackageValidationContext::default(),
        )
        .expect("dependency contract is valid");
        let selection = select(
            &root.mvs,
            &HashMap::from([(dep_version.clone(), dep.mvs.clone())]),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("selection reports only the known root cycle");
        let (graph, root_cycles) = check_selected_graph_with_root_cycles(&root.mvs, &selection)
            .expect("root-cycle reporting preserves the otherwise usable graph");
        assert_eq!(root_cycles.len(), 1);
        assert!(graph.is_valid());

        let contracts = BTreeMap::from([(dep_version.clone(), dep)]);
        let metadata = BTreeMap::from([(
            dep_version,
            LockPackageMetadata {
                fields: vec![LockPackageField::Source(LockSource::Path("../dep".into()))],
            },
        )]);
        assert!(matches!(
            build_resolved_lock(&root, &graph, &contracts, &metadata),
            Err(LockBuildError::RequirementEdgeMismatch { .. })
        ));
    }

    #[test]
    fn records_selected_version_above_a_requirement_minimum() {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/dep" }
"#,
        );
        let root_overrides = BTreeMap::new();
        let empty_paths = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &empty_paths,
            &root_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid");
        let root = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root contract is valid");
        let dep_version = package_version("acme:dep", "1.2.0");
        let dep_manifest = parse_for_role(
            br#"package acme:dep @ 1.2.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let dep =
            build_resolver_contract(&dep_manifest, &context, PackageValidationContext::default())
                .expect("dependency contract is valid");
        let minimum_manifest = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let minimum_contract = build_resolver_contract(
            &minimum_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("minimum contract is valid");
        let floors = HashMap::from([(dep_version.node(), dep_version.version().clone())]);
        let selection = select(
            &root.mvs,
            &HashMap::from([
                (
                    package_version("acme:dep", "1.0.0"),
                    minimum_contract.mvs.clone(),
                ),
                (dep_version.clone(), dep.mvs.clone()),
            ]),
            &floors,
            &HashMap::new(),
        )
        .expect("lock floor selects the audited version");
        let graph = check_selected_graph(&root.mvs, &selection).expect("selected graph is valid");
        assert_eq!(
            graph.edges[0].named_target.version(),
            &Version::parse("1.0.0").expect("test version is valid")
        );

        let contracts = BTreeMap::from([(dep_version.clone(), dep)]);
        let metadata = BTreeMap::from([(
            dep_version,
            LockPackageMetadata {
                fields: vec![
                    LockPackageField::Source(LockSource::Git("https://example.test/dep".into())),
                    LockPackageField::Commit("a".repeat(40).into()),
                    LockPackageField::Checksum(format!("sai1:{}", "b".repeat(64)).into()),
                    LockPackageField::Capabilities(Vec::new()),
                ],
            },
        )]);
        let lock = build_resolved_lock(&root, &graph, &contracts, &metadata)
            .expect("lock represents the selected version");
        let canonical = serialize_canonical(&lock).expect("lock is canonicalizable");
        assert!(canonical.contains("requires acme:dep @ ^1 -> 1.2.0"));
    }
}
