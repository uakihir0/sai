//! Complete-graph source selection from a validated `sai.lock`.
//!
//! This plans Mode A condition 6 before target/test activation. It never reads
//! package contents or resolves local paths. The caller must canonicalize each
//! returned path pin against its requiring package's locked source and compare
//! all of them with the target entry, then verify every pinned package under
//! Mode A condition 5.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::lock::{
    LockPackage, LockPackageField, LockParseOutput, LockRequirement, LockRootField, LockSource,
};
use crate::lock_graph_validation::{
    LockGraphNode, LockGraphValidationInputError, LockGraphValidationOutput,
    validate as validate_lock_graph,
};
use crate::lock_validation::{LockValidationOutput, validate as validate_lock};
use crate::manifest::{OverrideSource, RequirementSource};
use crate::mvs::{MvsPackageVersion, MvsRequirer};
use crate::package_path::{
    DependencyPackagePath, PackagePathError, RepositoryPackageDirectory,
    normalize_git_relative_package_directory,
};
use crate::resolver_contract::ResolverContractContext;
use crate::version::CompatibilityLine;

/// The validated source recorded for one package entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockPackageSource {
    pub package_version: MvsPackageVersion,
    pub source: LockSource,
    pub directory: Option<Box<str>>,
    pub commit: Option<Box<str>>,
}

/// The source-relative base of one locked path pin.
///
/// `None` denotes a root requirement or root path override. Package bases
/// retain the exact lock source, including Git commit and repository
/// directory, so a consumer cannot reinterpret a dependency-relative path
/// against the process working directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockPathPinBase {
    pub requirer: MvsRequirer,
    pub source: Option<LockPackageSource>,
}

/// Why a locked path source pins its target node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockPathPinKind {
    Requirement,
    RootPathOverride,
}

/// One path pin from the full recorded graph or a current root override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockPathPin {
    /// Exact node selected by the lock edge.
    pub target: MvsPackageVersion,
    /// For a root path override, this is the current package version read
    /// from that override path. Ordinary `from path` edges are checked against
    /// the current target contract after the package is loaded.
    pub declared_version: Option<MvsPackageVersion>,
    pub base: LockPathPinBase,
    pub path: Box<str>,
    pub kind: LockPathPinKind,
    pub span: Range<usize>,
}

/// The precedence rule that selected a lock entry's source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockSourceSelectionReason {
    RootPathOverride,
    RootGitOverride,
    PathPin,
    NamedRequirement,
    LockFloor,
}

/// The source-selection result for a dependency entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSelectedSource {
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
    /// The caller resolves every pin and confirms that they identify the
    /// same package as `LockSourcePlan::actual`.
    PathPins(Vec<LockPathPin>),
    /// No recorded edge named this exact version. The recorded source is the
    /// lock floor under resolver §2.8.1 rule 4.
    LockFloor,
}

/// A source-selection plan for one lock entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockSourcePlan {
    pub package_version: MvsPackageVersion,
    pub reason: LockSourceSelectionReason,
    pub selected: LockSelectedSource,
    /// Every effective path pin to this node, including pins not chosen as
    /// the source winner. Condition 5 validates all of them.
    pub path_pins: Vec<LockPathPin>,
    pub actual: LockPackageSource,
}

/// Resolves local path references while checking a source-selection plan.
///
/// Implementations should use the current filesystem snapshot for local
/// package sources and the exact locked URL/commit/directory for Git-backed
/// pins. They must not resolve a dependency-relative path from the process
/// working directory.
pub trait LockSourceCanonicalizer {
    type Key: Eq;
    type Error;

    fn canonicalize_pin(&mut self, pin: &LockPathPin) -> Result<Self::Key, Self::Error>;
    fn canonicalize_entry(&mut self, entry: &LockPackageSource) -> Result<Self::Key, Self::Error>;
}

/// Failure while checking one source-selection plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSourceSelectionCheckError<E> {
    PinCanonicalization {
        pin: Box<LockPathPin>,
        error: Box<E>,
    },
    EntryCanonicalization {
        entry: Box<LockPackageSource>,
        error: Box<E>,
    },
    ConflictingPathPins {
        first: Box<LockPathPin>,
        conflicting: Box<LockPathPin>,
    },
    SourceMismatch {
        plan: Box<LockSourcePlan>,
    },
}

/// Failure to derive a Git source identity from a locked path pin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockGitPathPinError {
    InvalidRequirerSource,
    MissingCommit,
    InvalidBaseDirectory(PackagePathError),
    InvalidDependencyPath(PackagePathError),
    PathEscapesRepository,
}

