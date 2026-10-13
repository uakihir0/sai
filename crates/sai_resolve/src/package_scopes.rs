//! Phase 1 of package scope construction: collect module-level declarations.
//!
//! Callers provide item trees that have already passed `@target` filtering.
//! This module does not resolve `use` declarations or compute the import
//! fixpoint. Extern-function children are checked against their validated
//! target attributes here because they are represented as children of an
//! extern block in the item tree.

use super::{ChildEntry, ModuleData};
use sai_diagnostics::{
    Child, ChildKind, Code, Diagnostic, Location, Related, Severity, SourceRef, Value,
};
use sai_hir::attrs::{
    AttributeTarget, TargetCfg, TargetKind, ValidatedExportName, validate_export_name,
};
use sai_hir::item_tree::{AttrName, ItemTreeKind, TargetFilterResult};
use sai_hir::{
    DefId, DefPath, DefPathData, DefPathError, DefPathInterner, DefPathInternerError,
    DisambiguatedDefPathData, LocalDefId, PackageId, Symbol, Visibility,
};
use sai_intern::{InternError, Interner, LookupError, PackageIdError, PackageInterner};
use sai_syntax::{
    GreenElement, GreenNode, SyntaxNode,
    ast::{self, AstNode, AstToken},
};
use sai_vfs::{FileId, SourceFile, Span, TextRange};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The entity kind stored by a declaration binding.
///
/// The variants match the name-resolution model in impl/03 §3.4. The
/// Phase-1 collector currently constructs only module-level item kinds.
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
}

/// A resolved entity used in module namespace bindings.
///
/// The later resolver phases extend this with imports, locals, and primitive
/// types. Phase 1 only creates definition bindings.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Res {
    Def(DefKind, DefId),
    Err,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingOrigin {
    Decl,
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Phase1ModuleScopes {
    /// Direct lookup by session-local symbol. This map is not traversed to
    /// generate output; all diagnostic ordering comes from the source walk.
    pub declared: HashMap<Symbol, Binding>,
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
    DefPathValidation(DefPathError),
    Interner(InternError),
    Symbol(LookupError),
    DefPath(DefPathInternerError),
    TooManyDefinitions,
}

impl From<PackageIdError> for PackageScopesError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
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
        let mut module_disambiguators = HashMap::<DefPathData, u32>::new();
        let mut declared_locations = HashMap::<Symbol, Location>::new();
        let mut module_scopes = Phase1ModuleScopes::default();
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
                .intern("acme:phase1")
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
    }

    fn text<'a>(symbols: &'a Interner, symbol: Symbol) -> String {
        symbols.resolve(symbol).unwrap().to_string()
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
        assert_eq!(paths.len().unwrap(), 3);
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
        assert_eq!(paths.len().unwrap(), 3);

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
        assert_eq!(paths.len().unwrap(), 2);
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
        assert_eq!(paths.len().unwrap(), 0);
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
