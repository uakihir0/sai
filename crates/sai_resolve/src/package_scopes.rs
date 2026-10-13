//! Phase 1 declaration collection and local module import resolution.
//!
//! Callers provide item trees that have already passed `@target` filtering.
//! [`collect_phase1`] builds direct declaration scopes. The
//! [`collect_local_module_alias_bindings`] pass then inserts local module
//! aliases, resolves item imports from local modules, and computes local
//! `pub use` members. Dependency targets, interface member tables, test-only
//! imports, and the complete prelude remain deferred.
//! Extern-function children are checked against their validated target
//! attributes here because they are represented as children of an extern block
//! in the item tree.

use super::{ChildEntry, ModuleData, ModuleId, ModuleTree};
use crate::module_target::ModuleTargetError;
use crate::use_binding::{UseBindingNameError, derive_use_binding_name};
use crate::use_item_binding::{UseItemBindingError, derive_use_item_binding};
use crate::use_path::{ParsedUsePath, UsePathError, parse_use_path};
use crate::use_target::{UseTarget, UseTargetError, resolve_parsed_use_target};
use sai_diagnostics::{
    Child, ChildKind, Code, Diagnostic, Location, Related, Severity, SourceRef, Value,
};
use sai_hir::attrs::{
    AttributeTarget, TargetCfg, TargetKind, ValidatedExportName, validate_export_name,
};
use sai_hir::item_tree::{AttrName, ErasedAstId, ItemTreeKind, TargetFilterResult, UseEntry};
use sai_hir::{
    DefId, DefPath, DefPathData, DefPathError, DefPathInterner, DefPathInternerError,
    DisambiguatedDefPathData, LocalDefId, PackageId, Symbol, Visibility,
};
use sai_intern::{InternError, Interner, LookupError, PackageIdError, PackageInterner};
use sai_syntax::{
    GreenElement, GreenNode, SyntaxNode,
    ast::{self, AstNode, AstToken},
};
use sai_vfs::{FileId, PackageKey, SourceFile, Span, TextRange, TextSize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The entity kind stored by a declaration binding.
///
/// The variants match the name-resolution model in impl/03 §3.4. The current
/// collectors construct module-level items and local modules.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefKind {
    Fn,
    AssocFn,
    Struct,
    Enum,
    Variant,
    Distinct,
    Refined,
    Alias,
    Resource,
    Trait,
    TyParam,
    ConstParam,
    Const,
    Effect,
    Interface,
    World,
    WorldFn,
    ExternFn,
    Closure,
    AsyncBlock,
    AssocTy,
    Module,
}

