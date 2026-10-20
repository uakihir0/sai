use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::fs::Dir;
use sai_driver::CancellationToken;
use sai_vfs::{PackageFileEntry, PackageFileKind};

/// The result of expanding the explicit paths passed to `sai fmt`.
#[derive(Debug, Default)]
pub(crate) struct FmtPathSelection {
    /// Unique physical files, sorted by their platform path encoding.
    pub files: Vec<SelectedFmtPath>,
    /// Every explicitly selected manifest and the target resolved for it,
    /// retained before physical-path deduplication.
    pub resolved_manifest_paths: Vec<(PathBuf, PathBuf)>,
    /// Input paths that do not exist, paired with their input position.
    pub missing: Vec<(usize, PathBuf)>,
    /// Filesystem paths that could not be inspected during traversal.
    pub errors: Vec<FmtPathSelectionError>,
    /// Visible package-relative entries recorded during a module-tree walk.
    pub module_tree_entries: Vec<PackageFileEntry>,
    /// Entries inspected during the module-tree walk, including hidden names.
    pub module_tree_inspected_entry_count: u64,
    /// Encoded package-relative path bytes inspected during the module-tree walk.
    pub module_tree_inspected_path_bytes: u64,
    /// Visible `.sai` files encountered during the module-tree walk.
    pub module_tree_source_file_count: u64,
    /// The module-tree walk stopped at a configured traversal limit.
    pub module_tree_limit_exceeded: bool,
    /// The selected module-tree root was a symlink and was not followed.
    pub module_tree_root_symlink: bool,
    /// Traversal stopped because cancellation was requested.
    pub interrupted: bool,
}

#[derive(Debug)]
pub(crate) struct SelectedFmtPath {
    pub path: PathBuf,
    pub is_package_manifest: bool,
    pub parent: Option<Arc<Dir>>,
    pub name: OsString,
    pub identity: sai_windows_fs::FileIdentity,
    pub parent_identity: sai_windows_fs::FileIdentity,
    pub identity_guard: Option<fs::File>,
}

/// A filesystem error encountered while selecting formatter inputs.
#[derive(Debug)]
pub(crate) struct FmtPathSelectionError {
    pub path: PathBuf,
    pub source: io::Error,
}

type SelectedPathIdentity = (
    sai_windows_fs::FileIdentity,
    sai_windows_fs::FileIdentity,
    OsString,
);

struct FmtSelectedPathState<'a> {
    selection: &'a mut FmtPathSelection,
    selected_files: &'a mut HashSet<PathBuf>,
    selected_identities: &'a mut HashSet<SelectedPathIdentity>,
    retain_capabilities: bool,
}

enum PendingDirectory {
    Opened(Arc<Dir>, PathBuf),
    Child {
        parent: Arc<Dir>,
        name: OsString,
        path: PathBuf,
    },
}

struct DirectoryWalker<'a> {
    selection: &'a mut FmtPathSelection,
    selected_files: &'a mut HashSet<PathBuf>,
    selected_identities: &'a mut HashSet<SelectedPathIdentity>,
    visited_directories: &'a mut HashSet<PathBuf>,
    cancellation: &'a CancellationToken,
    source_root: PathBuf,
    retain_capabilities: bool,
    update_directory_files: bool,
    traversal_mode: FmtTraversalMode,
    module_tree_limits: Option<sai_package::local_source::LocalPackageSourceSnapshotLimits>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FmtTraversalMode {
    #[default]
    ExplicitPaths,
    WorkspaceSource,
    ModuleTree,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DirectoryFileAccess {
    #[default]
    ReadOnly,
    Update,
}

/// Expands explicit file and directory arguments using the `sai fmt` rules.
///
/// Directory traversal does not follow symbolic links, skips hidden
/// directories, and skips a sibling `out` directory when the parent contains
/// a regular `sai.package` file. Explicit symbolic links to regular files are
/// accepted and resolved to their physical target before deduplication.
pub(crate) fn select_fmt_paths(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        true,
        DirectoryFileAccess::Update,
        FmtTraversalMode::ExplicitPaths,
        None,
    )
}

/// Expands paths for formatter checks that inspect source without replacing it.
///
/// File and parent handles remain retained so the selected input can be read
/// consistently, while directory capabilities request no write access.
pub(crate) fn select_fmt_paths_for_check(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        true,
        DirectoryFileAccess::ReadOnly,
        FmtTraversalMode::ExplicitPaths,
        None,
    )
}

pub(crate) fn select_fmt_paths_for_comparison(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        false,
        DirectoryFileAccess::ReadOnly,
        FmtTraversalMode::ExplicitPaths,
        None,
    )
}

/// Selects workspace manifests and source files below their declared source
/// directories. Unlike explicit directory selection, this includes hidden
/// directories and nested `out` directories because the CLI contract says
/// every `.sai` file below the declared source directory is selected.
pub(crate) fn select_fmt_workspace_paths(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        true,
        DirectoryFileAccess::Update,
        FmtTraversalMode::WorkspaceSource,
        None,
    )
}

pub(crate) fn select_fmt_workspace_paths_for_check(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        true,
        DirectoryFileAccess::ReadOnly,
        FmtTraversalMode::WorkspaceSource,
        None,
    )
}

