//! Adapts validated package manifests into resolver inputs and edge metadata.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use crate::lock_validation::canonical_git_url;
use crate::manifest::{
    Override, OverrideSource, PackageDeclaration, PackageParseError, PackageParseOutput,
    PackageParseWarning, PackageRole, Requirement, RequirementSource, Spanned,
};
use crate::mvs::{MvsContract, MvsEdge, MvsInputError, MvsNode, MvsPackageVersion};
use crate::validation::{
    PackageValidationContext, PackageValidationError, PackageValidationWarning,
    validate as validate_package,
};
use crate::version::Version;

/// A path requirement's resolved package, keyed by the requiring package and
/// the source spelling from its manifest.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResolverPathKey {
    pub requiring_package: MvsPackageVersion,
    pub path: Box<str>,
}

impl ResolverPathKey {
    pub fn new(requiring_package: MvsPackageVersion, path: impl Into<Box<str>>) -> Self {
        Self {
            requiring_package,
            path: path.into(),
        }
    }
}

/// Root graph inputs that are shared while adapting the root and its
/// dependencies.
///
/// The path maps contain concrete package versions whose manifests have
/// already been read and whose declared identity has been checked by the
/// caller. Direct path requirements are keyed by requiring package version
/// and source spelling. Root `use path` overrides are keyed by the overridden
/// identity. Root Git overrides are read from the validated root manifest.
#[derive(Clone, Debug)]
pub struct ResolverContractContext<'a> {
    root_manifest: PackageParseOutput,
    root_context_binding: Arc<u8>,
    root_package: MvsPackageVersion,
    root_targets: BTreeSet<Box<str>>,
    path_packages: &'a BTreeMap<ResolverPathKey, MvsPackageVersion>,
    root_path_overrides: &'a BTreeMap<Box<str>, MvsPackageVersion>,
    root_git_override_ids: BTreeSet<Box<str>>,
    root_overrides: Vec<Spanned<Override>>,
}

impl<'a> ResolverContractContext<'a> {
    /// Builds shared resolver context from a syntax-valid, semantically valid
    /// root manifest and the concrete packages reached by its path overrides.
    ///
    /// A version is required for every root path override because the
    /// override's compatibility line determines which graph edges it affects.
    pub fn from_root_manifest(
        root_manifest: &PackageParseOutput,
        path_packages: &'a BTreeMap<ResolverPathKey, MvsPackageVersion>,
        root_path_overrides: &'a BTreeMap<Box<str>, MvsPackageVersion>,
        validation_context: PackageValidationContext<'_>,
    ) -> Result<Self, ResolverContractBuildError> {
        if root_manifest.role() != PackageRole::Root {
            return Err(ResolverContractBuildError::RootManifestRoleMismatch(
                root_manifest.role(),
            ));
        }
        validate_manifest(root_manifest, validation_context)?;
        let header = root_manifest
            .file()
            .header
            .as_ref()
            .ok_or(ResolverContractBuildError::MissingPackageHeader)?;
        let root_package = MvsPackageVersion::new(
            header.value.identity.to_string(),
            header.value.version.clone(),
        )
        .map_err(ResolverContractBuildError::MvsInput)?;

        let root_targets = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| match &declaration.value {
                PackageDeclaration::Targets(targets) => Some(
                    targets
                        .iter()
                        .map(|target| target.value.clone())
                        .collect::<BTreeSet<_>>(),
                ),
                _ => None,
            })
            .ok_or(ResolverContractBuildError::MissingRootTargets)?;

        let mut root_path_override_ids = BTreeSet::new();
        let mut root_path_override_spans = BTreeMap::new();
        let mut root_git_override_ids = BTreeSet::new();
        let mut root_overrides = Vec::new();
        for declaration in &root_manifest.file().declarations {
            let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                continue;
            };
            for item in overrides {
                match &item.value.source {
                    OverrideSource::Path(_) => {
                        root_path_override_ids.insert(item.value.package.clone());
                        root_path_override_spans
                            .entry(item.value.package.clone())
                            .or_insert_with(|| item.span.clone());
                    }
                    OverrideSource::Git { .. } => {
                        root_git_override_ids.insert(item.value.package.clone());
                    }
                    OverrideSource::Artifact => {}
                }
                root_overrides.push(item.clone());
            }
        }
        for (package, package_version) in root_path_overrides {
            if !root_path_override_ids.contains(package.as_ref()) {
                return Err(
                    ResolverContractBuildError::UnexpectedRootPathOverrideVersion(package.clone()),
                );
            }
            if package_version.package() != package.as_ref() {
                return Err(ResolverContractBuildError::PathPackageIdentityMismatch {
                    expected: package.clone(),
                    found: package_version.package().into(),
                    span: root_path_override_spans[package].clone(),
                });
            }
        }
        for package in &root_path_override_ids {
            if !root_path_overrides.contains_key(package) {
                return Err(ResolverContractBuildError::MissingRootPathOverrideVersion {
                    package: package.clone(),
                    span: root_path_override_spans[package].clone(),
                });
            }
        }

        Ok(Self {
            root_manifest: root_manifest.clone(),
            root_context_binding: Arc::new(0),
            root_package,
            root_targets,
            path_packages,
            root_path_overrides,
            root_git_override_ids,
            root_overrides,
        })
    }

    pub const fn root_package(&self) -> &MvsPackageVersion {
        &self.root_package
    }

    pub fn root_targets(&self) -> &BTreeSet<Box<str>> {
        &self.root_targets
    }

    pub fn root_overrides(&self) -> &[Spanned<Override>] {
        &self.root_overrides
    }

    pub(crate) fn is_bound_to_root_contract(&self, contract: &ResolverContract) -> bool {
        contract
            .context_binding
            .as_ref()
            .is_some_and(|binding| Arc::ptr_eq(binding, &self.root_context_binding))
    }

    /// Returns the root path pins in the form consumed by [`crate::mvs::select`].
    pub fn root_override_pins(&self) -> HashMap<MvsNode, Version> {
        self.root_path_overrides
            .values()
            .map(|package_version| (package_version.node(), package_version.version().clone()))
            .collect()
    }
}

/// The MVS selection effect of one retained package requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverRequirementResolution {
    Included {
        named_version: Version,
        pin: ResolverPinKind,
    },
    ExcludedByRootTargets,
}

/// Why an included requirement names a concrete version for MVS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverPinKind {
    Minimum,
    Path,
    RootPathOverride,
}

/// A manifest requirement with its resolver and later graph metadata intact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverRequirement {
    pub package: Box<str>,
    pub version: crate::version_req::VersionReq,
    pub source: Option<Spanned<RequirementSource>>,
    pub only: Option<BTreeSet<Box<str>>>,
    pub grant: Option<BTreeSet<Box<str>>>,
    pub test: bool,
    pub span: Range<usize>,
    pub resolution: ResolverRequirementResolution,
}

/// A validated manifest adapted for MVS and subsequent graph processing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContract {
    pub mvs: MvsContract,
    role: PackageRole,
    /// Root requirements include test edges. Dependency test requirements are
    /// omitted as required by resolver §2.5.1.
    pub requirements: Vec<ResolverRequirement>,
    pub parse_warnings: Vec<PackageParseWarning>,
    pub validation_warnings: Vec<PackageValidationWarning>,
    context_binding: Option<Arc<u8>>,
}

