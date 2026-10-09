//! Chooses the manifest source for a concrete MVS contract request.

use std::collections::HashMap;
use std::ops::Range;

use crate::manifest::{OverrideSource, PackageRole, RequirementSource};
use crate::mvs::{MvsContractRequest, MvsNamedEdge, MvsPackageVersion, MvsRequirer};
use crate::resolver_contract::{
    ResolverContract, ResolverContractContext, ResolverRequirementResolution,
};

/// A package source before filesystem or Git resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverPackageSource {
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
    /// `path` is relative to `relative_to`. The provider determines whether
    /// that base is a local directory or a directory in a fetched Git tree.
    Path {
        relative_to: MvsPackageVersion,
        path: Box<str>,
    },
}

/// The rule that selected a source for a concrete package version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverSourceReason {
    RootPathOverride,
    RootGitOverride,
    PathPin,
    NamedRequirement,
    LockFloor,
}

/// The manifest or lock entry that caused a source to be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverSourceProvenance {
    RootPathOverride { span: Range<usize> },
    RootGitOverride { span: Range<usize> },
    PathPin { pin: ResolverPathPinSource },
    NamedRequirement { edge: MvsNamedEdge },
    LockFloor { package_version: MvsPackageVersion },
}

/// One path-pin source that the filesystem layer must canonicalize and
/// compare with every other pin source for the same node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverPathPinSource {
    pub source: ResolverPackageSource,
    pub requirer: MvsRequirer,
    pub span: Range<usize>,
}

/// The chosen source and the provenance required by later loading checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverSourceSelection {
    source: ResolverPackageSource,
    reason: ResolverSourceReason,
    provenance: ResolverSourceProvenance,
    /// Non-empty for `PathPin`; includes every path pin visible at dequeue.
    /// The caller resolves and compares these paths, including symlinks.
    path_pin_sources: Vec<ResolverPathPinSource>,
}

impl ResolverSourceSelection {
    pub const fn source(&self) -> &ResolverPackageSource {
        &self.source
    }

    pub const fn reason(&self) -> ResolverSourceReason {
        self.reason
    }

    pub const fn provenance(&self) -> &ResolverSourceProvenance {
        &self.provenance
    }

    pub fn path_pin_sources(&self) -> &[ResolverPathPinSource] {
        &self.path_pin_sources
    }
}

/// Checks that every path pin for a node resolves to the same source.
///
/// `canonicalize` owns source-specific resolution and must return a key for
/// the complete resolved source. For local paths the key should identify the
/// joined path after lexical normalization and symlink resolution. For a path
/// inside a Git package it should include the canonical repository URL, the
/// requiring package's commit, and the normalized repository tree path. Git
/// paths that leave the repository must be returned as errors. The callback
/// receives the requirer and source span so it can resolve the correct base.
pub fn validate_resolver_path_pins<K, E>(
    selection: &ResolverSourceSelection,
    mut canonicalize: impl FnMut(&ResolverPathPinSource) -> Result<K, E>,
) -> Result<(), ResolverPathPinValidationError<K, E>>
where
    K: Eq,
{
    if !source_selection_provenance_matches_reason(selection) {
        return Err(ResolverPathPinValidationError::InconsistentSelection);
    }
    let Some(first_pin) = selection.path_pin_sources.first() else {
        return if selection.reason == ResolverSourceReason::PathPin {
            Err(ResolverPathPinValidationError::InconsistentSelection)
        } else {
            Ok(())
        };
    };
    if selection.reason != ResolverSourceReason::PathPin
        || !matches!(
            &selection.provenance,
            ResolverSourceProvenance::PathPin { pin } if pin == first_pin
        )
        || selection.source != first_pin.source
        || selection
            .path_pin_sources
            .iter()
            .any(|pin| !matches!(pin.source, ResolverPackageSource::Path { .. }))
    {
        return Err(ResolverPathPinValidationError::InconsistentSelection);
    }

    let first_key = canonicalize(first_pin).map_err(|error| {
        ResolverPathPinValidationError::Canonicalization {
            pin: Box::new(first_pin.clone()),
            error: Box::new(error),
        }
    })?;
    for pin in &selection.path_pin_sources[1..] {
        let key = canonicalize(pin).map_err(|error| {
            ResolverPathPinValidationError::Canonicalization {
                pin: Box::new(pin.clone()),
                error: Box::new(error),
            }
        })?;
        if key != first_key {
            return Err(ResolverPathPinValidationError::ConflictingSources {
                first: Box::new(first_pin.clone()),
                conflicting: Box::new(pin.clone()),
                first_key: Box::new(first_key),
                conflicting_key: Box::new(key),
            });
        }
    }
    Ok(())
}

