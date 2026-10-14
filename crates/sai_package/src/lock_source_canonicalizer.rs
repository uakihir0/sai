//! Snapshot-backed source identity canonicalization for complete lock graphs.
//!
//! Local lock entries are opened once relative to the captured root directory
//! and retained for condition-5 validation. Path pins resolve from the
//! captured root or from an indexed local requirer. Git-backed pins are
//! canonicalized from their locked URL, commit, and repository directory;
//! their requirers are never fetched.

use std::collections::BTreeMap;

use crate::local_source::{LocalPackageDirectory, LocalPackageError};
use crate::lock::{LockPackage, LockPackageField, LockParseOutput, LockSource};
use crate::lock_graph_validation::validate as validate_lock_graph;
use crate::lock_source_selection::{
    LockGitPathPinError, LockPackageSource, LockPathPin, LockSourceCanonicalizer as Canonicalizer,
    git_path_pin_source,
};
use crate::lock_validation::{canonical_git_url, validate as validate_lock};
use crate::manifest::PackageRole;
use crate::mvs::{MvsPackageVersion, MvsRequirer};
use crate::package_path::{DependencyPackagePath, PackagePathError, RepositoryPackageDirectory};
use crate::version::Version;

/// A canonical source identity suitable for complete-graph source comparison.
///
/// Local equality delegates to [`LocalPackageDirectory`], which compares
/// opened directory identities on supported Unix hosts. Git identity includes
/// the canonical URL, exact locked commit, and normalized repository path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSourceIdentity {
    Local(LocalPackageDirectory),
    Git {
        url: Box<str>,
        commit: Box<str>,
        directory: Option<Box<str>>,
    },
}

/// A local snapshot or bounded Git identity could not be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSourceCanonicalizationError {
    GitPathPin(LockGitPathPinError),
    InvalidGitUrl,
    MissingGitCommit,
    InvalidGitDirectory(PackagePathError),
    InvalidLockedLocalSourcePath {
        package_version: Box<MvsPackageVersion>,
        error: PackagePathError,
    },
    LockedLocalSourceDirectory {
        package_version: Box<MvsPackageVersion>,
        error: LocalPackageError,
    },
    LockedLocalSourceManifestInvalid(MvsPackageVersion),
    LockedLocalSourceVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    ConflictingLockedLocalSource(MvsPackageVersion),
    LockedLocalDirectoryHasMultipleVersions {
        existing: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    InvalidLocalPath(PackagePathError),
    LocalPathResolution(LocalPackageError),
    LocalPinManifestInvalid,
    LocalPinVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    DeclaredPinVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    InvalidPinBase,
    MissingLockedRequirer(MvsPackageVersion),
    LockedRequirerMismatch(MvsPackageVersion),
    MissingLocalEntry(MvsPackageVersion),
    LockedEntryMismatch(MvsPackageVersion),
}

/// A complete lock or root directory failed snapshot initialization.
///
/// Errors intentionally retain no local filesystem paths. Lock and manifest
/// diagnostics are represented by bounded categories rather than source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSourceCanonicalizerBuildError {
    InvalidLockSyntax,
    InvalidLockSemantics,
    InvalidLockGraph,
    MissingLockRoot,
    InvalidLockedIdentity(Box<str>),
    RootLockMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    RootManifestInvalid,
    RootManifestMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    MissingLockedPackageSource(MvsPackageVersion),
}

/// Canonicalizes lock source identities against one captured filesystem view.
///
/// Construct this with the same captured root directory used to load the root
/// manifest. Every local `source path` in the validated full lock is opened
/// relative to that directory exactly once; use [`Self::local_directory`] to
/// pass the verified snapshot to later condition-5 checks.
#[derive(Clone, Debug)]
pub struct LockSourceIdentityCanonicalizer {
    root_package_version: MvsPackageVersion,
    root_directory: LocalPackageDirectory,
    locked_entries: BTreeMap<MvsPackageVersion, LockPackageSource>,
    local_entries: BTreeMap<MvsPackageVersion, LocalPackageDirectory>,
}

