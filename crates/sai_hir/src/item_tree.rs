use crate::DefPathData;
use crate::attrs::{
    AttrValidationError, AttributeTarget, ExternAbi, ImplKind, TargetCfg, TargetKind,
    ValidatedAttrs, validate_attrs,
};
use sai_diagnostics::{
    Diagnostic, LintLevel as DiagnosticLintLevel, Location, OverrideLintLevel, ScopedLintLevel,
    SourceRef,
};
use sai_intern::{InternError, Interner, LookupError, Symbol};
use sai_stable_hash::{StableHash, StableHasher};
use sai_syntax::{
    GreenElement, SyntaxElement, SyntaxKind, SyntaxNode,
    ast::{self, AstNode, AstToken},
};
use sai_vfs::{FileId, Span, TextRange, WorkspaceSnapshot, WorkspaceSnapshotId};
use std::collections::{BTreeMap, BTreeSet, HashSet};

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
    /// Whether the declaration contains a `UseItems` list, including `{}`.
    pub has_items: bool,
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
    /// Documentation payload attached to this declaration, without SAI's
    /// `///` delimiters. Snapshot-independent text is retained so generated
    /// WIT can pass it directly to the canonical printer.
    pub docs: Option<String>,
    pub vis: crate::Visibility,
    pub raw_attrs: Vec<RawAttr>,
    /// Direct declarations and generic parameters, in source order.
    pub children: Vec<ItemTreeEntry>,
    /// Present for impl blocks only (§2.8.2).
    pub header_hash: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceMemberPathError {
    ItemTreeMismatch,
    TooManyDefinitions,
}

/// Returns the DefPath component used by a direct member of an interface.
///
/// This is shared by resolver collection and HIR lowering so imported
/// interface members and lowered owners use the same canonical identity.
pub fn interface_member_path_component(entry: &ItemTreeEntry) -> Option<DefPathData> {
    Some(match entry.kind {
        ItemTreeKind::Struct
        | ItemTreeKind::Alias
        | ItemTreeKind::Refined
        | ItemTreeKind::Enum
        | ItemTreeKind::Resource => DefPathData::TypeNs(entry.name),
        ItemTreeKind::InterfaceFn => DefPathData::ValueNs(entry.name),
        _ => return None,
    })
}

/// Returns the source-ordered DefPath disambiguator for a direct interface
/// member, matching the lowering identity rules.
pub fn interface_member_path_disambiguator(
    interface: &ItemTreeEntry,
    member: &ItemTreeEntry,
) -> Result<u32, InterfaceMemberPathError> {
    let component = interface_member_path_component(member)
        .ok_or(InterfaceMemberPathError::ItemTreeMismatch)?;
    let mut disambiguator = 0u32;
    for candidate in &interface.children {
        if interface_member_path_component(candidate).as_ref() != Some(&component) {
            continue;
        }
        if candidate.ast_id == member.ast_id {
            return Ok(disambiguator);
        }
        disambiguator = disambiguator
            .checked_add(1)
            .ok_or(InterfaceMemberPathError::TooManyDefinitions)?;
    }
    Err(InterfaceMemberPathError::ItemTreeMismatch)
}