/// Why a source-selection plan cannot be constructed from the lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSourceSelectionInputError {
    InvalidLockSemantics(LockValidationOutput),
    InvalidLockSyntax,
    InvalidLockGraphInput(LockGraphValidationInputError),
    InvalidLockGraph(LockGraphValidationOutput),
    RootMismatch {
        lock: Box<MvsPackageVersion>,
        context: Box<MvsPackageVersion>,
    },
    MissingPackageSource(MvsPackageVersion),
    MissingEntryForEdge {
        package: Box<str>,
        line: CompatibilityLine,
        span: Range<usize>,
    },
    InvalidRootOverrideSourceUrl {
        span: Range<usize>,
    },
    FormatOneCannotSelectDependencySources,
}

/// Checks one complete-graph source-selection plan.
///
/// Named Git clauses are compared directly. Path pins are all canonicalized,
/// required to agree with each other, and compared with the entry's selected
/// source. `LockFloor` uses the package entry's recorded source by rule,
/// including a local path retained from an earlier root path override.
pub fn validate_lock_source_plan<C>(
    plan: &LockSourcePlan,
    canonicalizer: &mut C,
) -> Result<(), LockSourceSelectionCheckError<C::Error>>
where
    C: LockSourceCanonicalizer,
{
    match &plan.selected {
        LockSelectedSource::Git { url, directory } => {
            let matches = matches!(
                &plan.actual.source,
                LockSource::Git(actual_url)
                    if actual_url == url
                        && canonical_directory(plan.actual.directory.as_deref())
                            .as_deref()
                            == directory.as_deref()
            );
            if matches {
                Ok(())
            } else {
                Err(LockSourceSelectionCheckError::SourceMismatch {
                    plan: Box::new(plan.clone()),
                })
            }
        }
        LockSelectedSource::PathPins(pins) => {
            let Some(first_pin) = pins.first() else {
                return Err(LockSourceSelectionCheckError::SourceMismatch {
                    plan: Box::new(plan.clone()),
                });
            };
            let first_key = canonicalizer.canonicalize_pin(first_pin).map_err(|error| {
                LockSourceSelectionCheckError::PinCanonicalization {
                    pin: Box::new(first_pin.clone()),
                    error: Box::new(error),
                }
            })?;
            for pin in &pins[1..] {
                let key = canonicalizer.canonicalize_pin(pin).map_err(|error| {
                    LockSourceSelectionCheckError::PinCanonicalization {
                        pin: Box::new(pin.clone()),
                        error: Box::new(error),
                    }
                })?;
                if key != first_key {
                    return Err(LockSourceSelectionCheckError::ConflictingPathPins {
                        first: Box::new(first_pin.clone()),
                        conflicting: Box::new(pin.clone()),
                    });
                }
            }
            let entry_key = canonicalizer
                .canonicalize_entry(&plan.actual)
                .map_err(
                    |error| LockSourceSelectionCheckError::EntryCanonicalization {
                        entry: Box::new(plan.actual.clone()),
                        error: Box::new(error),
                    },
                )?;
            if entry_key == first_key {
                Ok(())
            } else {
                Err(LockSourceSelectionCheckError::SourceMismatch {
                    plan: Box::new(plan.clone()),
                })
            }
        }
        LockSelectedSource::LockFloor => Ok(()),
    }
}

/// Resolves a Git-backed `from path` clause against its requiring entry's
/// exact locked commit and directory.
///
/// Returns `Ok(None)` for a root or local-path base, which the caller must
/// resolve through its filesystem snapshot. The result preserves the
/// requiring package's repository and commit and rejects any path that would
/// escape the repository.
pub fn git_path_pin_source(
    pin: &LockPathPin,
) -> Result<Option<LockPackageSource>, LockGitPathPinError> {
    let base = match (&pin.base.requirer, &pin.base.source) {
        (MvsRequirer::Root(_), None) => return Ok(None),
        (MvsRequirer::Root(_), Some(_)) => {
            return Err(LockGitPathPinError::InvalidRequirerSource);
        }
        (MvsRequirer::Package(_), None) => {
            return Err(LockGitPathPinError::InvalidRequirerSource);
        }
        (MvsRequirer::Package(version), Some(source)) if &source.package_version == version => {
            source
        }
        (MvsRequirer::Package(_), Some(_)) => {
            return Err(LockGitPathPinError::InvalidRequirerSource);
        }
    };
    let LockSource::Git(url) = &base.source else {
        return Ok(None);
    };
    let commit = base
        .commit
        .as_ref()
        .ok_or(LockGitPathPinError::MissingCommit)?;
    let directory = base
        .directory
        .as_deref()
        .map(RepositoryPackageDirectory::parse)
        .transpose()
        .map_err(LockGitPathPinError::InvalidBaseDirectory)?;
    let dependency_path = DependencyPackagePath::parse(&pin.path)
        .map_err(LockGitPathPinError::InvalidDependencyPath)?;
    let directory = normalize_git_relative_package_directory(directory.as_ref(), &dependency_path)
        .map_err(|error| match error {
            PackagePathError::PathEscapesRepository => LockGitPathPinError::PathEscapesRepository,
            other => LockGitPathPinError::InvalidDependencyPath(other),
        })?;
    Ok(Some(LockPackageSource {
        package_version: pin.target.clone(),
        source: LockSource::Git(url.clone()),
        directory: (directory.as_str() != ".").then(|| directory.as_str().into()),
        commit: Some(commit.clone()),
    }))
}

