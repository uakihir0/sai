//! Filesystem primitives for local path package sources.
//!
//! Git-backed package trees are resolved by a separate provider. This module
//! handles local package directories without invoking external tools.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use rustix::fs::{CWD, Mode, OFlags, openat};
#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use std::os::unix::fs::MetadataExt;

use crate::manifest::{
    MAX_PACKAGE_MANIFEST_BYTES, OverrideSource, PackageDeclaration, PackageParseOutput,
    PackageRole, RequirementSource, Spanned, parse_for_role,
};
use crate::mvs::{MvsNode, MvsPackageVersion, MvsRequirer};
use crate::package_path::{DependencyPackagePath, PackagePathError};
use crate::resolver_contract::{
    ResolverContract, ResolverContractBuildError, ResolverContractContext, ResolverPathKey,
    build_resolver_contract as adapt_resolver_contract,
};
use crate::resolver_source::{
    ResolverPackageSource, ResolverPathPinSource, ResolverPathPinValidationError,
    ResolverSourceProvenance, ResolverSourceReason, ResolverSourceSelection,
    validate_resolver_path_pins,
};
use crate::resolver_source_provider::{
    ResolverLoadedContract, ResolverLoadedRootContract, ResolverSourceProviderError,
    ResolverSourceProviderIdentifiedOutput, ResolverSourceProviderIdentityLoadError,
    plan_resolver_final_sources, select_with_source_provider_and_canonicalized_pins_and_identities,
};
use crate::validation::PackageValidationContext;
use crate::version::Version;

/// A local package directory identified after filesystem path resolution.
///
/// Equality compares the opened directory identity on supported Unix targets
/// and the canonical path elsewhere. Dependency resolution is supported only
/// on Unix targets where it can be anchored to an open directory handle.
/// Other hosts fail closed because path-string identity cannot detect a
/// replaced directory reliably. The path is intentionally exposed only to
/// loader code; callers should not include it in user-facing diagnostics
/// because it may contain host-specific absolute paths.
#[derive(Clone)]
pub struct LocalPackageDirectory {
    canonical_path: PathBuf,
    manifest_source: Arc<[u8]>,
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    directory: Arc<File>,
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    identity: LocalPackageDirectoryIdentity,
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalPackageDirectoryIdentity {
    device: u64,
    inode: u64,
}

/// A filesystem failure while resolving or reading a local package.
///
/// Errors retain operation and `ErrorKind`, but not the affected absolute
/// path, so formatting an error cannot disclose a host filesystem location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageError {
    RelativeDirectory,
    Io {
        operation: LocalPackageIoOperation,
        kind: io::ErrorKind,
    },
    NotDirectory,
    DirectoryChanged,
    PlatformNotSupported,
    MissingManifest,
    ManifestNotFile,
    ManifestTooLarge,
}

/// The filesystem operation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPackageIoOperation {
    CanonicalizeDirectory,
    InspectDirectory,
    ResolveDependency,
    OpenManifest,
    ReadManifest,
}

/// Local directories already read for the root or concrete dependency versions.
///
/// A separate index can be used for each resolver load snapshot. Inserting
/// the same version twice is idempotent when both entries resolve to the same
/// dependency directory, and rejects two different local directories for that
/// dependency version. The root directory is stored separately so a root and a
/// dependency with the same package version keep distinct source identities.
#[derive(Clone, Debug, Default)]
pub struct LocalPackageDirectoryIndex {
    root: Option<(MvsPackageVersion, LocalPackageDirectory)>,
    entries: BTreeMap<MvsPackageVersion, LocalPackageDirectory>,
}

/// A conflict while indexing local package directories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageDirectoryIndexError {
    InvalidManifest(MvsPackageVersion),
    MissingPackageHeader(MvsPackageVersion),
    PackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found_package: Box<str>,
        found_version: Box<Version>,
    },
    ConflictingDirectories(MvsPackageVersion),
    ConflictingVersionsForDirectory {
        existing: Box<MvsPackageVersion>,
        discovered: Box<MvsPackageVersion>,
    },
}

/// Failure to discover and index a local dependency path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackagePathIndexError {
    MissingBaseDirectory(MvsPackageVersion),
    AmbiguousRequirerRole(MvsPackageVersion),
    InvalidPath(PackagePathError),
    Directory(LocalPackageError),
    InvalidManifest(Box<PackageParseOutput>),
    MissingPackageHeader(Box<PackageParseOutput>),
    InvalidPackageIdentity(Box<str>),
    Index(LocalPackageDirectoryIndexError),
    InvalidRootManifest(Box<PackageParseOutput>),
    MissingRootPackageHeader(Box<PackageParseOutput>),
    InvalidRootPackageIdentity(Box<str>),
    MissingRootDirectory(MvsPackageVersion),
    RootManifestSnapshotMismatch(MvsPackageVersion),
    MissingRootTargets(Box<PackageParseOutput>),
    DependencyPathRequirementBaseIsRoot(MvsPackageVersion),
    DuplicateRootPathOverride {
        package: Box<str>,
        span: Range<usize>,
    },
    RootPathOverrideIdentityMismatch {
        expected: Box<str>,
        found: Box<str>,
        span: Range<usize>,
    },
    PathDiscovery {
        package: Box<str>,
        span: Range<usize>,
        error: Box<LocalPackagePathIndexError>,
    },
    PathRequirementDiscovery {
        package: Box<str>,
        path: Box<str>,
        requiring_package: MvsPackageVersion,
        span: Range<usize>,
        error: Box<LocalPackagePathIndexError>,
    },
}

impl LocalPackagePathIndexError {
    /// Returns the captured parse result when path discovery fails on manifest
    /// syntax or a missing package header.
    pub fn manifest_parse_output(&self) -> Option<&PackageParseOutput> {
        match self {
            Self::InvalidManifest(parsed)
            | Self::MissingPackageHeader(parsed)
            | Self::InvalidRootManifest(parsed)
            | Self::MissingRootPackageHeader(parsed)
            | Self::MissingRootTargets(parsed) => Some(parsed),
            Self::PathDiscovery { error, .. } | Self::PathRequirementDiscovery { error, .. } => {
                error.manifest_parse_output()
            }
            _ => None,
        }
    }
}

/// Failure to resolve a local filesystem path pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackagePathPinError {
    UnsupportedGitTreeBase,
    SourceBaseMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    MissingRequirerDirectory(MvsPackageVersion),
    InvalidPath(PackagePathError),
    Directory(LocalPackageError),
}

/// Failure to map a selected local path source to its opened directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageSourceResolveError {
    InconsistentSelection,
    PathPinCountMismatch {
        expected: usize,
        found: usize,
    },
    SourceBaseMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    MissingSelectedDirectory(MvsPackageVersion),
    SelectedDirectoryMismatch(MvsPackageVersion),
    SelectedManifestChanged(MvsPackageVersion),
    PathLockFloorUnsupported(MvsPackageVersion),
    MissingBaseDirectory(MvsPackageVersion),
    InvalidPath(PackagePathError),
    Directory(LocalPackageError),
}

/// Failure to validate a captured local manifest as an MVS dependency contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageContractLoadError {
    InvalidContract(ResolverContractBuildError),
    PackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
}

/// A source-aware MVS candidate could not be loaded by the local filesystem
/// provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageMvsLoadError {
    UnsupportedSource {
        package_version: Box<MvsPackageVersion>,
        package_source: Box<ResolverPackageSource>,
    },
    SourceResolve(LocalPackageSourceResolveError),
    PathRequirements(LocalPackagePathIndexError),
    ResolverContext(ResolverContractBuildError),
    Contract(LocalPackageContractLoadError),
}

type LocalPackageMvsProviderError = ResolverSourceProviderIdentityLoadError<
    LocalPackageMvsLoadError,
    LocalPackageDirectory,
    LocalPackagePathPinError,
    LocalPackageDirectory,
>;

/// A source-aware local path resolution could not load every MVS candidate.
///
/// The partial provider result, evolving resolver context, and per-run
/// directory index are retained so diagnostics and independent successful
/// branches remain inspectable.
#[derive(Debug)]
pub struct LocalPackageMvsProviderFailure {
    error: Box<LocalPackageMvsProviderError>,
    context: ResolverContractContext<'static>,
    directory_index: LocalPackageDirectoryIndex,
}

impl LocalPackageMvsProviderFailure {
    pub const fn error(
        &self,
    ) -> &ResolverSourceProviderError<
        LocalPackageMvsLoadError,
        LocalPackageDirectory,
        LocalPackagePathPinError,
    > {
        self.error.error()
    }

    pub const fn context(&self) -> &ResolverContractContext<'static> {
        &self.context
    }

    pub const fn directory_index(&self) -> &LocalPackageDirectoryIndex {
        &self.directory_index
    }

    pub const fn source_identities(
        &self,
    ) -> &crate::resolver_source_provider::ResolverSourceIdentitySnapshot<LocalPackageDirectory>
    {
        self.error.source_identities()
    }
}

/// Failure to prepare or run local path-only package resolution.
#[derive(Debug)]
pub enum LocalPackageMvsError {
    RootDiscovery(Box<LocalPackagePathIndexError>),
    RootContract(Box<ResolverContractBuildError>),
    Provider(Box<LocalPackageMvsProviderFailure>),
}

/// A complete source-aware MVS load using local path package snapshots.
#[derive(Clone, Debug)]
pub struct LocalPackageMvsOutput {
    provider: ResolverSourceProviderIdentifiedOutput<LocalPackageDirectory>,
    context: ResolverContractContext<'static>,
    directory_index: LocalPackageDirectoryIndex,
}

impl LocalPackageMvsOutput {
    pub const fn provider(&self) -> &ResolverSourceProviderIdentifiedOutput<LocalPackageDirectory> {
        &self.provider
    }

    /// Context extended with path packages discovered in loaded dependencies.
    pub const fn context(&self) -> &ResolverContractContext<'static> {
        &self.context
    }

    /// Per-resolution snapshot index used by the provider and source audit.
    pub const fn directory_index(&self) -> &LocalPackageDirectoryIndex {
        &self.directory_index
    }

    /// Plans post-selection source choices using the accumulated path map.
    pub fn plan_final_sources(
        &self,
        locked_sources: &std::collections::HashMap<MvsPackageVersion, ResolverPackageSource>,
    ) -> Result<
        crate::resolver_source_provider::ResolverFinalSourcePlan,
        crate::resolver_source_provider::ResolverFinalSourcePlanError,
    > {
        plan_resolver_final_sources(self.provider.output(), &self.context, locked_sources)
    }
}

impl LocalPackageDirectory {
    /// Resolves an existing absolute directory and requires a `sai.package`
    /// entry within it.
    ///
    /// Filesystem canonicalization resolves symlinks using the host's path
    /// semantics and produces an absolute path without `.` or `..` components.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LocalPackageError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(LocalPackageError::RelativeDirectory);
        }

        let canonical_path = fs::canonicalize(path).map_err(|error| LocalPackageError::Io {
            operation: LocalPackageIoOperation::CanonicalizeDirectory,
            kind: error.kind(),
        })?;

        #[cfg(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        ))]
        {
            let directory = open_directory_from_cwd(
                &canonical_path,
                LocalPackageIoOperation::CanonicalizeDirectory,
            )?;
            let identity = directory_identity(&directory)?;
            let manifest_source = read_manifest_from_directory(&directory)?;
            Ok(Self {
                canonical_path,
                manifest_source,
                directory: Arc::new(directory),
                identity,
            })
        }

        #[cfg(not(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )))]
        {
            let metadata =
                fs::metadata(&canonical_path).map_err(|error| LocalPackageError::Io {
                    operation: LocalPackageIoOperation::InspectDirectory,
                    kind: error.kind(),
                })?;
            if !metadata.is_dir() {
                return Err(LocalPackageError::NotDirectory);
            }
            let manifest_source = read_manifest(&canonical_path.join("sai.package"))?;

            Ok(Self {
                canonical_path,
                manifest_source,
            })
        }
    }

    /// Resolves a dependency path relative to this package directory.
    pub fn resolve_dependency(
        &self,
        path: &DependencyPackagePath,
    ) -> Result<Self, LocalPackageError> {
        #[cfg(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        ))]
        {
            let directory = open_directory_relative(
                &self.directory,
                path.as_str(),
                LocalPackageIoOperation::ResolveDependency,
            )?;
            let canonical_path = fs::canonicalize(self.canonical_path.join(path.as_str()))
                .map_err(|error| LocalPackageError::Io {
                    operation: LocalPackageIoOperation::CanonicalizeDirectory,
                    kind: error.kind(),
                })?;
            let path_directory = open_directory_from_cwd(
                &canonical_path,
                LocalPackageIoOperation::CanonicalizeDirectory,
            )?;
            let identity = directory_identity(&directory)?;
            if directory_identity(&path_directory)? != identity {
                return Err(LocalPackageError::DirectoryChanged);
            }
            let manifest_source = read_manifest_from_directory(&directory)?;

            Ok(Self {
                canonical_path,
                manifest_source,
                directory: Arc::new(directory),
                identity,
            })
        }

        #[cfg(not(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )))]
        {
            let _ = path;
            Err(LocalPackageError::PlatformNotSupported)
        }
    }

    /// Returns the canonical path spelling captured when this directory opened.
    ///
    /// The path may become stale if the directory is renamed. On supported
    /// Unix targets, the open directory handle remains authoritative for
    /// dependency resolution. Avoid displaying this value in diagnostics;
    /// local paths are host-specific and may reveal private filesystem layout.
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    /// Reads and parses this package's manifest using the requested package
    /// role. The caller remains responsible for reporting parse diagnostics
    /// and validating package semantics before resolver use.
    pub fn parse_manifest(&self, role: PackageRole) -> PackageParseOutput {
        parse_for_role(&self.manifest_source, role)
    }

    /// Parses the captured manifest as a dependency and adapts it to an MVS
    /// contract after syntax and semantic validation.
    ///
    /// The expected version is checked against the validated contract before
    /// it can be returned to the source provider. This reads the bytes
    /// captured when this directory was opened; it does not reopen
    /// `sai.package`.
    pub fn build_dependency_contract(
        &self,
        expected: &MvsPackageVersion,
        context: &ResolverContractContext<'_>,
        validation_context: PackageValidationContext<'_>,
    ) -> Result<ResolverContract, LocalPackageContractLoadError> {
        let parsed = self.parse_manifest(PackageRole::Dependency);
        let contract = adapt_resolver_contract(&parsed, context, validation_context)
            .map_err(LocalPackageContractLoadError::InvalidContract)?;
        let found = contract.mvs.package_version();
        if found != expected {
            return Err(LocalPackageContractLoadError::PackageVersionMismatch {
                expected: Box::new(expected.clone()),
                found: Box::new(found.clone()),
            });
        }
        Ok(contract)
    }
}

