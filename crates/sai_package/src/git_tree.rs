//! Reads and hashes Git trees from an object database without a checkout.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io;
use std::path::Path;

use gix::prelude::{FindExt, HeaderExt};
use sha2::{Digest, Sha256};

use crate::manifest::{
    MAX_PACKAGE_MANIFEST_BYTES, PackageDeclaration, PackageParseOutput, PackageRole, parse_for_role,
};
use crate::tree_hash::{
    GitTreeDigestEntry, TreeHash, TreeHashError, TreeHashPurpose, artifact_checksum,
    hash_git_tree_digests, should_skip_path, validate_tree_path,
};

/// Maximum size of a blob selected from one package directory.
pub const MAX_PACKAGE_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum combined size of selected blobs in one package directory.
pub const MAX_PACKAGE_CONTENT_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum number of regular files selected from one package directory.
pub const MAX_PACKAGE_FILE_COUNT: u64 = 100_000;
/// Maximum size of an individual commit, tag, or tree object read while hashing.
pub const MAX_GIT_METADATA_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of annotated tag objects followed while peeling one release tag.
pub const MAX_TAG_CHAIN_DEPTH: u64 = 64;
/// Maximum number of entries inspected while walking one package tree.
pub const MAX_PACKAGE_TREE_ENTRIES: u64 = 1_000_000;
/// Maximum cumulative byte length of paths retained while walking one package tree.
pub const MAX_PACKAGE_TREE_PATH_BYTES: u64 = 64 * 1024 * 1024;

const MAX_OBJECT_ALLOCATION_BYTES: usize = MAX_PACKAGE_FILE_BYTES as usize;
const MAX_GIX_INDEX_SLOTS: u16 = (1 << 15) - 1;

/// A limit enforced while reading a package tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitTreeLimit {
    /// Maximum selected regular files.
    FileCount,
    /// Maximum bytes in one selected regular file.
    FileBytes,
    /// Maximum total bytes in selected regular files.
    PackageBytes,
    /// Maximum commit, tag, or tree object size.
    MetadataObjectBytes,
    /// Maximum number of annotated tags in one release-tag chain.
    TagChainDepth,
    /// Maximum entries inspected across the package tree.
    TreeEntryCount,
    /// Maximum cumulative bytes in paths retained while walking the package tree.
    PathBytes,
    /// Maximum size of the `sai.package` manifest.
    ManifestBytes,
}

impl GitTreeLimit {
    fn name(self) -> &'static str {
        match self {
            Self::FileCount => "selected file count",
            Self::FileBytes => "one selected file size",
            Self::PackageBytes => "selected package content size",
            Self::MetadataObjectBytes => "Git metadata object size",
            Self::TagChainDepth => "annotated tag chain depth",
            Self::TreeEntryCount => "inspected Git tree entry count",
            Self::PathBytes => "cumulative Git tree path bytes",
            Self::ManifestBytes => "package manifest size",
        }
    }
}

/// A failure while opening or traversing a Git object database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitTreeReadError {
    /// The object database could not be opened or an object could not be read.
    ObjectDatabase,
    /// The object named by the commit id is not a readable commit.
    InvalidCommitObject,
    /// Object contents do not match the object id used to look them up.
    ObjectIdMismatch,
    /// A tree object is malformed or unreadable.
    InvalidTreeObject,
    /// A tag object is malformed or unreadable.
    InvalidTagObject,
    /// The release tag ultimately points to a non-commit object.
    InvalidTagTarget {
        /// The actual kind of the terminal target object.
        kind: gix::objs::Kind,
    },
    /// A tag's declared target kind differs from the actual target object kind.
    InvalidTagTargetKind {
        /// The kind declared by the tag object.
        declared: gix::objs::Kind,
        /// The actual kind of the target object.
        actual: gix::objs::Kind,
    },
    /// A tree contains an unsupported entry mode or invalid component.
    InvalidTreeEntry,
    /// The selected package tree has no regular `sai.package` file.
    MissingPackageManifest,
    /// A declared WIT directory does not resolve to a Git tree.
    WitDirectoryNotFound {
        /// The package-relative WIT directory path.
        path: Box<str>,
    },
    /// A declared artifact path does not resolve to a regular package file.
    ArtifactNotFound {
        /// The package-relative artifact file path.
        path: Box<str>,
    },
    /// The requested package directory is not a normalized relative path.
    InvalidPackageDirectory,
    /// A component of the requested package directory does not exist.
    MissingPackageDirectory,
    /// A component of the requested package directory is not a directory.
    PackagePathNotDirectory,
    /// The object database uses alternates, which could expose objects outside it.
    AlternatesUnsupported,
    /// A fixed package resource limit was exceeded.
    PackageTooLarge {
        /// The exceeded limit.
        limit: GitTreeLimit,
        /// The value reached when the limit was exceeded.
        actual: u64,
    },
    /// A path or tree-hash check failed.
    Hash(TreeHashError),
}

