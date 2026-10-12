use std::collections::{HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use sai_driver::CancellationToken;
use sai_package::manifest::{
    MAX_PACKAGE_MANIFEST_BYTES, PackageDeclaration, PackageRole, RequirementSource,
    parse_for_role_with_error_limit,
};
use sai_package::package_path::{ContentPackagePath, DependencyPackagePath};
use sha2::{Digest, Sha256};

use crate::fmt_paths::FmtPathSelectionError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FmtManifestSnapshot {
    /// Manifest path under its package directory before resolving symlinks.
    pub logical_path: PathBuf,
    /// Canonical file whose bytes were read.
    pub path: PathBuf,
    /// Owning package for manifests selected for formatting. External path
    /// manifests are snapshotted but are not formatted or locked.
    pub package_directory: Option<PathBuf>,
    /// Fingerprint of the complete manifest, or of the bounded prefix for
    /// oversized manifests.
    pub digest: Option<[u8; 32]>,
    pub oversized: bool,
}

#[derive(Debug, Default)]
pub(crate) struct FmtWorkspaceDiscovery {
    pub workspace_root: Option<PathBuf>,
    pub diagnostic_base: Option<PathBuf>,
    /// Explicit manifest paths and source directories for workspace members.
    pub input_paths: Vec<PathBuf>,
    /// Canonical source directories paired with the package that declared
    /// them. A file can belong to more than one selected source tree.
    pub source_directories: Vec<FmtWorkspaceSourceDirectory>,
    /// Snapshots of every local path-package manifest visited, including
    /// packages outside the workspace whose path requirements may lead back
    /// into it.
    pub manifest_snapshots: Vec<FmtManifestSnapshot>,
    /// Package path errors needed to explain why workspace discovery could
    /// not follow or select a declared path.
    pub diagnostics: Vec<FmtWorkspaceDiagnostic>,
    pub errors: Vec<FmtPathSelectionError>,
    /// Total issues, including parser errors and issues omitted from the
    /// bounded diagnostic vectors.
    pub total_error_count: usize,
    pub interrupted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FmtWorkspaceSourceDirectory {
    pub directory: PathBuf,
    pub package_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FmtWorkspaceDiagnostic {
    /// Canonical file whose bytes and span produced the diagnostic.
    pub manifest_path: Option<PathBuf>,
    /// Manifest path as declared by the owning package, retained for display.
    pub display_path: PathBuf,
    pub code: &'static str,
    pub message: String,
    pub span: Option<Range<usize>>,
}

/// Discovers local path-package manifests reachable from the root package.
///
/// Only files belonging to packages physically inside `workspace_root` are
/// selected for formatting. External path packages are read only to follow
/// their transitive local path requirements; Git requirements are never
/// fetched.
#[cfg(test)]
pub(crate) fn discover_fmt_workspace(
    workspace_root: &Path,
    cancellation: &CancellationToken,
) -> FmtWorkspaceDiscovery {
    discover_fmt_workspace_with_error_limit_and_base(
        workspace_root,
        cancellation,
        usize::MAX,
        workspace_root,
    )
}

pub(crate) fn discover_fmt_workspace_with_error_limit_and_base(
    workspace_root: &Path,
    cancellation: &CancellationToken,
    error_limit: usize,
    diagnostic_base: &Path,
) -> FmtWorkspaceDiscovery {
    let mut result = FmtWorkspaceDiscovery {
        workspace_root: Some(workspace_root.to_path_buf()),
        diagnostic_base: Some(diagnostic_base.to_path_buf()),
        ..FmtWorkspaceDiscovery::default()
    };
    let workspace_root = match fs::canonicalize(workspace_root) {
        Ok(path) => path,
        Err(source) => {
            record_error(
                &mut result,
                FmtPathSelectionError {
                    path: workspace_root.to_path_buf(),
                    source,
                },
                error_limit,
            );
            return result;
        }
    };
    result.workspace_root = Some(workspace_root.clone());
    let mut pending = VecDeque::from([workspace_root.clone()]);
    let mut visited = HashSet::new();

    while let Some(package_directory) = pending.pop_front() {
        if cancellation.is_cancelled() {
            result.interrupted = true;
            return result;
        }
        let package_directory = match fs::canonicalize(&package_directory) {
            Ok(path) => path,
            Err(source) => {
                record_error(
                    &mut result,
                    FmtPathSelectionError {
                        path: safe_error_path(&workspace_root, &package_directory),
                        source,
                    },
                    error_limit,
                );
                continue;
            }
        };
        if !visited.insert(package_directory.clone()) {
            continue;
        }
        if !fs::metadata(&package_directory).is_ok_and(|metadata| metadata.is_dir()) {
            record_error(
                &mut result,
                FmtPathSelectionError {
                    path: safe_error_path(&workspace_root, &package_directory),
                    source: io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "local path requirement is not a directory",
                    ),
                },
                error_limit,
            );
            continue;
        }

        let manifest_path = package_directory.join("sai.package");
        let canonical_manifest = match fs::canonicalize(&manifest_path) {
            Ok(path) => path,
            Err(source) => {
                if package_directory.starts_with(&workspace_root) {
                    result.input_paths.push(manifest_path.clone());
                } else {
                    record_error(
                        &mut result,
                        FmtPathSelectionError {
                            path: workspace_root.join("sai.package"),
                            source,
                        },
                        error_limit,
                    );
                }
                continue;
            }
        };
        if !fs::metadata(&canonical_manifest).is_ok_and(|metadata| metadata.is_file()) {
            record_error(
                &mut result,
                FmtPathSelectionError {
                    path: safe_error_path(&workspace_root, &canonical_manifest),
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "local path requirement manifest is not a regular file",
                    ),
                },
                error_limit,
            );
            continue;
        }
        let is_workspace_member = package_directory.starts_with(&workspace_root);
        let (manifest_bytes, oversized) =
            match read_manifest_prefix(&canonical_manifest, cancellation) {
                Ok(result) => result,
                Err(ReadManifestError::Interrupted) => {
                    result.interrupted = true;
                    return result;
                }
                Err(ReadManifestError::Io(source)) => {
                    result.manifest_snapshots.push(FmtManifestSnapshot {
                        logical_path: manifest_path.clone(),
                        path: canonical_manifest.clone(),
                        package_directory: is_workspace_member.then_some(package_directory.clone()),
                        digest: None,
                        oversized: false,
                    });
                    if package_directory.starts_with(&workspace_root) {
                        result.input_paths.push(manifest_path.clone());
                        record_error(
                            &mut result,
                            FmtPathSelectionError {
                                path: safe_error_path(&workspace_root, &manifest_path),
                                source,
                            },
                            error_limit,
                        );
                    } else {
                        record_error(
                            &mut result,
                            FmtPathSelectionError {
                                path: workspace_root.join("sai.package"),
                                source,
                            },
                            error_limit,
                        );
                    }
                    continue;
                }
            };

        if is_workspace_member {
            result.input_paths.push(manifest_path.clone());
        }
        let digest = Some(Sha256::digest(&manifest_bytes).into());
        result.manifest_snapshots.push(FmtManifestSnapshot {
            logical_path: manifest_path,
            path: canonical_manifest.clone(),
            package_directory: is_workspace_member.then_some(package_directory.clone()),
            digest,
            oversized,
        });
        if oversized {
            if !is_workspace_member {
                record_error(
                    &mut result,
                    FmtPathSelectionError {
                        path: workspace_root.join("sai.package"),
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            "external local path manifest exceeds the discovery limit",
                        ),
                    },
                    error_limit,
                );
            }
            continue;
        }

        let role = if package_directory == workspace_root {
            PackageRole::Root
        } else {
            PackageRole::Dependency
        };
        let parsed = parse_for_role_with_error_limit(&manifest_bytes, role, error_limit);
        if is_workspace_member {
            result.total_error_count = result
                .total_error_count
                .saturating_add(parsed.invalid_path_error_count());
            for error in parsed.invalid_path_errors() {
                retain_diagnostic(
                    &mut result,
                    FmtWorkspaceDiagnostic {
                        manifest_path: Some(canonical_manifest.clone()),
                        display_path: package_directory.join("sai.package"),
                        code: "INVALID_PACKAGE_PATH",
                        message: "invalid package path".to_owned(),
                        span: Some(error.span.clone()),
                    },
                    error_limit,
                );
            }
        } else {
            let invalid_path_error_count = parsed.invalid_path_error_count();
            if invalid_path_error_count > 0 {
                record_diagnostic(
                    &mut result,
                    FmtWorkspaceDiagnostic {
                        manifest_path: None,
                        display_path: workspace_root.join("sai.package"),
                        code: "INVALID_PACKAGE_PATH",
                        message: "external local package manifest contains an invalid package path; workspace discovery may be incomplete".to_owned(),
                        span: None,
                    },
                    invalid_path_error_count,
                    error_limit,
                );
            }
            let other_error_count = parsed.lexical_error_count().saturating_add(
                parsed
                    .error_count()
                    .saturating_sub(invalid_path_error_count),
            );
            if other_error_count > 0 {
                record_diagnostic(
                    &mut result,
                    FmtWorkspaceDiagnostic {
                        manifest_path: None,
                        display_path: workspace_root.join("sai.package"),
                        code: "SOURCE_READ_FAILED",
                        message: "external local package manifest is incomplete; workspace discovery may omit path requirements".to_owned(),
                        span: None,
                    },
                    other_error_count,
                    error_limit,
                );
            }
        }

        for declaration in &parsed.file().declarations {
            match &declaration.value {
                PackageDeclaration::Source(source) if is_workspace_member => {
                    discover_source_directory(&package_directory, source, &mut result, error_limit);
                }
                PackageDeclaration::Requires { test, entries }
                    if !test || package_directory == workspace_root =>
                {
                    for requirement in entries {
                        let Some(RequirementSource::Path(path)) = requirement
                            .value
                            .source
                            .as_ref()
                            .map(|source| &source.value)
                        else {
                            continue;
                        };
                        let path = match DependencyPackagePath::parse(path) {
                            Ok(path) => path,
                            Err(_) => continue,
                        };
                        pending.push_back(package_directory.join(path.as_str()));
                    }
                }
                _ => {}
            }
        }
    }

    result.input_paths.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });
    result.input_paths.dedup();
    result.source_directories.sort_by(|left, right| {
        left.directory
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.directory.as_os_str().as_encoded_bytes())
            .then_with(|| {
                left.package_directory
                    .as_os_str()
                    .as_encoded_bytes()
                    .cmp(right.package_directory.as_os_str().as_encoded_bytes())
            })
    });
    result.source_directories.dedup();
    result.manifest_snapshots.sort_by(|left, right| {
        left.path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.path.as_os_str().as_encoded_bytes())
            .then_with(|| {
                left.package_directory
                    .as_deref()
                    .map_or(&[][..], |path| path.as_os_str().as_encoded_bytes())
                    .cmp(
                        right
                            .package_directory
                            .as_deref()
                            .map_or(&[][..], |path| path.as_os_str().as_encoded_bytes()),
                    )
            })
    });
    result
}

