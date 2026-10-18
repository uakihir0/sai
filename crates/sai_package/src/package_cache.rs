//! Content-addressed storage for verified package source trees.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use cap_std::ambient_authority;
use cap_std::fs::{Dir as CapabilityDir, Permissions as CapabilityPermissions};
use same_file::Handle as SameFileHandle;
use sha2::{Digest, Sha256};

use crate::git_tree::{
    GitPackageFile, GitPackageSourceTree, MAX_PACKAGE_CONTENT_BYTES, MAX_PACKAGE_FILE_BYTES,
    MAX_PACKAGE_FILE_COUNT, MAX_PACKAGE_TREE_ENTRIES, MAX_PACKAGE_TREE_PATH_BYTES,
};
use crate::lock_validation::canonical_git_url;
use crate::manifest::MAX_PACKAGE_MANIFEST_BYTES;
use crate::package_path::RepositoryPackageDirectory;
use crate::tree_hash::{reject_tree_path_collisions, should_skip_path, validate_tree_path};

const CACHE_LAYOUT_VERSION: &str = "v1";
const SOURCE_CHECKSUM_PREFIX: &str = "sai1:";
const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_CACHE_RECORD_BYTES: u64 = 16 * 1024;
const TAG_RECORD_MAGIC: &[u8] = b"SAI-CACHE-TAG-v1\0";
const SOURCE_RECORD_MAGIC: &[u8] = b"SAI-CACHE-SOURCE-v1\0";
const PACKAGE_CHECKSUM_RECORD_MAGIC: &[u8] = b"SAI-CACHE-PACKAGE-CHECKSUMS-v1\0";
const RECORD_DIGEST_BYTES: usize = 32;
#[cfg(windows)]
const WINDOWS_ERROR_LOCK_VIOLATION: i32 = 33;
#[cfg(windows)]
const WINDOWS_ERROR_SHARING_VIOLATION: i32 = 32;
#[cfg(windows)]
const CACHE_ENTRY_REMOVE_RETRY_LIMIT: usize = 500;
#[cfg(windows)]
const WINDOWS_FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// A failure while locating or writing the global package cache.
#[derive(Debug)]
pub enum PackageCacheError {
    /// `SAI_CACHE_DIR` was set to a relative path.
    RelativeCacheDirectory(PathBuf),
    /// No platform cache directory could be determined.
    CacheDirectoryUnavailable,
    /// A source checksum was not in the canonical `sai1:<sha256>` form.
    InvalidSourceChecksum(Box<str>),
    /// A Git URL was not in canonical form.
    InvalidGitUrl,
    /// A Git tag name was not a bounded full `refs/tags/` name.
    InvalidGitTag,
    /// A Git commit was not a canonical SHA-1 or SHA-256 object ID.
    InvalidGitCommit,
    /// A package directory was not a normalized repository-relative path.
    InvalidPackageDirectory,
    /// A cache record was malformed, mismatched, or too large.
    CorruptRecord(PathBuf),
    /// A package checksum record would exceed the cache record limit.
    PackageChecksumRecordTooLarge,
    /// A package checksum set is malformed or contains duplicate targets.
    InvalidPackageChecksums,
    /// A commit and package directory already have a different recorded checksum.
    ConflictingGitSourceRecord(Box<str>),
    /// A supposedly verified source file had an unsafe relative path.
    InvalidSourcePath(Box<str>),
    /// A cache filesystem operation failed.
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Original operating-system error.
        source: io::Error,
    },
}

impl fmt::Display for PackageCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeCacheDirectory(path) => write!(
                formatter,
                "SAI_CACHE_DIR must be absolute, got {}",
                path.display()
            ),
            Self::CacheDirectoryUnavailable => {
                formatter.write_str("could not determine a package cache directory")
            }
            Self::InvalidSourceChecksum(checksum) => {
                write!(formatter, "invalid package source checksum `{checksum}`")
            }
            Self::InvalidGitUrl => formatter.write_str("invalid canonical Git URL"),
            Self::InvalidGitTag => formatter.write_str("invalid Git release tag"),
            Self::InvalidGitCommit => formatter.write_str("invalid Git commit object ID"),
            Self::InvalidPackageDirectory => formatter.write_str("invalid Git package directory"),
            Self::CorruptRecord(path) => {
                write!(
                    formatter,
                    "package cache record is corrupt at `{}`",
                    path.display()
                )
            }
            Self::PackageChecksumRecordTooLarge => {
                formatter.write_str("package checksum record exceeds the cache record limit")
            }
            Self::InvalidPackageChecksums => {
                formatter.write_str("invalid or duplicate package checksums")
            }
            Self::ConflictingGitSourceRecord(checksum) => write!(
                formatter,
                "Git commit and package directory already have a different cached checksum than `{checksum}`"
            ),
            Self::InvalidSourcePath(path) => {
                write!(formatter, "invalid verified package source path `{path}`")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} `{}`: {source}",
                path.display()
            ),
        }
    }
}

impl Error for PackageCacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The global, versioned package-cache directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCache {
    root: PathBuf,
    versioned_root: PathBuf,
    git_directory: PathBuf,
    source_directory: PathBuf,
    tag_directory: PathBuf,
    temporary_directory: PathBuf,
    lock_path: PathBuf,
}

impl PackageCache {
    /// Open or create the cache at an absolute path.
    ///
    /// The path is explicit so callers can resolve environment variables at
    /// the CLI boundary and tests can use isolated temporary directories.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PackageCacheError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(PackageCacheError::RelativeCacheDirectory(
                root.to_path_buf(),
            ));
        }

        let versioned_root = root.join(CACHE_LAYOUT_VERSION);
        let git_directory = versioned_root.join("git");
        let source_directory = versioned_root.join("src");
        let tag_directory = versioned_root.join("tags");
        let temporary_directory = versioned_root.join("tmp");
        fs::create_dir_all(root).map_err(|source| PackageCacheError::Io {
            operation: "create package cache root",
            path: root.to_path_buf(),
            source,
        })?;
        ensure_cache_directory(&versioned_root)?;
        for directory in [
            git_directory.clone(),
            source_directory.clone(),
            tag_directory.clone(),
            temporary_directory.clone(),
        ] {
            ensure_cache_directory(&directory)?;
        }

        let lock_path = versioned_root.join("lock");
        ensure_lock_file(&lock_path)?;
        set_private_file_permissions(&lock_path)?;

        Ok(Self {
            root: root.to_path_buf(),
            versioned_root,
            git_directory,
            source_directory,
            tag_directory,
            temporary_directory,
            lock_path,
        })
    }

    /// The configured cache root, before the `v1/` layout directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the normative SHA-256 key for a canonical Git URL.
    pub fn repository_key(canonical_url: &str) -> Result<String, PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        Ok(lowercase_hex(&Sha256::digest(canonical_url.as_bytes())))
    }

    /// Return the cache location for a canonical Git repository.
    ///
    /// The directory is not created by this method.
    pub fn git_repository_directory(
        &self,
        canonical_url: &str,
    ) -> Result<PathBuf, PackageCacheError> {
        Ok(self
            .git_directory
            .join(Self::repository_key(canonical_url)?))
    }

    /// Record the commit reached by a successfully fetched release tag.
    ///
    /// Callers must perform the lock comparison and any required review before
    /// replacing a previously recorded tag with a different commit.
    pub fn record_git_tag(
        &self,
        canonical_url: &str,
        tag_name: &str,
        commit: &str,
    ) -> Result<(), PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_tag_name(tag_name)?;
        validate_git_commit(commit)?;

        let key = cache_record_key(b"tag", &[canonical_url.as_bytes(), tag_name.as_bytes()]);
        let target = self.tag_directory.join(format!("tag-{key}.record"));
        self.replace_cache_record(
            &target,
            TAG_RECORD_MAGIC,
            &[
                canonical_url.as_bytes(),
                tag_name.as_bytes(),
                commit.as_bytes(),
            ],
        )
    }

    /// Read the cached commit for a canonical URL and full release-tag name.
    pub fn cached_git_tag_commit(
        &self,
        canonical_url: &str,
        tag_name: &str,
    ) -> Result<Option<String>, PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_tag_name(tag_name)?;

        let key = cache_record_key(b"tag", &[canonical_url.as_bytes(), tag_name.as_bytes()]);
        let target = self.tag_directory.join(format!("tag-{key}.record"));
        let Some(record) = read_latest_cache_record(&target, TAG_RECORD_MAGIC, 3)? else {
            return Ok(None);
        };
        let fields = decode_cache_record(TAG_RECORD_MAGIC, &record.bytes, 3)
            .map(|(_, fields)| fields)
            .ok_or_else(|| PackageCacheError::CorruptRecord(target.clone()))?;
        if fields[0] != canonical_url.as_bytes() || fields[1] != tag_name.as_bytes() {
            return Err(PackageCacheError::CorruptRecord(target));
        }
        let commit = std::str::from_utf8(fields[2])
            .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
        validate_git_commit(commit).map_err(|_| PackageCacheError::CorruptRecord(target))?;
        Ok(Some(commit.to_owned()))
    }

    /// Record a verified checksum for one repository commit and package directory.
    pub fn record_git_source_checksum(
        &self,
        canonical_url: &str,
        commit: &str,
        directory: Option<&str>,
        checksum: &str,
    ) -> Result<(), PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_commit(commit)?;
        let directory = canonical_package_directory(directory)?;
        source_digest(checksum)?;

        let key = cache_record_key(
            b"source",
            &[
                canonical_url.as_bytes(),
                commit.as_bytes(),
                directory.as_bytes(),
            ],
        );
        let target = self.tag_directory.join(format!("source-{key}.record"));
        self.replace_cache_record(
            &target,
            SOURCE_RECORD_MAGIC,
            &[
                canonical_url.as_bytes(),
                commit.as_bytes(),
                directory.as_bytes(),
                checksum.as_bytes(),
            ],
        )
    }

    /// Read a previously recorded source checksum for one repository commit.
    pub fn cached_git_source_checksum(
        &self,
        canonical_url: &str,
        commit: &str,
        directory: Option<&str>,
    ) -> Result<Option<String>, PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_commit(commit)?;
        let directory = canonical_package_directory(directory)?;

        let key = cache_record_key(
            b"source",
            &[
                canonical_url.as_bytes(),
                commit.as_bytes(),
                directory.as_bytes(),
            ],
        );
        let target = self.tag_directory.join(format!("source-{key}.record"));
        let Some(record) = read_latest_cache_record(&target, SOURCE_RECORD_MAGIC, 4)? else {
            return Ok(None);
        };
        let fields = decode_cache_record(SOURCE_RECORD_MAGIC, &record.bytes, 4)
            .map(|(_, fields)| fields)
            .ok_or_else(|| PackageCacheError::CorruptRecord(target.clone()))?;
        if fields[0] != canonical_url.as_bytes()
            || fields[1] != commit.as_bytes()
            || fields[2] != directory.as_bytes()
        {
            return Err(PackageCacheError::CorruptRecord(target));
        }
        let checksum = std::str::from_utf8(fields[3])
            .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
        source_digest(checksum).map_err(|_| PackageCacheError::CorruptRecord(target))?;
        Ok(Some(checksum.to_owned()))
    }

    /// Read the checksum set computed from one exact Git package snapshot.
    ///
    /// The repository, commit, and package directory are part of the record
    /// key. This matters when `wit` is `"."`: its checksum can differ across
    /// commits whose source checksums are equal because `.sai` and `out` are
    /// excluded only from the source checksum.
    pub(crate) fn cached_package_checksums(
        &self,
        canonical_url: &str,
        commit: &str,
        directory: Option<&str>,
    ) -> Result<Option<CachedPackageChecksums>, PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_commit(commit)?;
        let directory = canonical_package_directory(directory)?;
        let key = package_checksum_record_key(canonical_url, commit, directory);
        let target = self
            .tag_directory
            .join(format!("package-checksums-{key}.record"));
        let Some(record) = read_latest_cache_record(&target, PACKAGE_CHECKSUM_RECORD_MAGIC, 6)?
        else {
            return Ok(None);
        };
        let decoded = decode_cache_record(PACKAGE_CHECKSUM_RECORD_MAGIC, &record.bytes, 6)
            .map(|(_, fields)| fields)
            .ok_or_else(|| PackageCacheError::CorruptRecord(target.clone()))?;
        if decoded[0] != canonical_url.as_bytes()
            || decoded[1] != commit.as_bytes()
            || decoded[2] != directory.as_bytes()
        {
            return Err(PackageCacheError::CorruptRecord(target));
        }
        let source_checksum = std::str::from_utf8(decoded[3])
            .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
        source_digest(source_checksum)
            .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
        let wit_checksum = if decoded[4].is_empty() {
            None
        } else {
            let checksum = std::str::from_utf8(decoded[4])
                .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
            source_digest(checksum)
                .map_err(|_| PackageCacheError::CorruptRecord(target.clone()))?;
            Some(checksum.into())
        };
        let artifact_checksums = decode_artifact_checksum_list(decoded[5])
            .ok_or_else(|| PackageCacheError::CorruptRecord(target.clone()))?;
        Ok(Some(CachedPackageChecksums {
            source_checksum: source_checksum.into(),
            wit_checksum,
            artifact_checksums,
        }))
    }

    pub(crate) fn record_verified_package_checksums(
        &self,
        canonical_url: &str,
        commit: &str,
        directory: Option<&str>,
        source: &GitPackageSourceTree,
    ) -> Result<(), PackageCacheError> {
        let artifacts = source
            .artifact_checksums()
            .iter()
            .map(|artifact| (artifact.target(), artifact.checksum()))
            .collect::<Vec<_>>();
        let metadata = PackageChecksumMetadata::new(
            canonical_url,
            commit,
            directory,
            &source.source_tree().checksum,
            source.wit_tree().map(|tree| tree.checksum.as_str()),
            &artifacts,
        )?;
        let key = package_checksum_record_key(
            &metadata.canonical_url,
            &metadata.commit,
            &metadata.directory,
        );
        let target = self
            .tag_directory
            .join(format!("package-checksums-{key}.record"));
        let fields = metadata.record_fields();
        let encoded = encode_cache_record(PACKAGE_CHECKSUM_RECORD_MAGIC, 1, &fields);
        if encoded.len() as u64 > MAX_CACHE_RECORD_BYTES {
            return Err(PackageCacheError::PackageChecksumRecordTooLarge);
        }
        self.replace_cache_record_recovering_corruption(
            &target,
            PACKAGE_CHECKSUM_RECORD_MAGIC,
            &fields,
        )
    }

    fn replace_cache_record(
        &self,
        target: &Path,
        magic: &[u8],
        fields: &[&[u8]],
    ) -> Result<(), PackageCacheError> {
        self.replace_cache_record_inner(target, magic, fields, false, false)
    }

    fn replace_cache_record_recovering_corruption(
        &self,
        target: &Path,
        magic: &[u8],
        fields: &[&[u8]],
    ) -> Result<(), PackageCacheError> {
        self.replace_cache_record_inner(target, magic, fields, true, true)
    }

    fn replace_cache_record_inner(
        &self,
        target: &Path,
        magic: &[u8],
        fields: &[&[u8]],
        repair_corruption: bool,
        reject_updates: bool,
    ) -> Result<(), PackageCacheError> {
        self.cleanup_stale_temporary_directories();
        let _lock = CachePublicationLock::acquire(&self.lock_path)?;
        let existing = match read_latest_cache_record(target, magic, fields.len()) {
            Ok(existing) => existing,
            Err(PackageCacheError::CorruptRecord(_)) if repair_corruption => {
                for slot in 0..2 {
                    let path = cache_record_slot_path(target, slot);
                    if path_exists_without_following(&path)?
                        && !remove_file_at_path(&path, &self.root)
                    {
                        return Err(PackageCacheError::Io {
                            operation: "remove corrupt package checksum record",
                            path,
                            source: io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "corrupt cache record could not be removed through its verified parent",
                            ),
                        });
                    }
                }
                None
            }
            Err(error) => return Err(error),
        };
        let next_generation = if let Some(existing) = &existing {
            let (_, old_fields) = decode_cache_record(magic, &existing.bytes, fields.len())
                .ok_or_else(|| PackageCacheError::CorruptRecord(target.to_path_buf()))?;
            let identity_field_count = fields.len().saturating_sub(1);
            if old_fields[..identity_field_count] != fields[..identity_field_count] {
                return Err(PackageCacheError::CorruptRecord(target.to_path_buf()));
            }
            if old_fields == fields {
                return Ok(());
            }
            if reject_updates {
                return Err(PackageCacheError::CorruptRecord(target.to_path_buf()));
            }
            if magic == SOURCE_RECORD_MAGIC {
                let new_checksum = std::str::from_utf8(fields[3])
                    .map_err(|_| PackageCacheError::CorruptRecord(target.to_path_buf()))?;
                let existing_checksum = std::str::from_utf8(old_fields[3])
                    .map_err(|_| PackageCacheError::CorruptRecord(target.to_path_buf()))?;
                if source_digest(existing_checksum).is_ok() && existing_checksum != new_checksum {
                    return Err(PackageCacheError::ConflictingGitSourceRecord(
                        new_checksum.into(),
                    ));
                }
            }
            existing
                .generation
                .checked_add(1)
                .ok_or_else(|| PackageCacheError::CorruptRecord(target.to_path_buf()))?
        } else {
            1
        };
        let destination_slot = existing.map_or(0, |record| 1 - record.slot);
        let destination = cache_record_slot_path(target, destination_slot);
        let bytes = encode_cache_record(magic, next_generation, fields);

        let staging = StagingDirectory::create(&self.temporary_directory, &self.root)?;
        let staged_record = staging.path().join("record");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_new_file_mode(&mut options);
        let mut file = options
            .open(&staged_record)
            .map_err(|source| PackageCacheError::Io {
                operation: "create package cache record",
                path: staged_record.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| PackageCacheError::Io {
                operation: "write package cache record",
                path: staged_record.clone(),
                source,
            })?;
        drop(file);
        staging.refresh_activity_age()?;

        if path_exists_without_following(&destination)? {
            #[cfg(windows)]
            if !remove_file_at_path(&destination, &self.root) {
                return Err(PackageCacheError::Io {
                    operation: "replace inactive package cache record slot",
                    path: destination.clone(),
                    source: io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "cache record could not be removed through its verified parent",
                    ),
                });
            }
        }
        fs::rename(&staged_record, &destination).map_err(|source| PackageCacheError::Io {
            operation: "publish package cache record",
            path: destination,
            source,
        })
    }

    /// Install a verified source snapshot and return its immutable cache path.
    ///
    /// Files are written below `tmp/` first. The complete directory is made
    /// read-only before an atomic rename publishes it below `src/`.
    pub fn materialize_source(
        &self,
        source: GitPackageSourceTree,
    ) -> Result<CachedPackageSource, PackageCacheError> {
        let (tree_hash, _wit_tree, _artifact_checksums, manifest, files) = source.into_parts();
        drop(manifest);

        let checksum = tree_hash.checksum.into_boxed_str();
        let digest = source_digest(&checksum)?;
        let target = self.source_directory.join(digest);
        self.cleanup_stale_temporary_directories();
        if path_exists_without_following(&target)? && cached_source_matches(&target, &checksum)? {
            return Ok(CachedPackageSource {
                checksum,
                path: target,
                disposition: CacheDisposition::AlreadyPresent,
            });
        }

        let mut staging = StagingDirectory::create(&self.temporary_directory, &self.root)?;
        for source_file in &files {
            write_source_file(staging.path(), source_file)?;
        }
        staging.refresh_activity_age()?;
        make_tree_read_only(staging.path())?;
        prepare_source_directory_rename(staging.path())?;

        let _lock = CachePublicationLock::acquire(&self.lock_path)?;
        if path_exists_without_following(&target)? && cached_source_matches(&target, &checksum)? {
            return Ok(CachedPackageSource {
                checksum,
                path: target,
                disposition: CacheDisposition::AlreadyPresent,
            });
        }

        if path_exists_without_following(&target)? {
            remove_cache_entry(&target)?;
        }

        if let Err(source) = fs::rename(staging.path(), &target) {
            if path_exists_without_following(&target)? && cached_source_matches(&target, &checksum)?
            {
                return Ok(CachedPackageSource {
                    checksum,
                    path: target,
                    disposition: CacheDisposition::AlreadyPresent,
                });
            }
            return Err(PackageCacheError::Io {
                operation: "publish verified package source",
                path: target,
                source,
            });
        }
        if let Err(error) = seal_published_source_directory(&target) {
            let _ = remove_tree_at_path(&target, &self.root);
            return Err(error);
        }
        staging.mark_published();

        Ok(CachedPackageSource {
            checksum,
            path: target,
            disposition: CacheDisposition::Installed,
        })
    }

    /// Re-hash and validate one content-addressed source directory.
    ///
    /// This is intended for `sai fetch`, which verifies every locked source.
    /// Other commands should use their locked cache path without re-hashing.
    /// A corrupt result includes the computed checksum when the tree could be
    /// hashed; `None` means its structure or paths prevented hashing.
    pub fn verify_source(
        &self,
        checksum: &str,
    ) -> Result<CachedSourceVerification, PackageCacheError> {
        let digest = source_digest(checksum)?;
        self.ensure_source_cache_directories()?;
        let target = self.source_directory.join(digest);
        match inspect_cached_source(&target)? {
            SourceTreeInspection::Missing => Ok(CachedSourceVerification::Missing),
            SourceTreeInspection::Corrupt { actual_checksum } => {
                Ok(CachedSourceVerification::Corrupt {
                    expected_checksum: checksum.into(),
                    actual_checksum: actual_checksum.map(Into::into),
                })
            }
            SourceTreeInspection::Hashed {
                actual_checksum,
                immutable,
            } if immutable && actual_checksum == checksum => {
                Ok(CachedSourceVerification::Verified(CachedPackageSource {
                    checksum: checksum.into(),
                    path: target,
                    disposition: CacheDisposition::AlreadyPresent,
                }))
            }
            SourceTreeInspection::Hashed {
                actual_checksum, ..
            } => Ok(CachedSourceVerification::Corrupt {
                expected_checksum: checksum.into(),
                actual_checksum: Some(actual_checksum.into()),
            }),
        }
    }

    /// Remove a source entry only when a fresh locked scan confirms corruption.
    ///
    /// The global cache lock is held for the scan and removal so a concurrent
    /// materializer cannot publish a valid replacement that gets removed.
    pub fn remove_corrupt_source(
        &self,
        checksum: &str,
    ) -> Result<SourceCacheRemoval, PackageCacheError> {
        let digest = source_digest(checksum)?;
        self.ensure_source_cache_directories()?;
        let _lock = CachePublicationLock::acquire(&self.lock_path)?;
        let target = self.source_directory.join(digest);
        match inspect_cached_source(&target)? {
            SourceTreeInspection::Missing => Ok(SourceCacheRemoval::Missing),
            SourceTreeInspection::Hashed {
                actual_checksum,
                immutable: true,
            } if actual_checksum == checksum => Ok(SourceCacheRemoval::Valid),
            SourceTreeInspection::Corrupt { .. } | SourceTreeInspection::Hashed { .. } => {
                remove_cache_entry(&target)?;
                Ok(SourceCacheRemoval::Removed)
            }
        }
    }

    fn ensure_source_cache_directories(&self) -> Result<(), PackageCacheError> {
        for path in [&self.versioned_root, &self.source_directory] {
            let metadata = fs::symlink_metadata(path).map_err(|source| PackageCacheError::Io {
                operation: "inspect package source cache directory",
                path: path.to_path_buf(),
                source,
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(PackageCacheError::Io {
                    operation: "validate package source cache directory",
                    path: path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cache layout entry is not a directory",
                    ),
                });
            }
        }
        Ok(())
    }

    /// Return the content-addressed path for a canonical source checksum.
    #[cfg(test)]
    fn source_path(&self, checksum: &str) -> Result<PathBuf, PackageCacheError> {
        Ok(self.source_directory.join(source_digest(checksum)?))
    }

    /// The `v1/` directory containing Git objects, source trees, tags, and lock.
    pub fn versioned_root(&self) -> &Path {
        &self.versioned_root
    }

    /// The `v1/tmp/` directory used for in-progress writes.
    pub fn temporary_directory(&self) -> &Path {
        &self.temporary_directory
    }

    pub(crate) fn cleanup_stale_temporary_directories(&self) {
        if !staging_activity_lock_supported() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.temporary_directory) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };

            if metadata.is_dir() && is_cache_temporary_directory_name(name) {
                if !is_stale_temporary_path(&path, now) {
                    continue;
                }
                let activity_path = staging_activity_lock_path(&path);
                let (activity_lock, marker_is_new_claim) =
                    match try_open_staging_activity_lock(&activity_path) {
                        Ok(Some(lock)) if is_stale_temporary_path(&activity_path, now) => {
                            (lock, false)
                        }
                        Ok(None)
                            if matches!(
                                path_exists_without_following(&activity_path),
                                Ok(false)
                            ) && legacy_staging_owner_is_gone(name) =>
                        {
                            // Claim the marker name atomically before removing a
                            // legacy directory whose old writer is no longer
                            // running. This coordinates cleaners and new writers.
                            match create_staging_activity_lock(&activity_path, &self.root) {
                                Ok(lock) => (lock, true),
                                Err(PackageCacheError::Io { source, .. })
                                    if source.kind() == io::ErrorKind::AlreadyExists =>
                                {
                                    continue;
                                }
                                Err(_) => continue,
                            }
                        }
                        _ => continue,
                    };
                if !is_stale_temporary_path(&path, now)
                    || (!marker_is_new_claim && !is_stale_temporary_path(&activity_path, now))
                {
                    continue;
                }
                let removed = remove_tree_at_path(&path, &self.root);
                release_staging_activity_lock(&activity_path, activity_lock, removed, &self.root);
                continue;
            }

            let Some(staging_name) = name.strip_suffix(".lock") else {
                continue;
            };
            if !metadata.is_file()
                || !is_cache_temporary_directory_name(staging_name)
                || !is_stale_temporary_path(&path, now)
            {
                continue;
            }
            let staging_path = self.temporary_directory.join(staging_name);
            if !matches!(path_exists_without_following(&staging_path), Ok(false)) {
                continue;
            }
            let Ok(Some(activity_lock)) = try_open_staging_activity_lock(&path) else {
                continue;
            };
            let orphaned = matches!(path_exists_without_following(&staging_path), Ok(false))
                && is_stale_temporary_path(&path, now);
            release_staging_activity_lock(&path, activity_lock, orphaned, &self.root);
        }
    }
}