fn source_selection_provenance_matches_reason(selection: &ResolverSourceSelection) -> bool {
    match (&selection.reason, &selection.provenance) {
        (
            ResolverSourceReason::RootPathOverride,
            ResolverSourceProvenance::RootPathOverride { .. },
        )
        | (
            ResolverSourceReason::RootGitOverride,
            ResolverSourceProvenance::RootGitOverride { .. },
        )
        | (
            ResolverSourceReason::NamedRequirement,
            ResolverSourceProvenance::NamedRequirement { .. },
        )
        | (ResolverSourceReason::LockFloor, ResolverSourceProvenance::LockFloor { .. }) => {
            selection.path_pin_sources.is_empty()
        }
        (ResolverSourceReason::PathPin, ResolverSourceProvenance::PathPin { pin }) => {
            selection.path_pin_sources.first() == Some(pin)
        }
        _ => false,
    }
}

/// A path pin could not be resolved consistently with the other pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverPathPinValidationError<K, E> {
    InconsistentSelection,
    Canonicalization {
        pin: Box<ResolverPathPinSource>,
        error: Box<E>,
    },
    ConflictingSources {
        first: Box<ResolverPathPinSource>,
        conflicting: Box<ResolverPathPinSource>,
        first_key: Box<K>,
        conflicting_key: Box<K>,
    },
}

/// Failure to map MVS edge provenance back to validated manifest metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverSourceSelectionError {
    RootContractRoleMismatch(PackageRole),
    RootContractMismatch {
        contract: Box<MvsPackageVersion>,
        context: Box<MvsPackageVersion>,
    },
    StaleRootContractContext,
    MissingRequirerContract(MvsPackageVersion),
    RequirerContractMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    RequirerContractRoleMismatch {
        requirer: MvsPackageVersion,
        expected: PackageRole,
        found: PackageRole,
    },
    StaleRequirerContractContext(MvsPackageVersion),
    MissingRequirement {
        requirer: MvsPackageVersion,
        package: Box<str>,
        span: Range<usize>,
    },
    AmbiguousRequirement {
        requirer: MvsPackageVersion,
        package: Box<str>,
        span: Range<usize>,
    },
    MissingRequirementSource {
        requirer: MvsPackageVersion,
        package: Box<str>,
        span: Range<usize>,
    },
    NamedVersionMismatch {
        requirer: MvsPackageVersion,
        package: Box<str>,
        requested: Box<crate::version::Version>,
        named: Box<crate::version::Version>,
        span: Range<usize>,
    },
    RequestPinMismatch {
        requirer: MvsPackageVersion,
        package: Box<str>,
        span: Range<usize>,
    },
    PathPinWithoutPathSource {
        requirer: MvsPackageVersion,
        package: Box<str>,
        span: Range<usize>,
    },
    MissingRootPathOverride(Box<str>),
    MissingLockSource(MvsPackageVersion),
    MissingNamedSource(MvsPackageVersion),
}

