//! Phase 1 of package scope construction: collect module-level declarations.
//!
//! Callers provide item trees that have already passed `@target` filtering.
//! This module does not resolve `use` declarations or compute the import
//! fixpoint. Extern-function children are checked against their validated
//! target attributes here because they are represented as children of an
//! extern block in the item tree.

use super::{ChildEntry, ModuleData};
use sai_diagnostics::{Code, Diagnostic, Location, Related, Severity, SourceRef};
use sai_hir::attrs::TargetCfg;
use sai_hir::item_tree::{ItemTreeKind, TargetFilterResult};
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
    /// Target-attribute and Phase-1 declaration diagnostics, sorted by
    /// source-relative file path and byte offset. Module-tree diagnostics
    /// remain owned by `module_tree`.
    pub diagnostics: Vec<Diagnostic>,
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
    target_cfg: &TargetCfg,
    packages: &PackageInterner,
    symbols: &Interner,
    def_paths: &DefPathInterner,
) -> Result<Phase1PackageScopes, PackageScopesError> {
    if module_tree.modules.len() > u32::MAX as usize {
        return Err(PackageScopesError::TooManyModules);
    }
    if def_paths.package() != module_tree.package {
        return Err(PackageScopesError::PackageMismatch);
    }

    let package_identity = packages.resolve(module_tree.package)?.to_string();
    let mut scopes = Vec::with_capacity(module_tree.modules.len());
    let mut diagnostics = Vec::new();

    for (raw_module_id, module) in module_tree.modules.iter().enumerate() {
        let module_id = super::ModuleId::from_raw(
            u32::try_from(raw_module_id).map_err(|_| PackageScopesError::TooManyModules)?,
        );
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;
        validate_module_input(module_id, module.file, input)?;

        let mut module_diagnostics = input.filtered.diagnostics.clone();
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

        for entry in &input.filtered.item_tree.items {
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

fn validate_module_input(
    module_id: super::ModuleId,
    file: FileId,
    input: &FilteredModule<'_>,
) -> Result<(), PackageScopesError> {
    let item_tree = &input.filtered.item_tree;
    if item_tree.file != file
        || !item_tree.matches_ast_ids(input.ast_ids)
        || !input.ast_ids.matches_root(file, input.root)
        || input.source.id() != file
        || !syntax_matches_source(input.root.green(), input.source.text())
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
                source,
                parsed,
                ast_ids,
                filtered,
                target_cfg,
            }
        }

        fn collect(&self) -> (Phase1PackageScopes, DefPathInterner) {
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
                        package: Some("acme:phase1".to_owned()),
                        snapshot: None,
                    },
                },
            )]);
            let result = collect_phase1(
                &tree,
                &inputs,
                &self.target_cfg,
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
                "fn nested() {}\nfn nested() {}\n",
            ),
            (
                FileId::new(63),
                "src/a.sai",
                "fn sibling() {}\nfn sibling() {}\n",
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
                            package: Some("acme:phase1".to_owned()),
                            snapshot: None,
                        },
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let scopes = collect_phase1(
            &module_tree,
            &inputs,
            &target_cfg,
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
                    package: Some("acme:phase1".to_owned()),
                    snapshot: None,
                },
            },
        )]);
        assert!(matches!(
            collect_phase1(
                &tree,
                &inputs,
                &fixture.target_cfg,
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
        assert!(matches!(
            collect_phase1(&tree, &inputs, &target_cfg, &packages, &symbols, &paths),
            Err(PackageScopesError::PackageMismatch)
        ));
    }
}