#[derive(Clone, Debug)]
pub struct ItemTree {
    pub file: FileId,
    /// Snapshot that supplied the AST pointers; absent for query-local trees.
    /// Ignored by structural equality.
    snapshot_id: Option<WorkspaceSnapshotId>,
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

/// The snapshot-independent semantic shape of an item tree.
///
/// Source ranges, syntax pointers, file IDs, and interner-local symbols are
/// intentionally absent. This makes the value suitable for stable query
/// fingerprints; snapshot-bound trees must still be collected or rebound for
/// source navigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableItemTree {
    has_module_docs: bool,
    module_docs_text: Option<String>,
    is_unsafe_module: bool,
    uses: Vec<StableUseEntry>,
    items: Vec<StableItemTreeEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableUseEntry {
    ast_id: ErasedAstId,
    is_pub: bool,
    pkg_path: String,
    alias: Option<StableSymbol>,
    has_items: bool,
    items: Vec<StableUseItem>,
    raw_attrs: Vec<StableRawAttr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableUseItem {
    name: StableSymbol,
    alias: Option<StableSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableItemTreeEntry {
    ast_id: ErasedAstId,
    kind: ItemTreeKind,
    name: StableSymbol,
    docs: Option<String>,
    vis: crate::Visibility,
    raw_attrs: Vec<StableRawAttr>,
    children: Vec<StableItemTreeEntry>,
    header_hash: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableRawAttr {
    name: AttrName,
    args: Option<Vec<StableRawAttrArg>>,
    has_error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StableRawAttrArg {
    Word {
        name: StableSymbol,
        raw: bool,
        is_upper: bool,
        args: Option<Vec<StableRawAttrArg>>,
    },
    Str(String),
    Int(Option<u128>),
    KeyValue {
        key: StableSymbol,
        raw: bool,
        value: StableRawAttrValue,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StableRawAttrValue {
    Str(String),
    Int(Option<u128>),
    Word(StableSymbol),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StableSymbol {
    Error,
    Name(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StableItemTreeError {
    Interner(LookupError),
}

impl StableItemTree {
    fn from_item_tree(tree: &ItemTree, symbols: &Interner) -> Result<Self, StableItemTreeError> {
        Ok(Self {
            has_module_docs: tree.module_docs.is_some(),
            module_docs_text: tree.module_docs_text.clone(),
            is_unsafe_module: tree.is_unsafe_module,
            uses: tree
                .uses
                .iter()
                .map(|entry| StableUseEntry::from_entry(entry, symbols))
                .collect::<Result<_, _>>()?,
            items: tree
                .items
                .iter()
                .map(|entry| StableItemTreeEntry::from_entry(entry, symbols))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Rebuilds snapshot-local pointers and spans from this stable shape.
    ///
    /// The stable shape must describe this source's item-tree semantics; it may
    /// be reused after edits that leave those semantics unchanged. The binder
    /// verifies each stored AST ID, module header, use declaration, and raw
    /// attribute before exposing the rebound tree.
    pub(crate) fn bind_to_snapshot(
        &self,
        file: FileId,
        source: &str,
        root: &SyntaxNode,
        ast_ids: &ast::AstIdMap,
        symbols: &Interner,
    ) -> Result<ItemTree, ItemTreeError> {
        if !ast_ids.matches_root(file, root) {
            return Err(ItemTreeError::FileMismatch);
        }
        if !green_text_matches_source(root, source) {
            return Err(ItemTreeError::SourceMismatch);
        }

        let expected_id_count = self
            .uses
            .len()
            .checked_add(count_entry_ids(&self.items))
            .ok_or(ItemTreeError::StableShapeMismatch)?;
        let mut seen_ids = HashSet::with_capacity(expected_id_count);
        // ItemTree is a projection of AstIdMap, not a complete copy. In
        // particular, the function syntax inside a world import is represented
        // by the enclosing world-item entry, so unrecorded IDs are permitted.

        let source_file = ast::SourceFile::cast(root.clone()).ok_or(ItemTreeError::FileMismatch)?;
        let mut module_docs = None;
        let mut module_docs_text = None;
        let mut is_unsafe_module = false;
        if let Some(header) = source_file.module_header() {
            is_unsafe_module = header.unsafe_token().is_some();
            if let Some(docs) = header.docs() {
                module_docs_text = Some(node_text(source, docs.syntax())?.to_owned());
                module_docs = Some(
                    ast::AstPtr::new(file, &docs).map_err(|_| ItemTreeError::InvalidAstPointer)?,
                );
            }
        }
        if self.has_module_docs != module_docs.is_some()
            || self.module_docs_text != module_docs_text
            || self.is_unsafe_module != is_unsafe_module
        {
            return Err(ItemTreeError::StableShapeMismatch);
        }

        let mut uses = Vec::with_capacity(self.uses.len());
        for expected in &self.uses {
            check_shape_ast_id(expected.ast_id, ast_ids, &mut seen_ids)?;
            let syntax = resolve_shape_ast_id(expected.ast_id, file, root, ast_ids)?;
            let declaration =
                ast::UseDecl::cast(syntax).ok_or(ItemTreeError::StableShapeMismatch)?;
            let entry = collect_use(file, source, &declaration, ast_ids, symbols)?;
            let actual =
                StableUseEntry::from_entry(&entry, symbols).map_err(ItemTreeError::StableShape)?;
            if &actual != expected {
                return Err(ItemTreeError::StableShapeMismatch);
            }
            uses.push(entry);
        }

        let mut items = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            items.push(bind_stable_entry(
                entry,
                file,
                source,
                root,
                ast_ids,
                symbols,
                &mut seen_ids,
            )?);
        }
        if seen_ids.len() != expected_id_count {
            return Err(ItemTreeError::StableShapeMismatch);
        }

        Ok(ItemTree {
            file,
            snapshot_id: ast_ids.snapshot_id(),
            module_docs,
            module_docs_text,
            is_unsafe_module,
            uses,
            items,
        })
    }
}

fn count_entry_ids(entries: &[StableItemTreeEntry]) -> usize {
    entries
        .iter()
        .map(|entry| 1 + count_entry_ids(&entry.children))
        .sum()
}

fn check_shape_ast_id(
    id: ErasedAstId,
    ast_ids: &ast::AstIdMap,
    seen_ids: &mut HashSet<u32>,
) -> Result<(), ItemTreeError> {
    let index = id.index();
    if usize::try_from(index).map_or(true, |index| index >= ast_ids.len())
        || !seen_ids.insert(index)
        || ast_ids.pointer(id).is_none()
    {
        return Err(ItemTreeError::StableShapeMismatch);
    }
    Ok(())
}

fn resolve_shape_ast_id(
    id: ErasedAstId,
    file: FileId,
    root: &SyntaxNode,
    ast_ids: &ast::AstIdMap,
) -> Result<SyntaxNode, ItemTreeError> {
    ast_ids
        .pointer(id)
        .and_then(|pointer| pointer.resolve(file, root))
        .ok_or(ItemTreeError::StableShapeMismatch)
}

fn bind_stable_entry(
    stable: &StableItemTreeEntry,
    file: FileId,
    source: &str,
    root: &SyntaxNode,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
    seen_ids: &mut HashSet<u32>,
) -> Result<ItemTreeEntry, ItemTreeError> {
    check_shape_ast_id(stable.ast_id, ast_ids, seen_ids)?;
    let syntax = resolve_shape_ast_id(stable.ast_id, file, root, ast_ids)?;
    let raw_attrs = raw_attrs(
        file,
        source,
        syntax.children().filter_map(ast::Attribute::cast),
        symbols,
    )?;
    let docs = declaration_docs(source, &syntax)?;
    if docs != stable.docs {
        return Err(ItemTreeError::StableShapeMismatch);
    }
    if stable_raw_attrs(&raw_attrs, symbols).map_err(ItemTreeError::StableShape)?
        != stable.raw_attrs
    {
        return Err(ItemTreeError::StableShapeMismatch);
    }

    let children = stable
        .children
        .iter()
        .map(|child| bind_stable_entry(child, file, source, root, ast_ids, symbols, seen_ids))
        .collect::<Result<_, _>>()?;
    Ok(ItemTreeEntry {
        ast_id: stable.ast_id,
        kind: stable.kind,
        name: stable.name.intern(symbols)?,
        docs,
        vis: stable.vis,
        raw_attrs,
        children,
        header_hash: stable.header_hash,
    })
}

impl StableUseEntry {
    fn from_entry(entry: &UseEntry, symbols: &Interner) -> Result<Self, StableItemTreeError> {
        Ok(Self {
            ast_id: entry.ast_id,
            is_pub: entry.is_pub,
            pkg_path: entry.pkg_path.clone(),
            alias: entry
                .alias
                .map(|symbol| stable_symbol(symbol, symbols))
                .transpose()?,
            has_items: entry.has_items,
            items: entry
                .items
                .iter()
                .map(|item| {
                    Ok(StableUseItem {
                        name: stable_symbol(item.name, symbols)?,
                        alias: item
                            .alias
                            .map(|symbol| stable_symbol(symbol, symbols))
                            .transpose()?,
                    })
                })
                .collect::<Result<_, StableItemTreeError>>()?,
            raw_attrs: stable_raw_attrs(&entry.raw_attrs, symbols)?,
        })
    }
}

impl StableItemTreeEntry {
    fn from_entry(entry: &ItemTreeEntry, symbols: &Interner) -> Result<Self, StableItemTreeError> {
        Ok(Self {
            ast_id: entry.ast_id,
            kind: entry.kind,
            name: stable_symbol(entry.name, symbols)?,
            docs: entry.docs.clone(),
            vis: entry.vis,
            raw_attrs: stable_raw_attrs(&entry.raw_attrs, symbols)?,
            children: entry
                .children
                .iter()
                .map(|child| Self::from_entry(child, symbols))
                .collect::<Result<_, _>>()?,
            header_hash: entry.header_hash,
        })
    }
}

fn stable_raw_attrs(
    attrs: &[RawAttr],
    symbols: &Interner,
) -> Result<Vec<StableRawAttr>, StableItemTreeError> {
    attrs
        .iter()
        .map(|attr| {
            Ok(StableRawAttr {
                name: attr.name,
                args: attr
                    .args
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .map(|arg| StableRawAttrArg::from_arg(arg, symbols))
                            .collect::<Result<_, _>>()
                    })
                    .transpose()?,
                has_error: attr.has_error,
            })
        })
        .collect()
}

impl StableRawAttrArg {
    fn from_arg(arg: &RawAttrArg, symbols: &Interner) -> Result<Self, StableItemTreeError> {
        Ok(match arg {
            RawAttrArg::Word {
                name,
                raw,
                is_upper,
                args,
                ..
            } => Self::Word {
                name: stable_symbol(*name, symbols)?,
                raw: *raw,
                is_upper: *is_upper,
                args: args
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .map(|arg| Self::from_arg(arg, symbols))
                            .collect::<Result<_, _>>()
                    })
                    .transpose()?,
            },
            RawAttrArg::Str { value, .. } => Self::Str(value.clone()),
            RawAttrArg::Int { value, .. } => Self::Int(*value),
            RawAttrArg::KeyValue {
                key, raw, value, ..
            } => Self::KeyValue {
                key: stable_symbol(*key, symbols)?,
                raw: *raw,
                value: StableRawAttrValue::from_value(value, symbols)?,
            },
        })
    }
}

impl StableRawAttrValue {
    fn from_value(value: &RawAttrValue, symbols: &Interner) -> Result<Self, StableItemTreeError> {
        Ok(match value {
            RawAttrValue::Str(value) => Self::Str(value.clone()),
            RawAttrValue::Int(value) => Self::Int(*value),
            RawAttrValue::Word(symbol) => Self::Word(stable_symbol(*symbol, symbols)?),
        })
    }
}

impl StableSymbol {
    fn intern(&self, symbols: &Interner) -> Result<Symbol, ItemTreeError> {
        match self {
            Self::Error => Ok(Symbol::ERROR),
            Self::Name(name) => symbols.intern(name).map_err(ItemTreeError::Interner),
        }
    }
}

fn stable_symbol(symbol: Symbol, symbols: &Interner) -> Result<StableSymbol, StableItemTreeError> {
    if symbol.is_error() {
        return Ok(StableSymbol::Error);
    }
    symbols
        .resolve(symbol)
        .map(|name| StableSymbol::Name(name.to_string()))
        .map_err(StableItemTreeError::Interner)
}

impl StableHash for StableItemTree {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD0);
        self.has_module_docs.stable_hash(hasher);
        stable_hash_option(&self.module_docs_text, hasher);
        self.is_unsafe_module.stable_hash(hasher);
        self.uses.stable_hash(hasher);
        self.items.stable_hash(hasher);
    }
}

impl StableHash for StableUseEntry {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xDA);
        self.ast_id.index().stable_hash(hasher);
        self.is_pub.stable_hash(hasher);
        self.pkg_path.stable_hash(hasher);
        stable_hash_option(&self.alias, hasher);
        self.has_items.stable_hash(hasher);
        self.items.stable_hash(hasher);
        self.raw_attrs.stable_hash(hasher);
    }
}

impl StableHash for StableUseItem {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD2);
        self.name.stable_hash(hasher);
        stable_hash_option(&self.alias, hasher);
    }
}

impl StableHash for StableItemTreeEntry {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD3);
        self.ast_id.index().stable_hash(hasher);
        stable_hash_item_kind(self.kind, hasher);
        self.name.stable_hash(hasher);
        stable_hash_option(&self.docs, hasher);
        stable_hash_visibility(self.vis, hasher);
        self.raw_attrs.stable_hash(hasher);
        self.children.stable_hash(hasher);
        stable_hash_option(&self.header_hash, hasher);
    }
}