pub(crate) fn select_fmt_workspace_paths_for_comparison(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        false,
        DirectoryFileAccess::ReadOnly,
        FmtTraversalMode::WorkspaceSource,
        None,
    )
}

/// Selects visible `.sai` files below a declared source root for module parsing.
///
/// The declared root itself is traversed even when its basename starts with a
/// dot. Dot-prefixed entries below it, including their contents, are skipped.
#[cfg(test)]
pub(crate) fn select_fmt_module_paths(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
) -> FmtPathSelection {
    select_fmt_module_paths_with_limits(
        paths,
        current_directory,
        cancellation,
        sai_package::local_source::LocalPackageSourceSnapshotLimits::default(),
    )
}

pub(crate) fn select_fmt_module_paths_with_limits(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
    limits: sai_package::local_source::LocalPackageSourceSnapshotLimits,
) -> FmtPathSelection {
    select_fmt_paths_inner(
        paths,
        current_directory,
        cancellation,
        true,
        DirectoryFileAccess::ReadOnly,
        FmtTraversalMode::ModuleTree,
        Some(limits),
    )
}

fn select_fmt_paths_inner(
    paths: &[PathBuf],
    current_directory: &Path,
    cancellation: &CancellationToken,
    retain_capabilities: bool,
    directory_file_access: DirectoryFileAccess,
    mode: FmtTraversalMode,
    module_tree_limits: Option<sai_package::local_source::LocalPackageSourceSnapshotLimits>,
) -> FmtPathSelection {
    let mut selection = FmtPathSelection::default();
    let mut selected_files = HashSet::new();
    let mut selected_identities = HashSet::new();
    let mut visited_directories = HashSet::new();
    let mut explicit_parent_directories = HashMap::new();

    let mut ordered_paths = paths
        .iter()
        .enumerate()
        .map(|(input_index, requested_path)| {
            let absolute_path = if requested_path.is_absolute() {
                requested_path.clone()
            } else {
                current_directory.join(requested_path)
            };
            (input_index, requested_path, absolute_path)
        })
        .collect::<Vec<_>>();
    ordered_paths.sort_by(|left, right| {
        left.2
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.2.as_os_str().as_encoded_bytes())
            .then_with(|| left.0.cmp(&right.0))
    });

    for (input_index, requested_path, absolute_path) in ordered_paths {
        if cancellation.is_cancelled() {
            selection.interrupted = true;
            break;
        }
        let metadata = match fs::symlink_metadata(&absolute_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                selection
                    .missing
                    .push((input_index, requested_path.clone()));
                continue;
            }
            Err(source) => {
                selection.errors.push(FmtPathSelectionError {
                    path: absolute_path,
                    source,
                });
                continue;
            }
        };

        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            if mode == FmtTraversalMode::ModuleTree {
                selection.module_tree_root_symlink = true;
                continue;
            }
            match fs::metadata(&absolute_path) {
                Ok(target_metadata) if target_metadata.is_file() => {
                    push_explicit_file(
                        &absolute_path,
                        &mut selection,
                        &mut selected_files,
                        &mut selected_identities,
                        &mut explicit_parent_directories,
                        retain_capabilities,
                        directory_file_access,
                    );
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    selection
                        .missing
                        .push((input_index, requested_path.clone()));
                }
                Err(source) => selection.errors.push(FmtPathSelectionError {
                    path: absolute_path,
                    source,
                }),
            }
        } else if file_type.is_file() {
            push_explicit_file(
                &absolute_path,
                &mut selection,
                &mut selected_files,
                &mut selected_identities,
                &mut explicit_parent_directories,
                retain_capabilities,
                directory_file_access,
            );
        } else if file_type.is_dir() {
            let opened_root = match mode {
                FmtTraversalMode::ModuleTree => {
                    sai_windows_fs::open_directory_path_without_following(&absolute_path)
                        .map(|root| (Dir::from_std_file(root), absolute_path.clone()))
                }
                FmtTraversalMode::ExplicitPaths | FmtTraversalMode::WorkspaceSource => {
                    open_explicit_directory(&absolute_path, directory_file_access)
                }
            };
            let (root_directory, root) = match opened_root {
                Ok(root) => root,
                Err(source) => {
                    selection.errors.push(FmtPathSelectionError {
                        path: absolute_path,
                        source,
                    });
                    continue;
                }
            };
            if mode == FmtTraversalMode::ExplicitPaths && should_skip_root_directory(&root) {
                continue;
            }
            DirectoryWalker {
                selection: &mut selection,
                selected_files: &mut selected_files,
                selected_identities: &mut selected_identities,
                visited_directories: &mut visited_directories,
                cancellation,
                source_root: root.clone(),
                retain_capabilities,
                update_directory_files: directory_file_access == DirectoryFileAccess::Update,
                traversal_mode: mode,
                module_tree_limits,
            }
            .walk(root_directory, root);
            if selection.interrupted || selection.module_tree_limit_exceeded {
                break;
            }
        } else {
            selection.errors.push(FmtPathSelectionError {
                path: absolute_path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "formatter input is not a regular file or directory",
                ),
            });
        }
    }

    if selection.module_tree_limit_exceeded {
        selection.files.clear();
        selection.missing.clear();
        selection.errors.clear();
        selection.module_tree_entries.clear();
    }
    selection.files.sort_by(|left, right| {
        left.path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.path.as_os_str().as_encoded_bytes())
    });
    selection.missing.sort_by(|left, right| {
        left.1
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.1.as_os_str().as_encoded_bytes())
            .then_with(|| left.0.cmp(&right.0))
    });
    selection.errors.sort_by(|left, right| {
        left.path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.path.as_os_str().as_encoded_bytes())
    });
    selection
}