/// Chooses a source using the precedence in resolver §§2.7.3 and 2.8.
///
/// `dependency_contracts` contains the validated resolver contracts already
/// read for the concrete dependency versions in the current selection. The
/// root is passed separately because its manifest has root-only overrides.
/// `locked_sources` is keyed by exact package version and is consulted only
/// for a floor-only request. The caller should have canonicalized and
/// validated lock sources before passing them here.
///
/// This function does not access the filesystem or network. In particular,
/// it returns every same-node path pin for the provider to canonicalize and
/// compare, including symlink resolution for local paths.
pub fn select_resolver_source(
    request: &MvsContractRequest,
    root_contract: &ResolverContract,
    root_context: &ResolverContractContext<'_>,
    dependency_contracts: &HashMap<MvsPackageVersion, ResolverContract>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
) -> Result<ResolverSourceSelection, ResolverSourceSelectionError> {
    if root_contract.role() != PackageRole::Root {
        return Err(ResolverSourceSelectionError::RootContractRoleMismatch(
            root_contract.role(),
        ));
    }
    let root_version = root_context.root_package();
    if root_contract.mvs.package_version() != root_version {
        return Err(ResolverSourceSelectionError::RootContractMismatch {
            contract: Box::new(root_contract.mvs.package_version().clone()),
            context: Box::new(root_version.clone()),
        });
    }
    if !root_context.is_bound_to_root_contract(root_contract) {
        return Err(ResolverSourceSelectionError::StaleRootContractContext);
    }
    for edge in &request.named_by {
        requirement_for_edge(
            edge,
            &request.package_version,
            root_contract,
            root_context,
            dependency_contracts,
        )?;
    }

    let package = request.package_version.package();
    let node = request.package_version.node();
    let root_path_pins = root_context.root_override_pins();
    for item in root_context.root_overrides() {
        if item.value.package.as_ref() != package {
            continue;
        }
        match &item.value.source {
            OverrideSource::Git { url, directory } => {
                return Ok(ResolverSourceSelection {
                    source: ResolverPackageSource::Git {
                        url: url.clone(),
                        directory: directory.clone(),
                    },
                    reason: ResolverSourceReason::RootGitOverride,
                    provenance: ResolverSourceProvenance::RootGitOverride {
                        span: item.span.clone(),
                    },
                    path_pin_sources: Vec::new(),
                });
            }
            OverrideSource::Path(path)
                if root_path_pins.get(&node) == Some(request.package_version.version()) =>
            {
                return Ok(ResolverSourceSelection {
                    source: ResolverPackageSource::Path {
                        relative_to: root_version.clone(),
                        path: path.clone(),
                    },
                    reason: ResolverSourceReason::RootPathOverride,
                    provenance: ResolverSourceProvenance::RootPathOverride {
                        span: item.span.clone(),
                    },
                    path_pin_sources: Vec::new(),
                });
            }
            OverrideSource::Artifact | OverrideSource::Path(_) => {}
        }
    }

    if request.named_by.iter().any(|edge| edge.root_override_pin) {
        return Err(ResolverSourceSelectionError::MissingRootPathOverride(
            package.into(),
        ));
    }

    let mut named_by = request.named_by.iter().collect::<Vec<_>>();
    named_by.sort_by(|left, right| compare_named_edges(left, right));

    let mut path_pin_sources = Vec::new();
    for edge in &named_by {
        if !edge.path_pin {
            continue;
        }
        let source = source_for_edge(
            edge,
            &request.package_version,
            root_contract,
            root_context,
            dependency_contracts,
        )?;
        if !matches!(source, ResolverPackageSource::Path { .. }) {
            return Err(ResolverSourceSelectionError::PathPinWithoutPathSource {
                requirer: edge.requirer.package_version().clone(),
                package: package.into(),
                span: edge.span.clone(),
            });
        }
        path_pin_sources.push(ResolverPathPinSource {
            source,
            requirer: edge.requirer.clone(),
            span: edge.span.clone(),
        });
    }
    if let Some(first) = path_pin_sources.first() {
        return Ok(ResolverSourceSelection {
            source: first.source.clone(),
            reason: ResolverSourceReason::PathPin,
            provenance: ResolverSourceProvenance::PathPin { pin: first.clone() },
            path_pin_sources,
        });
    }

    if let Some(edge) = named_by.first() {
        let source = source_for_edge(
            edge,
            &request.package_version,
            root_contract,
            root_context,
            dependency_contracts,
        )?;
        return Ok(ResolverSourceSelection {
            source,
            reason: ResolverSourceReason::NamedRequirement,
            provenance: ResolverSourceProvenance::NamedRequirement {
                edge: (**edge).clone(),
            },
            path_pin_sources: Vec::new(),
        });
    }

    if request.lock_floor {
        let source = locked_sources
            .get(&request.package_version)
            .cloned()
            .ok_or_else(|| {
                ResolverSourceSelectionError::MissingLockSource(request.package_version.clone())
            })?;
        return Ok(ResolverSourceSelection {
            source,
            reason: ResolverSourceReason::LockFloor,
            provenance: ResolverSourceProvenance::LockFloor {
                package_version: request.package_version.clone(),
            },
            path_pin_sources: Vec::new(),
        });
    }

    Err(ResolverSourceSelectionError::MissingNamedSource(
        request.package_version.clone(),
    ))
}

fn compare_named_edges(left: &MvsNamedEdge, right: &MvsNamedEdge) -> std::cmp::Ordering {
    left.requirer
        .package_version()
        .cmp(right.requirer.package_version())
        .then_with(|| left.span.start.cmp(&right.span.start))
        .then_with(|| left.span.end.cmp(&right.span.end))
}

pub(crate) fn source_for_edge(
    edge: &MvsNamedEdge,
    target_package_version: &MvsPackageVersion,
    root_contract: &ResolverContract,
    root_context: &ResolverContractContext<'_>,
    dependency_contracts: &HashMap<MvsPackageVersion, ResolverContract>,
) -> Result<ResolverPackageSource, ResolverSourceSelectionError> {
    let requirement = requirement_for_edge(
        edge,
        target_package_version,
        root_contract,
        root_context,
        dependency_contracts,
    )?;
    let requiring_package = edge.requirer.package_version();
    let target_package = target_package_version.package();
    let source = requirement.source.as_ref().ok_or_else(|| {
        ResolverSourceSelectionError::MissingRequirementSource {
            requirer: requiring_package.clone(),
            package: target_package.into(),
            span: edge.span.clone(),
        }
    })?;
    Ok(match &source.value {
        RequirementSource::Git { url, directory } => ResolverPackageSource::Git {
            url: url.clone(),
            directory: directory.clone(),
        },
        RequirementSource::Path(path) => ResolverPackageSource::Path {
            relative_to: requiring_package.clone(),
            path: path.clone(),
        },
    })
}

