#![forbid(unsafe_code)]

pub mod package_graph;

use crate::package_graph::{PackageForm, PackageGraph, PackageGraphRole};
use sai_diagnostics::{
    CauseKey, Child, Diagnostic, Label, LevelInfo, OverrideLintLevel, Related, SourceRef,
    source_text_status_diagnostic,
};
use sai_hir::OwnerId;
use sai_hir::arena::HirArena;
pub use sai_hir::attrs::{TargetCfg, TargetCfgError};
use sai_hir::item_tree::{ItemTree, ItemTreeKind, TargetFilterError, TargetFilterResult};
use sai_hir::items::HirOwner;
use sai_hir::lower::{
    ExternItemIds, HirLoweringContext, HirLoweringError, lower_alias_owner, lower_const_owner,
    lower_distinct_owner, lower_effect_owner, lower_enum_owner, lower_extern_block_owner,
    lower_extern_item_owner, lower_function_owner, lower_impl_owner, lower_interface_owner,
    lower_invariant_owner, lower_property_owner, lower_refined_owner, lower_resource_owner,
    lower_struct_owner, lower_test_owner, lower_trait_owner, lower_world_owner,
};
use sai_hir::source_map::HirSourceMap;
use sai_hir::{DefId, DefPath, DefPathInterner, DefPathInternerError};
use sai_hir::{ItemTreeQueries, ItemTreeQueryError};
use sai_intern::{Interner, LookupError, PackageId, PackageIdError, PackageInterner, Symbol};
use sai_package::manifest::PackageRole;
use sai_package::mixed_source::{MixedSourceFinalAuditPlan, MixedSourceMvsOutput};
use sai_package::mvs::{MvsPackageVersion, MvsSelectionOutput};
use sai_package::mvs_diagnostics::{
    MvsDuplicatePackageLineDiagnosticContext, MvsDuplicatePackageLineDiagnosticError,
    MvsManifestLocation, MvsManifestLocations, dependency_cycle_diagnostics,
    divergent_sources_diagnostics, duplicate_package_line_diagnostics,
    mixed_source_identity_failure_diagnostics, mixed_source_reload_failure_diagnostics,
    root_dependency_cycle_diagnostics, version_conflict_diagnostics,
};
use sai_package::version::Version;
use sai_query::{DerivedQuery, Durability, InputQuery, QueryDatabase, QueryError};
use sai_resolve::owner::{
    LabelResolution, LabelResolutionError, LocalResolution, LocalResolutionError,
    resolve_owner_control_flow, resolve_owner_locals, resolve_predicate_expression,
};
use sai_resolve::package_scopes::{
    BindingOrigin, DefKind, FilteredModule, ForeignPackageScope, PackageScopesError,
    PackageUseRequirement, RefinementPredicateOwnerKind, Res,
    collect_module_alias_bindings_with_foreign_scopes, collect_phase1,
    suppress_export_name_feature_gate,
};
pub use sai_resolve::package_scopes::{
    Phase1BuildContext, Phase1CollectionConfig, Phase1PackageScopes, TopLevelOwnerDescriptor,
    TopLevelOwnerKind,
};
use sai_resolve::use_target_diagnostics::{
    UseTargetDiagnosticsError, collect_local_use_target_diagnostics,
};
use sai_resolve::{ModuleId, ModuleTree, ModuleTreeQueries, ModuleTreeQueryError};
use sai_stable_hash::{Fingerprint, StableHash, StableHasher, fingerprint};
use sai_syntax::{Parse, ParseQueryError, SyntaxQueries, ast};
use sai_vfs::{
    FileContents, FileId, FileInterner, FileInternerError, FileKey, LineIndex, PackageFileKind,
    PackageFilesSnapshot, PackageKey, PackageKeyError, PackageRelativePath, PackageSourceDirectory,
    PackageSourcePath, SnapshotFile, SourceDecodeError, SourceFile, SourceText, VfsInputSnapshot,
    VfsInputUpdate, VfsInputs, WitFilesSnapshot, WorkspaceSnapshot, WorkspaceSnapshotError,
    WorkspaceSnapshotId,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// Owns the query runtime and the session state for the implemented frontend.
///
/// The driver applies VFS inputs before requesting tracked syntax and
/// item-tree queries. Snapshot-bound results are always rebound to the
/// [`WorkspaceSnapshot`] passed by the caller.
///
/// APIs that access the file interner require `FileId`s created by this
/// database. MVS dependency candidate manifests instead use distinct IDs from
/// their supplied snapshot because multiple versions on one compatibility
/// line share a stable package/file key.
pub struct RootDatabase {
    query_database: QueryDatabase,
    vfs: VfsInputs,
    syntax: SyntaxQueries,
    hir: ItemTreeQueries,
    resolve: ModuleTreeQueries<VfsInputs>,
    packages: Arc<PackageInterner>,
    files: FileInterner,
    symbols: Arc<Interner>,
    /// Retained for the lifetime of this database so every `DefId` returned
    /// by Phase 1 remains associated with its original package-local paths.
    def_paths: HashMap<PackageId, Arc<DefPathInterner>>,
    package_scopes: Phase1PackageScopesQueries,
    declared_requirements: DeclaredRequirementsQueries,
    package_graph: InputQuery<u8, Option<PackageGraph>>,
}

/// A semantically usable direct requirement retained from `sai.package`.
///
/// Target spellings stay as strings so query results remain independent of
/// session-local symbol IDs. The order of `only` targets is manifest order.
/// `declaration_order` is the source ordinal of the first declaration for
/// this identity, even though query results are sorted by identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredRequirement {
    pub identity: String,
    pub declaration_order: u32,
    pub only: Option<Arc<[String]>>,
    pub test: bool,
}

impl StableHash for DeclaredRequirement {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD4);
        self.identity.stable_hash(hasher);
        self.declaration_order.stable_hash(hasher);
        self.only.stable_hash(hasher);
        self.test.stable_hash(hasher);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeclaredRequirementsInput {
    source_bytes: Arc<[u8]>,
    dependency: bool,
}

impl StableHash for DeclaredRequirementsInput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD3);
        self.source_bytes.stable_hash(hasher);
        self.dependency.stable_hash(hasher);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeclaredRequirementsQueryOutput {
    requirements: Arc<[DeclaredRequirement]>,
    status: DeclaredRequirementsStatus,
}

impl StableHash for DeclaredRequirementsQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.requirements.stable_hash(hasher);
        self.status.stable_hash(hasher);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DeclaredRequirementsStatus {
    Valid,
    InvalidManifest,
    PackageMismatch {
        found_identity: String,
        found_compatibility_line: String,
    },
}

impl StableHash for DeclaredRequirementsStatus {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        match self {
            Self::Valid => hasher.write_tag(0xD5),
            Self::InvalidManifest => hasher.write_tag(0xD6),
            Self::PackageMismatch {
                found_identity,
                found_compatibility_line,
            } => {
                hasher.write_tag(0xD7);
                found_identity.stable_hash(hasher);
                found_compatibility_line.stable_hash(hasher);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclaredRequirementsError {
    Package(PackageIdError),
    PackageKey(PackageKeyError),
    Query(QueryError),
    InvalidManifest {
        package_key: String,
    },
    ManifestPackageMismatch {
        expected_package_key: String,
        found_identity: String,
        found_compatibility_line: String,
    },
}

impl From<PackageIdError> for DeclaredRequirementsError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<PackageKeyError> for DeclaredRequirementsError {
    fn from(error: PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<QueryError> for DeclaredRequirementsError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

struct DeclaredRequirementsQueries {
    input: InputQuery<PackageKey, DeclaredRequirementsInput>,
    query: DerivedQuery<PackageKey, DeclaredRequirementsQueryOutput>,
}

impl DeclaredRequirementsQueries {
    fn new() -> Result<Self, QueryError> {
        let input = InputQuery::<PackageKey, DeclaredRequirementsInput>::new(
            "declared_requirements_input",
            Durability::Low,
        )?;
        let query_input = input.clone();
        let query = DerivedQuery::new(
            "declared_requirements",
            move |database: &mut QueryDatabase, key: &PackageKey| {
                let input = query_input.get(database, key.clone())?;
                let role = if input.dependency {
                    PackageRole::Dependency
                } else {
                    PackageRole::Root
                };
                let parsed = sai_package::manifest::parse_for_role_with_error_limit(
                    &input.source_bytes,
                    role,
                    0,
                );
                if !parsed.is_syntax_valid() {
                    return Ok(DeclaredRequirementsQueryOutput {
                        requirements: Arc::from([]),
                        status: DeclaredRequirementsStatus::InvalidManifest,
                    });
                }
                let Some(header) = parsed.file().header.as_ref() else {
                    return Ok(DeclaredRequirementsQueryOutput {
                        requirements: Arc::from([]),
                        status: DeclaredRequirementsStatus::InvalidManifest,
                    });
                };
                let found_identity = header.value.identity.to_string();
                let found_compatibility_line =
                    header.value.version.compatibility_line().to_string();
                if !manifest_matches_package_key(key, &found_identity, &found_compatibility_line) {
                    return Ok(DeclaredRequirementsQueryOutput {
                        requirements: Arc::from([]),
                        status: DeclaredRequirementsStatus::PackageMismatch {
                            found_identity,
                            found_compatibility_line,
                        },
                    });
                }
                let mut by_identity = BTreeMap::<String, DeclaredRequirement>::new();
                let mut declaration_ordinal = 0_u32;
                for declaration in &parsed.file().declarations {
                    let sai_package::manifest::PackageDeclaration::Requires { test, entries } =
                        &declaration.value
                    else {
                        continue;
                    };
                    for entry in entries {
                        let source_ordinal = declaration_ordinal;
                        declaration_ordinal = declaration_ordinal.saturating_add(1);
                        let requirement = &entry.value;
                        let only = requirement.only.as_ref().map(|targets| {
                            targets
                                .value
                                .iter()
                                .map(|target| target.value.to_string())
                                .collect::<Vec<_>>()
                                .into()
                        });
                        by_identity
                            .entry(requirement.package.to_string())
                            .or_insert(DeclaredRequirement {
                                identity: requirement.package.to_string(),
                                declaration_order: source_ordinal,
                                only,
                                test: *test,
                            });
                    }
                }
                Ok(DeclaredRequirementsQueryOutput {
                    requirements: Arc::from(
                        by_identity
                            .into_values()
                            .collect::<Vec<DeclaredRequirement>>(),
                    ),
                    status: DeclaredRequirementsStatus::Valid,
                })
            },
        )?;
        Ok(Self { input, query })
    }
}

fn manifest_matches_package_key(
    package_key: &PackageKey,
    manifest_identity: &str,
    manifest_compatibility_line: &str,
) -> bool {
    if let Some((expected_identity, expected_line)) = package_key.as_str().split_once('#') {
        return expected_identity == manifest_identity
            && expected_line == manifest_compatibility_line;
    }

    matches!(package_key.as_str(), "sai:core" | "sai:alloc")
        && package_key.as_str() == manifest_identity
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Phase1PackageScopesIdentity {
    package: PackageKey,
    modules: Vec<Phase1ModuleIdentity>,
    namespaces: Vec<Phase1NamespaceIdentity>,
    target_name: String,
    target_capabilities: BTreeSet<String>,
    build_context: Phase1BuildContext,
    declared_requirements: Option<Vec<DeclaredRequirement>>,
    package_graph: Option<Phase1PackageGraphProjection>,
    foreign_packages: Vec<Phase1ForeignPackageIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Phase1ForeignPackageIdentity {
    visible_identity: String,
    package_key: PackageKey,
    input_fingerprint: Fingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Phase1PackageGraphProjection {
    Package {
        outgoing_edges: Vec<StableActivePackageEdge>,
    },
    ImplicitToolchain {
        version: Version,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableActivePackageEdge {
    visible_identity: String,
    target: PackageKey,
    target_version: Version,
    target_form: PackageForm,
    target_role: PackageGraphRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Phase1ModuleIdentity {
    path: Vec<String>,
    file_id: u32,
    source_path: String,
    source_package: Option<String>,
    document_version: Option<u64>,
    source_bytes: Arc<[u8]>,
    parent_dir: Option<u32>,
    children: Vec<(String, StableChildEntry)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Phase1NamespaceIdentity {
    path: Vec<String>,
    children: Vec<(String, StableChildEntry)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StableChildEntry {
    Module(u32),
    Namespace(u32),
    Both(u32, u32),
}

struct StagedPhase1PackageScopesInput {
    identity: Arc<Phase1PackageScopesIdentity>,
    prepared: Arc<PreparedPhase1PackageScopes>,
}

struct PreparedPhase1PackageScopes {
    module_tree: ModuleTree,
    snapshot: WorkspaceSnapshot,
    target_cfg: TargetCfg,
    build_context: Phase1BuildContext,
    declared_requirements: Option<Arc<[DeclaredRequirement]>>,
    foreign_packages: Vec<Phase1ForeignPackageScopes>,
    modules: Vec<OwnedPhase1ModuleInput>,
    def_paths: Arc<DefPathInterner>,
}

thread_local! {
    static STAGED_PHASE1_PACKAGE_SCOPES: RefCell<HashMap<PackageKey, StagedPhase1PackageScopesInput>> =
        RefCell::new(HashMap::new());
}

struct StagedPhase1InputGuard {
    key: PackageKey,
    previous: Option<StagedPhase1PackageScopesInput>,
}

impl StagedPhase1InputGuard {
    fn install(
        key: PackageKey,
        identity: Arc<Phase1PackageScopesIdentity>,
        prepared: Arc<PreparedPhase1PackageScopes>,
    ) -> Self {
        let previous = STAGED_PHASE1_PACKAGE_SCOPES.with(|staged| {
            staged.borrow_mut().insert(
                key.clone(),
                StagedPhase1PackageScopesInput { identity, prepared },
            )
        });
        Self { key, previous }
    }
}

impl Drop for StagedPhase1InputGuard {
    fn drop(&mut self) {
        STAGED_PHASE1_PACKAGE_SCOPES.with(|staged| {
            let mut staged = staged.borrow_mut();
            staged.remove(&self.key);
            if let Some(previous) = self.previous.take() {
                staged.insert(self.key.clone(), previous);
            }
        });
    }
}

struct OwnedPhase1ModuleInput {
    filtered: TargetFilterResult,
    ast_ids: sai_syntax::ast::AstIdMap,
    root: sai_syntax::SyntaxNode,
    source: Arc<SourceFile>,
    source_ref: SourceRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DetachedWorkspaceDiagnostic {
    code: sai_diagnostics::Code,
    severity: sai_diagnostics::Severity,
    message: String,
    primary: Option<sai_diagnostics::Location>,
    primary_label: String,
    labels: Vec<Label>,
    children: Vec<Child>,
    related: Vec<Related>,
    data: sai_diagnostics::ValueObject,
    node: Option<String>,
    symbol: Option<String>,
    package: Option<String>,
    cause: CauseKey,
    level: Option<LevelInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Phase1PackageScopesMemo {
    scopes: Phase1PackageScopes,
    detached_workspace_diagnostics: Vec<DetachedWorkspaceDiagnostic>,
}

#[derive(Clone)]
struct Phase1PackageScopesQueryOutput {
    identity: Arc<Phase1PackageScopesIdentity>,
    input_fingerprint: Fingerprint,
    result: Result<Phase1PackageScopesMemo, Phase1CollectionError>,
}

impl PartialEq for Phase1PackageScopesQueryOutput {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.result == other.result
    }
}

impl Eq for Phase1PackageScopesQueryOutput {}

impl StableHash for Phase1PackageScopesQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.input_fingerprint.stable_hash(hasher);
    }
}

struct Phase1PackageScopesQueries {
    input: InputQuery<PackageKey, Phase1PackageScopesIdentity>,
    query: DerivedQuery<PackageKey, Phase1PackageScopesQueryOutput>,
}

impl Phase1PackageScopesQueries {
    fn new(packages: Arc<PackageInterner>, symbols: Arc<Interner>) -> Result<Self, QueryError> {
        let input = InputQuery::<PackageKey, Phase1PackageScopesIdentity>::new(
            "package_scopes_input",
            Durability::Low,
        )?;
        let query_input = input.clone();
        let query = DerivedQuery::new(
            "package_scopes",
            move |database: &mut QueryDatabase, key: &PackageKey| {
                let identity = query_input.get(database, key.clone())?;
                let staged = STAGED_PHASE1_PACKAGE_SCOPES.with(|staged| {
                    staged
                        .borrow()
                        .get(key)
                        .filter(|staged| staged.identity.as_ref() == identity.as_ref())
                        .map(|staged| Arc::clone(&staged.prepared))
                });
                let Some(prepared) = staged else {
                    return Err(QueryError::MissingInput {
                        query: "package_scopes_prepared_inputs".to_owned(),
                    });
                };
                let result = collect_prepared_phase1_package_scopes(&prepared, &packages, &symbols);
                Ok(Phase1PackageScopesQueryOutput {
                    input_fingerprint: fingerprint(identity.as_ref()),
                    identity,
                    result,
                })
            },
        )?;
        Ok(Self { input, query })
    }
}

impl StableHash for Phase1PackageScopesIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD1);
        self.package.stable_hash(hasher);
        self.modules.stable_hash(hasher);
        self.namespaces.stable_hash(hasher);
        self.target_name.stable_hash(hasher);
        self.target_capabilities.stable_hash(hasher);
        self.build_context.is_root_package.stable_hash(hasher);
        self.build_context
            .is_root_core_module_build
            .stable_hash(hasher);
        self.build_context.is_test_build.stable_hash(hasher);
        self.build_context.analyzes_tests.stable_hash(hasher);
        self.declared_requirements.stable_hash(hasher);
        self.package_graph.stable_hash(hasher);
        self.foreign_packages.stable_hash(hasher);
    }
}

impl StableHash for Phase1ForeignPackageIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xDB);
        self.visible_identity.stable_hash(hasher);
        self.package_key.stable_hash(hasher);
        self.input_fingerprint.stable_hash(hasher);
    }
}

impl StableHash for Phase1PackageGraphProjection {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        match self {
            Self::Package { outgoing_edges } => {
                hasher.write_tag(0xD8);
                outgoing_edges.stable_hash(hasher);
            }
            Self::ImplicitToolchain { version } => {
                hasher.write_tag(0xD9);
                version.to_string().stable_hash(hasher);
            }
        }
    }
}

impl StableHash for StableActivePackageEdge {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xDA);
        self.visible_identity.stable_hash(hasher);
        self.target.stable_hash(hasher);
        self.target_version.to_string().stable_hash(hasher);
        self.target_form.stable_hash(hasher);
        self.target_role.stable_hash(hasher);
    }
}

impl StableHash for Phase1ModuleIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD2);
        self.path.stable_hash(hasher);
        self.file_id.stable_hash(hasher);
        self.source_path.stable_hash(hasher);
        self.source_package.stable_hash(hasher);
        self.document_version.stable_hash(hasher);
        self.source_bytes.stable_hash(hasher);
        self.parent_dir.stable_hash(hasher);
        self.children.stable_hash(hasher);
    }
}

impl StableHash for Phase1NamespaceIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD3);
        self.path.stable_hash(hasher);
        self.children.stable_hash(hasher);
    }
}

impl StableHash for StableChildEntry {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD4);
        match self {
            Self::Module(module) => {
                hasher.write_tag(0);
                module.stable_hash(hasher);
            }
            Self::Namespace(namespace) => {
                hasher.write_tag(1);
                namespace.stable_hash(hasher);
            }
            Self::Both(module, namespace) => {
                hasher.write_tag(2);
                module.stable_hash(hasher);
                namespace.stable_hash(hasher);
            }
        }
    }
}

/// Owns the CLI's cancellation state and installs the process interrupt handler.
pub struct InterruptHandler {
    cancellation: CancellationToken,
}

pub type CancellationToken = sai_query::CancellationToken;

/// Builds the target facts used by `sai check` for a target's core-module
/// analysis.
///
/// The check command analyzes the core-module column for core targets.
/// `host` therefore applies only to core targets; WASI targets have no
/// artifact-specific predicates here. Wasm64 always supplies `memory64`, and
/// native targets supply `native_ffi` and `process`.
pub fn target_cfg_for_check(target: &str) -> Result<TargetCfg, TargetCfgError> {
    let capabilities: &[&str] = match target {
        "wasm32-core" => &["host"],
        "wasm64-core" => &["host", "memory64"],
        "wasm32-wasi" => &[],
        "wasm64-wasi" => &["memory64"],
        "x86_64-linux" | "aarch64-linux" | "aarch64-macos" | "x86_64-windows" => {
            &["native_ffi", "process"]
        }
        _ => &[],
    };
    TargetCfg::new(target, capabilities.iter().copied())
}

impl InterruptHandler {
    /// Installs SIGINT/Ctrl-C handling. Every interrupt requests cancellation;
    /// filesystem owners then unwind through their cleanup paths.
    pub fn install() -> Result<Self, ctrlc::Error> {
        let cancellation = CancellationToken::default();
        let handler_cancellation = cancellation.clone();
        ctrlc::set_handler(move || {
            handler_cancellation.cancel();
        })?;
        Ok(Self { cancellation })
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

/// The package creation operations used by the command-line frontend.
///
/// Keeping this small facade in the driver lets `sai_cli` depend on only one
/// runtime crate while package filesystem work remains owned by `sai_package`.
pub mod new_package {
    use sai_query::CancellationToken;
    use std::path::Path;

    pub use sai_package::new_package::{
        NewPackageError, NewPackageFile, NewPackageReceipt, create_new_package,
        validate_new_package_identity,
    };

    /// Creates a package while honoring the driver's interrupt token at
    /// filesystem safe points.
    pub fn create_new_package_with_cancellation(
        destination: &Path,
        identity: &str,
        targets: &[String],
        cancellation: &CancellationToken,
    ) -> Result<NewPackageReceipt, NewPackageError> {
        sai_package::new_package::create_new_package_with_cancel_check(
            destination,
            identity,
            targets,
            || cancellation.is_cancelled(),
        )
    }
}

/// The package value validators exposed to command-line argument handling.
pub mod package_validation {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum GitUrlValidationError {
        Insecure,
        Unsupported,
        Credentials,
    }

    pub fn is_valid_identity(value: &str) -> bool {
        sai_package::identity::PackageIdentity::parse(value).is_ok()
    }

    pub fn is_standard_package_identity(value: &str) -> bool {
        matches!(
            value,
            "sai:collections"
                | "sai:text"
                | "sai:fmt"
                | "sai:io"
                | "sai:fs"
                | "sai:net"
                | "sai:http"
                | "sai:async"
                | "sai:process"
        )
    }

    pub fn is_valid_exact_version(value: &str) -> bool {
        sai_package::version::Version::parse(value).is_ok()
    }

    pub fn is_valid_version_requirement(value: &str) -> bool {
        sai_package::version_req::VersionReq::parse(value).is_ok()
    }

    pub fn is_supported_git_url(value: &str) -> bool {
        validate_git_url(value).is_ok()
    }

    pub fn validate_git_url(value: &str) -> Result<(), GitUrlValidationError> {
        sai_package::git_remote::validate_git_url(value).map_err(|error| match error {
            sai_package::git_remote::GitUrlValidationError::Insecure => {
                GitUrlValidationError::Insecure
            }
            sai_package::git_remote::GitUrlValidationError::Unsupported => {
                GitUrlValidationError::Unsupported
            }
            sai_package::git_remote::GitUrlValidationError::Credentials => {
                GitUrlValidationError::Credentials
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootDatabaseError {
    Query(QueryError),
    ModuleTree(ModuleTreeQueryError),
    Package(PackageIdError),
    PackageKey(PackageKeyError),
    FileInterner(FileInternerError),
    /// The numeric file index is outside this database's file interner.
    FileNotInterned,
    PackageDiagnostics(MvsDuplicatePackageLineDiagnosticError),
    MissingPackageManifest(MvsPackageVersion),
    UnexpectedPackageManifest(MvsPackageVersion),
    MixedSourceAuditLoadMismatch,
    MixedSourceManifestContractMismatch(MvsPackageVersion),
    FinalManifestSourceDecode {
        package_version: MvsPackageVersion,
        error: SourceDecodeError,
    },
    FinalManifestSnapshot(WorkspaceSnapshotError),
    FinalManifestFileIdsExhausted,
    FinalManifestPathOrdinalsExhausted,
    PackageManifestFileKeyMismatch {
        file_id: FileId,
        expected_package_key: String,
        found_package_key: String,
        found_path: String,
    },
}

/// A failure while assembling snapshot-bound inputs for Phase 1 collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Phase1CollectionError {
    Parse(ParseQueryError),
    ItemTree(ItemTreeQueryError),
    TargetFilter(TargetFilterError),
    PackageScopes(PackageScopesError),
    UseTargetDiagnostics(UseTargetDiagnosticsError),
    DefPathInterner(DefPathInternerError),
    MissingSnapshotFile(FileId),
    TooManyModules,
    SnapshotBoundDiagnosticHasFixes,
    Query(QueryError),
    DeclaredRequirements(DeclaredRequirementsError),
    Package(PackageIdError),
    PackageGraphMissingNode(PackageKey),
    PackageGraphRequirementEdgeMismatch {
        package: PackageKey,
        identity: String,
        expected_active: bool,
        found_active: bool,
    },
    ForeignPackageScopeMismatch {
        package: PackageKey,
        visible_identity: String,
        target: PackageKey,
    },
}

/// A finalized, direct source dependency scope available to one package's
/// Phase 2–3 import resolution.
///
/// `visible_identity` and `package_key` come from the requiring package's
/// exact active graph edge. The selected key is retained so query identity and
/// graph validation never resolve a dependency by identity alone.
#[derive(Clone, Debug)]
pub struct Phase1ForeignPackageScopes {
    pub visible_identity: String,
    pub package_key: PackageKey,
    pub module_tree: Arc<ModuleTree>,
    pub scopes: Arc<Phase1PackageScopes>,
    pub input_fingerprint: Fingerprint,
}

/// The finalized package scopes and the request provenance needed to bind
/// their owner descriptors to the same source snapshot and target settings.
#[derive(Clone, Debug)]
pub struct Phase1PackageScopesCollection {
    scopes: Phase1PackageScopes,
    input_fingerprint: Fingerprint,
    snapshot_id: WorkspaceSnapshotId,
    target_cfg: TargetCfg,
    module_tree: ModuleTree,
}

impl Phase1PackageScopesCollection {
    pub const fn input_fingerprint(&self) -> Fingerprint {
        self.input_fingerprint
    }

    pub fn scopes(&self) -> &Phase1PackageScopes {
        &self.scopes
    }

    pub fn into_scopes(self) -> Phase1PackageScopes {
        self.scopes
    }
}

/// Owned loop-label and local-name results for one top-level function owner.
///
/// The temporary HIR arena and snapshot-bound source map used to produce this
/// value are discarded before it is returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionOwnerResolution {
    owner: OwnerId,
    resolution: Result<LabelResolution, FunctionOwnerResolutionError>,
    local_resolution: Result<Option<LocalResolution>, FunctionOwnerResolutionError>,
    /// Diagnostics produced while lowering the owner, kept separate from the
    /// label resolver's diagnostics so the caller can preserve phase order.
    lowering_diagnostics: Vec<Diagnostic>,
}

impl FunctionOwnerResolution {
    pub const fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn resolution(&self) -> Result<&LabelResolution, &FunctionOwnerResolutionError> {
        self.resolution.as_ref()
    }

    pub fn local_resolution(
        &self,
    ) -> Result<Option<&LocalResolution>, &FunctionOwnerResolutionError> {
        self.local_resolution
            .as_ref()
            .map(|resolution| resolution.as_ref())
    }

    pub fn lowering_diagnostics(&self) -> &[Diagnostic] {
        &self.lowering_diagnostics
    }
}

/// Compatibility name for the original label result type.
pub type FunctionOwnerLabelResolution = FunctionOwnerResolution;

/// Per-owner label and local results tagged with the request provenance they
/// belong to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionOwnerResolutions {
    snapshot_id: WorkspaceSnapshotId,
    target_cfg: TargetCfg,
    collection_input_fingerprint: Fingerprint,
    owners: Vec<FunctionOwnerResolution>,
}

impl FunctionOwnerResolutions {
    pub const fn snapshot_id(&self) -> WorkspaceSnapshotId {
        self.snapshot_id
    }

    pub fn matches_snapshot(&self, snapshot: &WorkspaceSnapshot) -> bool {
        self.snapshot_id == snapshot.id()
    }

    /// Checks whether these results were produced for the given package
    /// collection, including its non-snapshot package inputs.
    pub fn matches_collection(&self, collection: &Phase1PackageScopesCollection) -> bool {
        self.snapshot_id == collection.snapshot_id
            && self.target_cfg == collection.target_cfg
            && self.collection_input_fingerprint == collection.input_fingerprint
    }

    pub fn target_cfg(&self) -> &TargetCfg {
        &self.target_cfg
    }

    pub fn owners(&self) -> &[FunctionOwnerResolution] {
        &self.owners
    }
}

/// Compatibility name for the original label result collection.
pub type FunctionOwnerLabelResolutions = FunctionOwnerResolutions;

/// Predicate diagnostics and local-name resolutions produced for one
/// top-level owner.
///
/// The temporary HIR arena and source map used to produce these results are
/// discarded before the value is returned. Predicate diagnostics and local
/// name results are independent so a local-resolution failure does not hide
/// owner-specific predicate diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredicateOwnerResolution {
    owner: OwnerId,
    resolution: Result<Vec<Diagnostic>, OwnerControlFlowResolutionError>,
    local_resolution: Result<Option<LocalResolution>, OwnerControlFlowResolutionError>,
    /// Diagnostics produced while lowering the owner, kept separate from the
    /// predicate resolver's diagnostics so the caller can preserve phase
    /// order.
    lowering_diagnostics: Vec<Diagnostic>,
}

impl PredicateOwnerResolution {
    pub const fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn resolution(&self) -> Result<&[Diagnostic], &OwnerControlFlowResolutionError> {
        self.resolution.as_deref()
    }

    pub fn local_resolution(
        &self,
    ) -> Result<Option<&LocalResolution>, &OwnerControlFlowResolutionError> {
        self.local_resolution
            .as_ref()
            .map(|resolution| resolution.as_ref())
    }

    pub fn lowering_diagnostics(&self) -> &[Diagnostic] {
        &self.lowering_diagnostics
    }
}

/// Per-owner predicate results tagged with request provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredicateOwnerResolutions {
    snapshot_id: WorkspaceSnapshotId,
    target_cfg: TargetCfg,
    collection_input_fingerprint: Fingerprint,
    owners: Vec<PredicateOwnerResolution>,
}

impl PredicateOwnerResolutions {
    pub const fn snapshot_id(&self) -> WorkspaceSnapshotId {
        self.snapshot_id
    }

    pub fn matches_snapshot(&self, snapshot: &WorkspaceSnapshot) -> bool {
        self.snapshot_id == snapshot.id()
    }

    /// Checks whether these results were produced for the given package
    /// collection, including its non-snapshot package inputs.
    pub fn matches_collection(&self, collection: &Phase1PackageScopesCollection) -> bool {
        self.snapshot_id == collection.snapshot_id
            && self.target_cfg == collection.target_cfg
            && self.collection_input_fingerprint == collection.input_fingerprint
    }

    pub fn target_cfg(&self) -> &TargetCfg {
        &self.target_cfg
    }

    pub fn owners(&self) -> &[PredicateOwnerResolution] {
        &self.owners
    }
}

/// Failure while lowering or resolving a current-snapshot function or
/// predicate owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnerControlFlowResolutionError {
    SnapshotMismatch,
    ModuleTreeMismatch,
    MissingSnapshotFile(FileId),
    Parse(ParseQueryError),
    ItemTree(ItemTreeQueryError),
    TargetFilter(TargetFilterError),
    DefPathInterner(DefPathInternerError),
    MissingDefPath(OwnerId),
    DescriptorNotInCurrentTarget {
        owner: OwnerId,
        file: FileId,
    },
    TopLevelOwnerDescriptorMismatch {
        owner: OwnerId,
        file: FileId,
    },
    TopLevelOwnerKindMismatch {
        owner: OwnerId,
        file: FileId,
        descriptor: TopLevelOwnerKind,
        current: ItemTreeKind,
    },
    MissingModuleScope(FileId),
    Lowering(HirLoweringError),
    Labels(LabelResolutionError),
    Locals(LocalResolutionError),
}

/// Compatibility name for the original function label adapter error.
pub type FunctionOwnerResolutionError = OwnerControlFlowResolutionError;

/// Compatibility name for the original function label adapter error.
pub type FunctionOwnerLabelResolutionError = FunctionOwnerResolutionError;

struct CurrentOwnerInputs {
    source: Arc<SourceFile>,
    parsed: Parse,
    ast_ids: ast::AstIdMap,
    filtered_item_tree: ItemTree,
    retained_item_kinds: HashMap<ast::ErasedAstId, ItemTreeKind>,
    source_ref: SourceRef,
}

fn index_top_level_owner_descriptors(
    collection: &Phase1PackageScopesCollection,
) -> HashMap<OwnerId, Option<TopLevelOwnerDescriptor>> {
    let mut index = HashMap::with_capacity(collection.scopes.top_level_owners.len());
    for descriptor in &collection.scopes.top_level_owners {
        if let Some(indexed) = index.get_mut(&descriptor.owner) {
            *indexed = None;
        } else {
            index.insert(descriptor.owner, Some(*descriptor));
        }
    }
    index
}

fn find_top_level_owner_descriptor(
    index: &HashMap<OwnerId, Option<TopLevelOwnerDescriptor>>,
    owner: OwnerId,
    file: FileId,
    item_id: ast::ErasedAstId,
    kind: TopLevelOwnerKind,
) -> Result<TopLevelOwnerDescriptor, OwnerControlFlowResolutionError> {
    match index.get(&owner).and_then(|descriptor| *descriptor) {
        Some(descriptor)
            if descriptor.file == file
                && descriptor.item_id == item_id
                && descriptor.kind == kind =>
        {
            Ok(descriptor)
        }
        _ => Err(OwnerControlFlowResolutionError::TopLevelOwnerDescriptorMismatch { owner, file }),
    }
}

fn item_tree_kind_for_top_level_owner(kind: TopLevelOwnerKind) -> ItemTreeKind {
    match kind {
        TopLevelOwnerKind::Function => ItemTreeKind::Function,
        TopLevelOwnerKind::Struct => ItemTreeKind::Struct,
        TopLevelOwnerKind::Alias => ItemTreeKind::Alias,
        TopLevelOwnerKind::Refined => ItemTreeKind::Refined,
        TopLevelOwnerKind::Enum => ItemTreeKind::Enum,
        TopLevelOwnerKind::Distinct => ItemTreeKind::Distinct,
        TopLevelOwnerKind::Resource => ItemTreeKind::Resource,
        TopLevelOwnerKind::Trait => ItemTreeKind::Trait,
        TopLevelOwnerKind::Impl => ItemTreeKind::Impl,
        TopLevelOwnerKind::Const => ItemTreeKind::Const,
        TopLevelOwnerKind::Invariant => ItemTreeKind::Invariant,
        TopLevelOwnerKind::Effect => ItemTreeKind::Effect,
        TopLevelOwnerKind::Interface => ItemTreeKind::Interface,
        TopLevelOwnerKind::World => ItemTreeKind::World,
        TopLevelOwnerKind::ExternBlock => ItemTreeKind::ExternBlock,
        TopLevelOwnerKind::Test => ItemTreeKind::Test,
        TopLevelOwnerKind::Property => ItemTreeKind::Property,
    }
}

fn lower_top_level_owner<'hir>(
    descriptor: TopLevelOwnerDescriptor,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    retained_item_kinds: &HashMap<ast::ErasedAstId, ItemTreeKind>,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), OwnerControlFlowResolutionError> {
    let Some(current_kind) = retained_item_kinds.get(&descriptor.item_id).copied() else {
        return Err(
            OwnerControlFlowResolutionError::DescriptorNotInCurrentTarget {
                owner: descriptor.owner,
                file: descriptor.file,
            },
        );
    };
    let expected_kind = item_tree_kind_for_top_level_owner(descriptor.kind);
    if current_kind != expected_kind {
        return Err(OwnerControlFlowResolutionError::TopLevelOwnerKindMismatch {
            owner: descriptor.owner,
            file: descriptor.file,
            descriptor: descriptor.kind,
            current: current_kind,
        });
    }
    let lowered = match descriptor.kind {
        TopLevelOwnerKind::Function => lower_function_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Struct => lower_struct_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Alias => lower_alias_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Refined => lower_refined_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Enum => lower_enum_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Distinct => lower_distinct_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Resource => lower_resource_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Trait => lower_trait_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Impl => lower_impl_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Const => lower_const_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Invariant => lower_invariant_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Effect => lower_effect_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Interface => lower_interface_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::World => lower_world_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::ExternBlock => lower_extern_block_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Test => lower_test_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
        TopLevelOwnerKind::Property => lower_property_owner(
            descriptor.owner,
            expected_path,
            item_tree,
            descriptor.item_id,
            context,
        ),
    };
    lowered.map_err(OwnerControlFlowResolutionError::Lowering)
}

