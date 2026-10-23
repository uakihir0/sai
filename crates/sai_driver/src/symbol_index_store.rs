//! Durable filesystem access for the symbol index.

use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
use sai_semantic::{PackageIndexFileName, SymbolIndex, SymbolIndexHeader, UnvalidatedSymbolIndex};
use sai_windows_fs::{
    ConditionalFileUpdate, FileIdentity, create_child_directory_without_following, file_identity,
    open_child_directory_for_file_updates_without_following,
    open_file_for_stable_read_without_following, remove_file_in_directory_if_unchanged,
    sync_directory, update_file_in_directory_if_matches,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const INDEX_DIRECTORY_NAME: &str = "index";
const HEADER_FILE_NAME: &str = "index.json";
// The index format does not define a size limit. This implementation applies
// an explicit 256 MiB read ceiling to one header or package file and to the
// total observed package-file generation. Oversized files fail closed; they
// are not misclassified as corrupt and are never discarded automatically.
const MAX_SYMBOL_INDEX_FILE_BYTES: usize = 256 * 1024 * 1024;
const MAX_SYMBOL_INDEX_GENERATION_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
struct ObservedIndexFile {
    identity: FileIdentity,
    bytes: Arc<[u8]>,
}

impl ObservedIndexFile {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Result of reconciling the complete index for one package with no symbols.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EmptySymbolIndexReconciliation {
    /// The canonical header and zero-byte package file were already present.
    Unchanged,
    /// The canonical index was published, optionally after discarding an
    /// invalid committed index.
    Rebuilt { discarded_reason: Option<String> },
}

/// Errors while safely reading or publishing a package's symbol index.
#[derive(Debug)]
pub enum EmptySymbolIndexStoreError {
    LockNotExclusive,
    ReadIo {
        path: PathBuf,
        source: io::Error,
    },
    WriteIo {
        path: PathBuf,
        source: io::Error,
    },
    FileTooLarge {
        path: PathBuf,
        maximum_bytes: usize,
    },
    AllocationFailed {
        path: PathBuf,
        requested_bytes: usize,
    },
    UnstableRead {
        path: PathBuf,
    },
    Codec(sai_semantic::SymbolIndexCodecError),
}

impl fmt::Display for EmptySymbolIndexStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockNotExclusive => formatter
                .write_str("symbol-index reconciliation requires an exclusive workspace lock"),
            Self::ReadIo { path, source } => {
                write!(
                    formatter,
                    "could not read symbol index at {path:?}: {source}"
                )
            }
            Self::WriteIo { path, source } => {
                write!(
                    formatter,
                    "could not publish symbol index at {path:?}: {source}"
                )
            }
            Self::FileTooLarge {
                path,
                maximum_bytes,
            } => write!(
                formatter,
                "symbol-index file at {path:?} exceeds the {maximum_bytes}-byte safe read limit"
            ),
            Self::AllocationFailed {
                path,
                requested_bytes,
            } => write!(
                formatter,
                "could not reserve {requested_bytes} bytes to read symbol index at {path:?}"
            ),
            Self::UnstableRead { path } => {
                write!(
                    formatter,
                    "symbol-index file changed while being read at {path:?}"
                )
            }
            Self::Codec(source) => write!(
                formatter,
                "could not encode canonical symbol index: {source}"
            ),
        }
    }
}

impl Error for EmptySymbolIndexStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadIo { source, .. } | Self::WriteIo { source, .. } => Some(source),
            Self::Codec(source) => Some(source),
            Self::LockNotExclusive
            | Self::FileTooLarge { .. }
            | Self::AllocationFailed { .. }
            | Self::UnstableRead { .. } => None,
        }
    }
}

impl EmptySymbolIndexStoreError {
    /// Filesystem path associated with this failure, when one is available.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::ReadIo { path, .. }
            | Self::WriteIo { path, .. }
            | Self::FileTooLarge { path, .. }
            | Self::AllocationFailed { path, .. }
            | Self::UnstableRead { path } => Some(path),
            Self::LockNotExclusive | Self::Codec(_) => None,
        }
    }

    /// Whether the failure occurred while publishing index files.
    pub const fn is_write_failure(&self) -> bool {
        matches!(self, Self::WriteIo { .. } | Self::Codec(_))
    }
}

