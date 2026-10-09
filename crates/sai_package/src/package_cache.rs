//! Content-addressed storage for verified package source trees.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::git_tree::{
    GitPackageFile, GitPackageSourceTree, MAX_PACKAGE_CONTENT_BYTES, MAX_PACKAGE_FILE_COUNT,
    MAX_PACKAGE_TREE_ENTRIES, MAX_PACKAGE_TREE_PATH_BYTES,
};
use crate::lock_validation::canonical_git_url;
use crate::package_path::ContentPackagePath;

const CACHE_LAYOUT_VERSION: &str = "v1";
const SOURCE_CHECKSUM_PREFIX: &str = "sai1:";
const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_CACHE_RECORD_BYTES: u64 = 16 * 1024;
const TAG_RECORD_MAGIC: &[u8] = b"SAI-CACHE-TAG-v1\0";
const SOURCE_RECORD_MAGIC: &[u8] = b"SAI-CACHE-SOURCE-v1\0";
const RECORD_DIGEST_BYTES: usize = 32;
#[cfg(windows)]
const WINDOWS_ERROR_LOCK_VIOLATION: i32 = 33;
#[cfg(windows)]
const WINDOWS_ERROR_SHARING_VIOLATION: i32 = 32;

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
    /// A package directory was not a normalized content path.
    InvalidPackageDirectory,
    /// A cache record was malformed, mismatched, or too large.
    CorruptRecord(PathBuf),
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

    fn replace_cache_record(
        &self,
        target: &Path,
        magic: &[u8],
        fields: &[&[u8]],
    ) -> Result<(), PackageCacheError> {
        self.cleanup_stale_temporary_directories();
        let _lock = CachePublicationLock::acquire(&self.lock_path)?;
        let existing = read_latest_cache_record(target, magic, fields.len())?;
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

        let staging = StagingDirectory::create(&self.temporary_directory)?;
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
            fs::remove_file(&destination).map_err(|source| PackageCacheError::Io {
                operation: "replace inactive package cache record slot",
                path: destination.clone(),
                source,
            })?;
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
        let (tree_hash, manifest, files) = source.into_parts();
        drop(manifest);

        let checksum = tree_hash.checksum.into_boxed_str();
        let digest = source_digest(&checksum)?;
        let target = self.source_directory.join(digest);
        self.cleanup_stale_temporary_directories();
        if path_exists_without_following(&target)? && cached_source_matches(&target, &files) {
            return Ok(CachedPackageSource {
                checksum,
                path: target,
                disposition: CacheDisposition::AlreadyPresent,
            });
        }

        let mut staging = StagingDirectory::create(&self.temporary_directory)?;
        for source_file in &files {
            write_source_file(staging.path(), source_file)?;
        }
        staging.refresh_activity_age()?;
        make_tree_read_only(staging.path())?;
        prepare_source_directory_rename(staging.path())?;

        let _lock = CachePublicationLock::acquire(&self.lock_path)?;
        if path_exists_without_following(&target)? && cached_source_matches(&target, &files) {
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
            if path_exists_without_following(&target)? && cached_source_matches(&target, &files) {
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
            make_tree_removable(&target);
            let _ = fs::remove_dir_all(&target);
            return Err(error);
        }
        staging.mark_published();

        Ok(CachedPackageSource {
            checksum,
            path: target,
            disposition: CacheDisposition::Installed,
        })
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
                            match create_staging_activity_lock(&activity_path) {
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
                make_tree_removable(&path);
                let removed = fs::remove_dir_all(&path).is_ok();
                release_staging_activity_lock(&activity_path, activity_lock, removed);
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
            release_staging_activity_lock(&path, activity_lock, orphaned);
        }
    }
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
    ContentPackagePath::parse(value)
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

fn create_staging_directory(path: &Path) -> Result<(), PackageCacheError> {
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
        let _ = fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn create_staging_activity_lock(path: &Path) -> Result<File, PackageCacheError> {
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
        let _ = fs::remove_file(path);
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

pub(crate) fn release_staging_activity_lock(path: &Path, lock: File, remove_marker: bool) {
    #[cfg(unix)]
    if remove_marker {
        let _ = fs::remove_file(path);
    }

    drop(lock);

    #[cfg(not(unix))]
    if remove_marker {
        // Windows does not allow unlinking an open marker. The path remains
        // present until after the exclusive handle is released, so a new
        // writer cannot claim this staging name in the gap.
        let _ = fs::remove_file(path);
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

struct CachedFileEntry {
    relative_path: String,
    absolute_path: PathBuf,
    size: u64,
}

fn cached_source_matches(root: &Path, expected_files: &[GitPackageFile]) -> bool {
    let Ok(root_metadata) = fs::symlink_metadata(root) else {
        return false;
    };
    if !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || !is_read_only_directory(&root_metadata)
    {
        return false;
    }

    let mut files = Vec::new();
    let mut directories = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    let mut entry_count = 0u64;
    let mut path_bytes = 0u64;
    let mut content_bytes = 0u64;

    while let Some((directory, parent_relative_path)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_PACKAGE_TREE_ENTRIES {
                return false;
            }

            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return false;
            };
            let relative_path = if parent_relative_path.is_empty() {
                name
            } else {
                format!("{parent_relative_path}/{name}")
            };
            path_bytes = path_bytes.saturating_add(relative_path.len() as u64);
            if path_bytes > MAX_PACKAGE_TREE_PATH_BYTES
                || safe_relative_path(&relative_path).is_err()
            {
                return false;
            }

            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                return false;
            };
            if metadata.file_type().is_symlink() {
                return false;
            }
            if metadata.is_dir() {
                if !is_read_only_directory(&metadata) {
                    return false;
                }
                directories.insert(relative_path.clone());
                pending.push((path, relative_path));
                continue;
            }
            if !metadata.is_file()
                || !is_read_only_file(&metadata)
                || files.len() as u64 >= MAX_PACKAGE_FILE_COUNT
            {
                return false;
            }
            content_bytes = content_bytes.saturating_add(metadata.len());
            if content_bytes > MAX_PACKAGE_CONTENT_BYTES {
                return false;
            }
            files.push(CachedFileEntry {
                relative_path,
                absolute_path: path,
                size: metadata.len(),
            });
        }
    }

    if files.len() != expected_files.len() {
        return false;
    }
    files.sort_by(|left, right| {
        left.relative_path
            .as_bytes()
            .cmp(right.relative_path.as_bytes())
    });

    let mut expected_directories = BTreeSet::new();
    for source_file in expected_files {
        for (separator, _) in source_file.path().match_indices('/') {
            expected_directories.insert(source_file.path()[..separator].to_owned());
        }
    }
    if directories != expected_directories {
        return false;
    }

    files.iter().zip(expected_files).all(|(cached, expected)| {
        cached.relative_path == expected.path()
            && cached.size == expected.content().len() as u64
            && cached_file_matches_content(&cached.absolute_path, expected.content())
    })
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

fn is_read_only_file(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777 == 0o444 && metadata.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        metadata.permissions().readonly()
    }
}

fn cached_file_matches_content(path: &Path, expected: &[u8]) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return false;
    }

    let mut buffer = [0; 16 * 1024];
    for chunk in expected.chunks(buffer.len()) {
        if file.read_exact(&mut buffer[..chunk.len()]).is_err() || buffer[..chunk.len()] != *chunk {
            return false;
        }
    }
    let mut extra = [0];
    matches!(file.read(&mut extra), Ok(0))
}