struct PackageChecksumMetadata {
    canonical_url: Box<str>,
    commit: Box<str>,
    directory: Box<str>,
    source_checksum: Box<str>,
    wit_checksum: Option<Box<str>>,
    encoded_artifacts: Vec<u8>,
}

pub(crate) struct CachedPackageChecksums {
    pub(crate) source_checksum: Box<str>,
    pub(crate) wit_checksum: Option<Box<str>>,
    pub(crate) artifact_checksums: Vec<(Box<str>, Box<str>)>,
}

impl PackageChecksumMetadata {
    fn new(
        canonical_url: &str,
        commit: &str,
        directory: Option<&str>,
        source_checksum: &str,
        wit_checksum: Option<&str>,
        artifact_checksums: &[(&str, &str)],
    ) -> Result<Self, PackageCacheError> {
        validate_canonical_git_url(canonical_url)?;
        validate_git_commit(commit)?;
        let directory = canonical_package_directory(directory)?;
        source_digest(source_checksum)?;
        if let Some(checksum) = wit_checksum {
            source_digest(checksum)?;
        }

        let mut artifacts = artifact_checksums
            .iter()
            .map(|(target, checksum)| ((*target).into(), (*checksum).into()))
            .collect::<Vec<(Box<str>, Box<str>)>>();
        artifacts.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        for (index, (target, checksum)) in artifacts.iter().enumerate() {
            if !crate::validation::is_component_target(target)
                || source_digest(checksum).is_err()
                || (index > 0 && artifacts[index - 1].0 == *target)
            {
                return Err(PackageCacheError::InvalidPackageChecksums);
            }
        }
        Ok(Self {
            canonical_url: canonical_url.into(),
            commit: commit.into(),
            directory: directory.into(),
            source_checksum: source_checksum.into(),
            wit_checksum: wit_checksum.map(Into::into),
            encoded_artifacts: encode_artifact_checksum_list(&artifacts)?,
        })
    }

    fn record_fields(&self) -> Vec<&[u8]> {
        let wit_checksum = self.wit_checksum.as_deref().unwrap_or("");
        vec![
            self.canonical_url.as_bytes(),
            self.commit.as_bytes(),
            self.directory.as_bytes(),
            self.source_checksum.as_bytes(),
            wit_checksum.as_bytes(),
            self.encoded_artifacts.as_slice(),
        ]
    }
}

fn package_checksum_record_key(canonical_url: &str, commit: &str, directory: &str) -> String {
    cache_record_key(
        b"package-checksums",
        &[
            canonical_url.as_bytes(),
            commit.as_bytes(),
            directory.as_bytes(),
        ],
    )
}

fn encode_artifact_checksum_list(
    artifacts: &[(Box<str>, Box<str>)],
) -> Result<Vec<u8>, PackageCacheError> {
    let count = u32::try_from(artifacts.len())
        .map_err(|_| PackageCacheError::PackageChecksumRecordTooLarge)?;
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&count.to_be_bytes());
    for (target, checksum) in artifacts {
        let target_length =
            u32::try_from(target.len()).map_err(|_| PackageCacheError::InvalidPackageChecksums)?;
        let checksum_length = u32::try_from(checksum.len())
            .map_err(|_| PackageCacheError::InvalidPackageChecksums)?;
        encoded.extend_from_slice(&target_length.to_be_bytes());
        encoded.extend_from_slice(target.as_bytes());
        encoded.extend_from_slice(&checksum_length.to_be_bytes());
        encoded.extend_from_slice(checksum.as_bytes());
        if encoded.len() as u64 > MAX_CACHE_RECORD_BYTES {
            return Err(PackageCacheError::PackageChecksumRecordTooLarge);
        }
    }
    Ok(encoded)
}

fn decode_artifact_checksum_list(encoded: &[u8]) -> Option<Vec<(Box<str>, Box<str>)>> {
    let count = u32::from_be_bytes(encoded.get(..4)?.try_into().ok()?) as usize;
    if count > encoded.len().saturating_sub(4) / 8 {
        return None;
    }

    let mut offset = 4usize;
    let mut artifacts = Vec::with_capacity(count);
    for _ in 0..count {
        let target_length_end = offset.checked_add(4)?;
        let target_length =
            u32::from_be_bytes(encoded.get(offset..target_length_end)?.try_into().ok()?) as usize;
        offset = target_length_end;
        let target_end = offset.checked_add(target_length)?;
        let target = std::str::from_utf8(encoded.get(offset..target_end)?).ok()?;
        offset = target_end;

        let checksum_length_end = offset.checked_add(4)?;
        let checksum_length =
            u32::from_be_bytes(encoded.get(offset..checksum_length_end)?.try_into().ok()?) as usize;
        offset = checksum_length_end;
        let checksum_end = offset.checked_add(checksum_length)?;
        let checksum = std::str::from_utf8(encoded.get(offset..checksum_end)?).ok()?;
        offset = checksum_end;

        if !crate::validation::is_component_target(target)
            || source_digest(checksum).is_err()
            || artifacts
                .last()
                .is_some_and(|(previous, _): &(Box<str>, Box<str>)| {
                    previous.as_bytes() >= target.as_bytes()
                })
        {
            return None;
        }
        artifacts.push((target.into(), checksum.into()));
    }
    (offset == encoded.len()).then_some(artifacts)
}

/// Result of installing or finding a content-addressed source tree.
#[derive(Debug, Eq, PartialEq)]
pub struct CachedPackageSource {
    checksum: Box<str>,
    path: PathBuf,
    disposition: CacheDisposition,
}

impl CachedPackageSource {
    /// The verified `sai1:` source checksum represented by this entry.
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    /// The directory containing exactly the selected source files.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this call installed a new entry or found an existing one.
    pub const fn disposition(&self) -> CacheDisposition {
        self.disposition
    }
}

/// Whether a content-addressed source entry was newly installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheDisposition {
    /// This call atomically published the directory.
    Installed,
    /// A directory with this content address already existed.
    AlreadyPresent,
}

/// Result of re-hashing one source cache entry.
#[derive(Debug, Eq, PartialEq)]
pub enum CachedSourceVerification {
    /// No entry exists at the requested checksum.
    Missing,
    /// The immutable tree matched the requested checksum and cache shape.
    Verified(CachedPackageSource),
    /// The entry failed validation; a checksum is present when its contents
    /// could still be hashed.
    Corrupt {
        /// The checksum used to locate the entry.
        expected_checksum: Box<str>,
        /// The checksum computed from its current regular files, if hashable.
        actual_checksum: Option<Box<str>>,
    },
}

/// Outcome after rechecking a source entry before removing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceCacheRemoval {
    /// No entry existed at the requested checksum.
    Missing,
    /// The entry still matched its checksum and immutable cache shape.
    Valid,
    /// The corrupt entry was removed.
    Removed,
}

fn source_digest(checksum: &str) -> Result<&str, PackageCacheError> {
    let invalid = || PackageCacheError::InvalidSourceChecksum(checksum.into());
    let digest = checksum
        .strip_prefix(SOURCE_CHECKSUM_PREFIX)
        .ok_or_else(invalid)?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    Ok(digest)
}

fn validate_canonical_git_url(value: &str) -> Result<(), PackageCacheError> {
    match canonical_git_url(value) {
        Ok(canonical) if canonical == value => Ok(()),
        _ => Err(PackageCacheError::InvalidGitUrl),
    }
}