fn requirement_for_edge<'a>(
    edge: &MvsNamedEdge,
    target_package_version: &MvsPackageVersion,
    root_contract: &'a ResolverContract,
    root_context: &ResolverContractContext<'_>,
    dependency_contracts: &'a HashMap<MvsPackageVersion, ResolverContract>,
) -> Result<&'a crate::resolver_contract::ResolverRequirement, ResolverSourceSelectionError> {
    let target_package = target_package_version.package();
    let requiring_package = edge.requirer.package_version();
    let contract = match &edge.requirer {
        MvsRequirer::Root(package_version) => {
            if root_contract.role() != PackageRole::Root {
                return Err(ResolverSourceSelectionError::RequirerContractRoleMismatch {
                    requirer: package_version.clone(),
                    expected: PackageRole::Root,
                    found: root_contract.role(),
                });
            }
            if root_contract.mvs.package_version() != package_version {
                return Err(ResolverSourceSelectionError::MissingRequirerContract(
                    package_version.clone(),
                ));
            }
            root_contract
        }
        MvsRequirer::Package(package_version) => {
            let contract = dependency_contracts.get(package_version).ok_or_else(|| {
                ResolverSourceSelectionError::MissingRequirerContract(package_version.clone())
            })?;
            if contract.role() != PackageRole::Dependency {
                return Err(ResolverSourceSelectionError::RequirerContractRoleMismatch {
                    requirer: package_version.clone(),
                    expected: PackageRole::Dependency,
                    found: contract.role(),
                });
            }
            contract
        }
    };
    let found_package_version = contract.mvs.package_version();
    if found_package_version != requiring_package {
        return Err(ResolverSourceSelectionError::RequirerContractMismatch {
            expected: Box::new(requiring_package.clone()),
            found: Box::new(found_package_version.clone()),
        });
    }
    if !root_context.is_bound_to_root_contract(contract) {
        return Err(ResolverSourceSelectionError::StaleRequirerContractContext(
            requiring_package.clone(),
        ));
    }

    let mut matches = contract.requirements.iter().filter(|requirement| {
        requirement.package.as_ref() == target_package
            && requirement.span == edge.span
            && matches!(
                requirement.resolution,
                ResolverRequirementResolution::Included { .. }
            )
    });
    let requirement =
        matches
            .next()
            .ok_or_else(|| ResolverSourceSelectionError::MissingRequirement {
                requirer: requiring_package.clone(),
                package: target_package.into(),
                span: edge.span.clone(),
            })?;
    if matches.next().is_some() {
        return Err(ResolverSourceSelectionError::AmbiguousRequirement {
            requirer: requiring_package.clone(),
            package: target_package.into(),
            span: edge.span.clone(),
        });
    }
    let (named_version, pin) = match &requirement.resolution {
        ResolverRequirementResolution::Included { named_version, pin } => (named_version, pin),
        ResolverRequirementResolution::ExcludedByRootTargets => {
            unreachable!("excluded requirements were omitted from MVS")
        }
    };
    if named_version != target_package_version.version() {
        return Err(ResolverSourceSelectionError::NamedVersionMismatch {
            requirer: requiring_package.clone(),
            package: target_package.into(),
            requested: Box::new(target_package_version.version().clone()),
            named: Box::new(named_version.clone()),
            span: edge.span.clone(),
        });
    }
    let expected_path_pin = *pin == crate::resolver_contract::ResolverPinKind::Path;
    let expected_root_override_pin =
        *pin == crate::resolver_contract::ResolverPinKind::RootPathOverride;
    if edge.path_pin != expected_path_pin || edge.root_override_pin != expected_root_override_pin {
        return Err(ResolverSourceSelectionError::RequestPinMismatch {
            requirer: requiring_package.clone(),
            package: target_package.into(),
            span: edge.span.clone(),
        });
    }
    Ok(requirement)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::ops::Range;

    use super::{
        ResolverPackageSource, ResolverPathPinSource, ResolverPathPinValidationError,
        ResolverSourceProvenance, ResolverSourceReason, ResolverSourceSelection,
        select_resolver_source, validate_resolver_path_pins,
    };
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::mvs::{MvsContractRequest, MvsNamedEdge, MvsPackageVersion, MvsRequirer};
    use crate::resolver_contract::{
        ResolverContract, ResolverContractContext, ResolverRequirementResolution,
        build_resolver_contract,
    };
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    fn package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, Version::parse(version).expect("valid version"))
            .expect("valid package version")
    }

    fn root_context<'a>(
        root: &crate::manifest::PackageParseOutput,
        path_packages: &'a BTreeMap<crate::resolver_contract::ResolverPathKey, MvsPackageVersion>,
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

    fn build_root_contract(
        root: &crate::manifest::PackageParseOutput,
        context: &ResolverContractContext<'_>,
    ) -> ResolverContract {
        build_resolver_contract(root, context, PackageValidationContext::default())
            .expect("root resolver contract is valid")
    }

    fn named_edge(
        contract: &ResolverContract,
        package: &str,
        requirer: MvsRequirer,
    ) -> MvsNamedEdge {
        let requirement = contract
            .requirements
            .iter()
            .find(|requirement| requirement.package.as_ref() == package)
            .expect("requirement metadata exists");
        let (path_pin, root_override_pin) = match requirement.resolution {
            ResolverRequirementResolution::Included { pin, .. } => (
                pin == crate::resolver_contract::ResolverPinKind::Path,
                pin == crate::resolver_contract::ResolverPinKind::RootPathOverride,
            ),
            ResolverRequirementResolution::ExcludedByRootTargets => {
                panic!("test requirement must be included")
            }
        };
        MvsNamedEdge {
            requirer,
            span: requirement.span.clone(),
            path_pin,
            root_override_pin,
        }
    }

    fn path_pin(path: &str, span: Range<usize>) -> ResolverPathPinSource {
        ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: package_version("acme:app", "1.0.0"),
                path: path.into(),
            },
            requirer: MvsRequirer::Root(package_version("acme:app", "1.0.0")),
            span,
        }
    }

    fn path_pin_selection(pins: Vec<ResolverPathPinSource>) -> ResolverSourceSelection {
        let pin = pins[0].clone();
        ResolverSourceSelection {
            source: pin.source.clone(),
            reason: ResolverSourceReason::PathPin,
            provenance: ResolverSourceProvenance::PathPin { pin },
            path_pin_sources: pins,
        }
    }

    #[test]
    fn path_pins_with_different_spellings_can_resolve_to_the_same_source() {
        let selection = path_pin_selection(vec![
            path_pin("../packages/dep/.", 2..12),
            path_pin("../dep", 20..25),
        ]);

        validate_resolver_path_pins(&selection, |_| Ok::<_, ()>("/workspace/dep"))
            .expect("canonicalized path pins agree");
    }

    #[test]
    fn conflicting_path_pins_retain_both_provenances_and_canonical_keys() {
        let first = path_pin("../packages/dep", 4..18);
        let conflicting = path_pin("../other/dep", 30..42);
        let selection = path_pin_selection(vec![first.clone(), conflicting.clone()]);

        assert_eq!(
            validate_resolver_path_pins(&selection, |pin| {
                Ok::<_, ()>(match pin.span.start {
                    4 => "/workspace/packages/dep",
                    _ => "/workspace/other/dep",
                })
            }),
            Err(ResolverPathPinValidationError::ConflictingSources {
                first: Box::new(first),
                conflicting: Box::new(conflicting),
                first_key: Box::new("/workspace/packages/dep"),
                conflicting_key: Box::new("/workspace/other/dep"),
            })
        );
    }

    #[test]
    fn path_pin_canonicalization_errors_identify_the_failing_pin() {
        let first = path_pin("../packages/dep", 4..18);
        let failing = path_pin("../missing/dep", 30..43);
        let selection = path_pin_selection(vec![first, failing.clone()]);

        assert_eq!(
            validate_resolver_path_pins(&selection, |pin| {
                if pin.span == failing.span {
                    Err("missing path")
                } else {
                    Ok("/workspace/packages/dep")
                }
            }),
            Err(ResolverPathPinValidationError::Canonicalization {
                pin: Box::new(failing),
                error: Box::new("missing path"),
            })
        );
    }

    #[test]
    fn rejects_non_path_or_mismatched_path_pin_selections() {
        let pin = path_pin("../dep", 2..9);
        let mut selection = path_pin_selection(vec![pin.clone()]);
        selection.reason = ResolverSourceReason::NamedRequirement;

        assert_eq!(
            validate_resolver_path_pins(&selection, |_| Ok::<_, ()>("unused")),
            Err(ResolverPathPinValidationError::InconsistentSelection)
        );

        let mut selection = path_pin_selection(vec![pin.clone()]);
        selection.source = ResolverPackageSource::Path {
            relative_to: package_version("acme:app", "1.0.0"),
            path: "../different".into(),
        };
        assert_eq!(
            validate_resolver_path_pins(&selection, |_| Ok::<_, ()>("unused")),
            Err(ResolverPathPinValidationError::InconsistentSelection)
        );

        let mut selection = path_pin_selection(vec![pin.clone(), path_pin("../another", 12..22)]);
        selection.path_pin_sources[1].source = ResolverPackageSource::Git {
            url: "https://example.test/dep".into(),
            directory: None,
        };
        assert_eq!(
            validate_resolver_path_pins(&selection, |_| Ok::<_, ()>("unused")),
            Err(ResolverPathPinValidationError::InconsistentSelection)
        );
    }

    #[test]
    fn rejects_path_pin_selections_without_pin_provenance() {
        let selection = ResolverSourceSelection {
            source: ResolverPackageSource::Path {
                relative_to: package_version("acme:app", "1.0.0"),
                path: "../dep".into(),
            },
            reason: ResolverSourceReason::PathPin,
            provenance: ResolverSourceProvenance::PathPin {
                pin: path_pin("../dep", 2..9),
            },
            path_pin_sources: Vec::new(),
        };

        assert_eq!(
            validate_resolver_path_pins(&selection, |_| Ok::<_, ()>("unused")),
            Err(ResolverPathPinValidationError::InconsistentSelection)
        );
    }

    #[test]
    fn selections_without_path_pins_need_no_canonicalization() {
        let selection = ResolverSourceSelection {
            source: ResolverPackageSource::Git {
                url: "https://example.test/dep".into(),
                directory: None,
            },
            reason: ResolverSourceReason::NamedRequirement,
            provenance: ResolverSourceProvenance::NamedRequirement {
                edge: MvsNamedEdge {
                    requirer: MvsRequirer::Root(package_version("acme:app", "1.0.0")),
                    span: 0..0,
                    path_pin: false,
                    root_override_pin: false,
                },
            },
            path_pin_sources: Vec::new(),
        };

        validate_resolver_path_pins(&selection, |_| -> Result<&str, ()> {
            panic!("the canonicalizer must not run without path pins")
        })
        .expect("selection has no path pins to compare");
    }

    #[test]
    fn chooses_the_source_of_the_least_requirer_not_manifest_order() {
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
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let alpha_manifest = parse_for_role(
            br#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/alpha-dep" }
"#,
            PackageRole::Dependency,
        );
        let alpha_contract = build_resolver_contract(
            &alpha_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("dependency resolver contract is valid");
        let alpha_version = alpha_contract.mvs.package_version().clone();
        let alpha_edge = named_edge(
            &alpha_contract,
            "acme:dep",
            MvsRequirer::Package(alpha_version.clone()),
        );
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![
                named_edge(
                    &root_contract,
                    "acme:dep",
                    MvsRequirer::Root(root_contract.mvs.package_version().clone()),
                ),
                alpha_edge.clone(),
            ],
            lock_floor: false,
        };
        let dependencies = HashMap::from([(alpha_version, alpha_contract)]);

        let selection = select_resolver_source(
            &request,
            &root_contract,
            &context,
            &dependencies,
            &HashMap::new(),
        )
        .expect("named requirement source is available");

        assert_eq!(selection.reason, ResolverSourceReason::NamedRequirement);
        assert_eq!(
            selection.provenance,
            ResolverSourceProvenance::NamedRequirement { edge: alpha_edge }
        );
        assert_eq!(
            selection.source,
            ResolverPackageSource::Git {
                url: "https://example.test/alpha-dep".into(),
                directory: None,
            }
        );
    }

    #[test]
    fn validates_non_selected_path_pins_before_choosing_a_named_source() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/root-dep" }