/// Reads, validates, and durably reconciles one complete package index whose
/// current semantic inventory is known to be empty.
///
/// The caller must first settle the operation journal under the same lock.
/// This function changes the selected package file and `index.json`, then
/// removes obsolete files listed by a valid old header after the new header
/// is durable. Unlisted orphans are not enumerated or removed. It publishes
/// the zero-byte package file before the header, with durable directory
/// synchronization at every publication boundary. Directory durability
/// failures are returned; callers must keep index-backed work gated and
/// reread before retrying.
pub fn reconcile_empty_symbol_index(
    lock: &WorkspaceLockGuard,
    file: PackageIndexFileName,
) -> Result<EmptySymbolIndexReconciliation, EmptySymbolIndexStoreError> {
    reconcile_empty_symbol_index_inner(lock, file, false)
}

/// Reconciles the first empty generation after journal recovery restored an
/// absent before header. In this one case, an existing `.sai/index/` directory
/// without `index.json` is the exact recovered empty state, not corruption.
pub fn reconcile_empty_symbol_index_after_journal_rollback(
    lock: &WorkspaceLockGuard,
    file: PackageIndexFileName,
) -> Result<EmptySymbolIndexReconciliation, EmptySymbolIndexStoreError> {
    reconcile_empty_symbol_index_inner(lock, file, true)
}

fn reconcile_empty_symbol_index_inner(
    lock: &WorkspaceLockGuard,
    file: PackageIndexFileName,
    missing_header_is_recovered_empty: bool,
) -> Result<EmptySymbolIndexReconciliation, EmptySymbolIndexStoreError> {
    reconcile_empty_symbol_index_with(
        lock,
        file,
        missing_header_is_recovered_empty,
        sync_directory,
        |directory, name, expected, desired| {
            update_file_in_directory_if_matches(directory, name, expected, desired)
        },
        || {},
    )
}

