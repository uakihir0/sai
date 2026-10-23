//! Shared and exclusive process locks for a package's `.sai/lock` file.
//!
//! The lock file is persistent state: this module creates it when needed but
//! never removes, replaces, or writes to it.

use std::cell::Cell;
use std::error::Error;
#[cfg(any(
    windows,
    all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    )
))]
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// Whether a workspace lock permits other readers or excludes all users.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceLockMode {
    /// Multiple commands may hold this lock while reading the workspace.
    Shared,
    /// Only one command may hold this lock while mutating the workspace.
    Exclusive,
}

/// An error while preparing or acquiring `.sai/lock`.
#[derive(Debug)]
pub enum WorkspaceLockError {
    Io { path: PathBuf, source: io::Error },
    InvalidPackageDirectory(PathBuf),
    InvalidStateDirectory(PathBuf),
    InvalidLockFile(PathBuf),
    NonEmptyLockFile(PathBuf),
}

impl fmt::Display for WorkspaceLockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "could not access workspace lock at {path:?}: {source}"
                )
            }
            Self::InvalidPackageDirectory(path) => {
                write!(
                    formatter,
                    "workspace package path is not a directory: {path:?}"
                )
            }
            Self::InvalidStateDirectory(path) => {
                write!(
                    formatter,
                    "workspace state path is not a directory: {path:?}"
                )
            }
            Self::InvalidLockFile(path) => {
                write!(formatter, "workspace lock is not a regular file: {path:?}")
            }
            Self::NonEmptyLockFile(path) => {
                write!(formatter, "workspace lock file must be empty: {path:?}")
            }
        }
    }
}

impl Error for WorkspaceLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidPackageDirectory(_)
            | Self::InvalidStateDirectory(_)
            | Self::InvalidLockFile(_)
            | Self::NonEmptyLockFile(_) => None,
        }
    }
}