impl ResolverContract {
    pub const fn role(&self) -> PackageRole {
        self.role
    }
}

/// A source clause after the normalization required for contract comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverContractSourceClause {
    Git {
        /// The canonical URL defined by distribution §4.2.2.
        canonical_url: Box<str>,
        /// `None` and `Some(".")` both identify the repository root.
        directory: Option<Box<str>>,
    },
    /// A path clause retains its validated manifest spelling. Its resolved
    /// filesystem or Git-tree identity is checked by source loading.
    Path {
        relative_to: MvsPackageVersion,
        path: Box<str>,
    },
}

/// One target-included dependency edge retained for divergent-source checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContractEdge {
    pub package: Box<str>,
    pub requirement: crate::version_req::VersionReq,
    /// Captures the concrete version named by a path pin. For ordinary
    /// requirements this equals `requirement.minimum()`.
    pub named_version: Version,
    pub source: Option<ResolverContractSourceClause>,
    pub only: Option<BTreeSet<Box<str>>>,
    pub grant: Option<BTreeSet<Box<str>>>,
}

/// Field-by-field comparison of two contracts for the same package version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContractEdgeComparison {
    package_version: MvsPackageVersion,
    expected_edges: Vec<ResolverContractEdge>,
    actual_edges: Vec<ResolverContractEdge>,
}

/// Whether the edge comparison is conclusive for every source clause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverContractComparisonStatus {
    Equivalent,
    Different,
    RequiresSourceBoundPathResolution,
    PathResolutionFailed,
}

/// Which contract supplied a path clause during source comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverContractSourceSide {
    Expected,
    Actual,
}

/// Input needed to resolve one dependency-relative path clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContractPathSourceRequest {
    pub side: ResolverContractSourceSide,
    pub requiring_package: MvsPackageVersion,
    pub package: Box<str>,
    pub path: Box<str>,
}

/// A path clause could not be resolved against its supplying source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContractPathResolutionFailure<E> {
    pub request: ResolverContractPathSourceRequest,
    pub error: Box<E>,
}

/// The result after attempting every source-bound path in both contracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverContractResolvedPathComparison<E> {
    status: ResolverContractComparisonStatus,
    failures: Vec<ResolverContractPathResolutionFailure<E>>,
}

impl<E> ResolverContractResolvedPathComparison<E> {
    pub const fn status(&self) -> ResolverContractComparisonStatus {
        self.status
    }

    pub fn failures(&self) -> &[ResolverContractPathResolutionFailure<E>] {
        &self.failures
    }
}

impl ResolverContractEdgeComparison {
    pub const fn package_version(&self) -> &MvsPackageVersion {
        &self.package_version
    }

    pub fn expected_edges(&self) -> &[ResolverContractEdge] {
        &self.expected_edges
    }

    pub fn actual_edges(&self) -> &[ResolverContractEdge] {
        &self.actual_edges
    }

    pub fn status(&self) -> ResolverContractComparisonStatus {
        if !same_except_source_bound_path(&self.expected_edges, &self.actual_edges) {
            return ResolverContractComparisonStatus::Different;
        }
        if has_path_source(&self.expected_edges) || has_path_source(&self.actual_edges) {
            return ResolverContractComparisonStatus::RequiresSourceBoundPathResolution;
        }
        ResolverContractComparisonStatus::Equivalent
    }

    pub fn equivalent(&self) -> bool {
        self.status() == ResolverContractComparisonStatus::Equivalent
    }

    /// Completes comparison after resolving every path clause in both trees.
    ///
    /// The callback must return a stable identity for the complete resolved
    /// source: local paths after lexical and symlink resolution, or paths
    /// inside Git sources with the canonical repository, requiring commit,
    /// and normalized tree path. The `side` identifies which source supplied
    /// the requiring package, so equal manifest spellings cannot accidentally
    /// reuse an identity from the other contract. The callback runs for every
    /// path on both sides, even after an identity mismatch or another lookup
    /// failure, so independent path errors are retained.
    pub fn compare_with_resolved_paths<K, E>(
        &self,
        mut resolve_path: impl FnMut(&ResolverContractPathSourceRequest) -> Result<K, E>,
    ) -> ResolverContractResolvedPathComparison<E>
    where
        K: Eq,
    {
        let mut failures = Vec::new();
        let expected_keys = resolve_path_edges(
            &self.expected_edges,
            ResolverContractSourceSide::Expected,
            &mut resolve_path,
            &mut failures,
        );
        let actual_keys = resolve_path_edges(
            &self.actual_edges,
            ResolverContractSourceSide::Actual,
            &mut resolve_path,
            &mut failures,
        );
        let mut different =
            !same_except_source_bound_path(&self.expected_edges, &self.actual_edges);
        for (index, (expected, actual)) in self
            .expected_edges
            .iter()
            .zip(&self.actual_edges)
            .enumerate()
        {
            if expected.package != actual.package
                || !matches!(
                    (&expected.source, &actual.source),
                    (
                        Some(ResolverContractSourceClause::Path { .. }),
                        Some(ResolverContractSourceClause::Path { .. })
                    )
                )
            {
                continue;
            }
            if let (Some(expected_key), Some(actual_key)) =
                (&expected_keys[index], &actual_keys[index])
            {
                different |= expected_key != actual_key;
            }
        }
        let status = if different {
            ResolverContractComparisonStatus::Different
        } else if !failures.is_empty() {
            ResolverContractComparisonStatus::PathResolutionFailed
        } else {
            ResolverContractComparisonStatus::Equivalent
        };
        ResolverContractResolvedPathComparison { status, failures }
    }
}

fn resolve_path_edges<K, E>(
    edges: &[ResolverContractEdge],
    side: ResolverContractSourceSide,
    resolve_path: &mut impl FnMut(&ResolverContractPathSourceRequest) -> Result<K, E>,
    failures: &mut Vec<ResolverContractPathResolutionFailure<E>>,
) -> Vec<Option<K>> {
    edges
        .iter()
        .map(|edge| {
            let Some(ResolverContractSourceClause::Path { relative_to, path }) = &edge.source
            else {
                return None;
            };
            let request = ResolverContractPathSourceRequest {
                side,
                requiring_package: relative_to.clone(),
                package: edge.package.clone(),
                path: path.clone(),
            };
            match resolve_path(&request) {
                Ok(key) => Some(key),
                Err(error) => {
                    failures.push(ResolverContractPathResolutionFailure {
                        request,
                        error: Box::new(error),
                    });
                    None
                }
            }
        })
        .collect()
}

fn same_except_source_bound_path(
    expected: &[ResolverContractEdge],
    actual: &[ResolverContractEdge],
) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            expected.package == actual.package
                && expected.requirement == actual.requirement
                && expected.named_version == actual.named_version
                && expected.only == actual.only
                && expected.grant == actual.grant
                && match (&expected.source, &actual.source) {
                    (
                        Some(ResolverContractSourceClause::Path { .. }),
                        Some(ResolverContractSourceClause::Path { .. }),
                    ) => true,
                    (expected, actual) => expected == actual,
                }
        })
}

fn has_path_source(edges: &[ResolverContractEdge]) -> bool {
    edges.iter().any(|edge| {
        matches!(
            &edge.source,
            Some(ResolverContractSourceClause::Path { .. })
        )
    })
}

