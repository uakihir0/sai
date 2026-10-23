//! Safe wrappers for filesystem handles, range locking, and atomic replacement.

use std::ffi::OsStr;
#[cfg(not(windows))]
use std::fs;
use std::fs::File;
use std::io::{self, Read};
#[cfg(not(windows))]
use std::path::PathBuf;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIdentity {
    volume: u64,
    file: [u8; 16],
}

/// A verified directory handle opened for a handle-relative rename.
///
/// This type can only be created by
/// [`open_child_directory_for_rename_without_following`], which opens a
/// synchronous Windows handle suitable for the rename operation.
#[derive(Debug)]
pub struct RenameableDirectory {
    file: File,
}

impl RenameableDirectory {
    /// Returns the underlying directory handle for metadata and identity
    /// queries. The handle remains owned by this wrapper.
    pub fn as_file(&self) -> &File {
        &self.file
    }
}

/// Returns a filesystem identity for an already-open file or directory.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata()?;
        let mut file_id = [0; 16];
        file_id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(FileIdentity {
            volume: metadata.dev(),
            file: file_id,
        })
    }
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        };

        let mut information = MaybeUninit::<FILE_ID_INFO>::zeroed();
        // SAFETY: `file` owns a live handle for this call and the output points
        // to a correctly sized, writable FILE_ID_INFO structure.
        let succeeded = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                FileIdInfo,
                information.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetFileInformationByHandle initialized the structure when it
        // returned nonzero.
        let information = unsafe { information.assume_init() };
        Ok(FileIdentity {
            volume: information.VolumeSerialNumber,
            file: information.FileId.Identifier,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file identity is unavailable on this platform",
        ))
    }
}

#[cfg(test)]
mod identity_tests {
    use std::fs::{self, File};
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::file_identity;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("sai-file-identity-{}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create temporary test directory: {error}"),
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
    fn file_identity_is_stable_for_cloned_handles_and_distinct_files() {
        let directory = TestDirectory::new();
        let first_path = directory.0.join("first");
        let second_path = directory.0.join("second");
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"second").unwrap();

        let first = File::open(&first_path).unwrap();
        let first_clone = first.try_clone().unwrap();
        let second = File::open(&second_path).unwrap();

        assert_eq!(
            file_identity(&first).unwrap(),
            file_identity(&first_clone).unwrap()
        );
        assert_ne!(
            file_identity(&first).unwrap(),
            file_identity(&second).unwrap()
        );
    }
}

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

/// Atomically move a completed file only when the destination does not exist.
///
/// The source and destination must be on the same volume. Callers should place
/// the temporary file beside the destination before calling this function.
pub fn move_file_without_replacing(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let source = encode_windows_path(source)?;
        let destination = encode_windows_path(destination)?;
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that stay alive
        // for this call; MoveFileExW does not retain either pointer. Omitting
        // MOVEFILE_REPLACE_EXISTING makes an existing destination an error.
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(source, destination)?;
        fs::remove_file(source)
    }
}

/// Atomically move a directory only when the destination does not exist.
///
/// The source and destination must be on the same volume.
pub fn move_directory_without_replacing(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let source = encode_windows_path(source)?;
        let destination = encode_windows_path(destination)?;
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that stay alive
        // for this call. MoveFileExW supports directory moves on one volume.
        // Omitting MOVEFILE_REPLACE_EXISTING leaves an existing destination
        // untouched.
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (source, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace directory moves are available only on Windows",
        ))
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
        move_directory_without_replacing, move_file_without_replacing,
        open_directory_for_rename_without_following, open_directory_without_following,
        open_file_for_stable_read_without_following, replace_file, unlock_file_range,
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
    fn move_without_replacing_preserves_an_existing_destination() {
        let directory = TestDirectory::new();
        let source = directory.0.join("temporary");
        let destination = directory.0.join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"existing").unwrap();

        let error = move_file_without_replacing(&source, &destination).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
    }

    #[test]
    fn move_without_replacing_publishes_a_missing_destination() {
        let directory = TestDirectory::new();
        let source = directory.0.join("temporary");
        let destination = directory.0.join("destination");
        fs::write(&source, b"complete").unwrap();

        move_file_without_replacing(&source, &destination).unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"complete");
    }

    #[test]
    fn directory_move_without_replacing_preserves_an_existing_destination() {
        let temporary = TestDirectory::new();
        let source = temporary.0.join("source");
        let destination = temporary.0.join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();

        let error = move_directory_without_replacing(&source, &destination).unwrap_err();

        assert!(matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
        ));
        assert!(source.is_dir());
        assert!(destination.is_dir());
    }

    #[test]
    fn renameable_directory_handle_allows_atomic_publication() {
        let temporary = TestDirectory::new();
        let source = temporary.0.join("source");
        let destination = temporary.0.join("destination");
        fs::create_dir(&source).unwrap();
        let handle = open_directory_for_rename_without_following(&source).unwrap();

        move_directory_without_replacing(&source, &destination).unwrap();

        assert!(!source.exists());
        assert!(destination.is_dir());
        assert!(!is_reparse_point(&handle).unwrap());
        assert!(handle.metadata().unwrap().is_dir());
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
    fn stable_read_open_excludes_writers_and_replacements_until_drop() {
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;

        let directory = TestDirectory::new();
        let path = directory.0.join("record.json");
        fs::write(&path, b"original").unwrap();

        let existing_writer = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        assert!(
            open_file_for_stable_read_without_following(
                &open_directory_without_following(&directory.0).unwrap(),
                &directory.0,
                path.file_name().unwrap(),
            )
            .is_err(),
            "a preexisting write-capable handle prevents a stable observation"
        );
        drop(existing_writer);

        let parent = open_directory_without_following(&directory.0).unwrap();
        let stable = open_file_for_stable_read_without_following(
            &parent,
            &directory.0,
            path.file_name().unwrap(),
        )
        .unwrap();
        let writer_result = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path);
        assert!(
            writer_result.is_err(),
            "a stable observation handle denies new writers"
        );

        let replacement = directory.0.join("replacement.tmp");
        fs::write(&replacement, b"replacement").unwrap();
        assert!(
            fs::rename(&replacement, &path).is_err(),
            "a stable observation handle denies replacement"
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");

        drop(stable);
        let writer = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        drop(writer);
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
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
    #[cfg(unix)]
    {
        use rustix::fs::{CWD, Mode, OFlags, openat};

        openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening directories without following links is unavailable on this platform",
        ))
    }
}

/// Open an absolute directory path without following any symbolic-link component.
pub fn open_directory_path_without_following(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{CWD, Mode, OFlags, openat};

        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be absolute",
            ));
        }
        let mut current = openat(
            CWD,
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        let mut current_path = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    current =
                        open_child_directory_without_following(&current, &current_path, name)?;
                    current_path.push(name);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path must be normalized",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(windows)]
    {
        let root = path.ancestors().last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be absolute",
            )
        })?;
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be normalized",
            )
        })?;
        let mut current = open_directory_without_following(root)?;
        let mut current_path = root.to_path_buf();
        for component in relative.components() {
            match component {
                Component::Normal(name) => {
                    current =
                        open_child_directory_without_following(&current, &current_path, name)?;
                    current_path.push(name);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path must be normalized",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening directory paths without following links is unavailable on this platform",
        ))
    }
}

