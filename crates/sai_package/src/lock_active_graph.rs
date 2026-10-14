//! Build-time activation from a complete, validated `sai.lock` graph.
//!
//! Lock-backed builds use recorded edge metadata to activate one target and
//! command without loading contracts for inactive packages. The lock does not
//! retain MVS named versions or dependency `targets`, so the output types in
//! this module describe only the lock's package versions and edge metadata.
//! Callers must load and verify package contracts and their declared targets
//! after this step, and only for returned active nodes. Pinned path nodes can
//! be checked separately under Mode A condition 5.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::ops::Range;

use crate::lock::{LockPackage, LockPackageField, LockParseOutput, LockRequirement, LockRootField};
use crate::lock_graph_validation::{
    LockGraphNode, LockGraphValidationInputError, LockGraphValidationOutput,
    validate as validate_lock_graph,
};
use crate::manifest::PackageRole;
use crate::mvs::MvsPackageVersion;
use crate::resolver_contract::{ResolverContract, ResolverPinKind, ResolverRequirementResolution};
use crate::version::Version;
use crate::version_req::VersionReq;

/// A package version recorded in a lock-backed active graph.
///
/// The `root` flag distinguishes the root contract from dependency entries.
/// Lock files do not record MVS named versions, so this type intentionally
/// does not expose one.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LockActiveNode {
    pub package: Box<str>,
    pub version: Version,
    pub root: bool,
}

/// One active lock edge. `selected` is the resolved lock entry's version.
///
/// The lock does not record the MVS named version of an edge; consumers must
/// not treat `selected` as one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockActiveEdge {
    pub from: LockActiveNode,
    pub to: LockActiveNode,
    pub requirement: VersionReq,
    pub test: bool,
    pub only: Option<BTreeSet<Box<str>>>,
    pub span: Range<usize>,
}

/// The lock graph reachable from the root through active edges for one build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockActiveGraph {
    pub root: LockActiveNode,
    /// Active dependencies only, sorted by identity bytes then version.
    pub packages: Vec<LockActiveNode>,
    /// Active edges, sorted by source then target.
    pub edges: Vec<LockActiveEdge>,
}

/// Canonical metadata used to compare an included root edge to the lock.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LockRootEdgeMetadata {
    pub package: Box<str>,
    pub requirement: Box<str>,
    pub test: bool,
    pub only: Option<Vec<Box<str>>>,
}

/// Why lock-backed activation could not produce an active graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockActiveGraphError {
    RootRoleMismatch(PackageRole),
    RootContractMetadataMismatch {
        manifest: Box<MvsPackageVersion>,
        mvs: Box<MvsPackageVersion>,
    },
    RootPackageMismatch {
        contract: Box<MvsPackageVersion>,
        lock: Box<MvsPackageVersion>,
    },
    RootTargetsMismatch {
        contract: Vec<Box<str>>,
        lock: Vec<Box<str>>,
    },
    RootTargetsInconsistent {
        package_targets: Vec<Box<str>>,
        root_targets: Vec<Box<str>>,
    },
    TargetNotDeclaredByRoot(Box<str>),
    RootRequirementsMismatch {
        missing_from_lock: Vec<LockRootEdgeMetadata>,
        unexpected_in_lock: Vec<LockRootEdgeMetadata>,
    },
    RootPinnedRequirementVersionMismatch {
        package: Box<str>,
        pin: ResolverPinKind,
        expected: Box<Version>,
        selected: Box<Version>,
    },
    InvalidLockGraphInput(LockGraphValidationInputError),
    InvalidLockGraph(LockGraphValidationOutput),
}

impl fmt::Display for LockActiveGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "could not activate lock package graph: {self:?}")
    }
}

impl std::error::Error for LockActiveGraphError {}

