//! Durable persistence for the semantic index-operation journal.
//!
//! This layer deliberately contains no transaction recovery policy. It keeps
//! one canonical journal under `.sai/index/operation.json`, anchored to the
//! directory handle owned by a workspace lock.

use crate::index_operation_journal_recovery::IndexGenerationObservation;
use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
use sai_semantic::{
    FileState, IndexJournalState, IndexOperationJournal, MAX_OPERATION_JOURNAL_BYTES,
    PackageIndexFileName, SymbolIndexHeader,
};
use sai_stable_hash::{ContentHash, ContentHasher};
use sai_windows_fs::{
    FileIdentity, create_child_directory_without_following,
    create_new_file_in_directory_without_following, file_identity,
    move_file_in_directory_without_replacing,
    open_child_directory_for_file_updates_without_following,
    open_child_directory_without_following, open_file_for_stable_read_without_following,
    open_file_without_following, remove_file_in_directory_if_identity,
    remove_file_in_directory_if_unchanged, replace_file_in_directory_if_unchanged, sync_directory,
    visit_directory_entry_names,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

    /// Observes whether the committed index is at the before or planned-after
    /// generation recorded by `journal`, or whether its affected files are a
    /// recoverable mixture of those exact snapshots.
    ///
    /// The caller must hold this store's exclusive workspace lock and pass a
    /// journal obtained from `IndexOperationJournal::decode` (or an equivalent
    /// complete codec validation). The method also requires the current
    /// on-disk operation record to decode to that same journal before and after
    /// observation. This method is read-only. Unsafe, raced, stale, or
    /// incomplete filesystem observations return `Unavailable`; stable bytes
    /// that are readable but match neither recorded generation return
    /// `Contradictory`. Errors while opening, reading, or decoding the current
    /// operation record are returned as `IndexOperationJournalStoreError`;
    /// `Unavailable` describes an absent or changed current record and unsafe
    /// or changing index-generation observations.
    pub fn observe_index_generation(
        &self,
        journal: &IndexOperationJournal,
    ) -> Result<IndexGenerationObservation, IndexOperationJournalStoreError> {
        self.observe_index_generation_with_hook(journal, || {})
    }

    fn observe_index_generation_with_hook(
        &self,
        journal: &IndexOperationJournal,
        after_initial_scan: impl FnOnce(),
    ) -> Result<IndexGenerationObservation, IndexOperationJournalStoreError> {
        self.require_exclusive()?;

        let index_directory = match self.open_index_directory_for_read() {
            Ok(Some(directory)) => directory,
            Ok(None) | Err(_) => return Ok(IndexGenerationObservation::Unavailable),
        };
        let state_directory_identity = match file_identity(self.lock.state_directory_handle()) {
            Ok(identity) => identity,
            Err(_) => return Ok(IndexGenerationObservation::Unavailable),
        };
        let index_directory_identity = match file_identity(&index_directory) {
            Ok(identity) => identity,
            Err(_) => return Ok(IndexGenerationObservation::Unavailable),
        };
        let Some(current) = self.read_current_for_observation(&index_directory)? else {
            return Ok(IndexGenerationObservation::Unavailable);
        };
        if current.record.journal != *journal {
            return Ok(IndexGenerationObservation::Unavailable);
        }

        let (observation, entry_handles) =
            match self.observe_index_generation_inner(journal, after_initial_scan) {
                Ok((Ok(observation), handles)) => (observation, handles),
                Ok((Err(IndexGenerationObservationFailure::Contradictory), handles)) => {
                    (IndexGenerationObservation::Contradictory, handles)
                }
                Ok((Err(IndexGenerationObservationFailure::Unavailable), handles)) => {
                    (IndexGenerationObservation::Unavailable, handles)
                }
                Err(IndexGenerationObservationFailure::Contradictory) => {
                    (IndexGenerationObservation::Contradictory, Vec::new())
                }
                Err(IndexGenerationObservationFailure::Unavailable) => {
                    (IndexGenerationObservation::Unavailable, Vec::new())
                }
            };

        let current_directory = match self.open_index_directory_for_read() {
            Ok(Some(directory)) => directory,
            Ok(None) | Err(_) => return Ok(IndexGenerationObservation::Unavailable),
        };
        if file_identity(self.lock.state_directory_handle()).ok() != Some(state_directory_identity)
            || file_identity(&current_directory).ok() != Some(index_directory_identity)
        {
            return Ok(IndexGenerationObservation::Unavailable);
        }
        let Some(current_after) = self.read_current_for_observation(&current_directory)? else {
            return Ok(IndexGenerationObservation::Unavailable);
        };
        if current_after.record.journal != current.record.journal
            || current_after.record.bytes != current.record.bytes
            || current_after.record.identity != current.record.identity
            || current_after.record.metadata != current.record.metadata
        {
            return Ok(IndexGenerationObservation::Unavailable);
        }
        drop(entry_handles);
        Ok(observation)
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

    fn observe_index_generation_inner(
        &self,
        journal: &IndexOperationJournal,
        after_initial_scan: impl FnOnce(),
    ) -> Result<
        (
            Result<IndexGenerationObservation, IndexGenerationObservationFailure>,
            Vec<File>,
        ),
        IndexGenerationObservationFailure,
    > {
        let state_directory_identity = file_identity(self.lock.state_directory_handle())
            .map_err(|_| IndexGenerationObservationFailure::Unavailable)?;
        let index_directory = self
            .open_index_directory_for_read()
            .map_err(|_| IndexGenerationObservationFailure::Unavailable)?;
        let index_directory_identity = index_directory
            .as_ref()
            .map(file_identity)
            .transpose()
            .map_err(|_| IndexGenerationObservationFailure::Unavailable)?;

        let mut rechecks = Vec::new();
        let mut initial_handles = Vec::new();
        let observation = (|| {
            let maximum_header_bytes = state_bytes(&journal.index_before.header)
                .map_or(0, <[u8]>::len)
                .max(state_bytes(&journal.index_after.header).map_or(0, <[u8]>::len));
            let header_observation = match &index_directory {
                Some(directory) => observe_stable_bounded_file(
                    directory,
                    &self.index_directory_path(),
                    OsStr::new("index.json"),
                    maximum_header_bytes,
                )
                .map_err(|_| IndexGenerationObservationFailure::Unavailable)?,
                None => StableFileObservation::absent(),
            };
            rechecks.push(EntryRecheck::Bounded {
                name: OsString::from("index.json"),
                maximum_bytes: maximum_header_bytes,
                expected: header_observation.fingerprint.clone(),
                identity: header_observation.identity,
                metadata: header_observation.metadata.clone(),
            });
            if let Some(handle) = header_observation.handle {
                initial_handles.push(handle);
            }
            let header_state = match &header_observation.state {
                ObservedFileState::Absent => FileState::Absent,
                ObservedFileState::Bytes(bytes) => FileState::RegularFile(bytes.clone()),
                ObservedFileState::Oversized => {
                    return Err(IndexGenerationObservationFailure::Contradictory);
                }
            };

            let candidate_header = match &header_state {
                FileState::Absent => None,
                FileState::RegularFile(bytes) => Some(
                    SymbolIndexHeader::decode(bytes)
                        .map_err(|_| IndexGenerationObservationFailure::Contradictory)?,
                ),
            };
            let before_header_matches = header_state == journal.index_before.header;
            let after_header_matches = header_state == journal.index_after.header;
            if !before_header_matches && !after_header_matches {
                return Err(IndexGenerationObservationFailure::Contradictory);
            }

            let affected_names = affected_package_file_names(journal)
                .map_err(|_| IndexGenerationObservationFailure::Unavailable)?;
            let mut affected_states = BTreeMap::new();
            for file in &affected_names {
                let (before, after) = affected_file_state(journal, file)
                    .ok_or(IndexGenerationObservationFailure::Unavailable)?;
                let maximum_bytes = state_bytes(before)
                    .map_or(0, <[u8]>::len)
                    .max(state_bytes(after).map_or(0, <[u8]>::len));
                let observed = match &index_directory {
                    Some(directory) => observe_stable_bounded_file(
                        directory,
                        &self.index_directory_path(),
                        OsStr::new(file.as_str()),
                        maximum_bytes,
                    )
                    .map_err(|_| IndexGenerationObservationFailure::Unavailable)?,
                    None => StableFileObservation::absent(),
                };
                rechecks.push(EntryRecheck::Bounded {
                    name: OsString::from(file.as_str()),
                    maximum_bytes,
                    expected: observed.fingerprint.clone(),
                    identity: observed.identity,
                    metadata: observed.metadata.clone(),
                });
                if let Some(handle) = observed.handle {
                    initial_handles.push(handle);
                }
                let state = match &observed.state {
                    ObservedFileState::Absent => FileState::Absent,
                    ObservedFileState::Bytes(bytes) => FileState::RegularFile(bytes.clone()),
                    ObservedFileState::Oversized => {
                        return Err(IndexGenerationObservationFailure::Contradictory);
                    }
                };
                affected_states.insert(file.clone(), state);
            }

            if let Some(header) = &candidate_header {
                for record in header.records() {
                    if affected_names.contains(record.file()) {
                        // An affected file may still have its other recorded
                        // generation's bytes while the candidate header is
                        // already published. That is precisely a recoverable
                        // partial commit; the side snapshots were validated by
                        // the journal codec.
                        continue;
                    }

                    let observed = match &index_directory {
                        Some(directory) => observe_stable_file_digest(
                            directory,
                            &self.index_directory_path(),
                            OsStr::new(record.file().as_str()),
                        )
                        .map_err(|_| IndexGenerationObservationFailure::Unavailable)?,
                        None => StableDigestObservation::absent(),
                    };
                    rechecks.push(EntryRecheck::Digest {
                        name: OsString::from(record.file().as_str()),
                        expected: observed.fingerprint.clone(),
                        identity: observed.identity,
                        metadata: observed.metadata.clone(),
                    });
                    if let Some(handle) = observed.handle {
                        initial_handles.push(handle);
                    }
                    match observed.state {
                        ObservedDigestState::Absent => {
                            return Err(IndexGenerationObservationFailure::Contradictory);
                        }
                        ObservedDigestState::Digest(actual) if actual != record.digest() => {
                            return Err(IndexGenerationObservationFailure::Contradictory);
                        }
                        ObservedDigestState::Digest(_) => {}
                    }
                }
            }

            let matches_before =
                before_header_matches && affected_states_match(journal, &affected_states, false);
            let matches_after =
                after_header_matches && affected_states_match(journal, &affected_states, true);
            if matches_before {
                return Ok(IndexGenerationObservation::Before);
            }
            if matches_after {
                return Ok(IndexGenerationObservation::PlannedAfter);
            }
            if !affected_states_match_either(journal, &affected_states) {
                return Err(IndexGenerationObservationFailure::Contradictory);
            }
            Ok(IndexGenerationObservation::RecoverablePartial)
        })();

        after_initial_scan();
        let (mut final_handles, entries_stable) = revalidate_entries(
            index_directory.as_ref(),
            &self.index_directory_path(),
            &rechecks,
        );
        let observation = if entries_stable
            && index_directory_matches(self, index_directory_identity, state_directory_identity)
                .is_ok()
        {
            observation
        } else {
            Err(IndexGenerationObservationFailure::Unavailable)
        };
        initial_handles.append(&mut final_handles);
        Ok((observation, initial_handles))
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
        let metadata = FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?;
        let bytes = read_bounded_file(&mut file, &path, MAX_OPERATION_JOURNAL_BYTES)?;
        if FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?
            != metadata
            || file_identity(&file).map_err(|source| self.io_error(path.clone(), source))?
                != identity
        {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        verify_child_identity(
            index_directory,
            &self.index_directory_path(),
            OsStr::new(JOURNAL_FILE_NAME),
            identity,
        )
        .map_err(|source| self.io_error(path.clone(), source))?;
        if FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?
            != metadata
        {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
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
            metadata,
        }))
    }

    fn read_current_for_observation(
        &self,
        index_directory: &File,
    ) -> Result<Option<ObservedCurrentRecord>, IndexOperationJournalStoreError> {
        let path = self.journal_path();
        let mut file = match open_file_for_stable_read_without_following(
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
        let metadata = FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?;
        let bytes = read_bounded_file(&mut file, &path, MAX_OPERATION_JOURNAL_BYTES)?;
        if FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?
            != metadata
            || file_identity(&file).map_err(|source| self.io_error(path.clone(), source))?
                != identity
        {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        verify_child_identity(
            index_directory,
            &self.index_directory_path(),
            OsStr::new(JOURNAL_FILE_NAME),
            identity,
        )
        .map_err(|source| self.io_error(path.clone(), source))?;
        if FileObservationMetadata::read(&file)
            .map_err(|source| self.io_error(path.clone(), source))?
            != metadata
        {
            return Err(IndexOperationJournalStoreError::JournalChanged);
        }
        let journal = IndexOperationJournal::decode(&bytes).map_err(|source| {
            IndexOperationJournalStoreError::Codec {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Some(ObservedCurrentRecord {
            record: CurrentRecord {
                journal,
                bytes,
                identity,
                metadata,
            },
            _handle: file,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexGenerationObservationFailure {
    Contradictory,
    Unavailable,
}

#[derive(Clone, Eq, PartialEq)]
enum FileContentFingerprint {
    Absent,
    Bytes { length: u64, digest: ContentHash },
    Oversized,
}

enum ObservedFileState {
    Absent,
    Bytes(Vec<u8>),
    Oversized,
}

struct StableFileObservation {
    state: ObservedFileState,
    fingerprint: FileContentFingerprint,
    identity: Option<FileIdentity>,
    metadata: Option<FileObservationMetadata>,
    handle: Option<File>,
}

impl StableFileObservation {
    fn absent() -> Self {
        Self {
            state: ObservedFileState::Absent,
            fingerprint: FileContentFingerprint::Absent,
            identity: None,
            metadata: None,
            handle: None,
        }
    }
}

enum ObservedDigestState {
    Absent,
    Digest(ContentHash),
}

struct StableDigestObservation {
    state: ObservedDigestState,
    fingerprint: FileContentFingerprint,
    identity: Option<FileIdentity>,
    metadata: Option<FileObservationMetadata>,
    handle: Option<File>,
}

impl StableDigestObservation {
    fn absent() -> Self {
        Self {
            state: ObservedDigestState::Absent,
            fingerprint: FileContentFingerprint::Absent,
            identity: None,
            metadata: None,
            handle: None,
        }
    }
}

enum EntryRecheck {
    Bounded {
        name: OsString,
        maximum_bytes: usize,
        expected: FileContentFingerprint,
        identity: Option<FileIdentity>,
        metadata: Option<FileObservationMetadata>,
    },
    Digest {
        name: OsString,
        expected: FileContentFingerprint,
        identity: Option<FileIdentity>,
        metadata: Option<FileObservationMetadata>,
    },
}

#[derive(Clone, Eq, PartialEq)]
struct FileObservationMetadata {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl FileObservationMetadata {
    fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "index entry is not a regular file",
            ));
        }
        let modified = metadata.modified()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Ok(Self {
                length: metadata.len(),
                modified,
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                length: metadata.len(),
                modified,
            })
        }
    }
}

fn state_bytes(state: &FileState) -> Option<&[u8]> {
    match state {
        FileState::Absent => None,
        FileState::RegularFile(bytes) => Some(bytes),
    }
}

fn affected_package_file_names(
    journal: &IndexOperationJournal,
) -> Result<BTreeSet<PackageIndexFileName>, ()> {
    let before = journal.index_before.package_files();
    let after = journal.index_after.package_files();
    let before_names = before
        .iter()
        .map(|package_file| package_file.file.clone())
        .collect::<BTreeSet<_>>();
    let after_names = after
        .iter()
        .map(|package_file| package_file.file.clone())
        .collect::<BTreeSet<_>>();
    if before_names.len() != before.len() || after_names.len() != after.len() {
        return Err(());
    }
    if before_names != after_names {
        return Err(());
    }
    Ok(before_names)
}

fn affected_file_state<'a>(
    journal: &'a IndexOperationJournal,
    file: &PackageIndexFileName,
) -> Option<(&'a FileState, &'a FileState)> {
    let before = journal.index_before.package_files();
    let after = journal.index_after.package_files();
    let before_index = before
        .binary_search_by(|package_file| package_file.file.cmp(file))
        .ok()?;
    let after_index = after
        .binary_search_by(|package_file| package_file.file.cmp(file))
        .ok()?;
    Some((&before[before_index].state, &after[after_index].state))
}

fn affected_states_match(
    journal: &IndexOperationJournal,
    observed: &BTreeMap<PackageIndexFileName, FileState>,
    planned_after: bool,
) -> bool {
    let snapshot = if planned_after {
        &journal.index_after
    } else {
        &journal.index_before
    };
    snapshot
        .package_files()
        .iter()
        .all(|package_file| observed.get(&package_file.file) == Some(&package_file.state))
}

fn affected_states_match_either(
    journal: &IndexOperationJournal,
    observed: &BTreeMap<PackageIndexFileName, FileState>,
) -> bool {
    let before = journal.index_before.package_files();
    let after = journal.index_after.package_files();
    before.len() == after.len()
        && before.iter().zip(after).all(|(before, after)| {
            before.file == after.file
                && observed
                    .get(&before.file)
                    .is_some_and(|actual| actual == &before.state || actual == &after.state)
        })
}

fn index_directory_matches(
    store: &IndexOperationJournalStore<'_>,
    expected_identity: Option<FileIdentity>,
    expected_state_directory_identity: FileIdentity,
) -> io::Result<()> {
    if file_identity(store.lock.state_directory_handle())? != expected_state_directory_identity {
        return Err(io::Error::other(
            "workspace state directory identity changed",
        ));
    }
    let current = store
        .open_index_directory_for_read()
        .map_err(|error| match error {
            IndexOperationJournalStoreError::Io { source, .. } => source,
            _ => io::Error::other("could not revalidate index directory"),
        })?;
    match (expected_identity, current) {
        (None, None) => Ok(()),
        (Some(expected), Some(directory)) if file_identity(&directory)? == expected => Ok(()),
        _ => Err(io::Error::other("index directory identity changed")),
    }
}

fn observe_stable_bounded_file(
    directory: &File,
    directory_path: &Path,
    name: &OsStr,
    maximum_bytes: usize,
) -> io::Result<StableFileObservation> {
    let mut file =
        match open_file_for_stable_read_without_following(directory, directory_path, name) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                verify_child_absent(directory, directory_path, name)?;
                return Ok(StableFileObservation::absent());
            }
            Err(source) => return Err(source),
        };
    let identity = file_identity(&file)?;
    let before = FileObservationMetadata::read(&file)?;

    if before.length > u64::try_from(maximum_bytes).unwrap_or(u64::MAX) {
        // The size alone proves this cannot equal either recorded snapshot.
        // Avoid reading an attacker-controlled oversized file; only establish
        // that the same regular file stayed at the same path and size.
        verify_child_identity(directory, directory_path, name, identity)?;
        if FileObservationMetadata::read(&file)? != before || file_identity(&file)? != identity {
            return Err(io::Error::other("index file changed during observation"));
        }
        return Ok(StableFileObservation {
            state: ObservedFileState::Oversized,
            fingerprint: FileContentFingerprint::Oversized,
            identity: Some(identity),
            metadata: Some(before),
            handle: Some(file),
        });
    }

    let first = read_file_prefix(&mut file, maximum_bytes, before.length)?;
    if first.len() > maximum_bytes {
        return Err(io::Error::other("index file grew during bounded read"));
    }
    let middle = FileObservationMetadata::read(&file)?;
    let second = read_file_prefix(&mut file, maximum_bytes, before.length)?;
    let after = FileObservationMetadata::read(&file)?;
    if first != second || before != middle || before != after || file_identity(&file)? != identity {
        return Err(io::Error::other("index file changed during observation"));
    }
    verify_child_identity(directory, directory_path, name, identity)?;
    if FileObservationMetadata::read(&file)? != before {
        return Err(io::Error::other("index file changed during observation"));
    }
    let fingerprint = fingerprint_for_bytes(&first);
    Ok(StableFileObservation {
        state: ObservedFileState::Bytes(first),
        fingerprint,
        identity: Some(identity),
        metadata: Some(before),
        handle: Some(file),
    })
}

fn observe_stable_file_digest(
    directory: &File,
    directory_path: &Path,
    name: &OsStr,
) -> io::Result<StableDigestObservation> {
    let mut file =
        match open_file_for_stable_read_without_following(directory, directory_path, name) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                verify_child_absent(directory, directory_path, name)?;
                return Ok(StableDigestObservation::absent());
            }
            Err(source) => return Err(source),
        };
    let identity = file_identity(&file)?;
    let before = FileObservationMetadata::read(&file)?;
    let first = hash_file_exact_length(&mut file, before.length)?;
    let middle = FileObservationMetadata::read(&file)?;
    let second = hash_file_exact_length(&mut file, before.length)?;
    let after = FileObservationMetadata::read(&file)?;
    if first != second || before != middle || before != after || file_identity(&file)? != identity {
        return Err(io::Error::other("index file changed during observation"));
    }
    verify_child_identity(directory, directory_path, name, identity)?;
    if FileObservationMetadata::read(&file)? != before {
        return Err(io::Error::other("index file changed during observation"));
    }
    Ok(StableDigestObservation {
        state: ObservedDigestState::Digest(first),
        fingerprint: FileContentFingerprint::Bytes {
            length: before.length,
            digest: first,
        },
        identity: Some(identity),
        metadata: Some(before),
        handle: Some(file),
    })
}