impl DirectoryWalker<'_> {
    fn record_module_tree_entry(&mut self, path: &Path) -> bool {
        let Some(limits) = self.module_tree_limits else {
            return true;
        };
        let next_entry_count = self
            .selection
            .module_tree_inspected_entry_count
            .saturating_add(1);
        let path_bytes = path
            .strip_prefix(&self.source_root)
            .unwrap_or(path)
            .as_os_str()
            .as_encoded_bytes()
            .len();
        let next_path_bytes = self
            .selection
            .module_tree_inspected_path_bytes
            .saturating_add(u64::try_from(path_bytes).unwrap_or(u64::MAX));
        if next_entry_count > limits.entry_count || next_path_bytes > limits.path_bytes {
            self.selection.module_tree_limit_exceeded = true;
            return false;
        }
        self.selection.module_tree_inspected_entry_count = next_entry_count;
        self.selection.module_tree_inspected_path_bytes = next_path_bytes;
        true
    }

    fn record_module_tree_source_file(&mut self) -> bool {
        let Some(limits) = self.module_tree_limits else {
            return true;
        };
        let next_file_count = self
            .selection
            .module_tree_source_file_count
            .saturating_add(1);
        if next_file_count > limits.sai_file_count {
            self.selection.module_tree_limit_exceeded = true;
            return false;
        }
        self.selection.module_tree_source_file_count = next_file_count;
        true
    }

    fn walk(&mut self, root_directory: Dir, root: PathBuf) {
        let mut pending = vec![PendingDirectory::Opened(Arc::new(root_directory), root)];
        while let Some(pending_directory) = pending.pop() {
            let (directory, directory_path) = match pending_directory {
                PendingDirectory::Opened(directory, path) => (directory, path),
                PendingDirectory::Child { parent, name, path } => {
                    let opened = parent
                        .try_clone()
                        .map(|parent| parent.into_std_file())
                        .and_then(|parent| {
                            let parent_path = path.parent().unwrap_or(&path);
                            match self.update_directory_files {
                                true => sai_windows_fs::open_child_directory_for_file_updates_without_following(
                                    &parent,
                                    parent_path,
                                    &name,
                                ),
                                false => sai_windows_fs::open_child_directory_without_following(
                                    &parent,
                                    parent_path,
                                    &name,
                                ),
                            }
                        });
                    match opened {
                        Ok(child) => (Arc::new(Dir::from_std_file(child)), path),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(source) => {
                            self.selection
                                .errors
                                .push(FmtPathSelectionError { path, source });
                            continue;
                        }
                    }
                }
            };
            if self.cancellation.is_cancelled() {
                self.selection.interrupted = true;
                return;
            }
            if !self.visited_directories.insert(directory_path.clone()) {
                continue;
            }

            let entries = match directory.entries() {
                Ok(entries) => entries,
                Err(source) => {
                    self.selection.errors.push(FmtPathSelectionError {
                        path: directory_path,
                        source,
                    });
                    continue;
                }
            };
            let mut ordered_entries = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let name = entry.file_name();
                        let path = directory_path.join(&name);
                        if self.traversal_mode == FmtTraversalMode::ModuleTree {
                            if !self.record_module_tree_entry(&path) {
                                return;
                            }
                            if is_hidden_entry_name(&name) {
                                continue;
                            }
                        }
                        ordered_entries.push(entry);
                    }
                    Err(source) => self.selection.errors.push(FmtPathSelectionError {
                        path: directory_path.clone(),
                        source,
                    }),
                }
            }
            ordered_entries.sort_by(|left, right| {
                left.file_name()
                    .as_encoded_bytes()
                    .cmp(right.file_name().as_encoded_bytes())
            });
            let mut child_directories = Vec::new();
            for entry in ordered_entries {
                if self.cancellation.is_cancelled() {
                    self.selection.interrupted = true;
                    return;
                }
                let name = entry.file_name();
                let path = directory_path.join(&name);
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(source) => {
                        self.selection
                            .errors
                            .push(FmtPathSelectionError { path, source });
                        continue;
                    }
                };

                if self.traversal_mode == FmtTraversalMode::ModuleTree
                    && is_hidden_entry_name(&name)
                {
                    continue;
                }
                if file_type.is_symlink() {
                    if self.traversal_mode == FmtTraversalMode::ModuleTree {
                        push_module_tree_entry(
                            self.selection,
                            &self.source_root,
                            &path,
                            PackageFileKind::Symlink,
                        );
                    }
                    continue;
                }
                if file_type.is_dir() {
                    let skip_directory = match self.traversal_mode {
                        FmtTraversalMode::ExplicitPaths => should_skip_directory(&directory, &name),
                        FmtTraversalMode::WorkspaceSource => false,
                        FmtTraversalMode::ModuleTree => is_hidden_entry_name(&name),
                    };
                    if skip_directory {
                        continue;
                    }
                    if self.traversal_mode == FmtTraversalMode::ModuleTree {
                        push_module_tree_entry(
                            self.selection,
                            &self.source_root,
                            &path,
                            PackageFileKind::Directory,
                        );
                    }
                    child_directories.push(PendingDirectory::Child {
                        parent: Arc::clone(&directory),
                        name,
                        path,
                    });
                    continue;
                }
                let selected_file = match self.traversal_mode {
                    FmtTraversalMode::ExplicitPaths => is_formatter_file(&path),
                    FmtTraversalMode::WorkspaceSource => is_source_file(&path),
                    FmtTraversalMode::ModuleTree => is_module_source_file(&path),
                };
                if file_type.is_file() && self.traversal_mode == FmtTraversalMode::ModuleTree {
                    push_module_tree_entry(
                        self.selection,
                        &self.source_root,
                        &path,
                        PackageFileKind::File,
                    );
                }
                if file_type.is_file() && selected_file {
                    if self.traversal_mode == FmtTraversalMode::ModuleTree
                        && !self.record_module_tree_source_file()
                    {
                        return;
                    }
                    let is_package_manifest = name == "sai.package";
                    push_selected_file(
                        path,
                        Arc::clone(&directory),
                        name,
                        FmtSelectedPathState {
                            selection: self.selection,
                            selected_files: self.selected_files,
                            selected_identities: self.selected_identities,
                            retain_capabilities: self.retain_capabilities,
                        },
                        is_package_manifest,
                    );
                }
            }
            pending.extend(child_directories.into_iter().rev());
        }
    }
}

