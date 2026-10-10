//! Bounded Git release-tag listing and locked-source fetching.

use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::hash::{BuildHasher, RandomState};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gix::bstr::{BStr, BString, ByteSlice};

use crate::git_tags::{
    GitReleaseTag, GitReleaseTagListError, GitReleaseTagListLimit, parse_exact_release_tag_ref,
    parse_release_tag_refs, release_tag_prefix,
};
use crate::git_tree::{
    GitPackageArtifactChecksum, GitPackageFile, GitPackageSourceTree, GitTreeReadError,
    GitTreeReader,
};
use crate::lock::{LockPackage, LockPackageField, LockSource};
use crate::lock_validation::{canonical_git_url, valid_checksum};
use crate::manifest::{PackageDeclaration, PackageParseOutput, PackageRole};
use crate::mvs::MvsPackageVersion;
#[cfg(not(test))]
use crate::package_cache::cache_directory_from_environment;
use crate::package_cache::{
    CachedPackageChecksums, CachedPackageSource, CachedSourceVerification, PackageCache,
    PackageCacheError, SourceCacheRemoval, create_staging_activity_lock,
    release_staging_activity_lock, staging_activity_lock_path,
};
use crate::package_path::{
    DependencyPackagePath, PackagePathError, RepositoryPackageDirectory,
    normalize_git_relative_package_directory,
};
use crate::resolver_contract::{
    ResolverContract, ResolverContractBuildError, ResolverContractContext, build_resolver_contract,
};
use crate::resolver_source::{
    ResolverPackageSource, ResolverSourceReason, ResolverSourceSelection,
};
use crate::tree_hash::{TreeHash, TreeHashError};
use crate::validation::PackageValidationContext;
use crate::version::Version;

const SSH_TAG_LIST_READ_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_GIT_FETCH_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// An artifact checksum expected from the lock file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedArtifactChecksum {
    /// The component target that selects this artifact.
    pub target: Box<str>,
    /// The expected `sai1:` checksum of the artifact bytes.
    pub checksum: Box<str>,
}

/// All checksums that a locked Git package must match before caching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedPackageChecksums {
    /// The expected package source tree checksum.
    pub source_checksum: Box<str>,
    /// The expected WIT tree checksum, if the lock records one.
    pub wit_checksum: Option<Box<str>>,
    /// Expected artifact checksums, one per target.
    pub artifact_checksums: Vec<ExpectedArtifactChecksum>,
}

impl ExpectedPackageChecksums {
    /// Collect checksum fields from a lock package after lock validation.
    ///
    /// Returns `None` when the package has no source checksum.
    pub fn from_lock_package(package: &LockPackage) -> Option<Self> {
        let mut source_checksum = None;
        let mut wit_checksum = None;
        let mut artifact_checksums = Vec::new();
        for field in &package.fields {
            match &field.value {
                LockPackageField::Checksum(value) => source_checksum = Some(value.clone()),
                LockPackageField::WitChecksum(value) => wit_checksum = Some(value.clone()),
                LockPackageField::Artifact { target, checksum } => {
                    artifact_checksums.push(ExpectedArtifactChecksum {
                        target: target.clone(),
                        checksum: checksum.clone(),
                    });
                }
                _ => {}
            }
        }
        Some(Self {
            source_checksum: source_checksum?,
            wit_checksum,
            artifact_checksums,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockedReleaseRequest {
    package: Box<str>,
    url: Box<str>,
    version: Version,
    directory: Option<RepositoryPackageDirectory>,
    commit: gix::ObjectId,
    checksums: ExpectedPackageChecksums,
}

fn locked_release_request(
    package: &LockPackage,
) -> Result<LockedReleaseRequest, GitRemoteReleaseFetchError> {
    let mut source = None;
    let mut directory = None;
    let mut commit = None;
    let mut has_checksum = false;
    let mut has_wit_checksum = false;
    let mut artifact_targets = BTreeSet::new();
    for field in &package.fields {
        match &field.value {
            LockPackageField::Source(LockSource::Git(url)) => {
                if source.replace(url.clone()).is_some() {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "source",
                    ));
                }
            }
            LockPackageField::Source(LockSource::Path(_)) => {
                return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                    "source",
                ));
            }
            LockPackageField::Directory(value) => {
                if directory.replace(value.clone()).is_some() {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "directory",
                    ));
                }
            }
            LockPackageField::Commit(value) => {
                if commit.replace(value.clone()).is_some() {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "commit",
                    ));
                }
            }
            LockPackageField::Checksum(value) => {
                if std::mem::replace(&mut has_checksum, true) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "checksum",
                    ));
                }
                if !valid_checksum(value) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "checksum",
                    ));
                }
            }
            LockPackageField::WitChecksum(value) => {
                if std::mem::replace(&mut has_wit_checksum, true) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "WIT checksum",
                    ));
                }
                if !valid_checksum(value) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "WIT checksum",
                    ));
                }
            }
            LockPackageField::Artifact { target, checksum } => {
                if !valid_checksum(checksum) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "artifact checksum",
                    ));
                }
                if !crate::validation::is_component_target(target) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "artifact target",
                    ));
                }
                if !artifact_targets.insert(target.as_ref()) {
                    return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                        "artifact target",
                    ));
                }
            }
            _ => {}
        }
    }

    let url = source.ok_or(GitRemoteReleaseFetchError::InvalidLockedPackageField(
        "Git source",
    ))?;
    let canonical_url = canonical_git_url(&url)
        .map_err(|_| GitRemoteReleaseFetchError::InvalidLockedPackageField("source"))?;
    if canonical_url != url.as_ref() {
        return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
            "source",
        ));
    }
    let directory = if let Some(value) = directory.as_deref() {
        Some(
            RepositoryPackageDirectory::parse(value)
                .map_err(|_| GitRemoteReleaseFetchError::InvalidLockedPackageField("directory"))?,
        )
    } else {
        None
    };
    if !artifact_targets.is_empty() && !has_wit_checksum {
        return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
            "WIT checksum",
        ));
    }
    let commit_text = commit.ok_or(GitRemoteReleaseFetchError::InvalidLockedPackageField(
        "commit",
    ))?;
    let commit = gix::ObjectId::from_hex(commit_text.as_bytes())
        .map_err(|_| GitRemoteReleaseFetchError::InvalidLockedPackageField("commit"))?;
    if commit.to_hex() != commit_text.as_ref() {
        return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
            "commit",
        ));
    }
    let checksums = ExpectedPackageChecksums::from_lock_package(package).ok_or(
        GitRemoteReleaseFetchError::InvalidLockedPackageField("source checksum"),
    )?;

    Ok(LockedReleaseRequest {
        package: package.package.clone(),
        url,
        version: package.version.clone(),
        directory,
        commit,
        checksums,
    })
}

/// The checksum kind named by a locked-content mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitPackageChecksumKind {
    /// The complete selected package source tree.
    Source,
    /// The separately declared WIT directory.
    Wit,
    /// The component artifact for this target.
    Artifact {
        /// Target associated with the artifact checksum.
        target: Box<str>,
    },
}

impl fmt::Display for GitPackageChecksumKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source => formatter.write_str("source"),
            Self::Wit => formatter.write_str("wit"),
            Self::Artifact { target } => write!(formatter, "artifact {target}"),
        }
    }
}

/// Package and source identity attached to a locked checksum mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedChecksumMismatchContext {
    package: Box<str>,
    version: Version,
    url: Box<str>,
    commit: Box<str>,
}

impl LockedChecksumMismatchContext {
    /// The locked package identity.
    pub fn package(&self) -> &str {
        &self.package
    }

    /// The locked package version.
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// The canonical source URL from the lock.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The commit recorded in the lock.
    pub fn commit(&self) -> &str {
        &self.commit
    }
}

/// Errors while listing package release tags from a Git remote.
///
/// Transport details are intentionally not retained: underlying Git, proxy,
/// and credential errors can contain private URLs or authentication material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteTagListError {
    /// The URL is not a supported secure SAI Git URL.
    InvalidGitUrl,
    /// The package directory does not map to a release-tag namespace.
    InvalidPackageDirectory,
    /// A temporary repository could not be created or initialized.
    TemporaryRepository,
    /// The remote URL or refspec could not be configured.
    RemoteConfiguration,
    /// The remote did not provide a usable ref advertisement.
    FetchFailed,
    /// The ref advertisement contained invalid or over-limit release tags.
    TagList(GitReleaseTagListError),
}

impl GitRemoteTagListError {
    /// Return the user-facing diagnostic code for this failure.
    pub const fn diagnostic_code(&self) -> sai_diagnostics::Code {
        match self {
            Self::InvalidGitUrl => sai_diagnostics::Code::UnsupportedGitUrl,
            Self::InvalidPackageDirectory => sai_diagnostics::Code::InvalidPackagePath,
            Self::TemporaryRepository | Self::RemoteConfiguration | Self::FetchFailed => {
                sai_diagnostics::Code::FetchFailed
            }
            Self::TagList(GitReleaseTagListError::LimitExceeded { .. }) => {
                sai_diagnostics::Code::PackageTooLarge
            }
            Self::TagList(GitReleaseTagListError::InvalidPackageDirectory) => {
                sai_diagnostics::Code::InvalidPackagePath
            }
            Self::TagList(GitReleaseTagListError::ConflictingReleaseTag { .. }) => {
                sai_diagnostics::Code::FetchFailed
            }
        }
    }
}

impl fmt::Display for GitRemoteTagListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGitUrl => formatter.write_str("Git URL is not supported"),
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git package directory is not normalized")
            }
            Self::TemporaryRepository => {
                formatter.write_str("Could not prepare the temporary Git repository")
            }
            Self::RemoteConfiguration => formatter.write_str("Could not configure the Git remote"),
            Self::FetchFailed => formatter.write_str("Could not list Git release tags"),
            Self::TagList(error) => error.fmt(formatter),
        }
    }
}

impl StdError for GitRemoteTagListError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::TagList(error) => Some(error),
            _ => None,
        }
    }
}

/// A release selected by its Git release tag and verified against its source tree.
#[derive(Debug, Eq, PartialEq)]
pub struct FetchedGitRelease {
    tag_name: Box<str>,
    version: Version,
    tag_object_id: Option<gix::ObjectId>,
    tag_missing: bool,
    source_identity: GitPackageSourceIdentity,
    source: GitPackageSourceTree,
}

/// A package source tree fetched from an exact Git repository commit.
#[derive(Debug, Eq, PartialEq)]
pub struct FetchedGitPackage {
    source_identity: GitPackageSourceIdentity,
    source: GitPackageSourceTree,
}

mod git_package_source_view_sealed {
    pub trait Sealed {}
}

/// A fetched Git package source whose manifest is bound to its source identity.
pub trait GitPackageSourceView: git_package_source_view_sealed::Sealed {
    /// The immutable Git source identity that supplied this package.
    fn git_source_identity(&self) -> &GitPackageSourceIdentity;

    /// The parsed dependency-role package manifest from this source snapshot.
    fn git_package_manifest(&self) -> &PackageParseOutput;
}

/// The complete immutable identity of a package directory in a Git commit.
///
/// A Git-relative path pin shares the repository and commit while changing
/// only the normalized repository-relative directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitPackageSourceIdentity {
    canonical_url: Box<str>,
    commit_id: gix::ObjectId,
    directory: RepositoryPackageDirectory,
}

/// A Git-relative package path could not be resolved inside its repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitPackagePathError {
    /// The dependency path violates the portable path syntax.
    InvalidPackagePath,
    /// The normalized dependency path leaves the repository root.
    PathEscapesRepository,
}

impl GitPackagePathError {
    /// Return the package diagnostic for this path-resolution failure.
    pub const fn diagnostic_code(self) -> sai_diagnostics::Code {
        match self {
            Self::InvalidPackagePath => sai_diagnostics::Code::InvalidPackagePath,
            Self::PathEscapesRepository => sai_diagnostics::Code::PathEscapesRepository,
        }
    }
}

impl GitPackageSourceIdentity {
    /// The canonical repository URL.
    pub fn canonical_url(&self) -> &str {
        &self.canonical_url
    }

    /// The exact commit containing this package directory.
    pub const fn commit_id(&self) -> &gix::ObjectId {
        &self.commit_id
    }

    /// The normalized repository-relative package directory.
    pub const fn directory(&self) -> &RepositoryPackageDirectory {
        &self.directory
    }

    /// Resolve a dependency path against this package directory in the same
    /// commit, preserving repository and commit identity.
    pub fn resolve_dependency_path(&self, path: &str) -> Result<Self, GitPackagePathError> {
        let dependency_path = DependencyPackagePath::parse(path).map_err(map_git_path_error)?;
        let directory =
            normalize_git_relative_package_directory(Some(&self.directory), &dependency_path)
                .map_err(map_git_path_error)?;
        Ok(Self {
            canonical_url: self.canonical_url.clone(),
            commit_id: self.commit_id,
            directory,
        })
    }
}

fn map_git_path_error(error: PackagePathError) -> GitPackagePathError {
    match error {
        PackagePathError::InvalidPackagePath => GitPackagePathError::InvalidPackagePath,
        PackagePathError::PathEscapesRepository => GitPackagePathError::PathEscapesRepository,
    }
}

impl FetchedGitPackage {
    /// The canonical repository, commit, and directory that supplied this
    /// snapshot.
    pub const fn source_identity(&self) -> &GitPackageSourceIdentity {
        &self.source_identity
    }

    /// The canonical repository URL used to fetch the package.
    pub fn canonical_url(&self) -> &str {
        self.source_identity.canonical_url()
    }

    /// The exact repository commit that supplied this package.
    pub const fn commit_id(&self) -> &gix::ObjectId {
        self.source_identity.commit_id()
    }

    /// The normalized repository-relative package directory.
    pub const fn directory(&self) -> &RepositoryPackageDirectory {
        self.source_identity.directory()
    }

    /// The validated manifest and source files read from this commit.
    pub const fn source(&self) -> &GitPackageSourceTree {
        &self.source
    }

    /// The exact bytes of the `sai.package` file in this source snapshot.
    pub fn package_manifest_bytes(&self) -> &[u8] {
        self.source.package_manifest_bytes()
    }

    /// Consume the package and return its verified source snapshot.
    pub fn into_source(self) -> GitPackageSourceTree {
        self.source
    }

    /// Validate the fetched manifest and build its resolver contract.
    ///
    /// The declared package identity and version must equal `expected`.
    pub fn build_dependency_contract(
        &self,
        expected: &MvsPackageVersion,
        context: &ResolverContractContext<'_>,
        validation_context: PackageValidationContext<'_>,
    ) -> Result<ResolverContract, GitRemoteReleaseContractError> {
        build_git_source_dependency_contract(&self.source, expected, context, validation_context)
    }
}

impl GitPackageSourceView for FetchedGitPackage {
    fn git_source_identity(&self) -> &GitPackageSourceIdentity {
        self.source_identity()
    }

    fn git_package_manifest(&self) -> &PackageParseOutput {
        self.source.package_manifest()
    }
}

impl git_package_source_view_sealed::Sealed for FetchedGitPackage {}

impl FetchedGitRelease {
    /// The canonical repository, exact commit, and package directory that
    /// supplied this release.
    pub const fn source_identity(&self) -> &GitPackageSourceIdentity {
        &self.source_identity
    }

    /// The complete Git ref name that was fetched.
    pub fn tag_name(&self) -> &str {
        &self.tag_name
    }

    /// The package version encoded by the release tag.
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// The direct object ID fetched for the release tag, if the tag still exists.
    ///
    /// This is `None` when a locked commit was fetched after its release tag
    /// had been deleted.
    pub const fn tag_object_id(&self) -> Option<&gix::ObjectId> {
        self.tag_object_id.as_ref()
    }

    /// Whether the tag was missing and the locked commit was fetched directly.
    pub const fn tag_missing(&self) -> bool {
        self.tag_missing
    }

    /// The commit reached by peeling the release tag.
    pub const fn commit_id(&self) -> &gix::ObjectId {
        self.source_identity.commit_id()
    }

    /// The checksum and warnings for the selected package source tree.
    pub const fn source_tree(&self) -> &TreeHash {
        self.source.source_tree()
    }

    /// The WIT checksum and warnings, when the fetched manifest declares WIT.
    pub const fn wit_tree(&self) -> Option<&TreeHash> {
        self.source.wit_tree()
    }

    /// The verified artifact checksums, sorted by target bytes.
    pub fn artifact_checksums(&self) -> &[GitPackageArtifactChecksum] {
        self.source.artifact_checksums()
    }

    /// The dependency-role parse result for the fetched `sai.package`.
    ///
    /// Syntax diagnostics remain available here. Call
    /// [`Self::build_dependency_contract`] before using this release in
    /// dependency resolution.
    pub const fn package_manifest(&self) -> &PackageParseOutput {
        self.source.package_manifest()
    }

    /// The exact bytes of the `sai.package` file in this source snapshot.
    pub fn package_manifest_bytes(&self) -> &[u8] {
        self.source.package_manifest_bytes()
    }

    /// The exact selected source files, sorted by their package-relative paths.
    pub fn source_files(&self) -> &[GitPackageFile] {
        self.source.files()
    }

    /// Consume the release and return its verified source snapshot for caching.
    ///
    /// The snapshot keeps the checksum, parsed manifest, and selected files
    /// together so callers cannot associate one tree's files with another
    /// tree's content address.
    pub fn into_source_tree(self) -> GitPackageSourceTree {
        self.source
    }

    /// Validate the fetched manifest and build its resolver contract.
    ///
    /// The declared package identity and version must equal `expected`.
    pub fn build_dependency_contract(
        &self,
        expected: &MvsPackageVersion,
        context: &ResolverContractContext<'_>,
        validation_context: PackageValidationContext<'_>,
    ) -> Result<ResolverContract, GitRemoteReleaseContractError> {
        if self.version != *expected.version() {
            return Err(GitRemoteReleaseContractError::ReleaseTagVersionMismatch {
                expected: expected.clone(),
                tag_version: self.version.clone(),
            });
        }
        build_git_source_dependency_contract(&self.source, expected, context, validation_context)
    }
}

impl GitPackageSourceView for FetchedGitRelease {
    fn git_source_identity(&self) -> &GitPackageSourceIdentity {
        self.source_identity()
    }

    fn git_package_manifest(&self) -> &PackageParseOutput {
        self.package_manifest()
    }
}

impl git_package_source_view_sealed::Sealed for FetchedGitRelease {}

fn build_git_source_dependency_contract(
    source: &GitPackageSourceTree,
    expected: &MvsPackageVersion,
    context: &ResolverContractContext<'_>,
    validation_context: PackageValidationContext<'_>,
) -> Result<ResolverContract, GitRemoteReleaseContractError> {
    let contract = build_resolver_contract(source.package_manifest(), context, validation_context)
        .map_err(GitRemoteReleaseContractError::InvalidContract)?;
    let found = contract.mvs.package_version();
    if found != expected {
        return Err(GitRemoteReleaseContractError::PackageVersionMismatch {
            expected: Box::new(expected.clone()),
            found: Box::new(found.clone()),
        });
    }
    Ok(contract)
}

/// A fetched `sai.package` could not become the requested dependency contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteReleaseContractError {
    /// Parsing or semantic validation rejected the manifest.
    InvalidContract(ResolverContractBuildError),
    /// The fetched release tag encodes a different version than the resolver selected.
    ReleaseTagVersionMismatch {
        /// The package version selected by the resolver.
        expected: MvsPackageVersion,
        /// The version encoded by the fetched release tag.
        tag_version: Version,
    },
    /// The manifest declares a different identity or version than requested.
    PackageVersionMismatch {
        /// The package version requested by the resolver.
        expected: Box<MvsPackageVersion>,
        /// The identity and version declared by the fetched manifest.
        found: Box<MvsPackageVersion>,
    },
}

impl GitRemoteReleaseContractError {
    /// Return the stable diagnostic code when the failure has one direct code.
    ///
    /// Manifest syntax and semantic validation failures return `None` because
    /// callers should report the preserved, source-located diagnostics from
    /// [`Self::InvalidContract`] instead.
    pub const fn diagnostic_code(&self) -> Option<sai_diagnostics::Code> {
        match self {
            Self::InvalidContract(_) => None,
            Self::ReleaseTagVersionMismatch { .. } | Self::PackageVersionMismatch { .. } => {
                Some(sai_diagnostics::Code::PackageIdentityMismatch)
            }
        }
    }
}

impl fmt::Display for GitRemoteReleaseContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContract(error) => fmt::Display::fmt(error, formatter),
            Self::ReleaseTagVersionMismatch {
                expected,
                tag_version,
            } => write!(
                formatter,
                "Git release tag selects {}@{tag_version}, but the resolver requested {}@{}",
                expected.package(),
                expected.package(),
                expected.version()
            ),
            Self::PackageVersionMismatch { expected, found } => {
                write!(
                    formatter,
                    "Git package manifest declares {}@{}, expected {}@{}",
                    found.package(),
                    found.version(),
                    expected.package(),
                    expected.version()
                )
            }
        }
    }
}

impl StdError for GitRemoteReleaseContractError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::InvalidContract(error) => Some(error),
            Self::ReleaseTagVersionMismatch { .. } | Self::PackageVersionMismatch { .. } => None,
        }
    }
}

/// Errors while fetching a release or a commit-pinned Git package source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteReleaseFetchError {
    /// A package entry passed to the lock-aware fetch API has invalid fields.
    InvalidLockedPackageField(&'static str),
    /// The URL is not a supported secure SAI Git URL.
    InvalidGitUrl,
    /// The package directory or release tag does not belong to its namespace.
    InvalidPackageDirectory,
    /// A path source contains an invalid repository-relative dependency path.
    InvalidGitDependencyPath,
    /// A Git-relative path attempts to leave the requiring package's repository.
    PathEscapesRepository,
    /// A Git-relative package directory is absent or has no `sai.package`.
    PathPackageNotFound,
    /// The Git path fetcher returned a package from another source identity.
    GitPathSourceIdentityMismatch,
    /// A temporary repository could not be created or initialized.
    TemporaryRepository,
    /// The remote URL or refspec could not be configured.
    RemoteConfiguration,
    /// The remote did not provide a usable fetch response.
    FetchFailed,
    /// The response data for this fetch exceeded the fixed transfer budget.
    TransferLimitExceeded {
        /// The configured transfer limit in bytes.
        limit_bytes: u64,
    },
    /// The requested release tag was not advertised by the remote.
    TagNotFound {
        /// The complete missing Git ref name.
        name: Box<str>,
    },
    /// The release tag and its locked commit are both unavailable from the remote.
    LockedCommitUnavailable {
        /// The complete release-tag ref name.
        name: Box<str>,
        /// The commit recorded in the lock.
        commit: gix::ObjectId,
    },
    /// A package-source commit pinned by a Git sibling path is unavailable.
    RepositoryCommitUnavailable {
        /// The commit containing the sibling package.
        commit: gix::ObjectId,
    },
    /// A ref advertisement exceeded a bounded Git listing limit.
    TagList(GitReleaseTagListError),
    /// A release tag could not be peeled to a valid commit.
    InvalidTag(GitTreeReadError),
    /// A locked release tag now points to a different commit.
    TagMoved {
        /// The complete release-tag ref name.
        name: Box<str>,
        /// The commit recorded in the lock.
        expected: gix::ObjectId,
        /// The commit reached by the current release tag.
        actual: gix::ObjectId,
    },
    /// The selected package source tree could not be read or validated.
    SourceTree(GitTreeReadError),
    /// A source, WIT, or artifact checksum differs from the lock.
    ChecksumMismatch {
        /// Kind of checksum that differs.
        kind: GitPackageChecksumKind,
        /// The checksum recorded in the lock, or `<missing>`.
        expected: Box<str>,
        /// The checksum computed from the fetched commit, or `<missing>`.
        actual: Box<str>,
    },
    /// A locked checksum mismatch with the package and lock-entry context.
    LockedChecksumMismatch {
        /// Locked package, version, canonical source URL, and commit.
        context: Box<LockedChecksumMismatchContext>,
        /// Kind of checksum that differs.
        kind: GitPackageChecksumKind,
        /// The checksum recorded in the lock.
        expected: Box<str>,
        /// The checksum computed from the locked commit or cache metadata.
        actual: Box<str>,
    },
}

/// Failure to fetch the Git source selected for a concrete resolver request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectedGitReleaseFetchError {
    /// The resolver selected a path source or another non-Git source.
    NotGitSource,
    /// The selected Git URL is not supported.
    InvalidGitUrl,
    /// The selected package directory is invalid.
    InvalidPackageDirectory,
    /// A supplied lock package belongs to another package version.
    LockedPackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    /// The supplied lock package is not a valid Git release entry.
    InvalidLockedPackage(Box<GitRemoteReleaseFetchError>),
    /// A lock-floor selection has no validated lock package.
    MissingLockedPackage(Box<MvsPackageVersion>),
    /// The lock-floor source differs from the selected Git source.
    LockedSourceMismatch(Box<MvsPackageVersion>),
    /// Looking up the exact remote release tag failed.
    TagLookup(Box<GitRemoteTagListError>),
    /// The selected package version has no matching release tag.
    VersionNotFound(Box<MvsPackageVersion>),
    /// Fetching or validating the selected release failed.
    Fetch(Box<GitRemoteReleaseFetchError>),
}

impl SelectedGitReleaseFetchError {
    /// Return the user-facing diagnostic code when this error has one.
    pub const fn diagnostic_code(&self) -> Option<sai_diagnostics::Code> {
        match self {
            Self::NotGitSource => None,
            Self::InvalidGitUrl => Some(sai_diagnostics::Code::UnsupportedGitUrl),
            Self::InvalidPackageDirectory => Some(sai_diagnostics::Code::InvalidPackagePath),
            Self::LockedPackageVersionMismatch { .. }
            | Self::InvalidLockedPackage(_)
            | Self::MissingLockedPackage(_)
            | Self::LockedSourceMismatch(_) => Some(sai_diagnostics::Code::LockCorrupt),
            Self::TagLookup(error) => Some(error.diagnostic_code()),
            Self::VersionNotFound(_) => Some(sai_diagnostics::Code::VersionNotFound),
            Self::Fetch(error) => Some(error.diagnostic_code()),
        }
    }
}

impl fmt::Display for SelectedGitReleaseFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotGitSource => formatter.write_str("selected package source is not Git"),
            Self::InvalidGitUrl => formatter.write_str("selected Git URL is not supported"),
            Self::InvalidPackageDirectory => {
                formatter.write_str("selected Git package directory is invalid")
            }
            Self::LockedPackageVersionMismatch { expected, found } => write!(
                formatter,
                "lock entry is for {}@{}, expected {}@{}",
                found.package(),
                found.version(),
                expected.package(),
                expected.version()
            ),
            Self::InvalidLockedPackage(error) | Self::Fetch(error) => {
                fmt::Display::fmt(error, formatter)
            }
            Self::MissingLockedPackage(package_version) => write!(
                formatter,
                "lock entry for {}@{} is required by the selected lock floor",
                package_version.package(),
                package_version.version()
            ),
            Self::LockedSourceMismatch(package_version) => write!(
                formatter,
                "lock source for {}@{} does not match the selected Git source",
                package_version.package(),
                package_version.version()
            ),
            Self::TagLookup(error) => fmt::Display::fmt(error, formatter),
            Self::VersionNotFound(package_version) => write!(
                formatter,
                "no Git release tag was found for {}@{}",
                package_version.package(),
                package_version.version()
            ),
        }
    }
}