/// Open an absolute directory path without following links, granting rights
/// to atomically update its direct child entries.
///
/// The handle remains anchored to the final directory while atomic file
/// replacements are performed. Earlier path components are opened with the
/// ordinary read-only directory access used for traversal.
pub fn open_directory_path_for_child_updates_without_following(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        let root = path.ancestors().last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be absolute",
            )
        })?;
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be normalized",
            )
        })?;
        if relative.components().next().is_none() {
            return windows_handle::open_directory_for_child_updates(root);
        }
        let mut current = open_directory_without_following(root)?;
        let mut current_path = root.to_path_buf();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            match component {
                Component::Normal(name) => {
                    current = if components.peek().is_none() {
                        open_child_directory_for_child_updates_without_following(
                            &current,
                            &current_path,
                            name,
                        )?
                    } else {
                        open_child_directory_without_following(&current, &current_path, name)?
                    };
                    current_path.push(name);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path must be normalized",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(not(windows))]
    {
        open_directory_path_without_following(path)
    }
}

/// Open an absolute directory path without following links, granting rights to
/// atomically update direct child files without creating child directories.
///
/// The handle remains anchored to the final directory while atomic file
/// replacements are performed. Earlier path components are opened with the
/// ordinary read-only directory access used for traversal.
pub fn open_directory_path_for_file_updates_without_following(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        let root = path.ancestors().last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be absolute",
            )
        })?;
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be normalized",
            )
        })?;
        if relative.components().next().is_none() {
            return windows_handle::open_directory_for_file_updates(root);
        }
        let mut current = open_directory_without_following(root)?;
        let mut current_path = root.to_path_buf();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            match component {
                Component::Normal(name) => {
                    current = if components.peek().is_none() {
                        open_child_directory_for_file_updates_without_following(
                            &current,
                            &current_path,
                            name,
                        )?
                    } else {
                        open_child_directory_without_following(&current, &current_path, name)?
                    };
                    current_path.push(name);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path must be normalized",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(not(windows))]
    {
        open_directory_path_without_following(path)
    }
}

/// Open a direct child directory without following a symbolic link or junction.
///
/// `child_name` must be one normal path component. Unix uses `openat`; Windows
/// uses `NtCreateFile` with `parent` as `RootDirectory`. The Windows handle
/// denies delete sharing so the opened directory cannot be moved while held.
pub fn open_child_directory_without_following(
    parent: &File,
    parent_path: &Path,
    child_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        let _ = parent_path;
        windows_handle::open_child(parent, child_name, true)
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let _ = parent_path;
        openat(
            parent,
            child_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, parent_path, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening child directories without following links is unavailable on this platform",
        ))
    }
}

/// Open a direct child directory without following a link, granting rights to
/// atomically update its direct child entries.
///
/// The opened directory denies delete sharing so it cannot be moved while its
/// formatter capability remains alive.
pub fn open_child_directory_for_child_updates_without_following(
    parent: &File,
    parent_path: &Path,
    child_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        let _ = parent_path;
        windows_handle::open_child_for_child_updates(parent, child_name)
    }
    #[cfg(unix)]
    {
        open_child_directory_without_following(parent, parent_path, child_name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, parent_path, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening child directories without following links is unavailable on this platform",
        ))
    }
}

/// Open a direct child directory without following a link, granting rights to
/// atomically update direct child files without creating child directories.
///
/// The opened directory denies delete sharing so it cannot be moved while its
/// file-update capability remains alive.
pub fn open_child_directory_for_file_updates_without_following(
    parent: &File,
    parent_path: &Path,
    child_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        let _ = parent_path;
        windows_handle::open_child_for_file_updates(parent, child_name)
    }
    #[cfg(unix)]
    {
        open_child_directory_without_following(parent, parent_path, child_name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, parent_path, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening child directories without following links is unavailable on this platform",
        ))
    }
}

/// Create one direct child directory without following or replacing an
/// existing entry.
pub fn create_child_directory_without_following(
    parent: &File,
    child_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        windows_handle::create_child_directory(parent, child_name)
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};

        mkdirat(parent, child_name, Mode::RUSR | Mode::WUSR | Mode::XUSR)
            .map_err(io::Error::from)?;
        openat(
            parent,
            child_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative directory creation is unavailable on this platform",
        ))
    }
}

/// Create a new direct child regular file without following or replacing an
/// existing entry. The returned handle is opened for writing and is suitable
/// for `sync_all` before publication.
pub fn create_new_file_in_directory_without_following(
    parent: &File,
    child_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        windows_handle::create_child_file(parent, child_name)
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let file = openat(
            parent,
            child_name,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "created child is not a regular file",
            ));
        }
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative file creation is unavailable on this platform",
        ))
    }
}

/// Atomically move one direct child file to a new direct child name without
/// replacing an existing entry.
///
/// Unix support is provided where the platform has an atomic exclusive rename
/// primitive (Linux, Android, Apple platforms, and Redox). Filesystems or
/// kernels that reject the primitive return their error; callers must not
/// fall back to a check-then-rename sequence.
pub fn move_file_in_directory_without_replacing(
    parent: &File,
    source_name: &OsStr,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_child_name(source_name)?;
    validate_child_name(destination_name)?;
    #[cfg(windows)]
    {
        windows_handle::move_child_without_replacing(parent, source_name, destination_name)
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "redox"
    ))]
    {
        use rustix::fs::{RenameFlags, renameat_with};

        renameat_with(
            parent,
            source_name,
            parent,
            destination_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(all(
        unix,
        not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "redox"
        ))
    ))]
    {
        let _ = (parent, source_name, destination_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace file rename is unavailable on this Unix platform",
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, source_name, destination_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace file rename is unavailable on this platform",
        ))
    }
}

/// Flush metadata for an open directory handle.
///
/// Unix uses `fsync` through `File::sync_all`. Windows does not expose a
/// supported general directory-flush operation, so it returns
/// [`io::ErrorKind::Unsupported`] rather than claiming durable namespace
/// updates.
pub fn sync_directory(directory: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        directory.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = directory;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "durable directory metadata flush is unavailable on Windows",
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory metadata flush is unavailable on this platform",
        ))
    }
}

/// Open a direct child regular file without following a symbolic link.
///
/// `file_name` must be one normal path component. Both supported platforms
/// resolve it relative to `parent` and reject reparse points or symlinks.
/// Nonblocking open on Unix ensures a file replaced with a FIFO cannot stall
/// a cancellable formatter after selection while it waits for a writer.
pub fn open_file_without_following(
    parent: &File,
    parent_path: &Path,
    file_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(file_name)?;
    #[cfg(windows)]
    {
        let _ = parent_path;
        windows_handle::open_child(parent, file_name, false)
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let _ = parent_path;
        let file = openat(
            parent,
            file_name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "formatter input is not a regular file",
            ));
        }
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, parent_path, file_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opening files without following links is unavailable on this platform",
        ))
    }
}

/// Open a direct child regular file without following a symbolic link, while
/// preventing Windows writers and replacements until the returned handle is
/// dropped.
///
/// On Windows the open fails if a write-capable handle already exists. The
/// returned handle allows readers but denies write and delete sharing, so
/// later target checks can retain it to keep the observed bytes stable. Unix
/// and unsupported platforms preserve the behavior of
/// [`open_file_without_following`].
pub fn open_file_for_stable_read_without_following(
    parent: &File,
    parent_path: &Path,
    file_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(file_name)?;
    #[cfg(windows)]
    {
        let _ = parent_path;
        windows_handle::open_child_for_stable_read(parent, file_name)
    }
    #[cfg(not(windows))]
    {
        open_file_without_following(parent, parent_path, file_name)
    }
}