/// A held shared or exclusive workspace lock.
///
/// Dropping the guard releases the OS lock. The `.sai/lock` file itself
/// remains in place so concurrent processes always lock the same file.
/// Participating processes must not remove or replace that directory entry
/// while a guard may be held; arbitrary external filesystem mutation is
/// outside this advisory lock's coordination contract. A guard is intentionally
/// not `Sync`: filesystem mutations perform a read-check-update sequence that
/// must not be interleaved by another thread sharing this guard. Concurrent
/// readers can acquire independent shared guards.
///
/// ```compile_fail
/// use sai_package::workspace_lock::WorkspaceLockGuard;
///
/// fn require_sync<T: Sync>() {}
/// require_sync::<WorkspaceLockGuard>();
/// ```
#[must_use = "dropping the guard releases the workspace lock"]
pub struct WorkspaceLockGuard {
    file: File,
    _package_directory: Option<File>,
    _state_directory: File,
    package_directory_path: PathBuf,
    path: PathBuf,
    mode: WorkspaceLockMode,
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for WorkspaceLockGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceLockGuard")
            .field("package_directory", &self.package_directory_path)
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl WorkspaceLockGuard {
    /// Acquires a workspace lock, blocking until it becomes available.
    pub fn acquire(
        package_directory: impl AsRef<Path>,
        mode: WorkspaceLockMode,
    ) -> Result<Self, WorkspaceLockError> {
        Self::acquire_with_waiting_callback(package_directory, mode, || {})
    }

    /// Acquires a lock and calls `on_wait` once if another process holds a
    /// conflicting lock when acquisition is first attempted.
    ///
    /// Command front ends can use the callback to print the documented
    /// `waiting for .sai/lock` progress line without putting CLI behavior in
    /// this package-level primitive.
    pub fn acquire_with_waiting_callback(
        package_directory: impl AsRef<Path>,
        mode: WorkspaceLockMode,
        on_wait: impl FnOnce(),
    ) -> Result<Self, WorkspaceLockError> {
        let package_directory_path = canonicalize_package_directory(package_directory.as_ref())?;
        let package_directory = pin_package_directory(&package_directory_path)?;
        let (state_directory_path, path) = prepare_lock_paths(&package_directory_path)?;
        let state_directory = open_state_directory(&state_directory_path, mode)?;
        validate_state_directory_binding(
            package_directory.as_ref(),
            &state_directory,
            &package_directory_path,
            &state_directory_path,
        )?;
        let file = open_lock_file(&state_directory, &path)?;
        validate_regular_lock_file(&file, &path)?;
        let guard = Self {
            file,
            _package_directory: package_directory,
            _state_directory: state_directory,
            package_directory_path,
            path,
            mode,
            _not_sync: PhantomData,
        };

        if !lock_platform(&guard.file, mode, false).map_err(|source| WorkspaceLockError::Io {
            path: guard.path.clone(),
            source,
        })? {
            on_wait();
            loop {
                match lock_platform(&guard.file, mode, true) {
                    Ok(true) => break,
                    Ok(false) => {
                        return Err(WorkspaceLockError::Io {
                            path: guard.path.clone(),
                            source: io::Error::other(
                                "blocking workspace lock unexpectedly returned WouldBlock",
                            ),
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(source) => {
                        return Err(WorkspaceLockError::Io {
                            path: guard.path.clone(),
                            source,
                        });
                    }
                }
            }
        }

        guard.validate_empty()?;
        Ok(guard)
    }

    /// Attempts to acquire a lock without waiting.
    ///
    /// Returns `Ok(None)` when another process holds an incompatible lock.
    pub fn try_acquire(
        package_directory: impl AsRef<Path>,
        mode: WorkspaceLockMode,
    ) -> Result<Option<Self>, WorkspaceLockError> {
        let package_directory_path = canonicalize_package_directory(package_directory.as_ref())?;
        let package_directory = pin_package_directory(&package_directory_path)?;
        let (state_directory_path, path) = prepare_lock_paths(&package_directory_path)?;
        let state_directory = open_state_directory(&state_directory_path, mode)?;
        validate_state_directory_binding(
            package_directory.as_ref(),
            &state_directory,
            &package_directory_path,
            &state_directory_path,
        )?;
        let file = open_lock_file(&state_directory, &path)?;
        validate_regular_lock_file(&file, &path)?;
        if !lock_platform(&file, mode, false).map_err(|source| WorkspaceLockError::Io {
            path: path.clone(),
            source,
        })? {
            return Ok(None);
        }

        let guard = Self {
            file,
            _package_directory: package_directory,
            _state_directory: state_directory,
            package_directory_path,
            path,
            mode,
            _not_sync: PhantomData,
        };
        guard.validate_empty()?;
        Ok(Some(guard))
    }

    /// The mode held by this guard.
    pub const fn mode(&self) -> WorkspaceLockMode {
        self.mode
    }

    /// The persistent `.sai/lock` path held by this guard.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The canonical package directory whose `.sai/lock` is held.
    pub fn package_directory(&self) -> &Path {
        &self.package_directory_path
    }

    /// The pinned package-root directory handle, when safe handle identity is
    /// available on this platform.
    pub fn package_directory_handle(&self) -> Option<&File> {
        self._package_directory.as_ref()
    }

    /// The pinned `.sai` directory handle associated with this lock.
    ///
    /// Filesystem operations that touch workspace state should resolve
    /// children relative to this handle so they remain anchored to the
    /// directory whose lock is held. Mutating callers must still require an
    /// exclusive lock before using write-capable child operations.
    pub fn state_directory_handle(&self) -> &File {
        &self._state_directory
    }

    fn validate_empty(&self) -> Result<(), WorkspaceLockError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|source| WorkspaceLockError::Io {
                path: self.path.clone(),
                source,
            })?;
        if metadata.len() != 0 {
            return Err(WorkspaceLockError::NonEmptyLockFile(self.path.clone()));
        }
        Ok(())
    }
}

impl Drop for WorkspaceLockGuard {
    fn drop(&mut self) {
        let _ = unlock_platform(&self.file);
    }
}

fn canonicalize_package_directory(path: &Path) -> Result<PathBuf, WorkspaceLockError> {
    let package_directory = fs::canonicalize(path).map_err(|source| WorkspaceLockError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(&package_directory).map_err(|source| WorkspaceLockError::Io {
        path: package_directory.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(WorkspaceLockError::InvalidPackageDirectory(
            package_directory,
        ));
    }
    Ok(package_directory)
}

fn pin_package_directory(path: &Path) -> Result<Option<File>, WorkspaceLockError> {
    #[cfg(any(
        windows,
        all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )
    ))]
    {
        let directory =
            sai_windows_fs::open_directory_without_following(path).map_err(|source| {
                WorkspaceLockError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        Ok(Some(directory))
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )
    )))]
    {
        let _ = path;
        Ok(None)
    }
}