#[derive(Clone, Debug)]
struct IncomingEdge {
    requirer: MvsRequirer,
    requirer_source: Option<LockPackageSource>,
    requirement: LockRequirement,
    span: Range<usize>,
}

/// Builds source-selection plans over every recorded lock edge before
/// activation pruning.
///
/// Format 1 remains usable for root-only locks, but its dependency-bearing
/// graphs do not carry enough edge metadata to prove condition 6. The caller
/// should report those locks as out of date and let `sai lock` rewrite them in
/// format 2.
pub fn plan_lock_source_selection(
    parsed: &LockParseOutput,
    root_context: &ResolverContractContext<'_>,
) -> Result<Vec<LockSourcePlan>, LockSourceSelectionInputError> {
    let local_validation = validate_lock(parsed);
    if !local_validation.is_valid() {
        return Err(LockSourceSelectionInputError::InvalidLockSemantics(
            local_validation,
        ));
    }
    if !parsed.is_syntax_valid() {
        return Err(LockSourceSelectionInputError::InvalidLockSyntax);
    }

    let graph_validation = validate_lock_graph(parsed)
        .map_err(LockSourceSelectionInputError::InvalidLockGraphInput)?;
    if !graph_validation.is_consistent() {
        return Err(LockSourceSelectionInputError::InvalidLockGraph(
            graph_validation,
        ));
    }

    let root = parsed
        .file()
        .root
        .as_ref()
        .expect("a validated lock graph has a root");
    let lock_root = MvsPackageVersion::new(root.value.package.clone(), root.value.version.clone())
        .expect("lock validation checks the root identity");
    if &lock_root != root_context.root_package() {
        return Err(LockSourceSelectionInputError::RootMismatch {
            lock: Box::new(lock_root.clone()),
            context: Box::new(root_context.root_package().clone()),
        });
    }

    let mut entries_by_node = BTreeMap::<LockGraphNode, LockPackageSource>::new();
    let mut ordered_entries = Vec::with_capacity(parsed.file().packages.len());
    for package in &parsed.file().packages {
        let source = package_source(&package.value).ok_or_else(|| {
            LockSourceSelectionInputError::MissingPackageSource(
                MvsPackageVersion::new(
                    package.value.package.clone(),
                    package.value.version.clone(),
                )
                .expect("lock validation checks package identities"),
            )
        })?;
        let node = LockGraphNode {
            package: source.package_version.package().into(),
            line: source.package_version.version().compatibility_line(),
        };
        entries_by_node.insert(node, source.clone());
        ordered_entries.push(source);
    }
    ordered_entries.sort_by(|left, right| left.package_version.cmp(&right.package_version));

    let format_version = parsed.file().format_version.unwrap_or(0);
    if format_version == 1
        && (!ordered_entries.is_empty()
            || root
                .value
                .fields
                .iter()
                .any(|field| matches!(field.value, LockRootField::Requires(_))))
    {
        return Err(LockSourceSelectionInputError::FormatOneCannotSelectDependencySources);
    }
    if ordered_entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut incoming = BTreeMap::<MvsPackageVersion, Vec<IncomingEdge>>::new();
    let root_requirer = MvsRequirer::Root(lock_root.clone());
    for field in &root.value.fields {
        let LockRootField::Requires(requirement) = &field.value else {
            continue;
        };
        append_incoming_edge(
            &mut incoming,
            &entries_by_node,
            &root_requirer,
            None,
            requirement,
            field.span.clone(),
        )?;
    }
    for package in &parsed.file().packages {
        let requirer_source = package_source(&package.value);
        let requirer_version =
            MvsPackageVersion::new(package.value.package.clone(), package.value.version.clone())
                .expect("lock validation checks package identities");
        let requirer = MvsRequirer::Package(requirer_version);
        for field in &package.value.fields {
            let LockPackageField::Requires(requirement) = &field.value else {
                continue;
            };
            append_incoming_edge(
                &mut incoming,
                &entries_by_node,
                &requirer,
                requirer_source.clone(),
                requirement,
                field.span.clone(),
            )?;
        }
    }

    let mut plans = Vec::with_capacity(ordered_entries.len());
    for actual in ordered_entries {
        let package_version = actual.package_version.clone();
        let edges = incoming.remove(&package_version).unwrap_or_default();
        let root_override = root_override_for_node(root_context, &package_version)?;

        let (reason, selected, path_pins) = match root_override {
            Some(RootOverride::Git { url, directory }) => (
                LockSourceSelectionReason::RootGitOverride,
                LockSelectedSource::Git { url, directory },
                Vec::new(),
            ),
            Some(RootOverride::Path {
                path,
                declared_version,
                span,
            }) => {
                let pin = LockPathPin {
                    target: package_version.clone(),
                    declared_version: Some(declared_version),
                    base: LockPathPinBase {
                        requirer: MvsRequirer::Root(lock_root.clone()),
                        source: None,
                    },
                    path,
                    kind: LockPathPinKind::RootPathOverride,
                    span,
                };
                (
                    LockSourceSelectionReason::RootPathOverride,
                    LockSelectedSource::PathPins(vec![pin.clone()]),
                    vec![pin],
                )
            }
            None => {
                let mut pins = edges
                    .iter()
                    .filter_map(|edge| {
                        let source = edge.requirement.source.as_ref()?;
                        let RequirementSource::Path(path) = &source.value else {
                            return None;
                        };
                        Some(LockPathPin {
                            target: package_version.clone(),
                            declared_version: None,
                            base: LockPathPinBase {
                                requirer: edge.requirer.clone(),
                                source: edge.requirer_source.clone(),
                            },
                            path: path.clone(),
                            kind: LockPathPinKind::Requirement,
                            span: source.span.clone(),
                        })
                    })
                    .collect::<Vec<_>>();
                pins.sort_by(compare_path_pins);
                if !pins.is_empty() {
                    (
                        LockSourceSelectionReason::PathPin,
                        LockSelectedSource::PathPins(pins.clone()),
                        pins,
                    )
                } else {
                    let mut named = edges
                        .iter()
                        .filter_map(|edge| {
                            (edge.requirement.requirement.minimum() == package_version.version())
                                .then_some(edge)
                        })
                        .collect::<Vec<_>>();
                    named.sort_by(|left, right| {
                        left.requirer
                            .package_version()
                            .cmp(right.requirer.package_version())
                            .then_with(|| left.span.start.cmp(&right.span.start))
                            .then_with(|| left.span.end.cmp(&right.span.end))
                    });
                    if let Some(edge) = named.first() {
                        let source = edge
                            .requirement
                            .source
                            .as_ref()
                            .expect("format 2 local validation requires edge sources");
                        let RequirementSource::Git { url, directory } = &source.value else {
                            unreachable!("all path sources were collected as pins");
                        };
                        (
                            LockSourceSelectionReason::NamedRequirement,
                            LockSelectedSource::Git {
                                url: url.clone(),
                                directory: canonical_directory(directory.as_deref()),
                            },
                            Vec::new(),
                        )
                    } else {
                        (
                            LockSourceSelectionReason::LockFloor,
                            LockSelectedSource::LockFloor,
                            Vec::new(),
                        )
                    }
                }
            }
        };

        plans.push(LockSourcePlan {
            package_version,
            reason,
            selected,
            path_pins,
            actual,
        });
    }
    Ok(plans)
}