/// The contracts could not be compared because their inputs were inconsistent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverContractComparisonError {
    DependencyRoleMismatch {
        expected: PackageRole,
        actual: PackageRole,
    },
    PackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        actual: Box<MvsPackageVersion>,
    },
    ContractContextMismatch(Box<MvsPackageVersion>),
    ContractMetadataMismatch(Box<MvsPackageVersion>),
    RootContextMismatch,
    InvalidGitUrl {
        package: Box<str>,
        url: Box<str>,
        span: Range<usize>,
    },
    InvalidGitDirectory {
        package: Box<str>,
        directory: Box<str>,
        span: Range<usize>,
    },
}

/// Compares the resolver-relevant included edges of two source-bound contracts.
///
/// This deliberately ignores declaration order, source spans, and parse
/// warnings. Dependency test requirements are absent from resolver contracts,
/// and requirements filtered by the root's targets are excluded. Git URLs are
/// compared in canonical form; path clauses retain their manifest spelling
/// because this pure comparison has no filesystem or Git-tree access. The
/// result reports `RequiresSourceBoundPathResolution` when any path clause
/// remains, so callers cannot accept it as fully equivalent before resolving
/// that path against the relevant source tree.
pub fn compare_resolver_contract_edges(
    expected: &ResolverContract,
    expected_context: &ResolverContractContext<'_>,
    actual: &ResolverContract,
    actual_context: &ResolverContractContext<'_>,
) -> Result<ResolverContractEdgeComparison, ResolverContractComparisonError> {
    if expected.role != PackageRole::Dependency || actual.role != PackageRole::Dependency {
        return Err(ResolverContractComparisonError::DependencyRoleMismatch {
            expected: expected.role,
            actual: actual.role,
        });
    }
    if expected.mvs.package_version() != actual.mvs.package_version() {
        return Err(ResolverContractComparisonError::PackageVersionMismatch {
            expected: Box::new(expected.mvs.package_version().clone()),
            actual: Box::new(actual.mvs.package_version().clone()),
        });
    }
    let package_version = expected.mvs.package_version().clone();
    if !expected_context.is_bound_to_root_contract(expected)
        || !actual_context.is_bound_to_root_contract(actual)
    {
        return Err(ResolverContractComparisonError::ContractContextMismatch(
            Box::new(package_version),
        ));
    }
    if !same_root_resolution_context(expected_context, actual_context) {
        return Err(ResolverContractComparisonError::RootContextMismatch);
    }
    if !contract_edges_match_mvs(expected) || !contract_edges_match_mvs(actual) {
        return Err(ResolverContractComparisonError::ContractMetadataMismatch(
            Box::new(package_version),
        ));
    }

    let expected_edges = comparable_contract_edges(expected, expected_context)?;
    let actual_edges = comparable_contract_edges(actual, actual_context)?;
    Ok(ResolverContractEdgeComparison {
        package_version,
        expected_edges,
        actual_edges,
    })
}

fn contract_edges_match_mvs(contract: &ResolverContract) -> bool {
    let mut included_requirements = contract
        .requirements
        .iter()
        .filter(|requirement| {
            matches!(
                requirement.resolution,
                ResolverRequirementResolution::Included { .. }
            )
        })
        .collect::<Vec<_>>();
    included_requirements
        .sort_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
    if included_requirements.len() != contract.mvs.edges().len() {
        return false;
    }
    included_requirements
        .into_iter()
        .zip(contract.mvs.edges())
        .all(|(requirement, edge)| {
            let ResolverRequirementResolution::Included { named_version, .. } =
                &requirement.resolution
            else {
                return false;
            };
            requirement.package.as_ref() == edge.package()
                && requirement.version == *edge.requirement()
                && requirement.span == edge.span()
                && named_version == edge.named_version()
        })
}

fn same_root_resolution_context(
    left: &ResolverContractContext<'_>,
    right: &ResolverContractContext<'_>,
) -> bool {
    left.root_manifest == right.root_manifest
        && left.root_package == right.root_package
        && left.root_targets == right.root_targets
        && left.root_path_overrides == right.root_path_overrides
        && left.root_overrides == right.root_overrides
        && root_path_packages(left) == root_path_packages(right)
}

fn root_path_packages(
    context: &ResolverContractContext<'_>,
) -> BTreeMap<ResolverPathKey, MvsPackageVersion> {
    context
        .path_packages
        .iter()
        .filter_map(|(key, package_version)| {
            (key.requiring_package == context.root_package)
                .then_some((key.clone(), package_version.clone()))
        })
        .collect()
}

fn comparable_contract_edges(
    contract: &ResolverContract,
    context: &ResolverContractContext<'_>,
) -> Result<Vec<ResolverContractEdge>, ResolverContractComparisonError> {
    let requiring_package = contract.mvs.package_version();
    let mut edges = Vec::new();
    for requirement in &contract.requirements {
        let ResolverRequirementResolution::Included { named_version, .. } = &requirement.resolution
        else {
            continue;
        };
        let source =
            canonical_requirement_source(requirement, named_version, requiring_package, context)?;
        edges.push(ResolverContractEdge {
            package: requirement.package.clone(),
            requirement: requirement.version.clone(),
            named_version: named_version.clone(),
            source,
            only: requirement.only.clone(),
            grant: requirement.grant.clone(),
        });
    }
    edges.sort_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
    Ok(edges)
}

fn canonical_requirement_source(
    requirement: &ResolverRequirement,
    named_version: &Version,
    requiring_package: &MvsPackageVersion,
    context: &ResolverContractContext<'_>,
) -> Result<Option<ResolverContractSourceClause>, ResolverContractComparisonError> {
    for item in &context.root_overrides {
        if item.value.package.as_ref() != requirement.package.as_ref() {
            continue;
        }
        match &item.value.source {
            OverrideSource::Git { url, directory } => {
                return canonical_source_clause(
                    requirement,
                    RequirementSource::Git {
                        url: url.clone(),
                        directory: directory.clone(),
                    },
                    item.span.clone(),
                    context.root_package.clone(),
                )
                .map(Some);
            }
            OverrideSource::Path(path)
                if context
                    .root_path_overrides
                    .get(requirement.package.as_ref())
                    .is_some_and(|package_version| {
                        package_version.version().compatibility_line()
                            == named_version.compatibility_line()
                    }) =>
            {
                return canonical_source_clause(
                    requirement,
                    RequirementSource::Path(path.clone()),
                    item.span.clone(),
                    context.root_package.clone(),
                )
                .map(Some);
            }
            OverrideSource::Artifact | OverrideSource::Path(_) => {}
        }
    }

    requirement
        .source
        .as_ref()
        .map(|spanned| {
            canonical_source_clause(
                requirement,
                spanned.value.clone(),
                spanned.span.clone(),
                requiring_package.clone(),
            )
        })
        .transpose()
}