fn read_file_prefix(
    file: &mut File,
    maximum_bytes: usize,
    initial_length: u64,
) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let limit = maximum_bytes
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or_else(|| io::Error::other("index file size bound overflowed"))?;
    let capacity = usize::try_from(initial_length.min(limit)).unwrap_or(maximum_bytes);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(io::Error::other)?;
    (&mut *file).take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn hash_file_exact_length(file: &mut File, expected_length: u64) -> io::Result<ContentHash> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = ContentHasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = expected_length;
    while remaining != 0 {
        let read_length = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded by the fixed hash buffer length");
        file.read_exact(&mut buffer[..read_length])?;
        hasher.update(&buffer[..read_length]);
        remaining -= read_length as u64;
    }
    let mut extra_byte = [0_u8; 1];
    if file.read(&mut extra_byte)? != 0 {
        return Err(io::Error::other("index file grew during bounded hash"));
    }
    Ok(hasher.finish())
}

fn fingerprint_for_bytes(bytes: &[u8]) -> FileContentFingerprint {
    let mut hasher = ContentHasher::new();
    hasher.update(bytes);
    FileContentFingerprint::Bytes {
        length: u64::try_from(bytes.len()).expect("a byte slice fits in u64"),
        digest: hasher.finish(),
    }
}

