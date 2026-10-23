//! Package-context checks for decoded index-operation journals.
//!
//! The validator operates on contexts assembled from captured package
//! directories. The inventory builder reads the complete lock graph before
//! target activation and retains directory capabilities for its workspace
//! packages. Callers must hold the exclusive workspace lock while building
//! the inventory. Recovery must still revalidate roots and traverse every
//! path component relative to open directory handles immediately before I/O.

#[cfg(test)]
use crate::package_graph::PackageGraphNode;
use crate::package_graph::{PackageForm, PackageGraphRole};
use sai_package::local_source::{
    LocalPackageDirectory, LocalPackageDirectoryIndex, LocalPackagePathIndexError,
};
use sai_package::lock::{LockParseOutput, LockSource};
use sai_package::lock_dependency_contract::{
    LockDependencyContractInputError, LockDependencyContractMismatch,
};
use sai_package::lock_root_contract::{LockRootContractInputError, LockRootContractMismatch};
use sai_package::lock_source_canonicalizer::{
    LockSourceCanonicalizationError, LockSourceCanonicalizerBuildError,
    LockSourceIdentityCanonicalizer,
};
use sai_package::lock_source_selection::{
    LockSourceCanonicalizer, LockSourceSelectionCheckError, LockSourceSelectionInputError,
    plan_lock_source_selection, validate_lock_source_plan,
};
use sai_package::manifest::{PackageDeclaration, PackageRole};
use sai_package::mvs::MvsPackageVersion;
use sai_package::package_path::{ContentPackagePath, RepositoryPackageDirectory};
use sai_package::resolver_contract::{ResolverContractBuildError, ResolverContractContext};
use sai_package::validation::PackageValidationContext;
use sai_package::version::Version;
use sai_semantic::{IndexOperationJournal, PackageIndexFileName};
use sai_vfs::{PackageRelativePath, PackageSourceDirectory, PackageSourcePath};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path};

/// Current metadata for one exact package that could appear in a journal.
///
/// `package_root` is the current workspace-relative root, not a value copied
/// from the journal. Dependency nodes have no workspace-relative root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexJournalPackageContext {
    package_node_key: String,
    package_root: Option<String>,
    source_directory: Option<PackageSourceDirectory>,
    index_file: PackageIndexFileName,
    form: PackageForm,
    role: PackageGraphRole,
}

impl IndexJournalPackageContext {
    /// Builds a context from a fresh workspace inventory role and captured
    /// package directories.
    ///
    /// The package's exact identity and version, form, and source directory
    /// are read from its captured manifest. The role comes from the caller's
    /// fresh workspace inventory because inactive workspace members may not
    /// be represented by the target-specific active package graph. Root and
    /// member paths are derived relative to the captured workspace-root
    /// directory; the caller must supply only packages established as
    /// members by fresh workspace discovery. Containment alone does not prove
    /// workspace membership.
    pub fn from_workspace_package(
        role: PackageGraphRole,
        workspace_root: &LocalPackageDirectory,
        package_directory: &LocalPackageDirectory,
    ) -> Result<Self, IndexOperationJournalValidationError> {
        let package_root = match role {
            PackageGraphRole::Root if package_directory.path() == workspace_root.path() => {
                Some(".".to_owned())
            }
            PackageGraphRole::Root => {
                return Err(IndexOperationJournalValidationError::InvalidPackageRoot);
            }
            PackageGraphRole::WorkspaceMember => {
                let package_root = workspace_relative_package_root(
                    workspace_root.path(),
                    package_directory.path(),
                )?;
                if package_root == "." {
                    return Err(IndexOperationJournalValidationError::InvalidPackageRoot);
                }
                Some(package_root)
            }
            PackageGraphRole::Dependency => None,
        };

        let parse_role = match role {
            PackageGraphRole::Root => PackageRole::Root,
            PackageGraphRole::WorkspaceMember | PackageGraphRole::Dependency => {
                PackageRole::Dependency
            }
        };
        let manifest = package_directory.parse_manifest(parse_role);
        if manifest.lexical_error_count() != 0 || manifest.error_count() != 0 {
            return Err(IndexOperationJournalValidationError::InvalidPackageManifest);
        }
        let header = manifest
            .file()
            .header
            .as_ref()
            .ok_or(IndexOperationJournalValidationError::InvalidPackageManifest)?;
        let identity = header.value.identity.to_string();
        let version = &header.value.version;
        let mut source_directories =
            manifest
                .file()
                .declarations
                .iter()
                .filter_map(|declaration| match &declaration.value {
                    PackageDeclaration::Source(source) => Some(source.as_ref()),
                    _ => None,
                });
        let source_directory = source_directories.next();
        if source_directories.next().is_some() {
            return Err(IndexOperationJournalValidationError::InvalidPackageManifest);
        }
        let source_directory = source_directory
            .map(|source| {
                ContentPackagePath::parse(source)
                    .map_err(|_| IndexOperationJournalValidationError::InvalidSourceDirectory)?;
                PackageSourceDirectory::new(source.to_owned())
                    .map_err(|_| IndexOperationJournalValidationError::InvalidSourceDirectory)
            })
            .transpose()?;
        let form = if source_directory.is_some() {
            PackageForm::Source
        } else {
            PackageForm::Artifact
        };
        Self::from_verified_metadata(
            &identity,
            version,
            form,
            role,
            package_root,
            source_directory,
        )
    }

    fn from_verified_metadata(
        identity: &str,
        version: &Version,
        form: PackageForm,
        role: PackageGraphRole,
        package_root: Option<String>,
        source_directory: Option<PackageSourceDirectory>,
    ) -> Result<Self, IndexOperationJournalValidationError> {
        match role {
            PackageGraphRole::Root if package_root.as_deref() == Some(".") => {}
            PackageGraphRole::WorkspaceMember
                if package_root.as_deref().is_some_and(|root| root != ".") =>
            {
                RepositoryPackageDirectory::parse(
                    package_root
                        .as_deref()
                        .expect("checked workspace member root"),
                )
                .map_err(|_| IndexOperationJournalValidationError::InvalidPackageRoot)?;
            }
            PackageGraphRole::Dependency if package_root.is_none() => {}
            _ => return Err(IndexOperationJournalValidationError::InvalidPackageRoot),
        }

        let (namespace, package) = identity
            .split_once(':')
            .ok_or(IndexOperationJournalValidationError::InvalidPackageIndexFile)?;
        let compatibility_line = version.compatibility_line().to_string();
        let index_file = PackageIndexFileName::new(namespace, package, &compatibility_line)
            .map_err(|_| IndexOperationJournalValidationError::InvalidPackageIndexFile)?;

        Ok(Self {
            package_node_key: format!("package:{identity}@{version}"),
            package_root,
            source_directory,
            index_file,
            form,
            role,
        })
    }

    #[cfg(test)]
    fn from_test_metadata(
        node: &PackageGraphNode,
        package_root: Option<&str>,
        source_directory: Option<&str>,
    ) -> Result<Self, IndexOperationJournalValidationError> {
        let package_root = package_root.map(str::to_owned);
        let source_directory = source_directory
            .map(|source| {
                ContentPackagePath::parse(source)
                    .map_err(|_| IndexOperationJournalValidationError::InvalidSourceDirectory)?;
                PackageSourceDirectory::new(source.to_owned())
                    .map_err(|_| IndexOperationJournalValidationError::InvalidSourceDirectory)
            })
            .transpose()?;
        Self::from_verified_metadata(
            node.identity(),
            node.version(),
            node.form(),
            node.role(),
            package_root,
            source_directory,
        )
    }

    /// Returns the full identity-and-version key used by journal records.
    pub fn package_node_key(&self) -> &str {
        &self.package_node_key
    }