fn canonical_source_clause(
    requirement: &ResolverRequirement,
    source: RequirementSource,
    span: Range<usize>,
    path_base: MvsPackageVersion,
) -> Result<ResolverContractSourceClause, ResolverContractComparisonError> {
    match source {
        RequirementSource::Git { url, directory } => {
            let canonical_url = canonical_git_url(&url).map_err(|()| {
                ResolverContractComparisonError::InvalidGitUrl {
                    package: requirement.package.clone(),
                    url: url.clone(),
                    span: span.clone(),
                }
            })?;
            let directory = canonical_git_directory(directory.as_deref()).map_err(|()| {
                ResolverContractComparisonError::InvalidGitDirectory {
                    package: requirement.package.clone(),
                    directory: directory.clone().unwrap_or_default(),
                    span,
                }
            })?;
            Ok(ResolverContractSourceClause::Git {
                canonical_url: canonical_url.into(),
                directory,
            })
        }
        RequirementSource::Path(path) => Ok(ResolverContractSourceClause::Path {
            relative_to: path_base,
            path,
        }),
    }
}

fn canonical_git_directory(directory: Option<&str>) -> Result<Option<Box<str>>, ()> {
    match directory {
        None | Some(".") => Ok(None),
        Some(directory) => {
            crate::package_path::ContentPackagePath::parse(directory).map_err(|_| ())?;
            Ok(Some(directory.into()))
        }
    }
}

/// Errors produced before a resolver contract can be constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverContractBuildError {
    RootManifestRoleMismatch(PackageRole),
    InvalidManifestSyntax {
        lexical_errors: Vec<crate::lexer::PackageLexError>,
        syntax_errors: Vec<PackageParseError>,
    },
    SemanticValidationErrors(Vec<PackageValidationError>),
    MissingPackageHeader,
    MissingRootTargets,
    RootManifestMismatch,
    UnexpectedRootPathOverrideVersion(Box<str>),
    MissingRootPathOverrideVersion {
        package: Box<str>,
        span: Range<usize>,
    },
    MissingPathPackageVersion {
        package: Box<str>,
        path: Box<str>,
        requiring_package: MvsPackageVersion,
        span: Range<usize>,
    },
    PathPackageIdentityMismatch {
        expected: Box<str>,
        found: Box<str>,
        span: Range<usize>,
    },
    PathVersionMismatch {
        package: Box<str>,
        requirement: Box<crate::version_req::VersionReq>,
        declared_version: Box<Version>,
        span: Range<usize>,
    },
    MvsInput(MvsInputError),
}

/// Parses no files and performs no I/O; converts a validated parse result into
/// an MVS contract plus metadata needed by source selection, locking, and
/// target activation.
pub fn build_resolver_contract(
    parsed: &PackageParseOutput,
    context: &ResolverContractContext<'_>,
    validation_context: PackageValidationContext<'_>,
) -> Result<ResolverContract, ResolverContractBuildError> {
    let validation = validate_manifest(parsed, validation_context)?;
    let header = parsed
        .file()
        .header
        .as_ref()
        .ok_or(ResolverContractBuildError::MissingPackageHeader)?;
    let package_version = MvsPackageVersion::new(
        header.value.identity.to_string(),
        header.value.version.clone(),
    )
    .map_err(ResolverContractBuildError::MvsInput)?;
    if parsed.role() == PackageRole::Root && parsed != &context.root_manifest {
        return Err(ResolverContractBuildError::RootManifestMismatch);
    }

    let mut mvs_edges = Vec::new();
    let mut requirements = Vec::new();
    for declaration in &parsed.file().declarations {
        let PackageDeclaration::Requires { test, entries } = &declaration.value else {
            continue;
        };
        if *test && parsed.role() == PackageRole::Dependency {
            continue;
        }
        for entry in entries {
            let requirement = &entry.value;
            let included = requirement.only.as_ref().is_none_or(|only| {
                only.value
                    .iter()
                    .any(|target| context.root_targets.contains(&target.value))
            });
            let resolution = if included {
                let (edge, named_version, pin) =
                    build_edge(requirement, entry.span.clone(), &package_version, context)?;
                mvs_edges.push(edge);
                ResolverRequirementResolution::Included { named_version, pin }
            } else {
                ResolverRequirementResolution::ExcludedByRootTargets
            };
            requirements.push(ResolverRequirement {
                package: requirement.package.clone(),
                version: requirement.version.clone(),
                source: requirement.source.clone(),
                only: requirement.only.as_ref().map(|only| {
                    only.value
                        .iter()
                        .map(|target| target.value.clone())
                        .collect()
                }),
                grant: requirement.grant.as_ref().map(|grant| {
                    grant
                        .value
                        .iter()
                        .map(|capability| capability.value.clone())
                        .collect()
                }),
                test: *test,
                span: entry.span.clone(),
                resolution,
            });
        }
    }
    requirements.sort_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
    let mvs = MvsContract::new(package_version, mvs_edges)
        .map_err(ResolverContractBuildError::MvsInput)?;

    Ok(ResolverContract {
        mvs,
        role: parsed.role(),
        requirements,
        parse_warnings: parsed.warnings().to_vec(),
        validation_warnings: validation.warnings,
        context_binding: Some(context.root_context_binding.clone()),
    })
}

fn validate_manifest(
    parsed: &PackageParseOutput,
    context: PackageValidationContext<'_>,
) -> Result<crate::validation::PackageValidationOutput, ResolverContractBuildError> {
    if !parsed.is_syntax_valid() {
        return Err(ResolverContractBuildError::InvalidManifestSyntax {
            lexical_errors: parsed.lexical_errors().to_vec(),
            syntax_errors: parsed.errors().to_vec(),
        });
    }
    let validation = validate_package(parsed, context);
    if !validation.is_valid() {
        return Err(ResolverContractBuildError::SemanticValidationErrors(
            validation.errors,
        ));
    }
    Ok(validation)
}

