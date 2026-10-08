use crate::attrs::{
    AttrValidationError, AttributeTarget, TargetCfg, TargetKind, ValidatedAttrs, validate_attrs,
};
use sai_diagnostics::Diagnostic;
use sai_intern::{InternError, Interner, Symbol};
use sai_stable_hash::StableHasher;
use sai_syntax::{
    GreenElement, SyntaxElement, SyntaxKind, SyntaxNode,
    ast::{self, AstNode, AstToken},
};
use sai_vfs::{FileId, Span, TextRange, WorkspaceSnapshot, WorkspaceSnapshotId};
use std::collections::BTreeMap;

pub use sai_syntax::ast::ErasedAstId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AttrName {
    Derive,
    ExportName,
    Inline,
    ImportName,
    Cold,
    Repr,
    MustUse,
    Flags,
    Deprecated,
    Target,
    Allow,
    Cases,
    Warn,
    Seed,
    Deny,
    Doc,
    PanicHook,
}

impl AttrName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Derive => "derive",
            Self::ExportName => "export_name",
            Self::Inline => "inline",
            Self::ImportName => "import_name",
            Self::Cold => "cold",
            Self::Repr => "repr",
            Self::MustUse => "must_use",
            Self::Flags => "flags",
            Self::Deprecated => "deprecated",
            Self::Target => "target",
            Self::Allow => "allow",
            Self::Cases => "cases",
            Self::Warn => "warn",
            Self::Seed => "seed",
            Self::Deny => "deny",
            Self::Doc => "doc",
            Self::PanicHook => "panic_hook",
        }
    }

    pub fn from_source_name(name: &str) -> Option<Self> {
        if !sai_syntax::ATTRIBUTE_NAMES.contains(&name) {
            return None;
        }
        Some(match name {
            "derive" => Self::Derive,
            "export_name" => Self::ExportName,
            "inline" => Self::Inline,
            "import_name" => Self::ImportName,
            "cold" => Self::Cold,
            "repr" => Self::Repr,
            "must_use" => Self::MustUse,
            "flags" => Self::Flags,
            "deprecated" => Self::Deprecated,
            "target" => Self::Target,
            "allow" => Self::Allow,
            "cases" => Self::Cases,
            "warn" => Self::Warn,
            "seed" => Self::Seed,
            "deny" => Self::Deny,
            "doc" => Self::Doc,
            "panic_hook" => Self::PanicHook,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct RawAttr {
    pub name: AttrName,
    /// `None` means no parentheses; `Some(vec![])` means an empty `()`.
    pub args: Option<Vec<RawAttrArg>>,
    /// Current-snapshot source location; excluded from structural equality.
    pub span: Span,
    /// Current-snapshot source pointer; excluded from structural equality.
    pub ptr: ast::AstPtr<ast::Attribute>,
    /// True if the parser recovered inside the attribute or its arguments.
    pub has_error: bool,
}

impl PartialEq for RawAttr {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.args == other.args && self.has_error == other.has_error
    }
}

impl Eq for RawAttr {}

#[derive(Clone, Debug)]
pub enum RawAttrArg {
    Word {
        name: Symbol,
        raw: bool,
        is_upper: bool,
        args: Option<Vec<RawAttrArg>>,
        /// Current-snapshot source location; excluded from structural equality.
        span: Span,
    },
    Str {
        value: String,
        /// Current-snapshot source location; excluded from structural equality.
        span: Span,
    },
    Int {
        value: Option<u128>,
        /// Current-snapshot source location; excluded from structural equality.
        span: Span,
    },
    KeyValue {
        key: Symbol,
        raw: bool,
        value: RawAttrValue,
        /// Current-snapshot source location of the value token.
        value_span: Span,
        /// Current-snapshot source location; excluded from structural equality.
        span: Span,
    },
}

impl PartialEq for RawAttrArg {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Word {
                    name: left_name,
                    raw: left_raw,
                    is_upper: left_is_upper,
                    args: left_args,
                    ..
                },
                Self::Word {
                    name: right_name,
                    raw: right_raw,
                    is_upper: right_is_upper,
                    args: right_args,
                    ..
                },
            ) => {
                left_name == right_name
                    && left_raw == right_raw
                    && left_is_upper == right_is_upper
                    && left_args == right_args
            }
            (Self::Str { value: left, .. }, Self::Str { value: right, .. }) => left == right,
            (Self::Int { value: left, .. }, Self::Int { value: right, .. }) => left == right,
            (
                Self::KeyValue {
                    key: left_key,
                    raw: left_raw,
                    value: left_value,
                    ..
                },
                Self::KeyValue {
                    key: right_key,
                    raw: right_raw,
                    value: right_value,
                    ..
                },
            ) => left_key == right_key && left_raw == right_raw && left_value == right_value,
            _ => false,
        }
    }
}

