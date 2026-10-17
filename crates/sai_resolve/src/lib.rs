#![forbid(unsafe_code)]

pub mod interface_impls;
pub mod interface_redirect;
pub mod interface_uses;
pub mod module_member_lookup;
pub mod module_path;
pub mod module_target;
pub mod owner;
pub mod package_scopes;
pub mod package_wit_names;
pub mod prelude;
pub mod use_binding;
pub mod use_item_binding;
pub mod use_path;
pub mod use_target;
pub mod use_target_diagnostics;
pub mod wit_name;
pub mod wit_name_collisions;
pub mod world_import_fns;
pub mod world_includes;
pub mod world_members;

pub use interface_impls::{
    InterfaceImpl, InterfaceImplFunction, InterfaceImplInput, InterfaceImplInterface,
    InterfaceImplInterfaceItem, InterfaceImplInterfaceItemKind, InterfaceImplItem,
    InterfaceImplItemKind, InterfaceImplResource, InterfaceImplResourceBinding,
    InterfaceImplTarget, InterfaceImplTypeResolution, InterfaceImplUnsupportedFunction,
    InterfaceImpls, InterfaceImplsInput, collect_interface_impls,
};
pub use interface_redirect::InterfaceRedirects;
pub use interface_uses::{
    InterfaceUseReference, InterfaceUses, InterfaceUsesInput, interface_uses,
};
pub use package_wit_names::{PackageWitNames, PackageWitNamesError, package_wit_names};
pub use wit_name::{sai_identifier_to_wit_name, suggested_representable_name};
pub use wit_name_collisions::{WitNameCollision, WitNameDefinition, wit_name_collisions};
pub use world_import_fns::{
    MissingLocalWorld, WorldImportFnItem, WorldImportFnWorld, WorldImportFnsInput,
    WorldImportFunction, world_import_fns,
};
pub use world_includes::{
    WorldIncludeReference, WorldIncludes, WorldIncludesInput, world_includes,
};
pub use world_members::{
    DeferredWorldField, WorldFieldResolution, WorldFieldResolutionError, WorldFieldResolutionInput,
    WorldImportFnLookup, WorldImportLookupState, WorldMemberDeferral, resolve_owner_world_fields,
};

use sai_diagnostics::{Code, Diagnostic, Location, Severity, SourceRef, Value};
use sai_intern::{InternError, Interner, PackageId, PackageIdError, PackageInterner, Symbol};
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_stable_hash::{StableHash, StableHasher};
use sai_syntax::is_reserved_identifier;
use sai_vfs::{
    FileId, FileInterner, PackageFileKind, PackageFilesSnapshot, PackageKey, PackageKeyError, Span,
    TextRange, TextSize, VfsInputs, VfsQueryGroup,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// A package-local module index. The root module, when present, is always zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleId(u32);

impl ModuleId {
    pub const ROOT: Self = Self(0);

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A package-local namespace-directory index.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NamespaceId(u32);

impl NamespaceId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A path component may name a source module, a namespace directory, or both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildEntry {
    Module(ModuleId),
    Namespace(NamespaceId),
    Both(ModuleId, NamespaceId),
}

/// One source file in a package's module tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleData {
    pub path: Vec<Symbol>,
    pub file: FileId,
    /// The nearest enclosing module whose path is a strict prefix of `path`.
    pub parent_dir: Option<ModuleId>,
    /// Directly nested modules and namespace directories, keyed by spelling.
    pub children: BTreeMap<String, ChildEntry>,
}

/// A directory path that has no corresponding source module file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceData {
    pub path: Vec<Symbol>,
    pub children: BTreeMap<String, ChildEntry>,
}

