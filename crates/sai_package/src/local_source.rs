//! Filesystem primitives for local path package sources.
//!
//! Git-backed package trees are resolved by a separate provider. This module
//! handles local package directories without invoking external tools.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
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
    MAX_PACKAGE_MANIFEST_BYTES, PackageParseOutput, PackageRole, parse_for_role,
};
use crate::mvs::MvsPackageVersion;
use crate::package_path::{DependencyPackagePath, PackagePathError};
use crate::resolver_contract::{
    ResolverContract, ResolverContractBuildError, ResolverContractContext,
    build_resolver_contract as adapt_resolver_contract,
};
use crate::resolver_source::{
    ResolverPackageSource, ResolverPathPinSource, ResolverPathPinValidationError,
    ResolverSourceProvenance, ResolverSourceReason, ResolverSourceSelection,
    validate_resolver_path_pins,
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

/// Local directories already read for concrete package versions.
///
/// A separate index can be used for each resolver load snapshot. Inserting
/// the same version twice is idempotent when both entries resolve to the same
/// directory, and rejects two different local directories for that version.
#[derive(Clone, Debug, Default)]
pub struct LocalPackageDirectoryIndex {
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
    InvalidPath(PackagePathError),
    Directory(LocalPackageError),
    InvalidManifest(Box<PackageParseOutput>),
    MissingPackageHeader(Box<PackageParseOutput>),
    InvalidPackageIdentity(Box<str>),
    Index(LocalPackageDirectoryIndexError),
}

impl LocalPackagePathIndexError {
    /// Returns the captured parse result when path discovery fails on manifest
    /// syntax or a missing package header.
    pub fn manifest_parse_output(&self) -> Option<&PackageParseOutput> {
        match self {
            Self::InvalidManifest(parsed) | Self::MissingPackageHeader(parsed) => Some(parsed),
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
        self.insert_parsed(package_version, directory, &parsed)
    }

    /// Resolves a dependency path from an indexed local package, discovers the
    /// dependency version from the captured manifest, and indexes that same
    /// opened directory snapshot.
    ///
    /// Call this when building local path-package maps so the version used to
    /// create resolver context and the manifest later consumed by the loader
    /// come from one per-load directory index.
    pub fn index_dependency_path(
        &mut self,
        relative_to: &MvsPackageVersion,
        path: &str,
    ) -> Result<MvsPackageVersion, LocalPackagePathIndexError> {
        let path =
            DependencyPackagePath::parse(path).map_err(LocalPackagePathIndexError::InvalidPath)?;
        let base = self
            .entries
            .get(relative_to)
            .ok_or_else(|| LocalPackagePathIndexError::MissingBaseDirectory(relative_to.clone()))?;
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
        self.insert_parsed(package_version.clone(), directory, &parsed)
            .map_err(LocalPackagePathIndexError::Index)?;
        Ok(package_version)
    }

    fn insert_parsed(
        &mut self,
        package_version: MvsPackageVersion,
        directory: LocalPackageDirectory,
        parsed: &PackageParseOutput,
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

        match self.entries.get(&package_version) {
            Some(existing) if existing == &directory => Ok(()),
            Some(_) => Err(LocalPackageDirectoryIndexError::ConflictingDirectories(
                package_version,
            )),
            None => {
                if let Some((existing, _)) = self
                    .entries
                    .iter()
                    .find(|(_, existing)| *existing == &directory)
                {
                    return Err(
                        LocalPackageDirectoryIndexError::ConflictingVersionsForDirectory {
                            existing: Box::new(existing.clone()),
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
        let base = self.entries.get(relative_to).ok_or_else(|| {
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
    /// `PathPin` selections use the exact keys returned to the loader by
    /// `select_with_source_provider_and_canonicalized_pins`. Root path
    /// overrides are resolved from the indexed root directory and matched
    /// against the snapshot indexed for the selected package version. Path
    /// lock floors are rejected because the lock format excludes path entries
    /// from floors. Git selections return `None` for a separate Git provider.
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
        let base = self.entries.get(relative_to).ok_or_else(|| {
            LocalPackageSourceResolveError::MissingBaseDirectory(relative_to.clone())
        })?;
        let path = DependencyPackagePath::parse(path)
            .map_err(LocalPackageSourceResolveError::InvalidPath)?;
        base.resolve_dependency(&path)
            .map_err(LocalPackageSourceResolveError::Directory)
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
                "root path override for {}@{} no longer resolves to its indexed directory",
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::manifest::{PackageRole, parse};
    use crate::mvs::{MvsPackageVersion, MvsRequirer};
    use crate::package_path::{DependencyPackagePath, PackagePathError};
    use crate::resolver_contract::{ResolverContractBuildError, ResolverContractContext};
    use crate::resolver_source::{ResolverPackageSource, ResolverPathPinSource};
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    use super::{
        LocalPackageContractLoadError, LocalPackageDirectory, LocalPackageDirectoryIndex,
        LocalPackageDirectoryIndexError, LocalPackageError, LocalPackagePathIndexError,
        LocalPackagePathPinError,
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