impl StdError for SelectedGitReleaseFetchError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::InvalidLockedPackage(error) | Self::Fetch(error) => Some(error),
            Self::TagLookup(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum SelectedGitReleaseFetchPlan<'a> {
    Locked(&'a LockPackage),
    Tag {
        url: Box<str>,
        directory: Option<RepositoryPackageDirectory>,
        package_version: MvsPackageVersion,
    },
}

fn plan_selected_git_release_fetch<'a>(
    package_version: &MvsPackageVersion,
    source: &ResolverPackageSource,
    reason: ResolverSourceReason,
    locked_package: Option<&'a LockPackage>,
) -> Result<SelectedGitReleaseFetchPlan<'a>, SelectedGitReleaseFetchError> {
    let ResolverPackageSource::Git { url, directory } = source else {
        return Err(SelectedGitReleaseFetchError::NotGitSource);
    };
    let canonical_url =
        canonical_git_url(url).map_err(|()| SelectedGitReleaseFetchError::InvalidGitUrl)?;
    let directory = directory
        .as_deref()
        .map(RepositoryPackageDirectory::parse)
        .transpose()
        .map_err(|_| SelectedGitReleaseFetchError::InvalidPackageDirectory)?;

    let matching_lock = if let Some(locked_package) = locked_package {
        let found = MvsPackageVersion::new(
            locked_package.package.to_string(),
            locked_package.version.clone(),
        )
        .map_err(|_| {
            SelectedGitReleaseFetchError::InvalidLockedPackage(Box::new(
                GitRemoteReleaseFetchError::InvalidLockedPackageField("package identity"),
            ))
        })?;
        if &found != package_version {
            return Err(SelectedGitReleaseFetchError::LockedPackageVersionMismatch {
                expected: Box::new(package_version.clone()),
                found: Box::new(found),
            });
        }

        let has_git_source = locked_package
            .fields
            .iter()
            .any(|field| matches!(&field.value, LockPackageField::Source(LockSource::Git(_))));
        if has_git_source {
            let locked = locked_release_request(locked_package).map_err(|error| {
                SelectedGitReleaseFetchError::InvalidLockedPackage(Box::new(error))
            })?;
            let same_directory = locked
                .directory
                .as_ref()
                .map_or(".", RepositoryPackageDirectory::as_str)
                == directory
                    .as_ref()
                    .map_or(".", RepositoryPackageDirectory::as_str);
            (locked.url.as_ref() == canonical_url && same_directory).then_some(locked_package)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(locked_package) = matching_lock {
        return Ok(SelectedGitReleaseFetchPlan::Locked(locked_package));
    }
    if reason == ResolverSourceReason::LockFloor {
        return Err(if locked_package.is_some() {
            SelectedGitReleaseFetchError::LockedSourceMismatch(Box::new(package_version.clone()))
        } else {
            SelectedGitReleaseFetchError::MissingLockedPackage(Box::new(package_version.clone()))
        });
    }

    Ok(SelectedGitReleaseFetchPlan::Tag {
        url: canonical_url.into(),
        directory,
        package_version: package_version.clone(),
    })
}

/// Fetch the Git release selected for one concrete MVS request.
///
/// A matching validated lock entry is fetched by commit and checksum. A
/// lock-floor selection requires that entry; other selections without a
/// matching entry look up and fetch only the exact version tag. Path
/// selections are handled by the exact-commit sibling source API.
pub fn fetch_selected_git_release(
    selection: &ResolverSourceSelection,
    locked_package: Option<&LockPackage>,
) -> Result<FetchedGitRelease, SelectedGitReleaseFetchError> {
    match plan_selected_git_release_fetch(
        selection.package_version(),
        selection.source(),
        selection.reason(),
        locked_package,
    )? {
        SelectedGitReleaseFetchPlan::Locked(package) => fetch_locked_package_tree(package)
            .map_err(|error| SelectedGitReleaseFetchError::Fetch(Box::new(error))),
        SelectedGitReleaseFetchPlan::Tag {
            url,
            directory,
            package_version,
        } => {
            let tag = find_remote_release_tag(&url, package_version.version(), directory.as_ref())
                .map_err(|error| SelectedGitReleaseFetchError::TagLookup(Box::new(error)))?;
            let tag = tag.ok_or_else(|| {
                SelectedGitReleaseFetchError::VersionNotFound(Box::new(package_version))
            })?;
            fetch_remote_release_tree(&url, &tag, directory.as_ref(), None, None)
                .map_err(|error| SelectedGitReleaseFetchError::Fetch(Box::new(error)))
        }
    }
}

/// Why a locked package source was obtained from the package cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockedPackageCacheStatus {
    /// All locked checksums and the cached source tree were verified locally.
    Cached,
    /// The locked commit was fetched and its verified files were installed.
    Fetched,
    /// A corrupt source entry was removed before the locked commit was fetched.
    RepairedCorrupt {
        /// The checksum computed from the corrupt entry, when its files were hashable.
        actual_checksum: Option<Box<str>>,
    },
}

/// A locked package source that is present and checksum-verified in the cache.
#[derive(Debug, Eq, PartialEq)]
pub struct CachedLockedPackage {
    source: CachedPackageSource,
    status: LockedPackageCacheStatus,
    tag_missing: bool,
}

impl CachedLockedPackage {
    /// The immutable cached source directory.
    pub const fn source(&self) -> &CachedPackageSource {
        &self.source
    }

    /// Whether this call used verified cached files, fetched, or repaired corruption.
    pub const fn status(&self) -> &LockedPackageCacheStatus {
        &self.status
    }

    /// Whether the remote release tag was missing during a network fetch.
    pub const fn tag_missing(&self) -> bool {
        self.tag_missing
    }
}

/// Details needed to report the `CACHE_CORRUPT` warning for a removed source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheCorruptionNotice {
    expected_checksum: Box<str>,
    actual_checksum: Option<Box<str>>,
}

impl CacheCorruptionNotice {
    /// The checksum used to locate the removed entry.
    pub fn expected_checksum(&self) -> &str {
        &self.expected_checksum
    }

    /// The checksum computed from the corrupt tree, when it could be hashed.
    pub fn actual_checksum(&self) -> Option<&str> {
        self.actual_checksum.as_deref()
    }
}

/// A failure while obtaining a locked package in the content cache.
#[derive(Debug)]
pub enum LockedPackageCacheFetchError {
    /// The locked release could not be fetched or verified.
    Fetch(GitRemoteReleaseFetchError),
    /// The package cache could not be read or updated.
    Cache(PackageCacheError),
    /// The lock's verified package content is not available without network access.
    OfflineFetchRequired {
        /// Package identity and locked version.
        package: Box<str>,
        /// Canonical source URL from the lock.
        url: Box<str>,
        /// Commit recorded in the lock.
        commit: Box<str>,
    },
    /// The source entry was removed as corrupt before the underlying failure.
    CorruptSourceObserved {
        /// Information for the `CACHE_CORRUPT` warning.
        notice: CacheCorruptionNotice,
        /// The offline, fetch, or cache failure that followed removal.
        cause: Box<LockedPackageCacheFetchError>,
    },
}

impl fmt::Display for LockedPackageCacheFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => fmt::Display::fmt(error, formatter),
            Self::Cache(error) => fmt::Display::fmt(error, formatter),
            Self::OfflineFetchRequired {
                package,
                url,
                commit,
            } => write!(
                formatter,
                "locked package {package} from {url} at commit {commit} is not available in the verified cache"
            ),
            Self::CorruptSourceObserved { cause, .. } => fmt::Display::fmt(cause, formatter),
        }
    }
}

impl StdError for LockedPackageCacheFetchError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Fetch(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::CorruptSourceObserved { cause, .. } => Some(cause),
            Self::OfflineFetchRequired { .. } => None,
        }
    }
}

impl LockedPackageCacheFetchError {
    /// Return the warning context when a corrupt cache entry was removed.
    pub fn cache_corruption_notice(&self) -> Option<&CacheCorruptionNotice> {
        match self {
            Self::CorruptSourceObserved { notice, .. } => Some(notice),
            _ => None,
        }
    }

    /// Return a stable diagnostic code when this failure has one.
    pub const fn diagnostic_code(&self) -> Option<sai_diagnostics::Code> {
        match self {
            Self::Fetch(error) => Some(error.diagnostic_code()),
            Self::Cache(_) => None,
            Self::OfflineFetchRequired { .. } => Some(sai_diagnostics::Code::OfflineFetchRequired),
            Self::CorruptSourceObserved { cause, .. } => cause.diagnostic_code(),
        }
    }
}

impl GitRemoteReleaseFetchError {
    /// Return the user-facing diagnostic code for this failure.
    pub const fn diagnostic_code(&self) -> sai_diagnostics::Code {
        match self {
            Self::InvalidLockedPackageField(_) => sai_diagnostics::Code::LockCorrupt,
            Self::InvalidGitUrl => sai_diagnostics::Code::UnsupportedGitUrl,
            Self::InvalidPackageDirectory => sai_diagnostics::Code::InvalidPackagePath,
            Self::InvalidGitDependencyPath => sai_diagnostics::Code::InvalidPackagePath,
            Self::PathEscapesRepository => sai_diagnostics::Code::PathEscapesRepository,
            Self::PathPackageNotFound => sai_diagnostics::Code::PathPackageNotFound,
            Self::GitPathSourceIdentityMismatch => sai_diagnostics::Code::FetchFailed,
            Self::TemporaryRepository | Self::RemoteConfiguration | Self::FetchFailed => {
                sai_diagnostics::Code::FetchFailed
            }
            Self::TransferLimitExceeded { .. } => sai_diagnostics::Code::PackageTooLarge,
            Self::TagNotFound { .. } | Self::LockedCommitUnavailable { .. } => {
                sai_diagnostics::Code::VersionNotFound
            }
            Self::RepositoryCommitUnavailable { .. } => sai_diagnostics::Code::FetchFailed,
            Self::TagList(GitReleaseTagListError::LimitExceeded { .. }) => {
                sai_diagnostics::Code::PackageTooLarge
            }
            Self::TagList(GitReleaseTagListError::InvalidPackageDirectory) => {
                sai_diagnostics::Code::InvalidPackagePath
            }
            Self::TagList(GitReleaseTagListError::ConflictingReleaseTag { .. }) => {
                sai_diagnostics::Code::FetchFailed
            }
            Self::InvalidTag(GitTreeReadError::PackageTooLarge { .. })
            | Self::SourceTree(GitTreeReadError::PackageTooLarge { .. }) => {
                sai_diagnostics::Code::PackageTooLarge
            }
            Self::InvalidTag(_) => sai_diagnostics::Code::InvalidTag,
            Self::TagMoved { .. } => sai_diagnostics::Code::TagMoved,
            Self::SourceTree(GitTreeReadError::InvalidPackageDirectory) => {
                sai_diagnostics::Code::InvalidPackagePath
            }
            Self::SourceTree(GitTreeReadError::Hash(TreeHashError::PathCollision { .. })) => {
                sai_diagnostics::Code::PathCollision
            }
            Self::SourceTree(GitTreeReadError::MissingPackageDirectory)
            | Self::SourceTree(GitTreeReadError::MissingPackageManifest)
            | Self::SourceTree(GitTreeReadError::PackagePathNotDirectory) => {
                sai_diagnostics::Code::MissingPackageFile
            }
            Self::SourceTree(GitTreeReadError::WitDirectoryNotFound { .. }) => {
                sai_diagnostics::Code::WitDirNotFound
            }
            Self::SourceTree(GitTreeReadError::ArtifactNotFound { .. }) => {
                sai_diagnostics::Code::ArtifactNotFound
            }
            Self::SourceTree(GitTreeReadError::Hash(_))
            | Self::SourceTree(GitTreeReadError::InvalidTreeEntry) => {
                sai_diagnostics::Code::InvalidPackageFilePath
            }
            Self::SourceTree(_) => sai_diagnostics::Code::FetchFailed,
            Self::ChecksumMismatch { .. } => sai_diagnostics::Code::ChecksumMismatch,
            Self::LockedChecksumMismatch { .. } => sai_diagnostics::Code::ChecksumMismatch,
        }
    }
}

impl fmt::Display for GitRemoteReleaseFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLockedPackageField(field) => {
                write!(formatter, "lock package has an invalid {field} field")
            }
            Self::InvalidGitUrl => formatter.write_str("Git URL is not supported"),
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git release tag is outside the package tag namespace")
            }
            Self::InvalidGitDependencyPath => {
                formatter.write_str("Git path package dependency path is invalid")
            }
            Self::PathEscapesRepository => {
                formatter.write_str("Git path package leaves its repository")
            }
            Self::PathPackageNotFound => {
                formatter.write_str("Git path package was not found in the repository commit")
            }
            Self::GitPathSourceIdentityMismatch => {
                formatter.write_str("Git path fetch returned a package from another source")
            }
            Self::TemporaryRepository => {
                formatter.write_str("Could not prepare the temporary Git repository")
            }
            Self::RemoteConfiguration => formatter.write_str("Could not configure the Git remote"),
            Self::FetchFailed => formatter.write_str("Could not fetch the Git source"),
            Self::TransferLimitExceeded { limit_bytes } => write!(
                formatter,
                "Git fetch exceeded the {limit_bytes}-byte transfer limit"
            ),
            Self::TagNotFound { name } => write!(formatter, "Git release tag {name} was not found"),
            Self::LockedCommitUnavailable { name, commit } => write!(
                formatter,
                "Git release tag {name} is missing and locked commit {commit} is unavailable from the source"
            ),
            Self::RepositoryCommitUnavailable { commit } => {
                write!(
                    formatter,
                    "Git repository commit {commit} is unavailable from the source"
                )
            }
            Self::TagList(error) => error.fmt(formatter),
            Self::InvalidTag(error) => write!(formatter, "invalid Git release tag: {error}"),
            Self::TagMoved {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "Git release tag {name} moved from locked commit {expected} to {actual}"
            ),
            Self::SourceTree(error) => {
                write!(formatter, "could not read Git package tree: {error}")
            }
            Self::ChecksumMismatch {
                kind,
                expected,
                actual,
            } => write!(
                formatter,
                "Git package {kind} checksum mismatch (expected {expected}, got {actual})"
            ),
            Self::LockedChecksumMismatch {
                context,
                kind,
                expected,
                actual,
            } => write!(
                formatter,
                "locked package {}@{} from {} at commit {} has a {kind} checksum mismatch (expected {expected}, got {actual})",
                context.package, context.version, context.url, context.commit
            ),
        }
    }
}

impl StdError for GitRemoteReleaseFetchError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::TagList(error) => Some(error),
            Self::InvalidTag(error) | Self::SourceTree(error) => Some(error),
            _ => None,
        }
    }
}

/// List canonical release tags for a secure Git URL without fetching objects.
///
/// The remote URL is canonicalized before use. Only credential-helper settings
/// from trusted user-level Git configuration and the documented HTTPS proxy
/// variables are passed to the isolated transport. Other Git configuration
/// and environment-based Git overrides are excluded.
pub fn list_remote_release_tags(
    url: &str,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<Vec<GitReleaseTag>, GitRemoteTagListError> {
    let refspec = release_tag_refspec(directory)?;
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteTagListError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides()?;
    let scratch =
        TemporaryGitDirectory::create().map_err(|()| GitRemoteTagListError::TemporaryRepository)?;
    let open_options = gix::open::Options::isolated().config_overrides(config_overrides);
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        open_options,
    )
    .map_err(|_| GitRemoteTagListError::TemporaryRepository)?;
    let repository = repository.to_thread_local();

    list_release_tags_in_repository(&repository, &canonical_url, &refspec, directory)
}

fn find_remote_release_tag(
    url: &str,
    version: &Version,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<Option<GitReleaseTag>, GitRemoteTagListError> {
    let tag_name = release_tag_name(version, directory)
        .map_err(|_| GitRemoteTagListError::InvalidPackageDirectory)?;
    let refspec = exact_release_tag_refspec_for_name(&tag_name);
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteTagListError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides()?;
    let scratch =
        TemporaryGitDirectory::create().map_err(|()| GitRemoteTagListError::TemporaryRepository)?;
    let open_options = gix::open::Options::isolated().config_overrides(config_overrides);
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        open_options,
    )
    .map_err(|_| GitRemoteTagListError::TemporaryRepository)?;
    let repository = repository.to_thread_local();

    find_release_tag_in_repository(
        &repository,
        &canonical_url,
        &refspec,
        &tag_name,
        version,
        directory,
    )
}

/// Fetch one release tag, peel it to a commit, and verify its selected source tree.
///
/// The exact ref is fetched with depth one and no implicit tag following. If
/// `expected_commit` is provided, the peeled commit must still match the lock.
/// If `expected_source_checksum` is provided, the computed `sai1:` source
/// checksum must also match the lock. If the tag is absent and a locked commit
/// is provided, that commit is fetched directly; [`FetchedGitRelease::tag_missing`]
/// reports that the caller should emit the `TAG_MISSING` warning.
pub fn fetch_remote_release_tree(
    url: &str,
    tag: &GitReleaseTag,
    directory: Option<&RepositoryPackageDirectory>,
    expected_commit: Option<&gix::ObjectId>,
    expected_source_checksum: Option<&str>,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let refspec = exact_release_tag_refspec(tag, directory)?;
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteReleaseFetchError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides().map_err(map_tag_list_error_for_fetch)?;
    let scratch = TemporaryGitDirectory::create()
        .map_err(|()| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options {
            object_hash: Some(tag.object_id().kind()),
            ..Default::default()
        },
        gix::open::Options::isolated().config_overrides(config_overrides),
    )
    .map_err(|_| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = repository.to_thread_local();
    let transfer_limit =
        gix_transport::client::blocking_io::FetchTransferLimit::new(MAX_GIT_FETCH_BYTES);

    fetch_release_tree_in_repository(
        &repository,
        ReleaseFetchRequest {
            source_url: GitSourceUrl::canonical(&canonical_url),
            refspec: &refspec,
            tag_name: tag.name(),
            version: tag.version(),
            directory,
            expected_commit,
            expected_source_checksum,
            expected_locked_checksums: None,
            transfer_limit: &transfer_limit,
        },
    )
}

/// Fetch a lock-selected release even when its tag disappeared before this call.
///
/// The release ref is derived from the locked version and package directory,
/// and the temporary repository uses the locked commit's object-hash kind.
/// The commit and all lock-recorded checksums are mandatory so both the tag
/// path and the deleted-tag fallback are verified before the release is returned.
pub fn fetch_locked_release_tree(
    url: &str,
    version: &Version,
    directory: Option<&RepositoryPackageDirectory>,
    locked_commit: &gix::ObjectId,
    expected_checksums: &ExpectedPackageChecksums,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteReleaseFetchError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides().map_err(map_tag_list_error_for_fetch)?;
    let scratch = TemporaryGitDirectory::create()
        .map_err(|()| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options {
            object_hash: Some(locked_commit.kind()),
            ..Default::default()
        },
        gix::open::Options::isolated().config_overrides(config_overrides),
    )
    .map_err(|_| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = repository.to_thread_local();
    let transfer_limit =
        gix_transport::client::blocking_io::FetchTransferLimit::new(MAX_GIT_FETCH_BYTES);

    fetch_locked_release_tree_in_repository(
        &repository,
        GitSourceUrl::canonical(&canonical_url),
        version,
        directory,
        locked_commit,
        expected_checksums,
        &transfer_limit,
    )
}

/// Fetch a package directory from an exact Git commit without consulting a
/// release tag.
///
/// This is used for sibling packages published in the same repository commit.
/// The caller supplies the requiring package's selected commit (which is
/// locked when resolving an existing lock) and a normalized repository-relative
/// directory; the selected tree and its manifest are validated with the same
/// bounded Git object reader used for releases.
pub fn fetch_git_package_source_tree_at_commit(
    url: &str,
    commit_id: &gix::ObjectId,
    directory: &RepositoryPackageDirectory,
) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError> {
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteReleaseFetchError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides().map_err(map_tag_list_error_for_fetch)?;
    let scratch = TemporaryGitDirectory::create()
        .map_err(|()| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options {
            object_hash: Some(commit_id.kind()),
            ..Default::default()
        },
        gix::open::Options::isolated().config_overrides(config_overrides),
    )
    .map_err(|_| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = repository.to_thread_local();
    let transfer_limit =
        gix_transport::client::blocking_io::FetchTransferLimit::new(MAX_GIT_FETCH_BYTES);

    fetch_git_package_source_tree_at_commit_in_repository(
        &repository,
        &canonical_url,
        commit_id,
        directory,
        &transfer_limit,
    )
}

/// Fetch a Git path package from its exact identity and verify a matching lock
/// entry's commit and all recorded checksums.
///
/// The lock entry must describe the exact repository, commit, and package
/// directory in `source_identity`. A stale entry for another identity should
/// be ignored by the resolver and the current source fetched without a lock.
pub fn fetch_locked_git_path_package_tree(
    package: &LockPackage,
    source_identity: &GitPackageSourceIdentity,
) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError> {
    let request = locked_git_path_request(package, source_identity)?;
    let fetched = fetch_git_package_source_tree_at_commit(
        source_identity.canonical_url(),
        source_identity.commit_id(),
        source_identity.directory(),
    )?;
    verify_locked_git_path_package_checksums(&request, source_identity, &fetched)?;
    Ok(fetched)
}

fn locked_git_path_request(
    package: &LockPackage,
    source_identity: &GitPackageSourceIdentity,
) -> Result<LockedReleaseRequest, GitRemoteReleaseFetchError> {
    let request = locked_release_request(package)?;
    let same_directory = request
        .directory
        .as_ref()
        .map_or(".", RepositoryPackageDirectory::as_str)
        == source_identity.directory().as_str();
    if request.url.as_ref() != source_identity.canonical_url()
        || request.commit != *source_identity.commit_id()
        || !same_directory
    {
        return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
            "Git path source identity",
        ));
    }
    Ok(request)
}

#[cfg(test)]
fn verify_locked_git_path_package(
    package: &LockPackage,
    source_identity: &GitPackageSourceIdentity,
    fetched: &FetchedGitPackage,
) -> Result<(), GitRemoteReleaseFetchError> {
    let request = locked_git_path_request(package, source_identity)?;
    verify_locked_git_path_package_checksums(&request, source_identity, fetched)
}

fn verify_locked_git_path_package_checksums(
    request: &LockedReleaseRequest,
    source_identity: &GitPackageSourceIdentity,
    fetched: &FetchedGitPackage,
) -> Result<(), GitRemoteReleaseFetchError> {
    verify_git_path_package_identity(source_identity, fetched)?;

    verify_cached_package_checksums(&request.checksums, &package_checksums(fetched.source()))
        .map_err(|error| with_locked_checksum_context(request, error))?;
    Ok(())
}

pub(crate) fn verify_git_path_package_identity(
    source_identity: &GitPackageSourceIdentity,
    fetched: &FetchedGitPackage,
) -> Result<(), GitRemoteReleaseFetchError> {
    if fetched.source_identity() != source_identity {
        return Err(GitRemoteReleaseFetchError::GitPathSourceIdentityMismatch);
    }
    Ok(())
}

/// Find a lock package that describes the exact Git path source identity.
///
/// A package's manifest version is checked later when its dependency contract
/// is built. Path sources are pinned by their requiring commit, so entries
/// for an older commit are stale and do not provide a checksum floor.
pub(crate) fn matching_locked_git_path_package_from_iter<'a>(
    source_identity: &GitPackageSourceIdentity,
    locked_packages: impl IntoIterator<Item = &'a LockPackage>,
) -> Result<Option<&'a LockPackage>, GitRemoteReleaseFetchError> {
    let mut matching = None;
    for package in locked_packages {
        if !package
            .fields
            .iter()
            .any(|field| matches!(&field.value, LockPackageField::Source(LockSource::Git(_))))
        {
            continue;
        }
        let request = locked_release_request(package)?;
        let same_directory = request
            .directory
            .as_ref()
            .map_or(".", RepositoryPackageDirectory::as_str)
            == source_identity.directory().as_str();
        if request.url.as_ref() == source_identity.canonical_url()
            && request.commit == *source_identity.commit_id()
            && same_directory
            && matching.replace(package).is_some()
        {
            return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                "duplicate Git path source identity",
            ));
        }
    }
    Ok(matching)
}

/// Find the lock entry for the exact release source selected for one package
/// version. A different source for that version is not a match.
pub(crate) fn matching_locked_git_release_package_from_iter<'a>(
    package_version: &MvsPackageVersion,
    source: &ResolverPackageSource,
    locked_packages: impl IntoIterator<Item = &'a LockPackage>,
) -> Result<Option<&'a LockPackage>, GitRemoteReleaseFetchError> {
    let ResolverPackageSource::Git { url, directory } = source else {
        return Ok(None);
    };
    let canonical_url =
        canonical_git_url(url).map_err(|_| GitRemoteReleaseFetchError::InvalidGitUrl)?;
    let directory = directory
        .as_deref()
        .map(RepositoryPackageDirectory::parse)
        .transpose()
        .map_err(|_| GitRemoteReleaseFetchError::InvalidPackageDirectory)?;

    let mut matching = None;
    for package in locked_packages {
        if package.package.as_ref() != package_version.package()
            || package.version != *package_version.version()
        {
            continue;
        }
        if !package
            .fields
            .iter()
            .any(|field| matches!(&field.value, LockPackageField::Source(LockSource::Git(_))))
        {
            continue;
        }
        let request = locked_release_request(package)?;
        let same_directory = request
            .directory
            .as_ref()
            .map_or(".", RepositoryPackageDirectory::as_str)
            == directory
                .as_ref()
                .map_or(".", RepositoryPackageDirectory::as_str);
        if request.url.as_ref() == canonical_url
            && same_directory
            && matching.replace(package).is_some()
        {
            return Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                "duplicate Git release source identity",
            ));
        }
    }
    Ok(matching)
}

/// Fetch a sibling package by resolving a dependency path inside the exact
/// Git commit that supplied its requiring package.
///
/// The path is joined to the requiring package's normalized repository
/// directory. The resulting package is fetched at the same commit without
/// consulting a release tag. A repository escape is reported as
/// `PATH_ESCAPES_REPOSITORY`.
pub fn fetch_git_path_package_source_tree(
    requiring_source: &GitPackageSourceIdentity,
    dependency_path: &str,
) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError> {
    let source = resolve_git_path_package_identity(requiring_source, dependency_path)?;
    let config_overrides = transport_config_overrides().map_err(map_tag_list_error_for_fetch)?;
    let scratch = TemporaryGitDirectory::create()
        .map_err(|()| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options {
            object_hash: Some(source.commit_id.kind()),
            ..Default::default()
        },
        gix::open::Options::isolated().config_overrides(config_overrides),
    )
    .map_err(|_| GitRemoteReleaseFetchError::TemporaryRepository)?;
    let repository = repository.to_thread_local();
    let transfer_limit =
        gix_transport::client::blocking_io::FetchTransferLimit::new(MAX_GIT_FETCH_BYTES);

    fetch_git_package_source_identity_in_repository(&repository, &source, &transfer_limit)
}

fn resolve_git_path_package_identity(
    requiring_source: &GitPackageSourceIdentity,
    dependency_path: &str,
) -> Result<GitPackageSourceIdentity, GitRemoteReleaseFetchError> {
    requiring_source
        .resolve_dependency_path(dependency_path)
        .map_err(|error| match error {
            GitPackagePathError::InvalidPackagePath => {
                GitRemoteReleaseFetchError::InvalidGitDependencyPath
            }
            GitPackagePathError::PathEscapesRepository => {
                GitRemoteReleaseFetchError::PathEscapesRepository
            }
        })
}

fn fetch_git_package_source_identity_in_repository(
    repository: &gix::Repository,
    source: &GitPackageSourceIdentity,
    transfer_limit: &gix_transport::client::blocking_io::FetchTransferLimit,
) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError> {
    fetch_git_package_source_tree_at_commit_in_repository(
        repository,
        source.canonical_url(),
        source.commit_id(),
        source.directory(),
        transfer_limit,
    )
    .map_err(|error| match error {
        GitRemoteReleaseFetchError::SourceTree(
            GitTreeReadError::MissingPackageDirectory
            | GitTreeReadError::MissingPackageManifest
            | GitTreeReadError::PackagePathNotDirectory,
        ) => GitRemoteReleaseFetchError::PathPackageNotFound,
        error => error,
    })
}

fn fetch_git_package_source_tree_at_commit_in_repository(
    repository: &gix::Repository,
    canonical_url: &str,
    commit_id: &gix::ObjectId,
    directory: &RepositoryPackageDirectory,
    transfer_limit: &gix_transport::client::blocking_io::FetchTransferLimit,
) -> Result<FetchedGitPackage, GitRemoteReleaseFetchError> {
    fetch_locked_commit(
        repository,
        canonical_url,
        commit_id,
        LockedCommitFetchContext::RepositoryPackage,
        transfer_limit,
    )?;
    let reader = GitTreeReader::open(
        repository.git_dir().join("objects"),
        repository.object_hash(),
    )
    .map_err(GitRemoteReleaseFetchError::SourceTree)?;
    let source = reader
        .read_commit_repository_package_source_tree(commit_id, directory)
        .map_err(GitRemoteReleaseFetchError::SourceTree)?;
    Ok(FetchedGitPackage {
        source_identity: GitPackageSourceIdentity {
            canonical_url: canonical_url.into(),
            commit_id: *commit_id,
            directory: directory.clone(),
        },
        source,
    })
}

/// Fetch one Git package directly from its validated lock entry.
///
/// The caller must run lock syntax and semantic validation first. The fetched
/// commit, source tree, WIT tree, and complete artifact checksum set are
/// verified before the release is returned.
pub fn fetch_locked_package_tree(
    package: &LockPackage,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let request = locked_release_request(package)?;
    fetch_locked_release_tree(
        &request.url,
        &request.version,
        request.directory.as_ref(),
        &request.commit,
        &request.checksums,
    )
    .map_err(|error| with_locked_checksum_context(&request, error))
}