/// The source files and pure namespace directories that make up a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleTree {
    pub package: PackageId,
    /// The root module is `ModuleId::ROOT` when this vector is non-empty.
    pub modules: Vec<ModuleData>,
    pub namespaces: Vec<NamespaceData>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ModuleTree {
    pub fn module(&self, id: ModuleId) -> Option<&ModuleData> {
        self.modules.get(id.raw() as usize)
    }

    pub fn namespace(&self, id: NamespaceId) -> Option<&NamespaceData> {
        self.namespaces.get(id.raw() as usize)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleTreeQueryError {
    Query(QueryError),
    Package(PackageIdError),
    PackageKey(PackageKeyError),
    Symbol(InternError),
    MissingFileId(String),
    FilePackageMismatch(String),
    FileKeyMismatch(String),
}

impl From<QueryError> for ModuleTreeQueryError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<PackageIdError> for ModuleTreeQueryError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<PackageKeyError> for ModuleTreeQueryError {
    fn from(error: PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<InternError> for ModuleTreeQueryError {
    fn from(error: InternError) -> Self {
        Self::Symbol(error)
    }
}

/// Memoizes a stable, snapshot-independent module-tree shape by package key.
///
/// The query retains only normalized paths and deterministic IDs. The public
/// result is rebound to the requesting package, file interner, and symbol
/// interner on each call.
pub struct ModuleTreeQueries<V = VfsInputs> {
    vfs: Arc<V>,
    stable_module_tree: DerivedQuery<PackageKey, StableModuleTree>,
}

impl<V> Clone for ModuleTreeQueries<V> {
    fn clone(&self) -> Self {
        Self {
            vfs: Arc::clone(&self.vfs),
            stable_module_tree: self.stable_module_tree.clone(),
        }
    }
}

impl<V> ModuleTreeQueries<V>
where
    V: VfsQueryGroup + Send + Sync + 'static,
{
    pub fn new(vfs: V) -> Result<Self, QueryError> {
        let vfs = Arc::new(vfs);
        let query_vfs = Arc::clone(&vfs);
        let stable_module_tree = DerivedQuery::<PackageKey, StableModuleTree>::new(
            "module_tree",
            move |database: &mut QueryDatabase, key: &PackageKey| {
                let files = query_vfs.package_files(database, key.clone())?;
                Ok(StableModuleTree::build(files))
            },
        )?;
        Ok(Self {
            vfs,
            stable_module_tree,
        })
    }

    /// Builds the Stage 0 module tree for `package`.
    ///
    /// `file_ids` maps source-directory-relative paths to IDs interned by the
    /// requesting database. `package_field` is the manifest location used by
    /// `MISSING_ROOT_MODULE`; it may be absent for callers without manifest
    /// source information.
    pub fn module_tree(
        &self,
        database: &mut QueryDatabase,
        package: PackageId,
        packages: &PackageInterner,
        files: &FileInterner,
        symbols: &Interner,
        file_ids: &BTreeMap<String, FileId>,
        package_field: Option<Location>,
    ) -> Result<Arc<ModuleTree>, ModuleTreeQueryError> {
        let package_key = PackageKey::new(packages.resolve(package)?.as_ref())?;
        let stable = self.stable_module_tree.get(database, package_key.clone())?;
        let tree = stable.bind(
            package,
            &package_key,
            files,
            symbols,
            file_ids,
            package_field,
        )?;
        Ok(Arc::new(tree))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableModuleTree {
    source_directory_exists: bool,
    modules: Vec<StableModuleData>,
    namespaces: Vec<StableNamespaceData>,
    diagnostics: Vec<StableModuleDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableModuleData {
    path: Vec<String>,
    file_path: String,
    parent_dir: Option<ModuleId>,
    children: BTreeMap<String, ChildEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableNamespaceData {
    path: Vec<String>,
    children: BTreeMap<String, ChildEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StableDiagnosticCode {
    InvalidModuleName,
    NonAsciiPublicName,
    SymlinkInSource,
    MissingRootModule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableModuleDiagnostic {
    code: StableDiagnosticCode,
    path: String,
    segment: Option<String>,
    primary_file: Option<String>,
}

impl StableHash for StableModuleTree {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC1);
        self.source_directory_exists.stable_hash(hasher);
        self.modules.stable_hash(hasher);
        self.namespaces.stable_hash(hasher);
        self.diagnostics.stable_hash(hasher);
    }
}

impl StableHash for StableModuleData {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC2);
        self.path.stable_hash(hasher);
        self.file_path.stable_hash(hasher);
        self.parent_dir.stable_hash(hasher);
        self.children.stable_hash(hasher);
    }
}

impl StableHash for StableNamespaceData {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC3);
        self.path.stable_hash(hasher);
        self.children.stable_hash(hasher);
    }
}

impl StableHash for ChildEntry {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC4);
        match self {
            Self::Module(id) => {
                hasher.write_tag(0);
                id.raw().stable_hash(hasher);
            }
            Self::Namespace(id) => {
                hasher.write_tag(1);
                id.raw().stable_hash(hasher);
            }
            Self::Both(module, namespace) => {
                hasher.write_tag(2);
                module.raw().stable_hash(hasher);
                namespace.raw().stable_hash(hasher);
            }
        }
    }
}

impl StableHash for ModuleId {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC7);
        self.raw().stable_hash(hasher);
    }
}