/// A resolved entity used in module namespace bindings.
///
/// Later resolver phases extend this with interfaces, imported items, locals,
/// and primitive types. Current declaration and local-module-alias passes
/// construct definition resolutions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Res {
    Def(DefKind, DefId),
    Err,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingOrigin {
    Decl,
    Use { import: ImportIndex },
    PubUse { import: ImportIndex },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImportIndex(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBinding {
    pub use_ast_id: ErasedAstId,
    pub item_index: Option<u32>,
    pub name: Symbol,
    pub name_span: Span,
    pub res: Res,
    pub is_pub: bool,
    pub test_only: bool,
}

#[derive(Clone, Debug)]
struct PendingLocalUseItem {
    module: ModuleId,
    target: PendingItemTarget,
    item_index: u32,
    import: ImportIndex,
    source_name: Symbol,
    binding_name: Symbol,
    source_span: Span,
    binding_span: Span,
    alias_case_mismatch: bool,
    is_pub: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingItemTarget {
    LocalModule(ModuleId),
    Deferred,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub res: Res,
    /// Decoded-source range of the declared name.
    pub name_span: Span,
    pub origin: BindingOrigin,
    /// Effective visibility after declaration-specific rules.
    pub visibility: Visibility,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase1ModuleScopes {
    /// Canonical local definition path for this module; the package root has
    /// an empty path.
    pub module_def_id: LocalDefId,
    /// Direct lookup by session-local symbol. This map is not traversed to
    /// generate output; all diagnostic ordering comes from the source walk.
    pub declared: HashMap<Symbol, Binding>,
    /// Module member namespace, including declarations and local `pub use`s.
    pub members: HashMap<Symbol, Binding>,
    /// Module-level scope after local aliases and resolved item imports.
    pub scope: HashMap<Symbol, Binding>,
    /// Use bindings retained in source order for later resolution stages.
    pub imports: Vec<ImportBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase1PackageScopes {
    /// Module index is the corresponding `ModuleId::raw()` value.
    pub modules: Vec<Phase1ModuleScopes>,
    /// First definition for each export name in a root core-module build.
    pub exports: BTreeMap<String, LocalDefId>,
    /// Target-attribute and Phase-1 declaration diagnostics, sorted by
    /// source-relative file path and byte offset. Module-tree diagnostics
    /// remain owned by `module_tree`.
    pub diagnostics: Vec<Diagnostic>,
}

/// Build context needed for Phase 1 checks whose rules depend on the artifact.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Phase1BuildContext {
    /// Whether this is the root package of a core-module build.
    ///
    /// This is false for dependencies, component builds, native builds, and
    /// callers that do not yet provide artifact context.
    pub is_root_core_module_build: bool,
    /// Whether this analysis includes the test harness.
    ///
    /// Test analysis validates `@export_name` targets for every Wasm target
    /// but does not define the package's core-module export namespace.
    pub is_test_build: bool,
}

/// Snapshot and build metadata shared by one Phase-1 package collection.
pub struct Phase1CollectionConfig<'a> {
    pub snapshot: &'a sai_vfs::WorkspaceSnapshot,
    pub target_cfg: &'a TargetCfg,
    pub build_context: Phase1BuildContext,
}

impl Phase1PackageScopes {
    pub fn module(&self, id: super::ModuleId) -> Option<&Phase1ModuleScopes> {
        self.modules.get(id.raw() as usize)
    }
}

/// Snapshot-bound inputs for one module whose top-level declarations have
/// already been target-filtered.
pub struct FilteredModule<'a> {
    pub filtered: &'a TargetFilterResult,
    pub ast_ids: &'a ast::AstIdMap,
    pub root: &'a SyntaxNode,
    pub source: &'a SourceFile,
    pub source_ref: SourceRef,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageScopesError {
    PackageMismatch,
    MissingModuleInput(super::ModuleId),
    UnexpectedModuleInput(super::ModuleId),
    InvalidModuleInput(super::ModuleId),
    TooManyModules,
    MissingAstNode,
    MissingName,
    NameMismatch,
    InvalidSourceRange,
    Attribute(sai_hir::attrs::AttrValidationError),
    TargetFilter(sai_hir::item_tree::TargetFilterError),
    Package(PackageIdError),
    PackageKey(sai_vfs::PackageKeyError),
    DefPathValidation(DefPathError),
    Interner(InternError),
    Symbol(LookupError),
    InvalidModuleTree,
    InvalidCurrentModule,
    UnsupportedPackageKey,
    DefPath(DefPathInternerError),
    TooManyDefinitions,
}

impl From<PackageIdError> for PackageScopesError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<sai_vfs::PackageKeyError> for PackageScopesError {
    fn from(error: sai_vfs::PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<LookupError> for PackageScopesError {
    fn from(error: LookupError) -> Self {
        Self::Symbol(error)
    }
}

impl From<InternError> for PackageScopesError {
    fn from(error: InternError) -> Self {
        Self::Interner(error)
    }
}

impl From<sai_hir::attrs::AttrValidationError> for PackageScopesError {
    fn from(error: sai_hir::attrs::AttrValidationError) -> Self {
        Self::Attribute(error)
    }
}

impl From<DefPathInternerError> for PackageScopesError {
    fn from(error: DefPathInternerError) -> Self {
        Self::DefPath(error)
    }
}

/// Collects Phase-1 module declarations without resolving imports.
///
/// Modules are visited in `ModuleId` order, then their item-tree entries in
/// source order. Diagnostics are sorted by source-relative path and byte
/// offset before returning. The `module_inputs` map must have one entry for
/// every module in `module_tree`; each entry must come from the same AST
/// snapshot as its item tree and source file. The session-owned `def_paths`
/// interner is supplied by the caller so returned IDs remain usable by later
/// HIR queries.
pub fn collect_phase1(
    module_tree: &super::ModuleTree,
    module_inputs: &BTreeMap<super::ModuleId, FilteredModule<'_>>,
    config: Phase1CollectionConfig<'_>,
    packages: &PackageInterner,
    symbols: &Interner,
    def_paths: &DefPathInterner,
) -> Result<Phase1PackageScopes, PackageScopesError> {
    let Phase1CollectionConfig {
        snapshot,
        target_cfg,
        build_context,
    } = config;
    if module_tree.modules.len() > u32::MAX as usize {
        return Err(PackageScopesError::TooManyModules);
    }
    if def_paths.package() != module_tree.package {
        return Err(PackageScopesError::PackageMismatch);
    }

    let package_identity = packages.resolve(module_tree.package)?.to_string();
    let mut scopes = Vec::with_capacity(module_tree.modules.len());
    let mut diagnostics = Vec::new();
    let mut export_candidates = Vec::new();

    for (raw_module_id, module) in module_tree.modules.iter().enumerate() {
        let module_id = super::ModuleId::from_raw(
            u32::try_from(raw_module_id).map_err(|_| PackageScopesError::TooManyModules)?,
        );
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
        validate_module_input(module_id, module.file, input, snapshot)?;

        let mut module_diagnostics = input.filtered.diagnostics.clone();
        let canonical_module_path =
            if build_context.is_root_core_module_build && !build_context.is_test_build {
                canonical_module_path(module, symbols)?
            } else {
                String::new()
            };
        let module_path = module
            .path
            .iter()
            .copied()
            .map(|symbol| DisambiguatedDefPathData {
                data: DefPathData::Module(symbol),
                disambiguator: 0,
            })
            .collect::<Vec<_>>();
        let module_def_id = intern_module_def_path(
            module_tree.package,
            &module.path,
            packages,
            symbols,
            def_paths,
        )?;
        let mut module_disambiguators = HashMap::<DefPathData, u32>::new();
        let mut declared_locations = HashMap::<Symbol, Location>::new();
        let mut module_scopes = Phase1ModuleScopes {
            module_def_id,
            declared: HashMap::new(),
            members: HashMap::new(),
            scope: HashMap::new(),
            imports: Vec::new(),
        };
        let mut test_names = BTreeMap::<String, Location>::new();

        for use_entry in &input.filtered.item_tree.uses {
            validate_export_name_on_use(
                &use_entry.raw_attrs,
                snapshot,
                &package_identity,
                symbols,
                &mut module_diagnostics,
            )?;
        }

        for entry in &input.filtered.item_tree.items {
            let validated_export_name = validate_export_name_tree(
                entry,
                true,
                input,
                snapshot,
                &package_identity,
                symbols,
                &mut module_diagnostics,
            )?;
            match entry.kind {
                ItemTreeKind::ExternBlock => {
                    let component = extern_block_component(entry, input, symbols)?;
                    let disambiguator =
                        next_disambiguator(&mut module_disambiguators, component.clone())?;
                    let block_id = intern_path(
                        module_tree.package,
                        &module_path,
                        component,
                        disambiguator,
                        packages,
                        symbols,
                        def_paths,
                    )?;
                    let mut child_disambiguators = HashMap::<DefPathData, u32>::new();
                    for child in &entry.children {
                        if child.kind != ItemTreeKind::ExternFn {
                            continue;
                        }
                        if !child_survives_target(child, input, target_cfg)? {
                            continue;
                        }
                        let component = DefPathData::ValueNs(child.name);
                        let disambiguator =
                            next_disambiguator(&mut child_disambiguators, component.clone())?;
                        let child_id = def_paths
                            .intern_children(block_id, &[(component, disambiguator)])?
                            .into_iter()
                            .next()
                            .ok_or(PackageScopesError::MissingAstNode)?;
                        bind_declaration(
                            child,
                            DefKind::ExternFn,
                            child_id,
                            input,
                            &package_identity,
                            &mut module_scopes,
                            &mut declared_locations,
                            &mut module_diagnostics,
                            symbols,
                            Visibility::Private,
                        )?;
                    }
                }
                ItemTreeKind::Test | ItemTreeKind::Property => {
                    collect_test_name(
                        entry,
                        &module_path,
                        module_tree.package,
                        input,
                        &mut module_disambiguators,
                        &mut test_names,
                        &mut module_diagnostics,
                        &package_identity,
                        packages,
                        symbols,
                        def_paths,
                    )?;
                }
                ItemTreeKind::Impl => {
                    let component = DefPathData::Impl {
                        header_hash: entry.header_hash.ok_or(PackageScopesError::NameMismatch)?,
                    };
                    let disambiguator =
                        next_disambiguator(&mut module_disambiguators, component.clone())?;
                    intern_path(
                        module_tree.package,
                        &module_path,
                        component,
                        disambiguator,
                        packages,
                        symbols,
                        def_paths,
                    )?;
                }
                ItemTreeKind::Invariant => {
                    let component = DefPathData::Invariant(entry.name);
                    let disambiguator =
                        next_disambiguator(&mut module_disambiguators, component.clone())?;
                    intern_path(
                        module_tree.package,
                        &module_path,
                        component,
                        disambiguator,
                        packages,
                        symbols,
                        def_paths,
                    )?;
                }
                _ => {
                    let Some((kind, component)) = binding_identity(entry.kind, entry.name) else {
                        continue;
                    };
                    let disambiguator =
                        next_disambiguator(&mut module_disambiguators, component.clone())?;
                    let local_def_id = intern_path(
                        module_tree.package,
                        &module_path,
                        component,
                        disambiguator,
                        packages,
                        symbols,
                        def_paths,
                    )?;
                    if entry.kind == ItemTreeKind::Interface
                        && interface_conflicts_with_module_file(module, entry.name, symbols)?
                    {
                        let name = symbols.resolve(entry.name)?;
                        let (_, location) = declaration_name_location(entry, input)?;
                        diagnostics.push(declaration_diagnostic(
                            Code::ModuleInterfaceConflict,
                            format!(
                                "interface `{name}` conflicts with a module file at the same path"
                            ),
                            "conflicting interface",
                            location,
                            &package_identity,
                        ));
                    }
                    if build_context.is_root_core_module_build
                        && !build_context.is_test_build
                        && entry.kind == ItemTreeKind::Function
                        && entry.vis == Visibility::Export
                        && let Some(candidate) = collect_export_candidate(
                            entry,
                            local_def_id,
                            &canonical_module_path,
                            input,
                            symbols,
                            validated_export_name,
                        )?
                    {
                        export_candidates.push(candidate);
                    }
                    bind_declaration(
                        entry,
                        kind,
                        local_def_id,
                        input,
                        &package_identity,
                        &mut module_scopes,
                        &mut declared_locations,
                        &mut module_diagnostics,
                        symbols,
                        entry.vis,
                    )?;
                }
            }
        }
        module_diagnostics.sort_by_key(|diagnostic| {
            diagnostic
                .primary()
                .map(|primary| primary.span.range.start().raw())
                .unwrap_or(u32::MAX)
        });
        module_scopes.members = module_scopes.declared.clone();
        module_scopes.scope = module_scopes.declared.clone();
        diagnostics.extend(module_diagnostics);
        scopes.push(module_scopes);
    }

    for module_id in module_inputs.keys() {
        if module_id.raw() as usize >= module_tree.modules.len() {
            return Err(PackageScopesError::UnexpectedModuleInput(*module_id));
        }
    }

    let exports = collect_exports(
        export_candidates,
        snapshot,
        &package_identity,
        &mut diagnostics,
    )?;
    diagnostics.sort_by(|left, right| match (left.primary(), right.primary()) {
        (Some(left), Some(right)) => left
            .file
            .path
            .as_bytes()
            .cmp(right.file.path.as_bytes())
            .then_with(|| left.span.range.start().cmp(&right.span.range.start())),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });

    Ok(Phase1PackageScopes {
        modules: scopes,
        exports,
        diagnostics,
    })
}

/// Resolves package-local use aliases and item imports.
///
/// Local module targets have module-member lookup and `pub use` re-export
/// support. Foreign and interface item tables are retained as deferred import
/// records. The target-diagnostic pass owns unknown/current-module errors.
pub fn collect_local_module_alias_bindings(
    module_tree: &ModuleTree,
    module_inputs: &BTreeMap<ModuleId, FilteredModule<'_>>,
    scopes: &mut Phase1PackageScopes,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<Vec<Diagnostic>, PackageScopesError> {
    if module_tree.modules.len() != scopes.modules.len() {
        return Err(PackageScopesError::PackageMismatch);
    }
    if module_tree.modules.is_empty() {
        return Ok(Vec::new());
    }

    let package_key_spelling = packages.resolve(module_tree.package)?;
    let package_key = PackageKey::new(package_key_spelling.to_string())
        .map_err(PackageScopesError::PackageKey)?;
    let package_identity = package_key_spelling.to_string();
    let module_def_ids = scopes
        .modules
        .iter()
        .map(|module| module.module_def_id)
        .collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    let mut pending_items = Vec::new();

    for raw_module_id in 0..module_tree.modules.len() {
        let module_id = ModuleId::from_raw(
            u32::try_from(raw_module_id).map_err(|_| PackageScopesError::TooManyModules)?,
        );
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;

        for entry in &input.filtered.item_tree.uses {
            let Some((path_range, has_error)) =
                crate::use_target_diagnostics::use_package_path_range(entry, input)?
            else {
                if entry.is_pub && !entry.has_items {
                    let use_range = use_declaration_range(entry, input)?;
                    diagnostics.push(pub_use_without_items_diagnostic(
                        source_location(input, use_range)?,
                        &package_identity,
                    ));
                }
                continue;
            };
            if entry.is_pub && !entry.has_items {
                diagnostics.push(pub_use_without_items_diagnostic(
                    source_location(input, path_range)?,
                    &package_identity,
                ));
                continue;
            }
            if has_error {
                continue;
            }
            let parsed = match parse_use_path(entry, symbols) {
                Ok(parsed) => parsed,
                Err(UsePathError::Intern(error)) => {
                    return Err(PackageScopesError::Interner(error));
                }
                Err(_) => continue,
            };
            let binding_name = match derive_use_binding_name(entry, symbols) {
                Ok(binding_name) => binding_name,
                Err(UseBindingNameError::Intern(error)) => {
                    return Err(PackageScopesError::Interner(error));
                }
                Err(UseBindingNameError::Symbol(error)) => {
                    return Err(PackageScopesError::Symbol(error));
                }
                Err(UseBindingNameError::Path(UsePathError::Intern(error))) => {
                    return Err(PackageScopesError::Interner(error));
                }
                Err(
                    UseBindingNameError::Path(_)
                    | UseBindingNameError::MissingSymbol
                    | UseBindingNameError::MalformedParsedPath,
                ) => None,
            };

            let (target_res, target) = match resolve_parsed_use_target(
                &parsed,
                &package_key,
                packages,
                module_id,
                module_tree,
                symbols,
            ) {
                Ok(UseTarget::LocalModule(target)) => {
                    let target_def_id = module_def_ids
                        .get(target.raw() as usize)
                        .ok_or(PackageScopesError::MissingModuleInput(target))?;
                    (
                        Some(Res::Def(DefKind::Module, target_def_id.to_def_id())),
                        PendingItemTarget::LocalModule(target),
                    )
                }
                Ok(UseTarget::ForeignPackage(_)) => (None, PendingItemTarget::Deferred),
                Err(UseTargetError::Path(UsePathError::Intern(error))) => {
                    return Err(PackageScopesError::Interner(error));
                }
                Err(UseTargetError::Path(_)) => (None, PendingItemTarget::Unavailable),
                Err(UseTargetError::Module(
                    ModuleTargetError::MissingRootModule | ModuleTargetError::UseOfCurrentModule(_),
                )) => (None, PendingItemTarget::Unavailable),
                Err(UseTargetError::Module(ModuleTargetError::UnknownModule { .. })) => {
                    match local_interface_target(module_tree, scopes, &parsed) {
                        Some(def_id) => (
                            Some(Res::Def(DefKind::Interface, def_id)),
                            PendingItemTarget::Deferred,
                        ),
                        None => (None, PendingItemTarget::Unavailable),
                    }
                }
                Err(UseTargetError::Module(ModuleTargetError::InvalidModuleTree)) => {
                    return Err(PackageScopesError::InvalidModuleTree);
                }
                Err(UseTargetError::Module(ModuleTargetError::InvalidCurrentModule)) => {
                    return Err(PackageScopesError::InvalidCurrentModule);
                }
                Err(UseTargetError::Module(ModuleTargetError::Symbol(error))) => {
                    return Err(PackageScopesError::Symbol(error));
                }
                Err(UseTargetError::PackageInterner(error)) => {
                    return Err(PackageScopesError::Package(error));
                }
                Err(UseTargetError::PackageKeyMismatch { .. }) => {
                    return Err(PackageScopesError::PackageMismatch);
                }
                Err(UseTargetError::UnsupportedPackageKey(_)) => {
                    return Err(PackageScopesError::UnsupportedPackageKey);
                }
            };

            if let Some(binding_name) = binding_name {
                let name = binding_name.symbol();
                let name_spelling = symbols.resolve(name)?;
                let name_range =
                    use_binding_name_range(entry, input, &parsed, path_range, symbols)?;
                let name_span = Span::new(input.filtered.item_tree.file, name_range);
                let location = source_location(input, name_range)?;
                let mut valid_binding = true;

                if binding_name.requires_alias() {
                    let mut diagnostic = use_binding_diagnostic(
                        Code::UseBindingIsKeyword,
                        Severity::Error,
                        format!(
                            "derived use binding `{name_spelling}` is a keyword or reserved word; an alias is required"
                        ),
                        "binding name",
                        location.clone(),
                        &package_identity,
                    );
                    diagnostic
                        .data
                        .insert("name".to_owned(), Value::String(name_spelling.to_string()));
                    diagnostics.push(diagnostic);
                    let module_scopes = scopes
                        .modules
                        .get_mut(raw_module_id)
                        .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
                    push_import_binding(module_scopes, entry, None, name, name_span, Res::Err)?;
                    valid_binding = false;
                } else if is_reserved_type_name(&name_spelling) {
                    diagnostics.push(use_binding_diagnostic(
                        Code::ReservedTypeName,
                        Severity::Error,
                        format!("`{name_spelling}` is a reserved type name"),
                        "reserved name",
                        location.clone(),
                        &package_identity,
                    ));
                    let module_scopes = scopes
                        .modules
                        .get_mut(raw_module_id)
                        .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
                    push_import_binding(module_scopes, entry, None, name, name_span, Res::Err)?;
                    valid_binding = false;
                }

                if valid_binding {
                    if let Some(res) = target_res {
                        let module_scopes = scopes
                            .modules
                            .get_mut(raw_module_id)
                            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
                        push_import_binding(module_scopes, entry, None, name, name_span, res)?;
                    }
                }
            }

            for (raw_item_index, item) in entry.items.iter().enumerate() {
                let item_binding = match derive_use_item_binding(item, symbols) {
                    Ok(binding) => binding,
                    Err(
                        UseItemBindingError::SourceSymbol(error)
                        | UseItemBindingError::AliasSymbol(error),
                    ) => {
                        return Err(PackageScopesError::Symbol(error));
                    }
                    Err(
                        UseItemBindingError::MissingSourceName
                        | UseItemBindingError::MissingAlias
                        | UseItemBindingError::MalformedSourceName
                        | UseItemBindingError::MalformedAlias,
                    ) => continue,
                };
                let item_index = u32::try_from(raw_item_index)
                    .map_err(|_| PackageScopesError::TooManyDefinitions)?;
                let source_span = Span::new(input.filtered.item_tree.file, item.range);
                let binding_range =
                    use_item_binding_name_range(entry, input, raw_item_index, item)?;
                let binding_span = Span::new(input.filtered.item_tree.file, binding_range);
                let module_scopes = scopes
                    .modules
                    .get_mut(raw_module_id)
                    .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
                let import = push_import_binding(
                    module_scopes,
                    entry,
                    Some(item_index),
                    item_binding.binding_name,
                    binding_span,
                    Res::Err,
                )?;
                pending_items.push(PendingLocalUseItem {
                    module: module_id,
                    target,
                    item_index,
                    import,
                    source_name: item_binding.source_name,
                    binding_name: item_binding.binding_name,
                    source_span,
                    binding_span,
                    alias_case_mismatch: item_binding.alias_case_mismatch,
                    is_pub: entry.is_pub,
                });
            }
        }
    }

    resolve_local_use_items(
        module_inputs,
        scopes,
        pending_items,
        symbols,
        &package_identity,
        &mut diagnostics,
    )?;
    insert_resolved_imports_into_scopes(
        module_inputs,
        scopes,
        symbols,
        &package_identity,
        &mut diagnostics,
    )?;

    Ok(diagnostics)
}

fn use_item_binding_name_range(
    entry: &UseEntry,
    input: &FilteredModule<'_>,
    item_index: usize,
    item: &sai_hir::item_tree::UseItem,
) -> Result<TextRange, PackageScopesError> {
    if item.alias.is_none() {
        return Ok(item.range);
    }
    let file = input.filtered.item_tree.file;
    let syntax = input
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(file, input.root))
        .ok_or(PackageScopesError::MissingAstNode)?;
    let use_decl = ast::UseDecl::cast(syntax).ok_or(PackageScopesError::NameMismatch)?;
    use_decl
        .items()
        .and_then(|items| items.items().nth(item_index))
        .and_then(|item| item.alias())
        .map(|name| name.syntax().text_range())
        .ok_or(PackageScopesError::MissingName)
}

fn resolve_local_use_items(
    module_inputs: &BTreeMap<ModuleId, FilteredModule<'_>>,
    scopes: &mut Phase1PackageScopes,
    pending: Vec<PendingLocalUseItem>,
    symbols: &Interner,
    package_identity: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), PackageScopesError> {
    let mut pub_use_candidates = HashMap::new();
    for (index, item) in pending.iter().enumerate() {
        if !item.is_pub || item.target == PendingItemTarget::Unavailable {
            continue;
        }
        let name = symbols.resolve(item.binding_name)?;
        if is_reserved_type_name(&name) {
            continue;
        }
        pub_use_candidates
            .entry((item.module, item.binding_name))
            .or_insert_with(Vec::new)
            .push(index);
    }

    let mut resolver = LocalModuleMemberResolver {
        module_inputs,
        scopes,
        pending: &pending,
        pub_use_candidates,
        memo: HashMap::new(),
        stack: Vec::new(),
        reported_cycles: std::collections::HashSet::new(),
        reported_unresolved: std::collections::HashSet::new(),
        reported_private_reexports: std::collections::HashSet::new(),
        symbols,
        package_identity,
        diagnostics,
    };

    // Build all public member namespaces before ordinary imports use them.
    // `lookup` still resolves re-export chains recursively, but this pass
    // order makes diagnostics and member-name suggestions observe completed
    // local `X(M)` tables.
    for item in pending.iter().filter(|item| item.is_pub) {
        resolver.resolve_item_import(item)?;
    }
    for item in pending.iter().filter(|item| !item.is_pub) {
        resolver.resolve_item_import(item)?;
    }
    Ok(())
}

fn insert_resolved_imports_into_scopes(
    module_inputs: &BTreeMap<ModuleId, FilteredModule<'_>>,
    scopes: &mut Phase1PackageScopes,
    symbols: &Interner,
    package_identity: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), PackageScopesError> {
    for (raw_module_id, module_scope) in scopes.modules.iter_mut().enumerate() {
        let module_id = ModuleId::from_raw(
            u32::try_from(raw_module_id).map_err(|_| PackageScopesError::TooManyModules)?,
        );
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
        let mut events = module_scope
            .declared
            .iter()
            .map(|(name, binding)| {
                Ok((
                    binding.name_span.range.start().raw(),
                    symbols.resolve(*name)?.to_string(),
                    *name,
                    binding.clone(),
                    None,
                ))
            })
            .collect::<Result<Vec<_>, PackageScopesError>>()?;
        for (raw_import_index, import) in module_scope.imports.iter().enumerate() {
            if import.res == Res::Err {
                continue;
            }
            let import_index = ImportIndex(
                u32::try_from(raw_import_index)
                    .map_err(|_| PackageScopesError::TooManyDefinitions)?,
            );
            let origin = if import.is_pub && import.item_index.is_some() {
                BindingOrigin::PubUse {
                    import: import_index,
                }
            } else {
                BindingOrigin::Use {
                    import: import_index,
                }
            };
            let visibility = if import.is_pub && import.item_index.is_some() {
                Visibility::Pub
            } else {
                Visibility::Private
            };
            events.push((
                import.name_span.range.start().raw(),
                symbols.resolve(import.name)?.to_string(),
                import.name,
                Binding {
                    res: import.res,
                    name_span: import.name_span,
                    origin,
                    visibility,
                },
                Some(import_index),
            ));
        }
        events.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.as_bytes().cmp(right.1.as_bytes()))
                .then_with(|| {
                    left.4
                        .map(|index| index.0)
                        .cmp(&right.4.map(|index| index.0))
                })
        });
        module_scope.scope.clear();
        for (_, spelling, name, binding, import_index) in events {
            let location = source_location(input, binding.name_span.range)?;
            let previous = module_scope.scope.get(&name).cloned();
            match previous {
                None => {
                    let is_module_alias = import_index
                        .and_then(|index| module_scope.imports.get(index.0 as usize))
                        .is_some_and(|import| import.item_index.is_none());
                    module_scope.scope.insert(name, binding);
                    if is_module_alias && is_prelude_module_alias_name(&spelling) {
                        diagnostics.push(use_binding_diagnostic(
                            Code::ShadowsPrelude,
                            Severity::Warning,
                            format!("module alias `{spelling}` shadows a prelude binding"),
                            "prelude shadow",
                            location,
                            package_identity,
                        ));
                    }
                }
                Some(previous) if previous.res == binding.res => {
                    let redundant_import = import_index
                        .map(|_| (binding.name_span, spelling.clone()))
                        .or_else(|| match previous.origin {
                            BindingOrigin::Use { .. } | BindingOrigin::PubUse { .. } => {
                                Some((previous.name_span, spelling.clone()))
                            }
                            BindingOrigin::Decl => None,
                        });
                    if let Some((span, imported_name)) = redundant_import {
                        diagnostics.push(use_binding_diagnostic(
                            Code::RedundantImport,
                            Severity::Warning,
                            format!("import `{imported_name}` already denotes the same entity"),
                            "redundant import",
                            source_location(input, span.range)?,
                            package_identity,
                        ));
                    }
                }
                Some(previous) => {
                    let mut diagnostic = use_binding_diagnostic(
                        Code::DuplicateBinding,
                        Severity::Error,
                        format!("`{spelling}` is already bound in this module"),
                        "duplicate binding",
                        location,
                        package_identity,
                    );
                    diagnostic
                        .push_related(Related::DeclaredHere {
                            location: source_location(input, previous.name_span.range)?,
                            what: "earlier binding".to_owned(),
                        })
                        .expect("diagnostics without a source snapshot accept related locations");
                    if let Some(import_index) = import_index {
                        let import = module_scope
                            .imports
                            .get_mut(import_index.0 as usize)
                            .ok_or(PackageScopesError::InvalidModuleTree)?;
                        import.res = Res::Err;
                    }
                    diagnostics.push(diagnostic);
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LocalMemberLookup {
    Missing,
    Deferred,
    Found(Binding),
    Failed,
    Cycle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LocalMemberMemo {
    InProgress,
    Done(LocalMemberLookup),
}

struct LocalMemberFrame {
    module: ModuleId,
    name: Symbol,
    pub_use_index: usize,
}

struct LocalModuleMemberResolver<'a> {
    module_inputs: &'a BTreeMap<ModuleId, FilteredModule<'a>>,
    scopes: &'a mut Phase1PackageScopes,
    pending: &'a [PendingLocalUseItem],
    pub_use_candidates: HashMap<(ModuleId, Symbol), Vec<usize>>,
    memo: HashMap<(ModuleId, Symbol), LocalMemberMemo>,
    stack: Vec<LocalMemberFrame>,
    reported_cycles: std::collections::HashSet<usize>,
    reported_unresolved: std::collections::HashSet<(ModuleId, ImportIndex)>,
    reported_private_reexports: std::collections::HashSet<(ModuleId, ImportIndex)>,
    symbols: &'a Interner,
    package_identity: &'a str,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl LocalModuleMemberResolver<'_> {
    fn resolve_item_import(
        &mut self,
        item: &PendingLocalUseItem,
    ) -> Result<(), PackageScopesError> {
        if item.alias_case_mismatch {
            let source_name = self.symbols.resolve(item.source_name)?;
            let binding_name = self.symbols.resolve(item.binding_name)?;
            let location = self.location(item.module, item.binding_span)?;
            self.diagnostics.push(use_binding_diagnostic(
                Code::AliasCaseMismatch,
                Severity::Error,
                format!(
                    "item alias `{binding_name}` has a different case class from `{source_name}`"
                ),
                "case mismatch",
                location,
                self.package_identity,
            ));
        }

        let binding_name = self.symbols.resolve(item.binding_name)?;
        if is_reserved_type_name(&binding_name) {
            self.diagnostics.push(use_binding_diagnostic(
                Code::ReservedTypeName,
                Severity::Error,
                format!("`{binding_name}` is a reserved type name"),
                "reserved name",
                self.location(item.module, item.binding_span)?,
                self.package_identity,
            ));
            self.set_import_res(item.module, item.import, Res::Err)?;
            return Ok(());
        }

        let target_module = match item.target {
            PendingItemTarget::LocalModule(module) => module,
            PendingItemTarget::Deferred => {
                // Foreign packages and local interfaces do not have a
                // member table in this slice.
                return Ok(());
            }
            PendingItemTarget::Unavailable => {
                // The use-target pass owns missing, current-module, and
                // malformed-target diagnostics.
                return Ok(());
            }
        };
        let recorded_import = self
            .scopes
            .modules
            .get(item.module.raw() as usize)
            .and_then(|module| module.imports.get(item.import.0 as usize))
            .ok_or(PackageScopesError::InvalidModuleTree)?;
        debug_assert_eq!(recorded_import.item_index, Some(item.item_index));
        debug_assert_eq!(recorded_import.name, item.binding_name);
        let result = self.lookup(target_module, item.source_name)?;
        let target_binding = match result {
            LocalMemberLookup::Found(binding) => binding,
            LocalMemberLookup::Cycle => {
                self.set_import_res(item.module, item.import, Res::Err)?;
                return Ok(());
            }
            LocalMemberLookup::Missing | LocalMemberLookup::Failed => {
                self.report_unresolved_import(item)?;
                self.set_import_res(item.module, item.import, Res::Err)?;
                return Ok(());
            }
            LocalMemberLookup::Deferred => return Ok(()),
        };

        if item.is_pub && target_binding.visibility != Visibility::Pub {
            self.report_private_reexport(item)?;
            self.set_import_res(item.module, item.import, Res::Err)?;
            return Ok(());
        }

        let res = target_binding.res;
        self.set_import_res(item.module, item.import, res)?;
        let visibility = if item.is_pub {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        let origin = if item.is_pub {
            BindingOrigin::PubUse {
                import: item.import,
            }
        } else {
            BindingOrigin::Use {
                import: item.import,
            }
        };
        let binding = Binding {
            res,
            name_span: item.binding_span,
            origin,
            visibility,
        };
        if item.is_pub {
            let module_scope = self
                .scopes
                .modules
                .get_mut(item.module.raw() as usize)
                .ok_or(PackageScopesError::MissingModuleInput(item.module))?;
            // The member namespace is computed independently from ordinary
            // scope conflicts. A colliding module alias still cannot hide a
            // successful public re-export from another module.
            module_scope
                .members
                .entry(item.binding_name)
                .or_insert_with(|| binding.clone());
        }
        Ok(())
    }

    fn lookup(
        &mut self,
        module: ModuleId,
        name: Symbol,
    ) -> Result<LocalMemberLookup, PackageScopesError> {
        let key = (module, name);
        if let Some(module_scope) = self.scopes.modules.get(module.raw() as usize) {
            if let Some(binding) = module_scope.declared.get(&name) {
                return Ok(LocalMemberLookup::Found(binding.clone()));
            }
        } else {
            return Err(PackageScopesError::MissingModuleInput(module));
        }
        match self.memo.get(&key) {
            Some(LocalMemberMemo::Done(result)) => return Ok(result.clone()),
            Some(LocalMemberMemo::InProgress) => {
                self.report_reexport_cycle(module, name)?;
                return Ok(LocalMemberLookup::Cycle);
            }
            None => {}
        }
        let Some(pub_use_indices) = self.pub_use_candidates.get(&key).cloned() else {
            self.memo
                .insert(key, LocalMemberMemo::Done(LocalMemberLookup::Missing));
            return Ok(LocalMemberLookup::Missing);
        };
        self.memo.insert(key, LocalMemberMemo::InProgress);
        let mut saw_deferred = false;
        let mut saw_cycle = false;
        let mut saw_failed = false;
        for pub_use_index in pub_use_indices {
            let edge = self
                .pending
                .get(pub_use_index)
                .ok_or(PackageScopesError::InvalidModuleTree)?
                .clone();
            let target_module = match edge.target {
                PendingItemTarget::LocalModule(target_module) => target_module,
                PendingItemTarget::Deferred => {
                    saw_deferred = true;
                    continue;
                }
                PendingItemTarget::Unavailable => continue,
            };
            self.stack.push(LocalMemberFrame {
                module,
                name,
                pub_use_index,
            });
            let target = self.lookup(target_module, edge.source_name)?;
            self.stack.pop();
            match target {
                LocalMemberLookup::Found(binding) if binding.visibility == Visibility::Pub => {
                    self.set_import_res(edge.module, edge.import, binding.res)?;
                    let result = LocalMemberLookup::Found(Binding {
                        res: binding.res,
                        name_span: edge.binding_span,
                        origin: BindingOrigin::PubUse {
                            import: edge.import,
                        },
                        visibility: Visibility::Pub,
                    });
                    self.memo.insert(key, LocalMemberMemo::Done(result.clone()));
                    return Ok(result);
                }
                LocalMemberLookup::Found(_) => {
                    self.report_private_reexport(&edge)?;
                    self.set_import_res(edge.module, edge.import, Res::Err)?;
                    saw_failed = true;
                }
                LocalMemberLookup::Missing | LocalMemberLookup::Failed => {
                    self.report_unresolved_import(&edge)?;
                    self.set_import_res(edge.module, edge.import, Res::Err)?;
                    saw_failed = true;
                }
                LocalMemberLookup::Cycle => {
                    self.set_import_res(edge.module, edge.import, Res::Err)?;
                    saw_cycle = true;
                }
                LocalMemberLookup::Deferred => {
                    saw_deferred = true;
                }
            }
        }
        let result = if saw_deferred {
            LocalMemberLookup::Deferred
        } else if saw_cycle {
            LocalMemberLookup::Cycle
        } else if saw_failed {
            LocalMemberLookup::Failed
        } else {
            LocalMemberLookup::Missing
        };
        self.memo.insert(key, LocalMemberMemo::Done(result.clone()));
        Ok(result)
    }

    fn report_reexport_cycle(
        &mut self,
        module: ModuleId,
        name: Symbol,
    ) -> Result<(), PackageScopesError> {
        let cycle_start = self
            .stack
            .iter()
            .position(|frame| frame.module == module && frame.name == name)
            .ok_or(PackageScopesError::InvalidModuleTree)?;
        let mut cycle_edges = self.stack[cycle_start..]
            .iter()
            .map(|frame| frame.pub_use_index)
            .collect::<Vec<_>>();
        cycle_edges.sort_unstable();
        cycle_edges.dedup();
        for index in &cycle_edges {
            let item = self
                .pending
                .get(*index)
                .ok_or(PackageScopesError::InvalidModuleTree)?;
            self.set_import_res(item.module, item.import, Res::Err)?;
        }
        if cycle_edges
            .iter()
            .all(|index| self.reported_cycles.contains(index))
        {
            return Ok(());
        }

        let mut cycle_items = cycle_edges
            .iter()
            .filter_map(|index| self.pending.get(*index).map(|item| (item, *index)))
            .collect::<Vec<_>>();
        cycle_items.sort_by(|(left, _), (right, _)| {
            let left_path = self
                .module_inputs
                .get(&left.module)
                .map(|input| input.source_ref.path.as_bytes())
                .unwrap_or_default();
            let right_path = self
                .module_inputs
                .get(&right.module)
                .map(|input| input.source_ref.path.as_bytes())
                .unwrap_or_default();
            left_path.cmp(right_path).then_with(|| {
                left.binding_span
                    .range
                    .start()
                    .cmp(&right.binding_span.range.start())
            })
        });
        let Some((primary_item, primary_import)) = cycle_items.first().copied() else {
            return Err(PackageScopesError::InvalidModuleTree);
        };
        let name = self.symbols.resolve(primary_item.binding_name)?;
        let mut diagnostic = use_binding_diagnostic(
            Code::ReexportCycle,
            Severity::Error,
            format!("re-export cycle involving `{name}`"),
            "re-export cycle",
            self.location(primary_item.module, primary_item.binding_span)?,
            self.package_identity,
        );
        for (item, edge_index) in cycle_items.iter().copied().skip(1) {
            diagnostic
                .push_related(Related::DeclaredHere {
                    location: self.location(item.module, item.binding_span)?,
                    what: "another re-export in this cycle".to_owned(),
                })
                .expect("diagnostics without a source snapshot accept related locations");
            self.reported_cycles.insert(edge_index);
        }
        self.reported_cycles.insert(primary_import);
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    fn report_unresolved_import(
        &mut self,
        item: &PendingLocalUseItem,
    ) -> Result<(), PackageScopesError> {
        if !self.reported_unresolved.insert((item.module, item.import)) {
            return Ok(());
        }
        let name = self.symbols.resolve(item.source_name)?;
        let candidates = match item.target {
            PendingItemTarget::LocalModule(module) => self.suggested_member_names(module, &name)?,
            PendingItemTarget::Deferred | PendingItemTarget::Unavailable => Vec::new(),
        };
        let mut diagnostic = use_binding_diagnostic(
            Code::UnresolvedImport,
            Severity::Error,
            format!("unresolved import `{name}`"),
            "unresolved import",
            self.location(item.module, item.source_span)?,
            self.package_identity,
        );
        diagnostic
            .data
            .insert("name".to_owned(), Value::String(name.to_string()));
        diagnostic.data.insert(
            "candidates".to_owned(),
            Value::Array(candidates.iter().cloned().map(Value::String).collect()),
        );
        if !candidates.is_empty() {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Help,
                    message: format!(
                        "similar names: {}",
                        candidates
                            .iter()
                            .map(|candidate| format!("`{candidate}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    location: None,
                })
                .expect("diagnostics without a source snapshot accept help children");
        }
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    fn suggested_member_names(
        &mut self,
        module: ModuleId,
        name: &str,
    ) -> Result<Vec<String>, PackageScopesError> {
        let module_scope = self
            .scopes
            .modules
            .get(module.raw() as usize)
            .ok_or(PackageScopesError::MissingModuleInput(module))?;
        let candidate_symbols = module_scope
            .members
            .keys()
            .copied()
            .chain(
                self.pub_use_candidates
                    .keys()
                    .filter_map(|(candidate_module, symbol)| {
                        (*candidate_module == module).then_some(*symbol)
                    }),
            )
            .collect::<Vec<_>>();
        let mut candidate_symbols = candidate_symbols
            .into_iter()
            .map(|symbol| Ok((self.symbols.resolve(symbol)?.to_string(), symbol)))
            .collect::<Result<Vec<_>, PackageScopesError>>()?;
        candidate_symbols.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        candidate_symbols.dedup();
        let mut names = Vec::<(u32, Symbol, String)>::new();
        for (spelling, symbol) in candidate_symbols {
            let LocalMemberLookup::Found(binding) = self.lookup(module, symbol)? else {
                continue;
            };
            if is_reserved_type_name(&spelling) {
                continue;
            }
            names.push((binding.name_span.range.start().raw(), symbol, spelling));
        }
        names.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.2.as_bytes().cmp(right.2.as_bytes()))
        });
        let mut seen = std::collections::HashSet::new();
        let mut ranked = names
            .into_iter()
            .filter_map(|(source_order, symbol, spelling)| {
                seen.insert(symbol)
                    .then(|| (damerau_levenshtein(name, &spelling), source_order, spelling))
            })
            .filter(|(distance, _, _)| *distance <= 2)
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.as_bytes().cmp(right.2.as_bytes()))
        });
        Ok(ranked
            .into_iter()
            .take(3)
            .map(|(_, _, spelling)| spelling)
            .collect())
    }

    fn report_private_reexport(
        &mut self,
        item: &PendingLocalUseItem,
    ) -> Result<(), PackageScopesError> {
        if !self
            .reported_private_reexports
            .insert((item.module, item.import))
        {
            return Ok(());
        }
        let name = self.symbols.resolve(item.source_name)?;
        self.diagnostics.push(use_binding_diagnostic(
            Code::PrivateReexport,
            Severity::Error,
            format!("cannot re-export package-private item `{name}`"),
            "private item",
            self.location(item.module, item.source_span)?,
            self.package_identity,
        ));
        Ok(())
    }

    fn set_import_res(
        &mut self,
        module: ModuleId,
        import: ImportIndex,
        res: Res,
    ) -> Result<(), PackageScopesError> {
        let module_scope = self
            .scopes
            .modules
            .get_mut(module.raw() as usize)
            .ok_or(PackageScopesError::MissingModuleInput(module))?;
        let binding = module_scope
            .imports
            .get_mut(import.0 as usize)
            .ok_or(PackageScopesError::InvalidModuleTree)?;
        binding.res = res;
        Ok(())
    }

    fn location(&self, module: ModuleId, span: Span) -> Result<Location, PackageScopesError> {
        let input = self
            .module_inputs
            .get(&module)
            .ok_or(PackageScopesError::MissingModuleInput(module))?;
        source_location(input, span.range)
    }
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let maximum = left.len() + right.len();
    let mut distances = vec![vec![0usize; right.len() + 2]; left.len() + 2];
    distances[0][0] = maximum;
    for index in 0..=left.len() {
        distances[index + 1][0] = maximum;
        distances[index + 1][1] = index;
    }
    for index in 0..=right.len() {
        distances[0][index + 1] = maximum;
        distances[1][index + 1] = index;
    }

    let mut last_seen = HashMap::<char, usize>::new();
    for left_index in 1..=left.len() {
        let mut last_match = 0usize;
        for right_index in 1..=right.len() {
            let previous_left = last_seen.get(&right[right_index - 1]).copied().unwrap_or(0);
            let previous_right = last_match;
            let substitution_cost = if left[left_index - 1] == right[right_index - 1] {
                last_match = right_index;
                0
            } else {
                1
            };
            distances[left_index + 1][right_index + 1] = [
                distances[left_index][right_index] + substitution_cost,
                distances[left_index + 1][right_index] + 1,
                distances[left_index][right_index + 1] + 1,
                distances[previous_left][previous_right]
                    + (left_index - previous_left - 1)
                    + 1
                    + (right_index - previous_right - 1),
            ]
            .into_iter()
            .min()
            .unwrap_or(maximum);
        }
        last_seen.insert(left[left_index - 1], left_index);
    }
    distances[left.len() + 1][right.len() + 1]
}

fn local_interface_target(
    module_tree: &ModuleTree,
    scopes: &Phase1PackageScopes,
    path: &ParsedUsePath,
) -> Option<DefId> {
    let interface_name = path.segments.last().copied()?;
    let parent_path = &path.segments[..path.segments.len() - 1];
    let parent_index = module_tree
        .modules
        .iter()
        .position(|module| module.path.as_slice() == parent_path)?;
    let binding = scopes
        .modules
        .get(parent_index)?
        .declared
        .get(&interface_name)?;
    match binding.res {
        Res::Def(DefKind::Interface, def_id) => Some(def_id),
        Res::Def(_, _) | Res::Err => None,
    }
}

/// Only lowercase prelude names can collide with a syntactically valid module
/// alias. The exact prelude's lowercase names are `panic` and `unreachable`;
/// item and declaration bindings use the complete prelude table in later
/// resolver work.
fn is_prelude_module_alias_name(name: &str) -> bool {
    matches!(name, "panic" | "unreachable")
}

fn use_declaration_range(
    entry: &UseEntry,
    input: &FilteredModule<'_>,
) -> Result<TextRange, PackageScopesError> {
    let file = input.filtered.item_tree.file;
    let syntax = input
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(file, input.root))
        .ok_or(PackageScopesError::MissingAstNode)?;
    let use_decl = ast::UseDecl::cast(syntax).ok_or(PackageScopesError::MissingAstNode)?;
    Ok(use_decl.syntax().text_range())
}

fn pub_use_without_items_diagnostic(location: Location, package_identity: &str) -> Diagnostic {
    use_binding_diagnostic(
        Code::PubUseWithoutItems,
        Severity::Error,
        "`pub use` requires an item list",
        "public use",
        location,
        package_identity,
    )
}

fn interface_conflicts_with_module_file(
    module: &ModuleData,
    interface_name: Symbol,
    symbols: &Interner,
) -> Result<bool, PackageScopesError> {
    if interface_name.is_error() {
        return Ok(false);
    }
    let name = symbols.resolve(interface_name)?;
    Ok(matches!(
        module.children.get(name.as_ref()),
        Some(ChildEntry::Module(_) | ChildEntry::Both(_, _))
    ))
}

fn use_binding_name_range(
    entry: &UseEntry,
    input: &FilteredModule<'_>,
    parsed: &ParsedUsePath,
    package_path_range: TextRange,
    symbols: &Interner,
) -> Result<TextRange, PackageScopesError> {
    let file = input.filtered.item_tree.file;
    let syntax = input
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(file, input.root))
        .ok_or(PackageScopesError::MissingAstNode)?;
    let use_decl = ast::UseDecl::cast(syntax).ok_or(PackageScopesError::NameMismatch)?;
    if entry.alias.is_some() {
        return use_decl
            .alias()
            .and_then(|alias| alias.name())
            .map(|name| name.syntax().text_range())
            .ok_or(PackageScopesError::MissingName);
    }

    let spelling = if let Some(segment) = parsed.segments.last() {
        symbols.resolve(*segment)?.to_string()
    } else {
        parsed
            .package_identity
            .rsplit_once(':')
            .map(|(_, package)| package.to_owned())
            .ok_or(PackageScopesError::NameMismatch)?
    };
    let relative_start = if parsed.segments.is_empty() {
        entry.pkg_path.rfind(':')
    } else {
        entry
            .pkg_path
            .rfind('/')
            .or_else(|| entry.pkg_path.rfind(':'))
    }
    .and_then(|offset| offset.checked_add(1))
    .ok_or(PackageScopesError::InvalidSourceRange)?;
    if entry.pkg_path.get(relative_start..) != Some(spelling.as_str()) {
        return Err(PackageScopesError::NameMismatch);
    }

    let start = package_path_range
        .start()
        .raw()
        .checked_add(
            u32::try_from(relative_start).map_err(|_| PackageScopesError::InvalidSourceRange)?,
        )
        .ok_or(PackageScopesError::InvalidSourceRange)?;
    let end = start
        .checked_add(
            u32::try_from(spelling.len()).map_err(|_| PackageScopesError::InvalidSourceRange)?,
        )
        .ok_or(PackageScopesError::InvalidSourceRange)?;
    TextRange::new(TextSize::new(start), TextSize::new(end))
        .ok_or(PackageScopesError::InvalidSourceRange)
}

fn push_import_binding(
    module_scopes: &mut Phase1ModuleScopes,
    entry: &UseEntry,
    item_index: Option<u32>,
    name: Symbol,
    name_span: Span,
    res: Res,
) -> Result<ImportIndex, PackageScopesError> {
    let import = ImportIndex(
        u32::try_from(module_scopes.imports.len())
            .map_err(|_| PackageScopesError::TooManyDefinitions)?,
    );
    module_scopes.imports.push(ImportBinding {
        use_ast_id: entry.ast_id,
        item_index,
        name,
        name_span,
        res,
        is_pub: entry.is_pub,
        test_only: false,
    });
    Ok(import)
}

fn use_binding_diagnostic(
    code: Code,
    severity: Severity,
    message: impl Into<String>,
    primary_label: &str,
    location: Location,
    package_identity: &str,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, severity, message, Some(location));
    diagnostic.primary_label = primary_label.to_owned();
    diagnostic.package = Some(package_identity.to_owned());
    diagnostic
}

struct ExportCandidate {
    module_path: String,
    source_offset: u32,
    name: String,
    def_id: LocalDefId,
    location: Location,
}

fn canonical_module_path(
    module: &ModuleData,
    symbols: &Interner,
) -> Result<String, PackageScopesError> {
    module
        .path
        .iter()
        .map(|symbol| symbols.resolve(*symbol).map(|name| name.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
        .map_err(Into::into)
}

fn validate_export_name_tree(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    module_level: bool,
    input: &FilteredModule<'_>,
    snapshot: &sai_vfs::WorkspaceSnapshot,
    package_identity: &str,
    symbols: &Interner,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<ValidatedExportName>, PackageScopesError> {
    let target = sai_hir::item_tree::entry_attribute_target(
        entry,
        module_level,
        input.source.id(),
        input.root,
        input.ast_ids,
    )
    .map_err(PackageScopesError::TargetFilter)?;
    let validated = validate_export_name(&entry.raw_attrs, target, symbols, snapshot)?;
    suppress_export_name_feature_gate(&entry.raw_attrs, diagnostics);
    for mut diagnostic in validated.diagnostics {
        diagnostic.package = Some(package_identity.to_owned());
        diagnostics.push(diagnostic);
    }
    for child in &entry.children {
        validate_export_name_tree(
            child,
            false,
            input,
            snapshot,
            package_identity,
            symbols,
            diagnostics,
        )?;
    }
    Ok(validated.export_name)
}

fn validate_export_name_on_use(
    attributes: &[sai_hir::item_tree::RawAttr],
    snapshot: &sai_vfs::WorkspaceSnapshot,
    package_identity: &str,
    symbols: &Interner,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), PackageScopesError> {
    let mut target = AttributeTarget::new(TargetKind::Use);
    target.module_level = true;
    let validated = validate_export_name(attributes, Some(target), symbols, snapshot)?;
    suppress_export_name_feature_gate(attributes, diagnostics);
    for mut diagnostic in validated.diagnostics {
        diagnostic.package = Some(package_identity.to_owned());
        diagnostics.push(diagnostic);
    }
    Ok(())
}

fn suppress_export_name_feature_gate(
    attributes: &[sai_hir::item_tree::RawAttr],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let export_attribute_spans = attributes
        .iter()
        .filter(|attribute| attribute.name == AttrName::ExportName && !attribute.has_error)
        .map(|attribute| attribute.span)
        .collect::<Vec<_>>();
    diagnostics.retain(|diagnostic| {
        !(diagnostic.code == Code::FeatureNotImplemented
            && matches!(
                diagnostic.data.get("feature"),
                Some(Value::String(feature)) if feature == "attribute @export_name"
            )
            && diagnostic
                .primary()
                .is_some_and(|primary| export_attribute_spans.contains(&primary.span)))
    });
}

fn collect_export_candidate(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    def_id: LocalDefId,
    module_path: &str,
    input: &FilteredModule<'_>,
    symbols: &Interner,
    export_name: Option<ValidatedExportName>,
) -> Result<Option<ExportCandidate>, PackageScopesError> {
    if entry.name.is_error() {
        return Ok(None);
    }
    let (name, location) = if let Some(export_name) = export_name {
        (
            export_name.value,
            source_location(input, export_name.string_span.range)?,
        )
    } else {
        let (_, location) = declaration_name_location(entry, input)?;
        (symbols.resolve(entry.name)?.to_string(), location)
    };
    let source_offset = location.span.range.start().raw();
    Ok(Some(ExportCandidate {
        module_path: module_path.to_owned(),
        source_offset,
        name,
        def_id,
        location,
    }))
}

fn collect_exports(
    mut candidates: Vec<ExportCandidate>,
    snapshot: &sai_vfs::WorkspaceSnapshot,
    package_identity: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<BTreeMap<String, LocalDefId>, PackageScopesError> {
    candidates.sort_by(|left, right| {
        left.module_path
            .as_bytes()
            .cmp(right.module_path.as_bytes())
            .then_with(|| left.source_offset.cmp(&right.source_offset))
    });

    let mut exports = BTreeMap::new();
    let mut first_by_name = BTreeMap::<String, Location>::new();
    for candidate in candidates {
        let first = first_by_name.get(&candidate.name).cloned();
        if first.is_none() {
            first_by_name.insert(candidate.name.clone(), candidate.location.clone());
            exports.insert(candidate.name.clone(), candidate.def_id);
        }
        if first.is_some() || candidate.name == "memory" {
            let mut diagnostic = Diagnostic::new(
                Code::DuplicateExportName,
                Severity::Error,
                format!("export name `{}` is not unique", candidate.name),
                Some(candidate.location),
            );
            diagnostic.primary_label = if candidate.name == "memory" {
                "reserved export name"
            } else {
                "duplicate export name"
            }
            .to_owned();
            diagnostic.package = Some(package_identity.to_owned());
            diagnostic = diagnostic
                .with_workspace_snapshot(snapshot)
                .map_err(|_| PackageScopesError::InvalidSourceRange)?;
            if let Some(location) = first {
                diagnostic
                    .push_related(Related::DeclaredHere {
                        location,
                        what: "first export with this name".to_owned(),
                    })
                    .map_err(|_| PackageScopesError::InvalidSourceRange)?;
            }
            if candidate.name == "memory" {
                diagnostic
                    .push_child(Child {
                        kind: ChildKind::Note,
                        message: "linear memory is exported under the name `memory`".to_owned(),
                        location: None,
                    })
                    .map_err(|_| PackageScopesError::InvalidSourceRange)?;
            }
            diagnostics.push(diagnostic);
        }
    }
    Ok(exports)
}

fn validate_module_input(
    module_id: super::ModuleId,
    file: FileId,
    input: &FilteredModule<'_>,
    snapshot: &sai_vfs::WorkspaceSnapshot,
) -> Result<(), PackageScopesError> {
    let item_tree = &input.filtered.item_tree;
    let snapshot_file = snapshot.file(file);
    if item_tree.file != file
        || !item_tree.matches_ast_ids(input.ast_ids)
        || !input.ast_ids.matches_root(file, input.root)
        || input.source.id() != file
        || !syntax_matches_source(input.root.green(), input.source.text())
        || snapshot_file.is_none_or(|snapshot_file| {
            snapshot_file.path() != input.source_ref.path
                || snapshot_file.package() != input.source_ref.package.as_deref()
                || snapshot_file.document_version() != input.source_ref.snapshot
                || snapshot_file.source().text() != input.source.text()
        })
    {
        return Err(PackageScopesError::InvalidModuleInput(module_id));
    }
    Ok(())
}

fn syntax_matches_source(root: &GreenNode, source: &str) -> bool {
    fn consume(source: &str, offset: &mut usize, text: &str) -> bool {
        let Some(end) = offset.checked_add(text.len()) else {
            return false;
        };
        let Some(source_text) = source.get(*offset..end) else {
            return false;
        };
        if source_text != text {
            return false;
        }
        *offset = end;
        true
    }

    fn matches_node(node: &GreenNode, source: &str, offset: &mut usize) -> bool {
        for child in node.children() {
            match child {
                GreenElement::Node(child) => {
                    if !matches_node(child, source, offset) {
                        return false;
                    }
                }
                GreenElement::Token(token) => {
                    for trivia in token.leading_trivia() {
                        if !consume(source, offset, trivia.text()) {
                            return false;
                        }
                    }
                    if !consume(source, offset, token.text()) {
                        return false;
                    }
                    for trivia in token.trailing_trivia() {
                        if !consume(source, offset, trivia.text()) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    let mut offset = 0;
    matches_node(root, source, &mut offset) && offset == source.len()
}

fn bind_declaration(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    kind: DefKind,
    local_def_id: LocalDefId,
    input: &FilteredModule<'_>,
    package_identity: &str,
    module_scopes: &mut Phase1ModuleScopes,
    declared_locations: &mut HashMap<Symbol, Location>,
    diagnostics: &mut Vec<Diagnostic>,
    symbols: &Interner,
    visibility: Visibility,
) -> Result<(), PackageScopesError> {
    if entry.name.is_error() {
        return Ok(());
    }
    let name = symbols.resolve(entry.name)?;
    let (name_span, location) = declaration_name_location(entry, input)?;
    if is_reserved_type_name(name.as_ref()) {
        diagnostics.push(declaration_diagnostic(
            Code::ReservedTypeName,
            format!("`{name}` is a reserved type name"),
            "reserved name",
            location,
            package_identity,
        ));
        return Ok(());
    }
    if let Some(previous) = declared_locations.get(&entry.name) {
        let mut diagnostic = declaration_diagnostic(
            Code::DuplicateBinding,
            format!("`{name}` is declared more than once in this module"),
            "duplicate declaration",
            location,
            package_identity,
        );
        diagnostic
            .push_related(Related::DeclaredHere {
                location: previous.clone(),
                what: "first declaration".to_owned(),
            })
            .expect("diagnostics without a source snapshot accept related locations");
        diagnostics.push(diagnostic);
        return Ok(());
    }

    declared_locations.insert(entry.name, location);
    module_scopes.declared.insert(
        entry.name,
        Binding {
            res: Res::Def(kind, local_def_id.to_def_id()),
            name_span,
            origin: BindingOrigin::Decl,
            visibility,
        },
    );
    Ok(())
}

fn collect_test_name(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    module_path: &[DisambiguatedDefPathData],
    package: PackageId,
    input: &FilteredModule<'_>,
    disambiguators: &mut HashMap<DefPathData, u32>,
    test_names: &mut BTreeMap<String, Location>,
    diagnostics: &mut Vec<Diagnostic>,
    package_identity: &str,
    packages: &PackageInterner,
    symbols: &Interner,
    def_paths: &DefPathInterner,
) -> Result<(), PackageScopesError> {
    let syntax = resolve_entry_syntax(entry, input)?;
    let (name, range) = match entry.kind {
        ItemTreeKind::Test => {
            let declaration =
                ast::TestDecl::cast(syntax).ok_or(PackageScopesError::NameMismatch)?;
            let Some(name) = declaration.name() else {
                return Ok(());
            };
            (name.value().into_owned(), name.syntax().text_range())
        }
        ItemTreeKind::Property => {
            let declaration =
                ast::PropertyDecl::cast(syntax).ok_or(PackageScopesError::NameMismatch)?;
            let Some(name) = declaration.name() else {
                return Ok(());
            };
            (name.value().into_owned(), name.syntax().text_range())
        }
        _ => return Err(PackageScopesError::NameMismatch),
    };
    if !entry.name.is_error() && symbols.resolve(entry.name)?.as_ref() != name {
        return Err(PackageScopesError::NameMismatch);
    }
    let location = source_location(input, range)?;
    let component = match entry.kind {
        ItemTreeKind::Test => DefPathData::Test(Arc::from(name.as_str())),
        ItemTreeKind::Property => DefPathData::Property(Arc::from(name.as_str())),
        _ => return Err(PackageScopesError::NameMismatch),
    };
    let disambiguator = next_disambiguator(disambiguators, component.clone())?;
    intern_path(
        package,
        module_path,
        component,
        disambiguator,
        packages,
        symbols,
        def_paths,
    )?;

    if name.is_empty() || name.chars().any(char::is_control) {
        diagnostics.push(declaration_diagnostic(
            Code::InvalidTestName,
            "test and property names must be non-empty and contain no control characters",
            "invalid test name",
            location,
            package_identity,
        ));
        return Ok(());
    }
    if let Some(previous) = test_names.get(&name) {
        let mut diagnostic = declaration_diagnostic(
            Code::DuplicateTestName,
            format!("test or property name `{name}` is already used in this module"),
            "duplicate test name",
            location,
            package_identity,
        );
        diagnostic
            .push_related(Related::DeclaredHere {
                location: previous.clone(),
                what: "first test or property with this name".to_owned(),
            })
            .expect("diagnostics without a source snapshot accept related locations");
        diagnostics.push(diagnostic);
    } else {
        test_names.insert(name, location);
    }
    Ok(())
}

fn binding_identity(kind: ItemTreeKind, name: Symbol) -> Option<(DefKind, DefPathData)> {
    Some(match kind {
        ItemTreeKind::Function => (DefKind::Fn, DefPathData::ValueNs(name)),
        ItemTreeKind::Struct => (DefKind::Struct, DefPathData::TypeNs(name)),
        ItemTreeKind::Alias => (DefKind::Alias, DefPathData::TypeNs(name)),
        ItemTreeKind::Refined => (DefKind::Refined, DefPathData::TypeNs(name)),
        ItemTreeKind::Enum => (DefKind::Enum, DefPathData::TypeNs(name)),
        ItemTreeKind::Distinct => (DefKind::Distinct, DefPathData::TypeNs(name)),
        ItemTreeKind::Resource => (DefKind::Resource, DefPathData::TypeNs(name)),
        ItemTreeKind::Trait => (DefKind::Trait, DefPathData::TypeNs(name)),
        ItemTreeKind::Const => (DefKind::Const, DefPathData::ValueNs(name)),
        ItemTreeKind::Effect => (DefKind::Effect, DefPathData::Effect(name)),
        ItemTreeKind::Interface => (DefKind::Interface, DefPathData::Interface(name)),
        ItemTreeKind::World => (DefKind::World, DefPathData::World(name)),
        ItemTreeKind::ExternFn => (DefKind::ExternFn, DefPathData::ValueNs(name)),
        _ => return None,
    })
}

fn extern_block_component(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    input: &FilteredModule<'_>,
    symbols: &Interner,
) -> Result<DefPathData, PackageScopesError> {
    let syntax = resolve_entry_syntax(entry, input)?;
    let declaration = ast::ExternBlock::cast(syntax).ok_or(PackageScopesError::NameMismatch)?;
    let abi = declaration
        .abi()
        .map(|literal| symbols.intern(literal.value().as_ref()))
        .transpose()?
        .unwrap_or(Symbol::ERROR);
    let module = declaration
        .module()
        .and_then(|module| module.value())
        .map(|literal| symbols.intern(literal.value().as_ref()))
        .transpose()?;
    Ok(DefPathData::ExternBlock { abi, module })
}

fn child_survives_target(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    input: &FilteredModule<'_>,
    target_cfg: &TargetCfg,
) -> Result<bool, PackageScopesError> {
    let attrs = input
        .filtered
        .validated_attrs
        .get(&entry.ast_id)
        .ok_or(PackageScopesError::MissingAstNode)?;
    Ok(attrs
        .target
        .as_ref()
        .is_none_or(|(predicate, _)| predicate.matches(target_cfg)))
}

fn declaration_name_location(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    input: &FilteredModule<'_>,
) -> Result<(Span, Location), PackageScopesError> {
    let syntax = resolve_entry_syntax(entry, input)?;
    let range = match entry.kind {
        ItemTreeKind::Function | ItemTreeKind::ExternFn => ast::FnDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Struct | ItemTreeKind::Alias | ItemTreeKind::Refined => {
            ast::TypeDecl::cast(syntax)
                .and_then(|declaration| declaration.name())
                .map(|name| name.syntax().text_range())
        }
        ItemTreeKind::Enum => ast::EnumDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Distinct => ast::DistinctDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Resource => ast::ResourceDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Trait => ast::TraitDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Const => ast::ConstDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Effect => ast::EffectDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Interface => ast::InterfaceDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::World => ast::WorldDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Test => ast::TestDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::Property => ast::PropertyDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        _ => None,
    }
    .ok_or(PackageScopesError::MissingName)?;
    let span = Span::new(input.filtered.item_tree.file, range);
    let location = source_location(input, range)?;
    Ok((span, location))
}

fn resolve_entry_syntax(
    entry: &sai_hir::item_tree::ItemTreeEntry,
    input: &FilteredModule<'_>,
) -> Result<SyntaxNode, PackageScopesError> {
    input
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(input.filtered.item_tree.file, input.root))
        .ok_or(PackageScopesError::MissingAstNode)
}

fn source_location(
    input: &FilteredModule<'_>,
    range: TextRange,
) -> Result<Location, PackageScopesError> {
    Location::from_decoded_source(
        input.source_ref.clone(),
        Span::new(input.filtered.item_tree.file, range),
        input.source,
    )
    .ok_or(PackageScopesError::InvalidSourceRange)
}

fn declaration_diagnostic(
    code: Code,
    message: impl Into<String>,
    primary_label: &str,
    primary: Location,
    package_identity: &str,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message, Some(primary));
    diagnostic.primary_label = primary_label.to_owned();
    diagnostic.package = Some(package_identity.to_owned());
    diagnostic
}

fn intern_path(
    package: PackageId,
    module_path: &[DisambiguatedDefPathData],
    component: DefPathData,
    disambiguator: u32,
    packages: &PackageInterner,
    symbols: &Interner,
    def_paths: &DefPathInterner,
) -> Result<LocalDefId, PackageScopesError> {
    let mut components = module_path.to_vec();
    components.push(DisambiguatedDefPathData {
        data: component,
        disambiguator,
    });
    let path = DefPath::new(package, components, packages, symbols)
        .map_err(PackageScopesError::DefPathValidation)?;
    Ok(def_paths.intern(path)?)
}

fn intern_module_def_path(
    package: PackageId,
    module_path: &[Symbol],
    packages: &PackageInterner,
    symbols: &Interner,
    def_paths: &DefPathInterner,
) -> Result<LocalDefId, PackageScopesError> {
    let components = module_path
        .iter()
        .copied()
        .map(|symbol| DisambiguatedDefPathData {
            data: DefPathData::Module(symbol),
            disambiguator: 0,
        })
        .collect();
    let path = DefPath::new(package, components, packages, symbols)
        .map_err(PackageScopesError::DefPathValidation)?;
    Ok(def_paths.intern(path)?)
}

fn next_disambiguator(
    disambiguators: &mut HashMap<DefPathData, u32>,
    component: DefPathData,
) -> Result<u32, PackageScopesError> {
    let disambiguator = disambiguators.entry(component).or_default();
    let current = *disambiguator;
    *disambiguator = disambiguator
        .checked_add(1)
        .ok_or(PackageScopesError::TooManyDefinitions)?;
    Ok(current)
}

fn is_reserved_type_name(name: &str) -> bool {
    matches!(
        name,
        "I8" | "I16"
            | "I32"
            | "I64"
            | "U8"
            | "U16"
            | "U32"
            | "U64"
            | "F32"
            | "F64"
            | "Bool"
            | "Byte"
            | "Char"
            | "ISize"
            | "USize"
            | "Int"
            | "UInt"
            | "Never"
            | "Send"
            | "Share"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleData, ModuleTree};
    use sai_hir::attrs::TargetCfg;
    use sai_hir::item_tree::{ItemTree, TargetFilterResult};
    use sai_intern::PackageInterner;
    use sai_syntax::{Parse, ast};
    use sai_vfs::{SnapshotFile, WorkspaceSnapshot};
    use std::sync::Arc;

    struct Fixture {
        package: PackageId,
        packages: PackageInterner,
        symbols: Interner,
        file: FileId,
        snapshot: WorkspaceSnapshot,
        source: SourceFile,
        parsed: Parse,
        ast_ids: ast::AstIdMap,
        filtered: TargetFilterResult,
        target_cfg: TargetCfg,
    }

    impl Fixture {
        fn new(text: &str, target: &str) -> Self {
            let package_interner = PackageInterner::new();
            let package = package_interner
                .intern("acme:phase1#0.1")
                .expect("package identity interns");
            let file = FileId::new(41);
            let source = SourceFile::new(file, text, false).expect("source is valid");
            let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::new(source.clone()),
                "src/lib.sai",
                None,
                None,
            )])
            .expect("snapshot is valid");
            let parsed = sai_syntax::parse(&snapshot, file).expect("source parses");
            assert!(
                parsed.errors.is_empty(),
                "parse errors: {:?}",
                parsed.errors
            );
            let ast_ids = ast::AstIdMap::new(file, &parsed.root).expect("AST IDs bind");
            let symbols = Interner::new();
            let target_cfg =
                TargetCfg::new(target, std::iter::empty::<&str>()).expect("target is valid");
            let filtered = ItemTree::collect_for_target(
                file,
                text,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg,
            )
            .expect("item tree collects and target-filters");
            Self {
                package,
                packages: package_interner,
                symbols,
                file,
                snapshot,
                source,
                parsed,
                ast_ids,
                filtered,
                target_cfg,
            }
        }

        fn collect(&self) -> (Phase1PackageScopes, DefPathInterner) {
            self.collect_with_context(Phase1BuildContext::default())
        }

        fn collect_with_context(
            &self,
            build_context: Phase1BuildContext,
        ) -> (Phase1PackageScopes, DefPathInterner) {
            let tree = ModuleTree {
                package: self.package,
                modules: vec![ModuleData {
                    path: Vec::new(),
                    file: self.file,
                    parent_dir: None,
                    children: BTreeMap::new(),
                }],
                namespaces: Vec::new(),
                diagnostics: Vec::new(),
            };
            let paths = DefPathInterner::new(self.package, &self.packages, &self.symbols)
                .expect("definition path interner");
            let inputs = BTreeMap::from([(
                super::super::ModuleId::ROOT,
                FilteredModule {
                    filtered: &self.filtered,
                    ast_ids: &self.ast_ids,
                    root: &self.parsed.root,
                    source: &self.source,
                    source_ref: SourceRef {
                        path: "src/lib.sai".to_owned(),
                        package: None,
                        snapshot: None,
                    },
                },
            )]);
            let result = collect_phase1(
                &tree,
                &inputs,
                Phase1CollectionConfig {
                    snapshot: &self.snapshot,
                    target_cfg: &self.target_cfg,
                    build_context,
                },
                &self.packages,
                &self.symbols,
                &paths,
            )
            .expect("Phase 1 collection");
            (result, paths)
        }

        fn collect_with_local_modules(
            &self,
            module_sources: &[(&str, &str)],
        ) -> Phase1PackageScopes {
            struct ExtraInput {
                module_name: Symbol,
                file: FileId,
                path: String,
                source: SourceFile,
                parsed: Parse,
                ast_ids: ast::AstIdMap,
                filtered: TargetFilterResult,
            }

            let mut sources = vec![(self.file, "src/lib.sai".to_owned(), self.source.clone())];
            let mut extras = Vec::new();
            for (index, (module_name, text)) in module_sources.iter().enumerate() {
                let file = FileId::new(42 + index as u32);
                let path = format!("src/{module_name}.sai");
                let source = SourceFile::new(file, *text, false).expect("source is valid");
                let symbol = self
                    .symbols
                    .intern(module_name)
                    .expect("module name interns");
                sources.push((file, path.clone(), source.clone()));
                extras.push((symbol, file, path, source, *text));
            }
            let snapshot = WorkspaceSnapshot::new(sources.iter().map(|(_, path, source)| {
                SnapshotFile::new(Arc::new(source.clone()), path, None, None)
            }))
            .expect("package snapshot is valid");

            let mut extra_inputs = Vec::new();
            for (module_name, file, path, source, text) in extras {
                let parsed = sai_syntax::parse(&snapshot, file).expect("source parses");
                assert!(
                    parsed.errors.is_empty(),
                    "parse errors: {:?}",
                    parsed.errors
                );
                let ast_ids = ast::AstIdMap::new(file, &parsed.root).expect("AST IDs bind");
                let filtered = ItemTree::collect_for_target(
                    file,
                    text,
                    &parsed.root,
                    &ast_ids,
                    &self.symbols,
                    &snapshot,
                    &self.target_cfg,
                )
                .expect("item tree collects and target-filters");
                extra_inputs.push(ExtraInput {
                    module_name,
                    file,
                    path,
                    source,
                    parsed,
                    ast_ids,
                    filtered,
                });
            }

            let mut root_children = BTreeMap::new();
            let mut modules = vec![ModuleData {
                path: Vec::new(),
                file: self.file,
                parent_dir: None,
                children: BTreeMap::new(),
            }];
            for (index, extra) in extra_inputs.iter().enumerate() {
                let id = ModuleId::from_raw((index + 1) as u32);
                let name = self
                    .symbols
                    .resolve(extra.module_name)
                    .expect("module name resolves")
                    .to_string();
                root_children.insert(name, ChildEntry::Module(id));
                modules.push(ModuleData {
                    path: vec![extra.module_name],
                    file: extra.file,
                    parent_dir: Some(ModuleId::ROOT),
                    children: BTreeMap::new(),
                });
            }
            modules[0].children = root_children;
            let tree = ModuleTree {
                package: self.package,
                modules,
                namespaces: Vec::new(),
                diagnostics: Vec::new(),
            };
            let root_input = FilteredModule {
                filtered: &self.filtered,
                ast_ids: &self.ast_ids,
                root: &self.parsed.root,
                source: &self.source,
                source_ref: SourceRef {
                    path: "src/lib.sai".to_owned(),
                    package: None,
                    snapshot: None,
                },
            };
            let mut inputs = BTreeMap::from([(ModuleId::ROOT, root_input)]);
            for (index, extra) in extra_inputs.iter().enumerate() {
                let module_id = ModuleId::from_raw((index + 1) as u32);
                inputs.insert(
                    module_id,
                    FilteredModule {
                        filtered: &extra.filtered,
                        ast_ids: &extra.ast_ids,
                        root: &extra.parsed.root,
                        source: &extra.source,
                        source_ref: SourceRef {
                            path: extra.path.clone(),
                            package: None,
                            snapshot: None,
                        },
                    },
                );
            }
            let paths = DefPathInterner::new(self.package, &self.packages, &self.symbols)
                .expect("definition path interner");
            let mut scopes = collect_phase1(
                &tree,
                &inputs,
                Phase1CollectionConfig {
                    snapshot: &snapshot,
                    target_cfg: &self.target_cfg,
                    build_context: Phase1BuildContext::default(),
                },
                &self.packages,
                &self.symbols,
                &paths,
            )
            .expect("Phase 1 collection");
            scopes.diagnostics.extend(
                crate::use_target_diagnostics::collect_local_use_target_diagnostics(
                    &tree,
                    &inputs,
                    &scopes,
                    &self.packages,
                    &self.symbols,
                )
                .expect("local use targets collect"),
            );
            let local_import_diagnostics = collect_local_module_alias_bindings(
                &tree,
                &inputs,
                &mut scopes,
                &self.packages,
                &self.symbols,
            )
            .expect("local use items collect");
            scopes.diagnostics.extend(local_import_diagnostics);
            scopes
        }
    }

    fn text<'a>(symbols: &'a Interner, symbol: Symbol) -> String {
        symbols.resolve(symbol).unwrap().to_string()
    }

    #[test]
    fn local_item_imports_keep_definition_identity_and_check_alias_conflicts() {
        let root = concat!(
            "use acme:phase1/model.{PublicType as publicType, public_fn as local_fn, private_fn}\n",
            "use acme:phase1/model.{public_fn as local_fn}\n",
            "use acme:phase1/model.{public_fn as clash}\n",
            "fn clash() {}\n",
            "use acme:phase1/model.{public_fn as publicFn}\n",
            "use acme:phase1/model.{public_fn as I32}\n",
            "use acme:phase1/model.{Missing}\n",
        );
        let fixture = Fixture::new(root, "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[(
            "model",
            "pub type PublicType {}\npub fn public_fn() {}\nfn private_fn() {}\n",
        )]);
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let model_scope = &scopes.modules[1];
        let public_fn = fixture.symbols.intern("public_fn").unwrap();
        let local_fn = fixture.symbols.intern("local_fn").unwrap();
        let reserved_alias = fixture.symbols.intern("I32").unwrap();
        let private_fn = fixture.symbols.intern("private_fn").unwrap();
        let public_type = fixture.symbols.intern("publicType").unwrap();

        assert_eq!(
            root_scope.scope.get(&local_fn).map(|binding| binding.res),
            model_scope
                .declared
                .get(&public_fn)
                .map(|binding| binding.res),
            "an item import keeps the original definition identity"
        );
        assert_eq!(
            root_scope.scope.get(&private_fn).map(|binding| binding.res),
            model_scope
                .declared
                .get(&private_fn)
                .map(|binding| binding.res),
            "package-private items are importable within their package"
        );
        assert!(!root_scope.members.contains_key(&local_fn));
        assert!(
            root_scope
                .imports
                .iter()
                .any(|import| { import.name == public_type && import.item_index == Some(0) })
        );
        assert!(root_scope.imports.iter().any(|import| {
            import.name == local_fn
                && import.item_index == Some(1)
                && import.res == model_scope.declared[&public_fn].res
        }));
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::RedundantImport)
        );
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateBinding)
        );
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ReservedTypeName)
        );
        assert!(!root_scope.scope.contains_key(&reserved_alias));
        let case_mismatch = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::AliasCaseMismatch)
            .expect("case-mismatched aliases are diagnosed");
        assert_eq!(
            case_mismatch.primary().unwrap().span.range,
            TextRange::new(
                TextSize::new(root.find("publicType").unwrap() as u32),
                TextSize::new((root.find("publicType").unwrap() + "publicType".len()) as u32),
            )
            .unwrap()
        );
        let missing = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::UnresolvedImport)
            .expect("missing local items are diagnosed");
        assert_eq!(
            missing.primary().unwrap().span.range,
            TextRange::new(
                TextSize::new(root.find("Missing").unwrap() as u32),
                TextSize::new((root.find("Missing").unwrap() + "Missing".len()) as u32),
            )
            .unwrap()
        );
        assert!(matches!(
            missing.data.get("candidates"),
            Some(Value::Array(candidates)) if candidates.is_empty()
        ));
    }

    #[test]
    fn local_pub_use_chains_populate_members_and_preserve_definition_identity() {
        let fixture = Fixture::new("use acme:phase1/bridge.{public_fn}\n", "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[
            ("model", "pub fn public_fn() {}\n"),
            (
                "facade",
                "pub use acme:phase1/model.{public_fn as from_model}\n",
            ),
            (
                "bridge",
                "pub use acme:phase1/facade.{from_model as public_fn}\n",
            ),
        ]);
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let model_scope = &scopes.modules[1];
        let facade_scope = &scopes.modules[2];
        let bridge_scope = &scopes.modules[3];
        let public_fn = fixture.symbols.intern("public_fn").unwrap();
        let from_model = fixture.symbols.intern("from_model").unwrap();
        let definition = model_scope.declared[&public_fn].res;

        for (scope, name) in [(facade_scope, from_model), (bridge_scope, public_fn)] {
            let member = scope.members.get(&name).expect("pub use becomes a member");
            assert_eq!(member.res, definition);
            assert_eq!(member.visibility, Visibility::Pub);
            assert!(matches!(member.origin, BindingOrigin::PubUse { .. }));
        }
        assert_eq!(root_scope.scope[&public_fn].res, definition);
        assert!(matches!(
            root_scope.scope[&public_fn].origin,
            BindingOrigin::Use { .. }
        ));
        assert_eq!(root_scope.scope[&public_fn].visibility, Visibility::Private);
        assert!(!root_scope.members.contains_key(&public_fn));
        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::UnresolvedImport)
        );
    }

    #[test]
    fn failed_reexports_do_not_hide_later_successful_members_or_become_suggestions() {
        let root = concat!(
            "use acme:phase1/facade.{item}\n",
            "use acme:phase1/facade.{deferred}\n",
            "use acme:phase1/facade.{typo}\n",
        );
        let fixture = Fixture::new(root, "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[
            ("model", "pub fn live() {}\n"),
            (
                "facade",
                concat!(
                    "pub use acme:phase1/model.{Missing as item}\n",
                    "pub use acme:phase1/model.{live as item}\n",
                    "pub use foreign:package/remote.{Remote as deferred}\n",
                    "pub use acme:phase1/model.{live as deferred}\n",
                    "pub use acme:phase1/model.{Missing as typo}\n",
                ),
            ),
        ]);
        let item = fixture.symbols.intern("item").unwrap();
        let deferred = fixture.symbols.intern("deferred").unwrap();
        let typo = fixture.symbols.intern("typo").unwrap();
        let live = fixture.symbols.intern("live").unwrap();
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let model_scope = &scopes.modules[1];
        let facade_scope = &scopes.modules[2];

        assert_eq!(
            root_scope.scope.get(&item).map(|binding| binding.res),
            model_scope.declared.get(&live).map(|binding| binding.res),
            "the later successful pub use is part of X(M)"
        );
        assert_eq!(
            facade_scope.members.get(&item).map(|binding| binding.res),
            model_scope.declared.get(&live).map(|binding| binding.res)
        );
        assert_eq!(
            root_scope.scope.get(&deferred).map(|binding| binding.res),
            model_scope.declared.get(&live).map(|binding| binding.res),
            "a deferred foreign candidate cannot hide a known local member"
        );
        assert!(!facade_scope.members.contains_key(&typo));
        let root_typo_import = root_scope
            .imports
            .iter()
            .find(|import| import.name == typo)
            .expect("the root typo import is recorded");
        assert_eq!(root_typo_import.res, Res::Err);
        let root_typo_start = root.find("typo").unwrap() as u32;
        let unresolved_root_typo = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == Code::UnresolvedImport
                    && diagnostic
                        .primary()
                        .is_some_and(|location| location.span.file == fixture.file)
                    && diagnostic.primary().unwrap().span.range.start().raw() == root_typo_start
            })
            .expect("the missing root typo import is diagnosed");
        assert_eq!(
            unresolved_root_typo.data.get("candidates"),
            Some(&Value::Array(Vec::new())),
            "a failed pub use must not be offered as a member candidate"
        );
    }

    #[test]
    fn module_alias_and_item_import_conflicts_follow_source_order() {
        let root = concat!(
            "use acme:phase1/model.{public_fn as shared}\n",
            "use acme:phase1/other as shared\n",
        );
        let fixture = Fixture::new(root, "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[
            ("model", "pub fn public_fn() {}\n"),
            ("other", "pub fn other_fn() {}\n"),
        ]);
        let shared = fixture.symbols.intern("shared").unwrap();
        let public_fn = fixture.symbols.intern("public_fn").unwrap();
        let root_scope = &scopes.modules[ModuleId::ROOT.raw() as usize];
        let model_scope = &scopes.modules[1];
        let duplicate = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .expect("the later alias conflicts with the earlier item import");
        assert_eq!(
            duplicate.primary().unwrap().span.range.start().raw(),
            root.rfind("shared").unwrap() as u32
        );
        assert_eq!(
            root_scope.scope.get(&shared).map(|binding| binding.res),
            model_scope
                .declared
                .get(&public_fn)
                .map(|binding| binding.res),
            "the earlier source binding remains in scope"
        );
        assert_eq!(root_scope.imports[0].res, root_scope.scope[&shared].res);
        assert_eq!(root_scope.imports[1].res, Res::Err);
    }

    #[test]
    fn local_pub_use_members_survive_local_scope_alias_collisions() {
        let fixture = Fixture::new("use acme:phase1/facade.{public_fn}\n", "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[
            ("model", "pub fn public_fn() {}\n"),
            (
                "facade",
                "pub use acme:phase1/model as public_fn.{public_fn}\n",
            ),
        ]);
        let public_fn = fixture.symbols.intern("public_fn").unwrap();
        let facade_scope = &scopes.modules[2];
        let model_scope = &scopes.modules[1];

        assert_eq!(
            facade_scope.members[&public_fn].res,
            model_scope.declared[&public_fn].res
        );
        assert!(matches!(
            facade_scope.scope[&public_fn].res,
            Res::Def(DefKind::Module, _)
        ));
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
                .count(),
            1
        );
        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::UnresolvedImport)
        );
    }

    #[test]
    fn private_reexports_and_reexport_cycles_have_distinct_diagnostics() {
        let private_fixture = Fixture::new("use acme:phase1/facade.{secret}\n", "wasm32-core");
        let private_scopes = private_fixture.collect_with_local_modules(&[
            ("model", "fn secret() {}\n"),
            ("facade", "pub use acme:phase1/model.{secret}\n"),
        ]);
        assert_eq!(
            private_scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::PrivateReexport)
                .count(),
            1
        );
        assert_eq!(
            private_scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::UnresolvedImport)
                .count(),
            1,
            "the failed re-export is absent from its member namespace"
        );

        let cycle_fixture = Fixture::new("use acme:phase1/a.{from_b}\n", "wasm32-core");
        let cycle_scopes = cycle_fixture.collect_with_local_modules(&[
            ("a", "pub use acme:phase1/b.{from_a as from_b}\n"),
            ("b", "pub use acme:phase1/a.{from_b as from_a}\n"),
        ]);
        let cycles = cycle_scopes
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ReexportCycle)
            .collect::<Vec<_>>();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].related().len(), 1);
        assert!(
            cycle_scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::UnresolvedImport)
        );
    }

    #[test]
    fn deferred_foreign_and_interface_item_tables_do_not_become_unresolved() {
        let fixture = Fixture::new(
            concat!(
                "interface codec {}\n",
                "use acme:phase1/codec.{Image}\n",
                "use foreign:package/remote.{Remote}\n",
                "use acme:phase1/facade.{Image}\n",
            ),
            "wasm32-core",
        );
        let scopes = fixture
            .collect_with_local_modules(&[("facade", "pub use acme:phase1/codec.{Image}\n")]);

        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::UnresolvedImport)
        );
        assert!(
            scopes.modules[0]
                .imports
                .iter()
                .all(|import| { import.item_index.is_some() && import.res == Res::Err })
        );
        assert!(
            scopes.modules[1]
                .imports
                .iter()
                .all(|import| { import.item_index.is_some() && import.res == Res::Err })
        );
    }

    #[test]
    fn unresolved_import_candidates_use_distance_then_source_order_and_are_capped() {
        let root = "use acme:phase1/model.{fo}\n";
        let fixture = Fixture::new(root, "wasm32-core");
        let scopes = fixture.collect_with_local_modules(&[(
            "model",
            "pub fn foo() {}\npub fn of() {}\npub fn foa() {}\npub fn fob() {}\n",
        )]);
        let missing = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::UnresolvedImport)
            .expect("typo is unresolved");
        assert_eq!(
            missing.data.get("candidates"),
            Some(&Value::Array(vec![
                Value::String("foo".to_owned()),
                Value::String("of".to_owned()),
                Value::String("foa".to_owned()),
            ]))
        );
        assert!(
            missing
                .children()
                .iter()
                .any(|child| child.message == "similar names: `foo`, `of`, `foa`")
        );
    }

    fn assert_path_already_interned(paths: &DefPathInterner, expected: DefPath) {
        let length = paths.len().unwrap();
        let id = paths.intern(expected.clone()).unwrap();
        assert_eq!(paths.len().unwrap(), length);
        assert_eq!(paths.resolve(id).unwrap(), Some(expected));
    }

    #[test]
    fn interface_name_conflicts_only_with_an_exact_module_file_child() {
        let symbols = Interner::new();
        let codec = symbols.intern("codec").unwrap();
        let parent = symbols.intern("parent").unwrap();
        let nested_module = ModuleData {
            path: vec![parent],
            file: FileId::new(42),
            parent_dir: Some(super::super::ModuleId::ROOT),
            children: BTreeMap::from([(
                "codec".to_owned(),
                ChildEntry::Module(super::super::ModuleId::from_raw(2)),
            )]),
        };
        assert!(interface_conflicts_with_module_file(&nested_module, codec, &symbols).unwrap());

        let module_and_namespace = ModuleData {
            children: BTreeMap::from([(
                "codec".to_owned(),
                ChildEntry::Both(
                    super::super::ModuleId::from_raw(2),
                    super::super::NamespaceId::from_raw(0),
                ),
            )]),
            ..nested_module.clone()
        };
        assert!(
            interface_conflicts_with_module_file(&module_and_namespace, codec, &symbols).unwrap()
        );

        let namespace_only = ModuleData {
            children: BTreeMap::from([(
                "codec".to_owned(),
                ChildEntry::Namespace(super::super::NamespaceId::from_raw(0)),
            )]),
            ..nested_module.clone()
        };
        assert!(!interface_conflicts_with_module_file(&namespace_only, codec, &symbols).unwrap());
        assert!(!interface_conflicts_with_module_file(&nested_module, parent, &symbols).unwrap());
        assert!(
            !interface_conflicts_with_module_file(&nested_module, Symbol::ERROR, &symbols).unwrap()
        );
    }

    #[test]
    fn collects_module_bindings_with_documented_definition_kinds_and_paths() {
        let fixture = Fixture::new(
            concat!(
                "fn run() {}\n",
                "type Record {}\n",
                "type Alias = I32\n",
                "type Refined = I32 where self > 0\n",
                "enum Choice {}\n",
                "distinct Meter = I32\n",
                "resource Handle {}\n",
                "trait Render {}\n",
                "const COUNT: I32 = 1\n",
                "extern \"wasm\" module \"env\" { fn read() }\n",
                "effect logging\n",
                "interface codec {}\n",
                "world app {}\n",
            ),
            "wasm32-core",
        );
        let (scopes, paths) = fixture.collect();
        let declared = &scopes.modules[0].declared;
        let root_module_path = paths
            .resolve(scopes.modules[0].module_def_id)
            .unwrap()
            .unwrap();
        assert!(root_module_path.data().is_empty());
        let expected = [
            ("run", DefKind::Fn),
            ("Record", DefKind::Struct),
            ("Alias", DefKind::Alias),
            ("Refined", DefKind::Refined),
            ("Choice", DefKind::Enum),
            ("Meter", DefKind::Distinct),
            ("Handle", DefKind::Resource),
            ("Render", DefKind::Trait),
            ("COUNT", DefKind::Const),
            ("read", DefKind::ExternFn),
            ("logging", DefKind::Effect),
            ("codec", DefKind::Interface),
            ("app", DefKind::World),
        ];
        assert_eq!(declared.len(), expected.len());
        for (name, kind) in expected {
            let symbol = fixture.symbols.intern(name).unwrap();
            let binding = declared.get(&symbol).expect("binding exists");
            let Res::Def(actual_kind, def_id) = binding.res else {
                panic!("expected definition binding");
            };
            assert_eq!(actual_kind, kind);
            assert_eq!(binding.origin, BindingOrigin::Decl);
            let path = paths.resolve_def_id(def_id).unwrap().unwrap();
            let last = &path.data()[path.data().len() - 1];
            let expected_symbol = fixture.symbols.intern(name).unwrap();
            let matches = match (last.data.clone(), kind) {
                (
                    DefPathData::ValueNs(actual),
                    DefKind::Fn | DefKind::Const | DefKind::ExternFn,
                )
                | (
                    DefPathData::TypeNs(actual),
                    DefKind::Struct
                    | DefKind::Alias
                    | DefKind::Refined
                    | DefKind::Enum
                    | DefKind::Distinct
                    | DefKind::Resource
                    | DefKind::Trait,
                )
                | (DefPathData::Effect(actual), DefKind::Effect)
                | (DefPathData::Interface(actual), DefKind::Interface)
                | (DefPathData::World(actual), DefKind::World) => actual == expected_symbol,
                _ => false,
            };
            assert!(matches, "unexpected DefPath component for `{name}`");
        }
        let read = declared
            .iter()
            .find(|(symbol, _)| text(&fixture.symbols, **symbol) == "read")
            .unwrap()
            .1;
        let Res::Def(_, read_id) = read.res else {
            unreachable!()
        };
        let read_path = paths.resolve_def_id(read_id).unwrap().unwrap();
        assert!(matches!(
            read_path.data().first().map(|entry| &entry.data),
            Some(DefPathData::ExternBlock {
                abi,
                module: Some(module)
            }) if text(&fixture.symbols, *abi) == "wasm"
                && text(&fixture.symbols, *module) == "env"
        ));
    }

    #[test]
    fn interned_module_definition_paths_follow_canonical_tree_paths() {
        let fixture = Fixture::new("", "wasm32-core");
        let ui = fixture.symbols.intern("ui").unwrap();
        let components = fixture.symbols.intern("components").unwrap();
        let paths =
            DefPathInterner::new(fixture.package, &fixture.packages, &fixture.symbols).unwrap();
        let module = intern_module_def_path(
            fixture.package,
            &[ui, components],
            &fixture.packages,
            &fixture.symbols,
            &paths,
        )
        .unwrap();
        let path = paths.resolve(module).unwrap().unwrap();
        assert_eq!(
            path.data(),
            &[
                DisambiguatedDefPathData {
                    data: DefPathData::Module(ui),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::Module(components),
                    disambiguator: 0,
                },
            ]
        );
    }

    #[test]
    fn collects_export_names_and_reports_duplicates_at_the_later_name() {
        let source = concat!(
            "export fn first() {}\n",
            "@export_name(\"first\")\n",
            "export fn second() {}\n",
            "@export_name(\"first\")\n",
            "export fn third() {}\n",
        );
        let fixture = Fixture::new(source, "wasm32-core");
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });

        assert_eq!(scopes.exports.len(), 1);
        assert!(scopes.exports.contains_key("first"));
        let duplicates = scopes
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateExportName)
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);

        let first_name_offset =
            sai_vfs::TextSize::new(u32::try_from(source.find("first()").unwrap()).unwrap());
        let duplicate_offsets = [
            source.find("\"first\"").unwrap(),
            source.rfind("\"first\"").unwrap(),
        ];
        for (duplicate, expected_offset) in duplicates.iter().zip(duplicate_offsets) {
            let primary = duplicate.primary().expect("duplicate has a primary");
            let expected_offset = u32::try_from(expected_offset).unwrap();
            assert_eq!(
                primary.span.range.start(),
                sai_vfs::TextSize::new(expected_offset)
            );
            assert_eq!(
                primary.span.range.end(),
                sai_vfs::TextSize::new(expected_offset + "\"first\"".len() as u32)
            );
            assert!(matches!(
                duplicate.related().first(),
                Some(Related::DeclaredHere { location, what })
                    if what == "first export with this name"
                        && location.span.range.start() == first_name_offset
            ));
        }
        assert!(
            scopes.diagnostics.iter().all(|diagnostic| {
                diagnostic.code != Code::FeatureNotImplemented
                    || !matches!(
                        diagnostic.data.get("feature"),
                        Some(Value::String(feature)) if feature == "attribute @export_name"
                    )
            }),
            "valid @export_name must not keep the Stage 0 feature-gate diagnostic"
        );
    }

    #[test]
    fn reserves_memory_and_compares_explicit_names_by_utf8_bytes() {
        let source = concat!(
            "export fn memory() {}\n",
            "@export_name(\"memory\") export fn explicit_memory() {}\n",
            "@export_name(\"é\") export fn precomposed() {}\n",
            "@export_name(\"e\\u{301}\") export fn decomposed() {}\n",
            "export fn r#type() {}\n",
        );
        let fixture = Fixture::new(source, "wasm32-core");
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });
        assert!(scopes.exports.contains_key("memory"));
        assert!(scopes.exports.contains_key("é"));
        assert!(scopes.exports.contains_key("e\u{301}"));
        assert!(scopes.exports.contains_key("type"));
        assert!(!scopes.exports.contains_key("r#type"));
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::DuplicateExportName)
                .count(),
            2
        );
        for diagnostic in scopes
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateExportName)
        {
            assert!(
                diagnostic
                    .children()
                    .iter()
                    .any(|child| child.kind == ChildKind::Note
                        && child.message.contains("linear memory")),
                "reserved-memory diagnostic should explain the reserved name"
            );
        }
    }

    #[test]
    fn invalid_export_name_falls_back_to_the_normalized_function_name() {
        let source = "@export_name(\"\")\nexport fn same() {}\nexport fn same() {}\n";
        let fixture = Fixture::new(source, "wasm32-core");
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });

        assert!(scopes.exports.contains_key("same"));
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidAttributeArgument)
        );
        let duplicate = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::DuplicateExportName)
            .expect("the later default name collides");
        assert_eq!(
            duplicate.primary().unwrap().span.range.start(),
            sai_vfs::TextSize::new(u32::try_from(source.rfind("same()").unwrap()).unwrap())
        );
    }

    #[test]
    fn export_name_reports_invalid_targets_instead_of_the_stage_gate() {
        let source = concat!(
            "@export_name(\"wire\")\n",
            "pub fn ordinary() {}\n",
            "@export_name(\"record-wire\")\n",
            "type Data {}\n",
        );
        let fixture = Fixture::new(source, "wasm32-core");
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });

        assert!(scopes.exports.is_empty());
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            2
        );
        assert!(
            scopes.diagnostics.iter().all(|diagnostic| {
                diagnostic.code != Code::FeatureNotImplemented
                    || !matches!(
                        diagnostic.data.get("feature"),
                        Some(Value::String(feature)) if feature == "attribute @export_name"
                    )
            }),
            "Stage 1 target validation should replace the Stage 0 gate"
        );
    }

    #[test]
    fn test_build_validates_export_names_without_building_package_exports() {
        let fixture = Fixture::new(
            concat!(
                "@export_name(\"wire\")\n",
                "export fn first() {}\n",
                "@export_name(\"wire\")\n",
                "export fn second() {}\n",
                "@export_name(\"invalid-target\")\n",
                "fn ordinary() {}\n",
            ),
            "wasm32-wasi",
        );
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: false,
            is_test_build: true,
        });

        assert!(scopes.exports.is_empty());
        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::DuplicateExportName)
        );
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            1
        );
        assert!(scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Code::FeatureNotImplemented
                || !matches!(
                    diagnostic.data.get("feature"),
                    Some(Value::String(feature)) if feature == "attribute @export_name"
                )
        }));
    }

    #[test]
    fn export_name_targets_are_validated_without_core_uniqueness_context() {
        let fixture = Fixture::new(
            concat!(
                "@export_name(\"wire\") export fn first() {}\n",
                "@export_name(\"wire\") export fn second() {}\n",
                "@export_name(\"invalid-target\") fn ordinary() {}\n",
            ),
            "wasm32-wasi",
        );
        let (scopes, _) = fixture.collect();

        assert!(scopes.exports.is_empty());
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            1
        );
        assert!(
            scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::DuplicateExportName)
        );
        assert!(scopes.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Code::FeatureNotImplemented
                || !matches!(
                    diagnostic.data.get("feature"),
                    Some(Value::String(feature)) if feature == "attribute @export_name"
                )
        }));
    }

    #[test]
    fn export_name_collection_uses_target_filtered_items_and_explicit_build_context() {
        let fixture = Fixture::new(
            concat!(
                "@target(\"wasm64-core\")\n",
                "export fn shared() {}\n",
                "export fn shared() {}\n",
            ),
            "wasm32-core",
        );
        let (core_scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });
        assert_eq!(core_scopes.exports.len(), 1);
        assert!(core_scopes.exports.contains_key("shared"));
        assert!(
            core_scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::DuplicateExportName)
        );

        let both_survive = Fixture::new(
            "export fn shared() {}\nexport fn shared() {}\n",
            "wasm32-core",
        );
        let (non_core_scopes, _) = both_survive.collect();
        assert!(non_core_scopes.exports.is_empty());
        assert!(
            non_core_scopes
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::DuplicateExportName)
        );
    }

    #[test]
    fn export_duplicates_follow_canonical_module_path_before_source_offset() {
        let fixture = Fixture::new("export fn alpha() {}\nexport fn beta() {}\n", "wasm32-core");
        let (scopes, _) = fixture.collect_with_context(Phase1BuildContext {
            is_root_core_module_build: true,
            ..Phase1BuildContext::default()
        });
        let def_id = *scopes
            .exports
            .get("alpha")
            .expect("first exported definition");
        let input = FilteredModule {
            filtered: &fixture.filtered,
            ast_ids: &fixture.ast_ids,
            root: &fixture.parsed.root,
            source: &fixture.source,
            source_ref: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
        };
        let later_offset = u32::try_from(fixture.source.text().find("beta()").unwrap()).unwrap();
        let earlier_offset = u32::try_from(fixture.source.text().find("alpha()").unwrap()).unwrap();
        let candidate_with_path = |module_path: &str, offset: u32| {
            let range = TextRange::new(
                sai_vfs::TextSize::new(offset),
                sai_vfs::TextSize::new(offset + 5),
            )
            .expect("candidate span is valid");
            ExportCandidate {
                module_path: module_path.to_owned(),
                source_offset: offset,
                name: "shared".to_owned(),
                def_id,
                location: source_location(&input, range).expect("candidate location"),
            }
        };
        let mut diagnostics = Vec::new();
        collect_exports(
            vec![
                candidate_with_path("z", earlier_offset),
                candidate_with_path("a", later_offset),
            ],
            &fixture.snapshot,
            "acme:phase1",
            &mut diagnostics,
        )
        .expect("collect duplicate exports");

        assert_eq!(diagnostics.len(), 1);
        let primary = diagnostics[0].primary().expect("duplicate primary");
        assert_eq!(
            primary.span.range.start(),
            sai_vfs::TextSize::new(earlier_offset),
            "the later canonical module path is diagnosed despite its earlier source offset"
        );
        assert!(matches!(
            diagnostics[0].related().first(),
            Some(Related::DeclaredHere { location, .. })
                if location.span.range.start() == sai_vfs::TextSize::new(later_offset)
        ));
    }

    #[test]
    fn duplicate_binding_keeps_the_first_and_reports_both_name_locations() {
        let fixture = Fixture::new("fn same() {}\nfn same() {}\n", "wasm32-core");
        let (scopes, paths) = fixture.collect();
        let same = fixture.symbols.intern("same").unwrap();
        let binding = scopes.modules[0].declared.get(&same).unwrap();
        let Res::Def(_, first_id) = binding.res else {
            unreachable!()
        };
        let first_path = paths.resolve_def_id(first_id).unwrap().unwrap();
        assert_eq!(first_path.data().last().unwrap().disambiguator, 0);
        let duplicate = scopes
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .expect("duplicate diagnostic");
        assert_eq!(duplicate.primary().unwrap().span.range.start().raw(), 16);
        assert!(matches!(
            duplicate.related().first(),
            Some(Related::DeclaredHere { location, .. })
                if location.span.range.start().raw() == 3
        ));
    }

    #[test]
    fn target_filtering_removes_duplicate_before_phase_one_binding() {
        let fixture = Fixture::new(
            "@target(\"wasm64-core\")\nfn choose() {}\nfn choose() {}\n",
            "wasm32-core",
        );
        let (scopes, paths) = fixture.collect();
        let choose = fixture.symbols.intern("choose").unwrap();
        let binding = scopes.modules[0]
            .declared
            .get(&choose)
            .expect("surviving declaration is bound");
        let Res::Def(_, def_id) = binding.res else {
            unreachable!()
        };
        assert_eq!(
            paths
                .resolve_def_id(def_id)
                .unwrap()
                .unwrap()
                .data()
                .last()
                .unwrap()
                .disambiguator,
            0
        );
        assert!(
            !scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateBinding),
            "{:?}",
            scopes.diagnostics
        );
    }

    #[test]
    fn invalid_extern_function_target_is_kept_before_duplicate_checks() {
        let fixture = Fixture::new(
            concat!(
                "extern \"wasm\" module \"env\" {\n",
                "  @target(\"wasm64-core\")\n",
                "  fn read()\n",
                "  fn read()\n",
                "}\n",
            ),
            "wasm32-core",
        );
        let (scopes, paths) = fixture.collect();
        let read = fixture.symbols.intern("read").unwrap();
        let binding = scopes.modules[0]
            .declared
            .get(&read)
            .expect("surviving extern function is bound");
        let Res::Def(DefKind::ExternFn, def_id) = binding.res else {
            panic!("expected extern function binding");
        };
        let path = paths.resolve_def_id(def_id).unwrap().unwrap();
        assert_eq!(path.data().last().unwrap().disambiguator, 0);
        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateBinding)
        );
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidAttributeTarget, Code::DuplicateBinding]
        );
    }

    #[test]
    fn diagnosed_extern_visibility_marker_does_not_make_the_binding_public() {
        let text = "extern \"wasm\" module \"env\" { pub fn read() }\n";
        let packages = PackageInterner::new();
        let package = packages.intern("acme:phase1").unwrap();
        let file = FileId::new(41);
        let source = SourceFile::new(file, text, false).unwrap();
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(source.clone()),
            "src/lib.sai",
            None,
            None,
        )])
        .unwrap();
        let parsed = sai_syntax::parse(&snapshot, file).unwrap();
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].code, Code::ExpectedToken);
        let ast_ids = ast::AstIdMap::new(file, &parsed.root).unwrap();
        let symbols = Interner::new();
        let target_cfg = TargetCfg::new("wasm32-core", std::iter::empty::<&str>()).unwrap();
        let filtered = ItemTree::collect_for_target(
            file,
            text,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg,
        )
        .unwrap();
        let mut fixture = Fixture {
            package,
            packages,
            symbols,
            file,
            snapshot,
            source,
            parsed,
            ast_ids,
            filtered,
            target_cfg,
        };
        let read = fixture.symbols.intern("read").unwrap();
        assert_eq!(
            fixture.filtered.item_tree.items[0].children[0].vis,
            Visibility::Private
        );
        // Exercise the collector's effective-visibility rule even if a
        // retained or future ItemTree stores the diagnosed marker as `Pub`.
        fixture.filtered.item_tree.items[0].children[0].vis = Visibility::Pub;
        let (scopes, _) = fixture.collect();
        assert_eq!(
            scopes.modules[0].declared.get(&read).unwrap().visibility,
            Visibility::Private
        );
    }

    #[test]
    fn diagnostics_are_sorted_by_source_path_then_offset() {
        struct ModuleInput {
            source: SourceFile,
            parsed: Parse,
            ast_ids: ast::AstIdMap,
            filtered: TargetFilterResult,
            path: String,
        }

        let specs = [
            (FileId::new(61), "src/lib.sai", "fn root() {}\n"),
            (
                FileId::new(62),
                "src/a/b.sai",
                "fn nested() {}\nfn nested() {}\nexport fn shared() {}\n",
            ),
            (
                FileId::new(63),
                "src/a.sai",
                "fn sibling() {}\nfn sibling() {}\nexport fn shared() {}\n",
            ),
        ];
        let sources = specs
            .iter()
            .map(|(file, _, text)| SourceFile::new(*file, *text, false).expect("source is valid"))
            .collect::<Vec<_>>();
        let snapshot =
            WorkspaceSnapshot::new(specs.iter().zip(&sources).map(|((_, path, _), source)| {
                SnapshotFile::new(Arc::new(source.clone()), *path, None, None)
            }))
            .expect("snapshot is valid");
        let packages = PackageInterner::new();
        let package = packages.intern("acme:phase1").unwrap();
        let symbols = Interner::new();
        let module_a = symbols.intern("a").unwrap();
        let module_b = symbols.intern("b").unwrap();
        let target_cfg = TargetCfg::new("wasm32-core", std::iter::empty::<&str>()).unwrap();
        let modules = specs
            .iter()
            .zip(&sources)
            .map(|((file, path, text), source)| {
                let parsed = sai_syntax::parse(&snapshot, *file).expect("source parses");
                assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
                let ast_ids = ast::AstIdMap::new(*file, &parsed.root).unwrap();
                let filtered = ItemTree::collect_for_target(
                    *file,
                    text,
                    &parsed.root,
                    &ast_ids,
                    &symbols,
                    &snapshot,
                    &target_cfg,
                )
                .expect("item tree collects");
                ModuleInput {
                    source: source.clone(),
                    parsed,
                    ast_ids,
                    filtered,
                    path: (*path).to_owned(),
                }
            })
            .collect::<Vec<_>>();
        let module_tree = ModuleTree {
            package,
            modules: vec![
                ModuleData {
                    path: Vec::new(),
                    file: specs[0].0,
                    parent_dir: None,
                    children: BTreeMap::new(),
                },
                // File traversal can visit the directory module before its
                // sibling source file, while diagnostics use path byte order.
                ModuleData {
                    path: vec![module_a, module_b],
                    file: specs[1].0,
                    parent_dir: Some(super::super::ModuleId::ROOT),
                    children: BTreeMap::new(),
                },
                ModuleData {
                    path: vec![module_a],
                    file: specs[2].0,
                    parent_dir: Some(super::super::ModuleId::ROOT),
                    children: BTreeMap::new(),
                },
            ],
            namespaces: Vec::new(),
            diagnostics: Vec::new(),
        };
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let inputs = modules
            .iter()
            .enumerate()
            .map(|(index, module)| {
                (
                    super::super::ModuleId::from_raw(index as u32),
                    FilteredModule {
                        filtered: &module.filtered,
                        ast_ids: &module.ast_ids,
                        root: &module.parsed.root,
                        source: &module.source,
                        source_ref: SourceRef {
                            path: module.path.clone(),
                            package: None,
                            snapshot: None,
                        },
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let scopes = collect_phase1(
            &module_tree,
            &inputs,
            Phase1CollectionConfig {
                snapshot: &snapshot,
                target_cfg: &target_cfg,
                build_context: Phase1BuildContext {
                    is_root_core_module_build: true,
                    ..Phase1BuildContext::default()
                },
            },
            &packages,
            &symbols,
            &paths,
        )
        .unwrap();
        let duplicates = scopes
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .collect::<Vec<_>>();
        assert_eq!(duplicates.len(), 2);
        assert_eq!(duplicates[0].primary().unwrap().file.path, "src/a.sai");
        assert_eq!(duplicates[1].primary().unwrap().file.path, "src/a/b.sai");
        let export_duplicates = scopes
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateExportName)
            .collect::<Vec<_>>();
        assert_eq!(export_duplicates.len(), 1);
        assert_eq!(
            export_duplicates[0].primary().unwrap().file.path,
            "src/a/b.sai"
        );
        assert!(matches!(
            export_duplicates[0].related().first(),
            Some(Related::DeclaredHere { location, .. })
                if location.file.path == "src/a.sai"
        ));
    }

    #[test]
    fn reserved_type_names_and_error_symbols_are_not_bound() {
        let fixture = Fixture::new("type I32 {}\ntype Send {}\nfn valid() {}\n", "wasm32-core");
        let (scopes, _) = fixture.collect();
        let declared = &scopes.modules[0].declared;
        assert!(!declared.contains_key(&fixture.symbols.intern("I32").unwrap()));
        assert!(!declared.contains_key(&fixture.symbols.intern("Send").unwrap()));
        assert!(declared.contains_key(&fixture.symbols.intern("valid").unwrap()));
        assert_eq!(
            scopes
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ReservedTypeName)
                .count(),
            2
        );
    }

    #[test]
    fn validates_test_names_and_detects_duplicates_across_tests_and_properties() {
        let fixture = Fixture::new(
            concat!(
                "test \"\" {}\n",
                "test \"bad\\u{7f}\" {}\n",
                "test \"shared\" {}\n",
                "property \"shared\"() {}\n",
            ),
            "wasm32-core",
        );
        let (scopes, _) = fixture.collect();
        let codes = scopes
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            [
                Code::InvalidTestName,
                Code::InvalidTestName,
                Code::DuplicateTestName
            ]
        );
        assert!(matches!(
            scopes.diagnostics[2].related().first(),
            Some(Related::DeclaredHere { .. })
        ));
    }

    #[test]
    fn error_named_declarations_are_collected_without_a_binding() {
        let mut fixture = Fixture::new(
            "fn first() {}\nfn second() {}\nfn good() {}\n",
            "wasm32-core",
        );
        fixture.filtered.item_tree.items[0].name = Symbol::ERROR;
        fixture.filtered.item_tree.items[1].name = Symbol::ERROR;
        let (scopes, paths) = fixture.collect();
        assert_eq!(scopes.modules[0].declared.len(), 1);
        assert_eq!(paths.len().unwrap(), 4);
        assert!(
            scopes.modules[0]
                .declared
                .contains_key(&fixture.symbols.intern("good").unwrap())
        );
        assert!(!scopes.modules[0].declared.contains_key(&Symbol::ERROR));

        for disambiguator in 0..=1 {
            let missing_name_path = DefPath::new(
                fixture.package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::ValueNs(Symbol::ERROR),
                    disambiguator,
                }],
                &fixture.packages,
                &fixture.symbols,
            )
            .unwrap();
            assert_path_already_interned(&paths, missing_name_path);
        }
        let good_path = DefPath::new(
            fixture.package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(fixture.symbols.intern("good").unwrap()),
                disambiguator: 0,
            }],
            &fixture.packages,
            &fixture.symbols,
        )
        .unwrap();
        assert_path_already_interned(&paths, good_path);
    }

    #[test]
    fn error_named_extern_functions_keep_def_paths_but_do_not_bind() {
        let mut fixture = Fixture::new(
            "extern \"wasm\" module \"env\" {\nfn first()\nfn second()\n}\n",
            "wasm32-core",
        );
        for child in &mut fixture.filtered.item_tree.items[0].children {
            child.name = Symbol::ERROR;
        }
        let (scopes, paths) = fixture.collect();
        assert!(scopes.modules[0].declared.is_empty());
        assert_eq!(paths.len().unwrap(), 4);

        let block_data = DefPathData::ExternBlock {
            abi: fixture.symbols.intern("wasm").unwrap(),
            module: Some(fixture.symbols.intern("env").unwrap()),
        };
        let block_component = DisambiguatedDefPathData {
            data: block_data,
            disambiguator: 0,
        };
        let block_path = DefPath::new(
            fixture.package,
            vec![block_component.clone()],
            &fixture.packages,
            &fixture.symbols,
        )
        .unwrap();
        assert_path_already_interned(&paths, block_path);
        for disambiguator in 0..=1 {
            let child_path = DefPath::new(
                fixture.package,
                vec![
                    block_component.clone(),
                    DisambiguatedDefPathData {
                        data: DefPathData::ValueNs(Symbol::ERROR),
                        disambiguator,
                    },
                ],
                &fixture.packages,
                &fixture.symbols,
            )
            .unwrap();
            assert_path_already_interned(&paths, child_path);
        }
    }

    #[test]
    fn error_symbol_test_and_property_entries_keep_recoverable_literal_paths() {
        let mut fixture = Fixture::new(
            "test \"recoverable test\" {}\nproperty \"recoverable property\"() {}\n",
            "wasm32-core",
        );
        for entry in &mut fixture.filtered.item_tree.items {
            entry.name = Symbol::ERROR;
        }
        let (scopes, paths) = fixture.collect();
        assert!(scopes.modules[0].declared.is_empty());
        assert_eq!(paths.len().unwrap(), 3);
        for component in [
            DefPathData::Test(Arc::from("recoverable test")),
            DefPathData::Property(Arc::from("recoverable property")),
        ] {
            let path = DefPath::new(
                fixture.package,
                vec![DisambiguatedDefPathData {
                    data: component,
                    disambiguator: 0,
                }],
                &fixture.packages,
                &fixture.symbols,
            )
            .unwrap();
            assert_path_already_interned(&paths, path);
        }
    }

    #[test]
    fn test_and_property_without_a_string_token_get_no_fabricated_def_path() {
        let text = "test {}\n";
        let packages = PackageInterner::new();
        let package = packages.intern("acme:phase1").unwrap();
        let file = FileId::new(41);
        let source = SourceFile::new(file, text, false).unwrap();
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(source.clone()),
            "src/lib.sai",
            None,
            None,
        )])
        .unwrap();
        let parsed = sai_syntax::parse(&snapshot, file).unwrap();
        let ast_ids = ast::AstIdMap::new(file, &parsed.root).unwrap();
        let symbols = Interner::new();
        let target_cfg = TargetCfg::new("wasm32-core", std::iter::empty::<&str>()).unwrap();
        let filtered = ItemTree::collect_for_target(
            file,
            text,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg,
        )
        .unwrap();
        let fixture = Fixture {
            package,
            packages,
            symbols,
            file,
            snapshot,
            source,
            parsed,
            ast_ids,
            filtered,
            target_cfg,
        };
        let (scopes, paths) = fixture.collect();
        assert!(scopes.modules[0].declared.is_empty());
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn rejects_source_text_stale_against_the_ast_snapshot() {
        let fixture = Fixture::new("fn valid() {}\n", "wasm32-core");
        let stale_source =
            SourceFile::new(fixture.file, "fn shifted() {}\n", false).expect("source is valid");
        let tree = ModuleTree {
            package: fixture.package,
            modules: vec![ModuleData {
                path: Vec::new(),
                file: fixture.file,
                parent_dir: None,
                children: BTreeMap::new(),
            }],
            namespaces: Vec::new(),
            diagnostics: Vec::new(),
        };
        let paths = DefPathInterner::new(fixture.package, &fixture.packages, &fixture.symbols)
            .expect("definition path interner");
        let inputs = BTreeMap::from([(
            super::super::ModuleId::ROOT,
            FilteredModule {
                filtered: &fixture.filtered,
                ast_ids: &fixture.ast_ids,
                root: &fixture.parsed.root,
                source: &stale_source,
                source_ref: SourceRef {
                    path: "src/lib.sai".to_owned(),
                    package: None,
                    snapshot: None,
                },
            },
        )]);
        assert!(matches!(
            collect_phase1(
                &tree,
                &inputs,
                Phase1CollectionConfig {
                    snapshot: &fixture.snapshot,
                    target_cfg: &fixture.target_cfg,
                    build_context: Phase1BuildContext::default(),
                },
                &fixture.packages,
                &fixture.symbols,
                &paths
            ),
            Err(PackageScopesError::InvalidModuleInput(
                super::super::ModuleId::ROOT
            ))
        ));
    }

    #[test]
    fn rejects_a_def_path_interner_for_another_package_even_when_empty() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:phase1").unwrap();
        let foreign_package = packages.intern("acme:other").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(foreign_package, &packages, &symbols).unwrap();
        let tree = ModuleTree {
            package,
            modules: Vec::new(),
            namespaces: Vec::new(),
            diagnostics: Vec::new(),
        };
        let inputs = BTreeMap::new();
        let target_cfg = TargetCfg::new("wasm32-core", std::iter::empty::<&str>()).unwrap();
        let source = Arc::new(SourceFile::new(FileId::new(0), "", false).unwrap());
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, None)]).unwrap();
        assert!(matches!(
            collect_phase1(
                &tree,
                &inputs,
                Phase1CollectionConfig {
                    snapshot: &snapshot,
                    target_cfg: &target_cfg,
                    build_context: Phase1BuildContext::default(),
                },
                &packages,
                &symbols,
                &paths
            ),
            Err(PackageScopesError::PackageMismatch)
        ));
    }
}