fn push_module_tree_entry(
    selection: &mut FmtPathSelection,
    source_root: &Path,
    path: &Path,
    kind: PackageFileKind,
) {
    let Ok(relative_path) = path.strip_prefix(source_root) else {
        selection.errors.push(FmtPathSelectionError {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "module-tree entry escaped its selected source root",
            ),
        });
        return;
    };
    let Some(relative_path) = relative_path.to_str() else {
        selection.errors.push(FmtPathSelectionError {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "module-tree paths must be valid UTF-8",
            ),
        });
        return;
    };
    let relative_path = relative_path.replace(std::path::MAIN_SEPARATOR, "/");
    match PackageFileEntry::new(relative_path, kind) {
        Ok(entry) => selection.module_tree_entries.push(entry),
        Err(_) => selection.errors.push(FmtPathSelectionError {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "module-tree path is not a canonical package-relative path",
            ),
        }),
    }
}

fn open_explicit_directory(
    path: &Path,
    file_access: DirectoryFileAccess,
) -> io::Result<(Dir, PathBuf)> {
    let name = path.file_name();
    let Some(name) = name else {
        let root_path = fs::canonicalize(path)?;
        let root = match file_access {
            DirectoryFileAccess::ReadOnly => {
                sai_windows_fs::open_directory_path_without_following(&root_path)
            }
            DirectoryFileAccess::Update => {
                sai_windows_fs::open_directory_path_for_file_updates_without_following(&root_path)
            }
        }?;
        return Ok((Dir::from_std_file(root), root_path));
    };
    let parent_path = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path has no parent component",
        )
    })?;
    let parent_path = fs::canonicalize(parent_path)?;
    let parent = sai_windows_fs::open_directory_path_without_following(&parent_path)?;
    let root = match file_access {
        DirectoryFileAccess::ReadOnly => {
            sai_windows_fs::open_child_directory_without_following(&parent, &parent_path, name)
        }
        DirectoryFileAccess::Update => {
            sai_windows_fs::open_child_directory_for_file_updates_without_following(
                &parent,
                &parent_path,
                name,
            )
        }
    }?;
    let root_identity = sai_windows_fs::file_identity(&root)?;
    let root_path = fs::canonicalize(path)?;
    let canonical_root = sai_windows_fs::open_directory_path_without_following(&root_path)?;
    if sai_windows_fs::file_identity(&canonical_root)? != root_identity {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "directory changed while resolving its canonical path",
        ));
    }
    Ok((Dir::from_std_file(root), root_path))
}

fn directory_identity(directory: &Dir) -> io::Result<sai_windows_fs::FileIdentity> {
    sai_windows_fs::file_identity(&directory.try_clone()?.into_std_file())
}

fn should_skip_root_directory(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    if is_hidden_entry_name(name) {
        return true;
    }
    if name != "out" {
        return false;
    }

    let Some(parent) = path.parent() else {
        return false;
    };
    fs::symlink_metadata(parent.join("sai.package"))
        .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn should_skip_directory(parent: &Dir, name: &OsStr) -> bool {
    if is_hidden_entry_name(name) {
        return true;
    }
    name == "out"
        && parent
            .symlink_metadata("sai.package")
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn is_hidden_entry_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
}

fn is_formatter_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == "sai.package" || path.extension().is_some_and(|extension| extension == "sai")
    })
}

fn is_source_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "sai")
}

