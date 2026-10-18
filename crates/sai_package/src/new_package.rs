use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use sha2::{Digest, Sha256};

use crate::identity::{PackageIdentity, PackageIdentityError};

const DEFAULT_TARGET: &str = "wasm32-wasi";
const PACKAGE_VERSION: &str = "0.1.0";
const KNOWN_TARGETS: &[&str] = &[
    "wasm32-core",
    "wasm64-core",
    "wasm32-wasi",
    "wasm64-wasi",
    "x86_64-linux",
    "aarch64-linux",
    "aarch64-macos",
    "x86_64-windows",
];
const TEMPORARY_FILE_PREFIX: &str = ".sai-new-";
const TEMPORARY_FILE_ATTEMPTS: usize = 10;

const GITIGNORE: &[u8] = b"/out/\n/.sai/\n";
const ROOT_MODULE: &[u8] = b"pub fn add(a: I32, b: I32) -> I32 {\n    a + b\n}\n\ntest \"add adds two numbers\" {\n    assert add(2, 3) == 5\n}\n";

/// Information about one file written by `sai new`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPackageFile {
    /// Package-relative path with `/` separators.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// Deterministic result metadata for a successfully created package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPackageReceipt {
    pub package: String,
    /// Destination as supplied by the caller, normalized for CLI output.
    pub directory: String,
    /// Files appear in `sai.package`, `.gitignore`, `src/lib.sai` order.
    pub files: Vec<NewPackageFile>,
}

/// An input or filesystem error while creating a package.
#[derive(Debug)]
pub enum NewPackageError {
    EmptyDestination,
    InvalidPackageIdentity(String),
    ReservedNamespace,
    ReservedPackageName,
    UnknownTarget(String),
    DuplicateTarget(String),
    DestinationNotEmpty(PathBuf),
    UnrepresentableDestination(PathBuf),
    Interrupted,
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for NewPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDestination => formatter.write_str("the destination path is empty"),
            Self::InvalidPackageIdentity(identity) => {
                write!(formatter, "invalid package identity {identity:?}")
            }
            Self::ReservedNamespace => {
                formatter.write_str("the package namespace `wasi` is reserved")
            }
            Self::ReservedPackageName => formatter
                .write_str("the package identities `sai:core` and `sai:alloc` are reserved"),
            Self::UnknownTarget(target) => write!(formatter, "unknown target {target:?}"),
            Self::DuplicateTarget(target) => {
                write!(formatter, "target {target:?} was specified twice")
            }
            Self::DestinationNotEmpty(path) => {
                write!(formatter, "destination is not an empty directory: {path:?}")
            }
            Self::UnrepresentableDestination(path) => write!(
                formatter,
                "destination path cannot be represented in command output: {path:?}"
            ),
            Self::Interrupted => formatter.write_str("package creation was interrupted"),
            Self::Io { path, source } => write!(formatter, "could not write {path:?}: {source}"),
        }
    }
}

impl Error for NewPackageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Creates a new library package in `destination`.
///
/// An empty `targets` list selects only `wasm32-wasi`. Supplied targets are
/// validated and kept in their original order. The parent directory must
/// already exist. If writing fails after the destination is prepared, files
/// created by this call are removed on a best-effort basis.
pub fn create_new_package(
    destination: &Path,
    identity: &str,
    targets: &[String],
) -> Result<NewPackageReceipt, NewPackageError> {
    create_new_package_with_cancel_check(destination, identity, targets, || false)
}