impl StableHash for NamespaceId {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC8);
        self.raw().stable_hash(hasher);
    }
}

impl StableHash for StableModuleDiagnostic {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC5);
        self.code.stable_hash(hasher);
        self.path.stable_hash(hasher);
        self.segment.stable_hash(hasher);
        self.primary_file.stable_hash(hasher);
    }
}

impl StableHash for StableDiagnosticCode {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC6);
        hasher.write_tag(match self {
            Self::InvalidModuleName => 0,
            Self::NonAsciiPublicName => 1,
            Self::SymlinkInSource => 2,
            Self::MissingRootModule => 3,
        });
    }
}

impl StableModuleTree {
    fn build(snapshot: Option<PackageFilesSnapshot>) -> Self {
        let source_directory_exists = snapshot
            .as_ref()
            .is_some_and(PackageFilesSnapshot::source_directory_exists);
        let Some(snapshot) = snapshot.filter(PackageFilesSnapshot::source_directory_exists) else {
            return Self {
                source_directory_exists,
                modules: Vec::new(),
                namespaces: Vec::new(),
                diagnostics: vec![missing_root_diagnostic("lib.sai")],
            };
        };

        let mut root = EntryNode::default();
        for entry in snapshot.entries() {
            root.insert(entry.path().as_str(), entry.kind());
        }

        let has_root = root
            .children
            .get("lib.sai")
            .is_some_and(|entry| entry.kind == Some(PackageFileKind::File));
        let mut builder = ModuleTreeBuilder::default();
        if has_root {
            builder.modules.push(StableModuleData {
                path: Vec::new(),
                file_path: "lib.sai".to_owned(),
                parent_dir: None,
                children: BTreeMap::new(),
            });
        }
        let root_directory = builder.walk_directory(&root, &[], true, has_root);
        if has_root {
            if let Some(root_module) = builder.modules.first_mut() {
                root_module.children = root_directory.children.clone();
            }
        }

        if has_root {
            builder.set_parent_modules();
        } else {
            // A tree without a root has no valid package-local ModuleId(0).
            builder.modules.clear();
            builder.namespaces.clear();
            builder.directory_children.clear();
            builder.diagnostics.push(missing_root_diagnostic("lib.sai"));
        }

        Self {
            source_directory_exists,
            modules: builder.modules,
            namespaces: builder.namespaces,
            diagnostics: builder.diagnostics,
        }
    }