fn module_aliases_for_file(
    collection: &Phase1PackageScopesCollection,
    module_tree: &ModuleTree,
    file: FileId,
) -> Result<HashSet<Symbol>, OwnerControlFlowResolutionError> {
    if collection.scopes.modules.len() != module_tree.modules.len() {
        return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
    }
    let mut matching_modules = module_tree
        .modules
        .iter()
        .enumerate()
        .filter(|(_, module)| module.file == file);
    let Some((index, _)) = matching_modules.next() else {
        return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
    };
    if matching_modules.next().is_some() {
        return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
    }
    let Ok(raw_id) = u32::try_from(index) else {
        return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
    };
    let module_id = ModuleId::from_raw(raw_id);
    let module_scope = collection
        .scopes
        .module(module_id)
        .ok_or(OwnerControlFlowResolutionError::MissingModuleScope(file))?;
    Ok(module_scope
        .scope
        .iter()
        .filter_map(|(name, binding)| {
            let import_index = match binding.origin {
                BindingOrigin::Decl => return None,
                BindingOrigin::Use { import } | BindingOrigin::PubUse { import } => import,
            };
            let import = module_scope.imports.get(import_index.0 as usize)?;
            if import.item_index.is_some() {
                return None;
            }

            let is_resolved_module_alias = matches!(
                binding.res,
                Res::Def(DefKind::Module | DefKind::Interface, _)
            );
            let is_unresolved_test_only_module_alias = import.test_only
                && matches!(binding.res, Res::TestOnly(res_import) if res_import == import_index);
            (is_resolved_module_alias || is_unresolved_test_only_module_alias).then_some(*name)
        })
        .collect())
}

impl From<ParseQueryError> for Phase1CollectionError {
    fn from(error: ParseQueryError) -> Self {
        Self::Parse(error)
    }
}

impl From<ItemTreeQueryError> for Phase1CollectionError {
    fn from(error: ItemTreeQueryError) -> Self {
        Self::ItemTree(error)
    }
}

impl From<TargetFilterError> for Phase1CollectionError {
    fn from(error: TargetFilterError) -> Self {
        Self::TargetFilter(error)
    }
}

impl From<PackageScopesError> for Phase1CollectionError {
    fn from(error: PackageScopesError) -> Self {
        Self::PackageScopes(error)
    }
}

impl From<UseTargetDiagnosticsError> for Phase1CollectionError {
    fn from(error: UseTargetDiagnosticsError) -> Self {
        Self::UseTargetDiagnostics(error)
    }
}

impl From<DefPathInternerError> for Phase1CollectionError {
    fn from(error: DefPathInternerError) -> Self {
        Self::DefPathInterner(error)
    }
}

impl From<QueryError> for Phase1CollectionError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<DeclaredRequirementsError> for Phase1CollectionError {
    fn from(error: DeclaredRequirementsError) -> Self {
        Self::DeclaredRequirements(error)
    }
}

impl From<PackageIdError> for Phase1CollectionError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