/// Creates a package while checking `is_cancelled` at filesystem safe points.
pub fn create_new_package_with_cancel_check(
    destination: &Path,
    identity: &str,
    targets: &[String],
    is_cancelled: impl Fn() -> bool,
) -> Result<NewPackageReceipt, NewPackageError> {
    if is_cancelled() {
        return Err(NewPackageError::Interrupted);
    }
    if destination.as_os_str().is_empty() {
        return Err(NewPackageError::EmptyDestination);
    }
    let formatted_destination = format_cli_path(destination)
        .ok_or_else(|| NewPackageError::UnrepresentableDestination(destination.to_path_buf()))?;
    validate_new_package_identity(identity)?;
    let targets = validate_targets(targets)?;
    let files = render_package_files(identity, &targets);

    let destination_for_io = normalized_destination(destination)?;
    let prepared = prepare_destination(&destination_for_io)?;
    if prepared.root_identity.is_none() {
        rollback(prepared, None, &[], None, None);
        return Err(NewPackageError::Io {
            path: destination_for_io,
            source: io::Error::other("could not identify the package destination"),
        });
    }
    if is_cancelled() {
        rollback(prepared, None, &[], None, None);
        return Err(NewPackageError::Interrupted);
    }
    let source_directory = destination_for_io.join("src");
    let (temporary_source_name, source_file) = match create_temporary_directory(
        &prepared.root_dir,
        &prepared.root_file,
        &destination_for_io,
    ) {
        Ok(source) => source,
        Err(source) => {
            rollback(prepared, None, &[], None, None);
            return Err(NewPackageError::Io {
                path: source_directory,
                source,
            });
        }
    };
    let Some(source_dir_identity) = file_identity(&source_file) else {
        drop(source_file);
        rollback(prepared, None, &[], None, None);
        return Err(NewPackageError::Io {
            path: source_directory,
            source: io::Error::other("could not identify the new source directory"),
        });
    };
    let temporary_source_path = destination_for_io.join(&temporary_source_name);
    drop(source_file);
    let publication_file = match open_directory_for_publication(
        &prepared.root_file,
        &temporary_source_name,
        &temporary_source_path,
    ) {
        Ok(file)
            if file_identity(publication_directory_file(&file)).as_ref()
                == Some(&source_dir_identity) =>
        {
            file
        }
        Ok(file) => {
            drop(file);
            remove_dir_if_unchanged(
                &prepared.root_dir,
                &temporary_source_name,
                &source_dir_identity,
            );
            rollback(prepared, None, &[], None, None);
            return Err(NewPackageError::DestinationNotEmpty(
                destination_for_io.clone(),
            ));
        }
        Err(source) => {
            remove_dir_if_unchanged(
                &prepared.root_dir,
                &temporary_source_name,
                &source_dir_identity,
            );
            rollback(prepared, None, &[], None, None);
            return Err(NewPackageError::Io {
                path: temporary_source_path,
                source,
            });
        }
    };
    if let Err(source) = publish_source_directory(
        &prepared.root_dir,
        &prepared.root_file,
        &temporary_source_name,
        &publication_file,
        OsStr::new("src"),
    ) {
        drop(publication_file);
        remove_dir_if_unchanged(
            &prepared.root_dir,
            &temporary_source_name,
            &source_dir_identity,
        );
        let destination_claimed = source.kind() == io::ErrorKind::AlreadyExists
            || source.kind() == io::ErrorKind::PermissionDenied
                && prepared.root_dir.symlink_metadata("src").is_ok();
        if destination_claimed {
            rollback(prepared, None, &[], None, None);
            return Err(NewPackageError::DestinationNotEmpty(
                destination_for_io.clone(),
            ));
        }
        rollback(prepared, None, &[], None, None);
        return Err(NewPackageError::Io {
            path: source_directory,
            source,
        });
    }
    drop(publication_file);
    // The handle used to publish the directory permits rename on Windows.
    // Reopen the published directory with the ordinary no-follow handle before
    // writing through it; that handle denies delete sharing on Windows, so
    // the directory cannot be moved while package files are being installed.
    let source_dir_file =
        match open_directory_nofollow(&prepared.root_file, OsStr::new("src"), &source_directory) {
            Ok(file) if file_identity(&file).as_ref() == Some(&source_dir_identity) => file,
            Ok(file) => {
                drop(file);
                rollback(prepared, None, &[], None, Some(source_dir_identity));
                return Err(NewPackageError::DestinationNotEmpty(
                    destination_for_io.clone(),
                ));
            }
            Err(source) => {
                rollback(prepared, None, &[], None, Some(source_dir_identity));
                return Err(NewPackageError::Io {
                    path: source_directory,
                    source,
                });
            }
        };
    let source_dir = Dir::from_std_file(source_dir_file);

    let mut created_root_files = Vec::with_capacity(2);
    let mut created_source_file = None;
    let mut receipt_files = vec![None; files.len()];
    let mut write_order = files.into_iter().enumerate().collect::<Vec<_>>();
    // Publish the manifest last so package discovery never sees a root whose
    // declared source files have not all been installed yet.
    write_order.sort_by_key(|(index, _)| *index == 0);
    for (index, (relative_path, contents)) in write_order {
        if is_cancelled() {
            rollback(
                prepared,
                Some(source_dir),
                &created_root_files,
                created_source_file,
                Some(source_dir_identity),
            );
            return Err(NewPackageError::Interrupted);
        }
        if relative_path == Path::new("sai.package")
            && (!destination_path_matches(&prepared)
                || !source_directory_path_matches(
                    &prepared.root_file,
                    &source_directory,
                    &source_dir_identity,
                ))
        {
            rollback(
                prepared,
                Some(source_dir),
                &created_root_files,
                created_source_file,
                Some(source_dir_identity),
            );
            return Err(NewPackageError::DestinationNotEmpty(
                destination_for_io.clone(),
            ));
        }
        let path = destination_for_io.join(&relative_path);
        let file_name = relative_path
            .file_name()
            .expect("generated package file has a name");
        let in_source_directory = relative_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty());
        let write_result = if in_source_directory {
            write_new_file_atomic(&source_dir, &source_directory, file_name, &contents)
        } else {
            write_new_file_atomic(
                &prepared.root_dir,
                &destination_for_io,
                file_name,
                &contents,
            )
        };
        let published_file = match write_result {
            Ok(published_file) => published_file,
            Err(source) => {
                rollback(
                    prepared,
                    Some(source_dir),
                    &created_root_files,
                    created_source_file,
                    Some(source_dir_identity),
                );
                return Err(NewPackageError::Io { path, source });
            }
        };
        if in_source_directory {
            created_source_file = Some(published_file);
        } else {
            created_root_files.push((file_name.to_os_string(), published_file));
        }

        let digest = Sha256::digest(&contents);
        receipt_files[index] = Some(NewPackageFile {
            path: format_cli_path(&relative_path)
                .expect("generated package-relative paths are representable"),
            bytes: contents.len() as u64,
            sha256: digest_to_hex(&digest),
        });
    }
    if is_cancelled() {
        rollback(
            prepared,
            Some(source_dir),
            &created_root_files,
            created_source_file,
            Some(source_dir_identity),
        );
        return Err(NewPackageError::Interrupted);
    }
    if !destination_path_matches(&prepared)
        || !source_directory_path_matches(
            &prepared.root_file,
            &source_directory,
            &source_dir_identity,
        )
    {
        rollback(
            prepared,
            Some(source_dir),
            &created_root_files,
            created_source_file,
            Some(source_dir_identity),
        );
        return Err(NewPackageError::DestinationNotEmpty(
            destination_for_io.clone(),
        ));
    }
    drop(source_dir);

    Ok(NewPackageReceipt {
        package: format!("{identity}@{PACKAGE_VERSION}"),
        directory: formatted_destination,
        files: receipt_files
            .into_iter()
            .map(|file| file.expect("every generated file was written"))
            .collect(),
    })
}