enum RootOverride {
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
    Path {
        path: Box<str>,
        declared_version: MvsPackageVersion,
        span: Range<usize>,
    },
}

fn root_override_for_node(
    root_context: &ResolverContractContext<'_>,
    package_version: &MvsPackageVersion,
) -> Result<Option<RootOverride>, LockSourceSelectionInputError> {
    for item in root_context.root_overrides() {
        if item.value.package.as_ref() != package_version.package() {
            continue;
        }
        match &item.value.source {
            OverrideSource::Git { url, directory } => {
                let url = crate::lock_validation::canonical_git_url(url).map_err(|_| {
                    LockSourceSelectionInputError::InvalidRootOverrideSourceUrl {
                        span: item.span.clone(),
                    }
                })?;
                return Ok(Some(RootOverride::Git {
                    url: url.into(),
                    directory: canonical_directory(directory.as_deref()),
                }));
            }
            OverrideSource::Path(path) => {
                let Some(declared_version) =
                    root_context.root_path_override(package_version.package())
                else {
                    return Ok(None);
                };
                if declared_version.version().compatibility_line()
                    == package_version.version().compatibility_line()
                {
                    return Ok(Some(RootOverride::Path {
                        path: path.clone(),
                        declared_version: declared_version.clone(),
                        span: item.span.clone(),
                    }));
                }
            }
            OverrideSource::Artifact => {}
        }
    }
    Ok(None)
}