/// Open a direct child regular file without following a reparse point, with
/// rights to update its file attributes.
///
/// `file_name` must be one normal path component. The returned handle can
/// update attributes such as the Windows read-only flag without granting
/// access to modify the file's contents. It is opened with a zero Windows
/// share-access mask. Windows does not apply share checks to attribute-only
/// accesses, so this handle is not an exclusive lock against another
/// attribute-only open or path-based hard-link creation. This also does not
/// lock the parent directory's namespace: an operation authorized by the
/// parent directory, such as a rename using `FILE_DELETE_CHILD`, can still
/// move the entry.
#[cfg(windows)]
pub fn open_file_for_attribute_updates_without_following(
    parent: &File,
    parent_path: &Path,
    file_name: &OsStr,
) -> io::Result<File> {
    validate_child_name(file_name)?;
    let _ = parent_path;
    windows_handle::open_child_for_attribute_updates(parent, file_name)
}

/// Atomically replace one child file with another, relative to `parent`.
///
/// Both names must be one normal path component.
pub fn replace_file_in_directory(
    parent: &File,
    source_name: &OsStr,
    destination_name: &OsStr,
) -> io::Result<()> {
    replace_file_between_directories(parent, source_name, parent, destination_name)
}

/// Atomically replace one child file with another, relative to open directories.
///
/// Both names must be one normal path component. The source and destination
/// directories must be on the same volume.
pub fn replace_file_between_directories(
    source_parent: &File,
    source_name: &OsStr,
    destination_parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_child_name(source_name)?;
    validate_child_name(destination_name)?;
    #[cfg(windows)]
    {
        windows_handle::replace_child(
            source_parent,
            source_name,
            destination_parent,
            destination_name,
        )
    }
    #[cfg(unix)]
    {
        use rustix::fs::renameat;

        renameat(
            source_parent,
            source_name,
            destination_parent,
            destination_name,
        )
        .map_err(Into::into)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            source_parent,
            source_name,
            destination_parent,
            destination_name,
        );
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative file replacement is unavailable on this platform",
        ))
    }
}

/// Open a direct child directory without following a reparse point while
/// allowing the opened directory to be renamed through its handle.
///
/// `child_name` must be one normal path component. The returned Windows handle
/// has delete access and permits delete sharing so a handle-relative rename can
/// publish the directory without resolving its source path again.
pub fn open_child_directory_for_rename_without_following(
    parent: &File,
    child_name: &OsStr,
) -> io::Result<RenameableDirectory> {
    validate_child_name(child_name)?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_READ_ATTRIBUTES};

        let file = windows_handle::open_child_with_access(
            parent,
            child_name,
            true,
            DELETE | FILE_READ_ATTRIBUTES,
            true,
        )?;
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
        Ok(RenameableDirectory { file })
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, child_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows directory handles are unavailable on this platform",
        ))
    }
}

/// Rename an already-open directory to a direct child of `parent` without
/// replacing an existing entry.
///
/// The source handle must have been opened with delete access and must refer to
/// a directory on the same volume as `parent`.
pub fn move_directory_handle_to_child_without_replacing(
    source: &RenameableDirectory,
    parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_child_name(destination_name)?;
    #[cfg(windows)]
    {
        if is_reparse_point(&source.file)? || !source.file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source handle is not a directory",
            ));
        }
        windows_handle::rename_open_child(parent, &source.file, destination_name, false)
    }
    #[cfg(not(windows))]
    {
        let _ = (source, parent, destination_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative directory moves are available only on Windows",
        ))
    }
}

/// Replace one child file only when it still has the selected identity and
/// contents.
///
/// Identity and contents are checked through the pinned parent directory
/// immediately before the atomic rename. Filesystems do not provide a portable
/// atomic compare-and-replace primitive, so callers must keep this check
/// adjacent to publication and serialize cooperating writers with their
/// workspace lock.
pub fn replace_file_in_directory_if_unchanged(
    parent: &File,
    source_name: &OsStr,
    destination_name: &OsStr,
    expected_identity: FileIdentity,
    expected_contents: &[u8],
) -> io::Result<()> {
    let mut current = open_file_without_following(parent, Path::new("."), destination_name)?;
    if file_identity(&current)? != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "destination file changed after it was selected",
        ));
    }
    let read_limit = expected_contents
        .len()
        .checked_add(1)
        .and_then(|length| u64::try_from(length).ok())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut current_contents = Vec::new();
    current_contents
        .try_reserve_exact(expected_contents.len().saturating_add(1))
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    (&mut current)
        .take(read_limit)
        .read_to_end(&mut current_contents)?;
    if current_contents != expected_contents {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "destination contents changed after they were read",
        ));
    }
    // Windows can reject replacement while a handle to the destination is
    // still open, even when that handle allows delete sharing. The identity
    // and contents check is complete; cooperating writers remain serialized
    // by the caller's workspace lock until the rename below.
    drop(current);
    replace_file_in_directory(parent, source_name, destination_name)
}

/// Remove one child file only when it still has the selected identity and
/// contents.
///
/// Identity and contents are checked through the pinned parent directory
/// immediately before removal. Filesystems do not provide a portable
/// handle-relative compare-and-unlink operation, so callers must serialize
/// cooperating writers with their workspace lock.
pub fn remove_file_in_directory_if_unchanged(
    parent: &File,
    name: &OsStr,
    expected_identity: FileIdentity,
    expected_contents: &[u8],
) -> io::Result<()> {
    let mut current = open_file_without_following(parent, Path::new("."), name)?;
    if file_identity(&current)? != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "file identity changed after it was selected",
        ));
    }
    let read_limit = expected_contents
        .len()
        .checked_add(1)
        .and_then(|length| u64::try_from(length).ok())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut current_contents = Vec::new();
    current_contents
        .try_reserve_exact(expected_contents.len().saturating_add(1))
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    (&mut current)
        .take(read_limit)
        .read_to_end(&mut current_contents)?;
    if current_contents != expected_contents {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "file contents changed after they were read",
        ));
    }
    // Windows may reject removal while a handle to the child is still open.
    // The identity and contents check is complete; cooperating writers remain
    // serialized by the caller's workspace lock until removal below.
    drop(current);
    remove_file_in_directory(parent, name)
}

/// Remove one child file only when it still has the selected identity.
///
/// This is intended for best-effort cleanup of a uniquely created temporary
/// file after a write failure, when its incomplete contents are unknown.
pub fn remove_file_in_directory_if_identity(
    parent: &File,
    name: &OsStr,
    expected_identity: FileIdentity,
) -> io::Result<()> {
    let current = open_file_without_following(parent, Path::new("."), name)?;
    if file_identity(&current)? != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "file identity changed after it was selected",
        ));
    }
    drop(current);
    remove_file_in_directory(parent, name)
}

/// Remove one child file using an already-open directory handle.
///
/// `name` must be one normal path component.
pub fn remove_file_in_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    validate_child_name(name)?;
    #[cfg(windows)]
    {
        windows_handle::remove_child(parent, name)
    }
    #[cfg(unix)]
    {
        use rustix::fs::unlinkat;

        unlinkat(parent, name, rustix::fs::AtFlags::empty()).map_err(Into::into)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative file removal is unavailable on this platform",
        ))
    }
}

fn validate_child_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "child name must be one normal path component",
    ))
}