    /// Returns the current workspace-relative package root, if this is a
    /// workspace package.
    pub fn package_root(&self) -> Option<&str> {
        self.package_root.as_deref()
    }

    /// Returns the manifest-declared source directory, if one exists.
    pub fn source_directory(&self) -> Option<&PackageSourceDirectory> {
        self.source_directory.as_ref()
    }

    /// Returns the package's compatibility-line index filename.
    pub fn index_file(&self) -> &PackageIndexFileName {
        &self.index_file
    }
}

/// A mismatch between a decoded journal and freshly collected package
/// metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexOperationJournalValidationError {
    InvalidPackageRoot,
    InvalidSourceDirectory,
    InvalidPackageIndexFile,
    InvalidPackageManifest,
    PackageDirectoryOutsideWorkspace,
    DuplicatePackageContext {
        package_node_key: String,
    },
    DuplicatePackageIndexFileContext {
        file: String,
    },
    PlannedIndexPackageSetMismatch,
    UnknownPackageNode {
        package_node_key: String,
    },
    PackageNotIndexable {
        package_node_key: String,
    },
    PackageRootMismatch {
        package_node_key: String,
        recorded_root: String,
        current_root: Option<String>,
    },
    MissingSourceDirectory {
        package_node_key: String,
    },
    InvalidSourcePath {
        path: String,
    },
    SourceOutsideDeclaredDirectory {
        package_node_key: String,
        path: String,
        source_directory: String,
    },
    InvalidPlannedIndexHeader(sai_semantic::IndexJournalCodecError),
    PackageMissingFromPlannedIndex {
        package_node_key: String,
        file: String,
    },
}

impl fmt::Display for IndexOperationJournalValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "index operation journal package validation failed: {self:?}"
        )
    }
}

impl Error for IndexOperationJournalValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPlannedIndexHeader(source) => Some(source),
            _ => None,
        }
    }
}

/// A complete, target-independent inventory for journal package validation.
///
/// `contexts` contains the root and every workspace package selected by a
/// local path source anywhere in the lock graph. `directories` retains the
/// opened directory capability for each corresponding full package key, so a
/// later recovery coordinator can use handle-relative operations. External
/// path dependencies and Git packages are deliberately not workspace
/// members.
#[derive(Clone, Debug)]
pub struct IndexJournalWorkspaceInventory {
    root_directory: LocalPackageDirectory,
    contexts: Vec<IndexJournalPackageContext>,
    directories: BTreeMap<String, LocalPackageDirectory>,
}

impl IndexJournalWorkspaceInventory {
    /// Returns the captured root directory used to build this inventory.
    pub const fn root_directory(&self) -> &LocalPackageDirectory {
        &self.root_directory
    }

    /// Returns the root and all lock-selected workspace package contexts,
    /// sorted by their full identity-and-version key.
    pub fn contexts(&self) -> &[IndexJournalPackageContext] {
        &self.contexts
    }

    /// Returns the retained directory for a full journal package key.
    pub fn directory_for_package(&self, package_node_key: &str) -> Option<&LocalPackageDirectory> {
        self.directories.get(package_node_key)
    }

    /// Iterates over retained root and workspace-member directory handles in
    /// deterministic package-key order.
    pub fn directories(&self) -> impl Iterator<Item = (&str, &LocalPackageDirectory)> {
        self.directories
            .iter()
            .map(|(package_node_key, directory)| (package_node_key.as_str(), directory))
    }
}

/// A lock, source, or captured package could not produce a complete workspace
/// inventory. The builder never returns a partially collected inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexJournalWorkspaceInventoryBuildError {
    RootPackageIndex(LocalPackagePathIndexError),
    RootPathOverrides(LocalPackagePathIndexError),
    RootContext(ResolverContractBuildError),
    RootContractInput(LockRootContractInputError),
    RootContractMismatch(Vec<LockRootContractMismatch>),
    SourceCanonicalizer(LockSourceCanonicalizerBuildError),
    SourcePlanning(LockSourceSelectionInputError),
    SourceValidation {
        package_version: MvsPackageVersion,
        error: Box<LockSourceSelectionCheckError<LockSourceCanonicalizationError>>,
    },
    LocalSourceInventoryMismatch {
        missing: Vec<MvsPackageVersion>,
        unexpected: Vec<MvsPackageVersion>,
    },
    PackageContext {
        package_version: MvsPackageVersion,
        error: Box<IndexOperationJournalValidationError>,
    },
    MissingLockPackageContract {
        package_version: MvsPackageVersion,
    },
    PackageContractInput {
        package_version: MvsPackageVersion,
        error: LockDependencyContractInputError,
    },
    PackageContractMismatch {
        package_version: MvsPackageVersion,
        mismatches: Vec<LockDependencyContractMismatch>,
    },
    PackageKeyMismatch {
        expected: String,
        found: String,
    },
    DuplicatePackageContext {
        package_node_key: String,
    },
    DuplicateWorkspaceIndexFile {
        file: String,
    },
}

impl fmt::Display for IndexJournalWorkspaceInventoryBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not build complete index journal workspace inventory: {self:?}"
        )
    }
}

impl Error for IndexJournalWorkspaceInventoryBuildError {}