impl LocalPackageDirectoryIndex {
    /// Adds the root directory under the package version in its captured
    /// manifest and returns that concrete version.
    pub fn index_root_directory(
        &mut self,
        directory: &LocalPackageDirectory,
    ) -> Result<MvsPackageVersion, LocalPackagePathIndexError> {
        let parsed = directory.parse_manifest(PackageRole::Root);
        if !parsed.is_syntax_valid() {
            return Err(LocalPackagePathIndexError::InvalidRootManifest(Box::new(
                parsed,
            )));
        }
        let header = parsed.file().header.as_ref().ok_or_else(|| {
            LocalPackagePathIndexError::MissingRootPackageHeader(Box::new(parsed.clone()))
        })?;
        let package = header.value.identity.to_string();
        let package_version = MvsPackageVersion::new(package.clone(), header.value.version.clone())
            .map_err(|_| LocalPackagePathIndexError::InvalidRootPackageIdentity(package.into()))?;
        self.insert(
            package_version.clone(),
            directory.clone(),
            PackageRole::Root,
        )
        .map_err(LocalPackagePathIndexError::Index)?;
        Ok(package_version)
    }

    /// Adds a directory after confirming its parsed header matches the version.
    ///
    /// This verifies syntax and header identity only; the caller remains
    /// responsible for reporting all manifest diagnostics and validating
    /// package semantics before resolver use.
    pub fn insert(
        &mut self,
        package_version: MvsPackageVersion,
        directory: LocalPackageDirectory,
        role: PackageRole,
    ) -> Result<(), LocalPackageDirectoryIndexError> {
        let parsed = directory.parse_manifest(role);
        self.insert_parsed(package_version, directory, &parsed, role)
    }

    /// Resolves a dependency path from an indexed local package, discovers the
    /// dependency version from the captured manifest, and indexes that same
    /// opened directory snapshot.
    ///
    /// Call this when building local path-package maps so the version used to
    /// create resolver context and the manifest later consumed by the loader
    /// come from one per-load directory index. It is kept for compatibility
    /// when the version identifies exactly one indexed base. Use
    /// [`Self::index_dependency_path_from`] when the requirer role is known;
    /// this method returns an error if both roles have that version.
    pub fn index_dependency_path(
        &mut self,
        relative_to: &MvsPackageVersion,
        path: &str,
    ) -> Result<MvsPackageVersion, LocalPackagePathIndexError> {
        let is_root = self
            .root
            .as_ref()
            .is_some_and(|(root_package, _)| root_package == relative_to);
        let is_dependency = self.entries.contains_key(relative_to);
        let requirer = match (is_root, is_dependency) {
            (true, true) => {
                return Err(LocalPackagePathIndexError::AmbiguousRequirerRole(
                    relative_to.clone(),
                ));
            }
            (true, false) => MvsRequirer::Root(relative_to.clone()),
            (false, _) => MvsRequirer::Package(relative_to.clone()),
        };
        self.index_dependency_path_from(&requirer, path)
    }

    /// Resolves a path from the exact root or dependency directory and indexes
    /// the resulting package snapshot.
    pub fn index_dependency_path_from(
        &mut self,
        requirer: &MvsRequirer,
        path: &str,
    ) -> Result<MvsPackageVersion, LocalPackagePathIndexError> {
        let path =
            DependencyPackagePath::parse(path).map_err(LocalPackagePathIndexError::InvalidPath)?;
        let requiring_package = requirer.package_version();
        let base = self.directory_for_requirer(requirer).ok_or_else(|| {
            LocalPackagePathIndexError::MissingBaseDirectory(requiring_package.clone())
        })?;
        let directory = base
            .resolve_dependency(&path)
            .map_err(LocalPackagePathIndexError::Directory)?;
        let parsed = directory.parse_manifest(PackageRole::Dependency);
        if !parsed.is_syntax_valid() {
            return Err(LocalPackagePathIndexError::InvalidManifest(Box::new(
                parsed,
            )));
        }
        let header = match parsed.file().header.as_ref() {
            Some(header) => header,
            None => {
                return Err(LocalPackagePathIndexError::MissingPackageHeader(Box::new(
                    parsed,
                )));
            }
        };
        let package = header.value.identity.to_string();
        let package_version = MvsPackageVersion::new(package.clone(), header.value.version.clone())
            .map_err(|_| LocalPackagePathIndexError::InvalidPackageIdentity(package.into()))?;
        self.insert_parsed(
            package_version.clone(),
            directory,
            &parsed,
            PackageRole::Dependency,
        )
        .map_err(LocalPackagePathIndexError::Index)?;
        Ok(package_version)
    }

    fn directory_for_requirer(&self, requirer: &MvsRequirer) -> Option<&LocalPackageDirectory> {
        match requirer {
            MvsRequirer::Root(package_version) => self
                .root
                .as_ref()
                .filter(|(root_package, _)| root_package == package_version)
                .map(|(_, directory)| directory),
            MvsRequirer::Package(package_version) => self.entries.get(package_version),
        }
    }

