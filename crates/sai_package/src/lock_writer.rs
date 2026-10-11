//! Atomic persistence of canonical `sai.lock` files.

use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::lock::LockParseOutput;
use crate::lock_serialization::{LockSerializationError, serialize_canonical};
use crate::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};

const LOCK_FILE_NAME: &str = "sai.lock";
const TEMPORARY_PREFIX: &str = ".sai.lock.";
const RANDOM_SUFFIX_BYTES: usize = 16;
const TEMPORARY_CREATE_ATTEMPTS: usize = 8;

/// Whether writing a canonical lock changed the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockWriteOutcome {
    /// The destination already contained the canonical bytes.
    Unchanged,
    /// The canonical bytes replaced the destination.
    Written { bytes: usize },
}

/// Failure while validating the destination or atomically writing a lock.
#[derive(Debug)]
pub enum LockWriteError {
    /// This API only writes a file named `sai.lock`.
    InvalidPath(PathBuf),
    /// The destination exists but is not a regular file.
    DestinationNotRegularFile(PathBuf),
    /// Lock syntax or lock-local semantic validation failed.
    Serialization(LockSerializationError),
    /// A filesystem operation failed.
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The new lock is installed, but syncing its parent directory failed.
    InstalledButDirectorySyncFailed { path: PathBuf, source: io::Error },
}

/// Failure while writing a canonical lock under a workspace guard.
#[derive(Debug)]
pub enum WorkspaceLockWriteError {
    WorkspaceLockNotExclusive(WorkspaceLockMode),
    DestinationAlreadyExists(PathBuf),
    LockWrite(LockWriteError),
}

impl fmt::Display for WorkspaceLockWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspaceLockNotExclusive(_) => {
                formatter.write_str("an exclusive workspace lock is required to write sai.lock")
            }
            Self::DestinationAlreadyExists(path) => {
                write!(formatter, "sai.lock already exists: {path:?}")
            }
            Self::LockWrite(error) => error.fmt(formatter),
        }
    }
}

impl Error for WorkspaceLockWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkspaceLockNotExclusive(_) => None,
            Self::DestinationAlreadyExists(_) => None,
            Self::LockWrite(error) => Some(error),
        }
    }
}

impl fmt::Display for LockWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(
                    formatter,
                    "lock destination must be named sai.lock: {path:?}"
                )
            }
            Self::DestinationNotRegularFile(path) => {
                write!(
                    formatter,
                    "lock destination is not a regular file: {path:?}"
                )
            }
            Self::Serialization(error) => {
                write!(formatter, "cannot write an invalid lock: {error:?}")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "could not {operation} {path:?}: {source}"),
            Self::InstalledButDirectorySyncFailed { path, source } => write!(
                formatter,
                "installed the canonical lock at {path:?}, but could not sync its directory: {source}"
            ),
        }
    }
}

impl Error for LockWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::InstalledButDirectorySyncFailed { source, .. } => {
                Some(source)
            }
            Self::InvalidPath(_) | Self::DestinationNotRegularFile(_) | Self::Serialization(_) => {
                None
            }
        }
    }
}