impl Eq for RawAttrArg {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawAttrValue {
    Str(String),
    Int(Option<u128>),
    Word(Symbol),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ItemTreeKind {
    Function,
    Struct,
    Alias,
    Refined,
    Enum,
    Distinct,
    Resource,
    Trait,
    Impl,
    Const,
    Invariant,
    Effect,
    Interface,
    World,
    ExternBlock,
    Test,
    Property,
    TypeParam,
    ConstParam,
    Field,
    Variant,
    VariantField,
    AssocType,
    AssocFn,
    InterfaceFn,
    ExternFn,
    WorldItem { import: bool },
    WorldInclude,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseEntry {
    pub ast_id: ErasedAstId,
    pub is_pub: bool,
    pub pkg_path: String,
    pub alias: Option<Symbol>,
    pub items: Vec<UseItem>,
    pub raw_attrs: Vec<RawAttr>,
}

#[derive(Clone, Debug)]
pub struct UseItem {
    pub name: Symbol,
    pub alias: Option<Symbol>,
    /// Current-snapshot source range; excluded from structural equality.
    pub range: TextRange,
}

impl PartialEq for UseItem {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.alias == other.alias
    }
}

impl Eq for UseItem {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemTreeEntry {
    pub ast_id: ErasedAstId,
    pub kind: ItemTreeKind,
    /// `Symbol::ERROR` means the name was missing or could not be recovered.
    pub name: Symbol,
    pub vis: crate::Visibility,
    pub raw_attrs: Vec<RawAttr>,
    /// Direct declarations and generic parameters, in source order.
    pub children: Vec<ItemTreeEntry>,
    /// Present for impl blocks only (§2.8.2).
    pub header_hash: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ItemTree {
    pub file: FileId,
    /// Snapshot that supplied the AST pointers; ignored by structural equality.
    snapshot_id: WorkspaceSnapshotId,
    /// Inner module documentation has a separate syntax role from attributes.
    pub module_docs: Option<ast::AstPtr<ast::Docs>>,
    /// Source text used to compare module documentation across snapshots.
    module_docs_text: Option<String>,
    pub is_unsafe_module: bool,
    pub uses: Vec<UseEntry>,
    pub items: Vec<ItemTreeEntry>,
}

impl PartialEq for ItemTree {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.module_docs.is_some() == other.module_docs.is_some()
            && self.module_docs_text == other.module_docs_text
            && self.is_unsafe_module == other.is_unsafe_module
            && self.uses == other.uses
            && self.items == other.items
    }
}

impl Eq for ItemTree {}

/// A target-filtered item tree and the validation results used to select it.
#[derive(Clone, Debug)]
pub struct TargetFilterResult {
    pub item_tree: ItemTree,
    /// Target-only attribute results for retained module-level entries.
    pub validated_targets: BTreeMap<ErasedAstId, ValidatedAttrs>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetFilterError {
    StaleItemTree,
    SnapshotMismatch,
    ItemTree(ItemTreeError),
    AttributeValidation(AttrValidationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemTreeError {
    FileMismatch,
    SourceMismatch,
    InvalidSourceRange,
    MissingAstId,
    InvalidAstPointer,
    Interner(InternError),
}

impl ItemTree {
    /// Whether this item tree was collected from the AST map's exact snapshot.
    pub fn matches_ast_ids(&self, ast_ids: &ast::AstIdMap) -> bool {
        self.file == ast_ids.file() && ast_ids.snapshot_id() == Some(self.snapshot_id)
    }

    pub fn collect(
        file: FileId,
        source: &str,
        root: &SyntaxNode,
        ast_ids: &ast::AstIdMap,
        symbols: &Interner,
    ) -> Result<Self, ItemTreeError> {
        if !ast_ids.matches_root(file, root) {
            return Err(ItemTreeError::FileMismatch);
        }
        if !green_text_matches_source(root, source) {
            return Err(ItemTreeError::SourceMismatch);
        }
        let snapshot_id = ast_ids.snapshot_id().ok_or(ItemTreeError::FileMismatch)?;
        let source_file = ast::SourceFile::cast(root.clone()).ok_or(ItemTreeError::FileMismatch)?;
        let mut tree = Self {
            file,
            snapshot_id,
            module_docs: None,
            module_docs_text: None,
            is_unsafe_module: false,
            uses: Vec::new(),
            items: Vec::new(),
        };

        if let Some(header) = source_file.module_header() {
            tree.is_unsafe_module = header.unsafe_token().is_some();
            if let Some(docs) = header.docs() {
                tree.module_docs_text = Some(node_text(source, docs.syntax())?.to_owned());
                tree.module_docs = Some(
                    ast::AstPtr::new(file, &docs).map_err(|_| ItemTreeError::InvalidAstPointer)?,
                );
            }
        }

        for item in source_file.items() {
            match item {
                ast::Item::Use(node) => {
                    tree.uses
                        .push(collect_use(file, source, &node, ast_ids, symbols)?);
                }
                ast::Item::Function(node) => tree.items.push(collect_function(
                    file,
                    source,
                    &node,
                    ItemTreeKind::Function,
                    ast_ids,
                    symbols,
                )?),
                ast::Item::Type(node) => {
                    tree.items
                        .push(collect_type(file, source, &node, ast_ids, symbols)?);
                }
                ast::Item::Enum(node) => tree
                    .items
                    .push(collect_enum(file, source, &node, ast_ids, symbols)?),
                ast::Item::Distinct(node) => tree
                    .items
                    .push(collect_distinct(file, source, &node, ast_ids, symbols)?),
                ast::Item::Resource(node) => tree
                    .items
                    .push(collect_resource(file, source, &node, ast_ids, symbols)?),
                ast::Item::Trait(node) => tree
                    .items
                    .push(collect_trait(file, source, &node, ast_ids, symbols)?),
                ast::Item::Impl(node) => tree
                    .items
                    .push(collect_impl(file, source, &node, ast_ids, symbols)?),
                ast::Item::Const(node) => tree.items.push(collect_named(
                    file,
                    source,
                    &node,
                    ItemTreeKind::Const,
                    node.name(),
                    direct_visibility(node.syntax(), crate::Visibility::Private),
                    None,
                    node.attrs(),
                    ast_ids,
                    symbols,
                )?),
                ast::Item::Invariant(node) => tree.items.push(collect_named(
                    file,
                    source,
                    &node,
                    ItemTreeKind::Invariant,
                    node.name(),
                    crate::Visibility::Private,
                    None,
                    node.attrs(),
                    ast_ids,
                    symbols,
                )?),
                ast::Item::Effect(node) => tree.items.push(collect_named(
                    file,
                    source,
                    &node,
                    ItemTreeKind::Effect,
                    node.name(),
                    direct_visibility(node.syntax(), crate::Visibility::Private),
                    None,
                    node.attrs(),
                    ast_ids,
                    symbols,
                )?),
                ast::Item::Interface(node) => tree
                    .items
                    .push(collect_interface(file, source, &node, ast_ids, symbols)?),
                ast::Item::World(node) => tree
                    .items
                    .push(collect_world(file, source, &node, ast_ids, symbols)?),
                ast::Item::ExternBlock(node) => tree
                    .items
                    .push(collect_extern_block(file, source, &node, ast_ids, symbols)?),
                ast::Item::Test(node) => {
                    let mut entry = collect_named(
                        file,
                        source,
                        &node,
                        ItemTreeKind::Test,
                        None::<ast::Name>,
                        crate::Visibility::Private,
                        None,
                        node.attrs(),
                        ast_ids,
                        symbols,
                    )?;
                    entry.name = node
                        .name()
                        .map(|name| {
                            symbols
                                .intern(&name.value())
                                .map_err(ItemTreeError::Interner)
                        })
                        .transpose()?
                        .unwrap_or(Symbol::ERROR);
                    tree.items.push(entry);
                }
                ast::Item::Property(node) => {
                    let mut entry = collect_named(
                        file,
                        source,
                        &node,
                        ItemTreeKind::Property,
                        None::<ast::Name>,
                        crate::Visibility::Private,
                        None,
                        node.attrs(),
                        ast_ids,
                        symbols,
                    )?;
                    entry.name = node
                        .name()
                        .map(|name| {
                            symbols
                                .intern(&name.value())
                                .map_err(ItemTreeError::Interner)
                        })
                        .transpose()?
                        .unwrap_or(Symbol::ERROR);
                    tree.items.push(entry);
                }
            }
        }

        Ok(tree)
    }

    /// Collects and target-filters one module for compiler use.
    ///
    /// This is the preferred item-collection entry point before name binding
    /// and duplicate checks.
    pub fn collect_for_target(
        file: FileId,
        source: &str,
        root: &SyntaxNode,
        ast_ids: &ast::AstIdMap,
        symbols: &Interner,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
    ) -> Result<TargetFilterResult, TargetFilterError> {
        let current = Self::collect(file, source, root, ast_ids, symbols)
            .map_err(TargetFilterError::ItemTree)?;
        if !ast_ids.matches_snapshot(snapshot)
            || snapshot.id() != current.snapshot_id
            || snapshot
                .file(file)
                .is_none_or(|snapshot_file| snapshot_file.source().text() != source)
        {
            return Err(TargetFilterError::SnapshotMismatch);
        }
        filter_collected_tree(current, symbols, snapshot, target_cfg)
    }

    /// Removes module-level entries whose valid `@target` predicate is false.
    ///
    /// The item tree is checked against the current AST map, source, and
    /// workspace snapshot before its attributes are used. Only `@target` is
    /// validated here: diagnostics on a filtered-out declaration are
    /// suppressed along with that declaration.
    pub fn filter_for_target(
        &self,
        source: &str,
        root: &SyntaxNode,
        ast_ids: &ast::AstIdMap,
        symbols: &Interner,
        snapshot: &WorkspaceSnapshot,
        target_cfg: &TargetCfg,
    ) -> Result<TargetFilterResult, TargetFilterError> {
        if !self.matches_ast_ids(ast_ids) || !ast_ids.matches_root(self.file, root) {
            return Err(TargetFilterError::StaleItemTree);
        }
        if !ast_ids.matches_snapshot(snapshot)
            || snapshot.id() != self.snapshot_id
            || snapshot
                .file(self.file)
                .is_none_or(|file| file.source().text() != source)
        {
            return Err(TargetFilterError::SnapshotMismatch);
        }

        let current = Self::collect(self.file, source, root, ast_ids, symbols)
            .map_err(TargetFilterError::ItemTree)?;
        if self != &current {
            return Err(TargetFilterError::StaleItemTree);
        }

        filter_collected_tree(current, symbols, snapshot, target_cfg)
    }
}

fn filter_collected_tree(
    current: ItemTree,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    target_cfg: &TargetCfg,
) -> Result<TargetFilterResult, TargetFilterError> {
    let mut validated_targets = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let mut uses = Vec::with_capacity(current.uses.len());
    for entry in current.uses.iter().cloned() {
        let validated =
            validate_target_attrs(&entry.raw_attrs, TargetKind::Use, symbols, snapshot)?;
        if target_matches(&validated, target_cfg) {
            diagnostics.extend(validated.diagnostics.iter().cloned());
            validated_targets.insert(entry.ast_id, validated);
            uses.push(entry);
        }
    }

    let mut items = Vec::with_capacity(current.items.len());
    for entry in current.items.iter().cloned() {
        let kind = module_target_kind(entry.kind).ok_or(TargetFilterError::StaleItemTree)?;
        let validated = validate_target_attrs(&entry.raw_attrs, kind, symbols, snapshot)?;
        if target_matches(&validated, target_cfg) {
            diagnostics.extend(validated.diagnostics.iter().cloned());
            validated_targets.insert(entry.ast_id, validated);
            items.push(entry);
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start())
    });

    let mut item_tree = current;
    item_tree.uses = uses;
    item_tree.items = items;
    Ok(TargetFilterResult {
        item_tree,
        validated_targets,
        diagnostics,
    })
}

fn validate_target_attrs(
    raw_attrs: &[RawAttr],
    kind: TargetKind,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
) -> Result<ValidatedAttrs, TargetFilterError> {
    let target_attrs = raw_attrs
        .iter()
        .filter(|attribute| attribute.name == AttrName::Target)
        .cloned()
        .collect::<Vec<_>>();
    if target_attrs.is_empty() {
        return Ok(ValidatedAttrs::default());
    }
    validate_attrs(
        &target_attrs,
        Some(AttributeTarget {
            module_level: true,
            ..AttributeTarget::new(kind)
        }),
        symbols,
        snapshot,
    )
    .map_err(TargetFilterError::AttributeValidation)
}

fn target_matches(validated: &ValidatedAttrs, target_cfg: &TargetCfg) -> bool {
    validated
        .target
        .as_ref()
        .is_none_or(|(predicate, _)| predicate.matches(target_cfg))
}

fn module_target_kind(kind: ItemTreeKind) -> Option<TargetKind> {
    Some(match kind {
        ItemTreeKind::Function => TargetKind::Fn,
        ItemTreeKind::Struct => TargetKind::Record,
        ItemTreeKind::Alias => TargetKind::Alias,
        ItemTreeKind::Refined => TargetKind::Refined,
        ItemTreeKind::Enum => TargetKind::Enum,
        ItemTreeKind::Distinct => TargetKind::Distinct,
        ItemTreeKind::Resource => TargetKind::Resource,
        ItemTreeKind::Trait => TargetKind::Trait,
        ItemTreeKind::Impl => TargetKind::Impl,
        ItemTreeKind::Const => TargetKind::Const,
        ItemTreeKind::Invariant => TargetKind::Invariant,
        ItemTreeKind::Effect => TargetKind::Effect,
        ItemTreeKind::Interface => TargetKind::Interface,
        ItemTreeKind::World => TargetKind::World,
        ItemTreeKind::ExternBlock => TargetKind::ExternBlock,
        ItemTreeKind::Test => TargetKind::Test,
        ItemTreeKind::Property => TargetKind::Property,
        ItemTreeKind::TypeParam
        | ItemTreeKind::ConstParam
        | ItemTreeKind::Field
        | ItemTreeKind::Variant
        | ItemTreeKind::VariantField
        | ItemTreeKind::AssocType
        | ItemTreeKind::AssocFn
        | ItemTreeKind::InterfaceFn
        | ItemTreeKind::ExternFn
        | ItemTreeKind::WorldItem { .. }
        | ItemTreeKind::WorldInclude => return None,
    })
}

fn collect_use(
    file: FileId,
    source: &str,
    node: &ast::UseDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<UseEntry, ItemTreeError> {
    let ast_id = erased_id(node, ast_ids)?;
    let pkg_path = node
        .package()
        .map(|path| path.syntax().text().to_owned())
        .unwrap_or_default();
    let alias = node
        .alias()
        .and_then(|alias| alias.name())
        .map(|name| name_symbol(source, &name, symbols))
        .transpose()?;
    let mut items = Vec::new();
    if let Some(use_items) = node.items() {
        for item in use_items.items() {
            let name = item
                .name()
                .map(|name| name_symbol(source, &name, symbols))
                .transpose()?
                .unwrap_or(Symbol::ERROR);
            let alias = item
                .alias()
                .map(|name| name_symbol(source, &name, symbols))
                .transpose()?;
            items.push(UseItem {
                name,
                alias,
                range: item
                    .name()
                    .map(|name| name.syntax().text_range())
                    .unwrap_or_else(|| item.syntax().text_range()),
            });
        }
    }
    Ok(UseEntry {
        ast_id,
        is_pub: node
            .visibility()
            .is_some_and(|visibility| visibility.pub_token().is_some()),
        pkg_path,
        alias,
        items,
        raw_attrs: raw_attrs(file, source, node.attrs(), symbols)?,
    })
}

fn collect_function(
    file: FileId,
    source: &str,
    node: &ast::FnDecl,
    kind: ItemTreeKind,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        kind,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    entry.children = collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?;
    Ok(entry)
}

fn collect_type(
    file: FileId,
    source: &str,
    node: &ast::TypeDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let (kind, fields) = if let Some(definition) = node.struct_def() {
        let fields = definition
            .fields()
            .map(|list| list.fields().collect::<Vec<_>>())
            .unwrap_or_default();
        (ItemTreeKind::Struct, fields)
    } else if let Some(definition) = node.alias_def() {
        (
            if definition.refinement().is_some() {
                ItemTreeKind::Refined
            } else {
                ItemTreeKind::Alias
            },
            Vec::new(),
        )
    } else {
        (ItemTreeKind::Alias, Vec::new())
    };
    let mut entry = collect_named(
        file,
        source,
        node,
        kind,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    let mut children = locate_entries(
        collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?,
        ast_ids,
    )?;
    for field in fields {
        let child = collect_field(file, source, &field, ast_ids, symbols)?;
        children.push((field.syntax().text_range().start().to_usize(), child));
    }
    entry.children = finish_children(children);
    Ok(entry)
}

fn collect_enum(
    file: FileId,
    source: &str,
    node: &ast::EnumDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let vis = direct_visibility(node.syntax(), crate::Visibility::Private);
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Enum,
        node.name(),
        vis,
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    let mut children = locate_entries(
        collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?,
        ast_ids,
    )?;
    if let Some(variants) = node.variants() {
        for variant in variants.variants() {
            let mut child = collect_named(
                file,
                source,
                &variant,
                ItemTreeKind::Variant,
                variant.name(),
                vis,
                None,
                variant.attrs(),
                ast_ids,
                symbols,
            )?;
            let mut fields = Vec::new();
            if let Some(shape) = variant.record_shape() {
                for field in shape.fields() {
                    fields.push(collect_named(
                        file,
                        source,
                        &field,
                        ItemTreeKind::VariantField,
                        field.name(),
                        vis,
                        None,
                        field.attrs(),
                        ast_ids,
                        symbols,
                    )?);
                }
            }
            child.children = fields;
            children.push((variant.syntax().text_range().start().to_usize(), child));
        }
    }
    entry.children = finish_children(children);
    Ok(entry)
}

fn collect_distinct(
    file: FileId,
    source: &str,
    node: &ast::DistinctDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Distinct,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    entry.children = collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?;
    Ok(entry)
}

fn collect_resource(
    file: FileId,
    source: &str,
    node: &ast::ResourceDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let vis = direct_visibility(node.syntax(), crate::Visibility::Private);
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Resource,
        node.name(),
        vis,
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    let mut children = Vec::new();
    if let Some(fields) = node.fields() {
        for field in fields.fields() {
            children.push(collect_field(file, source, &field, ast_ids, symbols)?);
        }
    }
    entry.children = children;
    Ok(entry)
}

fn collect_trait(
    file: FileId,
    source: &str,
    node: &ast::TraitDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Trait,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    let mut children = locate_entries(
        collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?,
        ast_ids,
    )?;
    if let Some(list) = node.items() {
        children.extend(collect_assoc_items(file, source, &list, ast_ids, symbols)?);
    }
    entry.children = finish_children(children);
    Ok(entry)
}

fn collect_impl(
    file: FileId,
    source: &str,
    node: &ast::ImplDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Impl,
        None::<ast::Name>,
        crate::Visibility::Private,
        Some(impl_header_hash(node)),
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    let mut children = locate_entries(
        collect_generic_params(file, source, node.generic_params(), ast_ids, symbols)?,
        ast_ids,
    )?;
    if let Some(list) = node.items() {
        children.extend(collect_assoc_items(file, source, &list, ast_ids, symbols)?);
    }
    entry.children = finish_children(children);
    Ok(entry)
}

fn collect_interface(
    file: FileId,
    source: &str,
    node: &ast::InterfaceDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::Interface,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    if let Some(list) = node.items() {
        entry.children = collect_interface_items(file, source, &list, ast_ids, symbols)?
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
    }
    Ok(entry)
}

fn collect_world(
    file: FileId,
    source: &str,
    node: &ast::WorldDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::World,
        node.name(),
        direct_visibility(node.syntax(), crate::Visibility::Private),
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    if let Some(list) = node.items() {
        entry.children = collect_world_items(file, source, &list, ast_ids, symbols)?
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
    }
    Ok(entry)
}

fn collect_extern_block(
    file: FileId,
    source: &str,
    node: &ast::ExternBlock,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::ExternBlock,
        None::<ast::Name>,
        crate::Visibility::Private,
        None,
        node.attrs(),
        ast_ids,
        symbols,
    )?;
    if let Some(list) = node.items() {
        entry.children = list
            .syntax()
            .children()
            .filter_map(ast::FnDecl::cast)
            .map(|function| {
                collect_function(
                    file,
                    source,
                    &function,
                    ItemTreeKind::ExternFn,
                    ast_ids,
                    symbols,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(entry)
}

fn collect_assoc_items(
    file: FileId,
    source: &str,
    list: &ast::ItemList,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<Vec<(usize, ItemTreeEntry)>, ItemTreeError> {
    let mut children = Vec::new();
    for child in list.syntax().children() {
        if let Some(function) = ast::FnDecl::cast(child.clone()) {
            let entry = collect_function(
                file,
                source,
                &function,
                ItemTreeKind::AssocFn,
                ast_ids,
                symbols,
            )?;
            children.push((child.text_range().start().to_usize(), entry));
        } else if let Some(declaration) = ast::AssocTypeDecl::cast(child.clone()) {
            let entry = collect_named(
                file,
                source,
                &declaration,
                ItemTreeKind::AssocType,
                declaration.name(),
                crate::Visibility::Private,
                None,
                declaration.attrs(),
                ast_ids,
                symbols,
            )?;
            children.push((child.text_range().start().to_usize(), entry));
        } else if let Some(definition) = ast::AssocTypeDef::cast(child.clone()) {
            let entry = collect_named(
                file,
                source,
                &definition,
                ItemTreeKind::AssocType,
                definition.name(),
                crate::Visibility::Private,
                None,
                definition.attrs(),
                ast_ids,
                symbols,
            )?;
            children.push((child.text_range().start().to_usize(), entry));
        }
    }
    Ok(children)
}

fn collect_interface_items(
    file: FileId,
    source: &str,
    list: &ast::ItemList,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<Vec<(usize, ItemTreeEntry)>, ItemTreeError> {
    let mut children = Vec::new();
    for child in list.syntax().children() {
        if let Some(ty) = ast::TypeDecl::cast(child.clone()) {
            children.push((
                child.text_range().start().to_usize(),
                collect_type(file, source, &ty, ast_ids, symbols)?,
            ));
        } else if let Some(enumeration) = ast::EnumDecl::cast(child.clone()) {
            children.push((
                child.text_range().start().to_usize(),
                collect_enum(file, source, &enumeration, ast_ids, symbols)?,
            ));
        } else if let Some(resource) = ast::IfaceResource::cast(child.clone()) {
            let mut entry = collect_named(
                file,
                source,
                &resource,
                ItemTreeKind::Resource,
                resource.name(),
                crate::Visibility::Private,
                None,
                resource.attrs(),
                ast_ids,
                symbols,
            )?;
            if let Some(list) = resource.items() {
                entry.children = collect_assoc_items(file, source, &list, ast_ids, symbols)?
                    .into_iter()
                    .map(|(_, entry)| entry)
                    .collect();
            }
            children.push((child.text_range().start().to_usize(), entry));
        } else if let Some(function) = ast::FnDecl::cast(child.clone()) {
            children.push((
                child.text_range().start().to_usize(),
                collect_function(
                    file,
                    source,
                    &function,
                    ItemTreeKind::InterfaceFn,
                    ast_ids,
                    symbols,
                )?,
            ));
        }
    }
    Ok(children)
}

fn collect_world_items(
    file: FileId,
    source: &str,
    list: &ast::ItemList,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<Vec<(usize, ItemTreeEntry)>, ItemTreeError> {
    let mut children = Vec::new();
    for child in list.syntax().children() {
        if let Some(item) = ast::WorldImport::cast(child.clone()) {
            children.push((
                child.text_range().start().to_usize(),
                collect_world_item(file, source, &item, true, ast_ids, symbols)?,
            ));
        } else if let Some(item) = ast::WorldExport::cast(child.clone()) {
            children.push((
                child.text_range().start().to_usize(),
                collect_world_item(file, source, &item, false, ast_ids, symbols)?,
            ));
        } else if let Some(item) = ast::WorldInclude::cast(child.clone()) {
            let name = item
                .reference()
                .map(|reference| world_ref_text(source, &reference))
                .transpose()?
                .map(|text| symbols.intern(&text).map_err(ItemTreeError::Interner))
                .transpose()?
                .unwrap_or(Symbol::ERROR);
            let entry = collect_named(
                file,
                source,
                &item,
                ItemTreeKind::WorldInclude,
                None::<ast::Name>,
                crate::Visibility::Private,
                None,
                item.attrs(),
                ast_ids,
                symbols,
            )?;
            children.push((
                child.text_range().start().to_usize(),
                ItemTreeEntry { name, ..entry },
            ));
        }
    }
    Ok(children)
}

fn collect_world_item<N: WorldItemAccessor>(
    file: FileId,
    source: &str,
    node: &N,
    import: bool,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let function = node.world_function();
    let name = if let Some(function) = function {
        named_symbol(source, function.name(), symbols)?
    } else if let Some(reference) = node.world_reference() {
        symbols
            .intern(&world_ref_text(source, &reference)?)
            .map_err(ItemTreeError::Interner)?
    } else {
        Symbol::ERROR
    };
    let mut entry = collect_named(
        file,
        source,
        node,
        ItemTreeKind::WorldItem { import },
        None::<ast::Name>,
        crate::Visibility::Private,
        None,
        node.world_attrs().into_iter(),
        ast_ids,
        symbols,
    )?;
    entry.name = name;
    Ok(entry)
}

trait WorldItemAccessor: AstNode {
    fn world_function(&self) -> Option<ast::FnDecl>;
    fn world_reference(&self) -> Option<ast::WorldRef>;
    fn world_attrs(&self) -> Vec<ast::Attribute>;
}

impl WorldItemAccessor for ast::WorldImport {
    fn world_function(&self) -> Option<ast::FnDecl> {
        self.function()
    }

    fn world_reference(&self) -> Option<ast::WorldRef> {
        self.reference()
    }

    fn world_attrs(&self) -> Vec<ast::Attribute> {
        self.attrs().collect()
    }
}

impl WorldItemAccessor for ast::WorldExport {
    fn world_function(&self) -> Option<ast::FnDecl> {
        self.function()
    }

    fn world_reference(&self) -> Option<ast::WorldRef> {
        self.reference()
    }

    fn world_attrs(&self) -> Vec<ast::Attribute> {
        self.attrs().collect()
    }
}

pub(crate) fn world_ref_text(
    source: &str,
    reference: &ast::WorldRef,
) -> Result<String, ItemTreeError> {
    if let Some(package) = reference.package() {
        return Ok(package.syntax().text().to_owned());
    }
    let mut segments = Vec::new();
    if let Some(path) = reference.mod_path() {
        for segment in path.segments() {
            let text = node_text(source, segment.syntax())?;
            segments.push(sai_syntax::normalize_identifier(
                text.strip_prefix("r#").unwrap_or(text),
            ));
        }
    }
    Ok(segments.join("."))
}

fn collect_field(
    file: FileId,
    source: &str,
    field: &ast::FieldDecl,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError> {
    let vis = if field.pub_token().is_some() {
        crate::Visibility::Pub
    } else {
        crate::Visibility::Private
    };
    collect_named(
        file,
        source,
        field,
        ItemTreeKind::Field,
        field.name(),
        vis,
        None,
        field.attrs(),
        ast_ids,
        symbols,
    )
}

fn collect_generic_params(
    file: FileId,
    source: &str,
    list: Option<ast::GenericParamList>,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<Vec<ItemTreeEntry>, ItemTreeError> {
    let mut entries = Vec::new();
    if let Some(list) = list {
        for parameter in list.params() {
            let entry = match parameter {
                ast::GenericParam::Type(node) => collect_named(
                    file,
                    source,
                    &node,
                    ItemTreeKind::TypeParam,
                    node.name(),
                    crate::Visibility::Private,
                    None,
                    node.attrs(),
                    ast_ids,
                    symbols,
                )?,
                ast::GenericParam::Const(node) => collect_named(
                    file,
                    source,
                    &node,
                    ItemTreeKind::ConstParam,
                    node.name(),
                    crate::Visibility::Private,
                    None,
                    node.attrs(),
                    ast_ids,
                    symbols,
                )?,
            };
            entries.push(entry);
        }
    }
    Ok(entries)
}

// Each parameter is an independent source-derived field of the resulting entry.
#[allow(clippy::too_many_arguments)]
fn collect_named<N, Name>(
    file: FileId,
    source: &str,
    node: &N,
    kind: ItemTreeKind,
    name: Option<Name>,
    vis: crate::Visibility,
    header_hash: Option<u64>,
    attrs: impl Iterator<Item = ast::Attribute>,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
) -> Result<ItemTreeEntry, ItemTreeError>
where
    N: AstNode,
    Name: AstNode,
{
    let name = named_symbol(source, name, symbols)?;
    Ok(ItemTreeEntry {
        ast_id: erased_id(node, ast_ids)?,
        kind,
        name,
        vis,
        raw_attrs: raw_attrs(file, source, attrs, symbols)?,
        children: Vec::new(),
        header_hash,
    })
}

pub(crate) fn named_symbol<N: AstNode>(
    source: &str,
    name: Option<N>,
    symbols: &Interner,
) -> Result<Symbol, ItemTreeError> {
    name.map(|name| name_symbol(source, &name, symbols))
        .transpose()
        .map(|name| name.unwrap_or(Symbol::ERROR))
}

fn name_symbol<N: AstNode>(
    source: &str,
    name: &N,
    symbols: &Interner,
) -> Result<Symbol, ItemTreeError> {
    let text = node_text(source, name.syntax())?;
    let raw_name = text.strip_prefix("r#");
    if matches!(raw_name, Some("self" | "Self" | "_"))
        || (raw_name.is_none() && sai_syntax::is_reserved_identifier(text))
    {
        return Ok(Symbol::ERROR);
    }
    let text = raw_name.unwrap_or(text);
    let text = sai_syntax::normalize_identifier(text);
    symbols.intern(&text).map_err(ItemTreeError::Interner)
}

fn direct_visibility(node: &SyntaxNode, default: crate::Visibility) -> crate::Visibility {
    node.children()
        .find_map(ast::Visibility::cast)
        .map_or(default, |visibility| {
            if visibility.export_token().is_some() {
                crate::Visibility::Export
            } else if visibility.pub_token().is_some() {
                crate::Visibility::Pub
            } else {
                default
            }
        })
}

pub(crate) fn raw_attrs(
    file: FileId,
    source: &str,
    attrs: impl Iterator<Item = ast::Attribute>,
    symbols: &Interner,
) -> Result<Vec<RawAttr>, ItemTreeError> {
    let mut raw_attrs = Vec::new();
    for attribute in attrs {
        let Some(name_node) = attribute.name() else {
            continue;
        };
        let source_name = node_text(source, name_node.syntax())?;
        let Some(name) = (!source_name.starts_with("r#"))
            .then(|| AttrName::from_source_name(source_name))
            .flatten()
        else {
            continue;
        };

        let mut has_error = syntax_has_error(attribute.syntax());
        let args = if let Some(args) = attribute.args() {
            has_error |= syntax_has_error(args.syntax());
            let mut raw_args = Vec::new();
            for arg in args.items() {
                if let Some(arg) = raw_attr_arg(file, source, &arg, symbols)? {
                    raw_args.push(arg);
                }
            }
            Some(raw_args)
        } else {
            None
        };
        let ptr =
            ast::AstPtr::new(file, &attribute).map_err(|_| ItemTreeError::InvalidAstPointer)?;
        raw_attrs.push(RawAttr {
            name,
            args,
            span: Span::new(file, attribute.syntax().text_range()),
            ptr,
            has_error,
        });
    }
    Ok(raw_attrs)
}

fn raw_attr_arg(
    file: FileId,
    source: &str,
    arg: &ast::AttrArg,
    symbols: &Interner,
) -> Result<Option<RawAttrArg>, ItemTreeError> {
    let span = Span::new(file, arg.syntax().text_range());
    let has_equals = arg.syntax().children_with_tokens().any(
        |element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::EQ),
    );
    if has_equals {
        let raw = if let Some(name) = arg.name() {
            node_text(source, name.syntax())?.starts_with("r#")
        } else {
            false
        };
        let Some(key) = arg
            .name()
            .map(|name| name_symbol(source, &name, symbols))
            .transpose()?
        else {
            return Ok(None);
        };
        let (value, value_span) = if let Some(value) = arg.value() {
            (
                RawAttrValue::Word(name_symbol(source, &value, symbols)?),
                Span::new(file, value.syntax().text_range()),
            )
        } else if let Some(value) = arg.string() {
            (
                RawAttrValue::Str(value.value().into_owned()),
                Span::new(file, value.syntax().text_range()),
            )
        } else {
            let Some(value) = arg.integer() else {
                return Ok(None);
            };
            (
                RawAttrValue::Int(value.value()),
                Span::new(file, value.syntax().text_range()),
            )
        };
        return Ok(Some(RawAttrArg::KeyValue {
            key,
            raw,
            value,
            value_span,
            span,
        }));
    }

    if let Some(name) = arg.name() {
        let is_upper = name
            .syntax()
            .first_token()
            .is_some_and(|token| token.kind() == SyntaxKind::UIDENT);
        let raw = node_text(source, name.syntax())?.starts_with("r#");
        let name = name_symbol(source, &name, symbols);
        let args = if let Some(args) = arg.args() {
            let mut raw_args = Vec::new();
            for arg in args.items() {
                if let Some(arg) = raw_attr_arg(file, source, &arg, symbols)? {
                    raw_args.push(arg);
                }
            }
            Some(raw_args)
        } else {
            None
        };
        return Ok(Some(RawAttrArg::Word {
            name: name?,
            raw,
            is_upper,
            args,
            span,
        }));
    }
    if let Some(string) = arg.string() {
        return Ok(Some(RawAttrArg::Str {
            value: string.value().into_owned(),
            span,
        }));
    }
    Ok(arg.integer().map(|integer| RawAttrArg::Int {
        value: integer.value(),
        span,
    }))
}

fn syntax_has_error(node: &SyntaxNode) -> bool {
    node.descendants().any(|node| {
        node.kind() == SyntaxKind::ERROR
            || node
                .children_with_tokens()
                .any(|element| matches!(element, SyntaxElement::Token(token) if token.has_error()))
    })
}

fn erased_id<N: AstNode>(node: &N, ast_ids: &ast::AstIdMap) -> Result<ErasedAstId, ItemTreeError> {
    ast_ids
        .ast_id(node)
        .map(ast::AstId::erase)
        .ok_or(ItemTreeError::MissingAstId)
}

fn locate_entries(
    entries: Vec<ItemTreeEntry>,
    ast_ids: &ast::AstIdMap,
) -> Result<Vec<(usize, ItemTreeEntry)>, ItemTreeError> {
    let mut located = Vec::with_capacity(entries.len());
    for entry in entries {
        let pointer = ast_ids
            .pointer(entry.ast_id)
            .ok_or(ItemTreeError::MissingAstId)?;
        if pointer.file() != ast_ids.file() {
            return Err(ItemTreeError::FileMismatch);
        }
        located.push((pointer.text_range().start().to_usize(), entry));
    }
    Ok(located)
}

fn finish_children(mut children: Vec<(usize, ItemTreeEntry)>) -> Vec<ItemTreeEntry> {
    children.sort_by_key(|(start, _)| *start);
    children.into_iter().map(|(_, entry)| entry).collect()
}

fn node_text<'a>(source: &'a str, node: &SyntaxNode) -> Result<&'a str, ItemTreeError> {
    let range = node.text_range();
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(ItemTreeError::InvalidSourceRange)
}

fn green_text_matches_source(root: &SyntaxNode, source: &str) -> bool {
    if usize::try_from(root.green().text_len().raw()).ok() != Some(source.len()) {
        return false;
    }

    let source = source.as_bytes();
    let mut offset = 0;
    let mut stack = root.green().children().iter().rev().collect::<Vec<_>>();
    while let Some(element) = stack.pop() {
        match element {
            GreenElement::Node(node) => stack.extend(node.children().iter().rev()),
            GreenElement::Token(token) => {
                for trivia in token.leading_trivia() {
                    if !consume_source_bytes(source, &mut offset, trivia.text()) {
                        return false;
                    }
                }
                if !consume_source_bytes(source, &mut offset, token.text()) {
                    return false;
                }
                for trivia in token.trailing_trivia() {
                    if !consume_source_bytes(source, &mut offset, trivia.text()) {
                        return false;
                    }
                }
            }
        }
    }

    offset == source.len()
}

fn consume_source_bytes(source: &[u8], offset: &mut usize, expected: &str) -> bool {
    let Some(end) = (*offset).checked_add(expected.len()) else {
        return false;
    };
    if source.get(*offset..end) != Some(expected.as_bytes()) {
        return false;
    }
    *offset = end;
    true
}

fn impl_header_hash(node: &ast::ImplDecl) -> u64 {
    let mut hasher = StableHasher::new();
    let body_start = node
        .items()
        .and_then(|items| items.syntax().first_token())
        .map(|token| token.text_range().start());
    let mut started = false;
    let mut stack = node
        .syntax()
        .children_with_tokens()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    while let Some(element) = stack.pop() {
        match element {
            SyntaxElement::Token(token) => {
                if body_start.is_some_and(|start| token.text_range().start() >= start) {
                    break;
                }
                if !started {
                    if token.kind() != SyntaxKind::IMPL_KW {
                        continue;
                    }
                    started = true;
                }
                if token.kind() != SyntaxKind::NEWLINE {
                    hasher.write_u16(token.kind() as u16);
                    hasher.write_len(token.text().len() as u64);
                    hasher.write_raw_bytes(token.text().as_bytes());
                }
            }
            SyntaxElement::Node(node) => {
                if body_start.is_some_and(|start| node.text_range().start() >= start) {
                    break;
                }
                stack.extend(
                    node.children_with_tokens()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                );
            }
        }
    }
    hasher.finish().raw() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};

    fn parse_snapshot(source: &str, file: FileId) -> (sai_syntax::Parse, ast::AstIdMap) {
        let source_file = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source_file, "src/lib.sai", None, None)])
                .expect("valid workspace snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("source parses");
        let ast_ids = ast::AstIdMap::new(file, &parsed.root).expect("snapshot-bound AST IDs");
        (parsed, ast_ids)
    }

    fn collect(source: &str, file: FileId) -> (sai_syntax::Parse, Interner, ItemTree) {
        let (parsed, ast_ids) = parse_snapshot(source, file);
        let symbols = Interner::new();
        let tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        (parsed, symbols, tree)
    }

    fn collect_with_snapshot(
        source: &str,
        file: FileId,
    ) -> (
        sai_syntax::Parse,
        ast::AstIdMap,
        Interner,
        ItemTree,
        WorkspaceSnapshot,
    ) {
        let source_file = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source_file, "src/lib.sai", None, None)])
                .expect("valid workspace snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("source parses");
        let ast_ids = ast::AstIdMap::new(file, &parsed.root).expect("snapshot-bound AST IDs");
        let symbols = Interner::new();
        let tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        (parsed, ast_ids, symbols, tree, snapshot)
    }

    fn target_cfg(target_name: &str, capabilities: &[&str]) -> TargetCfg {
        TargetCfg::new(target_name, capabilities.iter().copied())
            .expect("capabilities match the target")
    }

    fn child_kinds(entry: &ItemTreeEntry) -> Vec<ItemTreeKind> {
        entry.children.iter().map(|entry| entry.kind).collect()
    }

    #[test]
    fn target_filter_selects_duplicate_names_before_later_checks() {
        let source = concat!(
            "@target(\"wasm32-core\")\n",
            "fn choose() {}\n",
            "@target(\"wasm64-core\")\n",
            "fn choose() {}\n",
            "fn common() {}\n",
        );
        let file = FileId::new(720);
        let (parsed, ast_ids, symbols, _, snapshot) = collect_with_snapshot(source, file);
        assert!(parsed.errors.is_empty());

        let result = ItemTree::collect_for_target(
            file,
            source,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg("wasm32-core", &[]),
        )
        .expect("target filtering succeeds");
        assert_eq!(result.item_tree.items.len(), 2);
        assert_eq!(
            result
                .item_tree
                .items
                .iter()
                .map(|entry| symbols.resolve(entry.name).unwrap().to_string())
                .collect::<Vec<_>>(),
            ["choose", "common"]
        );
        assert_eq!(result.validated_targets.len(), 2);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn target_filter_uses_the_supplied_effective_capability_set() {
        let source = concat!(
            "@target(host)\n",
            "fn host_only() {}\n",
            "@target(none(host))\n",
            "fn without_host() {}\n",
            "@target(any(\"wasm64-core\", memory64))\n",
            "fn wide_memory() {}\n",
        );
        let file = FileId::new(725);
        let (parsed, ast_ids, symbols, tree, snapshot) = collect_with_snapshot(source, file);
        assert!(parsed.errors.is_empty());

        let without_host = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm32-core", &[]),
            )
            .expect("target filtering succeeds");
        assert_eq!(
            without_host
                .item_tree
                .items
                .iter()
                .map(|entry| symbols.resolve(entry.name).unwrap().to_string())
                .collect::<Vec<_>>(),
            ["without_host"]
        );

        let with_host = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm64-core", &["host", "memory64"]),
            )
            .expect("target filtering succeeds");
        assert_eq!(
            with_host
                .item_tree
                .items
                .iter()
                .map(|entry| symbols.resolve(entry.name).unwrap().to_string())
                .collect::<Vec<_>>(),
            ["host_only", "wide_memory"]
        );
    }

    #[test]
    fn target_filter_removes_uses_and_nested_items_with_their_parent() {
        let source = concat!(
            "@target(\"wasm64-core\")\n",
            "use sai:core/fmt.{Debug}\n",
            "@target(\"wasm64-core\")\n",
            "impl Copy for Thing { fn hidden(self: Self) {} }\n",
            "fn visible() {}\n",
        );
        let file = FileId::new(721);
        let (parsed, ast_ids, symbols, tree, snapshot) = collect_with_snapshot(source, file);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);

        let filtered = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm32-core", &[]),
            )
            .expect("target filtering succeeds");
        assert!(filtered.item_tree.uses.is_empty());
        assert_eq!(filtered.item_tree.items.len(), 1);
        assert_eq!(filtered.item_tree.items[0].kind, ItemTreeKind::Function);

        let retained = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm64-core", &["memory64"]),
            )
            .expect("target filtering succeeds");
        assert_eq!(retained.item_tree.uses.len(), 1);
        assert_eq!(retained.item_tree.items.len(), 2);
        assert_eq!(
            child_kinds(&retained.item_tree.items[0]),
            [ItemTreeKind::AssocFn]
        );
    }