fn append_incoming_edge(
    incoming: &mut BTreeMap<MvsPackageVersion, Vec<IncomingEdge>>,
    entries: &BTreeMap<LockGraphNode, LockPackageSource>,
    requirer: &MvsRequirer,
    requirer_source: Option<LockPackageSource>,
    requirement: &LockRequirement,
    span: Range<usize>,
) -> Result<(), LockSourceSelectionInputError> {
    let node = LockGraphNode {
        package: requirement.package.clone(),
        line: requirement.selected.compatibility_line(),
    };
    let Some(target) = entries.get(&node) else {
        return Err(LockSourceSelectionInputError::MissingEntryForEdge {
            package: requirement.package.clone(),
            line: node.line,
            span,
        });
    };
    if target.package_version.version() != &requirement.selected {
        return Err(LockSourceSelectionInputError::MissingEntryForEdge {
            package: requirement.package.clone(),
            line: node.line,
            span,
        });
    }
    incoming
        .entry(target.package_version.clone())
        .or_default()
        .push(IncomingEdge {
            requirer: requirer.clone(),
            requirer_source,
            requirement: requirement.clone(),
            span,
        });
    Ok(())
}

fn package_source(package: &LockPackage) -> Option<LockPackageSource> {
    let package_version =
        MvsPackageVersion::new(package.package.clone(), package.version.clone()).ok()?;
    let mut source = None;
    let mut directory = None;
    let mut commit = None;
    for field in &package.fields {
        match &field.value {
            LockPackageField::Source(value) => source = Some(value.clone()),
            LockPackageField::Directory(value) => directory = Some(value.clone()),
            LockPackageField::Commit(value) => commit = Some(value.clone()),
            _ => {}
        }
    }
    Some(LockPackageSource {
        package_version,
        source: source?,
        directory: directory.and_then(|value| (value.as_ref() != ".").then_some(value)),
        commit,
    })
}

fn canonical_directory(directory: Option<&str>) -> Option<Box<str>> {
    directory
        .filter(|directory| *directory != ".")
        .map(Into::into)
}

