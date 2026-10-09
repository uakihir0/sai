//! Drives MVS contract loading through manifest-aware source selection.

use std::collections::{BTreeMap, HashMap};

use crate::manifest::PackageRole;
use crate::mvs::{
    MvsContractProviderFailure, MvsContractRequest, MvsInputError, MvsNode, MvsPackageVersion,
    MvsSelectionOutput, MvsSelectionProviderError, select_with_provider,
};
use crate::resolver_contract::{ResolverContract, ResolverContractContext};
use crate::resolver_source::{
    ResolverPackageSource, ResolverSourceSelection, ResolverSourceSelectionError,
    select_resolver_source,
};
use crate::version::Version;

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
pub struct ResolverSourceProviderFailure<E> {
    pub request: MvsContractRequest,
    pub error: ResolverPackageLoadError<E>,
}

/// Why a contract request could not be resolved to a validated dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverPackageLoadError<E> {
    SourceSelection(ResolverSourceSelectionError),
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
pub enum ResolverSourceProviderError<E> {
    RootContract(ResolverSourceSelectionError),
    Input(Vec<MvsInputError>),
    ContractProvider {
        failures: Vec<ResolverSourceProviderFailure<E>>,
        /// Partial MVS selection and every successfully loaded source-bound
        /// contract, retained even when another branch failed.
        partial_selection: Box<ResolverSourceProviderOutput>,
    },
}

/// Loads contracts as deterministic MVS requests leave the priority queue.
///
/// The loader receives the selected source and every path-pin candidate for
/// the request. It must compare the path-pin candidates using the appropriate
/// local-filesystem or Git-tree rules before loading a contract. It is also
/// responsible for reading, parsing, and semantically validating the source,
/// then returning a dependency-role contract bound to `root_context`.
///
/// Root path pins come from `root_context`; `lock_floors` and `locked_sources`
/// must come from a validated lock. The result retains the source used for
/// every successfully loaded MVS candidate, not only the final selected
/// versions.
pub fn select_with_source_provider<E>(
    root_contract: &ResolverContract,
    root_context: &ResolverContractContext<'_>,
    lock_floors: &HashMap<MvsNode, Version>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    mut load_contract: impl FnMut(
        &MvsContractRequest,
        &ResolverSourceSelection,
    ) -> Result<ResolverContract, E>,
) -> Result<ResolverSourceProviderOutput, ResolverSourceProviderError<E>> {
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

    match selection {
        Ok(selection) => Ok(output(selection)),
        Err(MvsSelectionProviderError::Input(errors)) => {
            Err(ResolverSourceProviderError::Input(errors))
        }
        Err(MvsSelectionProviderError::ContractProvider {
            failures,
            partial_selection,
        }) => Err(ResolverSourceProviderError::ContractProvider {
            failures: failures
                .into_iter()
                .map(|MvsContractProviderFailure { request, error }| {
                    ResolverSourceProviderFailure { request, error }
                })
                .collect(),
            partial_selection: Box::new(output(*partial_selection)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{
        ResolverPackageLoadError, ResolverSourceProviderError, select_with_source_provider,
    };
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::mvs::{MvsContractRequest, MvsNamedEdge, MvsPackageVersion, MvsRequirer};
    use crate::resolver_contract::{
        ResolverContractContext, ResolverPathKey, build_resolver_contract,
    };
    use crate::resolver_source::{
        ResolverPackageSource, ResolverSourceReason, select_resolver_source,
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
            output.sources_used[&dep].reason,
            ResolverSourceReason::NamedRequirement
        );
        assert_eq!(
            output.sources_used[&dep].source,
            ResolverPackageSource::Git {
                url: "https://example.test/alpha-dep".into(),
                directory: None,
            }
        );
        assert_eq!(output.dependency_contracts.len(), 2);
        assert!(output.selection.is_complete());
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

        let Err(ResolverSourceProviderError::ContractProvider {
            failures,
            partial_selection,
        }) = result
        else {
            panic!("one failed branch should preserve a partial selection");
        };
        assert_eq!(failures.len(), 1);
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