fn validate_state_directory_binding(
    package_directory: Option<&File>,
    state_directory: &File,
    package_directory_path: &Path,
    state_directory_path: &Path,
) -> Result<(), WorkspaceLockError> {
    #[cfg(any(
        windows,
        all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )
    ))]
    {
        let package_directory = package_directory.ok_or_else(|| {
            WorkspaceLockError::InvalidPackageDirectory(package_directory_path.to_path_buf())
        })?;
        let pinned_state_directory = sai_windows_fs::open_child_directory_without_following(
            package_directory,
            package_directory_path,
            OsStr::new(".sai"),
        )
        .map_err(|source| {
            if source.kind() == io::ErrorKind::InvalidInput
                || source.kind() == io::ErrorKind::NotFound
                || source.kind() == io::ErrorKind::NotADirectory
            {
                return WorkspaceLockError::InvalidStateDirectory(
                    state_directory_path.to_path_buf(),
                );
            }
            WorkspaceLockError::Io {
                path: state_directory_path.to_path_buf(),
                source,
            }
        })?;
        let pinned_identity =
            sai_windows_fs::file_identity(&pinned_state_directory).map_err(|source| {
                WorkspaceLockError::Io {
                    path: state_directory_path.to_path_buf(),
                    source,
                }
            })?;
        let opened_identity = sai_windows_fs::file_identity(state_directory).map_err(|source| {
            WorkspaceLockError::Io {
                path: state_directory_path.to_path_buf(),
                source,
            }
        })?;
        if pinned_identity != opened_identity {
            return Err(WorkspaceLockError::InvalidStateDirectory(
                state_directory_path.to_path_buf(),
            ));
        }
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )
    )))]
    {
        let _ = (
            package_directory,
            state_directory,
            package_directory_path,
            state_directory_path,
        );
    }
    Ok(())
}

fn prepare_lock_paths(package_directory: &Path) -> Result<(PathBuf, PathBuf), WorkspaceLockError> {
    let state_directory = package_directory.join(".sai");
    match fs::symlink_metadata(&state_directory) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(WorkspaceLockError::InvalidStateDirectory(state_directory));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&state_directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(WorkspaceLockError::Io {
                        path: state_directory.clone(),
                        source,
                    });
                }
            }
            let metadata = fs::symlink_metadata(&state_directory).map_err(|source| {
                WorkspaceLockError::Io {
                    path: state_directory.clone(),
                    source,
                }
            })?;
            if !metadata.file_type().is_dir() {
                return Err(WorkspaceLockError::InvalidStateDirectory(state_directory));
            }
        }
        Err(source) => {
            return Err(WorkspaceLockError::Io {
                path: state_directory.clone(),
                source,
            });
        }
    }

    let lock_path = state_directory.join("lock");
    Ok((state_directory, lock_path))
}

