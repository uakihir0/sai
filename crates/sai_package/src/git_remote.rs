//! Bounded Git release-tag listing and locked-source fetching.

use std::error::Error as StdError;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gix::bstr::{BStr, BString, ByteSlice};

use crate::git_tags::{
    GitReleaseTag, GitReleaseTagListError, GitReleaseTagListLimit, parse_release_tag_refs,
};
use crate::git_tree::{GitTreeReadError, GitTreeReader};
use crate::lock_validation::canonical_git_url;
use crate::package_path::DependencyPackagePath;
use crate::tree_hash::{TreeHash, TreeHashError, TreeHashPurpose};
use crate::version::Version;

const SSH_TAG_LIST_READ_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_GIT_FETCH_BYTES: u64 = 4 * 1024 * 1024 * 1024;

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedGitRelease {
    tag_name: Box<str>,
    version: Version,
    tag_object_id: Option<gix::ObjectId>,
    tag_missing: bool,
    commit_id: gix::ObjectId,
    source_tree: TreeHash,
}

impl FetchedGitRelease {
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
        &self.commit_id
    }

    /// The checksum and warnings for the selected package source tree.
    pub const fn source_tree(&self) -> &TreeHash {
        &self.source_tree
    }
}

/// Errors while fetching, peeling, and checking one remote release tag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteReleaseFetchError {
    /// The URL is not a supported secure SAI Git URL.
    InvalidGitUrl,
    /// The package directory or release tag does not belong to its namespace.
    InvalidPackageDirectory,
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
    /// The source checksum differs from the lock.
    ChecksumMismatch {
        /// The checksum recorded in the lock.
        expected: Box<str>,
        /// The checksum computed from the fetched commit.
        actual: Box<str>,
    },
}

impl GitRemoteReleaseFetchError {
    /// Return the user-facing diagnostic code for this failure.
    pub const fn diagnostic_code(&self) -> sai_diagnostics::Code {
        match self {
            Self::InvalidGitUrl => sai_diagnostics::Code::UnsupportedGitUrl,
            Self::InvalidPackageDirectory => sai_diagnostics::Code::InvalidPackagePath,
            Self::TemporaryRepository | Self::RemoteConfiguration | Self::FetchFailed => {
                sai_diagnostics::Code::FetchFailed
            }
            Self::TransferLimitExceeded { .. } => sai_diagnostics::Code::PackageTooLarge,
            Self::TagNotFound { .. } | Self::LockedCommitUnavailable { .. } => {
                sai_diagnostics::Code::VersionNotFound
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
            | Self::SourceTree(GitTreeReadError::PackagePathNotDirectory) => {
                sai_diagnostics::Code::MissingPackageFile
            }
            Self::SourceTree(GitTreeReadError::Hash(_))
            | Self::SourceTree(GitTreeReadError::InvalidTreeEntry) => {
                sai_diagnostics::Code::InvalidPackageFilePath
            }
            Self::SourceTree(_) => sai_diagnostics::Code::FetchFailed,
            Self::ChecksumMismatch { .. } => sai_diagnostics::Code::ChecksumMismatch,
        }
    }
}

impl fmt::Display for GitRemoteReleaseFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGitUrl => formatter.write_str("Git URL is not supported"),
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git release tag is outside the package tag namespace")
            }
            Self::TemporaryRepository => {
                formatter.write_str("Could not prepare the temporary Git repository")
            }
            Self::RemoteConfiguration => formatter.write_str("Could not configure the Git remote"),
            Self::FetchFailed => formatter.write_str("Could not fetch the Git release tag"),
            Self::TransferLimitExceeded { limit_bytes } => write!(
                formatter,
                "Git fetch exceeded the {limit_bytes}-byte transfer limit"
            ),
            Self::TagNotFound { name } => write!(formatter, "Git release tag {name} was not found"),
            Self::LockedCommitUnavailable { name, commit } => write!(
                formatter,
                "Git release tag {name} is missing and locked commit {commit} is unavailable from the source"
            ),
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
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "Git package source checksum mismatch (expected {expected}, got {actual})"
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
    directory: Option<&DependencyPackagePath>,
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
    directory: Option<&DependencyPackagePath>,
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
            url: &canonical_url,
            refspec: &refspec,
            tag_name: tag.name(),
            version: tag.version(),
            directory,
            expected_commit,
            expected_source_checksum,
            transfer_limit: &transfer_limit,
        },
    )
}