/// Builds the authoritative root and workspace-member contexts from a
/// complete lock graph without applying target or test-edge activation.
///
/// This must run while the caller holds the exclusive workspace lock. The
/// root's path overrides are derived from its captured manifest; source
/// selection and all package membership decisions then come from every
/// recorded lock package, including test-only and target-inactive edges.
/// Every local lock entry is canonicalized before it is classified. Only
/// entries whose captured directory is beneath the captured workspace root
/// become workspace members. On any incomplete evidence or validation error,
/// the function returns an error and exposes no partial inventory.
pub fn build_index_journal_workspace_inventory(
    root_directory: &LocalPackageDirectory,
    parsed_lock: &LockParseOutput,
    validation_context: PackageValidationContext<'_>,
) -> Result<IndexJournalWorkspaceInventory, IndexJournalWorkspaceInventoryBuildError> {
    let root_manifest = root_directory.parse_manifest(PackageRole::Root);
    let mut root_directory_index = LocalPackageDirectoryIndex::default();
    let root_package_version = root_directory_index
        .index_root_directory(root_directory)
        .map_err(IndexJournalWorkspaceInventoryBuildError::RootPackageIndex)?;

    let root_contract = sai_package::lock_root_contract::compare(&root_manifest, parsed_lock)
        .map_err(IndexJournalWorkspaceInventoryBuildError::RootContractInput)?;
    if !root_contract.matches_contract() {
        return Err(
            IndexJournalWorkspaceInventoryBuildError::RootContractMismatch(
                root_contract.mismatches,
            ),
        );
    }
    // Initialize the full lock snapshot before following manifest override
    // paths. This rejects malformed lock state before resolving any
    // root-manifest path declarations.
    let mut source_canonicalizer = LockSourceIdentityCanonicalizer::new(
        root_directory.clone(),
        root_package_version.clone(),
        parsed_lock,
    )
    .map_err(IndexJournalWorkspaceInventoryBuildError::SourceCanonicalizer)?;

    let root_path_overrides = root_directory_index
        .index_root_path_overrides(root_directory)
        .map_err(IndexJournalWorkspaceInventoryBuildError::RootPathOverrides)?;
    let no_root_path_requirements = BTreeMap::new();
    let resolver_context = ResolverContractContext::from_root_manifest(
        &root_manifest,
        &no_root_path_requirements,
        &root_path_overrides,
        validation_context,
    )
    .map_err(IndexJournalWorkspaceInventoryBuildError::RootContext)?;
    if resolver_context.root_package() != &root_package_version {
        return Err(
            IndexJournalWorkspaceInventoryBuildError::PackageKeyMismatch {
                expected: package_node_key(&root_package_version),
                found: package_node_key(resolver_context.root_package()),
            },
        );
    }

    let source_plans = plan_lock_source_selection(parsed_lock, &resolver_context)
        .map_err(IndexJournalWorkspaceInventoryBuildError::SourcePlanning)?;
    for plan in &source_plans {
        validate_lock_source_plan(plan, &mut source_canonicalizer).map_err(|error| {
            IndexJournalWorkspaceInventoryBuildError::SourceValidation {
                package_version: plan.package_version.clone(),
                error: Box::new(error),
            }
        })?;

        // LockFloor deliberately performs no identity lookup during source
        // selection. Materialize every actual local entry explicitly so
        // condition 5 and workspace membership see the complete lock graph.
        if matches!(plan.actual.source, LockSource::Path(_)) {
            source_canonicalizer
                .canonicalize_entry(&plan.actual)
                .map_err(
                    |error| IndexJournalWorkspaceInventoryBuildError::SourceValidation {
                        package_version: plan.package_version.clone(),
                        error: Box::new(LockSourceSelectionCheckError::EntryCanonicalization {
                            entry: Box::new(plan.actual.clone()),
                            error: Box::new(error),
                        }),
                    },
                )?;
        }
    }

    let expected_local_sources = source_plans
        .iter()
        .filter(|plan| matches!(plan.actual.source, LockSource::Path(_)))
        .map(|plan| plan.package_version.clone())
        .collect::<BTreeSet<_>>();
    let materialized_local_sources = source_canonicalizer
        .local_directories()
        .map(|(package_version, _)| package_version.clone())
        .collect::<BTreeSet<_>>();
    if expected_local_sources != materialized_local_sources {
        return Err(
            IndexJournalWorkspaceInventoryBuildError::LocalSourceInventoryMismatch {
                missing: expected_local_sources
                    .difference(&materialized_local_sources)
                    .cloned()
                    .collect(),
                unexpected: materialized_local_sources
                    .difference(&expected_local_sources)
                    .cloned()
                    .collect(),
            },
        );
    }

    for (package_version, package_directory) in source_canonicalizer.local_directories() {
        let Some(locked_package) = parsed_lock.file().packages.iter().find(|package| {
            package.value.package.as_ref() == package_version.package()
                && &package.value.version == package_version.version()
        }) else {
            return Err(
                IndexJournalWorkspaceInventoryBuildError::MissingLockPackageContract {
                    package_version: package_version.clone(),
                },
            );
        };
        let package_manifest = package_directory.parse_manifest(PackageRole::Dependency);
        let contract = sai_package::lock_dependency_contract::compare(
            &package_manifest,
            locked_package,
            resolver_context.root_targets(),
        )
        .map_err(|error| {
            IndexJournalWorkspaceInventoryBuildError::PackageContractInput {
                package_version: package_version.clone(),
                error,
            }
        })?;
        if !contract.matches_contract() {
            return Err(
                IndexJournalWorkspaceInventoryBuildError::PackageContractMismatch {
                    package_version: package_version.clone(),
                    mismatches: contract.mismatches,
                },
            );
        }
    }

    let mut contexts = Vec::with_capacity(materialized_local_sources.len() + 1);
    let mut directories = BTreeMap::new();
    let root_context = IndexJournalPackageContext::from_workspace_package(
        PackageGraphRole::Root,
        root_directory,
        root_directory,
    )
    .map_err(
        |error| IndexJournalWorkspaceInventoryBuildError::PackageContext {
            package_version: root_package_version.clone(),
            error: Box::new(error),
        },
    )?;
    require_package_context_key(&root_context, &root_package_version)?;
    directories.insert(
        root_context.package_node_key.clone(),
        root_directory.clone(),
    );
    contexts.push(root_context);

    for (package_version, package_directory) in source_canonicalizer.local_directories() {
        if package_directory.path() == root_directory.path() {
            // The root may not also appear as a dependency package entry.
            return Err(IndexJournalWorkspaceInventoryBuildError::PackageContext {
                package_version: package_version.clone(),
                error: Box::new(
                    IndexOperationJournalValidationError::DuplicatePackageContext {
                        package_node_key: package_node_key(package_version),
                    },
                ),
            });
        }
        if !package_directory.path().starts_with(root_directory.path()) {
            // A lock-selected local package may live outside the workspace.
            // Its source identity was validated above, but it is not a
            // workspace package and cannot participate in the index header.
            continue;
        }

        let context = IndexJournalPackageContext::from_workspace_package(
            PackageGraphRole::WorkspaceMember,
            root_directory,
            package_directory,
        )
        .map_err(
            |error| IndexJournalWorkspaceInventoryBuildError::PackageContext {
                package_version: package_version.clone(),
                error: Box::new(error),
            },
        )?;
        require_package_context_key(&context, package_version)?;
        if directories
            .insert(context.package_node_key.clone(), package_directory.clone())
            .is_some()
        {
            return Err(
                IndexJournalWorkspaceInventoryBuildError::DuplicatePackageContext {
                    package_node_key: context.package_node_key,
                },
            );
        }
        contexts.push(context);
    }

    contexts.sort_by(|left, right| left.package_node_key.cmp(&right.package_node_key));
    validate_unique_inventory_contexts(&contexts)?;

    Ok(IndexJournalWorkspaceInventory {
        root_directory: root_directory.clone(),
        contexts,
        directories,
    })
}

fn package_node_key(package_version: &MvsPackageVersion) -> String {
    format!(
        "package:{}@{}",
        package_version.package(),
        package_version.version()
    )
}

fn require_package_context_key(
    context: &IndexJournalPackageContext,
    expected_package_version: &MvsPackageVersion,
) -> Result<(), IndexJournalWorkspaceInventoryBuildError> {
    let expected = package_node_key(expected_package_version);
    if context.package_node_key() == expected {
        return Ok(());
    }
    Err(
        IndexJournalWorkspaceInventoryBuildError::PackageKeyMismatch {
            expected,
            found: context.package_node_key().to_owned(),
        },
    )
}

fn validate_unique_inventory_contexts(
    contexts: &[IndexJournalPackageContext],
) -> Result<(), IndexJournalWorkspaceInventoryBuildError> {
    let mut package_keys = BTreeSet::new();
    let mut index_files = BTreeMap::<&str, &str>::new();
    for context in contexts {
        if !package_keys.insert(context.package_node_key()) {
            return Err(
                IndexJournalWorkspaceInventoryBuildError::DuplicatePackageContext {
                    package_node_key: context.package_node_key().to_owned(),
                },
            );
        }
        if context.package_root().is_some()
            && let Some(previous_package_key) =
                index_files.insert(context.index_file().as_str(), context.package_node_key())
            && previous_package_key != context.package_node_key()
        {
            return Err(
                IndexJournalWorkspaceInventoryBuildError::DuplicateWorkspaceIndexFile {
                    file: context.index_file().as_str().to_owned(),
                },
            );
        }
    }
    Ok(())
}