impl LockSourceIdentityCanonicalizer {
    /// Validates a complete lock and retains its source records.
    ///
    /// The caller must pass the captured root directory, not reopen it by
    /// path. The lock is checked for syntax, local semantic validity, and
    /// complete graph consistency. Local entries are resolved lazily when a
    /// path pin or a pinned target requires their filesystem identity.
    pub fn new(
        root_directory: LocalPackageDirectory,
        root_package_version: MvsPackageVersion,
        parsed_lock: &LockParseOutput,
    ) -> Result<Self, LockSourceCanonicalizerBuildError> {
        if !parsed_lock.is_syntax_valid() {
            return Err(LockSourceCanonicalizerBuildError::InvalidLockSyntax);
        }
        if !validate_lock(parsed_lock).is_valid() {
            return Err(LockSourceCanonicalizerBuildError::InvalidLockSemantics);
        }
        let graph = validate_lock_graph(parsed_lock)
            .map_err(|_| LockSourceCanonicalizerBuildError::InvalidLockGraph)?;
        if !graph.is_consistent() {
            return Err(LockSourceCanonicalizerBuildError::InvalidLockGraph);
        }

        let root = parsed_lock
            .file()
            .root
            .as_ref()
            .ok_or(LockSourceCanonicalizerBuildError::MissingLockRoot)?;
        let locked_root = package_version(&root.value.package, &root.value.version)?;
        if locked_root != root_package_version {
            return Err(LockSourceCanonicalizerBuildError::RootLockMismatch {
                expected: Box::new(root_package_version),
                found: Box::new(locked_root),
            });
        }
        let found_root = manifest_package_version(&root_directory, PackageRole::Root)
            .ok_or(LockSourceCanonicalizerBuildError::RootManifestInvalid)?;
        if found_root != locked_root {
            return Err(LockSourceCanonicalizerBuildError::RootManifestMismatch {
                expected: Box::new(locked_root),
                found: Box::new(found_root),
            });
        }

        let mut locked_entries = BTreeMap::<MvsPackageVersion, LockPackageSource>::new();
        for package in &parsed_lock.file().packages {
            let package_version = package_version(&package.value.package, &package.value.version)?;
            let source =
                lock_package_source(&package.value, package_version.clone()).ok_or_else(|| {
                    LockSourceCanonicalizerBuildError::MissingLockedPackageSource(
                        package_version.clone(),
                    )
                })?;
            locked_entries.insert(package_version.clone(), source.clone());
        }

        Ok(Self {
            root_package_version,
            root_directory,
            locked_entries,
            local_entries: BTreeMap::new(),
        })
    }

    /// Returns the root directory snapshot captured by the caller.
    pub const fn root_directory(&self) -> &LocalPackageDirectory {
        &self.root_directory
    }

    /// Returns an indexed local package snapshot without reopening its path.
    pub fn local_directory(
        &self,
        package_version: &MvsPackageVersion,
    ) -> Option<&LocalPackageDirectory> {
        self.local_entries.get(package_version)
    }

    /// Returns the local snapshots indexed from lock entries.
    pub fn local_directories(
        &self,
    ) -> impl Iterator<Item = (&MvsPackageVersion, &LocalPackageDirectory)> {
        self.local_entries.iter()
    }

    /// Returns the exact package source record retained from the validated lock.
    pub fn locked_package_source(
        &self,
        package_version: &MvsPackageVersion,
    ) -> Option<&LockPackageSource> {
        self.locked_entries.get(package_version)
    }