fn with_locked_checksum_context(
    request: &LockedReleaseRequest,
    error: GitRemoteReleaseFetchError,
) -> GitRemoteReleaseFetchError {
    match error {
        GitRemoteReleaseFetchError::ChecksumMismatch {
            kind,
            expected,
            actual,
        } => GitRemoteReleaseFetchError::LockedChecksumMismatch {
            context: Box::new(LockedChecksumMismatchContext {
                package: request.package.clone(),
                version: request.version.clone(),
                url: request.url.clone(),
                commit: request.commit.to_hex().to_string().into(),
            }),
            kind,
            expected,
            actual,
        },
        error => error,
    }
}

/// Verify a locked package from cache, or fetch and materialize it when absent.
///
/// `offline` forbids the network. A corrupt source entry is removed before this
/// function either reports an offline miss or fetches the locked commit again.
/// Callers must run full lock syntax and semantic validation first.
pub fn fetch_locked_package_to_cache(
    package: &LockPackage,
    cache: &PackageCache,
    offline: bool,
) -> Result<CachedLockedPackage, LockedPackageCacheFetchError> {
    let request = locked_release_request(package).map_err(LockedPackageCacheFetchError::Fetch)?;
    let locked_commit = request.commit.to_hex().to_string();
    // A validated source-only lock entry has no content hashes beyond the
    // source tree hash, so the verified source entry is sufficient even when
    // the auxiliary Git checksum record predates this cache format or is gone.
    let source_checksum_is_the_only_locked_hash =
        request.checksums.wit_checksum.is_none() && request.checksums.artifact_checksums.is_empty();
    let directory = request
        .directory
        .as_ref()
        .map(RepositoryPackageDirectory::as_str);
    let cached_checksums =
        match cache.cached_package_checksums(&request.url, &locked_commit, directory) {
            Ok(checksums) => checksums,
            // A malformed checksum record is a miss and can be rebuilt after a
            // fresh Git fetch verifies the lock.
            Err(PackageCacheError::CorruptRecord(_)) => None,
            Err(error) => return Err(LockedPackageCacheFetchError::Cache(error)),
        };
    if let Some(cached_checksums) = &cached_checksums {
        verify_cached_package_checksums(&request.checksums, cached_checksums)
            .map_err(|error| with_locked_checksum_context(&request, error))
            .map_err(LockedPackageCacheFetchError::Fetch)?;
    }
    let mut corruption_notice = None;

    match cache
        .verify_source(&request.checksums.source_checksum)
        .map_err(LockedPackageCacheFetchError::Cache)?
    {
        CachedSourceVerification::Verified(source) if cached_checksums.is_some() => {
            return Ok(CachedLockedPackage {
                source,
                status: LockedPackageCacheStatus::Cached,
                tag_missing: false,
            });
        }
        CachedSourceVerification::Verified(source)
            if source_checksum_is_the_only_locked_hash
                && cached_source_has_no_optional_checksums(&source) =>
        {
            return Ok(CachedLockedPackage {
                source,
                status: LockedPackageCacheStatus::Cached,
                tag_missing: false,
            });
        }
        CachedSourceVerification::Corrupt {
            actual_checksum, ..
        } => {
            match cache
                .remove_corrupt_source(&request.checksums.source_checksum)
                .map_err(LockedPackageCacheFetchError::Cache)?
            {
                SourceCacheRemoval::Removed | SourceCacheRemoval::Missing => {
                    corruption_notice = Some(CacheCorruptionNotice {
                        expected_checksum: request.checksums.source_checksum.clone(),
                        actual_checksum,
                    });
                }
                SourceCacheRemoval::Valid => {
                    if let Some(source) =
                        find_verified_source(cache, &request.checksums.source_checksum)?
                        && (cached_checksums.is_some()
                            || (source_checksum_is_the_only_locked_hash
                                && cached_source_has_no_optional_checksums(&source)))
                    {
                        return Ok(CachedLockedPackage {
                            source,
                            status: LockedPackageCacheStatus::Cached,
                            tag_missing: false,
                        });
                    }
                }
            }
        }
        CachedSourceVerification::Missing | CachedSourceVerification::Verified(_) => {}
    }

    if offline {
        return Err(attach_corruption_notice(
            LockedPackageCacheFetchError::OfflineFetchRequired {
                package: format!("{}@{}", package.package, package.version).into(),
                url: request.url,
                commit: request.commit.to_hex().to_string().into(),
            },
            corruption_notice.as_ref(),
        ));
    }

    let release = fetch_locked_release_tree(
        &request.url,
        &request.version,
        request.directory.as_ref(),
        &request.commit,
        &request.checksums,
    )
    .map_err(|error| with_locked_checksum_context(&request, error))
    .map_err(LockedPackageCacheFetchError::Fetch)
    .map_err(|error| attach_corruption_notice(error, corruption_notice.as_ref()))?;
    let tag_name = release.tag_name().to_owned();
    let tag_missing = release.tag_missing();
    let commit = release.commit_id().to_hex().to_string();
    let source_checksum = request.checksums.source_checksum.clone();
    let source_tree = release.into_source_tree();
    cache
        .record_verified_package_checksums(&request.url, &commit, directory, &source_tree)
        .map_err(LockedPackageCacheFetchError::Cache)
        .map_err(|error| attach_corruption_notice(error, corruption_notice.as_ref()))?;
    let source = cache
        .materialize_source(source_tree)
        .map_err(LockedPackageCacheFetchError::Cache)
        .map_err(|error| attach_corruption_notice(error, corruption_notice.as_ref()))?;
    cache
        .record_git_source_checksum(&request.url, &commit, directory, &source_checksum)
        .map_err(LockedPackageCacheFetchError::Cache)
        .map_err(|error| attach_corruption_notice(error, corruption_notice.as_ref()))?;
    if !tag_missing {
        cache
            .record_git_tag(&request.url, &tag_name, &commit)
            .map_err(LockedPackageCacheFetchError::Cache)
            .map_err(|error| attach_corruption_notice(error, corruption_notice.as_ref()))?;
    }

    let status = match &corruption_notice {
        Some(notice) => LockedPackageCacheStatus::RepairedCorrupt {
            actual_checksum: notice.actual_checksum.clone(),
        },
        None => LockedPackageCacheStatus::Fetched,
    };
    Ok(CachedLockedPackage {
        source,
        status,
        tag_missing,
    })
}

fn cached_source_has_no_optional_checksums(source: &CachedPackageSource) -> bool {
    let manifest_path = source.path().join("sai.package");
    let Ok(file) = File::open(&manifest_path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > crate::manifest::MAX_PACKAGE_MANIFEST_BYTES as u64 {
        return false;
    }

    let mut contents = Vec::new();
    if file
        .take(crate::manifest::MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .is_err()
        || contents.len() > crate::manifest::MAX_PACKAGE_MANIFEST_BYTES
    {
        return false;
    }
    let parsed = crate::manifest::parse_for_role(&contents, PackageRole::Dependency);
    parsed.is_syntax_valid()
        && !parsed.file().declarations.iter().any(|declaration| {
            matches!(
                declaration.value,
                PackageDeclaration::Wit(_) | PackageDeclaration::ArtifactComponents(_)
            )
        })
}

fn attach_corruption_notice(
    error: LockedPackageCacheFetchError,
    notice: Option<&CacheCorruptionNotice>,
) -> LockedPackageCacheFetchError {
    match notice {
        Some(notice) => LockedPackageCacheFetchError::CorruptSourceObserved {
            notice: notice.clone(),
            cause: Box::new(error),
        },
        None => error,
    }
}

fn verify_cached_package_checksums(
    expected: &ExpectedPackageChecksums,
    actual: &CachedPackageChecksums,
) -> Result<(), GitRemoteReleaseFetchError> {
    if expected.source_checksum != actual.source_checksum {
        return Err(checksum_mismatch(
            GitPackageChecksumKind::Source,
            &expected.source_checksum,
            &actual.source_checksum,
        ));
    }

    let expected_wit = expected.wit_checksum.as_deref().unwrap_or("<missing>");
    let actual_wit = actual.wit_checksum.as_deref().unwrap_or("<missing>");
    if expected_wit != actual_wit {
        return Err(checksum_mismatch(
            GitPackageChecksumKind::Wit,
            expected_wit,
            actual_wit,
        ));
    }

    let mut expected_artifacts = expected.artifact_checksums.iter().collect::<Vec<_>>();
    expected_artifacts.sort_by(|left, right| left.target.as_bytes().cmp(right.target.as_bytes()));
    let actual_artifacts = &actual.artifact_checksums;
    let (mut expected_index, mut actual_index) = (0, 0);
    while expected_index < expected_artifacts.len() || actual_index < actual_artifacts.len() {
        match (
            expected_artifacts.get(expected_index),
            actual_artifacts.get(actual_index),
        ) {
            (Some(expected), Some(actual)) => {
                match expected.target.as_bytes().cmp(actual.0.as_bytes()) {
                    std::cmp::Ordering::Less => {
                        return Err(checksum_mismatch(
                            GitPackageChecksumKind::Artifact {
                                target: expected.target.clone(),
                            },
                            &expected.checksum,
                            "<missing>",
                        ));
                    }
                    std::cmp::Ordering::Greater => {
                        return Err(checksum_mismatch(
                            GitPackageChecksumKind::Artifact {
                                target: actual.0.clone(),
                            },
                            "<missing>",
                            &actual.1,
                        ));
                    }
                    std::cmp::Ordering::Equal => {
                        if expected.checksum != actual.1 {
                            return Err(checksum_mismatch(
                                GitPackageChecksumKind::Artifact {
                                    target: expected.target.clone(),
                                },
                                &expected.checksum,
                                &actual.1,
                            ));
                        }
                        expected_index += 1;
                        actual_index += 1;
                    }
                }
            }
            (Some(expected), None) => {
                return Err(checksum_mismatch(
                    GitPackageChecksumKind::Artifact {
                        target: expected.target.clone(),
                    },
                    &expected.checksum,
                    "<missing>",
                ));
            }
            (None, Some(actual)) => {
                return Err(checksum_mismatch(
                    GitPackageChecksumKind::Artifact {
                        target: actual.0.clone(),
                    },
                    "<missing>",
                    &actual.1,
                ));
            }
            (None, None) => unreachable!("loop ends when both checksum lists are exhausted"),
        }
    }
    Ok(())
}

fn package_checksums(source: &GitPackageSourceTree) -> CachedPackageChecksums {
    CachedPackageChecksums {
        source_checksum: source.source_tree().checksum.clone().into(),
        wit_checksum: source.wit_tree().map(|tree| tree.checksum.clone().into()),
        artifact_checksums: source
            .artifact_checksums()
            .iter()
            .map(|artifact| (artifact.target().into(), artifact.checksum().into()))
            .collect(),
    }
}

fn find_verified_source(
    cache: &PackageCache,
    checksum: &str,
) -> Result<Option<CachedPackageSource>, LockedPackageCacheFetchError> {
    let CachedSourceVerification::Verified(source) = cache
        .verify_source(checksum)
        .map_err(LockedPackageCacheFetchError::Cache)?
    else {
        return Ok(None);
    };
    Ok(Some(source))
}

fn fetch_locked_release_tree_in_repository(
    repository: &gix::Repository,
    source_url: GitSourceUrl<'_>,
    version: &Version,
    directory: Option<&RepositoryPackageDirectory>,
    locked_commit: &gix::ObjectId,
    expected_checksums: &ExpectedPackageChecksums,
    transfer_limit: &gix_transport::client::blocking_io::FetchTransferLimit,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let tag_name = release_tag_name(version, directory)?;
    let refspec = format!("{tag_name}:refs/sai/package-release-tag");
    fetch_release_tree_in_repository(
        repository,
        ReleaseFetchRequest {
            source_url,
            refspec: &refspec,
            tag_name: &tag_name,
            version,
            directory,
            expected_commit: Some(locked_commit),
            expected_source_checksum: None,
            expected_locked_checksums: Some(expected_checksums),
            transfer_limit,
        },
    )
}

#[derive(Clone, Copy)]
enum GitSourceUrl<'a> {
    Canonical(&'a str),
    #[cfg(test)]
    TestTransport {
        transport_url: &'a str,
        canonical_url: &'a str,
    },
}

impl GitSourceUrl<'_> {
    fn transport_url(&self) -> &str {
        match self {
            Self::Canonical(url) => url,
            #[cfg(test)]
            Self::TestTransport { transport_url, .. } => transport_url,
        }
    }

    fn canonical_url(&self) -> &str {
        match self {
            Self::Canonical(url) => url,
            #[cfg(test)]
            Self::TestTransport { canonical_url, .. } => canonical_url,
        }
    }
}

impl<'a> GitSourceUrl<'a> {
    fn canonical(url: &'a str) -> Self {
        Self::Canonical(url)
    }

    #[cfg(test)]
    fn test_transport(transport_url: &'a str, canonical_url: &'a str) -> Self {
        Self::TestTransport {
            transport_url,
            canonical_url,
        }
    }
}

struct ReleaseFetchRequest<'a> {
    source_url: GitSourceUrl<'a>,
    refspec: &'a str,
    tag_name: &'a str,
    version: &'a Version,
    directory: Option<&'a RepositoryPackageDirectory>,
    expected_commit: Option<&'a gix::ObjectId>,
    expected_source_checksum: Option<&'a str>,
    expected_locked_checksums: Option<&'a ExpectedPackageChecksums>,
    transfer_limit: &'a gix_transport::client::blocking_io::FetchTransferLimit,
}

fn fetch_release_tree_in_repository(
    repository: &gix::Repository,
    request: ReleaseFetchRequest<'_>,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let ReleaseFetchRequest {
        source_url,
        refspec,
        tag_name,
        version,
        directory,
        expected_commit,
        expected_source_checksum,
        expected_locked_checksums,
        transfer_limit,
    } = request;
    let validated_canonical_url = canonical_git_url(source_url.canonical_url())
        .map_err(|()| GitRemoteReleaseFetchError::InvalidGitUrl)?;
    if validated_canonical_url != source_url.canonical_url() {
        return Err(GitRemoteReleaseFetchError::InvalidGitUrl);
    }
    let remote = repository
        .remote_at_without_url_rewrite(source_url.transport_url())
        .map_err(|_| GitRemoteReleaseFetchError::RemoteConfiguration)?
        .with_refspecs([BString::from(refspec)], gix::remote::Direction::Fetch)
        .map_err(|_| GitRemoteReleaseFetchError::RemoteConfiguration)?
        .with_fetch_tags(gix::remote::fetch::Tags::None);
    let transport_options = package_transport_options(
        repository,
        source_url.transport_url(),
        Some(transfer_limit.clone()),
    )?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(map_transport_error_for_fetch)?
        .with_transport_options(transport_options);
    let prepared = connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(map_transport_error_for_fetch)?;
    let tag_object_id = prepared.ref_map().remote_refs.iter().find_map(|reference| {
        let (name, object_id, _) = reference.unpack();
        if name.as_bytes() == tag_name.as_bytes() {
            object_id.map(|id| id.to_owned())
        } else {
            None
        }
    });
    let shallow_depth = std::num::NonZeroU32::new(1).expect("one is nonzero");
    let interrupt = AtomicBool::new(false);
    let (tag_object_id, commit_id, tag_missing) = if let Some(tag_object_id) = tag_object_id {
        if tag_object_id.kind() != repository.object_hash() {
            return Err(GitRemoteReleaseFetchError::FetchFailed);
        }
        prepared
            .with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(shallow_depth))
            .receive(gix::progress::Discard, &interrupt)
            .map_err(map_transport_error_for_fetch)?;

        let reader = GitTreeReader::open(
            repository.git_dir().join("objects"),
            repository.object_hash(),
        )
        .map_err(GitRemoteReleaseFetchError::SourceTree)?;
        let commit_id = reader
            .peel_tag_to_commit(&tag_object_id)
            .map_err(GitRemoteReleaseFetchError::InvalidTag)?;
        (Some(tag_object_id), commit_id, false)
    } else {
        let expected_commit =
            expected_commit.ok_or_else(|| GitRemoteReleaseFetchError::TagNotFound {
                name: tag_name.into(),
            })?;
        if expected_commit.kind() != repository.object_hash() {
            return Err(GitRemoteReleaseFetchError::FetchFailed);
        }
        drop(prepared);
        fetch_locked_commit(
            repository,
            source_url.transport_url(),
            expected_commit,
            LockedCommitFetchContext::ReleaseTag(tag_name),
            transfer_limit,
        )?;
        (None, expected_commit.to_owned(), true)
    };

    let reader = GitTreeReader::open(
        repository.git_dir().join("objects"),
        repository.object_hash(),
    )
    .map_err(GitRemoteReleaseFetchError::SourceTree)?;
    if let Some(expected_commit) = expected_commit
        && &commit_id != expected_commit
    {
        return Err(GitRemoteReleaseFetchError::TagMoved {
            name: tag_name.into(),
            expected: *expected_commit,
            actual: commit_id,
        });
    }

    let repository_root = RepositoryPackageDirectory::parse(".")
        .map_err(|_| GitRemoteReleaseFetchError::InvalidPackageDirectory)?;
    let package_directory = directory.unwrap_or(&repository_root);
    let package_source_tree = reader
        .read_commit_repository_package_source_tree(&commit_id, package_directory)
        .map_err(GitRemoteReleaseFetchError::SourceTree)?;
    if let Some(expected_source_checksum) = expected_source_checksum
        && package_source_tree.source_tree().checksum != expected_source_checksum
    {
        return Err(GitRemoteReleaseFetchError::ChecksumMismatch {
            kind: GitPackageChecksumKind::Source,
            expected: expected_source_checksum.into(),
            actual: package_source_tree.source_tree().checksum.clone().into(),
        });
    }
    if let Some(expected_checksums) = expected_locked_checksums {
        verify_locked_package_checksums(expected_checksums, &package_source_tree)?;
    }

    Ok(FetchedGitRelease {
        tag_name: tag_name.into(),
        version: version.clone(),
        tag_object_id,
        tag_missing,
        source_identity: GitPackageSourceIdentity {
            canonical_url: source_url.canonical_url().into(),
            commit_id,
            directory: package_directory.clone(),
        },
        source: package_source_tree,
    })
}

fn verify_locked_package_checksums(
    expected: &ExpectedPackageChecksums,
    actual: &GitPackageSourceTree,
) -> Result<(), GitRemoteReleaseFetchError> {
    if expected.source_checksum.as_ref() != actual.source_tree().checksum {
        return Err(checksum_mismatch(
            GitPackageChecksumKind::Source,
            &expected.source_checksum,
            &actual.source_tree().checksum,
        ));
    }

    let expected_wit = expected.wit_checksum.as_deref().unwrap_or("<missing>");
    let actual_wit = actual
        .wit_tree()
        .map_or("<missing>", |tree| tree.checksum.as_str());
    if expected_wit != actual_wit {
        return Err(checksum_mismatch(
            GitPackageChecksumKind::Wit,
            expected_wit,
            actual_wit,
        ));
    }

    let mut expected_artifacts = expected.artifact_checksums.iter().collect::<Vec<_>>();
    expected_artifacts.sort_by(|left, right| left.target.as_bytes().cmp(right.target.as_bytes()));
    let actual_artifacts = actual.artifact_checksums();
    let (mut expected_index, mut actual_index) = (0, 0);
    while expected_index < expected_artifacts.len() || actual_index < actual_artifacts.len() {
        match (
            expected_artifacts.get(expected_index),
            actual_artifacts.get(actual_index),
        ) {
            (Some(expected), Some(actual)) => {
                match expected.target.as_bytes().cmp(actual.target().as_bytes()) {
                    std::cmp::Ordering::Less => {
                        return Err(checksum_mismatch(
                            GitPackageChecksumKind::Artifact {
                                target: expected.target.clone(),
                            },
                            &expected.checksum,
                            "<missing>",
                        ));
                    }
                    std::cmp::Ordering::Greater => {
                        return Err(checksum_mismatch(
                            GitPackageChecksumKind::Artifact {
                                target: actual.target().into(),
                            },
                            "<missing>",
                            actual.checksum(),
                        ));
                    }
                    std::cmp::Ordering::Equal => {
                        if expected.checksum.as_ref() != actual.checksum() {
                            return Err(checksum_mismatch(
                                GitPackageChecksumKind::Artifact {
                                    target: expected.target.clone(),
                                },
                                &expected.checksum,
                                actual.checksum(),
                            ));
                        }
                        expected_index += 1;
                        actual_index += 1;
                    }
                }
            }
            (Some(expected), None) => {
                return Err(checksum_mismatch(
                    GitPackageChecksumKind::Artifact {
                        target: expected.target.clone(),
                    },
                    &expected.checksum,
                    "<missing>",
                ));
            }
            (None, Some(actual)) => {
                return Err(checksum_mismatch(
                    GitPackageChecksumKind::Artifact {
                        target: actual.target().into(),
                    },
                    "<missing>",
                    actual.checksum(),
                ));
            }
            (None, None) => unreachable!("loop ends when both checksum lists are exhausted"),
        }
    }
    Ok(())
}

fn checksum_mismatch(
    kind: GitPackageChecksumKind,
    expected: &str,
    actual: &str,
) -> GitRemoteReleaseFetchError {
    GitRemoteReleaseFetchError::ChecksumMismatch {
        kind,
        expected: expected.into(),
        actual: actual.into(),
    }
}

#[derive(Clone, Copy)]
enum LockedCommitFetchContext<'a> {
    ReleaseTag(&'a str),
    RepositoryPackage,
}

fn fetch_locked_commit(
    repository: &gix::Repository,
    url: &str,
    commit_id: &gix::ObjectId,
    context: LockedCommitFetchContext<'_>,
    transfer_limit: &gix_transport::client::blocking_io::FetchTransferLimit,
) -> Result<(), GitRemoteReleaseFetchError> {
    let refspec = format!("{commit_id}:refs/sai/locked-package-commit");
    let remote = repository
        .remote_at_without_url_rewrite(url)
        .map_err(|_| GitRemoteReleaseFetchError::RemoteConfiguration)?
        .with_refspecs([BString::from(refspec)], gix::remote::Direction::Fetch)
        .map_err(|_| GitRemoteReleaseFetchError::RemoteConfiguration)?
        .with_fetch_tags(gix::remote::fetch::Tags::None);
    let transport_options =
        package_transport_options(repository, url, Some(transfer_limit.clone()))?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(map_transport_error_for_fetch)?
        .with_transport_options(transport_options);
    let prepared = connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(map_transport_error_for_fetch)?;
    let shallow_depth = std::num::NonZeroU32::new(1).expect("one is nonzero");
    let interrupt = AtomicBool::new(false);
    prepared
        .with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(shallow_depth))
        .receive(gix::progress::Discard, &interrupt)
        .map(|_| ())
        .map_err(|error| {
            if is_locked_commit_unavailable(&error) {
                match context {
                    LockedCommitFetchContext::ReleaseTag(name) => {
                        GitRemoteReleaseFetchError::LockedCommitUnavailable {
                            name: name.into(),
                            commit: *commit_id,
                        }
                    }
                    LockedCommitFetchContext::RepositoryPackage => {
                        GitRemoteReleaseFetchError::RepositoryCommitUnavailable {
                            commit: *commit_id,
                        }
                    }
                }
            } else {
                map_transport_error_for_fetch(error)
            }
        })
}

fn is_locked_commit_unavailable(error: &gix::remote::fetch::Error) -> bool {
    let gix::remote::fetch::Error::Fetch(gix::protocol::fetch::Error::FetchResponse(
        gix::protocol::fetch::response::Error::UploadPack(packetline_error),
    )) = error
    else {
        return false;
    };
    is_missing_object_message(packetline_error.message.as_bstr())
}

fn is_missing_object_message(message: &BStr) -> bool {
    let Ok(message) = message.to_str() else {
        return false;
    };
    let message = message
        .trim()
        .strip_prefix("upload-pack: ")
        .unwrap_or(message.trim())
        .to_ascii_lowercase();
    message.starts_with("not our ref ")
        || message.starts_with("unadvertised object ")
        || message.contains("does not allow request for unadvertised object")
}

fn package_transport_options(
    repository: &gix::Repository,
    url: &str,
    transfer_limit: Option<gix_transport::client::blocking_io::FetchTransferLimit>,
) -> Result<Box<dyn std::any::Any>, GitRemoteReleaseFetchError> {
    if url.starts_with("https://") {
        let mut options = repository
            .transport_options(url.as_bytes().as_bstr(), None)
            .map_err(|_| GitRemoteReleaseFetchError::RemoteConfiguration)?
            .ok_or(GitRemoteReleaseFetchError::RemoteConfiguration)?;
        let http_options = options
            .downcast_mut::<gix_transport::client::blocking_io::http::Options>()
            .ok_or(GitRemoteReleaseFetchError::RemoteConfiguration)?;
        http_options.fetch_transfer_limit = transfer_limit;
        Ok(options)
    } else {
        let options = gix_transport::client::blocking_io::SubprocessReadTimeout::new(
            SSH_TAG_LIST_READ_TIMEOUT,
        );
        let options = match transfer_limit {
            Some(transfer_limit) => options.with_transfer_limit(transfer_limit),
            None => options,
        };
        Ok(Box::new(options))
    }
}

fn tag_list_transport_options(
    repository: &gix::Repository,
    url: &str,
) -> Result<Box<dyn std::any::Any>, GitRemoteTagListError> {
    if url.starts_with("https://") {
        repository
            .transport_options(url.as_bytes().as_bstr(), None)
            .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
            .ok_or(GitRemoteTagListError::RemoteConfiguration)
    } else {
        Ok(Box::new(
            gix_transport::client::blocking_io::SubprocessReadTimeout::new(
                SSH_TAG_LIST_READ_TIMEOUT,
            ),
        ))
    }
}