"#,
        );
        let caller_version = package_version("acme:zz", "1.0.0");
        let path_packages = BTreeMap::from([(
            crate::resolver_contract::ResolverPathKey::new(caller_version.clone(), "../dep"),
            package_version("acme:dep", "1.0.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let caller_manifest = parse_for_role(
            br#"package acme:zz @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
            PackageRole::Dependency,
        );
        let caller_contract = build_resolver_contract(
            &caller_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("path-requiring dependency contract is valid");
        let caller_edge = named_edge(
            &caller_contract,
            "acme:dep",
            MvsRequirer::Package(caller_version.clone()),
        );
        let root_edge = named_edge(
            &root_contract,
            "acme:dep",
            MvsRequirer::Root(root_contract.mvs.package_version().clone()),
        );
        let caller_span = caller_edge.span.clone();
        let mut request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![root_edge, caller_edge],
            lock_floor: false,
        };
        request
            .named_by
            .iter_mut()
            .find(|edge| matches!(&edge.requirer, MvsRequirer::Package(_)))
            .expect("path edge is present")
            .path_pin = false;
        let dependencies = HashMap::from([(caller_version.clone(), caller_contract)]);

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &context,
                &dependencies,
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::RequestPinMismatch {
                requirer: caller_version,
                package: "acme:dep".into(),
                span: caller_span,
            })
        );
    }

    #[test]
    fn rejects_a_root_contract_built_from_another_same_identity_context() {
        let first_root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/first" }
"#,
        );
        let second_root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/second" }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let first_context = root_context(&first_root, &path_packages, &root_path_overrides);
        let second_context = root_context(&second_root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&first_root, &first_context);
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![named_edge(
                &root_contract,
                "acme:dep",
                MvsRequirer::Root(root_contract.mvs.package_version().clone()),
            )],
            lock_floor: false,
        };

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &second_context,
                &HashMap::new(),
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::StaleRootContractContext)
        );
    }

    #[test]
    fn rejects_a_requirer_contract_built_from_another_root_context() {
        let first_root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let second_root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-wasi }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let first_context = root_context(&first_root, &path_packages, &root_path_overrides);
        let second_context = root_context(&second_root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&first_root, &first_context);
        let dependency_manifest = parse_for_role(
            br#"package acme:caller @ 1.0.0
source "src"
targets { wasm32-core, wasm32-wasi }
requires {
    acme:dep @ ^1 from git "https://example.test/dep" only { wasm32-wasi }
}
"#,
            PackageRole::Dependency,
        );
        let dependency_contract = build_resolver_contract(
            &dependency_manifest,
            &second_context,
            PackageValidationContext::default(),
        )
        .expect("dependency contract is valid under its root target set");
        let requirer = dependency_contract.mvs.package_version().clone();
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![named_edge(
                &dependency_contract,
                "acme:dep",
                MvsRequirer::Package(requirer.clone()),
            )],
            lock_floor: false,
        };
        let dependencies = HashMap::from([(requirer.clone(), dependency_contract)]);

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &first_context,
                &dependencies,
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::StaleRequirerContractContext(requirer))
        );
    }

    #[test]
    fn rejects_a_dependency_contract_stored_under_another_package_version() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let dependency_manifest = parse_for_role(
            br#"package acme:caller @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/dep" }
"#,
            PackageRole::Dependency,
        );
        let dependency_contract = build_resolver_contract(
            &dependency_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("dependency contract is valid");
        let actual_requirer = dependency_contract.mvs.package_version().clone();
        let expected_requirer = package_version("acme:caller", "2.0.0");
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.0.0"),
            named_by: vec![named_edge(
                &dependency_contract,
                "acme:dep",
                MvsRequirer::Package(expected_requirer.clone()),
            )],
            lock_floor: false,
        };
        let dependencies = HashMap::from([(expected_requirer.clone(), dependency_contract)]);

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &context,
                &dependencies,
                &HashMap::new(),
            ),
            Err(
                super::ResolverSourceSelectionError::RequirerContractMismatch {
                    expected: Box::new(expected_requirer),
                    found: Box::new(actual_requirer),
                }
            )
        );
    }

    #[test]
    fn root_git_override_replaces_sources_for_every_line() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^2 from git "https://example.test/original" }