/// Serializes a validated lock and atomically installs it at `path`.
///
/// The caller must hold the package directory's exclusive `.sai/lock` for the
/// complete operation. A noncanonical existing lock is rewritten even when
/// its canonical serialization is semantically equal to the output. Temporary
/// files are created beside the destination with mode `0600` on Unix, flushed
/// before replacement, and removed on failure. Stale regular temporary files
/// matching the generated 32-character lowercase hexadecimal suffix are
/// removed before writing. The destination directory is flushed on hosts that
/// support syncing directories.
///
/// The caller must also validate graph-dependent capability names in the
/// original parsed lock before calling this function. Canonical serialization
/// can remove covered capabilities, including unknown names, so this writer
/// can only perform lock-local validation.
pub(crate) fn write_canonical_lock_atomically(
    path: &Path,
    parsed: &LockParseOutput,
) -> Result<LockWriteOutcome, LockWriteError> {
    let serialized = serialize_canonical(parsed).map_err(LockWriteError::Serialization)?;
    let parent = destination_parent(path)?;

    // Remove leftovers even when the destination is already canonical. The
    // lock directory is serialized by the caller's exclusive workspace lock.
    remove_leftover_temporary_files(parent)?;
    if existing_regular_file_matches(path, serialized.as_bytes())? {
        return Ok(LockWriteOutcome::Unchanged);
    }

    let (temporary_path, mut temporary_file) = create_temporary_file(parent)?;
    let mut temporary = TemporaryLockFile {
        path: temporary_path.clone(),
        published: false,
    };
    temporary_file
        .write_all(serialized.as_bytes())
        .and_then(|()| temporary_file.sync_all())
        .map_err(|source| LockWriteError::Io {
            operation: "write temporary lock",
            path: temporary_path.clone(),
            source,
        })?;
    drop(temporary_file);

    sai_windows_fs::replace_file(&temporary_path, path).map_err(|source| LockWriteError::Io {
        operation: "atomically replace lock",
        path: path.to_path_buf(),
        source,
    })?;
    temporary.published = true;
    sync_parent_directory(parent).map_err(|source| {
        LockWriteError::InstalledButDirectorySyncFailed {
            path: path.to_path_buf(),
            source,
        }
    })?;

    Ok(LockWriteOutcome::Written {
        bytes: serialized.len(),
    })
}

/// Atomically write `sai.lock` while the caller holds the package's exclusive
/// workspace lock.
///
/// The guard captures the canonical package directory, so the lock and target
/// resolve to the same workspace. Commands should acquire the guard before
/// reading workspace files and retain it for their whole duration.
/// Graph-dependent capability validation must be complete before this call.
pub fn write_workspace_lock_atomically(
    guard: &WorkspaceLockGuard,
    parsed: &LockParseOutput,
) -> Result<LockWriteOutcome, WorkspaceLockWriteError> {
    if guard.mode() != WorkspaceLockMode::Exclusive {
        return Err(WorkspaceLockWriteError::WorkspaceLockNotExclusive(
            guard.mode(),
        ));
    }
    let path = guard.package_directory().join(LOCK_FILE_NAME);
    write_canonical_lock_atomically(&path, parsed).map_err(WorkspaceLockWriteError::LockWrite)
}

/// Create `sai.lock` only when no directory entry with that name exists.
///
/// The caller must hold the package's exclusive workspace lock. Participating
/// processes are serialized by that guard; unrelated filesystem mutation is
/// outside the workspace lock's coordination contract.
pub fn create_workspace_lock_atomically(
    guard: &WorkspaceLockGuard,
    parsed: &LockParseOutput,
) -> Result<LockWriteOutcome, WorkspaceLockWriteError> {
    if guard.mode() != WorkspaceLockMode::Exclusive {
        return Err(WorkspaceLockWriteError::WorkspaceLockNotExclusive(
            guard.mode(),
        ));
    }
    let path = guard.package_directory().join(LOCK_FILE_NAME);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(WorkspaceLockWriteError::DestinationAlreadyExists(path));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(WorkspaceLockWriteError::LockWrite(LockWriteError::Io {
                operation: "inspect lock destination",
                path,
                source,
            }));
        }
    }
    write_canonical_lock_atomically(&path, parsed).map_err(WorkspaceLockWriteError::LockWrite)
}