fn validate_git_tag_name(value: &str) -> Result<(), PackageCacheError> {
    let components = value.strip_prefix("refs/tags/").map(|name| name.split('/'));
    let valid = value.len() <= 8192
        && value.is_ascii()
        && components.is_some_and(|mut components| {
            let mut saw_component = false;
            for component in &mut components {
                saw_component = true;
                if component.is_empty()
                    || component.starts_with('.')
                    || component.ends_with('.')
                    || component.ends_with(".lock")
                    || component.contains("..")
                    || component.contains("@{")
                    || !component
                        .bytes()
                        .all(|byte| (b'!'..=b'~').contains(&byte) && !b"~^:?*[\\".contains(&byte))
                {
                    return false;
                }
            }
            saw_component
        });
    if valid {
        Ok(())
    } else {
        Err(PackageCacheError::InvalidGitTag)
    }
}

fn validate_git_commit(value: &str) -> Result<(), PackageCacheError> {
    if matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PackageCacheError::InvalidGitCommit)
    }
}

fn canonical_package_directory(value: Option<&str>) -> Result<&str, PackageCacheError> {
    let Some(value) = value else {
        return Ok("");
    };
    if value == "." {
        return Ok("");
    }
    RepositoryPackageDirectory::parse(value)
        .map(|_| value)
        .map_err(|_| PackageCacheError::InvalidPackageDirectory)
}

fn cache_record_key(domain: &[u8], fields: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"SAI-CACHE-KEY-v1\0");
    hash_length_prefixed_field(&mut hasher, domain);
    for field in fields {
        hash_length_prefixed_field(&mut hasher, field);
    }
    lowercase_hex(&hasher.finalize())
}

fn hash_length_prefixed_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

struct CacheRecordSlot {
    generation: u64,
    bytes: Vec<u8>,
    slot: usize,
}

fn cache_record_slot_path(base: &Path, slot: usize) -> PathBuf {
    let mut path = base.as_os_str().to_os_string();
    path.push(format!(".{slot}"));
    PathBuf::from(path)
}

fn read_latest_cache_record(
    base: &Path,
    magic: &[u8],
    field_count: usize,
) -> Result<Option<CacheRecordSlot>, PackageCacheError> {
    let mut records = Vec::with_capacity(2);
    let mut corrupt_slots = Vec::with_capacity(2);
    for slot in 0..2 {
        let path = cache_record_slot_path(base, slot);
        let bytes = match read_cache_record(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(PackageCacheError::CorruptRecord(_)) => {
                corrupt_slots.push((path, None));
                continue;
            }
            Err(error) => return Err(error),
        };
        match decode_cache_record(magic, &bytes, field_count) {
            Some((generation, _)) => records.push(CacheRecordSlot {
                generation,
                bytes,
                slot,
            }),
            None => corrupt_slots.push((path, cache_record_generation(magic, &bytes))),
        }
    }
    let newest = match records.as_slice() {
        [] => None,
        [record] => Some(record),
        [first, second] if first.generation != second.generation => {
            Some(if first.generation > second.generation {
                first
            } else {
                second
            })
        }
        _ => return Err(PackageCacheError::CorruptRecord(base.to_path_buf())),
    };
    if let Some(newest) = newest {
        if corrupt_slots.iter().any(|(_, generation)| {
            generation.is_none_or(|generation| generation >= newest.generation)
        }) {
            return Err(PackageCacheError::CorruptRecord(base.to_path_buf()));
        }
        return Ok(Some(CacheRecordSlot {
            generation: newest.generation,
            bytes: newest.bytes.clone(),
            slot: newest.slot,
        }));
    }
    if let Some((path, _)) = corrupt_slots.first() {
        return Err(PackageCacheError::CorruptRecord(path.clone()));
    }
    Ok(None)
}

fn encode_cache_record(magic: &[u8], generation: u64, fields: &[&[u8]]) -> Vec<u8> {
    let mut record = Vec::with_capacity(
        magic.len()
            + 2 * std::mem::size_of::<u64>()
            + fields
                .iter()
                .map(|field| std::mem::size_of::<u32>() + field.len())
                .sum::<usize>()
            + RECORD_DIGEST_BYTES,
    );
    record.extend_from_slice(magic);
    record.extend_from_slice(&generation.to_be_bytes());
    record.extend_from_slice(&(!generation).to_be_bytes());
    for field in fields {
        record.extend_from_slice(
            &u32::try_from(field.len())
                .expect("cache record fields are bounded")
                .to_be_bytes(),
        );
        record.extend_from_slice(field);
    }
    let digest = Sha256::digest(&record);
    record.extend_from_slice(&digest);
    record
}

fn decode_cache_record<'a>(
    magic: &[u8],
    record: &'a [u8],
    field_count: usize,
) -> Option<(u64, Vec<&'a [u8]>)> {
    if record.len() as u64 > MAX_CACHE_RECORD_BYTES || !record.starts_with(magic) {
        return None;
    }
    let payload_end = record.len().checked_sub(RECORD_DIGEST_BYTES)?;
    let (payload, digest) = record.split_at(payload_end);
    if Sha256::digest(payload).as_slice() != digest {
        return None;
    }
    let generation = cache_record_generation(magic, record)?;
    let generation_end = magic.len().checked_add(2 * std::mem::size_of::<u64>())?;
    let mut offset = generation_end;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let length_end = offset.checked_add(std::mem::size_of::<u32>())?;
        let length = u32::from_be_bytes(payload.get(offset..length_end)?.try_into().ok()?) as usize;
        offset = length_end;
        let field_end = offset.checked_add(length)?;
        fields.push(payload.get(offset..field_end)?);
        offset = field_end;
    }
    (offset == payload.len()).then_some((generation, fields))
}

fn cache_record_generation(magic: &[u8], record: &[u8]) -> Option<u64> {
    if record.len() as u64 > MAX_CACHE_RECORD_BYTES || !record.starts_with(magic) {
        return None;
    }
    let generation_start = magic.len();
    let complement_start = generation_start.checked_add(std::mem::size_of::<u64>())?;
    let fields_start = complement_start.checked_add(std::mem::size_of::<u64>())?;
    let generation = u64::from_be_bytes(
        record
            .get(generation_start..complement_start)?
            .try_into()
            .ok()?,
    );
    let complement = u64::from_be_bytes(
        record
            .get(complement_start..fields_start)?
            .try_into()
            .ok()?,
    );
    (generation != 0 && complement == !generation).then_some(generation)
}

fn read_cache_record(path: &Path) -> Result<Option<Vec<u8>>, PackageCacheError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "inspect package cache record",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_CACHE_RECORD_BYTES
    {
        return Err(PackageCacheError::CorruptRecord(path.to_path_buf()));
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "open package cache record",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| PackageCacheError::Io {
            operation: "read package cache record",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_CACHE_RECORD_BYTES {
        return Err(PackageCacheError::CorruptRecord(path.to_path_buf()));
    }
    Ok(Some(bytes))
}

fn lowercase_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}

fn ensure_cache_directory(path: &Path) -> Result<(), PackageCacheError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(PackageCacheError::Io {
                    operation: "create package cache directory",
                    path: path.to_path_buf(),
                    source,
                });
            }
        },
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "inspect package cache directory",
                path: path.to_path_buf(),
                source,
            });
        }
        Ok(_) => {}
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| PackageCacheError::Io {
        operation: "inspect package cache directory after creation",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PackageCacheError::Io {
            operation: "validate package cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache layout entry is not a directory",
            ),
        });
    }
    set_private_directory_permissions(path)
}

fn create_private_directory(path: &Path) -> Result<(), PackageCacheError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    set_new_directory_mode(&mut builder);
    builder
        .create(path)
        .map_err(|source| PackageCacheError::Io {
            operation: "create package cache directory",
            path: path.to_path_buf(),
            source,
        })?;
    set_private_directory_permissions(path)
}

fn create_staging_directory(path: &Path, cache_root: &Path) -> Result<(), PackageCacheError> {
    let mut builder = fs::DirBuilder::new();
    set_new_directory_mode(&mut builder);
    builder
        .create(path)
        .map_err(|source| PackageCacheError::Io {
            operation: "create unique package cache staging directory",
            path: path.to_path_buf(),
            source,
        })?;
    if let Err(error) = set_private_directory_permissions(path) {
        let _ = remove_tree_at_path(path, cache_root);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn create_staging_activity_lock(
    path: &Path,
    cache_root: &Path,
) -> Result<File, PackageCacheError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    set_new_file_mode(&mut options);
    set_exclusive_file_sharing(&mut options);
    let file = options.open(path).map_err(|source| PackageCacheError::Io {
        operation: "create package cache staging activity lock",
        path: path.to_path_buf(),
        source,
    })?;
    if let Err(source) = acquire_platform_lock(&file) {
        drop(file);
        let _ = remove_file_at_path(path, cache_root);
        return Err(PackageCacheError::Io {
            operation: "lock package cache staging directory",
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(file)
}

fn try_open_staging_activity_lock(path: &Path) -> Result<Option<File>, PackageCacheError> {
    let file = match open_lock_file(path) {
        Ok(file) => file,
        Err(PackageCacheError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound || is_platform_lock_busy(&source) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    match try_acquire_platform_lock(&file) {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(source) => Err(PackageCacheError::Io {
            operation: "check package cache staging activity",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn staging_activity_lock_supported() -> bool {
    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    ))]
    {
        true
    }
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(any(
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        ),
        windows
    )))]
    {
        false
    }
}

fn try_acquire_platform_lock(file: &File) -> io::Result<bool> {
    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    ))]
    {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map(|()| true)
            .map_err(Into::into)
            .or_else(|error: io::Error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    Ok(false)
                } else {
                    Err(error)
                }
            })
    }

    #[cfg(windows)]
    {
        let _ = file;
        // The open handle already has share_mode(0), which excludes another
        // process from opening the activity marker until this handle closes.
        return Ok(true);
    }

    #[cfg(not(any(
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        ),
        windows
    )))]
    {
        let _ = file;
        Ok(false)
    }
}

fn is_platform_lock_busy(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error().is_some_and(|code| {
            matches!(
                code,
                WINDOWS_ERROR_SHARING_VIOLATION | WINDOWS_ERROR_LOCK_VIOLATION
            )
        })
    }
    #[cfg(not(windows))]
    {
        error.kind() == io::ErrorKind::WouldBlock
    }
}

pub(crate) fn staging_activity_lock_path(staging_path: &Path) -> PathBuf {
    let mut path = staging_path.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

pub(crate) fn release_staging_activity_lock(
    path: &Path,
    lock: File,
    remove_marker: bool,
    cache_root: &Path,
) {
    let marker_parent = remove_marker
        .then(|| open_parent_at_boundary(path, cache_root))
        .flatten();

    #[cfg(unix)]
    if let Some((parent, name)) = &marker_parent {
        let _ = remove_file_from_parent(parent, name);
    }

    drop(lock);

    #[cfg(not(unix))]
    if let Some((parent, name)) = &marker_parent {
        // Windows does not allow unlinking an open marker. Keep the verified
        // parent capability while releasing the exclusive handle.
        let _ = remove_file_from_parent(parent, name);
    }
}

fn legacy_staging_owner_is_gone(name: &str) -> bool {
    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    ))]
    {
        let Some(pid) = name
            .strip_prefix("source-")
            .or_else(|| name.strip_prefix("sai-git-remote-"))
            .and_then(|suffix| suffix.split_once('-').map(|(pid, _)| pid))
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            return false;
        };

        // A recycled PID can conservatively keep an abandoned legacy tree;
        // treating an unknown/live process as active avoids deleting an old
        // writer's staging directory during a toolchain upgrade.
        matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        )
    }
    #[cfg(not(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    )))]
    {
        let _ = name;
        false
    }
}

fn is_cache_temporary_directory_name(name: &str) -> bool {
    name.starts_with("source-") || name.starts_with("sai-git-remote-")
}

fn is_stale_temporary_path(path: &Path, now: SystemTime) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    now.duration_since(modified)
        .is_ok_and(|age| age >= STALE_TEMPORARY_AGE)
}

fn ensure_lock_file(path: &Path) -> Result<(), PackageCacheError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_new_file_mode(&mut options);
    match options.open(path) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_lock_file(path)?;
        }
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "create package cache lock",
                path: path.to_path_buf(),
                source,
            });
        }
    }
    Ok(())
}

fn validate_lock_file(path: &Path) -> Result<(), PackageCacheError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PackageCacheError::Io {
        operation: "inspect package cache lock",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        return Ok(());
    }
    Err(PackageCacheError::Io {
        operation: "validate package cache lock",
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache lock is not a regular file",
        ),
    })
}

fn open_lock_file(path: &Path) -> Result<File, PackageCacheError> {
    validate_lock_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    set_exclusive_file_sharing(&mut options);
    options.open(path).map_err(|source| PackageCacheError::Io {
        operation: "open package cache lock",
        path: path.to_path_buf(),
        source,
    })
}

fn open_publication_lock(path: &Path) -> Result<File, PackageCacheError> {
    #[cfg(windows)]
    {
        loop {
            match open_lock_file(path) {
                Ok(file) => return Ok(file),
                Err(PackageCacheError::Io { source, .. })
                    if source.raw_os_error().is_some_and(|code| {
                        matches!(
                            code,
                            WINDOWS_ERROR_SHARING_VIOLATION | WINDOWS_ERROR_LOCK_VIOLATION
                        )
                    }) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        }
    }

    #[cfg(not(windows))]
    {
        open_lock_file(path)
    }
}

fn write_source_file(
    staging_root: &Path,
    source_file: &GitPackageFile,
) -> Result<(), PackageCacheError> {
    let relative_path = safe_relative_path(source_file.path())?;
    let path = staging_root.join(relative_path);
    let parent = path.parent().expect("validated package path has a parent");
    create_private_directory(parent)?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_new_file_mode(&mut options);
    let mut file = options
        .open(&path)
        .map_err(|source| PackageCacheError::Io {
            operation: "create package source file",
            path: path.clone(),
            source,
        })?;
    file.write_all(source_file.content())
        .map_err(|source| PackageCacheError::Io {
            operation: "write package source file",
            path: path.clone(),
            source,
        })?;
    file.flush().map_err(|source| PackageCacheError::Io {
        operation: "flush package source file",
        path: path.clone(),
        source,
    })?;
    set_read_only_file_permissions(&file, &path)
}

fn safe_relative_path(value: &str) -> Result<PathBuf, PackageCacheError> {
    let invalid = || PackageCacheError::InvalidSourcePath(value.into());
    if value.is_empty() || value.contains('\\') || value.len() > 4096 {
        return Err(invalid());
    }

    let mut relative = PathBuf::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment.len() > 255 {
            return Err(invalid());
        }
        relative.push(segment);
    }
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid());
    }
    Ok(relative)
}

fn path_exists_without_following(path: &Path) -> Result<bool, PackageCacheError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PackageCacheError::Io {
            operation: "inspect package cache entry",
            path: path.to_path_buf(),
            source,
        }),
    }
}

struct CachedHashFileEntry {
    relative_path: String,
    absolute_path: PathBuf,
    parent_directory: CapabilityDir,
    name: OsString,
    file: File,
    size: u64,
    read_only: bool,
}

struct CachedHashDirectoryEntry {
    parent_directory: CapabilityDir,
    name: OsString,
    directory: CapabilityDir,
    relative_path: String,
}

enum CachedSourceRoot {
    Missing,
    Corrupt,
    Opened {
        source_directory: CapabilityDir,
        root_name: OsString,
        root_directory: CapabilityDir,
    },
}

enum SourceTreeInspection {
    Missing,
    Corrupt {
        actual_checksum: Option<String>,
    },
    Hashed {
        actual_checksum: String,
        immutable: bool,
    },
}