fn reconcile_empty_symbol_index_with(
    lock: &WorkspaceLockGuard,
    file: PackageIndexFileName,
    missing_header_is_recovered_empty: bool,
    mut sync_directory_fn: impl FnMut(&File) -> io::Result<()>,
    mut update_file_fn: impl FnMut(
        &File,
        &OsStr,
        Option<&[u8]>,
        Option<&[u8]>,
    ) -> io::Result<ConditionalFileUpdate>,
    after_observation: impl FnOnce(),
) -> Result<EmptySymbolIndexReconciliation, EmptySymbolIndexStoreError> {
    if lock.mode() != WorkspaceLockMode::Exclusive {
        return Err(EmptySymbolIndexStoreError::LockNotExclusive);
    }

    let path = lock.path();
    let state_directory_path = path
        .parent()
        .expect("workspace lock path has a state-directory parent");
    let index_directory_path = state_directory_path.join(INDEX_DIRECTORY_NAME);
    verify_bound_state_directory(lock, state_directory_path)
        .map_err(|source| write_io_error(state_directory_path.to_path_buf(), source))?;
    // Acquiring the workspace lock may have just created `.sai`. Persist that
    // directory entry in its package-root parent before any index publication
    // can be reported as durable. Windows remains read-only here because its
    // directory-flush implementation is intentionally unsupported.
    #[cfg(unix)]
    {
        let package_directory = lock.package_directory_handle().ok_or_else(|| {
            write_io_error(
                lock.package_directory().to_path_buf(),
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "workspace root directory handle is unavailable",
                ),
            )
        })?;
        (sync_directory_fn)(package_directory)
            .map_err(|source| write_io_error(lock.package_directory().to_path_buf(), source))?;
    }
    let (index_directory, created_index_directory) = open_or_create_index_directory(
        lock.state_directory_handle(),
        state_directory_path,
        &index_directory_path,
        &mut sync_directory_fn,
    )?;
    verify_bound_index_directory(
        lock,
        &index_directory,
        state_directory_path,
        &index_directory_path,
    )
    .map_err(|source| write_io_error(index_directory_path.clone(), source))?;

    let encoded = SymbolIndex::empty_for_package(file.clone())
        .encode()
        .map_err(EmptySymbolIndexStoreError::Codec)?;
    let expected_header = encoded.header();
    let expected_package_file = encoded
        .package_files()
        .get(&file)
        .expect("empty symbol index contains its package file");

    let header_path = index_directory_path.join(HEADER_FILE_NAME);
    let current_header = read_child_file(
        &index_directory,
        &index_directory_path,
        OsStr::new(HEADER_FILE_NAME),
        &header_path,
    )?;
    let package_path = index_directory_path.join(file.as_str());
    let current_package_file = read_child_file(
        &index_directory,
        &index_directory_path,
        OsStr::new(file.as_str()),
        &package_path,
    )?;

    let (discarded_reason, obsolete_files) = observe_current_generation(
        &index_directory,
        &index_directory_path,
        current_header.as_ref(),
        &file,
        current_package_file.as_ref(),
        !created_index_directory,
        missing_header_is_recovered_empty,
    )?;
    after_observation();
    verify_bound_index_directory(
        lock,
        &index_directory,
        state_directory_path,
        &index_directory_path,
    )
    .map_err(|source| write_io_error(index_directory_path.clone(), source))?;

    let mut changed = false;
    if current_package_file.as_ref().map(ObservedIndexFile::bytes)
        != Some(expected_package_file.as_slice())
    {
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        let result = update_file_fn(
            &index_directory,
            OsStr::new(file.as_str()),
            current_package_file.as_ref().map(ObservedIndexFile::bytes),
            Some(expected_package_file),
        )
        .map_err(|source| write_io_error(package_path.clone(), source))?;
        changed |= result == ConditionalFileUpdate::Updated;
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
    }

    if current_header.as_ref().map(ObservedIndexFile::bytes) != Some(expected_header) {
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        let result = update_file_fn(
            &index_directory,
            OsStr::new(HEADER_FILE_NAME),
            current_header.as_ref().map(ObservedIndexFile::bytes),
            Some(expected_header),
        )
        .map_err(|source| write_io_error(header_path, source))?;
        changed |= result == ConditionalFileUpdate::Updated;
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
    }

    // The newly published header is the commit point. Only now may package
    // files from the previously committed header that are absent from the
    // new generation be removed. Arbitrary unlisted orphans are never
    // enumerated or touched.
    let mut removed_obsolete_file = false;
    for (obsolete_file, observed) in obsolete_files {
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        match remove_file_in_directory_if_unchanged(
            &index_directory,
            OsStr::new(obsolete_file.as_str()),
            observed.identity,
            observed.bytes(),
        ) {
            Ok(()) => removed_obsolete_file = true,
            // External replacement or removal after observation means the
            // old path is now an ignored orphan or is already gone. Preserve
            // the changed path rather than deleting it from a stale snapshot.
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(write_io_error(
                    index_directory_path.join(obsolete_file.as_str()),
                    source,
                ));
            }
        }
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
    }
    if removed_obsolete_file {
        (sync_directory_fn)(&index_directory)
            .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
    }

    if !changed && discarded_reason.is_none() {
        // A previous invocation may have completed a rename before its
        // directory flush failed. On platforms where this store can publish,
        // a canonical retry must establish namespace durability before it
        // reports success. Non-Unix platforms cannot publish through this
        // API today and keep canonical reads read-only.
        #[cfg(unix)]
        (sync_directory_fn)(&index_directory)
            .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        Ok(EmptySymbolIndexReconciliation::Unchanged)
    } else {
        verify_bound_index_directory(
            lock,
            &index_directory,
            state_directory_path,
            &index_directory_path,
        )
        .map_err(|source| write_io_error(index_directory_path.clone(), source))?;
        Ok(EmptySymbolIndexReconciliation::Rebuilt { discarded_reason })
    }
}