fn remove_cache_entry(path: &Path) -> Result<(), PackageCacheError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PackageCacheError::Io {
        operation: "inspect inconsistent package cache entry",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        make_tree_removable(path);
        fs::remove_dir_all(path)
    } else {
        if metadata.is_file() && is_single_link_file(&metadata) {
            set_writable_path_permissions(path)?;
        }
        fs::remove_file(path)
    }
    .map_err(|source| PackageCacheError::Io {
        operation: "remove inconsistent package cache entry",
        path: path.to_path_buf(),
        source,
    })
}

struct StagingDirectory {
    path: PathBuf,
    activity_lock_path: PathBuf,
    activity_lock: Option<File>,
    published: bool,
}

impl StagingDirectory {
    fn create(parent: &Path) -> Result<Self, PackageCacheError> {
        for _ in 0..32 {
            let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("source-{}-{id}", std::process::id()));
            let activity_lock_path = staging_activity_lock_path(&path);
            let activity_lock = match create_staging_activity_lock(&activity_lock_path) {
                Ok(file) => file,
                Err(PackageCacheError::Io { source, .. })
                    if source.kind() == io::ErrorKind::AlreadyExists =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            match create_staging_directory(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        activity_lock_path,
                        activity_lock: Some(activity_lock),
                        published: false,
                    });
                }
                Err(PackageCacheError::Io { source, .. })
                    if source.kind() == io::ErrorKind::AlreadyExists =>
                {
                    drop(activity_lock);
                    let _ = fs::remove_file(activity_lock_path);
                }
                Err(error) => {
                    drop(activity_lock);
                    let _ = fs::remove_file(activity_lock_path);
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
            release_staging_activity_lock(&self.activity_lock_path, lock, remove_marker);
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
        make_tree_removable(&self.path);
        let removed = match fs::remove_dir_all(&self.path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
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

fn make_tree_removable(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                make_tree_removable(&path);
                let _ = set_writable_directory_permissions(&path);
            } else if metadata.is_file() && is_single_link_file(&metadata) {
                let _ = set_writable_path_permissions(&path);
            }
        }
    }
    let _ = set_writable_directory_permissions(root);
}

