//! Safe wrappers for Windows filesystem metadata, range locking, and atomic replacement.

#[cfg(not(windows))]
use std::fs;
use std::fs::File;
use std::io;
use std::path::Path;

/// Atomically move a completed file over its destination.
///
/// The source and destination must be on the same volume. Callers should place
/// the temporary file beside the destination before calling this function.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let source = encode_windows_path(source)?;
        let destination = encode_windows_path(destination)?;
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that stay alive
        // for this call; MoveFileExW does not retain either pointer.
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus};

    use super::{
        is_reparse_point, lock_file_range, lock_file_range_windows,
        open_directory_without_following, replace_file, unlock_file_range,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("sai-windows-fs-{}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
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

    #[test]
    fn rejects_embedded_nul_in_windows_paths() {
        let error =
            replace_file(Path::new("source\0ignored"), Path::new("destination")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn directory_handle_reports_metadata_and_denies_rename() {
        let directory = TestDirectory::new();
        let handle = open_directory_without_following(&directory.0).unwrap();

        assert!(!is_reparse_point(&handle).unwrap());
        assert!(handle.metadata().unwrap().is_dir());
        assert!(fs::rename(&directory.0, directory.0.with_extension("moved")).is_err());
    }

    #[test]
    fn rejects_a_directory_reparse_point() {
        use std::os::windows::fs::symlink_dir;

        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        fs::create_dir(&target).unwrap();
        let link = directory.0.join("link");
        if let Err(error) = symlink_dir(&target, &link) {
            if error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("create directory symlink: {error}");
        }

        assert_eq!(
            open_directory_without_following(&link).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn byte_range_lock_supports_shared_and_exclusive_modes() {
        let directory = TestDirectory::new();
        let path = directory.0.join("lock");
        fs::write(&path, b"").unwrap();
        let open = || {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&path)
                .unwrap()
        };
        let first = open();
        let second = open();
        let third = open();

        lock_file_range(&first, true, false).unwrap();
        assert_eq!(
            lock_file_range(&second, true, false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        unlock_file_range(&first).unwrap();

        lock_file_range(&first, false, false).unwrap();
        lock_file_range(&second, false, false).unwrap();
        assert_eq!(
            lock_file_range(&second, true, false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        unlock_file_range(&second).unwrap();
        unlock_file_range(&first).unwrap();
        lock_file_range(&third, true, false).unwrap();
        unlock_file_range(&third).unwrap();
    }

    #[test]
    fn overlapped_lock_wait_keeps_operation_alive_until_released() {
        let directory = TestDirectory::new();
        let path = directory.0.join("lock");
        fs::write(&path, b"").unwrap();
        let mut options = OpenOptions::new();
        let overlapped_file = options
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(&path)
            .unwrap();
        // SAFETY: the file handle was opened for overlapped I/O, and a null
        // existing port requests a new completion port associated with it.
        let completion_port_raw = unsafe {
            CreateIoCompletionPort(overlapped_file.as_raw_handle(), std::ptr::null_mut(), 0, 1)
        };
        assert!(!completion_port_raw.is_null());
        // SAFETY: CreateIoCompletionPort returned an owned completion-port handle.
        let completion_port = unsafe { OwnedHandle::from_raw_handle(completion_port_raw) };
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        lock_file_range(&holder, true, false).unwrap();
        assert_eq!(
            lock_file_range(&overlapped_file, true, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );

        let (pending_sender, pending_receiver) = mpsc::channel();
        let (acquired_sender, acquired_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (done_sender, done_receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let lock_result = lock_file_range_windows(&overlapped_file, true, true, || {
                let _ = pending_sender.send(());
            });
            if lock_result.is_ok() {
                acquired_sender.send(Ok(())).unwrap();
                release_receiver.recv().unwrap();
                done_sender
                    .send(unlock_file_range(&overlapped_file))
                    .unwrap();
            } else {
                acquired_sender.send(lock_result).unwrap();
            }
        });
        pending_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("overlapped lock entered its pending state");

        unlock_file_range(&holder).unwrap();
        acquired_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("overlapped lock completed")
            .unwrap();
        let mut bytes_transferred = 0;
        let mut completion_key = 0;
        let mut overlapped_pointer = std::ptr::null_mut();
        // SAFETY: the completion port is owned by this test, the output
        // pointers are writable, and the finite timeout only observes packets.
        let packet_received = unsafe {
            GetQueuedCompletionStatus(
                completion_port.as_raw_handle(),
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped_pointer,
                100,
            )
        };
        assert_eq!(packet_received, 0);
        assert!(overlapped_pointer.is_null());
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(windows_sys::Win32::Foundation::WAIT_TIMEOUT as i32)
        );

        release_sender.send(()).unwrap();
        done_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("overlapped unlock completed")
            .unwrap();
        waiter.join().unwrap();
        // SAFETY: as above; this also checks that unlock completion cannot
        // enqueue a pointer to its stack OVERLAPPED after returning.
        let packet_received = unsafe {
            GetQueuedCompletionStatus(
                completion_port.as_raw_handle(),
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped_pointer,
                100,
            )
        };
        assert_eq!(packet_received, 0);
        assert!(overlapped_pointer.is_null());
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(windows_sys::Win32::Foundation::WAIT_TIMEOUT as i32)
        );
    }
}

#[cfg(windows)]
fn encode_windows_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut encoded = Vec::new();
    for unit in path.as_os_str().encode_wide() {
        if unit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows paths cannot contain NUL",
            ));
        }
        encoded.push(unit);
    }
    encoded.push(0);
    Ok(encoded)
}

/// Return the number of hard links that refer to an open file.
///
/// On Windows this uses the file handle, so it works for files opened through a
/// capability directory. Other platforms return `Unsupported`.
pub fn number_of_links(file: &File) -> io::Result<u32> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `file` owns a live Windows handle and `information` is a
        // writable structure of the exact type required by the API.
        let succeeded =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information.nNumberOfLinks)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows file-link metadata is unavailable on this platform",
        ))
    }
}