fn build_edge(
    requirement: &Requirement,
    span: Range<usize>,
    requiring_package: &MvsPackageVersion,
    context: &ResolverContractContext<'_>,
) -> Result<(MvsEdge, Version, ResolverPinKind), ResolverContractBuildError> {
    let override_path = context
        .root_path_overrides
        .get(requirement.package.as_ref())
        .filter(|package_version| {
            package_version.version().compatibility_line()
                == requirement.version.compatibility_line()
        });
    if let Some(package_version) = override_path {
        let edge = MvsEdge::new_root_override_pinned(
            requirement.package.clone(),
            requirement.version.clone(),
            package_version.version().clone(),
            span,
        )
        .map_err(ResolverContractBuildError::MvsInput)?;
        return Ok((
            edge,
            package_version.version().clone(),
            ResolverPinKind::RootPathOverride,
        ));
    }

    let root_git_override = context
        .root_git_override_ids
        .contains(requirement.package.as_ref());
    if !root_git_override
        && let Some(Spanned {
            value: RequirementSource::Path(path),
            ..
        }) = &requirement.source
    {
        let key = ResolverPathKey::new(requiring_package.clone(), path.clone());
        let package_version = context.path_packages.get(&key).ok_or_else(|| {
            ResolverContractBuildError::MissingPathPackageVersion {
                package: requirement.package.clone(),
                path: path.clone(),
                requiring_package: requiring_package.clone(),
                span: span.clone(),
            }
        })?;
        if package_version.package() != requirement.package.as_ref() {
            return Err(ResolverContractBuildError::PathPackageIdentityMismatch {
                expected: requirement.package.clone(),
                found: package_version.package().into(),
                span,
            });
        }
        if !requirement
            .version
            .matches_for_resolution(package_version.version())
        {
            return Err(ResolverContractBuildError::PathVersionMismatch {
                package: requirement.package.clone(),
                requirement: Box::new(requirement.version.clone()),
                declared_version: Box::new(package_version.version().clone()),
                span,
            });
        }
        let edge = MvsEdge::new_path_pinned(
            requirement.package.clone(),
            requirement.version.clone(),
            package_version.version().clone(),
            span,
        )
        .map_err(ResolverContractBuildError::MvsInput)?;
        return Ok((
            edge,
            package_version.version().clone(),
            ResolverPinKind::Path,
        ));
    }

    let named_version = requirement.version.minimum().clone();
    let edge = MvsEdge::new(
        requirement.package.clone(),
        requirement.version.clone(),
        named_version.clone(),
        span,
    )
    .map_err(ResolverContractBuildError::MvsInput)?;
    Ok((edge, named_version, ResolverPinKind::Minimum))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        ResolverContractBuildError, ResolverContractComparisonError,
        ResolverContractComparisonStatus, ResolverContractContext,
        ResolverContractPathSourceRequest, ResolverContractSourceSide, ResolverPathKey,
        ResolverPinKind, ResolverRequirementResolution, build_resolver_contract,
        compare_resolver_contract_edges,
    };
    use crate::manifest::{PackageRole, RequirementSource, parse, parse_for_role};
    use crate::mvs::{MvsPackageVersion, select};
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    fn package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, Version::parse(version).expect("version is valid"))
            .expect("package version is valid")
    }

    fn root_context<'a>(
        root: &crate::manifest::PackageParseOutput,
        path_packages: &'a BTreeMap<ResolverPathKey, MvsPackageVersion>,
        root_path_overrides: &'a BTreeMap<Box<str>, MvsPackageVersion>,
    ) -> ResolverContractContext<'a> {
        ResolverContractContext::from_root_manifest(
            root,
            path_packages,
            root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root manifest is valid")
    }

    #[test]
    fn retains_sorted_requirement_metadata_and_filters_dependency_edges_by_root_targets() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:z @ ^1 from git "https://example.test/z" grant { alloc }
    acme:a @ ^1 from git "https://example.test/a"
}
requires test {
    acme:test-helper @ ^1 from git "https://example.test/test"
}
"#,
        );
        let path_packages = BTreeMap::new();
        let path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &path_overrides);
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root resolver contract is valid");
        assert_eq!(
            root_contract
                .mvs
                .edges()
                .iter()
                .map(|edge| edge.package())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:test-helper", "acme:z"]
        );
        assert_eq!(
            root_contract
                .requirements
                .iter()
                .map(|requirement| requirement.package.as_ref())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:test-helper", "acme:z"]
        );
        let test_requirement = root_contract
            .requirements
            .iter()
            .find(|requirement| requirement.package.as_ref() == "acme:test-helper")
            .expect("root test edge is retained");
        assert!(test_requirement.test);
        assert!(matches!(
            test_requirement.resolution,
            ResolverRequirementResolution::Included {
                pin: ResolverPinKind::Minimum,
                ..
            }
        ));
        assert_eq!(
            root_contract
                .requirements
                .iter()
                .find(|requirement| requirement.package.as_ref() == "acme:z")
                .and_then(|requirement| requirement.grant.as_ref())
                .expect("grant metadata is retained"),
            &["alloc".into()].into()
        );

        let dependency = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:active @ ^1 from git "https://example.test/active" only { wasm32-core } grant { wasi:filesystem/types }
    acme:inactive @ ^1 from path "../inactive" only { wasm32-wasi }
    acme:plain @ ^1 from git "https://example.test/plain"
}
requires test { acme:dependency-test @ ^1 from git "https://example.test/test" }
"#,
            PackageRole::Dependency,
        );
        let dependency_contract =
            build_resolver_contract(&dependency, &context, PackageValidationContext::default())
                .expect("dependency resolver contract is valid");
        assert_eq!(
            dependency_contract
                .mvs
                .edges()
                .iter()
                .map(|edge| edge.package())
                .collect::<Vec<_>>(),
            ["acme:active", "acme:plain"]
        );
        assert!(
            !dependency_contract
                .requirements
                .iter()
                .any(|requirement| requirement.package.as_ref() == "acme:dependency-test")
        );
        let inactive = dependency_contract
            .requirements
            .iter()
            .find(|requirement| requirement.package.as_ref() == "acme:inactive")
            .expect("filtered edge metadata remains available");
        assert_eq!(
            inactive.resolution,
            ResolverRequirementResolution::ExcludedByRootTargets
        );
        assert_eq!(
            inactive.only.as_ref().expect("only set is retained"),
            &["wasm32-wasi".into()].into()
        );
        assert_eq!(
            dependency_contract
                .requirements
                .iter()
                .find(|requirement| requirement.package.as_ref() == "acme:active")
                .and_then(|requirement| requirement.grant.as_ref())
                .expect("grant set is retained"),
            &["wasi:filesystem/types".into()].into()
        );
    }

    #[test]
    fn direct_path_and_root_path_override_edges_name_declared_versions() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:local @ ^1 from path "../local"
    acme:overridden @ ^1 from git "https://example.test/original"
}
overrides {
    acme:overridden use path "../overridden"
}
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version, "../local"),
            package_version("acme:local", "1.5.0"),
        )]);
        let root_path_overrides = BTreeMap::from([(
            "acme:overridden".into(),
            package_version("acme:overridden", "1.6.0"),
        )]);
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("path resolver inputs are valid");
        assert_eq!(
            contract
                .mvs
                .edges()
                .iter()
                .map(|edge| (edge.package(), edge.named_version().to_string()))
                .collect::<Vec<_>>(),
            [
                ("acme:local", "1.5.0".to_owned()),
                ("acme:overridden", "1.6.0".to_owned())
            ]
        );
        assert_eq!(
            contract
                .requirements
                .iter()
                .find(|requirement| requirement.package.as_ref() == "acme:local")
                .expect("path metadata exists")
                .resolution,
            ResolverRequirementResolution::Included {
                named_version: Version::parse("1.5.0").expect("version is valid"),
                pin: ResolverPinKind::Path,
            }
        );
        assert_eq!(
            contract
                .requirements
                .iter()
                .find(|requirement| requirement.package.as_ref() == "acme:overridden")
                .expect("override metadata exists")
                .resolution,
            ResolverRequirementResolution::Included {
                named_version: Version::parse("1.6.0").expect("version is valid"),
                pin: ResolverPinKind::RootPathOverride,
            }
        );
        assert_eq!(
            context.root_override_pins(),
            std::collections::HashMap::from([(
                package_version("acme:overridden", "1.6.0").node(),
                Version::parse("1.6.0").expect("version is valid"),
            )])
        );

        let local_manifest = parse_for_role(
            br#"package acme:local @ 1.5.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let overridden_manifest = parse_for_role(
            br#"package acme:overridden @ 1.6.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let local_contract = build_resolver_contract(
            &local_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("path dependency contract is valid");
        let overridden_contract = build_resolver_contract(
            &overridden_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("root path override contract is valid");
        let contracts = std::collections::HashMap::from([
            (
                local_contract.mvs.package_version().clone(),
                local_contract.mvs,
            ),
            (
                overridden_contract.mvs.package_version().clone(),
                overridden_contract.mvs,
            ),
        ]);
        let selection = select(
            &contract.mvs,
            &contracts,
            &std::collections::HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("resolver contracts form valid MVS inputs");
        assert!(selection.is_complete());
        assert!(
            selection
                .named_by()
                .get(&package_version("acme:overridden", "1.6.0"))
                .expect("override target was named")[0]
                .root_override_pin,
        );
    }

    #[test]
    fn root_git_override_suppresses_a_direct_path_pin() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
overrides { acme:dep use git "https://example.test/fork" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version, "../dep"),
            package_version("acme:dep", "1.5.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root Git override is valid");
        assert_eq!(
            contract.mvs.edges()[0].named_version(),
            &Version::parse("1.0.0").unwrap()
        );
        assert_eq!(
            contract.requirements[0].resolution,
            ResolverRequirementResolution::Included {
                named_version: Version::parse("1.0.0").expect("version is valid"),
                pin: ResolverPinKind::Minimum,
            }
        );
    }

    #[test]
    fn root_path_override_only_pins_its_compatibility_line() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:caller @ ^1 from git "https://example.test/caller"
    acme:dep @ ^1 from git "https://example.test/original"
}
overrides { acme:dep use path "../dep" }
"#,
        );
        let dependency = parse_for_role(
            br#"package acme:caller @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^2 from git "https://example.test/other-line" }
"#,
            PackageRole::Dependency,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides =
            BTreeMap::from([("acme:dep".into(), package_version("acme:dep", "1.5.0"))]);
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root override contract is valid");
        let contract =
            build_resolver_contract(&dependency, &context, PackageValidationContext::default())
                .expect("other-line dependency edge remains valid");
        assert_eq!(
            contract.mvs.edges()[0].named_version(),
            &Version::parse("2.0.0").expect("version is valid")
        );
        assert_eq!(
            contract.requirements[0].resolution,
            ResolverRequirementResolution::Included {
                named_version: Version::parse("2.0.0").expect("version is valid"),
                pin: ResolverPinKind::Minimum,
            }
        );

        let dep_v1 = parse_for_role(
            br#"package acme:dep @ 1.5.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let dep_v2 = parse_for_role(
            br#"package acme:dep @ 2.0.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let dep_v1_contract =
            build_resolver_contract(&dep_v1, &context, PackageValidationContext::default())
                .expect("line one path contract is valid");
        let dep_v2_contract =
            build_resolver_contract(&dep_v2, &context, PackageValidationContext::default())
                .expect("line two Git contract is valid");
        let contracts = std::collections::HashMap::from([
            (contract.mvs.package_version().clone(), contract.mvs),
            (
                dep_v1_contract.mvs.package_version().clone(),
                dep_v1_contract.mvs,
            ),
            (
                dep_v2_contract.mvs.package_version().clone(),
                dep_v2_contract.mvs,
            ),
        ]);
        let selection = select(
            &root_contract.mvs,
            &contracts,
            &std::collections::HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("root path override pins only the matching line");
        assert!(selection.is_complete());
        assert_eq!(
            selection
                .selected()
                .get(&package_version("acme:dep", "1.0.0").node()),
            Some(&Version::parse("1.5.0").expect("version is valid"))
        );
        assert_eq!(
            selection
                .selected()
                .get(&package_version("acme:dep", "2.0.0").node()),
            Some(&Version::parse("2.0.0").expect("version is valid"))
        );
    }

    #[test]
    fn invalid_manifests_and_missing_path_inputs_never_produce_partial_contracts() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let invalid = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:missing @ ^1 from path "../missing" }
"#,
            PackageRole::Dependency,
        );
        assert!(matches!(
            build_resolver_contract(&invalid, &context, PackageValidationContext::default()),
            Err(ResolverContractBuildError::MissingPathPackageVersion { .. })
        ));

        let syntax_invalid = parse_for_role(
            br#"package acme:broken @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep"
"#,
            PackageRole::Dependency,
        );
        assert!(matches!(
            build_resolver_contract(
                &syntax_invalid,
                &context,
                PackageValidationContext::default()
            ),
            Err(ResolverContractBuildError::InvalidManifestSyntax { .. })
        ));

        let semantic_invalid = parse_for_role(
            br#"package acme:bad @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:bad @ ^1 from git "https://example.test/bad" }
"#,
            PackageRole::Dependency,
        );
        assert!(matches!(
            build_resolver_contract(
                &semantic_invalid,
                &context,
                PackageValidationContext::default()
            ),
            Err(ResolverContractBuildError::SemanticValidationErrors(_))
        ));
    }

    #[test]
    fn root_path_override_context_requires_all_resolved_path_packages() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