    fn bind(
        &self,
        package: PackageId,
        package_key: &PackageKey,
        files: &FileInterner,
        symbols: &Interner,
        file_ids: &BTreeMap<String, FileId>,
        package_field: Option<Location>,
    ) -> Result<ModuleTree, ModuleTreeQueryError> {
        let mut modules = Vec::with_capacity(self.modules.len());
        for module in &self.modules {
            let file = *file_ids
                .get(&module.file_path)
                .ok_or_else(|| ModuleTreeQueryError::MissingFileId(module.file_path.clone()))?;
            let key = files
                .file_key(file)
                .ok_or_else(|| ModuleTreeQueryError::MissingFileId(module.file_path.clone()))?;
            validate_file_key(package_key, key, &module.file_path)?;
            let path = intern_path(&module.path, symbols)?;
            modules.push(ModuleData {
                path,
                file,
                parent_dir: module.parent_dir,
                children: module.children.clone(),
            });
        }

        let mut namespaces = Vec::with_capacity(self.namespaces.len());
        for namespace in &self.namespaces {
            namespaces.push(NamespaceData {
                path: intern_path(&namespace.path, symbols)?,
                children: namespace.children.clone(),
            });
        }

        let diagnostics = self
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let code = match diagnostic.code {
                    StableDiagnosticCode::InvalidModuleName => Code::InvalidModuleName,
                    StableDiagnosticCode::NonAsciiPublicName => Code::NonAsciiPublicName,
                    StableDiagnosticCode::SymlinkInSource => Code::SymlinkInSource,
                    StableDiagnosticCode::MissingRootModule => Code::MissingRootModule,
                };
                let primary = match diagnostic.code {
                    StableDiagnosticCode::MissingRootModule => package_field.clone(),
                    _ => diagnostic
                        .primary_file
                        .as_ref()
                        .map(|path| source_location(package_key, files, file_ids, path))
                        .transpose()?,
                };
                let message = match diagnostic.code {
                    StableDiagnosticCode::InvalidModuleName => format!(
                        "invalid module name segment `{}`",
                        diagnostic.segment.as_deref().unwrap_or_default()
                    ),
                    StableDiagnosticCode::NonAsciiPublicName => {
                        "module name contains a non-ASCII character".to_owned()
                    }
                    StableDiagnosticCode::SymlinkInSource => {
                        "symbolic link in source directory".to_owned()
                    }
                    StableDiagnosticCode::MissingRootModule => {
                        "package source directory has no root module `lib.sai`".to_owned()
                    }
                };
                let mut result = Diagnostic::new(code, Severity::Error, message, primary);
                result.package = Some(package_key.as_str().to_owned());
                result
                    .data
                    .insert("path".to_owned(), Value::String(diagnostic.path.clone()));
                if let Some(segment) = &diagnostic.segment {
                    result
                        .data
                        .insert("segment".to_owned(), Value::String(segment.clone()));
                }
                Ok(result)
            })
            .collect::<Result<Vec<_>, ModuleTreeQueryError>>()?;

        Ok(ModuleTree {
            package,
            modules,
            namespaces,
            diagnostics,
        })
    }
}

fn intern_path(path: &[String], symbols: &Interner) -> Result<Vec<Symbol>, InternError> {
    path.iter().map(|segment| symbols.intern(segment)).collect()
}

fn source_location(
    package_key: &PackageKey,
    files: &FileInterner,
    file_ids: &BTreeMap<String, FileId>,
    source_relative_path: &str,
) -> Result<Location, ModuleTreeQueryError> {
    let file_id = *file_ids
        .get(source_relative_path)
        .ok_or_else(|| ModuleTreeQueryError::MissingFileId(source_relative_path.to_owned()))?;
    let key = files
        .file_key(file_id)
        .ok_or_else(|| ModuleTreeQueryError::MissingFileId(source_relative_path.to_owned()))?;
    validate_file_key(package_key, key, source_relative_path)?;
    Ok(Location {
        file: SourceRef {
            path: key.path().to_owned(),
            package: Some(key.package_key().to_owned()),
            snapshot: None,
        },
        span: Span::new(file_id, TextRange::empty(TextSize::new(0))),
    })
}

fn validate_file_key(
    package_key: &PackageKey,
    file_key: &sai_vfs::FileKey,
    source_relative_path: &str,
) -> Result<(), ModuleTreeQueryError> {
    if file_key.package_key() != package_key.as_str() {
        return Err(ModuleTreeQueryError::FilePackageMismatch(
            source_relative_path.to_owned(),
        ));
    }
    let path_matches = file_key.path() == source_relative_path
        || file_key
            .path()
            .strip_suffix(source_relative_path)
            .is_some_and(|prefix| prefix.ends_with('/'));
    if !path_matches {
        return Err(ModuleTreeQueryError::FileKeyMismatch(
            source_relative_path.to_owned(),
        ));
    }
    Ok(())
}

fn missing_root_diagnostic(path: &str) -> StableModuleDiagnostic {
    StableModuleDiagnostic {
        code: StableDiagnosticCode::MissingRootModule,
        path: path.to_owned(),
        segment: None,
        primary_file: None,
    }
}

#[derive(Default)]
struct EntryNode {
    kind: Option<PackageFileKind>,
    children: BTreeMap<String, EntryNode>,
}

impl EntryNode {
    fn insert(&mut self, path: &str, kind: PackageFileKind) {
        let mut node = self;
        for segment in path.split('/') {
            node = node.children.entry(segment.to_owned()).or_default();
        }
        node.kind = Some(kind);
    }
}

#[derive(Default)]
struct ModuleTreeBuilder {
    modules: Vec<StableModuleData>,
    namespaces: Vec<StableNamespaceData>,
    diagnostics: Vec<StableModuleDiagnostic>,
    directory_children: BTreeMap<Vec<String>, BTreeMap<String, ChildEntry>>,
}

