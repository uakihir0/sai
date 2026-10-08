#![forbid(unsafe_code)]

pub mod arena;
pub mod attrs;
pub mod body;
pub mod expressions;
pub mod item_tree;
pub mod items;
pub mod literals;
pub mod lower;
pub mod patterns;
pub mod source_map;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub use sai_intern::{BindingId, DefIndex, IndexVec, ItemLocalId, PackageId, Symbol};
use sai_intern::{
    IndexVecError, Interner, InternerSessionId, LookupError, PackageIdError, PackageInterner,
};
use sai_stable_hash::StableHasher;
pub use sai_vfs::{FileId, TextRange};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct DefId {
    pub package: PackageId,
    pub index: DefIndex,
    scope: Option<DefPathInternerId>,
}

impl DefId {
    pub const fn from_foreign(package: PackageId, index: DefIndex) -> Self {
        Self {
            package,
            index,
            scope: None,
        }
    }
}

impl std::fmt::Debug for DefId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefId")
            .field("package", &self.package)
            .field("index", &self.index)
            .finish()
    }
}

static NEXT_DEF_PATH_INTERNER_ID: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct DefPathInternerId(u32);

fn next_def_path_interner_id() -> Option<DefPathInternerId> {
    let mut id = NEXT_DEF_PATH_INTERNER_ID.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1)?;
        match NEXT_DEF_PATH_INTERNER_ID.compare_exchange_weak(
            id,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(DefPathInternerId(id)),
            Err(observed) => id = observed,
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct LocalDefId {
    package: PackageId,
    index: DefIndex,
    interner: DefPathInternerId,
}

impl LocalDefId {
    pub const fn package(self) -> PackageId {
        self.package
    }

    pub const fn index(self) -> DefIndex {
        self.index
    }

    pub const fn to_def_id(self) -> DefId {
        DefId {
            package: self.package,
            index: self.index,
            scope: Some(self.interner),
        }
    }
}

impl std::fmt::Debug for LocalDefId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalDefId")
            .field("package", &self.package)
            .field("index", &self.index)
            .finish()
    }
}

/// A session-local path. Its derived `Hash` is for in-memory collections only;
/// persistent identity must use `DefPathHash`.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct DefPath {
    package: PackageId,
    data: Vec<DisambiguatedDefPathData>,
    package_interner: InternerSessionId,
    symbol_interner: InternerSessionId,
}

impl std::fmt::Debug for DefPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefPath")
            .field("package", &self.package)
            .field("data", &self.data)
            .finish()
    }
}

impl DefPath {
    pub fn new(
        package: PackageId,
        data: Vec<DisambiguatedDefPathData>,
        packages: &PackageInterner,
        symbols: &Interner,
    ) -> Result<Self, DefPathError> {
        if package.session_id() != packages.session_id() {
            return Err(DefPathError::InternerSessionMismatch);
        }
        packages.resolve(package).map_err(DefPathError::Package)?;
        for component in &data {
            for symbol in component.data.symbols().into_iter().flatten() {
                if symbol.is_error() {
                    continue;
                }
                if symbol.session_id() != symbols.session_id() {
                    return Err(DefPathError::InternerSessionMismatch);
                }
                symbols.resolve(symbol).map_err(DefPathError::Symbol)?;
            }
        }

        Ok(Self {
            package,
            data,
            package_interner: packages.session_id(),
            symbol_interner: symbols.session_id(),
        })
    }

    pub const fn package(&self) -> PackageId {
        self.package
    }