#[derive(Default)]
struct ActiveGraphTraversal {
    active_packages: BTreeSet<LockActiveNode>,
    active_edges: Vec<LockActiveEdge>,
    pending: VecDeque<(LockGraphNode, LockActiveNode)>,
    visited: BTreeSet<LockGraphNode>,
}

/// Derives the build-active graph for `target` from a validated root contract
/// and the complete lock graph.
///
/// The lock graph is validated before activation, so inactive lock entries
/// must still be structurally reachable and acyclic. Only reachable active
/// dependency nodes are returned. This function performs no I/O and does not
/// require dependency contracts; callers load/verify those contracts after
/// activation, only for `packages`.
pub fn build_lock_active_graph(
    root_contract: &ResolverContract,
    lock: &LockParseOutput,
    target: &str,
    test_analysis: bool,
) -> Result<LockActiveGraph, LockActiveGraphError> {
    if root_contract.role() != PackageRole::Root {
        return Err(LockActiveGraphError::RootRoleMismatch(root_contract.role()));
    }

    let manifest_root = root_contract.manifest_package_version();
    let mvs_root = root_contract.mvs.package_version();
    if manifest_root != mvs_root || !root_contract.mvs_metadata_is_consistent() {
        return Err(LockActiveGraphError::RootContractMetadataMismatch {
            manifest: Box::new(manifest_root.clone()),
            mvs: Box::new(mvs_root.clone()),
        });
    }

    let package_targets = root_contract.package_targets();
    let root_targets = root_contract.root_targets();
    if package_targets != root_targets {
        return Err(LockActiveGraphError::RootTargetsInconsistent {
            package_targets: package_targets.iter().cloned().collect(),
            root_targets: root_targets.iter().cloned().collect(),
        });
    }
    if !root_targets.contains(target) {
        return Err(LockActiveGraphError::TargetNotDeclaredByRoot(target.into()));
    }

    let graph_validation =
        validate_lock_graph(lock).map_err(LockActiveGraphError::InvalidLockGraphInput)?;
    if !graph_validation.is_consistent() {
        return Err(LockActiveGraphError::InvalidLockGraph(graph_validation));
    }

    let lock_root = &lock
        .file
        .root
        .as_ref()
        .expect("a structurally valid lock has a root")
        .value;
    let lock_root_package =
        MvsPackageVersion::new(lock_root.package.clone(), lock_root.version.clone())
            .expect("the lock graph validator checks root identity");
    if manifest_root.package() != lock_root_package.package()
        || manifest_root.version() != lock_root_package.version()
    {
        return Err(LockActiveGraphError::RootPackageMismatch {
            contract: Box::new(manifest_root.clone()),
            lock: Box::new(lock_root_package),
        });
    }

    let lock_targets = lock_root
        .fields
        .iter()
        .find_map(|field| match &field.value {
            LockRootField::Targets(targets) => Some(
                targets
                    .iter()
                    .map(|target| target.value.clone())
                    .collect::<BTreeSet<_>>(),
            ),
            LockRootField::Requires(_) => None,
        })
        .expect("a structurally valid lock has root targets");
    if root_targets != &lock_targets {
        return Err(LockActiveGraphError::RootTargetsMismatch {
            contract: root_targets.iter().cloned().collect(),
            lock: lock_targets.into_iter().collect(),
        });
    }

    let (expected_edges, locked_edges) = compare_root_requirements(root_contract, lock_root);
    if expected_edges != locked_edges {
        return Err(root_edge_mismatch(&expected_edges, &locked_edges));
    }
    validate_root_requirement_pins(root_contract, lock_root)?;

    let root = LockActiveNode {
        package: manifest_root.package().into(),
        version: manifest_root.version().clone(),
        root: true,
    };
    let mut entries = BTreeMap::<LockGraphNode, (&LockPackage, LockActiveNode)>::new();
    for entry in &lock.file.packages {
        let node = LockActiveNode {
            package: entry.value.package.clone(),
            version: entry.value.version.clone(),
            root: false,
        };
        entries.insert(
            LockGraphNode {
                package: entry.value.package.clone(),
                line: entry.value.version.compatibility_line(),
            },
            (&entry.value, node),
        );
    }

    let root_graph_node = LockGraphNode {
        package: root.package.clone(),
        line: root.version.compatibility_line(),
    };
    let mut traversal = ActiveGraphTraversal::default();
    traversal.visited.insert(root_graph_node.clone());
    traversal.pending.push_back((root_graph_node, root.clone()));

    while let Some((source_graph_node, source)) = traversal.pending.pop_front() {
        match entries.get(&source_graph_node) {
            Some((package, _)) => {
                for field in &package.fields {
                    if let LockPackageField::Requires(requirement) = &field.value {
                        visit_edge(
                            requirement,
                            field.span.clone(),
                            &source,
                            target,
                            test_analysis,
                            &entries,
                            &mut traversal,
                        );
                    }
                }
            }
            None if source.root => {
                for field in &lock_root.fields {
                    if let LockRootField::Requires(requirement) = &field.value {
                        visit_edge(
                            requirement,
                            field.span.clone(),
                            &source,
                            target,
                            test_analysis,
                            &entries,
                            &mut traversal,
                        );
                    }
                }
            }
            None => unreachable!("lock graph validation guarantees every active node exists"),
        }
    }

    let mut packages = traversal.active_packages.into_iter().collect::<Vec<_>>();
    packages.sort_by(|left, right| {
        left.package
            .as_bytes()
            .cmp(right.package.as_bytes())
            .then_with(|| left.version.cmp(&right.version))
    });
    traversal.active_edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
            .then_with(|| {
                left.requirement
                    .to_string()
                    .cmp(&right.requirement.to_string())
            })
    });

    Ok(LockActiveGraph {
        root,
        packages,
        edges: traversal.active_edges,
    })
}

