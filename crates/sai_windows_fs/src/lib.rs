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