impl Display for GitTreeReadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ObjectDatabase => formatter.write_str("could not read the Git object database"),
            Self::InvalidCommitObject => formatter.write_str("Git object is not a valid commit"),
            Self::ObjectIdMismatch => {
                formatter.write_str("Git object contents do not match their object id")
            }
            Self::InvalidTreeObject => {
                formatter.write_str("Git tree object is malformed or missing")
            }
            Self::InvalidTagObject => formatter.write_str("Git tag object is malformed or missing"),
            Self::InvalidTagTarget { kind } => {
                write!(formatter, "Git tag points to a {kind}, not a commit")
            }
            Self::InvalidTagTargetKind { declared, actual } => {
                write!(
                    formatter,
                    "Git tag declares a {declared} target but points to a {actual}"
                )
            }
            Self::InvalidTreeEntry => formatter.write_str("Git tree contains an invalid entry"),
            Self::MissingPackageManifest => {
                formatter.write_str("package directory has no regular sai.package file")
            }
            Self::WitDirectoryNotFound { path } => {
                write!(formatter, "WIT directory `{path}` does not exist")
            }
            Self::ArtifactNotFound { path } => {
                write!(formatter, "artifact `{path}` is not a regular package file")
            }
            Self::InvalidPackageDirectory => {
                formatter.write_str("package directory is not a normalized relative path")
            }
            Self::MissingPackageDirectory => {
                formatter.write_str("package directory does not exist in the commit")
            }
            Self::PackagePathNotDirectory => {
                formatter.write_str("package directory path crosses a non-directory entry")
            }
            Self::AlternatesUnsupported => {
                formatter.write_str("Git object database alternates are not supported")
            }
            Self::PackageTooLarge { limit, actual } => {
                write!(
                    formatter,
                    "{} exceeded (reached {actual} bytes or entries)",
                    limit.name()
                )
            }
            Self::Hash(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for GitTreeReadError {}

impl From<TreeHashError> for GitTreeReadError {
    fn from(error: TreeHashError) -> Self {
        Self::Hash(error)
    }
}

#[derive(Clone, Copy)]
struct Limits {
    file_count: u64,
    file_bytes: u64,
    package_bytes: u64,
    metadata_object_bytes: u64,
    tree_entry_count: u64,
    path_bytes: u64,
}

impl Limits {
    const SPEC: Self = Self {
        file_count: MAX_PACKAGE_FILE_COUNT,
        file_bytes: MAX_PACKAGE_FILE_BYTES,
        package_bytes: MAX_PACKAGE_CONTENT_BYTES,
        metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
        tree_entry_count: MAX_PACKAGE_TREE_ENTRIES,
        path_bytes: MAX_PACKAGE_TREE_PATH_BYTES,
    };
}

#[derive(Clone, Debug)]
struct RawTreeEntry {
    mode: u16,
    name: Vec<u8>,
    object_id: gix::ObjectId,
}

enum OwnedTreeEntry {
    File {
        path: Vec<u8>,
        content: Vec<u8>,
        content_digest: [u8; 32],
        executable: bool,
    },
    HashedFile {
        path: Vec<u8>,
        content_digest: [u8; 32],
        executable: bool,
    },
    Directory {
        path: Vec<u8>,
    },
    Symlink {
        path: Vec<u8>,
    },
    Gitlink {
        path: Vec<u8>,
    },
}

struct HashedTreeContents {
    source_tree: TreeHash,
    package_manifest: Option<Vec<u8>>,
    entries: Vec<OwnedTreeEntry>,
}

/// One regular package file selected for the source checksum and cache.
#[derive(Debug, Eq, PartialEq)]
pub struct GitPackageFile {
    path: Box<str>,
    content: Vec<u8>,
}

impl GitPackageFile {
    /// The validated UTF-8 path relative to the package directory.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The exact blob bytes stored in the Git commit.
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// A validated source-tree snapshot ready for contract resolution or caching.
#[derive(Debug, Eq, PartialEq)]
pub struct GitPackageSourceTree {
    source_tree: TreeHash,
    wit_tree: Option<TreeHash>,
    artifact_checksums: Vec<GitPackageArtifactChecksum>,
    package_manifest: PackageParseOutput,
    files: Vec<GitPackageFile>,
}

/// The checked checksum for one component artifact in a Git package.
#[derive(Debug, Eq, PartialEq)]
pub struct GitPackageArtifactChecksum {
    target: Box<str>,
    path: Box<str>,
    checksum: Box<str>,
}

impl GitPackageArtifactChecksum {
    /// The target that selects this artifact.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The package-relative artifact file path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The `sai1:` SHA-256 checksum of the exact Git blob bytes.
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}

impl GitPackageSourceTree {
    /// The source checksum and warnings computed from this snapshot.
    pub const fn source_tree(&self) -> &TreeHash {
        &self.source_tree
    }

    /// The WIT checksum and warnings, when the manifest declares a WIT directory.
    pub const fn wit_tree(&self) -> Option<&TreeHash> {
        self.wit_tree.as_ref()
    }

    /// The artifact checksums, sorted by target bytes.
    pub fn artifact_checksums(&self) -> &[GitPackageArtifactChecksum] {
        &self.artifact_checksums
    }

    /// The dependency-role parse result from the root `sai.package`.
    pub const fn package_manifest(&self) -> &PackageParseOutput {
        &self.package_manifest
    }

    /// The selected regular files, sorted by their UTF-8 path bytes.
    pub fn files(&self) -> &[GitPackageFile] {
        &self.files
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        TreeHash,
        Option<TreeHash>,
        Vec<GitPackageArtifactChecksum>,
        PackageParseOutput,
        Vec<GitPackageFile>,
    ) {
        (
            self.source_tree,
            self.wit_tree,
            self.artifact_checksums,
            self.package_manifest,
            self.files,
        )
    }
}

/// A reader over a Git object directory.
///
/// The reader takes an explicit object hash kind, reads only that object
/// directory, ignores Git configuration and refs, applies no attributes or
/// filters, and never checks out files or follows Git symlinks.
pub struct GitTreeReader {
    objects: gix::odb::Handle,
    object_hash: gix::hash::Kind,
    limits: Limits,
}

impl GitTreeReader {
    /// Open the `objects/` directory of a bare Git repository.
    ///
    /// The caller must keep the object directory stable for the reader's
    /// lifetime. On Unix, package fetching hashes a uniquely named staging
    /// repository whose permission mode denies group and other access before
    /// publishing it to the immutable cache. Inherited filesystem ACLs are
    /// not cleared. Concurrent hostile mutation of the host filesystem is
    /// outside the package system's trust model.
    pub fn open(
        objects_directory: impl AsRef<Path>,
        object_hash: gix::hash::Kind,
    ) -> Result<Self, GitTreeReadError> {
        Self::open_with_limits(objects_directory, object_hash, Limits::SPEC)
    }

    fn open_with_limits(
        objects_directory: impl AsRef<Path>,
        object_hash: gix::hash::Kind,
        limits: Limits,
    ) -> Result<Self, GitTreeReadError> {
        let objects_directory = objects_directory.as_ref();
        let alternates = objects_directory.join("info").join("alternates");
        match fs::symlink_metadata(alternates) {
            Ok(_) => return Err(GitTreeReadError::AlternatesUnsupported),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(GitTreeReadError::ObjectDatabase),
        }

        let options = gix::odb::store::init::Options {
            // Avoid resolving alternates while determining the initial slot count.
            // This also leaves room for every pack index supported by gix.
            slots: gix::odb::store::init::Slots::Given(MAX_GIX_INDEX_SLOTS),
            alloc_limit_bytes: Some(MAX_OBJECT_ALLOCATION_BYTES),
            ..Default::default()
        };
        let mut objects = gix::odb::at_opts(
            objects_directory.to_path_buf(),
            object_hash,
            std::iter::empty(),
            options,
        )
        .map_err(|_| GitTreeReadError::ObjectDatabase)?;
        // The ODB resolves alternates on its first disk refresh. Initialize it
        // below, reject any resolved paths, and then keep that path list fixed.
        objects.refresh_never();

        let reader = Self {
            objects,
            object_hash,
            limits,
        };
        reader.ensure_no_alternates()?;
        Ok(reader)
    }

    /// Hash the package directory at `package_directory` in `commit_id`.
    ///
    /// `package_directory` is empty for the repository root and otherwise
    /// consists of slash-separated Git path bytes.
    pub fn hash_commit_tree(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
        purpose: TreeHashPurpose,
    ) -> Result<TreeHash, GitTreeReadError> {
        self.hash_commit_tree_internal(commit_id, package_directory, purpose, false, false)
            .map(|contents| contents.source_tree)
    }

    /// Hash a package's source tree and read its root `sai.package` manifest
    /// during the same bounded tree walk.
    ///
    /// The manifest blob is checked against the 16 MiB package-contract limit
    /// before its contents are loaded, and its Git object ID is verified with
    /// the same object reader used for the source checksum.
    pub fn hash_commit_tree_and_read_manifest(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
    ) -> Result<(TreeHash, Vec<u8>), GitTreeReadError> {
        let mut contents = self.hash_commit_tree_internal(
            commit_id,
            package_directory,
            TreeHashPurpose::Source,
            true,
            false,
        )?;
        let source_tree = contents.source_tree;
        contents
            .package_manifest
            .take()
            .map(|manifest| (source_tree, manifest))
            .ok_or(GitTreeReadError::MissingPackageManifest)
    }

    /// Read the selected source files and validate the root package manifest
    /// while computing the source checksum in one bounded tree walk.
    pub fn read_commit_package_source_tree(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
    ) -> Result<GitPackageSourceTree, GitTreeReadError> {
        let mut contents = self.hash_commit_tree_internal(
            commit_id,
            package_directory,
            TreeHashPurpose::Source,
            true,
            true,
        )?;
        let mut manifest_bytes = contents
            .package_manifest
            .take()
            .ok_or(GitTreeReadError::MissingPackageManifest)?;
        let package_manifest = parse_for_role(&manifest_bytes, PackageRole::Dependency);
        let wit_tree = package_manifest
            .file()
            .declarations
            .iter()
            .find_map(|declaration| match &declaration.value {
                PackageDeclaration::Wit(path) => Some(path.as_ref()),
                _ => None,
            })
            .map(|path| self.hash_commit_wit_tree(commit_id, package_directory, path))
            .transpose()?;
        let mut files = Vec::with_capacity(contents.entries.len());
        for entry in contents.entries {
            if let OwnedTreeEntry::File { path, content, .. } = entry {
                let content = if path == b"sai.package" {
                    std::mem::take(&mut manifest_bytes)
                } else {
                    content
                };
                let path = String::from_utf8(path)
                    .map_err(|error| {
                        GitTreeReadError::Hash(TreeHashError::InvalidPath {
                            path: error.into_bytes(),
                            reason: crate::tree_hash::InvalidPathReason::InvalidUtf8,
                        })
                    })?
                    .into_boxed_str();
                files.push(GitPackageFile { path, content });
            }
        }
        files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        let artifact_checksums = collect_artifact_checksums(&package_manifest, &files)?;

        Ok(GitPackageSourceTree {
            source_tree: contents.source_tree,
            wit_tree,
            artifact_checksums,
            package_manifest,
            files,
        })
    }

    fn hash_commit_tree_internal(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
        purpose: TreeHashPurpose,
        read_manifest: bool,
        retain_files: bool,
    ) -> Result<HashedTreeContents, GitTreeReadError> {
        let mut inspected_entries = 0;
        let package_tree_id =
            self.resolve_commit_package_tree(commit_id, package_directory, &mut inspected_entries)?;
        self.hash_tree(
            &package_tree_id,
            purpose,
            &mut inspected_entries,
            read_manifest,
            retain_files,
        )
    }

    fn hash_commit_wit_tree(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
        wit_directory: &str,
    ) -> Result<TreeHash, GitTreeReadError> {
        let mut inspected_entries = 0;
        let package_tree_id =
            self.resolve_commit_package_tree(commit_id, package_directory, &mut inspected_entries)?;
        let wit_tree_id = if wit_directory == "." {
            package_tree_id
        } else {
            validate_tree_path(wit_directory.as_bytes())?;
            self.resolve_package_tree(
                &package_tree_id,
                wit_directory.as_bytes(),
                &mut inspected_entries,
            )
            .map_err(|error| match error {
                GitTreeReadError::MissingPackageDirectory
                | GitTreeReadError::PackagePathNotDirectory => {
                    GitTreeReadError::WitDirectoryNotFound {
                        path: wit_directory.into(),
                    }
                }
                error => error,
            })?
        };
        self.hash_tree(
            &wit_tree_id,
            TreeHashPurpose::Wit,
            &mut inspected_entries,
            false,
            false,
        )
        .map(|contents| contents.source_tree)
    }

    fn resolve_commit_package_tree(
        &self,
        commit_id: &gix::ObjectId,
        package_directory: &[u8],
        inspected_entries: &mut u64,
    ) -> Result<gix::ObjectId, GitTreeReadError> {
        self.ensure_no_alternates()?;
        if !package_directory.is_empty() {
            validate_tree_path(package_directory)?;
        }

        if commit_id.kind() != self.object_hash {
            return Err(GitTreeReadError::InvalidCommitObject);
        }
        let header = self.read_object_header(commit_id, GitTreeReadError::InvalidCommitObject)?;
        if header.kind() != gix::objs::Kind::Commit
            || header.size() > self.limits.metadata_object_bytes
        {
            if header.size() > self.limits.metadata_object_bytes {
                return Err(GitTreeReadError::PackageTooLarge {
                    limit: GitTreeLimit::MetadataObjectBytes,
                    actual: header.size(),
                });
            }
            return Err(GitTreeReadError::InvalidCommitObject);
        }

        let (commit_kind, commit_data) =
            self.find_verified_object(commit_id, GitTreeReadError::InvalidCommitObject)?;
        if commit_kind != gix::objs::Kind::Commit {
            return Err(GitTreeReadError::InvalidCommitObject);
        }
        let root_tree_id = gix::objs::CommitRefIter::from_bytes(&commit_data, self.object_hash)
            .tree_id()
            .map_err(|_| GitTreeReadError::InvalidCommitObject)?;
        self.resolve_package_tree(&root_tree_id, package_directory, inspected_entries)
    }

    /// Follow a lightweight or annotated tag to its terminal commit object.
    ///
    /// Annotated tags may refer to other annotated tags. The object's own
    /// content is checked against its ID at every step, and the declared
    /// target kind must match the actual target object kind.
    pub fn peel_tag_to_commit(
        &self,
        tag_or_commit_id: &gix::ObjectId,
    ) -> Result<gix::ObjectId, GitTreeReadError> {
        if tag_or_commit_id.kind() != self.object_hash {
            return Err(GitTreeReadError::InvalidTagObject);
        }

        self.ensure_no_alternates()?;
        let mut current_id = *tag_or_commit_id;
        let mut seen = BTreeSet::new();
        let mut tag_depth = 0u64;
        let mut expected_target_kind = None;

        loop {
            if !seen.insert(current_id) {
                return Err(GitTreeReadError::InvalidTagObject);
            }
            let header =
                self.read_object_header(&current_id, GitTreeReadError::InvalidTagObject)?;
            if let Some(declared) = expected_target_kind
                && declared != header.kind()
            {
                return Err(GitTreeReadError::InvalidTagTargetKind {
                    declared,
                    actual: header.kind(),
                });
            }
            ensure_limit(
                GitTreeLimit::MetadataObjectBytes,
                header.size(),
                self.limits.metadata_object_bytes,
            )?;

            match header.kind() {
                gix::objs::Kind::Commit => {
                    let (kind, _) =
                        self.find_verified_object(&current_id, GitTreeReadError::InvalidTagObject)?;
                    if kind != gix::objs::Kind::Commit {
                        return Err(GitTreeReadError::InvalidTagObject);
                    }
                    return Ok(current_id);
                }
                gix::objs::Kind::Tag => {
                    tag_depth = tag_depth.saturating_add(1);
                    ensure_limit(GitTreeLimit::TagChainDepth, tag_depth, MAX_TAG_CHAIN_DEPTH)?;
                    let (kind, data) =
                        self.find_verified_object(&current_id, GitTreeReadError::InvalidTagObject)?;
                    if kind != gix::objs::Kind::Tag {
                        return Err(GitTreeReadError::InvalidTagObject);
                    }
                    let tag = gix::objs::TagRef::from_bytes(&data, self.object_hash)
                        .map_err(|_| GitTreeReadError::InvalidTagObject)?;
                    current_id = tag.target();
                    expected_target_kind = Some(tag.target_kind);
                }
                kind => return Err(GitTreeReadError::InvalidTagTarget { kind }),
            }
        }
    }

    fn ensure_no_alternates(&self) -> Result<(), GitTreeReadError> {
        let alternate_paths = self
            .objects
            .store_ref()
            .alternate_db_paths()
            .map_err(|_| GitTreeReadError::ObjectDatabase)?;
        if alternate_paths.is_empty() {
            Ok(())
        } else {
            Err(GitTreeReadError::AlternatesUnsupported)
        }
    }

    fn read_object_header(
        &self,
        object_id: &gix::ObjectId,
        read_error: GitTreeReadError,
    ) -> Result<gix::odb::find::Header, GitTreeReadError> {
        self.ensure_no_alternates()?;
        let header = self
            .objects
            .header(object_id.as_ref())
            .map_err(|_| read_error)?;
        self.ensure_no_alternates()?;
        Ok(header)
    }

    fn find_verified_object(
        &self,
        object_id: &gix::ObjectId,
        read_error: GitTreeReadError,
    ) -> Result<(gix::objs::Kind, Vec<u8>), GitTreeReadError> {
        self.ensure_no_alternates()?;
        let mut buffer = Vec::new();
        let object = self
            .objects
            .find(object_id.as_ref(), &mut buffer)
            .map_err(|_| read_error)?;
        let kind = object.kind;
        if gix::objs::compute_hash(self.object_hash, kind, object.data)
            .map_err(|_| GitTreeReadError::ObjectIdMismatch)?
            != *object_id
        {
            return Err(GitTreeReadError::ObjectIdMismatch);
        }
        self.ensure_no_alternates()?;
        Ok((kind, buffer))
    }

    fn resolve_package_tree(
        &self,
        root_tree_id: &gix::ObjectId,
        package_directory: &[u8],
        inspected_entries: &mut u64,
    ) -> Result<gix::ObjectId, GitTreeReadError> {
        if package_directory.is_empty() {
            return Ok(*root_tree_id);
        }

        let mut current_tree_id = *root_tree_id;
        for component in package_directory.split(|byte| *byte == b'/') {
            let entries = self.read_tree_entries(&current_tree_id, inspected_entries)?;
            let mut matching_entry = None;
            for entry in entries {
                if entry.name == component {
                    if matching_entry.is_some() {
                        return Err(GitTreeReadError::InvalidTreeEntry);
                    }
                    matching_entry = Some(entry);
                }
            }
            let Some(entry) = matching_entry else {
                return Err(GitTreeReadError::MissingPackageDirectory);
            };
            if entry.mode != 0o040000 {
                return Err(GitTreeReadError::PackagePathNotDirectory);
            }
            current_tree_id = entry.object_id;
        }
        Ok(current_tree_id)
    }

    fn hash_tree(
        &self,
        root_tree_id: &gix::ObjectId,
        purpose: TreeHashPurpose,
        tree_entry_count: &mut u64,
        read_manifest: bool,
        retain_files: bool,
    ) -> Result<HashedTreeContents, GitTreeReadError> {
        let mut pending = vec![(*root_tree_id, Vec::<u8>::new())];
        let mut owned_entries = Vec::<OwnedTreeEntry>::new();
        let mut package_manifest = None;
        let mut file_count = 0u64;
        let mut package_bytes = 0u64;
        let mut path_bytes = 0u64;

        while let Some((tree_id, parent_path)) = pending.pop() {
            for entry in self.read_tree_entries(&tree_id, tree_entry_count)? {
                if entry.name.is_empty() || entry.name.contains(&b'/') {
                    return Err(GitTreeReadError::InvalidTreeEntry);
                }
                let mut path = parent_path.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(&entry.name);
                if should_skip_path(&path, purpose) {
                    continue;
                }
                path_bytes = path_bytes.saturating_add(path.len() as u64);
                ensure_limit(GitTreeLimit::PathBytes, path_bytes, self.limits.path_bytes)?;

                match entry.mode {
                    0o040000 => {
                        if path.len() > 4096 {
                            validate_tree_path(&path)?;
                        }
                        owned_entries.push(OwnedTreeEntry::Directory { path: path.clone() });
                        pending.push((entry.object_id, path.clone()));
                    }
                    0o100644 | 0o100755 => {
                        validate_tree_path(&path)?;
                        file_count = file_count.saturating_add(1);
                        ensure_limit(GitTreeLimit::FileCount, file_count, self.limits.file_count)?;

                        let header = self.read_object_header(
                            &entry.object_id,
                            GitTreeReadError::InvalidTreeObject,
                        )?;
                        if header.kind() != gix::objs::Kind::Blob {
                            return Err(GitTreeReadError::InvalidTreeEntry);
                        }
                        ensure_limit(
                            GitTreeLimit::FileBytes,
                            header.size(),
                            self.limits.file_bytes,
                        )?;
                        let is_package_manifest = read_manifest && path == b"sai.package";
                        if is_package_manifest {
                            ensure_limit(
                                GitTreeLimit::ManifestBytes,
                                header.size(),
                                MAX_PACKAGE_MANIFEST_BYTES as u64,
                            )?;
                        }
                        package_bytes = package_bytes.saturating_add(header.size());
                        ensure_limit(
                            GitTreeLimit::PackageBytes,
                            package_bytes,
                            self.limits.package_bytes,
                        )?;

                        let (blob_kind, content) = self.find_verified_object(
                            &entry.object_id,
                            GitTreeReadError::InvalidTreeObject,
                        )?;
                        if blob_kind != gix::objs::Kind::Blob
                            || content.len() as u64 != header.size()
                        {
                            return Err(GitTreeReadError::InvalidTreeEntry);
                        }
                        let content_digest = Sha256::digest(&content).into();
                        if retain_files {
                            owned_entries.push(OwnedTreeEntry::File {
                                path,
                                content,
                                content_digest,
                                executable: entry.mode == 0o100755,
                            });
                        } else {
                            if is_package_manifest {
                                package_manifest = Some(content);
                            }
                            owned_entries.push(OwnedTreeEntry::HashedFile {
                                path,
                                content_digest,
                                executable: entry.mode == 0o100755,
                            });
                        }
                    }
                    0o120000 => owned_entries.push(OwnedTreeEntry::Symlink { path }),
                    0o160000 => owned_entries.push(OwnedTreeEntry::Gitlink { path }),
                    _ => return Err(GitTreeReadError::InvalidTreeEntry),
                }
            }
        }

        let hash_entries = owned_entries
            .iter()
            .map(|entry| -> GitTreeDigestEntry<'_> {
                match entry {
                    OwnedTreeEntry::File {
                        path,
                        content_digest,
                        executable: false,
                        ..
                    }
                    | OwnedTreeEntry::HashedFile {
                        path,
                        content_digest,
                        executable: false,
                    } => GitTreeDigestEntry::RegularFile {
                        path,
                        content_digest: *content_digest,
                    },
                    OwnedTreeEntry::File {
                        path,
                        content_digest,
                        executable: true,
                        ..
                    }
                    | OwnedTreeEntry::HashedFile {
                        path,
                        content_digest,
                        executable: true,
                    } => GitTreeDigestEntry::ExecutableFile {
                        path,
                        content_digest: *content_digest,
                    },
                    OwnedTreeEntry::Directory { path } => GitTreeDigestEntry::Directory { path },
                    OwnedTreeEntry::Symlink { path } => GitTreeDigestEntry::Symlink { path },
                    OwnedTreeEntry::Gitlink { path } => GitTreeDigestEntry::Gitlink { path },
                }
            })
            .collect::<Vec<_>>();
        let tree_hash = hash_git_tree_digests(hash_entries, purpose)?;
        if read_manifest && retain_files && package_manifest.is_none() {
            package_manifest = owned_entries.iter_mut().find_map(|entry| match entry {
                OwnedTreeEntry::File { path, content, .. } if path == b"sai.package" => {
                    Some(std::mem::take(content))
                }
                _ => None,
            });
        }
        Ok(HashedTreeContents {
            source_tree: tree_hash,
            package_manifest,
            entries: owned_entries,
        })
    }

    fn read_tree_entries(
        &self,
        tree_id: &gix::ObjectId,
        inspected_entries: &mut u64,
    ) -> Result<Vec<RawTreeEntry>, GitTreeReadError> {
        let header = self.read_object_header(tree_id, GitTreeReadError::InvalidTreeObject)?;
        if header.kind() != gix::objs::Kind::Tree {
            return Err(GitTreeReadError::InvalidTreeObject);
        }
        ensure_limit(
            GitTreeLimit::MetadataObjectBytes,
            header.size(),
            self.limits.metadata_object_bytes,
        )?;

        let (tree_kind, tree_data) =
            self.find_verified_object(tree_id, GitTreeReadError::InvalidTreeObject)?;
        if tree_kind != gix::objs::Kind::Tree || tree_data.len() as u64 != header.size() {
            return Err(GitTreeReadError::InvalidTreeObject);
        }

        let mut seen_names = BTreeSet::<Vec<u8>>::new();
        let mut result = Vec::new();
        for entry in gix::objs::TreeRefIter::from_bytes(&tree_data, self.object_hash) {
            let entry = entry.map_err(|_| GitTreeReadError::InvalidTreeObject)?;
            *inspected_entries = inspected_entries.saturating_add(1);
            ensure_limit(
                GitTreeLimit::TreeEntryCount,
                *inspected_entries,
                self.limits.tree_entry_count,
            )?;

            let name = entry.filename.to_vec();
            if name.is_empty() || name.contains(&b'/') || !seen_names.insert(name.clone()) {
                return Err(GitTreeReadError::InvalidTreeEntry);
            }
            result.push(RawTreeEntry {
                mode: entry.mode.value(),
                name,
                object_id: entry.oid.to_owned(),
            });
        }
        Ok(result)
    }
}

fn collect_artifact_checksums(
    manifest: &PackageParseOutput,
    files: &[GitPackageFile],
) -> Result<Vec<GitPackageArtifactChecksum>, GitTreeReadError> {
    collect_artifact_checksums_with(manifest, files, artifact_checksum)
}

fn collect_artifact_checksums_with(
    manifest: &PackageParseOutput,
    files: &[GitPackageFile],
    mut checksum: impl FnMut(&[u8]) -> String,
) -> Result<Vec<GitPackageArtifactChecksum>, GitTreeReadError> {
    let mut checksums = Vec::new();
    let mut checksums_by_file = Vec::new();
    checksums_by_file.resize_with(files.len(), || None::<Box<str>>);
    let mut seen_targets = BTreeSet::new();
    for declaration in &manifest.file().declarations {
        let PackageDeclaration::ArtifactComponents(entries) = &declaration.value else {
            continue;
        };
        for entry in entries {
            let target = entry.value.target.as_ref();
            if !crate::manifest::is_target_name(target)
                || !crate::validation::is_component_target(target)
                || !seen_targets.insert(target)
            {
                continue;
            }
            let path = entry.value.path.as_ref();
            let Ok(index) =
                files.binary_search_by(|file| file.path().as_bytes().cmp(path.as_bytes()))
            else {
                return Err(GitTreeReadError::ArtifactNotFound { path: path.into() });
            };
            let file = &files[index];
            let file_checksum = checksums_by_file[index]
                .get_or_insert_with(|| checksum(file.content()).into())
                .clone();
            checksums.push(GitPackageArtifactChecksum {
                target: entry.value.target.clone(),
                path: entry.value.path.clone(),
                checksum: file_checksum,
            });
        }
    }
    checksums.sort_by(|left, right| left.target.as_bytes().cmp(right.target.as_bytes()));
    Ok(checksums)
}

fn ensure_limit(limit: GitTreeLimit, actual: u64, maximum: u64) -> Result<(), GitTreeReadError> {
    if actual > maximum {
        Err(GitTreeReadError::PackageTooLarge { limit, actual })
    } else {
        Ok(())
    }
}

impl Display for GitTreeLimit {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GitPackageFile, GitTreeLimit, GitTreeReadError, GitTreeReader, Limits,
        MAX_GIT_METADATA_OBJECT_BYTES, MAX_PACKAGE_CONTENT_BYTES, MAX_PACKAGE_FILE_BYTES,
        MAX_PACKAGE_FILE_COUNT, MAX_PACKAGE_MANIFEST_BYTES, MAX_PACKAGE_TREE_ENTRIES,
        MAX_PACKAGE_TREE_PATH_BYTES, MAX_TAG_CHAIN_DEPTH, collect_artifact_checksums_with,
    };
    use crate::tree_hash::{
        GitTreeEntry, TreeHashError, TreeHashPurpose, artifact_checksum, hash_git_tree,
    };
    use gix::objs::Write as _;
    use gix::objs::tree::{Entry, EntryKind};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct BareTestRepository {
        path: PathBuf,
        repository: gix::Repository,
    }

    impl BareTestRepository {
        fn new() -> Self {
            Self::new_with_object_hash(None)
        }

        fn new_sha256() -> Self {
            Self::new_with_object_hash(Some(gix::hash::Kind::Sha256))
        }

        fn new_with_object_hash(object_hash: Option<gix::hash::Kind>) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-package-git-tree-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            let options = gix::create::Options {
                object_hash,
                ..Default::default()
            };
            let repository =
                gix::ThreadSafeRepository::init(&path, gix::create::Kind::Bare, options)
                    .unwrap()
                    .into();
            Self { path, repository }
        }

        fn objects_directory(&self) -> PathBuf {
            self.path.join("objects")
        }
    }

    impl Drop for BareTestRepository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write_blob(repository: &gix::Repository, content: &[u8]) -> gix::ObjectId {
        repository.write_blob(content).unwrap().detach()
    }

    fn write_tree(
        repository: &gix::Repository,
        entries: impl IntoIterator<Item = (&'static [u8], EntryKind, gix::ObjectId)>,
    ) -> gix::ObjectId {
        let mut entries: Vec<Entry> = entries
            .into_iter()
            .map(|(name, kind, object_id)| Entry {
                mode: kind.into(),
                filename: name.to_vec().into(),
                oid: object_id,
            })
            .collect();
        entries.sort();
        repository
            .write_object(gix::objs::Tree { entries })
            .unwrap()
            .detach()
    }

    fn write_commit(repository: &gix::Repository, tree: gix::ObjectId) -> gix::ObjectId {
        write_commit_with_message(repository, tree, "test commit")
    }

    fn write_commit_with_message(
        repository: &gix::Repository,
        tree: gix::ObjectId,
        message: &str,
    ) -> gix::ObjectId {
        repository
            .write_object(gix::objs::Commit {
                tree,
                parents: Default::default(),
                author: gix::actor::Signature::default(),
                committer: gix::actor::Signature::default(),
                encoding: None,
                message: message.into(),
                extra_headers: Vec::new(),
            })
            .unwrap()
            .detach()
    }

    fn write_tag(
        repository: &gix::Repository,
        target: gix::ObjectId,
        target_kind: gix::objs::Kind,
        name: &str,
        message: &str,
    ) -> gix::ObjectId {
        repository
            .write_object(gix::objs::Tag {
                target,
                target_kind,
                name: name.into(),
                tagger: None,
                message: message.into(),
                signature: None,
            })
            .unwrap()
            .detach()
    }

    fn write_pack(repository: &BareTestRepository, object_ids: &[gix::ObjectId]) {
        let object_hash = repository.repository.object_hash();
        let mut entries = object_ids
            .iter()
            .map(|id| {
                let object = repository.repository.find_object(*id).unwrap();
                let data = gix::objs::Data {
                    kind: object.kind,
                    object_hash,
                    data: &object.data,
                };
                let count = gix_pack::data::output::Count::from_data(*id, None);
                gix_pack::data::output::Entry::from_data(
                    &count,
                    &data,
                    gix::zlib::Compression::DEFAULT,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.id);

        let mut pack_writer = gix_pack::data::output::bytes::FromEntriesIter::new(
            std::iter::once(Ok::<_, std::io::Error>(entries)),
            Vec::new(),
            object_ids.len().try_into().unwrap(),
            gix_pack::data::Version::V2,
            object_hash,
        );
        for result in pack_writer.by_ref() {
            result.unwrap();
        }
        let pack_bytes = pack_writer.into_write();
        let pack_directory = repository.objects_directory().join("pack");
        std::fs::create_dir_all(&pack_directory).unwrap();
        let mut input = Cursor::new(pack_bytes);
        let outcome = gix_pack::Bundle::write_to_directory(
            &mut input,
            Some(&pack_directory),
            &mut gix::progress::Discard,
            &std::sync::atomic::AtomicBool::new(false),
            None::<gix::odb::Handle>,
            object_hash,
            gix_pack::bundle::write::Options::default(),
        )
        .unwrap();
        if let Some(keep_path) = outcome.keep_path {
            std::fs::remove_file(keep_path).unwrap();
        }
    }

    fn loose_object_path(repository: &BareTestRepository, id: &gix::ObjectId) -> PathBuf {
        let hex = id.to_hex().to_string();
        repository
            .objects_directory()
            .join(&hex[..2])
            .join(&hex[2..])
    }

    fn overwrite_loose_object(
        repository: &BareTestRepository,
        expected_id: &gix::ObjectId,
        replacement_id: &gix::ObjectId,
    ) {
        let replacement_path = loose_object_path(repository, replacement_id);
        let expected_path = loose_object_path(repository, expected_id);
        std::fs::remove_file(&expected_path).unwrap();
        std::fs::copy(replacement_path, expected_path).unwrap();
    }

    fn standard_reader(repository: &BareTestRepository) -> GitTreeReader {
        GitTreeReader::open(repository.objects_directory(), gix::hash::Kind::Sha1).unwrap()
    }

    fn full_test_tree(repository: &gix::Repository) -> (gix::ObjectId, gix::ObjectId) {
        let manifest = write_blob(repository, b"package acme:sample @ 1.0.0\n");
        let source = write_blob(repository, b"pub fn answer() -> i32 { 42 }\n");
        let state = write_blob(repository, b"private state\n");
        let output = write_blob(repository, b"wasm output\n");
        let git_metadata = write_blob(repository, b"unselected\n");
        let link_target = write_blob(repository, b"../../outside\n");
        let submodule_id = write_blob(repository, b"placeholder oid\n");

        let source_tree = write_tree(
            repository,
            [(b"main.sai".as_slice(), EntryKind::Blob, source)],
        );
        let sai_tree = write_tree(
            repository,
            [(b"state.json".as_slice(), EntryKind::Blob, state)],
        );
        let output_tree = write_tree(
            repository,
            [(b"build.wasm".as_slice(), EntryKind::Blob, output)],
        );
        let git_tree = write_tree(
            repository,
            [(b"config".as_slice(), EntryKind::Blob, git_metadata)],
        );
        let package_tree = write_tree(
            repository,
            [
                (b"sai.package".as_slice(), EntryKind::Blob, manifest),
                (b"src".as_slice(), EntryKind::Tree, source_tree),
                (b".sai".as_slice(), EntryKind::Tree, sai_tree),
                (b"out".as_slice(), EntryKind::Tree, output_tree),
                (b".git".as_slice(), EntryKind::Tree, git_tree),
                (b"link".as_slice(), EntryKind::Link, link_target),
                (b"dependency".as_slice(), EntryKind::Commit, submodule_id),
            ],
        );
        let root_tree = write_tree(
            repository,
            [(
                b"packages".as_slice(),
                EntryKind::Tree,
                write_tree(
                    repository,
                    [(b"acme".as_slice(), EntryKind::Tree, package_tree)],
                ),
            )],
        );
        (root_tree, write_commit(repository, root_tree))
    }

    #[test]
    fn hashes_the_package_subtree_without_checkout_or_git_config() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);
        let sentinel = repository.path.join("should-not-run");
        std::fs::write(
            repository.path.join("config"),
            format!(
                "[core]\n    useReplaceRefs = true\n[filter \"evil\"]\n    smudge = \"touch {}\"\n",
                sentinel.display()
            ),
        )
        .unwrap();

        let actual = standard_reader(&repository)
            .hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source)
            .unwrap();
        let expected = hash_git_tree(
            [
                GitTreeEntry::RegularFile {
                    path: b"sai.package",
                    content: b"package acme:sample @ 1.0.0\n",
                },
                GitTreeEntry::RegularFile {
                    path: b"src/main.sai",
                    content: b"pub fn answer() -> i32 { 42 }\n",
                },
                GitTreeEntry::Symlink { path: b"link" },
                GitTreeEntry::Gitlink {
                    path: b"dependency",
                },
                GitTreeEntry::Directory { path: b"src" },
            ],
            TreeHashPurpose::Source,
        )
        .unwrap();

        assert_eq!(actual, expected);
        assert!(!sentinel.exists());
    }

    #[test]
    fn hashes_and_reads_the_manifest_during_one_package_tree_walk() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);

        let (tree_hash, manifest) = standard_reader(&repository)
            .hash_commit_tree_and_read_manifest(&commit, b"packages/acme")
            .unwrap();

        assert_eq!(manifest, b"package acme:sample @ 1.0.0\n");
        assert!(tree_hash.checksum.starts_with("sai1:"));
        assert_eq!(tree_hash.warnings.len(), 2);
    }

    #[test]
    fn reads_selected_files_and_manifest_from_one_source_tree_snapshot() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);

        let source_tree = standard_reader(&repository)
            .read_commit_package_source_tree(&commit, b"packages/acme")
            .unwrap();

        assert_eq!(
            source_tree
                .files()
                .iter()
                .map(super::GitPackageFile::path)
                .collect::<Vec<_>>(),
            ["sai.package", "src/main.sai"]
        );
        assert_eq!(
            source_tree.files()[0].content(),
            b"package acme:sample @ 1.0.0\n"
        );
        assert_eq!(
            source_tree.files()[1].content(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
        assert_eq!(
            source_tree.package_manifest().role(),
            crate::manifest::PackageRole::Dependency
        );
        assert!(source_tree.package_manifest().is_syntax_valid());
        assert_eq!(
            source_tree.source_tree().checksum,
            standard_reader(&repository)
                .hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source)
                .unwrap()
                .checksum
        );
    }

    #[test]
    fn reads_wit_and_artifact_checksums_with_their_declared_hash_rules() {
        let repository = BareTestRepository::new();
        let manifest = write_blob(
            &repository.repository,
            b"package acme:sample @ 1.0.0\nwit \".\"\nartifact component {\n    wasm64-wasi \"dist/large.wasm\"\n    wasm32-wasi \"dist/small.wasm\"\n}\n",
        );
        let state = write_blob(&repository.repository, b"cache state\n");
        let output = write_blob(&repository.repository, b"generated output\n");
        let git_metadata = write_blob(&repository.repository, b"git metadata\n");
        let small_artifact = write_blob(&repository.repository, b"small component");
        let large_artifact = write_blob(&repository.repository, b"large component");
        let link_target = write_blob(&repository.repository, b"ignored link target\n");
        let submodule_id = write_blob(&repository.repository, b"placeholder oid\n");
        let sai_tree = write_tree(
            &repository.repository,
            [(b"state.json".as_slice(), EntryKind::Blob, state)],
        );
        let output_tree = write_tree(
            &repository.repository,
            [(b"build.wasm".as_slice(), EntryKind::Blob, output)],
        );
        let git_tree = write_tree(
            &repository.repository,
            [(b"config".as_slice(), EntryKind::Blob, git_metadata)],
        );
        let dist_tree = write_tree(
            &repository.repository,
            [
                (b"large.wasm".as_slice(), EntryKind::Blob, large_artifact),
                (b"small.wasm".as_slice(), EntryKind::Blob, small_artifact),
            ],
        );
        let package_tree = write_tree(
            &repository.repository,
            [
                (b"sai.package".as_slice(), EntryKind::Blob, manifest),
                (b".sai".as_slice(), EntryKind::Tree, sai_tree),
                (b".git".as_slice(), EntryKind::Tree, git_tree),
                (b"out".as_slice(), EntryKind::Tree, output_tree),
                (b"dist".as_slice(), EntryKind::Tree, dist_tree),
                (b"link".as_slice(), EntryKind::Link, link_target),
                (b"dependency".as_slice(), EntryKind::Commit, submodule_id),
            ],
        );
        let commit = write_commit(&repository.repository, package_tree);

        let source = standard_reader(&repository)
            .read_commit_package_source_tree(&commit, b"")
            .unwrap();

        let expected_wit = hash_git_tree(
            [
                GitTreeEntry::RegularFile {
                    path: b"sai.package",
                    content: b"package acme:sample @ 1.0.0\nwit \".\"\nartifact component {\n    wasm64-wasi \"dist/large.wasm\"\n    wasm32-wasi \"dist/small.wasm\"\n}\n",
                },
                GitTreeEntry::RegularFile {
                    path: b".sai/state.json",
                    content: b"cache state\n",
                },
                GitTreeEntry::RegularFile {
                    path: b"out/build.wasm",
                    content: b"generated output\n",
                },
                GitTreeEntry::RegularFile {
                    path: b"dist/large.wasm",
                    content: b"large component",
                },
                GitTreeEntry::RegularFile {
                    path: b"dist/small.wasm",
                    content: b"small component",
                },
                GitTreeEntry::Symlink { path: b"link" },
                GitTreeEntry::Gitlink {
                    path: b"dependency",
                },
                GitTreeEntry::Directory { path: b".sai" },
                GitTreeEntry::Directory { path: b"out" },
                GitTreeEntry::Directory { path: b"dist" },
            ],
            TreeHashPurpose::Wit,
        )
        .unwrap();
        assert_eq!(source.wit_tree(), Some(&expected_wit));
        assert_eq!(
            source
                .wit_tree()
                .unwrap()
                .warnings
                .iter()
                .map(|warning| warning.display_path())
                .collect::<Vec<_>>(),
            ["dependency", "link"]
        );
        assert_eq!(
            source
                .artifact_checksums()
                .iter()
                .map(|artifact| (artifact.target(), artifact.path(), artifact.checksum()))
                .collect::<Vec<_>>(),
            [
                (
                    "wasm32-wasi",
                    "dist/small.wasm",
                    artifact_checksum(b"small component").as_str()
                ),
                (
                    "wasm64-wasi",
                    "dist/large.wasm",
                    artifact_checksum(b"large component").as_str()
                ),
            ]
        );
        assert!(
            !source
                .files()
                .iter()
                .any(|file| file.path().starts_with(".sai/") || file.path().starts_with("out/"))
        );
        assert!(source.package_manifest().is_syntax_valid());
    }

    #[test]
    fn hashes_a_shared_artifact_file_once_and_ignores_duplicate_targets() {
        let manifest = crate::manifest::parse_for_role(
            b"package acme:sample @ 1.0.0\nartifact component {\n    wasm32-wasi \"dist/app.wasm\"\n    wasm64-wasi \"dist/app.wasm\"\n    wasm32-wasi \"dist/app.wasm\"\n}\n",
            crate::manifest::PackageRole::Dependency,
        );
        assert!(!manifest.is_syntax_valid());

        let files = [GitPackageFile {
            path: "dist/app.wasm".into(),
            content: vec![7; 4096],
        }];
        let mut hash_count = 0;
        let checksums = collect_artifact_checksums_with(&manifest, &files, |_| {
            hash_count += 1;
            "sai1:test".to_owned()
        })
        .unwrap();

        assert_eq!(hash_count, 1);
        assert_eq!(checksums.len(), 2);
        assert_eq!(checksums[0].checksum(), "sai1:test");
        assert_eq!(checksums[1].checksum(), "sai1:test");
        assert_eq!(
            checksums
                .iter()
                .map(|checksum| checksum.target())
                .collect::<Vec<_>>(),
            ["wasm32-wasi", "wasm64-wasi"]
        );
    }

    #[test]
    fn does_not_retain_artifact_checksums_for_unknown_targets() {
        let mut source = String::from("package acme:sample @ 1.0.0\nartifact component {\n");
        for index in 0..10_000 {
            use std::fmt::Write as _;
            writeln!(source, "    unknown{index} \"dist/app.wasm\"").unwrap();
        }
        source.push_str("}\n");
        let manifest = crate::manifest::parse_for_role(
            source.as_bytes(),
            crate::manifest::PackageRole::Dependency,
        );
        assert!(manifest.is_syntax_valid());

        let files = [GitPackageFile {
            path: "dist/app.wasm".into(),
            content: vec![7; 4096],
        }];
        let mut hash_count = 0;
        let checksums = collect_artifact_checksums_with(&manifest, &files, |_| {
            hash_count += 1;
            "sai1:test".to_owned()
        })
        .unwrap();

        assert!(checksums.is_empty());
        assert_eq!(hash_count, 0);
    }

    #[test]
    fn computes_wit_checksums_relative_to_the_declared_directory() {
        let repository = BareTestRepository::new();
        let manifest = write_blob(
            &repository.repository,
            b"package acme:sample @ 1.0.0\nwit \"wit\"\n",
        );
        let interface = write_blob(&repository.repository, b"package acme:sample;\n");
        let link_target = write_blob(&repository.repository, b"ignored link target\n");
        let wit_tree = write_tree(
            &repository.repository,
            [
                (b"api.wit".as_slice(), EntryKind::Blob, interface),
                (b"link".as_slice(), EntryKind::Link, link_target),
            ],
        );
        let package_tree = write_tree(
            &repository.repository,
            [
                (b"sai.package".as_slice(), EntryKind::Blob, manifest),
                (b"wit".as_slice(), EntryKind::Tree, wit_tree),
            ],
        );
        let commit = write_commit(&repository.repository, package_tree);

        let source = standard_reader(&repository)
            .read_commit_package_source_tree(&commit, b"")
            .unwrap();
        let expected = hash_git_tree(
            [
                GitTreeEntry::RegularFile {
                    path: b"api.wit",
                    content: b"package acme:sample;\n",
                },
                GitTreeEntry::Symlink { path: b"link" },
            ],
            TreeHashPurpose::Wit,
        )
        .unwrap();

        assert_eq!(source.wit_tree(), Some(&expected));
        assert_eq!(
            source.wit_tree().unwrap().warnings[0].display_path(),
            "link"
        );
    }

    #[test]
    fn reports_missing_wit_directories_and_non_regular_artifacts() {
        let repository = BareTestRepository::new();
        let manifest = write_blob(
            &repository.repository,
            b"package acme:sample @ 1.0.0\nwit \"missing\"\n",
        );
        let package_tree = write_tree(
            &repository.repository,
            [(b"sai.package".as_slice(), EntryKind::Blob, manifest)],
        );
        let commit = write_commit(&repository.repository, package_tree);

        assert_eq!(
            standard_reader(&repository)
                .read_commit_package_source_tree(&commit, b"")
                .unwrap_err(),
            GitTreeReadError::WitDirectoryNotFound {
                path: "missing".into(),
            }
        );

        let manifest = write_blob(
            &repository.repository,
            b"package acme:sample @ 1.0.0\nartifact component {\n    wasm32-wasi \"component.wasm\"\n}\n",
        );
        let link_target = write_blob(&repository.repository, b"component target\n");
        let package_tree = write_tree(
            &repository.repository,
            [
                (b"sai.package".as_slice(), EntryKind::Blob, manifest),
                (b"component.wasm".as_slice(), EntryKind::Link, link_target),
            ],
        );
        let commit = write_commit(&repository.repository, package_tree);

        assert_eq!(
            standard_reader(&repository)
                .read_commit_package_source_tree(&commit, b"")
                .unwrap_err(),
            GitTreeReadError::ArtifactNotFound {
                path: "component.wasm".into(),
            }
        );
    }

    #[test]
    fn requires_a_regular_package_manifest_file() {
        let repository = BareTestRepository::new();
        let source = write_blob(&repository.repository, b"source\n");
        let tree = write_tree(
            &repository.repository,
            [(b"lib.sai".as_slice(), EntryKind::Blob, source)],
        );
        let commit = write_commit(&repository.repository, tree);

        assert_eq!(
            standard_reader(&repository).hash_commit_tree_and_read_manifest(&commit, b""),
            Err(GitTreeReadError::MissingPackageManifest)
        );
    }

    #[test]
    fn rejects_oversized_package_manifests_before_loading_the_blob() {
        let repository = BareTestRepository::new();
        let manifest = vec![b'x'; MAX_PACKAGE_MANIFEST_BYTES + 1];
        let manifest_id = write_blob(&repository.repository, &manifest);
        let source = write_blob(&repository.repository, b"source\n");
        let tree = write_tree(
            &repository.repository,
            [
                (b"lib.sai".as_slice(), EntryKind::Blob, source),
                (b"sai.package".as_slice(), EntryKind::Blob, manifest_id),
            ],
        );
        let commit = write_commit(&repository.repository, tree);

        assert_eq!(
            standard_reader(&repository).hash_commit_tree_and_read_manifest(&commit, b""),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::ManifestBytes,
                actual: (MAX_PACKAGE_MANIFEST_BYTES + 1) as u64,
            })
        );
    }

    #[test]
    fn reads_sha256_object_databases() {
        let repository = BareTestRepository::new_sha256();
        let (_, commit) = full_test_tree(&repository.repository);
        let reader =
            GitTreeReader::open(repository.objects_directory(), gix::hash::Kind::Sha256).unwrap();

        let actual = reader
            .hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source)
            .unwrap();

        assert!(actual.checksum.starts_with("sai1:"));
        assert_eq!(actual.warnings.len(), 2);
    }

    #[test]
    fn wit_hash_keeps_source_excluded_directories() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);
        let reader = standard_reader(&repository);

        let source = reader
            .hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source)
            .unwrap();
        let wit = reader
            .hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Wit)
            .unwrap();

        assert_ne!(source.checksum, wit.checksum);
        assert_eq!(wit.warnings.len(), 2);
    }

    #[test]
    fn rejects_invalid_or_non_directory_package_paths() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.hash_commit_tree(&commit, b"packages/missing", TreeHashPurpose::Source),
            Err(GitTreeReadError::MissingPackageDirectory)
        );
        assert_eq!(
            reader.hash_commit_tree(
                &commit,
                b"packages/acme/sai.package",
                TreeHashPurpose::Source
            ),
            Err(GitTreeReadError::PackagePathNotDirectory)
        );
        assert!(matches!(
            reader.hash_commit_tree(&commit, b"packages/../acme", TreeHashPurpose::Source),
            Err(GitTreeReadError::Hash(TreeHashError::InvalidPath { .. }))
        ));
    }

    #[test]
    fn rejects_malformed_tree_names_and_duplicate_entries() {
        let repository = BareTestRepository::new();
        let blob = write_blob(&repository.repository, b"content\n");
        let duplicate_tree = write_tree(
            &repository.repository,
            [
                (b"same".as_slice(), EntryKind::Blob, blob),
                (b"same".as_slice(), EntryKind::Blob, blob),
            ],
        );
        let duplicate_commit = write_commit(&repository.repository, duplicate_tree);
        let slash_tree = write_tree(
            &repository.repository,
            [(b"folder/file".as_slice(), EntryKind::Blob, blob)],
        );
        let slash_commit = write_commit(&repository.repository, slash_tree);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.hash_commit_tree(&duplicate_commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::InvalidTreeEntry)
        );
        assert_eq!(
            reader.hash_commit_tree(&slash_commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::InvalidTreeEntry)
        );
    }

    #[test]
    fn rejects_noncanonical_regular_file_modes() {
        let repository = BareTestRepository::new();
        let blob = write_blob(&repository.repository, b"content\n");
        let tree = repository
            .repository
            .write_object(gix::objs::Tree {
                entries: vec![Entry {
                    mode: gix::objs::tree::EntryMode::try_from(0o100664).unwrap(),
                    filename: b"file".to_vec().into(),
                    oid: blob,
                }],
            })
            .unwrap()
            .detach();
        let commit = write_commit(&repository.repository, tree);

        assert_eq!(
            standard_reader(&repository).hash_commit_tree(&commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::InvalidTreeEntry)
        );
    }

    #[test]
    fn rejects_object_databases_with_alternates() {
        let repository = BareTestRepository::new();
        let alternates = repository.objects_directory().join("info/alternates");
        std::fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        std::fs::write(&alternates, "/outside/objects\n").unwrap();

        assert_eq!(
            GitTreeReader::open(repository.objects_directory(), gix::hash::Kind::Sha1)
                .err()
                .unwrap(),
            GitTreeReadError::AlternatesUnsupported
        );
    }

    #[test]
    fn does_not_resolve_alternates_added_after_open() {
        let repository = BareTestRepository::new();
        let alternate_repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&alternate_repository.repository);
        let reader = standard_reader(&repository);
        let alternates = repository.objects_directory().join("info/alternates");
        std::fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        std::fs::write(
            alternates,
            format!("{}\n", alternate_repository.objects_directory().display()),
        )
        .unwrap();

        assert_eq!(
            reader.hash_commit_tree(&commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::InvalidCommitObject)
        );
    }

    #[test]
    fn reads_objects_from_multiple_pack_indexes_without_a_multi_pack_index() {
        let repository = BareTestRepository::new();
        let blob = write_blob(&repository.repository, b"packed package data\n");
        let tree = write_tree(
            &repository.repository,
            [(b"file.sai".as_slice(), EntryKind::Blob, blob)],
        );
        let commit = write_commit(&repository.repository, tree);

        write_pack(&repository, &[commit, tree]);
        write_pack(&repository, &[blob]);
        for object_id in [commit, tree, blob] {
            std::fs::remove_file(loose_object_path(&repository, &object_id)).unwrap();
        }

        let packed_indices = std::fs::read_dir(repository.objects_directory().join("pack"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "idx"))
            .count();
        assert_eq!(packed_indices, 2);
        assert!(
            !repository
                .objects_directory()
                .join("pack/multi-pack-index")
                .exists()
        );

        let actual = standard_reader(&repository)
            .hash_commit_tree(&commit, b"", TreeHashPurpose::Source)
            .unwrap();
        let expected = hash_git_tree(
            [GitTreeEntry::RegularFile {
                path: b"file.sai",
                content: b"packed package data\n",
            }],
            TreeHashPurpose::Source,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn peels_lightweight_and_nested_annotated_tags_to_a_verified_commit() {
        let repository = BareTestRepository::new();
        let tree = write_tree(&repository.repository, []);
        let commit = write_commit(&repository.repository, tree);
        let inner_tag = write_tag(
            &repository.repository,
            commit,
            gix::objs::Kind::Commit,
            "inner",
            "inner tag",
        );
        let outer_tag = write_tag(
            &repository.repository,
            inner_tag,
            gix::objs::Kind::Tag,
            "outer",
            "outer tag",
        );
        let reader = standard_reader(&repository);

        assert_eq!(reader.peel_tag_to_commit(&commit), Ok(commit));
        assert_eq!(reader.peel_tag_to_commit(&outer_tag), Ok(commit));
    }

    #[test]
    fn rejects_tag_targets_that_are_not_commits_or_mismatch_their_declared_kind() {
        let repository = BareTestRepository::new();
        let tree = write_tree(&repository.repository, []);
        let tree_tag = write_tag(
            &repository.repository,
            tree,
            gix::objs::Kind::Tree,
            "tree-tag",
            "not a commit",
        );
        let mismatched_tag = write_tag(
            &repository.repository,
            tree,
            gix::objs::Kind::Commit,
            "mismatched-tag",
            "declared commit but targets tree",
        );
        let commit = write_commit(&repository.repository, tree);
        let mismatched_commit_tag = write_tag(
            &repository.repository,
            commit,
            gix::objs::Kind::Tree,
            "mismatched-commit-tag",
            "declared tree but targets commit",
        );
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.peel_tag_to_commit(&tree_tag),
            Err(GitTreeReadError::InvalidTagTarget {
                kind: gix::objs::Kind::Tree
            })
        );
        assert_eq!(
            reader.peel_tag_to_commit(&mismatched_tag),
            Err(GitTreeReadError::InvalidTagTargetKind {
                declared: gix::objs::Kind::Commit,
                actual: gix::objs::Kind::Tree,
            })
        );
        assert_eq!(
            reader.peel_tag_to_commit(&mismatched_commit_tag),
            Err(GitTreeReadError::InvalidTagTargetKind {
                declared: gix::objs::Kind::Tree,
                actual: gix::objs::Kind::Commit,
            })
        );
    }

    #[test]
    fn rejects_malformed_annotated_tag_objects() {
        let repository = BareTestRepository::new();
        let tag = repository
            .repository
            .objects
            .write_buf(gix::objs::Kind::Tag, b"not a valid tag object")
            .unwrap();
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.peel_tag_to_commit(&tag),
            Err(GitTreeReadError::InvalidTagObject)
        );
    }

    #[test]
    fn enforces_the_annotated_tag_chain_depth_limit() {
        let repository = BareTestRepository::new();
        let tree = write_tree(&repository.repository, []);
        let commit = write_commit(&repository.repository, tree);
        let mut tag = commit;
        let mut target_kind = gix::objs::Kind::Commit;
        for index in 0..=MAX_TAG_CHAIN_DEPTH {
            tag = write_tag(
                &repository.repository,
                tag,
                target_kind,
                &format!("tag-{index}"),
                "nested tag",
            );
            target_kind = gix::objs::Kind::Tag;
        }
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.peel_tag_to_commit(&tag),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::TagChainDepth,
                actual: MAX_TAG_CHAIN_DEPTH + 1,
            })
        );
    }

    #[test]
    fn verifies_annotated_tag_object_ids() {
        let repository = BareTestRepository::new();
        let tree = write_tree(&repository.repository, []);
        let commit = write_commit(&repository.repository, tree);
        let expected_tag = write_tag(
            &repository.repository,
            commit,
            gix::objs::Kind::Commit,
            "v1.0.0",
            "original message",
        );
        let replacement_tag = write_tag(
            &repository.repository,
            commit,
            gix::objs::Kind::Commit,
            "v1.0.0",
            "replaced message",
        );
        overwrite_loose_object(&repository, &expected_tag, &replacement_tag);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.peel_tag_to_commit(&expected_tag),
            Err(GitTreeReadError::ObjectIdMismatch)
        );
    }

    #[test]
    fn verifies_commit_object_ids() {
        let repository = BareTestRepository::new();
        let tree = write_tree(&repository.repository, []);
        let expected_commit =
            write_commit_with_message(&repository.repository, tree, "original commit");
        let replacement_commit =
            write_commit_with_message(&repository.repository, tree, "tampered commit");
        overwrite_loose_object(&repository, &expected_commit, &replacement_commit);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.hash_commit_tree(&expected_commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::ObjectIdMismatch)
        );
    }

    #[test]
    fn verifies_tree_object_ids() {
        let repository = BareTestRepository::new();
        let original_blob = write_blob(&repository.repository, b"left");
        let replacement_blob = write_blob(&repository.repository, b"rght");
        let expected_tree = write_tree(
            &repository.repository,
            [(b"file".as_slice(), EntryKind::Blob, original_blob)],
        );
        let replacement_tree = write_tree(
            &repository.repository,
            [(b"file".as_slice(), EntryKind::Blob, replacement_blob)],
        );
        let commit = write_commit(&repository.repository, expected_tree);
        overwrite_loose_object(&repository, &expected_tree, &replacement_tree);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.hash_commit_tree(&commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::ObjectIdMismatch)
        );
    }

    #[test]
    fn verifies_blob_object_ids() {
        let repository = BareTestRepository::new();
        let expected_blob = write_blob(&repository.repository, b"left");
        let replacement_blob = write_blob(&repository.repository, b"rght");
        let tree = write_tree(
            &repository.repository,
            [(b"file".as_slice(), EntryKind::Blob, expected_blob)],
        );
        let commit = write_commit(&repository.repository, tree);
        overwrite_loose_object(&repository, &expected_blob, &replacement_blob);
        let reader = standard_reader(&repository);

        assert_eq!(
            reader.hash_commit_tree(&commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::ObjectIdMismatch)
        );
    }

    #[test]
    fn applies_each_package_tree_limit() {
        let repository = BareTestRepository::new();
        let (_, commit) = full_test_tree(&repository.repository);
        let limits = Limits {
            file_count: 1,
            file_bytes: 4,
            package_bytes: 4,
            metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
            tree_entry_count: MAX_PACKAGE_TREE_ENTRIES,
            path_bytes: MAX_PACKAGE_TREE_PATH_BYTES,
        };
        let reader = GitTreeReader::open_with_limits(
            repository.objects_directory(),
            gix::hash::Kind::Sha1,
            limits,
        )
        .unwrap();

        assert!(matches!(
            reader.hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::FileBytes,
                actual,
            }) if actual > 4
        ));

        let limits = Limits {
            file_count: 1,
            file_bytes: MAX_PACKAGE_FILE_BYTES,
            package_bytes: MAX_PACKAGE_CONTENT_BYTES,
            metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
            tree_entry_count: MAX_PACKAGE_TREE_ENTRIES,
            path_bytes: MAX_PACKAGE_TREE_PATH_BYTES,
        };
        let reader = GitTreeReader::open_with_limits(
            repository.objects_directory(),
            gix::hash::Kind::Sha1,
            limits,
        )
        .unwrap();
        assert!(matches!(
            reader.hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::FileCount,
                actual: 2,
            })
        ));

        let limits = Limits {
            file_count: 1,
            file_bytes: MAX_PACKAGE_FILE_BYTES,
            package_bytes: MAX_PACKAGE_CONTENT_BYTES,
            metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
            tree_entry_count: 5,
            path_bytes: MAX_PACKAGE_TREE_PATH_BYTES,
        };
        let reader = GitTreeReader::open_with_limits(
            repository.objects_directory(),
            gix::hash::Kind::Sha1,
            limits,
        )
        .unwrap();
        assert!(matches!(
            reader.hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::TreeEntryCount,
                ..
            })
        ));

        let limits = Limits {
            file_count: MAX_PACKAGE_FILE_COUNT,
            file_bytes: MAX_PACKAGE_FILE_BYTES,
            package_bytes: MAX_PACKAGE_CONTENT_BYTES,
            metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
            tree_entry_count: MAX_PACKAGE_TREE_ENTRIES,
            path_bytes: 4,
        };
        let reader = GitTreeReader::open_with_limits(
            repository.objects_directory(),
            gix::hash::Kind::Sha1,
            limits,
        )
        .unwrap();
        assert!(matches!(
            reader.hash_commit_tree(&commit, b"packages/acme", TreeHashPurpose::Source),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::PathBytes,
                actual,
            }) if actual > 4
        ));
    }

    #[test]
    fn stops_decoding_when_tree_entry_budget_is_exhausted() {
        let repository = BareTestRepository::new();
        let blob = write_blob(&repository.repository, b"x");
        let entries = (0..100)
            .map(|index| Entry {
                mode: EntryKind::Blob.into(),
                filename: format!("file-{index:03}").into_bytes().into(),
                oid: blob,
            })
            .collect();
        let tree = repository
            .repository
            .write_object(gix::objs::Tree { entries })
            .unwrap()
            .detach();
        let commit = write_commit(&repository.repository, tree);
        let limits = Limits {
            file_count: MAX_PACKAGE_FILE_COUNT,
            file_bytes: MAX_PACKAGE_FILE_BYTES,
            package_bytes: MAX_PACKAGE_CONTENT_BYTES,
            metadata_object_bytes: MAX_GIT_METADATA_OBJECT_BYTES,
            tree_entry_count: 3,
            path_bytes: MAX_PACKAGE_TREE_PATH_BYTES,
        };
        let reader = GitTreeReader::open_with_limits(
            repository.objects_directory(),
            gix::hash::Kind::Sha1,
            limits,
        )
        .unwrap();

        assert!(matches!(
            reader.hash_commit_tree(&commit, b"", TreeHashPurpose::Source),
            Err(GitTreeReadError::PackageTooLarge {
                limit: GitTreeLimit::TreeEntryCount,
                actual: 4,
            })
        ));
    }

    #[test]
    fn uses_fixed_limits_documented_for_package_sources() {
        assert_eq!(MAX_PACKAGE_FILE_COUNT, 100_000);
        assert_eq!(MAX_PACKAGE_FILE_BYTES, 256 * 1024 * 1024);
        assert_eq!(MAX_PACKAGE_CONTENT_BYTES, 1024 * 1024 * 1024);
        assert_eq!(MAX_GIT_METADATA_OBJECT_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_TAG_CHAIN_DEPTH, 64);
        assert_eq!(MAX_PACKAGE_TREE_ENTRIES, 1_000_000);
        assert_eq!(MAX_PACKAGE_TREE_PATH_BYTES, 64 * 1024 * 1024);
    }
}