fn inspect_cached_source(root: &Path) -> Result<SourceTreeInspection, PackageCacheError> {
    let (source_directory, root_name, root_directory) = match open_cached_source_root(root)? {
        CachedSourceRoot::Missing => return Ok(SourceTreeInspection::Missing),
        CachedSourceRoot::Corrupt => {
            return Ok(SourceTreeInspection::Corrupt {
                actual_checksum: None,
            });
        }
        CachedSourceRoot::Opened {
            source_directory,
            root_name,
            root_directory,
        } => (source_directory, root_name, root_directory),
    };
    let root_metadata =
        capability_directory_metadata(&root_directory).map_err(|source| PackageCacheError::Io {
            operation: "inspect opened cached package source",
            path: root.to_path_buf(),
            source,
        })?;
    if !root_metadata.is_dir()
        || !cached_directory_entry_matches(&source_directory, &root_name, &root_directory)?
    {
        return Ok(SourceTreeInspection::Corrupt {
            actual_checksum: None,
        });
    }

    let mut immutable = is_read_only_directory(&root_metadata);
    let mut files = Vec::new();
    let mut directories = BTreeSet::new();
    let mut pending = vec![(root_directory, root_name, source_directory, String::new())];
    let mut directory_entries = Vec::new();
    let mut entry_count = 0u64;
    let mut path_bytes = 0u64;
    let mut content_bytes = 0u64;
    let mut saw_manifest = false;

    while let Some((directory, directory_name, parent_directory, parent_relative_path)) =
        pending.pop()
    {
        let directory_metadata =
            capability_directory_metadata(&directory).map_err(|source| PackageCacheError::Io {
                operation: "inspect opened cached package source directory",
                path: root.join(&parent_relative_path),
                source,
            })?;
        if !directory_metadata.is_dir() || !is_traversable_directory(&directory_metadata) {
            return Ok(SourceTreeInspection::Corrupt {
                actual_checksum: None,
            });
        }
        let entries = match directory.entries() {
            Ok(entries) => entries,
            Err(source) if cached_entry_error_is_corruption(&source) => {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }
            Err(source) => {
                return Err(PackageCacheError::Io {
                    operation: "read cached package source directory",
                    path: root.join(&parent_relative_path),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) if cached_entry_error_is_corruption(&source) => {
                    return Ok(SourceTreeInspection::Corrupt {
                        actual_checksum: None,
                    });
                }
                Err(source) => {
                    return Err(PackageCacheError::Io {
                        operation: "read cached package source entry",
                        path: root.join(&parent_relative_path),
                        source,
                    });
                }
            };
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_PACKAGE_TREE_ENTRIES {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }

            let name = entry.file_name();
            let Some(name_string) = name.to_str().map(str::to_owned) else {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            };
            let relative_path = if parent_relative_path.is_empty() {
                name_string
            } else {
                format!("{parent_relative_path}/{name_string}")
            };
            path_bytes = path_bytes.saturating_add(relative_path.len() as u64);
            if path_bytes > MAX_PACKAGE_TREE_PATH_BYTES
                || safe_relative_path(&relative_path).is_err()
                || validate_tree_path(relative_path.as_bytes()).is_err()
                || should_skip_path(
                    relative_path.as_bytes(),
                    crate::tree_hash::TreeHashPurpose::Source,
                )
            {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(source) if cached_entry_error_is_corruption(&source) => {
                    return Ok(SourceTreeInspection::Corrupt {
                        actual_checksum: None,
                    });
                }
                Err(source) => {
                    return Err(PackageCacheError::Io {
                        operation: "inspect cached package source entry type",
                        path: root.join(&relative_path),
                        source,
                    });
                }
            };
            if file_type.is_symlink() {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }

            if file_type.is_dir() {
                let child_directory = match entry.open_dir() {
                    Ok(directory) => directory,
                    Err(source) if cached_entry_error_is_corruption(&source) => {
                        return Ok(SourceTreeInspection::Corrupt {
                            actual_checksum: None,
                        });
                    }
                    Err(source) => {
                        return Err(PackageCacheError::Io {
                            operation: "open cached package source directory",
                            path: root.join(&relative_path),
                            source,
                        });
                    }
                };
                let child_metadata =
                    capability_directory_metadata(&child_directory).map_err(|source| {
                        PackageCacheError::Io {
                            operation: "inspect opened cached package source directory",
                            path: root.join(&relative_path),
                            source,
                        }
                    })?;
                if !child_metadata.is_dir()
                    || !cached_directory_entry_matches(&directory, &name, &child_directory)?
                {
                    return Ok(SourceTreeInspection::Corrupt {
                        actual_checksum: None,
                    });
                }
                immutable &= is_read_only_directory(&child_metadata);
                directories.insert(relative_path.clone());
                let parent_clone = CapabilityDir::reopen_dir(&directory).map_err(|source| {
                    PackageCacheError::Io {
                        operation: "retain cached package source directory handle",
                        path: root.join(&relative_path),
                        source,
                    }
                })?;
                pending.push((child_directory, name, parent_clone, relative_path));
                continue;
            }
            if !file_type.is_file() || files.len() as u64 >= MAX_PACKAGE_FILE_COUNT {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }
            let file = match open_cached_source_file(&directory, &name) {
                Ok(file) => file,
                Err(source) if cached_entry_error_is_corruption(&source) => {
                    return Ok(SourceTreeInspection::Corrupt {
                        actual_checksum: None,
                    });
                }
                Err(source) => {
                    return Err(PackageCacheError::Io {
                        operation: "open cached package source file",
                        path: root.join(&relative_path),
                        source,
                    });
                }
            };
            let metadata = file.metadata().map_err(|source| PackageCacheError::Io {
                operation: "inspect opened cached package source file",
                path: root.join(&relative_path),
                source,
            })?;
            if !metadata.is_file()
                || !cached_file_entry_matches(&directory, &name, &file)?
                || files.len() as u64 >= MAX_PACKAGE_FILE_COUNT
            {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }
            if metadata.len() > MAX_PACKAGE_FILE_BYTES {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }
            content_bytes = content_bytes.saturating_add(metadata.len());
            if content_bytes > MAX_PACKAGE_CONTENT_BYTES {
                return Ok(SourceTreeInspection::Corrupt {
                    actual_checksum: None,
                });
            }
            if relative_path == "sai.package" {
                saw_manifest = true;
                if metadata.len() > MAX_PACKAGE_MANIFEST_BYTES as u64 {
                    return Ok(SourceTreeInspection::Corrupt {
                        actual_checksum: None,
                    });
                }
            }
            let read_only = is_read_only_file(Some(&file), &metadata);
            immutable &= read_only;
            files.push(CachedHashFileEntry {
                absolute_path: root.join(&relative_path),
                parent_directory: CapabilityDir::reopen_dir(&directory).map_err(|source| {
                    PackageCacheError::Io {
                        operation: "retain cached package source directory handle",
                        path: root.join(&relative_path),
                        source,
                    }
                })?,
                relative_path,
                name,
                file,
                size: metadata.len(),
                read_only,
            });
        }
        directory_entries.push(CachedHashDirectoryEntry {
            parent_directory,
            name: directory_name,
            directory,
            relative_path: parent_relative_path,
        });
    }

    if !saw_manifest {
        return Ok(SourceTreeInspection::Corrupt {
            actual_checksum: None,
        });
    }
    files.sort_by(|left, right| {
        left.relative_path
            .as_bytes()
            .cmp(right.relative_path.as_bytes())
    });
    let mut expected_directories = BTreeSet::new();
    for file in &files {
        for (separator, _) in file.relative_path.match_indices('/') {
            expected_directories.insert(file.relative_path[..separator].to_owned());
        }
    }
    immutable &= directories == expected_directories;

    let paths = files
        .iter()
        .map(|file| file.relative_path.as_bytes())
        .collect::<Vec<_>>();
    if reject_tree_path_collisions(&paths).is_err() {
        return Ok(SourceTreeInspection::Corrupt {
            actual_checksum: None,
        });
    }

    for entry in &directory_entries {
        if !cached_directory_entry_matches(&entry.parent_directory, &entry.name, &entry.directory)?
        {
            return Ok(SourceTreeInspection::Corrupt {
                actual_checksum: None,
            });
        }
        let metadata = capability_directory_metadata(&entry.directory).map_err(|source| {
            PackageCacheError::Io {
                operation: "recheck cached package source directory permissions",
                path: root.join(&entry.relative_path),
                source,
            }
        })?;
        immutable &= is_read_only_directory(&metadata);
    }

    let mut tree_hasher = Sha256::new();
    for file in &files {
        let Some((content_digest, file_immutable)) = hash_cached_source_file(file)? else {
            return Ok(SourceTreeInspection::Corrupt {
                actual_checksum: None,
            });
        };
        immutable &= file_immutable && file.read_only;
        tree_hasher.update(lowercase_hex(&content_digest).as_bytes());
        tree_hasher.update(b"  ");
        tree_hasher.update(file.relative_path.as_bytes());
        tree_hasher.update(b"\n");
    }
    let actual_checksum = format!("sai1:{}", lowercase_hex(&tree_hasher.finalize()));
    Ok(SourceTreeInspection::Hashed {
        actual_checksum,
        immutable,
    })
}

fn open_cached_source_root(root: &Path) -> Result<CachedSourceRoot, PackageCacheError> {
    let (source_directory, root_name) = open_cached_source_parent(root)?;
    let metadata = match source_directory.symlink_metadata(&root_name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CachedSourceRoot::Missing);
        }
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "inspect cached package source entry",
                path: root.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink()
        || is_windows_directory_reparse_point(&metadata)
        || !metadata.is_dir()
    {
        return Ok(CachedSourceRoot::Corrupt);
    }
    let root_directory = match source_directory.open_dir(&root_name) {
        Ok(directory) => directory,
        Err(source) if cached_entry_error_is_corruption(&source) => {
            return Ok(CachedSourceRoot::Corrupt);
        }
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "open cached package source directory",
                path: root.to_path_buf(),
                source,
            });
        }
    };
    if !cached_directory_entry_matches(&source_directory, &root_name, &root_directory)? {
        return Ok(CachedSourceRoot::Corrupt);
    }
    Ok(CachedSourceRoot::Opened {
        source_directory,
        root_name,
        root_directory,
    })
}

fn open_cached_source_parent(root: &Path) -> Result<(CapabilityDir, OsString), PackageCacheError> {
    let Some(source_directory_path) = root.parent() else {
        return Err(invalid_cached_source_path(root));
    };
    let Some(versioned_root_path) = source_directory_path.parent() else {
        return Err(invalid_cached_source_path(root));
    };
    let Some(cache_root_path) = versioned_root_path.parent() else {
        return Err(invalid_cached_source_path(root));
    };
    if source_directory_path.file_name() != Some(OsStr::new("src"))
        || versioned_root_path.file_name() != Some(OsStr::new(CACHE_LAYOUT_VERSION))
    {
        return Err(invalid_cached_source_path(root));
    }

    let cache_root = CapabilityDir::open_ambient_dir(cache_root_path, ambient_authority())
        .map_err(|source| PackageCacheError::Io {
            operation: "open package source cache root",
            path: cache_root_path.to_path_buf(),
            source,
        })?;
    let versioned_root = open_cache_layout_directory(
        &cache_root,
        OsStr::new(CACHE_LAYOUT_VERSION),
        versioned_root_path,
    )?;
    let source_directory =
        open_cache_layout_directory(&versioned_root, OsStr::new("src"), source_directory_path)?;
    let root_name = root
        .file_name()
        .ok_or_else(|| invalid_cached_source_path(root))?
        .to_os_string();

    Ok((source_directory, root_name))
}

fn invalid_cached_source_path(path: &Path) -> PackageCacheError {
    PackageCacheError::InvalidSourceChecksum(path.display().to_string().into())
}

fn open_cache_layout_directory(
    parent: &CapabilityDir,
    name: &OsStr,
    path: &Path,
) -> Result<CapabilityDir, PackageCacheError> {
    let metadata = parent
        .symlink_metadata(name)
        .map_err(|source| PackageCacheError::Io {
            operation: "inspect package source cache directory",
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink()
        || is_windows_directory_reparse_point(&metadata)
        || !metadata.is_dir()
    {
        return Err(PackageCacheError::Io {
            operation: "validate package source cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache layout entry is not a directory",
            ),
        });
    }
    let directory = parent
        .open_dir(name)
        .map_err(|source| PackageCacheError::Io {
            operation: "open package source cache directory",
            path: path.to_path_buf(),
            source,
        })?;
    if !directory
        .dir_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(PackageCacheError::Io {
            operation: "validate opened package source cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache layout entry is not a directory",
            ),
        });
    }
    if !cached_directory_entry_matches(parent, name, &directory)? {
        return Err(PackageCacheError::Io {
            operation: "recheck package source cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache layout entry changed while opening",
            ),
        });
    }
    Ok(directory)
}

fn hash_cached_source_file(
    cached: &CachedHashFileEntry,
) -> Result<Option<([u8; 32], bool)>, PackageCacheError> {
    let opened_metadata = cached
        .file
        .metadata()
        .map_err(|source| PackageCacheError::Io {
            operation: "inspect opened cached package source file",
            path: cached.absolute_path.clone(),
            source,
        })?;
    if !opened_metadata.is_file()
        || opened_metadata.len() != cached.size
        || opened_metadata.len() > MAX_PACKAGE_FILE_BYTES
        || !cached_file_entry_matches(&cached.parent_directory, &cached.name, &cached.file)?
    {
        return Ok(None);
    }

    let mut file = cached
        .file
        .try_clone()
        .map_err(|source| PackageCacheError::Io {
            operation: "clone cached package source file handle",
            path: cached.absolute_path.clone(),
            source,
        })?;
    let mut content_hasher = Sha256::new();
    let mut bytes_read = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| PackageCacheError::Io {
                operation: "read cached package source file",
                path: cached.absolute_path.clone(),
                source,
            })?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(count as u64);
        if bytes_read > cached.size || bytes_read > MAX_PACKAGE_FILE_BYTES {
            return Ok(None);
        }
        content_hasher.update(&buffer[..count]);
    }
    let final_metadata = cached
        .file
        .metadata()
        .map_err(|source| PackageCacheError::Io {
            operation: "recheck opened package source file",
            path: cached.absolute_path.clone(),
            source,
        })?;
    if bytes_read != cached.size
        || !cached_file_metadata_unchanged(&opened_metadata, &final_metadata)
    {
        return Ok(None);
    }
    if !cached_file_entry_matches(&cached.parent_directory, &cached.name, &cached.file)? {
        return Ok(None);
    }
    let immutable = cached.read_only && is_read_only_file(Some(&cached.file), &final_metadata);
    Ok(Some((content_hasher.finalize().into(), immutable)))
}

fn capability_directory_metadata(directory: &CapabilityDir) -> io::Result<fs::Metadata> {
    CapabilityDir::reopen_dir(directory)?
        .into_std_file()
        .metadata()
}

fn cached_directory_entry_matches(
    parent_directory: &CapabilityDir,
    name: &OsStr,
    directory: &CapabilityDir,
) -> Result<bool, PackageCacheError> {
    let metadata = match parent_directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(source) if cached_entry_error_is_corruption(&source) => return Ok(false),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "recheck cached package source directory",
                path: PathBuf::from(name),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(false);
    }
    let current = match parent_directory.open_dir(name) {
        Ok(directory) => directory,
        Err(source) if cached_entry_error_is_corruption(&source) => return Ok(false),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "reopen cached package source directory",
                path: PathBuf::from(name),
                source,
            });
        }
    };
    same_cached_directories(directory, &current).map_err(|source| PackageCacheError::Io {
        operation: "compare cached package source directories",
        path: PathBuf::from(name),
        source,
    })
}

fn cached_file_entry_matches(
    parent_directory: &CapabilityDir,
    name: &OsStr,
    file: &File,
) -> Result<bool, PackageCacheError> {
    let metadata = match parent_directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(source) if cached_entry_error_is_corruption(&source) => return Ok(false),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "inspect cached package source file entry",
                path: PathBuf::from(name),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let current = match open_cached_source_file(parent_directory, name) {
        Ok(file) => file,
        Err(source) if cached_entry_error_is_corruption(&source) => return Ok(false),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "reopen cached package source file entry",
                path: PathBuf::from(name),
                source,
            });
        }
    };
    same_cached_file(file, &current).map_err(|source| PackageCacheError::Io {
        operation: "compare cached package source files",
        path: PathBuf::from(name),
        source,
    })
}

fn same_cached_file(first: &File, second: &File) -> io::Result<bool> {
    let first = SameFileHandle::from_file(first.try_clone()?)?;
    let second = SameFileHandle::from_file(second.try_clone()?)?;
    Ok(first == second)
}

fn same_cached_directories(first: &CapabilityDir, second: &CapabilityDir) -> io::Result<bool> {
    let first = CapabilityDir::reopen_dir(first)?.into_std_file();
    let second = CapabilityDir::reopen_dir(second)?.into_std_file();
    same_cached_file(&first, &second)
}

fn cached_file_metadata_unchanged(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    let unchanged = first.is_file()
        && second.is_file()
        && first.len() == second.len()
        && first.modified().ok() == second.modified().ok()
        && first.permissions().readonly() == second.permissions().readonly();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unchanged && first.ctime() == second.ctime() && first.ctime_nsec() == second.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        unchanged
    }
}

fn cached_entry_error_is_corruption(error: &io::Error) -> bool {
    let common = matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidInput
    );
    #[cfg(unix)]
    {
        common || error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        common
    }
}

fn is_windows_directory_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        };
        let attributes = metadata.file_attributes();
        attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn is_traversable_directory(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o500 == 0o500
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn cached_source_matches(root: &Path, expected_checksum: &str) -> Result<bool, PackageCacheError> {
    Ok(matches!(
        inspect_cached_source(root)?,
        SourceTreeInspection::Hashed {
            actual_checksum,
            immutable: true,
        } if actual_checksum == expected_checksum
    ))
}

fn is_read_only_directory(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777 == 0o555
    }
    #[cfg(not(unix))]
    {
        metadata.permissions().readonly()
    }
}