fn normalized_destination(destination: &Path) -> Result<PathBuf, NewPackageError> {
    let all_current_directory = destination
        .components()
        .all(|component| matches!(component, Component::CurDir));
    if all_current_directory {
        return std::env::current_dir().map_err(|source| NewPackageError::Io {
            path: destination.to_path_buf(),
            source,
        });
    }
    if destination.is_absolute() {
        return Ok(strip_trailing_separators(destination));
    }
    let current_directory = std::env::current_dir().map_err(|source| NewPackageError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    Ok(strip_trailing_separators(
        &current_directory.join(destination),
    ))
}

fn write_new_file_atomic(
    directory: &Dir,
    directory_path: &Path,
    file_name: &std::ffi::OsStr,
    contents: &[u8],
) -> io::Result<PublishedFile> {
    write_new_file_atomic_with_hook(directory, directory_path, file_name, contents, |_, _, _| {
        Ok(())
    })
}

fn write_new_file_atomic_with_hook(
    directory: &Dir,
    directory_path: &Path,
    file_name: &std::ffi::OsStr,
    contents: &[u8],
    before_publish: impl FnOnce(&Dir, &OsStr, &Path) -> io::Result<()>,
) -> io::Result<PublishedFile> {
    let (temporary_name, mut temporary_file) = create_temporary_file(directory)?;
    let identity_file = temporary_file.try_clone()?.into_std();
    let identity = sai_windows_fs::file_identity(&identity_file)?;
    let cleanup_identity = identity;
    let _cleanup = TemporaryFileCleanup {
        directory,
        name: temporary_name.clone(),
        identity: cleanup_identity,
    };
    temporary_file.write_all(contents)?;
    temporary_file.flush()?;
    before_publish(directory, &temporary_name, directory_path)?;
    if path_identity(directory, &temporary_name).as_ref() != Some(&identity) {
        return Err(io::Error::other(
            "temporary file was replaced before publication",
        ));
    }
    publish_new_file(directory, directory_path, &temporary_name, file_name)?;
    if path_identity(directory, file_name).as_ref() != Some(&identity) {
        return Err(io::Error::other(
            "published file identity changed before verification",
        ));
    }
    let pinned_handle = same_file::Handle::from_file(identity_file.try_clone()?)?;
    Ok(PublishedFile {
        identity: PinnedFileIdentity {
            _handle: pinned_handle,
            value: identity,
        },
    })
}

fn create_temporary_directory(
    directory: &Dir,
    directory_file: &File,
    directory_path: &Path,
) -> io::Result<(OsString, File)> {
    create_temporary_directory_with_open(
        directory,
        directory_file,
        directory_path,
        open_directory_nofollow,
    )
}

fn create_temporary_directory_with_open(
    directory: &Dir,
    directory_file: &File,
    directory_path: &Path,
    mut open: impl FnMut(&File, &OsStr, &Path) -> io::Result<File>,
) -> io::Result<(OsString, File)> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let name = temporary_name()?;
        match directory.create_dir(&name) {
            Ok(()) => {
                let path = directory_path.join(&name);
                let Some(identity) = path_identity(directory, &name) else {
                    return Err(io::Error::other(
                        "could not identify the new temporary directory",
                    ));
                };
                let source_directory = match open(directory_file, &name, &path) {
                    Ok(file) => file,
                    Err(error) => {
                        remove_dir_if_unchanged(directory, &name, &identity);
                        return Err(error);
                    }
                };
                if file_identity(&source_directory).as_ref() != Some(&identity) {
                    drop(source_directory);
                    remove_dir_if_unchanged(directory, &name, &identity);
                    return Err(io::Error::other(
                        "temporary directory changed before it was opened",
                    ));
                }
                if let Err(error) = ensure_empty_directory(&source_directory) {
                    drop(source_directory);
                    remove_dir_if_unchanged(directory, &name, &identity);
                    return Err(error);
                }
                return Ok((name, source_directory));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "random temporary directory name collided repeatedly",
    ))
}

fn ensure_empty_directory(directory_file: &File) -> io::Result<()> {
    let directory = Dir::from_std_file(directory_file.try_clone()?);
    let mut entries = directory.entries()?;
    match entries.next() {
        Some(Ok(_)) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary directory is not empty",
        )),
        Some(Err(error)) => Err(error),
        None => Ok(()),
    }
}

fn directory_entry_identity(parent: &File, component: &OsStr, path: &Path) -> Option<FileIdentity> {
    let directory = open_directory_nofollow(parent, component, path).ok()?;
    file_identity(&directory)
}

fn destination_path_matches(prepared: &PreparedDestination) -> bool {
    prepared.root_identity.as_ref().is_some_and(|identity| {
        path_identity(&prepared.parent_dir, &prepared.leaf).as_ref() == Some(identity)
    })
}

fn source_directory_path_matches(
    root_file: &File,
    source_directory: &Path,
    expected_identity: &FileIdentity,
) -> bool {
    directory_entry_identity(root_file, OsStr::new("src"), source_directory).as_ref()
        == Some(expected_identity)
}

fn open_directory_for_publication(
    parent: &File,
    component: &OsStr,
    path: &Path,
) -> io::Result<PublicationDirectory> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let _ = path;
        openat(
            parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        let _ = path;
        sai_windows_fs::open_child_directory_for_rename_without_following(parent, component)
    }
    #[cfg(not(any(unix, windows)))]
    {
        open_directory_nofollow(parent, component, path)
    }
}

#[cfg(windows)]
type PublicationDirectory = sai_windows_fs::RenameableDirectory;

#[cfg(not(windows))]
type PublicationDirectory = File;

#[cfg(windows)]
fn publication_directory_file(directory: &PublicationDirectory) -> &File {
    directory.as_file()
}

#[cfg(not(windows))]
fn publication_directory_file(directory: &PublicationDirectory) -> &File {
    directory
}