    /// Discovers and indexes every root `use path` override from the captured
    /// root manifest, returning the version map required by resolver context.
    ///
    /// The root directory must be the indexed directory and its parsed
    /// manifest must match the indexed parse snapshot. Each override is
    /// resolved relative to that directory, and its declared identity must
    /// match the overridden package identity.
    pub fn index_root_path_overrides(
        &mut self,
        root_directory: &LocalPackageDirectory,
    ) -> Result<BTreeMap<Box<str>, MvsPackageVersion>, LocalPackagePathIndexError> {
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        if !root_manifest.is_syntax_valid() {
            return Err(LocalPackagePathIndexError::InvalidRootManifest(Box::new(
                root_manifest,
            )));
        }
        let header = match root_manifest.file().header.as_ref() {
            Some(header) => header,
            None => {
                return Err(LocalPackagePathIndexError::MissingRootPackageHeader(
                    Box::new(root_manifest),
                ));
            }
        };
        let root_name = header.value.identity.to_string();
        let root_package = MvsPackageVersion::new(root_name.clone(), header.value.version.clone())
            .map_err(|_| {
                LocalPackagePathIndexError::InvalidRootPackageIdentity(root_name.into())
            })?;
        let indexed_root = self
            .directory_for_requirer(&MvsRequirer::Root(root_package.clone()))
            .ok_or_else(|| {
                LocalPackagePathIndexError::MissingRootDirectory(root_package.clone())
            })?;
        if indexed_root != root_directory
            || indexed_root.parse_manifest(PackageRole::Root) != root_manifest
        {
            return Err(LocalPackagePathIndexError::RootManifestSnapshotMismatch(
                root_package,
            ));
        }

        let mut staged = self.clone();
        let mut path_overrides = BTreeMap::new();
        for declaration in &root_manifest.file().declarations {
            let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                continue;
            };
            for item in overrides {
                let OverrideSource::Path(path) = &item.value.source else {
                    continue;
                };
                if path_overrides.contains_key(&item.value.package) {
                    return Err(LocalPackagePathIndexError::DuplicateRootPathOverride {
                        package: item.value.package.clone(),
                        span: item.span.clone(),
                    });
                }
                let package_version = staged
                    .index_dependency_path_from(&MvsRequirer::Root(root_package.clone()), path)
                    .map_err(|error| LocalPackagePathIndexError::PathDiscovery {
                        package: item.value.package.clone(),
                        span: item.span.clone(),
                        error: Box::new(error),
                    })?;
                if package_version.package() != item.value.package.as_ref() {
                    return Err(
                        LocalPackagePathIndexError::RootPathOverrideIdentityMismatch {
                            expected: item.value.package.clone(),
                            found: package_version.package().into(),
                            span: item.span.clone(),
                        },
                    );
                }
                path_overrides.insert(item.value.package.clone(), package_version);
            }
        }
        *self = staged;
        Ok(path_overrides)
    }

    /// Discovers active root `from path` requirements not replaced by a root
    /// source override, returning the map consumed by resolver context.
    ///
    /// Path overrides replace matching compatibility lines, and root Git
    /// overrides replace path sources for every line. Requirements restricted
    /// to targets absent from the root are not resolved.
    pub fn index_root_path_requirements(
        &mut self,
        root_directory: &LocalPackageDirectory,
        root_path_overrides: &BTreeMap<Box<str>, MvsPackageVersion>,
    ) -> Result<BTreeMap<ResolverPathKey, MvsPackageVersion>, LocalPackagePathIndexError> {
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        if !root_manifest.is_syntax_valid() {
            return Err(LocalPackagePathIndexError::InvalidRootManifest(Box::new(
                root_manifest,
            )));
        }
        let header = match root_manifest.file().header.as_ref() {
            Some(header) => header,
            None => {
                return Err(LocalPackagePathIndexError::MissingRootPackageHeader(
                    Box::new(root_manifest),
                ));
            }
        };
        let root_name = header.value.identity.to_string();
        let root_package = MvsPackageVersion::new(root_name.clone(), header.value.version.clone())
            .map_err(|_| {
                LocalPackagePathIndexError::InvalidRootPackageIdentity(root_name.into())
            })?;
        let indexed_root = self
            .directory_for_requirer(&MvsRequirer::Root(root_package.clone()))
            .ok_or_else(|| {
                LocalPackagePathIndexError::MissingRootDirectory(root_package.clone())
            })?;
        if indexed_root != root_directory
            || indexed_root.parse_manifest(PackageRole::Root) != root_manifest
        {
            return Err(LocalPackagePathIndexError::RootManifestSnapshotMismatch(
                root_package,
            ));
        }
        let root_targets: BTreeSet<Box<str>> = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| match &declaration.value {
                PackageDeclaration::Targets(targets) => {
                    Some(targets.iter().map(|target| target.value.clone()).collect())
                }
                _ => None,
            })
            .ok_or_else(|| {
                LocalPackagePathIndexError::MissingRootTargets(Box::new(root_manifest.clone()))
            })?;
        let root_git_override_ids = root_manifest
            .file()
            .declarations
            .iter()
            .filter_map(|declaration| {
                let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                    return None;
                };
                Some(
                    overrides
                        .iter()
                        .filter(|item| matches!(&item.value.source, OverrideSource::Git { .. }))
                        .map(|item| item.value.package.clone()),
                )
            })
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();

        let mut staged = self.clone();
        let mut path_packages = BTreeMap::new();
        for declaration in &root_manifest.file().declarations {
            let PackageDeclaration::Requires { entries, .. } = &declaration.value else {
                continue;
            };
            for item in entries {
                let requirement = &item.value;
                let included = requirement.only.as_ref().is_none_or(|only| {
                    only.value
                        .iter()
                        .any(|target| root_targets.contains(&target.value))
                });
                if !included
                    || root_git_override_ids.contains(&requirement.package)
                    || root_path_overrides
                        .get(requirement.package.as_ref())
                        .is_some_and(|package_version| {
                            package_version.version().compatibility_line()
                                == requirement.version.compatibility_line()
                        })
                {
                    continue;
                }
                let Some(Spanned {
                    value: RequirementSource::Path(path),
                    ..
                }) = &requirement.source
                else {
                    continue;
                };
                let key = ResolverPathKey::root(root_package.clone(), path.clone());
                if path_packages.contains_key(&key) {
                    continue;
                }
                let package_version = staged
                    .index_dependency_path_from(&MvsRequirer::Root(root_package.clone()), path)
                    .map_err(
                        |error| LocalPackagePathIndexError::PathRequirementDiscovery {
                            package: requirement.package.clone(),
                            path: path.clone(),
                            requiring_package: root_package.clone(),
                            span: item.span.clone(),
                            error: Box::new(error),
                        },
                    )?;
                path_packages.insert(key, package_version);
            }
        }
        *self = staged;
        Ok(path_packages)
    }

    /// Discovers the target-included local path requirements of one indexed
    /// dependency package.
    ///
    /// Dependency test requirements are omitted. Root target filtering and
    /// root source overrides are applied from `root_context`, matching the
    /// rules used when the dependency contract is built.
    pub fn index_dependency_path_requirements(
        &mut self,
        requiring_package: &MvsPackageVersion,
        root_context: &ResolverContractContext<'_>,
    ) -> Result<BTreeMap<ResolverPathKey, MvsPackageVersion>, LocalPackagePathIndexError> {
        let base = self.entries.get(requiring_package).ok_or_else(|| {
            LocalPackagePathIndexError::MissingBaseDirectory(requiring_package.clone())
        })?;
        let parsed = base.parse_manifest(PackageRole::Dependency);
        if !parsed.is_syntax_valid() {
            return Err(LocalPackagePathIndexError::InvalidManifest(Box::new(
                parsed,
            )));
        }
        let header = parsed.file().header.as_ref().ok_or_else(|| {
            LocalPackagePathIndexError::MissingPackageHeader(Box::new(parsed.clone()))
        })?;
        let package = header.value.identity.to_string();
        let found = MvsPackageVersion::new(package.clone(), header.value.version.clone())
            .map_err(|_| LocalPackagePathIndexError::InvalidPackageIdentity(package.into()))?;
        if &found != requiring_package {
            return Err(LocalPackagePathIndexError::Index(
                LocalPackageDirectoryIndexError::PackageVersionMismatch {
                    expected: Box::new(requiring_package.clone()),
                    found_package: found.package().into(),
                    found_version: Box::new(found.version().clone()),
                },
            ));
        }

        let mut staged = self.clone();
        let mut path_packages = BTreeMap::new();
        for declaration in &parsed.file().declarations {
            let PackageDeclaration::Requires { test, entries } = &declaration.value else {
                continue;
            };
            if *test {
                continue;
            }
            for item in entries {
                let requirement = &item.value;
                let included = requirement.only.as_ref().is_none_or(|only| {
                    only.value
                        .iter()
                        .any(|target| root_context.root_targets().contains(&target.value))
                });
                let path_override_applies = root_context
                    .root_path_override(&requirement.package)
                    .is_some_and(|package_version| {
                        package_version.version().compatibility_line()
                            == requirement.version.compatibility_line()
                    });
                if !included
                    || root_context.has_root_git_override(&requirement.package)
                    || path_override_applies
                {
                    continue;
                }
                let Some(Spanned {
                    value: RequirementSource::Path(path),
                    ..
                }) = &requirement.source
                else {
                    continue;
                };
                let key = ResolverPathKey::package(requiring_package.clone(), path.clone());
                if path_packages.contains_key(&key) {
                    continue;
                }
                let package_version = staged
                    .index_dependency_path_from(
                        &MvsRequirer::Package(requiring_package.clone()),
                        path,
                    )
                    .map_err(
                        |error| LocalPackagePathIndexError::PathRequirementDiscovery {
                            package: requirement.package.clone(),
                            path: path.clone(),
                            requiring_package: requiring_package.clone(),
                            span: item.span.clone(),
                            error: Box::new(error),
                        },
                    )?;
                path_packages.insert(key, package_version);
            }
        }
        *self = staged;
        Ok(path_packages)
    }

    fn insert_parsed(
        &mut self,
        package_version: MvsPackageVersion,
        directory: LocalPackageDirectory,
        parsed: &PackageParseOutput,
        role: PackageRole,
    ) -> Result<(), LocalPackageDirectoryIndexError> {
        if !parsed.is_syntax_valid() {
            return Err(LocalPackageDirectoryIndexError::InvalidManifest(
                package_version,
            ));
        }
        let header = parsed.file().header.as_ref().ok_or_else(|| {
            LocalPackageDirectoryIndexError::MissingPackageHeader(package_version.clone())
        })?;
        if header.value.identity.to_string() != package_version.package()
            || header.value.version != *package_version.version()
        {
            return Err(LocalPackageDirectoryIndexError::PackageVersionMismatch {
                expected: Box::new(package_version),
                found_package: header.value.identity.to_string().into(),
                found_version: Box::new(header.value.version.clone()),
            });
        }

        match role {
            PackageRole::Root => {
                if let Some((existing_version, existing_directory)) = &self.root {
                    if existing_version == &package_version && existing_directory == &directory {
                        return Ok(());
                    }
                    return Err(LocalPackageDirectoryIndexError::ConflictingDirectories(
                        existing_version.clone(),
                    ));
                }
                if let Some((existing, _)) =
                    self.entries.iter().find(|(existing, existing_directory)| {
                        *existing != &package_version && *existing_directory == &directory
                    })
                {
                    return Err(
                        LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                            existing: Box::new(existing.clone()),
                            discovered: Box::new(package_version),
                        },
                    );
                }
                self.root = Some((package_version, directory));
                Ok(())
            }
            PackageRole::Dependency => {
                match self.entries.get(&package_version) {
                    Some(existing) if existing == &directory => return Ok(()),
                    Some(_) => {
                        return Err(LocalPackageDirectoryIndexError::ConflictingDirectories(
                            package_version,
                        ));
                    }
                    None => {}
                }
                if let Some((existing, _)) = self
                    .entries
                    .iter()
                    .find(|(_, existing_directory)| *existing_directory == &directory)
                {
                    return Err(
                        LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                            existing: Box::new(existing.clone()),
                            discovered: Box::new(package_version),
                        },
                    );
                }
                if let Some((root_version, root_directory)) = &self.root
                    && root_version != &package_version
                    && root_directory == &directory
                {
                    return Err(
                        LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                            existing: Box::new(root_version.clone()),
                            discovered: Box::new(package_version),
                        },
                    );
                }
                self.entries.insert(package_version, directory);
                Ok(())
            }
        }
    }

    /// Resolves and canonicalizes a path pin relative to its requiring package.
    ///
    /// The pin's `relative_to` package must match its recorded requirer. A
    /// Git-backed base is left to the Git-tree source provider.
    pub fn canonicalize_path_pin(
        &self,
        pin: &ResolverPathPinSource,
    ) -> Result<LocalPackageDirectory, LocalPackagePathPinError> {
        let ResolverPackageSource::Path { relative_to, path } = &pin.source else {
            return Err(LocalPackagePathPinError::UnsupportedGitTreeBase);
        };
        let expected = pin.requirer.package_version();
        if relative_to != expected {
            return Err(LocalPackagePathPinError::SourceBaseMismatch {
                expected: Box::new(expected.clone()),
                found: Box::new(relative_to.clone()),
            });
        }
        let base = self.directory_for_requirer(&pin.requirer).ok_or_else(|| {
            LocalPackagePathPinError::MissingRequirerDirectory(relative_to.clone())
        })?;
        let path =
            DependencyPackagePath::parse(path).map_err(LocalPackagePathPinError::InvalidPath)?;
        base.resolve_dependency(&path)
            .map_err(LocalPackagePathPinError::Directory)
    }

    /// Validates every path pin and returns the directory opened for the winner.
    ///
    /// Keeping the returned directory lets the caller parse the exact manifest
    /// snapshot whose source identity was checked, instead of resolving the
    /// winning path a second time after validation.
    pub fn canonicalize_selection_path_pins(
        &self,
        selection: &ResolverSourceSelection,
    ) -> Result<
        Option<LocalPackageDirectory>,
        ResolverPathPinValidationError<LocalPackageDirectory, LocalPackagePathPinError>,
    > {
        let mut winner = None;
        validate_resolver_path_pins(selection, |pin| {
            let directory = self.canonicalize_path_pin(pin)?;
            if winner.is_none() {
                winner = Some(directory.clone());
            }
            Ok(directory)
        })?;
        Ok(winner)
    }

    /// Returns the captured local directory for a selected path source.
    ///
    /// `PathPin` selections compare the exact keys returned to the loader by
    /// `select_with_source_provider_and_canonicalized_pins` with the directory
    /// and manifest snapshot indexed for the selected package version, then
    /// return that indexed snapshot. Root path overrides are resolved from
    /// the indexed root directory and matched against the selected snapshot.
    /// Path lock floors are rejected because the lock format excludes path
    /// entries from floors. Git selections return `None` for a separate Git
    /// provider.
    pub fn resolve_selected_path_source(
        &self,
        selection: &ResolverSourceSelection,
        root_package: &MvsPackageVersion,
        canonicalized_path_pins: &[LocalPackageDirectory],
    ) -> Result<Option<LocalPackageDirectory>, LocalPackageSourceResolveError> {
        if selection.reason() != ResolverSourceReason::PathPin
            && (!selection.path_pin_sources().is_empty() || !canonicalized_path_pins.is_empty())
        {
            return Err(LocalPackageSourceResolveError::InconsistentSelection);
        }
        match (selection.reason(), selection.source()) {
            (ResolverSourceReason::PathPin, ResolverPackageSource::Path { .. }) => {
                let pins = selection.path_pin_sources();
                if pins.is_empty()
                    || selection.source() != &pins[0].source
                    || !matches!(
                        selection.provenance(),
                        ResolverSourceProvenance::PathPin { pin } if pin == &pins[0]
                    )
                    || pins
                        .iter()
                        .any(|pin| !matches!(pin.source, ResolverPackageSource::Path { .. }))
                {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                }
                if pins.len() != canonicalized_path_pins.len() {
                    return Err(LocalPackageSourceResolveError::PathPinCountMismatch {
                        expected: pins.len(),
                        found: canonicalized_path_pins.len(),
                    });
                }
                let Some(winner) = canonicalized_path_pins.first() else {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                };
                if canonicalized_path_pins
                    .iter()
                    .any(|directory| directory != winner)
                {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                }
                if let Some(indexed) = self.entries.get(selection.package_version()) {
                    if winner != indexed {
                        return Err(LocalPackageSourceResolveError::SelectedDirectoryMismatch(
                            selection.package_version().clone(),
                        ));
                    }
                    if winner.manifest_source.as_ref() != indexed.manifest_source.as_ref() {
                        return Err(LocalPackageSourceResolveError::SelectedManifestChanged(
                            selection.package_version().clone(),
                        ));
                    }
                    return Ok(Some(indexed.clone()));
                }
                Ok(Some(winner.clone()))
            }
            (
                ResolverSourceReason::RootPathOverride,
                ResolverPackageSource::Path { relative_to, path },
            ) => {
                let ResolverSourceProvenance::RootPathOverride {
                    root_package: provenance_root,
                    ..
                } = selection.provenance()
                else {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                };
                if provenance_root != root_package {
                    return Err(LocalPackageSourceResolveError::SourceBaseMismatch {
                        expected: Box::new(root_package.clone()),
                        found: Box::new(provenance_root.clone()),
                    });
                }
                let selected = self
                    .entries
                    .get(selection.package_version())
                    .ok_or_else(|| {
                        LocalPackageSourceResolveError::MissingSelectedDirectory(
                            selection.package_version().clone(),
                        )
                    })?;
                let current = self.resolve_path_from_package(relative_to, path, root_package)?;
                if &current != selected {
                    return Err(LocalPackageSourceResolveError::SelectedDirectoryMismatch(
                        selection.package_version().clone(),
                    ));
                }
                Ok(Some(selected.clone()))
            }
            (ResolverSourceReason::LockFloor, ResolverPackageSource::Path { .. }) => {
                let ResolverSourceProvenance::LockFloor { package_version } =
                    selection.provenance()
                else {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                };
                if package_version != selection.package_version() {
                    return Err(LocalPackageSourceResolveError::InconsistentSelection);
                }
                Err(LocalPackageSourceResolveError::PathLockFloorUnsupported(
                    package_version.clone(),
                ))
            }
            (ResolverSourceReason::PathPin, _) => {
                Err(LocalPackageSourceResolveError::InconsistentSelection)
            }
            (_, ResolverPackageSource::Git { .. }) => Ok(None),
            (_, ResolverPackageSource::Path { .. }) => {
                Err(LocalPackageSourceResolveError::InconsistentSelection)
            }
        }
    }

    fn resolve_path_from_package(
        &self,
        relative_to: &MvsPackageVersion,
        path: &str,
        expected_base: &MvsPackageVersion,
    ) -> Result<LocalPackageDirectory, LocalPackageSourceResolveError> {
        if relative_to != expected_base {
            return Err(LocalPackageSourceResolveError::SourceBaseMismatch {
                expected: Box::new(expected_base.clone()),
                found: Box::new(relative_to.clone()),
            });
        }
        let base = self
            .directory_for_requirer(&MvsRequirer::Root(expected_base.clone()))
            .ok_or_else(|| {
                LocalPackageSourceResolveError::MissingBaseDirectory(relative_to.clone())
            })?;
        let path = DependencyPackagePath::parse(path)
            .map_err(LocalPackageSourceResolveError::InvalidPath)?;
        base.resolve_dependency(&path)
            .map_err(LocalPackageSourceResolveError::Directory)
    }
}