fn is_read_only_file(file: Option<&File>, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        let _ = file;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777 == 0o444 && metadata.nlink() == 1
    }
    #[cfg(windows)]
    {
        metadata.permissions().readonly() && is_single_link_file(file, metadata)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        metadata.permissions().readonly()
    }
}

fn open_cached_source_file(directory: &CapabilityDir, name: &OsStr) -> io::Result<File> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        };
        // Keep the opened snapshot stable against writes while allowing a
        // corrupt entry to be renamed or removed by another cache operation.
        // Open reparse points themselves so a replacement symlink cannot make
        // this handle read or modify its target.
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
        options.custom_flags(WINDOWS_FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = directory
        .open_with(name, &options)
        .map(|file| file.into_std())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache entry is a Windows reparse point",
            ));
        }
    }
    Ok(file)
}

#[cfg(windows)]
fn remove_cache_entry(path: &Path) -> Result<(), PackageCacheError> {
    retry_cache_entry_removal(|| remove_cache_entry_once(path))
}

#[cfg(windows)]
fn retry_cache_entry_removal(
    mut remove_once: impl FnMut() -> Result<(), PackageCacheError>,
) -> Result<(), PackageCacheError> {
    let mut retries = 0;
    loop {
        match remove_once() {
            Err(error) if cache_entry_removal_is_busy(&error) => {
                if retries < CACHE_ENTRY_REMOVE_RETRY_LIMIT {
                    retries += 1;
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(error);
            }
            result => return result,
        }
    }
}

#[cfg(not(windows))]
fn remove_cache_entry(path: &Path) -> Result<(), PackageCacheError> {
    remove_cache_entry_once(path)
}

fn remove_cache_entry_once(path: &Path) -> Result<(), PackageCacheError> {
    let (parent_directory, name) = open_cached_source_parent(path)?;
    let metadata = match parent_directory.symlink_metadata(&name) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(PackageCacheError::Io {
                operation: "inspect inconsistent package cache entry",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        make_capability_tree_removable(&parent_directory, &name, path)?;
        parent_directory.remove_dir_all(&name)
    } else if is_windows_directory_reparse_point(&metadata) {
        parent_directory.remove_dir(&name)
    } else {
        if metadata.is_file() {
            let file = match open_cached_source_file(&parent_directory, &name) {
                Ok(file) => Some(file),
                Err(source) if cached_entry_error_is_corruption(&source) => None,
                Err(source) => {
                    return Err(PackageCacheError::Io {
                        operation: "open inconsistent package cache file",
                        path: path.to_path_buf(),
                        source,
                    });
                }
            };
            if let Some(file) = file {
                let opened_metadata = file.metadata().map_err(|source| PackageCacheError::Io {
                    operation: "inspect inconsistent package cache file",
                    path: path.to_path_buf(),
                    source,
                })?;
                if cached_file_entry_matches(&parent_directory, &name, &file)?
                    && is_single_link_file(Some(&file), &opened_metadata)
                {
                    set_writable_file_handle(&file, &opened_metadata, path)?;
                }
            }
        }
        if metadata.is_dir() {
            parent_directory.remove_dir(&name)
        } else {
            parent_directory.remove_file(&name)
        }
    }
    .map_err(|source| PackageCacheError::Io {
        operation: "remove inconsistent package cache entry",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn cache_entry_removal_is_busy(error: &PackageCacheError) -> bool {
    matches!(
        error,
        PackageCacheError::Io { source, .. }
            if matches!(
                source.raw_os_error(),
                Some(WINDOWS_ERROR_LOCK_VIOLATION)
                    | Some(WINDOWS_ERROR_SHARING_VIOLATION)
            )
    )
}

fn make_capability_tree_removable(
    parent_directory: &CapabilityDir,
    name: &OsStr,
    path: &Path,
) -> Result<(), PackageCacheError> {
    let metadata =
        parent_directory
            .symlink_metadata(name)
            .map_err(|source| PackageCacheError::Io {
                operation: "inspect inconsistent package cache directory",
                path: path.to_path_buf(),
                source,
            })?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }

    set_capability_directory_writable(parent_directory, name, &metadata, path)?;
    let directory = parent_directory
        .open_dir(name)
        .map_err(|source| PackageCacheError::Io {
            operation: "open inconsistent package cache directory",
            path: path.to_path_buf(),
            source,
        })?;
    if !cached_directory_entry_matches(parent_directory, name, &directory)? {
        return Err(PackageCacheError::Io {
            operation: "recheck inconsistent package cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache entry changed while opening",
            ),
        });
    }
    for entry in directory
        .entries()
        .map_err(|source| PackageCacheError::Io {
            operation: "read inconsistent package cache directory",
            path: path.to_path_buf(),
            source,
        })?
    {
        let entry = entry.map_err(|source| PackageCacheError::Io {
            operation: "read inconsistent package cache entry",
            path: path.to_path_buf(),
            source,
        })?;
        let child_name = entry.file_name();
        let child_path = path.join(&child_name);
        let file_type = entry.file_type().map_err(|source| PackageCacheError::Io {
            operation: "inspect inconsistent package cache entry type",
            path: child_path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            make_capability_tree_removable(&directory, &child_name, &child_path)?;
            continue;
        }
        if file_type.is_file() {
            let file = match open_cached_source_file(&directory, &child_name) {
                Ok(file) => file,
                Err(source) if cached_entry_error_is_corruption(&source) => continue,
                Err(source) => {
                    return Err(PackageCacheError::Io {
                        operation: "open inconsistent package cache file",
                        path: child_path.clone(),
                        source,
                    });
                }
            };
            let metadata = file.metadata().map_err(|source| PackageCacheError::Io {
                operation: "open inconsistent package cache file",
                path: child_path.clone(),
                source,
            })?;
            if cached_file_entry_matches(&directory, &child_name, &file)?
                && is_single_link_file(Some(&file), &metadata)
            {
                set_writable_file_handle(&file, &metadata, &child_path)?;
            }
        }
    }

    // The directory was made writable through its verified parent before it
    // was opened. Recheck that the name still points to that handle; chmod on
    // a reopened O_PATH directory handle can return EBADF on overlayfs.
    if !cached_directory_entry_matches(parent_directory, name, &directory)? {
        return Err(PackageCacheError::Io {
            operation: "recheck inconsistent package cache directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache entry changed while making it removable",
            ),
        });
    }
    Ok(())
}

fn set_writable_file_handle(
    file: &File,
    metadata: &fs::Metadata,
    path: &Path,
) -> Result<(), PackageCacheError> {
    let result = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
            file.set_permissions(fs::Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            file.set_permissions(permissions)
        }
    };
    result.map_err(|source| PackageCacheError::Io {
        operation: "make inconsistent package cache entry removable",
        path: path.to_path_buf(),
        source,
    })
}

fn set_capability_directory_writable(
    parent_directory: &CapabilityDir,
    name: &OsStr,
    metadata: &cap_std::fs::Metadata,
    path: &Path,
) -> Result<(), PackageCacheError> {
    match parent_directory.set_permissions(name, writable_capability_permissions(metadata, true)) {
        Ok(()) => Ok(()),
        Err(source) if permission_change_needs_path_fallback(&source) => {
            // Some Unix filesystems cannot open an unreadable directory for
            // handle-based chmod. Retry relative to the opened parent and ask
            // the OS not to follow a replacement symlink; if unsupported, fail
            // closed rather than changing an ambient path.
            let current = parent_directory.symlink_metadata(name).map_err(|source| {
                PackageCacheError::Io {
                    operation: "recheck inconsistent package cache directory",
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            if current.file_type().is_symlink() || !current.is_dir() {
                return Err(PackageCacheError::Io {
                    operation: "validate inconsistent package cache directory",
                    path: path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cache entry changed while preparing removal",
                    ),
                });
            }
            set_capability_directory_writable_nofollow(parent_directory, name, path)
        }
        Err(source) => Err(PackageCacheError::Io {
            operation: "make inconsistent package cache directory readable",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn permission_change_needs_path_fallback(error: &io::Error) -> bool {
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "wasi", target_os = "redox"))
    ))]
    {
        error.raw_os_error() == Some(libc::ENOTSUP)
    }
    #[cfg(not(all(
        unix,
        not(any(target_os = "espidf", target_os = "wasi", target_os = "redox"))
    )))]
    {
        let _ = error;
        false
    }
}

#[cfg(unix)]
fn set_capability_directory_writable_nofollow(
    parent_directory: &CapabilityDir,
    name: &OsStr,
    path: &Path,
) -> Result<(), PackageCacheError> {
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "wasi", target_os = "redox"))
    ))]
    {
        let parent = CapabilityDir::reopen_dir(parent_directory)
            .map_err(|source| PackageCacheError::Io {
                operation: "retain package cache parent while changing permissions",
                path: path.to_path_buf(),
                source,
            })?
            .into_std_file();
        rustix::fs::chmodat(
            &parent,
            name,
            rustix::fs::Mode::from_bits_truncate(0o700),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|source| PackageCacheError::Io {
            operation: "make inconsistent package cache directory readable",
            path: path.to_path_buf(),
            source: source.into(),
        })
    }
    #[cfg(not(all(
        unix,
        not(any(target_os = "espidf", target_os = "wasi", target_os = "redox"))
    )))]
    {
        let _ = (parent_directory, name);
        Err(PackageCacheError::Io {
            operation: "make inconsistent package cache directory readable",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "no-follow directory permission changes are unavailable",
            ),
        })
    }
}

#[cfg(not(unix))]
fn set_capability_directory_writable_nofollow(
    _parent_directory: &CapabilityDir,
    _name: &OsStr,
    path: &Path,
) -> Result<(), PackageCacheError> {
    Err(PackageCacheError::Io {
        operation: "make inconsistent package cache directory readable",
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow directory permission changes are unavailable",
        ),
    })
}

fn writable_capability_permissions(
    _metadata: &cap_std::fs::Metadata,
    directory: bool,
) -> CapabilityPermissions {
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        CapabilityPermissions::from_mode(if directory { 0o700 } else { 0o600 })
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        let mut permissions = _metadata.permissions();
        permissions.set_readonly(false);
        permissions
    }
}

struct StagingDirectory {
    path: PathBuf,
    cache_root: PathBuf,
    activity_lock_path: PathBuf,
    activity_lock: Option<File>,
    published: bool,
}

impl StagingDirectory {
    fn create(parent: &Path, cache_root: &Path) -> Result<Self, PackageCacheError> {
        for _ in 0..32 {
            let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("source-{}-{id}", std::process::id()));
            let activity_lock_path = staging_activity_lock_path(&path);
            let activity_lock = match create_staging_activity_lock(&activity_lock_path, cache_root)
            {
                Ok(file) => file,
                Err(PackageCacheError::Io { source, .. })
                    if source.kind() == io::ErrorKind::AlreadyExists =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            match create_staging_directory(&path, cache_root) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        cache_root: cache_root.to_path_buf(),
                        activity_lock_path,
                        activity_lock: Some(activity_lock),
                        published: false,
                    });
                }
                Err(PackageCacheError::Io { source, .. })
                    if source.kind() == io::ErrorKind::AlreadyExists =>
                {
                    release_staging_activity_lock(
                        &activity_lock_path,
                        activity_lock,
                        true,
                        cache_root,
                    );
                }
                Err(error) => {
                    release_staging_activity_lock(
                        &activity_lock_path,
                        activity_lock,
                        true,
                        cache_root,
                    );
                    return Err(error);
                }
            }
        }
        Err(PackageCacheError::Io {
            operation: "allocate a unique package cache staging directory",
            path: parent.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary directory name collision limit reached",
            ),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn refresh_activity_age(&self) -> Result<(), PackageCacheError> {
        let marker = &self.activity_lock_path;
        let lock = self
            .activity_lock
            .as_ref()
            .ok_or_else(|| PackageCacheError::Io {
                operation: "refresh package cache staging age",
                path: marker.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "staging activity lock was released before publication",
                ),
            })?;
        lock.set_times(FileTimes::new().set_modified(SystemTime::now()))
            .map_err(|source| PackageCacheError::Io {
                operation: "refresh package cache staging age",
                path: marker.clone(),
                source,
            })
    }

    fn release_activity_lock(&mut self, remove_marker: bool) {
        if let Some(lock) = self.activity_lock.take() {
            release_staging_activity_lock(
                &self.activity_lock_path,
                lock,
                remove_marker,
                &self.cache_root,
            );
        }
    }

    fn mark_published(&mut self) {
        self.published = true;
        self.release_activity_lock(true);
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let removed = remove_tree_at_path(&self.path, &self.cache_root);
        self.release_activity_lock(removed);
    }
}

struct CachePublicationLock {
    _file: File,
}

impl CachePublicationLock {
    fn acquire(path: &Path) -> Result<Self, PackageCacheError> {
        let file = open_publication_lock(path)?;
        acquire_platform_lock(&file).map_err(|source| PackageCacheError::Io {
            operation: "lock package cache publication",
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "espidf",
        target_os = "horizon",
        target_os = "solaris",
        target_os = "vita",
        target_os = "wasi"
    ))
))]
fn acquire_platform_lock(file: &File) -> io::Result<()> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::LockExclusive).map_err(Into::into)
}

#[cfg(not(all(
    unix,
    not(any(
        target_os = "espidf",
        target_os = "horizon",
        target_os = "solaris",
        target_os = "vita",
        target_os = "wasi"
    ))
)))]
fn acquire_platform_lock(_file: &File) -> io::Result<()> {
    // Windows takes the lock by opening the file with share_mode(0). On the
    // remaining targets without an advisory-lock API here, the source entry
    // still publishes atomically and cannot replace a non-empty cache entry.
    Ok(())
}

fn make_tree_read_only(root: &Path) -> Result<(), PackageCacheError> {
    let entries = fs::read_dir(root).map_err(|source| PackageCacheError::Io {
        operation: "read package cache staging directory",
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| PackageCacheError::Io {
            operation: "read package cache staging entry",
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| PackageCacheError::Io {
            operation: "inspect package cache staging entry",
            path: path.clone(),
            source,
        })?;
        if metadata.is_dir() {
            make_tree_read_only(&path)?;
            set_read_only_directory_permissions(&path)?;
        } else if metadata.is_file() {
            set_read_only_path_permissions(&path)?;
        } else {
            return Err(PackageCacheError::InvalidSourcePath(
                path.to_string_lossy().into_owned().into_boxed_str(),
            ));
        }
    }
    set_read_only_directory_permissions(root)
}

pub(crate) fn remove_tree_at_path(root: &Path, boundary: &Path) -> bool {
    let Some((parent, name)) = open_parent_at_boundary(root, boundary) else {
        return false;
    };

    let metadata = match parent.symlink_metadata(&name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if metadata.is_dir()
        && !metadata.file_type().is_symlink()
        && !is_windows_directory_reparse_point(&metadata)
    {
        if make_capability_tree_removable(&parent, &name, root).is_err() {
            return false;
        }
        parent.remove_dir_all(&name).is_ok()
    } else if is_windows_directory_reparse_point(&metadata) {
        parent.remove_dir(&name).is_ok()
    } else if metadata.is_file() {
        if let Ok(file) = open_cached_source_file(&parent, &name) {
            if let Ok(opened_metadata) = file.metadata()
                && cached_file_entry_matches(&parent, &name, &file).unwrap_or(false)
                && is_single_link_file(Some(&file), &opened_metadata)
            {
                let _ = set_writable_file_handle(&file, &opened_metadata, root);
            }
        }
        if metadata.is_dir() {
            parent.remove_dir(&name).is_ok()
        } else {
            parent.remove_file(&name).is_ok()
        }
    } else {
        parent.remove_file(&name).is_ok()
    }
}

fn open_parent_at_boundary(root: &Path, boundary: &Path) -> Option<(CapabilityDir, OsString)> {
    let relative = root.strip_prefix(boundary).ok()?;
    let mut components = relative.components().map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let names = components.by_ref().collect::<Option<Vec<_>>>()?;
    let (name, parents) = names.split_last()?;

    // Treat the supplied root as the trust boundary. Every directory below it
    // is opened relative to the preceding capability and checked for symlinks
    // or reparse points before it can become a parent.
    let mut parent = CapabilityDir::open_ambient_dir(boundary, ambient_authority()).ok()?;
    let mut parent_path = boundary.to_path_buf();
    for component in parents {
        parent_path.push(component);
        parent = open_cache_layout_directory(&parent, component, &parent_path).ok()?;
    }

    Some((parent, name.to_os_string()))
}

fn remove_file_at_path(path: &Path, boundary: &Path) -> bool {
    let Some((parent, name)) = open_parent_at_boundary(path, boundary) else {
        return false;
    };
    remove_file_from_parent(&parent, &name)
}

fn remove_file_from_parent(parent: &CapabilityDir, name: &OsStr) -> bool {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if is_windows_directory_reparse_point(&metadata) {
        parent.remove_dir(name).is_ok()
    } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
        false
    } else {
        parent.remove_file(name).is_ok()
    }
}

fn set_private_directory_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_directory_mode(path, 0o700, false)
}