fn publish_source_directory(
    directory: &Dir,
    parent_file: &File,
    temporary_name: &OsStr,
    source_directory: &PublicationDirectory,
    destination_name: &OsStr,
) -> io::Result<()> {
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox"
    ))]
    {
        use rustix::fs::{RenameFlags, renameat_with};

        let _ = (parent_file, source_directory, destination_name);
        renameat_with(
            directory,
            temporary_name,
            directory,
            "src",
            RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        let _ = (directory, temporary_name);
        sai_windows_fs::move_directory_handle_to_child_without_replacing(
            source_directory,
            parent_file,
            destination_name,
        )
    }
    #[cfg(not(any(
        windows,
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox"
    )))]
    {
        let _ = (
            directory,
            parent_file,
            temporary_name,
            source_directory,
            destination_name,
        );
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace directory publication is unsupported on this platform",
        ))
    }
}

fn publish_new_file(
    directory: &Dir,
    directory_path: &Path,
    temporary_name: &OsStr,
    file_name: &OsStr,
) -> io::Result<()> {
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox"
    ))]
    {
        use rustix::fs::{RenameFlags, renameat_with};

        let _ = directory_path;
        renameat_with(
            directory,
            temporary_name,
            directory,
            file_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        sai_windows_fs::move_file_without_replacing(
            &directory_path.join(temporary_name),
            &directory_path.join(file_name),
        )
    }
    #[cfg(not(any(
        windows,
        target_os = "android",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "redox"
    )))]
    {
        let _ = directory_path;
        directory.hard_link(temporary_name, directory, file_name)
    }
}

struct TemporaryFileCleanup<'a> {
    directory: &'a Dir,
    name: OsString,
    identity: FileIdentity,
}

impl Drop for TemporaryFileCleanup<'_> {
    fn drop(&mut self) {
        remove_file_if_unchanged(self.directory, &self.name, &self.identity);
    }
}

fn create_temporary_file(directory: &Dir) -> io::Result<(OsString, cap_std::fs::File)> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let name = temporary_name()?;
        let mut options = CapOpenOptions::new();
        options.write(true).create_new(true);
        match directory.open_with(&name, &options) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "random temporary filename collided repeatedly",
    ))
}

fn temporary_name() -> io::Result<OsString> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let mut name = String::with_capacity(TEMPORARY_FILE_PREFIX.len() + random.len() * 2);
    name.push_str(TEMPORARY_FILE_PREFIX);
    for byte in random {
        use fmt::Write as _;
        write!(name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(name.into())
}

fn open_directory_nofollow(parent: &File, component: &OsStr, path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};

        let _ = path;
        openat(
            parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        let _ = (parent, component);
        sai_windows_fs::open_directory_without_following(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, component);
        Dir::open_ambient_dir(path, ambient_authority()).map(Dir::into_std_file)
    }
}

struct PreparedDestination {
    parent_dir: Dir,
    root_file: File,
    root_dir: Dir,
    leaf: OsString,
    root_identity: Option<FileIdentity>,
}

fn prepare_destination(destination: &Path) -> Result<PreparedDestination, NewPackageError> {
    prepare_destination_with_opener(destination, open_directory_nofollow)
}