    #[test]
    fn target_filter_suppresses_other_attribute_diagnostics_on_removed_items() {
        let source = concat!(
            "@target(\"wasm64-core\")\n",
            "@derive(UnknownTrait)\n",
            "fn omitted() {}\n",
            "@target(\"wasm32-linux\")\n",
            "fn retained() {}\n",
        );
        let file = FileId::new(722);
        let (parsed, ast_ids, symbols, tree, snapshot) = collect_with_snapshot(source, file);
        assert!(parsed.errors.is_empty());

        let result = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm32-core", &[]),
            )
            .expect("target filtering succeeds");
        assert_eq!(result.item_tree.items.len(), 1);
        assert_eq!(
            symbols
                .resolve(result.item_tree.items[0].name)
                .unwrap()
                .as_ref(),
            "retained"
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            sai_diagnostics::Code::UnknownTarget
        );
    }

    #[test]
    fn target_filter_reports_disallowed_interface_target_and_keeps_item() {
        let source = "@target(\"wasm64-core\")\ninterface api {}\n";
        let file = FileId::new(723);
        let (parsed, ast_ids, symbols, tree, snapshot) = collect_with_snapshot(source, file);
        assert!(parsed.errors.is_empty());

        let result = tree
            .filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &snapshot,
                &target_cfg("wasm32-core", &[]),
            )
            .expect("target filtering succeeds");
        assert_eq!(result.item_tree.items.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            sai_diagnostics::Code::InvalidAttributeTarget
        );
    }