fn revalidate_entries(
    directory: Option<&File>,
    directory_path: &Path,
    rechecks: &[EntryRecheck],
) -> (Vec<File>, bool) {
    let mut handles = Vec::new();
    let mut all_stable = true;
    for recheck in rechecks {
        let (expected_fingerprint, expected_identity, expected_metadata, observed, observed_handle) =
            match recheck {
                EntryRecheck::Bounded {
                    name,
                    maximum_bytes,
                    expected,
                    identity,
                    metadata,
                } => {
                    let observed = match directory {
                        Some(directory) => match observe_stable_bounded_file(
                            directory,
                            directory_path,
                            name,
                            *maximum_bytes,
                        ) {
                            Ok(observed) => observed,
                            Err(_) => {
                                all_stable = false;
                                continue;
                            }
                        },
                        None => StableFileObservation::absent(),
                    };
                    (
                        expected,
                        *identity,
                        metadata,
                        (
                            observed.fingerprint.clone(),
                            observed.identity,
                            observed.metadata.clone(),
                        ),
                        observed.handle,
                    )
                }
                EntryRecheck::Digest {
                    name,
                    expected,
                    identity,
                    metadata,
                } => {
                    let observed = match directory {
                        Some(directory) => {
                            match observe_stable_file_digest(directory, directory_path, name) {
                                Ok(observed) => observed,
                                Err(_) => {
                                    all_stable = false;
                                    continue;
                                }
                            }
                        }
                        None => StableDigestObservation::absent(),
                    };
                    (
                        expected,
                        *identity,
                        metadata,
                        (
                            observed.fingerprint.clone(),
                            observed.identity,
                            observed.metadata.clone(),
                        ),
                        observed.handle,
                    )
                }
            };
        if observed.0 != *expected_fingerprint {
            all_stable = false;
        }
        // Absent entries have no identity or metadata; present entries must
        // retain both across the complete multi-file observation.
        if observed.1 != expected_identity || observed.2.as_ref() != expected_metadata.as_ref() {
            all_stable = false;
        }
        if let Some(handle) = observed_handle {
            handles.push(handle);
        }
    }
    (handles, all_stable)
}