fn record_error(
    result: &mut FmtWorkspaceDiscovery,
    error: FmtPathSelectionError,
    error_limit: usize,
) {
    result.total_error_count = result.total_error_count.saturating_add(1);
    result.errors.push(error);
    trim_retained_issues(result, error_limit);
}

fn record_diagnostic(
    result: &mut FmtWorkspaceDiscovery,
    diagnostic: FmtWorkspaceDiagnostic,
    error_count: usize,
    error_limit: usize,
) {
    result.total_error_count = result.total_error_count.saturating_add(error_count);
    result.diagnostics.push(diagnostic);
    trim_retained_issues(result, error_limit);
}

fn retain_diagnostic(
    result: &mut FmtWorkspaceDiscovery,
    diagnostic: FmtWorkspaceDiagnostic,
    error_limit: usize,
) {
    result.diagnostics.push(diagnostic);
    trim_retained_issues(result, error_limit);
}

fn trim_retained_issues(result: &mut FmtWorkspaceDiscovery, error_limit: usize) {
    if error_limit == usize::MAX {
        return;
    }
    let base = result.diagnostic_base.as_deref().unwrap_or_else(|| {
        result
            .workspace_root
            .as_deref()
            .expect("a discovery has a workspace root")
    });
    result
        .errors
        .sort_by(|left, right| error_sort_key(base, left).cmp(&error_sort_key(base, right)));
    result.diagnostics.sort_by(|left, right| {
        diagnostic_sort_key(base, left).cmp(&diagnostic_sort_key(base, right))
    });
    while result.errors.len().saturating_add(result.diagnostics.len()) > error_limit {
        let remove_error = match (result.errors.last(), result.diagnostics.last()) {
            (Some(error), Some(diagnostic)) => {
                error_sort_key(base, error) > diagnostic_sort_key(base, diagnostic)
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if remove_error {
            result.errors.pop();
        } else {
            result.diagnostics.pop();
        }
    }
}

fn error_sort_key(
    base: &Path,
    error: &FmtPathSelectionError,
) -> (String, usize, &'static str, String) {
    (
        display_path_from(base, &error.path),
        usize::MAX,
        "SOURCE_READ_FAILED",
        format!("could not inspect a formatter input path: {}", error.source),
    )
}

fn diagnostic_sort_key(
    base: &Path,
    diagnostic: &FmtWorkspaceDiagnostic,
) -> (String, usize, &'static str, String) {
    let message = if diagnostic.span.is_some() {
        diagnostic.message.clone()
    } else {
        format!(
            "{}: workspace discovery may be incomplete",
            diagnostic.message
        )
    };
    (
        display_path_from(base, &diagnostic.display_path),
        diagnostic
            .span
            .as_ref()
            .map_or(usize::MAX, |span| span.start),
        diagnostic.code,
        message,
    )
}

fn display_path_from(base: &Path, path: &Path) -> String {
    relative_path(base, path)
        .or_else(|| Some(path.file_name().unwrap_or(path.as_os_str()).into()))
        .map_or_else(String::new, |path| {
            path.to_string_lossy().replace('\\', "/")
        })
}

fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let (base_prefix, base_components) = absolute_components(base)?;
    let (target_prefix, target_components) = absolute_components(target)?;
    if base_prefix != target_prefix {
        return None;
    }
    let common_length = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in &base_components[common_length..] {
        relative.push("..");
    }
    for component in &target_components[common_length..] {
        relative.push(component);
    }
    Some(relative)
}

fn absolute_components(
    path: &Path,
) -> Option<(Option<std::ffi::OsString>, Vec<std::ffi::OsString>)> {
    if !path.is_absolute() {
        return None;
    }
    let mut components = path.components();
    let prefix = match components.next()? {
        Component::Prefix(prefix) => {
            if !matches!(components.next(), Some(Component::RootDir)) {
                return None;
            }
            Some(prefix.as_os_str().to_os_string())
        }
        Component::RootDir => None,
        _ => return None,
    };
    let mut normal_components = Vec::new();
    for component in components {
        match component {
            Component::Normal(component) => normal_components.push(component.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normal_components.pop();
            }
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some((prefix, normal_components))
}

fn safe_error_path(workspace_root: &Path, path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    if normalized.starts_with(workspace_root) {
        normalized
    } else {
        workspace_root.join("sai.package")
    }
}

fn discover_source_directory(
    package_directory: &Path,
    source: &str,
    result: &mut FmtWorkspaceDiscovery,
    error_limit: usize,
) {
    let source = match ContentPackagePath::parse(source) {
        Ok(source) => source,
        Err(_) => return,
    };
    let source_path = package_directory.join(source.as_str());
    let canonical_source = match fs::canonicalize(&source_path) {
        Ok(path) => path,
        Err(source) => {
            record_error(
                result,
                FmtPathSelectionError {
                    path: source_path,
                    source,
                },
                error_limit,
            );
            return;
        }
    };
    match fs::metadata(&canonical_source) {
        Ok(metadata) if metadata.is_dir() && canonical_source.starts_with(package_directory) => {
            result.input_paths.push(canonical_source.clone());
            result.source_directories.push(FmtWorkspaceSourceDirectory {
                directory: canonical_source,
                package_directory: package_directory.to_path_buf(),
            });
        }
        Ok(metadata) if !metadata.is_dir() => record_error(
            result,
            FmtPathSelectionError {
                path: source_path,
                source: io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "declared source path is not a directory",
                ),
            },
            error_limit,
        ),
        Ok(_) => record_error(
            result,
            FmtPathSelectionError {
                path: source_path,
                source: io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "declared source directory resolves outside its package",
                ),
            },
            error_limit,
        ),
        Err(source) => record_error(
            result,
            FmtPathSelectionError {
                path: canonical_source,
                source,
            },
            error_limit,
        ),
    }
}

enum ReadManifestError {
    Interrupted,
    Io(io::Error),
}

fn read_manifest_prefix(
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<(Vec<u8>, bool), ReadManifestError> {
    let file = File::open(path).map_err(ReadManifestError::Io)?;
    let mut file = file.take(MAX_PACKAGE_MANIFEST_BYTES.saturating_add(1) as u64);
    let mut bytes = Vec::with_capacity(MAX_PACKAGE_MANIFEST_BYTES.min(8192));
    let mut buffer = [0_u8; 8192];

    loop {
        if cancellation.is_cancelled() {
            return Err(ReadManifestError::Interrupted);
        }
        let remaining = MAX_PACKAGE_MANIFEST_BYTES
            .saturating_add(1)
            .saturating_sub(bytes.len());
        if remaining == 0 {
            return Ok((bytes, true));
        }
        let read_len = remaining.min(buffer.len());
        match file.read(&mut buffer[..read_len]) {
            Ok(0) => {
                let oversized = bytes.len() > MAX_PACKAGE_MANIFEST_BYTES;
                return Ok((bytes, oversized));
            }
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(ReadManifestError::Io(source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use sai_driver::CancellationToken;

    use super::discover_fmt_workspace;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-fmt-workspace-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn package(&self, relative: &str, manifest: &str) -> PathBuf {
            let package = self.0.join(relative);
            fs::create_dir_all(&package).unwrap();
            fs::write(package.join("sai.package"), manifest).unwrap();
            package
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(identity: &str, source: Option<&str>, requires: &str) -> String {
        let source = source.map_or_else(String::new, |source| format!("source \"{source}\"\n"));
        format!("package {identity} @ 1.0.0\n{source}{requires}")
    }

    fn discovered_paths(root: &Path) -> Vec<PathBuf> {
        let discovery = discover_fmt_workspace(root, &CancellationToken::default());
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        discovery.input_paths
    }

    #[test]
    fn discovers_transitive_members_but_only_selects_packages_inside_the_workspace() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "workspace",
            &manifest(
                "acme:root",
                Some("src"),
                "requires { acme:bridge @ ^1 from path \"../outside\" }\n",
            ),
        );
        let member = directory.package(
            "workspace/member",
            &manifest("acme:member", Some("code"), ""),
        );
        let outside = directory.package(
            "outside",
            &manifest(
                "acme:bridge",
                Some("src"),
                "requires { acme:member @ ^1 from path \"../workspace/member\" }\n",
            ),
        );
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(member.join("code")).unwrap();
        fs::create_dir_all(outside.join("src")).unwrap();

        let paths = discovered_paths(&fs::canonicalize(&root).unwrap());

        assert!(paths.contains(&fs::canonicalize(root.join("sai.package")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(root.join("src")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(member.join("sai.package")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(member.join("code")).unwrap()));
        assert!(!paths.iter().any(|path| path.starts_with(&outside)));
    }

    #[test]
    fn follows_root_test_path_requirements_but_ignores_dependency_test_requirements() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "workspace",
            &manifest(
                "acme:root",
                None,
                "requires {\n  acme:member @ ^1 from path \"member\"\n}\n\
                 requires test {\n  acme:root-test @ ^1 from path \"root-test\"\n}\n",
            ),
        );
        let member = directory.package(
            "workspace/member",
            &manifest(
                "acme:member",
                None,
                "requires test {\n  acme:member-test @ ^1 from path \"../member-test\"\n}\n",
            ),
        );
        let root_test = directory.package(
            "workspace/root-test",
            &manifest(
                "acme:root-test",
                None,
                "requires test {\n  acme:nested-test @ ^1 from path \"../nested-test\"\n}\n",
            ),
        );
        let _member_test = directory.package(
            "workspace/member-test",
            &manifest("acme:member-test", None, ""),
        );
        let _nested_test = directory.package(
            "workspace/nested-test",
            &manifest("acme:nested-test", None, ""),
        );

        let paths = discovered_paths(&fs::canonicalize(&root).unwrap());

        assert!(paths.contains(&fs::canonicalize(root.join("sai.package")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(member.join("sai.package")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(root_test.join("sai.package")).unwrap()));
        assert!(
            !paths
                .iter()
                .any(|path| path.ends_with("member-test/sai.package"))
        );
        assert!(
            !paths
                .iter()
                .any(|path| path.ends_with("nested-test/sai.package"))
        );
    }

    #[test]
    fn artifact_packages_have_no_guessed_source_directory() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "root",
            &manifest(
                "acme:root",
                None,
                "requires { acme:dep @ ^1 from path \"dep\" }\n",
            ),
        );
        let dependency = directory.package("root/dep", &manifest("acme:dep", None, ""));

        let paths = discovered_paths(&fs::canonicalize(&root).unwrap());

        assert_eq!(
            paths,
            vec![
                fs::canonicalize(dependency.join("sai.package")).unwrap(),
                fs::canonicalize(root.join("sai.package")).unwrap()
            ]
        );
    }

    #[test]
    fn cyclic_path_requirements_are_deduplicated() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "root",
            &manifest(
                "acme:root",
                None,
                "requires { acme:dep @ ^1 from path \"dep\" }\n",
            ),
        );
        let dependency = directory.package(
            "root/dep",
            &manifest(
                "acme:dep",
                None,
                "requires { acme:root @ ^1 from path \"..\" }\n",
            ),
        );

        let paths = discovered_paths(&fs::canonicalize(&root).unwrap());

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&fs::canonicalize(root.join("sai.package")).unwrap()));
        assert!(paths.contains(&fs::canonicalize(dependency.join("sai.package")).unwrap()));
    }

    #[test]
    fn malformed_manifest_keeps_known_inputs_for_formatter_diagnostics() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "root",
            "package acme:root @ 1.0.0\nsource \"src\"\nrequires { acme:dep @ ^1 from path \"dep\" }\nfuture {}\n",
        );
        let _dependency = directory.package("root/dep", &manifest("acme:dep", None, ""));
        fs::create_dir_all(root.join("src")).unwrap();

        let discovery = discover_fmt_workspace(
            &fs::canonicalize(&root).unwrap(),
            &CancellationToken::default(),
        );

        assert!(discovery.errors.is_empty());
        assert_eq!(discovery.total_error_count, 0);
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("sai.package")).unwrap())
        );
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("src")).unwrap())
        );
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("dep/sai.package")).unwrap())
        );
    }

    #[test]
    fn missing_local_path_is_reported_while_other_members_remain_selected() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "root",
            &manifest(
                "acme:root",
                None,
                "requires {\n  acme:missing @ ^1 from path \"missing\"\n  acme:present @ ^1 from path \"present\"\n}\n",
            ),
        );
        let present = directory.package("root/present", &manifest("acme:present", None, ""));

        let discovery = discover_fmt_workspace(
            &fs::canonicalize(&root).unwrap(),
            &CancellationToken::default(),
        );

        assert_eq!(discovery.errors.len(), 1);
        assert_eq!(
            discovery.errors[0].path,
            fs::canonicalize(&root).unwrap().join("missing")
        );
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("sai.package")).unwrap())
        );
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(present.join("sai.package")).unwrap())
        );
    }

    #[test]
    fn malformed_external_manifest_reports_incomplete_workspace_discovery() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "workspace",
            &manifest(
                "acme:root",
                None,
                "requires { acme:bridge @ ^1 from path \"../outside\" }\n",
            ),
        );
        let _outside = directory.package(
            "outside",
            "package acme:bridge @ 1.0.0\nrequires { acme:member @ ^1 from path \"../workspace/member\" }\nfuture {}\n",
        );
        let _member = directory.package("workspace/member", &manifest("acme:member", None, ""));

        let discovery = discover_fmt_workspace(
            &fs::canonicalize(&root).unwrap(),
            &CancellationToken::default(),
        );

        assert!(discovery.errors.is_empty());
        assert_eq!(discovery.diagnostics.len(), 1);
        assert!(discovery.total_error_count > 0);
        assert_eq!(
            discovery.diagnostics[0].display_path,
            fs::canonicalize(&root).unwrap().join("sai.package")
        );
        assert!(discovery.diagnostics[0].manifest_path.is_none());
        assert!(discovery.diagnostics[0].span.is_none());
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("member/sai.package")).unwrap())
        );
    }

    #[test]
    fn discovery_bounds_retained_errors_and_keeps_the_exact_external_parse_count() {
        let directory = TestDirectory::new();
        let root = directory.package(
            "workspace",
            &manifest(
                "acme:root",
                None,
                "requires {\n  acme:first @ ^1 from path \"../first\"\n  acme:second @ ^1 from path \"../second\"\n}\n",
            ),
        );
        let first = directory.package("first", "package acme:first @ 1.0.0\nunknown-one {}\n");
        let second = directory.package("second", "package acme:second @ 1.0.0\nunknown-two {}\n");
        let canonical_root = fs::canonicalize(&root).unwrap();

        let discovery = super::discover_fmt_workspace_with_error_limit_and_base(
            &canonical_root,
            &CancellationToken::default(),
            1,
            &canonical_root,
        );
        let expected_error_count = [&first, &second]
            .into_iter()
            .map(|package| {
                let bytes = fs::read(package.join("sai.package")).unwrap();
                let parsed = sai_package::manifest::parse_for_role(
                    &bytes,
                    sai_package::manifest::PackageRole::Dependency,
                );
                parsed
                    .lexical_error_count()
                    .saturating_add(parsed.error_count())
            })
            .sum::<usize>();

        assert_eq!(discovery.diagnostics.len() + discovery.errors.len(), 1);
        assert_eq!(discovery.total_error_count, expected_error_count);
        assert!(
            discovery
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.display_path == canonical_root.join("sai.package"))
        );
        assert!(
            !discovery
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.display_path.starts_with(&first)
                    || diagnostic.display_path.starts_with(&second))
        );
    }

    #[test]
    fn invalid_declared_source_path_reports_a_safe_manifest_span() {
        let directory = TestDirectory::new();
        let root = directory.package("root", &manifest("acme:root", Some("../outside"), ""));

        let discovery = discover_fmt_workspace(
            &fs::canonicalize(&root).unwrap(),
            &CancellationToken::default(),
        );

        assert!(discovery.errors.is_empty());
        assert_eq!(discovery.diagnostics.len(), 1);
        assert_eq!(discovery.diagnostics[0].code, "INVALID_PACKAGE_PATH");
        assert_eq!(discovery.total_error_count, 1);
        assert_eq!(
            discovery.diagnostics[0].display_path,
            fs::canonicalize(&root).unwrap().join("sai.package")
        );
        assert!(discovery.diagnostics[0].span.is_some());
        assert!(
            discovery
                .input_paths
                .contains(&fs::canonicalize(root.join("sai.package")).unwrap())
        );
        assert_eq!(discovery.input_paths.len(), 1);
    }
}