/// Resolves a root package using local path sources and source-aware MVS.
///
/// The root and every reachable path dependency are read from captured
/// `LocalPackageDirectory` snapshots. Git sources fail with an explicit
/// provider error so callers can dispatch them to a Git-backed resolver.
pub fn resolve_local_path_packages(
    root_directory: LocalPackageDirectory,
    lock_floors: &HashMap<MvsNode, Version>,
    locked_sources: &HashMap<MvsPackageVersion, ResolverPackageSource>,
    validation_context: PackageValidationContext<'_>,
) -> Result<LocalPackageMvsOutput, LocalPackageMvsError> {
    let mut directory_index = LocalPackageDirectoryIndex::default();
    directory_index
        .index_root_directory(&root_directory)
        .map_err(|error| LocalPackageMvsError::RootDiscovery(Box::new(error)))?;

    let root_manifest = root_directory.parse_manifest(PackageRole::Root);
    let root_path_overrides = directory_index
        .index_root_path_overrides(&root_directory)
        .map_err(|error| LocalPackageMvsError::RootDiscovery(Box::new(error)))?;
    let root_path_packages = directory_index
        .index_root_path_requirements(&root_directory, &root_path_overrides)
        .map_err(|error| LocalPackageMvsError::RootDiscovery(Box::new(error)))?;
    let root_context = ResolverContractContext::from_root_manifest(
        &root_manifest,
        &root_path_packages,
        &root_path_overrides,
        validation_context,
    )
    .map_err(|error| LocalPackageMvsError::RootContract(Box::new(error)))?
    .into_owned();
    let root_contract = adapt_resolver_contract(&root_manifest, &root_context, validation_context)
        .map_err(|error| LocalPackageMvsError::RootContract(Box::new(error)))?;

    let directory_index = RefCell::new(directory_index);
    let context = RefCell::new(root_context.clone());
    let result = select_with_source_provider_and_canonicalized_pins_and_identities(
        ResolverLoadedRootContract::new(root_contract, root_directory),
        &root_context,
        lock_floors,
        locked_sources,
        |pin| directory_index.borrow().canonicalize_path_pin(pin),
        |request, selection, canonicalized_path_pins| {
            let directory = directory_index
                .borrow()
                .resolve_selected_path_source(
                    selection,
                    root_context.root_package(),
                    canonicalized_path_pins,
                )
                .map_err(LocalPackageMvsLoadError::SourceResolve)?
                .ok_or_else(|| LocalPackageMvsLoadError::UnsupportedSource {
                    package_version: Box::new(request.package_version.clone()),
                    package_source: Box::new(selection.source().clone()),
                })?;

            let current_context = context.borrow().clone();
            let mut staged_index = directory_index.borrow().clone();
            let additions = staged_index
                .index_dependency_path_requirements(&request.package_version, &current_context)
                .map_err(LocalPackageMvsLoadError::PathRequirements)?;
            let next_context = current_context
                .with_path_packages(&additions)
                .map_err(LocalPackageMvsLoadError::ResolverContext)?;
            let contract = directory
                .build_dependency_contract(
                    &request.package_version,
                    &next_context,
                    validation_context,
                )
                .map_err(LocalPackageMvsLoadError::Contract)?;
            *directory_index.borrow_mut() = staged_index;
            *context.borrow_mut() = next_context;
            Ok(ResolverLoadedContract::new(contract, directory))
        },
    );

    let context = context.into_inner();
    let directory_index = directory_index.into_inner();
    match result {
        Ok(provider) => Ok(LocalPackageMvsOutput {
            provider,
            context,
            directory_index,
        }),
        Err(error) => Err(LocalPackageMvsError::Provider(Box::new(
            LocalPackageMvsProviderFailure {
                error,
                context,
                directory_index,
            },
        ))),
    }
}

impl PartialEq for LocalPackageDirectory {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        ))]
        {
            self.identity == other.identity
        }
        #[cfg(not(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )))]
        {
            self.canonical_path == other.canonical_path
        }
    }
}

impl Eq for LocalPackageDirectory {}

impl fmt::Display for LocalPackageDirectoryIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(package_version) => write!(
                formatter,
                "manifest for {}@{} is not syntactically valid",
                package_version.package(),
                package_version.version()
            ),
            Self::MissingPackageHeader(package_version) => write!(
                formatter,
                "manifest for {}@{} has no package header",
                package_version.package(),
                package_version.version()
            ),
            Self::PackageVersionMismatch {
                expected,
                found_package,
                found_version,
            } => write!(
                formatter,
                "expected {}@{}, found {}@{}",
                expected.package(),
                expected.version(),
                found_package,
                found_version
            ),
            Self::ConflictingDirectories(package_version) => write!(
                formatter,
                "multiple local directories were registered for {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::ConflictingVersionsForDirectory {
                existing,
                discovered,
            } => write!(
                formatter,
                "one local directory was registered as both {}@{} and {}@{}",
                existing.package(),
                existing.version(),
                discovered.package(),
                discovered.version()
            ),
        }
    }
}

impl fmt::Display for LocalPackagePathIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBaseDirectory(package_version) => write!(
                formatter,
                "no local package directory is registered for path base {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::AmbiguousRequirerRole(package_version) => write!(
                formatter,
                "local package {}@{} is indexed as both root and dependency; specify its requirer role",
                package_version.package(),
                package_version.version()
            ),
            Self::InvalidPath(error) => fmt::Display::fmt(error, formatter),
            Self::Directory(error) => fmt::Display::fmt(error, formatter),
            Self::InvalidManifest(_) => {
                formatter.write_str("local dependency manifest is not syntactically valid")
            }
            Self::MissingPackageHeader(_) => {
                formatter.write_str("local dependency manifest has no package header")
            }
            Self::InvalidPackageIdentity(package) => {
                write!(
                    formatter,
                    "local dependency has invalid package identity `{package}`"
                )
            }
            Self::Index(error) => fmt::Display::fmt(error, formatter),
            Self::InvalidRootManifest(_) => {
                formatter.write_str("root package manifest is not syntactically valid")
            }
            Self::MissingRootPackageHeader(_) => {
                formatter.write_str("root package manifest has no package header")
            }
            Self::MissingRootTargets(_) => {
                formatter.write_str("root package manifest has no targets declaration")
            }
            Self::DependencyPathRequirementBaseIsRoot(package_version) => write!(
                formatter,
                "cannot discover dependency path requirements for root package {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::InvalidRootPackageIdentity(package) => {
                write!(formatter, "root package has invalid identity `{package}`")
            }
            Self::MissingRootDirectory(package_version) => write!(
                formatter,
                "no indexed local directory exists for root package {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::RootManifestSnapshotMismatch(package_version) => write!(
                formatter,
                "root manifest snapshot for {}@{} does not match its indexed directory",
                package_version.package(),
                package_version.version()
            ),
            Self::DuplicateRootPathOverride { package, span } => write!(
                formatter,
                "root package has duplicate path overrides for `{package}` at bytes {}..{}",
                span.start, span.end
            ),
            Self::RootPathOverrideIdentityMismatch {
                expected,
                found,
                span,
            } => write!(
                formatter,
                "root path override for `{expected}` at bytes {}..{} resolved to package `{found}`",
                span.start, span.end
            ),
            Self::PathDiscovery {
                package,
                span,
                error,
            } => write!(
                formatter,
                "root path override for `{package}` at bytes {}..{}: {error}",
                span.start, span.end
            ),
            Self::PathRequirementDiscovery {
                package,
                path,
                requiring_package,
                span,
                error,
            } => write!(
                formatter,
                "path requirement for `{package}` at {path:?} from {}@{} at bytes {}..{}: {error}",
                requiring_package.package(),
                requiring_package.version(),
                span.start,
                span.end,
            ),
        }
    }
}

impl fmt::Display for LocalPackagePathPinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedGitTreeBase => {
                formatter.write_str("a Git-tree path pin needs the Git source provider")
            }
            Self::SourceBaseMismatch { expected, found } => write!(
                formatter,
                "path pin source base {}@{} does not match its requiring package {}@{}",
                found.package(),
                found.version(),
                expected.package(),
                expected.version()
            ),
            Self::MissingRequirerDirectory(package_version) => write!(
                formatter,
                "no local directory is registered for requiring package {}@{}",
                package_version.package(),
                package_version.version()
            ),
            Self::InvalidPath(error) => fmt::Display::fmt(error, formatter),
            Self::Directory(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl fmt::Display for LocalPackageContractLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContract(error) => {
                write!(formatter, "local dependency contract is invalid: {error}")
            }
            Self::PackageVersionMismatch { expected, found } => write!(
                formatter,
                "expected local package {}@{}, found {}@{}",
                expected.package(),
                expected.version(),
                found.package(),
                found.version()
            ),
        }
    }
}

impl fmt::Display for LocalPackageSourceResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InconsistentSelection => {
                formatter.write_str("selected local package source has inconsistent provenance")
            }
            Self::PathPinCountMismatch { expected, found } => write!(
                formatter,
                "path-pin selection has {expected} source pins but {found} captured directories"
            ),
            Self::SourceBaseMismatch { expected, found } => write!(
                formatter,
                "expected path source relative to {}@{}, found {}@{}",
                expected.package(),
                expected.version(),
                found.package(),
                found.version()
            ),
            Self::MissingSelectedDirectory(package_version) => write!(
                formatter,
                "selected local package directory for {}@{} is not indexed",
                package_version.package(),
                package_version.version()
            ),
            Self::SelectedDirectoryMismatch(package_version) => write!(
                formatter,
                "selected path source for {}@{} no longer resolves to its indexed directory",
                package_version.package(),
                package_version.version()
            ),
            Self::SelectedManifestChanged(package_version) => write!(
                formatter,
                "manifest for local package {}@{} changed after path discovery",
                package_version.package(),
                package_version.version()
            ),
            Self::PathLockFloorUnsupported(package_version) => write!(
                formatter,
                "path source for {}@{} cannot be used as a lock floor",
                package_version.package(),
                package_version.version()
            ),
            Self::MissingBaseDirectory(package_version) => write!(
                formatter,
                "root package directory for {}@{} is not indexed",
                package_version.package(),
                package_version.version()
            ),
            Self::InvalidPath(error) => fmt::Display::fmt(error, formatter),
            Self::Directory(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl fmt::Display for LocalPackageMvsLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSource {
                package_version,
                package_source,
            } => write!(
                formatter,
                "local path resolver cannot load {}@{} from {package_source:?}",
                package_version.package(),
                package_version.version()
            ),
            Self::SourceResolve(error) => fmt::Display::fmt(error, formatter),
            Self::PathRequirements(error) => fmt::Display::fmt(error, formatter),
            Self::ResolverContext(error) => {
                write!(formatter, "dependency resolver context is invalid: {error}")
            }
            Self::Contract(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl fmt::Display for LocalPackageMvsProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source-aware package loading failed: {:?}",
            self.error.error()
        )
    }
}

impl fmt::Display for LocalPackageMvsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootDiscovery(error) => fmt::Display::fmt(error, formatter),
            Self::RootContract(error) => {
                write!(formatter, "root package contract is invalid: {error}")
            }
            Self::Provider(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl fmt::Display for LocalPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeDirectory => {
                formatter.write_str("local package directory must be absolute")
            }
            Self::Io { operation, kind } => {
                write!(
                    formatter,
                    "local package filesystem error during {operation}: {kind}"
                )
            }
            Self::NotDirectory => formatter.write_str("local package source is not a directory"),
            Self::DirectoryChanged => {
                formatter.write_str("local package path changed while resolving its dependency")
            }
            Self::PlatformNotSupported => formatter
                .write_str("this host cannot safely resolve a local package path dependency"),
            Self::MissingManifest => {
                formatter.write_str("local package directory has no `sai.package` file")
            }
            Self::ManifestNotFile => {
                formatter.write_str("local package `sai.package` is not a file")
            }
            Self::ManifestTooLarge => write!(
                formatter,
                "local package `sai.package` exceeds the {MAX_PACKAGE_MANIFEST_BYTES} byte limit"
            ),
        }
    }
}