struct DirectoryResult {
    namespace: Option<NamespaceId>,
    children: BTreeMap<String, ChildEntry>,
}

impl ModuleTreeBuilder {
    fn walk_directory(
        &mut self,
        directory: &EntryNode,
        path: &[String],
        is_root: bool,
        has_root: bool,
    ) -> DirectoryResult {
        let mut child_modules = BTreeMap::<String, ModuleId>::new();
        let mut child_directories = BTreeMap::<String, DirectoryResult>::new();

        for (name, entry) in &directory.children {
            if name.starts_with('.') {
                continue;
            }
            let entry_path = append_path(path, name);
            match entry.kind {
                Some(PackageFileKind::Directory) => {
                    if let Some(code) = validate_segment(name) {
                        self.diagnostics.push(StableModuleDiagnostic {
                            code,
                            path: join_path(&entry_path),
                            segment: Some(name.clone()),
                            primary_file: first_sai_descendant(entry, &entry_path),
                        });
                        continue;
                    }
                    let result = self.walk_directory(entry, &entry_path, false, has_root);
                    child_directories.insert(name.clone(), result);
                }
                Some(PackageFileKind::Symlink) => {
                    self.diagnostics.push(StableModuleDiagnostic {
                        code: StableDiagnosticCode::SymlinkInSource,
                        path: join_path(&entry_path),
                        segment: None,
                        primary_file: None,
                    });
                }
                Some(PackageFileKind::File) if name.ends_with(".sai") => {
                    if is_root && has_root && name == "lib.sai" {
                        continue;
                    }
                    let stem = name.strip_suffix(".sai").unwrap_or_default();
                    if let Some(code) = validate_segment(stem) {
                        self.diagnostics.push(StableModuleDiagnostic {
                            code,
                            path: join_path(&entry_path),
                            segment: Some(stem.to_owned()),
                            primary_file: Some(join_path(&entry_path)),
                        });
                        continue;
                    }
                    let mut module_path = path.to_vec();
                    module_path.push(stem.to_owned());
                    let raw = u32::try_from(self.modules.len())
                        .expect("module ID space is bounded by the file ID space");
                    let id = ModuleId::from_raw(raw);
                    self.modules.push(StableModuleData {
                        path: module_path,
                        file_path: join_path(&entry_path),
                        parent_dir: None,
                        children: BTreeMap::new(),
                    });
                    child_modules.insert(stem.to_owned(), id);
                }
                Some(PackageFileKind::File) | None => {}
            }
        }

        let mut names = child_modules.keys().cloned().collect::<BTreeSet<_>>();
        names.extend(child_directories.keys().cloned());
        let mut children = BTreeMap::new();
        for name in names {
            let module = child_modules.get(&name).copied();
            let namespace = child_directories
                .get(&name)
                .and_then(|directory| directory.namespace);
            let entry = match (module, namespace) {
                (Some(module), Some(namespace)) => Some(ChildEntry::Both(module, namespace)),
                (Some(module), None) => Some(ChildEntry::Module(module)),
                (None, Some(namespace)) => Some(ChildEntry::Namespace(namespace)),
                (None, None) => None,
            };
            if let Some(entry) = entry {
                children.insert(name, entry);
            }
        }

        let has_visible_entries = directory.children.keys().any(|name| !name.starts_with('.'));
        let namespace = if !is_root && has_visible_entries {
            let raw = u32::try_from(self.namespaces.len())
                .expect("namespace ID space is bounded by source entry count");
            let id = NamespaceId::from_raw(raw);
            self.namespaces.push(StableNamespaceData {
                path: path.to_vec(),
                children: children.clone(),
            });
            Some(id)
        } else {
            None
        };
        self.directory_children
            .insert(path.to_vec(), children.clone());
        DirectoryResult {
            namespace,
            children,
        }
    }

    fn set_parent_modules(&mut self) {
        let paths = self
            .modules
            .iter()
            .enumerate()
            .map(|(index, module)| (module.path.clone(), ModuleId::from_raw(index as u32)))
            .collect::<BTreeMap<_, _>>();
        for (index, module) in self.modules.iter_mut().enumerate() {
            module.children = self
                .directory_children
                .get(&module.path)
                .cloned()
                .unwrap_or_default();
            if index == 0 {
                module.parent_dir = None;
                continue;
            }
            module.parent_dir = (0..module.path.len())
                .rev()
                .find_map(|end| paths.get(&module.path[..end]).copied());
        }
    }
}