fn visit_edge(
    requirement: &LockRequirement,
    span: Range<usize>,
    source: &LockActiveNode,
    target: &str,
    test_analysis: bool,
    entries: &BTreeMap<LockGraphNode, (&LockPackage, LockActiveNode)>,
    traversal: &mut ActiveGraphTraversal,
) {
    if (!test_analysis && requirement.test)
        || requirement
            .only
            .as_ref()
            .is_some_and(|only| !only.iter().any(|item| item.value.as_ref() == target))
    {
        return;
    }

    let graph_target = LockGraphNode {
        package: requirement.package.clone(),
        line: requirement.requirement.compatibility_line(),
    };
    let (_, target_node) = entries
        .get(&graph_target)
        .expect("lock graph validation guarantees every selected target exists");

    traversal.active_edges.push(LockActiveEdge {
        from: source.clone(),
        to: target_node.clone(),
        requirement: requirement.requirement.clone(),
        test: requirement.test,
        only: requirement.only.as_ref().map(|only| {
            only.iter()
                .map(|item| item.value.clone())
                .collect::<BTreeSet<_>>()
        }),
        span,
    });
    traversal.active_packages.insert(target_node.clone());
    if traversal.visited.insert(graph_target.clone()) {
        traversal
            .pending
            .push_back((graph_target, target_node.clone()));
    }
}