fn set_private_directory_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_directory_mode(path, 0o700, false)
}

fn set_read_only_directory_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_directory_mode(path, 0o555, true)
}

#[cfg(target_os = "macos")]
fn prepare_source_directory_rename(path: &Path) -> Result<(), PackageCacheError> {
    // APFS requires the moved directory itself to be writable for a rename
    // between different parents. The source parent is private; the directory
    // is sealed again immediately after publication.
    set_directory_mode(path, 0o755, false)
}

#[cfg(not(target_os = "macos"))]
fn prepare_source_directory_rename(_path: &Path) -> Result<(), PackageCacheError> {
    Ok(())
}

fn seal_published_source_directory(path: &Path) -> Result<(), PackageCacheError> {
    set_read_only_directory_permissions(path)
}

fn is_single_link_file(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn set_writable_directory_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_directory_mode(path, 0o700, false)
}

fn set_read_only_path_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_path_mode(path, 0o444, true)
}

fn set_writable_path_permissions(path: &Path) -> Result<(), PackageCacheError> {
    set_path_mode(path, 0o600, false)
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
        CacheDisposition, CacheHost, PackageCache, PackageCacheError, resolve_cache_directory,
    };
    use crate::git_tree::GitTreeReader;
    use gix::objs::tree::{Entry, EntryKind};
    use std::fs;
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
            super::make_tree_removable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
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
        let activity_lock = super::create_staging_activity_lock(&activity_path).unwrap();
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
        let staging = super::StagingDirectory::create(&cache.temporary_directory).unwrap();
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

        super::make_tree_removable(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(!root.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cross_parent_publish_temporarily_needs_a_writable_source_root() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDirectory::new("macos-cross-parent-rename");
        let cache = PackageCache::open(temp.path().join("cache")).unwrap();
        let stage = cache.temporary_directory().join("rename-check");
        let target = cache.source_directory.join("rename-check");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("file"), b"verified").unwrap();
        super::make_tree_read_only(&stage).unwrap();

        assert!(fs::rename(&stage, &target).is_err());

        super::prepare_source_directory_rename(&stage).unwrap();
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