overrides { acme:dep use git "https://example.test/fork" dir "packages/dep" }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "2.0.0"),
            named_by: vec![named_edge(
                &root_contract,
                "acme:dep",
                MvsRequirer::Root(root_contract.mvs.package_version().clone()),
            )],
            lock_floor: false,
        };

        let selection = select_resolver_source(
            &request,
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("root Git override is available");

        assert_eq!(selection.reason, ResolverSourceReason::RootGitOverride);
        let override_span = context
            .root_overrides()
            .iter()
            .find(|item| item.value.package.as_ref() == "acme:dep")
            .expect("root override is retained")
            .span
            .clone();
        assert_eq!(
            selection.provenance,
            ResolverSourceProvenance::RootGitOverride {
                span: override_span
            }
        );
        assert_eq!(
            selection.source,
            ResolverPackageSource::Git {
                url: "https://example.test/fork".into(),
                directory: Some("packages/dep".into()),
            }
        );

        let mut mismatched_request = request;
        let span = mismatched_request.named_by[0].span.clone();
        mismatched_request.package_version = package_version("acme:dep", "2.1.0");
        assert_eq!(
            select_resolver_source(
                &mismatched_request,
                &root_contract,
                &context,
                &HashMap::new(),
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::NamedVersionMismatch {
                requirer: root_contract.mvs.package_version().clone(),
                package: "acme:dep".into(),
                requested: Box::new(crate::version::Version::parse("2.1.0").unwrap()),
                named: Box::new(crate::version::Version::parse("2.0.0").unwrap()),
                span,
            })
        );
    }

    #[test]
    fn root_path_override_applies_only_to_its_pinned_line() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/original" }
