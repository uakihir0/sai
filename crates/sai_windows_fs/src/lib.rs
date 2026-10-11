//! Safe wrappers for Windows filesystem metadata and atomic replacement not exposed by stable Rust.

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
    use std::io;
    use std::path::Path;

    use super::replace_file;

    #[test]
    fn rejects_embedded_nul_in_windows_paths() {
        let error =
            replace_file(Path::new("source\0ignored"), Path::new("destination")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
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
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
        use windows_sys::Win32::Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
        };
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let mut flags = if exclusive {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        };
        if !wait {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: the file handle is live for this synchronous operation, and
        // `overlapped` remains writable and in scope until it returns.
        let succeeded =
            unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
        if succeeded != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !wait && error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        Err(error)
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

/// Release the lock over the first byte acquired by [`lock_file_range`].
pub fn unlock_file_range(file: &File) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let mut overlapped = OVERLAPPED::default();
        // SAFETY: `file` owns a live handle and `overlapped` points to a
        // writable structure containing the same zero offset used to lock.
        let succeeded = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
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