fn phase1_package_scopes_identity(
    package: PackageKey,
    module_tree: &ModuleTree,
    modules: &[OwnedPhase1ModuleInput],
    target_cfg: &TargetCfg,
    build_context: Phase1BuildContext,
    declared_requirements: Option<&[DeclaredRequirement]>,
    package_graph: Option<Phase1PackageGraphProjection>,
    foreign_packages: &[Phase1ForeignPackageScopes],
    symbols: &Interner,
) -> Result<Phase1PackageScopesIdentity, Phase1CollectionError> {
    let modules = module_tree
        .modules
        .iter()
        .zip(modules)
        .map(|(module, input)| {
            let path = module
                .path
                .iter()
                .map(|symbol| {
                    symbols
                        .resolve(*symbol)
                        .map(|name| name.to_string())
                        .map_err(PackageScopesError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let children = module
                .children
                .iter()
                .map(|(name, entry)| (name.clone(), stable_child_entry(*entry)))
                .collect();
            Ok(Phase1ModuleIdentity {
                path,
                file_id: module.file.raw(),
                source_path: input.source_ref.path.clone(),
                source_package: input.source_ref.package.clone(),
                document_version: input.source_ref.snapshot,
                source_bytes: Arc::from(input.source.raw_bytes()),
                parent_dir: module.parent_dir.map(ModuleId::raw),
                children,
            })
        })
        .collect::<Result<Vec<_>, Phase1CollectionError>>()?;
    let namespaces = module_tree
        .namespaces
        .iter()
        .map(|namespace| {
            let path = namespace
                .path
                .iter()
                .map(|symbol| {
                    symbols
                        .resolve(*symbol)
                        .map(|name| name.to_string())
                        .map_err(PackageScopesError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let children = namespace
                .children
                .iter()
                .map(|(name, entry)| (name.clone(), stable_child_entry(*entry)))
                .collect();
            Ok(Phase1NamespaceIdentity { path, children })
        })
        .collect::<Result<Vec<_>, Phase1CollectionError>>()?;

    let mut foreign_packages = foreign_packages
        .iter()
        .map(|foreign| Phase1ForeignPackageIdentity {
            visible_identity: foreign.visible_identity.clone(),
            package_key: foreign.package_key.clone(),
            input_fingerprint: foreign.input_fingerprint,
        })
        .collect::<Vec<_>>();
    foreign_packages.sort_by(|left, right| {
        left.visible_identity
            .as_bytes()
            .cmp(right.visible_identity.as_bytes())
            .then_with(|| left.package_key.cmp(&right.package_key))
    });

    Ok(Phase1PackageScopesIdentity {
        package,
        modules,
        namespaces,
        target_name: target_cfg.target_name().to_owned(),
        target_capabilities: target_cfg.capabilities().clone(),
        build_context,
        declared_requirements: declared_requirements.map(|requirements| requirements.to_vec()),
        package_graph,
        foreign_packages,
    })
}

fn phase1_package_graph_projection(
    graph: Option<&PackageGraph>,
    package: &PackageKey,
) -> Result<Option<Phase1PackageGraphProjection>, Phase1CollectionError> {
    let Some(graph) = graph else {
        return Ok(None);
    };
    if matches!(package.as_str(), "sai:core" | "sai:alloc") {
        return Ok(Some(Phase1PackageGraphProjection::ImplicitToolchain {
            version: graph.toolchain_version().clone(),
        }));
    }

    let node = graph
        .node(package)
        .ok_or_else(|| Phase1CollectionError::PackageGraphMissingNode(package.clone()))?;
    let outgoing_edges = node
        .active_edges()
        .iter()
        .map(|edge| {
            let target = graph.node(edge.target()).ok_or_else(|| {
                Phase1CollectionError::PackageGraphMissingNode(edge.target().clone())
            })?;
            Ok(StableActivePackageEdge {
                visible_identity: edge.visible_identity().to_owned(),
                target: target.key().clone(),
                target_version: target.version().clone(),
                target_form: target.form(),
                target_role: target.role(),
            })
        })
        .collect::<Result<Vec<_>, Phase1CollectionError>>()?;

    Ok(Some(Phase1PackageGraphProjection::Package {
        outgoing_edges,
    }))
}

fn validate_phase1_package_graph_requirements(
    package: &PackageKey,
    graph: Option<&Phase1PackageGraphProjection>,
    declared_requirements: Option<&[DeclaredRequirement]>,
    target_cfg: &TargetCfg,
    build_context: Phase1BuildContext,
) -> Result<(), Phase1CollectionError> {
    let (Some(Phase1PackageGraphProjection::Package { outgoing_edges }), Some(requirements)) =
        (graph, declared_requirements)
    else {
        return Ok(());
    };

    let active_edges = outgoing_edges
        .iter()
        .map(|edge| edge.visible_identity.clone())
        .collect::<BTreeSet<_>>();
    let expected_active_edges = requirements
        .iter()
        .filter(|requirement| !is_toolchain_package_identity(&requirement.identity))
        .filter(|requirement| {
            requirement.only.as_ref().map_or(true, |targets| {
                targets
                    .iter()
                    .any(|target| target == target_cfg.target_name())
            }) && (!requirement.test
                || (build_context.analyzes_tests && build_context.is_root_package))
        })
        .map(|requirement| requirement.identity.clone())
        .collect::<BTreeSet<_>>();

    let identities = active_edges
        .union(&expected_active_edges)
        .cloned()
        .collect::<Vec<_>>();
    for identity in identities {
        let expected_active = expected_active_edges.contains(&identity);
        let found_active = active_edges.contains(&identity);
        if expected_active != found_active {
            return Err(Phase1CollectionError::PackageGraphRequirementEdgeMismatch {
                package: package.clone(),
                identity,
                expected_active,
                found_active,
            });
        }
    }
    Ok(())
}

fn validate_phase1_foreign_package_scopes(
    package: &PackageKey,
    graph: Option<&Phase1PackageGraphProjection>,
    foreign_scopes: &[Phase1ForeignPackageScopes],
) -> Result<(), Phase1CollectionError> {
    let mut identities = BTreeSet::new();
    for foreign in foreign_scopes {
        let valid_edge = match graph {
            Some(Phase1PackageGraphProjection::Package { outgoing_edges }) => {
                outgoing_edges.iter().any(|edge| {
                    edge.visible_identity == foreign.visible_identity
                        && edge.target == foreign.package_key
                        && edge.target_form == PackageForm::Source
                })
            }
            Some(Phase1PackageGraphProjection::ImplicitToolchain { .. }) | None => false,
        };
        if !valid_edge
            || !identities.insert(foreign.visible_identity.as_str())
            || foreign.module_tree.modules.len() != foreign.scopes.modules.len()
        {
            return Err(Phase1CollectionError::ForeignPackageScopeMismatch {
                package: package.clone(),
                visible_identity: foreign.visible_identity.clone(),
                target: foreign.package_key.clone(),
            });
        }
    }
    Ok(())
}

fn is_toolchain_package_identity(identity: &str) -> bool {
    matches!(
        identity,
        "sai:core"
            | "sai:alloc"
            | "wasi:cli"
            | "wasi:clocks"
            | "wasi:filesystem"
            | "wasi:http"
            | "wasi:io"
            | "wasi:random"
            | "wasi:sockets"
    )
}

fn stable_child_entry(entry: sai_resolve::ChildEntry) -> StableChildEntry {
    match entry {
        sai_resolve::ChildEntry::Module(module) => StableChildEntry::Module(module.raw()),
        sai_resolve::ChildEntry::Namespace(namespace) => {
            StableChildEntry::Namespace(namespace.raw())
        }
        sai_resolve::ChildEntry::Both(module, namespace) => {
            StableChildEntry::Both(module.raw(), namespace.raw())
        }
    }
}

fn current_phase1_target_filter_diagnostics(input: &OwnedPhase1ModuleInput) -> Vec<Diagnostic> {
    fn suppress_entry_diagnostics(
        entry: &sai_hir::item_tree::ItemTreeEntry,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        suppress_export_name_feature_gate(&entry.raw_attrs, diagnostics);
        for child in &entry.children {
            suppress_entry_diagnostics(child, diagnostics);
        }
    }

    let mut diagnostics = input.filtered.diagnostics.clone();
    for use_entry in &input.filtered.item_tree.uses {
        suppress_export_name_feature_gate(&use_entry.raw_attrs, &mut diagnostics);
    }
    for entry in &input.filtered.item_tree.items {
        suppress_entry_diagnostics(entry, &mut diagnostics);
    }
    diagnostics
}

fn collect_prepared_phase1_package_scopes(
    prepared: &PreparedPhase1PackageScopes,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<Phase1PackageScopesMemo, Phase1CollectionError> {
    if prepared.module_tree.modules.len() > u32::MAX as usize {
        return Err(Phase1CollectionError::TooManyModules);
    }
    let module_inputs = prepared
        .modules
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let raw = u32::try_from(index).map_err(|_| Phase1CollectionError::TooManyModules)?;
            Ok((
                ModuleId::from_raw(raw),
                FilteredModule {
                    filtered: &input.filtered,
                    ast_ids: &input.ast_ids,
                    root: &input.root,
                    source: input.source.as_ref(),
                    source_ref: input.source_ref.clone(),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Phase1CollectionError>>()?;

    let mut scopes = collect_phase1(
        &prepared.module_tree,
        &module_inputs,
        Phase1CollectionConfig {
            snapshot: &prepared.snapshot,
            target_cfg: &prepared.target_cfg,
            build_context: prepared.build_context,
        },
        packages,
        symbols,
        &prepared.def_paths,
    )?;
    // Target filtering runs on every public request so its diagnostics always
    // carry that request's WorkspaceSnapshot. Keep those values out of the
    // package-scope memo; the adapter appends the freshly prepared diagnostics.
    for diagnostic in prepared
        .modules
        .iter()
        .flat_map(|input| &input.filtered.diagnostics)
    {
        if let Some(index) = scopes
            .diagnostics
            .iter()
            .position(|candidate| candidate == diagnostic)
        {
            scopes.diagnostics.remove(index);
        }
    }
    let use_target_diagnostics = collect_local_use_target_diagnostics(
        &prepared.module_tree,
        &module_inputs,
        &scopes,
        packages,
        symbols,
    )?;
    scopes.diagnostics.extend(use_target_diagnostics);
    let requirements = prepared
        .declared_requirements
        .as_deref()
        .map(|requirements| {
            requirements
                .iter()
                .map(|requirement| PackageUseRequirement {
                    identity: requirement.identity.clone(),
                    declaration_order: requirement.declaration_order,
                    only: requirement.only.clone(),
                    test: requirement.test,
                })
                .collect::<Vec<_>>()
        });
    let foreign_scopes = prepared
        .foreign_packages
        .iter()
        .map(|foreign| ForeignPackageScope {
            visible_identity: &foreign.visible_identity,
            module_tree: &foreign.module_tree,
            scopes: &foreign.scopes,
        })
        .collect::<Vec<_>>();
    let module_alias_diagnostics = collect_module_alias_bindings_with_foreign_scopes(
        &prepared.module_tree,
        &module_inputs,
        &mut scopes,
        packages,
        symbols,
        &prepared.def_paths,
        requirements.as_deref(),
        &prepared.target_cfg,
        prepared.build_context.analyzes_tests,
        prepared.build_context.is_root_package,
        &foreign_scopes,
    )?;
    scopes.diagnostics.extend(module_alias_diagnostics);
    sort_phase1_diagnostics(&mut scopes.diagnostics);

    // Snapshot-bound diagnostics cannot be retained in this package memo.
    // Store their source-relative structure, then validate and bind them to
    // the requesting workspace snapshot in the public adapter.
    let mut detached_workspace_diagnostics = Vec::new();
    let mut snapshot_independent_diagnostics = Vec::new();
    for diagnostic in scopes.diagnostics.drain(..) {
        if diagnostic.has_workspace_snapshot() {
            detached_workspace_diagnostics
                .push(DetachedWorkspaceDiagnostic::from_diagnostic(&diagnostic)?);
        } else {
            snapshot_independent_diagnostics.push(diagnostic);
        }
    }
    scopes.diagnostics = snapshot_independent_diagnostics;
    Ok(Phase1PackageScopesMemo {
        scopes,
        detached_workspace_diagnostics,
    })
}

impl DetachedWorkspaceDiagnostic {
    fn from_diagnostic(diagnostic: &Diagnostic) -> Result<Self, Phase1CollectionError> {
        if !diagnostic.fixes().is_empty() {
            return Err(Phase1CollectionError::SnapshotBoundDiagnosticHasFixes);
        }
        Ok(Self {
            code: diagnostic.code,
            severity: diagnostic.severity,
            message: diagnostic.message.clone(),
            primary: diagnostic.primary().cloned(),
            primary_label: diagnostic.primary_label.clone(),
            labels: diagnostic.labels().to_vec(),
            children: diagnostic.children().to_vec(),
            related: diagnostic.related().to_vec(),
            data: diagnostic.data.clone(),
            node: diagnostic.node.clone(),
            symbol: diagnostic.symbol.clone(),
            package: diagnostic.package.clone(),
            cause: diagnostic.cause().clone(),
            level: diagnostic.level().cloned(),
        })
    }

    fn bind(&self, snapshot: &WorkspaceSnapshot) -> Result<Diagnostic, PackageScopesError> {
        let mut diagnostic = Diagnostic::new(
            self.code,
            self.severity,
            self.message.clone(),
            self.primary.clone(),
        );
        diagnostic.primary_label = self.primary_label.clone();
        diagnostic.data = self.data.clone();
        diagnostic.node = self.node.clone();
        diagnostic.symbol = self.symbol.clone();
        diagnostic.package = self.package.clone();
        for label in &self.labels {
            diagnostic
                .push_label(label.clone())
                .map_err(|_| PackageScopesError::InvalidSourceRange)?;
        }
        for child in &self.children {
            diagnostic
                .push_child(child.clone())
                .map_err(|_| PackageScopesError::InvalidSourceRange)?;
        }
        for related in &self.related {
            diagnostic
                .push_related(related.clone())
                .map_err(|_| PackageScopesError::InvalidSourceRange)?;
        }
        diagnostic = diagnostic
            .with_cause(self.cause.clone())
            .map_err(|_| PackageScopesError::InvalidSourceRange)?;
        if let Some(level) = &self.level {
            diagnostic
                .set_level(Some(level.clone()))
                .map_err(|_| PackageScopesError::InvalidSourceRange)?;
        }
        diagnostic
            .with_workspace_snapshot(snapshot)
            .map_err(|_| PackageScopesError::InvalidSourceRange)
    }
}

fn sort_phase1_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| match (left.primary(), right.primary()) {
        (Some(left), Some(right)) => left
            .file
            .path
            .as_bytes()
            .cmp(right.file.path.as_bytes())
            .then_with(|| left.span.range.start().cmp(&right.span.range.start())),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// Reload diagnostics together with the workspace snapshot containing every
/// source file referenced by their locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MixedSourceReloadDiagnostics {
    snapshot: WorkspaceSnapshot,
    diagnostics: Vec<Diagnostic>,
}

impl MixedSourceReloadDiagnostics {
    /// The augmented snapshot used to resolve the diagnostics' file IDs.
    pub fn snapshot(&self) -> &WorkspaceSnapshot {
        &self.snapshot
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_parts(self) -> (WorkspaceSnapshot, Vec<Diagnostic>) {
        (self.snapshot, self.diagnostics)
    }
}

impl std::ops::Deref for MixedSourceReloadDiagnostics {
    type Target = [Diagnostic];

    fn deref(&self) -> &Self::Target {
        &self.diagnostics
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InternedWitFiles {
    NotApplicable,
    Listed(Vec<FileId>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternedPackageFiles {
    source_directory_exists: bool,
    entries: Vec<InternedPackageFileEntry>,
}

impl InternedPackageFiles {
    pub const fn source_directory_exists(&self) -> bool {
        self.source_directory_exists
    }

    pub fn entries(&self) -> &[InternedPackageFileEntry] {
        &self.entries
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InternedPackageFileEntry {
    File {
        path: PackageSourcePath,
        file_id: FileId,
    },
    Directory {
        path: PackageSourcePath,
    },
    Symlink {
        path: PackageSourcePath,
    },
}

impl InternedPackageFileEntry {
    pub fn path(&self) -> &PackageSourcePath {
        match self {
            Self::File { path, .. } | Self::Directory { path } | Self::Symlink { path } => path,
        }
    }

    pub const fn kind(&self) -> PackageFileKind {
        match self {
            Self::File { .. } => PackageFileKind::File,
            Self::Directory { .. } => PackageFileKind::Directory,
            Self::Symlink { .. } => PackageFileKind::Symlink,
        }
    }

    pub const fn file_id(&self) -> Option<FileId> {
        match self {
            Self::File { file_id, .. } => Some(*file_id),
            Self::Directory { .. } | Self::Symlink { .. } => None,
        }
    }
}

impl From<QueryError> for RootDatabaseError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<ModuleTreeQueryError> for RootDatabaseError {
    fn from(error: ModuleTreeQueryError) -> Self {
        Self::ModuleTree(error)
    }
}

impl From<PackageIdError> for RootDatabaseError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<PackageKeyError> for RootDatabaseError {
    fn from(error: PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<FileInternerError> for RootDatabaseError {
    fn from(error: FileInternerError) -> Self {
        Self::FileInterner(error)
    }
}

impl From<MvsDuplicatePackageLineDiagnosticError> for RootDatabaseError {
    fn from(error: MvsDuplicatePackageLineDiagnosticError) -> Self {
        Self::PackageDiagnostics(error)
    }
}

fn mixed_source_diagnostic_snapshot(
    snapshot: &WorkspaceSnapshot,
    audit_plan: &MixedSourceFinalAuditPlan,
) -> Result<(WorkspaceSnapshot, BTreeMap<MvsPackageVersion, FileId>), RootDatabaseError> {
    if audit_plan.final_manifest_snapshots().is_empty() {
        return Ok((snapshot.clone(), BTreeMap::new()));
    }

    let mut files = snapshot.iter().cloned().collect::<Vec<_>>();
    let mut used_file_ids = snapshot
        .iter()
        .map(|file| file.source().id())
        .collect::<BTreeSet<_>>();
    let mut used_paths = snapshot
        .iter()
        .map(|file| file.path().to_owned())
        .collect::<BTreeSet<_>>();
    let mut next_file_id = u32::MAX;
    let mut next_path_ordinal = 0_u64;
    let mut final_manifest_file_ids = BTreeMap::new();

    for (package_version, manifest_bytes) in audit_plan.final_manifest_snapshots() {
        let file_id = loop {
            let candidate = FileId::new(next_file_id);
            if used_file_ids.insert(candidate) {
                break candidate;
            }
            next_file_id = next_file_id
                .checked_sub(1)
                .ok_or(RootDatabaseError::FinalManifestFileIdsExhausted)?;
        };
        let path = loop {
            let candidate =
                MvsManifestLocation::final_snapshot_path(package_version, next_path_ordinal);
            if used_paths.insert(candidate.clone()) {
                break candidate;
            }
            next_path_ordinal = next_path_ordinal
                .checked_add(1)
                .ok_or(RootDatabaseError::FinalManifestPathOrdinalsExhausted)?;
        };
        let source = SourceFile::decode(file_id, manifest_bytes).map_err(|error| {
            RootDatabaseError::FinalManifestSourceDecode {
                package_version: package_version.clone(),
                error,
            }
        })?;
        files.push(SnapshotFile::new(
            Arc::new(source),
            path,
            Some(format!(
                "{}@{}",
                package_version.package(),
                package_version.version()
            )),
            None,
        ));
        final_manifest_file_ids.insert(package_version.clone(), file_id);
    }

    let snapshot =
        WorkspaceSnapshot::new(files).map_err(RootDatabaseError::FinalManifestSnapshot)?;
    Ok((snapshot, final_manifest_file_ids))
}

impl RootDatabase {
    /// Creates a fresh compiler session with empty VFS inputs.
    pub fn new() -> Result<Self, QueryError> {
        let mut query_database = QueryDatabase::new();
        let vfs = VfsInputs::new()?;
        let syntax = SyntaxQueries::new(vfs.clone())?;
        let hir = ItemTreeQueries::new(syntax.clone())?;
        let resolve = ModuleTreeQueries::new(vfs.clone())?;
        let packages = Arc::new(PackageInterner::new());
        let symbols = Arc::new(Interner::new());
        let package_scopes =
            Phase1PackageScopesQueries::new(Arc::clone(&packages), Arc::clone(&symbols))?;
        let declared_requirements = DeclaredRequirementsQueries::new()?;
        let package_graph =
            InputQuery::<u8, Option<PackageGraph>>::new("package_graph", Durability::Medium)?;
        package_graph.set(&mut query_database, 0, None)?;
        Ok(Self {
            query_database,
            vfs,
            syntax,
            hir,
            resolve,
            packages,
            files: FileInterner::new(),
            symbols,
            def_paths: HashMap::new(),
            package_scopes,
            declared_requirements,
            package_graph,
        })
    }

    /// Sets the resolved active SAI package graph for this build.
    ///
    /// `None` means resolution is unavailable. `Some(graph)` is an available
    /// graph, including when the selected package has no active outgoing
    /// edges. The graph input follows Medium durability as specified for
    /// resolver output.
    pub fn set_package_graph(&mut self, graph: Option<PackageGraph>) -> Result<bool, QueryError> {
        self.package_graph
            .set_at_durability(&mut self.query_database, 0, graph, Durability::Medium)
    }

    /// Interns a stable package key for this session.
    pub fn intern_package(&self, package_key: &str) -> Result<PackageId, PackageIdError> {
        self.packages.intern(package_key)
    }

    /// Sets the exact `sai.package` bytes used by
    /// [`RootDatabase::declared_requirements`].
    ///
    /// The role is part of the tracked input because root and dependency
    /// manifest parsing has different validation rules. Durability follows
    /// the package's place in the current build graph.
    pub fn set_package_manifest(
        &mut self,
        package: PackageId,
        source_bytes: Arc<[u8]>,
        role: PackageRole,
        durability: Durability,
    ) -> Result<bool, DeclaredRequirementsError> {
        let package_key = self
            .packages
            .resolve(package)
            .map_err(DeclaredRequirementsError::from)?;
        let package_key =
            PackageKey::new(package_key.as_ref()).map_err(DeclaredRequirementsError::from)?;
        self.declared_requirements
            .input
            .set_at_durability(
                &mut self.query_database,
                package_key,
                DeclaredRequirementsInput {
                    source_bytes,
                    dependency: role == PackageRole::Dependency,
                },
                durability,
            )
            .map_err(DeclaredRequirementsError::from)
    }

    /// Returns normalized direct requirements from this package's current
    /// tracked manifest input, sorted by package identity.
    ///
    /// Duplicate identities keep the first declaration, matching the
    /// manifest contract query's recovery behavior. This query intentionally
    /// does not return manifest diagnostics; those remain owned by
    /// `sai.package` validation.
    pub fn declared_requirements(
        &mut self,
        package: PackageId,
    ) -> Result<Arc<[DeclaredRequirement]>, DeclaredRequirementsError> {
        let package_key = self
            .packages
            .resolve(package)
            .map_err(DeclaredRequirementsError::from)?;
        let package_key =
            PackageKey::new(package_key.as_ref()).map_err(DeclaredRequirementsError::from)?;
        let expected_package_key = package_key.as_str().to_owned();
        let output = self
            .declared_requirements
            .query
            .get(&mut self.query_database, package_key)
            .map_err(DeclaredRequirementsError::from)?;
        match &output.status {
            DeclaredRequirementsStatus::Valid => Ok(Arc::clone(&output.requirements)),
            DeclaredRequirementsStatus::InvalidManifest => {
                Err(DeclaredRequirementsError::InvalidManifest {
                    package_key: expected_package_key,
                })
            }
            DeclaredRequirementsStatus::PackageMismatch {
                found_identity,
                found_compatibility_line,
            } => Err(DeclaredRequirementsError::ManifestPackageMismatch {
                expected_package_key,
                found_identity: found_identity.clone(),
                found_compatibility_line: found_compatibility_line.clone(),
            }),
        }
    }

    /// Interns a stable file key and returns its session-local file ID.
    pub fn intern_file_key(&mut self, key: FileKey) -> Result<FileId, FileInternerError> {
        self.files.intern(key)
    }

    /// Interns a source file beneath the package's configured source directory.
    pub fn intern_source_file(
        &mut self,
        package_id: PackageId,
        source_directory: PackageSourceDirectory,
        source_relative_path: PackageSourcePath,
    ) -> Result<FileId, FileInternerError> {
        self.files.intern_source_file(
            &self.packages,
            package_id,
            source_directory,
            source_relative_path,
        )
    }

    /// Interns a package-relative metadata or WIT file.
    pub fn intern_package_file(
        &mut self,
        package_id: PackageId,
        package_relative_path: PackageRelativePath,
    ) -> Result<FileId, FileInternerError> {
        self.files
            .intern_package_file(&self.packages, package_id, package_relative_path)
    }

    /// Returns the stable file identity assigned to a session-local ID.
    pub fn file_key(&self, file_id: FileId) -> Option<&FileKey> {
        self.files.file_key(file_id)
    }

    /// Resolves a session-local symbol returned by frontend query results.
    pub fn resolve_symbol(&self, symbol: Symbol) -> Result<Arc<str>, LookupError> {
        self.symbols.resolve(symbol)
    }

    /// Sets raw file bytes as a tracked VFS input.
    pub fn set_file_contents(
        &mut self,
        key: FileKey,
        contents: Arc<[u8]>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_file_contents(&mut self.query_database, key, contents, durability)
    }

    /// Records a file read failure as a tracked VFS input.
    pub fn set_file_read_error(
        &mut self,
        key: FileKey,
        io_error_kind: impl Into<String>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_file_read_error(&mut self.query_database, key, io_error_kind, durability)
    }

    /// Removes a file input when the file leaves the source snapshot.
    pub fn remove_file(&mut self, key: FileKey) -> Result<bool, QueryError> {
        self.vfs.remove_file(&mut self.query_database, key)
    }

    /// Sets the tracked source-file inventory for one package.
    pub fn set_package_files(
        &mut self,
        key: PackageKey,
        files: PackageFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_package_files(&mut self.query_database, key, files, durability)
    }

    /// Removes the tracked source-file inventory for one package.
    pub fn remove_package_files(&mut self, key: PackageKey) -> Result<bool, QueryError> {
        self.vfs.remove_package_files(&mut self.query_database, key)
    }

    /// Sets the tracked WIT-file inventory for one package.
    pub fn set_wit_files(
        &mut self,
        key: PackageKey,
        files: WitFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_wit_files(&mut self.query_database, key, files, durability)
    }

    /// Removes the tracked WIT-file inventory for one package.
    pub fn remove_wit_files(&mut self, key: PackageKey) -> Result<bool, QueryError> {
        self.vfs.remove_wit_files(&mut self.query_database, key)
    }

    /// Applies file and package-inventory updates in a single query revision.
    pub fn apply_vfs_updates(
        &mut self,
        updates: impl IntoIterator<Item = VfsInputUpdate>,
    ) -> Result<usize, QueryError> {
        self.vfs.apply_updates(&mut self.query_database, updates)
    }

    /// Reconciles a complete VFS snapshot, removing values omitted from it.
    pub fn replace_vfs_snapshot(
        &mut self,
        previous: Option<&VfsInputSnapshot>,
        current: &VfsInputSnapshot,
    ) -> Result<usize, QueryError> {
        self.vfs
            .replace_snapshot(&mut self.query_database, previous, current)
    }

    /// Reads tracked file contents, including tracked absence.
    pub fn file_contents(&mut self, key: FileKey) -> Result<Option<FileContents>, QueryError> {
        self.vfs.file_contents(&mut self.query_database, key)
    }

    /// Reads decoded source using the local key assigned to `file_id`.
    ///
    /// `file_id` must have been created by this database; its numeric
    /// representation cannot prove that it came from this session.
    pub fn source_text(&mut self, file_id: FileId) -> Result<SourceText, RootDatabaseError> {
        let key = self
            .files
            .file_key(file_id)
            .cloned()
            .ok_or(RootDatabaseError::FileNotInterned)?;
        self.vfs
            .source_text(&mut self.query_database, key, file_id)
            .map_err(RootDatabaseError::from)
    }

    /// Reads source text and converts a non-ready status to its diagnostic.
    ///
    /// The caller supplies the command- or document-specific source reference,
    /// since the stable file key does not contain a workspace-relative path or
    /// a document version. `source_ref` must identify the same file as
    /// `file_id`, and its snapshot must correspond to the VFS source returned
    /// by this call. This relationship cannot be validated here.
    pub fn source_text_and_diagnostic(
        &mut self,
        file_id: FileId,
        source_ref: SourceRef,
    ) -> Result<(SourceText, Option<Diagnostic>), RootDatabaseError> {
        let source_text = self.source_text(file_id)?;
        let diagnostic = source_text_status_diagnostic(&source_text, source_ref);
        Ok((source_text, diagnostic))
    }

    /// Reads the memoized line index for the local file key at `file_id`.
    ///
    /// `file_id` must have been created by this database; its numeric
    /// representation cannot prove that it came from this session.
    pub fn line_index(&mut self, file_id: FileId) -> Result<Arc<LineIndex>, RootDatabaseError> {
        let key = self
            .files
            .file_key(file_id)
            .cloned()
            .ok_or(RootDatabaseError::FileNotInterned)?;
        self.vfs
            .line_index(&mut self.query_database, key)
            .map_err(RootDatabaseError::from)
    }

    /// Reads a package's source inventory and interns file entries.
    ///
    /// `None` means no inventory input exists. A returned value preserves
    /// whether the configured source directory exists; directories and
    /// symlinks remain listed but receive no `FileId`.
    pub fn intern_package_files(
        &mut self,
        package_id: PackageId,
        source_directory: PackageSourceDirectory,
    ) -> Result<Option<InternedPackageFiles>, RootDatabaseError> {
        let package_name = self.packages.resolve(package_id)?;
        let package_key = PackageKey::new(package_name.as_ref())?;
        let Some(snapshot) = self
            .vfs
            .package_files(&mut self.query_database, package_key)?
        else {
            return Ok(None);
        };
        let source_paths = snapshot
            .entries()
            .iter()
            .filter(|entry| entry.kind() == PackageFileKind::File)
            .map(|entry| entry.path().clone())
            .collect::<Vec<_>>();
        let file_ids = self.files.intern_source_files(
            &self.packages,
            package_id,
            source_directory,
            source_paths,
        )?;
        let mut file_ids = file_ids.into_iter();
        let entries = snapshot
            .entries()
            .iter()
            .map(|entry| match entry.kind() {
                PackageFileKind::File => InternedPackageFileEntry::File {
                    path: entry.path().clone(),
                    file_id: file_ids
                        .next()
                        .expect("every inventoried source file has an interned file ID"),
                },
                PackageFileKind::Directory => InternedPackageFileEntry::Directory {
                    path: entry.path().clone(),
                },
                PackageFileKind::Symlink => InternedPackageFileEntry::Symlink {
                    path: entry.path().clone(),
                },
            })
            .collect();
        debug_assert!(file_ids.next().is_none());
        Ok(Some(InternedPackageFiles {
            source_directory_exists: snapshot.source_directory_exists(),
            entries,
        }))
    }

    /// Computes the Stage 0 module tree for a package's configured source directory.
    ///
    /// `package_field` locates the `package` field in `sai.package`, which is
    /// the primary location used by `MISSING_ROOT_MODULE`.
    pub fn module_tree(
        &mut self,
        package_id: PackageId,
        source_directory: PackageSourceDirectory,
        package_field: Option<sai_diagnostics::Location>,
    ) -> Result<Arc<ModuleTree>, RootDatabaseError> {
        let interned = self.intern_package_files(package_id, source_directory)?;
        let file_ids = interned
            .as_ref()
            .into_iter()
            .flat_map(InternedPackageFiles::entries)
            .filter_map(|entry| {
                entry
                    .file_id()
                    .map(|file_id| (entry.path().as_str().to_owned(), file_id))
            })
            .collect::<BTreeMap<_, _>>();
        self.resolve
            .module_tree(
                &mut self.query_database,
                package_id,
                &self.packages,
                &self.files,
                &self.symbols,
                &file_ids,
                package_field,
            )
            .map_err(RootDatabaseError::from)
    }

    /// Reads WIT paths and interns them as session-local file IDs.
    ///
    /// `None` means no WIT inventory input exists. `NotApplicable` preserves
    /// an explicitly recorded package without a usable WIT directory, while
    /// `Listed([])` preserves an existing directory with no direct WIT files.
    pub fn intern_wit_files(
        &mut self,
        package_id: PackageId,
    ) -> Result<Option<InternedWitFiles>, RootDatabaseError> {
        let package_name = self.packages.resolve(package_id)?;
        let package_key = PackageKey::new(package_name.as_ref())?;
        let Some(snapshot) = self.vfs.wit_files(&mut self.query_database, package_key)? else {
            return Ok(None);
        };
        match snapshot {
            WitFilesSnapshot::NotApplicable => Ok(Some(InternedWitFiles::NotApplicable)),
            WitFilesSnapshot::Listed { files, .. } => {
                let file_ids = self.files.intern_package_files(
                    &self.packages,
                    package_id,
                    files.iter().cloned(),
                )?;
                Ok(Some(InternedWitFiles::Listed(file_ids)))
            }
        }
    }

    /// Parses a file and binds syntax diagnostics to the supplied snapshot.
    pub fn parse_file(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<Parse, ParseQueryError> {
        self.syntax
            .parse(&mut self.query_database, &self.files, snapshot, file_id)
    }

    /// Binds the memoized AST-ID shape to the supplied snapshot.
    pub fn ast_id_map(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<ast::AstIdMap, ParseQueryError> {
        self.syntax
            .ast_id_map(&mut self.query_database, &self.files, snapshot, file_id)
    }

    /// Collects and snapshot-binds the item's declarations for one file.
    pub fn def_collector(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<Arc<ItemTree>, ItemTreeQueryError> {
        self.hir.def_collector(
            &mut self.query_database,
            &self.files,
            snapshot,
            file_id,
            &self.symbols,
        )
    }

    /// Builds target-filtered declaration scopes for every module in
    /// `module_tree`, using source and AST data bound to `snapshot`.
    ///
    /// Target filtering prepares this query's source-bound input. Phase 1
    /// declarations, local use-target diagnostics, and local module/interface
    /// bindings run in the tracked `package_scopes` query. Foreign dependency
    /// scopes are available through the explicit graph-aware overload.
    ///
    /// The database retains one append-only definition-path interner per
    /// package so returned `DefId`s remain resolvable for its lifetime.
    pub fn collect_phase1_package_scopes(
        &mut self,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
        build_context: Phase1BuildContext,
    ) -> Result<Phase1PackageScopes, Phase1CollectionError> {
        self.collect_phase1_package_scopes_collection(
            module_tree,
            snapshot,
            target_cfg,
            build_context,
        )
        .map(|collection| collection.scopes)
    }

    /// Collects Phase 1 scopes with request-local source and target provenance.
    ///
    /// The collection can be passed to snapshot-bound owner adapters. Unlike
    /// the memoized scope value, its provenance is not retained in a query.
    pub fn collect_phase1_package_scopes_collection(
        &mut self,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
        build_context: Phase1BuildContext,
    ) -> Result<Phase1PackageScopesCollection, Phase1CollectionError> {
        self.collect_phase1_package_scopes_with_requirements(
            module_tree,
            snapshot,
            target_cfg,
            build_context,
            None,
            &[],
        )
    }

    /// Sets the package's exact manifest input, reads its tracked requirement
    /// query, and collects scopes with those normalized requirements.
    ///
    /// Invalid manifests and header/key mismatches produce no requirement
    /// classifications here; package-manifest validation owns their
    /// diagnostics. `None` remains distinct from a valid manifest with no
    /// requirements in the package-scope query input.
    pub fn collect_phase1_package_scopes_with_manifest(
        &mut self,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
        build_context: Phase1BuildContext,
        manifest_bytes: Arc<[u8]>,
        manifest_role: PackageRole,
    ) -> Result<Phase1PackageScopes, Phase1CollectionError> {
        self.collect_phase1_package_scopes_with_manifest_and_foreign_scopes(
            module_tree,
            snapshot,
            target_cfg,
            build_context,
            manifest_bytes,
            manifest_role,
            &[],
        )
        .map(|collection| collection.scopes)
    }

    /// Collects package scopes using finalized scopes for exact active direct
    /// source dependencies. Each foreign input is validated against this
    /// package's graph edge and its fingerprint becomes part of the tracked
    /// query identity.
    pub fn collect_phase1_package_scopes_with_manifest_and_foreign_scopes(
        &mut self,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
        build_context: Phase1BuildContext,
        manifest_bytes: Arc<[u8]>,
        manifest_role: PackageRole,
        foreign_scopes: &[Phase1ForeignPackageScopes],
    ) -> Result<Phase1PackageScopesCollection, Phase1CollectionError> {
        self.set_package_manifest(
            module_tree.package,
            manifest_bytes,
            manifest_role,
            Durability::Low,
        )
        .map_err(Phase1CollectionError::from)?;
        let requirements = match self.declared_requirements(module_tree.package) {
            Ok(requirements) => Some(requirements),
            Err(
                DeclaredRequirementsError::InvalidManifest { .. }
                | DeclaredRequirementsError::ManifestPackageMismatch { .. },
            ) => None,
            Err(error) => return Err(Phase1CollectionError::from(error)),
        };
        self.collect_phase1_package_scopes_with_requirements(
            module_tree,
            snapshot,
            target_cfg,
            build_context,
            requirements,
            foreign_scopes,
        )
    }

    fn collect_phase1_package_scopes_with_requirements(
        &mut self,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
        build_context: Phase1BuildContext,
        declared_requirements: Option<Arc<[DeclaredRequirement]>>,
        foreign_scopes: &[Phase1ForeignPackageScopes],
    ) -> Result<Phase1PackageScopesCollection, Phase1CollectionError> {
        if module_tree.modules.len() > u32::MAX as usize {
            return Err(Phase1CollectionError::TooManyModules);
        }

        let mut owned_inputs = Vec::with_capacity(module_tree.modules.len());
        for module in &module_tree.modules {
            let snapshot_file = snapshot
                .file(module.file)
                .ok_or(Phase1CollectionError::MissingSnapshotFile(module.file))?;
            let source = Arc::clone(snapshot_file.source());
            let parsed = self.parse_file(snapshot, module.file)?;
            let ast_ids = self.ast_id_map(snapshot, module.file)?;
            let item_tree = self.def_collector(snapshot, module.file)?;
            let filtered = item_tree.filter_for_target(
                source.text(),
                &parsed.root,
                &ast_ids,
                &self.symbols,
                snapshot,
                target_cfg,
            )?;
            owned_inputs.push(OwnedPhase1ModuleInput {
                filtered,
                ast_ids,
                root: parsed.root,
                source,
                source_ref: SourceRef {
                    path: snapshot_file.path().to_owned(),
                    package: snapshot_file.package().map(str::to_owned),
                    // SourceRef.snapshot is the caller's document version;
                    // WorkspaceSnapshot IDs remain an internal binding.
                    snapshot: snapshot_file.document_version(),
                },
            });
        }

        let package_key = PackageKey::new(
            self.packages
                .resolve(module_tree.package)
                .map_err(PackageScopesError::from)?
                .as_ref(),
        )
        .map_err(PackageScopesError::from)?;
        let graph = self.package_graph.get(&mut self.query_database, 0)?;
        let package_graph = phase1_package_graph_projection(graph.as_ref().as_ref(), &package_key)?;
        for foreign in foreign_scopes {
            let expected_package = self
                .packages
                .intern(foreign.package_key.as_str())
                .map_err(Phase1CollectionError::Package)?;
            let contains_foreign_definition = foreign.scopes.modules.iter().all(|module| {
                module.module_def_id.package() == expected_package
                    && self
                        .resolve_def_path(module.module_def_id.to_def_id())
                        .is_ok_and(|path| path.is_some())
            });
            if foreign.module_tree.package != expected_package || !contains_foreign_definition {
                return Err(Phase1CollectionError::ForeignPackageScopeMismatch {
                    package: package_key.clone(),
                    visible_identity: foreign.visible_identity.clone(),
                    target: foreign.package_key.clone(),
                });
            }
        }
        validate_phase1_package_graph_requirements(
            &package_key,
            package_graph.as_ref(),
            declared_requirements.as_deref(),
            target_cfg,
            build_context,
        )?;
        validate_phase1_foreign_package_scopes(
            &package_key,
            package_graph.as_ref(),
            foreign_scopes,
        )?;
        let def_paths = if let Some(def_paths) = self.def_paths.get(&module_tree.package) {
            Arc::clone(def_paths)
        } else {
            let def_paths = Arc::new(DefPathInterner::new(
                module_tree.package,
                &self.packages,
                &self.symbols,
            )?);
            self.def_paths
                .insert(module_tree.package, Arc::clone(&def_paths));
            def_paths
        };
        let identity = Arc::new(phase1_package_scopes_identity(
            package_key.clone(),
            module_tree,
            &owned_inputs,
            target_cfg,
            build_context,
            declared_requirements.as_deref(),
            package_graph,
            foreign_scopes,
            &self.symbols,
        )?);
        let prepared = Arc::new(PreparedPhase1PackageScopes {
            module_tree: module_tree.clone(),
            snapshot: snapshot.clone(),
            target_cfg: target_cfg.clone(),
            build_context,
            declared_requirements,
            foreign_packages: foreign_scopes.to_vec(),
            modules: owned_inputs,
            def_paths,
        });
        let _staged_input = StagedPhase1InputGuard::install(
            package_key.clone(),
            Arc::clone(&identity),
            Arc::clone(&prepared),
        );
        self.package_scopes.input.set(
            &mut self.query_database,
            package_key.clone(),
            identity.as_ref().clone(),
        )?;
        let output = self
            .package_scopes
            .query
            .get(&mut self.query_database, package_key)?;
        let memo = output.result.as_ref().map_err(Clone::clone)?;
        let mut scopes = memo.scopes.clone();
        for input in &prepared.modules {
            scopes
                .diagnostics
                .extend(current_phase1_target_filter_diagnostics(input));
        }
        for diagnostic in &memo.detached_workspace_diagnostics {
            scopes.diagnostics.push(
                diagnostic
                    .bind(snapshot)
                    .map_err(Phase1CollectionError::from)?,
            );
        }
        sort_phase1_diagnostics(&mut scopes.diagnostics);
        Ok(Phase1PackageScopesCollection {
            scopes,
            input_fingerprint: output.input_fingerprint,
            snapshot_id: snapshot.id(),
            target_cfg: target_cfg.clone(),
            module_tree: module_tree.clone(),
        })
    }

    fn current_owner_inputs(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file: FileId,
        target_cfg: &TargetCfg,
    ) -> Result<CurrentOwnerInputs, OwnerControlFlowResolutionError> {
        let snapshot_file = snapshot
            .file(file)
            .ok_or(OwnerControlFlowResolutionError::MissingSnapshotFile(file))?;
        let source = Arc::clone(snapshot_file.source());
        let parsed = self
            .parse_file(snapshot, file)
            .map_err(OwnerControlFlowResolutionError::Parse)?;
        let ast_ids = self
            .ast_id_map(snapshot, file)
            .map_err(OwnerControlFlowResolutionError::Parse)?;
        let item_tree = self
            .def_collector(snapshot, file)
            .map_err(OwnerControlFlowResolutionError::ItemTree)?;
        let filtered_item_tree = item_tree
            .filter_for_target(
                source.text(),
                &parsed.root,
                &ast_ids,
                &self.symbols,
                snapshot,
                target_cfg,
            )
            .map_err(OwnerControlFlowResolutionError::TargetFilter)?
            .item_tree;
        let retained_item_kinds = filtered_item_tree
            .items
            .iter()
            .flat_map(|entry| {
                std::iter::once((entry.ast_id, entry.kind)).chain(
                    entry
                        .children
                        .iter()
                        .map(|child| (child.ast_id, child.kind)),
                )
            })
            .collect();
        let source_ref = SourceRef {
            path: snapshot_file.path().to_owned(),
            package: snapshot_file.package().map(str::to_owned),
            snapshot: snapshot_file.document_version(),
        };
        Ok(CurrentOwnerInputs {
            source,
            parsed,
            ast_ids,
            filtered_item_tree,
            retained_item_kinds,
            source_ref,
        })
    }

    /// Compatibility entry point for the combined function-owner analysis.
    ///
    /// The returned label-named result also contains local path resolutions;
    /// new callers should use [`Self::resolve_function_owner_analyses`].
    pub fn resolve_function_owner_labels(
        &mut self,
        collection: &Phase1PackageScopesCollection,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<FunctionOwnerLabelResolutions, FunctionOwnerLabelResolutionError> {
        self.resolve_function_owner_analyses(collection, module_tree, snapshot)
    }

    /// Resolves loop labels and local paths for each current top-level
    /// function owner. Module aliases are taken from the exact module scope
    /// associated with each descriptor's file.
    ///
    /// This is an uncached bridge: HIR and source maps remain local to each
    /// iteration, and only owned results and diagnostics are returned.
    pub fn resolve_function_owner_analyses(
        &mut self,
        collection: &Phase1PackageScopesCollection,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<FunctionOwnerResolutions, FunctionOwnerResolutionError> {
        if collection.snapshot_id != snapshot.id() {
            return Err(FunctionOwnerLabelResolutionError::SnapshotMismatch);
        }
        if collection.module_tree != *module_tree {
            return Err(FunctionOwnerLabelResolutionError::ModuleTreeMismatch);
        }

        let top_level_owner_index = index_top_level_owner_descriptors(collection);
        let descriptors = &collection.scopes.function_owners;
        let mut owners = Vec::with_capacity(descriptors.len());
        let mut group_start = 0;
        while group_start < descriptors.len() {
            let file = descriptors[group_start].file;
            let mut group_end = group_start + 1;
            while group_end < descriptors.len() && descriptors[group_end].file == file {
                group_end += 1;
            }
            let inputs =
                match self.current_owner_inputs(snapshot, file, &collection.target_cfg) {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        owners.extend(descriptors[group_start..group_end].iter().map(
                            |descriptor| FunctionOwnerResolution {
                                owner: descriptor.owner,
                                resolution: Err(error.clone()),
                                local_resolution: Err(error.clone()),
                                lowering_diagnostics: Vec::new(),
                            },
                        ));
                        group_start = group_end;
                        continue;
                    }
                };
            let module_aliases = module_aliases_for_file(collection, module_tree, file);

            for descriptor in &descriptors[group_start..group_end] {
                let mut lowering_diagnostics = Vec::new();
                let mut local_resolution = None;
                let resolution = (|| {
                    let owner_descriptor = find_top_level_owner_descriptor(
                        &top_level_owner_index,
                        descriptor.owner,
                        descriptor.file,
                        descriptor.item_id,
                        TopLevelOwnerKind::Function,
                    )?;

                    let def_paths = self
                        .def_paths
                        .get(&descriptor.owner.def_id.package())
                        .ok_or(FunctionOwnerLabelResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;
                    let expected_path = def_paths
                        .resolve_def_id(descriptor.owner.def_id.to_def_id())
                        .map_err(FunctionOwnerLabelResolutionError::DefPathInterner)?
                        .ok_or(FunctionOwnerLabelResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;

                    let arena = HirArena::new();
                    let context = HirLoweringContext::new(
                        &inputs.ast_ids,
                        snapshot,
                        &inputs.parsed.root,
                        &arena,
                        &self.symbols,
                        def_paths,
                    );
                    let (hir_owner, source_map) = lower_top_level_owner(
                        owner_descriptor,
                        &expected_path,
                        &inputs.filtered_item_tree,
                        &inputs.retained_item_kinds,
                        &context,
                    )?;
                    lowering_diagnostics = hir_owner.diagnostics.clone();
                    match hir_owner.body {
                        Some(body) => {
                            let labels = resolve_owner_control_flow(
                                body,
                                &inputs.source_ref,
                                inputs.source.as_ref(),
                                &self.symbols,
                            )
                            .map_err(FunctionOwnerLabelResolutionError::Labels);
                            let locals = match &module_aliases {
                                Ok(module_aliases) => resolve_owner_locals(
                                    body,
                                    &source_map,
                                    snapshot,
                                    &self.symbols,
                                    module_aliases,
                                )
                                .map_err(FunctionOwnerLabelResolutionError::Locals),
                                Err(error) => Err(error.clone()),
                            };
                            local_resolution = Some(locals.map(Some));
                            labels
                        }
                        None => {
                            local_resolution = Some(Ok(None));
                            Ok(LabelResolution::default())
                        }
                    }
                })();
                let local_resolution =
                    local_resolution.unwrap_or_else(|| resolution.clone().map(|_| None));
                owners.push(FunctionOwnerResolution {
                    owner: descriptor.owner,
                    resolution,
                    local_resolution,
                    lowering_diagnostics,
                });
            }
            group_start = group_end;
        }
        Ok(FunctionOwnerResolutions {
            snapshot_id: snapshot.id(),
            target_cfg: collection.target_cfg.clone(),
            collection_input_fingerprint: collection.input_fingerprint,
            owners,
        })
    }

    /// Lowers each Phase 1 invariant descriptor from the collection's exact
    /// module tree and snapshot, then diagnoses forbidden predicate syntax.
    ///
    /// This is an uncached bridge: HIR and source maps remain local to each
    /// iteration, and only owned diagnostics and errors are returned. The
    /// descriptor order is preserved so nested definition paths are interned
    /// deterministically.
    pub fn resolve_invariant_owner_predicates(
        &mut self,
        collection: &Phase1PackageScopesCollection,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<PredicateOwnerResolutions, OwnerControlFlowResolutionError> {
        if collection.snapshot_id != snapshot.id() {
            return Err(OwnerControlFlowResolutionError::SnapshotMismatch);
        }
        if collection.module_tree != *module_tree {
            return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
        }

        let top_level_owner_index = index_top_level_owner_descriptors(collection);
        let descriptors = &collection.scopes.invariant_owners;
        let mut owners = Vec::with_capacity(descriptors.len());
        let mut group_start = 0;
        while group_start < descriptors.len() {
            let file = descriptors[group_start].file;
            let mut group_end = group_start + 1;
            while group_end < descriptors.len() && descriptors[group_end].file == file {
                group_end += 1;
            }
            let inputs =
                match self.current_owner_inputs(snapshot, file, &collection.target_cfg) {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        owners.extend(descriptors[group_start..group_end].iter().map(
                            |descriptor| PredicateOwnerResolution {
                                owner: descriptor.owner,
                                resolution: Err(error.clone()),
                                local_resolution: Err(error.clone()),
                                lowering_diagnostics: Vec::new(),
                            },
                        ));
                        group_start = group_end;
                        continue;
                    }
                };
            let module_aliases = module_aliases_for_file(collection, module_tree, file);

            for descriptor in &descriptors[group_start..group_end] {
                let mut lowering_diagnostics = Vec::new();
                let mut local_resolution = None;
                let resolution = (|| {
                    let owner_descriptor = find_top_level_owner_descriptor(
                        &top_level_owner_index,
                        descriptor.owner,
                        descriptor.file,
                        descriptor.item_id,
                        TopLevelOwnerKind::Invariant,
                    )?;

                    let def_paths = self
                        .def_paths
                        .get(&descriptor.owner.def_id.package())
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;
                    let expected_path = def_paths
                        .resolve_def_id(descriptor.owner.def_id.to_def_id())
                        .map_err(OwnerControlFlowResolutionError::DefPathInterner)?
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;

                    let arena = HirArena::new();
                    let context = HirLoweringContext::new(
                        &inputs.ast_ids,
                        snapshot,
                        &inputs.parsed.root,
                        &arena,
                        &self.symbols,
                        def_paths,
                    );
                    let (hir_owner, source_map) = lower_top_level_owner(
                        owner_descriptor,
                        &expected_path,
                        &inputs.filtered_item_tree,
                        &inputs.retained_item_kinds,
                        &context,
                    )?;
                    lowering_diagnostics = hir_owner.diagnostics.clone();
                    match hir_owner.body {
                        Some(body) => {
                            let locals = match &module_aliases {
                                Ok(module_aliases) => resolve_owner_locals(
                                    body,
                                    &source_map,
                                    snapshot,
                                    &self.symbols,
                                    module_aliases,
                                )
                                .map(Some)
                                .map_err(OwnerControlFlowResolutionError::Locals),
                                Err(error) => Err(error.clone()),
                            };
                            local_resolution = Some(locals);
                            resolve_owner_control_flow(
                                body,
                                &inputs.source_ref,
                                inputs.source.as_ref(),
                                &self.symbols,
                            )
                            .map(|result| result.diagnostics)
                            .map_err(OwnerControlFlowResolutionError::Labels)
                        }
                        None => {
                            local_resolution = Some(Ok(None));
                            Ok(Vec::new())
                        }
                    }
                })();
                let local_resolution =
                    local_resolution.unwrap_or_else(|| resolution.clone().map(|_| None));
                owners.push(PredicateOwnerResolution {
                    owner: descriptor.owner,
                    resolution,
                    local_resolution,
                    lowering_diagnostics,
                });
            }
            group_start = group_end;
        }
        Ok(PredicateOwnerResolutions {
            snapshot_id: snapshot.id(),
            target_cfg: collection.target_cfg.clone(),
            collection_input_fingerprint: collection.input_fingerprint,
            owners,
        })
    }

    /// Lowers Phase 1 refined and refinement-bearing distinct descriptors,
    /// then diagnoses forbidden syntax in each predicate body value.
    ///
    /// A predicate expression is not an executable owner body, so this uses
    /// the predicate-only resolver and does not resolve its loop labels.
    /// Distinct declarations without a refinement clause are not indexed.
    pub fn resolve_refinement_owner_predicates(
        &mut self,
        collection: &Phase1PackageScopesCollection,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<PredicateOwnerResolutions, OwnerControlFlowResolutionError> {
        if collection.snapshot_id != snapshot.id() {
            return Err(OwnerControlFlowResolutionError::SnapshotMismatch);
        }
        if collection.module_tree != *module_tree {
            return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
        }

        let top_level_owner_index = index_top_level_owner_descriptors(collection);
        let descriptors = &collection.scopes.refinement_predicate_owners;
        let mut owners = Vec::with_capacity(descriptors.len());
        let mut group_start = 0;
        while group_start < descriptors.len() {
            let file = descriptors[group_start].file;
            let mut group_end = group_start + 1;
            while group_end < descriptors.len() && descriptors[group_end].file == file {
                group_end += 1;
            }
            let inputs =
                match self.current_owner_inputs(snapshot, file, &collection.target_cfg) {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        owners.extend(descriptors[group_start..group_end].iter().map(
                            |descriptor| PredicateOwnerResolution {
                                owner: descriptor.owner,
                                resolution: Err(error.clone()),
                                local_resolution: Err(error.clone()),
                                lowering_diagnostics: Vec::new(),
                            },
                        ));
                        group_start = group_end;
                        continue;
                    }
                };
            let module_aliases = module_aliases_for_file(collection, module_tree, file);

            for descriptor in &descriptors[group_start..group_end] {
                let mut lowering_diagnostics = Vec::new();
                let mut local_resolution = None;
                let resolution = (|| {
                    let owner_kind = match descriptor.kind {
                        RefinementPredicateOwnerKind::Refined => TopLevelOwnerKind::Refined,
                        RefinementPredicateOwnerKind::Distinct => TopLevelOwnerKind::Distinct,
                    };
                    let owner_descriptor = find_top_level_owner_descriptor(
                        &top_level_owner_index,
                        descriptor.owner,
                        descriptor.file,
                        descriptor.item_id,
                        owner_kind,
                    )?;

                    let def_paths = self
                        .def_paths
                        .get(&descriptor.owner.def_id.package())
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;
                    let expected_path = def_paths
                        .resolve_def_id(descriptor.owner.def_id.to_def_id())
                        .map_err(OwnerControlFlowResolutionError::DefPathInterner)?
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;

                    let arena = HirArena::new();
                    let context = HirLoweringContext::new(
                        &inputs.ast_ids,
                        snapshot,
                        &inputs.parsed.root,
                        &arena,
                        &self.symbols,
                        def_paths,
                    );
                    let (hir_owner, source_map) = lower_top_level_owner(
                        owner_descriptor,
                        &expected_path,
                        &inputs.filtered_item_tree,
                        &inputs.retained_item_kinds,
                        &context,
                    )?;
                    lowering_diagnostics = hir_owner.diagnostics.clone();
                    match hir_owner.body {
                        Some(body) => {
                            let locals = match &module_aliases {
                                Ok(module_aliases) => resolve_owner_locals(
                                    body,
                                    &source_map,
                                    snapshot,
                                    &self.symbols,
                                    module_aliases,
                                )
                                .map(Some)
                                .map_err(OwnerControlFlowResolutionError::Locals),
                                Err(error) => Err(error.clone()),
                            };
                            local_resolution = Some(locals);
                            resolve_predicate_expression(
                                body.value,
                                &inputs.source_ref,
                                inputs.source.as_ref(),
                            )
                            .map_err(OwnerControlFlowResolutionError::Labels)
                        }
                        None => {
                            local_resolution = Some(Ok(None));
                            Ok(Vec::new())
                        }
                    }
                })();
                let local_resolution =
                    local_resolution.unwrap_or_else(|| resolution.clone().map(|_| None));
                owners.push(PredicateOwnerResolution {
                    owner: descriptor.owner,
                    resolution,
                    local_resolution,
                    lowering_diagnostics,
                });
            }
            group_start = group_end;
        }
        Ok(PredicateOwnerResolutions {
            snapshot_id: snapshot.id(),
            target_cfg: collection.target_cfg.clone(),
            collection_input_fingerprint: collection.input_fingerprint,
            owners,
        })
    }

    /// Lowers top-level extern function owners and diagnoses their contract
    /// predicates.
    ///
    /// Both the enclosing extern block and its function are lowered in the
    /// same temporary HIR arena because the function owner validates its
    /// source-ordered slot against the block owner. Only owned diagnostics
    /// and errors escape this adapter.
    pub fn resolve_extern_function_predicates(
        &mut self,
        collection: &Phase1PackageScopesCollection,
        module_tree: &ModuleTree,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<PredicateOwnerResolutions, OwnerControlFlowResolutionError> {
        if collection.snapshot_id != snapshot.id() {
            return Err(OwnerControlFlowResolutionError::SnapshotMismatch);
        }
        if collection.module_tree != *module_tree {
            return Err(OwnerControlFlowResolutionError::ModuleTreeMismatch);
        }

        let top_level_owner_index = index_top_level_owner_descriptors(collection);
        let descriptors = &collection.scopes.extern_function_owners;
        let mut owners = Vec::with_capacity(descriptors.len());
        let mut group_start = 0;
        while group_start < descriptors.len() {
            let file = descriptors[group_start].file;
            let mut group_end = group_start + 1;
            while group_end < descriptors.len() && descriptors[group_end].file == file {
                group_end += 1;
            }
            let inputs =
                match self.current_owner_inputs(snapshot, file, &collection.target_cfg) {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        owners.extend(descriptors[group_start..group_end].iter().map(
                            |descriptor| PredicateOwnerResolution {
                                owner: descriptor.owner,
                                resolution: Err(error.clone()),
                                local_resolution: Err(error.clone()),
                                lowering_diagnostics: Vec::new(),
                            },
                        ));
                        group_start = group_end;
                        continue;
                    }
                };
            let module_aliases = module_aliases_for_file(collection, module_tree, file);

            for descriptor in &descriptors[group_start..group_end] {
                let mut lowering_diagnostics = Vec::new();
                let mut local_resolution = None;
                let resolution = (|| {
                    let block_descriptor = find_top_level_owner_descriptor(
                        &top_level_owner_index,
                        descriptor.block_owner,
                        descriptor.file,
                        descriptor.block_item_id,
                        TopLevelOwnerKind::ExternBlock,
                    )?;
                    if inputs.retained_item_kinds.get(&descriptor.block_item_id)
                        != Some(&ItemTreeKind::ExternBlock)
                        || inputs.retained_item_kinds.get(&descriptor.item_id)
                            != Some(&ItemTreeKind::ExternFn)
                    {
                        return Err(
                            OwnerControlFlowResolutionError::DescriptorNotInCurrentTarget {
                                owner: descriptor.owner,
                                file: descriptor.file,
                            },
                        );
                    }

                    let def_paths = self
                        .def_paths
                        .get(&descriptor.owner.def_id.package())
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;
                    let block_path = def_paths
                        .resolve_def_id(descriptor.block_owner.def_id.to_def_id())
                        .map_err(OwnerControlFlowResolutionError::DefPathInterner)?
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.block_owner,
                        ))?;
                    let expected_path = def_paths
                        .resolve_def_id(descriptor.owner.def_id.to_def_id())
                        .map_err(OwnerControlFlowResolutionError::DefPathInterner)?
                        .ok_or(OwnerControlFlowResolutionError::MissingDefPath(
                            descriptor.owner,
                        ))?;

                    let arena = HirArena::new();
                    let context = HirLoweringContext::new(
                        &inputs.ast_ids,
                        snapshot,
                        &inputs.parsed.root,
                        &arena,
                        &self.symbols,
                        def_paths,
                    );
                    let (block_hir, _block_source_map) = lower_top_level_owner(
                        block_descriptor,
                        &block_path,
                        &inputs.filtered_item_tree,
                        &inputs.retained_item_kinds,
                        &context,
                    )?;
                    lowering_diagnostics.extend(block_hir.diagnostics.iter().cloned());

                    let (hir_owner, source_map) = lower_extern_item_owner(
                        descriptor.owner,
                        &expected_path,
                        &block_hir,
                        ExternItemIds {
                            block: descriptor.block_item_id,
                            item: descriptor.item_id,
                        },
                        &inputs.filtered_item_tree,
                        &context,
                    )
                    .map_err(OwnerControlFlowResolutionError::Lowering)?;
                    lowering_diagnostics.extend(hir_owner.diagnostics.iter().cloned());
                    match hir_owner.body {
                        Some(body) => {
                            let locals = match &module_aliases {
                                Ok(module_aliases) => resolve_owner_locals(
                                    body,
                                    &source_map,
                                    snapshot,
                                    &self.symbols,
                                    module_aliases,
                                )
                                .map(Some)
                                .map_err(OwnerControlFlowResolutionError::Locals),
                                Err(error) => Err(error.clone()),
                            };
                            local_resolution = Some(locals);
                            resolve_owner_control_flow(
                                body,
                                &inputs.source_ref,
                                inputs.source.as_ref(),
                                &self.symbols,
                            )
                            .map(|result| result.diagnostics)
                            .map_err(OwnerControlFlowResolutionError::Labels)
                        }
                        None => {
                            local_resolution = Some(Ok(None));
                            Ok(Vec::new())
                        }
                    }
                })();
                let local_resolution =
                    local_resolution.unwrap_or_else(|| resolution.clone().map(|_| None));
                owners.push(PredicateOwnerResolution {
                    owner: descriptor.owner,
                    resolution,
                    local_resolution,
                    lowering_diagnostics,
                });
            }
            group_start = group_end;
        }
        Ok(PredicateOwnerResolutions {
            snapshot_id: snapshot.id(),
            target_cfg: collection.target_cfg.clone(),
            collection_input_fingerprint: collection.input_fingerprint,
            owners,
        })
    }

    /// Resolves a definition ID created in this database's Phase 1 results.
    ///
    /// Foreign IDs and IDs for packages that have not collected declarations
    /// in this database return `Ok(None)`.
    pub fn resolve_def_path(&self, def_id: DefId) -> Result<Option<DefPath>, DefPathInternerError> {
        match self.def_paths.get(&def_id.package) {
            Some(def_paths) => def_paths.resolve_def_id(def_id),
            None => Ok(None),
        }
    }

    /// Builds source-bound diagnostics for the MVS package graph.
    ///
    /// The manifest map must cover every contract visited by selection,
    /// including unselected candidates. The root manifest ID must be interned
    /// by this database. Dependency manifest IDs must identify distinct files
    /// in `snapshot`; they need not be present in this database's file
    /// interner because that interner keys files by compatibility line, not
    /// exact candidate version. Complete selections produce version conflicts,
    /// selected cycles, and duplicate-line lints. Root-cycle findings are also
    /// produced from incomplete selections; other selection errors remain the
    /// caller's responsibility.
    pub fn mvs_graph_diagnostics(
        &self,
        snapshot: &WorkspaceSnapshot,
        selection: &MvsSelectionOutput,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
        duplicate_line_cli: Option<OverrideLintLevel>,
        deny_warnings: bool,
    ) -> Result<Vec<Diagnostic>, RootDatabaseError> {
        let manifests = self.mvs_manifest_locations(
            snapshot,
            selection,
            root_manifest_file,
            dependency_manifest_files,
        )?;
        let mut diagnostics = Vec::new();

        if selection.is_complete() {
            diagnostics.extend(version_conflict_diagnostics(selection, &manifests)?);
            diagnostics.extend(dependency_cycle_diagnostics(selection, &manifests)?);
            diagnostics.extend(duplicate_package_line_diagnostics(
                selection,
                &MvsDuplicatePackageLineDiagnosticContext {
                    manifests: &manifests,
                    cli: duplicate_line_cli,
                    deny_warnings,
                },
            )?);
        }
        diagnostics.extend(root_dependency_cycle_diagnostics(selection, &manifests)?);
        Ok(diagnostics)
    }

    /// Builds source-bound `DIVERGENT_SOURCES` diagnostics for a mixed-source audit.
    ///
    /// This shares the manifest binding checks used by [`Self::mvs_graph_diagnostics`].
    /// The manifest map must include every dependency contract visited during
    /// MVS, including unselected candidates whose edges contributed source
    /// choices. Pass the audit plan returned by `mixed_source`; reload and
    /// identity failures remain available on that plan for the package-loading
    /// caller to report. The audit plan must come from the same MVS load, and
    /// the workspace manifest parses must match the contracts used by that
    /// load.
    pub fn mixed_source_divergent_diagnostics(
        &self,
        snapshot: &WorkspaceSnapshot,
        mixed_source: &MixedSourceMvsOutput,
        audit_plan: &MixedSourceFinalAuditPlan,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
    ) -> Result<Vec<Diagnostic>, RootDatabaseError> {
        let (_, manifests) = self.mixed_source_diagnostic_context(
            snapshot,
            mixed_source,
            audit_plan,
            root_manifest_file,
            dependency_manifest_files,
        )?;
        Ok(divergent_sources_diagnostics(
            &audit_plan.audit(),
            &manifests,
        )?)
    }

    /// Builds source-located diagnostics for reportable final-source identity
    /// failures retained by a mixed-source audit plan.
    ///
    /// The plan must belong to `mixed_source`, and snapshot manifests must
    /// match the contracts used for the original MVS load. Non-identity final
    /// reload failures remain on the audit plan for the package workflow.
    pub fn mixed_source_identity_diagnostics(
        &self,
        snapshot: &WorkspaceSnapshot,
        mixed_source: &MixedSourceMvsOutput,
        audit_plan: &MixedSourceFinalAuditPlan,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
    ) -> Result<Vec<Diagnostic>, RootDatabaseError> {
        let selection = mixed_source.provider().output().selection();
        let (_, manifests) = self.mixed_source_diagnostic_context(
            snapshot,
            mixed_source,
            audit_plan,
            root_manifest_file,
            dependency_manifest_files,
        )?;
        Ok(mixed_source_identity_failure_diagnostics(
            selection,
            audit_plan.source_identities().failures(),
            &manifests,
        )?)
    }

    /// Builds source-bound diagnostics for reportable final-source reload
    /// failures and retained lexical/parser issues. Local and Git path-
    /// requirement failures use their retained final manifest, and package
    /// identity mismatches point at the changed final header. The MVS chain
    /// stays attached to the original manifests.
    ///
    /// The audit plan must belong to `mixed_source`, and snapshot manifests
    /// must match the contracts used for the original MVS load. The returned
    /// value carries the possibly augmented snapshot needed to resolve every
    /// diagnostic file ID.
    pub fn mixed_source_reload_diagnostics(
        &self,
        snapshot: &WorkspaceSnapshot,
        mixed_source: &MixedSourceMvsOutput,
        audit_plan: &MixedSourceFinalAuditPlan,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
    ) -> Result<MixedSourceReloadDiagnostics, RootDatabaseError> {
        let (diagnostic_snapshot, manifests) = self.mixed_source_diagnostic_context(
            snapshot,
            mixed_source,
            audit_plan,
            root_manifest_file,
            dependency_manifest_files,
        )?;
        let diagnostics =
            mixed_source_reload_failure_diagnostics(mixed_source, audit_plan, &manifests)?;
        Ok(MixedSourceReloadDiagnostics {
            snapshot: diagnostic_snapshot,
            diagnostics,
        })
    }

    fn mixed_source_diagnostic_context(
        &self,
        snapshot: &WorkspaceSnapshot,
        mixed_source: &MixedSourceMvsOutput,
        audit_plan: &MixedSourceFinalAuditPlan,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
    ) -> Result<(WorkspaceSnapshot, MvsManifestLocations), RootDatabaseError> {
        if !audit_plan.belongs_to(mixed_source) {
            return Err(RootDatabaseError::MixedSourceAuditLoadMismatch);
        }
        let (diagnostic_snapshot, final_manifest_file_ids) =
            mixed_source_diagnostic_snapshot(snapshot, audit_plan)?;
        let selection = mixed_source.provider().output().selection();
        let mut manifests = self.mvs_manifest_locations(
            &diagnostic_snapshot,
            selection,
            root_manifest_file,
            dependency_manifest_files,
        )?;
        for (package_version, file_id) in final_manifest_file_ids {
            manifests.insert_final_package(MvsManifestLocation::from_final_snapshot(
                &diagnostic_snapshot,
                file_id,
                &package_version,
            )?)?;
        }
        let provider = mixed_source.provider().output();
        if manifests.root().is_none_or(|manifest| {
            manifest.manifest_digest() != provider.root_contract().manifest_digest()
        }) {
            return Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                selection.root().clone(),
            ));
        }
        if manifests.packages().len() != provider.dependency_contracts().len() {
            let package_version = selection
                .read_contracts()
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| selection.root().clone());
            return Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                package_version,
            ));
        }
        for (package_version, manifest) in manifests.packages() {
            let Some(contract) = provider.dependency_contracts().get(package_version) else {
                return Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                    package_version.clone(),
                ));
            };
            if manifest.manifest_digest() != contract.manifest_digest() {
                return Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                    package_version.clone(),
                ));
            }
        }
        Ok((diagnostic_snapshot, manifests))
    }

    fn mvs_manifest_locations(
        &self,
        snapshot: &WorkspaceSnapshot,
        selection: &MvsSelectionOutput,
        root_manifest_file: FileId,
        dependency_manifest_files: &BTreeMap<MvsPackageVersion, FileId>,
    ) -> Result<MvsManifestLocations, RootDatabaseError> {
        self.validate_package_manifest_file_key(root_manifest_file, selection.root())?;
        let root = MvsManifestLocation::from_snapshot(
            snapshot,
            root_manifest_file,
            selection.root(),
            PackageRole::Root,
        )?;
        let mut manifests = MvsManifestLocations::default();
        manifests.set_root(root)?;

        for package_version in selection.read_contracts().keys() {
            let Some(file_id) = dependency_manifest_files.get(package_version).copied() else {
                return Err(RootDatabaseError::MissingPackageManifest(
                    package_version.clone(),
                ));
            };
            manifests.insert_package(MvsManifestLocation::from_snapshot(
                snapshot,
                file_id,
                package_version,
                PackageRole::Dependency,
            )?)?;
        }
        if let Some(package_version) = dependency_manifest_files
            .keys()
            .find(|package_version| !selection.read_contracts().contains_key(*package_version))
        {
            return Err(RootDatabaseError::UnexpectedPackageManifest(
                package_version.clone(),
            ));
        }
        Ok(manifests)
    }

    fn validate_package_manifest_file_key(
        &self,
        file_id: FileId,
        package_version: &MvsPackageVersion,
    ) -> Result<(), RootDatabaseError> {
        let file_key = self
            .files
            .file_key(file_id)
            .ok_or(RootDatabaseError::FileNotInterned)?;
        let expected_package_key = format!(
            "{}#{}",
            package_version.package(),
            package_version.node().line()
        );
        if file_key.package_key() != expected_package_key || file_key.path() != "sai.package" {
            return Err(RootDatabaseError::PackageManifestFileKeyMismatch {
                file_id,
                expected_package_key,
                found_package_key: file_key.package_key().to_owned(),
                found_path: file_key.path().to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::package_graph::{PackageGraphEdge, PackageGraphNode};
    use sai_package::local_source::LocalPackageDirectory;
    use sai_package::mixed_source::resolve_mixed_source_packages;
    use sai_package::mvs::{MvsContract, MvsEdge, MvsPackageVersion, select};
    use sai_package::validation::PackageValidationContext;
    use sai_package::version::Version;
    use sai_package::version_req::VersionReq;
    use sai_vfs::{
        PackageFileEntry, PackageRelativePath, SnapshotFile, SourceFile, WorkspaceSnapshot,
    };

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-driver-test-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).expect("test directory is created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(file_id: FileId, contents: &[u8], version: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(file_id, contents).expect("source decodes")),
            "src/lib.sai",
            Some("acme:app@0.1.0".to_owned()),
            Some(version),
        )])
        .expect("workspace snapshot is valid")
    }

    fn snapshot_with_version(snapshot: &WorkspaceSnapshot, version: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot::new(snapshot.iter().map(|file| {
            SnapshotFile::new(
                Arc::clone(file.source()),
                file.path().to_owned(),
                file.package().map(str::to_owned),
                Some(version),
            )
        }))
        .expect("versioned workspace snapshot is valid")
    }

    fn snapshot_with_unrelated_file(snapshot: &WorkspaceSnapshot) -> WorkspaceSnapshot {
        let mut files = snapshot.iter().cloned().collect::<Vec<_>>();
        files.push(SnapshotFile::new(
            Arc::new(
                SourceFile::decode(FileId::new(u32::MAX), b"fn unrelated() {}\n")
                    .expect("unrelated source decodes"),
            ),
            "src/unrelated.sai",
            Some("acme:phase1@1.0.0".to_owned()),
            Some(1),
        ));
        WorkspaceSnapshot::new(files).expect("workspace snapshot with unrelated file is valid")
    }

    fn phase1_fixture(
        root_contents: &[u8],
        child_contents: &[u8],
    ) -> (RootDatabase, ModuleTree, WorkspaceSnapshot, PackageId) {
        const PACKAGE_KEY: &str = "acme:phase1#1";
        const PACKAGE_IDENTITY: &str = "acme:phase1@1.0.0";
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package(PACKAGE_KEY)
            .expect("package identity interns");
        let package_key = PackageKey::new(PACKAGE_KEY).expect("package key is valid");
        database
            .set_package_files(
                package_key,
                PackageFilesSnapshot::new(
                    true,
                    [
                        PackageFileEntry::new("lib.sai", PackageFileKind::File)
                            .expect("root entry is valid"),
                        PackageFileEntry::new("child.sai", PackageFileKind::File)
                            .expect("child entry is valid"),
                    ],
                )
                .expect("package source inventory is valid"),
                Durability::Low,
            )
            .expect("package inventory is recorded");
        database
            .set_file_contents(
                FileKey::new(PACKAGE_KEY, "src/lib.sai").expect("root file key is valid"),
                Arc::from(root_contents),
                Durability::Low,
            )
            .expect("root source is recorded");
        database
            .set_file_contents(
                FileKey::new(PACKAGE_KEY, "src/child.sai").expect("child file key is valid"),
                Arc::from(child_contents),
                Durability::Low,
            )
            .expect("child source is recorded");

        let module_tree = database
            .module_tree(
                package,
                PackageSourceDirectory::new("src").expect("source directory is valid"),
                None,
            )
            .expect("module tree is constructed");
        assert_eq!(module_tree.modules.len(), 2);
        let snapshot = WorkspaceSnapshot::new(
            module_tree
                .modules
                .iter()
                .map(|module| {
                    let key = database
                        .file_key(module.file)
                        .expect("module has a stable file key")
                        .clone();
                    let contents = match database
                        .file_contents(key.clone())
                        .expect("source input can be read")
                    {
                        Some(FileContents::Bytes(contents)) => contents,
                        other => panic!("module source bytes are missing: {other:?}"),
                    };
                    SnapshotFile::new(
                        Arc::new(
                            SourceFile::decode(module.file, &contents)
                                .expect("module source decodes"),
                        ),
                        key.path().to_owned(),
                        Some(PACKAGE_IDENTITY.to_owned()),
                        Some(17),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .expect("workspace snapshot is valid");
        (database, module_tree.as_ref().clone(), snapshot, package)
    }

    #[test]
    fn top_level_owner_kinds_map_to_their_item_tree_kinds() {
        let cases = [
            (TopLevelOwnerKind::Function, ItemTreeKind::Function),
            (TopLevelOwnerKind::Struct, ItemTreeKind::Struct),
            (TopLevelOwnerKind::Alias, ItemTreeKind::Alias),
            (TopLevelOwnerKind::Refined, ItemTreeKind::Refined),
            (TopLevelOwnerKind::Enum, ItemTreeKind::Enum),
            (TopLevelOwnerKind::Distinct, ItemTreeKind::Distinct),
            (TopLevelOwnerKind::Resource, ItemTreeKind::Resource),
            (TopLevelOwnerKind::Trait, ItemTreeKind::Trait),
            (TopLevelOwnerKind::Impl, ItemTreeKind::Impl),
            (TopLevelOwnerKind::Const, ItemTreeKind::Const),
            (TopLevelOwnerKind::Invariant, ItemTreeKind::Invariant),
            (TopLevelOwnerKind::Effect, ItemTreeKind::Effect),
            (TopLevelOwnerKind::Interface, ItemTreeKind::Interface),
            (TopLevelOwnerKind::World, ItemTreeKind::World),
            (TopLevelOwnerKind::ExternBlock, ItemTreeKind::ExternBlock),
            (TopLevelOwnerKind::Test, ItemTreeKind::Test),
            (TopLevelOwnerKind::Property, ItemTreeKind::Property),
        ];
        for (owner_kind, item_tree_kind) in cases {
            assert_eq!(
                item_tree_kind_for_top_level_owner(owner_kind),
                item_tree_kind
            );
        }
    }

    #[test]
    fn top_level_owner_dispatch_rejects_current_item_kind_mismatch() {
        let root = b"fn f() {}\n";
        let child = b"";
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root, child);
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("current-snapshot Phase 1 collection succeeds");
        let descriptor = *collection
            .scopes
            .top_level_owners
            .first()
            .expect("function owner is indexed");
        let inputs = database
            .current_owner_inputs(&snapshot, descriptor.file, &collection.target_cfg)
            .expect("current owner inputs are available");
        let mut retained_item_kinds = inputs.retained_item_kinds.clone();
        retained_item_kinds.insert(descriptor.item_id, ItemTreeKind::Struct);
        let def_paths = Arc::clone(
            database
                .def_paths
                .get(&descriptor.owner.def_id.package())
                .expect("owner package has a DefPath interner"),
        );
        let expected_path = def_paths
            .resolve_def_id(descriptor.owner.def_id.to_def_id())
            .expect("owner path resolves")
            .expect("owner path exists");
        let arena = HirArena::new();
        let context = HirLoweringContext::new(
            &inputs.ast_ids,
            &snapshot,
            &inputs.parsed.root,
            &arena,
            &database.symbols,
            &def_paths,
        );
        let error = match lower_top_level_owner(
            descriptor,
            &expected_path,
            &inputs.filtered_item_tree,
            &retained_item_kinds,
            &context,
        ) {
            Ok(_) => panic!("a mismatched current item kind must not lower"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            OwnerControlFlowResolutionError::TopLevelOwnerKindMismatch {
                owner: descriptor.owner,
                file: descriptor.file,
                descriptor: TopLevelOwnerKind::Function,
                current: ItemTreeKind::Struct,
            }
        );
    }

    #[test]
    fn function_owner_label_adapter_lowers_current_target_descriptors_in_source_order() {
        let root = concat!(
            "fn choose() {\n",
            "    'outer: loop {\n",
            "        loop { break 'outer; }\n",
            "    }\n",
            "}\n",
            "@target(\"wasm64-core\")\n",
            "fn excluded() { break; }\n",
            "fn choose() {}\n",
        );
        let child = "pub fn child() {}\n";
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(root.as_bytes(), child.as_bytes());
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("current-snapshot Phase 1 collection succeeds");

        assert_eq!(collection.scopes.function_owners.len(), 3);
        let resolved = database
            .resolve_function_owner_labels(&collection, &module_tree, &snapshot)
            .expect("current target function owners lower and resolve");
        assert!(resolved.matches_snapshot(&snapshot));
        assert!(resolved.matches_collection(&collection));
        assert_eq!(resolved.target_cfg(), &target_cfg);
        assert_eq!(resolved.owners().len(), 3);
        assert_eq!(
            resolved
                .owners()
                .iter()
                .map(FunctionOwnerLabelResolution::owner)
                .collect::<Vec<_>>(),
            collection
                .scopes
                .function_owners
                .iter()
                .map(|descriptor| descriptor.owner)
                .collect::<Vec<_>>(),
            "the driver preserves Phase 1's module and source order"
        );
        assert_eq!(
            resolved.owners()[0]
                .resolution()
                .expect("first function resolves")
                .labels
                .len(),
            1
        );
        assert!(
            resolved.owners()[0]
                .resolution()
                .expect("first function resolves")
                .labels
                .values()
                .all(|target| matches!(target, Ok(loop_id) if loop_id.owner == resolved.owners()[0].owner()))
        );
        assert!(resolved.owners().iter().all(|owner| {
            owner
                .resolution()
                .is_ok_and(|labels| labels.diagnostics.is_empty())
                && owner.lowering_diagnostics().is_empty()
        }));
        let root_tree = database
            .def_collector(&snapshot, module_tree.modules[0].file)
            .expect("root item tree is available");
        assert_eq!(
            root_tree
                .items
                .iter()
                .filter(|entry| entry.kind == ItemTreeKind::Function)
                .count(),
            3,
            "the unfiltered root tree contains the target-excluded function"
        );
        assert_eq!(
            collection
                .scopes
                .function_owners
                .iter()
                .filter(|descriptor| descriptor.file == module_tree.modules[0].file)
                .count(),
            2,
            "target filtering excludes its function from the owner descriptors"
        );
        let mut changed_module_tree = module_tree.clone();
        changed_module_tree.modules[1].parent_dir = None;
        let changed_collection = database
            .collect_phase1_package_scopes_collection(
                &changed_module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("same-snapshot collection with changed package topology succeeds");
        assert_ne!(
            collection.input_fingerprint(),
            changed_collection.input_fingerprint(),
            "package topology contributes to collection identity independently of the source snapshot"
        );
        assert!(!resolved.matches_collection(&changed_collection));
        let mut stale_module_tree = module_tree.clone();
        stale_module_tree.modules[0].parent_dir = Some(ModuleId::ROOT);
        assert!(matches!(
            database.resolve_function_owner_labels(&collection, &stale_module_tree, &snapshot),
            Err(FunctionOwnerLabelResolutionError::ModuleTreeMismatch)
        ));
    }

    #[test]
    fn function_owner_adapter_resolves_locals_with_each_modules_aliases() {
        let root = concat!(
            "use acme:phase1/child as child_alias\n",
            "@target(\"wasm64-core\")\n",
            "use acme:phase1/child as hidden_alias\n",
            "fn root_fn(child_alias: Bool) { let next = child_alias; next }\n",
            "fn filtered_fn(hidden_alias: Bool) { hidden_alias }\n",
        );
        let child = "pub fn child_fn(child_alias: Bool) { child_alias }\n";
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(root.as_bytes(), child.as_bytes());
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("current-snapshot Phase 1 collection succeeds");

        let resolved = database
            .resolve_function_owner_analyses(&collection, &module_tree, &snapshot)
            .expect("function owner analyses succeed");
        assert_eq!(resolved.owners().len(), 3);

        let root_local = resolved.owners()[0]
            .local_resolution()
            .expect("root local resolution succeeds")
            .expect("root function has a body");
        assert_eq!(root_local.paths.len(), 2);
        assert_eq!(root_local.bindings.len(), 2);
        assert!(
            root_local
                .paths
                .values()
                .all(|path| matches!(path.base, Res::Local(_)))
        );
        assert_eq!(
            root_local
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule)
                .count(),
            1,
            "the root module alias shadows the first function's parameter"
        );

        let filtered_local = resolved.owners()[1]
            .local_resolution()
            .expect("filtered local resolution succeeds")
            .expect("filtered function has a body");
        assert_eq!(filtered_local.paths.len(), 1);
        assert!(
            !filtered_local
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule),
            "a target-filtered module alias is absent from the module scope"
        );

        let child_local = resolved.owners()[2]
            .local_resolution()
            .expect("child local resolution succeeds")
            .expect("child function has a body");
        assert_eq!(child_local.paths.len(), 1);
        assert!(
            !child_local
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule),
            "the root module's alias does not leak into the child module"
        );
    }

    #[test]
    fn function_owner_adapter_does_not_treat_a_direct_interface_as_a_module_alias() {
        let root = "interface codec {}\nfn run(codec: Bool) { codec }\n";
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("current-snapshot Phase 1 collection succeeds");

        let resolved = database
            .resolve_function_owner_analyses(&collection, &module_tree, &snapshot)
            .expect("function owner analyses succeed");
        let local = resolved.owners()[0]
            .local_resolution()
            .expect("local resolution succeeds")
            .expect("function has a body");
        assert!(
            !local
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule),
            "a directly declared interface is not a `use` module alias"
        );
    }

    #[test]
    fn function_owner_adapter_keeps_unresolved_test_only_module_aliases() {
        let root = concat!(
            "use acme:test-support/fixtures as client\n",
            "fn run(client: Bool) { client }\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires test { acme:test-support @ ^1 }\n"[..],
        );
        let collection = database
            .collect_phase1_package_scopes_with_manifest_and_foreign_scopes(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
                manifest,
                PackageRole::Root,
                &[],
            )
            .expect("current-snapshot Phase 1 collection succeeds");
        let import = &collection.scopes.modules[ModuleId::ROOT.raw() as usize].imports[0];
        assert!(import.test_only);
        assert!(import.item_index.is_none());
        assert!(matches!(import.res, Res::TestOnly(_)));

        let resolved = database
            .resolve_function_owner_analyses(&collection, &module_tree, &snapshot)
            .expect("function owner analyses succeed");
        let local = resolved.owners()[0]
            .local_resolution()
            .expect("local resolution succeeds")
            .expect("function has a body");
        assert_eq!(
            local
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule)
                .count(),
            1,
            "a test-only module alias remains a module alias before test analysis resolves it"
        );
    }

    #[test]
    fn function_owner_control_flow_reports_contract_jumps_without_label_cascades() {
        let root = concat!(
            "fn contracted() -> Bool ",
            "requires { break 'missing; } ",
            "ensures { return true; } ",
            "{ true }\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("function contracts collect");

        let resolved = database
            .resolve_function_owner_labels(&collection, &module_tree, &snapshot)
            .expect("function owner control flow resolves");

        let owner = resolved.owners().first().expect("function owner resolves");
        let labels = owner.resolution().expect("function lowering succeeds");
        assert!(labels.labels.is_empty());
        assert_eq!(
            labels
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                sai_diagnostics::Code::InvalidContractExpression,
                sai_diagnostics::Code::InvalidContractExpression,
            ]
        );
    }

    #[test]
    fn function_owner_control_flow_reports_remaining_contract_restrictions() {
        let root = concat!(
            "fn contracted(value: Bool) -> Bool ",
            "requires { ",
            "let okay = value; ",
            "value?; ",
            "value.await; ",
            "value = true; ",
            "value += true; ",
            "var changed = true; ",
            "unsafe { true }; ",
            "region r { true }; ",
            "fn() { return true; }; ",
            "} ",
            "{ true }\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("function contracts collect");

        let resolved = database
            .resolve_function_owner_labels(&collection, &module_tree, &snapshot)
            .expect("function owner control flow resolves");

        let owner = resolved.owners().first().expect("function owner resolves");
        let labels = owner.resolution().expect("function lowering succeeds");
        assert_eq!(
            labels
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [sai_diagnostics::Code::InvalidContractExpression; 8]
        );
    }

    #[test]
    fn invariant_owner_adapter_reports_forbidden_predicate_syntax() {
        let root = concat!(
            "use acme:phase1/child as local\n",
            "invariant Account { { let local = true; local? }, true }\n",
            "invariant Safe { true }\n",
        );
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(root.as_bytes(), b"pub fn child_fn() {}\n");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("invariant declarations collect");
        assert_eq!(collection.scopes().invariant_owners.len(), 2);

        let resolved = database
            .resolve_invariant_owner_predicates(&collection, &module_tree, &snapshot)
            .expect("invariant owner predicates resolve");

        assert!(resolved.matches_collection(&collection));
        assert_eq!(resolved.owners().len(), 2);
        assert_eq!(
            resolved.owners()[0]
                .resolution()
                .expect("first invariant lowers")
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [sai_diagnostics::Code::InvalidContractExpression]
        );
        let account_locals = resolved.owners()[0]
            .local_resolution()
            .expect("invariant local resolution succeeds")
            .expect("invariant owner has a body");
        assert_eq!(account_locals.bindings.len(), 1);
        assert_eq!(account_locals.paths.len(), 1);
        assert!(
            account_locals
                .paths
                .values()
                .all(|path| matches!(path.base, Res::Local(_))),
            "the block-local use resolves even though the predicate has a separate restriction diagnostic"
        );
        assert_eq!(
            account_locals
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::ShadowsModule)
                .count(),
            1,
            "local bindings in invariant predicates use the exact module's aliases"
        );
        assert!(
            resolved.owners()[1]
                .resolution()
                .expect("second invariant lowers")
                .is_empty()
        );
        let safe_locals = resolved.owners()[1]
            .local_resolution()
            .expect("second invariant local resolution succeeds")
            .expect("second invariant owner has a body");
        assert!(safe_locals.bindings.is_empty());
        assert!(safe_locals.paths.is_empty());
    }

    #[test]
    fn refinement_owner_adapter_checks_refined_and_distinct_predicate_values() {
        let root = concat!(
            "type Positive = I32 where { let local = true; local? }\n",
            "distinct Age = U8 where { let local = true; local? }\n",
            "distinct Plain = U8\n",
            "type Safe = I32 where self > 0\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("refinement predicate owners collect");
        assert_eq!(collection.scopes().refinement_predicate_owners.len(), 3);

        let resolved = database
            .resolve_refinement_owner_predicates(&collection, &module_tree, &snapshot)
            .expect("refinement predicate owners resolve");

        assert!(resolved.matches_collection(&collection));
        assert_eq!(resolved.owners().len(), 3);
        assert_eq!(
            resolved.owners()[0]
                .resolution()
                .expect("refined owner lowers")
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [sai_diagnostics::Code::InvalidContractExpression]
        );
        assert_eq!(
            resolved.owners()[1]
                .resolution()
                .expect("distinct owner lowers")
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [sai_diagnostics::Code::InvalidContractExpression]
        );
        for owner in &resolved.owners()[..2] {
            let locals = owner
                .local_resolution()
                .expect("refinement local resolution succeeds")
                .expect("predicate owner has a body");
            assert_eq!(locals.bindings.len(), 1);
            assert_eq!(locals.paths.len(), 1);
            assert!(
                locals
                    .paths
                    .values()
                    .all(|path| matches!(path.base, Res::Local(_))),
                "refinement local use resolves independently of predicate diagnostics"
            );
        }
        assert!(
            resolved.owners()[2]
                .resolution()
                .expect("valid refined owner lowers")
                .is_empty()
        );
        let self_locals = resolved.owners()[2]
            .local_resolution()
            .expect("valid refined local resolution succeeds")
            .expect("valid refined owner has a body");
        assert!(
            self_locals.paths.is_empty(),
            "implicit self remains for owner-aware resolution"
        );
    }

    #[test]
    fn extern_function_adapter_checks_bodyless_requires_and_ensures_per_owner() {
        let root = concat!(
            "unsafe extern \"wasm\" module \"env\" {\n",
            "  fn contracted(value: Bool) -> Bool ",
            "requires { break 'missing; } ",
            "ensures { return true; }\n",
            "  fn safe(value: Bool) -> Bool ",
            "requires { value } ",
            "ensures { result }\n",
            "  fn no_contract();\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("extern function owners collect");
        assert_eq!(collection.scopes().extern_function_owners.len(), 3);

        let resolved = database
            .resolve_extern_function_predicates(&collection, &module_tree, &snapshot)
            .expect("extern function predicates resolve");

        assert!(resolved.matches_collection(&collection));
        assert_eq!(resolved.owners().len(), 3);
        assert_eq!(
            resolved.owners()[0]
                .resolution()
                .expect("first extern function lowers")
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                sai_diagnostics::Code::InvalidContractExpression,
                sai_diagnostics::Code::InvalidContractExpression,
            ]
        );
        assert!(
            resolved.owners()[1]
                .resolution()
                .expect("second extern function lowers")
                .is_empty()
        );
        let safe_locals = resolved.owners()[1]
            .local_resolution()
            .expect("extern local resolution succeeds")
            .expect("extern declaration retains a contract body");
        assert_eq!(safe_locals.bindings.len(), 1);
        assert_eq!(safe_locals.paths.len(), 1);
        assert!(
            safe_locals
                .paths
                .values()
                .all(|path| matches!(path.base, Res::Local(_))),
            "extern contract parameters resolve through the function's source map"
        );
        assert!(
            resolved.owners()[2]
                .resolution()
                .expect("bodyless extern function lowers")
                .is_empty()
        );
        assert!(
            resolved.owners()[2]
                .local_resolution()
                .expect("bodyless extern local resolution succeeds")
                .is_none(),
            "an extern function without a body or contracts has no local scope"
        );
    }

    #[test]
    fn extern_function_adapter_uses_target_filtered_block_disambiguators() {
        let root = concat!(
            "@target(\"wasm64-core\")\n",
            "extern \"wasm\" module \"env\" { fn filtered() }\n",
            "extern \"wasm\" module \"env\" {\n",
            "  fn retained() -> Bool requires { return true; }\n",
            "}\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("target-filtered extern function owners collect");
        assert_eq!(collection.scopes().extern_function_owners.len(), 1);

        let resolved = database
            .resolve_extern_function_predicates(&collection, &module_tree, &snapshot)
            .expect("target-filtered extern function predicate resolves");

        assert!(resolved.matches_collection(&collection));
        assert_eq!(resolved.owners().len(), 1);
        assert_eq!(
            resolved.owners()[0]
                .resolution()
                .expect("retained extern function lowers")
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [sai_diagnostics::Code::InvalidContractExpression]
        );
    }

    #[test]
    fn function_owner_label_adapter_rejects_a_collection_from_another_snapshot() {
        let root = b"fn duplicate() { 0 }\nfn duplicate() { 1 }\n";
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root, b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let old_collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("old snapshot collection succeeds");

        let root_file = module_tree.modules[0].file;
        let edited_root = b"fn duplicate() { 2 }\nfn duplicate() { 0 }\nfn duplicate() { 1 }\n";
        database
            .set_file_contents(
                FileKey::new("acme:phase1#1", "src/lib.sai").expect("root key is valid"),
                Arc::from(edited_root.as_slice()),
                Durability::Low,
            )
            .expect("edited source is recorded");
        let new_snapshot = WorkspaceSnapshot::new(snapshot.iter().map(|file| {
            let source = if file.source().id() == root_file {
                Arc::new(SourceFile::decode(root_file, edited_root).expect("edited source decodes"))
            } else {
                Arc::clone(file.source())
            };
            SnapshotFile::new(
                source,
                file.path().to_owned(),
                file.package().map(str::to_owned),
                Some(18),
            )
        }))
        .expect("edited workspace snapshot is valid");

        assert!(matches!(
            database.resolve_function_owner_labels(&old_collection, &module_tree, &new_snapshot),
            Err(FunctionOwnerLabelResolutionError::SnapshotMismatch)
        ));

        let new_collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &new_snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("new snapshot collection succeeds");
        let resolved = database
            .resolve_function_owner_labels(&new_collection, &module_tree, &new_snapshot)
            .expect("new descriptors lower against the new snapshot");
        assert_eq!(resolved.owners().len(), 3);
    }

    #[test]
    fn function_owner_label_adapter_keeps_processing_after_one_descriptor_failure() {
        let root = "fn first() {}\nfn later() { break; }\n";
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let mut collection = database
            .collect_phase1_package_scopes_collection(
                &module_tree,
                &snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 collection succeeds");
        // Simulate an internal descriptor-corruption failure in the legacy
        // function index: the public collection keeps this index immutable.
        let later_owner = collection.scopes.function_owners[1].owner;
        collection.scopes.function_owners[0].owner = later_owner;

        let resolved = database
            .resolve_function_owner_labels(&collection, &module_tree, &snapshot)
            .expect("per-owner lower failures do not abort the package");
        assert_eq!(resolved.owners().len(), 2);
        assert!(matches!(
            resolved.owners()[0].resolution(),
            Err(FunctionOwnerLabelResolutionError::TopLevelOwnerDescriptorMismatch { .. })
        ));
        assert!(matches!(
            resolved.owners()[0].local_resolution(),
            Err(FunctionOwnerResolutionError::TopLevelOwnerDescriptorMismatch { .. })
        ));
        let later = resolved.owners()[1]
            .resolution()
            .expect("later function still resolves");
        assert!(
            later
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == sai_diagnostics::Code::BreakOutsideLoop })
        );
    }

    fn graph_edge(target: &str, visible_identity: &str) -> PackageGraphEdge {
        PackageGraphEdge::new(
            PackageKey::new(target).expect("graph target key is valid"),
            visible_identity,
        )
        .expect("graph edge is valid")
    }

    fn graph_node(
        key: &str,
        identity: &str,
        version: &str,
        role: PackageGraphRole,
        edges: Vec<PackageGraphEdge>,
    ) -> PackageGraphNode {
        PackageGraphNode::new(
            PackageKey::new(key).expect("graph package key is valid"),
            identity,
            Version::parse(version).expect("graph package version is valid"),
            PackageForm::Source,
            role,
            edges,
        )
        .expect("graph package node is valid")
    }

    fn phase1_graph(dependency_version: &str, dependency_has_leaf: bool) -> PackageGraph {
        let dependency_edges = if dependency_has_leaf {
            vec![graph_edge("acme:leaf#1", "acme:leaf")]
        } else {
            Vec::new()
        };
        let mut nodes = vec![
            graph_node(
                "acme:phase1#1",
                "acme:phase1",
                "1.0.0",
                PackageGraphRole::Root,
                vec![graph_edge("acme:dep#1", "acme:dep")],
            ),
            graph_node(
                "acme:dep#1",
                "acme:dep",
                dependency_version,
                PackageGraphRole::Dependency,
                dependency_edges,
            ),
        ];
        if dependency_has_leaf {
            nodes.push(graph_node(
                "acme:leaf#1",
                "acme:leaf",
                "1.0.0",
                PackageGraphRole::Dependency,
                Vec::new(),
            ));
        }
        PackageGraph::new(
            Version::parse("1.8.2").expect("toolchain version is valid"),
            nodes,
        )
        .expect("active graph is valid")
    }

    fn phase1_root_only_graph() -> PackageGraph {
        phase1_root_only_graph_with_toolchain("1.8.2")
    }

    fn phase1_root_only_graph_with_toolchain(toolchain_version: &str) -> PackageGraph {
        PackageGraph::new(
            Version::parse(toolchain_version).expect("toolchain version is valid"),
            [graph_node(
                "acme:phase1#1",
                "acme:phase1",
                "1.0.0",
                PackageGraphRole::Root,
                Vec::new(),
            )],
        )
        .expect("root-only graph is valid")
    }

    #[test]
    fn package_scopes_query_tracks_manifest_requirements_and_backdates_unrelated_edits() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(
            b"use acme:test-support/fixtures as fixtures\n",
            b"fn child() {}\n",
        );
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext::default();
        let manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires test { acme:test-support @ ^1 }\n"[..],
        );
        let first = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                context,
                Arc::clone(&manifest),
                PackageRole::Root,
            )
            .expect("manifest-backed package scopes collect");
        let first_import = &first.modules[0].imports[0];
        assert!(first_import.test_only);
        assert!(matches!(
            first_import.res,
            sai_resolve::package_scopes::Res::TestOnly(_)
        ));

        let package_key = PackageKey::new("acme:phase1#1").expect("package key is valid");
        let first_memo = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("first package-scope memo exists");
        let unrelated_edit = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              description \"does not alter requirements\"\n\
              requires test { acme:test-support @ ^1 }\n"[..],
        );
        let second = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                context,
                unrelated_edit,
                PackageRole::Root,
            )
            .expect("scopes collect after unrelated manifest edit");
        assert!(second.modules[0].imports[0].test_only);
        let second_memo = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("second package-scope memo exists");
        assert!(
            Arc::ptr_eq(&first_memo, &second_memo),
            "manifest edits that preserve the normalized requirements backdate"
        );

        let restricted = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires { acme:test-support @ ^1 only { wasm32-wasi } }\n"[..],
        );
        let third = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                context,
                restricted,
                PackageRole::Root,
            )
            .expect("scopes collect after the requirement changes");
        assert!(
            third
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InactiveDependency)
        );
        assert_eq!(
            third.modules[0].imports[0].res,
            sai_resolve::package_scopes::Res::Err
        );
        let third_memo = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key)
            .expect("third package-scope memo exists");
        assert!(
            !Arc::ptr_eq(&second_memo, &third_memo),
            "changing normalized requirements invalidates package scopes"
        );
    }

    #[test]
    fn package_scopes_query_tracks_root_package_context_for_test_edges() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(
            b"use acme:test-support/fixtures as fixtures\n",
            b"fn child() {}\n",
        );
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires test { acme:test-support @ ^1 }\n"[..],
        );
        let root_context = Phase1BuildContext {
            is_root_package: true,
            analyzes_tests: true,
            ..Phase1BuildContext::default()
        };
        let root_scopes = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                root_context,
                Arc::clone(&manifest),
                PackageRole::Root,
            )
            .expect("root test dependencies are classified");
        assert!(root_scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::TestDependencyUnavailable
        }));

        let member_context = Phase1BuildContext {
            is_root_package: false,
            ..root_context
        };
        let member_scopes = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                member_context,
                Arc::clone(&manifest),
                PackageRole::Root,
            )
            .expect("non-root test dependencies are rejected");
        assert!(member_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::TestDependencyUnavailable
        }));
        assert!(member_scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::UnknownPackage
                && diagnostic.code != sai_diagnostics::Code::UnknownModule
                && diagnostic.code != sai_diagnostics::Code::UnresolvedImport
        }));

        let repeated_root = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                root_context,
                manifest,
                PackageRole::Root,
            )
            .expect("the root result remains distinct from the member result");
        assert!(repeated_root.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::TestDependencyUnavailable
        }));
    }

    #[test]
    fn package_scopes_do_not_classify_requirements_recovered_from_invalid_manifests() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(
            b"use acme:test-support/fixtures as fixtures\n",
            b"fn child() {}\n",
        );
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let invalid_manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires test { acme:test-support @ ^1 }\n\
              invalid declaration\n"[..],
        );
        let scopes = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
                invalid_manifest,
                PackageRole::Root,
            )
            .expect("invalid manifest ownership stays with package validation");
        assert!(scopes.modules[0].imports.is_empty());
        assert!(scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::TestDependencyOutsideTest
                && diagnostic.code != sai_diagnostics::Code::InactiveDependency
        }));
    }

    #[test]
    fn declared_requirements_are_sorted_keep_the_first_identity_and_preserve_only_order() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:manifest#1")
            .expect("package key interns");
        let source: Arc<[u8]> = Arc::from(
            &b"package acme:manifest @ 1.0.0\n\
              requires {\n\
                  zed:dep @ ^1 only { wasm64-core, wasm32-core }\n\
                  zed:dep @ ^2 only { native }\n\
              }\n\
              requires test { acme:test @ ^1 }\n"[..],
        );

        database
            .set_package_manifest(package, source, PackageRole::Root, Durability::Low)
            .expect("manifest input is set");
        let requirements = database
            .declared_requirements(package)
            .expect("requirements query succeeds");

        assert_eq!(
            requirements.as_ref(),
            [
                DeclaredRequirement {
                    identity: "acme:test".to_owned(),
                    declaration_order: 2,
                    only: None,
                    test: true,
                },
                DeclaredRequirement {
                    identity: "zed:dep".to_owned(),
                    declaration_order: 0,
                    only: Some(Arc::from([
                        "wasm64-core".to_owned(),
                        "wasm32-core".to_owned(),
                    ])),
                    test: false,
                },
            ]
        );
    }

    #[test]
    fn declared_requirements_track_reordering_while_remaining_identity_sorted() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:reordered#1")
            .expect("package key interns");
        let first_source: Arc<[u8]> = Arc::from(
            &b"package acme:reordered @ 1.0.0\n\
              requires {\n\
                  zed:dep @ ^1\n\
                  acme:dep @ ^1\n\
              }\n"[..],
        );
        database
            .set_package_manifest(package, first_source, PackageRole::Root, Durability::Low)
            .expect("initial manifest input is set");
        let first = database
            .declared_requirements(package)
            .expect("initial requirements query succeeds");

        assert_eq!(
            first
                .iter()
                .map(|requirement| {
                    (requirement.identity.as_str(), requirement.declaration_order)
                })
                .collect::<Vec<_>>(),
            [("acme:dep", 1), ("zed:dep", 0)]
        );

        let reordered_source: Arc<[u8]> = Arc::from(
            &b"package acme:reordered @ 1.0.0\n\
              requires {\n\
                  acme:dep @ ^1\n\
                  zed:dep @ ^1\n\
              }\n"[..],
        );
        database
            .set_package_manifest(
                package,
                reordered_source,
                PackageRole::Root,
                Durability::Low,
            )
            .expect("reordered manifest input is set");
        let reordered = database
            .declared_requirements(package)
            .expect("reordered requirements query succeeds");

        assert!(
            !Arc::ptr_eq(&first, &reordered),
            "reordering first declarations changes the tracked requirement value"
        );
        assert_eq!(
            reordered
                .iter()
                .map(|requirement| {
                    (requirement.identity.as_str(), requirement.declaration_order)
                })
                .collect::<Vec<_>>(),
            [("acme:dep", 0), ("zed:dep", 1)]
        );
    }

    #[test]
    fn declared_requirements_retain_dependency_test_entries() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:dependency#2")
            .expect("package key interns");
        let source: Arc<[u8]> = Arc::from(
            &b"package acme:dependency @ 2.0.0\n\
              requires test { acme:test-support @ ^1 only { wasm32-core } }\n"[..],
        );

        database
            .set_package_manifest(package, source, PackageRole::Dependency, Durability::Medium)
            .expect("dependency manifest input is set");
        let requirements = database
            .declared_requirements(package)
            .expect("requirements query succeeds");

        assert_eq!(
            requirements.as_ref(),
            [DeclaredRequirement {
                identity: "acme:test-support".to_owned(),
                declaration_order: 0,
                only: Some(Arc::from(["wasm32-core".to_owned()])),
                test: true,
            }]
        );
    }

    #[test]
    fn declared_requirements_reject_recovered_entries_from_invalid_manifests() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:invalid-manifest#1")
            .expect("package key interns");
        let mut source = b"package acme:invalid-manifest @ 1.0.0\n\
              requires test { acme:test-support @ ^1 }\n\
              unexpected declaration\n//"
            .to_vec();
        for _ in 0..20_000 {
            source.extend_from_slice("\u{202e}".as_bytes());
        }
        source.push(b'\n');
        database
            .set_package_manifest(
                package,
                Arc::from(source),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("manifest input is set");

        assert!(matches!(
            database.declared_requirements(package),
            Err(DeclaredRequirementsError::InvalidManifest { .. })
        ));
    }

    #[test]
    fn declared_requirements_reject_manifest_identity_and_compatibility_mismatches() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let identity_mismatch = database
            .intern_package("acme:expected#1")
            .expect("package key interns");
        database
            .set_package_manifest(
                identity_mismatch,
                Arc::from(&b"package acme:found @ 1.0.0\nrequires { acme:dep @ ^1 }\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("identity-mismatched manifest input is retained for validation");
        assert!(matches!(
            database.declared_requirements(identity_mismatch),
            Err(DeclaredRequirementsError::ManifestPackageMismatch {
                expected_package_key,
                found_identity,
                found_compatibility_line,
            }) if expected_package_key == "acme:expected#1"
                && found_identity == "acme:found"
                && found_compatibility_line == "1"
        ));

        let line_mismatch = database
            .intern_package("acme:found#2")
            .expect("package key interns");
        database
            .set_package_manifest(
                line_mismatch,
                Arc::from(&b"package acme:found @ 1.0.0\nrequires { acme:dep @ ^1 }\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("line-mismatched manifest input is retained for validation");
        assert!(matches!(
            database.declared_requirements(line_mismatch),
            Err(DeclaredRequirementsError::ManifestPackageMismatch {
                expected_package_key,
                found_identity,
                found_compatibility_line,
            }) if expected_package_key == "acme:found#2"
                && found_identity == "acme:found"
                && found_compatibility_line == "1"
        ));
    }

    #[test]
    fn declared_requirements_backdate_unrelated_manifest_edits_and_track_semantic_changes() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:incremental#1")
            .expect("package key interns");
        let initial: Arc<[u8]> = Arc::from(
            &b"package acme:incremental @ 1.0.0\n\
              requires test { acme:test-support @ ^1 only { wasm32-core } }\n"[..],
        );
        database
            .set_package_manifest(package, initial, PackageRole::Root, Durability::Low)
            .expect("initial manifest input is set");
        let first = database
            .declared_requirements(package)
            .expect("initial requirements query succeeds");

        let unrelated_edit: Arc<[u8]> = Arc::from(
            &b"package acme:incremental @ 1.0.0\n\
              description \"same requirements\"\n\
              requires test { acme:test-support @ ^1 only { wasm32-core } }\n"[..],
        );
        database
            .set_package_manifest(package, unrelated_edit, PackageRole::Root, Durability::Low)
            .expect("unrelated manifest edit is set");
        let second = database
            .declared_requirements(package)
            .expect("requirements query remains valid");
        assert!(
            Arc::ptr_eq(&first, &second),
            "unrelated manifest fields should backdate the derived value"
        );

        let changed_requirement: Arc<[u8]> = Arc::from(
            &b"package acme:incremental @ 1.0.0\n\
              description \"same requirements\"\n\
              requires test { acme:test-support @ ^1 only { wasm64-core } }\n"[..],
        );
        database
            .set_package_manifest(
                package,
                changed_requirement,
                PackageRole::Root,
                Durability::Low,
            )
            .expect("changed target restriction is set");
        let third = database
            .declared_requirements(package)
            .expect("changed requirements query succeeds");
        assert!(!Arc::ptr_eq(&second, &third));
        assert_eq!(
            third[0].only.as_deref(),
            Some(["wasm64-core".to_owned()].as_slice())
        );
    }

    fn phase1_function_def(
        database: &RootDatabase,
        scopes: &Phase1PackageScopes,
        module: ModuleId,
        name: &str,
    ) -> DefId {
        use sai_resolve::package_scopes::{DefKind, Res};

        scopes
            .module(module)
            .expect("module scopes exist")
            .declared
            .iter()
            .find_map(|(symbol, binding)| {
                let symbol_name = database.resolve_symbol(*symbol).ok()?;
                if symbol_name.as_ref() != name {
                    return None;
                }
                match binding.res {
                    Res::Def(DefKind::Fn, def_id) => Some(def_id),
                    _ => None,
                }
            })
            .unwrap_or_else(|| panic!("function `{name}` was not declared"))
    }

    fn phase1_has_function(
        database: &RootDatabase,
        scopes: &Phase1PackageScopes,
        module: ModuleId,
        name: &str,
    ) -> bool {
        scopes
            .module(module)
            .expect("module scopes exist")
            .declared
            .keys()
            .any(|symbol| {
                database
                    .resolve_symbol(*symbol)
                    .is_ok_and(|symbol_name| symbol_name.as_ref() == name)
            })
    }

    #[test]
    fn check_target_configs_match_artifact_capabilities() {
        assert!(
            target_cfg_for_check("wasm32-core")
                .expect("target is known")
                .capabilities()
                .contains("host")
        );
        let wasm64_core = target_cfg_for_check("wasm64-core").expect("target is known");
        assert!(wasm64_core.capabilities().contains("host"));
        assert!(wasm64_core.capabilities().contains("memory64"));
        assert!(
            target_cfg_for_check("wasm32-wasi")
                .expect("target is known")
                .capabilities()
                .is_empty()
        );
        assert_eq!(
            target_cfg_for_check("wasm64-wasi")
                .expect("target is known")
                .capabilities(),
            &BTreeSet::from(["memory64".to_owned()])
        );
        let native = target_cfg_for_check("aarch64-macos").expect("target is known");
        assert_eq!(
            native.capabilities(),
            &BTreeSet::from(["native_ffi".to_owned(), "process".to_owned()])
        );
        assert_eq!(
            target_cfg_for_check("not-a-target"),
            Err(TargetCfgError::UnknownTarget)
        );
    }

    #[test]
    fn phase1_driver_bridge_filters_targets_and_reuses_package_def_paths() {
        fn module_alias_def(
            database: &RootDatabase,
            scope: &sai_resolve::package_scopes::Phase1ModuleScopes,
            alias: &str,
        ) -> Option<DefId> {
            scope.scope.iter().find_map(|(symbol, binding)| {
                let name = database.resolve_symbol(*symbol).ok()?;
                if name.as_ref() != alias {
                    return None;
                }
                match binding.res {
                    sai_resolve::package_scopes::Res::Def(
                        sai_resolve::package_scopes::DefKind::Module,
                        def_id,
                    ) => Some(def_id),
                    _ => None,
                }
            })
        }

        let root = concat!(
            "use acme:phase1/child\n",
            "use acme:phase1/child as explicit_child\n",
            "pub use acme:phase1/child as reexport_alias.{public_child}\n",
            "@target(\"wasm64-core\")\n",
            "use acme:phase1/child as wasm64_child\n",
            "@target(\"wasm64-core\")\n",
            "fn choose() {}\n",
            "fn choose() {}\n",
        )
        .as_bytes();
        let child = b"pub fn public_child() {}\n";
        let (mut database, module_tree, snapshot, _) = phase1_fixture(root, child);

        let wasm32 = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let wasm32_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &wasm32,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 scopes collect for both modules");
        assert_eq!(wasm32_scopes.modules.len(), 2);
        assert!(
            !wasm32_scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::DuplicateBinding)
        );

        let choose_def = phase1_function_def(&database, &wasm32_scopes, ModuleId::ROOT, "choose");
        let choose_path = database
            .resolve_def_path(choose_def)
            .expect("definition path resolves")
            .expect("definition was created by this database");
        let child_module = module_tree
            .modules
            .iter()
            .position(|module| module.file != module_tree.modules[0].file)
            .map(|index| ModuleId::from_raw(index as u32))
            .expect("child module exists");
        let root_module_scopes = &wasm32_scopes.modules[ModuleId::ROOT.raw() as usize];
        let child_alias_def = module_alias_def(&database, root_module_scopes, "child")
            .expect("the derived local module alias is in the root scope");
        let explicit_child_alias_def =
            module_alias_def(&database, root_module_scopes, "explicit_child")
                .expect("the explicit local module alias is in the root scope");
        let reexport_alias_def = module_alias_def(&database, root_module_scopes, "reexport_alias")
            .expect("the pub-use alias remains a local module alias");
        assert_eq!(
            child_alias_def,
            wasm32_scopes.modules[child_module.raw() as usize]
                .module_def_id
                .to_def_id()
        );
        assert_eq!(explicit_child_alias_def, child_alias_def);
        assert_eq!(reexport_alias_def, child_alias_def);
        assert!(module_alias_def(&database, root_module_scopes, "wasm64_child").is_none());
        assert_eq!(root_module_scopes.imports.len(), 4);
        let import_names = root_module_scopes
            .imports
            .iter()
            .map(|import| {
                database
                    .resolve_symbol(import.name)
                    .expect("import names resolve")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            import_names,
            ["child", "explicit_child", "reexport_alias", "public_child"]
        );
        assert_eq!(
            root_module_scopes
                .imports
                .iter()
                .map(|import| import.item_index)
                .collect::<Vec<_>>(),
            [None, None, None, Some(0)]
        );
        assert!(
            root_module_scopes
                .imports
                .iter()
                .all(|import| !import.test_only)
        );
        assert_eq!(
            root_module_scopes
                .imports
                .iter()
                .map(|import| import.is_pub)
                .collect::<Vec<_>>(),
            [false, false, true, true]
        );
        let reexport_symbol = root_module_scopes.imports[2].name;
        assert_eq!(
            root_module_scopes
                .scope
                .get(&reexport_symbol)
                .map(|binding| binding.visibility),
            Some(sai_hir::Visibility::Private)
        );
        for (raw_import_index, import) in root_module_scopes.imports.iter().enumerate() {
            let expected_origin = if import.item_index.is_some() {
                sai_resolve::package_scopes::BindingOrigin::PubUse {
                    import: sai_resolve::package_scopes::ImportIndex(raw_import_index as u32),
                }
            } else {
                sai_resolve::package_scopes::BindingOrigin::Use {
                    import: sai_resolve::package_scopes::ImportIndex(raw_import_index as u32),
                }
            };
            assert_eq!(
                root_module_scopes
                    .scope
                    .get(&import.name)
                    .map(|binding| binding.origin),
                Some(expected_origin)
            );
        }
        let child_def =
            phase1_function_def(&database, &wasm32_scopes, child_module, "public_child");
        let reexported_child = root_module_scopes
            .scope
            .iter()
            .find_map(|(symbol, binding)| {
                let name = database.resolve_symbol(*symbol).ok()?;
                (name.as_ref() == "public_child").then_some(binding.res)
            })
            .expect("listed public item is imported into the root scope");
        assert_eq!(
            reexported_child,
            sai_resolve::package_scopes::Res::Def(
                sai_resolve::package_scopes::DefKind::Fn,
                child_def,
            )
        );
        assert_eq!(
            root_module_scopes
                .members
                .get(
                    &root_module_scopes
                        .imports
                        .iter()
                        .find(|import| import.item_index.is_some())
                        .unwrap()
                        .name
                )
                .map(|binding| binding.res),
            Some(reexported_child)
        );
        assert!(
            database
                .resolve_def_path(child_def)
                .expect("child definition path resolves")
                .is_some()
        );

        let repeated = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &wasm32,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 can run again in the same database");
        let repeated_choose = phase1_function_def(&database, &repeated, ModuleId::ROOT, "choose");
        assert_eq!(repeated_choose, choose_def);
        assert_eq!(
            database
                .resolve_def_path(repeated_choose)
                .expect("repeated definition path resolves"),
            Some(choose_path)
        );

        let wasm64 = target_cfg_for_check("wasm64-core").expect("target config is valid");
        let wasm64_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &wasm64,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 scopes collect for wasm64");
        let duplicate = wasm64_scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::DuplicateBinding)
            .expect("target-filtered declarations report a duplicate binding");
        let wasm64_root_scopes = &wasm64_scopes.modules[ModuleId::ROOT.raw() as usize];
        assert!(module_alias_def(&database, wasm64_root_scopes, "wasm64_child").is_some());
        assert_eq!(wasm64_root_scopes.imports.len(), 5);
        assert_eq!(
            duplicate.primary().map(|location| location.file.snapshot),
            Some(Some(17))
        );
        let previous_declaration = duplicate
            .related()
            .iter()
            .find_map(|related| match related {
                sai_diagnostics::Related::DeclaredHere { location, .. } => Some(location),
                _ => None,
            });
        assert_eq!(
            previous_declaration.map(|location| location.file.snapshot),
            Some(Some(17))
        );
    }

    #[test]
    fn package_scopes_query_reuses_memo_for_the_same_prepared_input() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(b"fn stable() {}\n", b"");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let _ = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("phase 1 scopes collect");
        let package_key = PackageKey::new("acme:phase1#1").expect("package key is valid");
        let first = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("package scopes query is memoized");
        let _ = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("the repeated request succeeds");
        let second = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key)
            .expect("repeated package scopes query is memoized");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the derived query should reuse the same memo for identical inputs"
        );
    }

    #[test]
    fn package_scope_query_identity_tracks_foreign_scope_fingerprints() {
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let foreign_identity = |input_fingerprint| Phase1PackageScopesIdentity {
            package: PackageKey::new("acme:phase1#1").expect("package key is valid"),
            modules: Vec::new(),
            namespaces: Vec::new(),
            target_name: target.target_name().to_owned(),
            target_capabilities: target.capabilities().clone(),
            build_context: Phase1BuildContext::default(),
            declared_requirements: None,
            package_graph: None,
            foreign_packages: vec![Phase1ForeignPackageIdentity {
                visible_identity: "acme:dep".to_owned(),
                package_key: PackageKey::new("acme:dep#1").expect("package key is valid"),
                input_fingerprint,
            }],
        };
        let before = foreign_identity(Fingerprint::new(1));
        let after = foreign_identity(Fingerprint::new(2));

        assert_ne!(
            fingerprint(&before),
            fingerprint(&after),
            "a dependency scope change must invalidate its consumer's tracked query"
        );
    }

    #[test]
    fn package_scopes_graph_identity_distinguishes_unavailable_and_empty_edges() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(b"fn stable() {}\n", b"");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext::default();
        let package_key = PackageKey::new("acme:phase1#1").expect("package key is valid");

        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("scopes collect while resolution is unavailable");
        let unavailable = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("unavailable graph still has a package-scope memo");
        assert_eq!(unavailable.identity.package_graph, None);

        let graph = phase1_root_only_graph();
        assert!(
            database
                .set_package_graph(Some(graph.clone()))
                .expect("available graph input is recorded")
        );
        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("scopes collect with an available root-only graph");
        let available = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("available graph has a package-scope memo");
        assert_eq!(
            available.identity.package_graph,
            Some(Phase1PackageGraphProjection::Package {
                outgoing_edges: Vec::new()
            })
        );
        assert!(
            !Arc::ptr_eq(&unavailable, &available),
            "an available package with no edges differs from unavailable resolution"
        );

        assert!(
            !database
                .set_package_graph(Some(graph))
                .expect("identical graph input is accepted")
        );
        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("repeated available-graph collection succeeds");
        let repeated = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key)
            .expect("repeated collection has a package-scope memo");
        assert!(
            Arc::ptr_eq(&available, &repeated),
            "identical graph projections reuse the package-scope memo"
        );
    }

    #[test]
    fn package_scopes_graph_identity_tracks_only_outgoing_edges_and_target_selection() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(b"fn stable() {}\n", b"");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext::default();
        let package_key = PackageKey::new("acme:phase1#1").expect("package key is valid");

        database
            .set_package_graph(Some(phase1_graph("1.0.0", false)))
            .expect("initial graph is recorded");
        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("initial scopes collect");
        let initial = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("initial package-scope memo exists");

        database
            .set_package_graph(Some(phase1_graph("1.0.0", true)))
            .expect("unrelated dependency edge is recorded");
        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("scopes collect after an unrelated dependency edge changes");
        let unrelated_change = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("package-scope memo remains available");
        assert!(
            Arc::ptr_eq(&initial, &unrelated_change),
            "changes below the root's direct outgoing edges do not invalidate its local scopes"
        );

        database
            .set_package_graph(Some(phase1_graph("1.1.0", false)))
            .expect("selected dependency version change is recorded");
        database
            .collect_phase1_package_scopes(&module_tree, &snapshot, &target, context)
            .expect("scopes collect after an active target version changes");
        let target_change = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key)
            .expect("updated package-scope memo exists");
        assert!(
            !Arc::ptr_eq(&unrelated_change, &target_change),
            "the exact selected version of a direct edge target is part of the projection"
        );
    }

    #[test]
    fn package_scopes_classify_foreign_requirements_from_manifest() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"use acme:dep/module as dep\n", b"fn child() {}\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext {
            is_root_package: true,
            ..Phase1BuildContext::default()
        };
        let manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires { acme:dep @ ^1 }\n"[..],
        );

        database
            .set_package_graph(Some(phase1_graph("1.0.0", false)))
            .expect("graph with the dependency edge is recorded");
        let active = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                context,
                Arc::clone(&manifest),
                PackageRole::Root,
            )
            .expect("active dependency is classified");
        assert!(
            active.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented
            })
        );
        assert!(
            active
                .diagnostics
                .iter()
                .all(|diagnostic| { diagnostic.code != sai_diagnostics::Code::UnknownPackage })
        );

        database
            .set_package_graph(Some(phase1_root_only_graph()))
            .expect("available graph without the dependency edge is recorded");
        let missing_edge = database.collect_phase1_package_scopes_with_manifest(
            &module_tree,
            &snapshot,
            &target,
            context,
            manifest,
            PackageRole::Root,
        );
        assert!(
            matches!(
                missing_edge,
                Err(Phase1CollectionError::PackageGraphRequirementEdgeMismatch {
                    ref package,
                    ref identity,
                    expected_active: true,
                    found_active: false,
                }) if package.as_str() == "acme:phase1#1" && identity == "acme:dep"
            ),
            "a target-allowed declaration missing from an available graph is a graph/manifest mismatch"
        );
    }

    #[test]
    fn package_scopes_reject_active_edges_without_manifest_requirements() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"use acme:dep/module as dep\n", b"fn child() {}\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext {
            is_root_package: true,
            ..Phase1BuildContext::default()
        };
        let empty_manifest = Arc::<[u8]>::from(&b"package acme:phase1 @ 1.0.0\n"[..]);
        database
            .set_package_graph(Some(phase1_graph("1.0.0", false)))
            .expect("graph with an undeclared dependency edge is recorded");

        let result = database.collect_phase1_package_scopes_with_manifest(
            &module_tree,
            &snapshot,
            &target,
            context,
            empty_manifest,
            PackageRole::Root,
        );
        assert!(
            matches!(
                result,
                Err(Phase1CollectionError::PackageGraphRequirementEdgeMismatch {
                    ref identity,
                    expected_active: false,
                    found_active: true,
                    ..
                }) if identity == "acme:dep"
            ),
            "a graph edge absent from the valid manifest is a graph/manifest mismatch"
        );
    }

    #[test]
    fn package_scopes_allow_target_excluded_requirements_without_active_edges() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"use acme:dep/module as dep\n", b"fn child() {}\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext {
            is_root_package: true,
            ..Phase1BuildContext::default()
        };
        let inactive_manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires { acme:dep @ ^1 only { wasm64-core } }\n"[..],
        );
        database
            .set_package_graph(Some(phase1_root_only_graph()))
            .expect("graph without a target-excluded edge is recorded");

        let scopes = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                context,
                inactive_manifest,
                PackageRole::Root,
            )
            .expect("an excluded requirement has no active graph edge");
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InactiveDependency)
        );
        assert!(scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::UnknownPackage
                && diagnostic.code != sai_diagnostics::Code::FeatureNotImplemented
        }));
    }

    #[test]
    fn package_scopes_reject_stale_edges_for_target_excluded_requirements() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"use acme:dep/module as dep\n", b"fn child() {}\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext {
            is_root_package: true,
            ..Phase1BuildContext::default()
        };
        let inactive_manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires { acme:dep @ ^1 only { wasm64-core } }\n"[..],
        );
        database
            .set_package_graph(Some(phase1_graph("1.0.0", false)))
            .expect("graph with a stale target-excluded edge is recorded");

        let result = database.collect_phase1_package_scopes_with_manifest(
            &module_tree,
            &snapshot,
            &target,
            context,
            inactive_manifest,
            PackageRole::Root,
        );
        assert!(
            matches!(
                result,
                Err(Phase1CollectionError::PackageGraphRequirementEdgeMismatch {
                    ref identity,
                    expected_active: false,
                    found_active: true,
                    ..
                }) if identity == "acme:dep"
            ),
            "a stale edge must be rejected before import diagnostics are collected"
        );
    }

    #[test]
    fn package_scopes_validate_root_test_edges_against_test_analysis() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"use acme:dep/module as dep\n", b"fn child() {}\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let test_manifest = Arc::<[u8]>::from(
            &b"package acme:phase1 @ 1.0.0\n\
              requires test { acme:dep @ ^1 }\n"[..],
        );
        let without_tests = Phase1BuildContext {
            is_root_package: true,
            ..Phase1BuildContext::default()
        };

        database
            .set_package_graph(Some(phase1_root_only_graph()))
            .expect("test edge is absent from a non-test active graph");
        let ordinary_build = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                without_tests,
                Arc::clone(&test_manifest),
                PackageRole::Root,
            )
            .expect("inactive test requirements need no active edge");
        assert!(ordinary_build.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != sai_diagnostics::Code::UnknownPackage
                && diagnostic.code != sai_diagnostics::Code::FeatureNotImplemented
        }));

        database
            .set_package_graph(Some(phase1_graph("1.0.0", false)))
            .expect("test graph activates the root test edge");
        let with_tests = Phase1BuildContext {
            is_root_package: true,
            analyzes_tests: true,
            ..Phase1BuildContext::default()
        };
        let test_build = database
            .collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                with_tests,
                Arc::clone(&test_manifest),
                PackageRole::Root,
            )
            .expect("the active test edge agrees with the manifest");
        assert!(
            test_build
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented)
        );

        database
            .set_package_graph(Some(phase1_root_only_graph()))
            .expect("test graph without its active test edge is recorded");
        assert!(matches!(
            database.collect_phase1_package_scopes_with_manifest(
                &module_tree,
                &snapshot,
                &target,
                with_tests,
                test_manifest,
                PackageRole::Root,
            ),
            Err(Phase1CollectionError::PackageGraphRequirementEdgeMismatch {
                expected_active: true,
                found_active: false,
                ..
            })
        ));
    }

    #[test]
    fn implicit_toolchain_scope_projection_tracks_toolchain_version() {
        let first_graph = phase1_root_only_graph_with_toolchain("1.8.2");
        let next_graph = phase1_root_only_graph_with_toolchain("1.8.3");
        for package in ["sai:core", "sai:alloc"] {
            let package = PackageKey::new(package).expect("implicit package key is valid");
            let unavailable = phase1_package_graph_projection(None, &package)
                .expect("unavailable graph projection succeeds");
            let first = phase1_package_graph_projection(Some(&first_graph), &package)
                .expect("first toolchain projection succeeds");
            let next = phase1_package_graph_projection(Some(&next_graph), &package)
                .expect("next toolchain projection succeeds");

            assert_eq!(unavailable, None);
            assert_eq!(
                first,
                Some(Phase1PackageGraphProjection::ImplicitToolchain {
                    version: Version::parse("1.8.2").expect("toolchain version is valid"),
                })
            );
            assert_ne!(
                first,
                next,
                "implicit {} scopes must track the exact toolchain version",
                package.as_str()
            );
        }
    }

    #[test]
    fn package_scopes_rejects_a_package_missing_from_an_available_graph() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(b"fn stable() {}\n", b"");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let graph = PackageGraph::new(
            Version::parse("1.8.2").expect("toolchain version is valid"),
            [graph_node(
                "other:root#1",
                "other:root",
                "1.0.0",
                PackageGraphRole::Root,
                Vec::new(),
            )],
        )
        .expect("other root graph is valid");
        database
            .set_package_graph(Some(graph))
            .expect("available graph is recorded");

        let error = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect_err("a missing selected package must not be treated as an empty graph node");
        assert_eq!(
            error,
            Phase1CollectionError::PackageGraphMissingNode(
                PackageKey::new("acme:phase1#1").expect("package key is valid")
            )
        );
    }

    #[test]
    fn package_scopes_query_invalidates_when_target_configuration_changes() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"@target(\"wasm64-core\")\nfn only_wasm64() {}\n", b"");
        let wasm32 = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let wasm32_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &wasm32,
                Phase1BuildContext::default(),
            )
            .expect("32-bit target scopes collect");
        assert!(!phase1_has_function(
            &database,
            &wasm32_scopes,
            ModuleId::ROOT,
            "only_wasm64"
        ));

        let wasm64 = target_cfg_for_check("wasm64-core").expect("target config is valid");
        let wasm64_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &wasm64,
                Phase1BuildContext::default(),
            )
            .expect("64-bit target scopes collect");
        assert!(phase1_has_function(
            &database,
            &wasm64_scopes,
            ModuleId::ROOT,
            "only_wasm64"
        ));
    }

    #[test]
    fn package_scopes_query_rebinds_export_diagnostics_and_invalidates_versions() {
        let root = concat!(
            "@export_name(\"duplicate\")\n",
            "export fn first() {}\n",
            "@export_name(\"duplicate\")\n",
            "export fn second() {}\n",
        );
        let (mut database, module_tree, first_snapshot, _) = phase1_fixture(root.as_bytes(), b"");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let context = Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        };
        let first_scopes = database
            .collect_phase1_package_scopes(&module_tree, &first_snapshot, &target, context)
            .expect("initial scopes collect");
        assert!(first_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::DuplicateExportName
                && diagnostic
                    .primary()
                    .is_some_and(|primary| primary.file.snapshot == Some(17))
        }));

        let same_version_snapshot = snapshot_with_version(&first_snapshot, 17);
        let rebound_scopes = database
            .collect_phase1_package_scopes(&module_tree, &same_version_snapshot, &target, context)
            .expect("same-version snapshot scopes collect");
        let rebound = rebound_scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::DuplicateExportName)
            .expect("duplicate export diagnostic is present");
        assert!(
            rebound
                .clone()
                .with_workspace_snapshot(&same_version_snapshot)
                .is_ok(),
            "cached export diagnostics bind to the current workspace snapshot"
        );

        let newer_snapshot = snapshot_with_version(&first_snapshot, 18);
        let newer_scopes = database
            .collect_phase1_package_scopes(&module_tree, &newer_snapshot, &target, context)
            .expect("new document version scopes collect");
        assert!(newer_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::DuplicateExportName
                && diagnostic
                    .primary()
                    .is_some_and(|primary| primary.file.snapshot == Some(18))
        }));
    }

    #[test]
    fn package_scopes_query_reuses_memo_and_refreshes_attribute_diagnostics_for_new_snapshots() {
        let source = concat!(
            "@derive(Clone)\n",
            "@export_name(1)\n",
            "export fn staged() {}\n",
        );
        let (mut database, module_tree, snapshot, _) = phase1_fixture(
            source.as_bytes(),
            b"@export_name(\"valid\")\nexport fn valid() {}\n",
        );
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let first_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("initial scopes collect");
        let first_diagnostic = first_scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidAttributeArgument)
            .expect("the invalid export-name argument is diagnosed");
        assert!(
            first_diagnostic
                .clone()
                .with_workspace_snapshot(&snapshot)
                .is_ok(),
            "the first attribute diagnostic is bound to the first snapshot"
        );
        assert!(first_diagnostic.has_workspace_snapshot());
        assert!(first_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented
                && diagnostic
                    .primary()
                    .is_some_and(|primary| primary.file.snapshot == Some(17))
        }));
        assert!(!first_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented
                && matches!(
                    diagnostic.data.get("feature"),
                    Some(sai_diagnostics::Value::String(feature))
                        if feature == "attribute @export_name"
                )
        }));
        let package_key = PackageKey::new("acme:phase1#1").expect("package key is valid");
        let first_memo = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key.clone())
            .expect("initial package scope memo exists");

        let updated_snapshot = snapshot_with_unrelated_file(&snapshot);
        let updated_scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &updated_snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("updated workspace scopes collect");
        let updated_diagnostic = updated_scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidAttributeArgument)
            .expect("the invalid export-name argument remains diagnosed");
        assert!(
            updated_diagnostic
                .clone()
                .with_workspace_snapshot(&updated_snapshot)
                .is_ok(),
            "attribute diagnostics must bind to the current workspace snapshot"
        );
        assert!(updated_diagnostic.has_workspace_snapshot());
        let rebound_target_filter_diagnostic = updated_scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented)
            .expect("the staged derive attribute remains diagnosed");
        assert!(
            rebound_target_filter_diagnostic
                .clone()
                .with_workspace_snapshot(&updated_snapshot)
                .is_ok(),
            "target-filter diagnostics must be freshly bound to the current snapshot"
        );
        assert!(!updated_scopes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::FeatureNotImplemented
                && matches!(
                    diagnostic.data.get("feature"),
                    Some(sai_diagnostics::Value::String(feature))
                        if feature == "attribute @export_name"
                )
        }));
        let updated_memo = database
            .package_scopes
            .query
            .get(&mut database.query_database, package_key)
            .expect("updated package scope memo exists");
        assert!(
            Arc::ptr_eq(&first_memo, &updated_memo),
            "unrelated workspace files must not invalidate the package-scope memo"
        );
    }

    #[test]
    fn phase1_driver_bridge_binds_local_interface_aliases() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"interface codec {}\n", b"use acme:phase1/codec as api\n");
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 scopes collect for local interface alias");
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let interface_def = root_scope
            .declared
            .iter()
            .find_map(|(symbol, binding)| {
                let name = database.resolve_symbol(*symbol).ok()?;
                if name.as_ref() != "codec" {
                    return None;
                }
                match binding.res {
                    sai_resolve::package_scopes::Res::Def(
                        sai_resolve::package_scopes::DefKind::Interface,
                        def_id,
                    ) => Some(def_id),
                    _ => None,
                }
            })
            .expect("the root interface has a canonical definition");
        let child_scope = &scopes.modules[1];
        let alias_def = child_scope
            .scope
            .iter()
            .find_map(|(symbol, binding)| {
                let name = database.resolve_symbol(*symbol).ok()?;
                if name.as_ref() != "api" {
                    return None;
                }
                match binding.res {
                    sai_resolve::package_scopes::Res::Def(
                        sai_resolve::package_scopes::DefKind::Interface,
                        def_id,
                    ) => Some(def_id),
                    _ => None,
                }
            })
            .expect("the interface alias resolves in the importing module");
        assert_eq!(alias_def, interface_def);
        assert_eq!(child_scope.imports.len(), 1);
        assert!(
            !scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::UnknownModule)
        );
    }

    #[test]
    fn phase1_driver_bridge_resolves_local_interface_members_by_canonical_id() {
        let (mut database, module_tree, snapshot, _) = phase1_fixture(
            b"pub interface codec { type Request {} fn send() }\n",
            b"use acme:phase1/codec.{Request, send}\n",
        );
        let target = target_cfg_for_check("wasm32-core").expect("target config is valid");
        let scopes = database
            .collect_phase1_package_scopes(
                &module_tree,
                &snapshot,
                &target,
                Phase1BuildContext::default(),
            )
            .expect("Phase 1 scopes collect local interface members");
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let child_scope = &scopes.modules[ModuleId::from_raw(1).raw() as usize];
        let interface_def = root_scope
            .declared
            .iter()
            .find_map(|(symbol, binding)| {
                let name = database.resolve_symbol(*symbol).ok()?;
                if name.as_ref() != "codec" {
                    return None;
                }
                match binding.res {
                    sai_resolve::package_scopes::Res::Def(
                        sai_resolve::package_scopes::DefKind::Interface,
                        def_id,
                    ) => Some(def_id),
                    _ => None,
                }
            })
            .expect("the root interface has a canonical definition");
        let interface_scope = scopes
            .interfaces
            .get(&interface_def)
            .expect("Phase 1 collects the local interface member table");
        for (name, kind) in [
            ("Request", sai_resolve::package_scopes::DefKind::Struct),
            ("send", sai_resolve::package_scopes::DefKind::Fn),
        ] {
            let (symbol, member) = interface_scope
                .members
                .iter()
                .find_map(|(symbol, binding)| {
                    let spelling = database.resolve_symbol(*symbol).ok()?;
                    (spelling.as_ref() == name).then_some((*symbol, binding))
                })
                .expect("interface member is present by name");
            assert_eq!(
                child_scope.scope.get(&symbol).map(|binding| binding.res),
                Some(member.res),
                "the child import keeps the canonical interface member resolution"
            );
            assert_eq!(member.visibility, sai_hir::Visibility::Pub);
            let sai_resolve::package_scopes::Res::Def(actual_kind, def_id) = member.res else {
                panic!("interface member resolves to a definition");
            };
            assert_eq!(actual_kind, kind);
            assert!(
                database
                    .resolve_def_path(def_id)
                    .expect("member DefPath resolves")
                    .is_some()
            );
            assert!(!root_scope.members.contains_key(&symbol));
        }
        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != sai_diagnostics::Code::UnresolvedImport)
        );
    }

    #[test]
    fn phase1_driver_bridge_rejects_a_snapshot_with_mismatched_module_bytes() {
        let (mut database, module_tree, snapshot, _) =
            phase1_fixture(b"fn original() {}\n", b"fn child() {}\n");
        let stale_snapshot = WorkspaceSnapshot::new(
            module_tree
                .modules
                .iter()
                .map(|module| {
                    let original = snapshot
                        .file(module.file)
                        .expect("fixture snapshot contains each module");
                    let source = if module.file == module_tree.modules[0].file {
                        Arc::new(
                            SourceFile::decode(module.file, b"fn stale() {}\n")
                                .expect("stale source decodes"),
                        )
                    } else {
                        Arc::clone(original.source())
                    };
                    SnapshotFile::new(
                        source,
                        original.path().to_owned(),
                        original.package().map(str::to_owned),
                        original.document_version(),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .expect("stale workspace snapshot is structurally valid");
        let target_cfg = target_cfg_for_check("wasm32-core").expect("target config is valid");

        assert_eq!(
            database.collect_phase1_package_scopes(
                &module_tree,
                &stale_snapshot,
                &target_cfg,
                Phase1BuildContext::default(),
            ),
            Err(Phase1CollectionError::Parse(
                ParseQueryError::SnapshotSourceMismatch
            ))
        );
    }

    #[test]
    fn mixed_source_divergent_diagnostics_bind_audits_to_the_workspace() {
        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        std::fs::create_dir_all(root_path.join("src")).unwrap();
        let root_manifest = b"package acme:app @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("root package resolves without dependencies");
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the final source audit plan is complete");
        assert!(audit_plan.is_equivalent());
        let other_mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the same package can be loaded in an independent resolver run");
        let other_audit_plan = other_mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the second source audit plan is complete");

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:app#1", "sai.package").expect("file key is valid"))
            .expect("root manifest key is interned");
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
            "sai.package",
            Some("acme:app@1.0.0".to_owned()),
            Some(1),
        )])
        .expect("root manifest snapshot is valid");
        let diagnostics = database
            .mixed_source_divergent_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::new(),
            )
            .expect("the source audit binds to the same workspace snapshot");
        assert!(diagnostics.is_empty());
        assert_eq!(
            database.mixed_source_divergent_diagnostics(
                &snapshot,
                &mixed_output,
                &other_audit_plan,
                root_file,
                &BTreeMap::new(),
            ),
            Err(RootDatabaseError::MixedSourceAuditLoadMismatch)
        );
        assert_eq!(
            mixed_source_reload_failure_diagnostics(
                &mixed_output,
                &other_audit_plan,
                &MvsManifestLocations::default(),
            ),
            Err(MvsDuplicatePackageLineDiagnosticError::MixedSourceAuditLoadMismatch)
        );

        let mut stale_manifest = root_manifest.to_vec();
        stale_manifest.extend_from_slice(b"requires { acme:unseen @ ^1 }\n");
        let stale_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(root_file, &stale_manifest).unwrap()),
            "sai.package",
            Some("acme:app@1.0.0".to_owned()),
            Some(2),
        )])
        .expect("stale but syntactically valid manifest snapshot is valid");
        assert_eq!(
            database.mixed_source_divergent_diagnostics(
                &stale_snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::new(),
            ),
            Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                mixed_output.provider().output().selection().root().clone(),
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_divergent_diagnostics_report_a_matching_changed_local_source() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let first_path = directory.0.join("target-first");
        let final_path = directory.0.join("target-final");
        for path in [&root_path, &first_path, &final_path] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }
        let root_manifest = b"package acme:root @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n\
                             requires { acme:target @ ^1 from path \"switch\" }\n";
        let first_manifest = b"package acme:target @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n";
        let final_manifest = b"package acme:target @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n\
                              requires { acme:leaf @ ^1 from path \"leaf\" }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(first_path.join("sai.package"), first_manifest).unwrap();
        std::fs::write(first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(final_path.join("sai.package"), final_manifest).unwrap();
        std::fs::write(final_path.join("src/main.sai"), b"").unwrap();
        std::fs::create_dir_all(final_path.join("leaf/src")).unwrap();
        std::fs::write(
            final_path.join("leaf/sai.package"),
            b"package acme:leaf @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n",
        )
        .unwrap();
        std::fs::write(final_path.join("leaf/src/main.sai"), b"").unwrap();
        symlink("../target-first", root_path.join("switch")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the initial local source resolves");

        std::fs::remove_file(root_path.join("switch")).unwrap();
        symlink("../target-final", root_path.join("switch")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the changed local source is reloaded and audited");
        assert!(audit_plan.audit().is_complete());
        assert!(audit_plan.audit().has_divergent_sources());

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, first_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the read-time manifests belong to one workspace snapshot");
        let target_version = mixed_output
            .provider()
            .output()
            .selection()
            .read_contracts()
            .keys()
            .next()
            .expect("one dependency was loaded")
            .clone();
        let dependency_manifest_files = BTreeMap::from([(target_version.clone(), target_file)]);
        let diagnostics = database
            .mixed_source_divergent_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &dependency_manifest_files,
            )
            .expect("the matching audit is bound to the snapshot contracts");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "DIVERGENT_SOURCES");

        let mut stale_target_manifest = first_manifest.to_vec();
        stale_target_manifest.extend_from_slice(b"requires { acme:other @ ^1 }\n");
        let stale_snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(2),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, &stale_target_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(2),
            ),
        ])
        .expect("stale dependency manifest remains a valid snapshot");
        assert_eq!(
            database.mixed_source_divergent_diagnostics(
                &stale_snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &dependency_manifest_files,
            ),
            Err(RootDatabaseError::MixedSourceManifestContractMismatch(
                target_version,
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_reload_diagnostics_bound_final_path_comparison_failures() {
        use std::os::unix::fs::symlink;

        const PATH_REQUIREMENT_COUNT: usize = 35;
        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let first_path = directory.0.join("target-first");
        let final_path = directory.0.join("target-final");
        for path in [&root_path, &first_path, &final_path] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }

        let mut root_manifest = String::from(
            "package acme:root @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             overrides {\n",
        );
        for index in 0..PATH_REQUIREMENT_COUNT {
            root_manifest.push_str(&format!(
                "  acme:leaf-{index} use path \"root-leaf-{index}\"\n"
            ));
        }
        root_manifest.push_str("}\nrequires { acme:target @ ^1 from path \"switch\" }\n");
        let root_manifest = root_manifest.into_bytes();
        let leaf_manifests = (0..PATH_REQUIREMENT_COUNT)
            .map(|index| {
                let package = format!("acme:leaf-{index}");
                let manifest = format!(
                    "package {package} @ 1.0.0\n\
                     source \"src\"\n\
                     targets {{ wasm32-core }}\n"
                );
                (package, manifest.into_bytes())
            })
            .collect::<Vec<_>>();
        let mut target_manifest = String::from(
            "package acme:target @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             requires {\n",
        );
        for (package, _) in &leaf_manifests {
            target_manifest.push_str(&format!("  {package} @ ^1 from path \"ignored/leaf\"\n"));
        }
        target_manifest.push_str("}\n");
        let read_time_target_manifest = target_manifest.clone().into_bytes();
        let final_target_manifest =
            format!("// final source snapshot\n{target_manifest}").into_bytes();
        std::fs::write(root_path.join("sai.package"), &root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(first_path.join("sai.package"), &read_time_target_manifest).unwrap();
        std::fs::write(first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(final_path.join("sai.package"), &final_target_manifest).unwrap();
        std::fs::write(final_path.join("src/main.sai"), b"").unwrap();
        for (index, (_, leaf_manifest)) in leaf_manifests.iter().enumerate() {
            let leaf_path = root_path.join(format!("root-leaf-{index}"));
            std::fs::create_dir_all(leaf_path.join("src")).unwrap();
            std::fs::write(leaf_path.join("sai.package"), leaf_manifest).unwrap();
            std::fs::write(leaf_path.join("src/main.sai"), b"").unwrap();
        }
        symlink("../target-first", root_path.join("switch")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the read-time path graph resolves");
        std::fs::remove_file(root_path.join("switch")).unwrap();
        symlink("../target-final", root_path.join("switch")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the changed final source and its path package are loaded");

        let selection = mixed_output.provider().output().selection();
        let target_version = selection
            .read_contracts()
            .keys()
            .find(|package_version| package_version.package() == "acme:target")
            .expect("the target dependency was loaded")
            .clone();
        let leaf_versions = selection
            .read_contracts()
            .keys()
            .filter(|package_version| package_version.package().starts_with("acme:leaf-"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(leaf_versions.len(), PATH_REQUIREMENT_COUNT);
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_target_manifest.as_slice(),
            "successful reloads retain their exact final manifest for path diagnostics"
        );
        assert_ne!(
            read_time_target_manifest, final_target_manifest,
            "the final manifest must differ from the read-time bytes"
        );

        // Remove root override targets only after final manifests and contracts
        // have been captured. Both Expected and Actual lookups fail, but only
        // the Actual failure belongs to the final contract comparison.
        for index in 0..PATH_REQUIREMENT_COUNT {
            std::fs::remove_dir_all(root_path.join(format!("root-leaf-{index}"))).unwrap();
        }

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let mut dependency_manifest_files = BTreeMap::from([(target_version.clone(), target_file)]);
        let mut snapshot_files = vec![
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, &root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, &read_time_target_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
        ];
        for (package, leaf_manifest) in &leaf_manifests {
            let package_version = leaf_versions
                .iter()
                .find(|package_version| package_version.package() == package)
                .expect("every path requirement selected its matching package");
            let file_id = database
                .intern_file_key(FileKey::new(format!("{package}#1"), "sai.package").unwrap())
                .unwrap();
            dependency_manifest_files.insert(package_version.clone(), file_id);
            snapshot_files.push(SnapshotFile::new(
                Arc::new(SourceFile::decode(file_id, leaf_manifest).unwrap()),
                format!("dep:{package}@1.0.0/sai.package"),
                Some(format!("{package}@1.0.0")),
                Some(1),
            ));
        }
        let snapshot = WorkspaceSnapshot::new(snapshot_files)
            .expect("the read-time manifests belong to one workspace snapshot");
        let diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &dependency_manifest_files,
            )
            .expect("the path comparison failure binds to the retained final source");
        assert_eq!(diagnostics.len(), 32);
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() == "PATH_PACKAGE_NOT_FOUND")
        );
        let diagnostic = diagnostics
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .data
                    .contains_key("additional_path_resolution_errors_omitted")
            })
            .expect("the final emitted diagnostic marks omitted path failures");
        assert_eq!(diagnostic.package.as_deref(), Some("acme:target"));
        assert_eq!(
            diagnostic.data.get("package"),
            Some(&sai_diagnostics::Value::String("acme:target".to_owned()))
        );
        assert_eq!(
            diagnostic
                .data
                .get("additional_path_resolution_errors_omitted"),
            Some(&sai_diagnostics::Value::Bool(true))
        );
        let path_package = match diagnostic.data.get("path_package") {
            Some(sai_diagnostics::Value::String(package)) => package,
            other => panic!("expected the failed path package name, got {other:?}"),
        };
        assert_eq!(
            diagnostic.message,
            format!(
                "could not resolve final path dependency {} while comparing acme:target@1.0.0: a local package required by the final source was not found",
                path_package
            )
        );
        assert_eq!(
            diagnostic.data.get("side"),
            Some(&sai_diagnostics::Value::String("final_source".to_owned()))
        );
        assert!(
            !diagnostic.data.contains_key("path"),
            "the unresolved relative path is not copied into diagnostic data"
        );
        let primary = diagnostic
            .primary()
            .expect("exact final requirement location");
        assert_eq!(primary.file.path, "sai.package");
        assert_eq!(primary.file.package.as_deref(), Some("acme:root@1.0.0"));
        let start = primary.span.range.start().to_usize();
        let end = primary.span.range.end().to_usize();
        assert_eq!(
            &diagnostics
                .snapshot()
                .file(primary.span.file)
                .expect("the workspace snapshot retains the root manifest")
                .source()
                .raw_bytes()[start..end],
            &root_manifest[start..end]
        );
        assert!(
            root_manifest[start..end].starts_with(b"acme:leaf-")
                && root_manifest[start..end]
                    .windows(b"use path \"root-leaf-".len())
                    .any(|window| { window == b"use path \"root-leaf-" }),
            "the primary span selects one exact root override"
        );
        assert!(
            matches!(
                diagnostic.data.get("requirement_chain"),
                Some(sai_diagnostics::Value::Object(fields))
                    if matches!(
                        fields.get("steps"),
                        Some(sai_diagnostics::Value::Array(steps)) if !steps.is_empty()
                    )
            ),
            "the original MVS chain remains attached"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_identity_diagnostics_report_final_local_path_failures() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let target_path = directory.0.join("target-first");
        for path in [&root_path, &target_path] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }
        let root_manifest = b"package acme:root @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n\
                             requires { acme:target @ ^1 from path \"switch\" }\n";
        let target_manifest = b"package acme:target @ 1.0.0\n\
                               source \"src\"\n\
                               targets { wasm32-core }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(target_path.join("sai.package"), target_manifest).unwrap();
        std::fs::write(target_path.join("src/main.sai"), b"").unwrap();
        symlink("../target-first", root_path.join("switch")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the read-time local source resolves");

        std::fs::remove_file(root_path.join("switch")).unwrap();
        symlink("../target-missing", root_path.join("switch")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the final-source failure is retained on the audit plan");
        assert_eq!(audit_plan.source_identities().failures().len(), 1);

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, target_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the read-time manifests belong to one workspace snapshot");
        let target_version = mixed_output
            .provider()
            .output()
            .selection()
            .read_contracts()
            .keys()
            .next()
            .expect("one dependency was loaded")
            .clone();
        let diagnostics = database
            .mixed_source_identity_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::from([(target_version.clone(), target_file)]),
            )
            .expect("the failure is bound to the original manifest snapshot");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "PATH_PACKAGE_NOT_FOUND");
        assert!(diagnostics[0].primary().is_some());
        assert!(
            diagnostics[0]
                .data
                .get("requirement_chain")
                .is_some_and(|chain| matches!(
                    chain,
                    sai_diagnostics::Value::Object(fields)
                        if matches!(
                            fields.get("steps"),
                            Some(sai_diagnostics::Value::Array(steps)) if !steps.is_empty()
                        )
                ))
        );
        let reload_diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::from([(target_version, target_file)]),
            )
            .expect("the reload audit remains bound to the original manifest snapshot");
        assert!(
            reload_diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code.as_str() != "INTERNAL_COMPILER_ERROR"),
            "the missing path identity is explained by the reported identity failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_reload_diagnostics_report_final_package_identity_mismatches() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let first_path = directory.0.join("target-first");
        let final_path = directory.0.join("target-final");
        for path in [&root_path, &first_path, &final_path] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }
        let root_manifest = b"package acme:root @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n\
                             requires { acme:target @ ^1 from path \"switch\" }\n";
        let first_manifest = b"package acme:target @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n";
        let final_manifest = b"package acme:target @ 2.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(first_path.join("sai.package"), first_manifest).unwrap();
        std::fs::write(first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(final_path.join("sai.package"), final_manifest).unwrap();
        std::fs::write(final_path.join("src/main.sai"), b"").unwrap();
        symlink("../target-first", root_path.join("switch")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the read-time local source resolves");

        std::fs::remove_file(root_path.join("switch")).unwrap();
        symlink("../target-final", root_path.join("switch")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the final-source reload failures are retained");
        assert!(
            audit_plan.reload_failures().iter().any(|failure| matches!(
                &failure.error,
                sai_package::mixed_source::MixedSourceFinalReloadError::LocalDirectoryIndex(error)
                    if matches!(
                        error.as_ref(),
                        sai_package::local_source::LocalPackageDirectoryIndexError::PackageVersionMismatch { .. }
                    )
            ))
        );

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, first_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the read-time manifests belong to one workspace snapshot");
        let target_version = mixed_output
            .provider()
            .output()
            .selection()
            .read_contracts()
            .keys()
            .next()
            .expect("one dependency was loaded")
            .clone();
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_manifest
        );
        let diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::from([(target_version, target_file)]),
            )
            .expect("the reload failure is bound to the original workspace");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "PACKAGE_IDENTITY_MISMATCH");
        assert_eq!(
            diagnostics[0].message,
            "could not reload selected package acme:target@1.0.0: expected acme:target@1.0.0, found acme:target@2.0.0"
        );
        assert_eq!(
            diagnostics[0]
                .primary()
                .expect("final manifest header location")
                .file
                .path,
            "dep:acme:target@1.0.0/.sai/final-source-0/sai.package"
        );
        let primary = diagnostics[0].primary().unwrap();
        assert_eq!(
            primary.span.file,
            diagnostics
                .snapshot()
                .file(primary.span.file)
                .unwrap()
                .source()
                .id()
        );
        let start = primary.span.range.start().to_usize();
        let end = primary.span.range.end().to_usize();
        assert_eq!(
            &diagnostics
                .snapshot()
                .file(primary.span.file)
                .unwrap()
                .source()
                .raw_bytes()[start..end],
            b"package acme:target @ 2.0.0"
        );
        assert_eq!(
            diagnostics
                .snapshot()
                .file(primary.span.file)
                .unwrap()
                .source()
                .raw_bytes(),
            final_manifest
        );
        assert!(matches!(
            diagnostics[0].data.get("requirement_chain"),
            Some(sai_diagnostics::Value::Object(fields))
                if matches!(
                    fields.get("steps"),
                    Some(sai_diagnostics::Value::Array(steps)) if !steps.is_empty()
                )
        ));
        assert!(
            !diagnostics[0]
                .message
                .contains(directory.0.to_str().unwrap()),
            "diagnostic messages do not expose local absolute paths"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_reload_diagnostics_bind_final_manifest_syntax_errors() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let target_first_path = directory.0.join("target-first");
        let target_final_path = directory.0.join("target-final");
        let other_first_path = directory.0.join("other-first");
        let other_final_path = directory.0.join("other-final");
        for path in [
            &root_path,
            &target_first_path,
            &target_final_path,
            &other_first_path,
            &other_final_path,
        ] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }
        let root_manifest = b"package acme:root @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n\
                             requires {\n\
                                 acme:target @ ^1 from path \"switch-target\"\n\
                                 acme:other @ ^1 from path \"switch-other\"\n\
                             }\n";
        let target_first_manifest = b"package acme:target @ 1.0.0\n\
                                      source \"src\"\n\
                                      targets { wasm32-core }\n";
        let target_final_manifest = b"source \"src\" targets { wasm32-core }\n";
        let other_first_manifest = b"package acme:other @ 1.0.0\n\
                                     source \"src\"\n\
                                     targets { wasm32-core }\n";
        let other_final_manifest = b"package acme:other @ 1.0.0\n\
                                     source \"src\"\n\
                                     targets { wasm32-core }\n\
                                     requires { acme:missing @ ^1 from path \"missing\" }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(target_first_path.join("sai.package"), target_first_manifest).unwrap();
        std::fs::write(target_first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(target_final_path.join("sai.package"), target_final_manifest).unwrap();
        std::fs::write(target_final_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(other_first_path.join("sai.package"), other_first_manifest).unwrap();
        std::fs::write(other_first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(other_final_path.join("sai.package"), other_final_manifest).unwrap();
        std::fs::write(other_final_path.join("src/main.sai"), b"").unwrap();
        symlink("../target-first", root_path.join("switch-target")).unwrap();
        symlink("../other-first", root_path.join("switch-other")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the read-time local source resolves");
        std::fs::remove_file(root_path.join("switch-target")).unwrap();
        symlink("../target-final", root_path.join("switch-target")).unwrap();
        std::fs::remove_file(root_path.join("switch-other")).unwrap();
        symlink("../other-final", root_path.join("switch-other")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the invalid final manifest is retained for diagnostics");

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let other_file = database
            .intern_file_key(FileKey::new("acme:other#1", "sai.package").unwrap())
            .unwrap();
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, target_first_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(other_file, other_first_manifest).unwrap()),
                "dep:acme:other@1.0.0/sai.package",
                Some("acme:other@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the read-time manifests belong to one workspace snapshot");
        let read_contracts = mixed_output
            .provider()
            .output()
            .selection()
            .read_contracts();
        let target_version = read_contracts
            .keys()
            .find(|package_version| package_version.package() == "acme:target")
            .expect("the target dependency was loaded")
            .clone();
        let other_version = read_contracts
            .keys()
            .find(|package_version| package_version.package() == "acme:other")
            .expect("the other dependency was loaded")
            .clone();
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            target_final_manifest
        );
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&other_version].as_ref(),
            other_final_manifest
        );
        let diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::from([
                    (target_version.clone(), target_file),
                    (other_version.clone(), other_file),
                ]),
            )
            .expect("the final syntax diagnostic is bound to the source snapshot");
        assert_eq!(diagnostics.len(), 3);
        let expected_token_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "EXPECTED_TOKEN")
            .expect("the missing declaration separator is reported");
        let syntax_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "MISSING_PACKAGE_DECL")
            .expect("the target's final syntax error is reported");
        let path_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "PATH_PACKAGE_NOT_FOUND")
            .expect("the other package's independent reload failure is reported");
        assert!(matches!(
            expected_token_diagnostic.data.get("expected_tokens"),
            Some(sai_diagnostics::Value::Array(tokens))
                if tokens == &[sai_diagnostics::Value::String("NEWLINE".to_owned())]
        ));
        let expected_primary = expected_token_diagnostic
            .primary()
            .expect("the missing separator points into the final manifest");
        let expected_final_file = diagnostics
            .snapshot()
            .file(expected_primary.span.file)
            .expect("the augmented snapshot includes the malformed final manifest");
        let expected_span = expected_primary.span.range.start().to_usize()
            ..expected_primary.span.range.end().to_usize();
        assert_eq!(
            &expected_final_file.source().raw_bytes()[expected_span],
            b"targets"
        );
        assert_eq!(
            syntax_diagnostic.message,
            "could not reload selected package acme:target@1.0.0: missing `package` declaration"
        );
        let primary = syntax_diagnostic
            .primary()
            .expect("the missing declaration points into the final manifest");
        assert!(
            primary
                .file
                .path
                .starts_with("dep:acme:target@1.0.0/.sai/final-source-")
        );
        assert_eq!(
            primary.span.range.start().to_usize(),
            target_final_manifest.len()
        );
        assert_eq!(
            primary.span.range.end().to_usize(),
            target_final_manifest.len()
        );
        assert!(matches!(
            syntax_diagnostic.data.get("requirement_chain"),
            Some(sai_diagnostics::Value::Object(fields))
                if matches!(
                    fields.get("steps"),
                    Some(sai_diagnostics::Value::Array(steps)) if !steps.is_empty()
                )
        ));
        let bound_final_file = diagnostics
            .snapshot()
            .file(primary.span.file)
            .expect("the augmented snapshot includes the invalid final manifest");
        assert_eq!(bound_final_file.source().raw_bytes(), target_final_manifest);
        let path_primary = path_diagnostic
            .primary()
            .expect("the path failure points into the other final manifest");
        assert!(
            path_primary
                .file
                .path
                .starts_with("dep:acme:other@1.0.0/.sai/final-source-")
        );
        let path_span =
            path_primary.span.range.start().to_usize()..path_primary.span.range.end().to_usize();
        assert_eq!(
            &diagnostics
                .snapshot()
                .file(path_primary.span.file)
                .expect("the augmented snapshot includes the other final manifest")
                .source()
                .raw_bytes()[path_span],
            b"acme:missing @ ^1 from path \"missing\""
        );

        let oversized_final_manifest = vec![b';'; 128];
        std::fs::write(
            target_final_path.join("sai.package"),
            &oversized_final_manifest,
        )
        .unwrap();
        let truncated_audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the oversized final syntax is retained for diagnostics");
        let truncated_diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &truncated_audit_plan,
                root_file,
                &BTreeMap::from([(target_version, target_file), (other_version, other_file)]),
            )
            .expect("bounded syntax diagnostics are produced");
        let target_syntax_diagnostics = truncated_diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic
                    .primary()
                    .is_some_and(|primary| primary.file.path.starts_with("dep:acme:target@"))
            })
            .collect::<Vec<_>>();
        assert_eq!(target_syntax_diagnostics.len(), 32);
        let omission_diagnostic = target_syntax_diagnostics
            .iter()
            .find(|diagnostic| {
                matches!(
                    diagnostic.data.get("additional_syntax_errors_omitted"),
                    Some(sai_diagnostics::Value::Bool(true))
                )
            })
            .expect("the final syntax diagnostic marks omitted errors");
        assert!(omission_diagnostic.children().iter().any(|child| {
            child
                .message
                .contains("additional final manifest syntax errors were omitted")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn mixed_source_reload_diagnostics_report_final_path_and_semantic_failures() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let root_path = directory.0.join("root");
        let first_path = directory.0.join("target-first");
        let final_path = directory.0.join("target-final");
        for path in [&root_path, &first_path, &final_path] {
            std::fs::create_dir_all(path.join("src")).unwrap();
        }
        let root_manifest = b"package acme:root @ 1.0.0\n\
                             source \"src\"\n\
                             targets { wasm32-core }\n\
                             requires { acme:target @ ^1 from path \"switch\" }\n";
        let first_manifest = b"package acme:target @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n";
        let final_manifest = b"package acme:target @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core, wasm32-invalid }\n\
                              requires { acme:secret @ ^1 from path \"missing/private-workspace\" }\n";
        std::fs::write(root_path.join("sai.package"), root_manifest).unwrap();
        std::fs::write(root_path.join("src/main.sai"), b"fn main() {}\n").unwrap();
        std::fs::write(first_path.join("sai.package"), first_manifest).unwrap();
        std::fs::write(first_path.join("src/main.sai"), b"").unwrap();
        std::fs::write(final_path.join("sai.package"), final_manifest).unwrap();
        std::fs::write(final_path.join("src/main.sai"), b"").unwrap();
        symlink("../target-first", root_path.join("switch")).unwrap();

        let mixed_output = resolve_mixed_source_packages(
            LocalPackageDirectory::open(&root_path).unwrap(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            PackageValidationContext::default(),
        )
        .expect("the read-time local source resolves");

        std::fs::remove_file(root_path.join("switch")).unwrap();
        symlink("../target-final", root_path.join("switch")).unwrap();
        let audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("the final-source reload failures are retained");
        assert!(audit_plan.reload_failures().iter().any(|failure| matches!(
            &failure.error,
            sai_package::mixed_source::MixedSourceFinalReloadError::LocalPathIndex(error)
                if matches!(
                    error.as_ref(),
                    sai_package::local_source::LocalPackagePathIndexError::PathRequirementDiscovery { .. }
                )
        )), "unexpected final reload failures: {:#?}", audit_plan.reload_failures());

        let mut database = RootDatabase::new().expect("driver initializes");
        let root_file = database
            .intern_file_key(FileKey::new("acme:root#1", "sai.package").unwrap())
            .unwrap();
        let target_file = database
            .intern_file_key(FileKey::new("acme:target#1", "sai.package").unwrap())
            .unwrap();
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(root_file, root_manifest).unwrap()),
                "sai.package",
                Some("acme:root@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(target_file, first_manifest).unwrap()),
                "dep:acme:target@1.0.0/sai.package",
                Some("acme:target@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("the read-time manifests belong to one workspace snapshot");
        let target_version = mixed_output
            .provider()
            .output()
            .selection()
            .read_contracts()
            .keys()
            .next()
            .expect("one dependency was loaded")
            .clone();
        assert_eq!(
            audit_plan.final_manifest_snapshots()[&target_version].as_ref(),
            final_manifest
        );
        let diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &audit_plan,
                root_file,
                &BTreeMap::from([(target_version.clone(), target_file)]),
            )
            .expect("the reload failure is bound to the original workspace");

        assert_eq!(diagnostics.len(), 2);
        let path_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "PATH_PACKAGE_NOT_FOUND")
            .expect("the missing path package remains reportable");
        let semantic_diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "UNKNOWN_TARGET")
            .expect("the invalid final target is source-located");
        assert_eq!(
            path_diagnostic.message,
            "could not reload selected package acme:target@1.0.0: a local package required by the final source was not found"
        );
        assert_eq!(
            path_diagnostic.primary_label,
            "this path dependency could not be resolved in the final source"
        );
        let primary = path_diagnostic
            .primary()
            .expect("final-manifest path requirement location");
        assert!(
            primary
                .file
                .path
                .starts_with("dep:acme:target@1.0.0/.sai/final-source-")
        );
        assert_eq!(primary.file.package.as_deref(), Some("acme:target@1.0.0"));
        let final_source_file = diagnostics
            .snapshot()
            .file(primary.span.file)
            .expect("the returned snapshot contains the final-source file");
        assert_eq!(final_source_file.source().raw_bytes(), final_manifest);
        assert_eq!(
            diagnostics
                .snapshot()
                .file(root_file)
                .expect("the returned snapshot keeps the original MVS files")
                .source()
                .raw_bytes(),
            root_manifest
        );
        let start = primary.span.range.start().to_usize();
        let end = primary.span.range.end().to_usize();
        assert_eq!(
            &final_manifest[start..end],
            b"acme:secret @ ^1 from path \"missing/private-workspace\""
        );
        assert!(
            path_diagnostic.data.contains_key("requirement_chain"),
            "the original MVS chain remains attached"
        );
        assert!(
            path_diagnostic.children().iter().any(|child| {
                child.location.as_ref().is_some_and(|location| {
                    location.file.path == "sai.package"
                        && location.file.package.as_deref() == Some("acme:root@1.0.0")
                })
            }),
            "the original MVS edge remains attached to the read-time root manifest"
        );
        assert!(
            !path_diagnostic
                .message
                .contains("missing/private-workspace"),
            "final paths are not exposed in diagnostics"
        );

        let semantic_primary = semantic_diagnostic
            .primary()
            .expect("the semantic error points into the final manifest");
        let semantic_source = diagnostics
            .snapshot()
            .file(semantic_primary.span.file)
            .expect("the augmented snapshot includes the final semantic source");
        assert_eq!(
            &semantic_source.source().raw_bytes()[semantic_primary.span.range.start().to_usize()
                ..semantic_primary.span.range.end().to_usize()],
            b"wasm32-invalid"
        );
        assert!(
            semantic_diagnostic.data.contains_key("requirement_chain"),
            "semantic diagnostics retain the original MVS chain"
        );

        let mut oversized_final_manifest =
            String::from("package acme:target @ 1.0.0\nsource \"src\"\ntargets { wasm32-core");
        for index in 0..40 {
            oversized_final_manifest.push_str(&format!(", unknown_target_{index}"));
        }
        oversized_final_manifest.push_str(
            " }\nrequires { acme:secret @ ^1 from path \"missing/private-workspace\" }\n",
        );
        std::fs::write(
            final_path.join("sai.package"),
            oversized_final_manifest.as_bytes(),
        )
        .unwrap();
        let truncated_audit_plan = mixed_output
            .plan_final_source_audit(
                &HashMap::new(),
                &HashMap::new(),
                PackageValidationContext::default(),
                |_, _| unreachable!("this fixture has no Git release"),
                |_, _| unreachable!("this fixture has no Git path"),
            )
            .expect("semantic validation errors are retained for diagnostics");
        let truncated_diagnostics = database
            .mixed_source_reload_diagnostics(
                &snapshot,
                &mixed_output,
                &truncated_audit_plan,
                root_file,
                &BTreeMap::from([(target_version, target_file)]),
            )
            .expect("bounded semantic diagnostics are produced");
        let target_semantic_diagnostics = truncated_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "UNKNOWN_TARGET")
            .collect::<Vec<_>>();
        assert_eq!(target_semantic_diagnostics.len(), 32);
        let omission_indices = target_semantic_diagnostics
            .iter()
            .enumerate()
            .filter_map(|(index, diagnostic)| {
                matches!(
                    diagnostic.data.get("additional_semantic_errors_omitted"),
                    Some(sai_diagnostics::Value::Bool(true))
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            omission_indices,
            vec![target_semantic_diagnostics.len() - 1],
            "only the final emitted semantic diagnostic marks omitted errors"
        );
        let omission_diagnostic = target_semantic_diagnostics
            .last()
            .expect("the bounded diagnostics include a final entry");
        assert!(omission_diagnostic.children().iter().any(|child| {
            child
                .message
                .contains("additional final manifest semantic errors were omitted")
        }));
    }

    #[test]
    fn root_database_updates_inputs_and_rebinds_syntax_and_item_tree() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let key = FileKey::new("acme:app#0.1", "src/lib.sai").expect("file key is valid");
        let file_id = database
            .intern_file_key(key.clone())
            .expect("file key is interned");
        let first_contents: Arc<[u8]> = Arc::from(&b"fn alpha() {}\n"[..]);
        assert!(
            database
                .set_file_contents(key.clone(), Arc::clone(&first_contents), Durability::Low)
                .expect("initial source is set")
        );
        let first_snapshot = snapshot(file_id, &first_contents, 1);

        let first_parse = database
            .parse_file(&first_snapshot, file_id)
            .expect("first parse binds to the snapshot");
        let first_tree = database
            .def_collector(&first_snapshot, file_id)
            .expect("first item tree binds to the snapshot");
        assert_eq!(first_tree.items.len(), 1);
        assert_eq!(
            database
                .resolve_symbol(first_tree.items[0].name)
                .expect("item name belongs to the database")
                .as_ref(),
            "alpha"
        );
        let other_database = RootDatabase::new().expect("another session initializes");
        assert_eq!(
            other_database.resolve_symbol(first_tree.items[0].name),
            Err(LookupError::UnknownSymbol)
        );
        let first_ast_ids = database
            .ast_id_map(&first_snapshot, file_id)
            .expect("first AST IDs bind to the snapshot");
        assert!(first_tree.matches_ast_ids(&first_ast_ids));
        assert_eq!(
            database
                .source_text(file_id)
                .expect("interned file has decoded source")
                .source()
                .text(),
            "fn alpha() {}\n"
        );
        assert_eq!(
            database
                .line_index(file_id)
                .expect("interned file has a line index")
                .line_count(),
            2
        );
        assert_eq!(
            database.source_text(FileId::new(99)),
            Err(RootDatabaseError::FileNotInterned)
        );
        assert_eq!(
            database.line_index(FileId::new(99)),
            Err(RootDatabaseError::FileNotInterned)
        );

        let second_contents: Arc<[u8]> = Arc::from(&b"fn beta() {}\n"[..]);
        assert!(
            database
                .set_file_contents(key, Arc::clone(&second_contents), Durability::Low)
                .expect("updated source is set")
        );
        let second_snapshot = snapshot(file_id, &second_contents, 2);
        let second_parse = database
            .parse_file(&second_snapshot, file_id)
            .expect("updated parse binds to the snapshot");
        let second_tree = database
            .def_collector(&second_snapshot, file_id)
            .expect("updated item tree binds to the snapshot");
        let second_ast_ids = database
            .ast_id_map(&second_snapshot, file_id)
            .expect("updated AST IDs bind to the snapshot");

        assert_ne!(first_parse.root.green(), second_parse.root.green());
        assert_eq!(second_tree.items.len(), 1);
        assert_ne!(first_tree.items[0].name, second_tree.items[0].name);
        assert_eq!(
            database
                .resolve_symbol(second_tree.items[0].name)
                .expect("updated item name belongs to the database")
                .as_ref(),
            "beta"
        );
        assert!(second_tree.matches_ast_ids(&second_ast_ids));
    }

    #[test]
    fn root_database_returns_source_status_diagnostics_with_bound_file_ids() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let key = FileKey::new("acme:app#0.1", "src/lib.sai").expect("file key is valid");
        let file_id = database
            .intern_file_key(key.clone())
            .expect("file key is interned");
        let source_ref = SourceRef {
            path: "src/lib.sai".to_owned(),
            package: Some("acme:app@0.1.0".to_owned()),
            snapshot: None,
        };

        let (missing_source, diagnostic) = database
            .source_text_and_diagnostic(file_id, source_ref.clone())
            .expect("missing source is returned as a value");
        assert_eq!(missing_source.source().id(), file_id);
        let diagnostic = diagnostic.expect("missing source gets a read diagnostic");
        assert_eq!(diagnostic.code, sai_diagnostics::Code::SourceReadFailed);
        assert_eq!(
            diagnostic
                .primary()
                .expect("source read diagnostic has a location")
                .span
                .file,
            file_id
        );
        assert_eq!(
            diagnostic.data.get("reason"),
            Some(&sai_diagnostics::Value::String("not found".to_owned()))
        );

        database
            .set_file_contents(key, Arc::from(&b"fn ready() {}\n"[..]), Durability::Low)
            .expect("source bytes are set");
        let (_, diagnostic) = database
            .source_text_and_diagnostic(file_id, source_ref)
            .expect("ready source is returned");
        assert!(diagnostic.is_none());
    }

    #[test]
    fn root_database_collects_source_bound_root_cycle_diagnostics() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let root_version = MvsPackageVersion::new(
            "acme:app",
            Version::parse("1.0.0").expect("version is valid"),
        )
        .expect("root identity is valid");
        let dependency_version =
            MvsPackageVersion::new("acme:a", Version::parse("1.0.0").expect("version is valid"))
                .expect("dependency identity is valid");
        let root_package_id = database
            .intern_package("acme:app#1")
            .expect("root package is interned");
        let dependency_package_id = database
            .intern_package("acme:a#1")
            .expect("dependency package is interned");
        let root_file = database
            .intern_package_file(
                root_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("root manifest is interned");
        let dependency_file = database
            .intern_package_file(
                dependency_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("dependency manifest is interned");

        let root_source = "package acme:app @ 1.0.0\nrequires {\n    acme:a @ ^1\n}\n";
        let dependency_source = "package acme:a @ 1.0.0\nrequires {\n    acme:app @ ^1\n}\n";
        let root_span = root_source.find("acme:a @ ^1").expect("root edge exists")
            ..root_source.find("acme:a @ ^1").expect("root edge exists") + "acme:a @ ^1".len();
        let dependency_span = dependency_source
            .find("acme:app @ ^1")
            .expect("cycle edge exists")
            ..dependency_source
                .find("acme:app @ ^1")
                .expect("cycle edge exists")
                + "acme:app @ ^1".len();
        let root_contract = MvsContract::new(
            root_version.clone(),
            vec![
                MvsEdge::new(
                    "acme:a",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    root_span,
                )
                .expect("root requirement is valid"),
            ],
        )
        .expect("root contract is valid");
        let dependency_contract = MvsContract::new(
            dependency_version.clone(),
            vec![
                MvsEdge::new(
                    "acme:app",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    dependency_span,
                )
                .expect("cycle requirement is valid"),
            ],
        )
        .expect("dependency contract is valid");
        let selection = select(
            &root_contract,
            &HashMap::from([(dependency_version.clone(), dependency_contract)]),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("MVS selection records the root cycle");

        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(root_file, root_source.as_bytes())
                        .expect("root manifest decodes"),
                ),
                "sai.package",
                Some("acme:app@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(dependency_file, dependency_source.as_bytes())
                        .expect("dependency manifest decodes"),
                ),
                "dep:acme:a@1.0.0/sai.package",
                Some("acme:a@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("manifest snapshot is valid");

        let diagnostics = database
            .mvs_graph_diagnostics(
                &snapshot,
                &selection,
                root_file,
                &BTreeMap::from([(dependency_version.clone(), dependency_file)]),
                None,
                false,
            )
            .expect("source-bound MVS diagnostics");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "DEPENDENCY_CYCLE");
        assert_eq!(
            diagnostics[0].primary().expect("closing edge").file.path,
            "dep:acme:a@1.0.0/sai.package"
        );

        assert_eq!(
            database.mvs_graph_diagnostics(
                &snapshot,
                &selection,
                root_file,
                &BTreeMap::new(),
                None,
                false,
            ),
            Err(RootDatabaseError::MissingPackageManifest(
                dependency_version.clone()
            ))
        );
        assert!(matches!(
            database.mvs_graph_diagnostics(
                &snapshot,
                &selection,
                dependency_file,
                &BTreeMap::from([(dependency_version, dependency_file)]),
                None,
                false,
            ),
            Err(RootDatabaseError::PackageManifestFileKeyMismatch {
                expected_package_key,
                found_package_key,
                ..
            }) if expected_package_key == "acme:app#1" && found_package_key == "acme:a#1"
        ));
    }

    #[test]
    fn root_database_collects_selected_cycle_and_its_root_chain() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let root_version = MvsPackageVersion::new(
            "acme:app",
            Version::parse("1.0.0").expect("version is valid"),
        )
        .expect("root identity is valid");
        let a_version =
            MvsPackageVersion::new("acme:a", Version::parse("1.0.0").expect("version is valid"))
                .expect("package identity is valid");
        let b_version =
            MvsPackageVersion::new("acme:b", Version::parse("1.0.0").expect("version is valid"))
                .expect("package identity is valid");
        let root_package_id = database
            .intern_package("acme:app#1")
            .expect("root package is interned");
        let a_package_id = database
            .intern_package("acme:a#1")
            .expect("package is interned");
        let b_package_id = database
            .intern_package("acme:b#1")
            .expect("package is interned");
        let root_file = database
            .intern_package_file(
                root_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("root manifest is interned");
        let a_file = database
            .intern_package_file(
                a_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("manifest is interned");
        let b_file = database
            .intern_package_file(
                b_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("manifest is interned");

        let root_source = "package acme:app @ 1.0.0\nrequires {\n    acme:a @ ^1\n}\n";
        let a_source = "package acme:a @ 1.0.0\nrequires {\n    acme:b @ ^1\n}\n";
        let b_source = "package acme:b @ 1.0.0\nrequires {\n    acme:a @ ^1\n}\n";
        let root_edge_start = root_source.find("acme:a @ ^1").expect("edge exists");
        let a_edge_start = a_source.find("acme:b @ ^1").expect("edge exists");
        let b_edge_start = b_source.find("acme:a @ ^1").expect("edge exists");
        let edge_length = "acme:a @ ^1".len();
        let root_contract = MvsContract::new(
            root_version,
            vec![
                MvsEdge::new(
                    "acme:a",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    root_edge_start..root_edge_start + edge_length,
                )
                .expect("edge is valid"),
            ],
        )
        .expect("root contract is valid");
        let a_contract = MvsContract::new(
            a_version.clone(),
            vec![
                MvsEdge::new(
                    "acme:b",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    a_edge_start..a_edge_start + "acme:b @ ^1".len(),
                )
                .expect("edge is valid"),
            ],
        )
        .expect("contract is valid");
        let b_contract = MvsContract::new(
            b_version.clone(),
            vec![
                MvsEdge::new(
                    "acme:a",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    b_edge_start..b_edge_start + edge_length,
                )
                .expect("edge is valid"),
            ],
        )
        .expect("contract is valid");
        let selection = select(
            &root_contract,
            &HashMap::from([
                (a_version.clone(), a_contract),
                (b_version.clone(), b_contract),
            ]),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("selected graph contains a cycle");
        assert!(selection.is_complete());

        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(root_file, root_source.as_bytes())
                        .expect("root manifest decodes"),
                ),
                "sai.package",
                Some("acme:app@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(a_file, a_source.as_bytes()).expect("manifest decodes"),
                ),
                "dep:acme:a@1.0.0/sai.package",
                Some("acme:a@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(b_file, b_source.as_bytes()).expect("manifest decodes"),
                ),
                "dep:acme:b@1.0.0/sai.package",
                Some("acme:b@1.0.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("manifest snapshot is valid");

        let diagnostics = database
            .mvs_graph_diagnostics(
                &snapshot,
                &selection,
                root_file,
                &BTreeMap::from([(a_version, a_file), (b_version, b_file)]),
                None,
                false,
            )
            .expect("source-bound MVS diagnostics");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "DEPENDENCY_CYCLE");
        let sai_diagnostics::Value::Object(chain) = diagnostics[0]
            .data
            .get("requirement_chain")
            .expect("root chain")
        else {
            panic!("cycle diagnostic has a requirement chain");
        };
        let sai_diagnostics::Value::Array(steps) = chain.get("steps").expect("chain steps") else {
            panic!("requirement-chain steps are an array");
        };
        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn root_database_maps_same_line_candidates_to_distinct_snapshot_files() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let root_version = MvsPackageVersion::new(
            "acme:app",
            Version::parse("1.0.0").expect("version is valid"),
        )
        .expect("root identity is valid");
        let named_candidate =
            MvsPackageVersion::new("acme:a", Version::parse("1.0.0").expect("version is valid"))
                .expect("candidate identity is valid");
        let floor_candidate =
            MvsPackageVersion::new("acme:a", Version::parse("1.5.0").expect("version is valid"))
                .expect("candidate identity is valid");
        let root_package_id = database
            .intern_package("acme:app#1")
            .expect("root package is interned");
        let root_file = database
            .intern_package_file(
                root_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("root manifest is interned");
        let named_file = FileId::new(10_001);
        let floor_file = FileId::new(10_002);

        let root_source = "package acme:app @ 1.0.0\nrequires {\n    acme:a @ ^1\n}\n";
        let named_source = "package acme:a @ 1.0.0\n";
        let floor_source = "package acme:a @ 1.5.0\n";
        let edge_start = root_source.find("acme:a @ ^1").expect("root edge exists");
        let root_contract = MvsContract::new(
            root_version,
            vec![
                MvsEdge::new(
                    "acme:a",
                    VersionReq::parse("^1").expect("requirement is valid"),
                    Version::parse("1.0.0").expect("version is valid"),
                    edge_start..edge_start + "acme:a @ ^1".len(),
                )
                .expect("root requirement is valid"),
            ],
        )
        .expect("root contract is valid");
        let contracts = HashMap::from([
            (
                named_candidate.clone(),
                MvsContract::new(named_candidate.clone(), vec![])
                    .expect("named candidate contract is valid"),
            ),
            (
                floor_candidate.clone(),
                MvsContract::new(floor_candidate.clone(), vec![])
                    .expect("floor candidate contract is valid"),
            ),
        ]);
        let floors = HashMap::from([(
            named_candidate.node(),
            Version::parse("1.5.0").expect("floor version is valid"),
        )]);
        let selection = select(&root_contract, &contracts, &floors, &HashMap::new())
            .expect("MVS selection visits both same-line candidates");
        assert!(selection.is_complete());
        assert_eq!(selection.read_contracts().len(), 2);

        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(root_file, root_source.as_bytes())
                        .expect("root manifest decodes"),
                ),
                "sai.package",
                Some("acme:app@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(named_file, named_source.as_bytes())
                        .expect("named manifest decodes"),
                ),
                "dep:acme:a@1.0.0/sai.package",
                Some("acme:a@1.0.0".to_owned()),
                Some(1),
            ),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(floor_file, floor_source.as_bytes())
                        .expect("floor manifest decodes"),
                ),
                "dep:acme:a@1.5.0/sai.package",
                Some("acme:a@1.5.0".to_owned()),
                Some(1),
            ),
        ])
        .expect("candidate manifests have distinct snapshot IDs");

        let diagnostics = database
            .mvs_graph_diagnostics(
                &snapshot,
                &selection,
                root_file,
                &BTreeMap::from([(named_candidate, named_file), (floor_candidate, floor_file)]),
                None,
                false,
            )
            .expect("same-line candidate manifests map independently");
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn mvs_diagnostic_chains_resolve_unselected_candidate_manifests() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let root_version = MvsPackageVersion::new(
            "acme:app",
            Version::parse("1.0.0").expect("root version is valid"),
        )
        .expect("root package is valid");
        let root_package_id = database
            .intern_package("acme:app#1")
            .expect("root package is interned");
        let root_file = database
            .intern_package_file(
                root_package_id,
                PackageRelativePath::new("sai.package").expect("manifest path is valid"),
            )
            .expect("root manifest is interned");

        let root_source = "package acme:app @ 1.0.0\nrequires {\n    acme:a @ ^1\n    acme:c @ ^1\n    acme:lib @ ^1\n}\n";
        let manifest = |package: &str, version: &str, requirements: &[(&str, &str, &str)]| {
            let mut source = format!("package {package} @ {version}\n");
            if !requirements.is_empty() {
                source.push_str("requires {\n");
                for (target, requirement, _) in requirements {
                    source.push_str(&format!("    {target} @ {requirement}\n"));
                }
                source.push_str("}\n");
            }
            source
        };
        let root_requirements = [
            ("acme:a", "^1", "1.0.0"),
            ("acme:c", "^1", "1.0.0"),
            ("acme:lib", "^1", "1.0.0"),
        ];
        let root_contract = MvsContract::new(
            root_version,
            root_requirements
                .iter()
                .map(|(target, requirement, named_version)| {
                    let text = format!("{target} @ {requirement}");
                    let start = root_source.find(&text).expect("root requirement exists");
                    MvsEdge::new(
                        *target,
                        VersionReq::parse(requirement).expect("requirement is valid"),
                        Version::parse(named_version).expect("named version is valid"),
                        start..start + text.len(),
                    )
                    .expect("root edge is valid")
                })
                .collect(),
        )
        .expect("root contract is valid");

        let dependency_specs = [
            ("acme:a", "1.0.0", vec![("acme:b", "^1", "1.0.0")]),
            ("acme:b", "1.0.0", vec![("acme:lib", "^2", "2.0.0")]),
            ("acme:b", "1.4.0", vec![]),
            (
                "acme:c",
                "1.0.0",
                vec![("acme:b", ">=1.4", "1.4.0"), ("acme:d", "^1", "1.0.0")],
            ),
            ("acme:d", "1.0.0", vec![("acme:e", "^1", "1.0.0")]),
            ("acme:e", "1.0.0", vec![("acme:lib", "^2", "2.0.0")]),
            ("acme:lib", "1.0.0", vec![]),
            ("acme:lib", "2.0.0", vec![]),
        ];
        let mut contracts = HashMap::new();
        let mut manifest_files = BTreeMap::new();
        let mut snapshot_files = vec![SnapshotFile::new(
            Arc::new(
                SourceFile::decode(root_file, root_source.as_bytes())
                    .expect("root manifest decodes"),
            ),
            "sai.package",
            Some("acme:app@1.0.0".to_owned()),
            Some(1),
        )];

        for (index, (package, version, requirements)) in dependency_specs.into_iter().enumerate() {
            let package_version = MvsPackageVersion::new(
                package,
                Version::parse(version).expect("dependency version is valid"),
            )
            .expect("dependency package is valid");
            let source = manifest(package, version, &requirements);
            let edges = requirements
                .iter()
                .map(|(target, requirement, named_version)| {
                    let text = format!("{target} @ {requirement}");
                    let start = source.find(&text).expect("dependency requirement exists");
                    MvsEdge::new(
                        *target,
                        VersionReq::parse(requirement).expect("requirement is valid"),
                        Version::parse(named_version).expect("named version is valid"),
                        start..start + text.len(),
                    )
                    .expect("dependency edge is valid")
                })
                .collect();
            let contract = MvsContract::new(package_version.clone(), edges)
                .expect("dependency contract is valid");
            let file_id = FileId::new(20_001 + index as u32);
            contracts.insert(package_version.clone(), contract);
            manifest_files.insert(package_version.clone(), file_id);
            snapshot_files.push(SnapshotFile::new(
                Arc::new(
                    SourceFile::decode(file_id, source.as_bytes())
                        .expect("dependency manifest decodes"),
                ),
                format!("dep:{package}@{version}/sai.package"),
                Some(format!("{package}@{version}")),
                Some(index as u64 + 2),
            ));
        }
        let selection = select(&root_contract, &contracts, &HashMap::new(), &HashMap::new())
            .expect("MVS selection succeeds");
        assert!(selection.is_complete());
        let snapshot =
            WorkspaceSnapshot::new(snapshot_files).expect("all manifests share one snapshot");

        let diagnostics = database
            .mvs_graph_diagnostics(
                &snapshot,
                &selection,
                root_file,
                &manifest_files,
                None,
                false,
            )
            .expect("driver maps candidate manifests into diagnostics");
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "DUPLICATE_PACKAGE_LINE")
            .expect("the selected graph has duplicate lib lines");
        let sai_diagnostics::Value::Array(lines) = diagnostic
            .data
            .get("lines")
            .expect("duplicate lines are recorded")
        else {
            panic!("duplicate line data is an array");
        };
        let sai_diagnostics::Value::Object(lib_v2) = lines
            .iter()
            .find(|line| {
                matches!(
                    line,
                    sai_diagnostics::Value::Object(fields)
                        if fields.get("version")
                            == Some(&sai_diagnostics::Value::String("2.0.0".to_owned()))
                )
            })
            .expect("second lib line is present")
        else {
            panic!("duplicate line entry is an object");
        };
        let sai_diagnostics::Value::Array(chain) =
            lib_v2.get("chain").expect("line provenance is recorded")
        else {
            panic!("line provenance is an array");
        };
        let b_candidate_link = chain
            .iter()
            .find_map(|link| {
                let sai_diagnostics::Value::Object(fields) = link else {
                    return None;
                };
                (fields.get("requirer")
                    == Some(&sai_diagnostics::Value::String("acme:b".to_owned()))
                    && fields.get("requirer_version")
                        == Some(&sai_diagnostics::Value::String("1.0.0".to_owned())))
                .then_some(fields)
            })
            .expect("the shortest provenance chain uses the unselected b candidate");
        assert_eq!(
            b_candidate_link.get("requirer_selected"),
            Some(&sai_diagnostics::Value::Bool(false))
        );
        let sai_diagnostics::Value::Object(location) = b_candidate_link
            .get("location")
            .expect("candidate requirement has a source location")
        else {
            panic!("candidate source location is an object");
        };
        assert_eq!(
            location.get("path"),
            Some(&sai_diagnostics::Value::String(
                "dep:acme:b@1.0.0/sai.package".to_owned()
            ))
        );
        assert_eq!(
            location.get("snapshot"),
            Some(&sai_diagnostics::Value::Integer(3))
        );
    }

    #[test]
    fn root_database_interns_wit_paths_and_preserves_listing_states() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let package_id = database
            .intern_package("acme:app#0.1")
            .expect("package is interned");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");

        assert_eq!(database.intern_wit_files(package_id), Ok(None));

        let foreign_packages = PackageInterner::new();
        let foreign_package_id = foreign_packages
            .intern("acme:app#0.1")
            .expect("foreign package is interned");
        assert_eq!(
            database.intern_wit_files(foreign_package_id),
            Err(RootDatabaseError::Package(PackageIdError::UnknownPackage))
        );

        assert!(
            database
                .set_wit_files(
                    package_key.clone(),
                    WitFilesSnapshot::not_applicable(),
                    Durability::Low,
                )
                .expect("not-applicable inventory is set")
        );
        assert_eq!(
            database.intern_wit_files(package_id),
            Ok(Some(InternedWitFiles::NotApplicable))
        );

        assert!(
            database
                .set_wit_files(
                    package_key.clone(),
                    WitFilesSnapshot::listed("wit", []).expect("empty WIT inventory is valid"),
                    Durability::Low,
                )
                .expect("empty inventory is set")
        );
        assert_eq!(
            database.intern_wit_files(package_id),
            Ok(Some(InternedWitFiles::Listed(Vec::new())))
        );

        assert!(
            database
                .set_wit_files(
                    package_key,
                    WitFilesSnapshot::listed(
                        "wit",
                        [PackageRelativePath::new("wit/app.wit").expect("WIT path is valid")],
                    )
                    .expect("WIT inventory is valid"),
                    Durability::Low,
                )
                .expect("WIT file inventory is set")
        );
        let Some(InternedWitFiles::Listed(file_ids)) = database
            .intern_wit_files(package_id)
            .expect("WIT paths intern")
        else {
            panic!("one WIT file should be listed");
        };
        assert_eq!(file_ids.len(), 1);
        assert_eq!(
            database
                .file_key(file_ids[0])
                .expect("WIT file ID resolves")
                .path(),
            "wit/app.wit"
        );
        assert_eq!(
            database
                .file_key(file_ids[0])
                .expect("WIT file ID resolves")
                .package_key(),
            "acme:app#0.1"
        );
    }

    #[test]
    fn root_database_interns_package_source_files_and_preserves_inventory_entries() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let package_id = database
            .intern_package("acme:app#0.1")
            .expect("package is interned");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");
        let source_directory =
            PackageSourceDirectory::new("src").expect("source directory is valid");

        assert_eq!(
            database.intern_package_files(package_id, source_directory.clone()),
            Ok(None)
        );

        let foreign_packages = PackageInterner::new();
        let foreign_package_id = foreign_packages
            .intern("acme:app#0.1")
            .expect("foreign package is interned");
        assert_eq!(
            database.intern_package_files(foreign_package_id, source_directory.clone()),
            Err(RootDatabaseError::Package(PackageIdError::UnknownPackage))
        );

        assert!(
            database
                .set_package_files(
                    package_key.clone(),
                    PackageFilesSnapshot::new(false, []).expect("missing source dir is valid"),
                    Durability::Low,
                )
                .expect("missing source inventory is set")
        );
        let absent = database
            .intern_package_files(package_id, source_directory.clone())
            .expect("missing source inventory is read")
            .expect("inventory input exists");
        assert!(!absent.source_directory_exists());
        assert!(absent.entries().is_empty());

        assert!(
            database
                .set_package_files(
                    package_key.clone(),
                    PackageFilesSnapshot::new(true, []).expect("empty source dir is valid"),
                    Durability::Low,
                )
                .expect("empty source inventory is set")
        );
        let empty = database
            .intern_package_files(package_id, source_directory.clone())
            .expect("empty source inventory is read")
            .expect("inventory input exists");
        assert!(empty.source_directory_exists());
        assert!(empty.entries().is_empty());

        assert!(
            database
                .set_package_files(
                    package_key,
                    PackageFilesSnapshot::new(
                        true,
                        [
                            sai_vfs::PackageFileEntry::new(
                                "nested",
                                PackageFileKind::Directory,
                            )
                            .expect("directory entry is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "nested/lib.sai",
                                PackageFileKind::File,
                            )
                            .expect("source file entry is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "link",
                                PackageFileKind::Symlink,
                            )
                            .expect("symlink entry is valid"),
                        ],
                    )
                    .expect("source inventory is valid"),
                    Durability::Low,
                )
                .expect("source inventory is set")
        );
        let listed = database
            .intern_package_files(package_id, source_directory)
            .expect("source inventory is read")
            .expect("inventory input exists");
        assert!(listed.source_directory_exists());
        assert_eq!(listed.entries().len(), 3);
        assert_eq!(listed.entries()[0].path().as_str(), "link");
        assert_eq!(listed.entries()[0].kind(), PackageFileKind::Symlink);
        assert_eq!(listed.entries()[0].file_id(), None);
        assert_eq!(listed.entries()[1].path().as_str(), "nested");
        assert_eq!(listed.entries()[1].kind(), PackageFileKind::Directory);
        assert_eq!(listed.entries()[1].file_id(), None);
        let source_file = &listed.entries()[2];
        assert_eq!(source_file.path().as_str(), "nested/lib.sai");
        assert_eq!(source_file.kind(), PackageFileKind::File);
        let file_id = source_file.file_id().expect("source files have IDs");
        let stable_key = database
            .file_key(file_id)
            .expect("source file ID resolves in this session");
        assert_eq!(stable_key.package_key(), "acme:app#0.1");
        assert_eq!(stable_key.path(), "src/nested/lib.sai");
    }

    #[test]
    fn root_database_computes_the_tracked_module_tree_for_package_source() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let package_id = database
            .intern_package("acme:app#0.1")
            .expect("package is interned");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");
        assert!(
            database
                .set_package_files(
                    package_key,
                    PackageFilesSnapshot::new(
                        true,
                        [
                            sai_vfs::PackageFileEntry::new(
                                "lib.sai",
                                PackageFileKind::File,
                            )
                            .expect("root module path is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "model",
                                PackageFileKind::Directory,
                            )
                            .expect("namespace path is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "model/user.sai",
                                PackageFileKind::File,
                            )
                            .expect("module path is valid"),
                        ],
                    )
                    .expect("source inventory is valid"),
                    Durability::Low,
                )
                .expect("source inventory is set")
        );

        let tree = database
            .module_tree(
                package_id,
                PackageSourceDirectory::new("src").expect("source directory is valid"),
                None,
            )
            .expect("tracked module tree is computed");
        assert_eq!(tree.modules.len(), 2);
        assert_eq!(
            database
                .file_key(tree.modules[0].file)
                .expect("root file ID resolves")
                .path(),
            "src/lib.sai"
        );
        let user_path = tree.modules[1]
            .path
            .iter()
            .map(|symbol| {
                database
                    .resolve_symbol(*symbol)
                    .expect("module path symbol resolves")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(user_path, ["model", "user"]);
        assert_eq!(
            tree.modules[1].parent_dir,
            Some(sai_resolve::ModuleId::ROOT)
        );
    }
}