overrides { acme:dep use path "../dep" }
"#,
        );
        assert!(matches!(
            ResolverContractContext::from_root_manifest(
                &root,
                &BTreeMap::new(),
                &BTreeMap::new(),
                PackageValidationContext::default(),
            ),
            Err(ResolverContractBuildError::MissingRootPathOverrideVersion { package, .. })
                if package.as_ref() == "acme:dep"
        ));
    }

    #[test]
    fn root_contract_must_match_the_manifest_that_built_its_context() {
        let context_manifest = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:first @ ^1 from git "https://example.test/first" }
"#,
        );
        let different_manifest = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:second @ ^1 from git "https://example.test/second" }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&context_manifest, &path_packages, &root_path_overrides);
        assert_eq!(
            build_resolver_contract(
                &different_manifest,
                &context,
                PackageValidationContext::default()
            ),
            Err(ResolverContractBuildError::RootManifestMismatch)
        );
    }

    #[test]
    fn path_dependency_versions_must_satisfy_the_requirement() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:local @ ^1 from path "../local" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(root_version, "../local"),
            package_version("acme:local", "2.0.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        assert!(matches!(
            build_resolver_contract(&root, &context, PackageValidationContext::default()),
            Err(ResolverContractBuildError::PathVersionMismatch {
                package,
                declared_version,
                ..
            }) if package.as_ref() == "acme:local"
                && *declared_version == Version::parse("2.0.0").expect("version is valid")
        ));
    }

    #[test]
    fn contract_edge_comparison_ignores_order_spans_and_filtered_requirements() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let expected = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:a @ ^1 from git "https://example.test/a"
    acme:z @ ^1 from git "HTTPS://EXAMPLE.TEST:443/repo/" dir "z" only { wasm32-core, wasm32-wasi } grant { alloc, wasi:filesystem/types }
    acme:ignored @ ^1 from git "https://example.test/ignored" only { wasm32-wasi }
}
requires test { acme:test @ ^1 from git "https://example.test/test" }
"#,
            PackageRole::Dependency,
        );
        let actual = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:z @ ^1 from git "https://example.test/repo" dir "z" only { wasm32-wasi, wasm32-core } grant { wasi:filesystem/types, alloc }
    acme:a @ ^1 from git "HTTPS://EXAMPLE.TEST:443/a/" dir "."
    acme:ignored @ ^2 from path "../not-loaded" only { wasm32-wasi }
}
requires test { acme:other-test @ ^1 from git "https://example.test/other-test" }
"#,
            PackageRole::Dependency,
        );
        let expected =
            build_resolver_contract(&expected, &context, PackageValidationContext::default())
                .expect("expected dependency contract is valid");
        let actual =
            build_resolver_contract(&actual, &context, PackageValidationContext::default())
                .expect("actual dependency contract is valid");

        let comparison = compare_resolver_contract_edges(&expected, &context, &actual, &context)
            .expect("the contracts have comparable source clauses");

        assert!(comparison.equivalent());
        assert_eq!(
            comparison
                .expected_edges
                .iter()
                .map(|edge| edge.package.as_ref())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:z"]
        );
    }

    #[test]
    fn contract_edge_comparison_detects_every_resolver_edge_field() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let build = |requirement: &str| {
            let source = format!(
                "package acme:dep @ 1.0.0\nsource \"src\"\ntargets {{ wasm32-core, wasm32-wasi }}\nrequires {{ {requirement} }}\n"
            );
            let parsed = parse_for_role(source.as_bytes(), PackageRole::Dependency);
            build_resolver_contract(&parsed, &context, PackageValidationContext::default())
                .expect("dependency contract is valid")
        };
        let expected = build(
            "acme:edge @ ^1 from git \"https://example.test/edge\" only { wasm32-core } grant { alloc }",
        );

        for requirement in [
            "acme:other @ ^1 from git \"https://example.test/edge\" only { wasm32-core } grant { alloc }",
            "acme:edge @ ^2 from git \"https://example.test/edge\" only { wasm32-core } grant { alloc }",
            "acme:edge @ ^1 from git \"https://mirror.test/edge\" only { wasm32-core } grant { alloc }",
            "acme:edge @ ^1 from git \"https://example.test/edge\" only { wasm32-core, wasm32-wasi } grant { alloc }",
            "acme:edge @ ^1 from git \"https://example.test/edge\" only { wasm32-core }",
        ] {
            let actual = build(requirement);
            let comparison =
                compare_resolver_contract_edges(&expected, &context, &actual, &context)
                    .expect("both source clauses are valid");
            assert!(
                !comparison.equivalent(),
                "expected edge difference for {requirement}"
            );
        }

        let mut inconsistent = expected.clone();
        inconsistent.requirements[0].version =
            crate::version_req::VersionReq::parse("^2").expect("version requirement is valid");
        assert_eq!(
            compare_resolver_contract_edges(&expected, &context, &inconsistent, &context),
            Err(ResolverContractComparisonError::ContractMetadataMismatch(
                Box::new(package_version("acme:dep", "1.0.0"))
            ))
        );
    }

    #[test]
    fn contract_edge_comparison_applies_root_git_and_path_overrides() {
        let root_git = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
overrides { acme:edge use git "HTTPS://EXAMPLE.TEST:443/override/" }
"#,
        );
        let empty_paths = BTreeMap::new();
        let empty_overrides = BTreeMap::new();
        let git_context = root_context(&root_git, &empty_paths, &empty_overrides);
        let source_contract = |source: &str, context: &ResolverContractContext<'_>| {
            let bytes = format!(
                "package acme:dep @ 1.0.0\nsource \"src\"\ntargets {{ wasm32-core }}\nrequires {{ acme:edge @ ^1 from git \"{source}\" }}\n"
            );
            let parsed = parse_for_role(bytes.as_bytes(), PackageRole::Dependency);
            build_resolver_contract(&parsed, context, PackageValidationContext::default())
                .expect("dependency contract is valid")
        };
        let original_git = source_contract("https://original.test/edge", &git_context);
        let mirror_git = source_contract("https://mirror.test/edge", &git_context);
        let git_comparison =
            compare_resolver_contract_edges(&original_git, &git_context, &mirror_git, &git_context)
                .expect("root Git override supplies the effective source");
        assert!(git_comparison.equivalent());

        let root_path = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
