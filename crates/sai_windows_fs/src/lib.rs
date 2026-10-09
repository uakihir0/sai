//! Small, safe wrappers for Windows filesystem metadata not exposed by stable Rust.

use std::fs::File;
use std::io;

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