/// Validates all journal package keys, roots, source paths, and planned index
/// membership against freshly discovered package metadata.
///
/// The contexts must be complete and current for the workspace. This check
/// does not authorize filesystem access by itself: recovery must repeat the
/// root check and perform handle-relative no-follow traversal immediately
/// before each read, replace, or remove.
pub fn validate_index_operation_journal_package_context(
    journal: &IndexOperationJournal,
    package_contexts: &[IndexJournalPackageContext],
) -> Result<(), IndexOperationJournalValidationError> {
    let mut contexts_by_key = BTreeMap::new();
    for context in package_contexts {
        if contexts_by_key
            .insert(context.package_node_key.as_str(), context)
            .is_some()
        {
            return Err(
                IndexOperationJournalValidationError::DuplicatePackageContext {
                    package_node_key: context.package_node_key.clone(),
                },
            );
        }
    }

    let mut workspace_index_files_by_name = BTreeMap::new();
    let mut current_workspace_index_files = BTreeSet::new();
    for context in package_contexts
        .iter()
        .filter(|context| context.package_root.is_some())
    {
        if let Some(previous_context) =
            workspace_index_files_by_name.insert(&context.index_file, context)
            && previous_context.package_node_key != context.package_node_key
        {
            return Err(
                IndexOperationJournalValidationError::DuplicatePackageIndexFileContext {
                    file: context.index_file.as_str().to_owned(),
                },
            );
        }
        current_workspace_index_files.insert(context.index_file.clone());
    }

    let planned_index_files = journal
        .planned_index_package_files()
        .map_err(IndexOperationJournalValidationError::InvalidPlannedIndexHeader)?
        .into_iter()
        .collect::<BTreeSet<_>>();

    for source in &journal.sources {
        let context = find_indexable_context(&contexts_by_key, &source.package_node_key)?;
        if context.package_root.as_deref() != Some(source.package_root.as_str()) {
            return Err(IndexOperationJournalValidationError::PackageRootMismatch {
                package_node_key: source.package_node_key.clone(),
                recorded_root: source.package_root.clone(),
                current_root: context.package_root.clone(),
            });
        }

        let Some(source_directory) = context.source_directory.as_ref() else {
            return Err(
                IndexOperationJournalValidationError::MissingSourceDirectory {
                    package_node_key: source.package_node_key.clone(),
                },
            );
        };

        validate_recorded_source_path(&source.package_node_key, &source.path, source_directory)?;
        require_planned_index_file(context, &source.package_node_key, &planned_index_files)?;
    }

    for mapping in &journal.symbol_mappings {
        let context = find_indexable_context(&contexts_by_key, &mapping.package_node_key)?;
        require_planned_index_file(context, &mapping.package_node_key, &planned_index_files)?;
    }

    if current_workspace_index_files != planned_index_files {
        return Err(IndexOperationJournalValidationError::PlannedIndexPackageSetMismatch);
    }

    Ok(())
}

fn workspace_relative_package_root(
    workspace_root: &Path,
    package_directory: &Path,
) -> Result<String, IndexOperationJournalValidationError> {
    if package_directory == workspace_root {
        return Ok(".".to_owned());
    }

    let relative = package_directory
        .strip_prefix(workspace_root)
        .map_err(|_| IndexOperationJournalValidationError::PackageDirectoryOutsideWorkspace)?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(IndexOperationJournalValidationError::InvalidPackageRoot);
        };
        let component = component
            .to_str()
            .ok_or(IndexOperationJournalValidationError::InvalidPackageRoot)?;
        components.push(component);
    }
    if components.is_empty() {
        return Err(IndexOperationJournalValidationError::InvalidPackageRoot);
    }
    let package_root = components.join("/");
    RepositoryPackageDirectory::parse(&package_root)
        .map_err(|_| IndexOperationJournalValidationError::InvalidPackageRoot)?;
    Ok(package_root)
}

fn find_indexable_context<'a>(
    contexts_by_key: &BTreeMap<&str, &'a IndexJournalPackageContext>,
    package_node_key: &str,
) -> Result<&'a IndexJournalPackageContext, IndexOperationJournalValidationError> {
    let context = contexts_by_key
        .get(package_node_key)
        .copied()
        .ok_or_else(
            || IndexOperationJournalValidationError::UnknownPackageNode {
                package_node_key: package_node_key.to_owned(),
            },
        )?;

    if context.form != PackageForm::Source
        || !matches!(
            context.role,
            PackageGraphRole::Root | PackageGraphRole::WorkspaceMember
        )
    {
        return Err(IndexOperationJournalValidationError::PackageNotIndexable {
            package_node_key: package_node_key.to_owned(),
        });
    }
    Ok(context)
}

fn require_planned_index_file(
    context: &IndexJournalPackageContext,
    package_node_key: &str,
    planned_index_files: &BTreeSet<PackageIndexFileName>,
) -> Result<(), IndexOperationJournalValidationError> {
    if planned_index_files.contains(&context.index_file) {
        return Ok(());
    }
    Err(
        IndexOperationJournalValidationError::PackageMissingFromPlannedIndex {
            package_node_key: package_node_key.to_owned(),
            file: context.index_file.as_str().to_owned(),
        },
    )
}

fn validate_recorded_source_path(
    package_node_key: &str,
    path: &str,
    source_directory: &PackageSourceDirectory,
) -> Result<(), IndexOperationJournalValidationError> {
    let invalid_path = || IndexOperationJournalValidationError::InvalidSourcePath {
        path: path.to_owned(),
    };
    PackageRelativePath::new(path.to_owned()).map_err(|_| invalid_path())?;

    let source_relative_path = if source_directory.as_str() == "." {
        path
    } else {
        let prefix = format!("{}/", source_directory.as_str());
        path.strip_prefix(&prefix).ok_or_else(|| {
            IndexOperationJournalValidationError::SourceOutsideDeclaredDirectory {
                package_node_key: package_node_key.to_owned(),
                path: path.to_owned(),
                source_directory: source_directory.as_str().to_owned(),
            }
        })?
    };
    if PackageSourcePath::new(source_relative_path.to_owned()).is_err()
        || !is_indexable_source_module_path(source_relative_path)
    {
        return Err(invalid_path());
    }
    Ok(())
}

fn is_indexable_source_module_path(path: &str) -> bool {
    if path.len() > 4096 {
        return false;
    }
    let Some(file_name) = path.rsplit('/').next() else {
        return false;
    };
    let Some(stem) = file_name.strip_suffix(".sai") else {
        return false;
    };
    let mut components = path.split('/').collect::<Vec<_>>();
    let Some(file_component) = components.pop() else {
        return false;
    };
    if file_component.starts_with('.')
        || components
            .iter()
            .any(|component| component.starts_with('.'))
    {
        return false;
    }
    if components.is_empty() && stem == "lib" {
        return true;
    }
    if stem == "lib" {
        return false;
    }
    components
        .into_iter()
        .chain(std::iter::once(stem))
        .all(is_valid_module_segment)
}

fn is_valid_module_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    if !bytes
        .iter()
        .copied()
        .skip(1)
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return false;
    }
    segment != "lib"
        && !sai_syntax::is_strict_keyword(segment)
        && !sai_syntax::is_reserved_identifier(segment)
}