fn compare_root_requirements(
    root_contract: &ResolverContract,
    lock_root: &crate::lock::LockRoot,
) -> (
    BTreeSet<LockRootEdgeMetadata>,
    BTreeSet<LockRootEdgeMetadata>,
) {
    let expected = root_contract
        .requirements()
        .iter()
        .filter(|requirement| {
            matches!(
                requirement.resolution,
                ResolverRequirementResolution::Included { .. }
            )
        })
        .map(|requirement| LockRootEdgeMetadata {
            package: requirement.package.clone(),
            requirement: requirement.version.to_string().into(),
            test: requirement.test,
            only: requirement
                .only
                .as_ref()
                .map(|only| only.iter().cloned().collect()),
        })
        .collect();
    let actual = lock_root
        .fields
        .iter()
        .filter_map(|field| match &field.value {
            LockRootField::Requires(requirement) => Some(LockRootEdgeMetadata {
                package: requirement.package.clone(),
                requirement: requirement.requirement.to_string().into(),
                test: requirement.test,
                only: requirement.only.as_ref().map(|only| {
                    only.iter()
                        .map(|item| item.value.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                }),
            }),
            LockRootField::Targets(_) => None,
        })
        .collect();
    (expected, actual)
}

fn validate_root_requirement_pins(
    root_contract: &ResolverContract,
    lock_root: &crate::lock::LockRoot,
) -> Result<(), LockActiveGraphError> {
    for requirement in root_contract.requirements() {
        let ResolverRequirementResolution::Included { named_version, pin } =
            &requirement.resolution
        else {
            continue;
        };
        if *pin == ResolverPinKind::Minimum {
            continue;
        }

        let selected = lock_root
            .fields
            .iter()
            .find_map(|field| match &field.value {
                LockRootField::Requires(locked)
                    if locked.package == requirement.package
                        && locked.requirement == requirement.version =>
                {
                    Some(&locked.selected)
                }
                LockRootField::Targets(_) | LockRootField::Requires(_) => None,
            })
            .expect("root requirement metadata was matched against the lock");
        if selected != named_version {
            return Err(LockActiveGraphError::RootPinnedRequirementVersionMismatch {
                package: requirement.package.clone(),
                pin: *pin,
                expected: Box::new(named_version.clone()),
                selected: Box::new(selected.clone()),
            });
        }
    }
    Ok(())
}

fn root_edge_mismatch(
    expected: &BTreeSet<LockRootEdgeMetadata>,
    actual: &BTreeSet<LockRootEdgeMetadata>,
) -> LockActiveGraphError {
    LockActiveGraphError::RootRequirementsMismatch {
        missing_from_lock: expected.difference(actual).cloned().collect(),
        unexpected_in_lock: actual.difference(expected).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{LockActiveGraphError, build_lock_active_graph};
    use crate::lock::{LockPackageField, LockRootField, parse as parse_lock};
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::resolver_contract::{
        ResolverContract, ResolverContractContext, ResolverPathKey, build_resolver_contract,
    };
    use crate::validation::PackageValidationContext;
    use crate::{mvs::MvsPackageVersion, version::Version};

    fn root_contract() -> ResolverContract {
        let manifest = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-wasi, wasm64-wasi }
requires {
    acme:base @ ^1 from git "https://example.test/base" only { wasm32-wasi }
    acme:other-target @ ^1 from git "https://example.test/other" only { wasm64-wasi }
}
requires test { acme:test-base @ ^1 from git "https://example.test/test-base" }
"#,
        );
        let paths = BTreeMap::new();
        let overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &manifest,
            &paths,
            &overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root manifest");
        build_resolver_contract(&manifest, &context, PackageValidationContext::default())
            .expect("valid root contract")
    }

    fn lock() -> crate::lock::LockParseOutput {
        parse_lock(
            br#"lock 1
root acme:app @ 1.0.0
    targets { wasm32-wasi, wasm64-wasi }
    requires acme:base @ ^1 -> 1.0.0 only { wasm32-wasi }
    requires acme:other-target @ ^1 -> 1.0.0 only { wasm64-wasi }
    requires test acme:test-base @ ^1 -> 1.0.0
package acme:base @ 1.0.0
    source path "../base"
    requires acme:transitive @ ^1 -> 1.0.0 only { wasm64-wasi }
package acme:other-target @ 1.0.0
    source path "../other-target"
package acme:test-base @ 1.0.0
    source path "../test-base"
    requires acme:test-child @ ^1 -> 1.0.0
package acme:transitive @ 1.0.0
    source path "../transitive"
package acme:test-child @ 1.0.0
    source path "../test-child"
"#,
        )
    }

    fn path_pinned_root_contract() -> ResolverContract {
        let manifest = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-wasi }
requires {
    acme:base @ ^1 from path "../base"
}
"#,
        );
        let root = MvsPackageVersion::new(
            "acme:app",
            Version::parse("1.0.0").expect("valid root version"),
        )
        .expect("valid root package");
        let paths = BTreeMap::from([(
            ResolverPathKey::root(root, "../base"),
            MvsPackageVersion::new(
                "acme:base",
                Version::parse("1.0.0").expect("valid pinned version"),
            )
            .expect("valid pinned package"),
        )]);
        let overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &manifest,
            &paths,
            &overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        build_resolver_contract(&manifest, &context, PackageValidationContext::default())
            .expect("valid pinned root contract")
    }

    fn root_override_pinned_contract() -> ResolverContract {
        let manifest = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-wasi }