overrides { acme:dep use path "../patched" }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides =
            BTreeMap::from([("acme:dep".into(), package_version("acme:dep", "1.5.0"))]);
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.5.0"),
            named_by: vec![named_edge(
                &root_contract,
                "acme:dep",
                MvsRequirer::Root(root_contract.mvs.package_version().clone()),
            )],
            lock_floor: false,
        };

        let selection = select_resolver_source(
            &request,
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("root path override is available");

        assert_eq!(selection.reason, ResolverSourceReason::RootPathOverride);
        let override_span = context
            .root_overrides()
            .iter()
            .find(|item| item.value.package.as_ref() == "acme:dep")
            .expect("root override is retained")
            .span
            .clone();
        assert_eq!(
            selection.provenance,
            ResolverSourceProvenance::RootPathOverride {
                span: override_span
            }
        );
        assert_eq!(
            selection.source,
            ResolverPackageSource::Path {
                relative_to: package_version("acme:app", "1.0.0"),
                path: "../patched".into(),
            }
        );

        let caller_manifest = parse_for_role(
            br#"package acme:caller @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^2 from git "https://example.test/line-two" }
"#,
            PackageRole::Dependency,
        );
        let caller_contract = build_resolver_contract(
            &caller_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("caller resolver contract is valid");
        let caller_version = caller_contract.mvs.package_version().clone();
        let other_line_request = MvsContractRequest {
            package_version: package_version("acme:dep", "2.0.0"),
            named_by: vec![named_edge(
                &caller_contract,
                "acme:dep",
                MvsRequirer::Package(caller_version.clone()),
            )],
            lock_floor: false,
        };
        let dependencies = HashMap::from([(caller_version, caller_contract)]);
        let other_line = select_resolver_source(
            &other_line_request,
            &root_contract,
            &context,
            &dependencies,
            &HashMap::new(),
        )
        .expect("another compatibility line uses its named source");
        assert_eq!(other_line.reason, ResolverSourceReason::NamedRequirement);
        assert_eq!(
            other_line.source,
            ResolverPackageSource::Git {
                url: "https://example.test/line-two".into(),
                directory: None,
            }
        );
    }

    #[test]
    fn direct_path_pins_return_all_paths_for_filesystem_comparison() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from path "../dep" }