    fn verified_local_base<'a>(
        &'a mut self,
        pin: &LockPathPin,
    ) -> Result<&'a LocalPackageDirectory, LockSourceCanonicalizationError> {
        match &pin.base.requirer {
            MvsRequirer::Root(version) => {
                if version != &self.root_package_version || pin.base.source.is_some() {
                    return Err(LockSourceCanonicalizationError::InvalidPinBase);
                }
                Ok(&self.root_directory)
            }
            MvsRequirer::Package(version) => {
                let Some(locked_source) = self.locked_entries.get(version).cloned() else {
                    return Err(LockSourceCanonicalizationError::MissingLockedRequirer(
                        version.clone(),
                    ));
                };
                if pin.base.source.as_ref() != Some(&locked_source) {
                    return Err(LockSourceCanonicalizationError::LockedRequirerMismatch(
                        version.clone(),
                    ));
                }
                if !matches!(locked_source.source, LockSource::Path(_)) {
                    return Err(LockSourceCanonicalizationError::InvalidPinBase);
                }
                self.local_entry_directory(version)
            }
        }
    }

    fn local_entry_directory(
        &mut self,
        package_version: &MvsPackageVersion,
    ) -> Result<&LocalPackageDirectory, LockSourceCanonicalizationError> {
        if self.local_entries.contains_key(package_version) {
            return Ok(self
                .local_entries
                .get(package_version)
                .expect("checked local entry"));
        }
        let Some(source) = self.locked_entries.get(package_version).cloned() else {
            return Err(LockSourceCanonicalizationError::MissingLocalEntry(
                package_version.clone(),
            ));
        };
        let LockSource::Path(path) = source.source else {
            return Err(LockSourceCanonicalizationError::MissingLocalEntry(
                package_version.clone(),
            ));
        };
        let relative_path = DependencyPackagePath::parse(&path).map_err(|error| {
            LockSourceCanonicalizationError::InvalidLockedLocalSourcePath {
                package_version: Box::new(package_version.clone()),
                error,
            }
        })?;
        let directory = self
            .root_directory
            .resolve_dependency(&relative_path)
            .map_err(
                |error| LockSourceCanonicalizationError::LockedLocalSourceDirectory {
                    package_version: Box::new(package_version.clone()),
                    error,
                },
            )?;
        let found =
            manifest_package_version(&directory, PackageRole::Dependency).ok_or_else(|| {
                LockSourceCanonicalizationError::LockedLocalSourceManifestInvalid(
                    package_version.clone(),
                )
            })?;
        if &found != package_version {
            return Err(
                LockSourceCanonicalizationError::LockedLocalSourceVersionMismatch {
                    expected: Box::new(package_version.clone()),
                    found: Box::new(found),
                },
            );
        }
        if let Some(existing) = self.local_entries.get(package_version) {
            if existing != &directory {
                return Err(
                    LockSourceCanonicalizationError::ConflictingLockedLocalSource(
                        package_version.clone(),
                    ),
                );
            }
        } else if let Some((existing_version, _)) = self
            .local_entries
            .iter()
            .find(|(_, existing_directory)| *existing_directory == &directory)
        {
            return Err(
                LockSourceCanonicalizationError::LockedLocalDirectoryHasMultipleVersions {
                    existing: Box::new(existing_version.clone()),
                    found: Box::new(package_version.clone()),
                },
            );
        }
        self.local_entries
            .insert(package_version.clone(), directory);
        Ok(self
            .local_entries
            .get(package_version)
            .expect("inserted local entry"))
    }

    fn validate_pin_requirer(
        &self,
        pin: &LockPathPin,
    ) -> Result<(), LockSourceCanonicalizationError> {
        match &pin.base.requirer {
            MvsRequirer::Root(version) => {
                if version == &self.root_package_version && pin.base.source.is_none() {
                    Ok(())
                } else {
                    Err(LockSourceCanonicalizationError::InvalidPinBase)
                }
            }
            MvsRequirer::Package(version) => {
                let Some(locked_source) = self.locked_entries.get(version) else {
                    return Err(LockSourceCanonicalizationError::MissingLockedRequirer(
                        version.clone(),
                    ));
                };
                if pin.base.source.as_ref() != Some(locked_source) {
                    return Err(LockSourceCanonicalizationError::LockedRequirerMismatch(
                        version.clone(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn canonicalize_git(
        &self,
        source: &LockPackageSource,
    ) -> Result<LockSourceIdentity, LockSourceCanonicalizationError> {
        let LockSource::Git(url) = &source.source else {
            return Err(LockSourceCanonicalizationError::InvalidPinBase);
        };
        let url =
            canonical_git_url(url).map_err(|_| LockSourceCanonicalizationError::InvalidGitUrl)?;
        let commit = source
            .commit
            .as_ref()
            .ok_or(LockSourceCanonicalizationError::MissingGitCommit)?;
        let directory = source
            .directory
            .as_deref()
            .filter(|directory| *directory != ".")
            .map(RepositoryPackageDirectory::parse)
            .transpose()
            .map_err(LockSourceCanonicalizationError::InvalidGitDirectory)?;
        Ok(LockSourceIdentity::Git {
            url: url.into(),
            commit: commit.clone(),
            directory: directory.map(|directory| directory.as_str().into()),
        })
    }
}

impl Canonicalizer for LockSourceIdentityCanonicalizer {
    type Key = LockSourceIdentity;
    type Error = LockSourceCanonicalizationError;

    fn canonicalize_pin(&mut self, pin: &LockPathPin) -> Result<Self::Key, Self::Error> {
        self.validate_pin_requirer(pin)?;
        if let Some(source) = git_path_pin_source(pin).map_err(Self::Error::GitPathPin)? {
            return self.canonicalize_git(&source);
        }

        let base = self.verified_local_base(pin)?;
        let path =
            DependencyPackagePath::parse(&pin.path).map_err(Self::Error::InvalidLocalPath)?;
        let directory = base
            .resolve_dependency(&path)
            .map_err(Self::Error::LocalPathResolution)?;
        let found = manifest_package_version(&directory, PackageRole::Dependency)
            .ok_or(Self::Error::LocalPinManifestInvalid)?;
        if found != pin.target {
            return Err(Self::Error::LocalPinVersionMismatch {
                expected: Box::new(pin.target.clone()),
                found: Box::new(found),
            });
        }
        if let Some(declared) = &pin.declared_version
            && declared != &found
        {
            return Err(Self::Error::DeclaredPinVersionMismatch {
                expected: Box::new(declared.clone()),
                found: Box::new(found),
            });
        }
        Ok(LockSourceIdentity::Local(directory))
    }

    fn canonicalize_entry(&mut self, entry: &LockPackageSource) -> Result<Self::Key, Self::Error> {
        if self.locked_entries.get(&entry.package_version) != Some(entry) {
            return Err(Self::Error::LockedEntryMismatch(
                entry.package_version.clone(),
            ));
        }
        match &entry.source {
            LockSource::Git(_) => self.canonicalize_git(entry),
            LockSource::Path(_) => self
                .local_entry_directory(&entry.package_version)
                .map(|directory| LockSourceIdentity::Local(directory.clone())),
        }
    }
}

fn package_version(
    package: &str,
    version: &Version,
) -> Result<MvsPackageVersion, LockSourceCanonicalizerBuildError> {
    MvsPackageVersion::new(package, version.clone())
        .map_err(|_| LockSourceCanonicalizerBuildError::InvalidLockedIdentity(package.into()))
}

fn manifest_package_version(
    directory: &LocalPackageDirectory,
    role: PackageRole,
) -> Option<MvsPackageVersion> {
    let parsed = directory.parse_manifest(role);
    if !parsed.is_syntax_valid() {
        return None;
    }
    let header = parsed.file().header.as_ref()?;
    MvsPackageVersion::new(
        header.value.identity.to_string(),
        header.value.version.clone(),
    )
    .ok()
}

fn lock_package_source(
    package: &LockPackage,
    package_version: MvsPackageVersion,
) -> Option<LockPackageSource> {
    let mut source = None;
    let mut directory = None;
    let mut commit = None;
    for field in &package.fields {
        match &field.value {
            LockPackageField::Source(value) => source = Some(value.clone()),
            LockPackageField::Directory(value) => directory = Some(value.clone()),
            LockPackageField::Commit(value) => commit = Some(value.clone()),
            _ => {}
        }
    }
    Some(LockPackageSource {
        package_version,
        source: source?,
        directory: directory.and_then(|value| (value.as_ref() != ".").then_some(value)),
        commit,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::lock::LockSource;
    use crate::lock::parse;
    use crate::lock_source_selection::{
        LockPackageSource, LockPathPin, LockPathPinBase, LockPathPinKind, LockSelectedSource,
        LockSourceCanonicalizer, LockSourcePlan, LockSourceSelectionCheckError,
        LockSourceSelectionReason, plan_lock_source_selection, validate_lock_source_plan,
    };
    use crate::manifest::{PackageRole, parse_for_role};
    use crate::mvs::{MvsPackageVersion, MvsRequirer};
    use crate::resolver_contract::ResolverContractContext;
    use crate::validation::PackageValidationContext;
    use crate::version::Version;

    use super::{
        LockSourceCanonicalizationError, LockSourceIdentity, LockSourceIdentityCanonicalizer,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    const COMMIT: &str = "0123456789012345678901234567890123456789";
    const CHECKSUM: &str = "sai1:0123456789012345678901234567890123456789012345678901234567890123";

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-lock-source-canonicalizer-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary package tree");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn package(&self, relative: &str, manifest: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create package directory");
            fs::write(path.join("sai.package"), manifest).expect("write package manifest");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn version(package: &str, value: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, Version::parse(value).expect("valid test version"))
            .expect("valid test package identity")
    }

    fn root_only_lock() -> String {
        "lock 2\n\
         root acme:root @ 1.0.0\n\
             targets { wasm32-core }\n"
            .into()
    }

    fn source_for(
        package_version: MvsPackageVersion,
        source: LockSource,
        directory: Option<&str>,
        commit: Option<&str>,
    ) -> LockPackageSource {
        LockPackageSource {
            package_version,
            source,
            directory: directory.map(Into::into),
            commit: commit.map(Into::into),
        }
    }

    fn pin(
        target: MvsPackageVersion,
        requirer: MvsRequirer,
        base_source: Option<LockPackageSource>,
        path: &str,
    ) -> LockPathPin {
        LockPathPin {
            target,
            declared_version: None,
            base: LockPathPinBase {
                requirer,
                source: base_source,
            },
            path: path.into(),
            kind: LockPathPinKind::Requirement,
            span: 0..0,
        }
    }

    fn local_root_manifest() -> &'static str {
        "package acme:root @ 1.0.0\n\
         source \"src\"\n\
         targets { wasm32-core }\n"
    }

    fn root_context() -> ResolverContractContext<'static> {
        let root = parse_for_role(local_root_manifest().as_bytes(), PackageRole::Root);
        let path_packages = BTreeMap::new();
        let path_overrides = BTreeMap::new();
        ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &path_overrides,
            PackageValidationContext::default(),
        )
        .expect("valid root context")
        .into_owned()
    }

    #[test]
    fn resolves_root_path_pin_from_captured_root_and_exposes_snapshot() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/dep",
            "package acme:dep @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(
            b"lock 2\n\
              root acme:root @ 1.0.0\n\
                  targets { wasm32-core }\n\
                  requires acme:dep @ ^1 -> 1.0.0 from path \"packages/dep\"\n\
              package acme:dep @ 1.0.0\n\
                  source path \"packages/dep\"\n",
        );
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let root = version("acme:root", "1.0.0");
        let dep = version("acme:dep", "1.0.0");
        let mut canonicalizer =
            LockSourceIdentityCanonicalizer::new(root_directory, root.clone(), &lock)
                .expect("initialize complete lock snapshots");
        let path_pin = pin(dep.clone(), MvsRequirer::Root(root), None, "packages/dep");
        let entry = source_for(
            dep.clone(),
            LockSource::Path("packages/dep".into()),
            None,
            None,
        );

        let pin_identity = canonicalizer
            .canonicalize_pin(&path_pin)
            .expect("canonicalize root path pin");
        let entry_identity = canonicalizer
            .canonicalize_entry(&entry)
            .expect("canonicalize indexed entry");
        assert_eq!(pin_identity, entry_identity);
        assert!(canonicalizer.local_directory(&dep).is_some());
    }

    #[test]
    fn resolves_dependency_relative_pin_from_indexed_local_requirer() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/parent",
            "package acme:parent @ 1.0.0\n\
             source \"src\"\n",
        );
        tree.package(
            "root/packages/child",
            "package acme:child @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(
            b"lock 2\n\
              root acme:root @ 1.0.0\n\
                  targets { wasm32-core }\n\
                  requires acme:parent @ ^1 -> 1.0.0 from path \"packages/parent\"\n\
              package acme:parent @ 1.0.0\n\
                  source path \"packages/parent\"\n\
                  requires acme:child @ ^1 -> 1.0.0 from path \"../child\"\n\
              package acme:child @ 1.0.0\n\
                  source path \"packages/child\"\n",
        );
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let parent = version("acme:parent", "1.0.0");
        let child = version("acme:child", "1.0.0");
        let mut canonicalizer = LockSourceIdentityCanonicalizer::new(
            root_directory,
            version("acme:root", "1.0.0"),
            &lock,
        )
        .expect("initialize complete lock snapshots");
        let parent_source = source_for(
            parent.clone(),
            LockSource::Path("packages/parent".into()),
            None,
            None,
        );
        let path_pin = pin(
            child.clone(),
            MvsRequirer::Package(parent),
            Some(parent_source),
            "../child",
        );
        let entry = source_for(
            child.clone(),
            LockSource::Path("packages/child".into()),
            None,
            None,
        );

        let pin_identity = canonicalizer
            .canonicalize_pin(&path_pin)
            .expect("canonicalize package-relative pin");
        let entry_identity = canonicalizer
            .canonicalize_entry(&entry)
            .expect("canonicalize indexed child entry");
        assert_eq!(pin_identity, entry_identity);
        assert!(canonicalizer.local_directory(&child).is_some());
    }

    #[test]
    fn normalizes_git_sibling_identity_without_loading_the_requirer() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/mono",
            "package acme:mono @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(
            format!(
                "lock 2\n\
                 root acme:root @ 1.0.0\n\
                     targets {{ wasm32-core }}\n\
                     requires acme:mono @ ^1 -> 1.0.0 from git \"https://example.invalid/mono\"\n\
                 package acme:mono @ 1.0.0\n\
                     source git \"https://example.invalid/mono\"\n\
                     commit \"{COMMIT}\"\n\
                     checksum \"{CHECKSUM}\"\n\
                     capabilities {{ }}\n\
                     dir \"packages/mono\"\n\
                     requires acme:sibling @ ^1 -> 1.0.0 from path \"../sibling\"\n\
                 package acme:sibling @ 1.0.0\n\
                     source git \"https://example.invalid/mono\"\n\
                     commit \"{COMMIT}\"\n\
                     checksum \"{CHECKSUM}\"\n\
                     capabilities {{ }}\n\
                     dir \"packages/sibling\"\n"
            )
            .as_bytes(),
        );
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let mono = version("acme:mono", "1.0.0");
        let sibling = version("acme:sibling", "1.0.0");
        let mut canonicalizer = LockSourceIdentityCanonicalizer::new(
            root_directory,
            version("acme:root", "1.0.0"),
            &lock,
        )
        .expect("initialize complete lock snapshots");
        let mono_source = source_for(
            mono.clone(),
            LockSource::Git("https://example.invalid/mono".into()),
            Some("packages/mono"),
            Some(COMMIT),
        );
        let path_pin = pin(
            sibling.clone(),
            MvsRequirer::Package(mono),
            Some(mono_source),
            "../sibling",
        );
        let entry = source_for(
            sibling,
            LockSource::Git("https://example.invalid/mono".into()),
            Some("packages/sibling"),
            Some(COMMIT),
        );

        assert_eq!(
            canonicalizer
                .canonicalize_pin(&path_pin)
                .expect("canonicalize locked Git sibling pin"),
            canonicalizer
                .canonicalize_entry(&entry)
                .expect("canonicalize locked Git entry")
        );
        assert!(matches!(
            canonicalizer
                .canonicalize_pin(&path_pin)
                .expect("Git identity is local metadata only"),
            LockSourceIdentity::Git {
                url,
                commit,
                directory: Some(directory),
            } if url.as_ref() == "https://example.invalid/mono"
                && commit.as_ref() == COMMIT
                && directory.as_ref() == "packages/sibling"
        ));
    }

    #[test]
    fn rejects_a_local_pin_that_resolves_to_a_different_directory() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/dep",
            "package acme:dep @ 1.0.0\n\
             source \"src\"\n",
        );
        tree.package(
            "root/packages/alternate",
            "package acme:dep @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(
            b"lock 2\n\
              root acme:root @ 1.0.0\n\
                  targets { wasm32-core }\n\
                  requires acme:dep @ ^1 -> 1.0.0 from path \"packages/dep\"\n\
              package acme:dep @ 1.0.0\n\
                  source path \"packages/dep\"\n",
        );
        let root = version("acme:root", "1.0.0");
        let dep = version("acme:dep", "1.0.0");
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let mut canonicalizer =
            LockSourceIdentityCanonicalizer::new(root_directory, root.clone(), &lock)
                .expect("initialize complete lock snapshots");
        let path_pin = pin(
            dep.clone(),
            MvsRequirer::Root(root),
            None,
            "packages/alternate",
        );
        let entry = source_for(
            dep.clone(),
            LockSource::Path("packages/dep".into()),
            None,
            None,
        );
        let plan = LockSourcePlan {
            package_version: dep,
            reason: LockSourceSelectionReason::PathPin,
            selected: LockSelectedSource::PathPins(vec![path_pin.clone()]),
            path_pins: vec![path_pin],
            actual: entry,
        };

        assert!(matches!(
            validate_lock_source_plan(&plan, &mut canonicalizer),
            Err(crate::lock_source_selection::LockSourceSelectionCheckError::SourceMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_path_pin_for_a_different_declared_package_version() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/dep",
            "package acme:other @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(root_only_lock().as_bytes());
        let root = version("acme:root", "1.0.0");
        let dep = version("acme:dep", "1.0.0");
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let mut canonicalizer =
            LockSourceIdentityCanonicalizer::new(root_directory, root.clone(), &lock)
                .expect("initialize root-only lock snapshot");

        assert!(matches!(
            canonicalizer.canonicalize_pin(&pin(
                dep.clone(),
                MvsRequirer::Root(root),
                None,
                "packages/dep",
            )),
            Err(LockSourceCanonicalizationError::LocalPinVersionMismatch {
                expected,
                found,
            }) if *expected == dep && found.package() == "acme:other"
        ));
    }

    #[test]
    fn does_not_open_an_unused_lock_floor_path_source() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        let lock = parse(
            b"lock 2\n\
              root acme:root @ 1.0.0\n\
                  targets { wasm32-core }\n\
                  requires acme:dep @ ^1 -> 1.2.0 from git \"https://example.invalid/dep\"\n\
              package acme:dep @ 1.2.0\n\
                  source path \"missing/dep\"\n",
        );
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let dep = version("acme:dep", "1.2.0");
        let mut canonicalizer = LockSourceIdentityCanonicalizer::new(
            root_directory,
            version("acme:root", "1.0.0"),
            &lock,
        )
        .expect("retain lock records without opening unselected local source");

        let plans = plan_lock_source_selection(&lock, &root_context())
            .expect("build complete-graph source plan");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].reason, LockSourceSelectionReason::LockFloor);
        validate_lock_source_plan(&plans[0], &mut canonicalizer)
            .expect("lock-floor source needs no path canonicalization");
        assert!(canonicalizer.local_directory(&dep).is_none());
    }

    #[test]
    fn opens_a_required_local_target_lazily_and_fails_if_the_locked_source_is_missing() {
        let tree = TempTree::new();
        tree.package("root", local_root_manifest());
        tree.package(
            "root/packages/dep",
            "package acme:dep @ 1.0.0\n\
             source \"src\"\n",
        );
        let lock = parse(
            b"lock 2\n\
              root acme:root @ 1.0.0\n\
                  targets { wasm32-core }\n\
                  requires acme:dep @ ^1 -> 1.0.0 from path \"packages/dep\"\n\
              package acme:dep @ 1.0.0\n\
                  source path \"missing/dep\"\n",
        );
        let root_directory =
            crate::local_source::LocalPackageDirectory::open(tree.path().join("root"))
                .expect("open root snapshot");
        let mut canonicalizer = LockSourceIdentityCanonicalizer::new(
            root_directory,
            version("acme:root", "1.0.0"),
            &lock,
        )
        .expect("retain lock records before resolving pinned sources");
        let plans = plan_lock_source_selection(&lock, &root_context())
            .expect("build complete-graph source plan");
        assert_eq!(plans[0].reason, LockSourceSelectionReason::PathPin);

        assert!(matches!(
            validate_lock_source_plan(&plans[0], &mut canonicalizer),
            Err(LockSourceSelectionCheckError::EntryCanonicalization { error, .. })
                if matches!(
                    *error,
                    LockSourceCanonicalizationError::LockedLocalSourceDirectory { .. }
                )
        ));
    }
}