overrides { acme:edge use path "../override-edge" }
"#,
        );
        let root_edge_version = package_version("acme:edge", "1.5.0");
        let path_overrides = BTreeMap::from([("acme:edge".into(), root_edge_version)]);
        let path_context = root_context(&root_path, &empty_paths, &path_overrides);
        let original_path_source = source_contract("https://original.test/edge", &path_context);
        let mirror_path_source = source_contract("https://mirror.test/edge", &path_context);
        let path_comparison = compare_resolver_contract_edges(
            &original_path_source,
            &path_context,
            &mirror_path_source,
            &path_context,
        )
        .expect("root path override supplies the effective source");
        assert_eq!(
            path_comparison.status(),
            ResolverContractComparisonStatus::RequiresSourceBoundPathResolution
        );
    }

    #[test]
    fn contract_edge_comparison_includes_source_specific_path_pin_versions() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let requiring_version = package_version("acme:dep", "1.0.0");
        let expected_paths = BTreeMap::from([(
            ResolverPathKey::new(requiring_version.clone(), "../local"),
            package_version("acme:local", "1.1.0"),
        )]);
        let actual_paths = BTreeMap::from([(
            ResolverPathKey::new(requiring_version, "../local"),
            package_version("acme:local", "1.2.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let expected_context = root_context(&root, &expected_paths, &root_path_overrides);
        let actual_context = root_context(&root, &actual_paths, &root_path_overrides);
        let manifest = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:local @ ^1 from path "../local" }
"#,
            PackageRole::Dependency,
        );
        let expected = build_resolver_contract(
            &manifest,
            &expected_context,
            PackageValidationContext::default(),
        )
        .expect("expected source-specific path package is valid");
        let actual = build_resolver_contract(
            &manifest,
            &actual_context,
            PackageValidationContext::default(),
        )
        .expect("actual source-specific path package is valid");

        let comparison =
            compare_resolver_contract_edges(&expected, &expected_context, &actual, &actual_context)
                .expect("both contracts use compatible root resolution contexts");

        assert!(!comparison.equivalent());
        assert_eq!(
            comparison.expected_edges()[0].named_version,
            Version::parse("1.1.0").expect("version is valid")
        );
        assert_eq!(
            comparison.actual_edges()[0].named_version,
            Version::parse("1.2.0").expect("version is valid")
        );
        assert_eq!(
            comparison.status(),
            ResolverContractComparisonStatus::Different
        );
    }

    #[test]
    fn contract_edge_comparison_defers_dependency_relative_path_identity() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let requiring_version = package_version("acme:dep", "1.0.0");
        let path_packages = BTreeMap::from([(
            ResolverPathKey::new(requiring_version, "../local"),
            package_version("acme:local", "1.1.0"),
        )]);
        let empty_root_overrides = BTreeMap::new();
        let expected_context = root_context(&root, &path_packages, &empty_root_overrides);
        let actual_context = root_context(&root, &path_packages, &empty_root_overrides);
        let manifest = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:local @ ^1 from path "../local" }
