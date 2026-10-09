//! Adapts validated package manifests into resolver inputs and edge metadata.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

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
    /// Root requirements include test edges. Dependency test requirements are
    /// omitted as required by resolver §2.5.1.
    pub requirements: Vec<ResolverRequirement>,
    pub parse_warnings: Vec<PackageParseWarning>,
    pub validation_warnings: Vec<PackageValidationWarning>,
    context_binding: Option<Arc<u8>>,
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
        ResolverContractBuildError, ResolverContractContext, ResolverPathKey, ResolverPinKind,
        ResolverRequirementResolution, build_resolver_contract,
    };
    use crate::manifest::{PackageRole, parse, parse_for_role};
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
}