requires {
    acme:base @ ^1 from git "https://example.test/base"
}
overrides {
    acme:base use path "../base"
}
"#,
        );
        let paths = BTreeMap::new();
        let overrides = BTreeMap::from([(
            "acme:base".into(),
            MvsPackageVersion::new(
                "acme:base",
                Version::parse("1.0.0").expect("valid pinned version"),
            )
            .expect("valid pinned package"),
        )]);
        let context = ResolverContractContext::from_root_manifest(
            &manifest,
            &paths,
            &overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        build_resolver_contract(&manifest, &context, PackageValidationContext::default())
            .expect("valid pinned root contract")
    }

    #[test]
    fn filters_multi_level_only_edges_and_omits_inactive_nodes_without_contracts() {
        let active = build_lock_active_graph(&root_contract(), &lock(), "wasm32-wasi", false)
            .expect("valid lock activation");

        let package_names = active
            .packages
            .iter()
            .map(|package| package.package.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(package_names, ["acme:base"]);
        assert_eq!(active.edges.len(), 1);
        assert_eq!(active.edges[0].to.package.as_ref(), "acme:base");
        assert!(active.edges[0].from.root);
    }

    #[test]
    fn root_test_edges_activate_only_when_test_analysis_is_enabled() {
        let lock = lock();
        let ordinary = build_lock_active_graph(&root_contract(), &lock, "wasm32-wasi", false)
            .expect("ordinary build");
        assert!(
            ordinary
                .packages
                .iter()
                .all(|package| package.package.as_ref() != "acme:test-base")
        );

        let tests = build_lock_active_graph(&root_contract(), &lock, "wasm32-wasi", true)
            .expect("test analysis");
        let package_names = tests
            .packages
            .iter()
            .map(|package| package.package.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(
            package_names,
            ["acme:base", "acme:test-base", "acme:test-child"]
        );
        assert!(
            tests
                .edges
                .iter()
                .any(|edge| edge.test && edge.to.package.as_ref() == "acme:test-base")
        );
    }

    #[test]
    fn root_contract_identity_must_match_the_lock() {
        let manifest = parse(
            br#"package acme:other @ 1.0.0
source "src"
targets { wasm32-wasi, wasm64-wasi }
requires {
    acme:base @ ^1 from git "https://example.test/base" only { wasm32-wasi }
    acme:other-target @ ^1 from git "https://example.test/other" only { wasm64-wasi }
}
requires test { acme:test-base @ ^1 from git "https://example.test/test-base" }
"#,
        );
        let paths = BTreeMap::new();
        let overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &manifest,
            &paths,
            &overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root manifest");
        let contract =
            build_resolver_contract(&manifest, &context, PackageValidationContext::default())
                .expect("valid root contract");

        assert!(matches!(
            build_lock_active_graph(&contract, &lock(), "wasm32-wasi", false),
            Err(LockActiveGraphError::RootPackageMismatch { .. })
        ));
    }

    #[test]
    fn invalid_lock_graph_is_rejected_before_activation() {
        let mut lock = lock();
        let base = lock
            .file
            .packages
            .iter_mut()
            .find(|package| package.value.package.as_ref() == "acme:base")
            .expect("base lock entry");
        base.value.fields.retain(|field| {
            !matches!(
                field.value,
                LockPackageField::Requires(ref requirement)
                    if requirement.package.as_ref() == "acme:transitive"
            )
        });

        assert!(matches!(
            build_lock_active_graph(&root_contract(), &lock, "wasm32-wasi", false),
            Err(LockActiveGraphError::InvalidLockGraph(_))
        ));
    }

    #[test]
    fn root_edge_test_metadata_must_match_the_contract() {
        let mut lock = lock();
        let test_edge = lock
            .file
            .root
            .as_mut()
            .expect("root block")
            .value
            .fields
            .iter_mut()
            .find(|field| {
                matches!(
                    field.value,
                    LockRootField::Requires(ref requirement)
                        if requirement.package.as_ref() == "acme:test-base"
                )
            })
            .expect("root test edge");
        let LockRootField::Requires(requirement) = &mut test_edge.value else {
            unreachable!();
        };
        requirement.test = false;

        assert!(matches!(
            build_lock_active_graph(&root_contract(), &lock, "wasm32-wasi", false),
            Err(LockActiveGraphError::RootRequirementsMismatch { .. })
        ));
    }

    #[test]
    fn root_path_pin_must_match_the_locked_version() {
        let lock = parse_lock(
            br#"lock 1
root acme:app @ 1.0.0
    targets { wasm32-wasi }
    requires acme:base @ ^1 -> 1.1.0
package acme:base @ 1.1.0
    source path "../base"
"#,
        );

        assert!(matches!(
            build_lock_active_graph(&path_pinned_root_contract(), &lock, "wasm32-wasi", false),
            Err(LockActiveGraphError::RootPinnedRequirementVersionMismatch {
                package,
                pin: crate::resolver_contract::ResolverPinKind::Path,
                expected,
                selected,
            }) if package.as_ref() == "acme:base"
                && expected.to_string() == "1.0.0"
                && selected.to_string() == "1.1.0"
        ));
    }

    #[test]
    fn root_path_override_pin_must_match_the_locked_version() {
        let lock = parse_lock(
            br#"lock 1
root acme:app @ 1.0.0
    targets { wasm32-wasi }
    requires acme:base @ ^1 -> 1.1.0
package acme:base @ 1.1.0
    source path "../base"
"#,
        );

        assert!(matches!(
            build_lock_active_graph(
                &root_override_pinned_contract(),
                &lock,
                "wasm32-wasi",
                false
            ),
            Err(LockActiveGraphError::RootPinnedRequirementVersionMismatch {
                package,
                pin: crate::resolver_contract::ResolverPinKind::RootPathOverride,
                expected,
                selected,
            }) if package.as_ref() == "acme:base"
                && expected.to_string() == "1.0.0"
                && selected.to_string() == "1.1.0"
        ));
    }

    #[test]
    fn root_manifest_must_be_adapted_as_a_root_contract() {
        let parsed = parse_for_role(
            b"package acme:lib @ 1.0.0\nsource \"src\"\ntargets { wasm32-wasi }\n",
            PackageRole::Dependency,
        );
        let paths = BTreeMap::new();
        let overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &parse(b"package acme:app @ 1.0.0\nsource \"src\"\ntargets { wasm32-wasi }\n"),
            &paths,
            &overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        let dependency =
            build_resolver_contract(&parsed, &context, PackageValidationContext::default())
                .expect("valid dependency contract");
        assert!(matches!(
            build_lock_active_graph(&dependency, &lock(), "wasm32-wasi", false),
            Err(LockActiveGraphError::RootRoleMismatch(
                PackageRole::Dependency
            ))
        ));
    }
}
