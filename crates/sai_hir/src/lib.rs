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

use std::collections::HashMap;
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