#[cfg(test)]
mod tests {
    use super::{
        IndexJournalPackageContext, IndexJournalWorkspaceInventoryBuildError,
        IndexOperationJournalValidationError, LockRootContractInputError,
        build_index_journal_workspace_inventory,
        validate_index_operation_journal_package_context as validate_package_context,
        validate_unique_inventory_contexts,
    };
    use crate::package_graph::{PackageForm, PackageGraphNode, PackageGraphRole};
    use sai_package::local_source::LocalPackageDirectory;
    use sai_package::lock::parse as parse_lock;
    use sai_package::validation::PackageValidationContext;
    use sai_package::version::Version;
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange,
    };
    use sai_stable_hash::hash_bytes;
    use sai_vfs::PackageKey;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct TestWorkspace(PathBuf);

    impl TestWorkspace {
        fn new() -> Self {
            let id = NEXT_TEST_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-journal-context-validation-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary workspace");
            Self(path)
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_package(root: &PathBuf, relative: &str, manifest: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(&path).expect("create package directory");
        fs::write(path.join("sai.package"), manifest).expect("write package manifest");
        path
    }

    fn build_inventory(
        root: &PathBuf,
        lock_text: &str,
    ) -> Result<super::IndexJournalWorkspaceInventory, IndexJournalWorkspaceInventoryBuildError>
    {
        let root_directory = LocalPackageDirectory::open(root).expect("open root package");
        let parsed_lock = parse_lock(lock_text.as_bytes());
        build_index_journal_workspace_inventory(
            &root_directory,
            &parsed_lock,
            PackageValidationContext::default(),
        )
    }

    fn lock_root(requirements: &str, entries: &str) -> String {
        format!(
            "lock 2\n\
             root acme:app @ 1.0.0\n\
                 targets {{ wasm32-core, wasm32-wasi }}\n\
             {requirements}{entries}"
        )
    }

    fn path_lock_entry(package: &str, version: &str, path: &str, requirements: &str) -> String {
        format!(
            "\npackage {package} @ {version}\n\
                 source path \"{path}\"\n\
                 {requirements}"
        )
    }

    fn graph_node(
        identity: &str,
        version: &str,
        role: PackageGraphRole,
        form: PackageForm,
    ) -> PackageGraphNode {
        let version = Version::parse(version).expect("package version is valid");
        let package_key = PackageKey::new(format!("{identity}#{}", version.major()))
            .expect("compatibility package key is valid");
        PackageGraphNode::new(package_key, identity, version, form, role, Vec::new())
            .expect("package graph node is valid")
    }

    fn source_change(package_node_key: &str, package_root: &str, path: &str) -> SourceFileChange {
        SourceFileChange {
            package_node_key: package_node_key.to_owned(),
            package_root: package_root.to_owned(),
            path: path.to_owned(),
            before: FileState::RegularFile(b"before".to_vec()),
            planned_after: FileState::RegularFile(b"after".to_vec()),
        }
    }

    fn journal(source: SourceFileChange, planned_index_files: &[&str]) -> IndexOperationJournal {
        journal_with_index_files(source, planned_index_files, planned_index_files)
    }

    fn journal_with_index_files(
        source: SourceFileChange,
        before_index_files: &[&str],
        after_index_files: &[&str],
    ) -> IndexOperationJournal {
        let before_files = before_index_files
            .iter()
            .map(|file| PackageIndexFileName::parse(file).expect("index filename is valid"))
            .collect::<BTreeSet<_>>();
        let after_files = after_index_files
            .iter()
            .map(|file| PackageIndexFileName::parse(file).expect("index filename is valid"))
            .collect::<BTreeSet<_>>();
        let changed_file = after_files
            .first()
            .or_else(|| before_files.first())
            .expect("journal fixture has a package index file")
            .clone();
        let affected_files = before_files
            .union(&after_files)
            .cloned()
            .collect::<Vec<_>>();
        let before_states = affected_files
            .iter()
            .map(|file| {
                let state = if before_files.contains(file) {
                    FileState::RegularFile(package_index_bytes(file, file == &changed_file, false))
                } else {
                    FileState::Absent
                };
                PackageFileState {
                    file: file.clone(),
                    state,
                }
            })
            .collect::<Vec<_>>();
        let after_states = affected_files
            .iter()
            .map(|file| {
                let state = if after_files.contains(file) {
                    FileState::RegularFile(package_index_bytes(file, file == &changed_file, true))
                } else {
                    FileState::Absent
                };
                PackageFileState {
                    file: file.clone(),
                    state,
                }
            })
            .collect::<Vec<_>>();
        let before_header = index_header(&before_files, &changed_file, false);
        let after_header = index_header(&after_files, &changed_file, true);
        IndexOperationJournal {
            operation_id: "5197cf1c-dbec-4b13-9077-1c2c1ce2d664".to_owned(),
            origin: IndexJournalOrigin::Cli,
            state: IndexJournalState::Prepared,
            sources: vec![source],
            index_before: IndexSnapshot {
                header: FileState::RegularFile(before_header),
                package_files: before_states,
            },
            index_after: IndexSnapshot {
                header: FileState::RegularFile(after_header),
                package_files: after_states,
            },
            symbol_mappings: Vec::new(),
        }
    }

    fn package_index_bytes(file: &PackageIndexFileName, changed: bool, after: bool) -> Vec<u8> {
        if !changed {
            return Vec::new();
        }
        let mut segments = file.as_str().strip_suffix(".jsonl").unwrap().split('.');
        let namespace = segments.next().unwrap();
        let package = segments.next().unwrap();
        let package_identity = format!("{namespace}:{package}");
        let fingerprint = if after { "b" } else { "a" }.repeat(32);
        format!(
            "{{\"hex\":\"000001\",\"def_path_hash\":\"{}\",\"path\":\"{package_identity}.sample\",\"kind\":\"fn\",\"fingerprint\":\"{fingerprint}\"}}\n",
            "0".repeat(32),
        )
        .into_bytes()
    }

    fn index_header(
        files: &BTreeSet<PackageIndexFileName>,
        changed_file: &PackageIndexFileName,
        after: bool,
    ) -> Vec<u8> {
        let package_records = files
            .iter()
            .map(|file| {
                let bytes = package_index_bytes(file, file == changed_file, after);
                format!(
                    "{{\"file\":\"{}\",\"blake3\":\"{}\"}}",
                    file.as_str(),
                    hash_bytes(&bytes).to_hex()
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{package_records}]}}\n")
            .into_bytes()
    }

    fn validate_index_operation_journal_package_context(
        journal: &IndexOperationJournal,
        contexts: &[IndexJournalPackageContext],
    ) -> Result<(), IndexOperationJournalValidationError> {
        let encoded = journal.encode().expect("test journal is codec-valid");
        let decoded = IndexOperationJournal::decode(&encoded).expect("journal decodes");
        validate_package_context(&decoded, contexts)
    }

    fn root_context(source_directory: Option<&str>) -> IndexJournalPackageContext {
        let node = graph_node(
            "acme:app",
            "1.2.3",
            PackageGraphRole::Root,
            PackageForm::Source,
        );
        IndexJournalPackageContext::from_test_metadata(&node, Some("."), source_directory)
            .expect("root package context is valid")
    }

    fn member_context(source_directory: Option<&str>) -> IndexJournalPackageContext {
        let node = graph_node(
            "acme:widget",
            "1.2.3",
            PackageGraphRole::WorkspaceMember,
            PackageForm::Source,
        );
        IndexJournalPackageContext::from_test_metadata(
            &node,
            Some("crates/widget"),
            source_directory,
        )
        .expect("workspace member context is valid")
    }

    fn member_journal(path: &str) -> IndexOperationJournal {
        journal(
            source_change("package:acme:widget@1.2.3", "crates/widget", path),
            &["acme.widget.1.jsonl"],
        )
    }

    #[test]
    fn validates_root_and_nested_workspace_member_sources() {
        let root = root_context(Some("src"));
        let root_journal = journal(
            source_change("package:acme:app@1.2.3", ".", "src/main.sai"),
            &["acme.app.1.jsonl"],
        );
        validate_index_operation_journal_package_context(&root_journal, &[root])
            .expect("workspace root source is valid");

        validate_index_operation_journal_package_context(
            &member_journal("src/lib.sai"),
            &[member_context(Some("src"))],
        )
        .expect("nested workspace member source is valid");
    }

    #[test]
    fn builds_contexts_from_captured_manifest_without_active_graph_nodes() {
        let workspace = TestWorkspace::new();
        let root_manifest = "package acme:app @ 1.2.3\nsource \"src\"\n";
        fs::write(workspace.0.join("sai.package"), root_manifest)
            .expect("write workspace root manifest");
        let member_path = workspace.0.join("crates/widget");
        fs::create_dir_all(&member_path).expect("create workspace member");
        fs::write(
            member_path.join("sai.package"),
            "package acme:widget @ 1.2.3\nsource \"src\"\nlints {}\nlints {}\n",
        )
        .expect("write workspace member manifest");

        let workspace_root =
            LocalPackageDirectory::open(&workspace.0).expect("open workspace root capability");
        let package_directory =
            LocalPackageDirectory::open(&member_path).expect("open package capability");
        let root = IndexJournalPackageContext::from_workspace_package(
            PackageGraphRole::Root,
            &workspace_root,
            &workspace_root,
        )
        .expect("workspace-root context is derived");
        let member = IndexJournalPackageContext::from_workspace_package(
            PackageGraphRole::WorkspaceMember,
            &workspace_root,
            &package_directory,
        )
        .expect("member context is derived");
        assert_eq!(root.package_root(), Some("."));
        assert_eq!(root.package_node_key(), "package:acme:app@1.2.3");
        assert_eq!(root.source_directory().unwrap().as_str(), "src");
        assert_eq!(member.package_root(), Some("crates/widget"));
        assert_eq!(member.package_node_key(), "package:acme:widget@1.2.3");
        assert_eq!(member.source_directory().unwrap().as_str(), "src");

        assert!(matches!(
            IndexJournalPackageContext::from_workspace_package(
                PackageGraphRole::WorkspaceMember,
                &workspace_root,
                &workspace_root,
            ),
            Err(IndexOperationJournalValidationError::InvalidPackageRoot)
        ));
        assert!(matches!(
            IndexJournalPackageContext::from_workspace_package(
                PackageGraphRole::Root,
                &workspace_root,
                &package_directory,
            ),
            Err(IndexOperationJournalValidationError::InvalidPackageRoot)
        ));

        let source_less_path = workspace.0.join("crates/source-less");
        fs::create_dir_all(&source_less_path).expect("create source-less member");
        fs::write(
            source_less_path.join("sai.package"),
            "package acme:source-less @ 1.2.3\n\
             wit \"wit\"\n\
             export world api\n\
             targets { wasm32-wasi }\n\
             artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
        )
        .expect("write artifact-only member manifest");
        let source_less_directory = LocalPackageDirectory::open(&source_less_path)
            .expect("open artifact-only member capability");
        let artifact = IndexJournalPackageContext::from_workspace_package(
            PackageGraphRole::WorkspaceMember,
            &workspace_root,
            &source_less_directory,
        )
        .expect("artifact-only member has an index context");
        assert!(artifact.source_directory().is_none());
    }

    #[test]
    fn rejects_member_directories_outside_the_workspace() {
        let workspace = TestWorkspace::new();
        let outside = TestWorkspace::new();
        fs::write(
            workspace.0.join("sai.package"),
            "package acme:app @ 1.2.3\nsource \"src\"\n",
        )
        .expect("write workspace root manifest");
        fs::write(
            outside.0.join("sai.package"),
            "package acme:outside @ 1.2.3\nsource \"src\"\n",
        )
        .expect("write outside package manifest");
        let workspace_root =
            LocalPackageDirectory::open(&workspace.0).expect("open workspace root capability");
        let outside_directory =
            LocalPackageDirectory::open(&outside.0).expect("open outside package capability");

        assert!(matches!(
            IndexJournalPackageContext::from_workspace_package(
                PackageGraphRole::WorkspaceMember,
                &workspace_root,
                &outside_directory,
            ),
            Err(IndexOperationJournalValidationError::PackageDirectoryOutsideWorkspace)
        ));
    }

    #[test]
    fn matches_package_nodes_by_full_version_not_compatibility_key() {
        let context = member_context(Some("src"));
        assert_eq!(context.package_node_key(), "package:acme:widget@1.2.3");
        assert_eq!(context.index_file().as_str(), "acme.widget.1.jsonl");

        let changed_version = graph_node(
            "acme:widget",
            "1.4.0",
            PackageGraphRole::WorkspaceMember,
            PackageForm::Source,
        );
        let changed_context = IndexJournalPackageContext::from_test_metadata(
            &changed_version,
            Some("crates/widget"),
            Some("src"),
        )
        .expect("new version context is valid");
        assert_eq!(changed_context.index_file(), context.index_file());
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[changed_context],
            ),
            Err(IndexOperationJournalValidationError::UnknownPackageNode { .. })
        ));
    }

    #[test]
    fn rejects_unknown_or_removed_package_nodes() {
        assert!(matches!(
            validate_index_operation_journal_package_context(&member_journal("src/lib.sai"), &[],),
            Err(IndexOperationJournalValidationError::UnknownPackageNode { .. })
        ));
    }

    #[test]
    fn rejects_a_recorded_root_that_no_longer_matches_the_workspace() {
        let mut record = member_journal("src/lib.sai");
        record.sources[0].package_root = "crates/other".to_owned();
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &record,
                &[member_context(Some("src"))],
            ),
            Err(IndexOperationJournalValidationError::PackageRootMismatch { .. })
        ));
    }

    #[test]
    fn rejects_dependency_and_artifact_packages() {
        let dependency = graph_node(
            "acme:widget",
            "1.2.3",
            PackageGraphRole::Dependency,
            PackageForm::Source,
        );
        let dependency_context =
            IndexJournalPackageContext::from_test_metadata(&dependency, None, Some("src"))
                .expect("dependency context is valid");
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[dependency_context],
            ),
            Err(IndexOperationJournalValidationError::PackageNotIndexable { .. })
        ));

        let artifact = graph_node(
            "acme:widget",
            "1.2.3",
            PackageGraphRole::Dependency,
            PackageForm::Artifact,
        );
        let artifact_context =
            IndexJournalPackageContext::from_test_metadata(&artifact, None, None)
                .expect("artifact context is valid");
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[artifact_context],
            ),
            Err(IndexOperationJournalValidationError::PackageNotIndexable { .. })
        ));
    }

    #[test]
    fn rejects_packages_without_a_declared_source_directory() {
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[member_context(None)],
            ),
            Err(IndexOperationJournalValidationError::MissingSourceDirectory { .. })
        ));
    }

    #[test]
    fn accepts_the_package_root_as_source_directory() {
        let root = root_context(Some("."));
        let record = journal(
            source_change("package:acme:app@1.2.3", ".", "lib.sai"),
            &["acme.app.1.jsonl"],
        );
        validate_index_operation_journal_package_context(&record, &[root])
            .expect("root source path is valid");
    }

    #[test]
    fn enforces_source_directory_component_boundaries() {
        for path in ["src2/lib.sai", "src/lib-extra/file.sai", "src"] {
            assert!(matches!(
                validate_index_operation_journal_package_context(
                    &member_journal(path),
                    &[member_context(Some("src/lib"))],
                ),
                Err(IndexOperationJournalValidationError::SourceOutsideDeclaredDirectory { .. })
            ));
        }
        validate_index_operation_journal_package_context(
            &member_journal("src/lib/file.sai"),
            &[member_context(Some("src/lib"))],
        )
        .expect("nested source directory path is valid");
    }

    #[test]
    fn rejects_ambiguous_duplicate_contexts() {
        let context = member_context(Some("src"));
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[context.clone(), context],
            ),
            Err(IndexOperationJournalValidationError::DuplicatePackageContext { .. })
        ));
    }

    #[test]
    fn requires_each_recorded_package_in_the_planned_index_header() {
        let record = journal(
            source_change("package:acme:widget@1.2.3", "crates/widget", "src/lib.sai"),
            &["acme.other.1.jsonl"],
        );
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &record,
                &[member_context(Some("src"))],
            ),
            Err(IndexOperationJournalValidationError::PackageMissingFromPlannedIndex { .. })
        ));
    }

    #[test]
    fn planned_header_must_keep_untouched_workspace_packages() {
        let root = root_context(Some("src"));
        let member = member_context(Some("src"));
        let record = journal_with_index_files(
            source_change("package:acme:app@1.2.3", ".", "src/main.sai"),
            &["acme.app.1.jsonl", "acme.widget.1.jsonl"],
            &["acme.app.1.jsonl"],
        );
        assert!(matches!(
            validate_index_operation_journal_package_context(&record, &[root, member]),
            Err(IndexOperationJournalValidationError::PlannedIndexPackageSetMismatch)
        ));
    }

    #[test]
    fn planned_header_must_not_add_stale_or_non_workspace_packages() {
        let root = root_context(Some("src"));
        let record = journal_with_index_files(
            source_change("package:acme:app@1.2.3", ".", "src/main.sai"),
            &["acme.app.1.jsonl", "acme.stale.1.jsonl"],
            &["acme.app.1.jsonl", "acme.stale.1.jsonl"],
        );
        assert!(matches!(
            validate_index_operation_journal_package_context(&record, &[root]),
            Err(IndexOperationJournalValidationError::PlannedIndexPackageSetMismatch)
        ));
    }

    #[test]
    fn rejects_distinct_workspace_nodes_that_collide_on_index_filename() {
        let first = member_context(Some("src"));
        let second_node = graph_node(
            "acme:widget",
            "1.4.0",
            PackageGraphRole::WorkspaceMember,
            PackageForm::Source,
        );
        let second = IndexJournalPackageContext::from_test_metadata(
            &second_node,
            Some("crates/widget-v14"),
            Some("src"),
        )
        .expect("second compatibility-line context is valid");
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal("src/lib.sai"),
                &[first, second],
            ),
            Err(IndexOperationJournalValidationError::DuplicatePackageIndexFileContext { .. })
        ));
    }

    #[test]
    fn accepts_long_package_relative_paths_when_source_relative_path_fits_limit() {
        let source_directory = format!("src/{}a", "a/".repeat(2043));
        assert!(source_directory.len() < 4096);
        let path = format!("{source_directory}/lib.sai");
        assert!(path.len() > 4096);
        let record = member_journal(&path);
        validate_index_operation_journal_package_context(
            &record,
            &[member_context(Some(&source_directory))],
        )
        .expect("the source-root prefix does not count against source-relative path limit");
    }

    #[test]
    fn rejects_non_module_files_hidden_paths_and_invalid_module_names() {
        for path in [
            "src/README.md",
            "src/.scratch.sai",
            "src/.hidden/main.sai",
            "src/Upper.sai",
            "src/return.sai",
            "src/lib/lib.sai",
            "src/café.sai",
        ] {
            assert!(
                matches!(
                    validate_index_operation_journal_package_context(
                        &member_journal(path),
                        &[member_context(Some("src"))],
                    ),
                    Err(IndexOperationJournalValidationError::InvalidSourcePath { .. })
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn source_relative_path_limit_matches_source_tree_discovery() {
        let path = format!("src/{}main.sai", "a/".repeat(2046));
        assert!(matches!(
            validate_index_operation_journal_package_context(
                &member_journal(&path),
                &[member_context(Some("src"))],
            ),
            Err(IndexOperationJournalValidationError::InvalidSourcePath { .. })
        ));
    }

    #[test]
    fn rejects_invalid_workspace_root_contexts() {
        let root = graph_node(
            "acme:app",
            "1.2.3",
            PackageGraphRole::Root,
            PackageForm::Source,
        );
        assert!(matches!(
            IndexJournalPackageContext::from_test_metadata(&root, Some("app"), Some("src")),
            Err(IndexOperationJournalValidationError::InvalidPackageRoot)
        ));

        let member = graph_node(
            "acme:widget",
            "1.2.3",
            PackageGraphRole::WorkspaceMember,
            PackageForm::Source,
        );
        assert!(matches!(
            IndexJournalPackageContext::from_test_metadata(&member, Some("."), Some("src")),
            Err(IndexOperationJournalValidationError::InvalidPackageRoot)
        ));
    }

    #[test]
    fn builds_target_independent_inventory_from_all_lock_edges() {
        let workspace = TestWorkspace::new();
        let root_manifest = "package acme:app @ 1.0.0\n\
            source \"src\"\n\
            targets { wasm32-core, wasm32-wasi }\n\
            requires {\n\
                acme:parent @ ^1 from path \"packages/parent\"\n\
                acme:only @ ^1 from path \"packages/only\" only { wasm32-wasi }\n\
            }\n\
            requires test {\n\
                acme:fixture @ ^1 from path \"packages/fixture\"\n\
            }\n";
        write_package(&workspace.0, ".", root_manifest);
        write_package(
            &workspace.0,
            "packages/parent",
            "package acme:parent @ 1.0.0\n\
             targets { wasm32-core, wasm32-wasi }\n\
             source \"src\"\n\
             requires {\n\
                 acme:child @ ^1 from path \"../child\"\n\
             }\n",
        );
        write_package(
            &workspace.0,
            "packages/child",
            "package acme:child @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );
        write_package(
            &workspace.0,
            "packages/only",
            "package acme:only @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );
        write_package(
            &workspace.0,
            "packages/fixture",
            "package acme:fixture @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );

        let lock = lock_root(
            "    requires acme:parent @ ^1 -> 1.0.0 from path \"packages/parent\"\n\
                 requires acme:only @ ^1 -> 1.0.0 from path \"packages/only\" only { wasm32-wasi }\n\
                 requires test acme:fixture @ ^1 -> 1.0.0 from path \"packages/fixture\"\n",
            &format!(
                "{}{}{}{}",
                path_lock_entry(
                    "acme:parent",
                    "1.0.0",
                    "packages/parent",
                    "    requires acme:child @ ^1 -> 1.0.0 from path \"../child\"\n",
                ),
                path_lock_entry("acme:child", "1.0.0", "packages/child", ""),
                path_lock_entry("acme:only", "1.0.0", "packages/only", ""),
                path_lock_entry("acme:fixture", "1.0.0", "packages/fixture", ""),
            ),
        );
        let inventory =
            build_inventory(&workspace.0, &lock).expect("all lock-selected packages are valid");
        let keys = inventory
            .contexts()
            .iter()
            .map(IndexJournalPackageContext::package_node_key)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "package:acme:app@1.0.0",
                "package:acme:child@1.0.0",
                "package:acme:fixture@1.0.0",
                "package:acme:only@1.0.0",
                "package:acme:parent@1.0.0",
            ]
        );
        assert_eq!(
            inventory
                .directory_for_package("package:acme:child@1.0.0")
                .unwrap()
                .path(),
            fs::canonicalize(workspace.0.join("packages/child"))
                .unwrap()
                .as_path()
        );
        assert_eq!(
            inventory.contexts()[0].package_root(),
            Some("."),
            "the root context is retained alongside non-active workspace members"
        );
    }

    #[test]
    fn rejects_local_dependency_requirements_missing_from_the_lock() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:parent @ ^1 from path \"packages/parent\" }\n",
        );
        write_package(
            &workspace.0,
            "packages/parent",
            "package acme:parent @ 1.0.0\n\
             targets { wasm32-core }\n\
             source \"src\"\n\
             requires {\n\
                 acme:child @ ^1 from path \"../child\"\n\
             }\n",
        );
        write_package(
            &workspace.0,
            "packages/child",
            "package acme:child @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n",
        );

        let stale_lock = lock_root(
            "    requires acme:parent @ ^1 -> 1.0.0 from path \"packages/parent\"\n",
            &path_lock_entry("acme:parent", "1.0.0", "packages/parent", ""),
        );
        let error = build_inventory(&workspace.0, &stale_lock)
            .expect_err("the captured member differs from its lock entry");
        assert!(
            matches!(
                &error,
                IndexJournalWorkspaceInventoryBuildError::PackageContractMismatch {
                    package_version,
                    ..
                } if package_version.package() == "acme:parent"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn rejects_root_requirements_missing_from_the_lock() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:dep @ ^1 from path \"packages/dep\" }\n",
        );
        write_package(
            &workspace.0,
            "packages/dep",
            "package acme:dep @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );

        let stale_lock = lock_root(
            "",
            &path_lock_entry("acme:dep", "1.0.0", "packages/dep", ""),
        );
        assert!(matches!(
            build_inventory(&workspace.0, &stale_lock),
            Err(IndexJournalWorkspaceInventoryBuildError::RootContractMismatch(_))
        ));
    }

    #[test]
    fn rejects_test_edges_in_dependency_lock_syntax() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:parent @ ^1 from path \"packages/parent\" }\n",
        );
        write_package(
            &workspace.0,
            "packages/parent",
            "package acme:parent @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );
        write_package(
            &workspace.0,
            "packages/helper",
            "package acme:helper @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );

        let lock = lock_root(
            "    requires acme:parent @ ^1 -> 1.0.0 from path \"packages/parent\"\n",
            &format!(
                "{}{}",
                path_lock_entry(
                    "acme:parent",
                    "1.0.0",
                    "packages/parent",
                    "    requires test acme:helper @ ^1 -> 1.0.0 from path \"../helper\"\n",
                ),
                path_lock_entry("acme:helper", "1.0.0", "packages/helper", ""),
            ),
        );
        assert!(matches!(
            build_inventory(&workspace.0, &lock),
            Err(IndexJournalWorkspaceInventoryBuildError::RootContractInput(
                LockRootContractInputError::InvalidLockSyntax { .. }
            ))
        ));
    }

    #[test]
    fn includes_artifact_only_workspace_members_without_source_directories() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires {\n\
                 acme:artifact @ ^1 from path \"packages/artifact\"\n\
                     only { wasm32-wasi }\n\
             }\n",
        );
        write_package(
            &workspace.0,
            "packages/artifact",
            "package acme:artifact @ 1.0.0\n\
             wit \"wit\"\n\
             export world api\n\
             targets { wasm32-wasi }\n\
             artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
        );

        let lock = lock_root(
            "    requires acme:artifact @ ^1 -> 1.0.0 from path \"packages/artifact\" only { wasm32-wasi }\n",
            &path_lock_entry("acme:artifact", "1.0.0", "packages/artifact", ""),
        );
        let inventory = build_inventory(&workspace.0, &lock)
            .expect("artifact-only packages are valid workspace members");
        let artifact = inventory
            .contexts()
            .iter()
            .find(|context| context.package_node_key() == "package:acme:artifact@1.0.0")
            .expect("artifact member appears in the workspace index set");
        assert_eq!(artifact.package_root(), Some("packages/artifact"));
        assert!(artifact.source_directory().is_none());
    }

    #[test]
    fn excludes_external_path_and_git_sources_from_workspace_members() {
        let workspace = TestWorkspace::new();
        let outside = TestWorkspace::new();
        let outside_name = outside.0.file_name().unwrap().to_str().unwrap();
        write_package(
            &outside.0,
            ".",
            "package acme:outside @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );
        let root_manifest = format!(
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core, wasm32-wasi }}\n\
             requires {{\n\
                 acme:outside @ ^1 from path \"../{outside_name}\"\n\
                 acme:remote @ ^1 from git \"https://example.invalid/remote\"\n\
             }}\n"
        );
        write_package(&workspace.0, ".", &root_manifest);

        let lock = lock_root(
            &format!(
                "    requires acme:outside @ ^1 -> 1.0.0 from path \"../{}\"\n\
                     requires acme:remote @ ^1 -> 1.0.0 from git \"https://example.invalid/remote\"\n",
                outside_name
            ),
            &format!(
                "{}\n\
                 package acme:remote @ 1.0.0\n\
                     source git \"https://example.invalid/remote\"\n\
                     commit \"0123456789012345678901234567890123456789\"\n\
                     checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                     capabilities {{ }}\n",
                path_lock_entry(
                    "acme:outside",
                    "1.0.0",
                    &format!("../{}", outside.0.file_name().unwrap().to_str().unwrap()),
                    "",
                )
            ),
        );
        let inventory = build_inventory(&workspace.0, &lock)
            .expect("external path and Git lock entries validate");
        assert_eq!(inventory.contexts().len(), 1);
        assert_eq!(
            inventory.contexts()[0].package_node_key(),
            "package:acme:app@1.0.0"
        );
        assert_eq!(inventory.directories().count(), 1);
    }

    #[test]
    fn materializes_lock_floor_path_entries_into_the_inventory() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires {\n\
                 acme:floor @ ^1 from git \"https://example.invalid/floor\"\n\
             }\n",
        );
        write_package(
            &workspace.0,
            "packages/floor",
            "package acme:floor @ 1.1.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        );
        let lock = lock_root(
            "    requires acme:floor @ ^1 -> 1.1.0 from git \"https://example.invalid/floor\"\n",
            &path_lock_entry("acme:floor", "1.1.0", "packages/floor", ""),
        );

        let inventory =
            build_inventory(&workspace.0, &lock).expect("LockFloor local source is materialized");
        assert!(
            inventory
                .contexts()
                .iter()
                .any(|context| context.package_node_key() == "package:acme:floor@1.1.0")
        );
        assert_eq!(
            inventory
                .directory_for_package("package:acme:floor@1.1.0")
                .unwrap()
                .path(),
            fs::canonicalize(workspace.0.join("packages/floor"))
                .unwrap()
                .as_path()
        );
    }

    #[test]
    fn rejects_missing_and_mismatched_locked_packages_without_partial_inventory() {
        let workspace = TestWorkspace::new();
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:missing @ ^1 from path \"packages/missing\" }\n",
        );
        let missing = lock_root(
            "    requires acme:missing @ ^1 -> 1.0.0 from path \"packages/missing\"\n",
            "",
        );
        let missing_error = build_inventory(&workspace.0, &missing)
            .expect_err("missing path package cannot produce an inventory");
        assert!(
            matches!(
                &missing_error,
                IndexJournalWorkspaceInventoryBuildError::SourceCanonicalizer(_)
                    | IndexJournalWorkspaceInventoryBuildError::RootPathOverrides(_)
                    | IndexJournalWorkspaceInventoryBuildError::SourcePlanning(_)
            ),
            "{missing_error:?}"
        );

        write_package(
            &workspace.0,
            "packages/dep",
            "package acme:dep @ 1.1.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n",
        );
        write_package(
            &workspace.0,
            ".",
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:dep @ ^1 from path \"packages/dep\" }\n",
        );
        let mismatched = lock_root(
            "    requires acme:dep @ ^1 -> 1.0.0 from path \"packages/dep\"\n",
            &path_lock_entry("acme:dep", "1.0.0", "packages/dep", ""),
        );
        assert!(matches!(
            build_inventory(&workspace.0, &mismatched),
            Err(IndexJournalWorkspaceInventoryBuildError::SourceValidation { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_full_keys_and_compatibility_index_filenames() {
        let root = graph_node(
            "acme:app",
            "1.2.3",
            PackageGraphRole::Root,
            PackageForm::Source,
        );
        let root_context =
            IndexJournalPackageContext::from_test_metadata(&root, Some("."), Some("src"))
                .expect("root context");
        assert!(matches!(
            validate_unique_inventory_contexts(&[root_context.clone(), root_context.clone()]),
            Err(IndexJournalWorkspaceInventoryBuildError::DuplicatePackageContext { .. })
        ));

        let newer = graph_node(
            "acme:app",
            "1.8.0",
            PackageGraphRole::WorkspaceMember,
            PackageForm::Source,
        );
        let member_context = IndexJournalPackageContext::from_test_metadata(
            &newer,
            Some("members/app"),
            Some("src"),
        )
        .expect("member context");
        assert_eq!(root_context.package_node_key(), "package:acme:app@1.2.3");
        assert_eq!(member_context.package_node_key(), "package:acme:app@1.8.0");
        assert!(matches!(
            validate_unique_inventory_contexts(&[root_context, member_context]),
            Err(IndexJournalWorkspaceInventoryBuildError::DuplicateWorkspaceIndexFile { .. })
        ));
    }
}