fn first_sai_descendant(directory: &EntryNode, path: &[String]) -> Option<String> {
    for (name, entry) in &directory.children {
        if name.starts_with('.') {
            continue;
        }
        let entry_path = append_path(path, name);
        match entry.kind {
            Some(PackageFileKind::File) if name.ends_with(".sai") => {
                return Some(join_path(&entry_path));
            }
            Some(PackageFileKind::Directory) => {
                if let Some(path) = first_sai_descendant(entry, &entry_path) {
                    return Some(path);
                }
            }
            Some(PackageFileKind::Symlink) | Some(PackageFileKind::File) | None => {}
        }
    }
    None
}

fn append_path(path: &[String], segment: &str) -> Vec<String> {
    let mut result = path.to_vec();
    result.push(segment.to_owned());
    result
}

fn join_path(path: &[String]) -> String {
    path.join("/")
}

fn validate_segment(segment: &str) -> Option<StableDiagnosticCode> {
    if !segment.is_ascii() {
        return Some(StableDiagnosticCode::NonAsciiPublicName);
    }
    let bytes = segment.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_lowercase()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        || STRICT_KEYWORDS.contains(&segment)
        || is_reserved_identifier(segment)
        || segment == "lib"
    {
        return Some(StableDiagnosticCode::InvalidModuleName);
    }
    None
}

const STRICT_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "borrow", "break", "const", "continue", "else",
    "enum", "export", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "move",
    "mut", "not", "or", "owned", "pub", "return", "self", "Self", "trait", "true", "type",
    "unsafe", "use", "var", "where", "while",
];

#[cfg(test)]
mod tests {
    use super::*;
    use sai_diagnostics::{SourceRef, Value};
    use sai_query::Durability;
    use sai_vfs::{
        PackageFileEntry, PackageFilesSnapshot, PackageSourceDirectory, PackageSourcePath,
    };

    const PACKAGE_KEY: &str = "acme:app#0.1";

    struct TestDatabase {
        query: QueryDatabase,
        vfs: VfsInputs,
        queries: ModuleTreeQueries<VfsInputs>,
        packages: PackageInterner,
        files: FileInterner,
        symbols: Interner,
        package: PackageId,
        key: PackageKey,
    }

    impl TestDatabase {
        fn new() -> Self {
            let vfs = VfsInputs::new().expect("VFS query handles initialize");
            let queries = ModuleTreeQueries::new(vfs.clone()).expect("module query initializes");
            let packages = PackageInterner::new();
            let package = packages.intern(PACKAGE_KEY).expect("package key is valid");
            Self {
                query: QueryDatabase::new(),
                vfs,
                queries,
                packages,
                files: FileInterner::new(),
                symbols: Interner::new(),
                package,
                key: PackageKey::new(PACKAGE_KEY).expect("package key is valid"),
            }
        }

        fn set_files(
            &mut self,
            source_directory_exists: bool,
            entries: impl IntoIterator<Item = (&'static str, PackageFileKind)>,
        ) -> BTreeMap<String, FileId> {
            let entries = entries
                .into_iter()
                .map(|(path, kind)| {
                    PackageFileEntry::new(path, kind).expect("source path is canonical")
                })
                .collect::<Vec<_>>();
            let snapshot = PackageFilesSnapshot::new(source_directory_exists, entries.clone())
                .expect("source inventory is canonical");
            self.vfs
                .set_package_files(&mut self.query, self.key.clone(), snapshot, Durability::Low)
                .expect("source inventory input is accepted");
            let paths = entries
                .iter()
                .filter(|entry| entry.kind() == PackageFileKind::File)
                .map(|entry| entry.path().clone())
                .collect::<Vec<PackageSourcePath>>();
            let ids = self
                .files
                .intern_source_files(
                    &self.packages,
                    self.package,
                    PackageSourceDirectory::new("src").expect("source directory is canonical"),
                    paths,
                )
                .expect("file IDs are interned");
            entries
                .iter()
                .filter(|entry| entry.kind() == PackageFileKind::File)
                .zip(ids)
                .map(|(entry, id)| (entry.path().as_str().to_owned(), id))
                .collect()
        }

        fn tree(
            &mut self,
            file_ids: &BTreeMap<String, FileId>,
            package_field: Option<Location>,
        ) -> Arc<ModuleTree> {
            self.queries
                .module_tree(
                    &mut self.query,
                    self.package,
                    &self.packages,
                    &self.files,
                    &self.symbols,
                    file_ids,
                    package_field,
                )
                .expect("module tree binds to current session")
        }

        fn module_path(&self, module: &ModuleData) -> Vec<String> {
            module
                .path
                .iter()
                .map(|symbol| {
                    self.symbols
                        .resolve(*symbol)
                        .expect("symbol resolves")
                        .to_string()
                })
                .collect()
        }

        fn namespace_path(&self, namespace: &NamespaceData) -> Vec<String> {
            namespace
                .path
                .iter()
                .map(|symbol| {
                    self.symbols
                        .resolve(*symbol)
                        .expect("symbol resolves")
                        .to_string()
                })
                .collect()
        }
    }