fn open_state_directory(path: &Path, mode: WorkspaceLockMode) -> Result<File, WorkspaceLockError> {
    #[cfg(not(windows))]
    let _ = mode;

    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    ))]
    {
        use rustix::fs::{CWD, Mode, OFlags, openat};

        let descriptor = openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| {
            let source: io::Error = error.into();
            if source.raw_os_error() == Some(libc::ELOOP)
                || source.kind() == io::ErrorKind::NotADirectory
            {
                return WorkspaceLockError::InvalidStateDirectory(path.to_path_buf());
            }
            WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        Ok(descriptor.into())
    }

    #[cfg(windows)]
    {
        let directory = if mode == WorkspaceLockMode::Exclusive {
            sai_windows_fs::open_directory_path_for_child_updates_without_following(path)
        } else {
            sai_windows_fs::open_directory_path_without_following(path)
        }
        .map_err(|source| {
            if source.kind() == io::ErrorKind::InvalidInput {
                return WorkspaceLockError::InvalidStateDirectory(path.to_path_buf());
            }
            WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        return Ok(directory);
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "redox",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let directory = File::open(path).map_err(|source| WorkspaceLockError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if !directory
            .metadata()
            .map_err(|source| WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .is_dir()
        {
            return Err(WorkspaceLockError::InvalidStateDirectory(
                path.to_path_buf(),
            ));
        }
        Ok(directory)
    }
}

fn open_lock_file(state_directory: &File, path: &Path) -> Result<File, WorkspaceLockError> {
    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita",
            target_os = "wasi"
        ))
    ))]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let descriptor = openat(
            state_directory,
            "lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| {
            let source: io::Error = error.into();
            if source.raw_os_error() == Some(libc::ELOOP) {
                return WorkspaceLockError::InvalidLockFile(path.to_path_buf());
            }
            WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let file: File = descriptor.into();
        validate_regular_lock_file(&file, path)?;
        Ok(file)
    }

    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let _ = state_directory;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|source| WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        validate_regular_lock_file(&file, path)?;
        return Ok(file);
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "redox",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let _ = state_directory;
        use std::fs::OpenOptions;

        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Err(WorkspaceLockError::InvalidLockFile(path.to_path_buf())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(WorkspaceLockError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|source| WorkspaceLockError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        validate_regular_lock_file(&file, path)?;
        Ok(file)
    }
}

fn validate_regular_lock_file(file: &File, path: &Path) -> Result<(), WorkspaceLockError> {
    #[cfg(windows)]
    if sai_windows_fs::is_reparse_point(file).map_err(|source| WorkspaceLockError::Io {
        path: path.to_path_buf(),
        source,
    })? {
        return Err(WorkspaceLockError::InvalidLockFile(path.to_path_buf()));
    }

    let metadata = file.metadata().map_err(|source| WorkspaceLockError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(WorkspaceLockError::InvalidLockFile(path.to_path_buf()));
    }
    Ok(())
}

