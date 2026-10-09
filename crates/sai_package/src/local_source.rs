//! Filesystem primitives for local path package sources.
//!
//! Git-backed package trees are resolved by a separate provider. This module
//! handles local package directories without invoking external tools.

use std::fmt;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::manifest::{
    MAX_PACKAGE_MANIFEST_BYTES, PackageParseOutput, PackageRole, parse_for_role,
};
use crate::package_path::DependencyPackagePath;

/// A local package directory identified after filesystem path resolution.
///
/// Equality compares canonical paths and is suitable for checking whether two
/// local path pins name the same directory. The path is intentionally exposed
/// only to loader code; callers should not include it in user-facing
/// diagnostics because it may contain host-specific absolute paths. The
/// manifest bytes are captured during `open`, so later parsing uses the file
/// that was safely opened rather than reopening a mutable path.
#[derive(Clone)]
pub struct LocalPackageDirectory {
    canonical_path: PathBuf,
    manifest_source: Arc<[u8]>,
}

/// A filesystem failure while resolving or reading a local package.
///
/// Errors retain operation and `ErrorKind`, but not the affected absolute
/// path, so formatting an error cannot disclose a host filesystem location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalPackageError {
    RelativeDirectory,
    Io {
        operation: LocalPackageIoOperation,
        kind: io::ErrorKind,
    },
    NotDirectory,
    MissingManifest,
    ManifestNotFile,
    ManifestTooLarge,
}

/// The filesystem operation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPackageIoOperation {
    CanonicalizeDirectory,
    InspectDirectory,
    OpenManifest,
    ReadManifest,
}

impl LocalPackageDirectory {
    /// Resolves an existing absolute directory and requires a `sai.package`
    /// entry within it.
    ///
    /// Filesystem canonicalization resolves symlinks using the host's path
    /// semantics and produces an absolute path without `.` or `..` components.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LocalPackageError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(LocalPackageError::RelativeDirectory);
        }

        let canonical_path = fs::canonicalize(path).map_err(|error| LocalPackageError::Io {
            operation: LocalPackageIoOperation::CanonicalizeDirectory,
            kind: error.kind(),
        })?;
        let metadata = fs::metadata(&canonical_path).map_err(|error| LocalPackageError::Io {
            operation: LocalPackageIoOperation::InspectDirectory,
            kind: error.kind(),
        })?;
        if !metadata.is_dir() {
            return Err(LocalPackageError::NotDirectory);
        }

        let manifest_source = read_manifest(&canonical_path.join("sai.package"))?;

        Ok(Self {
            canonical_path,
            manifest_source,
        })
    }

    /// Resolves a dependency path relative to this package directory.
    pub fn resolve_dependency(
        &self,
        path: &DependencyPackagePath,
    ) -> Result<Self, LocalPackageError> {
        Self::open(self.canonical_path.join(path.as_str()))
    }

    /// Returns the absolute canonical directory for loader use.
    ///
    /// Avoid displaying this value in diagnostics; local paths are
    /// host-specific and may reveal private filesystem layout.
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    /// Reads and parses this package's manifest using the requested package
    /// role. The caller remains responsible for reporting parse diagnostics
    /// and validating package semantics before resolver use.
    pub fn parse_manifest(&self, role: PackageRole) -> PackageParseOutput {
        parse_for_role(&self.manifest_source, role)
    }
}

impl PartialEq for LocalPackageDirectory {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
    }
}

impl Eq for LocalPackageDirectory {}

impl fmt::Display for LocalPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeDirectory => {
                formatter.write_str("local package directory must be absolute")
            }
            Self::Io { operation, kind } => {
                write!(
                    formatter,
                    "local package filesystem error during {operation}: {kind}"
                )
            }
            Self::NotDirectory => formatter.write_str("local package source is not a directory"),
            Self::MissingManifest => {
                formatter.write_str("local package directory has no `sai.package` file")
            }
            Self::ManifestNotFile => {
                formatter.write_str("local package `sai.package` is not a file")
            }
            Self::ManifestTooLarge => write!(
                formatter,
                "local package `sai.package` exceeds the {MAX_PACKAGE_MANIFEST_BYTES} byte limit"
            ),
        }
    }
}