impl fmt::Display for LocalPackageIoOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::CanonicalizeDirectory => "canonicalizing its directory",
            Self::InspectDirectory => "inspecting its directory",
            Self::ResolveDependency => "resolving a dependency directory",
            Self::OpenManifest => "opening `sai.package`",
            Self::ReadManifest => "reading `sai.package`",
        };
        formatter.write_str(operation)
    }
}

impl fmt::Debug for LocalPackageDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalPackageDirectory")
            .finish_non_exhaustive()
    }
}

impl std::error::Error for LocalPackageError {}
impl std::error::Error for LocalPackageDirectoryIndexError {}
impl std::error::Error for LocalPackagePathIndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath(error) => Some(error),
            Self::Directory(error) => Some(error),
            Self::Index(error) => Some(error),
            Self::PathDiscovery { error, .. } | Self::PathRequirementDiscovery { error, .. } => {
                Some(error)
            }
            _ => None,
        }
    }
}
impl std::error::Error for LocalPackagePathPinError {}
impl std::error::Error for LocalPackageSourceResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath(error) => Some(error),
            Self::Directory(error) => Some(error),
            _ => None,
        }
    }
}
impl std::error::Error for LocalPackageContractLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidContract(error) => Some(error),
            Self::PackageVersionMismatch { .. } => None,
        }
    }
}

impl std::error::Error for LocalPackageMvsLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsupportedSource { .. } => None,
            Self::SourceResolve(error) => Some(error),
            Self::PathRequirements(error) => Some(error),
            Self::ResolverContext(error) => Some(error),
            Self::Contract(error) => Some(error),
        }
    }
}

impl std::error::Error for LocalPackageMvsProviderFailure {}

impl std::error::Error for LocalPackageMvsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RootDiscovery(error) => Some(error),
            Self::RootContract(error) => Some(error),
            Self::Provider(error) => Some(error),
        }
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn open_directory_from_cwd(
    path: &Path,
    operation: LocalPackageIoOperation,
) -> Result<File, LocalPackageError> {
    openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| map_directory_open_error(error.into(), operation))
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn open_directory_relative(
    base: &File,
    path: &str,
    operation: LocalPackageIoOperation,
) -> Result<File, LocalPackageError> {
    openat(
        base,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| map_directory_open_error(error.into(), operation))
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn map_directory_open_error(
    error: io::Error,
    operation: LocalPackageIoOperation,
) -> LocalPackageError {
    if error.kind() == io::ErrorKind::NotADirectory {
        LocalPackageError::NotDirectory
    } else {
        LocalPackageError::Io {
            operation,
            kind: error.kind(),
        }
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn directory_identity(file: &File) -> Result<LocalPackageDirectoryIdentity, LocalPackageError> {
    let metadata = file.metadata().map_err(|error| LocalPackageError::Io {
        operation: LocalPackageIoOperation::InspectDirectory,
        kind: error.kind(),
    })?;
    if !metadata.is_dir() {
        return Err(LocalPackageError::NotDirectory);
    }
    Ok(LocalPackageDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn read_manifest_from_directory(directory: &File) -> Result<Arc<[u8]>, LocalPackageError> {
    let file = openat(
        directory,
        "sai.package",
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| map_manifest_open_error(error.into()))?;
    read_manifest_file(file)
}

#[cfg(any(
    not(unix),
    target_os = "espidf",
    target_os = "horizon",
    target_os = "redox"
))]
fn read_manifest(path: &Path) -> Result<Arc<[u8]>, LocalPackageError> {
    let file = open_manifest(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => LocalPackageError::MissingManifest,
        kind => LocalPackageError::Io {
            operation: LocalPackageIoOperation::OpenManifest,
            kind,
        },
    })?;
    read_manifest_file(file)
}

fn map_manifest_open_error(error: io::Error) -> LocalPackageError {
    match error.kind() {
        io::ErrorKind::NotFound => LocalPackageError::MissingManifest,
        kind => LocalPackageError::Io {
            operation: LocalPackageIoOperation::OpenManifest,
            kind,
        },
    }
}

fn read_manifest_file(file: File) -> Result<Arc<[u8]>, LocalPackageError> {
    let metadata = file.metadata().map_err(|error| LocalPackageError::Io {
        operation: LocalPackageIoOperation::OpenManifest,
        kind: error.kind(),
    })?;
    if !metadata.is_file() {
        return Err(LocalPackageError::ManifestNotFile);
    }
    if metadata.len() > MAX_PACKAGE_MANIFEST_BYTES as u64 {
        return Err(LocalPackageError::ManifestTooLarge);
    }

    let mut source = Vec::new();
    file.take(MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut source)
        .map_err(|error| LocalPackageError::Io {
            operation: LocalPackageIoOperation::ReadManifest,
            kind: error.kind(),
        })?;
    if source.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(LocalPackageError::ManifestTooLarge);
    }
    Ok(Arc::from(source))
}