fn prepare_destination_with_opener(
    destination: &Path,
    open_directory: impl FnOnce(&File, &OsStr, &Path) -> io::Result<File>,
) -> Result<PreparedDestination, NewPackageError> {
    let Some(leaf) = destination.file_name().map(OsString::from) else {
        return Err(NewPackageError::DestinationNotEmpty(
            destination.to_path_buf(),
        ));
    };
    let parent_path = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_dir = Dir::open_ambient_dir(parent_path, ambient_authority()).map_err(|source| {
        NewPackageError::Io {
            path: parent_path.to_path_buf(),
            source,
        }
    })?;
    let parent_file = parent_dir.into_std_file();
    let parent_dir =
        Dir::from_std_file(
            parent_file
                .try_clone()
                .map_err(|source| NewPackageError::Io {
                    path: parent_path.to_path_buf(),
                    source,
                })?,
        );

    match parent_dir.symlink_metadata(&leaf) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(NewPackageError::DestinationNotEmpty(
                destination.to_path_buf(),
            ));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            parent_dir.create_dir(&leaf).map_err(|source| {
                if source.kind() == io::ErrorKind::AlreadyExists {
                    NewPackageError::DestinationNotEmpty(destination.to_path_buf())
                } else {
                    NewPackageError::Io {
                        path: destination.to_path_buf(),
                        source,
                    }
                }
            })?;
        }
        Err(source) => {
            return Err(NewPackageError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };

    let root_file = match open_directory(&parent_file, &leaf, destination) {
        Ok(directory) => directory,
        Err(source) => {
            // No directory handle was obtained, so the current entry may
            // belong to a competing creator. Preserve it rather than remove
            // an entry whose identity is unknown.
            return Err(NewPackageError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };
    let root_identity = file_identity(&root_file);
    let root_dir = Dir::from_std_file(match root_file.try_clone() {
        Ok(file) => file,
        Err(source) => {
            drop(root_file);
            return Err(NewPackageError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    });
    let mut entries = match root_dir.entries() {
        Ok(entries) => entries,
        Err(source) => {
            drop(root_dir);
            drop(root_file);
            return Err(NewPackageError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };
    match entries.next() {
        Some(Ok(_)) => {
            return Err(NewPackageError::DestinationNotEmpty(
                destination.to_path_buf(),
            ));
        }
        Some(Err(source)) => {
            drop(entries);
            drop(root_dir);
            drop(root_file);
            return Err(NewPackageError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
        None => {}
    }

    Ok(PreparedDestination {
        parent_dir,
        root_file,
        root_dir,
        leaf,
        root_identity,
    })
}

pub fn validate_new_package_identity(identity: &str) -> Result<(), NewPackageError> {
    match PackageIdentity::parse(identity) {
        Ok(_) => Ok(()),
        Err(PackageIdentityError::InvalidPackageName) => {
            Err(NewPackageError::InvalidPackageIdentity(identity.to_owned()))
        }
        Err(PackageIdentityError::ReservedNamespace) => Err(NewPackageError::ReservedNamespace),
        Err(PackageIdentityError::ReservedPackageName) => Err(NewPackageError::ReservedPackageName),
    }
}

fn validate_targets(targets: &[String]) -> Result<Vec<String>, NewPackageError> {
    if targets.is_empty() {
        return Ok(vec![DEFAULT_TARGET.to_owned()]);
    }

    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(targets.len());
    for target in targets {
        if !KNOWN_TARGETS.contains(&target.as_str()) {
            return Err(NewPackageError::UnknownTarget(target.clone()));
        }
        if !seen.insert(target.as_str()) {
            return Err(NewPackageError::DuplicateTarget(target.clone()));
        }
        validated.push(target.clone());
    }
    Ok(validated)
}

fn render_package_files(identity: &str, targets: &[String]) -> Vec<(PathBuf, Vec<u8>)> {
    let targets = targets.join(", ");
    let target_line = format!("targets {{ {targets} }}");
    let target_declaration = if target_line.len() <= 100 {
        target_line
    } else {
        let entries = targets
            .split(", ")
            .map(|target| format!("    {target}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("targets {{\n{entries}\n}}")
    };
    let language_version = env!("CARGO_PKG_VERSION")
        .split_once('.')
        .map(|(major, remainder)| {
            let minor = remainder
                .split_once('.')
                .map_or(remainder, |(minor, _)| minor);
            format!("{major}.{minor}")
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let manifest = format!(
        "package {identity} @ {PACKAGE_VERSION}\ncompiler ^{language_version}\nsource \"src\"\n\n{target_declaration}\n"
    );

    vec![
        (PathBuf::from("sai.package"), manifest.into_bytes()),
        (PathBuf::from(".gitignore"), GITIGNORE.to_vec()),
        (PathBuf::from("src/lib.sai"), ROOT_MODULE.to_vec()),
    ]
}

fn format_cli_path(path: &Path) -> Option<String> {
    let mut path = path.to_str()?.to_owned();
    #[cfg(windows)]
    {
        path = path.replace('\\', "/");
    }
    if !Path::new(&path)
        .components()
        .all(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        path = path.trim_end_matches('/').to_owned();
    }
    Some(path)
}

fn strip_trailing_separators(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => parent.join(file_name),
        _ => path.to_path_buf(),
    }
}

fn digest_to_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = Vec::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize]);
        encoded.push(HEX[(byte & 0x0f) as usize]);
    }
    String::from_utf8(encoded).expect("hexadecimal encoding is always valid UTF-8")
}

fn rollback(
    prepared: PreparedDestination,
    source_dir: Option<Dir>,
    created_root_files: &[(OsString, PublishedFile)],
    created_source_file: Option<PublishedFile>,
    source_dir_identity: Option<FileIdentity>,
) {
    if let Some(PublishedFile { identity }) = created_source_file
        && let Some(source_dir) = &source_dir
    {
        remove_file_if_unchanged(source_dir, "lib.sai", &identity.value);
    }
    for (file_name, published) in created_root_files.iter().rev() {
        remove_file_if_unchanged(&prepared.root_dir, file_name, &published.identity.value);
    }
    drop(source_dir);
    if let Some(identity) = source_dir_identity {
        remove_dir_if_unchanged(&prepared.root_dir, "src", &identity);
    }
}

#[derive(Debug)]
struct PublishedFile {
    identity: PinnedFileIdentity,
}

#[derive(Debug)]
struct PinnedFileIdentity {
    _handle: same_file::Handle,
    value: FileIdentity,
}

type FileIdentity = sai_windows_fs::FileIdentity;

fn file_identity(file: &File) -> Option<FileIdentity> {
    sai_windows_fs::file_identity(file).ok()
}

fn path_identity(directory: &Dir, name: &std::ffi::OsStr) -> Option<FileIdentity> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, openat};

        openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()
        .map(File::from)?
    };
    #[cfg(not(unix))]
    let file = {
        let metadata = directory.symlink_metadata(name).ok()?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return None;
        }
        if file_type.is_dir() {
            directory.open_dir(name).ok()?.into_std_file()
        } else if file_type.is_file() {
            directory.open(name).ok()?.into_std()
        } else {
            return None;
        }
    };
    file_identity(&file)
}

fn remove_file_if_unchanged(
    directory: &Dir,
    name: impl AsRef<std::ffi::OsStr>,
    identity: &FileIdentity,
) {
    let name = name.as_ref();
    if path_identity(directory, name).as_ref() == Some(identity) {
        let _ = directory.remove_file(name);
    }
}

fn remove_dir_if_unchanged(
    directory: &Dir,
    name: impl AsRef<std::ffi::OsStr>,
    identity: &FileIdentity,
) {
    let name = name.as_ref();
    if path_identity(directory, name).as_ref() == Some(identity) {
        let _ = directory.remove_dir(name);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        NewPackageError, create_new_package, create_new_package_with_cancel_check, format_cli_path,
        normalized_destination, write_new_file_atomic,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "sai-package-new-{}-{nonce}-{}",
                std::process::id(),
                NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_the_documented_library_package_and_receipt() {
        use sha2::{Digest, Sha256};

        let parent = TempDirectory::new();
        let destination = parent.path().join("image");
        let receipt = create_new_package(&destination, "acme:image", &[]).unwrap();

        assert_eq!(receipt.package, "acme:image@0.1.0");
        assert_eq!(receipt.directory, format_cli_path(&destination).unwrap());
        assert_eq!(
            receipt
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["sai.package", ".gitignore", "src/lib.sai"]
        );
        assert_eq!(
            receipt
                .files
                .iter()
                .map(|file| file.bytes)
                .collect::<Vec<_>>(),
            vec![79, 13, 107]
        );
        assert!(receipt.files.iter().all(|file| file.sha256.len() == 64));
        for file in &receipt.files {
            let contents = fs::read(destination.join(&file.path)).unwrap();
            let digest = Sha256::digest(contents);
            let expected = digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(file.sha256, expected);
        }
        assert_eq!(
            fs::read_to_string(destination.join("sai.package")).unwrap(),
            concat!(
                "package acme:image @ 0.1.0\n",
                "compiler ^0.1\n",
                "source \"src\"\n",
                "\n",
                "targets { wasm32-wasi }\n",
            )
        );
        assert_eq!(
            fs::read(destination.join(".gitignore")).unwrap(),
            b"/out/\n/.sai/\n"
        );
        assert_eq!(
            fs::read(destination.join("src/lib.sai")).unwrap(),
            b"pub fn add(a: I32, b: I32) -> I32 {\n    a + b\n}\n\ntest \"add adds two numbers\" {\n    assert add(2, 3) == 5\n}\n"
        );
        assert!(!destination.join("sai.lock").exists());
    }

    #[test]
    fn preserves_the_declared_target_order() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("multi");
        let targets = vec!["wasm32-core".to_owned(), "wasm64-wasi".to_owned()];

        create_new_package(&destination, "acme:multi", &targets).unwrap();

        let manifest = fs::read_to_string(destination.join("sai.package")).unwrap();
        assert!(manifest.ends_with("targets { wasm32-core, wasm64-wasi }\n"));
    }

    #[test]
    fn accepts_native_target_names_even_when_native_building_is_not_implemented() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("native");
        let targets = vec!["x86_64-linux".to_owned(), "aarch64-macos".to_owned()];

        create_new_package(&destination, "acme:native", &targets).unwrap();

        let manifest = fs::read_to_string(destination.join("sai.package")).unwrap();
        assert!(manifest.ends_with("targets { x86_64-linux, aarch64-macos }\n"));
    }

    #[test]
    fn fills_an_existing_empty_destination_directory() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("empty");
        fs::create_dir(&destination).unwrap();

        create_new_package(&destination, "acme:empty", &[]).unwrap();

        assert!(destination.join("sai.package").is_file());
    }

    #[test]
    fn dot_destination_resolves_to_the_current_directory() {
        assert_eq!(
            normalized_destination(Path::new(".")).unwrap(),
            std::env::current_dir().unwrap()
        );
    }

    #[test]
    fn cancellation_rolls_back_files_and_preserves_the_destination_directory() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("interrupted");
        let checks = Cell::new(0);

        let result =
            create_new_package_with_cancel_check(&destination, "acme:interrupted", &[], || {
                let next = checks.get() + 1;
                checks.set(next);
                next == 4
            });

        assert!(matches!(result, Err(NewPackageError::Interrupted)));
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    fn loses_the_source_directory_claim_without_removing_the_winner() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("claim-race");
        let checks = Cell::new(0);

        let result =
            create_new_package_with_cancel_check(&destination, "acme:claim-race", &[], || {
                let next = checks.get() + 1;
                checks.set(next);
                if next == 2 {
                    fs::create_dir(destination.join("src")).unwrap();
                }
                false
            });

        assert!(matches!(
            result,
            Err(NewPackageError::DestinationNotEmpty(_))
        ));
        assert!(destination.join("src").is_dir());
        assert!(!destination.join("sai.package").exists());
    }

    #[test]
    fn failed_root_open_preserves_an_unidentified_replacement() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("open-failure");

        let result = super::prepare_destination_with_opener(&destination, |_, _, path| {
            fs::remove_dir(path).unwrap();
            fs::create_dir(path).unwrap();
            fs::write(path.join("competitor"), b"preserve").unwrap();
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated open failure after a competing replacement",
            ))
        });

        assert!(matches!(result, Err(NewPackageError::Io { .. })));
        assert_eq!(
            fs::read(destination.join("competitor")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn source_directory_publication_does_not_adopt_a_competing_entry() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("open-failure");
        let prepared = super::prepare_destination(&destination).unwrap();
        let (temporary_name, source_file) = super::create_temporary_directory(
            &prepared.root_dir,
            &prepared.root_file,
            &destination,
        )
        .unwrap();
        let source_dir_identity = super::file_identity(&source_file).unwrap();
        drop(source_file);
        let temporary_path = destination.join(&temporary_name);
        let publication_file = super::open_directory_for_publication(
            &prepared.root_file,
            &temporary_name,
            &temporary_path,
        )
        .unwrap();
        let source_directory = destination.join("src");
        fs::create_dir(&source_directory).unwrap();
        fs::write(source_directory.join("competitor"), b"preserve").unwrap();

        let error = super::publish_source_directory(
            &prepared.root_dir,
            &prepared.root_file,
            &temporary_name,
            &publication_file,
            std::ffi::OsStr::new("src"),
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        super::remove_dir_if_unchanged(&prepared.root_dir, &temporary_name, &source_dir_identity);
        assert!(source_directory.is_dir());
        assert_eq!(
            fs::read(source_directory.join("competitor")).unwrap(),
            b"preserve"
        );
        assert!(!destination.join("sai.package").exists());
    }

    #[cfg(unix)]
    #[test]
    fn destination_identity_guard_detects_a_replaced_root_directory() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        let moved_destination = parent.path().join("moved-root");
        let prepared = super::prepare_destination(&destination).unwrap();

        fs::rename(&destination, &moved_destination).unwrap();
        fs::create_dir(&destination).unwrap();

        assert!(!super::destination_path_matches(&prepared));

        drop(prepared);
        fs::remove_dir(&destination).unwrap();
        fs::remove_dir_all(moved_destination).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rollback_preserves_a_root_directory_replaced_after_open() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        let moved_destination = parent.path().join("moved-root");
        let prepared = super::prepare_destination(&destination).unwrap();

        fs::rename(&destination, &moved_destination).unwrap();
        fs::create_dir(&destination).unwrap();
        super::rollback(prepared, None, &[], None, None);

        assert!(destination.is_dir());
        assert!(moved_destination.is_dir());
        fs::remove_dir(destination).unwrap();
        fs::remove_dir_all(moved_destination).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_directory_identity_guard_detects_a_replaced_source_directory() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        let prepared = super::prepare_destination(&destination).unwrap();
        let source_directory = destination.join("src");
        let moved_source_directory = destination.join("moved-src");
        fs::create_dir(&source_directory).unwrap();
        let source_file = super::open_directory_nofollow(
            &prepared.root_file,
            std::ffi::OsStr::new("src"),
            &source_directory,
        )
        .unwrap();
        let source_identity = super::file_identity(&source_file).unwrap();

        assert!(super::source_directory_path_matches(
            &prepared.root_file,
            &source_directory,
            &source_identity
        ));
        fs::rename(&source_directory, &moved_source_directory).unwrap();
        fs::create_dir(&source_directory).unwrap();
        assert!(!super::source_directory_path_matches(
            &prepared.root_file,
            &source_directory,
            &source_identity
        ));

        drop(source_file);
        drop(prepared);
        fs::remove_dir(&source_directory).unwrap();
        fs::remove_dir_all(moved_source_directory).unwrap();
        fs::remove_dir(destination).unwrap();
    }

    #[test]
    fn temporary_source_directory_must_be_empty_before_publication() {
        use std::ffi::OsStr;

        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        fs::create_dir(&destination).unwrap();
        let root_dir =
            cap_std::fs::Dir::open_ambient_dir(&destination, cap_std::ambient_authority()).unwrap();
        root_dir.create_dir(".sai-new-staging").unwrap();
        fs::write(
            destination.join(".sai-new-staging").join("competitor"),
            b"preserve",
        )
        .unwrap();
        let root_file = root_dir.into_std_file();
        let staging_file = super::open_directory_nofollow(
            &root_file,
            OsStr::new(".sai-new-staging"),
            &destination.join(".sai-new-staging"),
        )
        .unwrap();

        let error = super::ensure_empty_directory(&staging_file).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(destination.join(".sai-new-staging").join("competitor")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn temporary_source_directory_replacement_is_not_adopted_or_removed() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        let prepared = super::prepare_destination(&destination).unwrap();

        let result = super::create_temporary_directory_with_open(
            &prepared.root_dir,
            &prepared.root_file,
            &destination,
            |parent_file, name, path| {
                fs::remove_dir(path)?;
                fs::create_dir(path)?;
                fs::write(path.join("competitor"), b"preserve")?;
                super::open_directory_nofollow(parent_file, name, path)
            },
        );

        assert!(result.is_err());
        let entries = fs::read_dir(&destination)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            fs::read(entries[0].path().join("competitor")).unwrap(),
            b"preserve"
        );
        assert!(!destination.join("src").exists());
    }

    #[test]
    fn atomic_file_publication_never_replaces_an_existing_entry() {
        use std::ffi::OsStr;

        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sai.package"), b"preserve").unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&destination, cap_std::ambient_authority()).unwrap();

        let error = write_new_file_atomic(
            &directory,
            &destination,
            OsStr::new("sai.package"),
            b"replacement",
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(destination.join("sai.package")).unwrap(),
            b"preserve"
        );
        assert_eq!(
            fs::read_dir(&destination).unwrap().count(),
            1,
            "the temporary file is cleaned after a conflicting entry"
        );
    }

    #[test]
    fn temporary_file_replacement_is_preserved_and_never_published() {
        use std::cell::RefCell;
        use std::ffi::OsStr;

        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        fs::create_dir(&destination).unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&destination, cap_std::ambient_authority()).unwrap();
        let replaced_path = RefCell::new(None);

        let result = super::write_new_file_atomic_with_hook(
            &directory,
            &destination,
            OsStr::new("sai.package"),
            b"intended",
            |_, temporary_name, directory_path| {
                let path = directory_path.join(temporary_name);
                fs::remove_file(&path)?;
                fs::write(&path, b"competitor")?;
                *replaced_path.borrow_mut() = Some(path);
                Ok(())
            },
        );

        assert!(result.is_err());
        assert!(!destination.join("sai.package").exists());
        let replaced_path = replaced_path.into_inner().unwrap();
        assert_eq!(fs::read(replaced_path).unwrap(), b"competitor");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_file_symlink_to_original_is_not_published() {
        use std::ffi::OsStr;
        use std::os::unix::fs::symlink;

        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        fs::create_dir(&destination).unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&destination, cap_std::ambient_authority()).unwrap();
        let retained_path = std::cell::RefCell::new(None);

        let result = super::write_new_file_atomic_with_hook(
            &directory,
            &destination,
            OsStr::new("sai.package"),
            b"intended",
            |_, temporary_name, directory_path| {
                let path = directory_path.join(temporary_name);
                let retained = path.with_extension("retained");
                fs::rename(&path, &retained)?;
                symlink(&retained, &path)?;
                *retained_path.borrow_mut() = Some((path, retained));
                Ok(())
            },
        );

        assert!(result.is_err());
        assert!(!destination.join("sai.package").exists());
        let (temporary_path, retained_path) = retained_path.into_inner().unwrap();
        assert_eq!(fs::read_link(&temporary_path).unwrap(), retained_path);
        assert_eq!(fs::read(retained_path).unwrap(), b"intended");
    }

    #[test]
    fn rollback_preserves_a_file_replaced_after_publication() {
        use std::fs::File;

        let parent = TempDirectory::new();
        let destination = parent.path().join("root");
        fs::create_dir(&destination).unwrap();
        let original_path = destination.join("sai.package");
        let replacement_path = destination.join("replacement");
        fs::write(&original_path, b"ours").unwrap();
        fs::write(&replacement_path, b"theirs").unwrap();
        let file = File::open(&original_path).unwrap();
        let identity = super::PinnedFileIdentity {
            _handle: same_file::Handle::from_file(file.try_clone().unwrap()).unwrap(),
            value: super::file_identity(&file).unwrap(),
        };
        fs::rename(&replacement_path, &original_path).unwrap();

        let parent_dir =
            cap_std::fs::Dir::open_ambient_dir(parent.path(), cap_std::ambient_authority())
                .unwrap();
        let root_file =
            cap_std::fs::Dir::open_ambient_dir(&destination, cap_std::ambient_authority())
                .unwrap()
                .into_std_file();
        let root_dir = cap_std::fs::Dir::from_std_file(root_file.try_clone().unwrap());
        let prepared = super::PreparedDestination {
            parent_dir,
            root_file,
            root_dir,
            leaf: OsString::from("root"),
            root_identity: None,
        };
        super::rollback(
            prepared,
            None,
            &[(
                OsString::from("sai.package"),
                super::PublishedFile { identity },
            )],
            None,
            None,
        );

        assert_eq!(fs::read(original_path).unwrap(), b"theirs");
    }

    #[test]
    fn does_not_create_missing_parent_directories() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("missing").join("package");

        assert!(matches!(
            create_new_package(&destination, "acme:package", &[]),
            Err(NewPackageError::Io { .. })
        ));
        assert!(!destination.parent().unwrap().exists());
    }

    #[test]
    fn refuses_nonempty_destinations_without_changing_them() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("occupied");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep.txt"), b"keep").unwrap();

        let result = create_new_package(&destination, "acme:occupied", &[]);

        assert!(matches!(
            result,
            Err(NewPackageError::DestinationNotEmpty(_))
        ));
        assert_eq!(fs::read(destination.join("keep.txt")).unwrap(), b"keep");
        assert!(!destination.join("sai.package").exists());
    }

    #[test]
    fn refuses_invalid_and_reserved_package_identities() {
        let parent = TempDirectory::new();
        let cases = [
            ("acme", "invalid"),
            ("Acme:image", "invalid"),
            ("acme:two:parts", "invalid"),
            ("acme:bad_name", "invalid"),
            ("acme:trailing-", "invalid"),
            ("wasi:demo", "namespace"),
            ("sai:core", "package"),
            ("sai:alloc", "package"),
        ];

        for (identity, expected) in cases {
            let destination = parent
                .path()
                .join(format!("case-{}", identity.replace(':', "-")));
            let error = create_new_package(&destination, identity, &[]).unwrap_err();
            match expected {
                "invalid" => assert!(matches!(error, NewPackageError::InvalidPackageIdentity(_))),
                "namespace" => assert!(matches!(error, NewPackageError::ReservedNamespace)),
                "package" => assert!(matches!(error, NewPackageError::ReservedPackageName)),
                _ => unreachable!(),
            }
            assert!(!destination.exists());
        }
    }

    #[test]
    fn enforces_package_segment_length_and_rejects_path_traversal() {
        let parent = TempDirectory::new();
        let valid = format!("a:{}", "n".repeat(64));
        let destination = parent.path().join("valid");

        create_new_package(&destination, &valid, &[]).unwrap();

        for (index, identity) in [format!("a:{}", "n".repeat(65)), "acme:../escape".to_owned()]
            .into_iter()
            .enumerate()
        {
            let destination = parent.path().join(format!("invalid-{index}"));
            assert!(matches!(
                create_new_package(&destination, &identity, &[]),
                Err(NewPackageError::InvalidPackageIdentity(_))
            ));
            assert!(!destination.exists());
        }
    }

    #[test]
    fn rejects_unknown_and_duplicate_targets_before_writing() {
        let parent = TempDirectory::new();
        let unknown_destination = parent.path().join("unknown");
        let unknown = vec!["native-linux".to_owned()];
        assert!(matches!(
            create_new_package(&unknown_destination, "acme:unknown", &unknown),
            Err(NewPackageError::UnknownTarget(_))
        ));
        assert!(!unknown_destination.exists());

        let duplicate_destination = parent.path().join("duplicate");
        let duplicate = vec!["wasm32-core".to_owned(), "wasm32-core".to_owned()];
        assert!(matches!(
            create_new_package(&duplicate_destination, "acme:duplicate", &duplicate),
            Err(NewPackageError::DuplicateTarget(_))
        ));
        assert!(!duplicate_destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_destination_without_following_it() {
        use std::os::unix::fs::symlink;

        let parent = TempDirectory::new();
        let target = parent.path().join("target");
        let destination = parent.path().join("link");
        fs::create_dir(&target).unwrap();
        symlink(&target, &destination).unwrap();

        let result = create_new_package(&destination, "acme:link", &[]);

        assert!(matches!(
            result,
            Err(NewPackageError::DestinationNotEmpty(_))
        ));
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_destination_with_a_trailing_separator() {
        use std::os::unix::fs::symlink;

        let parent = TempDirectory::new();
        let target = parent.path().join("target");
        let destination = parent.path().join("link");
        fs::create_dir(&target).unwrap();
        symlink(&target, &destination).unwrap();
        let destination_with_separator = PathBuf::from(format!("{}/", destination.display()));

        let result = create_new_package(&destination_with_separator, "acme:link", &[]);

        assert!(matches!(
            result,
            Err(NewPackageError::DestinationNotEmpty(_))
        ));
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_backslashes_in_unix_destination_names() {
        let parent = TempDirectory::new();
        let destination = parent.path().join("back\\slash");

        let receipt = create_new_package(&destination, "acme:backslash", &[]).unwrap();

        assert!(receipt.directory.ends_with("back\\slash"));
        assert_eq!(receipt.files[0].path, "sai.package");
        assert!(destination.join("sai.package").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_destination_paths_before_writing() {
        use std::os::unix::ffi::OsStringExt;

        let parent = TempDirectory::new();
        let destination = parent
            .path()
            .join(OsString::from_vec(vec![b'p', b'k', b'g', 0xff]));

        assert!(matches!(
            create_new_package(&destination, "acme:unicode", &[]),
            Err(NewPackageError::UnrepresentableDestination(_))
        ));
        assert!(!destination.exists());
    }
}