fn is_module_source_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| !is_hidden_entry_name(name))
        && is_source_file(path)
}
fn push_explicit_file(
    path: &Path,
    selection: &mut FmtPathSelection,
    selected_files: &mut HashSet<PathBuf>,
    selected_identities: &mut HashSet<SelectedPathIdentity>,
    parent_directories: &mut HashMap<PathBuf, Arc<Dir>>,
    retain_capabilities: bool,
    directory_file_access: DirectoryFileAccess,
) {
    let is_package_manifest = path.file_name().is_some_and(|name| name == "sai.package");
    let canonical_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(source) => {
            selection.errors.push(FmtPathSelectionError {
                path: path.to_path_buf(),
                source,
            });
            return;
        }
    };
    if is_package_manifest {
        selection
            .resolved_manifest_paths
            .push((path.to_path_buf(), canonical_path.clone()));
    }
    let Some(name) = canonical_path.file_name().map(OsStr::to_os_string) else {
        selection.errors.push(FmtPathSelectionError {
            path: canonical_path,
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "formatter input has no file name",
            ),
        });
        return;
    };
    let Some(parent_path) = canonical_path.parent() else {
        selection.errors.push(FmtPathSelectionError {
            path: canonical_path,
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "formatter input has no parent directory",
            ),
        });
        return;
    };
    let opened = (|| {
        let parent_path = fs::canonicalize(parent_path)?;
        let retain_parent_directory = retain_capabilities;
        let parent = if retain_parent_directory {
            if let Some(parent) = parent_directories.get(&parent_path) {
                Arc::clone(parent)
            } else {
                let parent = Arc::new(Dir::from_std_file(match directory_file_access {
                    DirectoryFileAccess::ReadOnly => {
                        sai_windows_fs::open_directory_path_without_following(&parent_path)
                    }
                    DirectoryFileAccess::Update => {
                        sai_windows_fs::open_directory_path_for_file_updates_without_following(
                            &parent_path,
                        )
                    }
                }?));
                parent_directories.insert(parent_path.clone(), Arc::clone(&parent));
                parent
            }
        } else {
            Arc::new(Dir::from_std_file(match directory_file_access {
                DirectoryFileAccess::ReadOnly => {
                    sai_windows_fs::open_directory_path_without_following(&parent_path)
                }
                DirectoryFileAccess::Update => {
                    sai_windows_fs::open_directory_path_for_file_updates_without_following(
                        &parent_path,
                    )
                }
            }?))
        };
        Ok::<_, io::Error>((parent_path, parent))
    })();
    match opened {
        Ok((parent_path, parent)) => {
            let physical_path = parent_path.join(&name);
            push_selected_file(
                physical_path,
                parent,
                name,
                FmtSelectedPathState {
                    selection,
                    selected_files,
                    selected_identities,
                    retain_capabilities,
                },
                is_package_manifest,
            );
        }
        Err(source) => selection.errors.push(FmtPathSelectionError {
            path: canonical_path,
            source,
        }),
    }
}