fn existing_regular_file_matches(path: &Path, expected: &[u8]) -> Result<bool, LockWriteError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(LockWriteError::DestinationNotRegularFile(
                path.to_path_buf(),
            ));
        }
        Err(source) => {
            return Err(LockWriteError::Io {
                operation: "open existing lock",
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let metadata = file.metadata().map_err(|source| LockWriteError::Io {
        operation: "inspect opened lock",
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(LockWriteError::DestinationNotRegularFile(
                path.to_path_buf(),
            ));
        }
    }
    if !metadata.is_file() {
        return Err(LockWriteError::DestinationNotRegularFile(
            path.to_path_buf(),
        ));
    }

    if metadata.len() != expected.len() as u64 {
        return Ok(false);
    }

    const COMPARE_BUFFER_SIZE: usize = 8 * 1024;
    let mut buffer = [0; COMPARE_BUFFER_SIZE];
    let mut offset = 0;
    while offset < expected.len() {
        let chunk_length = buffer.len().min(expected.len() - offset);
        let bytes_read =
            file.read(&mut buffer[..chunk_length])
                .map_err(|source| LockWriteError::Io {
                    operation: "read existing lock",
                    path: path.to_path_buf(),
                    source,
                })?;
        if bytes_read == 0 || buffer[..bytes_read] != expected[offset..offset + bytes_read] {
            return Ok(false);
        }
        offset += bytes_read;
    }

    let mut trailing_byte = [0];
    file.read(&mut trailing_byte)
        .map(|bytes_read| bytes_read == 0)
        .map_err(|source| LockWriteError::Io {
            operation: "finish comparing existing lock",
            path: path.to_path_buf(),
            source,
        })
}

fn destination_parent(path: &Path) -> Result<&Path, LockWriteError> {
    let Some(file_name) = path.file_name() else {
        return Err(LockWriteError::InvalidPath(path.to_path_buf()));
    };
    if file_name != LOCK_FILE_NAME {
        return Err(LockWriteError::InvalidPath(path.to_path_buf()));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    Ok(parent)
}

fn remove_leftover_temporary_files(parent: &Path) -> Result<(), LockWriteError> {
    let entries = fs::read_dir(parent).map_err(|source| LockWriteError::Io {
        operation: "list lock directory",
        path: parent.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| LockWriteError::Io {
            operation: "read lock directory entry",
            path: parent.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_temporary_lock_name(name) {
            continue;
        }
        let file_type = entry.file_type().map_err(|source| LockWriteError::Io {
            operation: "inspect leftover temporary lock",
            path: entry.path(),
            source,
        })?;
        if file_type.is_file() {
            fs::remove_file(entry.path()).map_err(|source| LockWriteError::Io {
                operation: "remove leftover temporary lock",
                path: entry.path(),
                source,
            })?;
        }
    }
    Ok(())
}

fn is_temporary_lock_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix(TEMPORARY_PREFIX) else {
        return false;
    };
    suffix.len() == RANDOM_SUFFIX_BYTES * 2
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn create_temporary_file(parent: &Path) -> Result<(PathBuf, File), LockWriteError> {
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let mut suffix = [0; RANDOM_SUFFIX_BYTES];
        getrandom::fill(&mut suffix).map_err(|error| LockWriteError::Io {
            operation: "generate a temporary lock name",
            path: parent.to_path_buf(),
            source: io::Error::other(error),
        })?;
        let mut name = String::with_capacity(TEMPORARY_PREFIX.len() + suffix.len() * 2);
        name.push_str(TEMPORARY_PREFIX);
        for byte in suffix {
            use fmt::Write as _;
            write!(name, "{byte:02x}").expect("writing to a String cannot fail");
        }
        let path = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(LockWriteError::Io {
                    operation: "create temporary lock",
                    path,
                    source,
                });
            }
        }
    }

    Err(LockWriteError::Io {
        operation: "create a unique temporary lock",
        path: parent.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "random temporary name collided repeatedly",
        ),
    })
}

fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let directory = File::open(parent)?;
        match directory.sync_all() {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
                ) => {}
            Err(source) => return Err(source),
        }
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

struct TemporaryLockFile {
    path: PathBuf,
    published: bool,
}