"#,
        );
        let root_version = package_version("acme:app", "1.0.0");
        let path_packages = BTreeMap::from([(
            crate::resolver_contract::ResolverPathKey::new(root_version, "../dep"),
            package_version("acme:dep", "1.5.0"),
        )]);
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.5.0"),
            named_by: vec![named_edge(
                &root_contract,
                "acme:dep",
                MvsRequirer::Root(root_contract.mvs.package_version().clone()),
            )],
            lock_floor: false,
        };

        let selection = select_resolver_source(
            &request,
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("path pin source is available");

        assert_eq!(selection.reason, ResolverSourceReason::PathPin);
        assert_eq!(selection.path_pin_sources.len(), 1);
        assert_eq!(
            selection.provenance,
            ResolverSourceProvenance::PathPin {
                pin: selection.path_pin_sources[0].clone()
            }
        );
        assert_eq!(
            selection.source,
            ResolverPackageSource::Path {
                relative_to: package_version("acme:app", "1.0.0"),
                path: "../dep".into(),
            }
        );

        let mut wrong_pin_request = request;
        let pin_span = wrong_pin_request.named_by[0].span.clone();
        wrong_pin_request.named_by[0].path_pin = false;
        assert_eq!(
            select_resolver_source(
                &wrong_pin_request,
                &root_contract,
                &context,
                &HashMap::new(),
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::RequestPinMismatch {
                requirer: root_contract.mvs.package_version().clone(),
                package: "acme:dep".into(),
                span: pin_span,
            })
        );
    }

    #[test]
    fn rejects_a_request_version_not_named_by_its_requirement() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/dep" }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let request_span = named_edge(
            &root_contract,
            "acme:dep",
            MvsRequirer::Root(root_contract.mvs.package_version().clone()),
        )
        .span;
        let request = MvsContractRequest {
            package_version: package_version("acme:dep", "1.1.0"),
            named_by: vec![MvsNamedEdge {
                requirer: MvsRequirer::Root(root_contract.mvs.package_version().clone()),
                span: request_span.clone(),
                path_pin: false,
                root_override_pin: false,
            }],
            lock_floor: false,
        };

        assert_eq!(
            select_resolver_source(
                &request,
                &root_contract,
                &context,
                &HashMap::new(),
                &HashMap::new(),
            ),
            Err(super::ResolverSourceSelectionError::NamedVersionMismatch {
                requirer: root_contract.mvs.package_version().clone(),
                package: "acme:dep".into(),
                requested: Box::new(crate::version::Version::parse("1.1.0").unwrap()),
                named: Box::new(crate::version::Version::parse("1.0.0").unwrap()),
                span: request_span,
            })
        );
    }

    #[test]
    fn lock_source_is_used_only_for_a_floor_only_request() {
        let root = parse(
            br#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
"#,
        );
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = root_context(&root, &path_packages, &root_path_overrides);
        let root_contract = build_root_contract(&root, &context);
        let target = package_version("acme:dep", "1.8.0");
        let request = MvsContractRequest {
            package_version: target.clone(),
            named_by: Vec::new(),
            lock_floor: true,
        };
        let locked_sources = HashMap::from([(
            target,
            ResolverPackageSource::Git {
                url: "https://example.test/locked".into(),
                directory: None,
            },
        )]);

        let selection = select_resolver_source(
            &request,
            &root_contract,
            &context,
            &HashMap::new(),
            &locked_sources,
        )
        .expect("lock source is available");

        assert_eq!(selection.reason, ResolverSourceReason::LockFloor);
        assert_eq!(
            selection.provenance,
            ResolverSourceProvenance::LockFloor {
                package_version: package_version("acme:dep", "1.8.0")
            }
        );
        assert_eq!(
            selection.source,
            ResolverPackageSource::Git {
                url: "https://example.test/locked".into(),
                directory: None,
            }
        );
    }
}