    pub fn data(&self) -> &[DisambiguatedDefPathData] {
        &self.data
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DisambiguatedDefPathData {
    pub data: DefPathData,
    pub disambiguator: u32,
}

/// Session-local path data. Persisted identities must hash resolved strings,
/// never the raw `Symbol` values stored here.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DefPathData {
    Module(Symbol),
    TypeNs(Symbol),
    ValueNs(Symbol),
    Interface(Symbol),
    World(Symbol),
    Effect(Symbol),
    Field(Symbol),
    Variant(Symbol),
    AssocType(Symbol),
    AssocFn(Symbol),
    TypeParam(Symbol),
    ConstParam(Symbol),
    Impl { header_hash: u64 },
    DerivedImpl { trait_name: Symbol },
    Invariant(Symbol),
    ExternBlock { abi: Symbol, module: Option<Symbol> },
    Test(Arc<str>),
    Property(Arc<str>),
    Closure,
    AsyncBlock,
    WorldItem { import: bool, name: Symbol },
    WorldInclude(Symbol),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DefPathHash(u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefPathError {
    Package(PackageIdError),
    Symbol(LookupError),
    InternerSessionMismatch,
}

impl DefPathHash {
    /// Computes the persistent identity from a canonical package key and
    /// resolved symbol strings, never from their session-local numeric IDs.
    pub fn compute(
        path: &DefPath,
        packages: &PackageInterner,
        symbols: &Interner,
    ) -> Result<Self, DefPathError> {
        if path.package_interner != packages.session_id()
            || path.symbol_interner != symbols.session_id()
        {
            return Err(DefPathError::InternerSessionMismatch);
        }

        let package_key = packages
            .resolve(path.package)
            .map_err(DefPathError::Package)?;
        let mut hasher = StableHasher::new();
        write_stable_string(&mut hasher, &package_key);

        for component in &path.data {
            component
                .data
                .write_stable(&mut hasher, component.disambiguator, symbols)?;
        }

        Ok(Self(hasher.finish().raw()))
    }

    pub const fn raw(self) -> u128 {
        self.0
    }
}

const DEF_PATH_TAG_MODULE: u8 = 0x01;
const DEF_PATH_TAG_TYPE_NS: u8 = 0x02;
const DEF_PATH_TAG_VALUE_NS: u8 = 0x03;
const DEF_PATH_TAG_INTERFACE: u8 = 0x04;
const DEF_PATH_TAG_WORLD: u8 = 0x05;
const DEF_PATH_TAG_EFFECT: u8 = 0x06;
const DEF_PATH_TAG_FIELD: u8 = 0x07;
const DEF_PATH_TAG_VARIANT: u8 = 0x08;
const DEF_PATH_TAG_ASSOC_TYPE: u8 = 0x09;
const DEF_PATH_TAG_ASSOC_FN: u8 = 0x0A;
const DEF_PATH_TAG_TYPE_PARAM: u8 = 0x0B;
const DEF_PATH_TAG_CONST_PARAM: u8 = 0x0C;
const DEF_PATH_TAG_IMPL: u8 = 0x0D;
const DEF_PATH_TAG_DERIVED_IMPL: u8 = 0x0E;
const DEF_PATH_TAG_INVARIANT: u8 = 0x0F;
const DEF_PATH_TAG_EXTERN_BLOCK: u8 = 0x10;
const DEF_PATH_TAG_TEST: u8 = 0x11;
const DEF_PATH_TAG_PROPERTY: u8 = 0x12;
const DEF_PATH_TAG_CLOSURE: u8 = 0x13;
const DEF_PATH_TAG_ASYNC_BLOCK: u8 = 0x14;
const DEF_PATH_TAG_WORLD_ITEM: u8 = 0x15;
const DEF_PATH_TAG_WORLD_INCLUDE: u8 = 0x16;

impl DefPathData {
    fn symbols(&self) -> [Option<Symbol>; 2] {
        match self {
            Self::Module(name)
            | Self::TypeNs(name)
            | Self::ValueNs(name)
            | Self::Interface(name)
            | Self::World(name)
            | Self::Effect(name)
            | Self::Field(name)
            | Self::Variant(name)
            | Self::AssocType(name)
            | Self::AssocFn(name)
            | Self::TypeParam(name)
            | Self::ConstParam(name)
            | Self::DerivedImpl { trait_name: name }
            | Self::Invariant(name)
            | Self::WorldInclude(name) => [Some(*name), None],
            Self::ExternBlock { abi, module } => [Some(*abi), *module],
            Self::WorldItem { name, .. } => [Some(*name), None],
            Self::Impl { .. }
            | Self::Test(_)
            | Self::Property(_)
            | Self::Closure
            | Self::AsyncBlock => [None, None],
        }
    }

    fn write_stable(
        &self,
        hasher: &mut StableHasher,
        disambiguator: u32,
        symbols: &Interner,
    ) -> Result<(), DefPathError> {
        match self {
            Self::Module(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_MODULE, *name, disambiguator, symbols)?
            }
            Self::TypeNs(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_TYPE_NS, *name, disambiguator, symbols)?
            }
            Self::ValueNs(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_VALUE_NS,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::Interface(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_INTERFACE,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::World(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_WORLD, *name, disambiguator, symbols)?
            }
            Self::Effect(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_EFFECT, *name, disambiguator, symbols)?
            }
            Self::Field(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_FIELD, *name, disambiguator, symbols)?
            }
            Self::Variant(name) => {
                write_symbol_component(hasher, DEF_PATH_TAG_VARIANT, *name, disambiguator, symbols)?
            }
            Self::AssocType(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_ASSOC_TYPE,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::AssocFn(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_ASSOC_FN,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::TypeParam(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_TYPE_PARAM,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::ConstParam(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_CONST_PARAM,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::Impl { header_hash } => {
                hasher.write_tag(DEF_PATH_TAG_IMPL);
                hasher.write_u64(*header_hash);
                hasher.write_u32(disambiguator);
            }
            Self::DerivedImpl { trait_name } => write_symbol_component(
                hasher,
                DEF_PATH_TAG_DERIVED_IMPL,
                *trait_name,
                disambiguator,
                symbols,
            )?,
            Self::Invariant(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_INVARIANT,
                *name,
                disambiguator,
                symbols,
            )?,
            Self::ExternBlock { abi, module } => {
                hasher.write_tag(DEF_PATH_TAG_EXTERN_BLOCK);
                write_resolved_symbol(hasher, *abi, symbols)?;
                if let Some(module) = module {
                    hasher.write_u8(1);
                    write_resolved_symbol(hasher, *module, symbols)?;
                } else {
                    hasher.write_u8(0);
                }
                hasher.write_u32(disambiguator);
            }
            Self::Test(name) => {
                write_string_component(hasher, DEF_PATH_TAG_TEST, name, disambiguator)
            }
            Self::Property(name) => {
                write_string_component(hasher, DEF_PATH_TAG_PROPERTY, name, disambiguator)
            }
            Self::Closure => {
                hasher.write_tag(DEF_PATH_TAG_CLOSURE);
                hasher.write_u32(disambiguator);
            }
            Self::AsyncBlock => {
                hasher.write_tag(DEF_PATH_TAG_ASYNC_BLOCK);
                hasher.write_u32(disambiguator);
            }
            Self::WorldItem { import, name } => {
                hasher.write_tag(DEF_PATH_TAG_WORLD_ITEM);
                hasher.write_u8(u8::from(*import));
                write_resolved_symbol(hasher, *name, symbols)?;
                hasher.write_u32(disambiguator);
            }
            Self::WorldInclude(name) => write_symbol_component(
                hasher,
                DEF_PATH_TAG_WORLD_INCLUDE,
                *name,
                disambiguator,
                symbols,
            )?,
        }
        Ok(())
    }
}

fn write_symbol_component(
    hasher: &mut StableHasher,
    tag: u8,
    symbol: Symbol,
    disambiguator: u32,
    symbols: &Interner,
) -> Result<(), DefPathError> {
    hasher.write_tag(tag);
    write_resolved_symbol(hasher, symbol, symbols)?;
    hasher.write_u32(disambiguator);
    Ok(())
}

fn write_string_component(hasher: &mut StableHasher, tag: u8, value: &str, disambiguator: u32) {
    hasher.write_tag(tag);
    write_stable_string(hasher, value);
    hasher.write_u32(disambiguator);
}

fn write_resolved_symbol(
    hasher: &mut StableHasher,
    symbol: Symbol,
    symbols: &Interner,
) -> Result<(), DefPathError> {
    if symbol.is_error() {
        // No Rust string can have u64::MAX bytes, so this length is a
        // collision-free marker in the same length-prefixed string encoding.
        hasher.write_len(u64::MAX);
        return Ok(());
    }
    let value = symbols.resolve(symbol).map_err(DefPathError::Symbol)?;
    write_stable_string(hasher, &value);
    Ok(())
}

fn write_stable_string(hasher: &mut StableHasher, value: &str) {
    hasher.write_len(u64::try_from(value.len()).unwrap_or(u64::MAX));
    hasher.write_raw_bytes(value.as_bytes());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefPathInternerError {
    Package(PackageIdError),
    InternerSessionMismatch,
    ForeignDefinitionId,
    ParentDefinitionNotFound,
    InvalidNestedComponent,
    InvalidAssociatedItem,
    WrongPackage,
    TooManyInternerInstances,
    TooManyDefinitions,
    LockPoisoned,
}

#[derive(Default)]
struct DefPathStorage {
    by_path: HashMap<DefPath, LocalDefId>,
    paths: IndexVec<DefIndex, DefPath>,
}

/// Session-local, append-only interning of definition paths for one package.
pub struct DefPathInterner {
    package: PackageId,
    package_interner: InternerSessionId,
    symbol_interner: InternerSessionId,
    id: DefPathInternerId,
    storage: Mutex<DefPathStorage>,
}

impl DefPathInterner {
    pub fn new(
        package: PackageId,
        packages: &PackageInterner,
        symbols: &Interner,
    ) -> Result<Self, DefPathInternerError> {
        if package.session_id() != packages.session_id() {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }
        packages
            .resolve(package)
            .map_err(DefPathInternerError::Package)?;
        let id =
            next_def_path_interner_id().ok_or(DefPathInternerError::TooManyInternerInstances)?;

        Ok(Self {
            package,
            package_interner: packages.session_id(),
            symbol_interner: symbols.session_id(),
            id,
            storage: Mutex::new(DefPathStorage::default()),
        })
    }

    pub fn intern(&self, path: DefPath) -> Result<LocalDefId, DefPathInternerError> {
        if path.package_interner != self.package_interner
            || path.symbol_interner != self.symbol_interner
        {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }
        if path.package != self.package {
            return Err(DefPathInternerError::WrongPackage);
        }

        let mut storage = self
            .storage
            .lock()
            .map_err(|_| DefPathInternerError::LockPoisoned)?;
        if let Some(id) = storage.by_path.get(&path) {
            return Ok(*id);
        }

        let index = storage
            .paths
            .push(path.clone())
            .map_err(|IndexVecError::TooManyElements| DefPathInternerError::TooManyDefinitions)?;
        let id = LocalDefId {
            package: self.package,
            index,
            interner: self.id,
        };
        storage.by_path.insert(path, id);
        Ok(id)
    }

    /// Intern a closure or async-block definition below its lexical parent.
    ///
    /// The caller supplies the preorder disambiguator assigned within the
    /// enclosing owner. Named and otherwise symbol-bearing components must
    /// use `DefPath::new` and `intern` so their interner sessions are checked.
    pub fn intern_nested(
        &self,
        parent: LocalDefId,
        component: DefPathData,
        disambiguator: u32,
    ) -> Result<LocalDefId, DefPathInternerError> {
        if !matches!(&component, DefPathData::Closure | DefPathData::AsyncBlock) {
            return Err(DefPathInternerError::InvalidNestedComponent);
        }
        let mut path = self
            .resolve(parent)?
            .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
        path.data.push(DisambiguatedDefPathData {
            data: component,
            disambiguator,
        });
        self.intern(path)
    }

    /// Intern a batch of fields below their declaring definition.
    ///
    /// The disambiguator is the ordinal among sibling fields with the same
    /// symbol, in source order. All symbol sessions and path capacity are
    /// validated before any path is added.
    pub fn intern_fields(
        &self,
        parent: LocalDefId,
        fields: &[(Symbol, u32)],
    ) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        if fields.is_empty() {
            self.resolve(parent)?
                .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
            return Ok(Vec::new());
        }
        let fields = fields
            .iter()
            .map(|(name, disambiguator)| (parent, *name, *disambiguator))
            .collect::<Vec<_>>();
        self.intern_fields_for_parents(&fields)
    }

    /// Intern a batch of fields below their individual declaring definitions.
    ///
    /// This is used for record-variant fields, whose parent path is the
    /// corresponding enum variant. Symbol sessions and all parent paths are
    /// validated before the batch mutates the interner.
    pub fn intern_fields_for_parents(
        &self,
        fields: &[(LocalDefId, Symbol, u32)],
    ) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        if fields
            .iter()
            .any(|(_, name, _)| !name.is_error() && name.session_id() != self.symbol_interner)
        {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }

        let paths = fields
            .iter()
            .map(|(parent, name, disambiguator)| {
                let mut path = self
                    .resolve(*parent)?
                    .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
                path.data.push(DisambiguatedDefPathData {
                    data: DefPathData::Field(*name),
                    disambiguator: *disambiguator,
                });
                Ok(path)
            })
            .collect::<Result<Vec<_>, DefPathInternerError>>()?;
        self.intern_paths(paths)
    }

    /// Intern a batch of variants below their declaring enum.
    ///
    /// The disambiguator is the ordinal among sibling variants with the same
    /// symbol, in source order.
    pub fn intern_variants(
        &self,
        parent: LocalDefId,
        variants: &[(Symbol, u32)],
    ) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        if variants
            .iter()
            .any(|(name, _)| !name.is_error() && name.session_id() != self.symbol_interner)
        {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }

        let parent_path = self
            .resolve(parent)?
            .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
        let paths = variants
            .iter()
            .map(|(name, disambiguator)| {
                let mut path = parent_path.clone();
                path.data.push(DisambiguatedDefPathData {
                    data: DefPathData::Variant(*name),
                    disambiguator: *disambiguator,
                });
                path
            })
            .collect();
        self.intern_paths(paths)
    }

    /// Intern trait or impl associated items below their declaring definition.
    ///
    /// Only `AssocType` and `AssocFn` components are accepted. Disambiguators
    /// are supplied by the caller as the ordinal among equal sibling
    /// components in source order. The full batch is validated before mutation.
    pub fn intern_assoc_items(
        &self,
        parent: LocalDefId,
        items: &[(DefPathData, u32)],
    ) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        let parent_path = self
            .resolve(parent)?
            .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if items.iter().any(|(component, _)| {
            !matches!(
                component,
                DefPathData::AssocType(_) | DefPathData::AssocFn(_)
            )
        }) {
            return Err(DefPathInternerError::InvalidAssociatedItem);
        }

        let paths = items
            .iter()
            .map(|(component, disambiguator)| {
                let mut path = parent_path.clone();
                path.data.push(DisambiguatedDefPathData {
                    data: component.clone(),
                    disambiguator: *disambiguator,
                });
                path
            })
            .collect();
        self.intern_paths(paths)
    }

    /// Intern a type or const generic parameter below its declaring definition.
    ///
    /// The disambiguator is the ordinal among siblings with the same parameter
    /// kind and symbol, in source order.
    pub fn intern_generic_param(
        &self,
        parent: LocalDefId,
        name: Symbol,
        is_const: bool,
        disambiguator: u32,
    ) -> Result<LocalDefId, DefPathInternerError> {
        self.intern_generic_params(parent, &[(name, is_const, disambiguator)])?
            .into_iter()
            .next()
            .ok_or(DefPathInternerError::ParentDefinitionNotFound)
    }

    /// Intern a batch of type and const generic parameters below one parent.
    ///
    /// All symbol sessions and definition-path capacity are validated before
    /// any path is added, so a failed batch leaves the interner unchanged.
    pub fn intern_generic_params(
        &self,
        parent: LocalDefId,
        parameters: &[(Symbol, bool, u32)],
    ) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        if parameters
            .iter()
            .any(|(name, _, _)| !name.is_error() && name.session_id() != self.symbol_interner)
        {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }

        let parent_path = self
            .resolve(parent)?
            .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
        let paths = parameters
            .iter()
            .map(|(name, is_const, disambiguator)| {
                let mut path = parent_path.clone();
                path.data.push(DisambiguatedDefPathData {
                    data: if *is_const {
                        DefPathData::ConstParam(*name)
                    } else {
                        DefPathData::TypeParam(*name)
                    },
                    disambiguator: *disambiguator,
                });
                path
            })
            .collect::<Vec<_>>();

        self.intern_paths(paths)
    }