    fn entry(path: &'static str, kind: PackageFileKind) -> (&'static str, PackageFileKind) {
        (path, kind)
    }

    fn package_location() -> Location {
        Location {
            file: SourceRef {
                path: "sai.package".to_owned(),
                package: Some(PACKAGE_KEY.to_owned()),
                snapshot: None,
            },
            span: Span::new(FileId::new(100), TextRange::empty(TextSize::new(7))),
        }
    }

    #[test]
    fn root_and_depth_first_module_ids_and_namespace_entries_are_deterministic() {
        let mut db = TestDatabase::new();
        let entries = vec![
            entry("lib.sai", PackageFileKind::File),
            entry("a.sai", PackageFileKind::File),
            entry("a", PackageFileKind::Directory),
            entry("a/b.sai", PackageFileKind::File),
            entry("a/b", PackageFileKind::Directory),
            entry("a/b/c.sai", PackageFileKind::File),
            entry("z.sai", PackageFileKind::File),
            entry("ns", PackageFileKind::Directory),
            entry("ns/item.sai", PackageFileKind::File),
            entry("docs", PackageFileKind::Directory),
            entry("docs/readme.txt", PackageFileKind::File),
            entry("empty", PackageFileKind::Directory),
            entry(".hidden.sai", PackageFileKind::File),
            entry(".hidden", PackageFileKind::Directory),
            entry(".hidden/secret.sai", PackageFileKind::File),
        ];
        let file_ids = db.set_files(true, entries.clone());
        let first = db.tree(&file_ids, None);

        assert_eq!(first.modules[0].file, file_ids["lib.sai"]);
        assert_eq!(
            first
                .modules
                .iter()
                .map(|module| db.module_path(module))
                .collect::<Vec<_>>(),
            vec![
                vec![],
                vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
                vec!["a".to_owned(), "b".to_owned()],
                vec!["a".to_owned()],
                vec!["ns".to_owned(), "item".to_owned()],
                vec!["z".to_owned()],
            ]
        );
        assert_eq!(first.modules[1].parent_dir, Some(ModuleId::from_raw(2)));
        assert_eq!(first.modules[2].parent_dir, Some(ModuleId::from_raw(3)));
        assert_eq!(first.modules[3].parent_dir, Some(ModuleId::ROOT));
        assert_eq!(first.modules[4].parent_dir, Some(ModuleId::ROOT));
        assert_eq!(
            first.modules[0].children.get("a"),
            Some(&ChildEntry::Both(
                ModuleId::from_raw(3),
                first
                    .namespaces
                    .iter()
                    .enumerate()
                    .find(|(_, namespace)| db.namespace_path(namespace) == ["a"])
                    .map(|(id, _)| NamespaceId::from_raw(id as u32))
                    .expect("paired directory is represented as a namespace")
            ))
        );
        assert!(matches!(
            first.modules[0].children.get("ns"),
            Some(ChildEntry::Namespace(_))
        ));
        assert!(matches!(
            first.modules[0].children.get("docs"),
            Some(ChildEntry::Namespace(_))
        ));
        assert_eq!(first.namespaces.len(), 4);
        assert!(first.diagnostics.is_empty());

        let mut reversed = entries;
        reversed.reverse();
        let file_ids = db.set_files(true, reversed);
        let second = db.tree(&file_ids, None);
        assert_eq!(first.as_ref(), second.as_ref());
    }