/// Open a directory without following a reparse point while allowing rename.
///
/// This is intended for a directory that is renamed into its final name after
/// it has been opened and verified. Unlike
/// [`open_directory_without_following`], the handle permits delete sharing.
pub fn open_directory_for_rename_without_following(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
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

#[cfg(windows)]
mod windows_handle {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;
    use std::mem::{self, offset_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;
    use std::ptr;

    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
        FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation,
        FileRenameInformationEx, NtCreateFile, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_READ_ATTRIBUTES, FileDispositionInfo, SYNCHRONIZE,
        SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    use windows_sys::Win32::System::WindowsProgramming::{
        FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
    };

    use super::is_reparse_point;

    pub(super) fn open_child(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
        let file = open_child_with_access(
            parent,
            name,
            directory,
            GENERIC_READ | FILE_READ_ATTRIBUTES,
            !directory,
        )?;
        validate_opened_child(file, directory)
    }

    pub(super) fn open_child_for_stable_read(parent: &File, name: &OsStr) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let file = open_child_with_access_and_share(
            parent,
            name,
            false,
            GENERIC_READ | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
        )?;
        validate_opened_child(file, false)
    }

    pub(super) fn open_child_for_child_updates(parent: &File, name: &OsStr) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD,
        };

        let file = open_child_with_access(
            parent,
            name,
            true,
            GENERIC_READ
                | FILE_READ_ATTRIBUTES
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_DELETE_CHILD,
            false,
        )?;
        validate_opened_child(file, true)
    }

    pub(super) fn open_child_for_file_updates(parent: &File, name: &OsStr) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ADD_FILE, FILE_DELETE_CHILD};

        let file = open_child_with_access(
            parent,
            name,
            true,
            GENERIC_READ | FILE_READ_ATTRIBUTES | FILE_ADD_FILE | FILE_DELETE_CHILD,
            false,
        )?;
        validate_opened_child(file, true)
    }

    pub(super) fn open_child_for_attribute_updates(
        parent: &File,
        name: &OsStr,
    ) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
        };

        let file = open_child_with_access_and_share(
            parent,
            name,
            false,
            FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
            0,
        )?;
        validate_opened_child(file, false)
    }

    pub(super) fn open_directory_for_child_updates(path: &Path) -> io::Result<File> {
        open_directory_for_updates(path, true)
    }

    pub(super) fn open_directory_for_file_updates(path: &Path) -> io::Result<File> {
        open_directory_for_updates(path, false)
    }

    fn open_directory_for_updates(path: &Path, allow_child_directories: bool) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let desired_access = GENERIC_READ
            | FILE_READ_ATTRIBUTES
            | FILE_ADD_FILE
            | FILE_DELETE_CHILD
            | if allow_child_directories {
                FILE_ADD_SUBDIRECTORY
            } else {
                0
            };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .access_mode(desired_access)
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

    fn validate_opened_child(file: File, directory: bool) -> io::Result<File> {
        if is_reparse_point(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "formatter input is a Windows reparse point",
            ));
        }
        let metadata = file.metadata()?;
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                if directory {
                    "path is not a directory"
                } else {
                    "formatter input is not a regular file"
                },
            ));
        }
        Ok(file)
    }

    pub(super) fn open_child_with_access(
        parent: &File,
        name: &OsStr,
        directory: bool,
        desired_access: u32,
        share_delete: bool,
    ) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let share_access =
            FILE_SHARE_READ | FILE_SHARE_WRITE | if share_delete { FILE_SHARE_DELETE } else { 0 };
        open_child_with_access_and_share(parent, name, directory, desired_access, share_access)
    }

    pub(super) fn open_child_with_access_and_share(
        parent: &File,
        name: &OsStr,
        directory: bool,
        desired_access: u32,
        share_access: u32,
    ) -> io::Result<File> {
        super::validate_child_name(name)?;
        let wide = name.encode_wide().collect::<Vec<_>>();
        let byte_length = wide
            .len()
            .checked_mul(mem::size_of::<u16>())
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "child name is too long"))?;
        let mut object_name = UNICODE_STRING {
            Length: byte_length,
            MaximumLength: byte_length,
            Buffer: wide.as_ptr().cast_mut(),
        };
        let object_attributes = OBJECT_ATTRIBUTES {
            Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle().cast(),
            ObjectName: &mut object_name,
            Attributes: OBJ_CASE_INSENSITIVE,
            ..OBJECT_ATTRIBUTES::default()
        };
        let mut io_status = IO_STATUS_BLOCK::default();
        let mut handle = ptr::null_mut();
        let create_options = FILE_OPEN_REPARSE_POINT
            | FILE_SYNCHRONOUS_IO_NONALERT
            | if directory {
                FILE_DIRECTORY_FILE
            } else {
                FILE_NON_DIRECTORY_FILE
            };
        // SAFETY: all pointers refer to live stack or vector storage for the
        // duration of the synchronous call. `RootDirectory` is an open
        // directory handle, and the single-component name cannot escape it.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                desired_access | SYNCHRONIZE,
                &object_attributes,
                &mut io_status,
                ptr::null(),
                0,
                share_access,
                FILE_OPEN,
                create_options,
                ptr::null(),
                0,
            )
        };
        if status < 0 {
            // SAFETY: this conversion accepts any NTSTATUS value.
            let win32_error = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(win32_error as i32));
        }
        if handle.is_null() {
            return Err(io::Error::other("Windows returned an empty file handle"));
        }
        // SAFETY: NtCreateFile succeeded and transferred ownership of `handle`.
        Ok(unsafe { File::from_raw_handle(handle.cast()) })
    }

    pub(super) fn create_child_directory(parent: &File, name: &OsStr) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD,
        };

        let file = create_child_with_access(
            parent,
            name,
            true,
            GENERIC_READ
                | FILE_READ_ATTRIBUTES
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_DELETE_CHILD,
            false,
        )?;
        validate_opened_child(file, true)
    }

    pub(super) fn create_child_file(parent: &File, name: &OsStr) -> io::Result<File> {
        let file = create_child_with_access(
            parent,
            name,
            false,
            GENERIC_WRITE | FILE_READ_ATTRIBUTES,
            true,
        )?;
        validate_opened_child(file, false)
    }

    pub(super) fn move_child_without_replacing(
        parent: &File,
        source_name: &OsStr,
        destination_name: &OsStr,
    ) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_READ_ATTRIBUTES};

        let source = open_child_with_access(
            parent,
            source_name,
            false,
            DELETE | FILE_READ_ATTRIBUTES,
            true,
        )?;
        validate_opened_child(source.try_clone()?, false)?;
        rename_open_child(parent, &source, destination_name, false)
    }

    fn create_child_with_access(
        parent: &File,
        name: &OsStr,
        directory: bool,
        desired_access: u32,
        write_through: bool,
    ) -> io::Result<File> {
        use windows_sys::Wdk::Storage::FileSystem::{FILE_CREATE, FILE_WRITE_THROUGH};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        super::validate_child_name(name)?;
        let wide = name.encode_wide().collect::<Vec<_>>();
        let byte_length = wide
            .len()
            .checked_mul(mem::size_of::<u16>())
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "child name is too long"))?;
        let mut object_name = UNICODE_STRING {
            Length: byte_length,
            MaximumLength: byte_length,
            Buffer: wide.as_ptr().cast_mut(),
        };
        let object_attributes = OBJECT_ATTRIBUTES {
            Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle().cast(),
            ObjectName: &mut object_name,
            Attributes: OBJ_CASE_INSENSITIVE,
            ..OBJECT_ATTRIBUTES::default()
        };
        let mut io_status = IO_STATUS_BLOCK::default();
        let mut handle = ptr::null_mut();
        let create_options = FILE_OPEN_REPARSE_POINT
            | FILE_SYNCHRONOUS_IO_NONALERT
            | if directory {
                FILE_DIRECTORY_FILE
            } else {
                FILE_NON_DIRECTORY_FILE
            }
            | if write_through { FILE_WRITE_THROUGH } else { 0 };
        // SAFETY: pointers refer to live stack or vector storage for the
        // synchronous call. `RootDirectory` is an open directory handle and
        // the validated single-component name cannot escape it.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                desired_access | SYNCHRONIZE,
                &object_attributes,
                &mut io_status,
                ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                create_options,
                ptr::null(),
                0,
            )
        };
        if status < 0 {
            // SAFETY: this conversion accepts any NTSTATUS value.
            let win32_error = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(win32_error as i32));
        }
        if handle.is_null() {
            return Err(io::Error::other("Windows returned an empty file handle"));
        }
        // SAFETY: NtCreateFile succeeded and transferred ownership of `handle`.
        Ok(unsafe { File::from_raw_handle(handle.cast()) })
    }

    pub(super) fn replace_child(
        source_parent: &File,
        source_name: &OsStr,
        destination_parent: &File,
        destination_name: &OsStr,
    ) -> io::Result<()> {
        let source = open_child_with_access(
            source_parent,
            source_name,
            false,
            DELETE | FILE_READ_ATTRIBUTES,
            true,
        )?;
        rename_open_child(destination_parent, &source, destination_name, true)
    }

    pub(super) fn rename_open_child(
        parent: &File,
        source: &File,
        destination_name: &OsStr,
        replace_if_exists: bool,
    ) -> io::Result<()> {
        let destination = destination_name.encode_wide().collect::<Vec<_>>();
        let file_name_bytes = destination
            .len()
            .checked_mul(mem::size_of::<u16>())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination is too long")
            })?;
        let file_name_bytes_u32 = u32::try_from(file_name_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination is too long"))?;
        let file_name_offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
        let information_length = mem::size_of::<FILE_RENAME_INFORMATION>()
            .checked_add(file_name_bytes)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "destination is too long")
            })?;
        let information_length_u32 = u32::try_from(information_length)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination is too long"))?;
        let mut storage = vec![0usize; information_length.div_ceil(mem::size_of::<usize>())];
        let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        let information_class = if replace_if_exists {
            FileRenameInformationEx
        } else {
            FileRenameInformation
        };
        // SAFETY: `storage` is pointer-aligned and sized for the fixed header
        // plus the UTF-16 filename bytes. The trailing name bytes are copied
        // into the flexible-array tail expected by FILE_RENAME_INFORMATION.
        unsafe {
            if replace_if_exists {
                // Preserve open readers on the old file while atomically
                // rebinding its name to the replacement.
                (*information).Anonymous.Flags =
                    FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
            } else {
                (*information).Anonymous.ReplaceIfExists = false;
            }
            (*information).RootDirectory = parent.as_raw_handle().cast();
            (*information).FileNameLength = file_name_bytes_u32;
            ptr::copy_nonoverlapping(
                destination.as_ptr(),
                information.cast::<u8>().add(file_name_offset).cast::<u16>(),
                destination.len(),
            );
        }
        let mut io_status = IO_STATUS_BLOCK::default();
        // SAFETY: `source` is an open file or directory with DELETE access.
        // `parent` is an open destination directory on the same volume. The
        // information buffer has the native layout and both handles remain
        // alive for the synchronous call.
        let status = unsafe {
            NtSetInformationFile(
                source.as_raw_handle().cast(),
                &mut io_status,
                information.cast(),
                information_length_u32,
                information_class,
            )
        };
        // The rename API only accepts `RenameableDirectory`, whose handle is
        // opened with FILE_SYNCHRONOUS_IO_NONALERT. A pending result would
        // violate that contract and would leave the stack-backed I/O status
        // block and rename buffer live for too short a period.
        if status == windows_sys::Win32::Foundation::STATUS_PENDING {
            return Err(io::Error::other(
                "synchronous directory rename unexpectedly returned STATUS_PENDING",
            ));
        }
        if status < 0 {
            // SAFETY: this conversion accepts any NTSTATUS value.
            let win32_error = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(win32_error as i32));
        }
        Ok(())
    }

    pub(super) fn remove_child(parent: &File, name: &OsStr) -> io::Result<()> {
        let file =
            open_child_with_access(parent, name, false, DELETE | FILE_READ_ATTRIBUTES, true)?;
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `file` has DELETE access and the disposition structure stays
        // alive for the duration of the synchronous call.
        let succeeded = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle().cast(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        create_child_directory_without_following, create_new_file_in_directory_without_following,
        move_file_in_directory_without_replacing, open_child_directory_without_following,
        open_directory_without_following, open_file_without_following, remove_file_in_directory,
        remove_file_in_directory_if_unchanged, replace_file_between_directories, sync_directory,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-dir-nofollow-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temporary test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opens_directories_and_rejects_a_final_symlink() {
        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        let link = directory.0.join("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let parent = open_directory_without_following(&directory.0).unwrap();
        assert!(open_directory_without_following(&target).is_ok());
        assert!(
            open_child_directory_without_following(
                &parent,
                &directory.0,
                std::ffi::OsStr::new("link"),
            )
            .is_err()
        );
        assert!(open_directory_without_following(&link).is_err());
    }

    #[test]
    fn child_file_operations_reject_names_that_escape_the_directory() {
        let directory = TestDirectory::new();
        let root = directory.0.join("root");
        let sibling = directory.0.join("victim");
        fs::create_dir(&root).unwrap();
        fs::write(&sibling, b"keep").unwrap();
        fs::write(root.join("temporary"), b"safe").unwrap();
        let root_handle = open_directory_without_following(&root).unwrap();

        assert!(
            open_file_without_following(&root_handle, &root, std::ffi::OsStr::new("../victim"),)
                .is_err()
        );
        assert!(remove_file_in_directory(&root_handle, std::ffi::OsStr::new("../victim")).is_err());
        assert!(
            super::replace_file_in_directory(
                &root_handle,
                std::ffi::OsStr::new("temporary"),
                std::ffi::OsStr::new("../victim"),
            )
            .is_err()
        );
        assert_eq!(fs::read(sibling).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("temporary")).unwrap(), b"safe");
    }

    #[test]
    fn replaces_a_file_across_open_directories_without_a_delete_gap() {
        let directory = TestDirectory::new();
        let source_directory = directory.0.join("source");
        let destination_directory = directory.0.join("destination");
        fs::create_dir(&source_directory).unwrap();
        fs::create_dir(&destination_directory).unwrap();
        fs::write(source_directory.join("staged"), b"new record").unwrap();
        let destination_path = destination_directory.join("record");
        fs::write(&destination_path, b"old record").unwrap();
        let old_reader = fs::File::open(&destination_path).unwrap();

        let source_parent = open_directory_without_following(&source_directory).unwrap();
        let destination_parent = open_directory_without_following(&destination_directory).unwrap();
        replace_file_between_directories(
            &source_parent,
            std::ffi::OsStr::new("staged"),
            &destination_parent,
            std::ffi::OsStr::new("record"),
        )
        .unwrap();

        assert_eq!(fs::read(&destination_path).unwrap(), b"new record");
        assert_eq!(
            fs::read(source_directory.join("staged"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let mut old_bytes = Vec::new();
        use std::io::Read;
        let mut old_reader = old_reader;
        old_reader.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, b"old record");
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "redox"
    ))]
    #[test]
    fn creates_and_publishes_files_relative_to_durable_directory_handles() {
        use std::io::Write;

        let directory = TestDirectory::new();
        let state = open_directory_without_following(&directory.0).unwrap();
        let index = create_child_directory_without_following(&state, std::ffi::OsStr::new("index"))
            .unwrap();
        sync_directory(&state).unwrap();
        let mut staged =
            create_new_file_in_directory_without_following(&index, std::ffi::OsStr::new("stage"))
                .unwrap();
        staged.write_all(b"prepared journal").unwrap();
        staged.sync_all().unwrap();
        drop(staged);

        move_file_in_directory_without_replacing(
            &index,
            std::ffi::OsStr::new("stage"),
            std::ffi::OsStr::new("operation.json"),
        )
        .unwrap();
        sync_directory(&index).unwrap();
        assert_eq!(
            fs::read(directory.0.join("index/operation.json")).unwrap(),
            b"prepared journal"
        );
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "redox"
    ))]
    #[test]
    fn no_replace_publication_preserves_an_existing_entry() {
        use std::io::Write;

        let directory = TestDirectory::new();
        let handle = open_directory_without_following(&directory.0).unwrap();
        fs::write(directory.0.join("operation.json"), b"existing").unwrap();
        let mut staged =
            create_new_file_in_directory_without_following(&handle, std::ffi::OsStr::new("stage"))
                .unwrap();
        staged.write_all(b"replacement").unwrap();
        staged.sync_all().unwrap();
        drop(staged);

        let error = move_file_in_directory_without_replacing(
            &handle,
            std::ffi::OsStr::new("stage"),
            std::ffi::OsStr::new("operation.json"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(directory.0.join("operation.json")).unwrap(),
            b"existing"
        );
        assert_eq!(fs::read(directory.0.join("stage")).unwrap(), b"replacement");
    }

    #[test]
    fn guarded_removal_requires_the_selected_identity_and_contents() {
        let directory = TestDirectory::new();
        let handle = open_directory_without_following(&directory.0).unwrap();
        let name = std::ffi::OsStr::new("operation.json");
        fs::write(directory.0.join(name), b"expected").unwrap();
        let original = open_file_without_following(&handle, &directory.0, name).unwrap();
        let identity = super::file_identity(&original).unwrap();
        drop(original);

        fs::write(directory.0.join(name), b"changed").unwrap();
        let error = remove_file_in_directory_if_unchanged(&handle, name, identity, b"expected")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(directory.0.join(name)).unwrap(), b"changed");

        let current = open_file_without_following(&handle, &directory.0, name).unwrap();
        let current_identity = super::file_identity(&current).unwrap();
        drop(current);
        remove_file_in_directory_if_unchanged(&handle, name, current_identity, b"changed").unwrap();
        assert!(!directory.0.join(name).exists());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        create_child_directory_without_following, create_new_file_in_directory_without_following,
        file_identity, move_directory_handle_to_child_without_replacing,
        move_file_in_directory_without_replacing,
        open_child_directory_for_rename_without_following, open_child_directory_without_following,
        open_directory_path_for_child_updates_without_following,
        open_directory_path_for_file_updates_without_following,
        open_directory_path_without_following, open_file_for_attribute_updates_without_following,
        remove_file_in_directory, replace_file_between_directories, replace_file_in_directory,
        replace_file_in_directory_if_unchanged, sync_directory,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-windows-handle-fs-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temporary test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn handle_relative_replace_and_remove_stay_in_the_open_directory() {
        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        let moved = directory.0.join("moved");
        fs::create_dir(&package).unwrap();
        fs::write(package.join("temporary"), b"formatted").unwrap();
        fs::write(package.join("sai.package"), b"original").unwrap();

        let canonical = fs::canonicalize(&package).unwrap();
        let package_handle =
            open_directory_path_for_file_updates_without_following(&canonical).unwrap();
        assert!(fs::rename(&package, &moved).is_err());

        replace_file_in_directory(
            &package_handle,
            std::ffi::OsStr::new("temporary"),
            std::ffi::OsStr::new("sai.package"),
        )
        .unwrap();
        assert_eq!(fs::read(package.join("sai.package")).unwrap(), b"formatted");
        assert!(!package.join("temporary").exists());

        remove_file_in_directory(&package_handle, std::ffi::OsStr::new("sai.package")).unwrap();
        assert!(!package.join("sai.package").exists());
    }

    #[test]
    fn cross_directory_replace_keeps_existing_reader_handles_valid() {
        let directory = TestDirectory::new();
        let source_directory = directory.0.join("source");
        let destination_directory = directory.0.join("destination");
        fs::create_dir(&source_directory).unwrap();
        fs::create_dir(&destination_directory).unwrap();
        fs::write(source_directory.join("staged"), b"new record").unwrap();
        let destination_path = destination_directory.join("record");
        fs::write(&destination_path, b"old record").unwrap();
        let old_reader = fs::File::open(&destination_path).unwrap();

        let source_parent =
            open_directory_path_for_file_updates_without_following(&source_directory).unwrap();
        let destination_parent =
            open_directory_path_for_file_updates_without_following(&destination_directory).unwrap();
        replace_file_between_directories(
            &source_parent,
            std::ffi::OsStr::new("staged"),
            &destination_parent,
            std::ffi::OsStr::new("record"),
        )
        .unwrap();

        assert_eq!(fs::read(&destination_path).unwrap(), b"new record");
        assert_eq!(
            fs::read(source_directory.join("staged"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let mut old_bytes = Vec::new();
        use std::io::Read;
        let mut old_reader = old_reader;
        old_reader.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, b"old record");
    }

    #[test]
    fn compare_before_replace_releases_the_destination_handle() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("temporary"), b"formatted").unwrap();
        fs::write(directory.0.join("sai.package"), b"original").unwrap();

        let package_path = fs::canonicalize(&directory.0).unwrap();
        let package =
            open_directory_path_for_file_updates_without_following(&package_path).unwrap();
        let current = super::open_file_without_following(
            &package,
            &package_path,
            std::ffi::OsStr::new("sai.package"),
        )
        .unwrap();
        let identity_guard = file_identity(&current).unwrap();

        replace_file_in_directory_if_unchanged(
            &package,
            std::ffi::OsStr::new("temporary"),
            std::ffi::OsStr::new("sai.package"),
            identity_guard,
            b"original",
        )
        .unwrap();
        assert_eq!(file_identity(&current).unwrap(), identity_guard);
        assert_eq!(
            fs::read(directory.0.join("sai.package")).unwrap(),
            b"formatted"
        );
    }

    #[test]
    fn opens_child_directories_relative_to_their_parent_handle() {
        let directory = TestDirectory::new();
        let parent_path = directory.0.join("parent");
        let child_path = parent_path.join("child");
        fs::create_dir(&parent_path).unwrap();
        fs::create_dir(&child_path).unwrap();

        let parent_path = fs::canonicalize(&parent_path).unwrap();
        let parent = open_directory_path_without_following(&parent_path).unwrap();
        let child = open_child_directory_without_following(
            &parent,
            &parent_path,
            std::ffi::OsStr::new("child"),
        )
        .unwrap();
        assert!(child.metadata().unwrap().is_dir());
    }

    #[test]
    fn attribute_update_handle_can_clear_readonly_without_content_write_access() {
        let directory = TestDirectory::new();
        let file_path = directory.0.join("readonly");
        let moved_path = directory.0.join("moved");
        fs::write(&file_path, b"unchanged").unwrap();

        let directory_handle =
            open_directory_path_without_following(&fs::canonicalize(&directory.0).unwrap())
                .unwrap();
        let mut update_guard = open_file_for_attribute_updates_without_following(
            &directory_handle,
            &directory.0,
            std::ffi::OsStr::new("readonly"),
        )
        .unwrap();
        assert!(
            update_guard.write_all(b"changed").is_err(),
            "the attribute handle cannot write file contents"
        );
        drop(update_guard);

        let mut permissions = fs::metadata(&file_path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file_path, permissions).unwrap();

        let mut file = open_file_for_attribute_updates_without_following(
            &directory_handle,
            &directory.0,
            std::ffi::OsStr::new("readonly"),
        )
        .unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        assert!(permissions.readonly());
        permissions.set_readonly(false);
        file.set_permissions(permissions).unwrap();
        assert!(
            file.write_all(b"changed").is_err(),
            "clearing the read-only flag does not grant content write access"
        );
        drop(file);

        assert!(!fs::metadata(&file_path).unwrap().permissions().readonly());
        assert_eq!(fs::read(&file_path).unwrap(), b"unchanged");
        fs::rename(&file_path, &moved_path).unwrap();
    }

    #[test]
    fn attribute_only_update_handles_do_not_exclude_each_other() {
        let directory = TestDirectory::new();
        let file_path = directory.0.join("readonly");
        fs::write(&file_path, b"unchanged").unwrap();

        let directory_handle =
            open_directory_path_without_following(&fs::canonicalize(&directory.0).unwrap())
                .unwrap();
        let update_guard = open_file_for_attribute_updates_without_following(
            &directory_handle,
            &directory.0,
            std::ffi::OsStr::new("readonly"),
        )
        .unwrap();

        let second_update_guard = open_file_for_attribute_updates_without_following(
            &directory_handle,
            &directory.0,
            std::ffi::OsStr::new("readonly"),
        )
        .expect("Windows does not apply share checks to attribute-only opens");
        drop(second_update_guard);
        drop(update_guard);
    }

    #[test]
    fn handle_relative_directory_move_preserves_identity_and_never_replaces() {
        let directory = TestDirectory::new();
        let parent = directory.0.join("parent");
        let source = parent.join("temporary");
        let destination = parent.join("src");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("marker"), b"source").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("marker"), b"destination").unwrap();

        let parent_handle = open_directory_path_for_child_updates_without_following(
            &fs::canonicalize(&parent).unwrap(),
        )
        .unwrap();
        let source_handle = open_child_directory_for_rename_without_following(
            &parent_handle,
            std::ffi::OsStr::new("temporary"),
        )
        .unwrap();
        let source_identity = file_identity(source_handle.as_file()).unwrap();

        let error = move_directory_handle_to_child_without_replacing(
            &source_handle,
            &parent_handle,
            std::ffi::OsStr::new("src"),
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            file_identity(source_handle.as_file()).unwrap(),
            source_identity
        );
        assert_eq!(fs::read(source.join("marker")).unwrap(), b"source");
        assert_eq!(
            fs::read(destination.join("marker")).unwrap(),
            b"destination"
        );
        drop(source_handle);
        fs::remove_dir_all(&destination).unwrap();

        let source_handle = open_child_directory_for_rename_without_following(
            &parent_handle,
            std::ffi::OsStr::new("temporary"),
        )
        .unwrap();
        move_directory_handle_to_child_without_replacing(
            &source_handle,
            &parent_handle,
            std::ffi::OsStr::new("src"),
        )
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("marker")).unwrap(), b"source");
        assert_eq!(
            file_identity(source_handle.as_file()).unwrap(),
            source_identity
        );
    }

    #[test]
    fn directory_flush_is_unsupported_and_child_publication_is_handle_relative() {
        use std::io::Write;

        let directory = TestDirectory::new();
        let state_path = fs::canonicalize(&directory.0).unwrap();
        let state = open_directory_path_for_child_updates_without_following(&state_path).unwrap();
        assert_eq!(
            sync_directory(&state).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        let index = create_child_directory_without_following(&state, std::ffi::OsStr::new("index"))
            .unwrap();
        let mut staged =
            create_new_file_in_directory_without_following(&index, std::ffi::OsStr::new("stage"))
                .unwrap();
        staged.write_all(b"prepared journal").unwrap();
        staged.sync_all().unwrap();
        drop(staged);

        move_file_in_directory_without_replacing(
            &index,
            std::ffi::OsStr::new("stage"),
            std::ffi::OsStr::new("operation.json"),
        )
        .unwrap();
        assert_eq!(
            fs::read(directory.0.join("index/operation.json")).unwrap(),
            b"prepared journal"
        );
    }
}