impl fmt::Display for LocalPackageIoOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::CanonicalizeDirectory => "canonicalizing its directory",
            Self::InspectDirectory => "inspecting its directory",
            Self::OpenManifest => "opening `sai.package`",
            Self::ReadManifest => "reading `sai.package`",
        };
        formatter.write_str(operation)
    }
}

impl fmt::Debug for LocalPackageDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalPackageDirectory")
            .finish_non_exhaustive()
    }
}

impl std::error::Error for LocalPackageError {}

fn read_manifest(path: &Path) -> Result<Arc<[u8]>, LocalPackageError> {
    let file = open_manifest(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => LocalPackageError::MissingManifest,
        kind => LocalPackageError::Io {
            operation: LocalPackageIoOperation::OpenManifest,
            kind,
        },
    })?;
    let metadata = file.metadata().map_err(|error| LocalPackageError::Io {
        operation: LocalPackageIoOperation::OpenManifest,
        kind: error.kind(),
    })?;
    if !metadata.is_file() {
        return Err(LocalPackageError::ManifestNotFile);
    }
    if metadata.len() > MAX_PACKAGE_MANIFEST_BYTES as u64 {
        return Err(LocalPackageError::ManifestTooLarge);
    }

    let mut source = Vec::new();
    file.take(MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut source)
        .map_err(|error| LocalPackageError::Io {
            operation: LocalPackageIoOperation::ReadManifest,
            kind: error.kind(),
        })?;
    if source.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(LocalPackageError::ManifestTooLarge);
    }
    Ok(Arc::from(source))
}