"#,
            PackageRole::Dependency,
        );
        let expected = build_resolver_contract(
            &manifest,
            &expected_context,
            PackageValidationContext::default(),
        )
        .expect("expected dependency contract is valid");
        let actual = build_resolver_contract(
            &manifest,
            &actual_context,
            PackageValidationContext::default(),
        )
        .expect("actual dependency contract is valid");

        let comparison =
            compare_resolver_contract_edges(&expected, &expected_context, &actual, &actual_context)
                .expect("both contracts have compatible root settings");

        assert_eq!(
            comparison.status(),
            ResolverContractComparisonStatus::RequiresSourceBoundPathResolution
        );
        assert!(!comparison.equivalent());
        let mut resolved_requests = Vec::new();
        let resolved = comparison.compare_with_resolved_paths(|request| {
            resolved_requests.push(request.clone());
            Ok::<_, &'static str>(match request.side {
                ResolverContractSourceSide::Expected => "expected tree",
                ResolverContractSourceSide::Actual => "actual tree",
            })
        });
        assert_eq!(
            resolved.status(),
            ResolverContractComparisonStatus::Different
        );
        assert!(resolved.failures().is_empty());
        assert_eq!(
            resolved_requests
                .iter()
                .map(|request| request.side)
                .collect::<Vec<_>>(),
            [
                ResolverContractSourceSide::Expected,
                ResolverContractSourceSide::Actual
            ]
        );
        assert_eq!(resolved_requests[0].package.as_ref(), "acme:local");
        assert_eq!(
            resolved_requests[0].requiring_package,
            package_version("acme:dep", "1.0.0")
        );
        assert_eq!(resolved_requests[0].path.as_ref(), "../local");
    }

    #[test]
    fn path_spelling_differences_wait_for_source_bound_resolution() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let requiring_version = package_version("acme:dep", "1.0.0");
        let local_version = package_version("acme:local", "1.1.0");
        let other_version = package_version("acme:other", "1.0.0");
        let path_packages = BTreeMap::from([
            (
                ResolverPathKey::new(requiring_version.clone(), "../child"),
                local_version.clone(),
            ),
            (
                ResolverPathKey::new(requiring_version.clone(), "../tmp/../child"),
                local_version,
            ),
            (
                ResolverPathKey::new(requiring_version, "../other"),
                other_version,
            ),
        ]);
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let build = |path: &str| {
            let source = format!(
                "package acme:dep @ 1.0.0\nsource \"src\"\ntargets {{ wasm32-core }}\nrequires {{\n    acme:local @ ^1 from path \"{path}\"\n    acme:other @ ^1 from path \"../other\"\n}}\n"
            );
            let parsed = parse_for_role(source.as_bytes(), PackageRole::Dependency);
            build_resolver_contract(&parsed, &context, PackageValidationContext::default())
                .expect("path source is valid")
        };
        let expected = build("../child");
        let actual = build("../tmp/../child");

        let comparison = compare_resolver_contract_edges(&expected, &context, &actual, &context)
            .expect("both contracts use the same root settings");

        assert_eq!(
            comparison.status(),
            ResolverContractComparisonStatus::RequiresSourceBoundPathResolution
        );
        assert!(!comparison.equivalent());
        assert_eq!(
            comparison
                .compare_with_resolved_paths(|request| {
                    Ok::<_, &'static str>(match request.package.as_ref() {
                        "acme:local" => "same resolved child",
                        "acme:other" => "same resolved other",
                        _ => "unexpected path",
                    })
                })
                .status(),
            ResolverContractComparisonStatus::Equivalent
        );
        let mut resolved_requests = Vec::new();
        let resolved = comparison.compare_with_resolved_paths(|request| {
            resolved_requests.push(request.clone());
            Ok::<_, &'static str>(match (request.package.as_ref(), request.side) {
                ("acme:local", ResolverContractSourceSide::Expected) => "expected child",
                ("acme:local", ResolverContractSourceSide::Actual) => "actual child",
                ("acme:other", _) => return Err("other path lookup failed"),
                _ => "unexpected path",
            })
        });
        assert_eq!(
            resolved.status(),
            ResolverContractComparisonStatus::Different
        );
        assert_eq!(resolved.failures().len(), 2);
        assert_eq!(resolved_requests.len(), 4);
        assert_eq!(resolved_requests[0].path.as_ref(), "../child");
        assert_eq!(resolved_requests[2].path.as_ref(), "../tmp/../child");
        assert_eq!(
            resolved.failures()[0].request.package.as_ref(),
            "acme:other"
        );
        assert_eq!(
            resolved.failures()[0].request.side,
            ResolverContractSourceSide::Expected
        );

        let unresolved = comparison.compare_with_resolved_paths(
            |_request: &ResolverContractPathSourceRequest| {
                Err::<(), _>("path is missing in the source tree")
            },
        );
        assert_eq!(
            unresolved.status(),
            ResolverContractComparisonStatus::PathResolutionFailed
        );
        assert_eq!(unresolved.failures().len(), 4);
        assert_eq!(
            unresolved
                .failures()
                .iter()
                .map(|failure| failure.request.side)
                .collect::<Vec<_>>(),
            [
                ResolverContractSourceSide::Expected,
                ResolverContractSourceSide::Expected,
                ResolverContractSourceSide::Actual,
                ResolverContractSourceSide::Actual
            ]
        );
        assert_eq!(
            unresolved.failures()[0].request.package.as_ref(),
            "acme:local"
        );
        assert_eq!(
            unresolved.failures()[0].error.as_ref(),
            &"path is missing in the source tree"
        );
    }

    #[test]
    fn contract_edge_comparison_rejects_invalid_git_source_urls() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let invalid = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:edge @ ^1 from git "http://example.test/edge" }
"#,
            PackageRole::Dependency,
        );
        let contract =
            build_resolver_contract(&invalid, &context, PackageValidationContext::default())
                .expect("resolver adapter retains the source for canonicalization");

        assert!(matches!(
            compare_resolver_contract_edges(&contract, &context, &contract, &context),
            Err(ResolverContractComparisonError::InvalidGitUrl { package, .. })
                if package.as_ref() == "acme:edge"
        ));

        let valid_directory = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:edge @ ^1 from git "https://example.test/repo" dir "edge" }
"#,
            PackageRole::Dependency,
        );
        let valid_directory_contract = build_resolver_contract(
            &valid_directory,
            &context,
            PackageValidationContext::default(),
        )
        .expect("source directory is retained for normalization");
        for invalid_directory in ["../edge", "group/../edge"] {
            let mut contract = valid_directory_contract.clone();
            let RequirementSource::Git { directory, .. } = &mut contract.requirements[0]
                .source
                .as_mut()
                .expect("Git source is retained")
                .value
            else {
                panic!("expected a Git source");
            };
            *directory = Some(invalid_directory.into());
            assert!(matches!(
                compare_resolver_contract_edges(&contract, &context, &contract, &context),
                Err(ResolverContractComparisonError::InvalidGitDirectory { package, .. })
                    if package.as_ref() == "acme:edge"
            ));
        }
    }

    #[test]
    fn contract_edge_comparison_rejects_changed_root_path_packages() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:local @ ^1 from path "../local" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let root_path_overrides = BTreeMap::new();
        let expected_paths = BTreeMap::from([(
            ResolverPathKey::new(root_version.clone(), "../local"),
            package_version("acme:local", "1.0.0"),
        )]);
        let actual_paths = BTreeMap::from([(
            ResolverPathKey::new(root_version, "../local"),
            package_version("acme:local", "1.1.0"),
        )]);
        let expected_context = root_context(&root, &expected_paths, &root_path_overrides);
        let actual_context = root_context(&root, &actual_paths, &root_path_overrides);
        let manifest = parse_for_role(
            br#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
"#,
            PackageRole::Dependency,
        );
        let expected = build_resolver_contract(
            &manifest,
            &expected_context,
            PackageValidationContext::default(),
        )
        .expect("expected dependency contract is valid");
        let actual = build_resolver_contract(
            &manifest,
            &actual_context,
            PackageValidationContext::default(),
        )
        .expect("actual dependency contract is valid");

        assert_eq!(
            compare_resolver_contract_edges(&expected, &expected_context, &actual, &actual_context),
            Err(ResolverContractComparisonError::RootContextMismatch)
        );
    }
}