/// Fetch a lock-selected release even when its tag disappeared before this call.
///
/// The release ref is derived from the locked version and package directory,
/// and the temporary repository uses the locked commit's object-hash kind.
/// The commit and source checksum are mandatory so both the tag path and the
/// deleted-tag fallback are verified against the lock.
pub fn fetch_locked_release_tree(
    url: &str,
    version: &Version,
    directory: Option<&DependencyPackagePath>,
    locked_commit: &gix::ObjectId,
    locked_source_checksum: &str,
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
        &canonical_url,
        version,
        directory,
        locked_commit,
        locked_source_checksum,
        &transfer_limit,
    )
}

fn fetch_locked_release_tree_in_repository(
    repository: &gix::Repository,
    url: &str,
    version: &Version,
    directory: Option<&DependencyPackagePath>,
    locked_commit: &gix::ObjectId,
    locked_source_checksum: &str,
    transfer_limit: &gix_transport::client::blocking_io::FetchTransferLimit,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let tag_name = release_tag_name(version, directory)?;
    let refspec = format!("{tag_name}:refs/sai/package-release-tag");
    fetch_release_tree_in_repository(
        repository,
        ReleaseFetchRequest {
            url,
            refspec: &refspec,
            tag_name: &tag_name,
            version,
            directory,
            expected_commit: Some(locked_commit),
            expected_source_checksum: Some(locked_source_checksum),
            transfer_limit,
        },
    )
}

struct ReleaseFetchRequest<'a> {
    url: &'a str,
    refspec: &'a str,
    tag_name: &'a str,
    version: &'a Version,
    directory: Option<&'a DependencyPackagePath>,
    expected_commit: Option<&'a gix::ObjectId>,
    expected_source_checksum: Option<&'a str>,
    transfer_limit: &'a gix_transport::client::blocking_io::FetchTransferLimit,
}

fn fetch_release_tree_in_repository(
    repository: &gix::Repository,
    request: ReleaseFetchRequest<'_>,
) -> Result<FetchedGitRelease, GitRemoteReleaseFetchError> {
    let ReleaseFetchRequest {
        url,
        refspec,
        tag_name,
        version,
        directory,
        expected_commit,
        expected_source_checksum,
        transfer_limit,
    } = request;
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
        fetch_locked_commit(repository, url, expected_commit, tag_name, transfer_limit)?;
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

    let package_directory = match directory.map(DependencyPackagePath::as_str) {
        None | Some(".") => &[][..],
        Some(directory) if directory.split('/').any(|component| component == "..") => {
            return Err(GitRemoteReleaseFetchError::InvalidPackageDirectory);
        }
        Some(directory) => directory.as_bytes(),
    };
    let source_tree = reader
        .hash_commit_tree(&commit_id, package_directory, TreeHashPurpose::Source)
        .map_err(GitRemoteReleaseFetchError::SourceTree)?;
    if let Some(expected_source_checksum) = expected_source_checksum
        && source_tree.checksum != expected_source_checksum
    {
        return Err(GitRemoteReleaseFetchError::ChecksumMismatch {
            expected: expected_source_checksum.into(),
            actual: source_tree.checksum.clone().into(),
        });
    }

    Ok(FetchedGitRelease {
        tag_name: tag_name.into(),
        version: version.clone(),
        tag_object_id,
        tag_missing,
        commit_id,
        source_tree,
    })
}