fn compare_path_pins(left: &LockPathPin, right: &LockPathPin) -> std::cmp::Ordering {
    left.base
        .requirer
        .package_version()
        .cmp(right.base.requirer.package_version())
        .then_with(|| left.span.start.cmp(&right.span.start))
        .then_with(|| left.span.end.cmp(&right.span.end))
        .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::lock::parse;
    use crate::manifest::{PackageRole, parse_for_role};
    use crate::resolver_contract::ResolverContractContext;
    use crate::validation::PackageValidationContext;

    use super::{LockSelectedSource, LockSourceSelectionReason, plan_lock_source_selection};

    const CHECKSUM: &str = "sai1:0123456789012345678901234567890123456789012345678901234567890123";
    const COMMIT: &str = "0123456789012345678901234567890123456789";

    fn package_version(package: &str) -> crate::mvs::MvsPackageVersion {
        package_version_at(package, "1.0.0")
    }

    fn package_version_at(package: &str, version: &str) -> crate::mvs::MvsPackageVersion {
        crate::mvs::MvsPackageVersion::new(
            package,
            crate::version::Version::parse(version).expect("version is valid"),
        )
        .expect("package version is valid")
    }

    fn git_source(package_version: crate::mvs::MvsPackageVersion) -> super::LockPackageSource {
        super::LockPackageSource {
            package_version,
            source: crate::lock::LockSource::Git("https://example.invalid/mono".into()),
            directory: Some("packages/mono".into()),
            commit: Some(COMMIT.into()),
        }
    }

    fn path_pin(
        requirer: crate::mvs::MvsRequirer,
        source: Option<super::LockPackageSource>,
        path: &str,
    ) -> super::LockPathPin {
        super::LockPathPin {
            target: package_version("acme:target"),
            declared_version: None,
            base: super::LockPathPinBase { requirer, source },
            path: path.into(),
            kind: super::LockPathPinKind::Requirement,
            span: 0..1,
        }
    }

    fn context() -> ResolverContractContext<'static> {
        let root = parse_for_role(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-wasi, wasm32-core }\n",
            PackageRole::Root,
        );
        let path_packages = BTreeMap::new();
        let path_overrides = BTreeMap::new();
        ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context")
        .into_owned()
    }

    fn package(package: &str, version: &str, url: &str, edge: Option<&str>) -> String {
        package_in_dir(package, version, url, None, edge)
    }

    fn package_in_dir(
        package: &str,
        version: &str,
        url: &str,
        directory: Option<&str>,
        edge: Option<&str>,
    ) -> String {
        let mut source = format!(
            "package {package} @ {version}\n\
             source git \"{url}\"\n\
             commit \"{COMMIT}\"\n\
             checksum \"{CHECKSUM}\"\n\
             capabilities {{ }}\n"
        );
        if let Some(directory) = directory {
            source.push_str(&format!("dir \"{directory}\"\n"));
        }
        if let Some(edge) = edge {
            source.push_str("    ");
            source.push_str(edge);
            source.push('\n');
        }
        source
    }

    fn lock(root_edges: &[&str], packages: &[String]) -> String {
        let mut source = String::from(
            "lock 2\n\
             root acme:root @ 1.0.0\n\
                 targets { wasm32-wasi, wasm32-core }\n",
        );
        for edge in root_edges {
            source.push_str("    ");
            source.push_str(edge);
            source.push('\n');
        }
        for package in packages {
            source.push('\n');
            source.push_str(package);
        }
        source
    }

    #[test]
    fn chooses_the_least_requirer_source_among_edges_naming_the_selected_version() {
        let source_b = "https://example.invalid/source-b";
        let parsed = parse(
            lock(
                &[
                    "requires acme:z @ ^1 -> 1.0.0 from git \"https://example.invalid/source-a\"",
                    "requires acme:a @ ^1 -> 1.0.0 from git \"https://example.invalid/source-a\"",
                ],
                &[
                    package(
                        "acme:a",
                        "1.0.0",
                        "https://example.invalid/a",
                        Some("requires acme:z @ ^1 -> 1.0.0 from git \"https://example.invalid/source-b\""),
                    ),
                    package("acme:z", "1.0.0", source_b, None),
                ],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let selected = plans
            .iter()
            .find(|plan| plan.package_version.package() == "acme:z")
            .expect("z plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::NamedRequirement);
        assert_eq!(
            selected.selected,
            LockSelectedSource::Git {
                url: source_b.into(),
                directory: None,
            }
        );
    }

    #[test]
    fn ignores_edges_that_do_not_name_the_selected_version() {
        let source_b = "https://example.invalid/source-b";
        let parsed = parse(
            lock(
                &[
                    "requires acme:z @ ^1 -> 1.1.0 from git \"https://example.invalid/source-a\"",
                    "requires acme:a @ ^1 -> 1.1.0 from git \"https://example.invalid/a\"",
                ],
                &[
                    package(
                        "acme:a",
                        "1.1.0",
                        "https://example.invalid/a",
                        Some("requires acme:z @ >=1.1 -> 1.1.0 from git \"https://example.invalid/source-b\""),
                    ),
                    package("acme:z", "1.1.0", source_b, None),
                ],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let selected = plans
            .iter()
            .find(|plan| plan.package_version.package() == "acme:z")
            .expect("z plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::NamedRequirement);
        assert_eq!(
            selected.selected,
            LockSelectedSource::Git {
                url: source_b.into(),
                directory: None,
            }
        );
    }

    #[test]
    fn selects_the_recorded_source_when_the_version_is_only_a_lock_floor() {
        let floor_source = "https://example.invalid/floor";
        let parsed = parse(
            lock(
                &["requires acme:a @ ^1 -> 1.1.0 from git \"https://example.invalid/old\""],
                &[package("acme:a", "1.1.0", floor_source, None)],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let selected = plans.first().expect("one plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::LockFloor);
        assert_eq!(selected.selected, LockSelectedSource::LockFloor);
        assert_eq!(
            selected.actual.source,
            crate::lock::LockSource::Git(floor_source.into())
        );
    }

    #[test]
    fn retains_a_local_path_source_as_the_lock_floor_after_an_override_is_removed() {
        let parsed = parse(
            lock(
                &["requires acme:dep @ ^1 -> 1.2.0 from git \"https://example.invalid/dep\""],
                &["package acme:dep @ 1.2.0\n    source path \"vendor/dep\"\n".into()],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let selected = plans.first().expect("one plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::LockFloor);
        assert_eq!(selected.selected, LockSelectedSource::LockFloor);
        assert_eq!(
            selected.actual.source,
            crate::lock::LockSource::Path("vendor/dep".into())
        );
        let mut canonicalizer = NoopCanonicalizer;
        super::validate_lock_source_plan(selected, &mut canonicalizer)
            .expect("the recorded floor source is the selected source");
    }

    #[test]
    fn collects_inactive_path_pins_with_their_locked_requirer_source() {
        let parsed = parse(
            lock(
                &["requires acme:mono @ ^1 -> 1.0.0 from git \"https://example.invalid/mono\""],
                &[
                    package_in_dir(
                        "acme:mono",
                        "1.0.0",
                        "https://example.invalid/mono",
                        Some("packages/mono"),
                        Some("requires acme:sibling @ ^1 -> 1.0.0 from path \"../sibling\" only { wasm32-core }"),
                    ),
                    package_in_dir(
                        "acme:sibling",
                        "1.0.0",
                        "https://example.invalid/mono",
                        Some("packages/sibling"),
                        None,
                    ),
                ],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let selected = plans
            .iter()
            .find(|plan| plan.package_version.package() == "acme:sibling")
            .expect("sibling plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::PathPin);
        assert_eq!(selected.path_pins.len(), 1);
        assert_eq!(selected.path_pins[0].path.as_ref(), "../sibling");
        let base = selected.path_pins[0]
            .base
            .source
            .as_ref()
            .expect("dependency requirer source");
        assert_eq!(
            base.source,
            crate::lock::LockSource::Git("https://example.invalid/mono".into())
        );
        assert_eq!(base.commit.as_deref(), Some(COMMIT));
    }

    #[test]
    fn git_path_pin_keeps_the_locked_repository_commit_and_normalizes_its_directory() {
        let mono = package_version("acme:mono");
        let pin = path_pin(
            crate::mvs::MvsRequirer::Package(mono.clone()),
            Some(git_source(mono)),
            "../sibling",
        );
        let source = super::git_path_pin_source(&pin)
            .expect("pin path is valid")
            .expect("Git-backed pin has a source");
        assert_eq!(
            source.source,
            crate::lock::LockSource::Git("https://example.invalid/mono".into())
        );
        assert_eq!(source.directory.as_deref(), Some("packages/sibling"));
        assert_eq!(source.commit.as_deref(), Some(COMMIT));
    }

    #[test]
    fn git_path_pin_rejects_paths_that_escape_the_locked_repository() {
        let mono = package_version("acme:mono");
        let pin = path_pin(
            crate::mvs::MvsRequirer::Package(mono.clone()),
            Some(git_source(mono)),
            "../../../outside",
        );
        assert_eq!(
            super::git_path_pin_source(&pin),
            Err(super::LockGitPathPinError::PathEscapesRepository)
        );
    }

    #[test]
    fn git_path_pin_rejects_source_anchors_that_do_not_match_the_requirer() {
        let source = git_source(package_version("acme:mono"));
        let invalid_pins = [
            path_pin(
                crate::mvs::MvsRequirer::Root(package_version("acme:root")),
                Some(source.clone()),
                "../sibling",
            ),
            path_pin(
                crate::mvs::MvsRequirer::Package(package_version_at("acme:mono", "2.0.0")),
                Some(source),
                "../sibling",
            ),
        ];

        for pin in invalid_pins {
            assert_eq!(
                super::git_path_pin_source(&pin),
                Err(super::LockGitPathPinError::InvalidRequirerSource)
            );
        }
    }

    #[test]
    fn applies_current_root_git_override_to_the_preserved_declared_source() {
        let declared = "https://example.invalid/original";
        let overridden = "https://example.invalid/override";
        let override_spelling = "HTTPS://EXAMPLE.INVALID:443/override/";
        let parsed = parse(
            lock(
                &[&format!(
                    "requires acme:z @ ^1 -> 1.0.0 from git \"{declared}\""
                )],
                &[package("acme:z", "1.0.0", overridden, None)],
            )
            .as_bytes(),
        );

        let overridden_root = parse_for_role(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-wasi, wasm32-core }\n\
              overrides { acme:z use git \"HTTPS://EXAMPLE.INVALID:443/override/\" }\n",
            PackageRole::Root,
        );
        let empty_path_packages = BTreeMap::new();
        let empty_path_overrides = BTreeMap::new();
        let overridden_context = ResolverContractContext::from_root_manifest(
            &overridden_root,
            &empty_path_packages,
            &empty_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root override")
        .into_owned();
        let overridden_plan =
            plan_lock_source_selection(&parsed, &overridden_context).expect("valid lock");
        let selected = overridden_plan.first().expect("one plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::RootGitOverride);
        assert_eq!(
            selected.selected,
            LockSelectedSource::Git {
                url: overridden.into(),
                directory: None,
            }
        );
        assert_ne!(override_spelling, overridden);

        let without_override = context();
        let original_plan =
            plan_lock_source_selection(&parsed, &without_override).expect("valid lock");
        let selected = original_plan.first().expect("one plan");
        assert_eq!(selected.reason, LockSourceSelectionReason::NamedRequirement);
        assert_eq!(
            selected.selected,
            LockSelectedSource::Git {
                url: declared.into(),
                directory: None,
            }
        );
        let mut canonicalizer = NoopCanonicalizer;
        assert!(matches!(
            super::validate_lock_source_plan(selected, &mut canonicalizer),
            Err(super::LockSourceSelectionCheckError::SourceMismatch { .. })
        ));
    }

    #[test]
    fn rejects_an_invalid_current_root_git_override_url() {
        let parsed = parse(
            lock(
                &["requires acme:z @ ^1 -> 1.0.0 from git \"https://example.invalid/z\""],
                &[package(
                    "acme:z",
                    "1.0.0",
                    "https://example.invalid/z",
                    None,
                )],
            )
            .as_bytes(),
        );
        let invalid_override_root = parse_for_role(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-wasi, wasm32-core }\n\
              overrides { acme:z use git \"file://example.invalid/z\" }\n",
            PackageRole::Root,
        );
        let empty_path_packages = BTreeMap::new();
        let empty_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &invalid_override_root,
            &empty_path_packages,
            &empty_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("URL validation is performed during source selection")
        .into_owned();
        assert!(matches!(
            plan_lock_source_selection(&parsed, &context),
            Err(super::LockSourceSelectionInputError::InvalidRootOverrideSourceUrl { .. })
        ));
    }

    struct NoopCanonicalizer;

    impl super::LockSourceCanonicalizer for NoopCanonicalizer {
        type Key = ();
        type Error = ();

        fn canonicalize_pin(&mut self, _: &super::LockPathPin) -> Result<Self::Key, Self::Error> {
            Err(())
        }

        fn canonicalize_entry(
            &mut self,
            _: &super::LockPackageSource,
        ) -> Result<Self::Key, Self::Error> {
            Err(())
        }
    }

    #[test]
    fn validates_matching_git_path_pins_against_the_actual_entry() {
        let parsed = parse(
            lock(
                &["requires acme:mono @ ^1 -> 1.0.0 from git \"https://example.invalid/mono\""],
                &[
                    package_in_dir(
                        "acme:mono",
                        "1.0.0",
                        "https://example.invalid/mono",
                        Some("packages/mono"),
                        Some("requires acme:sibling @ ^1 -> 1.0.0 from path \"../sibling\""),
                    ),
                    package_in_dir(
                        "acme:sibling",
                        "1.0.0",
                        "https://example.invalid/mono",
                        Some("packages/sibling"),
                        None,
                    ),
                ],
            )
            .as_bytes(),
        );
        let plans = plan_lock_source_selection(&parsed, &context()).expect("valid lock");
        let plan = plans
            .iter()
            .find(|plan| plan.package_version.package() == "acme:sibling")
            .expect("sibling plan");
        let mut canonicalizer = GitPathCanonicalizer;
        super::validate_lock_source_plan(plan, &mut canonicalizer)
            .expect("path pin and entry identify the same source");
    }

    struct GitPathCanonicalizer;

    impl super::LockSourceCanonicalizer for GitPathCanonicalizer {
        type Key = super::LockPackageSource;
        type Error = super::LockGitPathPinError;

        fn canonicalize_pin(&mut self, pin: &super::LockPathPin) -> Result<Self::Key, Self::Error> {
            super::git_path_pin_source(pin)?.ok_or(super::LockGitPathPinError::MissingCommit)
        }

        fn canonicalize_entry(
            &mut self,
            entry: &super::LockPackageSource,
        ) -> Result<Self::Key, Self::Error> {
            Ok(entry.clone())
        }
    }
}