fn open_or_create_index_directory(
    state_directory: &File,
    state_directory_path: &Path,
    index_directory_path: &Path,
    sync_directory_fn: &mut impl FnMut(&File) -> io::Result<()>,
) -> Result<(File, bool), EmptySymbolIndexStoreError> {
    match open_child_directory_for_file_updates_without_following(
        state_directory,
        state_directory_path,
        OsStr::new(INDEX_DIRECTORY_NAME),
    ) {
        Ok(directory) => Ok((directory, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Preflight directory durability before creating the index
            // directory. Platforms without a supported flush fail closed.
            sync_directory_fn(state_directory)
                .map_err(|source| write_io_error(state_directory_path.to_path_buf(), source))?;
            let directory = create_child_directory_without_following(
                state_directory,
                OsStr::new(INDEX_DIRECTORY_NAME),
            )
            .map_err(|source| write_io_error(index_directory_path.to_path_buf(), source))?;
            sync_directory_fn(state_directory)
                .map_err(|source| write_io_error(state_directory_path.to_path_buf(), source))?;
            Ok((directory, true))
        }
        Err(source) => Err(read_io_error(index_directory_path.to_path_buf(), source)),
    }
}

fn verify_bound_state_directory(
    lock: &WorkspaceLockGuard,
    state_directory_path: &Path,
) -> io::Result<()> {
    let _ = open_bound_state_directory(lock, state_directory_path)?;
    Ok(())
}

fn open_bound_state_directory(
    lock: &WorkspaceLockGuard,
    state_directory_path: &Path,
) -> io::Result<File> {
    let package_directory = lock.package_directory_handle().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace root identity is unavailable",
        )
    })?;
    let current_state_directory = open_child_directory_for_file_updates_without_following(
        package_directory,
        lock.package_directory(),
        OsStr::new(".sai"),
    )?;
    if file_identity(&current_state_directory)? != file_identity(lock.state_directory_handle())? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("workspace state directory identity changed at {state_directory_path:?}"),
        ));
    }
    Ok(current_state_directory)
}

fn verify_bound_index_directory(
    lock: &WorkspaceLockGuard,
    index_directory: &File,
    state_directory_path: &Path,
    index_directory_path: &Path,
) -> io::Result<()> {
    let current_state_directory = open_bound_state_directory(lock, state_directory_path)?;
    let current_index_directory = open_child_directory_for_file_updates_without_following(
        &current_state_directory,
        state_directory_path,
        OsStr::new(INDEX_DIRECTORY_NAME),
    )?;
    if file_identity(&current_index_directory)? != file_identity(index_directory)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("symbol-index directory identity changed at {index_directory_path:?}"),
        ));
    }
    Ok(())
}

fn read_child_file(
    directory: &File,
    directory_path: &Path,
    name: &OsStr,
    file_path: &Path,
) -> Result<Option<ObservedIndexFile>, EmptySymbolIndexStoreError> {
    let mut file =
        match open_file_for_stable_read_without_following(directory, directory_path, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(read_io_error(file_path.to_path_buf(), source)),
        };
    let identity =
        file_identity(&file).map_err(|source| read_io_error(file_path.to_path_buf(), source))?;
    let first = read_bounded(&mut file, file_path)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| read_io_error(file_path.to_path_buf(), source))?;
    let second = read_bounded(&mut file, file_path)?;
    let metadata = file
        .metadata()
        .map_err(|source| read_io_error(file_path.to_path_buf(), source))?;
    let final_identity =
        file_identity(&file).map_err(|source| read_io_error(file_path.to_path_buf(), source))?;
    if first != second || !metadata.is_file() || identity != final_identity {
        return Err(EmptySymbolIndexStoreError::UnstableRead {
            path: file_path.to_path_buf(),
        });
    }
    Ok(Some(ObservedIndexFile {
        identity,
        bytes: first.into(),
    }))
}

