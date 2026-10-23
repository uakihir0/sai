//! Durable persistence for the semantic index-operation journal.
//!
//! This layer deliberately contains no transaction recovery policy. It keeps
//! one canonical journal under `.sai/index/operation.json`, anchored to the
//! directory handle owned by a workspace lock.

use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
use sai_semantic::{IndexJournalState, IndexOperationJournal, MAX_OPERATION_JOURNAL_BYTES};
use sai_windows_fs::{
    FileIdentity, create_child_directory_without_following,
    create_new_file_in_directory_without_following, file_identity,
    move_file_in_directory_without_replacing,
    open_child_directory_for_file_updates_without_following,
    open_child_directory_without_following, open_file_without_following,
    remove_file_in_directory_if_identity, remove_file_in_directory_if_unchanged,
    replace_file_in_directory_if_unchanged, sync_directory, visit_directory_entry_names,
};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const INDEX_DIRECTORY_NAME: &str = "index";
const JOURNAL_FILE_NAME: &str = "operation.json";
const TEMP_FILE_ATTEMPTS: usize = 8;

/// Errors from reading or durably updating `.sai/index/operation.json`.
#[derive(Debug)]
pub enum IndexOperationJournalStoreError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Codec {
        path: PathBuf,
        source: sai_semantic::IndexJournalCodecError,
    },
    FileTooLarge {
        path: PathBuf,
        maximum_bytes: usize,
    },
    AllocationFailed {
        path: PathBuf,
        requested_bytes: usize,
    },
    LockNotExclusive,
    JournalAlreadyExists,
    JournalChanged,
    InvalidPreparedState,
    OperationIdChanged,
}

impl fmt::Display for IndexOperationJournalStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "could not access operation journal at {path:?}: {source}"
                )
            }
            Self::Codec { path, source } => {
                write!(formatter, "invalid operation journal at {path:?}: {source}")
            }
            Self::FileTooLarge {
                path,
                maximum_bytes,
            } => write!(
                formatter,
                "operation journal at {path:?} exceeds the {maximum_bytes}-byte limit"
            ),
            Self::AllocationFailed {
                path,
                requested_bytes,
            } => write!(
                formatter,
                "could not reserve {requested_bytes} bytes to read operation journal at {path:?}"
            ),
            Self::LockNotExclusive => formatter
                .write_str("operation journal mutation requires an exclusive workspace lock"),
            Self::JournalAlreadyExists => {
                formatter.write_str("an unresolved operation journal already exists")
            }
            Self::JournalChanged => {
                formatter.write_str("operation journal changed since it was read")
            }
            Self::InvalidPreparedState => {
                formatter.write_str("a new operation journal must be in the prepared state")
            }
            Self::OperationIdChanged => formatter
                .write_str("an operation journal replacement cannot change its operation ID"),
        }
    }
}

impl Error for IndexOperationJournalStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec { source, .. } => Some(source),
            Self::FileTooLarge { .. }
            | Self::AllocationFailed { .. }
            | Self::LockNotExclusive
            | Self::JournalAlreadyExists
            | Self::JournalChanged
            | Self::InvalidPreparedState
            | Self::OperationIdChanged => None,
        }
    }
}

/// A journal store whose lifetime is tied to the lock protecting its workspace.
pub struct IndexOperationJournalStore<'lock> {
    lock: &'lock WorkspaceLockGuard,
}

impl<'lock> IndexOperationJournalStore<'lock> {
    /// Binds a journal store to a held workspace lock.
    pub fn new(lock: &'lock WorkspaceLockGuard) -> Self {
        Self { lock }
    }

    /// Reads and validates the journal, returning `None` only when the index
    /// directory or journal file is absent.
    ///
    /// An unsupported, oversized, unreadable, noncanonical, or corrupt record
    /// is preserved and returned as an error.
    pub fn read(&self) -> Result<Option<IndexOperationJournal>, IndexOperationJournalStoreError> {
        let Some(index_directory) = self.open_index_directory_for_read()? else {
            return Ok(None);
        };
        self.read_current(&index_directory)
            .map(|current| current.map(|record| record.journal))
    }