    #[test]
    fn missing_root_has_manifest_location_and_no_module_ids() {
        let mut db = TestDatabase::new();
        let file_ids = db.set_files(
            true,
            [
                entry("orphan.sai", PackageFileKind::File),
                entry("dir", PackageFileKind::Directory),
                entry("dir/child.sai", PackageFileKind::File),
            ],
        );
        let manifest = package_location();
        let tree = db.tree(&file_ids, Some(manifest.clone()));

        assert!(tree.modules.is_empty());
        assert!(tree.namespaces.is_empty());
        assert_eq!(tree.diagnostics.len(), 1);
        assert_eq!(tree.diagnostics[0].code, Code::MissingRootModule);
        assert_eq!(tree.diagnostics[0].primary(), Some(&manifest));
        assert_eq!(
            tree.diagnostics[0].data.get("path"),
            Some(&Value::String("lib.sai".to_owned()))
        );

        let missing = db.set_files(false, []);
        let tree = db.tree(&missing, Some(manifest.clone()));
        assert!(tree.modules.is_empty());
        assert_eq!(tree.diagnostics[0].code, Code::MissingRootModule);
        assert_eq!(tree.diagnostics[0].primary(), Some(&manifest));
    }

    #[test]
    fn invalid_names_symlinks_and_hidden_paths_follow_stage_zero_rules() {
        let mut db = TestDatabase::new();
        let file_ids = db.set_files(
            true,
            [
                entry("lib.sai", PackageFileKind::File),
                entry("Bad", PackageFileKind::Directory),
                entry("Bad/child.sai", PackageFileKind::File),
                entry("class.sai", PackageFileKind::File),
                entry("good", PackageFileKind::Directory),
                entry("good/result.sai", PackageFileKind::File),
                entry("lib", PackageFileKind::Directory),
                entry("lib/child.sai", PackageFileKind::File),
                entry("link.sai", PackageFileKind::Symlink),
                entry("näme.sai", PackageFileKind::File),
                entry(".hidden.sai", PackageFileKind::File),
            ],
        );
        let tree = db.tree(&file_ids, None);

        assert_eq!(
            tree.modules
                .iter()
                .map(|module| db.module_path(module))
                .collect::<Vec<_>>(),
            vec![vec![], vec!["good".to_owned(), "result".to_owned()]]
        );
        assert_eq!(
            tree.diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![
                Code::InvalidModuleName,
                Code::InvalidModuleName,
                Code::InvalidModuleName,
                Code::SymlinkInSource,
                Code::NonAsciiPublicName,
            ]
        );
        let bad_directory = &tree.diagnostics[0];
        assert_eq!(
            bad_directory.data.get("path"),
            Some(&Value::String("Bad".to_owned()))
        );
        assert_eq!(
            bad_directory.data.get("segment"),
            Some(&Value::String("Bad".to_owned()))
        );
        assert_eq!(
            bad_directory
                .primary()
                .map(|location| location.file.path.as_str()),
            Some("src/Bad/child.sai")
        );
        assert_eq!(tree.diagnostics[3].primary(), None);
        assert_eq!(
            tree.diagnostics[3].data.get("path"),
            Some(&Value::String("link.sai".to_owned()))
        );
    }

    #[test]
    fn rejects_a_file_id_bound_to_another_source_path() {
        let mut db = TestDatabase::new();
        let mut file_ids = db.set_files(true, [entry("lib.sai", PackageFileKind::File)]);
        let wrong_id = db
            .files
            .intern_source_file(
                &db.packages,
                db.package,
                PackageSourceDirectory::new("src").expect("source directory is canonical"),
                PackageSourcePath::new("wrong.sai").expect("source path is canonical"),
            )
            .expect("second file key is interned");
        file_ids.insert("lib.sai".to_owned(), wrong_id);

        assert_eq!(
            db.queries.module_tree(
                &mut db.query,
                db.package,
                &db.packages,
                &db.files,
                &db.symbols,
                &file_ids,
                None,
            ),
            Err(ModuleTreeQueryError::FileKeyMismatch("lib.sai".to_owned()))
        );
    }
}
