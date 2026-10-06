#![forbid(unsafe_code)]

use std::sync::Arc;

pub use sai_intern::{BindingId, DefIndex, ItemLocalId, PackageId, Symbol};
use sai_intern::{Interner, InternerSessionId, LookupError, PackageIdError, PackageInterner};
use sai_stable_hash::StableHasher;
pub use sai_vfs::{FileId, TextRange};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DefId {
    pub package: PackageId,
    pub index: DefIndex,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalDefId {
    pub index: DefIndex,
}

impl LocalDefId {
    pub const fn to_def_id(self, package: PackageId) -> DefId {
        DefId {
            package,
            index: self.index,
        }
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
    let value = symbols.resolve(symbol).map_err(DefPathError::Symbol)?;
    write_stable_string(hasher, &value);
    Ok(())
}

fn write_stable_string(hasher: &mut StableHasher, value: &str) {
    hasher.write_len(u64::try_from(value.len()).unwrap_or(u64::MAX));
    hasher.write_raw_bytes(value.as_bytes());
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

    #[test]
    fn definition_and_owner_ids_keep_their_scopes() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let mut definitions = IndexVec::<DefIndex, ()>::new();
        let index = definitions.push(()).expect("definition index fits");
        let local_def = LocalDefId { index };
        let owner = OwnerId { def_id: local_def };
        let mut local_ids = IndexVec::<ItemLocalId, ()>::new();
        let local_id = local_ids.push(()).expect("HIR local ID fits");

        assert_eq!(local_def.to_def_id(package), DefId { package, index });
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
}