fn push_selected_file(
    path: PathBuf,
    parent: Arc<Dir>,
    name: OsString,
    state: FmtSelectedPathState<'_>,
    is_package_manifest: bool,
) {
    let FmtSelectedPathState {
        selection,
        selected_files,
        selected_identities,
        retain_capabilities,
    } = state;
    if path.to_str().is_none() {
        selection.errors.push(FmtPathSelectionError {
            path,
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "formatter paths must be valid UTF-8 to appear in CLI output",
            ),
        });
        return;
    }
    if selected_files.contains(&path) {
        return;
    }
    let parent_identity = match directory_identity(&parent) {
        Ok(identity) => identity,
        Err(source) => {
            selection
                .errors
                .push(FmtPathSelectionError { path, source });
            return;
        }
    };
    let parent_handle = match parent.try_clone() {
        Ok(parent) => parent.into_std_file(),
        Err(source) => {
            selection
                .errors
                .push(FmtPathSelectionError { path, source });
            return;
        }
    };
    let input = match sai_windows_fs::open_file_without_following(
        &parent_handle,
        path.parent().unwrap_or(&path),
        &name,
    ) {
        Ok(input) => input,
        Err(source) => {
            selection
                .errors
                .push(FmtPathSelectionError { path, source });
            return;
        }
    };
    let identity = match sai_windows_fs::file_identity(&input) {
        Ok(identity) => identity,
        Err(source) => {
            selection
                .errors
                .push(FmtPathSelectionError { path, source });
            return;
        }
    };
    if !selected_identities.insert((identity, parent_identity, name.clone())) {
        return;
    }
    selected_files.insert(path.clone());
    let identity_guard = (is_package_manifest && retain_capabilities).then_some(input);
    selection.files.push(SelectedFmtPath {
        path,
        is_package_manifest,
        parent: retain_capabilities.then_some(parent),
        name,
        identity,
        parent_identity,
        identity_guard,
    });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use sai_driver::CancellationToken;

    use super::{
        select_fmt_module_paths_with_limits, select_fmt_paths as select_fmt_paths_with_cancellation,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn select_fmt_paths(paths: &[PathBuf], current_directory: &Path) -> super::FmtPathSelection {
        select_fmt_paths_with_cancellation(paths, current_directory, &CancellationToken::default())
    }

    fn selected_paths(selection: &super::FmtPathSelection) -> Vec<PathBuf> {
        selection
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect()
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-fmt-paths-{}-{id}", std::process::id()));
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
    fn module_tree_scan_stops_at_entry_and_path_budgets() {
        let directory = TestDirectory::new();
        let source_root = directory.0.join("src");
        fs::create_dir(&source_root).unwrap();
        let source_root = fs::canonicalize(source_root).unwrap();
        fs::write(source_root.join("a.sai"), b"").unwrap();
        fs::write(source_root.join("b.sai"), b"").unwrap();
        let limits =
            |entry_count, path_bytes| sai_package::local_source::LocalPackageSourceSnapshotLimits {
                entry_count,
                path_bytes,
                sai_file_count: u64::MAX,
                total_sai_file_bytes: u64::MAX,
            };

        let entry_limited = select_fmt_module_paths_with_limits(
            std::slice::from_ref(&source_root),
            &directory.0,
            &CancellationToken::default(),
            limits(1, u64::MAX),
        );
        assert!(
            entry_limited.module_tree_limit_exceeded,
            "{entry_limited:#?}"
        );
        assert_eq!(entry_limited.module_tree_inspected_entry_count, 1);
        assert!(entry_limited.module_tree_entries.is_empty());
        assert!(entry_limited.files.is_empty());

        let path_limited = select_fmt_module_paths_with_limits(
            std::slice::from_ref(&source_root),
            &directory.0,
            &CancellationToken::default(),
            limits(u64::MAX, 4),
        );
        assert!(path_limited.module_tree_limit_exceeded);
        assert_eq!(path_limited.module_tree_inspected_entry_count, 0);
        assert_eq!(path_limited.module_tree_inspected_path_bytes, 0);
        assert!(path_limited.files.is_empty());
    }

    #[test]
    fn module_tree_scan_stops_before_retaining_too_many_source_files() {
        let directory = TestDirectory::new();
        let source_root = directory.0.join("src");
        fs::create_dir(&source_root).unwrap();
        let source_root = fs::canonicalize(source_root).unwrap();
        fs::write(source_root.join("a.sai"), b"").unwrap();
        fs::write(source_root.join("b.sai"), b"").unwrap();

        let selection = select_fmt_module_paths_with_limits(
            std::slice::from_ref(&source_root),
            &directory.0,
            &CancellationToken::default(),
            sai_package::local_source::LocalPackageSourceSnapshotLimits {
                entry_count: 10,
                path_bytes: u64::MAX,
                sai_file_count: 1,
                total_sai_file_bytes: u64::MAX,
            },
        );

        assert!(selection.module_tree_limit_exceeded, "{selection:#?}");
        assert_eq!(selection.module_tree_inspected_entry_count, 2);
        assert_eq!(selection.module_tree_source_file_count, 1);
        assert!(selection.files.is_empty());
    }

    #[test]
    fn directory_search_selects_only_package_and_source_files_in_sorted_order() {
        let directory = TestDirectory::new();
        fs::create_dir_all(directory.0.join("src/nested")).unwrap();
        fs::write(directory.0.join("sai.package"), b"").unwrap();
        fs::write(directory.0.join("src/z.sai"), b"").unwrap();
        fs::write(directory.0.join("src/nested/a.sai"), b"").unwrap();
        fs::write(directory.0.join("README.md"), b"").unwrap();

        let selection = select_fmt_paths(std::slice::from_ref(&directory.0), &directory.0);
        let canonical_directory = fs::canonicalize(&directory.0).unwrap();
        let relative = selection
            .files
            .iter()
            .map(|file| file.path.strip_prefix(&canonical_directory).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            relative,
            [
                PathBuf::from("sai.package"),
                PathBuf::from("src/nested/a.sai"),
                PathBuf::from("src/z.sai"),
            ]
        );
        assert!(selection.missing.is_empty());
        assert!(selection.errors.is_empty());
    }

    #[test]
    fn directory_search_skips_hidden_out_and_symbolic_link_directories() {
        let directory = TestDirectory::new();
        fs::create_dir_all(directory.0.join(".hidden")).unwrap();
        fs::create_dir_all(directory.0.join("out")).unwrap();
        fs::create_dir_all(directory.0.join("visible")).unwrap();
        fs::write(directory.0.join("sai.package"), b"").unwrap();
        fs::write(directory.0.join(".hidden/hidden.sai"), b"").unwrap();
        fs::write(directory.0.join("out/generated.sai"), b"").unwrap();
        fs::write(directory.0.join("visible/ok.sai"), b"").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(directory.0.join("visible"), directory.0.join("linked"))
            .unwrap();

        let selection = select_fmt_paths(std::slice::from_ref(&directory.0), &directory.0);
        let canonical_directory = fs::canonicalize(&directory.0).unwrap();
        let relative = selection
            .files
            .iter()
            .map(|file| file.path.strip_prefix(&canonical_directory).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            relative,
            [
                PathBuf::from("sai.package"),
                PathBuf::from("visible/ok.sai")
            ]
        );
    }

    #[test]
    fn workspace_source_search_includes_hidden_and_out_files_but_not_nested_manifests() {
        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        let source = package.join("src");
        fs::create_dir_all(source.join(".hidden")).unwrap();
        fs::create_dir_all(source.join("out")).unwrap();
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(package.join("sai.package"), b"").unwrap();
        fs::write(source.join("root.sai"), b"").unwrap();
        fs::write(source.join(".hidden/hidden.sai"), b"").unwrap();
        fs::write(source.join("out/generated.sai"), b"").unwrap();
        fs::write(source.join("nested/sai.package"), b"").unwrap();
        fs::write(source.join("nested/module.sai"), b"").unwrap();

        let paths = [
            fs::canonicalize(package.join("sai.package")).unwrap(),
            fs::canonicalize(&source).unwrap(),
        ];
        let canonical_package = fs::canonicalize(&package).unwrap();
        let selection =
            super::select_fmt_workspace_paths(&paths, &directory.0, &CancellationToken::default());
        let comparison = super::select_fmt_workspace_paths_for_comparison(
            &paths,
            &directory.0,
            &CancellationToken::default(),
        );
        assert_eq!(selected_paths(&selection), selected_paths(&comparison));
        let relative = selection
            .files
            .iter()
            .map(|file| file.path.strip_prefix(&canonical_package).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            relative,
            [
                PathBuf::from("sai.package"),
                PathBuf::from("src/.hidden/hidden.sai"),
                PathBuf::from("src/nested/module.sai"),
                PathBuf::from("src/out/generated.sai"),
                PathBuf::from("src/root.sai"),
            ]
        );
    }

    #[test]
    fn overlapping_paths_and_explicit_files_are_deduplicated() {
        let directory = TestDirectory::new();
        let source = directory.0.join("src.sai");
        fs::write(&source, b"").unwrap();

        let selection = select_fmt_paths(
            &[directory.0.clone(), source.clone(), directory.0.clone()],
            &directory.0,
        );
        assert_eq!(
            selected_paths(&selection),
            [fs::canonicalize(source).unwrap()]
        );
    }

    #[test]
    fn distinct_hard_link_names_are_selected_as_distinct_paths() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.sai");
        let second = directory.0.join("second.sai");
        fs::write(&first, b"same inode").unwrap();
        fs::hard_link(&first, &second).unwrap();

        let selection = select_fmt_paths(std::slice::from_ref(&directory.0), &directory.0);

        assert_eq!(
            selected_paths(&selection),
            [
                fs::canonicalize(first).unwrap(),
                fs::canonicalize(second).unwrap()
            ]
        );
    }

    #[test]
    fn explicit_files_share_parent_capabilities_and_guard_manifests() {
        let directory = TestDirectory::new();
        let manifest = directory.0.join("sai.package");
        let source = directory.0.join("src.sai");
        let second_source = directory.0.join("other.sai");
        fs::write(&manifest, b"").unwrap();
        fs::write(&source, b"").unwrap();
        fs::write(&second_source, b"").unwrap();

        let selection = select_fmt_paths(&[source, second_source, manifest], &directory.0);

        assert_eq!(selection.files.len(), 3);
        let manifest = selection
            .files
            .iter()
            .find(|file| file.name == "sai.package")
            .unwrap();
        let source = selection
            .files
            .iter()
            .find(|file| file.name == "src.sai")
            .unwrap();
        let second_source = selection
            .files
            .iter()
            .find(|file| file.name == "other.sai")
            .unwrap();
        assert!(manifest.parent.is_some());
        assert!(source.parent.is_some());
        assert!(std::sync::Arc::ptr_eq(
            source.parent.as_ref().unwrap(),
            second_source.parent.as_ref().unwrap()
        ));
        assert!(manifest.identity_guard.is_some());
        assert!(source.identity_guard.is_none());
        assert_eq!(
            selection
                .files
                .iter()
                .filter(|file| file.identity_guard.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn comparison_selection_drops_all_path_capabilities_after_identity_sampling() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("sai.package"), b"").unwrap();
        fs::write(directory.0.join("src.sai"), b"").unwrap();

        let selection = super::select_fmt_paths_for_comparison(
            std::slice::from_ref(&directory.0),
            &directory.0,
            &CancellationToken::default(),
        );

        assert_eq!(selection.files.len(), 2);
        assert!(
            selection
                .files
                .iter()
                .all(|file| file.parent.is_none() && file.identity_guard.is_none())
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_file_symlinks_resolve_to_their_target() {
        let directory = TestDirectory::new();
        let target = directory.0.join("target.txt");
        let link = directory.0.join("link.txt");
        fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let selection = select_fmt_paths(&[link], &directory.0);

        assert_eq!(
            selected_paths(&selection),
            [fs::canonicalize(target).unwrap()]
        );
        assert!(selection.missing.is_empty());
        assert!(selection.errors.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_directory_symlinks_are_skipped() {
        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        let link = directory.0.join("link");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("source.sai"), b"").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let selection = select_fmt_paths(std::slice::from_ref(&link), &directory.0);

        assert!(selection.files.is_empty());
        assert!(selection.missing.is_empty());
        assert!(selection.errors.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn module_tree_roots_reject_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        fs::create_dir_all(target.join("src")).unwrap();
        fs::write(target.join("src/lib.sai"), b"").unwrap();
        let direct_link = directory.0.join("source-link");
        symlink(target.join("src"), &direct_link).unwrap();

        let direct_selection = super::select_fmt_module_paths(
            std::slice::from_ref(&direct_link),
            &directory.0,
            &CancellationToken::default(),
        );
        assert!(direct_selection.module_tree_root_symlink);
        assert!(direct_selection.files.is_empty());
        assert!(direct_selection.module_tree_entries.is_empty());
        assert!(direct_selection.errors.is_empty());

        let ancestor_link = directory.0.join("alias");
        symlink(&target, &ancestor_link).unwrap();
        let ancestor_source = ancestor_link.join("src");
        let ancestor_selection = super::select_fmt_module_paths(
            std::slice::from_ref(&ancestor_source),
            &directory.0,
            &CancellationToken::default(),
        );
        assert!(!ancestor_selection.module_tree_root_symlink);
        assert!(ancestor_selection.files.is_empty());
        assert!(ancestor_selection.module_tree_entries.is_empty());
        assert_eq!(ancestor_selection.errors.len(), 1);
    }

    #[test]
    fn explicit_directory_paths_use_the_filesystem_canonical_spelling() {
        let directory = TestDirectory::new();
        let canonical = directory.0.join("Package");
        let alias = directory.0.join("PACKAGE");
        fs::create_dir(&canonical).unwrap();
        fs::write(canonical.join("sai.package"), b"").unwrap();
        if fs::symlink_metadata(&alias).is_err() {
            return;
        }

        let canonical_selection = select_fmt_paths(std::slice::from_ref(&canonical), &directory.0);
        let alias_selection = select_fmt_paths(std::slice::from_ref(&alias), &directory.0);

        assert_eq!(
            selected_paths(&canonical_selection),
            selected_paths(&alias_selection)
        );
    }

    #[test]
    fn missing_paths_are_kept_and_sorted_for_diagnostics() {
        let directory = TestDirectory::new();
        let selection = select_fmt_paths(
            &[PathBuf::from("z.missing"), PathBuf::from("a.missing")],
            &directory.0,
        );
        assert!(selection.files.is_empty());
        assert_eq!(
            selection
                .missing
                .iter()
                .map(|(_, path)| path.as_path())
                .collect::<Vec<_>>(),
            [
                std::path::Path::new("a.missing"),
                std::path::Path::new("z.missing")
            ]
        );
    }

    #[test]
    fn canceled_traversal_stops_before_selecting_paths() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("source.sai"), b"").unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let selection = select_fmt_paths_with_cancellation(
            std::slice::from_ref(&directory.0),
            &directory.0,
            &cancellation,
        );

        assert!(selection.interrupted);
        assert!(selection.files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn selected_file_parent_handle_survives_path_replacement() {
        use std::io::Read;

        let directory = TestDirectory::new();
        let package = directory.0.join("package");
        let moved = directory.0.join("moved");
        let outside = directory.0.join("outside");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(package.join("sai.package"), b"original").unwrap();
        fs::write(outside.join("sai.package"), b"outside").unwrap();

        let selection = select_fmt_paths(std::slice::from_ref(&package), &directory.0);
        assert_eq!(selection.files.len(), 1);

        fs::rename(&package, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &package).unwrap();

        let selected = &selection.files[0];
        let parent = selected
            .parent
            .as_ref()
            .unwrap()
            .try_clone()
            .unwrap()
            .into_std_file();
        let mut input = sai_windows_fs::open_file_without_following(
            &parent,
            selected.path.parent().unwrap(),
            &selected.name,
        )
        .unwrap();
        let mut contents = Vec::new();
        input.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"original");
        assert_eq!(fs::read(outside.join("sai.package")).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_paths_are_rejected_without_lossy_deduplication() {
        use std::os::unix::ffi::OsStringExt;

        let directory = TestDirectory::new();
        let mut selection = super::FmtPathSelection::default();
        let mut selected_files = std::collections::HashSet::new();
        let mut selected_identities = std::collections::HashSet::new();
        let parent = std::sync::Arc::new(
            cap_std::fs::Dir::open_ambient_dir(&directory.0, cap_std::ambient_authority()).unwrap(),
        );
        for byte in [0x80, 0x81] {
            let name = std::ffi::OsString::from_vec(vec![b'p', byte]);
            let child = directory.0.join(&name).join("sai.package");
            super::push_selected_file(
                child,
                std::sync::Arc::clone(&parent),
                name,
                super::FmtSelectedPathState {
                    selection: &mut selection,
                    selected_files: &mut selected_files,
                    selected_identities: &mut selected_identities,
                    retain_capabilities: true,
                },
                false,
            );
        }

        assert!(selection.files.is_empty());
        assert_eq!(selection.errors.len(), 2);
        assert!(
            selection
                .errors
                .iter()
                .all(|error| error.source.kind() == std::io::ErrorKind::InvalidInput)
        );
        assert_ne!(
            selection.errors[0].path.as_os_str().as_encoded_bytes(),
            selection.errors[1].path.as_os_str().as_encoded_bytes()
        );
    }
}