/// Visit the names of the direct entries in an already-open directory.
///
/// The visitor receives each filename as an [`OsStr`], so Unix filenames that
/// are not valid UTF-8 are preserved. The `.` and `..` entries are omitted.
/// Enumeration is anchored to `directory`; this function never resolves or
/// globs a path. On Windows, the handle must be synchronous and grant
/// directory-list access.
pub fn visit_directory_entry_names(
    directory: &File,
    mut visitor: impl FnMut(&OsStr) -> io::Result<()>,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        if !directory.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory entry visitor requires a directory handle",
            ));
        }

        let mut entries = rustix::fs::Dir::read_from(directory).map_err(io::Error::from)?;
        while let Some(entry) = entries.next() {
            let entry = entry.map_err(io::Error::from)?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == OsStr::new(".") || name == OsStr::new("..") {
                continue;
            }
            visitor(name)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory entry visitor requires a directory handle",
            ));
        }
        if is_reparse_point(directory)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory entry visitor does not enumerate a reparse point",
            ));
        }
        windows_directory_entries::visit(directory, &mut visitor)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, &mut visitor);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative directory enumeration is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
mod windows_directory_entries {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_NAMES_INFORMATION, FileNamesInformation, NtQueryDirectoryFile,
    };
    use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, STATUS_NO_MORE_FILES};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const BUFFER_BYTES: usize = 64 * 1024;

    pub(super) fn visit(
        directory: &File,
        visitor: &mut impl FnMut(&OsStr) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut restart_scan = true;
        loop {
            let mut buffer = vec![0usize; BUFFER_BYTES / size_of::<usize>()];
            let buffer_bytes = buffer.len() * size_of::<usize>();
            // SAFETY: `buffer` is pointer-aligned and owns `buffer_bytes`
            // writable bytes, which remain alive until the native query has
            // completed and its records have been consumed.
            let buffer = unsafe {
                std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer_bytes)
            };
            let mut io_status = IO_STATUS_BLOCK::default();
            // SAFETY: `directory` is required to be an open synchronous
            // directory handle. With synchronous I/O, Event and ApcRoutine
            // must be null; the call completes before returning, so the
            // output buffer and I/O status block remain alive for the query.
            // No callback or filename filter is supplied, so the query
            // returns names from this handle only.
            let status = unsafe {
                NtQueryDirectoryFile(
                    directory.as_raw_handle(),
                    ptr::null_mut(),
                    None,
                    ptr::null(),
                    &mut io_status,
                    buffer.as_mut_ptr().cast(),
                    u32::try_from(buffer_bytes).expect("directory query buffer fits u32"),
                    FileNamesInformation,
                    false,
                    ptr::null(),
                    restart_scan,
                )
            };

            if status == STATUS_NO_MORE_FILES {
                return Ok(());
            }
            if status != 0 {
                // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS value.
                let error = unsafe { RtlNtStatusToDosError(status) };
                return Err(io::Error::from_raw_os_error(error as i32));
            }

            let bytes_returned = io_status.Information;
            if bytes_returned > buffer_bytes {
                return Err(invalid_directory_record(
                    "Windows returned more directory data than the supplied buffer",
                ));
            }
            if bytes_returned == 0 {
                return Ok(());
            }
            visit_names_information(&buffer[..bytes_returned], visitor)?;
            restart_scan = false;
        }
    }

    fn visit_names_information(
        buffer: &[u8],
        visitor: &mut impl FnMut(&OsStr) -> io::Result<()>,
    ) -> io::Result<()> {
        let header_bytes = offset_of!(FILE_NAMES_INFORMATION, FileName);
        let mut offset = 0usize;

        loop {
            let remaining = buffer.len().checked_sub(offset).ok_or_else(|| {
                invalid_directory_record("Windows directory record offset overflowed")
            })?;
            if remaining < header_bytes {
                return Err(invalid_directory_record(
                    "Windows directory record is shorter than its header",
                ));
            }

            let next_entry_offset = read_u32(buffer, offset)? as usize;
            let name_bytes = read_u32(buffer, offset + 8)? as usize;
            if name_bytes % size_of::<u16>() != 0 {
                return Err(invalid_directory_record(
                    "Windows directory filename has an odd UTF-16 byte length",
                ));
            }
            if name_bytes == 0 {
                return Err(invalid_directory_record(
                    "Windows directory record has an empty filename",
                ));
            }
            let record_bytes = header_bytes.checked_add(name_bytes).ok_or_else(|| {
                invalid_directory_record("Windows directory record length overflowed")
            })?;
            if record_bytes > remaining {
                return Err(invalid_directory_record(
                    "Windows directory filename exceeds its record buffer",
                ));
            }
            if next_entry_offset != 0
                && (next_entry_offset < record_bytes
                    || next_entry_offset > remaining
                    || next_entry_offset % size_of::<u32>() != 0)
            {
                return Err(invalid_directory_record(
                    "Windows directory next-entry offset is outside its record buffer",
                ));
            }

            let name_start = offset + header_bytes;
            let name_end = name_start + name_bytes;
            let units = buffer[name_start..name_end]
                .chunks_exact(size_of::<u16>())
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            if units.contains(&0) {
                return Err(invalid_directory_record(
                    "Windows directory filename contains a NUL code unit",
                ));
            }
            let name = OsString::from_wide(&units);
            if name != OsStr::new(".") && name != OsStr::new("..") {
                if name.to_string_lossy().contains(['\\', '/']) {
                    return Err(invalid_directory_record(
                        "Windows directory filename contains a path separator",
                    ));
                }
                visitor(&name)?;
            }

            if next_entry_offset == 0 {
                return Ok(());
            }
            offset = offset.checked_add(next_entry_offset).ok_or_else(|| {
                invalid_directory_record("Windows directory record offset overflowed")
            })?;
        }
    }

    fn read_u32(buffer: &[u8], offset: usize) -> io::Result<u32> {
        let end = offset
            .checked_add(size_of::<u32>())
            .ok_or_else(|| invalid_directory_record("Windows directory field offset overflowed"))?;
        let bytes = buffer
            .get(offset..end)
            .ok_or_else(|| invalid_directory_record("Windows directory header is truncated"))?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn invalid_directory_record(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    #[cfg(test)]
    mod tests {
        use super::{FILE_NAMES_INFORMATION, visit_names_information};
        use std::io;
        use std::mem::offset_of;

        #[test]
        fn rejects_odd_utf16_lengths_and_out_of_bounds_records() {
            let header_bytes = offset_of!(FILE_NAMES_INFORMATION, FileName);
            let mut odd_length = vec![0u8; header_bytes + 2];
            odd_length[8..12].copy_from_slice(&1u32.to_le_bytes());
            assert_eq!(
                visit_names_information(&odd_length, &mut |_| Ok(()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
            );

            let mut truncated = vec![0u8; header_bytes + 1];
            truncated[8..12].copy_from_slice(&2u32.to_le_bytes());
            assert_eq!(
                visit_names_information(&truncated, &mut |_| Ok(()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
            );
        }
    }
}

#[cfg(all(test, unix))]
mod directory_entry_visitor_unix_tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{open_directory_without_following, visit_directory_entry_names};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-directory-visitor-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temporary test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn collect_names(directory: &fs::File) -> io::Result<BTreeSet<OsString>> {
        let mut names = BTreeSet::new();
        visit_directory_entry_names(directory, |name| {
            names.insert(name.to_os_string());
            Ok(())
        })?;
        Ok(names)
    }

    #[test]
    fn visits_only_direct_entries_and_restarts_on_repeated_scans() {
        let temporary = TestDirectory::new();
        let root = temporary.0.join("index");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("header.json"), b"header").unwrap();
        fs::write(root.join("café-東京.json"), b"unicode").unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/hidden.json"), b"nested").unwrap();

        let handle = open_directory_without_following(&root).unwrap();
        let expected = BTreeSet::from([
            OsString::from("header.json"),
            OsString::from("café-東京.json"),
            OsString::from("nested"),
        ]);
        assert_eq!(collect_names(&handle).unwrap(), expected);
        assert_eq!(collect_names(&handle).unwrap(), expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_non_utf8_names() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let temporary = TestDirectory::new();
        let name = OsStr::from_bytes(b"entry-\xff.json");
        fs::write(temporary.0.join(name), b"opaque name").unwrap();
        let handle = open_directory_without_following(&temporary.0).unwrap();

        let names = collect_names(&handle).unwrap();

        assert_eq!(names.len(), 1);
        assert_eq!(
            names.first().unwrap().as_os_str().as_bytes(),
            b"entry-\xff.json"
        );
    }

    #[test]
    fn enumeration_stays_anchored_after_directory_path_is_replaced() {
        let temporary = TestDirectory::new();
        let original_path = temporary.0.join("index");
        let moved_path = temporary.0.join("index-moved");
        fs::create_dir(&original_path).unwrap();
        fs::write(original_path.join("before.json"), b"original").unwrap();
        let handle = open_directory_without_following(&original_path).unwrap();

        fs::rename(&original_path, &moved_path).unwrap();
        fs::create_dir(&original_path).unwrap();
        fs::write(original_path.join("replacement.json"), b"replacement").unwrap();

        let names = collect_names(&handle).unwrap();

        assert_eq!(names, BTreeSet::from([OsString::from("before.json")]));
    }
}

#[cfg(all(test, windows))]
mod directory_entry_visitor_windows_tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{open_directory_without_following, visit_directory_entry_names};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-directory-visitor-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temporary test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn collect_names(directory: &fs::File) -> io::Result<BTreeSet<OsString>> {
        let mut names = BTreeSet::new();
        visit_directory_entry_names(directory, |name| {
            names.insert(name.to_os_string());
            Ok(())
        })?;
        Ok(names)
    }

    #[test]
    fn native_query_reads_direct_unicode_entries_and_restarts() {
        let temporary = TestDirectory::new();
        let root = temporary.0.join("index");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("header.json"), b"header").unwrap();
        fs::write(root.join("café-東京.json"), b"unicode").unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/hidden.json"), b"nested").unwrap();
        let handle = open_directory_without_following(&root).unwrap();
        let expected = BTreeSet::from([
            OsString::from("header.json"),
            OsString::from("café-東京.json"),
            OsString::from("nested"),
        ]);

        assert_eq!(collect_names(&handle).unwrap(), expected);
        assert_eq!(collect_names(&handle).unwrap(), expected);
    }
}