#[cfg(all(
    unix,
    any(target_os = "espidf", target_os = "horizon", target_os = "redox")
))]
fn open_manifest(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_manifest(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::manifest::{PackageDeclaration, PackageRole, parse};
    use crate::mvs::{MvsPackageVersion, MvsRequirer};
    use crate::package_path::{DependencyPackagePath, PackagePathError};
    use crate::resolver_contract::{
        ResolverContractBuildError, ResolverContractContext, ResolverPathKey,
        build_resolver_contract,
    };
    use crate::resolver_source::{
        ResolverPackageSource, ResolverPathPinSource, select_resolver_source,
    };
    use crate::resolver_source_provider::{ResolverPackageLoadError, ResolverSourceProviderError};
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    use super::{
        LocalPackageContractLoadError, LocalPackageDirectory, LocalPackageDirectoryIndex,
        LocalPackageDirectoryIndexError, LocalPackageError, LocalPackageMvsError,
        LocalPackageMvsLoadError, LocalPackagePathIndexError, LocalPackagePathPinError,
        LocalPackageSourceResolveError, resolve_local_path_packages,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-package-local-source-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary package tree");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn package(&self, relative: &str, manifest: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create package directory");
            fs::write(path.join("sai.package"), manifest).expect("write package manifest");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const ROOT_MANIFEST: &[u8] = b"package acme:root @ 1.0.0\n\
        source \"src\"\n\
        targets { wasm32-core }\n";
    const DEPENDENCY_MANIFEST: &[u8] = b"package acme:dependency @ 1.2.0\n\
        source \"src\"\n\
        targets { wasm32-core }\n";

    fn package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(
            package,
            Version::parse(version).expect("valid package version"),
        )
        .expect("valid package identity")
    }

    #[test]
    fn opens_absolute_package_directories_and_reads_manifests_by_role() {
        let tree = TempTree::new();
        let directory = tree.package("root", ROOT_MANIFEST);
        let package = LocalPackageDirectory::open(&directory).expect("open local package");

        assert!(package.path().is_absolute());
        assert!(package.path().ends_with("root"));
        assert!(!format!("{package:?}").contains(tree.path().to_str().unwrap()));
        let parsed = package.parse_manifest(PackageRole::Root);
        assert_eq!(parsed.role(), PackageRole::Root);
        assert!(parsed.errors().is_empty());
        assert_eq!(
            parsed
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .identity
                .to_string(),
            "acme:root"
        );

        let dependency_path = tree.package("dependency", DEPENDENCY_MANIFEST);
        let dependency = LocalPackageDirectory::open(dependency_path)
            .expect("open local dependency package")
            .parse_manifest(PackageRole::Dependency);
        assert_eq!(dependency.role(), PackageRole::Dependency);
        assert!(dependency.errors().is_empty());
    }

    #[test]
    fn manifest_parsing_uses_the_bytes_captured_when_the_directory_was_opened() {
        let tree = TempTree::new();
        let directory = tree.package("root", ROOT_MANIFEST);
        let first = LocalPackageDirectory::open(&directory).expect("open local package");

        let changed_manifest = b"package acme:root @ 2.0.0\n\
            source \"src\"\n\
            targets { wasm32-core }\n";
        fs::write(directory.join("sai.package"), changed_manifest)
            .expect("replace package manifest after opening source");

        let first_parse = first.parse_manifest(PackageRole::Root);
        assert_eq!(
            first_parse
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .version
                .to_string(),
            "1.0.0"
        );

        let second = LocalPackageDirectory::open(&directory).expect("reopen local package");
        assert_eq!(
            first, second,
            "directory identity is independent of content"
        );
        assert_eq!(
            second
                .parse_manifest(PackageRole::Root)
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .version
                .to_string(),
            "2.0.0"
        );
    }

    #[test]
    fn dependency_contract_uses_the_captured_manifest_and_checks_its_version() {
        let tree = TempTree::new();
        let root = parse(ROOT_MANIFEST);
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");
        let directory = tree.package("dependency", DEPENDENCY_MANIFEST);
        let dependency =
            LocalPackageDirectory::open(&directory).expect("open dependency directory");
        fs::write(
            directory.join("sai.package"),
            b"package acme:dependency @ 2.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .expect("replace manifest after opening directory");

        let expected = package_version("acme:dependency", "1.2.0");
        let contract = dependency
            .build_dependency_contract(&expected, &context, PackageValidationContext::default())
            .expect("captured manifest builds a valid dependency contract");
        assert_eq!(contract.role(), PackageRole::Dependency);
        assert_eq!(contract.mvs.package_version(), &expected);

        assert_eq!(
            dependency.build_dependency_contract(
                &package_version("acme:dependency", "2.0.0"),
                &context,
                PackageValidationContext::default(),
            ),
            Err(LocalPackageContractLoadError::PackageVersionMismatch {
                expected: Box::new(package_version("acme:dependency", "2.0.0")),
                found: Box::new(expected),
            })
        );

        let invalid = tree.package(
            "invalid",
            b"package acme:invalid @ 1.0.0\ntargets { wasm32-core }\n",
        );
        let invalid = LocalPackageDirectory::open(invalid).expect("open invalid dependency");
        let error = invalid
            .build_dependency_contract(
                &package_version("acme:invalid", "1.0.0"),
                &context,
                PackageValidationContext::default(),
            )
            .expect_err("semantic validation rejects a package without source");
        assert!(matches!(
            &error,
            LocalPackageContractLoadError::InvalidContract(
                ResolverContractBuildError::SemanticValidationErrors(_)
            )
        ));
        assert!(
            error
                .to_string()
                .contains("manifest semantic validation failed")
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn resolves_paths_after_lexical_normalization_and_symlink_canonicalization() {
        let tree = TempTree::new();
        let requiring = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency = tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        fs::create_dir(tree.path().join("workspace/root/unused"))
            .expect("create intermediate package path component");
        let requiring = LocalPackageDirectory::open(requiring).expect("open requiring package");

        // The existing intermediate directory is traversed and then removed
        // by the operating system's handling of `..`.
        let spelling = DependencyPackagePath::parse("unused/../../dependency")
            .expect("valid package-relative dependency path");
        let resolved = requiring
            .resolve_dependency(&spelling)
            .expect("resolve package-relative dependency");
        assert_eq!(
            resolved.path(),
            fs::canonicalize(dependency).expect("canonical dependency path")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let alias = tree.path().join("dependency-alias");
            symlink(resolved.path(), &alias).expect("create package directory symlink");
            let alias_path = DependencyPackagePath::parse("../../dependency-alias")
                .expect("valid package-relative symlink path");
            let through_symlink = requiring
                .resolve_dependency(&alias_path)
                .expect("resolve symlink package directory");
            assert_eq!(through_symlink, resolved);
        }
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_after_symlinks_follow_filesystem_resolution() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let requiring = tree.package("workspace/root", ROOT_MANIFEST);
        let root_dependency = tree.package("workspace/root/dep", ROOT_MANIFEST);
        let outside_dependency = tree.package("outside/dep", DEPENDENCY_MANIFEST);
        let symlink_target = tree.path().join("outside/nested");
        fs::create_dir(&symlink_target).expect("create symlink target");
        symlink(&symlink_target, requiring.join("link")).expect("create directory symlink");

        let requiring = LocalPackageDirectory::open(requiring).expect("open requiring package");
        let path = DependencyPackagePath::parse("link/../dep")
            .expect("valid package-relative symlink path");
        let resolved = requiring
            .resolve_dependency(&path)
            .expect("resolve path through symlink and parent component");

        assert_eq!(
            resolved.path(),
            fs::canonicalize(outside_dependency).expect("canonical outside dependency")
        );
        assert_ne!(
            resolved.path(),
            fs::canonicalize(root_dependency).expect("canonical root dependency")
        );
    }

    #[cfg(not(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    )))]
    #[test]
    fn unsupported_hosts_fail_closed_for_local_dependency_resolution() {
        let tree = TempTree::new();
        let requiring = LocalPackageDirectory::open(tree.package("root", ROOT_MANIFEST))
            .expect("open requiring package");
        let path = DependencyPackagePath::parse("../dependency").expect("valid dependency path");

        assert_eq!(
            requiring.resolve_dependency(&path),
            Err(LocalPackageError::PlatformNotSupported)
        );
    }

    #[test]
    fn local_directory_index_resolves_path_pins_from_the_recorded_requirer() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency_directory = tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        let root_version = package_version("acme:root", "1.0.0");

        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");
        let pin = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: root_version.clone(),
                path: "../dependency".into(),
            },
            requirer: MvsRequirer::Root(root_version),
            span: 12..36,
        };

        let resolved = index
            .canonicalize_path_pin(&pin)
            .expect("resolve local path pin");
        assert_eq!(
            resolved.path(),
            fs::canonicalize(dependency_directory).expect("canonical dependency directory")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let root_version = package_version("acme:root", "1.0.0");
            let root_directory = tree.path().join("workspace/root");
            symlink(
                tree.path().join("workspace/dependency"),
                tree.path().join("dependency-alias"),
            )
            .expect("create dependency directory alias");
            let alias_pin = ResolverPathPinSource {
                source: ResolverPackageSource::Path {
                    relative_to: root_version.clone(),
                    path: "../../dependency-alias".into(),
                },
                requirer: MvsRequirer::Root(root_version),
                span: 40..68,
            };
            let requiring = LocalPackageDirectory::open(root_directory)
                .expect("open indexed requiring package");
            let direct_pin_directory = requiring
                .resolve_dependency(
                    &DependencyPackagePath::parse("../dependency")
                        .expect("valid direct package path"),
                )
                .expect("resolve direct package path");
            let alias_pin_directory = index
                .canonicalize_path_pin(&alias_pin)
                .expect("resolve symlink path pin");
            assert_eq!(direct_pin_directory, alias_pin_directory);
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_and_dependency_with_the_same_version_keep_distinct_path_bases() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency_directory = tree.package("workspace/dependency-root", ROOT_MANIFEST);
        let root_child = tree.package("workspace/root-child", DEPENDENCY_MANIFEST);
        let dependency_child = tree.package(
            "workspace/dependency-child",
            b"package acme:package-child @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let same_version = package_version("acme:root", "1.0.0");

        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                same_version.clone(),
                LocalPackageDirectory::open(&root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");
        index
            .insert(
                same_version.clone(),
                LocalPackageDirectory::open(&dependency_directory)
                    .expect("open same-version dependency package"),
                PackageRole::Dependency,
            )
            .expect("index same-version dependency package");

        assert_eq!(
            index.index_dependency_path(&same_version, "../root-child"),
            Err(LocalPackagePathIndexError::AmbiguousRequirerRole(
                same_version.clone()
            )),
            "the version-only compatibility API fails closed when the requirer role is ambiguous"
        );
        assert_eq!(
            index
                .index_dependency_path_from(
                    &MvsRequirer::Root(same_version.clone()),
                    "../root-child"
                )
                .expect("resolve from root directory"),
            package_version("acme:dependency", "1.2.0")
        );
        assert_eq!(
            index
                .index_dependency_path_from(
                    &MvsRequirer::Package(same_version.clone()),
                    "../dependency-child",
                )
                .expect("resolve from dependency directory"),
            package_version("acme:package-child", "1.0.0")
        );

        let root_pin = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: same_version.clone(),
                path: "../root-child".into(),
            },
            requirer: MvsRequirer::Root(same_version.clone()),
            span: 12..36,
        };
        let dependency_pin = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: same_version.clone(),
                path: "../dependency-child".into(),
            },
            requirer: MvsRequirer::Package(same_version),
            span: 40..68,
        };
        assert_eq!(
            index
                .canonicalize_path_pin(&root_pin)
                .expect("canonicalize root-relative pin")
                .path(),
            fs::canonicalize(root_child).expect("canonical root child path")
        );
        assert_eq!(
            index
                .canonicalize_path_pin(&dependency_pin)
                .expect("canonicalize dependency-relative pin")
                .path(),
            fs::canonicalize(dependency_child).expect("canonical dependency child path")
        );
    }

    #[test]
    fn root_and_dependency_directory_version_conflicts_are_order_independent() {
        let tree = TempTree::new();
        let shared_path = tree.package("shared", ROOT_MANIFEST);
        let root_directory =
            LocalPackageDirectory::open(&shared_path).expect("capture root v1 directory");
        fs::write(
            shared_path.join("sai.package"),
            b"package acme:root @ 2.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .expect("change shared directory manifest");
        let dependency_directory =
            LocalPackageDirectory::open(&shared_path).expect("capture dependency v2 directory");
        let root_version = package_version("acme:root", "1.0.0");
        let dependency_version = package_version("acme:root", "2.0.0");

        let mut root_first = LocalPackageDirectoryIndex::default();
        root_first
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root first");
        assert_eq!(
            root_first.insert(
                dependency_version.clone(),
                dependency_directory.clone(),
                PackageRole::Dependency,
            ),
            Err(
                LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                    existing: Box::new(root_version.clone()),
                    discovered: Box::new(dependency_version.clone()),
                }
            )
        );

        let mut dependency_first = LocalPackageDirectoryIndex::default();
        dependency_first
            .insert(
                dependency_version.clone(),
                dependency_directory,
                PackageRole::Dependency,
            )
            .expect("index dependency first");
        assert_eq!(
            dependency_first.insert(root_version.clone(), root_directory, PackageRole::Root),
            Err(
                LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                    existing: Box::new(dependency_version),
                    discovered: Box::new(root_version),
                }
            )
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn indexing_a_dependency_path_returns_its_version_and_captured_manifest() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency_directory = tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        let root_version = package_version("acme:root", "1.0.0");
        let expected_dependency = package_version("acme:dependency", "1.2.0");
        let root_manifest = parse(ROOT_MANIFEST);
        let path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context");

        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");
        let discovered = index
            .index_dependency_path(&root_version, "../dependency")
            .expect("discover and index path dependency");
        assert_eq!(discovered, expected_dependency);

        fs::write(
            dependency_directory.join("sai.package"),
            b"package acme:dependency @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:late @ ^1 from git \"https://example.test/late\" }\n",
        )
        .expect("replace dependency manifest after indexing");
        let contract = index.entries[&discovered]
            .build_dependency_contract(&discovered, &context, PackageValidationContext::default())
            .expect("load the manifest snapshot used during version discovery");
        assert!(contract.mvs.edges().is_empty());
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn indexing_one_directory_under_two_versions_is_rejected() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency_directory = tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        let root_version = package_version("acme:root", "1.0.0");
        let first_version = package_version("acme:dependency", "1.2.0");
        let second_version = package_version("acme:dependency", "2.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");
        assert_eq!(
            index
                .index_dependency_path(&root_version, "../dependency")
                .expect("index first manifest snapshot"),
            first_version
        );

        fs::write(
            dependency_directory.join("sai.package"),
            b"package acme:dependency @ 2.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .expect("replace dependency version during discovery");
        assert_eq!(
            index.index_dependency_path(&root_version, "../dependency"),
            Err(LocalPackagePathIndexError::Index(
                LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                    existing: Box::new(first_version),
                    discovered: Box::new(second_version),
                }
            ))
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn indexing_a_dependency_path_reports_missing_bases_and_invalid_paths() {
        let root_version = package_version("acme:root", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();

        assert_eq!(
            index.index_dependency_path(&root_version, "../dependency"),
            Err(LocalPackagePathIndexError::MissingBaseDirectory(
                root_version
            ))
        );
        assert_eq!(
            index.index_dependency_path(&package_version("acme:root", "1.0.0"), "./dependency",),
            Err(LocalPackagePathIndexError::InvalidPath(
                PackagePathError::InvalidPackagePath
            ))
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn indexing_a_dependency_path_retains_manifest_syntax_diagnostics() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        tree.package(
            "workspace/invalid",
            b"package acme:invalid @ 1.0.0\nsource \"src\"\nthis is invalid\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");

        let error = index
            .index_dependency_path(&root_version, "../invalid")
            .expect_err("reject invalid dependency syntax");
        assert!(matches!(
            &error,
            LocalPackagePathIndexError::InvalidManifest(_)
        ));
        assert!(
            !error
                .manifest_parse_output()
                .expect("syntax failure retains its parse output")
                .errors()
                .is_empty()
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_path_override_discovery_rolls_back_on_identity_mismatch() {
        let tree = TempTree::new();
        let root_manifest_bytes = b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              overrides {\n\
                acme:first use path \"../first\"\n\
                acme:expected use path \"../second\"\n\
              }\n";
        let root_directory = tree.package("workspace/root", root_manifest_bytes);
        tree.package(
            "workspace/first",
            b"package acme:first @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        tree.package(
            "workspace/second",
            b"package acme:actual @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root package");

        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let expected_span = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| {
                let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                    return None;
                };
                overrides
                    .iter()
                    .find(|item| item.value.package.as_ref() == "acme:expected")
                    .map(|item| item.span.clone())
            })
            .expect("expected path override span");
        let error = index
            .index_root_path_overrides(&root_directory)
            .expect_err("mismatched override identity should fail");
        let display = error.to_string();
        assert!(display.contains("acme:expected"));
        assert!(display.contains(&format!(
            "bytes {}..{}",
            expected_span.start, expected_span.end
        )));
        assert!(matches!(
            error,
            LocalPackagePathIndexError::RootPathOverrideIdentityMismatch {
                expected,
                found,
                span,
            } if expected.as_ref() == "acme:expected"
                && found.as_ref() == "acme:actual"
                && span == expected_span
        ));
        assert!(
            index.entries.is_empty(),
            "failed discovery does not retain the first override snapshot"
        );
        assert_eq!(
            index.root.as_ref().map(|(version, _)| version),
            Some(&root_version),
            "failed discovery preserves the separately indexed root"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_path_override_discovery_retains_source_span_for_path_errors() {
        let tree = TempTree::new();
        let root_manifest_bytes = b"package acme:root @ 1.0.0\n\
            source \"src\"\n\
            targets { wasm32-core }\n\
            overrides { acme:missing use path \"../missing\" }\n";
        let root_directory = tree.package("workspace/root", root_manifest_bytes);
        let root_version = package_version("acme:root", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(root_version, root_directory.clone(), PackageRole::Root)
            .expect("index root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let expected_span = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| {
                let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                    return None;
                };
                overrides
                    .iter()
                    .find(|item| item.value.package.as_ref() == "acme:missing")
                    .map(|item| item.span.clone())
            })
            .expect("missing path override span");

        let error = index
            .index_root_path_overrides(&root_directory)
            .expect_err("missing override path should fail");
        let display = error.to_string();
        assert!(display.contains("acme:missing"));
        assert!(display.contains(&format!(
            "bytes {}..{}",
            expected_span.start, expected_span.end
        )));
        assert!(matches!(
            error,
            LocalPackagePathIndexError::PathDiscovery {
                package,
                span,
                error: source_error,
            } if package.as_ref() == "acme:missing"
                && span == expected_span
                && matches!(
                    *source_error,
                    LocalPackagePathIndexError::Directory(_)
                )
        ));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn duplicate_root_path_override_error_displays_its_span() {
        let tree = TempTree::new();
        let root_manifest_bytes = b"package acme:root @ 1.0.0\n\
            source \"src\"\n\
            targets { wasm32-core }\n\
            overrides {\n\
              acme:dependency use path \"../dependency\"\n\
              acme:dependency use path \"../other\"\n\
            }\n";
        let root_directory = tree.package("workspace/root", root_manifest_bytes);
        tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let root_version = package_version("acme:root", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(root_version, root_directory.clone(), PackageRole::Root)
            .expect("index root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let expected_span = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| {
                let PackageDeclaration::Overrides(overrides) = &declaration.value else {
                    return None;
                };
                overrides
                    .iter()
                    .filter(|item| item.value.package.as_ref() == "acme:dependency")
                    .nth(1)
                    .map(|item| item.span.clone())
            })
            .expect("second duplicate override span");

        let error = index
            .index_root_path_overrides(&root_directory)
            .expect_err("duplicate override should fail");
        assert!(matches!(
            &error,
            LocalPackagePathIndexError::DuplicateRootPathOverride { package, span }
                if package.as_ref() == "acme:dependency" && *span == expected_span
        ));
        let display = error.to_string();
        assert!(display.contains("acme:dependency"));
        assert!(display.contains(&format!(
            "bytes {}..{}",
            expected_span.start, expected_span.end
        )));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_path_requirement_discovery_indexes_active_direct_and_test_paths() {
        let tree = TempTree::new();
        let root_directory = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:direct @ ^1 from path \"../direct\"\n\
                acme:filtered @ ^1 from path \"../filtered\" only { wasm32-wasi }\n\
              }\n\
              requires test { acme:test-tool @ ^1 from path \"../test-tool\" }\n",
        );
        tree.package(
            "workspace/direct",
            b"package acme:direct @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        tree.package(
            "workspace/test-tool",
            b"package acme:test-tool @ 1.4.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root package");

        let path_packages = index
            .index_root_path_requirements(&root_directory, &BTreeMap::new())
            .expect("index active root path requirements");
        assert_eq!(
            path_packages,
            BTreeMap::from([
                (
                    ResolverPathKey::root(root_version.clone(), "../direct"),
                    package_version("acme:direct", "1.2.0"),
                ),
                (
                    ResolverPathKey::root(root_version.clone(), "../test-tool"),
                    package_version("acme:test-tool", "1.4.0"),
                ),
            ])
        );
        assert!(
            !index
                .entries
                .contains_key(&package_version("acme:filtered", "1.0.0")),
            "a path excluded by root targets is not opened"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_source_overrides_replace_direct_path_requirement_sources() {
        let tree = TempTree::new();
        let root_directory = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              overrides {\n\
                acme:patched use path \"../patch\"\n\
                acme:git use git \"https://example.test/acme/git\"\n\
              }\n\
              requires {\n\
                acme:patched @ ^1 from path \"../missing-patched\"\n\
                acme:other @ ^1 from path \"../other\"\n\
                acme:git @ ^2 from path \"../missing-git\"\n\
              }\n",
        );
        tree.package(
            "workspace/patch",
            b"package acme:patched @ 1.3.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        tree.package(
            "workspace/other",
            b"package acme:other @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root package");
        let root_path_overrides = index
            .index_root_path_overrides(&root_directory)
            .expect("index root path override");

        let path_packages = index
            .index_root_path_requirements(&root_directory, &root_path_overrides)
            .expect("index remaining root path requirements");
        assert_eq!(
            path_packages,
            BTreeMap::from([(
                ResolverPathKey::root(root_version, "../other"),
                package_version("acme:other", "1.2.0"),
            )])
        );
        assert!(
            index
                .entries
                .contains_key(&package_version("acme:patched", "1.3.0")),
            "the path override snapshot remains indexed"
        );
        assert_eq!(
            index.entries.len(),
            2,
            "neither replaced path source is opened"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_path_requirement_discovery_rolls_back_and_reports_failing_span() {
        let tree = TempTree::new();
        let root_directory = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:first @ ^1 from path \"../first\"\n\
                acme:missing @ ^1 from path \"../missing\"\n\
              }\n",
        );
        tree.package(
            "workspace/first",
            b"package acme:first @ 1.1.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let expected_span = root_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| {
                let PackageDeclaration::Requires { entries, .. } = &declaration.value else {
                    return None;
                };
                entries
                    .iter()
                    .find(|item| item.value.package.as_ref() == "acme:missing")
                    .map(|item| item.span.clone())
            })
            .expect("missing root path requirement span");

        let error = index
            .index_root_path_requirements(&root_directory, &BTreeMap::new())
            .expect_err("missing path should fail");
        assert!(matches!(
            &error,
            LocalPackagePathIndexError::PathRequirementDiscovery {
                package,
                path,
                requiring_package,
                span,
                error: source_error,
            } if package.as_ref() == "acme:missing"
                && path.as_ref() == "../missing"
                && requiring_package == &root_version
                && *span == expected_span
                && matches!(
                    source_error.as_ref(),
                    LocalPackagePathIndexError::Directory(_)
                )
        ));
        let display = error.to_string();
        assert!(display.contains("acme:missing"));
        assert!(display.contains("../missing"));
        assert!(display.contains(&format!(
            "bytes {}..{}",
            expected_span.start, expected_span.end
        )));
        assert!(
            index.entries.is_empty(),
            "failed root requirement discovery commits no partial paths"
        );
        assert_eq!(
            index.root.as_ref().map(|(version, _)| version),
            Some(&root_version),
            "failed discovery preserves the separately indexed root"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn dependency_path_requirement_discovery_filters_targets_and_test_edges() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency_directory = tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core, wasm32-wasi }\n\
              requires {\n\
                acme:active @ ^1 from path \"../active\"\n\
                acme:inactive @ ^1 from path \"../inactive\" only { wasm32-wasi }\n\
              }\n\
              requires test { acme:test-only @ ^1 from path \"../test-only\" }\n",
        );
        tree.package(
            "workspace/active",
            b"package acme:active @ 1.3.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let dependency_version = package_version("acme:dependency", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let root_path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let root_context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &root_path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("build root resolver context");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(root_version.clone(), root_directory, PackageRole::Root)
            .expect("index root package");
        index
            .index_dependency_path(&root_version, "../dependency")
            .expect("index requiring dependency");

        let path_packages = index
            .index_dependency_path_requirements(&dependency_version, &root_context)
            .expect("discover dependency path requirements");
        assert_eq!(
            path_packages,
            BTreeMap::from([(
                ResolverPathKey::package(dependency_version.clone(), "../active"),
                package_version("acme:active", "1.3.0"),
            )])
        );
        assert!(
            !index
                .entries
                .contains_key(&package_version("acme:inactive", "1.0.0")),
            "dependency paths excluded by root targets are not opened"
        );
        assert!(
            !index
                .entries
                .contains_key(&package_version("acme:test-only", "1.0.0")),
            "dependency test requirements are not part of resolution"
        );
        assert!(dependency_directory.is_dir());
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn dependency_source_overrides_replace_path_requirement_sources() {
        let tree = TempTree::new();
        let root_directory = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              overrides {\n\
                acme:patched use path \"../patch\"\n\
                acme:git use git \"https://example.test/acme/git\"\n\
              }\n",
        );
        tree.package(
            "workspace/patch",
            b"package acme:patched @ 1.4.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:patched @ ^1 from path \"../missing-patched\"\n\
                acme:git @ ^2 from path \"../missing-git\"\n\
                acme:other @ ^1 from path \"../other\"\n\
              }\n",
        );
        tree.package(
            "workspace/other",
            b"package acme:other @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let dependency_version = package_version("acme:dependency", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_directory.clone(),
                PackageRole::Root,
            )
            .expect("index root package");
        let root_path_overrides = index
            .index_root_path_overrides(&root_directory)
            .expect("index root path override");
        let root_path_packages = BTreeMap::new();
        let root_context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &root_path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("build root resolver context");
        index
            .index_dependency_path(&root_version, "../dependency")
            .expect("index requiring dependency");

        let path_packages = index
            .index_dependency_path_requirements(&dependency_version, &root_context)
            .expect("discover non-overridden dependency paths");
        assert_eq!(
            path_packages,
            BTreeMap::from([(
                ResolverPathKey::package(dependency_version, "../other"),
                package_version("acme:other", "1.2.0"),
            )])
        );
        assert_eq!(index.entries.len(), 3);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn dependency_path_requirement_discovery_is_atomic_and_source_located() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:first @ ^1 from path \"../first\"\n\
                acme:missing @ ^1 from path \"../missing\"\n\
              }\n",
        );
        tree.package(
            "workspace/first",
            b"package acme:first @ 1.1.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_version = package_version("acme:root", "1.0.0");
        let dependency_version = package_version("acme:dependency", "1.0.0");
        let root_directory =
            LocalPackageDirectory::open(root_directory).expect("open root package");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let root_path_packages = BTreeMap::new();
        let root_path_overrides = BTreeMap::new();
        let root_context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &root_path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("build root resolver context");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(root_version.clone(), root_directory, PackageRole::Root)
            .expect("index root package");
        index
            .index_dependency_path(&root_version, "../dependency")
            .expect("index requiring dependency");
        let dependency_directory = &index.entries[&dependency_version];
        let dependency_manifest = dependency_directory.parse_manifest(PackageRole::Dependency);
        let expected_span = dependency_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| {
                let PackageDeclaration::Requires { entries, .. } = &declaration.value else {
                    return None;
                };
                entries
                    .iter()
                    .find(|item| item.value.package.as_ref() == "acme:missing")
                    .map(|item| item.span.clone())
            })
            .expect("missing path requirement span");

        let error = index
            .index_dependency_path_requirements(&dependency_version, &root_context)
            .expect_err("missing dependency path should fail");
        assert!(matches!(
            &error,
            LocalPackagePathIndexError::PathRequirementDiscovery {
                package,
                path,
                requiring_package,
                span,
                error: source_error,
            } if package.as_ref() == "acme:missing"
                && path.as_ref() == "../missing"
                && requiring_package == &dependency_version
                && *span == expected_span
                && matches!(
                    source_error.as_ref(),
                    LocalPackagePathIndexError::Directory(_)
                )
        ));
        assert_eq!(
            index.entries.keys().cloned().collect::<Vec<_>>(),
            [dependency_version],
            "failed discovery does not retain the first path snapshot"
        );
        assert_eq!(
            index.root.as_ref().map(|(version, _)| version),
            Some(&root_version),
            "failed discovery preserves the separately indexed root"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn root_path_override_discovery_requires_the_indexed_root_directory_and_snapshot() {
        let tree = TempTree::new();
        let root_manifest_bytes = b"package acme:root @ 1.0.0\n\
            source \"src\"\n\
            targets { wasm32-core }\n\
            overrides { acme:dependency use path \"../dependency\" }\n";
        let root_directory = tree.package("workspace/root", root_manifest_bytes);
        let identical_root_directory =
            tree.package("workspace/identical-root", root_manifest_bytes);
        let root_version = package_version("acme:root", "1.0.0");
        let root_snapshot =
            LocalPackageDirectory::open(&root_directory).expect("open root package");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                root_snapshot.clone(),
                PackageRole::Root,
            )
            .expect("index root package");
        let identical_root = LocalPackageDirectory::open(identical_root_directory)
            .expect("open identical root manifest from another directory");
        assert_eq!(
            index.index_root_path_overrides(&identical_root),
            Err(LocalPackagePathIndexError::RootManifestSnapshotMismatch(
                root_version.clone()
            ))
        );
        fs::write(
            root_directory.join("sai.package"),
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              overrides { acme:dependency use path \"../replacement\" }\n",
        )
        .expect("replace root manifest");
        let changed_root =
            LocalPackageDirectory::open(&root_directory).expect("reopen root package");

        assert_eq!(
            index.index_root_path_overrides(&changed_root),
            Err(LocalPackagePathIndexError::RootManifestSnapshotMismatch(
                root_version
            ))
        );
    }

    #[test]
    fn local_directory_index_rejects_substituted_bases_and_sources() {
        let tree = TempTree::new();
        let root_version = package_version("acme:root", "1.0.0");
        let other_version = package_version("acme:other", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(tree.package("root", ROOT_MANIFEST))
                    .expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");

        let mismatched_base = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: other_version.clone(),
                path: "../dependency".into(),
            },
            requirer: MvsRequirer::Root(root_version.clone()),
            span: 0..1,
        };
        assert_eq!(
            index.canonicalize_path_pin(&mismatched_base),
            Err(LocalPackagePathPinError::SourceBaseMismatch {
                expected: Box::new(root_version.clone()),
                found: Box::new(other_version.clone()),
            })
        );

        let git_tree_base = ResolverPathPinSource {
            source: ResolverPackageSource::Git {
                url: "https://example.test/repo".into(),
                directory: None,
            },
            requirer: MvsRequirer::Package(other_version.clone()),
            span: 1..2,
        };
        assert_eq!(
            index.canonicalize_path_pin(&git_tree_base),
            Err(LocalPackagePathPinError::UnsupportedGitTreeBase)
        );

        assert_eq!(
            index.insert(
                other_version.clone(),
                LocalPackageDirectory::open(tree.package("wrong", ROOT_MANIFEST))
                    .expect("open mismatched package"),
                PackageRole::Dependency,
            ),
            Err(LocalPackageDirectoryIndexError::PackageVersionMismatch {
                expected: Box::new(other_version.clone()),
                found_package: "acme:root".into(),
                found_version: Box::new(Version::parse("1.0.0").unwrap()),
            })
        );

        let root_directory = tree.path().join("root");
        assert_eq!(
            index.insert(
                root_version.clone(),
                LocalPackageDirectory::open(&root_directory)
                    .expect("reopen root package directory"),
                PackageRole::Root,
            ),
            Ok(()),
            "registering the same canonical directory should be idempotent"
        );
        assert_eq!(
            index.insert(
                root_version.clone(),
                LocalPackageDirectory::open(tree.package("root-alias", ROOT_MANIFEST))
                    .expect("open second root package directory"),
                PackageRole::Root,
            ),
            Err(LocalPackageDirectoryIndexError::ConflictingDirectories(
                root_version.clone()
            ))
        );

        let missing_requirer = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: other_version.clone(),
                path: "../dependency".into(),
            },
            requirer: MvsRequirer::Package(other_version.clone()),
            span: 2..3,
        };
        assert_eq!(
            index.canonicalize_path_pin(&missing_requirer),
            Err(LocalPackagePathPinError::MissingRequirerDirectory(
                other_version
            ))
        );

        let invalid_path = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: root_version.clone(),
                path: "./dependency".into(),
            },
            requirer: MvsRequirer::Root(root_version.clone()),
            span: 3..4,
        };
        assert_eq!(
            index.canonicalize_path_pin(&invalid_path),
            Err(LocalPackagePathPinError::InvalidPath(
                crate::package_path::PackagePathError::InvalidPackagePath
            ))
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn path_resolution_fails_closed_when_the_requiring_directory_is_replaced() {
        let tree = TempTree::new();
        let root_directory = tree.package("workspace/root", ROOT_MANIFEST);
        tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        tree.package("moved/dependency", DEPENDENCY_MANIFEST);

        let root_version = package_version("acme:root", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .insert(
                root_version.clone(),
                LocalPackageDirectory::open(&root_directory).expect("open root package"),
                PackageRole::Root,
            )
            .expect("index root package");

        fs::rename(&root_directory, tree.path().join("moved/root"))
            .expect("move the opened root directory");
        tree.package("workspace/root", ROOT_MANIFEST);

        let pin = ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to: root_version.clone(),
                path: "../dependency".into(),
            },
            requirer: MvsRequirer::Root(root_version),
            span: 0..14,
        };
        assert_eq!(
            index.canonicalize_path_pin(&pin),
            Err(LocalPackagePathPinError::Directory(
                LocalPackageError::DirectoryChanged
            ))
        );
    }

    #[test]
    fn rejects_missing_and_non_directory_package_sources_without_disclosing_paths() {
        let tree = TempTree::new();
        let missing = tree.path().join("missing");
        assert!(matches!(
            LocalPackageDirectory::open(&missing),
            Err(LocalPackageError::Io { .. })
        ));

        let no_manifest = tree.path().join("no-manifest");
        fs::create_dir(&no_manifest).expect("create directory without manifest");
        assert_eq!(
            LocalPackageDirectory::open(&no_manifest),
            Err(LocalPackageError::MissingManifest)
        );

        let file = tree.path().join("file");
        fs::write(&file, b"not a directory").expect("write regular file");
        assert_eq!(
            LocalPackageDirectory::open(&file),
            Err(LocalPackageError::NotDirectory)
        );

        let relative = Path::new("relative/package");
        assert_eq!(
            LocalPackageDirectory::open(relative),
            Err(LocalPackageError::RelativeDirectory)
        );
        assert!(
            !format!("{}", LocalPackageError::MissingManifest)
                .contains(tree.path().to_str().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_manifest_symlink_to_a_special_file() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let directory = tree.path().join("special-manifest");
        fs::create_dir(&directory).expect("create package directory");
        symlink("/dev/null", directory.join("sai.package")).expect("link special file");

        assert_eq!(
            LocalPackageDirectory::open(directory),
            Err(LocalPackageError::ManifestNotFile)
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios")),
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn opens_fifo_manifests_without_blocking() {
        use std::sync::mpsc;
        use std::time::Duration;

        use rustix::fs::{CWD, Mode, mkfifoat};

        let tree = TempTree::new();
        let directory = tree.path().join("fifo-manifest");
        fs::create_dir(&directory).expect("create package directory");
        mkfifoat(CWD, directory.join("sai.package"), Mode::RUSR | Mode::WUSR)
            .expect("create FIFO manifest");

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(LocalPackageDirectory::open(directory));
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)),
            Ok(Err(LocalPackageError::ManifestNotFile)),
            "opening a FIFO manifest must return without waiting for a writer"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn resolves_transitive_local_paths_through_source_aware_mvs() {
        let tree = TempTree::new();
        let root_path = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:dependency @ ^1 from path \"../dependency\" }\n",
        );
        let dependency_path = tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:sibling @ ^1 from path \"../sibling\" }\n",
        );
        let sibling_path = tree.package(
            "workspace/sibling",
            b"package acme:sibling @ 1.4.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_directory = LocalPackageDirectory::open(root_path).expect("open root");
        let expected_root_path =
            fs::canonicalize(root_directory.path()).expect("canonical root path");

        let output = resolve_local_path_packages(
            root_directory,
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("resolve local path packages");

        let dependency = package_version("acme:dependency", "1.2.0");
        let sibling = package_version("acme:sibling", "1.4.0");
        let selected = output.provider().output().selection().selected();
        assert_eq!(selected.get(&dependency.node()), Some(dependency.version()));
        assert_eq!(selected.get(&sibling.node()), Some(sibling.version()));
        assert_eq!(
            output
                .context()
                .path_packages()
                .get(&ResolverPathKey::package(dependency.clone(), "../sibling")),
            Some(&sibling)
        );

        let root_identity = output
            .provider()
            .source_identities()
            .root_identity()
            .expect("root source identity");
        assert_eq!(root_identity.path(), expected_root_path);
        let package_identities = output.provider().source_identities().package_identities();
        assert_eq!(
            package_identities[&dependency].path(),
            fs::canonicalize(dependency_path).expect("canonical dependency path")
        );
        assert_eq!(
            package_identities[&sibling].path(),
            fs::canonicalize(sibling_path).expect("canonical sibling path")
        );

        let plan = output
            .plan_final_sources(&HashMap::new())
            .expect("plan final sources from accumulated path context");
        assert_eq!(plan.entries().len(), 2);
        assert!(plan.failures().is_empty());
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn rejects_a_path_manifest_changed_after_version_discovery() {
        let tree = TempTree::new();
        let root_path = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:dependency @ ^1 from path \"../dependency\" }\n",
        );
        let dependency_path = tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_directory = LocalPackageDirectory::open(root_path).expect("open root");
        let root_version = package_version("acme:root", "1.0.0");
        let mut index = LocalPackageDirectoryIndex::default();
        index
            .index_root_directory(&root_directory)
            .expect("index root");
        let root_manifest = root_directory.parse_manifest(PackageRole::Root);
        let root_path_overrides = index
            .index_root_path_overrides(&root_directory)
            .expect("index root path overrides");
        let root_path_packages = index
            .index_root_path_requirements(&root_directory, &root_path_overrides)
            .expect("discover root path requirements");
        let context = ResolverContractContext::from_root_manifest(
            &root_manifest,
            &root_path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("build root resolver context");
        let root_contract = build_resolver_contract(
            &root_manifest,
            &context,
            PackageValidationContext::default(),
        )
        .expect("build root resolver contract");
        let selection = crate::mvs::select(
            &root_contract.mvs,
            &HashMap::new(),
            &HashMap::new(),
            &context.root_override_pins(),
        )
        .expect("select path dependency");
        let request = selection
            .contract_requests()
            .first()
            .expect("root names one dependency");
        let source_selection = select_resolver_source(
            request,
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("select dependency path source");

        fs::write(
            dependency_path.join("sai.package"),
            b"package acme:dependency @ 1.2.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:late @ ^1 from git \"https://example.test/late\" }\n",
        )
        .expect("change the dependency manifest in place");
        let canonicalized = index
            .canonicalize_selection_path_pins(&source_selection)
            .expect("canonicalize the path pin")
            .expect("path pin has a directory");

        assert_eq!(
            index.resolve_selected_path_source(&source_selection, &root_version, &[canonicalized],),
            Err(LocalPackageSourceResolveError::SelectedManifestChanged(
                package_version("acme:dependency", "1.2.0")
            ))
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn local_path_resolver_uses_a_root_path_override_for_git_requirement() {
        let tree = TempTree::new();
        let root_path = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:dependency @ ^1 from git \"https://example.test/dependency\" }\n\
              overrides { acme:dependency use path \"../dependency\" }\n",
        );
        tree.package(
            "workspace/dependency",
            b"package acme:dependency @ 1.3.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_directory = LocalPackageDirectory::open(root_path).expect("open root");

        let output = resolve_local_path_packages(
            root_directory,
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("resolve dependency through root path override");

        let dependency = package_version("acme:dependency", "1.3.0");
        assert_eq!(
            output
                .provider()
                .output()
                .selection()
                .selected()
                .get(&dependency.node()),
            Some(dependency.version())
        );
        let source = output
            .provider()
            .output()
            .sources_used()
            .get(&dependency)
            .expect("selected source");
        assert_eq!(
            source.reason(),
            crate::resolver_source::ResolverSourceReason::RootPathOverride
        );
        assert_eq!(
            source.source(),
            &ResolverPackageSource::Path {
                relative_to: package_version("acme:root", "1.0.0"),
                path: "../dependency".into(),
            }
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn local_path_resolver_reports_git_source_as_unsupported_with_partial_selection() {
        let tree = TempTree::new();
        let root_path = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:dependency @ ^1 from git \"https://example.test/dependency\" }\n",
        );
        let root_directory = LocalPackageDirectory::open(root_path).expect("open root");

        let error = resolve_local_path_packages(
            root_directory,
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect_err("a Git source must not be treated as a local directory");
        let LocalPackageMvsError::Provider(failure) = error else {
            panic!("expected source-provider failure");
        };
        let ResolverSourceProviderError::Resolution {
            failures, output, ..
        } = failure.error()
        else {
            panic!("expected a partial MVS resolution result");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0].error,
            ResolverPackageLoadError::Provider(error)
                if matches!(
                    **error,
                    LocalPackageMvsLoadError::UnsupportedSource {
                        package_version: ref found_version,
                        ..
                    } if found_version.as_ref()
                        == &package_version("acme:dependency", "1.0.0")
                )
        ));
        assert!(
            output
                .selection()
                .selected()
                .contains_key(&package_version("acme:dependency", "1.0.0").node()),
            "failed source loading preserves the partial selected node"
        );
        assert!(
            failure.context().root_targets().contains("wasm32-core"),
            "the accumulated root context remains available with partial results"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn failed_dependency_contract_does_not_commit_its_discovered_paths() {
        let tree = TempTree::new();
        let root_path = tree.package(
            "workspace/root",
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:bad @ ^1 from path \"../bad\"\n\
                acme:valid @ ^1 from path \"../valid\"\n\
              }\n",
        );
        tree.package(
            "workspace/bad",
            b"package acme:bad @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:expected @ ^1 from path \"../bad-common\" }\n",
        );
        tree.package(
            "workspace/bad-common",
            b"package acme:actual @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        tree.package(
            "workspace/valid",
            b"package acme:valid @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires { acme:actual @ ^1 from path \"../valid-common\" }\n",
        );
        let valid_common_path = tree.package(
            "workspace/valid-common",
            b"package acme:actual @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        );
        let root_directory = LocalPackageDirectory::open(root_path).expect("open root");

        let LocalPackageMvsError::Provider(failure) = resolve_local_path_packages(
            root_directory,
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect_err("the bad dependency contract is invalid") else {
            panic!("expected provider failure");
        };
        let ResolverSourceProviderError::Resolution {
            failures, output, ..
        } = failure.error()
        else {
            panic!("expected partial MVS state");
        };
        assert_eq!(
            failures.len(),
            1,
            "the valid independent dependency must not fail due to the bad branch"
        );
        let valid = package_version("acme:valid", "1.0.0");
        let actual = package_version("acme:actual", "1.0.0");
        assert!(
            output.dependency_contracts().contains_key(&valid),
            "the valid dependency contract should load"
        );
        assert!(
            output.dependency_contracts().contains_key(&actual),
            "the valid branch's path dependency should load"
        );
        assert_eq!(
            failure.directory_index().entries[&actual].path(),
            fs::canonicalize(valid_common_path).expect("canonical valid path")
        );
        assert_eq!(
            failure
                .context()
                .path_packages()
                .get(&ResolverPathKey::package(valid.clone(), "../valid-common")),
            Some(&actual)
        );
        assert!(
            !failure
                .directory_index()
                .entries
                .values()
                .any(|directory| directory.path().ends_with("workspace/bad-common")),
            "path discoveries from an invalid contract must be rolled back"
        );
    }

    #[test]
    fn rejects_local_manifests_over_the_fixed_size_limit() {
        let tree = TempTree::new();
        let directory = tree.path().join("large-manifest");
        fs::create_dir(&directory).expect("create package directory");
        fs::File::create(directory.join("sai.package"))
            .expect("create package manifest")
            .set_len(super::MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
            .expect("extend package manifest");

        assert_eq!(
            LocalPackageDirectory::open(directory),
            Err(LocalPackageError::ManifestTooLarge)
        );
    }
}
