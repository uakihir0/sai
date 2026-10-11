use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Failure while locating the nearest package containing a regular
/// `sai.package` file.
#[derive(Debug)]
pub enum PackageDiscoveryError {
    StartDirectoryIo { path: PathBuf, source: io::Error },
    StartPathNotDirectory(PathBuf),
    ManifestMetadataIo { path: PathBuf, source: io::Error },
    NoPackageFound { start_directory: PathBuf },
}

impl fmt::Display for PackageDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartDirectoryIo { path, source } => {
                write!(
                    formatter,
                    "could not resolve start directory {path:?}: {source}"
                )
            }
            Self::StartPathNotDirectory(path) => {
                write!(
                    formatter,
                    "package search start path is not a directory: {path:?}"
                )
            }
            Self::ManifestMetadataIo { path, source } => {
                write!(
                    formatter,
                    "could not inspect package manifest {path:?}: {source}"
                )
            }
            Self::NoPackageFound { start_directory } => {
                write!(
                    formatter,
                    "no package found from start directory {start_directory:?}"
                )
            }
        }
    }
}

impl Error for PackageDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StartDirectoryIo { source, .. } | Self::ManifestMetadataIo { source, .. } => {
                Some(source)
            }
            Self::StartPathNotDirectory(_) | Self::NoPackageFound { .. } => None,
        }
    }
}

/// Finds the nearest package at or above `start_directory`.
///
/// Relative start paths are resolved against `current_directory`. The search
/// begins at the canonicalized start directory and returns the canonical
/// package directory. A `sai.package` directory or broken symlink is ignored;
/// a symlink whose target is a regular file counts as a regular file.
pub fn find_root_package(
    start_directory: impl AsRef<Path>,
    current_directory: impl AsRef<Path>,
) -> Result<PathBuf, PackageDiscoveryError> {
    let requested_start = start_directory.as_ref();
    let absolute_start = if requested_start.is_absolute() {
        requested_start.to_path_buf()
    } else {
        current_directory.as_ref().join(requested_start)
    };
    let start = fs::canonicalize(&absolute_start).map_err(|source| {
        PackageDiscoveryError::StartDirectoryIo {
            path: absolute_start.clone(),
            source,
        }
    })?;
    if !fs::metadata(&start)
        .map_err(|source| PackageDiscoveryError::StartDirectoryIo {
            path: start.clone(),
            source,
        })?
        .is_dir()
    {
        return Err(PackageDiscoveryError::StartPathNotDirectory(start));
    }

    let mut directory = start.as_path();
    loop {
        let manifest = directory.join("sai.package");
        match fs::metadata(&manifest) {
            Ok(metadata) if metadata.is_file() => return Ok(directory.to_path_buf()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(PackageDiscoveryError::ManifestMetadataIo {
                    path: manifest,
                    source,
                });
            }
        }

        let Some(parent) = directory.parent() else {
            return Err(PackageDiscoveryError::NoPackageFound {
                start_directory: start,
            });
        };
        directory = parent;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{PackageDiscoveryError, find_root_package};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sai-package-discovery-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn add_package(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("sai.package"), b"").unwrap();
    }

    #[test]
    fn finds_the_nearest_package_and_stops_before_an_enclosing_package() {
        let directory = TestDirectory::new();
        add_package(&directory.0);
        let nested = directory.0.join("nested");
        add_package(&nested);
        let start = nested.join("src/deep");
        fs::create_dir_all(&start).unwrap();

        assert_eq!(
            find_root_package(&start, &directory.0).unwrap(),
            fs::canonicalize(nested).unwrap()
        );
    }

    #[test]
    fn resolves_relative_start_paths_against_the_supplied_current_directory() {
        let directory = TestDirectory::new();
        add_package(&directory.0);
        let nested = directory.0.join("nested");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            find_root_package("nested", &directory.0).unwrap(),
            fs::canonicalize(&directory.0).unwrap()
        );
    }

    #[test]
    fn ignores_a_manifest_directory_while_searching_ancestors() {
        let directory = TestDirectory::new();
        add_package(&directory.0);
        let nested = directory.0.join("nested");
        fs::create_dir_all(nested.join("sai.package")).unwrap();

        assert_eq!(
            find_root_package(&nested, &directory.0).unwrap(),
            fs::canonicalize(&directory.0).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_a_broken_manifest_link_while_searching_ancestors() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        add_package(&directory.0);
        let nested = directory.0.join("nested");
        fs::create_dir_all(&nested).unwrap();
        symlink(
            directory.0.join("missing-manifest-target"),
            nested.join("sai.package"),
        )
        .unwrap();

        assert_eq!(
            find_root_package(&nested, &directory.0).unwrap(),
            fs::canonicalize(&directory.0).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_a_start_directory_symlink_before_searching_ancestors() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        add_package(&package);
        let real_start = package.join("nested");
        fs::create_dir_all(&real_start).unwrap();
        let alias = directory.0.join("start-alias");
        symlink(&real_start, &alias).unwrap();

        assert_eq!(
            find_root_package(&alias, &directory.0).unwrap(),
            fs::canonicalize(package).unwrap()
        );
    }

    #[test]
    fn reports_when_no_package_exists_before_the_filesystem_root() {
        let directory = TestDirectory::new();
        let error = find_root_package(&directory.0, &directory.0).unwrap_err();

        assert!(matches!(
            error,
            PackageDiscoveryError::NoPackageFound { start_directory }
                if start_directory == fs::canonicalize(&directory.0).unwrap()
        ));
    }

    #[test]
    fn rejects_a_file_as_the_start_directory() {
        let directory = TestDirectory::new();
        let file = directory.0.join("file");
        fs::write(&file, b"").unwrap();

        assert!(matches!(
            find_root_package(&file, &directory.0),
            Err(PackageDiscoveryError::StartPathNotDirectory(path))
                if path == fs::canonicalize(file).unwrap()
        ));
    }
}
