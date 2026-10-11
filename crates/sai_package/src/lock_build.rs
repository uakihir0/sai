//! Construction of `sai.lock` from a validated final resolver graph.
//!
//! Content-derived fields are supplied by the caller after source selection
//! and final-source auditing. Requirements are derived from the resolver
//! contracts and checked against the selected graph, so callers cannot persist
//! edges from a different resolution.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use crate::lock::{
    LockFile, LockPackage, LockPackageField, LockParseOutput, LockRequirement, LockRoot,
    LockRootField,
};
use crate::lock_graph_validation::{
    LockGraphValidationInputError, LockGraphValidationOutput, validate as validate_lock_graph,
};
use crate::lock_validation::{
    LockValidationError, SUPPORTED_LOCK_FORMAT_VERSION, validate as validate_lock,
};
use crate::manifest::{PackageRole, Spanned};
use crate::mvs::{MvsPackageVersion, MvsRequirer};
use crate::mvs_graph_validation::{MvsDependencyCycle, MvsSelectedGraph, MvsVersionConflict};
use crate::resolver_contract::{ResolverContract, ResolverPinKind, ResolverRequirementResolution};

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
pub(crate) enum LockBuildError {
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
                let only = requirement.only.as_ref().map(|targets| {
                    targets
                        .iter()
                        .map(|target| Spanned {
                            value: target.clone(),
                            span: requirement.span.clone(),
                        })
                        .collect()
                });
                output.push(Spanned {
                    value: F::from_requirement(LockRequirement {
                        test: requirement.test,
                        package: requirement.package.clone(),
                        requirement: requirement.version.clone(),
                        selected: edge.target.version().clone(),
                        only,
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

    use super::{LockBuildError, LockPackageMetadata, build_resolved_lock};
    use crate::lock::{LockPackageField, LockSource};
    use crate::lock_graph_validation::validate as validate_lock_graph;
    use crate::lock_serialization::serialize_canonical;
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

    fn fixture() -> Fixture {
        let root_manifest = parse(
            br#"package acme:root @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from path "../dep" only { wasm32-core }
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
                "lock 1\n\n",
                "root acme:root @ 1.0.0\n",
                "    targets { wasm32-core }\n",
                "    requires acme:dep @ ^1 -> 1.2.0 only { wasm32-core }\n",
                "    requires test acme:testdep @ ^1 -> 1.1.0 only { wasm32-core }\n",
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
                    only: None,
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