fn exact_release_tag_refspec(
    tag: &GitReleaseTag,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<String, GitRemoteReleaseFetchError> {
    let expected_tag_name = release_tag_name(tag.version(), directory)?;
    if tag.name() != expected_tag_name {
        return Err(GitRemoteReleaseFetchError::InvalidPackageDirectory);
    }
    Ok(exact_release_tag_refspec_for_name(&expected_tag_name))
}

fn exact_release_tag_refspec_for_name(tag_name: &str) -> String {
    format!("{tag_name}:refs/sai/package-release-tag")
}

fn release_tag_name(
    version: &Version,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<String, GitRemoteReleaseFetchError> {
    let prefix = release_tag_prefix(directory)
        .map_err(|_| GitRemoteReleaseFetchError::InvalidPackageDirectory)?;
    Ok(format!("{prefix}{version}"))
}

fn map_tag_list_error_for_fetch(error: GitRemoteTagListError) -> GitRemoteReleaseFetchError {
    match error {
        GitRemoteTagListError::InvalidGitUrl => GitRemoteReleaseFetchError::InvalidGitUrl,
        GitRemoteTagListError::InvalidPackageDirectory => {
            GitRemoteReleaseFetchError::InvalidPackageDirectory
        }
        GitRemoteTagListError::TemporaryRepository => {
            GitRemoteReleaseFetchError::TemporaryRepository
        }
        GitRemoteTagListError::RemoteConfiguration => {
            GitRemoteReleaseFetchError::RemoteConfiguration
        }
        GitRemoteTagListError::FetchFailed => GitRemoteReleaseFetchError::FetchFailed,
        GitRemoteTagListError::TagList(error) => GitRemoteReleaseFetchError::TagList(error),
    }
}

fn map_transport_error_for_fetch(error: impl StdError + 'static) -> GitRemoteReleaseFetchError {
    if let Some(limit_bytes) = fetch_transfer_limit_in_error(&error) {
        return GitRemoteReleaseFetchError::TransferLimitExceeded { limit_bytes };
    }
    map_tag_list_error_for_fetch(map_transport_error(error))
}

fn fetch_transfer_limit_in_error(error: &(dyn StdError + 'static)) -> Option<u64> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(limit_error) =
            error.downcast_ref::<gix_transport::client::blocking_io::FetchTransferLimitExceeded>()
        {
            return Some(limit_error.limit_bytes);
        }
        if let Some(io_error) = error.downcast_ref::<std::io::Error>()
            && let Some(source) = io_error.get_ref()
            && let Some(limit_error) =
                source
                    .downcast_ref::<gix_transport::client::blocking_io::FetchTransferLimitExceeded>(
                    )
        {
            return Some(limit_error.limit_bytes);
        }
        current = error.source();
    }
    None
}

fn list_release_tags_in_repository(
    repository: &gix::Repository,
    url: &str,
    refspec: &str,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<Vec<GitReleaseTag>, GitRemoteTagListError> {
    let remote = repository
        .remote_at_without_url_rewrite(url)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_refspecs([BString::from(refspec)], gix::remote::Direction::Fetch)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_fetch_tags(gix::remote::fetch::Tags::None);
    let transport_options = tag_list_transport_options(repository, url)?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(map_transport_error)?
        .with_transport_options(transport_options);
    let (ref_map, _) = connection
        .ref_map(gix::progress::Discard, Default::default())
        .map_err(map_transport_error)?;

    let refs = ref_map.remote_refs.into_iter().filter_map(|reference| {
        let (name, object_id, _peeled_id) = reference.unpack();
        Some((name.to_owned(), object_id?.to_owned()))
    });
    parse_release_tag_refs(refs, directory).map_err(GitRemoteTagListError::TagList)
}

fn find_release_tag_in_repository(
    repository: &gix::Repository,
    url: &str,
    refspec: &str,
    tag_name: &str,
    version: &Version,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<Option<GitReleaseTag>, GitRemoteTagListError> {
    let remote = repository
        .remote_at_without_url_rewrite(url)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_refspecs([BString::from(refspec)], gix::remote::Direction::Fetch)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_fetch_tags(gix::remote::fetch::Tags::None);
    let transport_options = tag_list_transport_options(repository, url)?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(map_transport_error)?
        .with_transport_options(transport_options);
    let (ref_map, _) = connection
        .ref_map(gix::progress::Discard, Default::default())
        .map_err(map_transport_error)?;

    let mut matching_refs = ref_map.remote_refs.into_iter().filter_map(|reference| {
        let (name, object_id, _) = reference.unpack();
        let object_id = object_id?;
        (name.as_bytes() == tag_name.as_bytes()).then(|| (name.to_owned(), object_id.to_owned()))
    });
    let Some((name, object_id)) = matching_refs.next() else {
        return Ok(None);
    };
    let tag = parse_exact_release_tag_ref(name.as_bytes(), object_id, version, directory)
        .map_err(GitRemoteTagListError::TagList)?
        .ok_or(GitRemoteTagListError::FetchFailed)?;
    if matching_refs.next().is_some() {
        return Err(GitRemoteTagListError::FetchFailed);
    }
    Ok(Some(tag))
}

fn map_transport_error(error: impl StdError + 'static) -> GitRemoteTagListError {
    let mut source = Some(&error as &(dyn StdError + 'static));
    while let Some(current) = source {
        let limit = current
            .downcast_ref::<gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded>()
            .or_else(|| {
                current
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref)
                    .and_then(|source| {
                        source.downcast_ref::<
                            gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded,
                        >()
                    })
            });
        if let Some(limit) = limit {
            let actual = limit.actual;
            let limit = match limit.limit {
                gix::protocol::handshake::refs::parse::AdvertisementLimit::RefCount => {
                    GitReleaseTagListLimit::GitRefCount
                }
                gix::protocol::handshake::refs::parse::AdvertisementLimit::RefNameBytes => {
                    GitReleaseTagListLimit::GitRefNameBytes
                }
            };
            return GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit,
                actual,
            });
        }
        source = current.source();
    }
    GitRemoteTagListError::FetchFailed
}

fn release_tag_refspec(
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<String, GitRemoteTagListError> {
    let prefix = release_tag_prefix(directory)
        .map_err(|_| GitRemoteTagListError::InvalidPackageDirectory)?;
    Ok(format!("{prefix}*:{prefix}*"))
}

fn transport_config_overrides() -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut overrides = vec![
        config_override("protocol.version", "2"),
        config_override("http.sslVerify", "true"),
        config_override("gitoxide.https.sslNoVerify", "false"),
        config_override("gitoxide.credentials.terminalPrompt", "false"),
        config_override("gitoxide.credentials.helperStderr", "false"),
        config_override("ssh.variant", "ssh"),
        config_override("http.followRedirects", "initial"),
        config_override("gitoxide.credentials.useHttpPath", "false"),
        config_override("core.sshCommand", ssh_command()),
    ];
    overrides.extend(proxy_environment_overrides(|name| std::env::var_os(name))?);
    overrides.extend(user_credential_overrides()?);
    Ok(overrides)
}

fn ssh_command() -> &'static str {
    #[cfg(windows)]
    {
        "ssh -F NUL -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    }
    #[cfg(not(windows))]
    {
        "ssh -F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    }
}

fn config_override(key: &str, value: &str) -> BString {
    let mut bytes = Vec::with_capacity(key.len() + value.len() + 1);
    bytes.extend_from_slice(key.as_bytes());
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());
    bytes.into()
}

fn proxy_environment_overrides(
    mut get: impl FnMut(&str) -> Option<OsString>,
) -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut overrides = Vec::new();
    if let Some(proxy) = first_environment_value(&mut get, "HTTPS_PROXY", "https_proxy")? {
        if !proxy.is_empty() {
            overrides.push(config_override("http.proxy", &proxy));
        }
    }
    if let Some(no_proxy) = first_environment_value(&mut get, "NO_PROXY", "no_proxy")? {
        if !no_proxy.is_empty() {
            overrides.push(config_override("gitoxide.http.noProxy", &no_proxy));
        }
    }
    Ok(overrides)
}

fn first_environment_value(
    get: &mut impl FnMut(&str) -> Option<OsString>,
    upper: &str,
    lower: &str,
) -> Result<Option<String>, GitRemoteTagListError> {
    get(upper)
        .or_else(|| get(lower))
        .map(|value| {
            value
                .into_string()
                .map_err(|_| GitRemoteTagListError::RemoteConfiguration)
        })
        .transpose()
}

fn user_credential_overrides() -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut environment = |name: &str| std::env::var_os(name);
    let home = gix::path::env::home_dir();
    let paths = user_credential_config_paths(&mut environment, home.as_deref());
    let mut overrides = Vec::new();
    for (path, source) in paths {
        if matches!(fs::metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        overrides.extend(credential_overrides_from_path(
            path,
            source,
            home.as_deref(),
        )?);
    }
    Ok(overrides)
}

fn credential_overrides_from_path(
    path: PathBuf,
    source: gix::config::Source,
    home: Option<&Path>,
) -> Result<Vec<BString>, GitRemoteTagListError> {
    let metadata = gix::config::file::Metadata::try_from_path(path, source)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?;
    let config = gix::config::File::from_paths_metadata(
        [metadata],
        gix::config::file::init::Options {
            includes: gix::config::file::includes::Options::follow_without_conditional(home),
            ..Default::default()
        },
    )
    .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
    .ok_or(GitRemoteTagListError::RemoteConfiguration)?;
    Ok(credential_overrides_from_file(&config))
}

fn user_credential_config_paths(
    environment: &mut impl FnMut(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Vec<(PathBuf, gix::config::Source)> {
    let mut paths = Vec::new();
    if let Some(path) = gix::path::env::xdg_config("config", environment) {
        paths.push((path, gix::config::Source::Git));
    }
    if let Some(home) = home {
        let path = home.join(".gitconfig");
        if !paths.iter().any(|(existing, _)| existing == &path) {
            paths.push((path, gix::config::Source::User));
        }
    }
    paths
}

fn credential_overrides_from_file(config: &gix::config::File) -> Vec<BString> {
    let mut overrides = Vec::new();
    for section in config.sections() {
        if section.meta().trust != gix::sec::Trust::Full
            || !section
                .header()
                .name()
                .as_bytes()
                .eq_ignore_ascii_case(b"credential")
        {
            continue;
        }

        let subsection = section.header().subsection_name();
        for name in ["helper", "username", "useHttpPath", "protectProtocol"] {
            let key = credential_key(subsection, name.as_bytes());
            for value in section.values(name) {
                let mut setting = key.clone();
                setting.push(b'=');
                setting.extend_from_slice(&value);
                overrides.push(setting);
            }
        }
    }
    overrides
}

fn credential_key(subsection: Option<&BStr>, value_name: &[u8]) -> BString {
    let mut key = b"credential".to_vec();
    if let Some(subsection) = subsection {
        key.push(b'.');
        key.extend_from_slice(subsection.as_bytes());
    }
    key.push(b'.');
    key.extend_from_slice(value_name);
    key.into()
}

struct TemporaryGitDirectory {
    path: PathBuf,
    cache_root: PathBuf,
    activity_lock_path: PathBuf,
    activity_lock: Option<File>,
}

impl TemporaryGitDirectory {
    fn create() -> Result<Self, ()> {
        #[cfg(test)]
        {
            let temp_dir = std::env::temp_dir();
            Self::create_in(&temp_dir, &temp_dir)
        }
        #[cfg(not(test))]
        {
            let cache_root = cache_directory_from_environment().map_err(|_| ())?;
            let cache = PackageCache::open(cache_root).map_err(|_| ())?;
            Self::create_with_cache(&cache)
        }
    }

    fn create_with_cache(cache: &PackageCache) -> Result<Self, ()> {
        cache.cleanup_stale_temporary_directories();
        Self::create_in(cache.temporary_directory(), cache.root())
    }

    fn create_in(parent: &Path, cache_root: &Path) -> Result<Self, ()> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        for _ in 0..128 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let nonce = RandomState::new().hash_one((std::process::id(), id));
            let path = parent.join(format!(
                "sai-git-remote-{}-{id}-{nonce:016x}",
                std::process::id()
            ));
            let activity_lock_path = staging_activity_lock_path(&path);
            let activity_lock = match create_staging_activity_lock(&activity_lock_path, cache_root)
            {
                Ok(lock) => lock,
                Err(PackageCacheError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    continue;
                }
                Err(_) => return Err(()),
            };
            match create_temporary_directory(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        cache_root: cache_root.to_path_buf(),
                        activity_lock_path,
                        activity_lock: Some(activity_lock),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    release_staging_activity_lock(
                        &activity_lock_path,
                        activity_lock,
                        true,
                        cache_root,
                    );
                }
                Err(_) => {
                    release_staging_activity_lock(
                        &activity_lock_path,
                        activity_lock,
                        true,
                        cache_root,
                    );
                    return Err(());
                }
            }
        }
        Err(())
    }
}

fn create_temporary_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

impl Drop for TemporaryGitDirectory {
    fn drop(&mut self) {
        let removed = crate::package_cache::remove_tree_at_path(&self.path, &self.cache_root);
        if let Some(lock) = self.activity_lock.take() {
            release_staging_activity_lock(
                &self.activity_lock_path,
                lock,
                removed,
                &self.cache_root,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::ops::Range;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU64;

    use super::{
        FetchedGitPackage, GitRemoteReleaseContractError, GitRemoteReleaseFetchError,
        GitRemoteTagListError, SelectedGitReleaseFetchError, SelectedGitReleaseFetchPlan,
        TemporaryGitDirectory, credential_overrides_from_file, credential_overrides_from_path,
        exact_release_tag_refspec_for_name, fetch_release_tree_in_repository,
        find_release_tag_in_repository, list_release_tags_in_repository, map_transport_error,
        plan_selected_git_release_fetch, proxy_environment_overrides, release_tag_name,
        release_tag_refspec, user_credential_config_paths, verify_locked_git_path_package,
    };
    use crate::git_path_requirements::{
        GitPathRequirementDiscoveryError, GitPathRequirementEdge, discover_git_path_requirements,
    };
    use crate::git_source_index::{
        GitPackageSourceIdentityIndex, GitPackageSourceIdentityIndexError,
    };
    use crate::git_tags::{GitReleaseTagListError, GitReleaseTagListLimit, parse_release_tag_refs};
    use crate::git_tree::{GitTreeReadError, GitTreeReader};
    use crate::local_source::LocalPackageDirectory;
    use crate::lock::{LockPackage, LockPackageField, LockSource};
    use crate::lock_conflict::{reconstruct_conflict_sides, validate_conflict_sides};
    use crate::manifest::{PackageRole, Spanned, parse_for_role};
    use crate::mixed_source::{
        MixedPackageSourceIdentity, resolve_mixed_source_packages_from_lock_merge_with_fetchers,
        resolve_mixed_source_packages_with_fetchers,
    };
    use crate::mvs::{MvsPackageVersion, MvsRequirer};
    use crate::mvs_diagnostics::{
        MvsManifestLocation, MvsManifestLocations, mixed_source_reload_failure_diagnostics,
    };
    use crate::package_cache::PackageCache;
    use crate::package_path::RepositoryPackageDirectory;
    use crate::resolver_contract::ResolverContractContext;
    use crate::resolver_source::{
        ResolverPackageSource, ResolverPathPinSource, ResolverSourceReason,
    };
    use crate::validation::PackageValidationContext;
    use gix::objs::tree::{Entry, EntryKind};
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};

    const TEST_CANONICAL_GIT_URL: &str = "https://example.test/acme/sample.git";

    fn source_byte_span(source: &[u8], text: &[u8]) -> Range<usize> {
        let start = source
            .windows(text.len())
            .position(|window| window == text)
            .expect("test source contains expected text");
        start..start + text.len()
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(prefix: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(1);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-git-remote-test-{}-{prefix}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct TestRemote {
        directory: TemporaryGitDirectory,
        tag: super::GitReleaseTag,
        commit_id: gix::ObjectId,
        parent_commit_id: gix::ObjectId,
        unrequested_tag_id: gix::ObjectId,
    }

    impl TestRemote {
        fn new(tag_name: &str, nested_tags: bool) -> Self {
            Self::new_with_object_hash(tag_name, nested_tags, gix::hash::Kind::Sha1)
        }

        fn new_with_object_hash(
            tag_name: &str,
            nested_tags: bool,
            object_hash: gix::hash::Kind,
        ) -> Self {
            Self::new_with_package_manifest(
                tag_name,
                nested_tags,
                object_hash,
                Some(
                    b"package acme:sample @ 1.2.3\n\
                      source \"src\"\n\
                      targets { wasm32-core }\n",
                ),
            )
        }

        fn new_without_package_manifest(tag_name: &str, nested_tags: bool) -> Self {
            Self::new_with_package_manifest(tag_name, nested_tags, gix::hash::Kind::Sha1, None)
        }

        fn new_with_package_manifest(
            tag_name: &str,
            nested_tags: bool,
            object_hash: gix::hash::Kind,
            manifest: Option<&[u8]>,
        ) -> Self {
            let directory = TemporaryGitDirectory::create().unwrap();
            let repository = init_bare_repository(&directory.path, Some(object_hash));
            let blob_id = repository.write_blob(b"package source").unwrap().detach();
            let mut entries = vec![Entry {
                mode: EntryKind::Blob.into(),
                filename: b"lib.sai".to_vec().into(),
                oid: blob_id,
            }];
            if let Some(manifest) = manifest {
                let manifest_id = repository.write_blob(manifest).unwrap().detach();
                entries.push(Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"sai.package".to_vec().into(),
                    oid: manifest_id,
                });
                if manifest
                    .windows(b"wit \"wit\"".len())
                    .any(|window| window == b"wit \"wit\"")
                {
                    let interface_id = repository
                        .write_blob(b"package acme:sample;\n")
                        .unwrap()
                        .detach();
                    let wit_tree = repository
                        .write_object(gix::objs::Tree {
                            entries: vec![Entry {
                                mode: EntryKind::Blob.into(),
                                filename: b"api.wit".to_vec().into(),
                                oid: interface_id,
                            }],
                        })
                        .unwrap()
                        .detach();
                    entries.push(Entry {
                        mode: EntryKind::Tree.into(),
                        filename: b"wit".to_vec().into(),
                        oid: wit_tree,
                    });
                }
                if manifest
                    .windows(b"component.wasm".len())
                    .any(|window| window == b"component.wasm")
                {
                    let artifact_id = repository
                        .write_blob(b"verified component bytes")
                        .unwrap()
                        .detach();
                    entries.push(Entry {
                        mode: EntryKind::Blob.into(),
                        filename: b"component.wasm".to_vec().into(),
                        oid: artifact_id,
                    });
                }
            }
            entries.sort();
            let tree_id = repository
                .write_object(gix::objs::Tree { entries })
                .unwrap()
                .detach();
            let parent_commit_id = repository
                .write_object(gix::objs::Commit {
                    tree: tree_id,
                    parents: Default::default(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture parent".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            let commit_id = repository
                .write_object(gix::objs::Commit {
                    tree: tree_id,
                    parents: vec![parent_commit_id].into(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture commit".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            let unrequested_tag_id = write_test_tag(&repository, parent_commit_id, "v9.9.9");
            let direct_tag_id = write_test_tag(&repository, commit_id, "inner");
            let tag_id = if nested_tags {
                write_test_tag(&repository, direct_tag_id, "outer")
            } else {
                direct_tag_id
            };
            let refs = directory.path.join("refs/tags");
            std::fs::create_dir_all(&refs).unwrap();
            std::fs::write(
                refs.join(tag_name.trim_start_matches("refs/tags/")),
                format!("{tag_id}\n"),
            )
            .unwrap();
            std::fs::write(refs.join("v9.9.9"), format!("{unrequested_tag_id}\n")).unwrap();
            let branches = directory.path.join("refs/heads");
            std::fs::create_dir_all(&branches).unwrap();
            std::fs::write(branches.join("main"), format!("{commit_id}\n")).unwrap();
            let tag = parse_release_tag_refs([(tag_name, tag_id)], None)
                .unwrap()
                .remove(0);
            Self {
                directory,
                tag,
                commit_id,
                parent_commit_id,
                unrequested_tag_id,
            }
        }

        fn new_with_sibling_target_manifests(
            tag_name: &str,
            requiring_manifest: &[u8],
            target_manifest: &[u8],
        ) -> Self {
            let directory = TemporaryGitDirectory::create().unwrap();
            let repository = init_bare_repository(&directory.path, Some(gix::hash::Kind::Sha1));
            let requiring_manifest_id = repository.write_blob(requiring_manifest).unwrap().detach();
            let requiring_source_id = repository
                .write_blob(b"pub fn requiring() {}\n")
                .unwrap()
                .detach();
            let requiring_entries = vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"main.sai".to_vec().into(),
                    oid: requiring_source_id,
                },
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"sai.package".to_vec().into(),
                    oid: requiring_manifest_id,
                },
            ];
            let requiring_tree_id = repository
                .write_object(gix::objs::Tree {
                    entries: requiring_entries.clone(),
                })
                .unwrap()
                .detach();
            let parent_commit_id = repository
                .write_object(gix::objs::Commit {
                    tree: requiring_tree_id,
                    parents: Default::default(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture parent".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();

            let target_manifest_id = repository.write_blob(target_manifest).unwrap().detach();
            let target_source_id = repository
                .write_blob(b"pub fn target() {}\n")
                .unwrap()
                .detach();
            let mut target_entries = vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"main.sai".to_vec().into(),
                    oid: target_source_id,
                },
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"sai.package".to_vec().into(),
                    oid: target_manifest_id,
                },
            ];
            target_entries.sort();
            let target_tree_id = repository
                .write_object(gix::objs::Tree {
                    entries: target_entries,
                })
                .unwrap()
                .detach();
            let packages_tree_id = repository
                .write_object(gix::objs::Tree {
                    entries: vec![Entry {
                        mode: EntryKind::Tree.into(),
                        filename: b"target".to_vec().into(),
                        oid: target_tree_id,
                    }],
                })
                .unwrap()
                .detach();
            let mut root_entries = requiring_entries;
            root_entries.push(Entry {
                mode: EntryKind::Tree.into(),
                filename: b"packages".to_vec().into(),
                oid: packages_tree_id,
            });
            root_entries.sort();
            let root_tree_id = repository
                .write_object(gix::objs::Tree {
                    entries: root_entries,
                })
                .unwrap()
                .detach();
            let commit_id = repository
                .write_object(gix::objs::Commit {
                    tree: root_tree_id,
                    parents: vec![parent_commit_id].into(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture commit with target sibling".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            let unrequested_tag_id = write_test_tag(&repository, parent_commit_id, "v9.9.9");
            let tag_id = write_test_tag(&repository, commit_id, tag_name);
            let refs = directory.path.join("refs/tags");
            std::fs::create_dir_all(&refs).unwrap();
            std::fs::write(
                refs.join(tag_name.trim_start_matches("refs/tags/")),
                format!("{tag_id}\n"),
            )
            .unwrap();
            std::fs::write(refs.join("v9.9.9"), format!("{unrequested_tag_id}\n")).unwrap();
            let branches = directory.path.join("refs/heads");
            std::fs::create_dir_all(&branches).unwrap();
            std::fs::write(branches.join("main"), format!("{commit_id}\n")).unwrap();
            let tag = parse_release_tag_refs([(tag_name, tag_id)], None)
                .unwrap()
                .remove(0);
            Self {
                directory,
                tag,
                commit_id,
                parent_commit_id,
                unrequested_tag_id,
            }
        }

        fn url(&self) -> String {
            format!("file://{}", self.directory.path.display())
        }

        fn commit_with_sibling_package(&self) -> gix::ObjectId {
            let repository =
                gix::open_opts(&self.directory.path, gix::open::Options::isolated()).unwrap();
            let manifest_id = repository
                .write_blob(
                    b"package acme:sibling @ 1.4.0\n\
                      source \"main.sai\"\n\
                      targets { wasm32-core }\n",
                )
                .unwrap()
                .detach();
            let source_id = repository
                .write_blob(b"pub fn sibling() {}\n")
                .unwrap()
                .detach();
            let mut package_entries = vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"main.sai".to_vec().into(),
                    oid: source_id,
                },
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: b"sai.package".to_vec().into(),
                    oid: manifest_id,
                },
            ];
            package_entries.sort();
            let package_tree = repository
                .write_object(gix::objs::Tree {
                    entries: package_entries,
                })
                .unwrap()
                .detach();
            let packages_tree = repository
                .write_object(gix::objs::Tree {
                    entries: vec![Entry {
                        mode: EntryKind::Tree.into(),
                        filename: b"sibling".to_vec().into(),
                        oid: package_tree,
                    }],
                })
                .unwrap()
                .detach();
            let root_tree = repository
                .write_object(gix::objs::Tree {
                    entries: vec![Entry {
                        mode: EntryKind::Tree.into(),
                        filename: b"packages".to_vec().into(),
                        oid: packages_tree,
                    }],
                })
                .unwrap()
                .detach();
            let commit_id = repository
                .write_object(gix::objs::Commit {
                    tree: root_tree,
                    parents: vec![self.commit_id].into(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture sibling commit".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            std::fs::write(
                self.directory.path.join("refs/heads/main"),
                format!("{commit_id}\n"),
            )
            .unwrap();
            commit_id
        }

        fn commit_with_client_and_sibling_packages(
            &self,
            client_manifest: &[u8],
            sibling_manifest: &[u8],
        ) -> gix::ObjectId {
            let repository =
                gix::open_opts(&self.directory.path, gix::open::Options::isolated()).unwrap();
            let package_tree = |manifest: &[u8], source: &[u8]| {
                let manifest_id = repository.write_blob(manifest).unwrap().detach();
                let source_id = repository.write_blob(source).unwrap().detach();
                let mut entries = vec![
                    Entry {
                        mode: EntryKind::Blob.into(),
                        filename: b"main.sai".to_vec().into(),
                        oid: source_id,
                    },
                    Entry {
                        mode: EntryKind::Blob.into(),
                        filename: b"sai.package".to_vec().into(),
                        oid: manifest_id,
                    },
                ];
                entries.sort();
                repository
                    .write_object(gix::objs::Tree { entries })
                    .unwrap()
                    .detach()
            };
            let client_tree = package_tree(client_manifest, b"pub fn client() {}\n");
            let sibling_tree = package_tree(sibling_manifest, b"pub fn sibling() {}\n");
            let packages_tree = repository
                .write_object(gix::objs::Tree {
                    entries: vec![
                        Entry {
                            mode: EntryKind::Tree.into(),
                            filename: b"client".to_vec().into(),
                            oid: client_tree,
                        },
                        Entry {
                            mode: EntryKind::Tree.into(),
                            filename: b"sibling".to_vec().into(),
                            oid: sibling_tree,
                        },
                    ],
                })
                .unwrap()
                .detach();
            let root_tree = repository
                .write_object(gix::objs::Tree {
                    entries: vec![Entry {
                        mode: EntryKind::Tree.into(),
                        filename: b"packages".to_vec().into(),
                        oid: packages_tree,
                    }],
                })
                .unwrap()
                .detach();
            repository
                .write_object(gix::objs::Commit {
                    tree: root_tree,
                    parents: vec![self.commit_id].into(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "fixture client and sibling packages".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach()
        }
    }

    fn init_bare_repository(path: &Path, object_hash: Option<gix::hash::Kind>) -> gix::Repository {
        gix::ThreadSafeRepository::init_opts(
            path,
            gix::create::Kind::Bare,
            gix::create::Options {
                object_hash,
                ..Default::default()
            },
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local()
    }

    fn write_test_tag(
        repository: &gix::Repository,
        target: gix::ObjectId,
        name: &str,
    ) -> gix::ObjectId {
        let target_kind = repository.find_object(target).unwrap().kind;
        repository
            .write_object(gix::objs::Tag {
                target,
                target_kind,
                name: name.into(),
                tagger: None,
                message: "fixture tag".into(),
                signature: None,
            })
            .unwrap()
            .detach()
    }

    fn fetch_fixture(
        remote: &TestRemote,
        expected_commit: Option<&gix::ObjectId>,
        expected_checksum: Option<&str>,
    ) -> Result<super::FetchedGitRelease, GitRemoteReleaseFetchError> {
        fetch_fixture_with_canonical_url(
            remote,
            expected_commit,
            expected_checksum,
            TEST_CANONICAL_GIT_URL,
        )
    }

    fn fetch_fixture_with_canonical_url(
        remote: &TestRemote,
        expected_commit: Option<&gix::ObjectId>,
        expected_checksum: Option<&str>,
        canonical_url: &str,
    ) -> Result<super::FetchedGitRelease, GitRemoteReleaseFetchError> {
        fetch_fixture_at_directory(
            remote,
            expected_commit,
            expected_checksum,
            canonical_url,
            None,
        )
    }

    fn fetch_fixture_at_directory(
        remote: &TestRemote,
        expected_commit: Option<&gix::ObjectId>,
        expected_checksum: Option<&str>,
        canonical_url: &str,
        directory: Option<&RepositoryPackageDirectory>,
    ) -> Result<super::FetchedGitRelease, GitRemoteReleaseFetchError> {
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(remote.tag.object_id().kind()));
        let refspec = super::exact_release_tag_refspec(&remote.tag, None).unwrap();
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);
        fetch_release_tree_in_repository(
            &repository,
            super::ReleaseFetchRequest {
                source_url: super::GitSourceUrl::test_transport(&remote.url(), canonical_url),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory,
                expected_commit,
                expected_source_checksum: expected_checksum,
                expected_locked_checksums: None,
                transfer_limit: &transfer_limit,
            },
        )
    }

    #[test]
    fn maps_advertisement_limit_failures_to_package_too_large() {
        let error = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded {
                limit: gix::protocol::handshake::refs::parse::AdvertisementLimit::RefCount,
                actual: 1_000_001,
            },
        );

        assert_eq!(
            map_transport_error(error),
            GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: 1_000_001,
            })
        );
        assert_eq!(
            GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: 1_000_001,
            })
            .diagnostic_code(),
            sai_diagnostics::Code::PackageTooLarge
        );
    }

    #[test]
    fn https_fetch_options_keep_repository_settings_and_add_transfer_limit() {
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = gix::ThreadSafeRepository::init_opts(
            &scratch.path,
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated().config_overrides(vec![gix::bstr::BString::from(
                "http.proxy=http://proxy.example.test:8080",
            )]),
        )
        .unwrap()
        .to_thread_local();
        let transfer_limit = gix_transport::client::blocking_io::FetchTransferLimit::new(12);

        let options = super::package_transport_options(
            &repository,
            "https://example.test/acme/package",
            Some(transfer_limit),
        )
        .unwrap()
        .downcast::<gix_transport::client::blocking_io::http::Options>()
        .expect("HTTPS uses the repository's HTTP transport options");

        assert_eq!(
            options.proxy.as_deref(),
            Some("http://proxy.example.test:8080")
        );
        assert_eq!(
            options
                .fetch_transfer_limit
                .as_ref()
                .map(|limit| limit.limit_bytes()),
            Some(12)
        );
    }

    #[test]
    fn refspec_requests_only_the_package_release_tag_prefix() {
        assert_eq!(
            release_tag_refspec(None).unwrap(),
            "refs/tags/v*:refs/tags/v*"
        );
        let directory = RepositoryPackageDirectory::parse("packages/text").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&directory)).unwrap(),
            "refs/tags/packages/text/v*:refs/tags/packages/text/v*"
        );
        let output_directory = RepositoryPackageDirectory::parse("out/debug").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&output_directory)).unwrap(),
            "refs/tags/out/debug/v*:refs/tags/out/debug/v*"
        );
        let state_directory = RepositoryPackageDirectory::parse(".sai/cache").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&state_directory)),
            Err(GitRemoteTagListError::InvalidPackageDirectory)
        );
    }

    #[test]
    fn selected_git_release_looks_up_only_the_exact_mvs_version_tag() {
        let remote = TestDirectory::new("many-release-tags");
        std::fs::create_dir_all(remote.0.join("refs/tags")).unwrap();
        std::fs::write(
            remote.0.join("config"),
            "[core]\n\tbare = true\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        std::fs::write(remote.0.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut refs = (0..=crate::git_tags::MAX_RELEASE_TAGS_PER_LISTING)
            .map(|patch| {
                (
                    format!("refs/tags/v1.2.{patch}"),
                    format!("{:040x}", patch.saturating_add(1)),
                )
            })
            .collect::<Vec<_>>();
        refs.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        std::fs::write(
            remote.0.join("packed-refs"),
            format!(
                "# pack-refs with: sorted\n{}\n",
                refs.iter()
                    .map(|(name, object_id)| format!("{object_id} {name}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
        std::fs::create_dir_all(remote.0.join("objects/info")).unwrap();
        std::fs::create_dir_all(remote.0.join("objects/pack")).unwrap();

        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, None);
        let remote_url = format!("file://{}", remote.0.display());
        let selected = expected_package_version("acme:sample", "1.2.3");
        let tag_name = release_tag_name(selected.version(), None).unwrap();
        let refspec = exact_release_tag_refspec_for_name(&tag_name);

        assert_eq!(refspec, "refs/tags/v1.2.3:refs/sai/package-release-tag");
        let tag = find_release_tag_in_repository(
            &repository,
            &remote_url,
            &refspec,
            &tag_name,
            selected.version(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(tag.name(), tag_name);
        assert_eq!(tag.version(), selected.version());
        assert_eq!(tag.object_id().to_hex().to_string(), format!("{:040x}", 4));

        assert_eq!(
            refs.len(),
            crate::git_tags::MAX_RELEASE_TAGS_PER_LISTING + 1
        );
        let missing = expected_package_version("acme:sample", "1.3.0");
        let missing_name = release_tag_name(missing.version(), None).unwrap();
        assert_eq!(
            find_release_tag_in_repository(
                &repository,
                &remote_url,
                &exact_release_tag_refspec_for_name(&missing_name),
                &missing_name,
                missing.version(),
                None,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn selected_git_release_uses_a_matching_lock_and_requires_one_for_lock_floors() {
        let selected = expected_package_version("acme:sample", "1.2.3");
        let source = ResolverPackageSource::Git {
            url: TEST_CANONICAL_GIT_URL.into(),
            directory: Some("packages/sample".into()),
        };
        let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let lock_package = LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                LockPackageField::Source(LockSource::Git(TEST_CANONICAL_GIT_URL.into())),
                LockPackageField::Directory("packages/sample".into()),
                LockPackageField::Commit(commit.to_hex().to_string().into()),
                LockPackageField::Checksum(format!("sai1:{}", "a".repeat(64)).into()),
            ]
            .into_iter()
            .map(|value| Spanned { value, span: 0..0 })
            .collect(),
        };

        let plan = plan_selected_git_release_fetch(
            &selected,
            &source,
            ResolverSourceReason::LockFloor,
            Some(&lock_package),
        )
        .unwrap();
        assert!(matches!(
            plan,
            SelectedGitReleaseFetchPlan::Locked(package) if package == &lock_package
        ));
        assert_eq!(
            plan_selected_git_release_fetch(
                &selected,
                &source,
                ResolverSourceReason::LockFloor,
                None,
            ),
            Err(SelectedGitReleaseFetchError::MissingLockedPackage(
                Box::new(selected.clone())
            ))
        );
    }

    #[test]
    fn locked_git_path_package_verifies_identity_and_source_checksum() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched_release = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let identity = fetched_release.source_identity().clone();
        let source = FetchedGitPackage {
            source_identity: identity.clone(),
            source: fetched_release.into_source_tree(),
        };
        let lock_package = LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                LockPackageField::Source(LockSource::Git(TEST_CANONICAL_GIT_URL.into())),
                LockPackageField::Commit(identity.commit_id().to_hex().to_string().into()),
                LockPackageField::Checksum(source.source().source_tree().checksum.clone().into()),
            ]
            .into_iter()
            .map(|value| Spanned { value, span: 0..0 })
            .collect(),
        };

        verify_locked_git_path_package(&lock_package, &identity, &source)
            .expect("matching lock identity and checksum verify");

        let mut bad_checksum = lock_package.clone();
        let checksum = bad_checksum
            .fields
            .iter_mut()
            .find(|field| matches!(&field.value, LockPackageField::Checksum(_)))
            .expect("lock contains a source checksum");
        checksum.value = LockPackageField::Checksum(format!("sai1:{}", "0".repeat(64)).into());
        assert!(matches!(
            verify_locked_git_path_package(&bad_checksum, &identity, &source),
            Err(GitRemoteReleaseFetchError::LockedChecksumMismatch { .. })
        ));

        let mut wrong_commit_identity = identity.clone();
        wrong_commit_identity.commit_id =
            gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            verify_locked_git_path_package(&lock_package, &wrong_commit_identity, &source),
            Err(GitRemoteReleaseFetchError::InvalidLockedPackageField(
                "Git path source identity"
            ))
        );
    }

    #[test]
    fn selected_git_release_does_not_reuse_a_lock_from_another_source() {
        let selected = expected_package_version("acme:sample", "1.2.3");
        let lock_package = LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                LockPackageField::Source(LockSource::Git(TEST_CANONICAL_GIT_URL.into())),
                LockPackageField::Commit("0123456789abcdef0123456789abcdef01234567".into()),
                LockPackageField::Checksum(format!("sai1:{}", "a".repeat(64)).into()),
            ]
            .into_iter()
            .map(|value| Spanned { value, span: 0..0 })
            .collect(),
        };
        let override_source = ResolverPackageSource::Git {
            url: "https://example.test/acme/fork.git".into(),
            directory: None,
        };

        let plan = plan_selected_git_release_fetch(
            &selected,
            &override_source,
            ResolverSourceReason::RootGitOverride,
            Some(&lock_package),
        )
        .unwrap();
        assert!(matches!(
            plan,
            SelectedGitReleaseFetchPlan::Tag { url, .. }
                if url.as_ref() == "https://example.test/acme/fork.git"
        ));
    }

    #[test]
    fn selected_git_release_rejects_path_sources() {
        let requiring = expected_package_version("acme:client", "2.0.0");
        let package = expected_package_version("acme:sibling", "1.2.3");
        let source = ResolverPackageSource::Path {
            relative_to: requiring,
            path: "../sibling".into(),
        };

        assert_eq!(
            plan_selected_git_release_fetch(&package, &source, ResolverSourceReason::PathPin, None,),
            Err(SelectedGitReleaseFetchError::NotGitSource)
        );
    }

    #[test]
    fn only_trusted_credential_keys_are_copied_from_user_configuration() {
        let config = gix::config::File::try_from(
            r#"
                [credential]
                    helper = store
                    helper =
                    useHttpPath = true
                [credential "https://example.com/team/repo"]
                    helper = "!command with spaces"
                    username = build-user
                [http]
                    proxy = http://ignored.example
                    extraHeader = Authorization: ignored
                [core]
                    sshCommand = ignored
                [url "https://mirror.example/"]
                    insteadOf = https://example.com/
            "#,
        )
        .unwrap();

        let values = credential_overrides_from_file(&config)
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                "credential.helper=store",
                "credential.helper=",
                "credential.useHttpPath=true",
                "credential.https://example.com/team/repo.helper=!command with spaces",
                "credential.https://example.com/team/repo.username=build-user",
            ]
        );
    }

    #[test]
    fn finds_the_standard_xdg_git_config_path() {
        let mut environment = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(OsString::from("/home/build/.config")),
            _ => None,
        };
        let paths = user_credential_config_paths(&mut environment, Some(Path::new("/home/build")));

        assert_eq!(
            paths.into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
            [
                PathBuf::from("/home/build/.config/git/config"),
                PathBuf::from("/home/build/.gitconfig"),
            ]
        );
    }

    #[test]
    fn extracts_only_credential_settings_from_trusted_user_includes() {
        let directory = TemporaryGitDirectory::create().unwrap();
        let user_config = directory.path.join(".gitconfig");
        std::fs::write(
            &user_config,
            "[include]\n\tpath = credentials.inc\n[http]\n\tsslVerify = false\n",
        )
        .unwrap();
        std::fs::write(
            directory.path.join("credentials.inc"),
            "[credential]\n\thelper = manager\n[core]\n\tsshCommand = ignored\n",
        )
        .unwrap();

        let values = credential_overrides_from_path(
            user_config,
            gix::config::Source::User,
            Some(&directory.path),
        )
        .unwrap()
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();

        assert_eq!(values, ["credential.helper=manager"]);
    }

    #[test]
    fn honors_only_the_documented_https_proxy_environment_variables() {
        let values = proxy_environment_overrides(|name| match name {
            "HTTPS_PROXY" => Some(OsString::from("proxy.internal:8080")),
            "https_proxy" => Some(OsString::from("ignored-lowercase:8080")),
            "NO_PROXY" => Some(OsString::from("localhost,.internal")),
            "HTTP_PROXY" | "ALL_PROXY" => Some(OsString::from("ignored")),
            _ => None,
        })
        .unwrap()
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                "http.proxy=proxy.internal:8080",
                "gitoxide.http.noProxy=localhost,.internal",
            ]
        );
    }

    #[test]
    fn ref_advertisements_do_not_fetch_tag_objects() {
        let id = "1111111111111111111111111111111111111111";
        let peeled = "2222222222222222222222222222222222222222";
        let test_dir = super::TemporaryGitDirectory::create().unwrap();
        let test_dir_path = &test_dir.path;
        std::fs::create_dir_all(test_dir_path.join("refs/tags")).unwrap();
        std::fs::write(
            test_dir_path.join("config"),
            "[core]\n\tbare = true\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        std::fs::write(test_dir_path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            test_dir_path.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled\n{id} refs/tags/v1.2.3\n^{peeled}\n"),
        )
        .unwrap();
        let object_dir = test_dir_path.join("objects");
        std::fs::create_dir_all(object_dir.join("info")).unwrap();
        std::fs::create_dir_all(object_dir.join("pack")).unwrap();

        let scratch = super::TemporaryGitDirectory::create().unwrap();
        let repository = gix::ThreadSafeRepository::init_opts(
            &scratch.path,
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        let remote = format!("file://{}", test_dir_path.display());
        let refspec = "refs/tags/v*:refs/tags/v*";
        let tags = list_release_tags_in_repository(&repository, &remote, refspec, None).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name(), "refs/tags/v1.2.3");
        assert_eq!(tags[0].object_id().to_string(), id);
        for entry in std::fs::read_dir(scratch.path.join("objects")).unwrap() {
            let entry = entry.unwrap();
            assert!(matches!(entry.file_name().to_str(), Some("info" | "pack")));
            assert!(entry.file_type().unwrap().is_dir());
            assert!(entry.path().read_dir().unwrap().next().is_none());
        }
    }

    #[test]
    fn fetches_an_exact_release_tag_peels_the_chain_and_checks_the_lock() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);

        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();

        assert_eq!(fetched.tag_name(), "refs/tags/v1.2.3");
        assert_eq!(fetched.tag_object_id(), Some(remote.tag.object_id()));
        assert!(!fetched.tag_missing());
        assert_eq!(fetched.commit_id(), &remote.commit_id);
        assert_eq!(
            fetched.source_identity().canonical_url(),
            TEST_CANONICAL_GIT_URL
        );
        assert_eq!(fetched.source_identity().commit_id(), &remote.commit_id);
        assert_eq!(fetched.source_identity().directory().as_str(), ".");
        assert!(fetched.source_tree().checksum.starts_with("sai1:"));
        assert!(fetched.source_tree().warnings.is_empty());
        assert!(fetched.package_manifest().is_syntax_valid());
        assert_eq!(fetched.package_manifest().role(), PackageRole::Dependency);
        assert_eq!(
            fetched
                .source_files()
                .iter()
                .map(crate::git_tree::GitPackageFile::path)
                .collect::<Vec<_>>(),
            ["lib.sai", "sai.package"]
        );
        assert_eq!(fetched.source_files()[0].content(), b"package source");
        assert_eq!(
            fetched.source_files()[1].content(),
            b"package acme:sample @ 1.2.3\n\
              source \"src\"\n\
              targets { wasm32-core }\n"
        );
    }

    #[test]
    fn rejects_noncanonical_source_identity_before_using_transport_url() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);

        let result = fetch_fixture_with_canonical_url(
            &remote,
            Some(&remote.commit_id),
            None,
            "https://EXAMPLE.test/acme/sample.git",
        );

        assert!(matches!(
            result,
            Err(GitRemoteReleaseFetchError::InvalidGitUrl)
        ));
    }

    fn root_contract_context() -> ResolverContractContext<'static> {
        let root = parse_for_role(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
            PackageRole::Root,
        );
        ResolverContractContext::from_root_manifest(
            &root,
            &BTreeMap::new(),
            &BTreeMap::new(),
            PackageValidationContext::default(),
        )
        .expect("valid root context")
        .into_owned()
    }

    fn path_discovery_root_context() -> ResolverContractContext<'static> {
        let root = parse_for_role(
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              overrides { acme:ignored use git \"https://example.test/acme/ignored.git\" }\n",
            PackageRole::Root,
        );
        ResolverContractContext::from_root_manifest(
            &root,
            &BTreeMap::new(),
            &BTreeMap::new(),
            PackageValidationContext::default(),
        )
        .expect("valid path-discovery root context")
        .into_owned()
    }

    fn expected_package_version(package: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(
            package,
            crate::version::Version::parse(version).expect("valid package version"),
        )
        .expect("valid package identity")
    }

    #[test]
    fn mixed_source_resolver_loads_local_git_and_git_sibling_path_packages() {
        let directory = TestDirectory::new("mixed-source");
        let workspace = directory.0.join("workspace");
        let root_path = workspace.join("root");
        let local_path = workspace.join("local");
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::create_dir_all(&local_path).unwrap();
        std::fs::write(
            root_path.join("sai.package"),
            b"package acme:root @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              requires {\n\
                acme:local @ ^1 from path \"../local\"\n\
                acme:client @ ^2 from git \"https://example.test/acme/sample.git\" dir \"packages/client\"\n\
              }\n",
        )
        .unwrap();
        std::fs::write(
            local_path.join("sai.package"),
            b"package acme:local @ 1.1.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .unwrap();

        let client_manifest = b"package acme:client @ 2.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n\
                               requires { acme:sibling @ ^1 from path \"../sibling\" }\n";
        let sibling_manifest = b"package acme:sibling @ 1.4.0\n\
                                source \"main.sai\"\n\
                                targets { wasm32-core }\n";
        let mut remote = TestRemote::new("refs/tags/v2.0.0", false);
        let commit_id =
            remote.commit_with_client_and_sibling_packages(client_manifest, sibling_manifest);
        let repository =
            gix::open_opts(&remote.directory.path, gix::open::Options::isolated()).unwrap();
        let tag_id = write_test_tag(&repository, commit_id, "v2.0.0");
        std::fs::write(
            remote.directory.path.join("refs/tags/v2.0.0"),
            format!("{tag_id}\n"),
        )
        .unwrap();
        remote.tag = parse_release_tag_refs([("refs/tags/v2.0.0", tag_id)], None)
            .unwrap()
            .remove(0);
        remote.commit_id = commit_id;

        let client_directory = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let fetched_release = fetch_fixture_at_directory(
            &remote,
            Some(&commit_id),
            None,
            TEST_CANONICAL_GIT_URL,
            Some(&client_directory),
        )
        .unwrap();
        let client_identity = fetched_release.source_identity().clone();
        let sibling_identity = client_identity
            .resolve_dependency_path("../sibling")
            .unwrap();
        let reader = GitTreeReader::open(
            repository.git_dir().join("objects"),
            repository.object_hash(),
        )
        .unwrap();

        let root_directory =
            LocalPackageDirectory::open(&root_path).expect("open local root package");
        let local_directory =
            LocalPackageDirectory::open(&local_path).expect("open local dependency");
        let mut fetched_release = Some(fetched_release);
        let sibling_source = reader
            .read_commit_repository_package_source_tree(
                sibling_identity.commit_id(),
                sibling_identity.directory(),
            )
            .unwrap();
        let locked_sibling = LockPackage {
            package: "acme:sibling".into(),
            version: crate::version::Version::parse("1.4.0").unwrap(),
            fields: [
                LockPackageField::Source(LockSource::Git(TEST_CANONICAL_GIT_URL.into())),
                LockPackageField::Directory("packages/sibling".into()),
                LockPackageField::Commit(sibling_identity.commit_id().to_hex().to_string().into()),
                LockPackageField::Checksum(sibling_source.source_tree().checksum.clone().into()),
            ]
            .into_iter()
            .map(|value| Spanned { value, span: 0..0 })
            .collect(),
        };
        let locked_packages = std::collections::HashMap::from([(
            expected_package_version("acme:sibling", "1.4.0"),
            locked_sibling,
        )]);
        let mut git_release_fetches = 0;
        let mut sibling_fetches = 0;
        let output = resolve_mixed_source_packages_with_fetchers(
            root_directory.clone(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &locked_packages,
            PackageValidationContext::default(),
            |selection, locked_package| {
                git_release_fetches += 1;
                assert!(locked_package.is_none());
                assert!(matches!(
                    selection.source(),
                    ResolverPackageSource::Git { url, directory }
                        if url.as_ref() == TEST_CANONICAL_GIT_URL
                            && directory.as_deref() == Some("packages/client")
                ));
                Ok(fetched_release
                    .take()
                    .expect("selected Git release is fetched only once"))
            },
            |identity, locked_package| {
                sibling_fetches += 1;
                let locked_package =
                    locked_package.expect("the exact Git sibling identity uses its lock entry");
                assert_eq!(identity, &sibling_identity);
                let source = reader
                    .read_commit_repository_package_source_tree(
                        identity.commit_id(),
                        identity.directory(),
                    )
                    .unwrap();
                let fetched = FetchedGitPackage {
                    source_identity: identity.clone(),
                    source,
                };
                super::verify_locked_git_path_package(locked_package, identity, &fetched)?;
                Ok(fetched)
            },
        )
        .expect("mixed local and Git graph resolves");

        let local_version = expected_package_version("acme:local", "1.1.0");
        let client_version = expected_package_version("acme:client", "2.0.0");
        let sibling_version = expected_package_version("acme:sibling", "1.4.0");
        let selected = output.provider().output().selection().selected();
        assert_eq!(
            selected.get(&local_version.node()),
            Some(local_version.version())
        );
        assert_eq!(
            selected.get(&client_version.node()),
            Some(client_version.version())
        );
        assert_eq!(
            selected.get(&sibling_version.node()),
            Some(sibling_version.version())
        );
        assert_eq!(git_release_fetches, 1);
        assert_eq!(sibling_fetches, 1);
        assert!(fetched_release.is_none());

        let identities = output.provider().source_identities();
        assert_eq!(
            identities.get(&MvsRequirer::Root(expected_package_version(
                "acme:root",
                "1.0.0"
            ))),
            Some(&MixedPackageSourceIdentity::Local(root_directory))
        );
        assert_eq!(
            identities.get(&MvsRequirer::Package(local_version.clone())),
            Some(&MixedPackageSourceIdentity::Local(local_directory))
        );
        assert_eq!(
            identities.get(&MvsRequirer::Package(client_version.clone())),
            Some(&MixedPackageSourceIdentity::Git(client_identity.clone()))
        );
        assert_eq!(
            identities.get(&MvsRequirer::Package(sibling_version.clone())),
            Some(&MixedPackageSourceIdentity::Git(sibling_identity.clone()))
        );
        assert_eq!(
            output
                .git_identities()
                .get(&MvsRequirer::Package(client_version.clone())),
            Some(&client_identity)
        );
        assert_eq!(
            output
                .git_identities()
                .get(&MvsRequirer::Package(sibling_version.clone())),
            Some(&sibling_identity)
        );
        let sibling_path_key =
            crate::resolver_contract::ResolverPathKey::package(client_version, "../sibling");
        assert_eq!(
            output
                .git_path_requirements()
                .get(&sibling_path_key.requirer)
                .and_then(|index| index.package_version(&sibling_path_key)),
            Some(&sibling_version)
        );

        let final_sources = output
            .plan_final_sources(&std::collections::HashMap::new())
            .expect("final sources can be planned");
        assert!(final_sources.failures().is_empty());
        assert!(final_sources.root_cycles().is_empty());
        assert_eq!(final_sources.entries().len(), 3);
        assert!(
            final_sources
                .entries()
                .iter()
                .all(|entry| entry.read_source() == entry.final_source())
        );
    }

    #[test]
    fn lock_merge_resolution_reuses_the_record_for_the_selected_git_source() {
        const SOURCE_A: &str = "https://example.test/acme/dep-a.git";
        const SOURCE_B: &str = "https://example.test/acme/dep-b.git";
        let lock_entry = |url: &str, commit: char, checksum: char| {
            format!(
                "package acme:dep @ 1.4.0\n    source git \"{url}\"\n    commit \"{}\"\n    checksum \"sai1:{}\"\n    capabilities {{ }}\n",
                commit.to_string().repeat(40),
                checksum.to_string().repeat(64),
            )
        };
        let lock_text = format!(
            "lock 1\n\nroot acme:root @ 1.0.0\n    targets {{ wasm32-core }}\n    requires acme:dep @ >=1.4.0 -> 1.4.0\n<<<<<<< ours\n{}=======\n{}>>>>>>> theirs\n",
            lock_entry(SOURCE_A, 'a', '1'),
            lock_entry(SOURCE_B, 'b', '2'),
        );
        let conflict_sides = reconstruct_conflict_sides(lock_text.as_bytes())
            .expect("lock conflict markers are balanced")
            .expect("the fixture contains both lock sides");
        let lock_sides = validate_conflict_sides(&conflict_sides, &Default::default())
            .expect("both source variants are valid lock entries");

        let directory = TestDirectory::new("lock-merge-source-selection");
        let root_path = directory.0.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::write(
            root_path.join("sai.package"),
            format!(
                "package acme:root @ 1.0.0\nsource \"src\"\ntargets {{ wasm32-core }}\nrequires {{ acme:dep @ >=1.4.0 from git \"{SOURCE_B}\" }}\n"
            ),
        )
        .unwrap();
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.4.0",
            false,
            gix::hash::Kind::Sha1,
            Some(
                b"package acme:dep @ 1.4.0\n\
                  source \"src\"\n\
                  targets { wasm32-core }\n",
            ),
        );
        let mut release = Some(
            fetch_fixture_with_canonical_url(&remote, Some(&remote.commit_id), None, SOURCE_B)
                .unwrap(),
        );
        let mut fetches = 0;
        let output = resolve_mixed_source_packages_from_lock_merge_with_fetchers(
            LocalPackageDirectory::open(&root_path).expect("open the local root package"),
            &lock_sides,
            PackageValidationContext::default(),
            |selection, locked_package| {
                fetches += 1;
                assert_eq!(selection.package_version().package(), "acme:dep");
                assert_eq!(selection.package_version().version().to_string(), "1.4.0");
                assert!(matches!(
                    selection.source(),
                    ResolverPackageSource::Git { url, directory: None }
                        if url.as_ref() == SOURCE_B
                ));
                let locked_package =
                    locked_package.expect("the selected source has a matching lock record");
                let expected_commit = "b".repeat(40);
                assert!(locked_package.fields.iter().any(|field| matches!(
                    &field.value,
                    LockPackageField::Source(LockSource::Git(url)) if url.as_ref() == SOURCE_B
                )));
                assert!(locked_package.fields.iter().any(|field| matches!(
                    &field.value,
                    LockPackageField::Commit(commit) if commit.as_ref() == expected_commit
                )));
                Ok(release.take().expect("the dependency is loaded once"))
            },
            |_, _| unreachable!("the fixture has no Git-relative package requirements"),
        )
        .expect("the selected source resolves from the merged lock");

        assert_eq!(fetches, 1);
        assert!(release.is_none());
        assert!(
            output
                .plan_final_sources_from_lock()
                .expect("retained lock sources plan the final graph")
                .failures()
                .is_empty()
        );
        let audit = output
            .plan_final_source_audit_from_lock(
                PackageValidationContext::default(),
                |_, _| unreachable!("the final source is the source read during MVS"),
                |_, _| unreachable!("the fixture has no Git-relative package requirements"),
            )
            .expect("the lock catalog also supports final-source auditing");
        assert!(audit.is_equivalent());
    }

    #[test]
    fn lock_floor_merge_catalog_reuses_final_source_record_during_audit_reload() {
        const SOURCE_A: &str = "https://example.test/acme/dep-a.git";
        const SOURCE_B: &str = "https://example.test/acme/dep-b.git";
        const HELPER_SOURCE: &str = "https://example.test/acme/z-helper.git";
        let lock_entry = |url: &str, commit: char, checksum: char| {
            format!(
                "package acme:dep @ 1.4.0\n    source git \"{url}\"\n    commit \"{}\"\n    checksum \"sai1:{}\"\n    capabilities {{ }}\n",
                commit.to_string().repeat(40),
                checksum.to_string().repeat(64),
            )
        };
        let lock_text = format!(
            "lock 1\n\nroot acme:root @ 1.0.0\n    targets {{ wasm32-core }}\n<<<<<<< ours\n{}=======\n{}>>>>>>> theirs\n",
            lock_entry(SOURCE_A, 'a', '1'),
            lock_entry(SOURCE_B, 'b', '2'),
        );
        let conflict_sides = reconstruct_conflict_sides(lock_text.as_bytes())
            .expect("lock conflict markers are balanced")
            .expect("the fixture contains both lock sides");
        let lock_sides = validate_conflict_sides(&conflict_sides, &Default::default())
            .expect("both source variants are valid lock entries");

        let directory = TestDirectory::new("lock-merge-floor-final-audit-source");
        let root_path = directory.0.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::write(
            root_path.join("sai.package"),
            format!(
                "package acme:root @ 1.0.0\n\
                 source \"src\"\n\
                 targets {{ wasm32-core }}\n\
                 requires {{\n\
                   acme:dep @ ^1 from git \"{SOURCE_A}\"\n\
                   acme:z-helper @ ^1 from git \"{HELPER_SOURCE}\"\n\
                 }}\n"
            ),
        )
        .unwrap();

        let dep_named_manifest = b"package acme:dep @ 1.0.0\n\
                                  source \"src\"\n\
                                  targets { wasm32-core }\n";
        let dep_floor_manifest = b"package acme:dep @ 1.4.0\n\
                                  source \"src\"\n\
                                  targets { wasm32-core }\n";
        let dep_a_named_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(dep_named_manifest),
        );
        let dep_a_floor_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.4.0",
            false,
            gix::hash::Kind::Sha1,
            Some(dep_floor_manifest),
        );
        let dep_b_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.4.0",
            false,
            gix::hash::Kind::Sha1,
            Some(dep_floor_manifest),
        );
        let helper_manifest = format!(
            "package acme:z-helper @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core }}\n\
             requires {{ acme:dep @ =1.4.0 from git \"{SOURCE_B}\" }}\n"
        );
        let helper_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(helper_manifest.as_bytes()),
        );
        let mut dep_a_named_release = Some(
            fetch_fixture_with_canonical_url(
                &dep_a_named_remote,
                Some(&dep_a_named_remote.commit_id),
                None,
                SOURCE_A,
            )
            .unwrap(),
        );
        let mut dep_a_floor_release = Some(
            fetch_fixture_with_canonical_url(
                &dep_a_floor_remote,
                Some(&dep_a_floor_remote.commit_id),
                None,
                SOURCE_A,
            )
            .unwrap(),
        );
        let mut dep_b_release = Some(
            fetch_fixture_with_canonical_url(
                &dep_b_remote,
                Some(&dep_b_remote.commit_id),
                None,
                SOURCE_B,
            )
            .unwrap(),
        );
        let mut helper_release = Some(
            fetch_fixture_with_canonical_url(
                &helper_remote,
                Some(&helper_remote.commit_id),
                None,
                HELPER_SOURCE,
            )
            .unwrap(),
        );

        let mut dep_read_fetches = 0;
        let mut helper_read_fetches = 0;
        let output = resolve_mixed_source_packages_from_lock_merge_with_fetchers(
            LocalPackageDirectory::open(&root_path).expect("open the local root package"),
            &lock_sides,
            PackageValidationContext::default(),
            |selection, locked_package| match selection.package_version().package() {
                "acme:dep" => {
                    dep_read_fetches += 1;
                    assert!(matches!(
                        selection.source(),
                        ResolverPackageSource::Git { url, directory: None }
                            if url.as_ref() == SOURCE_A
                    ));
                    if selection.package_version().version().to_string() == "1.0.0" {
                        assert_eq!(selection.reason(), ResolverSourceReason::NamedRequirement);
                        assert!(locked_package.is_none());
                        Ok(dep_a_named_release
                            .take()
                            .expect("the named candidate is read once"))
                    } else {
                        assert_eq!(selection.package_version().version().to_string(), "1.4.0");
                        assert_eq!(selection.reason(), ResolverSourceReason::LockFloor);
                        let locked_package =
                            locked_package.expect("the floor source has its merged lock record");
                        assert!(locked_package.fields.iter().any(|field| matches!(
                        &field.value,
                        LockPackageField::Source(LockSource::Git(url)) if url.as_ref() == SOURCE_A
                    )));
                        assert!(locked_package.fields.iter().any(|field| matches!(
                            &field.value,
                            LockPackageField::Commit(commit) if commit.as_ref() == "a".repeat(40)
                        )));
                        Ok(dep_a_floor_release
                            .take()
                            .expect("the floor-selected source is read once"))
                    }
                }
                "acme:z-helper" => {
                    helper_read_fetches += 1;
                    assert_eq!(selection.reason(), ResolverSourceReason::NamedRequirement);
                    assert!(matches!(
                        selection.source(),
                        ResolverPackageSource::Git { url, directory: None }
                            if url.as_ref() == HELPER_SOURCE
                    ));
                    assert!(locked_package.is_none());
                    Ok(helper_release
                        .take()
                        .expect("the helper source is read once"))
                }
                package => panic!("unexpected Git package request for {package}"),
            },
            |_, _| unreachable!("the fixture has no Git-relative package requirements"),
        )
        .expect("the lock floor resolves from its canonical source");

        assert_eq!(dep_read_fetches, 2);
        assert_eq!(helper_read_fetches, 1);
        let final_sources = output
            .plan_final_sources_from_lock()
            .expect("final sources can be planned from both lock sides");
        let dep_version = expected_package_version("acme:dep", "1.4.0");
        let dep_final_source = final_sources
            .entries()
            .iter()
            .find(|entry| entry.package_version() == &dep_version)
            .expect("the selected dependency has a final source");
        assert_eq!(
            dep_final_source.read_source().reason(),
            ResolverSourceReason::LockFloor
        );
        assert!(matches!(
            dep_final_source.final_source().source(),
            ResolverPackageSource::Git { url, directory: None }
                if url.as_ref() == SOURCE_B
        ));

        let mut final_reload_fetches = 0;
        let audit = output
            .plan_final_source_audit_from_lock(
                PackageValidationContext::default(),
                |selection, locked_package| {
                    final_reload_fetches += 1;
                    assert_eq!(selection.package_version(), &dep_version);
                    assert_eq!(selection.reason(), ResolverSourceReason::NamedRequirement);
                    assert!(matches!(
                        selection.source(),
                        ResolverPackageSource::Git { url, directory: None }
                            if url.as_ref() == SOURCE_B
                    ));
                    let locked_package =
                        locked_package.expect("the final source has its exact merged lock record");
                    assert!(locked_package.fields.iter().any(|field| matches!(
                        &field.value,
                        LockPackageField::Source(LockSource::Git(url)) if url.as_ref() == SOURCE_B
                    )));
                    assert!(locked_package.fields.iter().any(|field| matches!(
                        &field.value,
                        LockPackageField::Commit(commit) if commit.as_ref() == "b".repeat(40)
                    )));
                    Ok(dep_b_release
                        .take()
                        .expect("the final source reload runs once"))
                },
                |_, _| unreachable!("the fixture has no Git-relative package requirements"),
            )
            .expect("the final source can be reloaded from its matching lock record");
        assert_eq!(final_reload_fetches, 1);
        assert!(audit.is_equivalent());
    }

    #[test]
    fn mixed_source_final_audit_reloads_late_path_pins_and_compares_contracts() {
        const TARGET_URL: &str = "https://example.test/acme/target.git";
        const SHARED_URL: &str = "https://example.test/acme/shared.git";
        const OTHER_SHARED_URL: &str = "https://example.test/acme/other-shared.git";
        let git_target_manifest = format!(
            "package acme:target @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{ acme:shared @ ^1 from git \"{SHARED_URL}\" }}\n"
        );
        let shared_manifest = b"package acme:shared @ 1.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n";

        for (case_name, local_shared_url, expected_equivalent) in [
            ("same-contract", SHARED_URL, true),
            ("different-contract", OTHER_SHARED_URL, false),
        ] {
            let directory = TestDirectory::new(case_name);
            let workspace = directory.0.join("workspace");
            let root_path = workspace.join("root");
            let late_path = workspace.join("z_late");
            let target_path = workspace.join("target");
            std::fs::create_dir_all(&root_path).unwrap();
            std::fs::create_dir_all(&late_path).unwrap();
            std::fs::create_dir_all(&target_path).unwrap();
            std::fs::write(
                root_path.join("sai.package"),
                format!(
                    "package acme:root @ 1.0.0\n\
                     source \"src\"\n\
                     targets {{ wasm32-core }}\n\
                     requires {{\n\
                       acme:target @ ^1 from git \"{TARGET_URL}\"\n\
                     acme:z-late @ ^1 from path \"../z_late\"\n\
                     }}\n"
                ),
            )
            .unwrap();
            std::fs::write(
                late_path.join("sai.package"),
                b"package acme:z-late @ 1.0.0\n\
                  source \"main.sai\"\n\
                  targets { wasm32-core }\n\
                  requires { acme:target @ ^1 from path \"../target\" }\n",
            )
            .unwrap();
            std::fs::write(
                target_path.join("sai.package"),
                format!(
                    "package acme:target @ 1.0.0\n\
                     source \"main.sai\"\n\
                     targets {{ wasm32-core }}\n\
                     requires {{ acme:shared @ ^1 from git \"{local_shared_url}\" }}\n"
                ),
            )
            .unwrap();

            let target_remote = TestRemote::new_with_package_manifest(
                "refs/tags/v1.0.0",
                false,
                gix::hash::Kind::Sha1,
                Some(git_target_manifest.as_bytes()),
            );
            let shared_remote = TestRemote::new_with_package_manifest(
                "refs/tags/v1.0.0",
                false,
                gix::hash::Kind::Sha1,
                Some(shared_manifest),
            );
            let mut target_release = Some(
                fetch_fixture_with_canonical_url(
                    &target_remote,
                    Some(&target_remote.commit_id),
                    None,
                    TARGET_URL,
                )
                .unwrap(),
            );
            let mut shared_release = Some(
                fetch_fixture_with_canonical_url(
                    &shared_remote,
                    Some(&shared_remote.commit_id),
                    None,
                    SHARED_URL,
                )
                .unwrap(),
            );
            let root_directory = LocalPackageDirectory::open(&root_path).unwrap();
            let output = resolve_mixed_source_packages_with_fetchers(
                root_directory,
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |selection, locked_package| {
                    assert!(locked_package.is_none());
                    match selection.package_version().package() {
                        "acme:target" => Ok(target_release.take().unwrap()),
                        "acme:shared" => Ok(shared_release.take().unwrap()),
                        package => panic!("unexpected Git package request for {package}"),
                    }
                },
                |_, _| unreachable!("this fixture has no Git-relative path requirements"),
            )
            .expect("the read-time MVS graph resolves");

            let target_version = expected_package_version("acme:target", "1.0.0");
            let sources = output
                .plan_final_sources(&std::collections::HashMap::new())
                .expect("final source plan is available");
            let target = sources
                .entries()
                .iter()
                .find(|entry| entry.package_version() == &target_version)
                .expect("target has a final source entry");
            assert_eq!(
                target.read_source().reason(),
                ResolverSourceReason::NamedRequirement
            );
            assert_eq!(
                target.final_source().reason(),
                ResolverSourceReason::PathPin
            );
            assert_ne!(target.read_source(), target.final_source());

            let audit_plan = output
                .plan_final_source_audit(
                    &std::collections::HashMap::new(),
                    &std::collections::HashMap::new(),
                    PackageValidationContext::default(),
                    |_, _| unreachable!("the selected Git releases were already captured"),
                    |_, _| unreachable!("this fixture has no Git-relative path requirements"),
                )
                .expect("the final source identities are plannable");
            assert_eq!(audit_plan.source_identities().reloads().len(), 1);
            assert!(audit_plan.reload_failures().is_empty());
            assert!(audit_plan.audit().is_complete());
            assert_eq!(audit_plan.is_equivalent(), expected_equivalent);
            assert_eq!(
                audit_plan.audit().has_divergent_sources(),
                !expected_equivalent
            );
        }
    }

    #[test]
    fn mixed_source_final_git_path_failure_retains_manifest_edge() {
        const TARGET_URL: &str = "https://example.test/acme/target.git";
        const LATE_URL: &str = "https://example.test/acme/late.git";
        let read_time_target_manifest = b"package acme:target @ 1.0.0\n\
                                         source \"main.sai\"\n\
                                         targets { wasm32-core }\n";
        let final_target_manifest = b"package acme:target @ 1.0.0\n\
                                     source \"main.sai\"\n\
                                     targets { wasm32-core, wasm32-invalid }\n\
                                     requires { acme:outside @ ^1 from path \"../../../outside\" }\n";
        let late_manifest = b"package acme:z-late @ 1.0.0\n\
                              source \"main.sai\"\n\
                              targets { wasm32-core }\n\
                              requires { acme:target @ ^1 from path \"packages/target\" }\n";
        let directory = TestDirectory::new("mixed-source-final-git-path-failure");
        let workspace = directory.0.join("workspace");
        let root_path = workspace.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        let root_manifest = format!(
            "package acme:root @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{\n\
               acme:target @ ^1 from git \"{TARGET_URL}\"\n\
               acme:z-late @ ^1 from git \"{LATE_URL}\"\n\
             }}\n"
        );
        std::fs::write(root_path.join("sai.package"), &root_manifest).unwrap();

        let target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(read_time_target_manifest),
        );
        let late_remote = TestRemote::new_with_sibling_target_manifests(
            "refs/tags/v1.0.0",
            late_manifest,
            final_target_manifest,
        );
        let mut target_release = Some(
            fetch_fixture_with_canonical_url(
                &target_remote,
                Some(&target_remote.commit_id),
                None,
                TARGET_URL,
            )
            .unwrap(),
        );
        let mut late_release = Some(
            fetch_fixture_with_canonical_url(
                &late_remote,
                Some(&late_remote.commit_id),
                None,
                LATE_URL,
            )
            .unwrap(),
        );
        let target_directory = RepositoryPackageDirectory::parse("packages/target").unwrap();
        let output = resolve_mixed_source_packages_with_fetchers(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            PackageValidationContext::default(),
            |selection, locked_package| {
                assert!(locked_package.is_none());
                match selection.package_version().package() {
                    "acme:target" => Ok(target_release.take().unwrap()),
                    "acme:z-late" => Ok(late_release.take().unwrap()),
                    package => panic!("unexpected Git package request for {package}"),
                }
            },
            |identity, locked_package| {
                assert!(locked_package.is_none());
                assert_eq!(identity.canonical_url(), LATE_URL);
                assert_eq!(identity.directory(), &target_directory);
                fetch_fixture_at_directory(
                    &late_remote,
                    Some(identity.commit_id()),
                    None,
                    LATE_URL,
                    Some(&target_directory),
                )
                .map(|release| super::FetchedGitPackage {
                    source_identity: release.source_identity,
                    source: release.source,
                })
            },
        )
        .expect("the read-time MVS graph resolves from the original sources");

        let audit_plan = output
            .plan_final_source_audit(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("the final target source is a Git path pin"),
                |identity, locked_package| {
                    assert!(locked_package.is_none());
                    assert_eq!(identity.canonical_url(), LATE_URL);
                    assert_eq!(identity.directory(), &target_directory);
                    fetch_fixture_at_directory(
                        &late_remote,
                        Some(identity.commit_id()),
                        None,
                        LATE_URL,
                        Some(&target_directory),
                    )
                    .map(|release| super::FetchedGitPackage {
                        source_identity: release.source_identity,
                        source: release.source,
                    })
                },
            )
            .expect("the final source plan retains reload failures");
        let target_version = expected_package_version("acme:target", "1.0.0");
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_target_manifest
        );
        let failure = audit_plan
            .reload_failures()
            .iter()
            .find(|failure| {
                failure.package_version.as_ref() == Some(&target_version)
                    && matches!(
                        &failure.error,
                        crate::mixed_source::MixedSourceFinalReloadError::GitPathDiscovery(_)
                    )
            })
            .expect("the final Git path failure is retained");
        let crate::mixed_source::MixedSourceFinalReloadError::GitPathDiscovery(error) =
            &failure.error
        else {
            panic!(
                "expected a Git path-discovery failure, got {:#?}",
                failure.error
            );
        };
        assert_eq!(
            error.requirement_edge().map(|edge| edge.package()),
            Some("acme:outside")
        );
        let edge = error.requirement_edge().unwrap();
        assert_eq!(edge.path(), "../../../outside");
        assert_eq!(
            &final_target_manifest[edge.span().clone()],
            b"acme:outside @ ^1 from path \"../../../outside\""
        );
        assert_eq!(
            failure.error.diagnostic_code(),
            Some(sai_diagnostics::Code::PathEscapesRepository)
        );
        assert!(
            audit_plan.reload_failures().iter().any(|failure| {
                failure.package_version.as_ref() == Some(&target_version)
                    && matches!(
                        &failure.error,
                        crate::mixed_source::MixedSourceFinalReloadError::GitContract(error)
                            if matches!(
                                error.as_ref(),
                                GitRemoteReleaseContractError::InvalidContract(
                                    crate::resolver_contract::ResolverContractBuildError::SemanticValidationErrors(errors)
                                ) if errors.iter().any(|error| matches!(
                                    &error.kind,
                                    crate::validation::PackageValidationErrorKind::UnknownTarget(target)
                                        if target.as_ref() == "wasm32-invalid"
                                ))
                            )
                    )
            }),
            "the final Git contract retains the independent semantic validation error"
        );

        let root_version = expected_package_version("acme:root", "1.0.0");
        let late_version = expected_package_version("acme:z-late", "1.0.0");
        let root_file_id = sai_diagnostics::FileId::new(51);
        let target_file_id = sai_diagnostics::FileId::new(52);
        let late_file_id = sai_diagnostics::FileId::new(53);
        let final_file_id = sai_diagnostics::FileId::new(54);
        let snapshot = sai_vfs::WorkspaceSnapshot::new([
            sai_vfs::SnapshotFile::new(
                std::sync::Arc::new(
                    sai_vfs::SourceFile::decode(root_file_id, root_manifest.as_bytes()).unwrap(),
                ),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            sai_vfs::SnapshotFile::new(
                std::sync::Arc::new(
                    sai_vfs::SourceFile::decode(
                        target_file_id,
                        read_time_target_manifest.as_slice(),
                    )
                    .unwrap(),
                ),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
            sai_vfs::SnapshotFile::new(
                std::sync::Arc::new(
                    sai_vfs::SourceFile::decode(late_file_id, late_manifest.as_slice()).unwrap(),
                ),
                "dep:acme:z-late@1.0.0/sai.package",
                Some("acme:z-late@1.0.0".to_owned()),
                Some(1),
            ),
            sai_vfs::SnapshotFile::new(
                std::sync::Arc::new(
                    sai_vfs::SourceFile::decode(final_file_id, final_target_manifest.as_slice())
                        .unwrap(),
                ),
                crate::mvs_diagnostics::MvsManifestLocation::final_snapshot_path(
                    &target_version,
                    0,
                ),
                Some("acme:target@1.0.0".to_owned()),
                None,
            ),
        ])
        .unwrap();
        let mut manifests = crate::mvs_diagnostics::MvsManifestLocations::default();
        manifests
            .set_root(
                crate::mvs_diagnostics::MvsManifestLocation::from_snapshot(
                    &snapshot,
                    root_file_id,
                    &root_version,
                    PackageRole::Root,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_package(
                crate::mvs_diagnostics::MvsManifestLocation::from_snapshot(
                    &snapshot,
                    target_file_id,
                    &target_version,
                    PackageRole::Dependency,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_package(
                crate::mvs_diagnostics::MvsManifestLocation::from_snapshot(
                    &snapshot,
                    late_file_id,
                    &late_version,
                    PackageRole::Dependency,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_final_package(
                crate::mvs_diagnostics::MvsManifestLocation::from_final_snapshot(
                    &snapshot,
                    final_file_id,
                    &target_version,
                )
                .unwrap(),
            )
            .unwrap();
        let diagnostics = crate::mvs_diagnostics::mixed_source_reload_failure_diagnostics(
            &output,
            &audit_plan,
            &manifests,
        )
        .unwrap();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        let path_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::PathEscapesRepository)
            .expect("the independent final Git path failure is reported");
        let semantic_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "UNKNOWN_TARGET")
            .expect("the final Git semantic error is reported");
        assert_eq!(
            path_diagnostic.primary_label,
            "this path dependency could not be resolved in the final source"
        );
        let primary = path_diagnostic
            .primary()
            .expect("the diagnostic points into the final Git manifest");
        assert_eq!(primary.span.file, final_file_id);
        assert_eq!(
            &snapshot
                .file(primary.span.file)
                .unwrap()
                .source()
                .raw_bytes()
                [primary.span.range.start().to_usize()..primary.span.range.end().to_usize()],
            b"acme:outside @ ^1 from path \"../../../outside\""
        );
        assert_eq!(
            snapshot
                .file(primary.span.file)
                .unwrap()
                .source()
                .raw_bytes(),
            final_target_manifest
        );
        let semantic_primary = semantic_diagnostic
            .primary()
            .expect("the semantic diagnostic points into the final Git manifest");
        assert_eq!(semantic_primary.span.file, final_file_id);
        assert_eq!(
            &snapshot
                .file(semantic_primary.span.file)
                .unwrap()
                .source()
                .raw_bytes()[semantic_primary.span.range.start().to_usize()
                ..semantic_primary.span.range.end().to_usize()],
            b"wasm32-invalid"
        );
        assert!(
            semantic_diagnostic.data.contains_key("requirement_chain"),
            "the final Git semantic error retains the original MVS chain"
        );
    }

    #[test]
    fn mixed_source_final_git_release_path_failure_retains_manifest_edge() {
        const TARGET_URL: &str = "https://example.test/acme/target.git";
        const LATE_URL: &str = "https://example.test/acme/late.git";
        const FINAL_URL: &str = "https://example.test/acme/final-target.git";
        let read_time_target_manifest = b"package acme:target @ 1.0.0\n\
                                         source \"main.sai\"\n\
                                         targets { wasm32-core }\n";
        let late_manifest = format!(
            "package acme:z-late @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{ acme:target @ ^1 from git \"{FINAL_URL}\" }}\n"
        );
        let final_target_manifest = b"package acme:target @ 1.0.0\n\
                                     source \"main.sai\"\n\
                                     targets { wasm32-core }\n\
                                     requires { acme:outside @ ^1 from path \"../../../outside\" }\n";
        let directory = TestDirectory::new("mixed-source-final-git-release-path-failure");
        let workspace = directory.0.join("workspace");
        let root_path = workspace.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        let root_manifest = format!(
            "package zz:root @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{\n\
               acme:target @ ^1 from git \"{TARGET_URL}\"\n\
               acme:z-late @ ^1 from git \"{LATE_URL}\"\n\
             }}\n"
        );
        std::fs::write(root_path.join("sai.package"), &root_manifest).unwrap();

        let target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(read_time_target_manifest),
        );
        let late_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(late_manifest.as_bytes()),
        );
        let final_target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(final_target_manifest),
        );
        let mut target_release = Some(
            fetch_fixture_with_canonical_url(
                &target_remote,
                Some(&target_remote.commit_id),
                None,
                TARGET_URL,
            )
            .unwrap(),
        );
        let mut late_release = Some(
            fetch_fixture_with_canonical_url(
                &late_remote,
                Some(&late_remote.commit_id),
                None,
                LATE_URL,
            )
            .unwrap(),
        );
        let output = resolve_mixed_source_packages_with_fetchers(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            PackageValidationContext::default(),
            |selection, locked_package| {
                assert!(locked_package.is_none());
                match selection.package_version().package() {
                    "acme:target" => Ok(target_release.take().unwrap()),
                    "acme:z-late" => Ok(late_release.take().unwrap()),
                    package => panic!("unexpected Git package request for {package}"),
                }
            },
            |_, _| unreachable!("this fixture has no Git-relative path requirements"),
        )
        .expect("the read-time MVS graph resolves from its original Git sources");

        let target_version = expected_package_version("acme:target", "1.0.0");
        let successful_final_target_manifest = format!(
            "// final source snapshot\n{}",
            String::from_utf8_lossy(read_time_target_manifest)
        )
        .into_bytes();
        let successful_final_target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(&successful_final_target_manifest),
        );
        let mut successful_final_target_release = Some(
            fetch_fixture_with_canonical_url(
                &successful_final_target_remote,
                Some(&successful_final_target_remote.commit_id),
                None,
                FINAL_URL,
            )
            .unwrap(),
        );
        let successful_audit_plan = output
            .plan_final_source_audit(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |selection, locked_package| {
                    assert!(locked_package.is_none());
                    assert_eq!(selection.package_version().package(), "acme:target");
                    let ResolverPackageSource::Git { url, .. } = selection.source() else {
                        panic!("the late named edge must select a final Git release")
                    };
                    assert_eq!(url.as_ref(), FINAL_URL);
                    Ok(successful_final_target_release.take().unwrap())
                },
                |_, _| unreachable!("this fixture has no Git-relative path requirements"),
            )
            .expect("the final Git release contract reloads successfully");
        assert!(successful_audit_plan.reload_failures().is_empty());
        assert_ne!(
            successful_final_target_manifest, read_time_target_manifest,
            "the final Git manifest must differ from the read-time bytes"
        );
        assert_eq!(
            successful_audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            successful_final_target_manifest.as_slice(),
            "successful Git release reloads retain the exact final manifest bytes"
        );

        let mut final_target_release = Some(
            fetch_fixture_with_canonical_url(
                &final_target_remote,
                Some(&final_target_remote.commit_id),
                None,
                FINAL_URL,
            )
            .unwrap(),
        );
        let audit_plan = output
            .plan_final_source_audit(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |selection, locked_package| {
                    assert!(locked_package.is_none());
                    assert_eq!(selection.package_version().package(), "acme:target");
                    let ResolverPackageSource::Git { url, .. } = selection.source() else {
                        panic!("the late named edge must select a final Git release")
                    };
                    assert_eq!(url.as_ref(), FINAL_URL);
                    Ok(final_target_release.take().unwrap())
                },
                |_, _| unreachable!("this fixture has no Git-relative path requirements"),
            )
            .expect("the final source plan retains Git release reload failures");
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_target_manifest
        );
        let failure = audit_plan
            .reload_failures()
            .iter()
            .find(|failure| failure.package_version.as_ref() == Some(&target_version))
            .expect("the final Git release path failure is retained");
        let crate::mixed_source::MixedSourceFinalReloadError::GitPathDiscovery(error) =
            &failure.error
        else {
            panic!(
                "expected a Git path-discovery failure, got {:#?}",
                failure.error
            );
        };
        let edge = error
            .requirement_edge()
            .expect("the final release failure retains its requirement edge");
        assert_eq!(edge.package(), "acme:outside");
        assert_eq!(edge.path(), "../../../outside");
        assert_eq!(
            &final_target_manifest[edge.span().clone()],
            b"acme:outside @ ^1 from path \"../../../outside\""
        );
        assert_eq!(
            failure.error.diagnostic_code(),
            Some(sai_diagnostics::Code::PathEscapesRepository)
        );
    }

    #[test]
    fn mixed_source_final_git_release_identity_mismatch_retains_manifest_bytes() {
        const TARGET_URL: &str = "https://example.test/acme/target.git";
        const LATE_URL: &str = "https://example.test/acme/late.git";
        const FINAL_URL: &str = "https://example.test/acme/final-target.git";
        let read_time_target_manifest = b"package acme:target @ 1.0.0\n\
                                         source \"main.sai\"\n\
                                         targets { wasm32-core }\n";
        let late_manifest = format!(
            "package acme:z-late @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{ acme:target @ ^1 from git \"{FINAL_URL}\" }}\n"
        );
        let final_target_manifest = b"package acme:target @ 2.0.0\n\
                                     source \"main.sai\"\n\
                                     targets { wasm32-core }\n";
        let directory = TestDirectory::new("mixed-source-final-git-release-contract-failure");
        let root_path = directory.0.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        let root_manifest = format!(
            "package zz:root @ 1.0.0\n\
             source \"main.sai\"\n\
             targets {{ wasm32-core }}\n\
             requires {{\n\
               acme:target @ ^1 from git \"{TARGET_URL}\"\n\
               acme:z-late @ ^1 from git \"{LATE_URL}\"\n\
             }}\n"
        );
        std::fs::write(root_path.join("sai.package"), root_manifest.as_bytes()).unwrap();

        let target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(read_time_target_manifest),
        );
        let late_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(late_manifest.as_bytes()),
        );
        let final_target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(final_target_manifest),
        );
        let mut target_release = Some(
            fetch_fixture_with_canonical_url(
                &target_remote,
                Some(&target_remote.commit_id),
                None,
                TARGET_URL,
            )
            .unwrap(),
        );
        let mut late_release = Some(
            fetch_fixture_with_canonical_url(
                &late_remote,
                Some(&late_remote.commit_id),
                None,
                LATE_URL,
            )
            .unwrap(),
        );
        let output = resolve_mixed_source_packages_with_fetchers(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            PackageValidationContext::default(),
            |selection, locked_package| {
                assert!(locked_package.is_none());
                match selection.package_version().package() {
                    "acme:target" => {
                        let ResolverPackageSource::Git { url, .. } = selection.source() else {
                            panic!("the read-time target source must be Git")
                        };
                        assert_eq!(url.as_ref(), TARGET_URL);
                        Ok(target_release.take().unwrap())
                    }
                    "acme:z-late" => Ok(late_release.take().unwrap()),
                    package => panic!("unexpected Git package request for {package}"),
                }
            },
            |_, _| unreachable!("this fixture has no Git-relative path requirements"),
        )
        .expect("the read-time MVS graph resolves from its original Git sources");

        let mut final_target_release = Some(
            fetch_fixture_with_canonical_url(
                &final_target_remote,
                Some(&final_target_remote.commit_id),
                None,
                FINAL_URL,
            )
            .unwrap(),
        );
        let audit_plan = output
            .plan_final_source_audit(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |selection, locked_package| {
                    assert!(locked_package.is_none());
                    assert_eq!(selection.package_version().package(), "acme:target");
                    let ResolverPackageSource::Git { url, .. } = selection.source() else {
                        panic!("the late named edge must select a final Git release")
                    };
                    assert_eq!(url.as_ref(), FINAL_URL);
                    Ok(final_target_release.take().unwrap())
                },
                |_, _| unreachable!("this fixture has no Git-relative path requirements"),
            )
            .expect("the final source plan retains Git identity failures");
        let target_version = expected_package_version("acme:target", "1.0.0");
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_target_manifest
        );
        let failure = audit_plan
            .reload_failures()
            .iter()
            .find(|failure| failure.package_version.as_ref() == Some(&target_version))
            .expect("the final Git identity failure is retained");
        let crate::mixed_source::MixedSourceFinalReloadError::GitPathDiscovery(error) =
            &failure.error
        else {
            panic!(
                "expected a Git path-discovery failure, got {:#?}",
                failure.error
            );
        };
        let crate::git_path_requirements::GitPathRequirementDiscoveryError::RequirerVersionMismatch {
            expected,
            found,
        } = error.as_ref()
        else {
            panic!("expected a Git requirer identity mismatch, got {error:#?}");
        };
        assert_eq!(**expected, target_version);
        assert_eq!(**found, expected_package_version("acme:target", "2.0.0"));
        assert_eq!(
            failure.error.diagnostic_code(),
            Some(sai_diagnostics::Code::PackageIdentityMismatch)
        );

        let root_version = expected_package_version("zz:root", "1.0.0");
        let late_version = expected_package_version("acme:z-late", "1.0.0");
        let root_file_id = sai_diagnostics::FileId::new(1);
        let target_file_id = sai_diagnostics::FileId::new(2);
        let final_file_id = sai_diagnostics::FileId::new(3);
        let late_file_id = sai_diagnostics::FileId::new(4);
        let diagnostic_snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                std::sync::Arc::new(
                    SourceFile::decode(root_file_id, root_manifest.as_bytes()).unwrap(),
                ),
                "sai.package",
                Some("zz:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                std::sync::Arc::new(
                    SourceFile::decode(target_file_id, read_time_target_manifest).unwrap(),
                ),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                std::sync::Arc::new(
                    SourceFile::decode(final_file_id, final_target_manifest).unwrap(),
                ),
                MvsManifestLocation::final_snapshot_path(&target_version, 0),
                Some("acme:target@1.0.0".to_owned()),
                None,
            ),
            SnapshotFile::new(
                std::sync::Arc::new(
                    SourceFile::decode(late_file_id, late_manifest.as_bytes()).unwrap(),
                ),
                "dep:acme:z-late@1.0.0/sai.package",
                Some("acme:z-late@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the diagnostic snapshot contains read-time and final manifests");
        let mut manifests = MvsManifestLocations::default();
        manifests
            .set_root(
                MvsManifestLocation::from_snapshot(
                    &diagnostic_snapshot,
                    root_file_id,
                    &root_version,
                    PackageRole::Root,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_package(
                MvsManifestLocation::from_snapshot(
                    &diagnostic_snapshot,
                    late_file_id,
                    &late_version,
                    PackageRole::Dependency,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_package(
                MvsManifestLocation::from_snapshot(
                    &diagnostic_snapshot,
                    target_file_id,
                    &target_version,
                    PackageRole::Dependency,
                )
                .unwrap(),
            )
            .unwrap();
        manifests
            .insert_final_package(
                MvsManifestLocation::from_final_snapshot(
                    &diagnostic_snapshot,
                    final_file_id,
                    &target_version,
                )
                .unwrap(),
            )
            .unwrap();
        let diagnostics =
            mixed_source_reload_failure_diagnostics(&output, &audit_plan, &manifests).unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            sai_diagnostics::Code::PackageIdentityMismatch
        );
        let primary = diagnostics[0]
            .primary()
            .expect("the changed final Git package header is primary");
        assert_eq!(
            primary.file.path,
            MvsManifestLocation::final_snapshot_path(&target_version, 0)
        );
        let start = primary.span.range.start().to_usize();
        let end = primary.span.range.end().to_usize();
        assert_eq!(
            &final_target_manifest[start..end],
            b"package acme:target @ 2.0.0"
        );
    }

    #[test]
    fn mixed_source_final_audit_rejects_a_misrouted_git_path_reload() {
        const TARGET_URL: &str = "https://example.test/acme/target.git";
        const LATE_URL: &str = "https://example.test/acme/late.git";
        const WRONG_URL: &str = "https://example.test/acme/wrong.git";
        let target_manifest = b"package acme:target @ 1.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n";
        let late_manifest = b"package acme:z-late @ 1.0.0\n\
                              source \"main.sai\"\n\
                              targets { wasm32-core }\n\
                              requires { acme:target @ ^1 from path \"packages/target\" }\n";
        let directory = TestDirectory::new("mixed-source-misrouted-git-reload");
        let workspace = directory.0.join("workspace");
        let root_path = workspace.join("root");
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::write(
            root_path.join("sai.package"),
            format!(
                "package acme:root @ 1.0.0\n\
                 source \"main.sai\"\n\
                 targets {{ wasm32-core }}\n\
                 requires {{\n\
                   acme:target @ ^1 from git \"{TARGET_URL}\"\n\
                   acme:z-late @ ^1 from git \"{LATE_URL}\"\n\
                 }}\n"
            ),
        )
        .unwrap();

        let target_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(target_manifest),
        );
        let late_remote = TestRemote::new_with_sibling_target_manifests(
            "refs/tags/v1.0.0",
            late_manifest,
            target_manifest,
        );
        let wrong_remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.0.0",
            false,
            gix::hash::Kind::Sha1,
            Some(target_manifest),
        );
        let mut target_release = Some(
            fetch_fixture_with_canonical_url(
                &target_remote,
                Some(&target_remote.commit_id),
                None,
                TARGET_URL,
            )
            .unwrap(),
        );
        let mut late_release = Some(
            fetch_fixture_with_canonical_url(
                &late_remote,
                Some(&late_remote.commit_id),
                None,
                LATE_URL,
            )
            .unwrap(),
        );
        let target_directory = RepositoryPackageDirectory::parse("packages/target").unwrap();
        let sibling_release = fetch_fixture_at_directory(
            &late_remote,
            Some(&late_remote.commit_id),
            None,
            LATE_URL,
            Some(&target_directory),
        )
        .unwrap();
        let sibling_identity = sibling_release.source_identity.clone();
        let mut sibling_package = Some(super::FetchedGitPackage {
            source_identity: sibling_release.source_identity,
            source: sibling_release.source,
        });
        let wrong_release = fetch_fixture_with_canonical_url(
            &wrong_remote,
            Some(&wrong_remote.commit_id),
            None,
            WRONG_URL,
        )
        .unwrap();
        let mut wrong_package = Some(super::FetchedGitPackage {
            source_identity: wrong_release.source_identity.clone(),
            source: wrong_release.source,
        });

        let output = resolve_mixed_source_packages_with_fetchers(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            PackageValidationContext::default(),
            |selection, locked_package| {
                assert!(locked_package.is_none());
                match selection.package_version().package() {
                    "acme:target" => Ok(target_release.take().unwrap()),
                    "acme:z-late" => Ok(late_release.take().unwrap()),
                    package => panic!("unexpected Git package request for {package}"),
                }
            },
            |identity, locked_package| {
                assert!(locked_package.is_none());
                assert_eq!(identity, &sibling_identity);
                Ok(sibling_package.take().unwrap())
            },
        )
        .expect("read-time MVS resolves");

        let audit_plan = output
            .plan_final_source_audit(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("all final sources are path pins"),
                |_, _| Ok(wrong_package.take().unwrap()),
            )
            .expect("the final source plan is available");
        let target_version = expected_package_version("acme:target", "1.0.0");
        assert_eq!(audit_plan.source_identities().reloads().len(), 1);
        let failure = audit_plan
            .reload_failures()
            .iter()
            .find(|failure| failure.package_version.as_ref() == Some(&target_version))
            .expect("a misrouted Git path reload must be retained as a failure");
        let crate::mixed_source::MixedSourceFinalReloadError::GitPathFetch(error) = &failure.error
        else {
            panic!("misrouted Git path reload should report a fetch error");
        };
        assert_eq!(
            **error,
            GitRemoteReleaseFetchError::GitPathSourceIdentityMismatch
        );
        assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::FetchFailed);
        assert!(!audit_plan.is_equivalent());
    }

    fn git_path_pin(
        requirer: MvsRequirer,
        relative_to: MvsPackageVersion,
        path: &str,
    ) -> ResolverPathPinSource {
        ResolverPathPinSource {
            source: ResolverPackageSource::Path {
                relative_to,
                path: path.into(),
            },
            requirer,
            span: 0..1,
        }
    }

    #[test]
    fn git_source_identity_index_canonicalizes_path_pins_from_the_requirers_commit() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let requiring = expected_package_version("acme:sample", "1.2.3");
        let requiring_edge = MvsRequirer::Package(requiring.clone());
        let mut index = GitPackageSourceIdentityIndex::default();
        index
            .insert(requiring_edge.clone(), fetched.source_identity().clone())
            .unwrap();

        let resolved = index
            .canonicalize_path_pin(&git_path_pin(
                requiring_edge.clone(),
                requiring.clone(),
                "modules/shared",
            ))
            .unwrap();

        assert_eq!(resolved.canonical_url(), TEST_CANONICAL_GIT_URL);
        assert_eq!(resolved.commit_id(), &remote.commit_id);
        assert_eq!(resolved.directory().as_str(), "modules/shared");
        assert_eq!(index.get(&requiring_edge), Some(fetched.source_identity()));

        let conflicting_identity = resolved.resolve_dependency_path("other").unwrap();
        assert_eq!(
            index.insert(requiring_edge.clone(), conflicting_identity),
            Err(
                GitPackageSourceIdentityIndexError::ConflictingSourceIdentity(
                    requiring_edge.clone()
                )
            )
        );
        let conflicting_version = expected_package_version("acme:sample", "1.2.4");
        let conflicting_version_edge = MvsRequirer::Package(conflicting_version.clone());
        assert_eq!(
            index.insert(
                conflicting_version_edge.clone(),
                fetched.source_identity().clone()
            ),
            Err(
                GitPackageSourceIdentityIndexError::ConflictingPackageVersion {
                    existing: Box::new(requiring),
                    discovered: Box::new(conflicting_version),
                }
            )
        );
    }

    #[test]
    fn git_source_identity_index_keeps_root_and_package_identities_separate() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let requiring = expected_package_version("acme:sample", "1.2.3");
        let root = MvsRequirer::Root(requiring.clone());
        let dependency = MvsRequirer::Package(requiring.clone());
        let dependency_identity = fetched
            .source_identity()
            .resolve_dependency_path("modules/dependency")
            .unwrap();
        let mut index = GitPackageSourceIdentityIndex::default();

        index
            .insert(root.clone(), fetched.source_identity().clone())
            .unwrap();
        index
            .insert(dependency.clone(), dependency_identity.clone())
            .unwrap();

        let root_pin = index
            .canonicalize_path_pin(&git_path_pin(
                root.clone(),
                requiring.clone(),
                "modules/shared",
            ))
            .unwrap();
        let dependency_pin = index
            .canonicalize_path_pin(&git_path_pin(
                dependency.clone(),
                requiring.clone(),
                "modules/shared",
            ))
            .unwrap();

        assert_eq!(index.get(&root), Some(fetched.source_identity()));
        assert_eq!(index.get(&dependency), Some(&dependency_identity));
        assert_eq!(root_pin.directory().as_str(), "modules/shared");
        assert_eq!(
            dependency_pin.directory().as_str(),
            "modules/dependency/modules/shared"
        );
        assert_eq!(root_pin.canonical_url(), TEST_CANONICAL_GIT_URL);
        assert_eq!(dependency_pin.canonical_url(), TEST_CANONICAL_GIT_URL);
        assert_eq!(root_pin.commit_id(), &remote.commit_id);
        assert_eq!(dependency_pin.commit_id(), &remote.commit_id);
    }

    #[test]
    fn discovers_only_active_git_path_requirements_and_indexes_a_nested_sibling_snapshot() {
        let client_manifest = b"package acme:client @ 2.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n\
                               requires {\n\
                                 acme:sibling @ ^1 from path \"../sibling\"\n\
                                 acme:inactive @ ^1 from path \"../inactive\" only { wasm32-wasi }\n\
                                 acme:ignored @ ^1 from path \"../ignored\"\n\
                               }\n\
                               requires test { acme:test-tool @ ^1 from path \"../test-tool\" }\n";
        let sibling_manifest = b"package acme:sibling @ 1.4.0\n\
                                source \"main.sai\"\n\
                                targets { wasm32-core }\n";
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(b"package acme:sample @ 1.2.3\nsource \"src\"\ntargets { wasm32-core }\n"),
        );
        let commit_id =
            remote.commit_with_client_and_sibling_packages(client_manifest, sibling_manifest);
        let requiring = expected_package_version("acme:client", "2.0.0");
        let client_directory = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let requiring_identity = super::GitPackageSourceIdentity {
            canonical_url: TEST_CANONICAL_GIT_URL.into(),
            commit_id,
            directory: client_directory.clone(),
        };
        let repository =
            gix::open_opts(&remote.directory.path, gix::open::Options::isolated()).unwrap();
        let reader = GitTreeReader::open(
            repository.git_dir().join("objects"),
            repository.object_hash(),
        )
        .unwrap();
        let requiring_source = reader
            .read_commit_repository_package_source_tree(&commit_id, &client_directory)
            .unwrap();
        let fetched = FetchedGitPackage {
            source_identity: requiring_identity.clone(),
            source: requiring_source,
        };
        let root_context = path_discovery_root_context();
        let matching_root_manifest = parse_for_role(
            b"package acme:client @ 2.0.0\n\
              source \"main.sai\"\n\
              targets { wasm32-core }\n\
              overrides { acme:ignored use git \"https://example.test/acme/ignored.git\" }\n\
              requires { acme:sibling @ ^1 from path \"../sibling\" }\n",
            PackageRole::Root,
        );
        let matching_root_context = ResolverContractContext::from_root_manifest(
            &matching_root_manifest,
            &BTreeMap::new(),
            &BTreeMap::new(),
            PackageValidationContext::default(),
        )
        .expect("valid root context for the fetched requiring package")
        .into_owned();
        let mut fetch_count = 0;

        let index = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Package(requiring.clone()),
            &root_context,
            |identity| {
                fetch_count += 1;
                assert_eq!(
                    identity,
                    &requiring_identity
                        .resolve_dependency_path("../sibling")
                        .unwrap()
                );
                let source = reader
                    .read_commit_repository_package_source_tree(
                        identity.commit_id(),
                        identity.directory(),
                    )
                    .unwrap();
                Ok::<_, ()>(FetchedGitPackage {
                    source_identity: identity.clone(),
                    source,
                })
            },
        )
        .unwrap();

        let path_key =
            crate::resolver_contract::ResolverPathKey::package(requiring.clone(), "../sibling");
        let sibling_version = expected_package_version("acme:sibling", "1.4.0");
        assert_eq!(fetch_count, 1, "only the included sibling is fetched");
        assert_eq!(
            index.path_packages(),
            &BTreeMap::from([(path_key.clone(), sibling_version.clone())])
        );
        assert_eq!(index.package_version(&path_key), Some(&sibling_version));
        assert_eq!(
            index
                .package(&path_key)
                .map(FetchedGitPackage::source_identity),
            Some(
                &requiring_identity
                    .resolve_dependency_path("../sibling")
                    .unwrap()
            )
        );

        let mismatch = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Root(requiring.clone()),
            &root_context,
            |_| -> Result<FetchedGitPackage, ()> {
                panic!("a root-context mismatch must fail before sibling fetch")
            },
        )
        .unwrap_err();
        assert_eq!(
            mismatch,
            crate::git_path_requirements::GitPathRequirementDiscoveryError::RootContextMismatch {
                expected: Box::new(root_context.root_package().clone()),
                found: Box::new(requiring.clone()),
            }
        );

        let root_index = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Root(requiring.clone()),
            &matching_root_context,
            |identity| {
                assert_eq!(
                    identity,
                    &requiring_identity
                        .resolve_dependency_path("../sibling")
                        .unwrap()
                );
                let source = reader
                    .read_commit_repository_package_source_tree(
                        identity.commit_id(),
                        identity.directory(),
                    )
                    .unwrap();
                Ok::<_, ()>(FetchedGitPackage {
                    source_identity: identity.clone(),
                    source,
                })
            },
        )
        .unwrap();
        let root_path_key =
            crate::resolver_contract::ResolverPathKey::root(requiring, "../sibling");
        assert_eq!(
            root_index.path_packages(),
            &BTreeMap::from([(root_path_key.clone(), sibling_version.clone())])
        );
        assert_eq!(
            root_index.package_version(&root_path_key),
            Some(&sibling_version)
        );
    }

    #[test]
    fn git_path_requirement_discovery_rejects_repository_escape_before_fetch() {
        let manifest = b"package acme:sample @ 1.2.3\n\
                         source \"src\"\n\
                         targets { wasm32-core }\n\
                         requires { acme:outside @ ^1 from path \"../../outside\" }\n";
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(manifest),
        );
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let requiring = expected_package_version("acme:sample", "1.2.3");

        let error = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Package(requiring.clone()),
            &path_discovery_root_context(),
            |_| -> Result<FetchedGitPackage, ()> { panic!("escaping path must fail before fetch") },
        )
        .unwrap_err();

        assert_eq!(
            error,
            GitPathRequirementDiscoveryError::DependencyPath {
                requirement: GitPathRequirementEdge::new(
                    "acme:outside",
                    "../../outside",
                    source_byte_span(manifest, b"acme:outside @ ^1 from path \"../../outside\"",),
                ),
                error: super::GitPackagePathError::PathEscapesRepository,
            }
        );
    }

    #[test]
    fn git_path_requirement_discovery_checks_the_requiring_manifest_version() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let expected = expected_package_version("acme:sample", "1.2.4");

        let error = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Package(expected.clone()),
            &path_discovery_root_context(),
            |_| -> Result<FetchedGitPackage, ()> {
                panic!("invalid requirer must fail before fetch")
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            GitPathRequirementDiscoveryError::RequirerVersionMismatch {
                expected: Box::new(expected),
                found: Box::new(expected_package_version("acme:sample", "1.2.3")),
            }
        );
    }

    #[test]
    fn git_path_requirement_discovery_preserves_sibling_manifest_diagnostics() {
        let client_manifest = b"package acme:client @ 2.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n\
                               requires { acme:sibling @ ^1 from path \"../sibling\" }\n";
        let sibling_manifest = b"package acme:sibling @ 1.4.0\n\
                                source \"main.sai\"\n\
                                targets { wasm32-core }\n\
                                requires {\n";
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id =
            remote.commit_with_client_and_sibling_packages(client_manifest, sibling_manifest);
        let requiring = expected_package_version("acme:client", "2.0.0");
        let client_directory = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let requiring_identity = super::GitPackageSourceIdentity {
            canonical_url: TEST_CANONICAL_GIT_URL.into(),
            commit_id,
            directory: client_directory.clone(),
        };
        let repository =
            gix::open_opts(&remote.directory.path, gix::open::Options::isolated()).unwrap();
        let reader = GitTreeReader::open(
            repository.git_dir().join("objects"),
            repository.object_hash(),
        )
        .unwrap();
        let requiring_source = reader
            .read_commit_repository_package_source_tree(&commit_id, &client_directory)
            .unwrap();
        let fetched = FetchedGitPackage {
            source_identity: requiring_identity,
            source: requiring_source,
        };

        let error = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Package(requiring.clone()),
            &path_discovery_root_context(),
            |identity| {
                let source = reader
                    .read_commit_repository_package_source_tree(
                        identity.commit_id(),
                        identity.directory(),
                    )
                    .unwrap();
                Ok::<_, ()>(FetchedGitPackage {
                    source_identity: identity.clone(),
                    source,
                })
            },
        )
        .unwrap_err();

        let parsed = error
            .manifest_parse_output()
            .expect("sibling parse diagnostics are available on discovery errors");
        assert!(!parsed.is_syntax_valid());
        assert!(!parsed.errors().is_empty());
        assert!(matches!(
            error,
            GitPathRequirementDiscoveryError::InvalidSiblingManifest { ref path, .. }
                if path.as_ref() == "../sibling"
        ));
    }

    #[test]
    fn git_path_requirement_discovery_fetches_a_normalized_source_identity_once() {
        let client_manifest = b"package acme:client @ 2.0.0\n\
                               source \"main.sai\"\n\
                               targets { wasm32-core }\n\
                               requires {\n\
                                 acme:sibling @ ^1 from path \"../sibling\"\n\
                                 acme:other @ ^1 from path \"../alias/../sibling\"\n\
                               }\n";
        let sibling_manifest = b"package acme:sibling @ 1.4.0\n\
                                source \"main.sai\"\n\
                                targets { wasm32-core }\n";
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id =
            remote.commit_with_client_and_sibling_packages(client_manifest, sibling_manifest);
        let requiring = expected_package_version("acme:client", "2.0.0");
        let client_directory = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let requiring_identity = super::GitPackageSourceIdentity {
            canonical_url: TEST_CANONICAL_GIT_URL.into(),
            commit_id,
            directory: client_directory.clone(),
        };
        let repository =
            gix::open_opts(&remote.directory.path, gix::open::Options::isolated()).unwrap();
        let reader = GitTreeReader::open(
            repository.git_dir().join("objects"),
            repository.object_hash(),
        )
        .unwrap();
        let requiring_source = reader
            .read_commit_repository_package_source_tree(&commit_id, &client_directory)
            .unwrap();
        let fetched = FetchedGitPackage {
            source_identity: requiring_identity,
            source: requiring_source,
        };
        let mut fetch_count = 0;

        let error = discover_git_path_requirements(
            &fetched,
            &MvsRequirer::Package(requiring.clone()),
            &path_discovery_root_context(),
            |identity| {
                fetch_count += 1;
                let source = reader
                    .read_commit_repository_package_source_tree(
                        identity.commit_id(),
                        identity.directory(),
                    )
                    .unwrap();
                Ok::<_, ()>(FetchedGitPackage {
                    source_identity: identity.clone(),
                    source,
                })
            },
        )
        .unwrap_err();

        assert_eq!(
            fetch_count, 1,
            "normalized source identities are fetched once"
        );
        assert_eq!(
            error,
            GitPathRequirementDiscoveryError::SiblingIdentityMismatch {
                requirement: GitPathRequirementEdge::new(
                    "acme:other",
                    "../alias/../sibling",
                    source_byte_span(
                        client_manifest,
                        b"acme:other @ ^1 from path \"../alias/../sibling\"",
                    ),
                ),
                expected: "acme:other".into(),
                found: "acme:sibling".into(),
            }
        );
    }

    #[test]
    fn git_source_identity_index_rejects_unbound_and_escaping_path_pins() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let requiring = expected_package_version("acme:sample", "1.2.3");
        let other = expected_package_version("acme:other", "1.0.0");
        let requiring_edge = MvsRequirer::Package(requiring.clone());
        let pin = git_path_pin(requiring_edge.clone(), requiring.clone(), "modules/shared");
        let empty_index = GitPackageSourceIdentityIndex::default();
        assert_eq!(
            empty_index.canonicalize_path_pin(&pin),
            Err(GitPackageSourceIdentityIndexError::MissingRequirer(
                requiring_edge.clone()
            ))
        );

        let mut index = GitPackageSourceIdentityIndex::default();
        index
            .insert(requiring_edge.clone(), fetched.source_identity().clone())
            .unwrap();
        assert_eq!(
            index.canonicalize_path_pin(&git_path_pin(
                requiring_edge.clone(),
                other.clone(),
                "modules/shared",
            )),
            Err(GitPackageSourceIdentityIndexError::RequirerMismatch {
                expected: Box::new(requiring.clone()),
                found: Box::new(other),
            })
        );
        assert_eq!(
            index.canonicalize_path_pin(&git_path_pin(requiring_edge, requiring, "../outside",)),
            Err(GitPackageSourceIdentityIndexError::Path(
                super::GitPackagePathError::PathEscapesRepository
            ))
        );
    }

    #[test]
    fn fetched_release_builds_a_validated_dependency_contract() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let context = root_contract_context();

        let contract = fetched
            .build_dependency_contract(
                &expected_package_version("acme:sample", "1.2.3"),
                &context,
                PackageValidationContext::default(),
            )
            .unwrap();

        assert_eq!(
            contract.mvs.package_version(),
            &expected_package_version("acme:sample", "1.2.3")
        );
        assert_eq!(contract.role(), PackageRole::Dependency);
    }

    #[test]
    fn fetched_release_rejects_a_manifest_with_another_identity_or_version() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let context = root_contract_context();

        let error = fetched
            .build_dependency_contract(
                &expected_package_version("acme:other", "1.2.3"),
                &context,
                PackageValidationContext::default(),
            )
            .unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseContractError::PackageVersionMismatch {
                expected: Box::new(expected_package_version("acme:other", "1.2.3")),
                found: Box::new(expected_package_version("acme:sample", "1.2.3")),
            }
        );
        assert_eq!(
            error.diagnostic_code(),
            Some(sai_diagnostics::Code::PackageIdentityMismatch)
        );
    }

    #[test]
    fn fetched_release_rejects_a_manifest_version_that_differs_from_its_tag() {
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(
                b"package acme:sample @ 1.2.4\n\
                  source \"src\"\n\
                  targets { wasm32-core }\n",
            ),
        );
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let context = root_contract_context();

        let error = fetched
            .build_dependency_contract(
                &expected_package_version("acme:sample", "1.2.3"),
                &context,
                PackageValidationContext::default(),
            )
            .unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseContractError::PackageVersionMismatch {
                expected: Box::new(expected_package_version("acme:sample", "1.2.3")),
                found: Box::new(expected_package_version("acme:sample", "1.2.4")),
            }
        );
        assert_eq!(
            error.diagnostic_code(),
            Some(sai_diagnostics::Code::PackageIdentityMismatch)
        );
    }

    #[test]
    fn fetched_release_rejects_a_tag_version_that_differs_from_the_resolver_selection() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let mut fetched = fetch_fixture(&remote, None, None).unwrap();
        fetched.version = crate::version::Version::parse("1.2.4").unwrap();
        let context = root_contract_context();

        let error = fetched
            .build_dependency_contract(
                &expected_package_version("acme:sample", "1.2.3"),
                &context,
                PackageValidationContext::default(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            GitRemoteReleaseContractError::ReleaseTagVersionMismatch {
                expected: expected_package_version("acme:sample", "1.2.3"),
                tag_version: crate::version::Version::parse("1.2.4").unwrap(),
            }
        );
        assert_eq!(
            error.diagnostic_code(),
            Some(sai_diagnostics::Code::PackageIdentityMismatch)
        );
    }

    #[test]
    fn fetched_release_preserves_source_located_manifest_validation_errors() {
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(b"package acme:sample @\n"),
        );
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let context = root_contract_context();

        assert!(!fetched.package_manifest().is_syntax_valid());
        let error = fetched
            .build_dependency_contract(
                &expected_package_version("acme:sample", "1.2.3"),
                &context,
                PackageValidationContext::default(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            GitRemoteReleaseContractError::InvalidContract(_)
        ));
        assert_eq!(error.diagnostic_code(), None);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn fetch_requires_a_regular_package_manifest_file() {
        let remote = TestRemote::new_without_package_manifest("refs/tags/v1.2.3", false);

        let error = fetch_fixture(&remote, None, None).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::SourceTree(GitTreeReadError::MissingPackageManifest)
        );
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::MissingPackageFile
        );
    }

    #[test]
    fn enforces_the_shared_transfer_budget_before_fetching_objects() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(remote.tag.object_id().kind()));
        let refspec = super::exact_release_tag_refspec(&remote.tag, None).unwrap();
        let transfer_limit = gix_transport::client::blocking_io::FetchTransferLimit::new(8);

        let error = fetch_release_tree_in_repository(
            &repository,
            super::ReleaseFetchRequest {
                source_url: super::GitSourceUrl::test_transport(
                    &remote.url(),
                    TEST_CANONICAL_GIT_URL,
                ),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory: None,
                expected_commit: None,
                expected_source_checksum: None,
                expected_locked_checksums: None,
                transfer_limit: &transfer_limit,
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::TransferLimitExceeded { limit_bytes: 8 }
        );
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::PackageTooLarge
        );
        assert_eq!(transfer_limit.consumed_bytes(), 8);
    }

    #[test]
    fn exact_fetch_does_not_follow_other_tags_or_commit_history() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(remote.tag.object_id().kind()));
        let refspec = super::exact_release_tag_refspec(&remote.tag, None).unwrap();
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);

        fetch_release_tree_in_repository(
            &repository,
            super::ReleaseFetchRequest {
                source_url: super::GitSourceUrl::test_transport(
                    &remote.url(),
                    TEST_CANONICAL_GIT_URL,
                ),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory: None,
                expected_commit: None,
                expected_source_checksum: None,
                expected_locked_checksums: None,
                transfer_limit: &transfer_limit,
            },
        )
        .unwrap();

        assert!(repository.find_object(remote.parent_commit_id).is_err());
        assert!(repository.find_object(remote.unrequested_tag_id).is_err());
    }

    #[test]
    fn fetches_sha256_release_tags() {
        let remote =
            TestRemote::new_with_object_hash("refs/tags/v1.2.3", true, gix::hash::Kind::Sha256);

        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();

        assert_eq!(fetched.commit_id().kind(), gix::hash::Kind::Sha256);
        assert_eq!(fetched.source_tree().checksum.len(), 69);
    }

    #[test]
    fn reports_a_moved_tag_when_the_peeled_commit_differs_from_the_lock() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let expected =
            gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff").unwrap();

        let error = fetch_fixture(&remote, Some(&expected), None).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::TagMoved {
                name: "refs/tags/v1.2.3".into(),
                expected,
                actual: remote.commit_id,
            }
        );
        assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::TagMoved);
    }

    #[test]
    fn reports_a_missing_exact_release_tag() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        std::fs::remove_file(remote.directory.path.join("refs/tags/v1.2.3")).unwrap();

        let error = fetch_fixture(&remote, None, None).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::TagNotFound {
                name: "refs/tags/v1.2.3".into(),
            }
        );
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::VersionNotFound
        );
    }

    #[test]
    fn fetches_locked_commit_when_tag_was_missing_before_the_call() {
        let remote = TestRemote::new("refs/tags/v1.2.3", true);
        let checksum = fetch_fixture(&remote, Some(&remote.commit_id), None)
            .unwrap()
            .source_tree()
            .checksum
            .clone();
        let version = remote.tag.version().clone();
        std::fs::remove_file(remote.directory.path.join("refs/tags/v1.2.3")).unwrap();
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(remote.commit_id.kind()));
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);
        let expected_checksums = super::ExpectedPackageChecksums {
            source_checksum: checksum.clone().into(),
            wit_checksum: None,
            artifact_checksums: Vec::new(),
        };

        let fetched = super::fetch_locked_release_tree_in_repository(
            &repository,
            super::GitSourceUrl::test_transport(&remote.url(), TEST_CANONICAL_GIT_URL),
            &version,
            None,
            &remote.commit_id,
            &expected_checksums,
            &transfer_limit,
        )
        .unwrap();

        assert_eq!(fetched.tag_name(), "refs/tags/v1.2.3");
        assert_eq!(fetched.tag_object_id(), None);
        assert!(fetched.tag_missing());
        assert_eq!(fetched.commit_id(), &remote.commit_id);
        assert_eq!(fetched.source_tree().checksum, checksum);
    }

    #[test]
    fn fetches_git_sibling_source_tree_from_its_exact_repository_commit() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id = remote.commit_with_sibling_package();
        std::fs::remove_file(remote.directory.path.join("refs/tags/v1.2.3")).unwrap();
        let directory = RepositoryPackageDirectory::parse("packages/sibling").unwrap();
        let requiring_directory = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let requiring_source = super::GitPackageSourceIdentity {
            canonical_url: remote.url().into(),
            commit_id,
            directory: requiring_directory,
        };
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(commit_id.kind()));
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);

        let resolved_source =
            super::resolve_git_path_package_identity(&requiring_source, "../sibling").unwrap();
        let fetched = super::fetch_git_package_source_identity_in_repository(
            &repository,
            &resolved_source,
            &transfer_limit,
        )
        .unwrap();

        let manifest = fetched
            .source()
            .files()
            .iter()
            .find(|file| file.path() == "sai.package")
            .unwrap();
        assert_eq!(
            manifest.content(),
            b"package acme:sibling @ 1.4.0\n\
              source \"main.sai\"\n\
              targets { wasm32-core }\n"
        );
        assert_eq!(fetched.commit_id(), &commit_id);
        assert_eq!(fetched.directory(), &directory);
        assert_eq!(fetched.canonical_url(), remote.url());
        assert_eq!(
            fetched.source_identity(),
            &requiring_source
                .resolve_dependency_path("../sibling")
                .unwrap()
        );
        assert!(fetched.source().source_tree().checksum.starts_with("sai1:"));

        let contract = fetched
            .build_dependency_contract(
                &expected_package_version("acme:sibling", "1.4.0"),
                &root_contract_context(),
                PackageValidationContext::default(),
            )
            .unwrap();
        assert_eq!(
            contract.mvs.package_version(),
            &expected_package_version("acme:sibling", "1.4.0")
        );
        assert_eq!(contract.role(), PackageRole::Dependency);
    }

    #[test]
    fn reports_git_sibling_repository_escape_with_the_specific_diagnostic() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id = remote.commit_with_sibling_package();
        let requiring_source = super::GitPackageSourceIdentity {
            canonical_url: remote.url().into(),
            commit_id,
            directory: RepositoryPackageDirectory::parse("packages/client").unwrap(),
        };
        let error =
            super::fetch_git_path_package_source_tree(&requiring_source, "../../../outside")
                .unwrap_err();

        assert_eq!(error, GitRemoteReleaseFetchError::PathEscapesRepository);
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::PathEscapesRepository
        );
    }

    #[test]
    fn reports_invalid_git_sibling_path_with_path_specific_context() {
        let requiring_source = super::GitPackageSourceIdentity {
            canonical_url: "https://example.test/monorepo.git".into(),
            commit_id: gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff")
                .unwrap(),
            directory: RepositoryPackageDirectory::parse("packages/client").unwrap(),
        };

        let error = super::fetch_git_path_package_source_tree(&requiring_source, "src//helper")
            .unwrap_err();

        assert_eq!(error, GitRemoteReleaseFetchError::InvalidGitDependencyPath);
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::InvalidPackagePath
        );
        assert_eq!(
            error.to_string(),
            "Git path package dependency path is invalid"
        );
    }

    #[test]
    fn reports_a_missing_git_sibling_with_path_package_not_found() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id = remote.commit_with_sibling_package();
        let requiring_source = super::GitPackageSourceIdentity {
            canonical_url: remote.url().into(),
            commit_id,
            directory: RepositoryPackageDirectory::parse("packages/client").unwrap(),
        };
        let resolved_source =
            super::resolve_git_path_package_identity(&requiring_source, "../missing").unwrap();
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(commit_id.kind()));
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);

        let error = super::fetch_git_package_source_identity_in_repository(
            &repository,
            &resolved_source,
            &transfer_limit,
        )
        .unwrap_err();

        assert_eq!(error, GitRemoteReleaseFetchError::PathPackageNotFound);
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::PathPackageNotFound
        );
    }

    #[test]
    fn git_sibling_fetch_errors_use_source_neutral_messages() {
        assert_eq!(
            GitRemoteReleaseFetchError::FetchFailed.to_string(),
            "Could not fetch the Git source"
        );

        let commit = gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff").unwrap();
        let unavailable = GitRemoteReleaseFetchError::RepositoryCommitUnavailable { commit };
        assert!(
            unavailable
                .to_string()
                .starts_with("Git repository commit ")
        );
        assert!(
            unavailable
                .to_string()
                .contains("is unavailable from the source")
        );
        assert!(!unavailable.to_string().contains("locked"));
    }

    #[test]
    fn git_sibling_contract_rejects_another_identity_or_version() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let commit_id = remote.commit_with_sibling_package();
        let directory = RepositoryPackageDirectory::parse("packages/sibling").unwrap();
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(commit_id.kind()));
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);
        let fetched = super::fetch_git_package_source_tree_at_commit_in_repository(
            &repository,
            &remote.url(),
            &commit_id,
            &directory,
            &transfer_limit,
        )
        .unwrap();

        for expected in [
            expected_package_version("acme:other", "1.4.0"),
            expected_package_version("acme:sibling", "1.5.0"),
        ] {
            let error = fetched
                .build_dependency_contract(
                    &expected,
                    &root_contract_context(),
                    PackageValidationContext::default(),
                )
                .unwrap_err();
            assert_eq!(
                error,
                super::GitRemoteReleaseContractError::PackageVersionMismatch {
                    expected: Box::new(expected),
                    found: Box::new(expected_package_version("acme:sibling", "1.4.0")),
                }
            );
            assert_eq!(
                error.diagnostic_code(),
                Some(sai_diagnostics::Code::PackageIdentityMismatch)
            );
        }
    }

    #[test]
    fn rejects_local_file_urls_for_git_sibling_fetches() {
        let commit_id =
            gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff").unwrap();
        let directory = RepositoryPackageDirectory::parse("packages/sibling").unwrap();

        let error = super::fetch_git_package_source_tree_at_commit(
            "file:///tmp/sai-test-repository",
            &commit_id,
            &directory,
        )
        .unwrap_err();

        assert_eq!(error, GitRemoteReleaseFetchError::InvalidGitUrl);
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::UnsupportedGitUrl
        );
    }

    #[test]
    fn reports_a_missing_git_sibling_commit_without_tag_language() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let unavailable =
            gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff").unwrap();
        let directory = RepositoryPackageDirectory::parse("packages/sibling").unwrap();
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(unavailable.kind()));
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);

        let error = super::fetch_git_package_source_tree_at_commit_in_repository(
            &repository,
            &remote.url(),
            &unavailable,
            &directory,
            &transfer_limit,
        )
        .unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::RepositoryCommitUnavailable {
                commit: unavailable
            }
        );
        assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::FetchFailed);
    }

    #[test]
    fn builds_locked_fetch_request_from_a_validated_lock_package() {
        let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
            .expect("valid SHA-1 commit");
        let source_checksum: Box<str> = format!("sai1:{}", "1".repeat(64)).into();
        let wit_checksum: Box<str> = format!("sai1:{}", "2".repeat(64)).into();
        let artifact_checksum: Box<str> = format!("sai1:{}", "3".repeat(64)).into();
        let package = crate::lock::LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                    "https://example.com/acme/sample.git".into(),
                )),
                crate::lock::LockPackageField::Directory("packages/sample".into()),
                crate::lock::LockPackageField::Commit(commit.to_hex().to_string().into()),
                crate::lock::LockPackageField::Checksum(source_checksum.clone()),
                crate::lock::LockPackageField::WitChecksum(wit_checksum.clone()),
                crate::lock::LockPackageField::Artifact {
                    target: "wasm32-wasi".into(),
                    checksum: artifact_checksum.clone(),
                },
            ]
            .into_iter()
            .map(|value| crate::manifest::Spanned { value, span: 0..0 })
            .collect(),
        };

        let request = super::locked_release_request(&package).unwrap();

        assert_eq!(request.url.as_ref(), "https://example.com/acme/sample.git");
        assert_eq!(request.version.to_string(), "1.2.3");
        assert_eq!(
            request.directory.as_ref().map(|path| path.as_str()),
            Some("packages/sample")
        );
        assert_eq!(request.commit, commit);
        assert_eq!(request.checksums.source_checksum, source_checksum);
        assert_eq!(request.checksums.wit_checksum, Some(wit_checksum));
        assert_eq!(
            request.checksums.artifact_checksums,
            [super::ExpectedArtifactChecksum {
                target: "wasm32-wasi".into(),
                checksum: artifact_checksum,
            }]
        );
    }

    #[test]
    fn locked_fetch_reuses_only_metadata_for_the_same_commit_and_reports_checksum_mismatches() {
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(
                b"package acme:sample @ 1.2.3\n\
                  wit \"wit\"\n\
                  artifact component {\n\
                      wasm32-wasi \"component.wasm\"\n\
                  }\n",
            ),
        );
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let source_checksum = fetched.source_tree().checksum.clone();
        let wit_checksum = fetched.wit_tree().unwrap().checksum.clone();
        let artifacts = fetched
            .artifact_checksums()
            .iter()
            .map(|artifact| {
                (
                    artifact.target().to_owned().into_boxed_str(),
                    artifact.checksum().to_owned().into_boxed_str(),
                )
            })
            .collect::<Vec<_>>();
        let package_url = "https://example.com/acme/sample.git";
        let commit = remote.commit_id.to_hex().to_string();
        let cache_directory = TestDirectory::new("locked-package-cache");
        let cache = PackageCache::open(cache_directory.0.join("cache")).unwrap();
        let source_tree = fetched.into_source_tree();
        cache
            .record_verified_package_checksums(package_url, &commit, None, &source_tree)
            .unwrap();
        cache.materialize_source(source_tree).unwrap();

        let make_package = |locked_commit: &str, locked_wit: &str| {
            let mut fields = vec![
                crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                    package_url.into(),
                )),
                crate::lock::LockPackageField::Commit(locked_commit.into()),
                crate::lock::LockPackageField::Checksum(source_checksum.clone().into()),
                crate::lock::LockPackageField::WitChecksum(locked_wit.into()),
            ];
            fields.extend(artifacts.iter().map(|(target, checksum)| {
                crate::lock::LockPackageField::Artifact {
                    target: target.clone(),
                    checksum: checksum.clone(),
                }
            }));
            crate::lock::LockPackage {
                package: "acme:sample".into(),
                version: crate::version::Version::parse("1.2.3").unwrap(),
                fields: fields
                    .into_iter()
                    .map(|value| crate::manifest::Spanned { value, span: 0..0 })
                    .collect(),
            }
        };

        let cached = super::fetch_locked_package_to_cache(
            &make_package(&commit, &wit_checksum),
            &cache,
            true,
        )
        .unwrap();
        assert_eq!(cached.status(), &super::LockedPackageCacheStatus::Cached);
        assert_eq!(cached.source().checksum(), source_checksum.as_str());

        let wrong_wit = format!("sai1:{}", "0".repeat(64));
        let mismatch =
            super::fetch_locked_package_to_cache(&make_package(&commit, &wrong_wit), &cache, true)
                .unwrap_err();
        assert!(matches!(
            mismatch,
            super::LockedPackageCacheFetchError::Fetch(
                GitRemoteReleaseFetchError::LockedChecksumMismatch {
                    context,
                    kind: super::GitPackageChecksumKind::Wit,
                    ..
                }
            ) if context.package() == "acme:sample"
                && context.url() == "https://example.com/acme/sample.git"
                && context.commit() == commit.as_str()
                && context.version().to_string() == "1.2.3"
        ));

        let other_commit = "0".repeat(40);
        assert!(matches!(
            super::fetch_locked_package_to_cache(
                &make_package(&other_commit, &wit_checksum),
                &cache,
                true
            ),
            Err(super::LockedPackageCacheFetchError::OfflineFetchRequired { .. })
        ));

        let cached_path = cached.source().path().to_path_buf();
        let source_file = cached_path.join("lib.sai");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&source_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = std::fs::metadata(&source_file).unwrap().permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(&source_file, permissions).unwrap();
        }
        std::fs::write(&source_file, b"tampered source").unwrap();
        let offline_error = super::fetch_locked_package_to_cache(
            &make_package(&commit, &wit_checksum),
            &cache,
            true,
        )
        .unwrap_err();
        let notice = offline_error.cache_corruption_notice().unwrap();
        assert_eq!(notice.expected_checksum(), source_checksum);
        assert!(matches!(
            offline_error,
            super::LockedPackageCacheFetchError::CorruptSourceObserved { cause, .. }
                if matches!(
                    *cause,
                    super::LockedPackageCacheFetchError::OfflineFetchRequired { .. }
                )
        ));
        assert!(!cached_path.exists());
    }

    #[test]
    fn locked_source_only_package_reuses_verified_cache_without_git_checksum_metadata() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let source_checksum = fetched.source_tree().checksum.clone();
        let commit = remote.commit_id.to_hex().to_string();
        let package_url = "https://example.com/acme/sample.git";
        let cache_directory = TestDirectory::new("locked-source-only-cache");
        let cache = PackageCache::open(cache_directory.0.join("cache")).unwrap();
        cache
            .materialize_source(fetched.into_source_tree())
            .unwrap();
        assert!(
            cache
                .cached_package_checksums(package_url, &commit, None)
                .unwrap()
                .is_none()
        );

        let package = crate::lock::LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                    package_url.into(),
                )),
                crate::lock::LockPackageField::Commit(commit.into()),
                crate::lock::LockPackageField::Checksum(source_checksum.clone().into()),
            ]
            .into_iter()
            .map(|value| crate::manifest::Spanned { value, span: 0..0 })
            .collect(),
        };

        let cached = super::fetch_locked_package_to_cache(&package, &cache, true).unwrap();
        assert_eq!(cached.status(), &super::LockedPackageCacheStatus::Cached);
        assert_eq!(cached.source().checksum(), source_checksum);
    }

    #[test]
    fn locked_package_with_wit_does_not_skip_missing_checksum_metadata() {
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(b"package acme:sample @ 1.2.3\nwit \"wit\"\n"),
        );
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let source_checksum = fetched.source_tree().checksum.clone();
        let commit = remote.commit_id.to_hex().to_string();
        let package_url = "https://example.com/acme/sample.git";
        let cache_directory = TestDirectory::new("locked-package-missing-wit-cache");
        let cache = PackageCache::open(cache_directory.0.join("cache")).unwrap();
        let cached_source = cache
            .materialize_source(fetched.into_source_tree())
            .unwrap();
        let package = crate::lock::LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [
                crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                    package_url.into(),
                )),
                crate::lock::LockPackageField::Commit(commit.into()),
                crate::lock::LockPackageField::Checksum(source_checksum.clone().into()),
            ]
            .into_iter()
            .map(|value| crate::manifest::Spanned { value, span: 0..0 })
            .collect(),
        };

        assert!(matches!(
            super::fetch_locked_package_to_cache(&package, &cache, true),
            Err(super::LockedPackageCacheFetchError::OfflineFetchRequired { .. })
        ));
        assert!(cached_source.path().exists());
    }

    #[test]
    fn rejects_malformed_locked_checksums_before_fetching() {
        let valid_checksum: Box<str> = format!("sai1:{}", "1".repeat(64)).into();
        let mut invalid_source_fields = vec![
            crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                "https://example.com/acme/sample.git".into(),
            )),
            crate::lock::LockPackageField::Commit(
                "0123456789abcdef0123456789abcdef01234567".into(),
            ),
            crate::lock::LockPackageField::Checksum("sai1:bad".into()),
        ];
        let mut invalid_wit_fields = vec![
            crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                "https://example.com/acme/sample.git".into(),
            )),
            crate::lock::LockPackageField::Commit(
                "0123456789abcdef0123456789abcdef01234567".into(),
            ),
            crate::lock::LockPackageField::Checksum(valid_checksum.clone()),
            crate::lock::LockPackageField::WitChecksum("sai1:bad".into()),
        ];
        let mut invalid_artifact_fields = vec![
            crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                "https://example.com/acme/sample.git".into(),
            )),
            crate::lock::LockPackageField::Commit(
                "0123456789abcdef0123456789abcdef01234567".into(),
            ),
            crate::lock::LockPackageField::Checksum(valid_checksum),
            crate::lock::LockPackageField::Artifact {
                target: "wasm32-wasi".into(),
                checksum: "sai1:bad".into(),
            },
        ];
        let cases = [
            (&mut invalid_source_fields, "checksum"),
            (&mut invalid_wit_fields, "WIT checksum"),
            (&mut invalid_artifact_fields, "artifact checksum"),
        ];

        for (fields, field_name) in cases {
            let package = crate::lock::LockPackage {
                package: "acme:sample".into(),
                version: crate::version::Version::parse("1.2.3").unwrap(),
                fields: fields
                    .drain(..)
                    .map(|value| crate::manifest::Spanned { value, span: 0..0 })
                    .collect(),
            };

            let error = super::fetch_locked_package_tree(&package).unwrap_err();

            assert_eq!(
                error,
                GitRemoteReleaseFetchError::InvalidLockedPackageField(field_name)
            );
            assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::LockCorrupt);
        }
    }

    #[test]
    fn rejects_noncanonical_locked_fields_before_fetching() {
        let valid_checksum: Box<str> = format!("sai1:{}", "1".repeat(64)).into();
        let commit: Box<str> = "0123456789abcdef0123456789abcdef01234567".into();
        let cases = [
            (
                vec![
                    crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                        "https://EXAMPLE.com/acme/sample.git".into(),
                    )),
                    crate::lock::LockPackageField::Commit(commit.clone()),
                    crate::lock::LockPackageField::Checksum(valid_checksum.clone()),
                ],
                "source",
            ),
            (
                vec![
                    crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                        "https://example.com/acme/sample.git".into(),
                    )),
                    crate::lock::LockPackageField::Directory("packages/../other".into()),
                    crate::lock::LockPackageField::Commit(commit.clone()),
                    crate::lock::LockPackageField::Checksum(valid_checksum.clone()),
                ],
                "directory",
            ),
            (
                vec![
                    crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                        "https://example.com/acme/sample.git".into(),
                    )),
                    crate::lock::LockPackageField::Commit(commit),
                    crate::lock::LockPackageField::Checksum(valid_checksum.clone()),
                    crate::lock::LockPackageField::Artifact {
                        target: "wasm32-wasi".into(),
                        checksum: valid_checksum.clone(),
                    },
                ],
                "WIT checksum",
            ),
        ];

        for (fields, field_name) in cases {
            let package = crate::lock::LockPackage {
                package: "acme:sample".into(),
                version: crate::version::Version::parse("1.2.3").unwrap(),
                fields: fields
                    .into_iter()
                    .map(|value| crate::manifest::Spanned { value, span: 0..0 })
                    .collect(),
            };

            let error = super::fetch_locked_package_tree(&package).unwrap_err();

            assert_eq!(
                error,
                GitRemoteReleaseFetchError::InvalidLockedPackageField(field_name)
            );
            assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::LockCorrupt);
        }
    }

    #[test]
    fn rejects_invalid_lock_entries_before_fetching() {
        let package = crate::lock::LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields: [crate::lock::LockPackageField::Source(
                crate::lock::LockSource::Path("vendor/sample".into()),
            )]
            .into_iter()
            .map(|value| crate::manifest::Spanned { value, span: 0..0 })
            .collect(),
        };

        let error = super::locked_release_request(&package).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::InvalidLockedPackageField("source")
        );
        assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::LockCorrupt);
    }

    #[test]
    fn rejects_duplicate_locked_artifact_targets_before_fetching() {
        let checksum: Box<str> = format!("sai1:{}", "1".repeat(64)).into();
        let fields = [
            crate::lock::LockPackageField::Source(crate::lock::LockSource::Git(
                "https://example.com/acme/sample.git".into(),
            )),
            crate::lock::LockPackageField::Commit(
                "0123456789abcdef0123456789abcdef01234567".into(),
            ),
            crate::lock::LockPackageField::Checksum(checksum.clone()),
            crate::lock::LockPackageField::Artifact {
                target: "wasm32-wasi".into(),
                checksum: checksum.clone(),
            },
            crate::lock::LockPackageField::Artifact {
                target: "wasm32-wasi".into(),
                checksum,
            },
        ]
        .into_iter()
        .map(|value| crate::manifest::Spanned { value, span: 0..0 })
        .collect();
        let package = crate::lock::LockPackage {
            package: "acme:sample".into(),
            version: crate::version::Version::parse("1.2.3").unwrap(),
            fields,
        };

        let error = super::locked_release_request(&package).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::InvalidLockedPackageField("artifact target")
        );
        assert_eq!(error.diagnostic_code(), sai_diagnostics::Code::LockCorrupt);
    }

    #[test]
    fn reports_locked_commit_unavailable_when_deleted_tag_cannot_be_fetched() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        std::fs::remove_file(remote.directory.path.join("refs/tags/v1.2.3")).unwrap();
        let unavailable =
            gix::ObjectId::from_hex(b"ffffffffffffffffffffffffffffffffffffffff").unwrap();

        let error = fetch_fixture(&remote, Some(&unavailable), None).unwrap_err();

        assert_eq!(
            error,
            GitRemoteReleaseFetchError::LockedCommitUnavailable {
                name: "refs/tags/v1.2.3".into(),
                commit: unavailable,
            }
        );
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::VersionNotFound
        );
    }

    #[test]
    fn verifies_the_source_checksum_against_the_lock() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let fetched = fetch_fixture(&remote, None, None).unwrap();
        let expected = fetched.source_tree().checksum.clone();
        assert!(fetch_fixture(&remote, None, Some(&expected)).is_ok());

        let wrong = format!("sai1:{}", "0".repeat(64));
        let error = fetch_fixture(&remote, None, Some(&wrong)).unwrap_err();
        assert_eq!(
            error.diagnostic_code(),
            sai_diagnostics::Code::ChecksumMismatch
        );
    }

    #[test]
    fn locked_fetch_verifies_source_wit_and_artifact_checksums() {
        let remote = TestRemote::new_with_package_manifest(
            "refs/tags/v1.2.3",
            false,
            gix::hash::Kind::Sha1,
            Some(
                b"package acme:sample @ 1.2.3\n\
                  wit \"wit\"\n\
                  artifact component {\n\
                      wasm32-wasi \"component.wasm\"\n\
                  }\n",
            ),
        );
        let fetched = fetch_fixture(&remote, Some(&remote.commit_id), None).unwrap();
        let expected = super::ExpectedPackageChecksums {
            source_checksum: fetched.source_tree().checksum.clone().into(),
            wit_checksum: Some(fetched.wit_tree().unwrap().checksum.clone().into()),
            artifact_checksums: fetched
                .artifact_checksums()
                .iter()
                .map(|artifact| super::ExpectedArtifactChecksum {
                    target: artifact.target().into(),
                    checksum: artifact.checksum().into(),
                })
                .collect(),
        };

        let fetch_locked = |expected: &super::ExpectedPackageChecksums| {
            let scratch = TemporaryGitDirectory::create().unwrap();
            let repository = init_bare_repository(&scratch.path, Some(remote.commit_id.kind()));
            let transfer_limit = gix_transport::client::blocking_io::FetchTransferLimit::new(
                super::MAX_GIT_FETCH_BYTES,
            );
            super::fetch_locked_release_tree_in_repository(
                &repository,
                super::GitSourceUrl::test_transport(&remote.url(), TEST_CANONICAL_GIT_URL),
                remote.tag.version(),
                None,
                &remote.commit_id,
                expected,
                &transfer_limit,
            )
        };

        let locked = fetch_locked(&expected).unwrap();
        assert_eq!(
            locked.source_tree().checksum,
            expected.source_checksum.as_ref()
        );
        assert_eq!(
            locked.wit_tree().unwrap().checksum,
            expected.wit_checksum.as_deref().unwrap()
        );
        assert_eq!(
            locked.artifact_checksums()[0].checksum(),
            expected.artifact_checksums[0].checksum.as_ref()
        );

        let mut wrong_wit = expected.clone();
        wrong_wit.wit_checksum = Some(format!("sai1:{}", "0".repeat(64)).into());
        assert!(matches!(
            fetch_locked(&wrong_wit),
            Err(GitRemoteReleaseFetchError::ChecksumMismatch {
                kind: super::GitPackageChecksumKind::Wit,
                ..
            })
        ));

        let mut wrong_artifact = expected.clone();
        wrong_artifact.artifact_checksums[0].checksum = format!("sai1:{}", "0".repeat(64)).into();
        assert!(matches!(
            fetch_locked(&wrong_artifact),
            Err(GitRemoteReleaseFetchError::ChecksumMismatch {
                kind: super::GitPackageChecksumKind::Artifact { target },
                ..
            }) if target.as_ref() == "wasm32-wasi"
        ));

        let mut missing_wit = expected.clone();
        missing_wit.wit_checksum = None;
        assert!(matches!(
            fetch_locked(&missing_wit),
            Err(GitRemoteReleaseFetchError::ChecksumMismatch {
                kind: super::GitPackageChecksumKind::Wit,
                expected,
                ..
            }) if expected.as_ref() == "<missing>"
        ));

        let mut missing_artifact = expected;
        missing_artifact.artifact_checksums.clear();
        assert!(matches!(
            fetch_locked(&missing_artifact),
            Err(GitRemoteReleaseFetchError::ChecksumMismatch {
                kind: super::GitPackageChecksumKind::Artifact { target },
                expected,
                ..
            }) if target.as_ref() == "wasm32-wasi" && expected.as_ref() == "<missing>"
        ));
    }

    #[test]
    fn exact_fetch_rejects_a_tag_from_another_package_namespace() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let directory = RepositoryPackageDirectory::parse("packages/core").unwrap();

        assert_eq!(
            super::exact_release_tag_refspec(&remote.tag, Some(&directory)),
            Err(GitRemoteReleaseFetchError::InvalidPackageDirectory)
        );
    }

    #[test]
    fn temporary_repository_uses_the_global_cache_tmp_directory() {
        let temp = TestDirectory::new("cache");
        let cache = PackageCache::open(temp.0.join("cache")).unwrap();
        let temporary = TemporaryGitDirectory::create_with_cache(&cache).unwrap();
        let path = temporary.path.clone();
        let activity_lock_path = temporary.activity_lock_path.clone();

        assert_eq!(path.parent(), Some(cache.temporary_directory()));
        assert!(activity_lock_path.is_file());

        drop(temporary);
        assert!(!path.exists());
        assert!(!activity_lock_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_git_temporary_repository_is_preserved_while_its_lock_is_held() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("active-cache");
        let cache = PackageCache::open(temp.0.join("cache")).unwrap();
        let temporary = TemporaryGitDirectory::create_with_cache(&cache).unwrap();
        let path = temporary.path.clone();
        let activity_lock_path = temporary.activity_lock_path.clone();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        for path in [&path, &activity_lock_path] {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }

        cache.cleanup_stale_temporary_directories();
        assert!(path.is_dir());
        assert!(activity_lock_path.is_file());

        drop(temporary);
        assert!(!path.exists());
        assert!(!activity_lock_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn temporary_repository_directory_restricts_posix_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TemporaryGitDirectory::create().unwrap();
        let mode = std::fs::metadata(&temporary.path)
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o077, 0);
    }
}