fn fetch_locked_commit(
    repository: &gix::Repository,
    url: &str,
    commit_id: &gix::ObjectId,
    tag_name: &str,
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
                GitRemoteReleaseFetchError::LockedCommitUnavailable {
                    name: tag_name.into(),
                    commit: *commit_id,
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
    directory: Option<&DependencyPackagePath>,
) -> Result<String, GitRemoteReleaseFetchError> {
    let expected_tag_name = release_tag_name(tag.version(), directory)?;
    if tag.name() != expected_tag_name {
        return Err(GitRemoteReleaseFetchError::InvalidPackageDirectory);
    }
    Ok(format!("{expected_tag_name}:refs/sai/package-release-tag"))
}

fn release_tag_name(
    version: &Version,
    directory: Option<&DependencyPackagePath>,
) -> Result<String, GitRemoteReleaseFetchError> {
    let prefix = match directory.map(DependencyPackagePath::as_str) {
        None | Some(".") => "refs/tags/v".to_owned(),
        Some(directory) if directory.split('/').any(|component| component == "..") => {
            return Err(GitRemoteReleaseFetchError::InvalidPackageDirectory);
        }
        Some(directory) => format!("refs/tags/{directory}/v"),
    };
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
    directory: Option<&DependencyPackagePath>,
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
    directory: Option<&DependencyPackagePath>,
) -> Result<String, GitRemoteTagListError> {
    let prefix = match directory.map(DependencyPackagePath::as_str) {
        None | Some(".") => "refs/tags/v".to_owned(),
        Some(directory) if directory.split('/').any(|component| component == "..") => {
            return Err(GitRemoteTagListError::InvalidPackageDirectory);
        }
        Some(directory) => format!("refs/tags/{directory}/v"),
    };
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
}

impl TemporaryGitDirectory {
    fn create() -> Result<Self, ()> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir();
        for _ in 0..128 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let nonce = RandomState::new().hash_one((std::process::id(), id));
            let path = base.join(format!(
                "sai-git-remote-{}-{id}-{nonce:016x}",
                std::process::id()
            ));
            match create_temporary_directory(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(()),
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
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{
        GitRemoteReleaseFetchError, GitRemoteTagListError, TemporaryGitDirectory,
        credential_overrides_from_file, credential_overrides_from_path,
        fetch_release_tree_in_repository, list_release_tags_in_repository, map_transport_error,
        proxy_environment_overrides, release_tag_refspec, user_credential_config_paths,
    };
    use crate::git_tags::{GitReleaseTagListError, GitReleaseTagListLimit, parse_release_tag_refs};
    use crate::package_path::DependencyPackagePath;
    use gix::objs::tree::{Entry, EntryKind};

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
            let directory = TemporaryGitDirectory::create().unwrap();
            let repository = init_bare_repository(&directory.path, Some(object_hash));
            let blob_id = repository.write_blob(b"package source").unwrap().detach();
            let tree_id = repository
                .write_object(gix::objs::Tree {
                    entries: vec![Entry {
                        mode: EntryKind::Blob.into(),
                        filename: b"lib.sai".to_vec().into(),
                        oid: blob_id,
                    }],
                })
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

        fn url(&self) -> String {
            format!("file://{}", self.directory.path.display())
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
        let scratch = TemporaryGitDirectory::create().unwrap();
        let repository = init_bare_repository(&scratch.path, Some(remote.tag.object_id().kind()));
        let refspec = super::exact_release_tag_refspec(&remote.tag, None).unwrap();
        let transfer_limit =
            gix_transport::client::blocking_io::FetchTransferLimit::new(super::MAX_GIT_FETCH_BYTES);
        fetch_release_tree_in_repository(
            &repository,
            super::ReleaseFetchRequest {
                url: &remote.url(),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory: None,
                expected_commit,
                expected_source_checksum: expected_checksum,
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
        let directory = DependencyPackagePath::parse("packages/text").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&directory)).unwrap(),
            "refs/tags/packages/text/v*:refs/tags/packages/text/v*"
        );
    }

    #[test]
    fn rejects_dependency_directories_that_escape_the_repository() {
        let directory = DependencyPackagePath::parse("../shared").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&directory)),
            Err(GitRemoteTagListError::InvalidPackageDirectory)
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
        assert!(fetched.source_tree().checksum.starts_with("sai1:"));
        assert!(fetched.source_tree().warnings.is_empty());
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
                url: &remote.url(),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory: None,
                expected_commit: None,
                expected_source_checksum: None,
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
                url: &remote.url(),
                refspec: &refspec,
                tag_name: remote.tag.name(),
                version: remote.tag.version(),
                directory: None,
                expected_commit: None,
                expected_source_checksum: None,
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

        let fetched = super::fetch_locked_release_tree_in_repository(
            &repository,
            &remote.url(),
            &version,
            None,
            &remote.commit_id,
            &checksum,
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
    fn exact_fetch_rejects_a_tag_from_another_package_namespace() {
        let remote = TestRemote::new("refs/tags/v1.2.3", false);
        let directory = DependencyPackagePath::parse("packages/core").unwrap();

        assert_eq!(
            super::exact_release_tag_refspec(&remote.tag, Some(&directory)),
            Err(GitRemoteReleaseFetchError::InvalidPackageDirectory)
        );
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