impl StableHash for StableRawAttr {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD4);
        self.name.as_str().stable_hash(hasher);
        stable_hash_option(&self.args, hasher);
        self.has_error.stable_hash(hasher);
    }
}

impl StableHash for StableRawAttrArg {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD5);
        match self {
            Self::Word {
                name,
                raw,
                is_upper,
                args,
            } => {
                hasher.write_tag(0);
                name.stable_hash(hasher);
                raw.stable_hash(hasher);
                is_upper.stable_hash(hasher);
                stable_hash_option(args, hasher);
            }
            Self::Str(value) => {
                hasher.write_tag(1);
                value.stable_hash(hasher);
            }
            Self::Int(value) => {
                hasher.write_tag(2);
                stable_hash_option(value, hasher);
            }
            Self::KeyValue { key, raw, value } => {
                hasher.write_tag(3);
                key.stable_hash(hasher);
                raw.stable_hash(hasher);
                value.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for StableRawAttrValue {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD6);
        match self {
            Self::Str(value) => {
                hasher.write_tag(0);
                value.stable_hash(hasher);
            }
            Self::Int(value) => {
                hasher.write_tag(1);
                stable_hash_option(value, hasher);
            }
            Self::Word(value) => {
                hasher.write_tag(2);
                value.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for StableSymbol {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD7);
        match self {
            Self::Error => hasher.write_tag(0),
            Self::Name(name) => {
                hasher.write_tag(1);
                name.stable_hash(hasher);
            }
        }
    }
}

fn stable_hash_item_kind(kind: ItemTreeKind, hasher: &mut StableHasher) {
    hasher.write_tag(0xD8);
    let (tag, payload): (u8, Option<bool>) = match kind {
        ItemTreeKind::Function => (0, None),
        ItemTreeKind::Struct => (1, None),
        ItemTreeKind::Alias => (2, None),
        ItemTreeKind::Refined => (3, None),
        ItemTreeKind::Enum => (4, None),
        ItemTreeKind::Distinct => (5, None),
        ItemTreeKind::Resource => (6, None),
        ItemTreeKind::Trait => (7, None),
        ItemTreeKind::Impl => (8, None),
        ItemTreeKind::Const => (9, None),
        ItemTreeKind::Invariant => (10, None),
        ItemTreeKind::Effect => (11, None),
        ItemTreeKind::Interface => (12, None),
        ItemTreeKind::World => (13, None),
        ItemTreeKind::ExternBlock => (14, None),
        ItemTreeKind::Test => (15, None),
        ItemTreeKind::Property => (16, None),
        ItemTreeKind::TypeParam => (17, None),
        ItemTreeKind::ConstParam => (18, None),
        ItemTreeKind::Field => (19, None),
        ItemTreeKind::Variant => (20, None),
        ItemTreeKind::VariantField => (21, None),
        ItemTreeKind::AssocType => (22, None),
        ItemTreeKind::AssocFn => (23, None),
        ItemTreeKind::InterfaceFn => (24, None),
        ItemTreeKind::ExternFn => (25, None),
        ItemTreeKind::WorldItem { import } => (26, Some(import)),
        ItemTreeKind::WorldInclude => (27, None),
    };
    tag.stable_hash(hasher);
    if let Some(payload) = payload {
        payload.stable_hash(hasher);
    }
}

fn stable_hash_visibility(visibility: crate::Visibility, hasher: &mut StableHasher) {
    hasher.write_tag(0xD9);
    match visibility {
        crate::Visibility::Private => hasher.write_tag(0),
        crate::Visibility::Pub => hasher.write_tag(1),
        crate::Visibility::Export => hasher.write_tag(2),
    }
}

fn stable_hash_option<T: StableHash>(value: &Option<T>, hasher: &mut StableHasher) {
    match value {
        None => hasher.write_tag(0),
        Some(value) => {
            hasher.write_tag(1);
            value.stable_hash(hasher);
        }
    }
}

/// A target-filtered item tree and attribute results for retained entries.
#[derive(Clone, Debug)]
pub struct TargetFilterResult {
    pub item_tree: ItemTree,
    /// Validated attributes for every retained item-tree entry and `use`.
    pub validated_attrs: BTreeMap<ErasedAstId, ValidatedAttrs>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintScopeCollectionError {
    StaleItemTree,
    SnapshotMismatch,
    MissingValidatedAttrs,
    UnexpectedValidatedAttrs,
    MissingAstNode,
    InvalidSourceRange,
    ForeignSymbol,
    InvalidLintAttributeScope,
    InvalidLintLevel,
}

impl TargetFilterResult {
    /// Builds source-bound lint attribute scopes for the retained item tree.
    ///
    /// Each scope runs from the first outer doc comment or attribute through
    /// the declaration's last token. The returned spans use original-file
    /// offsets, matching diagnostic locations.
    pub fn scoped_lint_levels(
        &self,
        ast_ids: &ast::AstIdMap,
        root: &SyntaxNode,
        symbols: &Interner,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<ScopedLintLevel>, LintScopeCollectionError> {
        let file = self.item_tree.file;
        if !self.item_tree.matches_ast_ids(ast_ids) || !ast_ids.matches_root(file, root) {
            return Err(LintScopeCollectionError::StaleItemTree);
        }
        if !ast_ids.matches_snapshot(snapshot) || self.item_tree.snapshot_id != Some(snapshot.id())
        {
            return Err(LintScopeCollectionError::SnapshotMismatch);
        }

        let snapshot_file = snapshot
            .file(file)
            .ok_or(LintScopeCollectionError::SnapshotMismatch)?;
        let source = snapshot_file.source();
        if !green_text_matches_source(root, source.text()) {
            return Err(LintScopeCollectionError::SnapshotMismatch);
        }
        let source_ref = SourceRef {
            path: snapshot_file.path().to_owned(),
            package: snapshot_file.package().map(str::to_owned),
            snapshot: snapshot_file.document_version(),
        };

        let mut seen_ids = BTreeSet::new();
        let mut scopes = Vec::new();
        for entry in &self.item_tree.uses {
            collect_lint_scopes_for_id(
                entry.ast_id,
                &self.validated_attrs,
                ast_ids,
                root,
                source_ref.clone(),
                source,
                symbols,
                &mut seen_ids,
                &mut scopes,
            )?;
        }
        collect_entry_lint_scopes(
            &self.item_tree.items,
            &self.validated_attrs,
            ast_ids,
            root,
            &source_ref,
            source,
            symbols,
            &mut seen_ids,
            &mut scopes,
        )?;

        if seen_ids.len() != self.validated_attrs.len()
            || self
                .validated_attrs
                .keys()
                .any(|ast_id| !seen_ids.contains(ast_id))
        {
            return Err(LintScopeCollectionError::UnexpectedValidatedAttrs);
        }

        scopes.sort_by(|left, right| {
            let left_range = left.syntax_range().range;
            let right_range = right.syntax_range().range;
            left_range
                .start()
                .cmp(&right_range.start())
                .then_with(|| right_range.end().cmp(&left_range.end()))
                .then_with(|| {
                    left.location()
                        .span
                        .range
                        .start()
                        .cmp(&right.location().span.range.start())
                })
                .then_with(|| left.code().cmp(right.code()))
        });
        Ok(scopes)
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_entry_lint_scopes(
    entries: &[ItemTreeEntry],
    validated_attrs: &BTreeMap<ErasedAstId, ValidatedAttrs>,
    ast_ids: &ast::AstIdMap,
    root: &SyntaxNode,
    source_ref: &SourceRef,
    source: &sai_vfs::SourceFile,
    symbols: &Interner,
    seen_ids: &mut BTreeSet<ErasedAstId>,
    scopes: &mut Vec<ScopedLintLevel>,
) -> Result<(), LintScopeCollectionError> {
    for entry in entries {
        collect_lint_scopes_for_id(
            entry.ast_id,
            validated_attrs,
            ast_ids,
            root,
            source_ref.clone(),
            source,
            symbols,
            seen_ids,
            scopes,
        )?;
        collect_entry_lint_scopes(
            &entry.children,
            validated_attrs,
            ast_ids,
            root,
            source_ref,
            source,
            symbols,
            seen_ids,
            scopes,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_lint_scopes_for_id(
    ast_id: ErasedAstId,
    validated_attrs: &BTreeMap<ErasedAstId, ValidatedAttrs>,
    ast_ids: &ast::AstIdMap,
    root: &SyntaxNode,
    source_ref: SourceRef,
    source: &sai_vfs::SourceFile,
    symbols: &Interner,
    seen_ids: &mut BTreeSet<ErasedAstId>,
    scopes: &mut Vec<ScopedLintLevel>,
) -> Result<(), LintScopeCollectionError> {
    if !seen_ids.insert(ast_id) {
        return Err(LintScopeCollectionError::UnexpectedValidatedAttrs);
    }
    let validated = validated_attrs
        .get(&ast_id)
        .ok_or(LintScopeCollectionError::MissingValidatedAttrs)?;
    if validated.lint_levels.is_empty() {
        return Ok(());
    }

    let syntax = ast_ids
        .pointer(ast_id)
        .ok_or(LintScopeCollectionError::MissingAstNode)?
        .resolve(ast_ids.file(), root)
        .ok_or(LintScopeCollectionError::MissingAstNode)?;
    let decoded_scope = Span::new(ast_ids.file(), syntax.text_range());
    let scope_location = Location::from_decoded_source(source_ref.clone(), decoded_scope, source)
        .ok_or(LintScopeCollectionError::InvalidSourceRange)?;

    for lint in &validated.lint_levels {
        let level = match lint.level {
            DiagnosticLintLevel::Allow => OverrideLintLevel::Allow,
            DiagnosticLintLevel::Warn => OverrideLintLevel::Warn,
            DiagnosticLintLevel::Deny => OverrideLintLevel::Deny,
            DiagnosticLintLevel::Info => return Err(LintScopeCollectionError::InvalidLintLevel),
        };
        let name = symbols
            .resolve(lint.name)
            .map_err(|_| LintScopeCollectionError::ForeignSymbol)?;
        let location = Location::from_decoded_source(source_ref.clone(), lint.span, source)
            .ok_or(LintScopeCollectionError::InvalidSourceRange)?;
        let scoped = ScopedLintLevel::new(name.as_ref(), level, scope_location.span, location)
            .ok_or(LintScopeCollectionError::InvalidLintAttributeScope)?;
        scopes.push(scoped);
    }
    Ok(())
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
    StableShape(StableItemTreeError),
    StableShapeMismatch,
}

impl ItemTree {
    /// Whether this item tree was collected from the AST map's exact snapshot.
    pub fn matches_ast_ids(&self, ast_ids: &ast::AstIdMap) -> bool {
        self.file == ast_ids.file()
            && self.snapshot_id.is_some()
            && ast_ids.snapshot_id() == self.snapshot_id
    }

    /// Returns the stable semantic shape without snapshot-local source locations.
    ///
    /// The supplied interner must own every non-error symbol in this tree.
    pub fn stable_shape(&self, symbols: &Interner) -> Result<StableItemTree, StableItemTreeError> {
        StableItemTree::from_item_tree(self, symbols)
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
        let snapshot_id = ast_ids.snapshot_id();
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
            || current.snapshot_id != Some(snapshot.id())
            || snapshot
                .file(file)
                .is_none_or(|snapshot_file| snapshot_file.source().text() != source)
        {
            return Err(TargetFilterError::SnapshotMismatch);
        }
        filter_collected_tree(current, root, ast_ids, symbols, snapshot, target_cfg)
    }

    /// Removes module-level entries whose valid `@target` predicate is false.
    ///
    /// The item tree is checked against the current AST map, source, and
    /// workspace snapshot before its attributes are used. `@target` is
    /// validated first; other attributes are validated only for retained
    /// declarations.
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
            || self.snapshot_id != Some(snapshot.id())
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

        filter_collected_tree(current, root, ast_ids, symbols, snapshot, target_cfg)
    }
}

fn filter_collected_tree(
    current: ItemTree,
    root: &SyntaxNode,
    ast_ids: &ast::AstIdMap,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    target_cfg: &TargetCfg,
) -> Result<TargetFilterResult, TargetFilterError> {
    let mut attribute_validation =
        ItemAttributeValidation::new(current.file, root, ast_ids, symbols, snapshot);
    let mut uses = Vec::with_capacity(current.uses.len());
    for entry in current.uses.iter().cloned() {
        let target = AttributeTarget {
            module_level: true,
            ..AttributeTarget::new(TargetKind::Use)
        };
        let target_attrs = validate_target_attrs(&entry.raw_attrs, target, symbols, snapshot)?;
        if !target_matches(&target_attrs, target_cfg) {
            continue;
        }
        attribute_validation.validate_raw(entry.ast_id, &entry.raw_attrs, target)?;
        uses.push(entry);
    }

    let mut items = Vec::with_capacity(current.items.len());
    for entry in current.items.iter().cloned() {
        if module_target_kind(entry.kind).is_none() {
            return Err(TargetFilterError::StaleItemTree);
        }
        let target = attribute_validation
            .target_for_entry(&entry, true)?
            .ok_or(TargetFilterError::StaleItemTree)?;
        let target_attrs = validate_target_attrs(&entry.raw_attrs, target, symbols, snapshot)?;
        if !target_matches(&target_attrs, target_cfg) {
            continue;
        }
        attribute_validation.validate_entry_tree(&entry, Some(target))?;
        items.push(entry);
    }
    attribute_validation.diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start())
    });

    let mut item_tree = current;
    item_tree.uses = uses;
    item_tree.items = items;
    Ok(TargetFilterResult {
        item_tree,
        validated_attrs: attribute_validation.validated_attrs,
        diagnostics: attribute_validation.diagnostics,
    })
}

struct ItemAttributeValidation<'a> {
    file: FileId,
    root: &'a SyntaxNode,
    ast_ids: &'a ast::AstIdMap,
    symbols: &'a Interner,
    snapshot: &'a WorkspaceSnapshot,
    validated_attrs: BTreeMap<ErasedAstId, ValidatedAttrs>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> ItemAttributeValidation<'a> {
    fn new(
        file: FileId,
        root: &'a SyntaxNode,
        ast_ids: &'a ast::AstIdMap,
        symbols: &'a Interner,
        snapshot: &'a WorkspaceSnapshot,
    ) -> Self {
        Self {
            file,
            root,
            ast_ids,
            symbols,
            snapshot,
            validated_attrs: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }

    fn target_for_entry(
        &self,
        entry: &ItemTreeEntry,
        module_level: bool,
    ) -> Result<Option<AttributeTarget>, TargetFilterError> {
        entry_attribute_target(entry, module_level, self.file, self.root, self.ast_ids)
    }

    fn validate_raw(
        &mut self,
        ast_id: ErasedAstId,
        raw_attrs: &[RawAttr],
        target: AttributeTarget,
    ) -> Result<(), TargetFilterError> {
        let validated = validate_module_attrs(raw_attrs, target, self.symbols, self.snapshot)?;
        self.diagnostics
            .extend(validated.diagnostics.iter().cloned());
        self.validated_attrs.insert(ast_id, validated);
        Ok(())
    }

    fn validate_entry_tree(
        &mut self,
        entry: &ItemTreeEntry,
        target: Option<AttributeTarget>,
    ) -> Result<(), TargetFilterError> {
        if let Some(target) = target {
            self.validate_raw(entry.ast_id, &entry.raw_attrs, target)?;
        } else {
            // The parser owns the single INVALID_ATTRIBUTE_TARGET diagnostic
            // for positions where the grammar prohibits attributes.
            self.validated_attrs
                .insert(entry.ast_id, ValidatedAttrs::default());
        }
        for child in &entry.children {
            let target = self.target_for_entry(child, false)?;
            self.validate_entry_tree(child, target)?;
        }
        Ok(())
    }
}

/// Derives the complete attribute target for one item-tree entry.
///
/// This is shared with narrow later-stage validators that must diagnose an
/// attribute on its actual syntactic target without enabling general
/// validation for that stage.
pub fn entry_attribute_target(
    entry: &ItemTreeEntry,
    module_level: bool,
    file: FileId,
    root: &SyntaxNode,
    ast_ids: &ast::AstIdMap,
) -> Result<Option<AttributeTarget>, TargetFilterError> {
    let syntax = ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(file, root))
        .ok_or(TargetFilterError::StaleItemTree)?;
    let mut ancestors = Vec::new();
    let mut parent = syntax.parent();
    while let Some(node) = parent {
        parent = node.parent();
        ancestors.push(node);
    }
    let containing_owner = || {
        ancestors.iter().find(|node| {
            matches!(
                node.kind(),
                SyntaxKind::TRAIT_DECL
                    | SyntaxKind::IMPL_DECL
                    | SyntaxKind::INTERFACE_DECL
                    | SyntaxKind::IFACE_RESOURCE
                    | SyntaxKind::EXTERN_BLOCK
            )
        })
    };
    let kind = match entry.kind {
        ItemTreeKind::Function if module_level => TargetKind::Fn,
        ItemTreeKind::Function | ItemTreeKind::AssocFn => {
            match containing_owner().map(SyntaxNode::kind) {
                Some(SyntaxKind::TRAIT_DECL) => TargetKind::TraitFn,
                Some(SyntaxKind::IMPL_DECL) => TargetKind::ImplFn,
                Some(SyntaxKind::INTERFACE_DECL | SyntaxKind::IFACE_RESOURCE) => {
                    TargetKind::InterfaceFn
                }
                Some(SyntaxKind::EXTERN_BLOCK) => TargetKind::ExternFn,
                _ => return Err(TargetFilterError::StaleItemTree),
            }
        }
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
        | ItemTreeKind::VariantField
        | ItemTreeKind::WorldItem { .. }
        | ItemTreeKind::WorldInclude => return Ok(None),
        ItemTreeKind::Field => TargetKind::Field,
        ItemTreeKind::Variant => TargetKind::Variant,
        ItemTreeKind::AssocType => TargetKind::AssocType,
        ItemTreeKind::InterfaceFn => TargetKind::InterfaceFn,
        ItemTreeKind::ExternFn => TargetKind::ExternFn,
    };
    let mut target = AttributeTarget {
        module_level,
        is_exported: module_level && entry.vis == crate::Visibility::Export,
        ..AttributeTarget::new(kind)
    };
    if matches!(
        kind,
        TargetKind::Fn | TargetKind::TraitFn | TargetKind::ImplFn
    ) {
        let function = ast::FnDecl::cast(syntax).ok_or(TargetFilterError::StaleItemTree)?;
        target.has_body = function.body().is_some();
    }
    target.inside_interface = ancestors
        .iter()
        .any(|node| node.kind() == SyntaxKind::INTERFACE_DECL);
    target.assoc_type_in_trait = kind == TargetKind::AssocType
        && ancestors
            .iter()
            .any(|node| node.kind() == SyntaxKind::TRAIT_DECL);
    if let Some(impl_node) = ancestors
        .iter()
        .find(|node| node.kind() == SyntaxKind::IMPL_DECL)
        .and_then(|node| ast::ImplDecl::cast(node.clone()))
    {
        target.impl_kind = Some(if impl_node.interface_path().is_some() {
            ImplKind::Interface
        } else if impl_node
            .syntax()
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::FOR_KW)
        {
            ImplKind::Trait
        } else {
            ImplKind::Inherent
        });
    }
    if let Some(extern_node) = ancestors
        .iter()
        .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
    {
        target.extern_abi = ast::ExternBlock::cast(extern_node.clone())
            .and_then(|block| block.abi())
            .map(|abi| match abi.value().as_ref() {
                "wasm" => ExternAbi::Wasm,
                "C" => ExternAbi::C,
                _ => ExternAbi::Other,
            });
        target.extern_unsafe = extern_node
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::UNSAFE_KW);
    }
    Ok(Some(target))
}

fn validate_module_attrs(
    raw_attrs: &[RawAttr],
    target: AttributeTarget,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
) -> Result<ValidatedAttrs, TargetFilterError> {
    validate_attrs(raw_attrs, Some(target), symbols, snapshot)
        .map_err(TargetFilterError::AttributeValidation)
}

fn validate_target_attrs(
    raw_attrs: &[RawAttr],
    target: AttributeTarget,
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
    validate_module_attrs(&target_attrs, target, symbols, snapshot)
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
        has_items: node.items().is_some(),
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
        docs: declaration_docs(source, node.syntax())?,
        vis,
        raw_attrs: raw_attrs(file, source, attrs, symbols)?,
        children: Vec::new(),
        header_hash,
    })
}