impl Drop for TemporaryLockFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        LockWriteError, LockWriteOutcome, WorkspaceLockWriteError,
        create_workspace_lock_atomically, write_canonical_lock_atomically,
        write_workspace_lock_atomically,
    };
    use crate::lock::parse;
    use crate::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-lock-writer-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn lock_path(&self) -> PathBuf {
            self.0.join("sai.lock")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const CANONICAL: &[u8] = b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-core }\n";

    #[test]
    fn writes_rewrites_noncanonical_and_skips_identical_lock_bytes() {
        let directory = TestDirectory::new();
        let path = directory.lock_path();
        let parsed = parse(CANONICAL);

        assert_eq!(
            write_canonical_lock_atomically(&path, &parsed).unwrap(),
            LockWriteOutcome::Written {
                bytes: CANONICAL.len()
            }
        );
        assert_eq!(fs::read(&path).unwrap(), CANONICAL);
        let leftover = directory
            .0
            .join(".sai.lock.0123456789abcdef0123456789abcdef");
        let backup = directory.0.join(".sai.lock.backup");
        fs::write(&leftover, b"partial output").unwrap();
        fs::write(&backup, b"keep backup").unwrap();
        assert_eq!(
            write_canonical_lock_atomically(&path, &parsed).unwrap(),
            LockWriteOutcome::Unchanged
        );
        assert!(!leftover.exists());
        assert_eq!(fs::read(&backup).unwrap(), b"keep backup");

        let noncanonical =
            b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-core, wasm32-core }\n";
        fs::write(&path, noncanonical).unwrap();
        fs::write(&leftover, b"partial output").unwrap();
        assert!(matches!(
            write_canonical_lock_atomically(&path, &parse(noncanonical)).unwrap(),
            LockWriteOutcome::Written { .. }
        ));
        assert_eq!(fs::read(&path).unwrap(), CANONICAL);
        assert!(!leftover.exists());
        assert_eq!(fs::read(&backup).unwrap(), b"keep backup");
        assert!(fs::read_dir(&directory.0).unwrap().all(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .is_none_or(|name| !super::is_temporary_lock_name(name))
        }));
    }

    #[cfg(unix)]
    #[test]
    fn creates_lock_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TestDirectory::new();
        let path = directory.lock_path();
        write_canonical_lock_atomically(&path, &parse(CANONICAL)).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn workspace_writer_uses_the_held_exclusive_lock() {
        let directory = TestDirectory::new();
        let parsed = parse(CANONICAL);
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert_eq!(
            write_workspace_lock_atomically(&guard, &parsed).unwrap(),
            LockWriteOutcome::Written {
                bytes: CANONICAL.len()
            }
        );
        assert_eq!(fs::read(directory.lock_path()).unwrap(), CANONICAL);
        assert_eq!(
            fs::metadata(directory.0.join(".sai/lock")).unwrap().len(),
            0
        );
        assert!(
            WorkspaceLockGuard::try_acquire(&directory.0, WorkspaceLockMode::Exclusive)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn workspace_creator_creates_once_and_preserves_an_existing_lock() {
        let directory = TestDirectory::new();
        let canonical_lock_path = fs::canonicalize(&directory.0).unwrap().join("sai.lock");
        let parsed = parse(CANONICAL);
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert_eq!(
            create_workspace_lock_atomically(&guard, &parsed).unwrap(),
            LockWriteOutcome::Written {
                bytes: CANONICAL.len()
            }
        );
        assert!(matches!(
            create_workspace_lock_atomically(&guard, &parse(b"not a lock")),
            Err(WorkspaceLockWriteError::DestinationAlreadyExists(path))
                if path == canonical_lock_path
        ));
        assert_eq!(fs::read(directory.lock_path()).unwrap(), CANONICAL);
    }

    #[test]
    fn workspace_creator_requires_an_exclusive_lock_before_touching_the_destination() {
        let directory = TestDirectory::new();
        let guard = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();

        assert!(matches!(
            create_workspace_lock_atomically(&guard, &parse(CANONICAL)),
            Err(WorkspaceLockWriteError::WorkspaceLockNotExclusive(
                WorkspaceLockMode::Shared
            ))
        ));
        assert!(!directory.lock_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_creator_rejects_a_broken_symlink_destination() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.0.join("missing-target");
        symlink(&target, directory.lock_path()).unwrap();
        let canonical_lock_path = fs::canonicalize(&directory.0).unwrap().join("sai.lock");
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            create_workspace_lock_atomically(&guard, &parse(CANONICAL)),
            Err(WorkspaceLockWriteError::DestinationAlreadyExists(path))
                if path == canonical_lock_path
        ));
        assert!(
            fs::symlink_metadata(directory.lock_path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_writer_uses_the_guarded_canonical_directory() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        fs::create_dir(&package).unwrap();
        let alias = directory.0.join("package-alias");
        symlink(&package, &alias).unwrap();
        let guard = WorkspaceLockGuard::acquire(&alias, WorkspaceLockMode::Exclusive).unwrap();

        assert_eq!(
            guard.package_directory(),
            fs::canonicalize(&package).unwrap()
        );
        write_workspace_lock_atomically(&guard, &parse(CANONICAL)).unwrap();
        assert_eq!(fs::read(package.join("sai.lock")).unwrap(), CANONICAL);
    }

    #[test]
    fn workspace_writer_requires_an_exclusive_lock() {
        let directory = TestDirectory::new();
        let guard = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();
        let parsed = parse(b"not a lock");

        assert!(matches!(
            write_workspace_lock_atomically(&guard, &parsed),
            Err(WorkspaceLockWriteError::WorkspaceLockNotExclusive(
                WorkspaceLockMode::Shared
            ))
        ));
    }

    #[test]
    fn workspace_writer_keeps_the_callers_lock_after_serialization_failure() {
        let directory = TestDirectory::new();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let parsed = parse(b"not a lock");

        assert!(matches!(
            write_workspace_lock_atomically(&guard, &parsed),
            Err(WorkspaceLockWriteError::LockWrite(
                LockWriteError::Serialization(_)
            ))
        ));
        assert!(
            WorkspaceLockGuard::try_acquire(&directory.0, WorkspaceLockMode::Exclusive)
                .unwrap()
                .is_none()
        );
        drop(guard);
        assert!(
            WorkspaceLockGuard::try_acquire(&directory.0, WorkspaceLockMode::Exclusive)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn rejects_invalid_lock_before_touching_destination() {
        let directory = TestDirectory::new();
        let path = directory.lock_path();
        fs::write(&path, b"preserve me").unwrap();
        let invalid = parse(b"not a lock\n");

        assert!(matches!(
            write_canonical_lock_atomically(&path, &invalid),
            Err(LockWriteError::Serialization(_))
        ));
        assert_eq!(fs::read(path).unwrap(), b"preserve me");
    }

    #[test]
    fn rejects_non_lock_paths_and_non_regular_destinations() {
        let directory = TestDirectory::new();
        let parsed = parse(CANONICAL);
        assert!(matches!(
            write_canonical_lock_atomically(&directory.0.join("other.lock"), &parsed),
            Err(LockWriteError::InvalidPath(_))
        ));

        let path = directory.lock_path();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            write_canonical_lock_atomically(&path, &parsed),
            Err(LockWriteError::DestinationNotRegularFile(_))
        ));
        assert!(path.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_destination_without_reading_or_replacing_target() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        fs::write(&target, b"keep target").unwrap();
        let path = directory.lock_path();
        symlink(&target, &path).unwrap();

        assert!(matches!(
            write_canonical_lock_atomically(&path, &parse(CANONICAL)),
            Err(LockWriteError::DestinationNotRegularFile(_))
        ));
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"keep target");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_destination_without_waiting_for_a_writer() {
        use std::process::Command;
        use std::process::Stdio;
        use std::thread;
        use std::time::{Duration, Instant};

        let directory = TestDirectory::new();
        let path = directory.lock_path();
        let status = Command::new("mkfifo").arg(&path).status().unwrap();
        assert!(status.success());

        const HELPER_ENV: &str = "SAI_LOCK_WRITER_FIFO_HELPER_PATH";
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_writer::tests::fifo_destination_helper"])
            .env(HELPER_ENV, &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("opening a FIFO lock destination exceeded the test timeout");
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "FIFO helper process failed: {status}");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_destination_helper() {
        let Some(path) = std::env::var_os("SAI_LOCK_WRITER_FIFO_HELPER_PATH") else {
            return;
        };
        assert!(matches!(
            write_canonical_lock_atomically(&PathBuf::from(path), &parse(CANONICAL)),
            Err(LockWriteError::DestinationNotRegularFile(_))
        ));
    }
}