    fn intern_paths(&self, paths: Vec<DefPath>) -> Result<Vec<LocalDefId>, DefPathInternerError> {
        if paths.iter().any(|path| {
            path.package != self.package
                || path.package_interner != self.package_interner
                || path.symbol_interner != self.symbol_interner
                || path.data.iter().any(|component| {
                    component
                        .data
                        .symbols()
                        .into_iter()
                        .flatten()
                        .any(|symbol| {
                            !symbol.is_error() && symbol.session_id() != self.symbol_interner
                        })
                })
        }) {
            return Err(DefPathInternerError::InternerSessionMismatch);
        }

        let mut storage = self
            .storage
            .lock()
            .map_err(|_| DefPathInternerError::LockPoisoned)?;
        let mut paths_to_add = HashSet::new();
        for path in &paths {
            if !storage.by_path.contains_key(path) {
                paths_to_add.insert(path.clone());
            }
        }
        let final_len = storage
            .paths
            .len()
            .checked_add(paths_to_add.len())
            .ok_or(DefPathInternerError::TooManyDefinitions)?;
        let maximum_len = usize::try_from(u64::from(u32::MAX) + 1).unwrap_or(usize::MAX);
        if final_len > maximum_len {
            return Err(DefPathInternerError::TooManyDefinitions);
        }

        let mut ids = Vec::with_capacity(paths.len());
        for path in paths {
            let id = if let Some(id) = storage.by_path.get(&path) {
                *id
            } else {
                let index = storage.paths.push(path.clone()).map_err(
                    |IndexVecError::TooManyElements| DefPathInternerError::TooManyDefinitions,
                )?;
                let id = LocalDefId {
                    package: self.package,
                    index,
                    interner: self.id,
                };
                storage.by_path.insert(path, id);
                id
            };
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn resolve(&self, id: LocalDefId) -> Result<Option<DefPath>, DefPathInternerError> {
        if id.interner != self.id {
            return Err(DefPathInternerError::ForeignDefinitionId);
        }
        if id.package != self.package {
            return Err(DefPathInternerError::WrongPackage);
        }

        let storage = self
            .storage
            .lock()
            .map_err(|_| DefPathInternerError::LockPoisoned)?;
        Ok(storage.paths.get(id.index).cloned())
    }

    pub fn resolve_def_id(&self, id: DefId) -> Result<Option<DefPath>, DefPathInternerError> {
        if id.scope != Some(self.id) {
            return Err(DefPathInternerError::ForeignDefinitionId);
        }
        if id.package != self.package {
            return Err(DefPathInternerError::WrongPackage);
        }

        self.resolve(LocalDefId {
            package: id.package,
            index: id.index,
            interner: self.id,
        })
    }

    pub fn len(&self) -> Result<usize, DefPathInternerError> {
        self.storage
            .lock()
            .map(|storage| storage.paths.len())
            .map_err(|_| DefPathInternerError::LockPoisoned)
    }

    pub fn is_empty(&self) -> Result<bool, DefPathInternerError> {
        self.len().map(|len| len == 0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OwnerId {
    pub def_id: LocalDefId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HirId {
    pub owner: OwnerId,
    pub local_id: ItemLocalId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyId {
    pub owner: OwnerId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Span {
    pub file: FileId,
    pub range: TextRange,
    pub ctxt: SpanCtxt,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpanCtxt {
    Root,
    Desugared(DesugaringKind),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DesugaringKind {
    WhileLoop,
    WhileLet,
    IfLet,
    ForLoop,
    ForAwait,
    VarStmt,
    AsyncFn,
    AsyncBlock,
    Await,
    TryOp,
    CompoundAssign,
    Assert,
    FormatString,
    Derive,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Visibility {
    Private,
    Pub,
    Export,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Safety {
    Safe,
    Unsafe,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Mutability {
    Not,
    Mut,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_intern::{IndexVec, PackageInterner};
    use std::thread;

    fn def_path(
        package: PackageId,
        packages: &PackageInterner,
        symbols: &Interner,
        name: &str,
    ) -> DefPath {
        let name = symbols.intern(name).expect("symbol interns");
        DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(name),
                disambiguator: 0,
            }],
            packages,
            symbols,
        )
        .expect("path belongs to its interners")
    }

    #[test]
    fn definition_and_owner_ids_keep_their_scopes() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let path = def_path(package, &packages, &symbols, "Widget");
        let local_def = paths.intern(path.clone()).expect("definition path interns");
        let index = local_def.index();
        let owner = OwnerId { def_id: local_def };
        let mut local_ids = IndexVec::<ItemLocalId, ()>::new();
        let local_id = local_ids.push(()).expect("HIR local ID fits");

        assert_eq!(local_def.package(), package);
        let def_id = local_def.to_def_id();
        assert_eq!(def_id.package, package);
        assert_eq!(def_id.index, index);
        assert_eq!(paths.resolve_def_id(def_id), Ok(Some(path)));
        assert_eq!(HirId { owner, local_id }.owner, owner);
        assert_eq!(BodyId { owner }.owner, owner);
    }

    #[test]
    fn spans_preserve_source_and_desugaring_context() {
        let source = Span {
            file: FileId::new(3),
            range: TextRange::empty(sai_vfs::TextSize::new(5)),
            ctxt: SpanCtxt::Root,
        };
        let synthetic = Span {
            ctxt: SpanCtxt::Desugared(DesugaringKind::ForLoop),
            ..source
        };

        assert_eq!(source.file, synthetic.file);
        assert_eq!(source.range, synthetic.range);
        assert_eq!(source.ctxt, SpanCtxt::Root);
        assert_eq!(synthetic.ctxt, SpanCtxt::Desugared(DesugaringKind::ForLoop));
    }

    #[test]
    fn definition_paths_retain_disambiguated_components() {
        let symbols = sai_intern::Interner::new();
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let symbol = symbols.intern("Widget").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
                disambiguator: 2,
            }],
            &packages,
            &symbols,
        )
        .expect("path belongs to its interners");

        assert_eq!(path.data()[0].disambiguator, 2);
        assert_eq!(path.data()[0].data, DefPathData::TypeNs(symbol));
    }

    #[test]
    fn def_path_hash_uses_package_keys_and_resolved_symbols() {
        let first_packages = PackageInterner::new();
        first_packages.intern("noise:package#1").unwrap();
        let first_package = first_packages.intern("acme:app#1.0").unwrap();
        let first_symbols = Interner::new();
        first_symbols.intern("Noise").unwrap();
        let first_module = first_symbols.intern("ui").unwrap();
        let first_name = first_symbols.intern("Widget").unwrap();
        let first_path = DefPath::new(
            first_package,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Module(first_module),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(first_name),
                    disambiguator: 1,
                },
            ],
            &first_packages,
            &first_symbols,
        )
        .unwrap();

        let second_packages = PackageInterner::new();
        let second_package = second_packages.intern("acme:app#1.0").unwrap();
        let second_symbols = Interner::new();
        let second_name = second_symbols.intern("Widget").unwrap();
        let second_module = second_symbols.intern("ui").unwrap();
        let second_path = DefPath::new(
            second_package,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Module(second_module),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(second_name),
                    disambiguator: 1,
                },
            ],
            &second_packages,
            &second_symbols,
        )
        .unwrap();

        assert_eq!(
            DefPathHash::compute(&first_path, &first_packages, &first_symbols),
            DefPathHash::compute(&second_path, &second_packages, &second_symbols)
        );
        assert_eq!(
            DefPath::new(
                first_package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(first_name),
                    disambiguator: 1,
                }],
                &second_packages,
                &second_symbols,
            ),
            Err(DefPathError::InternerSessionMismatch)
        );
        assert_eq!(
            DefPath::new(
                second_package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::Module(first_module),
                    disambiguator: 0,
                }],
                &second_packages,
                &second_symbols,
            ),
            Err(DefPathError::InternerSessionMismatch)
        );
        assert_ne!(
            DefPathHash::compute(&first_path, &first_packages, &first_symbols),
            DefPathHash::compute(
                &DefPath::new(
                    first_package,
                    vec![DisambiguatedDefPathData {
                        data: DefPathData::TypeNs(first_name),
                        disambiguator: 1,
                    }],
                    &first_packages,
                    &first_symbols,
                )
                .unwrap(),
                &first_packages,
                &first_symbols,
            )
        );

        let matching_ids_in_other_session = DefPath::new(
            second_package,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Module(second_module),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(second_name),
                    disambiguator: 1,
                },
            ],
            &second_packages,
            &second_symbols,
        )
        .unwrap();
        assert_eq!(
            DefPathHash::compute(&first_path, &second_packages, &second_symbols),
            Err(DefPathError::InternerSessionMismatch)
        );
        assert_ne!(first_path, matching_ids_in_other_session);
    }

    #[test]
    fn error_symbol_has_a_collision_free_stable_hash_encoding() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let literal_marker = symbols.intern("\0SAI_ERROR_SYMBOL\0").unwrap();

        let error_path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(Symbol::ERROR),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let literal_path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(literal_marker),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();

        assert_ne!(
            DefPathHash::compute(&error_path, &packages, &symbols),
            DefPathHash::compute(&literal_path, &packages, &symbols)
        );
    }

    #[test]
    fn def_path_rejects_symbols_and_packages_outside_their_interners() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let name = symbols.intern("Widget").unwrap();

        assert_eq!(
            DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(name),
                    disambiguator: 0,
                }],
                &PackageInterner::new(),
                &symbols,
            ),
            Err(DefPathError::InternerSessionMismatch)
        );
        assert_eq!(
            DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(name),
                    disambiguator: 0,
                }],
                &packages,
                &Interner::new(),
            ),
            Err(DefPathError::InternerSessionMismatch)
        );

        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        assert_eq!(
            DefPathHash::compute(&path, &packages, &Interner::new()),
            Err(DefPathError::InternerSessionMismatch)
        );
    }

    #[test]
    fn definition_paths_are_interned_idempotently_and_resolve_by_local_id() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let path = def_path(package, &packages, &symbols, "Widget");
        let later_path = def_path(package, &packages, &symbols, "Later");

        let id = paths.intern(path.clone()).unwrap();
        assert_eq!(paths.intern(path.clone()), Ok(id));
        assert_eq!(paths.resolve(id), Ok(Some(path)));
        let later_id = paths.intern(later_path.clone()).unwrap();
        assert_ne!(id, later_id);
        assert_eq!(
            paths.resolve(id),
            Ok(Some(def_path(package, &packages, &symbols, "Widget")))
        );
        assert_eq!(paths.resolve(later_id), Ok(Some(later_path)));
        assert_eq!(paths.len(), Ok(2));
        assert_eq!(paths.is_empty(), Ok(false));
    }

    #[test]
    fn nested_closure_and_async_paths_keep_lexical_parents_and_ordinals() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner = paths
            .intern(def_path(package, &packages, &symbols, "f"))
            .unwrap();

        let closure = paths.intern_nested(owner, DefPathData::Closure, 0).unwrap();
        let nested_closure = paths
            .intern_nested(closure, DefPathData::Closure, 1)
            .unwrap();
        let async_block = paths
            .intern_nested(owner, DefPathData::AsyncBlock, 2)
            .unwrap();

        assert_eq!(
            paths.resolve(closure).unwrap().unwrap().data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            })
        );
        let nested_path = paths.resolve(nested_closure).unwrap().unwrap();
        assert_eq!(nested_path.data().len(), 3);
        assert_eq!(
            nested_path.data()[1],
            DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            }
        );
        assert_eq!(
            nested_path.data()[2],
            DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 1,
            }
        );
        assert_eq!(
            paths.resolve(async_block).unwrap().unwrap().data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::AsyncBlock,
                disambiguator: 2,
            })
        );
        assert_eq!(
            paths.intern_nested(owner, DefPathData::Closure, 0),
            Ok(closure)
        );
        assert_eq!(
            paths.intern_nested(
                owner,
                DefPathData::TypeNs(symbols.intern("Bad").unwrap()),
                3
            ),
            Err(DefPathInternerError::InvalidNestedComponent)
        );
    }

    #[test]
    fn definition_path_interner_interns_field_batches_atomically() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner = paths
            .intern(def_path(package, &packages, &symbols, "Record"))
            .unwrap();
        let field = symbols.intern("value").unwrap();

        let fields = paths
            .intern_fields(owner, &[(field, 0), (field, 1)])
            .expect("duplicate field names receive separate paths");
        assert_eq!(fields.len(), 2);
        for (disambiguator, field_id) in fields.iter().enumerate() {
            let path = paths
                .resolve(*field_id)
                .expect("field path resolves")
                .expect("field path exists");
            assert_eq!(
                path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Field(field),
                    disambiguator: disambiguator as u32,
                })
            );
        }
        assert_eq!(paths.len(), Ok(3));
        assert_eq!(paths.intern_fields(owner, &[]), Ok(Vec::new()));

        let foreign_symbols = Interner::new();
        let foreign_field = foreign_symbols.intern("foreign").unwrap();
        assert_eq!(
            paths.intern_fields(owner, &[(field, 2), (foreign_field, 0)]),
            Err(DefPathInternerError::InternerSessionMismatch)
        );
        assert_eq!(
            paths.len(),
            Ok(3),
            "a failed field batch leaves the interner unchanged"
        );
    }

    #[test]
    fn definition_path_interner_interns_variant_and_nested_field_batches_atomically() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner = paths
            .intern(def_path(package, &packages, &symbols, "Choice"))
            .unwrap();
        let repeated = symbols.intern("Repeated").unwrap();
        let payload = symbols.intern("payload").unwrap();

        let variants = paths
            .intern_variants(owner, &[(repeated, 0), (repeated, 1)])
            .expect("duplicate variant names receive separate paths");
        assert_eq!(variants.len(), 2);
        for (disambiguator, variant_id) in variants.iter().enumerate() {
            let path = paths
                .resolve(*variant_id)
                .expect("variant path resolves")
                .expect("variant path exists");
            assert_eq!(
                path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Variant(repeated),
                    disambiguator: disambiguator as u32,
                })
            );
        }

        let fields = paths
            .intern_fields_for_parents(&[
                (variants[0], payload, 0),
                (variants[1], payload, 0),
                (variants[1], payload, 1),
            ])
            .expect("variant fields are interned below their variants");
        assert_eq!(fields.len(), 3);
        assert_eq!(paths.resolve(fields[0]).unwrap().unwrap().data().len(), 3);
        assert_eq!(
            paths.resolve(fields[0]).unwrap().unwrap().data()[1],
            DisambiguatedDefPathData {
                data: DefPathData::Variant(repeated),
                disambiguator: 0,
            }
        );
        assert_eq!(
            paths.resolve(fields[1]).unwrap().unwrap().data()[1],
            DisambiguatedDefPathData {
                data: DefPathData::Variant(repeated),
                disambiguator: 1,
            }
        );
        assert_eq!(
            paths.resolve(fields[2]).unwrap().unwrap().data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Field(payload),
                disambiguator: 1,
            })
        );
        assert_eq!(paths.len(), Ok(6));

        let foreign_symbols = Interner::new();
        let foreign_name = foreign_symbols.intern("foreign").unwrap();
        assert_eq!(
            paths.intern_fields_for_parents(&[
                (variants[0], payload, 1),
                (variants[1], foreign_name, 0),
            ]),
            Err(DefPathInternerError::InternerSessionMismatch)
        );
        assert_eq!(
            paths.intern_variants(owner, &[(repeated, 2), (foreign_name, 0)]),
            Err(DefPathInternerError::InternerSessionMismatch)
        );
        assert_eq!(
            paths.len(),
            Ok(6),
            "failed variant and field batches leave the interner unchanged"
        );
    }

    #[test]
    fn definition_path_interner_interns_associated_item_batches_atomically() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let trait_name = symbols.intern("Container").unwrap();
        let associated_type = symbols.intern("Item").unwrap();
        let method = symbols.intern("get").unwrap();
        let parent = paths
            .intern(
                DefPath::new(
                    package,
                    vec![DisambiguatedDefPathData {
                        data: DefPathData::TypeNs(trait_name),
                        disambiguator: 0,
                    }],
                    &packages,
                    &symbols,
                )
                .unwrap(),
            )
            .unwrap();

        let associated = paths
            .intern_assoc_items(
                parent,
                &[
                    (DefPathData::AssocType(associated_type), 0),
                    (DefPathData::AssocType(associated_type), 1),
                    (DefPathData::AssocFn(method), 0),
                    (DefPathData::AssocFn(method), 1),
                ],
            )
            .expect("duplicate associated item declarations receive distinct paths");
        assert_eq!(associated.len(), 4);
        for (index, component) in [
            DefPathData::AssocType(associated_type),
            DefPathData::AssocType(associated_type),
            DefPathData::AssocFn(method),
            DefPathData::AssocFn(method),
        ]
        .into_iter()
        .enumerate()
        {
            let path = paths
                .resolve(associated[index])
                .expect("associated item path resolves")
                .expect("associated item path exists");
            assert_eq!(
                path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: component,
                    disambiguator: if index % 2 == 0 { 0 } else { 1 },
                })
            );
        }
        assert_eq!(paths.len(), Ok(5));

        let foreign_symbols = Interner::new();
        let foreign_name = foreign_symbols.intern("Foreign").unwrap();
        assert_eq!(
            paths.intern_assoc_items(
                parent,
                &[
                    (DefPathData::AssocType(associated_type), 2),
                    (DefPathData::AssocFn(foreign_name), 0),
                ],
            ),
            Err(DefPathInternerError::InternerSessionMismatch)
        );
        assert_eq!(
            paths.intern_assoc_items(parent, &[(DefPathData::Field(method), 0)]),
            Err(DefPathInternerError::InvalidAssociatedItem)
        );
        assert_eq!(
            paths.len(),
            Ok(5),
            "failed associated item batches leave the interner unchanged"
        );
    }

    #[test]
    fn definition_path_interner_rejects_other_packages_and_sessions() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let other_package = packages.intern("acme:other#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();

        assert_eq!(
            paths.intern(def_path(other_package, &packages, &symbols, "Widget")),
            Err(DefPathInternerError::WrongPackage)
        );

        let other_paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let first_id = paths
            .intern(def_path(package, &packages, &symbols, "First"))
            .unwrap();
        let other_id = other_paths
            .intern(def_path(package, &packages, &symbols, "Other"))
            .unwrap();
        assert_eq!(first_id.index(), other_id.index());
        assert_ne!(first_id, other_id);
        let first_def_id = first_id.to_def_id();
        let other_def_id = other_id.to_def_id();
        assert_ne!(first_def_id, other_def_id);
        let foreign_def_id = DefId::from_foreign(package, first_id.index());
        assert_ne!(first_def_id, foreign_def_id);
        assert_eq!(
            paths.resolve(other_id),
            Err(DefPathInternerError::ForeignDefinitionId)
        );
        assert_eq!(
            other_paths.resolve(first_id),
            Err(DefPathInternerError::ForeignDefinitionId)
        );
        assert_eq!(
            paths.resolve_def_id(other_def_id),
            Err(DefPathInternerError::ForeignDefinitionId)
        );
        assert_eq!(
            other_paths.resolve_def_id(first_def_id),
            Err(DefPathInternerError::ForeignDefinitionId)
        );
        assert_eq!(
            paths.resolve_def_id(foreign_def_id),
            Err(DefPathInternerError::ForeignDefinitionId)
        );

        let foreign_packages = PackageInterner::new();
        let foreign_package = foreign_packages.intern("acme:app#1.0").unwrap();
        let foreign_symbols = Interner::new();
        assert_eq!(
            paths.intern(def_path(
                foreign_package,
                &foreign_packages,
                &foreign_symbols,
                "Widget"
            )),
            Err(DefPathInternerError::InternerSessionMismatch)
        );
    }

    #[test]
    fn concurrent_definition_path_interning_returns_one_local_id() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let symbols = Interner::new();
        let paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let path = def_path(package, &packages, &symbols, "Widget");

        let workers = (0..16)
            .map(|_| {
                let paths = Arc::clone(&paths);
                let path = path.clone();
                thread::spawn(move || paths.intern(path).expect("path interns"))
            })
            .collect::<Vec<_>>();
        let ids = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker completes"))
            .collect::<Vec<_>>();

        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_eq!(paths.len(), Ok(1));
    }
}