/// Whether an open file handle refers to a Windows reparse point.
///
/// This is useful after opening with `FILE_FLAG_OPEN_REPARSE_POINT`, where
/// metadata should be checked without following a symbolic link or junction.
pub fn is_reparse_point(file: &File) -> io::Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
        };

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `file` owns a live Windows handle and `information` is a
        // writable structure of the exact type required by the API.
        let succeeded =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows reparse-point metadata is unavailable on this platform",
        ))
    }
}

/// Open a directory without following a symbolic link or junction.
///
/// The returned handle denies delete sharing so the opened directory cannot be
/// renamed or replaced while the handle remains alive.
pub fn open_directory_without_following(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };

        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if is_reparse_point(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path is a Windows reparse point",
            ));
        }
        if !file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        Ok(file)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows directory handles are unavailable on this platform",
        ))
    }
}

/// Acquire a lock over the first byte of an open file.
///
/// When `wait` is false, a conflicting lock returns `WouldBlock`. Without the
/// exclusive flag, the lock is shared with other readers.
pub fn lock_file_range(file: &File, exclusive: bool, wait: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        lock_file_range_windows(file, exclusive, wait, || {})
    }
    #[cfg(not(windows))]
    {
        let _ = (file, exclusive, wait);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows byte-range locking is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
fn lock_file_range_windows(
    file: &File,
    exclusive: bool,
    wait: bool,
    on_pending: impl FnOnce(),
) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, OVERLAPPED};

    let mut flags = if exclusive {
        LOCKFILE_EXCLUSIVE_LOCK
    } else {
        0
    };
    if !wait {
        flags |= LOCKFILE_FAIL_IMMEDIATELY;
    }
    let event = create_overlapped_event()?;
    let mut overlapped = OVERLAPPED {
        // Setting the low bit prevents completion ports from retaining a
        // pointer to this stack OVERLAPPED after GetOverlappedResult returns.
        hEvent: event.as_raw_handle().map_addr(|address| address | 1),
        ..OVERLAPPED::default()
    };
    // SAFETY: the file handle is live and `overlapped` remains writable and
    // in scope until the operation completes, including an overlapped wait.
    // The per-operation event remains alive until that completion is drained.
    let succeeded = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
    if succeeded != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32) {
        // The observer is test-only in practice, but keep the OVERLAPPED and
        // event alive until completion even if an observer panics.
        let observer_panic =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_pending)).err();
        let completion = if wait {
            complete_overlapped_operation(file.as_raw_handle(), &overlapped)
        } else {
            // A nonblocking lock should not pend, but an overlapped handle can
            // report pending completion. Cancel it and drain the completion
            // before allowing the OVERLAPPED or event to be dropped. If it
            // wins the race with cancellation, report that the lock was
            // acquired so callers will release it normally.
            unsafe { CancelIoEx(file.as_raw_handle(), &overlapped) };
            match complete_overlapped_operation(file.as_raw_handle(), &overlapped) {
                Ok(()) => Ok(()),
                Err(completion_error)
                    if completion_error.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32)
                        || completion_error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) =>
                {
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                }
                Err(completion_error) => Err(completion_error),
            }
        };
        if let Some(payload) = observer_panic {
            std::panic::resume_unwind(payload);
        }
        return completion;
    }
    if !wait && error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    Err(error)
}

/// Release the lock over the first byte acquired by [`lock_file_range`].
pub fn unlock_file_range(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let event = create_overlapped_event()?;
        let mut overlapped = OVERLAPPED {
            // Setting the low bit prevents completion ports from retaining a
            // pointer to this stack OVERLAPPED after its completion is drained.
            hEvent: event.as_raw_handle().map_addr(|address| address | 1),
            ..OVERLAPPED::default()
        };
        // SAFETY: `file` owns a live handle and `overlapped` points to a
        // writable structure containing the same zero offset used to lock.
        // It and the per-operation event remain alive through any asynchronous
        // completion.
        let succeeded = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
        if succeeded == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32)
            {
                return complete_overlapped_operation(file.as_raw_handle(), &overlapped);
            }
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows byte-range unlocking is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
fn create_overlapped_event() -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::CreateEventW;

    // SAFETY: null security attributes and name are accepted; a manual-reset,
    // initially nonsignaled event is appropriate for one overlapped request.
    let handle = unsafe {
        CreateEventW(
            std::ptr::null(),
            windows_sys::Win32::Foundation::TRUE,
            windows_sys::Win32::Foundation::FALSE,
            std::ptr::null(),
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned a valid owned event handle.
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) })
}

#[cfg(windows)]
fn complete_overlapped_operation(
    handle: std::os::windows::io::RawHandle,
    overlapped: &windows_sys::Win32::System::IO::OVERLAPPED,
) -> io::Result<()> {
    use windows_sys::Win32::System::IO::GetOverlappedResult;

    let mut transferred = 0;
    // SAFETY: the handle remains owned by the caller, `overlapped` remains
    // alive and unchanged, and the final flag asks Windows to wait for
    // completion before returning.
    let succeeded = unsafe {
        GetOverlappedResult(
            handle,
            overlapped,
            &mut transferred,
            windows_sys::Win32::Foundation::TRUE,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