#[cfg(unix)]
fn open_manifest(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_manifest(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::manifest::PackageRole;
    use crate::package_path::DependencyPackagePath;

    use super::{LocalPackageDirectory, LocalPackageError};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-package-local-source-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary package tree");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn package(&self, relative: &str, manifest: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create package directory");
            fs::write(path.join("sai.package"), manifest).expect("write package manifest");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const ROOT_MANIFEST: &[u8] = b"package acme:root @ 1.0.0\n\
        source \"src\"\n\
        targets { wasm32-core }\n";
    const DEPENDENCY_MANIFEST: &[u8] = b"package acme:dependency @ 1.2.0\n\
        source \"src\"\n\
        targets { wasm32-core }\n";

    #[test]
    fn opens_absolute_package_directories_and_reads_manifests_by_role() {
        let tree = TempTree::new();
        let directory = tree.package("root", ROOT_MANIFEST);
        let package = LocalPackageDirectory::open(&directory).expect("open local package");

        assert!(package.path().is_absolute());
        assert!(package.path().ends_with("root"));
        assert!(!format!("{package:?}").contains(tree.path().to_str().unwrap()));
        let parsed = package.parse_manifest(PackageRole::Root);
        assert_eq!(parsed.role(), PackageRole::Root);
        assert!(parsed.errors().is_empty());
        assert_eq!(
            parsed
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .identity
                .to_string(),
            "acme:root"
        );

        let dependency_path = tree.package("dependency", DEPENDENCY_MANIFEST);
        let dependency = LocalPackageDirectory::open(dependency_path)
            .expect("open local dependency package")
            .parse_manifest(PackageRole::Dependency);
        assert_eq!(dependency.role(), PackageRole::Dependency);
        assert!(dependency.errors().is_empty());
    }

    #[test]
    fn manifest_parsing_uses_the_bytes_captured_when_the_directory_was_opened() {
        let tree = TempTree::new();
        let directory = tree.package("root", ROOT_MANIFEST);
        let first = LocalPackageDirectory::open(&directory).expect("open local package");

        let changed_manifest = b"package acme:root @ 2.0.0\n\
            source \"src\"\n\
            targets { wasm32-core }\n";
        fs::write(directory.join("sai.package"), changed_manifest)
            .expect("replace package manifest after opening source");

        let first_parse = first.parse_manifest(PackageRole::Root);
        assert_eq!(
            first_parse
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .version
                .to_string(),
            "1.0.0"
        );

        let second = LocalPackageDirectory::open(&directory).expect("reopen local package");
        assert_eq!(
            first, second,
            "directory identity is independent of content"
        );
        assert_eq!(
            second
                .parse_manifest(PackageRole::Root)
                .file()
                .header
                .as_ref()
                .unwrap()
                .value
                .version
                .to_string(),
            "2.0.0"
        );
    }

    #[test]
    fn resolves_paths_after_lexical_normalization_and_symlink_canonicalization() {
        let tree = TempTree::new();
        let requiring = tree.package("workspace/root", ROOT_MANIFEST);
        let dependency = tree.package("workspace/dependency", DEPENDENCY_MANIFEST);
        fs::create_dir(tree.path().join("workspace/root/unused"))
            .expect("create intermediate package path component");
        let requiring = LocalPackageDirectory::open(requiring).expect("open requiring package");

        // The existing intermediate directory is traversed and then removed
        // by the operating system's handling of `..`.
        let spelling = DependencyPackagePath::parse("unused/../../dependency")
            .expect("valid package-relative dependency path");
        let resolved = requiring
            .resolve_dependency(&spelling)
            .expect("resolve package-relative dependency");
        assert_eq!(
            resolved.path(),
            fs::canonicalize(dependency).expect("canonical dependency path")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let alias = tree.path().join("dependency-alias");
            symlink(resolved.path(), &alias).expect("create package directory symlink");
            let alias_path = DependencyPackagePath::parse("../../dependency-alias")
                .expect("valid package-relative symlink path");
            let through_symlink = requiring
                .resolve_dependency(&alias_path)
                .expect("resolve symlink package directory");
            assert_eq!(through_symlink, resolved);
        }
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_after_symlinks_follow_filesystem_resolution() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let requiring = tree.package("workspace/root", ROOT_MANIFEST);
        let root_dependency = tree.package("workspace/root/dep", ROOT_MANIFEST);
        let outside_dependency = tree.package("outside/dep", DEPENDENCY_MANIFEST);
        let symlink_target = tree.path().join("outside/nested");
        fs::create_dir(&symlink_target).expect("create symlink target");
        symlink(&symlink_target, requiring.join("link")).expect("create directory symlink");

        let requiring = LocalPackageDirectory::open(requiring).expect("open requiring package");
        let path = DependencyPackagePath::parse("link/../dep")
            .expect("valid package-relative symlink path");
        let resolved = requiring
            .resolve_dependency(&path)
            .expect("resolve path through symlink and parent component");

        assert_eq!(
            resolved.path(),
            fs::canonicalize(outside_dependency).expect("canonical outside dependency")
        );
        assert_ne!(
            resolved.path(),
            fs::canonicalize(root_dependency).expect("canonical root dependency")
        );
    }

    #[test]
    fn rejects_missing_and_non_directory_package_sources_without_disclosing_paths() {
        let tree = TempTree::new();
        let missing = tree.path().join("missing");
        assert!(matches!(
            LocalPackageDirectory::open(&missing),
            Err(LocalPackageError::Io { .. })
        ));

        let no_manifest = tree.path().join("no-manifest");
        fs::create_dir(&no_manifest).expect("create directory without manifest");
        assert_eq!(
            LocalPackageDirectory::open(&no_manifest),
            Err(LocalPackageError::MissingManifest)
        );

        let file = tree.path().join("file");
        fs::write(&file, b"not a directory").expect("write regular file");
        assert_eq!(
            LocalPackageDirectory::open(&file),
            Err(LocalPackageError::NotDirectory)
        );

        let relative = Path::new("relative/package");
        assert_eq!(
            LocalPackageDirectory::open(relative),
            Err(LocalPackageError::RelativeDirectory)
        );
        assert!(
            !format!("{}", LocalPackageError::MissingManifest)
                .contains(tree.path().to_str().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_manifest_symlink_to_a_special_file() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let directory = tree.path().join("special-manifest");
        fs::create_dir(&directory).expect("create package directory");
        symlink("/dev/null", directory.join("sai.package")).expect("link special file");

        assert_eq!(
            LocalPackageDirectory::open(directory),
            Err(LocalPackageError::ManifestNotFile)
        );
    }

    #[test]
    fn rejects_local_manifests_over_the_fixed_size_limit() {
        let tree = TempTree::new();
        let directory = tree.path().join("large-manifest");
        fs::create_dir(&directory).expect("create package directory");
        fs::File::create(directory.join("sai.package"))
            .expect("create package manifest")
            .set_len(super::MAX_PACKAGE_MANIFEST_BYTES as u64 + 1)
            .expect("extend package manifest");

        assert_eq!(
            LocalPackageDirectory::open(directory),
            Err(LocalPackageError::ManifestTooLarge)
        );
    }
}