fn verify_child_absent(directory: &File, directory_path: &Path, name: &OsStr) -> io::Result<()> {
    match open_file_without_following(directory, directory_path, name) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::other("index entry appeared during observation")),
        Err(source) => Err(source),
    }
}

fn verify_child_identity(
    directory: &File,
    directory_path: &Path,
    name: &OsStr,
    expected_identity: FileIdentity,
) -> io::Result<()> {
    let current = open_file_without_following(directory, directory_path, name)?;
    if file_identity(&current)? == expected_identity {
        return Ok(());
    }
    Err(io::Error::other(
        "index entry identity changed during observation",
    ))
}

struct CurrentRecord {
    journal: IndexOperationJournal,
    bytes: Vec<u8>,
    identity: FileIdentity,
    metadata: FileObservationMetadata,
}

struct ObservedCurrentRecord {
    record: CurrentRecord,
    _handle: File,
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
    use crate::index_operation_journal_recovery::IndexGenerationObservation;
    use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange, SymbolHex, SymbolIdMapping,
    };
    use sai_stable_hash::hash_bytes;
    use std::fs;
    #[cfg(windows)]
    use std::io;
    use std::path::{Path, PathBuf};
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

    fn empty_index_header() -> Vec<u8> {
        b"{\"format\":\"sai-index\",\"version\":1,\"packages\":[]}\n".to_vec()
    }

    fn journal_before_absent() -> IndexOperationJournal {
        let mut journal = journal();
        journal.index_before.header = FileState::Absent;
        journal.index_before.package_files[0].state = FileState::Absent;
        journal.symbol_mappings[0].before_path = None;
        journal.encode().expect("fixture remains a valid journal");
        journal
    }

    fn journal_before_empty_header() -> IndexOperationJournal {
        let mut journal = journal_before_absent();
        journal.index_before.header = FileState::RegularFile(empty_index_header());
        journal.encode().expect("fixture remains a valid journal");
        journal
    }

    fn journal_before_orphan() -> (IndexOperationJournal, Vec<u8>) {
        let mut journal = journal_before_empty_header();
        let orphan_bytes = index_entry("acme:user.greet");
        journal.index_before.package_files[0].state = FileState::RegularFile(orphan_bytes.clone());
        journal
            .encode()
            .expect("fixture with a recorded unlisted orphan is valid");
        (journal, orphan_bytes)
    }

    fn journal_with_unaffected_file() -> (IndexOperationJournal, PackageIndexFileName) {
        let affected_file =
            PackageIndexFileName::parse("acme.user.1.jsonl").expect("valid affected filename");
        let unaffected_file =
            PackageIndexFileName::parse("zeta.tools.1.jsonl").expect("valid unaffected filename");
        let before_entries = index_entry("acme:user.greet");
        let after_entries = index_entry("acme:user.say");
        let unaffected_entries = Vec::new();
        let before_header = header_with_two_files(
            &affected_file,
            &before_entries,
            &unaffected_file,
            &unaffected_entries,
        );
        let after_header = header_with_two_files(
            &affected_file,
            &after_entries,
            &unaffected_file,
            &unaffected_entries,
        );
        let mut journal = journal();
        journal.index_before.header = FileState::RegularFile(before_header);
        journal.index_after.header = FileState::RegularFile(after_header);
        journal.encode().expect("fixture is a valid journal");
        (journal, unaffected_file)
    }

    fn header_with_two_files(
        first_file: &PackageIndexFileName,
        first_bytes: &[u8],
        second_file: &PackageIndexFileName,
        second_bytes: &[u8],
    ) -> Vec<u8> {
        let mut records = [
            (first_file.as_str(), hash_bytes(first_bytes).to_hex()),
            (second_file.as_str(), hash_bytes(second_bytes).to_hex()),
        ];
        records.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{}\",\"blake3\":\"{}\"}},{{\"file\":\"{}\",\"blake3\":\"{}\"}}]}}\n",
            records[0].0, records[0].1, records[1].0, records[1].1
        )
        .into_bytes()
    }

    fn write_snapshot(index_directory: &Path, snapshot: &IndexSnapshot) {
        fs::create_dir_all(index_directory).unwrap();
        write_file_state(&index_directory.join("index.json"), &snapshot.header);
        for package_file in &snapshot.package_files {
            write_file_state(
                &index_directory.join(package_file.file.as_str()),
                &package_file.state,
            );
        }
    }

    fn write_current_journal(index_directory: &Path, journal: &IndexOperationJournal) {
        fs::create_dir_all(index_directory).unwrap();
        fs::write(
            index_directory.join("operation.json"),
            journal.encode().expect("valid test journal encodes"),
        )
        .unwrap();
    }

    fn write_file_state(path: &std::path::Path, state: &FileState) {
        match state {
            FileState::Absent => {
                let _ = fs::remove_file(path);
            }
            FileState::RegularFile(bytes) => fs::write(path, bytes).unwrap(),
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
        assert!(matches!(
            store.observe_index_generation(&journal()),
            Err(IndexOperationJournalStoreError::LockNotExclusive)
        ));
        assert!(!directory.0.join(".sai/index").exists());
    }

    #[test]
    fn observes_before_and_planned_after_generations() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let journal = journal();
        let index_directory = lock.package_directory().join(".sai/index");

        write_snapshot(&index_directory, &journal.index_before);
        write_current_journal(&index_directory, &journal);
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Before
        );

        write_snapshot(&index_directory, &journal.index_after);
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::PlannedAfter
        );
    }

    #[test]
    fn observes_recoverable_partial_generation_without_hashing_affected_mixed_side() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let journal = journal();
        let index_directory = lock.package_directory().join(".sai/index");

        write_snapshot(&index_directory, &journal.index_after);
        write_current_journal(&index_directory, &journal);
        write_file_state(
            &index_directory.join(journal.index_before.package_files[0].file.as_str()),
            &journal.index_before.package_files[0].state,
        );

        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::RecoverablePartial
        );
    }

    #[test]
    fn checks_the_current_journal_and_rejects_a_stale_argument() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let current = journal();
        let index_directory = lock.package_directory().join(".sai/index");
        write_snapshot(&index_directory, &current.index_before);
        write_current_journal(&index_directory, &current);

        let mut stale = current.clone();
        stale.operation_id = "27d632c8-f4b3-41f9-9cc2-3cf1f95e95fc".into();
        stale.encode().expect("stale argument remains valid");
        assert_eq!(
            store.observe_index_generation(&stale).unwrap(),
            IndexGenerationObservation::Unavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn revalidates_the_current_operation_record_after_observing_files() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let journal = journal();
        let index_directory = lock.package_directory().join(".sai/index");
        write_snapshot(&index_directory, &journal.index_before);
        write_current_journal(&index_directory, &journal);

        let mut replacement = journal.clone();
        replacement.operation_id = "27d632c8-f4b3-41f9-9cc2-3cf1f95e95fc".into();
        let observed = store
            .observe_index_generation_with_hook(&journal, || {
                write_current_journal(&index_directory, &replacement);
            })
            .unwrap();
        assert_eq!(observed, IndexGenerationObservation::Unavailable);
    }

    #[cfg(unix)]
    #[test]
    fn revalidates_every_scanned_file_after_the_initial_scan() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let (journal, unaffected_file) = journal_with_unaffected_file();
        let index_directory = lock.package_directory().join(".sai/index");
        write_snapshot(&index_directory, &journal.index_after);
        write_current_journal(&index_directory, &journal);
        let unaffected_path = index_directory.join(unaffected_file.as_str());
        let held_path = index_directory.join("held-unaffected.jsonl");
        fs::write(&unaffected_path, []).unwrap();
        fs::rename(&unaffected_path, &held_path).unwrap();
        fs::write(&unaffected_path, []).unwrap();

        let observed = store
            .observe_index_generation_with_hook(&journal, || {
                fs::remove_file(&unaffected_path).unwrap();
                fs::rename(&held_path, &unaffected_path).unwrap();
            })
            .unwrap();
        assert_eq!(observed, IndexGenerationObservation::Unavailable);
    }

    #[test]
    fn observes_recorded_orphans_exactly_and_allows_before_side_orphan_after_publish() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let (journal, orphan_bytes) = journal_before_orphan();
        let index_directory = lock.package_directory().join(".sai/index");

        write_snapshot(&index_directory, &journal.index_before);
        write_current_journal(&index_directory, &journal);
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Before,
            "the exact recorded bytes of an unlisted orphan match the before generation"
        );
        assert_eq!(
            fs::read(index_directory.join(journal.index_before.package_files[0].file.as_str()))
                .unwrap(),
            orphan_bytes,
            "observation leaves the orphan unchanged"
        );

        fs::write(
            index_directory.join(journal.index_before.package_files[0].file.as_str()),
            b"unexpected orphan bytes",
        )
        .unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Contradictory
        );

        write_snapshot(&index_directory, &journal.index_after);
        fs::write(
            index_directory.join(journal.index_before.package_files[0].file.as_str()),
            &orphan_bytes,
        )
        .unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::RecoverablePartial,
            "an exact before-side orphan may remain after the after header is published"
        );
    }

    #[test]
    fn contradicts_a_third_header_and_unexpected_affected_bytes() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let journal = journal();
        let index_directory = lock.package_directory().join(".sai/index");
        fs::create_dir_all(&index_directory).unwrap();
        write_current_journal(&index_directory, &journal);
        let package_file = &journal.index_before.package_files[0].file;
        let third_header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{package_file}\",\"blake3\":\"{}\"}}]}}\n",
            hash_bytes(b"third generation").to_hex()
        );
        fs::write(index_directory.join("index.json"), third_header).unwrap();
        let FileState::RegularFile(before_bytes) = &journal.index_before.package_files[0].state
        else {
            panic!("fixture before package file is present");
        };
        fs::write(index_directory.join(package_file.as_str()), before_bytes).unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Contradictory
        );

        write_snapshot(&index_directory, &journal.index_after);
        fs::write(
            index_directory.join(package_file.as_str()),
            b"unexpected bytes",
        )
        .unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Contradictory
        );
    }

    #[test]
    fn contradicts_missing_or_corrupt_unaffected_header_files() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let (journal, unaffected_file) = journal_with_unaffected_file();
        let index_directory = lock.package_directory().join(".sai/index");

        write_snapshot(&index_directory, &journal.index_after);
        write_current_journal(&index_directory, &journal);
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Contradictory,
            "a listed but missing unaffected file contradicts the generation"
        );

        fs::write(index_directory.join(unaffected_file.as_str()), b"corrupt").unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Contradictory,
            "a listed unaffected file with a wrong digest contradicts the generation"
        );
    }

    #[test]
    fn streams_and_verifies_unaffected_files_for_both_generations() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let (journal, unaffected_file) = journal_with_unaffected_file();
        let index_directory = lock.package_directory().join(".sai/index");

        write_snapshot(&index_directory, &journal.index_before);
        write_current_journal(&index_directory, &journal);
        fs::write(index_directory.join(unaffected_file.as_str()), []).unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Before
        );

        write_snapshot(&index_directory, &journal.index_after);
        fs::write(index_directory.join(unaffected_file.as_str()), []).unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::PlannedAfter
        );
    }

    #[test]
    fn missing_index_directory_header_and_unlisted_orphan_are_observed_exactly() {
        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let absent_before = journal_before_absent();
        assert_eq!(
            store.observe_index_generation(&absent_before).unwrap(),
            IndexGenerationObservation::Unavailable,
            "a missing index directory cannot contain the matching current operation record"
        );

        let index_directory = lock.package_directory().join(".sai/index");
        fs::create_dir_all(&index_directory).unwrap();
        assert_eq!(
            store.observe_index_generation(&absent_before).unwrap(),
            IndexGenerationObservation::Unavailable,
            "a missing operation record cannot authorize generation classification"
        );
        write_current_journal(&index_directory, &absent_before);
        assert_eq!(
            store.observe_index_generation(&absent_before).unwrap(),
            IndexGenerationObservation::Before,
            "a missing header and missing affected file match recorded absence"
        );

        let empty_header_before = journal_before_empty_header();
        write_snapshot(&index_directory, &empty_header_before.index_before);
        write_current_journal(&index_directory, &empty_header_before);
        assert_eq!(
            store
                .observe_index_generation(&empty_header_before)
                .unwrap(),
            IndexGenerationObservation::Before,
            "an absent affected orphan matches its exact before snapshot"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_index_directory_header_and_package_targets_are_unavailable() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let lock = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let store = IndexOperationJournalStore::new(&lock);
        let journal = journal();
        let index_directory = lock.package_directory().join(".sai/index");
        let external = directory.0.join("external");
        fs::create_dir_all(&external).unwrap();
        write_snapshot(&external, &journal.index_before);
        write_current_journal(&external, &journal);
        symlink(&external, lock.package_directory().join(".sai/index")).unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Unavailable
        );

        fs::remove_file(lock.package_directory().join(".sai/index")).unwrap();
        fs::create_dir_all(&index_directory).unwrap();
        let header_target = directory.0.join("header-target");
        let FileState::RegularFile(header_bytes) = &journal.index_before.header else {
            panic!("fixture before header is present");
        };
        fs::write(&header_target, header_bytes).unwrap();
        write_current_journal(&index_directory, &journal);
        symlink(&header_target, index_directory.join("index.json")).unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Unavailable
        );

        fs::remove_file(index_directory.join("index.json")).unwrap();
        fs::write(index_directory.join("index.json"), header_bytes).unwrap();
        let package_target = directory.0.join("package-target");
        let FileState::RegularFile(package_bytes) = &journal.index_before.package_files[0].state
        else {
            panic!("fixture before package file is present");
        };
        fs::write(&package_target, package_bytes).unwrap();
        write_current_journal(&index_directory, &journal);
        symlink(
            &package_target,
            index_directory.join(journal.index_before.package_files[0].file.as_str()),
        )
        .unwrap();
        assert_eq!(
            store.observe_index_generation(&journal).unwrap(),
            IndexGenerationObservation::Unavailable
        );
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