fn set_read_only_directory_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_directory_mode(path, 0o555, true)
}

#[cfg(unix)]
fn prepare_source_directory_rename(path: &Path) -> Result<(), PackageCacheError> {
    // APFS and overlayfs require the moved directory itself to be writable
    // when it is renamed between different parents. The staging parent is
    // private, and the directory is sealed again immediately after publish.
    set_directory_mode(path, 0o700, false)
}

#[cfg(not(unix))]
fn prepare_source_directory_rename(_path: &Path) -> Result<(), PackageCacheError> {
    Ok(())
}

fn seal_published_source_directory(path: &Path) -> Result<(), PackageCacheError> {
    set_read_only_directory_permissions(path)
}

fn is_single_link_file(file: Option<&File>, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        let _ = file;
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() == 1
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        file.and_then(|file| sai_windows_fs::number_of_links(file).ok()) == Some(1)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        let _ = metadata;
        false
    }
}

fn set_read_only_path_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_path_mode(path, 0o444, true)
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), PackageCacheError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        PackageCacheError::Io {
            operation: "set package cache file permissions",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), PackageCacheError> {
    Ok(())
}

#[cfg(unix)]
fn set_read_only_file_permissions(file: &File, path: &Path) -> Result<(), PackageCacheError> {
    set_file_mode(file, path, 0o444)
}

#[cfg(not(unix))]
fn set_read_only_file_permissions(_file: &File, path: &Path) -> Result<(), PackageCacheError> {
    set_path_mode(path, 0, true)
}

