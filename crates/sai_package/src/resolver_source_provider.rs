//! Drives MVS contract loading through manifest-aware source selection.

use std::collections::{BTreeMap, HashMap};

use crate::manifest::PackageRole;
use crate::mvs::{
    MvsContractProviderFailure, MvsContractRequest, MvsInputError, MvsNode, MvsPackageVersion,
    MvsSelectionOutput, MvsSelectionProviderError, select_with_provider,
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
        ResolverPackageLoadError, ResolverPathPinProviderFailureKind, ResolverSourceProviderError,
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