    /// Durably creates a prepared journal only if no journal currently exists.
    ///
    /// On platforms where a directory-entry flush is unsupported, this fails
    /// before creating `.sai/index` or publishing the record. If the final
    /// directory flush fails after publication, the record may be visible; the
    /// caller must reread it under the lock before deciding whether to retry.
    pub fn write_prepared(
        &self,
        journal: &IndexOperationJournal,
    ) -> Result<(), IndexOperationJournalStoreError> {
        self.require_exclusive()?;
        if journal.state != IndexJournalState::Prepared {
            return Err(IndexOperationJournalStoreError::InvalidPreparedState);
        }
        let bytes = self.encode_at_journal_path(journal)?;
        let index_directory = self
            .open_index_directory_for_mutation(true)?
            .ok_or_else(|| {
                self.io_error(
                    self.index_directory_path(),
                    io::Error::from(io::ErrorKind::NotFound),
                )
            })?;
        self.cleanup_staged_files(&index_directory)?;
        if self.read_current(&index_directory)?.is_some() {
            return Err(IndexOperationJournalStoreError::JournalAlreadyExists);
        }

        let (temporary_name, temporary_identity) =
            self.create_staged_file(&index_directory, &bytes)?;
        if let Err(source) = move_file_in_directory_without_replacing(
            &index_directory,
            &temporary_name,
            OsStr::new(JOURNAL_FILE_NAME),
        ) {
            let _ = remove_file_in_directory_if_unchanged(
                &index_directory,
                &temporary_name,
                temporary_identity,
                &bytes,
            );
            return Err(self.io_error(self.journal_path(), source));
        }
        self.sync_index_directory(&index_directory)?;
        Ok(())
    }