    #[test]
    fn target_filter_rejects_stale_trees_and_foreign_snapshots() {
        let source = "@target(\"wasm64-core\")\nfn selected() {}\n";
        let file = FileId::new(724);
        let (parsed, ast_ids, symbols, tree, snapshot) = collect_with_snapshot(source, file);
        let mut stale = tree.clone();
        stale.items.clear();
        assert_eq!(
            stale
                .filter_for_target(
                    source,
                    &parsed.root,
                    &ast_ids,
                    &symbols,
                    &snapshot,
                    &target_cfg("wasm64-core", &["memory64"]),
                )
                .unwrap_err(),
            TargetFilterError::StaleItemTree
        );

        let other_source = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let other_snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(other_source, "src/lib.sai", None, None)])
                .expect("another valid workspace snapshot");
        assert_eq!(
            tree.filter_for_target(
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &other_snapshot,
                &target_cfg("wasm64-core", &["memory64"]),
            )
            .unwrap_err(),
            TargetFilterError::SnapshotMismatch
        );
        assert_eq!(
            ItemTree::collect_for_target(
                file,
                source,
                &parsed.root,
                &ast_ids,
                &symbols,
                &other_snapshot,
                &target_cfg("wasm64-core", &["memory64"]),
            )
            .unwrap_err(),
            TargetFilterError::SnapshotMismatch
        );
    }

    #[test]
    fn attribute_names_cover_the_syntax_registry() {
        assert_eq!(sai_syntax::ATTRIBUTE_NAMES.len(), 17);
        for name in sai_syntax::ATTRIBUTE_NAMES {
            assert!(AttrName::from_source_name(name).is_some(), "{name}");
        }
        assert!(AttrName::from_source_name("r#derive").is_none());
    }

    #[test]
    fn invalid_and_reserved_item_names_use_the_error_symbol() {
        for (index, (source, diagnostic)) in [
            ("fn r#self() {}\n", sai_diagnostics::Code::InvalidRawIdent),
            ("fn null() {}\n", sai_diagnostics::Code::NoNull),
        ]
        .into_iter()
        .enumerate()
        {
            let (parsed, _symbols, tree) = collect(source, FileId::new(700 + index as u32));

            assert!(
                parsed.errors.iter().any(|error| error.code == diagnostic),
                "the parser reports the invalid name in {source:?}"
            );
            assert!(
                tree.items[0].name.is_error(),
                "ItemTree agrees with HIR name recovery for {source:?}"
            );
        }
    }

    #[test]
    fn collects_module_header_uses_and_nested_item_kinds_in_source_order() {
        let source = concat!(
            "//! Module docs\n",
            "unsafe module\n",
            "@target(any(\"wasm32-core\", \"wasm64-core\"))\n",
            "pub use sai:core/fmt.{Format, Debug as D}\n",
            "@derive(Clone)\n",
            "pub type Pair<T> {\n",
            "    pub first: T\n",
            "    second: T\n",
            "}\n",
            "enum State { Ready, Failed(message: Str) }\n",
            "trait Convert<T> {\n",
            "    type Output\n",
            "    fn convert(self: Self) -> T\n",
            "}\n",
            "impl<T> Convert<T> for Pair<T> {\n",
            "    type Output = T\n",
            "    fn convert(self: Self) -> T { self.first }\n",
            "}\n",
            "interface api {\n",
            "    type Id = U32\n",
            "    enum Status { Ready }\n",
            "    resource Handle { fn close(self: borrow Self) }\n",
            "    fn lookup(id: U32) -> U32\n",
            "}\n",
            "world api_world {\n",
            "    import fn now() -> U64\n",
            "    include base\n",
            "}\n",
            "extern \"wasm\" module \"env\" { fn read() -> U8 }\n",
            "test \"smoke\" {}\n",
            "property \"laws\"() {}\n",
        );
        let file = FileId::new(701);
        let (parsed, symbols, tree) = collect(source, file);

        assert!(
            parsed.errors.is_empty(),
            "fixture should be syntactically valid: {:?}",
            parsed.errors
        );
        assert!(tree.is_unsafe_module);
        let docs = tree.module_docs.as_ref().expect("module docs preserved");
        assert!(
            docs.resolve(file, &parsed.root).is_some(),
            "module docs pointer resolves in the source snapshot"
        );
        assert_eq!(tree.uses.len(), 1);
        assert_eq!(tree.uses[0].pkg_path, "sai:core/fmt");
        assert!(tree.uses[0].is_pub);
        assert_eq!(tree.uses[0].items.len(), 2);
        assert_eq!(
            &*symbols
                .resolve(tree.uses[0].items[1].alias.unwrap())
                .unwrap(),
            "D"
        );
        assert_eq!(tree.items.len(), 9);

        assert_eq!(tree.items[0].kind, ItemTreeKind::Struct);
        assert_eq!(
            child_kinds(&tree.items[0]),
            vec![
                ItemTreeKind::TypeParam,
                ItemTreeKind::Field,
                ItemTreeKind::Field
            ]
        );
        assert_eq!(tree.items[0].raw_attrs[0].name, AttrName::Derive);
        assert!(!tree.items[0].raw_attrs[0].has_error);
        assert!(
            tree.items[0].raw_attrs[0]
                .ptr
                .resolve(file, &parsed.root)
                .is_some()
        );

        assert_eq!(
            child_kinds(&tree.items[1]),
            vec![ItemTreeKind::Variant, ItemTreeKind::Variant]
        );
        assert_eq!(
            tree.items[1].children[1].children[0].kind,
            ItemTreeKind::VariantField
        );
        assert_eq!(
            child_kinds(&tree.items[2]),
            vec![
                ItemTreeKind::TypeParam,
                ItemTreeKind::AssocType,
                ItemTreeKind::AssocFn
            ]
        );
        assert_eq!(
            child_kinds(&tree.items[3]),
            vec![
                ItemTreeKind::TypeParam,
                ItemTreeKind::AssocType,
                ItemTreeKind::AssocFn
            ]
        );
        assert!(tree.items[3].header_hash.is_some());
        assert_eq!(
            child_kinds(&tree.items[4]),
            vec![
                ItemTreeKind::Alias,
                ItemTreeKind::Enum,
                ItemTreeKind::Resource,
                ItemTreeKind::InterfaceFn
            ]
        );
        assert_eq!(
            child_kinds(&tree.items[5]),
            vec![
                ItemTreeKind::WorldItem { import: true },
                ItemTreeKind::WorldInclude
            ]
        );
        assert_eq!(tree.items[6].children[0].kind, ItemTreeKind::ExternFn);
        assert_eq!(tree.items[7].kind, ItemTreeKind::Test);
        assert_eq!(tree.items[8].kind, ItemTreeKind::Property);
    }

    #[test]
    fn impl_header_hash_ignores_body_changes_and_tracks_header_changes() {
        fn hash(source: &str, file: FileId) -> u64 {
            let (parsed, _, tree) = collect(source, file);
            assert!(
                parsed.errors.is_empty(),
                "fixture should be syntactically valid: {:?}",
                parsed.errors
            );
            tree.items[0]
                .header_hash
                .expect("impl entry has a header hash")
        }

        let first = hash("impl Copy for Point { fn a() {} }\n", FileId::new(702));
        let body_edit = hash("impl Copy for Point { fn b() {} }\n", FileId::new(703));
        let header_edit = hash("impl Clone for Point { fn a() {} }\n", FileId::new(704));

        assert_eq!(first, body_edit);
        assert_ne!(first, header_edit);

        let effect_alloc = hash(
            "impl<T: Fn() effects { alloc }> Marker for T {}\n",
            FileId::new(709),
        );
        let effect_host = hash(
            "impl<T: Fn() effects { host }> Marker for T {}\n",
            FileId::new(710),
        );
        assert_ne!(effect_alloc, effect_host);
    }

    #[test]
    fn item_tree_equality_ignores_snapshot_identity_but_keeps_current_pointers() {
        let first_source = concat!(
            "//! module docs\n",
            "fn before() { 1 }\n",
            "@target(any(\"wasm32-core\"))\n",
            "fn run() { 2 }\n",
            "pub use sai:core/fmt.{Format}\n",
        );
        let second_source = concat!(
            "//! module docs\n",
            "fn before() { 123456 }\n",
            "@target(any(\"wasm32-core\"))\n",
            "fn run() { 222222 }\n",
            "pub use sai:core/fmt.{Format}\n",
        );
        let file = FileId::new(706);
        let symbols = Interner::new();

        let (first, first_ids) = parse_snapshot(first_source, file);
        let first_tree = ItemTree::collect(file, first_source, &first.root, &first_ids, &symbols)
            .expect("first item tree collects");
        let (second, second_ids) = parse_snapshot(second_source, file);
        let second_tree =
            ItemTree::collect(file, second_source, &second.root, &second_ids, &symbols)
                .expect("second item tree collects");

        assert_eq!(first_tree, second_tree);
        assert_ne!(
            first_tree.items[1].raw_attrs[0].span,
            second_tree.items[1].raw_attrs[0].span
        );
        assert_ne!(first_tree.module_docs, second_tree.module_docs);
        assert_ne!(
            first_tree.items[1].raw_attrs[0].ptr,
            second_tree.items[1].raw_attrs[0].ptr
        );
        assert_ne!(
            first_tree.uses[0].items[0].range,
            second_tree.uses[0].items[0].range
        );
        assert!(
            second_tree
                .module_docs
                .as_ref()
                .unwrap()
                .resolve(file, &second.root)
                .is_some()
        );
        assert!(
            second_tree.items[1].raw_attrs[0]
                .ptr
                .resolve(file, &second.root)
                .is_some()
        );
    }

    #[test]
    fn rejects_source_text_that_does_not_match_the_ast_snapshot() {
        let file = FileId::new(707);
        let (parsed, ast_ids) = parse_snapshot("fn first() {}\n", file);
        let symbols = Interner::new();

        assert_eq!(
            ItemTree::collect(file, "fn other() {}\n", &parsed.root, &ast_ids, &symbols)
                .map(|_| ()),
            Err(ItemTreeError::SourceMismatch)
        );
    }

    #[test]
    fn interns_identifiers_in_nfc_after_reporting_non_nfc_source() {
        let source = "fn cafe\u{301}() {}\n";
        let (parsed, symbols, tree) = collect(source, FileId::new(711));

        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "IDENT_NOT_NFC")
        );
        assert_eq!(&*symbols.resolve(tree.items[0].name).unwrap(), "café");

        let world_source = "world api { include cafe\u{301} }\n";
        let (_, world_symbols, world_tree) = collect(world_source, FileId::new(712));
        assert_eq!(
            &*world_symbols
                .resolve(world_tree.items[0].children[0].name)
                .unwrap(),
            "café"
        );
    }

    #[test]
    fn rejects_an_ast_id_map_from_another_snapshot_of_the_same_file() {
        let file = FileId::new(708);
        let (first, ast_ids) = parse_snapshot("fn first() {}\n", file);
        let (second, _) = parse_snapshot("fn second() {}\n", file);
        let symbols = Interner::new();

        assert_eq!(
            ItemTree::collect(file, "fn second() {}\n", &second.root, &ast_ids, &symbols)
                .map(|_| ()),
            Err(ItemTreeError::FileMismatch)
        );
        assert_eq!(
            ItemTree::collect(file, "fn first() {}\n", &first.root, &ast_ids, &symbols).map(|_| ()),
            Ok(())
        );
    }
}