#[cfg(unix)]
fn set_file_mode(file: &File, path: &Path, mode: u32) -> Result<(), PackageCacheError> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| PackageCacheError::Io {
            operation: "set package cache file permissions",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: u32, _read_only: bool) -> Result<(), PackageCacheError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        PackageCacheError::Io {
            operation: "set package cache directory permissions",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_directory_mode(path: &Path, _mode: u32, read_only: bool) -> Result<(), PackageCacheError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| PackageCacheError::Io {
            operation: "inspect package cache directory permissions",
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions).map_err(|source| PackageCacheError::Io {
        operation: "set package cache directory permissions",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn set_path_mode(path: &Path, mode: u32, _read_only: bool) -> Result<(), PackageCacheError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        PackageCacheError::Io {
            operation: "set package cache file permissions",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_path_mode(path: &Path, _mode: u32, read_only: bool) -> Result<(), PackageCacheError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| PackageCacheError::Io {
            operation: "inspect package cache file permissions",
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions).map_err(|source| PackageCacheError::Io {
        operation: "set package cache file permissions",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn set_new_directory_mode(builder: &mut fs::DirBuilder) {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
}

#[cfg(not(unix))]
fn set_new_directory_mode(_builder: &mut fs::DirBuilder) {}

#[cfg(unix)]
fn set_new_file_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_new_file_mode(_options: &mut OpenOptions) {}

#[cfg(windows)]
fn set_exclusive_file_sharing(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.share_mode(0);
}

#[cfg(not(windows))]
fn set_exclusive_file_sharing(_options: &mut OpenOptions) {}

/// Resolve the global cache root using only the package subsystem's documented
/// location variables.
pub fn cache_directory_from_environment() -> Result<PathBuf, PackageCacheError> {
    let override_path = std::env::var_os("SAI_CACHE_DIR");
    let xdg_cache_home = std::env::var_os("XDG_CACHE_HOME");
    let home = std::env::var_os("HOME");
    let local_app_data = std::env::var_os("LOCALAPPDATA");
    resolve_cache_directory(
        override_path.as_deref(),
        xdg_cache_home.as_deref(),
        home.as_deref(),
        local_app_data.as_deref(),
        CacheHost::current(),
    )
}

#[allow(dead_code)] // Non-native variants are exercised by location-resolution unit tests.
#[derive(Clone, Copy)]
enum CacheHost {
    MacOs,
    Unix,
    Windows,
    Unsupported,
}

impl CacheHost {
    const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            return Self::MacOs;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            return Self::Unix;
        }
        #[cfg(windows)]
        {
            return Self::Windows;
        }
        #[allow(unreachable_code)]
        Self::Unsupported
    }
}

fn resolve_cache_directory(
    override_path: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
    local_app_data: Option<&OsStr>,
    host: CacheHost,
) -> Result<PathBuf, PackageCacheError> {
    if let Some(value) = nonempty(override_path) {
        let path = PathBuf::from(value);
        if !is_absolute_for_host(&path, host) {
            return Err(PackageCacheError::RelativeCacheDirectory(path));
        }
        return Ok(path);
    }

    if let Some(value) = nonempty(xdg_cache_home) {
        let path = PathBuf::from(value);
        if is_absolute_for_host(&path, host) && matches!(host, CacheHost::Unix) {
            return Ok(path.join("sai"));
        }
    }

    match host {
        CacheHost::MacOs => {
            absolute_home(home, host).map(|path| path.join("Library").join("Caches").join("sai"))
        }
        CacheHost::Unix => absolute_home(home, host).map(|path| path.join(".cache").join("sai")),
        CacheHost::Windows => nonempty(local_app_data)
            .map(PathBuf::from)
            .filter(|path| is_absolute_for_host(path, host))
            .map(|path| path.join("sai").join("cache")),
        CacheHost::Unsupported => None,
    }
    .ok_or(PackageCacheError::CacheDirectoryUnavailable)
}

fn absolute_home(value: Option<&OsStr>, host: CacheHost) -> Option<PathBuf> {
    nonempty(value)
        .map(PathBuf::from)
        .filter(|path| is_absolute_for_host(path, host))
}

fn is_absolute_for_host(path: &Path, host: CacheHost) -> bool {
    path.is_absolute()
        || match host {
            CacheHost::MacOs | CacheHost::Unix => is_unix_absolute(path.as_os_str()),
            CacheHost::Windows => is_windows_absolute(path.as_os_str()),
            CacheHost::Unsupported => false,
        }
}

#[cfg(unix)]
fn is_unix_absolute(path: &OsStr) -> bool {
    Path::new(path).is_absolute()
}

#[cfg(not(unix))]
fn is_unix_absolute(path: &OsStr) -> bool {
    path.to_str().is_some_and(|path| path.starts_with('/'))
}

#[cfg(windows)]
fn is_windows_absolute(path: &OsStr) -> bool {
    Path::new(path).is_absolute()
}

#[cfg(not(windows))]
fn is_windows_absolute(path: &OsStr) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with("\\\\")
}

fn nonempty(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        CacheDisposition, CacheHost, CachedSourceVerification, PackageCache, PackageCacheError,
        SourceCacheRemoval, resolve_cache_directory,
    };
    use crate::git_tree::GitTreeReader;
    use gix::objs::tree::{Entry, EntryKind};
    use std::fs;
    #[cfg(windows)]
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-package-cache-{name}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let Some(boundary) = self.0.parent() else {
                return;
            };
            let _ = super::remove_tree_at_path(&self.0, boundary);
        }
    }

    struct TestRepository {
        path: PathBuf,
        repository: gix::Repository,
    }

    impl TestRepository {
        fn new() -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-package-cache-git-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            let repository =
                gix::ThreadSafeRepository::init(&path, gix::create::Kind::Bare, Default::default())
                    .unwrap()
                    .into();
            Self { path, repository }
        }

        fn verified_source(&self) -> crate::git_tree::GitPackageSourceTree {
            let manifest = self
                .repository
                .write_blob(b"package acme:sample @ 1.0.0\n")
                .unwrap()
                .detach();
            let touch = self
                .repository
                .write_blob(b"package-owned staging-name sentinel\n")
                .unwrap()
                .detach();
            let source = self
                .repository
                .write_blob(b"pub fn answer() -> i32 { 42 }\n")
                .unwrap()
                .detach();
            let tree = self
                .repository
                .write_object(gix::objs::Tree {
                    entries: vec![
                        Entry {
                            mode: EntryKind::Blob.into(),
                            filename: b".sai-staging-touch".to_vec().into(),
                            oid: touch,
                        },
                        Entry {
                            mode: EntryKind::Blob.into(),
                            filename: b"main.sai".to_vec().into(),
                            oid: source,
                        },
                        Entry {
                            mode: EntryKind::Blob.into(),
                            filename: b"sai.package".to_vec().into(),
                            oid: manifest,
                        },
                    ],
                })
                .unwrap()
                .detach();
            let commit = self
                .repository
                .write_object(gix::objs::Commit {
                    tree,
                    parents: Default::default(),
                    author: gix::actor::Signature::default(),
                    committer: gix::actor::Signature::default(),
                    encoding: None,
                    message: "cache fixture".into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            GitTreeReader::open(self.path.join("objects"), self.repository.object_hash())
                .unwrap()
                .read_commit_package_source_tree(&commit, b"")
                .unwrap()
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn creates_the_documented_layout_and_rejects_relative_roots() {
        let relative = PackageCache::open("relative-cache").unwrap_err();
        assert!(matches!(
            relative,
            PackageCacheError::RelativeCacheDirectory(_)
        ));

        let temp = TestDirectory::new("layout");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        assert!(cache.versioned_root().join("git").is_dir());
        assert!(cache.versioned_root().join("src").is_dir());
        assert!(cache.versioned_root().join("tags").is_dir());
        assert!(cache.temporary_directory().is_dir());
        assert!(cache.versioned_root().join("lock").is_file());
    }

    #[test]
    fn repository_keys_and_directories_follow_the_cache_specification() {
        let temp = TestDirectory::new("repository-key");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let url = "https://example.com/acme/repo";

        assert_eq!(
            PackageCache::repository_key(url).unwrap(),
            "fb7118253e93c2fc4455ee42d54bf19dd7fff74426e8e6b8c24b9b731dc530cc"
        );
        assert_eq!(
            cache.git_repository_directory(url).unwrap(),
            cache
                .versioned_root()
                .join("git")
                .join("fb7118253e93c2fc4455ee42d54bf19dd7fff74426e8e6b8c24b9b731dc530cc")
        );
        assert!(!cache.git_repository_directory(url).unwrap().exists());
        assert!(matches!(
            PackageCache::repository_key("HTTPS://EXAMPLE.COM/acme/repo"),
            Err(PackageCacheError::InvalidGitUrl)
        ));
    }

    #[test]
    fn persists_release_tag_and_source_records_across_cache_handles() {
        let temp = TestDirectory::new("records");
        let root = temp.path().join("cache");
        let cache = PackageCache::open(&root).unwrap();
        let url = "https://example.com/acme/repo";
        let tag = "refs/tags/packages/json/v1.2.3";
        let commit = "a".repeat(40);
        let checksum = format!("sai1:{}", "b".repeat(64));

        assert_eq!(cache.cached_git_tag_commit(url, tag).unwrap(), None);
        cache.record_git_tag(url, tag, &commit).unwrap();
        assert_eq!(
            cache.cached_git_tag_commit(url, tag).unwrap(),
            Some(commit.clone())
        );

        cache
            .record_git_source_checksum(url, &commit, Some("packages/json"), &checksum)
            .unwrap();
        let reopened = PackageCache::open(&root).unwrap();
        assert_eq!(
            reopened.cached_git_tag_commit(url, tag).unwrap().as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(
            reopened
                .cached_git_source_checksum(url, &commit, Some("packages/json"))
                .unwrap(),
            Some(checksum)
        );
        assert_eq!(
            reopened
                .cached_git_source_checksum(url, &commit, None)
                .unwrap(),
            None
        );
        assert_eq!(
            reopened
                .cached_git_source_checksum(url, &commit, Some("."))
                .unwrap(),
            None
        );
        assert_eq!(
            fs::read_dir(cache.temporary_directory()).unwrap().count(),
            0
        );
    }

    #[test]
    fn release_tag_updates_allow_moved_tags_but_source_records_are_immutable() {
        let temp = TestDirectory::new("record-updates");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let url = "ssh://git@example.com/acme/repo";
        let tag = "refs/tags/v1.2.3";
        let first_commit = "a".repeat(40);
        let next_commit = "c".repeat(64);
        let first_checksum = format!("sai1:{}", "b".repeat(64));
        let next_checksum = format!("sai1:{}", "d".repeat(64));

        cache.record_git_tag(url, tag, &first_commit).unwrap();
        cache.record_git_tag(url, tag, &next_commit).unwrap();
        assert_eq!(
            cache.cached_git_tag_commit(url, tag).unwrap(),
            Some(next_commit.clone())
        );

        cache
            .record_git_source_checksum(url, &first_commit, None, &first_checksum)
            .unwrap();
        assert!(matches!(
            cache.record_git_source_checksum(url, &first_commit, Some("."), &next_checksum),
            Err(PackageCacheError::ConflictingGitSourceRecord(_))
        ));
        assert_eq!(
            cache
                .cached_git_source_checksum(url, &first_commit, None)
                .unwrap(),
            Some(first_checksum)
        );
    }

    #[test]
    fn validates_cache_record_inputs_and_rejects_corruption() {
        let temp = TestDirectory::new("record-corruption");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let url = "https://example.com/acme/repo";
        let tag = "refs/tags/v1.2.3";
        let commit = "a".repeat(40);
        assert!(matches!(
            cache.record_git_tag(url, "refs/tags/v1..2", &commit),
            Err(PackageCacheError::InvalidGitTag)
        ));
        assert!(matches!(
            cache.record_git_tag(url, tag, &commit.to_ascii_uppercase()),
            Err(PackageCacheError::InvalidGitCommit)
        ));
        assert!(matches!(
            cache.record_git_source_checksum(
                url,
                &commit,
                Some("../outside"),
                &format!("sai1:{}", "b".repeat(64))
            ),
            Err(PackageCacheError::InvalidPackageDirectory)
        ));
        assert!(matches!(
            cache.record_git_source_checksum(url, &commit, None, "sai1:bad"),
            Err(PackageCacheError::InvalidSourceChecksum(_))
        ));

        cache.record_git_tag(url, tag, &commit).unwrap();
        let key = super::cache_record_key(b"tag", &[url.as_bytes(), tag.as_bytes()]);
        let record = super::cache_record_slot_path(
            &cache.tag_directory.join(format!("tag-{key}.record")),
            0,
        );
        fs::write(&record, b"truncated record").unwrap();
        assert!(matches!(
            cache.cached_git_tag_commit(url, tag),
            Err(PackageCacheError::CorruptRecord(_))
        ));
    }

    #[test]
    fn readers_keep_the_latest_record_while_the_inactive_slot_is_missing() {
        let temp = TestDirectory::new("record-slot-gap");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let url = "https://example.com/acme/repo";
        let tag = "refs/tags/v1.2.3";
        let first = "a".repeat(40);
        let second = "b".repeat(40);
        let third = "c".repeat(40);

        cache.record_git_tag(url, tag, &first).unwrap();
        cache.record_git_tag(url, tag, &second).unwrap();
        let key = super::cache_record_key(b"tag", &[url.as_bytes(), tag.as_bytes()]);
        let base = cache.tag_directory.join(format!("tag-{key}.record"));
        let older_slot = super::cache_record_slot_path(&base, 0);
        fs::remove_file(older_slot).unwrap();

        assert_eq!(
            cache.cached_git_tag_commit(url, tag).unwrap(),
            Some(second.clone())
        );
        cache.record_git_tag(url, tag, &third).unwrap();
        assert_eq!(cache.cached_git_tag_commit(url, tag).unwrap(), Some(third));
    }

    #[test]
    fn cache_record_reads_recover_only_from_a_verifiably_older_corrupt_slot() {
        let url = "https://example.com/acme/repo";
        let tag = "refs/tags/v1.2.3";

        let stale_temp = TestDirectory::new("stale-corrupt-record");
        let stale_cache = PackageCache::open(stale_temp.path().join("cache")).unwrap();
        let first = "a".repeat(40);
        let second = "b".repeat(40);
        stale_cache.record_git_tag(url, tag, &first).unwrap();
        stale_cache.record_git_tag(url, tag, &second).unwrap();
        let key = super::cache_record_key(b"tag", &[url.as_bytes(), tag.as_bytes()]);
        let base = stale_cache.tag_directory.join(format!("tag-{key}.record"));
        let stale_slot = super::cache_record_slot_path(&base, 0);
        let mut stale_bytes = fs::read(&stale_slot).unwrap();
        *stale_bytes.last_mut().unwrap() ^= 1;
        fs::write(&stale_slot, stale_bytes).unwrap();
        assert_eq!(
            stale_cache.cached_git_tag_commit(url, tag).unwrap(),
            Some(second.clone())
        );
        stale_cache
            .record_git_tag(url, tag, &"c".repeat(40))
            .unwrap();

        let newest_temp = TestDirectory::new("newest-corrupt-record");
        let newest_cache = PackageCache::open(newest_temp.path().join("cache")).unwrap();
        newest_cache.record_git_tag(url, tag, &first).unwrap();
        newest_cache.record_git_tag(url, tag, &second).unwrap();
        let newest_base = newest_cache.tag_directory.join(format!("tag-{key}.record"));
        let newest_slot = super::cache_record_slot_path(&newest_base, 1);
        let mut newest_bytes = fs::read(&newest_slot).unwrap();
        *newest_bytes.last_mut().unwrap() ^= 1;
        fs::write(&newest_slot, newest_bytes).unwrap();
        assert!(matches!(
            newest_cache.cached_git_tag_commit(url, tag),
            Err(PackageCacheError::CorruptRecord(_))
        ));
    }

    #[test]
    fn concurrent_tag_record_updates_publish_complete_values() {
        use std::sync::{Arc, Barrier};

        const WORKERS: usize = 8;
        let temp = TestDirectory::new("concurrent-records");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let barrier = Arc::new(Barrier::new(WORKERS));
        let commits = (0..WORKERS)
            .map(|index| format!("{index:040x}"))
            .collect::<Vec<_>>();
        let workers = commits
            .iter()
            .map(|commit| {
                let cache = cache.clone();
                let barrier = Arc::clone(&barrier);
                let commit = commit.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    cache
                        .record_git_tag(
                            "https://example.com/acme/repo",
                            "refs/tags/v1.2.3",
                            &commit,
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }

        let recorded = cache
            .cached_git_tag_commit("https://example.com/acme/repo", "refs/tags/v1.2.3")
            .unwrap()
            .unwrap();
        assert!(commits.contains(&recorded));
    }

    #[test]
    fn concurrent_readers_keep_tag_records_available_during_updates() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier};

        const READERS: usize = 3;
        const UPDATES: usize = 500;
        let temp = TestDirectory::new("record-readers");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let url = "https://example.com/acme/repo";
        let tag = "refs/tags/v1.2.3";
        cache.record_git_tag(url, tag, &"0".repeat(40)).unwrap();

        let start = Arc::new(Barrier::new(READERS + 1));
        let done = Arc::new(AtomicBool::new(false));
        let readers = (0..READERS)
            .map(|_| {
                let cache = cache.clone();
                let start = Arc::clone(&start);
                let done = Arc::clone(&done);
                std::thread::spawn(move || {
                    start.wait();
                    loop {
                        assert!(cache.cached_git_tag_commit(url, tag).unwrap().is_some());
                        if done.load(Ordering::Acquire) {
                            break;
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        for index in 1..=UPDATES {
            cache
                .record_git_tag(url, tag, &format!("{index:040x}"))
                .unwrap();
        }
        done.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
    }

    #[test]
    fn concurrent_initialization_creates_one_valid_layout() {
        use std::sync::{Arc, Barrier};

        const WORKERS: usize = 8;
        let temp = TestDirectory::new("concurrent-open");
        let cache_root = temp.path().join("cache");
        let barrier = Arc::new(Barrier::new(WORKERS));
        let workers = (0..WORKERS)
            .map(|_| {
                let cache_root = cache_root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    PackageCache::open(cache_root).unwrap()
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            let cache = worker.join().unwrap();
            assert!(cache.versioned_root().join("git").is_dir());
            assert!(cache.versioned_root().join("src").is_dir());
            assert!(cache.versioned_root().join("tags").is_dir());
            assert!(cache.temporary_directory().is_dir());
            assert!(cache.versioned_root().join("lock").is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn stale_unlocked_staging_directories_are_removed_by_the_next_writer() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("stale-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache.temporary_directory.join("source-abandoned-0");
        let activity_lock = super::staging_activity_lock_path(&staging);
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"incomplete").unwrap();
        fs::write(&activity_lock, b"").unwrap();
        super::make_tree_read_only(&staging).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        for path in [&staging, &activity_lock] {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }

        let source = TestRepository::new().verified_source();
        cache.materialize_source(source).unwrap();

        assert!(!staging.exists());
        assert!(!activity_lock.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_git_temporary_repositories_are_removed_by_the_next_writer() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("stale-git-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache
            .temporary_directory
            .join(format!("sai-git-remote-{}-0-deadbeef", i32::MAX));
        let activity_lock = super::staging_activity_lock_path(&staging);
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("objects"), b"incomplete Git scratch").unwrap();
        fs::write(&activity_lock, b"").unwrap();
        super::make_tree_read_only(&staging).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        for path in [&staging, &activity_lock] {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }

        let source = TestRepository::new().verified_source();
        cache.materialize_source(source).unwrap();

        assert!(!staging.exists());
        assert!(!activity_lock.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_staging_with_a_recent_activity_marker_is_preserved() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("recent-marker-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache.temporary_directory.join("source-abandoned-recent");
        let activity_lock = super::staging_activity_lock_path(&staging);
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"incomplete").unwrap();
        fs::write(&activity_lock, b"").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        File::open(&staging)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old_time))
            .unwrap();
        File::open(&activity_lock)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now()))
            .unwrap();

        cache.cleanup_stale_temporary_directories();

        assert!(staging.is_dir());
        assert!(activity_lock.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn stale_legacy_staging_directories_with_dead_writers_are_removed() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("legacy-stale-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache
            .temporary_directory
            .join(format!("source-{}-0", i32::MAX));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"incomplete legacy staging").unwrap();
        super::make_tree_read_only(&staging).unwrap();
        File::open(&staging)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60)),
            )
            .unwrap();

        let source = TestRepository::new().verified_source();
        cache.materialize_source(source).unwrap();

        assert!(!staging.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_markerless_staging_from_a_live_legacy_writer_is_preserved() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("live-legacy-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache
            .temporary_directory
            .join(format!("source-{}-0", std::process::id()));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"incomplete legacy staging").unwrap();
        super::make_tree_read_only(&staging).unwrap();
        File::open(&staging)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60)),
            )
            .unwrap();

        let source = TestRepository::new().verified_source();
        cache.materialize_source(source).unwrap();

        assert!(staging.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_staging_cleanup_skips_a_directory_claimed_by_another_cleaner() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("claimed-legacy-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging = cache
            .temporary_directory
            .join(format!("source-{}-claimed", i32::MAX));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"incomplete legacy staging").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        File::open(&staging)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old_time))
            .unwrap();

        let activity_path = super::staging_activity_lock_path(&staging);
        let activity_lock =
            super::create_staging_activity_lock(&activity_path, &cache.root).unwrap();
        cache.cleanup_stale_temporary_directories();

        assert!(staging.is_dir());
        drop(activity_lock);
        fs::remove_file(&activity_path).unwrap();
        cache.cleanup_stale_temporary_directories();
        assert!(!staging.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_staging_directory_with_a_live_activity_lock_is_preserved() {
        use std::fs::{File, FileTimes};
        use std::time::{Duration, SystemTime};

        let temp = TestDirectory::new("active-stale-temp");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let staging =
            super::StagingDirectory::create(&cache.temporary_directory, &cache.root).unwrap();
        let staging_path = staging.path().to_path_buf();
        let activity_path = staging.activity_lock_path.clone();
        let old_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        for path in [&staging_path, &activity_path] {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }

        cache.cleanup_stale_temporary_directories();

        assert!(staging_path.is_dir());
        drop(staging);
        assert!(!staging_path.exists());
        assert!(!activity_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_in_managed_cache_directories() {
        use std::os::unix::fs::symlink;

        let temp = TestDirectory::new("symlink");
        let cache_root = temp.path().join("cache");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let versioned = cache_root.join("v1");
        fs::create_dir_all(&versioned).unwrap();
        fs::create_dir(versioned.join("git")).unwrap();
        fs::create_dir(versioned.join("src")).unwrap();
        fs::create_dir(versioned.join("tags")).unwrap();
        symlink(&outside, versioned.join("tmp")).unwrap();

        assert!(matches!(
            PackageCache::open(&cache_root),
            Err(PackageCacheError::Io { .. })
        ));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_as_the_cache_lock_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("lock-symlink");
        let cache_root = temp.path().join("cache");
        let versioned = cache_root.join("v1");
        fs::create_dir_all(versioned.join("git")).unwrap();
        fs::create_dir(versioned.join("src")).unwrap();
        fs::create_dir(versioned.join("tags")).unwrap();
        fs::create_dir(versioned.join("tmp")).unwrap();
        let outside_file = temp.path().join("outside-lock-target");
        fs::write(&outside_file, b"do not chmod this file").unwrap();
        fs::set_permissions(&outside_file, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&outside_file, versioned.join("lock")).unwrap();

        assert!(matches!(
            PackageCache::open(&cache_root),
            Err(PackageCacheError::Io { .. })
        ));
        assert_eq!(
            fs::metadata(&outside_file).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn materializes_only_verified_files_under_the_checksum_and_makes_them_read_only() {
        let temp = TestDirectory::new("materialize");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let expected_path = cache.source_path(&checksum).unwrap();

        let installed = cache.materialize_source(source).unwrap();

        assert_eq!(installed.path(), expected_path);
        assert_eq!(installed.checksum(), checksum);
        assert_eq!(installed.disposition(), CacheDisposition::Installed);
        assert_eq!(
            fs::read(expected_path.join("sai.package")).unwrap(),
            b"package acme:sample @ 1.0.0\n"
        );
        assert_eq!(
            fs::read(expected_path.join("main.sai")).unwrap(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
        assert_eq!(
            fs::read(expected_path.join(".sai-staging-touch")).unwrap(),
            b"package-owned staging-name sentinel\n"
        );
        assert_eq!(
            fs::read_dir(cache.temporary_directory()).unwrap().count(),
            0
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(expected_path.join("sai.package"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
            assert_eq!(
                fs::metadata(&expected_path).unwrap().permissions().mode() & 0o777,
                0o555
            );
        }
    }

    #[test]
    fn verified_package_checksums_are_bound_to_url_commit_and_directory() {
        let temp = TestDirectory::new("package-checksum-record");
        let cache_root = temp.path().join("cache");
        let cache = PackageCache::open(&cache_root).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let url = "https://example.com/acme/repo";
        let commit = "a".repeat(40);

        cache
            .record_verified_package_checksums(url, &commit, Some("out/sample"), &source)
            .unwrap();

        cache.materialize_source(source).unwrap();

        let metadata = cache
            .cached_package_checksums(url, &commit, Some("out/sample"))
            .unwrap()
            .unwrap();
        assert_eq!(metadata.source_checksum.as_ref(), checksum.as_str());
        assert_eq!(metadata.wit_checksum, None);
        assert!(metadata.artifact_checksums.is_empty());
        assert!(
            cache
                .cached_package_checksums(url, &"b".repeat(40), Some("out/sample"))
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .cached_package_checksums(url, &commit, None)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .cached_package_checksums(url, &commit, Some("packages/sample"))
                .unwrap()
                .is_none()
        );
        let reopened = PackageCache::open(&cache_root).unwrap();
        assert_eq!(
            reopened
                .cached_package_checksums(url, &commit, Some("out/sample"))
                .unwrap()
                .unwrap()
                .source_checksum
                .as_ref(),
            checksum.as_str()
        );
    }

    #[test]
    fn verifies_a_cached_tree_by_streaming_its_contents() {
        let temp = TestDirectory::new("verify-source");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let installed = cache.materialize_source(source).unwrap();

        let verification = cache.verify_source(&checksum).unwrap();

        let CachedSourceVerification::Verified(verified) = verification else {
            panic!("expected a verified cache entry, got {verification:?}");
        };
        assert_eq!(verified.checksum(), checksum);
        assert_eq!(verified.path(), installed.path());
        assert_eq!(verified.disposition(), CacheDisposition::AlreadyPresent);
    }

    #[test]
    fn cached_file_identity_compares_open_file_handles() {
        use std::fs::File;

        let temp = TestDirectory::new("cached-file-identity");
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        fs::write(&first_path, b"same bytes").unwrap();
        fs::write(&second_path, b"same bytes").unwrap();
        let first = File::open(first_path).unwrap();
        let same_file = first.try_clone().unwrap();
        let second = File::open(second_path).unwrap();

        assert!(super::same_cached_file(&first, &same_file).unwrap());
        assert!(!super::same_cached_file(&first, &second).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn same_size_rewrite_with_restored_mtime_is_detected_by_change_time() {
        use std::fs::{FileTimes, OpenOptions};
        use std::io::{Seek, SeekFrom, Write};

        let temp = TestDirectory::new("verify-source-change-time");
        let path = temp.path().join("cached-file");
        fs::write(&path, b"before").unwrap();
        let before = fs::metadata(&path).unwrap();
        let original_modified = before.modified().unwrap();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"after!").unwrap();
        file.set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        let after = file.metadata().unwrap();

        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert!(!super::cached_file_metadata_unchanged(&before, &after));
    }

    #[cfg(unix)]
    #[test]
    fn nofollow_permission_fallback_cannot_change_a_symlink_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("verify-source-chmodat-nofollow");
        let parent_path = temp.path().join("cache");
        fs::create_dir(&parent_path).unwrap();
        let parent =
            super::CapabilityDir::open_ambient_dir(&parent_path, super::ambient_authority())
                .unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        let link = parent_path.join("entry");
        symlink(&outside, &link).unwrap();

        let _result = super::set_capability_directory_writable_nofollow(
            &parent,
            super::OsStr::new("entry"),
            &link,
        );

        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn cached_source_file_open_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TestDirectory::new("verify-source-open-nofollow");
        let parent_path = temp.path().join("cache");
        fs::create_dir(&parent_path).unwrap();
        let parent =
            super::CapabilityDir::open_ambient_dir(&parent_path, super::ambient_authority())
                .unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, parent_path.join("entry")).unwrap();

        assert!(super::open_cached_source_file(&parent, super::OsStr::new("entry")).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[cfg(windows)]
    #[test]
    fn windows_cached_readonly_file_with_external_hardlink_is_not_immutable() {
        let temp = TestDirectory::new("verify-source-windows-hardlink");
        let cache_file = temp.path().join("cached-file");
        let external_file = temp.path().join("external-file");
        fs::write(&cache_file, b"cached content").unwrap();
        let mut permissions = fs::metadata(&cache_file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&cache_file, permissions).unwrap();
        fs::hard_link(&cache_file, &external_file).unwrap();
        let cache_handle = File::open(&cache_file).unwrap();
        let external_handle = File::open(&external_file).unwrap();

        assert!(!super::is_read_only_file(
            Some(&cache_handle),
            &fs::metadata(&cache_file).unwrap()
        ));
        assert!(!super::is_single_link_file(
            Some(&external_handle),
            &fs::metadata(&external_file).unwrap()
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_source_removal_retries_transient_sharing_violations() {
        use std::cell::Cell;

        let attempts = Cell::new(0);
        let result = super::retry_cache_entry_removal(|| {
            let current_attempt = attempts.get() + 1;
            attempts.set(current_attempt);
            if current_attempt < 3 {
                Err(PackageCacheError::Io {
                    operation: "test removal retry",
                    path: PathBuf::from("cache-entry"),
                    source: std::io::Error::from_raw_os_error(
                        super::WINDOWS_ERROR_SHARING_VIOLATION,
                    ),
                })
            } else {
                Ok(())
            }
        });

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);
    }

    #[cfg(windows)]
    #[test]
    fn corrupt_source_removal_retries_while_a_reader_holds_a_directory_handle() {
        use std::time::Duration;

        let temp = TestDirectory::new("verify-source-windows-reader-removal");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let source_path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let source_file = source_path.join("main.sai");
        let mut permissions = fs::metadata(&source_file).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&source_file, permissions).unwrap();
        fs::write(&source_file, b"corrupt package content").unwrap();
        let mut permissions = fs::metadata(&source_file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&source_file, permissions).unwrap();

        let reader =
            super::CapabilityDir::open_ambient_dir(&source_path, super::ambient_authority())
                .unwrap();
        let cache_root = cache.root.clone();
        let worker_checksum = checksum.clone();
        let worker = std::thread::spawn(move || {
            PackageCache::open(cache_root)
                .unwrap()
                .remove_corrupt_source(&worker_checksum)
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(reader);

        assert_eq!(worker.join().unwrap().unwrap(), SourceCacheRemoval::Removed);
    }

    #[test]
    fn reports_a_missing_cached_tree_without_creating_it() {
        let temp = TestDirectory::new("verify-source-missing");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let checksum = crate::tree_hash::artifact_checksum(b"missing");

        assert_eq!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Missing
        );
        assert!(!cache.source_path(&checksum).unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn reports_the_actual_checksum_when_cached_file_contents_change() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("verify-source-content");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let source_file = path.join("main.sai");
        fs::set_permissions(&source_file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&source_file, b"pub fn answer() -> i32 { 43 }\n").unwrap();
        fs::set_permissions(&source_file, fs::Permissions::from_mode(0o444)).unwrap();

        let verification = cache.verify_source(&checksum).unwrap();

        let CachedSourceVerification::Corrupt {
            expected_checksum,
            actual_checksum: Some(actual_checksum),
        } = verification
        else {
            panic!("expected a content checksum mismatch, got {verification:?}");
        };
        assert_eq!(&*expected_checksum, checksum);
        assert_ne!(&*actual_checksum, checksum.as_str());
        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Removed
        );
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_extra_excluded_paths_even_when_the_source_hash_would_ignore_them() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("verify-source-excluded");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(path.join(".git")).unwrap();
        fs::set_permissions(path.join(".git"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: None,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_cached_files_that_are_executable_even_when_the_hash_matches() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("verify-source-executable");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let source_file = path.join("main.sai");
        fs::set_permissions(&source_file, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: Some(actual),
                ..
            } if actual.as_ref() == checksum
        ));
    }

    #[cfg(unix)]
    #[test]
    fn classifies_an_unreadable_cached_directory_as_corrupt_and_removes_it() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("verify-source-unreadable-directory");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let unreadable = path.join("private");
        fs::create_dir(&unreadable).unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: None,
                ..
            }
        ));
        match cache.remove_corrupt_source(&checksum) {
            Ok(SourceCacheRemoval::Removed) => {}
            Err(PackageCacheError::Io { source, .. })
                if source
                    .raw_os_error()
                    .is_some_and(|code| code == libc::ENOTSUP || code == libc::EOPNOTSUPP) =>
            {
                // Some Unix systems do not implement no-follow chmodat.
                // Restore access so the capability-handle path can remove it.
                fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
                assert_eq!(
                    cache.remove_corrupt_source(&checksum).unwrap(),
                    SourceCacheRemoval::Removed
                );
            }
            other => panic!("unexpected corrupt-cache removal result: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_replaced_source_cache_parent_directory() {
        use std::os::unix::fs::symlink;

        let temp = TestDirectory::new("verify-source-parent-symlink");
        let cache_root = temp.path().join("cache");
        let cache = PackageCache::open(&cache_root).unwrap();
        let source_directory = cache.versioned_root().join("src");
        let moved_source_directory = cache.versioned_root().join("src-original");
        fs::rename(&source_directory, &moved_source_directory).unwrap();
        symlink(&moved_source_directory, &source_directory).unwrap();
        let checksum = crate::tree_hash::artifact_checksum(b"missing");

        assert!(matches!(
            cache.verify_source(&checksum),
            Err(PackageCacheError::Io { .. })
        ));
    }

    #[test]
    fn corrupt_source_removal_preserves_valid_and_missing_entries() {
        let temp = TestDirectory::new("remove-corrupt-source");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        cache.materialize_source(source).unwrap();

        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Valid
        );
        assert_eq!(
            cache
                .remove_corrupt_source(&crate::tree_hash::artifact_checksum(b"missing"))
                .unwrap(),
            SourceCacheRemoval::Missing
        );
        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Verified(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cache_verification_rejects_symlinks_and_removal_does_not_follow_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("verify-source-symlink");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let outside_file = temp.path().join("outside");
        fs::write(&outside_file, b"keep this file").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&outside_file, path.join("escape")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: None,
                ..
            }
        ));
        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Removed
        );
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep this file");
    }

    #[cfg(unix)]
    #[test]
    fn cache_verification_does_not_traverse_a_symlinked_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("verify-source-symlink-directory");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let outside_directory = temp.path().join("outside");
        fs::create_dir(&outside_directory).unwrap();
        fs::write(outside_directory.join("secret"), b"outside the cache").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&outside_directory, path.join("vendor")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: None,
                ..
            }
        ));
        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Removed
        );
        assert_eq!(
            fs::read(outside_directory.join("secret")).unwrap(),
            b"outside the cache"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_verification_rejects_hard_links_and_removal_preserves_the_external_link() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = TestDirectory::new("verify-source-hardlink");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let path = cache
            .materialize_source(source)
            .unwrap()
            .path()
            .to_path_buf();
        let cached_file = path.join("main.sai");
        let outside_file = temp.path().join("outside");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::hard_link(&cached_file, &outside_file).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(matches!(
            cache.verify_source(&checksum).unwrap(),
            CachedSourceVerification::Corrupt {
                actual_checksum: Some(_),
                ..
            }
        ));
        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Removed
        );
        assert_eq!(
            fs::read(&outside_file).unwrap(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
        assert_eq!(fs::metadata(&outside_file).unwrap().nlink(), 1);
    }

    #[test]
    fn an_existing_entry_is_reused_without_replacing_its_contents() {
        let temp = TestDirectory::new("existing");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let first = TestRepository::new().verified_source();
        let checksum = first.source_tree().checksum.clone();
        cache.materialize_source(first).unwrap();

        let second = TestRepository::new().verified_source();
        let existing = cache.materialize_source(second).unwrap();

        assert_eq!(existing.disposition(), CacheDisposition::AlreadyPresent);
        assert_eq!(
            fs::read(existing.path().join("main.sai")).unwrap(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
        assert_eq!(existing.checksum(), checksum);
        assert_eq!(
            fs::read_dir(cache.temporary_directory()).unwrap().count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_writable_existing_entry_is_not_accepted_as_immutable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("repair-permissions");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let first = TestRepository::new().verified_source();
        let path = cache
            .materialize_source(first)
            .unwrap()
            .path()
            .to_path_buf();
        fs::set_permissions(path.join("main.sai"), fs::Permissions::from_mode(0o644)).unwrap();

        let second = TestRepository::new().verified_source();
        let repaired = cache.materialize_source(second).unwrap();

        assert_eq!(repaired.disposition(), CacheDisposition::Installed);
        assert_eq!(
            fs::metadata(path.join("main.sai"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
    }

    #[test]
    fn a_mismatched_existing_entry_is_removed_and_replaced_from_the_verified_snapshot() {
        let temp = TestDirectory::new("repair-corrupt");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let existing_path = cache.source_path(&checksum).unwrap();
        fs::create_dir_all(&existing_path).unwrap();
        fs::write(existing_path.join("sai.package"), b"wrong manifest\n").unwrap();
        fs::write(existing_path.join("unexpected"), b"unverified").unwrap();

        let installed = cache.materialize_source(source).unwrap();

        assert_eq!(installed.disposition(), CacheDisposition::Installed);
        assert_eq!(
            fs::read(existing_path.join("sai.package")).unwrap(),
            b"package acme:sample @ 1.0.0\n"
        );
        assert!(!existing_path.join("unexpected").exists());
    }

    #[test]
    fn a_read_only_file_at_a_source_address_is_removed_and_replaced() {
        let temp = TestDirectory::new("repair-readonly-file");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let existing_path = cache.source_path(&source.source_tree().checksum).unwrap();
        fs::write(&existing_path, b"not a directory").unwrap();
        super::set_read_only_path_permissions(&existing_path).unwrap();

        let installed = cache.materialize_source(source).unwrap();

        assert_eq!(installed.disposition(), CacheDisposition::Installed);
        assert!(existing_path.is_dir());
        assert_eq!(
            fs::read(existing_path.join("sai.package")).unwrap(),
            b"package acme:sample @ 1.0.0\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_hard_linked_file_entry_preserves_the_external_file_mode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = TestDirectory::new("repair-hardlinked-file-entry");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let cache_entry = cache.source_path(&checksum).unwrap();
        let external_file = temp.path().join("external-file");
        fs::write(&cache_entry, b"wrong cache entry").unwrap();
        super::set_read_only_path_permissions(&cache_entry).unwrap();
        fs::hard_link(&cache_entry, &external_file).unwrap();

        let installed = cache.materialize_source(source).unwrap();

        assert_eq!(installed.disposition(), CacheDisposition::Installed);
        assert!(cache_entry.is_dir());
        assert_eq!(fs::read(&external_file).unwrap(), b"wrong cache entry");
        let metadata = fs::metadata(&external_file).unwrap();
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o444);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_a_source_address_is_removed_without_following_it() {
        use std::os::unix::fs::symlink;

        let temp = TestDirectory::new("repair-symlink");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let existing_path = cache.source_path(&checksum).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"keep").unwrap();
        symlink(&outside, &existing_path).unwrap();

        let installed = cache.materialize_source(source).unwrap();

        assert_eq!(installed.disposition(), CacheDisposition::Installed);
        assert!(
            !fs::symlink_metadata(&existing_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_symlink_at_a_source_address_is_removed_without_following_it() {
        use std::os::windows::fs::symlink_dir;

        let temp = TestDirectory::new("repair-directory-symlink");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let source = TestRepository::new().verified_source();
        let checksum = source.source_tree().checksum.clone();
        let existing_path = cache.source_path(&checksum).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"keep").unwrap();
        match symlink_dir(&outside, &existing_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("could not create directory symlink for test: {error}"),
        }

        assert_eq!(
            cache.remove_corrupt_source(&checksum).unwrap(),
            SourceCacheRemoval::Removed
        );
        assert!(
            fs::symlink_metadata(&existing_path).unwrap_err().kind()
                == std::io::ErrorKind::NotFound
        );
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_cached_file_is_not_reused_or_made_writable() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = TestDirectory::new("repair-hardlink");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let first = TestRepository::new().verified_source();
        let cached = cache.materialize_source(first).unwrap();
        let cached_file = cached.path().join("main.sai");
        let external_file = temp.path().join("outside-main.sai");
        fs::hard_link(&cached_file, &external_file).unwrap();

        let second = TestRepository::new().verified_source();
        let repaired = cache.materialize_source(second).unwrap();

        assert_eq!(repaired.disposition(), CacheDisposition::Installed);
        assert_ne!(fs::metadata(&external_file).unwrap().nlink(), 2);
        assert_eq!(
            fs::metadata(&external_file).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(
            fs::read(repaired.path().join("main.sai")).unwrap(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
    }

    #[test]
    fn concurrent_writers_publish_one_complete_same_digest_entry() {
        let temp = TestDirectory::new("concurrent");
        let cache_root = temp.path().join("cache");
        let source_a = TestRepository::new().verified_source();
        let source_b = TestRepository::new().verified_source();
        let cache_a = PackageCache::open(&cache_root).unwrap();
        let cache_b = PackageCache::open(&cache_root).unwrap();
        let checksum = source_a.source_tree().checksum.clone();
        assert_eq!(source_b.source_tree().checksum, checksum);

        let writer_a = std::thread::spawn(move || cache_a.materialize_source(source_a).unwrap());
        let writer_b = std::thread::spawn(move || cache_b.materialize_source(source_b).unwrap());
        let first = writer_a.join().unwrap();
        let second = writer_b.join().unwrap();

        assert_ne!(first.disposition(), second.disposition());
        assert_eq!(first.path(), second.path());
        assert_eq!(
            fs::read(first.path().join("main.sai")).unwrap(),
            b"pub fn answer() -> i32 { 42 }\n"
        );
        let cache = PackageCache::open(cache_root).unwrap();
        assert_eq!(
            fs::read_dir(cache.temporary_directory()).unwrap().count(),
            0
        );
    }

    #[test]
    fn failed_staging_cleanup_restores_file_writability_before_removal() {
        let temp = TestDirectory::new("readonly-cleanup");
        let root = temp.path().join("tree");
        fs::create_dir(&root).unwrap();
        let file = root.join("partial");
        fs::write(&file, b"partial").unwrap();
        super::set_read_only_path_permissions(&file).unwrap();
        super::set_read_only_directory_permissions(&root).unwrap();

        assert!(super::remove_tree_at_path(&root, temp.path()));
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_refuses_a_symlinked_parent_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("readonly-cleanup-parent-symlink");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let outside_entry = outside.join("entry");
        fs::create_dir(&outside_entry).unwrap();
        let outside_file = outside_entry.join("keep");
        fs::write(&outside_file, b"outside").unwrap();
        fs::set_permissions(&outside_file, fs::Permissions::from_mode(0o444)).unwrap();

        let cache_parent_link = temp.path().join("cache-parent");
        symlink(&outside, &cache_parent_link).unwrap();

        assert!(!super::remove_tree_at_path(
            &cache_parent_link.join("entry"),
            temp.path()
        ));
        assert!(outside_entry.is_dir());
        assert_eq!(
            fs::metadata(outside_file).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_refuses_a_symlinked_cache_layout_ancestor() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TestDirectory::new("readonly-cleanup-cache-ancestor-symlink");
        let cache_root = temp.path().join("cache");
        fs::create_dir(&cache_root).unwrap();
        let outside = temp.path().join("outside");
        let outside_source = outside.join("src");
        let outside_entry = outside_source.join("entry");
        fs::create_dir_all(&outside_entry).unwrap();
        let outside_file = outside_entry.join("keep");
        fs::write(&outside_file, b"outside").unwrap();
        fs::set_permissions(&outside_file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&outside_entry, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&outside_source, fs::Permissions::from_mode(0o555)).unwrap();
        symlink(&outside, cache_root.join(super::CACHE_LAYOUT_VERSION)).unwrap();

        assert!(!super::remove_tree_at_path(
            &cache_root
                .join(super::CACHE_LAYOUT_VERSION)
                .join("src")
                .join("entry"),
            &cache_root
        ));
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
        assert_eq!(
            fs::metadata(&outside_file).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[cfg(unix)]
    #[test]
    fn marker_cleanup_refuses_a_symlinked_cache_layout_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = TestDirectory::new("marker-cleanup-cache-ancestor-symlink");
        let cache_root = temp.path().join("cache");
        fs::create_dir(&cache_root).unwrap();
        let outside = temp.path().join("outside");
        let outside_tmp = outside.join("tmp");
        fs::create_dir_all(&outside_tmp).unwrap();
        let outside_marker = outside_tmp.join("source-1-2.lock");
        fs::write(&outside_marker, b"outside marker").unwrap();
        symlink(&outside, cache_root.join(super::CACHE_LAYOUT_VERSION)).unwrap();

        assert!(!super::remove_file_at_path(
            &cache_root
                .join(super::CACHE_LAYOUT_VERSION)
                .join("tmp")
                .join("source-1-2.lock"),
            &cache_root
        ));
        assert_eq!(fs::read(outside_marker).unwrap(), b"outside marker");
    }

    #[cfg(unix)]
    #[test]
    fn cross_parent_publish_temporarily_needs_a_writable_source_root() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("unix-cross-parent-rename");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let stage = cache.temporary_directory().join("rename-check");
        let target = cache.source_directory.join("rename-check");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("file"), b"verified").unwrap();
        super::make_tree_read_only(&stage).unwrap();

        super::prepare_source_directory_rename(&stage).unwrap();
        assert_eq!(
            fs::metadata(&stage).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::rename(&stage, &target).unwrap();
        super::seal_published_source_directory(&target).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    #[test]
    fn source_checksum_validation_rejects_noncanonical_cache_names() {
        let temp = TestDirectory::new("checksum");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();

        for checksum in [
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sai1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "sai1:abc",
        ] {
            assert!(matches!(
                cache.source_path(checksum),
                Err(PackageCacheError::InvalidSourceChecksum(_))
            ));
        }
    }

    #[test]
    fn resolves_cache_roots_from_documented_location_variables() {
        use std::ffi::OsStr;

        let path = resolve_cache_directory(
            Some(OsStr::new("/custom/cache")),
            Some(OsStr::new("/xdg")),
            Some(OsStr::new("/home/test")),
            Some(OsStr::new("C:\\Users\\test\\AppData\\Local")),
            CacheHost::Unix,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/custom/cache"));

        let xdg = resolve_cache_directory(
            None,
            Some(OsStr::new("/xdg")),
            Some(OsStr::new("/home/test")),
            None,
            CacheHost::Unix,
        )
        .unwrap();
        assert_eq!(xdg, PathBuf::from("/xdg/sai"));

        let mac = resolve_cache_directory(
            None,
            Some(OsStr::new("/xdg")),
            Some(OsStr::new("/Users/test")),
            None,
            CacheHost::MacOs,
        )
        .unwrap();
        assert_eq!(mac, PathBuf::from("/Users/test/Library/Caches/sai"));

        let windows = resolve_cache_directory(
            None,
            None,
            None,
            Some(OsStr::new("C:\\Users\\test\\AppData\\Local")),
            CacheHost::Windows,
        )
        .unwrap();
        assert_eq!(
            windows,
            PathBuf::from("C:\\Users\\test\\AppData\\Local")
                .join("sai")
                .join("cache")
        );
    }

    #[test]
    fn ignores_relative_xdg_and_uses_the_host_default() {
        use std::ffi::OsStr;

        let path = resolve_cache_directory(
            Some(OsStr::new("")),
            Some(OsStr::new("relative-xdg")),
            Some(OsStr::new("/home/test")),
            None,
            CacheHost::Unix,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/home/test/.cache/sai"));

        assert!(matches!(
            resolve_cache_directory(None, None, None, None, CacheHost::Unsupported),
            Err(PackageCacheError::CacheDirectoryUnavailable)
        ));
    }
}