    /// Atomically replaces the journal only when its complete decoded value
    /// still equals `expected` and the operation ID is unchanged.
    ///
    /// State-transition legality belongs to the transaction coordinator.
    /// If the final directory flush fails, the replacement may already be
    /// visible while its durability is unknown; reread under the exclusive
    /// workspace lock before deciding whether to retry.
    pub fn replace_if_current(
        &self,
        expected: &IndexOperationJournal,
        replacement: &IndexOperationJournal,
    ) -> Result<(), IndexOperationJournalStoreError> {
        self.require_exclusive()?;
        if expected.operation_id != replacement.operation_id {
            return Err(IndexOperationJournalStoreError::OperationIdChanged);
        }
        let replacement_bytes = self.encode_at_journal_path(replacement)?;
        let index_directory = self
            .open_index_directory_for_mutation(false)?
            .ok_or(IndexOperationJournalStoreError::JournalChanged)?;
        self.cleanup_staged_files(&index_directory)?;
        let Some(current) = self.read_current(&index_directory)? else {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        };
        if &current.journal != expected {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        if current.journal == *replacement {
            return Ok(());
        }

        let (temporary_name, temporary_identity) =
            self.create_staged_file(&index_directory, &replacement_bytes)?;
        if let Err(source) = replace_file_in_directory_if_unchanged(
            &index_directory,
            &temporary_name,
            OsStr::new(JOURNAL_FILE_NAME),
            current.identity,
            &current.bytes,
        ) {
            let _ = remove_file_in_directory_if_unchanged(
                &index_directory,
                &temporary_name,
                temporary_identity,
                &replacement_bytes,
            );
            return Err(self.io_error(self.journal_path(), source));
        }
        self.sync_index_directory(&index_directory)?;
        Ok(())
    }

    /// Removes the journal only when its complete decoded value still equals
    /// `expected`. Returns `false` if it has already been removed.
    ///
    /// If the final directory flush fails, removal may already be visible
    /// while its durability is unknown; reread under the exclusive workspace
    /// lock before deciding whether to retry.
    pub fn clear_if_current(
        &self,
        expected: &IndexOperationJournal,
    ) -> Result<bool, IndexOperationJournalStoreError> {
        self.require_exclusive()?;
        let Some(index_directory) = self.open_index_directory_for_mutation(false)? else {
            return Ok(false);
        };
        self.cleanup_staged_files(&index_directory)?;
        let Some(current) = self.read_current(&index_directory)? else {
            return Ok(false);
        };
        if &current.journal != expected {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        remove_file_in_directory_if_unchanged(
            &index_directory,
            OsStr::new(JOURNAL_FILE_NAME),
            current.identity,
            &current.bytes,
        )
        .map_err(|source| self.io_error(self.journal_path(), source))?;
        self.sync_index_directory(&index_directory)?;
        Ok(true)
    }

    /// Best-effort removal of crash-left journal staging files.
    ///
    /// Only exact lowercase staging names are considered. Entries are opened
    /// relative to the already-open index directory without following links,
    /// and only regular files with one link are removed. Candidates that
    /// cannot be verified or removed are preserved. Enumeration and directory
    /// flush errors still fail the store operation.
    fn cleanup_staged_files(
        &self,
        index_directory: &File,
    ) -> Result<(), IndexOperationJournalStoreError> {
        self.require_exclusive()?;
        let index_path = self.index_directory_path();
        let mut removed_any = false;
        let enumeration = visit_directory_entry_names(index_directory, |name| {
            if !is_staged_file_name(name) {
                return Ok(());
            }

            let file = match open_file_without_following(index_directory, &index_path, name) {
                Ok(file) => file,
                // Staging-looking symlinks, directories, and entries changed
                // by an external actor are not cleanup targets.
                Err(_) => return Ok(()),
            };
            if !matches!(has_one_link(&file), Ok(true)) {
                return Ok(());
            }
            let Ok(identity) = file_identity(&file) else {
                return Ok(());
            };
            drop(file);

            if remove_file_in_directory_if_identity(index_directory, name, identity).is_ok() {
                removed_any = true;
            }
            Ok(())
        });

        if removed_any {
            self.sync_index_directory(index_directory)?;
        }
        enumeration.map_err(|source| self.io_error(index_path, source))?;
        Ok(())
    }

    fn require_exclusive(&self) -> Result<(), IndexOperationJournalStoreError> {
        if self.lock.mode() == WorkspaceLockMode::Exclusive {
            return Ok(());
        }
        Err(IndexOperationJournalStoreError::LockNotExclusive)
    }

    fn open_index_directory_for_read(
        &self,
    ) -> Result<Option<File>, IndexOperationJournalStoreError> {
        let state_path = self.state_directory_path();
        match open_child_directory_without_following(
            self.lock.state_directory_handle(),
            &state_path,
            OsStr::new(INDEX_DIRECTORY_NAME),
        ) {
            Ok(directory) => Ok(Some(directory)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(self.io_error(self.index_directory_path(), source)),
        }
    }

    fn open_index_directory_for_mutation(
        &self,
        create_if_missing: bool,
    ) -> Result<Option<File>, IndexOperationJournalStoreError> {
        self.require_exclusive()?;
        let state_directory = self.lock.state_directory_handle();
        let state_path = self.state_directory_path();

        let index_directory = if create_if_missing {
            // This preflight deliberately happens before namespace mutation.
            // It fails closed on platforms without a supported directory flush.
            sync_directory(state_directory)
                .map_err(|source| self.io_error(state_path.clone(), source))?;

            match create_child_directory_without_following(
                state_directory,
                OsStr::new(INDEX_DIRECTORY_NAME),
            ) {
                Ok(directory) => {
                    sync_directory(state_directory)
                        .map_err(|source| self.io_error(state_path, source))?;
                    Some(directory)
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    self.open_existing_index_directory_for_mutation(state_directory)?
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(source) => {
                    return Err(self.io_error(self.index_directory_path(), source));
                }
            }
        } else {
            self.open_existing_index_directory_for_mutation(state_directory)?
        };
        if let Some(index_directory) = &index_directory {
            sync_directory(index_directory)
                .map_err(|source| self.io_error(self.index_directory_path(), source))?;
        }
        Ok(index_directory)
    }

    fn open_existing_index_directory_for_mutation(
        &self,
        state_directory: &File,
    ) -> Result<Option<File>, IndexOperationJournalStoreError> {
        match open_child_directory_for_file_updates_without_following(
            state_directory,
            &self.state_directory_path(),
            OsStr::new(INDEX_DIRECTORY_NAME),
        ) {
            Ok(directory) => Ok(Some(directory)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(self.io_error(self.index_directory_path(), source)),
        }
    }

    fn read_current(
        &self,
        index_directory: &File,
    ) -> Result<Option<CurrentRecord>, IndexOperationJournalStoreError> {
        let path = self.journal_path();
        let mut file = match open_file_without_following(
            index_directory,
            &self.index_directory_path(),
            OsStr::new(JOURNAL_FILE_NAME),
        ) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(self.io_error(path, source)),
        };
        let identity =
            file_identity(&file).map_err(|source| self.io_error(path.clone(), source))?;
        let bytes = read_bounded_file(&mut file, &path, MAX_OPERATION_JOURNAL_BYTES)?;
        let journal = IndexOperationJournal::decode(&bytes).map_err(|source| {
            IndexOperationJournalStoreError::Codec {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Some(CurrentRecord {
            journal,
            bytes,
            identity,
        }))
    }

    fn create_staged_file(
        &self,
        index_directory: &File,
        bytes: &[u8],
    ) -> Result<(OsString, FileIdentity), IndexOperationJournalStoreError> {
        for _ in 0..TEMP_FILE_ATTEMPTS {
            let temporary_name = random_temporary_name()
                .map_err(|source| self.io_error(self.index_directory_path(), source))?;
            let mut file = match create_new_file_in_directory_without_following(
                index_directory,
                &temporary_name,
            ) {
                Ok(file) => file,
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(
                        self.io_error(self.index_directory_path().join(&temporary_name), source)
                    );
                }
            };
            let temporary_identity = file_identity(&file).map_err(|source| {
                self.io_error(self.index_directory_path().join(&temporary_name), source)
            })?;
            let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
            drop(file);
            if let Err(source) = write_result {
                let _ = remove_file_in_directory_if_identity(
                    index_directory,
                    &temporary_name,
                    temporary_identity,
                );
                return Err(
                    self.io_error(self.index_directory_path().join(&temporary_name), source)
                );
            }
            if let Err(error) =
                self.verify_staged_file(index_directory, &temporary_name, temporary_identity, bytes)
            {
                let _ = remove_file_in_directory_if_identity(
                    index_directory,
                    &temporary_name,
                    temporary_identity,
                );
                return Err(error);
            }
            return Ok((temporary_name, temporary_identity));
        }
        Err(self.io_error(
            self.index_directory_path(),
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique journal staging filename",
            ),
        ))
    }

    fn verify_staged_file(
        &self,
        index_directory: &File,
        name: &OsStr,
        expected_identity: FileIdentity,
        expected_bytes: &[u8],
    ) -> Result<(), IndexOperationJournalStoreError> {
        let path = self.index_directory_path().join(name);
        let mut file =
            open_file_without_following(index_directory, &self.index_directory_path(), name)
                .map_err(|source| self.io_error(path.clone(), source))?;
        if file_identity(&file).map_err(|source| self.io_error(path.clone(), source))?
            != expected_identity
        {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        let bytes = read_bounded_file(&mut file, &path, expected_bytes.len())?;
        if bytes != expected_bytes {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        Ok(())
    }

    fn encode_at_journal_path(
        &self,
        journal: &IndexOperationJournal,
    ) -> Result<Vec<u8>, IndexOperationJournalStoreError> {
        journal
            .encode()
            .map_err(|source| IndexOperationJournalStoreError::Codec {
                path: self.journal_path(),
                source,
            })
    }

    fn sync_index_directory(
        &self,
        index_directory: &File,
    ) -> Result<(), IndexOperationJournalStoreError> {
        sync_directory(index_directory)
            .map_err(|source| self.io_error(self.index_directory_path(), source))
    }

    fn state_directory_path(&self) -> PathBuf {
        self.lock
            .path()
            .parent()
            .expect("workspace lock path has a state-directory parent")
            .to_path_buf()
    }

    fn index_directory_path(&self) -> PathBuf {
        self.state_directory_path().join(INDEX_DIRECTORY_NAME)
    }

    fn journal_path(&self) -> PathBuf {
        self.index_directory_path().join(JOURNAL_FILE_NAME)
    }

    fn io_error(&self, path: PathBuf, source: io::Error) -> IndexOperationJournalStoreError {
        IndexOperationJournalStoreError::Io { path, source }
    }
}

struct CurrentRecord {
    journal: IndexOperationJournal,
    bytes: Vec<u8>,
    identity: FileIdentity,
}

fn random_temporary_name() -> io::Result<OsString> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let mut hex = String::with_capacity(random.len() * 2);
    for byte in random {
        use fmt::Write as _;
        write!(hex, "{byte:02x}").expect("writing to a string cannot fail");
    }
    Ok(OsString::from(format!(".operation-{hex}.tmp")))
}

fn is_staged_file_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(hex) = name
        .strip_prefix(".operation-")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    hex.len() == 32
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn has_one_link(file: &File) -> io::Result<bool> {
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(metadata.nlink() == 1)
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        Ok(sai_windows_fs::number_of_links(file)? == 1)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Ok(false)
    }
}

fn read_bounded_file(
    file: &mut File,
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, IndexOperationJournalStoreError> {
    let metadata = file
        .metadata()
        .map_err(|source| IndexOperationJournalStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(IndexOperationJournalStoreError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "journal is not a regular file"),
        });
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(IndexOperationJournalStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        IndexOperationJournalStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        }
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|_| {
        IndexOperationJournalStoreError::AllocationFailed {
            path: path.to_path_buf(),
            requested_bytes: capacity,
        }
    })?;
    let bounded_read_limit = maximum_bytes
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or_else(|| IndexOperationJournalStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        })?;
    file.take(bounded_read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| IndexOperationJournalStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > maximum_bytes {
        return Err(IndexOperationJournalStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::is_staged_file_name;
    use super::{IndexOperationJournalStore, IndexOperationJournalStoreError, read_bounded_file};
    use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange, SymbolHex, SymbolIdMapping,
    };
    use sai_stable_hash::hash_bytes;
    use std::fs;
    #[cfg(windows)]
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-journal-store-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temporary workspace");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn journal() -> IndexOperationJournal {
        let package_file =
            PackageIndexFileName::parse("acme.user.1.jsonl").expect("valid package-index filename");
        let before_entries = index_entry("acme:user.greet");
        let after_entries = index_entry("acme:user.say");
        IndexOperationJournal {
            operation_id: "1c71050c-1e25-4acd-8a56-03af72c9ce7a".into(),
            origin: IndexJournalOrigin::Cli,
            state: IndexJournalState::Prepared,
            sources: vec![SourceFileChange {
                package_node_key: "package:acme:user@1.0.0".into(),
                package_root: ".".into(),
                path: "src/lib.sai".into(),
                before: FileState::RegularFile(b"pub fn greet() {}\n".to_vec()),
                planned_after: FileState::RegularFile(b"pub fn say() {}\n".to_vec()),
            }],
            index_before: index_snapshot(package_file.clone(), before_entries),
            index_after: index_snapshot(package_file, after_entries),
            symbol_mappings: vec![SymbolIdMapping {
                package_node_key: "package:acme:user@1.0.0".into(),
                hex: SymbolHex::parse("000001").expect("valid symbol hex"),
                before_path: Some("acme:user.greet".into()),
                after_path: Some("acme:user.say".into()),
            }],
        }
    }

    fn index_entry(path: &str) -> Vec<u8> {
        format!(
            "{{\"hex\":\"000001\",\"def_path_hash\":\"00000000000000000000000000000001\",\"path\":\"{path}\",\"kind\":\"fn\",\"fingerprint\":\"00000000000000000000000000000002\"}}\n"
        )
        .into_bytes()
    }

    fn index_snapshot(file: PackageIndexFileName, entries: Vec<u8>) -> IndexSnapshot {
        let digest = hash_bytes(&entries).to_hex();
        let header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{file}\",\"blake3\":\"{digest}\"}}]}}\n"
        )
        .into_bytes();
        IndexSnapshot {
            header: FileState::RegularFile(header),
            package_files: vec![PackageFileState {
                file,
                state: FileState::RegularFile(entries),
            }],
        }
    }

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "redox"
        )
    ))]
    #[test]
    fn writes_replaces_and_clears_one_prepared_record() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let prepared = journal();

        assert!(store.read().unwrap().is_none());
        store.write_prepared(&prepared).unwrap();
        assert_eq!(store.read().unwrap(), Some(prepared.clone()));

        let mut sources_applied = prepared.clone();
        sources_applied.state = IndexJournalState::SourcesApplied;
        store
            .replace_if_current(&prepared, &sources_applied)
            .unwrap();
        assert_eq!(store.read().unwrap(), Some(sources_applied.clone()));

        assert!(store.clear_if_current(&sources_applied).unwrap());
        assert!(store.read().unwrap().is_none());
        assert!(!store.clear_if_current(&sources_applied).unwrap());
    }

    #[test]
    fn shared_lock_can_read_but_cannot_create_a_journal() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();
        let store = IndexOperationJournalStore::new(&lock);

        assert!(store.read().unwrap().is_none());
        assert!(matches!(
            store.write_prepared(&journal()),
            Err(IndexOperationJournalStoreError::LockNotExclusive)
        ));
        assert!(!directory.0.join(".sai/index").exists());
    }

    #[cfg(unix)]
    #[test]
    fn corrupt_record_is_preserved_and_blocks_a_new_write() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let index_directory = lock.package_directory().join(".sai").join("index");
        fs::create_dir(&index_directory).unwrap();
        let journal_path = index_directory.join("operation.json");
        fs::write(&journal_path, b"{not canonical json}\n").unwrap();
        let original = fs::read(&journal_path).unwrap();
        let store = IndexOperationJournalStore::new(&lock);

        assert!(matches!(
            store.read(),
            Err(IndexOperationJournalStoreError::Codec { .. })
        ));
        assert!(matches!(
            store.write_prepared(&journal()),
            Err(IndexOperationJournalStoreError::Codec { .. })
        ));
        assert_eq!(fs::read(journal_path).unwrap(), original);
    }

    #[test]
    fn bounded_read_rejects_oversized_metadata_before_reserving_input() {
        let directory = TestDirectory::new();
        let path = directory.0.join("oversized");
        fs::write(&path, b"123456789").unwrap();
        let mut file = fs::File::open(&path).unwrap();

        assert!(matches!(
            read_bounded_file(&mut file, &path, 8),
            Err(IndexOperationJournalStoreError::FileTooLarge {
                maximum_bytes: 8,
                ..
            })
        ));
    }

    #[test]
    fn recognizes_only_canonical_staging_names() {
        assert!(is_staged_file_name(std::ffi::OsStr::new(
            ".operation-0123456789abcdef0123456789abcdef.tmp"
        )));
        for name in [
            ".operation-0123456789abcdef0123456789abcdeg.tmp",
            ".operation-0123456789ABCDEF0123456789ABCDEF.tmp",
            ".operation-0123456789abcdef0123456789abcdef.TMP",
            ".operation-0123456789abcdef0123456789abcdef.tmp.extra",
            ".operation-0123456789abcdef0123456789abcde.tmp",
            "operation-0123456789abcdef0123456789abcdef.tmp",
        ] {
            assert!(!is_staged_file_name(std::ffi::OsStr::new(name)), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_prepared_removes_only_single_link_staging_orphans() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let index_directory = lock.package_directory().join(".sai").join("index");
        fs::create_dir_all(&index_directory).unwrap();

        let stale = ".operation-0123456789abcdef0123456789abcdef.tmp";
        fs::write(index_directory.join(stale), b"interrupted staging bytes").unwrap();
        let near_match = ".operation-0123456789abcdef0123456789abcdef.tmp.extra";
        fs::write(index_directory.join(near_match), b"preserve").unwrap();
        let hard_link = ".operation-1123456789abcdef0123456789abcdef.tmp";
        fs::write(index_directory.join("linked-data"), b"preserve").unwrap();
        fs::hard_link(
            index_directory.join("linked-data"),
            index_directory.join(hard_link),
        )
        .unwrap();
        let symlink_name = ".operation-2123456789abcdef0123456789abcdef.tmp";
        symlink("linked-data", index_directory.join(symlink_name)).unwrap();
        let directory_name = ".operation-3123456789abcdef0123456789abcdef.tmp";
        fs::create_dir(index_directory.join(directory_name)).unwrap();

        let store = IndexOperationJournalStore::new(&lock);
        store.write_prepared(&journal()).unwrap();

        assert!(!index_directory.join(stale).exists());
        assert!(index_directory.join(near_match).exists());
        assert!(index_directory.join(hard_link).exists());
        assert!(
            index_directory
                .join(symlink_name)
                .symlink_metadata()
                .is_ok()
        );
        assert!(index_directory.join(directory_name).is_dir());
        assert_eq!(
            fs::read(index_directory.join("linked-data")).unwrap(),
            b"preserve"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_orphan_cleanup_is_not_run_with_a_shared_lock() {
        let directory = TestDirectory::new();
        let index_directory = directory.0.join(".sai").join("index");
        fs::create_dir_all(&index_directory).unwrap();
        let stale = ".operation-0123456789abcdef0123456789abcdef.tmp";
        fs::write(index_directory.join(stale), b"preserve under shared lock").unwrap();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();

        let store = IndexOperationJournalStore::new(&lock);
        assert!(store.read().unwrap().is_none());
        assert!(matches!(
            store.write_prepared(&journal()),
            Err(IndexOperationJournalStoreError::LockNotExclusive)
        ));
        assert_eq!(
            fs::read(index_directory.join(stale)).unwrap(),
            b"preserve under shared lock"
        );
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_directory_flush_fails_before_creating_the_index_directory() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);

        let error = store.write_prepared(&journal()).unwrap_err();
        assert!(matches!(
            error,
            IndexOperationJournalStoreError::Io {
                source,
                ..
            } if source.kind() == io::ErrorKind::Unsupported
        ));
        assert!(!directory.0.join(".sai/index").exists());
    }
}