fn read_bounded(file: &mut File, path: &Path) -> Result<Vec<u8>, EmptySymbolIndexStoreError> {
    let metadata = file
        .metadata()
        .map_err(|source| read_io_error(path.to_path_buf(), source))?;
    if !metadata.is_file() {
        return Err(read_io_error(
            path.to_path_buf(),
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbol-index path is not a regular file",
            ),
        ));
    }
    let length =
        usize::try_from(metadata.len()).map_err(|_| EmptySymbolIndexStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes: MAX_SYMBOL_INDEX_FILE_BYTES,
        })?;
    if length > MAX_SYMBOL_INDEX_FILE_BYTES {
        return Err(EmptySymbolIndexStoreError::FileTooLarge {
            path: path.to_path_buf(),
            maximum_bytes: MAX_SYMBOL_INDEX_FILE_BYTES,
        });
    }
    let read_length =
        length
            .checked_add(1)
            .ok_or_else(|| EmptySymbolIndexStoreError::FileTooLarge {
                path: path.to_path_buf(),
                maximum_bytes: MAX_SYMBOL_INDEX_FILE_BYTES,
            })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(read_length).map_err(|_| {
        EmptySymbolIndexStoreError::AllocationFailed {
            path: path.to_path_buf(),
            requested_bytes: read_length,
        }
    })?;
    file.take(read_length as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| read_io_error(path.to_path_buf(), source))?;
    if bytes.len() > MAX_SYMBOL_INDEX_FILE_BYTES || bytes.len() != length {
        return Err(EmptySymbolIndexStoreError::UnstableRead {
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

fn observe_current_generation(
    directory: &File,
    directory_path: &Path,
    current_header: Option<&ObservedIndexFile>,
    file: &PackageIndexFileName,
    current_package_file: Option<&ObservedIndexFile>,
    index_directory_existed: bool,
    missing_header_is_recovered_empty: bool,
) -> Result<
    (
        Option<String>,
        Vec<(PackageIndexFileName, ObservedIndexFile)>,
    ),
    EmptySymbolIndexStoreError,
> {
    let Some(header_file) = current_header else {
        return Ok((
            if index_directory_existed && !missing_header_is_recovered_empty {
                Some("the index header is missing from the existing index directory".to_owned())
            } else {
                None
            },
            Vec::new(),
        ));
    };
    let decoded_header = match SymbolIndexHeader::decode(header_file.bytes()) {
        Ok(header) => header,
        Err(error) => return Ok((Some(error.to_string()), Vec::new())),
    };

    let mut package_files = BTreeMap::new();
    let mut obsolete_files = Vec::new();
    let mut total_package_bytes = 0usize;
    for record in decoded_header.records() {
        let package_file = record.file();
        let observed = if package_file == file {
            current_package_file.cloned()
        } else {
            let package_path = directory_path.join(package_file.as_str());
            match read_child_file(
                directory,
                directory_path,
                OsStr::new(package_file.as_str()),
                &package_path,
            ) {
                Ok(observed) => observed,
                Err(error) => return Err(error),
            }
        };
        let Some(observed) = observed else {
            continue;
        };
        total_package_bytes = total_package_bytes
            .checked_add(observed.bytes().len())
            .ok_or_else(|| EmptySymbolIndexStoreError::FileTooLarge {
                path: directory_path.to_path_buf(),
                maximum_bytes: MAX_SYMBOL_INDEX_GENERATION_BYTES,
            })?;
        if total_package_bytes > MAX_SYMBOL_INDEX_GENERATION_BYTES {
            return Err(EmptySymbolIndexStoreError::FileTooLarge {
                path: directory_path.join(package_file.as_str()),
                maximum_bytes: MAX_SYMBOL_INDEX_GENERATION_BYTES,
            });
        }
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(observed.bytes().len())
            .map_err(|_| EmptySymbolIndexStoreError::AllocationFailed {
                path: directory_path.join(package_file.as_str()),
                requested_bytes: observed.bytes().len(),
            })?;
        snapshot.extend_from_slice(observed.bytes());
        package_files.insert(package_file.as_str().to_owned(), snapshot);
        if package_file != file {
            obsolete_files.push((package_file.clone(), observed));
        }
    }

    match UnvalidatedSymbolIndex::decode(header_file.bytes(), &package_files) {
        Ok(_) => Ok((None, obsolete_files)),
        Err(error) => Ok((Some(error.to_string()), obsolete_files)),
    }
}

fn read_io_error(path: PathBuf, source: io::Error) -> EmptySymbolIndexStoreError {
    EmptySymbolIndexStoreError::ReadIo { path, source }
}

fn write_io_error(path: PathBuf, source: io::Error) -> EmptySymbolIndexStoreError {
    EmptySymbolIndexStoreError::WriteIo { path, source }
}

#[cfg(all(
    test,
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
mod tests {
    use super::*;
    use sai_package::workspace_lock::WorkspaceLockGuard;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct TestWorkspace(PathBuf);

    impl TestWorkspace {
        fn new() -> Self {
            let id = NEXT_TEST_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "sai-empty-symbol-index-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create workspace");
            Self(root)
        }

        fn lock(&self) -> WorkspaceLockGuard {
            WorkspaceLockGuard::acquire(&self.0, WorkspaceLockMode::Exclusive)
                .expect("acquire exclusive lock")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn empty_index_can_be_created_and_then_is_a_noop() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        assert!(matches!(
            reconcile_empty_symbol_index(&lock, file.clone()).unwrap(),
            EmptySymbolIndexReconciliation::Rebuilt {
                discarded_reason: None
            }
        ));
        let package_path = workspace.0.join(".sai/index/acme.app.0.1.jsonl");
        let header_path = workspace.0.join(".sai/index/index.json");
        let package_identity = file_identity(&File::open(&package_path).unwrap()).unwrap();
        let header_identity = file_identity(&File::open(&header_path).unwrap()).unwrap();
        assert_eq!(
            fs::read(&package_path).unwrap(),
            b"",
            "empty inventory has a zero-byte package file"
        );
        assert_eq!(
            reconcile_empty_symbol_index(&lock, file).unwrap(),
            EmptySymbolIndexReconciliation::Unchanged
        );
        assert_eq!(
            file_identity(&File::open(package_path).unwrap()).unwrap(),
            package_identity
        );
        assert_eq!(
            file_identity(&File::open(header_path).unwrap()).unwrap(),
            header_identity
        );
    }

    #[test]
    fn corrupt_header_is_reported_and_rebuilt() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("index.json"), b"{corrupt\n").unwrap();
        fs::write(directory.join("acme.app.0.1.jsonl"), b"old entry\n").unwrap();

        let result = reconcile_empty_symbol_index(
            &lock,
            PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap(),
        )
        .unwrap();
        let EmptySymbolIndexReconciliation::Rebuilt { discarded_reason } = result else {
            panic!("corrupt index is rebuilt");
        };
        assert_eq!(
            discarded_reason.as_deref(),
            Some("invalid symbol-index JSON")
        );
        assert_eq!(fs::read(directory.join("acme.app.0.1.jsonl")).unwrap(), b"");
        let expected = SymbolIndex::empty_for_package(
            PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap(),
        )
        .encode()
        .unwrap();
        assert_eq!(
            fs::read(directory.join("index.json")).unwrap(),
            expected.header()
        );
    }

    #[test]
    fn valid_but_stale_entries_are_reconciled_without_discard_diagnostic() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let old_package = br#"{"hex":"000001","def_path_hash":"00000000000000000000000000000001","path":"acme:app.fn","kind":"fn","fingerprint":"00000000000000000000000000000001"}
"#;
        let old_header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{}\",\"blake3\":\"{}\"}}]}}\n",
            file.as_str(),
            sai_stable_hash::hash_bytes(old_package)
        );
        fs::write(directory.join("index.json"), old_header).unwrap();
        fs::write(directory.join(file.as_str()), old_package).unwrap();

        assert_eq!(
            reconcile_empty_symbol_index(&lock, file.clone()).unwrap(),
            EmptySymbolIndexReconciliation::Rebuilt {
                discarded_reason: None
            },
            "a valid previous generation with stale inventory is reconciled, not discarded"
        );
        let expected = SymbolIndex::empty_for_package(file.clone())
            .encode()
            .unwrap();
        assert_eq!(fs::read(directory.join(file.as_str())).unwrap(), b"");
        assert_eq!(
            fs::read(directory.join("index.json")).unwrap(),
            expected.header()
        );
    }

    #[test]
    fn unreadable_committed_package_fails_closed_without_replacing_index() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let selected = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let committed = PackageIndexFileName::parse("other.pkg.1.jsonl").unwrap();
        let committed_bytes = b"committed bytes\n";
        let header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{}\",\"blake3\":\"{}\"}}]}}\n",
            committed.as_str(),
            sai_stable_hash::hash_bytes(committed_bytes)
        );
        fs::write(directory.join("index.json"), &header).unwrap();
        fs::write(directory.join(committed.as_str()), committed_bytes).unwrap();
        let selected_before = b"preserve selected bytes\n";
        fs::write(directory.join(selected.as_str()), selected_before).unwrap();
        fs::remove_file(directory.join(committed.as_str())).unwrap();
        fs::create_dir(directory.join(committed.as_str())).unwrap();

        assert!(matches!(
            reconcile_empty_symbol_index(&lock, selected.clone()),
            Err(EmptySymbolIndexStoreError::ReadIo { .. })
        ));
        assert_eq!(
            fs::read(directory.join("index.json")).unwrap(),
            header.as_bytes()
        );
        assert_eq!(
            fs::read(directory.join(selected.as_str())).unwrap(),
            selected_before
        );
        assert!(directory.join(committed.as_str()).is_dir());
    }

    #[test]
    fn missing_header_in_existing_directory_is_reported_and_rebuilt() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("unlisted-orphan.jsonl"), b"preserve\n").unwrap();

        let result = reconcile_empty_symbol_index(
            &lock,
            PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap(),
        )
        .unwrap();
        assert_eq!(
            result,
            EmptySymbolIndexReconciliation::Rebuilt {
                discarded_reason: Some(
                    "the index header is missing from the existing index directory".to_owned()
                )
            }
        );
        assert_eq!(
            fs::read(directory.join("unlisted-orphan.jsonl")).unwrap(),
            b"preserve\n",
            "unlisted orphans are not part of the committed set"
        );
    }

    #[test]
    fn removes_only_old_header_files_after_rebuilding_the_generation() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let selected = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let obsolete = PackageIndexFileName::parse("other.pkg.1.jsonl").unwrap();
        let empty_digest = sai_stable_hash::hash_bytes(b"").to_hex();
        let old_header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{}\",\"blake3\":\"{}\"}},{{\"file\":\"{}\",\"blake3\":\"{}\"}}]}}\n",
            selected.as_str(),
            empty_digest,
            obsolete.as_str(),
            empty_digest
        );
        fs::write(directory.join("index.json"), old_header).unwrap();
        fs::write(directory.join(selected.as_str()), b"").unwrap();
        fs::write(directory.join(obsolete.as_str()), b"").unwrap();
        fs::write(directory.join("orphan.jsonl"), b"preserve\n").unwrap();

        let result = reconcile_empty_symbol_index(&lock, selected.clone()).unwrap();
        assert!(matches!(
            result,
            EmptySymbolIndexReconciliation::Rebuilt {
                discarded_reason: None
            }
        ));
        assert!(
            !directory.join(obsolete.as_str()).exists(),
            "a file in the old committed header is removed after the new header is durable"
        );
        assert_eq!(
            fs::read(directory.join("orphan.jsonl")).unwrap(),
            b"preserve\n",
            "arbitrary unlisted orphans are retained"
        );
        let expected = SymbolIndex::empty_for_package(selected).encode().unwrap();
        assert_eq!(
            fs::read(directory.join("index.json")).unwrap(),
            expected.header()
        );
    }

    #[test]
    fn package_publication_error_does_not_publish_the_header() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let mut updates = 0;

        let error = reconcile_empty_symbol_index_with(
            &lock,
            file.clone(),
            false,
            sync_directory,
            |parent, name, expected, desired| {
                let result = update_file_in_directory_if_matches(parent, name, expected, desired)?;
                updates += 1;
                if updates == 1 {
                    Err(io::Error::other(
                        "injected package publication flush failure",
                    ))
                } else {
                    Ok(result)
                }
            },
            || {},
        )
        .unwrap_err();
        assert!(matches!(error, EmptySymbolIndexStoreError::WriteIo { .. }));
        assert_eq!(
            fs::read(directory.join(file.as_str())).unwrap(),
            b"",
            "the package file may be visible after an uncertain publication result"
        );
        assert!(
            !directory.join(HEADER_FILE_NAME).exists(),
            "the header remains the commit point and is not published after package failure"
        );

        assert!(matches!(
            reconcile_empty_symbol_index(&lock, file),
            Ok(EmptySymbolIndexReconciliation::Rebuilt {
                discarded_reason: Some(_)
            })
        ));
    }

    #[test]
    fn header_publication_error_is_reconciled_by_a_durable_noop_retry() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let mut updates = 0;

        let error = reconcile_empty_symbol_index_with(
            &lock,
            file.clone(),
            false,
            sync_directory,
            |parent, name, expected, desired| {
                let result = update_file_in_directory_if_matches(parent, name, expected, desired)?;
                updates += 1;
                if updates == 2 {
                    Err(io::Error::other(
                        "injected header publication flush failure",
                    ))
                } else {
                    Ok(result)
                }
            },
            || {},
        )
        .unwrap_err();
        assert!(matches!(error, EmptySymbolIndexStoreError::WriteIo { .. }));
        assert!(directory.join(HEADER_FILE_NAME).exists());

        assert_eq!(
            reconcile_empty_symbol_index(&lock, file).unwrap(),
            EmptySymbolIndexReconciliation::Unchanged,
            "retry rereads canonical bytes and synchronizes the directory before success"
        );
    }

    #[test]
    fn canonical_noop_sync_failure_does_not_report_success() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        reconcile_empty_symbol_index(&lock, file.clone()).unwrap();
        let index_directory_identity =
            file_identity(&File::open(workspace.0.join(".sai/index")).unwrap()).unwrap();

        let error = reconcile_empty_symbol_index_with(
            &lock,
            file,
            false,
            |directory| {
                if file_identity(directory)? == index_directory_identity {
                    Err(io::Error::other("injected index directory sync failure"))
                } else {
                    sync_directory(directory)
                }
            },
            |parent, name, expected, desired| {
                update_file_in_directory_if_matches(parent, name, expected, desired)
            },
            || {},
        )
        .unwrap_err();
        assert!(matches!(error, EmptySymbolIndexStoreError::WriteIo { .. }));
    }

    #[test]
    fn package_root_sync_failure_prevents_index_publication() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let package_root_identity = file_identity(
            lock.package_directory_handle()
                .expect("pinned package root"),
        )
        .unwrap();
        let mut package_root_sync_attempted = false;

        let error = reconcile_empty_symbol_index_with(
            &lock,
            file,
            false,
            |directory| {
                if file_identity(directory)? == package_root_identity {
                    package_root_sync_attempted = true;
                    Err(io::Error::other(
                        "injected package-root directory sync failure",
                    ))
                } else {
                    sync_directory(directory)
                }
            },
            |parent, name, expected, desired| {
                update_file_in_directory_if_matches(parent, name, expected, desired)
            },
            || {},
        )
        .unwrap_err();

        assert!(package_root_sync_attempted);
        assert!(matches!(error, EmptySymbolIndexStoreError::WriteIo { .. }));
        assert!(
            !workspace.0.join(".sai/index").exists(),
            "a failed package-root sync must block index-directory publication"
        );
    }

    #[test]
    fn index_directory_rebinding_after_observation_fails_closed() {
        let workspace = TestWorkspace::new();
        let lock = workspace.lock();
        let directory = workspace.0.join(".sai/index");
        fs::create_dir_all(&directory).unwrap();
        let file = PackageIndexFileName::parse("acme.app.0.1.jsonl").unwrap();
        let expected = SymbolIndex::empty_for_package(file.clone())
            .encode()
            .unwrap();
        fs::write(directory.join(HEADER_FILE_NAME), expected.header()).unwrap();
        fs::write(
            directory.join(file.as_str()),
            expected
                .package_files()
                .get(&file)
                .expect("empty package file exists"),
        )
        .unwrap();

        let detached_directory = workspace.0.join(".sai/index-detached");
        let error = reconcile_empty_symbol_index_with(
            &lock,
            file,
            false,
            sync_directory,
            |parent, name, before, after| {
                update_file_in_directory_if_matches(parent, name, before, after)
            },
            || {
                fs::rename(&directory, &detached_directory).unwrap();
                fs::create_dir(&directory).unwrap();
            },
        )
        .unwrap_err();
        let EmptySymbolIndexStoreError::WriteIo { source, .. } = error else {
            panic!("directory rebinding is an output write failure");
        };
        assert_eq!(source.kind(), io::ErrorKind::WouldBlock);
        assert!(
            !directory.join(HEADER_FILE_NAME).exists(),
            "the replacement directory is not populated"
        );
        assert!(
            detached_directory.join(HEADER_FILE_NAME).exists(),
            "the detached directory is not reported as the current index"
        );
    }
}