fn lock_platform(file: &File, mode: WorkspaceLockMode, wait: bool) -> io::Result<bool> {
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
        let operation = match (mode, wait) {
            (WorkspaceLockMode::Shared, true) => rustix::fs::FlockOperation::LockShared,
            (WorkspaceLockMode::Exclusive, true) => rustix::fs::FlockOperation::LockExclusive,
            (WorkspaceLockMode::Shared, false) => rustix::fs::FlockOperation::NonBlockingLockShared,
            (WorkspaceLockMode::Exclusive, false) => {
                rustix::fs::FlockOperation::NonBlockingLockExclusive
            }
        };
        loop {
            match rustix::fs::flock(file, operation) {
                Ok(()) => return Ok(true),
                Err(error)
                    if !wait && io::Error::from(error).kind() == io::ErrorKind::WouldBlock =>
                {
                    return Ok(false);
                }
                Err(error) if io::Error::from(error).kind() == io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    #[cfg(windows)]
    {
        match sai_windows_fs::lock_file_range(file, mode == WorkspaceLockMode::Exclusive, wait) {
            Ok(()) => Ok(true),
            Err(error) if !wait && error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let _ = (file, mode, wait);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace locking is unsupported on this platform",
        ))
    }
}

fn unlock_platform(file: &File) -> io::Result<()> {
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
        rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock).map_err(Into::into)
    }

    #[cfg(windows)]
    {
        sai_windows_fs::unlock_file_range(file)
    }

    #[cfg(not(any(
        windows,
        all(
            unix,
            not(any(
                target_os = "espidf",
                target_os = "horizon",
                target_os = "solaris",
                target_os = "vita",
                target_os = "wasi"
            ))
        )
    )))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace unlocking is unsupported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{WorkspaceLockError, WorkspaceLockGuard, WorkspaceLockMode};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    const PROBE_DIRECTORY_ENV: &str = "SAI_WORKSPACE_LOCK_PROBE_DIRECTORY";
    const PROBE_MODE_ENV: &str = "SAI_WORKSPACE_LOCK_PROBE_MODE";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("sai-workspace-lock-{}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn probe_in_subprocess(directory: &std::path::Path, mode: WorkspaceLockMode) -> String {
        let mode = match mode {
            WorkspaceLockMode::Shared => "shared",
            WorkspaceLockMode::Exclusive => "exclusive",
        };
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workspace_lock::tests::subprocess_try_lock_probe",
                "--nocapture",
            ])
            .env(PROBE_DIRECTORY_ENV, directory)
            .env(PROBE_MODE_ENV, mode)
            .output()
            .expect("lock-probe test process starts");
        assert!(
            output.status.success(),
            "lock probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn subprocess_try_lock_probe() {
        let Ok(directory) = std::env::var(PROBE_DIRECTORY_ENV) else {
            return;
        };
        let mode = match std::env::var(PROBE_MODE_ENV).as_deref() {
            Ok("shared") => WorkspaceLockMode::Shared,
            Ok("exclusive") => WorkspaceLockMode::Exclusive,
            other => panic!("invalid lock probe mode: {other:?}"),
        };
        let result = WorkspaceLockGuard::try_acquire(directory, mode).expect("lock probe is valid");
        println!(
            "{}",
            if result.is_some() {
                "acquired"
            } else {
                "blocked"
            }
        );
    }

    #[test]
    fn creates_an_empty_persistent_lock_file() {
        let directory = TestDirectory::new();
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        assert_eq!(
            guard.path(),
            fs::canonicalize(&directory.0).unwrap().join(".sai/lock")
        );
        assert_eq!(guard.mode(), WorkspaceLockMode::Exclusive);
        assert!(guard.path().is_file());
        assert_eq!(fs::metadata(guard.path()).unwrap().len(), 0);
        drop(guard);
        assert!(directory.0.join(".sai/lock").is_file());
    }

    #[test]
    fn rejects_a_non_directory_package_path() {
        let directory = TestDirectory::new();
        let package_file = directory.0.join("package");
        fs::write(&package_file, b"not a directory").unwrap();

        assert!(matches!(
            WorkspaceLockGuard::acquire(&package_file, WorkspaceLockMode::Shared),
            Err(WorkspaceLockError::InvalidPackageDirectory(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn pins_the_package_directory_until_the_lock_is_released() {
        let directory = TestDirectory::new();
        let moved = directory.0.with_extension("moved");
        let guard =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();

        assert!(fs::rename(&directory.0, &moved).is_err());
        drop(guard);
        fs::rename(&directory.0, &moved).unwrap();
        fs::remove_dir_all(moved).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_a_package_directory_alias_before_locking() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        fs::create_dir(&package).unwrap();
        let alias = directory.0.join("package-alias");
        symlink(&package, &alias).unwrap();

        let guard = WorkspaceLockGuard::acquire(&alias, WorkspaceLockMode::Exclusive).unwrap();

        assert_eq!(
            guard.path(),
            fs::canonicalize(&package).unwrap().join(".sai/lock")
        );
        assert!(package.join(".sai/lock").is_file());
    }

    #[test]
    fn shared_and_exclusive_locks_coordinate_across_processes() {
        let directory = TestDirectory::new();

        let exclusive =
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        assert!(probe_in_subprocess(&directory.0, WorkspaceLockMode::Shared).contains("blocked"));
        drop(exclusive);

        let shared = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared).unwrap();
        assert!(probe_in_subprocess(&directory.0, WorkspaceLockMode::Shared).contains("acquired"));
        assert!(
            probe_in_subprocess(&directory.0, WorkspaceLockMode::Exclusive).contains("blocked")
        );
        drop(shared);
        assert!(
            probe_in_subprocess(&directory.0, WorkspaceLockMode::Exclusive).contains("acquired")
        );
    }

    #[cfg(unix)]
    #[test]
    fn waiting_callback_runs_once_before_a_blocking_acquire() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let directory = TestDirectory::new();
        let held = WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive).unwrap();
        let callback_count = Arc::new(AtomicUsize::new(0));
        let waiter_count = Arc::clone(&callback_count);
        let wait_directory = directory.0.clone();
        let (waiting_sender, waiting_receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            WorkspaceLockGuard::acquire_with_waiting_callback(
                wait_directory,
                WorkspaceLockMode::Exclusive,
                || {
                    waiter_count.fetch_add(1, Ordering::Relaxed);
                    waiting_sender.send(()).unwrap();
                },
            )
            .unwrap()
        });

        waiting_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the callback runs before the blocking lock wait");
        drop(held);
        assert_eq!(waiter.join().unwrap().mode(), WorkspaceLockMode::Exclusive);
        assert_eq!(callback_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn independent_package_directories_have_independent_locks() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        let _guard = WorkspaceLockGuard::acquire(&first.0, WorkspaceLockMode::Exclusive).unwrap();

        let other =
            WorkspaceLockGuard::try_acquire(&second.0, WorkspaceLockMode::Exclusive).unwrap();
        assert!(other.is_some());
    }

    #[test]
    fn rejects_a_non_directory_sai_path() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join(".sai"), b"not a directory").unwrap();

        assert!(matches!(
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared),
            Err(WorkspaceLockError::InvalidStateDirectory(_))
        ));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn rejects_a_state_directory_from_a_replacement_workspace() {
        use super::{
            open_state_directory, pin_package_directory, prepare_lock_paths,
            validate_state_directory_binding,
        };

        let directory = TestDirectory::new();
        let package_directory = directory.0.join("package");
        fs::create_dir_all(package_directory.join(".sai")).unwrap();
        let package_handle = pin_package_directory(&package_directory)
            .unwrap()
            .expect("supported Unix pins the package root");
        let (state_directory_path, _) = prepare_lock_paths(&package_directory).unwrap();

        let moved_package_directory = directory.0.join("moved-package");
        fs::rename(&package_directory, &moved_package_directory).unwrap();
        fs::create_dir_all(package_directory.join(".sai")).unwrap();
        let replacement_state =
            open_state_directory(&state_directory_path, WorkspaceLockMode::Exclusive).unwrap();

        assert!(matches!(
            validate_state_directory_binding(
                Some(&package_handle),
                &replacement_state,
                &package_directory,
                &state_directory_path,
            ),
            Err(WorkspaceLockError::InvalidStateDirectory(_))
        ));
    }

    #[test]
    fn rejects_a_nonempty_lock_file_without_changing_it() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join(".sai")).unwrap();
        fs::write(directory.0.join(".sai/lock"), b"unexpected").unwrap();

        assert!(matches!(
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Exclusive),
            Err(WorkspaceLockError::NonEmptyLockFile(_))
        ));
        assert_eq!(
            fs::read(directory.0.join(".sai/lock")).unwrap(),
            b"unexpected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_lock_file() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join(".sai")).unwrap();
        let target = directory.0.join("target");
        fs::write(&target, b"").unwrap();
        symlink(&target, directory.0.join(".sai/lock")).unwrap();

        assert!(matches!(
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared),
            Err(WorkspaceLockError::InvalidLockFile(_))
        ));
        assert_eq!(fs::read(target).unwrap(), b"");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_state_directory() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.0.join("state-target");
        fs::create_dir(&target).unwrap();
        symlink(&target, directory.0.join(".sai")).unwrap();

        assert!(matches!(
            WorkspaceLockGuard::acquire(&directory.0, WorkspaceLockMode::Shared),
            Err(WorkspaceLockError::InvalidStateDirectory(_))
        ));
        assert!(!target.join("lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn opens_the_lock_relative_to_the_verified_state_directory_handle() {
        use std::os::unix::fs::symlink;

        use super::{open_lock_file, open_state_directory, prepare_lock_paths};

        let directory = TestDirectory::new();
        let (state_path, lock_path) = prepare_lock_paths(&directory.0).unwrap();
        let state_handle = open_state_directory(&state_path, WorkspaceLockMode::Shared).unwrap();
        let original_state = directory.0.join(".sai-original");
        fs::rename(&state_path, &original_state).unwrap();
        let replacement = directory.0.join("replacement");
        fs::create_dir(&replacement).unwrap();
        symlink(&replacement, &state_path).unwrap();

        let lock = open_lock_file(&state_handle, &lock_path).unwrap();
        assert!(lock.metadata().unwrap().is_file());
        assert!(original_state.join("lock").is_file());
        assert!(!replacement.join("lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_fifo_lock_file_without_blocking() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join(".sai")).unwrap();
        let status = Command::new("mkfifo")
            .arg(directory.0.join(".sai/lock"))
            .status()
            .expect("mkfifo is available on Unix");
        assert!(status.success());

        assert!(matches!(
            WorkspaceLockGuard::try_acquire(&directory.0, WorkspaceLockMode::Exclusive),
            Err(WorkspaceLockError::InvalidLockFile(_))
        ));
    }
}