fn declaration_docs(source: &str, syntax: &SyntaxNode) -> Result<Option<String>, ItemTreeError> {
    let Some(docs) = syntax.children().find_map(ast::Docs::cast) else {
        return Ok(None);
    };
    let raw = node_text(source, docs.syntax())?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let payload = raw
        .lines()
        .map(|line| {
            let line = line.trim_start();
            line.strip_prefix("///")
                .map(|content| content.strip_prefix(' ').unwrap_or(content))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some(payload))
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
    if raw_name == Some("_") || (raw_name.is_none() && sai_syntax::is_reserved_identifier(text)) {
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
        assert_eq!(result.validated_attrs.len(), 2);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn target_collection_returns_full_attributes_with_function_context() {
        let source = concat!(
            "@inline\n",
            "fn with_body() {}\n",
            "@inline\n",
            "fn without_body() -> U32\n",
            "@export_name(\"entry\")\n",
            "export fn exported() {}\n",
        );
        let file = FileId::new(726);
        let (parsed, ast_ids, symbols, _, snapshot) = collect_with_snapshot(source, file);

        let result = ItemTree::collect_for_target(
            file,
            source,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg("wasm32-core", &[]),
        )
        .expect("target collection succeeds");
        let inline_for = |name: &str| {
            let entry = result
                .item_tree
                .items
                .iter()
                .find(|entry| symbols.resolve(entry.name).unwrap().as_ref() == name)
                .expect("function retained");
            result
                .validated_attrs
                .get(&entry.ast_id)
                .expect("attributes validated")
        };
        assert!(inline_for("with_body").inline.is_some());
        assert!(inline_for("without_body").inline.is_none());
        let exported = result
            .item_tree
            .items
            .iter()
            .find(|entry| symbols.resolve(entry.name).unwrap().as_ref() == "exported")
            .expect("exported function retained");
        let exported_context = entry_attribute_target(exported, true, file, &parsed.root, &ast_ids)
            .expect("exported function context resolves")
            .expect("function has an attribute target");
        assert!(exported_context.is_exported);
        assert!(exported_context.has_body);
        let private_context = entry_attribute_target(
            result
                .item_tree
                .items
                .iter()
                .find(|entry| symbols.resolve(entry.name).unwrap().as_ref() == "with_body")
                .expect("private function retained"),
            true,
            file,
            &parsed.root,
            &ast_ids,
        )
        .expect("private function context resolves")
        .expect("function has an attribute target");
        assert!(!private_context.is_exported);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget
        }));
    }

    #[test]
    fn retained_attributes_produce_original_source_lint_scopes() {
        let source = concat!(
            "/// record documentation\n",
            "@allow(UNUSED_VARIABLE)\n",
            "type Record {\n",
            "    @deny(UNUSED_VARIABLE)\n",
            "    field: U32,\n",
            "}\n",
            "@warn(UNUSED_VARIABLE)\n",
            "use sai:core/fmt.{Format}\n",
            "@target(\"wasm64-core\")\n",
            "@warn(UNUSED_VARIABLE)\n",
            "fn removed() {}\n",
        );
        let file = FileId::new(730);
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
        .expect("target collection succeeds");
        let scopes = result
            .scoped_lint_levels(&ast_ids, &parsed.root, &symbols, &snapshot)
            .expect("scopes come from the retained validated attributes");
        assert_eq!(result.item_tree.uses.len(), 1);
        assert_eq!(scopes.len(), 3);
        assert_eq!(
            scopes.iter().map(|scope| scope.level()).collect::<Vec<_>>(),
            [
                OverrideLintLevel::Allow,
                OverrideLintLevel::Deny,
                OverrideLintLevel::Warn
            ]
        );

        let record_start = source.find("/// record documentation").unwrap();
        let record_end = source.find("}\n@warn").unwrap() + 1;
        assert_eq!(
            scopes[0].syntax_range().range,
            TextRange::new(
                sai_vfs::TextSize::new(record_start as u32),
                sai_vfs::TextSize::new(record_end as u32),
            )
            .unwrap()
        );
        let field_attribute_start = source.find("@deny(UNUSED_VARIABLE)").unwrap();
        let field_end = source.find("field: U32").unwrap() + "field: U32".len();
        assert_eq!(
            scopes[1].syntax_range().range,
            TextRange::new(
                sai_vfs::TextSize::new(field_attribute_start as u32),
                sai_vfs::TextSize::new(field_end as u32),
            )
            .unwrap()
        );
        assert_eq!(scopes[0].syntax_range().file, file);
        assert_eq!(scopes[0].location().file.path, "src/lib.sai");

        let record_name_start = source.find("Record").unwrap() as u32;
        let record_primary = Location::new(
            "src/lib.sai",
            Span::new(
                file,
                TextRange::new(
                    sai_vfs::TextSize::new(record_name_start),
                    sai_vfs::TextSize::new(record_name_start + "Record".len() as u32),
                )
                .unwrap(),
            ),
        );
        let field_primary_start =
            source.find("field: U32").unwrap() as u32 + "field: ".len() as u32;
        let primary = Location::new(
            "src/lib.sai",
            Span::new(
                file,
                TextRange::new(
                    sai_vfs::TextSize::new(field_primary_start),
                    sai_vfs::TextSize::new(field_primary_start + 3),
                )
                .unwrap(),
            ),
        );
        let policies = sai_diagnostics::LintPolicies::default();
        let record_level = sai_diagnostics::resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &sai_diagnostics::LintResolutionContext {
                primary: Some(&record_primary),
                attributes: &scopes,
                cli: None,
                manifest: None,
                package_policy: &policies,
                root_policy: &policies,
                is_root_or_workspace_member: true,
                deny_warnings: false,
            },
        )
        .expect("registered lint");
        assert_eq!(record_level, sai_diagnostics::LintResolution::Suppressed);

        let resolved = sai_diagnostics::resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &sai_diagnostics::LintResolutionContext {
                primary: Some(&primary),
                attributes: &scopes,
                cli: None,
                manifest: None,
                package_policy: &policies,
                root_policy: &policies,
                is_root_or_workspace_member: true,
                deny_warnings: false,
            },
        )
        .expect("registered lint");
        assert!(matches!(
            resolved,
            sai_diagnostics::LintResolution::Emitted {
                severity: sai_diagnostics::Severity::Error,
                level: sai_diagnostics::LevelInfo {
                    value: DiagnosticLintLevel::Deny,
                    source: sai_diagnostics::LevelSource::Attribute,
                    ..
                }
            }
        ));
    }

    #[test]
    fn target_collection_validates_nested_attributes_with_owner_context() {
        fn find_entry<'a>(
            entries: &'a [ItemTreeEntry],
            symbols: &Interner,
            kind: ItemTreeKind,
            name: &str,
        ) -> Option<&'a ItemTreeEntry> {
            entries.iter().find_map(|entry| {
                if entry.kind == kind && symbols.resolve(entry.name).unwrap().as_ref() == name {
                    Some(entry)
                } else {
                    find_entry(&entry.children, symbols, kind, name)
                }
            })
        }

        let source = concat!(
            "trait Trait {\n",
            "    @inline fn defaulted() {}\n",
            "    @inline fn required()\n",
            "    @deprecated type Assoc\n",
            "}\n",
            "impl Value {\n",
            "    @deprecated fn inherent() {}\n",
            "    @deprecated type Assoc = U32\n",
            "}\n",
            "impl Trait for Value {\n",
            "    @deprecated fn trait_impl() {}\n",
            "}\n",
            "impl api {\n",
            "    @deprecated fn interface_impl() {}\n",
            "}\n",
            "interface api {\n",
            "    @repr(C) type Record { @deprecated field: U32 }\n",
            "    @must_use fn call() -> U32\n",
            "    resource Thing {\n",
            "        @must_use fn read()\n",
            "    }\n",
            "}\n",
            "extern \"wasm\" module \"env\" {\n",
            "    @deprecated fn imported()\n",
            "}\n",
            "unsafe extern \"C\" {\n",
            "    @deprecated fn c_import()\n",
            "}\n",
            "world sample {\n",
            "    @deprecated import fn now() -> U64\n",
            "    @doc(hidden) include base\n",
            "}\n",
            "fn generic<@deprecated T>() {}\n",
        );
        let file = FileId::new(727);
        let (parsed, ast_ids, symbols, _, snapshot) = collect_with_snapshot(source, file);
        let target_for = |entry: &ItemTreeEntry, module_level| {
            entry_attribute_target(entry, module_level, file, &parsed.root, &ast_ids)
                .expect("attribute target resolves")
                .expect("entry has an attribute target")
        };
        let result = ItemTree::collect_for_target(
            file,
            source,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg("wasm32-core", &[]),
        )
        .expect("target collection succeeds");
        let entries = &result.item_tree.items;
        let attribute_for = |kind, name: &str| {
            let entry = find_entry(entries, &symbols, kind, name)
                .unwrap_or_else(|| panic!("missing retained {kind:?} entry `{name}`"));
            result
                .validated_attrs
                .get(&entry.ast_id)
                .expect("entry attributes validated")
        };

        assert!(
            attribute_for(ItemTreeKind::AssocFn, "defaulted")
                .inline
                .is_some()
        );
        assert!(
            attribute_for(ItemTreeKind::AssocFn, "required")
                .inline
                .is_none()
        );
        assert!(
            attribute_for(ItemTreeKind::AssocType, "Assoc")
                .deprecated
                .is_some()
        );
        assert!(
            attribute_for(ItemTreeKind::AssocFn, "inherent")
                .deprecated
                .is_some()
        );
        assert!(
            attribute_for(ItemTreeKind::AssocFn, "trait_impl")
                .deprecated
                .is_none()
        );
        assert!(attribute_for(ItemTreeKind::Struct, "Record").repr.is_none());
        assert!(
            attribute_for(ItemTreeKind::Field, "field")
                .deprecated
                .is_some()
        );
        assert!(
            attribute_for(ItemTreeKind::InterfaceFn, "call")
                .must_use
                .is_some()
        );
        assert!(
            attribute_for(ItemTreeKind::AssocFn, "read")
                .must_use
                .is_some()
        );
        let resource_method = find_entry(entries, &symbols, ItemTreeKind::AssocFn, "read").unwrap();
        assert_eq!(
            target_for(resource_method, false).kind,
            TargetKind::InterfaceFn
        );
        assert!(
            attribute_for(ItemTreeKind::ExternFn, "imported")
                .deprecated
                .is_some()
        );

        let trait_fn = find_entry(entries, &symbols, ItemTreeKind::AssocFn, "defaulted")
            .expect("trait method retained");
        let trait_context = target_for(trait_fn, false);
        assert!(trait_context.has_body);
        let required_fn = find_entry(entries, &symbols, ItemTreeKind::AssocFn, "required").unwrap();
        assert!(!target_for(required_fn, false).has_body);

        let impl_method = find_entry(entries, &symbols, ItemTreeKind::AssocFn, "inherent").unwrap();
        assert_eq!(
            target_for(impl_method, false).impl_kind,
            Some(ImplKind::Inherent)
        );
        let trait_impl_method =
            find_entry(entries, &symbols, ItemTreeKind::AssocFn, "trait_impl").unwrap();
        assert_eq!(
            target_for(trait_impl_method, false).impl_kind,
            Some(ImplKind::Trait)
        );
        let interface_impl_method =
            find_entry(entries, &symbols, ItemTreeKind::AssocFn, "interface_impl").unwrap();
        assert_eq!(
            target_for(interface_impl_method, false).impl_kind,
            Some(ImplKind::Interface)
        );

        let interface_record =
            find_entry(entries, &symbols, ItemTreeKind::Struct, "Record").unwrap();
        assert!(target_for(interface_record, false).inside_interface);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == sai_diagnostics::Code::WitIncompatibleType
            })
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget
        }));

        let imported = find_entry(entries, &symbols, ItemTreeKind::ExternFn, "imported").unwrap();
        let extern_context = target_for(imported, false);
        assert_eq!(extern_context.extern_abi, Some(ExternAbi::Wasm));
        assert!(!extern_context.extern_unsafe);
        let c_import = find_entry(entries, &symbols, ItemTreeKind::ExternFn, "c_import").unwrap();
        let c_context = target_for(c_import, false);
        assert_eq!(c_context.extern_abi, Some(ExternAbi::C));
        assert!(c_context.extern_unsafe);

        let world_item = find_entry(
            entries,
            &symbols,
            ItemTreeKind::WorldItem { import: true },
            "now",
        )
        .expect("world item retained");
        assert!(
            entry_attribute_target(world_item, false, file, &parsed.root, &ast_ids)
                .unwrap()
                .is_none()
        );
        let world_include =
            find_entry(entries, &symbols, ItemTreeKind::WorldInclude, "base").unwrap();
        assert!(
            entry_attribute_target(world_include, false, file, &parsed.root, &ast_ids)
                .unwrap()
                .is_none()
        );

        let generic = find_entry(entries, &symbols, ItemTreeKind::Function, "generic").unwrap();
        let generic_parameter = generic
            .children
            .iter()
            .find(|entry| entry.kind == ItemTreeKind::TypeParam)
            .expect("generic parameter collected");
        assert!(
            result
                .validated_attrs
                .get(&generic_parameter.ast_id)
                .unwrap()
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn target_collection_keeps_invalid_attribute_target_entries() {
        fn find_entry<'a>(
            entries: &'a [ItemTreeEntry],
            symbols: &Interner,
            kind: ItemTreeKind,
            name: &str,
        ) -> Option<&'a ItemTreeEntry> {
            entries.iter().find_map(|entry| {
                if entry.kind == kind && symbols.resolve(entry.name).unwrap().as_ref() == name {
                    Some(entry)
                } else {
                    find_entry(&entry.children, symbols, kind, name)
                }
            })
        }

        let source = concat!(
            "enum Message { Error(@deprecated @derive(Clone) code: U32) }\n",
            "fn generic<@deprecated @derive(Clone) T>() {}\n",
            "world sample {\n",
            "    @deprecated @derive(Clone) import fn now() -> U64\n",
            "    @deprecated @derive(Clone) include base\n",
            "}\n",
            "trait Api {\n",
            "    @target(\"wasm64-core\") type Associated\n",
            "    @target(\"wasm64-core\") fn method() {}\n",
            "}\n",
        );
        let file = FileId::new(728);
        let (parsed, ast_ids, symbols, _, snapshot) = collect_with_snapshot(source, file);
        let parser_target_errors = parsed
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget)
            .count();
        assert_eq!(parser_target_errors, 4, "{:?}", parsed.errors);

        let result = ItemTree::collect_for_target(
            file,
            source,
            &parsed.root,
            &ast_ids,
            &symbols,
            &snapshot,
            &target_cfg("wasm32-core", &[]),
        )
        .expect("target collection succeeds");
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(
                    |diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget
                )
                .count(),
            2,
            "associated @target attributes are diagnosed by HIR validation"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget),
            "HIR emits only the invalid-target diagnostics for associated attributes"
        );

        for (kind, name) in [
            (ItemTreeKind::VariantField, "code"),
            (ItemTreeKind::TypeParam, "T"),
            (ItemTreeKind::WorldItem { import: true }, "now"),
            (ItemTreeKind::WorldInclude, "base"),
            (ItemTreeKind::AssocType, "Associated"),
            (ItemTreeKind::AssocFn, "method"),
        ] {
            let entry = find_entry(&result.item_tree.items, &symbols, kind, name)
                .unwrap_or_else(|| panic!("missing retained {kind:?} `{name}`"));
            let attrs = result
                .validated_attrs
                .get(&entry.ast_id)
                .expect("all retained entries have an attribute result");
            assert!(attrs.inline.is_none());
            assert!(attrs.deprecated.is_none());
            assert!(attrs.derives.is_empty());
            let invalid_target_count = attrs
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget
                })
                .count();
            assert_eq!(
                invalid_target_count,
                usize::from(matches!(
                    kind,
                    ItemTreeKind::AssocType | ItemTreeKind::AssocFn
                )),
                "associated @target attributes are diagnosed while the members remain retained"
            );
            assert!(
                attrs.diagnostics.iter().all(|diagnostic| {
                    diagnostic.code == sai_diagnostics::Code::InvalidAttributeTarget
                }),
                "invalid target is the only HIR validation diagnostic for these entries"
            );
        }
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
            "trait Omitted {\n",
            "    @target(\"wasm32-core\")\n",
            "    fn nested() {}\n",
            "}\n",
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
            ("fn r#_() {}\n", sai_diagnostics::Code::InvalidRawIdent),
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
    fn raw_self_spellings_are_valid_item_names() {
        let source = "fn r#self() {}\ntype r#Self = Unit\n";
        let (parsed, symbols, tree) = collect(source, FileId::new(710));

        assert!(
            parsed
                .errors
                .iter()
                .all(|error| error.code != sai_diagnostics::Code::InvalidRawIdent)
        );
        assert_eq!(tree.items.len(), 2);
        assert!(tree.items.iter().all(|item| !item.name.is_error()));
        let names = tree
            .items
            .iter()
            .map(|item| symbols.resolve(item.name).unwrap().as_ref().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["self", "Self"]);
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
        let (parsed, ast_ids, symbols, tree, _snapshot) = collect_with_snapshot(source, file);

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
        let stable = tree
            .stable_shape(&symbols)
            .expect("item tree has a stable shape");
        let rebound = stable
            .bind_to_snapshot(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("all item-like nodes and locations rebind");
        assert_eq!(rebound, tree);
        assert!(rebound.matches_ast_ids(&ast_ids));
        assert_eq!(tree.uses.len(), 1);
        assert_eq!(tree.uses[0].pkg_path, "sai:core/fmt");
        assert!(tree.uses[0].is_pub);
        assert!(tree.uses[0].has_items);
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
    fn stable_item_tree_preserves_empty_use_items_presence() {
        let file = FileId::new(721);
        let source_without_items = "use acme:package\n";
        let source_with_empty_items = "use acme:package.{}\n";
        let (parsed_without, ast_ids_without, symbols, tree_without, _) =
            collect_with_snapshot(source_without_items, file);
        let (parsed_with, ast_ids_with, _, tree_with, _) =
            collect_with_snapshot(source_with_empty_items, file);

        assert!(parsed_without.errors.is_empty());
        assert_eq!(parsed_with.errors.len(), 1);
        assert_eq!(
            parsed_with.errors[0].code,
            sai_diagnostics::Code::ExpectedToken
        );
        assert!(!tree_without.uses[0].has_items);
        assert!(tree_with.uses[0].has_items);
        assert!(tree_with.uses[0].items.is_empty());

        let stable_without = tree_without
            .stable_shape(&symbols)
            .expect("item tree has a stable shape");
        let stable_with = tree_with
            .stable_shape(&symbols)
            .expect("item tree has a stable shape");
        assert_ne!(stable_without, stable_with);

        assert_eq!(
            stable_without
                .bind_to_snapshot(
                    file,
                    source_without_items,
                    &parsed_without.root,
                    &ast_ids_without,
                    &symbols,
                )
                .unwrap(),
            tree_without
        );
        assert_eq!(
            stable_with
                .bind_to_snapshot(
                    file,
                    source_with_empty_items,
                    &parsed_with.root,
                    &ast_ids_with,
                    &symbols,
                )
                .unwrap(),
            tree_with
        );
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
    fn stable_item_tree_shape_resolves_symbols_and_omits_source_locations() {
        let first_source = concat!(
            "//! module docs\n",
            "fn before() { 1 }\n",
            "@derive(Foo(bar))\n",
            "@deprecated(note = \"use replacement\")\n",
            "@seed(123)\n",
            "fn run() { 2 }\n",
            "pub use sai:core/fmt.{Format as Fmt}\n",
        );
        let second_source = first_source.replace("{ 1 }", "{ 123456 }");
        let file = FileId::new(711);

        let (first, first_ids) = parse_snapshot(first_source, file);
        let first_symbols = Interner::new();
        let first_tree =
            ItemTree::collect(file, first_source, &first.root, &first_ids, &first_symbols)
                .expect("first item tree collects");

        let (second, second_ids) = parse_snapshot(&second_source, file);
        let second_symbols = Interner::new();
        second_symbols
            .intern("reserved-before-collection")
            .expect("reserved symbol interns");
        let second_tree = ItemTree::collect(
            file,
            &second_source,
            &second.root,
            &second_ids,
            &second_symbols,
        )
        .expect("second item tree collects");

        let first_shape = first_tree
            .stable_shape(&first_symbols)
            .expect("first item tree resolves through its interner");
        let second_shape = second_tree
            .stable_shape(&second_symbols)
            .expect("second item tree resolves through its interner");

        assert_eq!(first_shape, second_shape);
        assert_eq!(first_shape.fingerprint(), second_shape.fingerprint());
        assert_ne!(
            first_tree.uses[0].items[0].range,
            second_tree.uses[0].items[0].range
        );
        assert_ne!(
            first_tree.items[1].raw_attrs[0].span,
            second_tree.items[1].raw_attrs[0].span
        );
        let rebound = first_shape
            .bind_to_snapshot(
                file,
                &second_source,
                &second.root,
                &second_ids,
                &second_symbols,
            )
            .expect("stable shape rebinds without collecting a new item tree");
        assert_eq!(rebound, second_tree);
        assert!(rebound.matches_ast_ids(&second_ids));
        assert!(!rebound.matches_ast_ids(&first_ids));

        let changed_source = second_source.replace("fn run()", "fn renamed()");
        let (changed, changed_ids) = parse_snapshot(&changed_source, file);
        let changed_tree = ItemTree::collect(
            file,
            &changed_source,
            &changed.root,
            &changed_ids,
            &second_symbols,
        )
        .expect("changed item tree collects");
        let changed_shape = changed_tree
            .stable_shape(&second_symbols)
            .expect("changed item tree resolves through its interner");
        assert_ne!(second_shape, changed_shape);
        assert_ne!(second_shape.fingerprint(), changed_shape.fingerprint());
    }

    #[test]
    fn item_tree_retains_nested_declaration_docs_in_stable_shapes() {
        let source = concat!(
            "/// Interface documentation.\n",
            "/// A second line.\n",
            "interface api {\n",
            "    /// Type documentation.\n",
            "    type Payload {\n",
            "        /// Field documentation.\n",
            "        value: U32\n",
            "    }\n",
            "    /// Function documentation.\n",
            "    fn send(payload: Payload) -> U32\n",
            "}\n",
            "/// World documentation.\n",
            "world client {\n",
            "    /// Import documentation.\n",
            "    import api\n",
            "    /// Include documentation.\n",
            "    include base\n",
            "}\n",
            "/// Helper documentation.\n",
            "fn helper() { 1 }\n",
        );
        let file = FileId::new(719);
        let (parsed, ast_ids, symbols, tree, _) = collect_with_snapshot(source, file);
        assert!(
            parsed.errors.is_empty(),
            "documentation fixture should parse: {:?}",
            parsed.errors
        );

        let interface = &tree.items[0];
        assert_eq!(
            interface.docs.as_deref(),
            Some("Interface documentation.\nA second line.")
        );
        assert_eq!(
            interface.children[0].docs.as_deref(),
            Some("Type documentation.")
        );
        assert_eq!(
            interface.children[0].children[0].docs.as_deref(),
            Some("Field documentation.")
        );
        assert_eq!(
            interface.children[1].docs.as_deref(),
            Some("Function documentation.")
        );
        assert_eq!(tree.items[1].docs.as_deref(), Some("World documentation."));
        assert_eq!(
            tree.items[1].children[0].docs.as_deref(),
            Some("Import documentation.")
        );
        assert_eq!(
            tree.items[1].children[1].docs.as_deref(),
            Some("Include documentation.")
        );

        let stable = tree
            .stable_shape(&symbols)
            .expect("documentation is part of the stable item-tree shape");
        let rebound = stable
            .bind_to_snapshot(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("documentation-bearing entries rebind");
        assert_eq!(rebound, tree);

        let changed_docs_source = source.replace(
            "/// Function documentation.",
            "/// Changed function documentation.",
        );
        let (changed_docs, changed_docs_ids) = parse_snapshot(&changed_docs_source, file);
        let changed_docs_tree = ItemTree::collect(
            file,
            &changed_docs_source,
            &changed_docs.root,
            &changed_docs_ids,
            &symbols,
        )
        .expect("changed documentation tree collects");
        let changed_docs_shape = changed_docs_tree
            .stable_shape(&symbols)
            .expect("changed documentation shape collects");
        assert_ne!(stable, changed_docs_shape);
        assert_ne!(stable.fingerprint(), changed_docs_shape.fingerprint());

        let changed_body_source = source.replace("{ 1 }", "{ 123456 }");
        let (changed_body, changed_body_ids) = parse_snapshot(&changed_body_source, file);
        let changed_body_tree = ItemTree::collect(
            file,
            &changed_body_source,
            &changed_body.root,
            &changed_body_ids,
            &symbols,
        )
        .expect("body-only edit tree collects");
        let changed_body_shape = changed_body_tree
            .stable_shape(&symbols)
            .expect("body-only edit keeps a stable item-tree shape");
        assert_eq!(stable, changed_body_shape);
        assert_eq!(stable.fingerprint(), changed_body_shape.fingerprint());

        let mut tampered = stable.clone();
        tampered.items[0].children[1].docs = Some("/// forged documentation".to_owned());
        assert_eq!(
            tampered.bind_to_snapshot(file, source, &parsed.root, &ast_ids, &symbols),
            Err(ItemTreeError::StableShapeMismatch)
        );
    }

    #[test]
    fn stable_item_tree_shape_preserves_error_names_and_rejects_foreign_symbols() {
        let file = FileId::new(712);
        let (_, symbols, tree) = collect("fn named() {}\n", file);
        let valid_shape = tree
            .stable_shape(&symbols)
            .expect("item tree resolves through its interner");

        let mut error_tree = tree.clone();
        error_tree.items[0].name = Symbol::ERROR;
        let error_shape = error_tree
            .stable_shape(&symbols)
            .expect("error symbols have a stable sentinel representation");
        assert_ne!(valid_shape, error_shape);
        assert_ne!(valid_shape.fingerprint(), error_shape.fingerprint());

        assert_eq!(
            tree.stable_shape(&Interner::new()),
            Err(StableItemTreeError::Interner(LookupError::UnknownSymbol))
        );
    }

    #[test]
    fn stable_item_tree_binding_rejects_duplicate_and_unresolvable_ids() {
        let source = "use sai:core/fmt.{Format}\nfn run() {}\n";
        let file = FileId::new(713);
        let (parsed, ast_ids, symbols, tree, _) = collect_with_snapshot(source, file);
        let stable = tree
            .stable_shape(&symbols)
            .expect("item tree has a stable shape");

        let mut duplicate_id = stable.clone();
        duplicate_id.items[0].ast_id = duplicate_id.uses[0].ast_id;
        assert_eq!(
            duplicate_id.bind_to_snapshot(file, source, &parsed.root, &ast_ids, &symbols),
            Err(ItemTreeError::StableShapeMismatch)
        );

        let mut missing_id = stable;
        missing_id.items[0].ast_id = ErasedAstId::new(u32::MAX);
        assert_eq!(
            missing_id.bind_to_snapshot(file, source, &parsed.root, &ast_ids, &symbols),
            Err(ItemTreeError::StableShapeMismatch)
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
