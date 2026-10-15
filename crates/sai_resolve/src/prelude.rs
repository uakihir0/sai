//! Canonical bindings in the implicit prelude.
//!
//! The table in spec/16 §16.4 contains primitive names and items from
//! `sai:core` and `sai:alloc`. This module owns that fixed name/path table and
//! builds its `Symbol`-keyed resolution map. The caller supplies canonical
//! item lookup because the finalized standard-package member tables are
//! assembled outside `sai_resolve`.

use crate::package_scopes::{DefKind, Res};
use sai_hir::{PackageId, Symbol};
use sai_intern::{InternError, Interner, PackageIdError, PackageInterner};
use std::collections::HashMap;

/// Primitive types and constructors available through the prelude.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    ISize,
    USize,
    F32,
    F64,
    Bool,
    Byte,
    Char,
    Never,
    Array,
}

/// Namespace component in a canonical prelude item path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CanonicalPreludeSegment {
    Module(&'static str),
    Type(&'static str),
    Value(&'static str),
    Variant(&'static str),
}

/// Semantic kind expected at a canonical prelude path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CanonicalPreludeKind {
    Type,
    Trait,
    Variant,
    Function,
}

/// Canonical item path used to resolve one non-primitive prelude binding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CanonicalPreludeItemPath {
    /// Canonical package identity (`sai:core` or `sai:alloc`).
    pub package: &'static str,
    /// Module and item components, including the final item.
    pub segments: &'static [CanonicalPreludeSegment],
    /// Expected category of the item at the final path component.
    pub kind: CanonicalPreludeKind,
}

/// A source-level name and the standard-library item it denotes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CanonicalPreludeItem {
    pub name: &'static str,
    pub path: CanonicalPreludeItemPath,
}

/// One binding in the prelude.
///
/// Primitive bindings remain explicit in the provider so callers can
/// distinguish built-in names from canonical standard-library items. `Res`
/// can also represent these primitives for owner-level path results.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PreludeBinding {
    Primitive(PrimTy),
    Item(Res),
}

/// Complete prelude lookup table with deterministic lexical iteration.
///
/// `Symbol` is an opaque session-local key and does not implement `Ord`, so
/// lookup uses a hash map and the ordered entry slice provides deterministic
/// iteration for consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prelude {
    by_symbol: HashMap<Symbol, PreludeBinding>,
    entries: Vec<(Symbol, PreludeBinding)>,
}

impl Prelude {
    /// Look up a prelude binding with a symbol from the interner used to build
    /// this table.
    pub fn get(&self, name: Symbol) -> Option<&PreludeBinding> {
        self.by_symbol.get(&name)
    }

    /// Return entries sorted by their source-level spelling.
    pub fn entries(&self) -> &[(Symbol, PreludeBinding)] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

macro_rules! prelude_type {
    ($name:literal, $package:literal, $($module:literal),+ $(,)?) => {
        CanonicalPreludeItem {
            name: $name,
            path: CanonicalPreludeItemPath {
                package: $package,
                segments: &[
                    $(CanonicalPreludeSegment::Module($module),)+
                    CanonicalPreludeSegment::Type($name),
                ],
                kind: CanonicalPreludeKind::Type,
            },
        }
    };
}

macro_rules! prelude_trait_in_package {
    ($name:literal, $package:literal, $($module:literal),+ $(,)?) => {
        CanonicalPreludeItem {
            name: $name,
            path: CanonicalPreludeItemPath {
                package: $package,
                segments: &[
                    $(CanonicalPreludeSegment::Module($module),)+
                    CanonicalPreludeSegment::Type($name),
                ],
                kind: CanonicalPreludeKind::Trait,
            },
        }
    };
}

macro_rules! prelude_trait {
    ($name:literal, $($module:literal),+ $(,)?) => {
        prelude_trait_in_package!($name, "sai:core", $($module),+)
    };
}

macro_rules! prelude_variant {
    ($name:literal, $module:literal, $parent:literal) => {
        CanonicalPreludeItem {
            name: $name,
            path: CanonicalPreludeItemPath {
                package: "sai:core",
                segments: &[
                    CanonicalPreludeSegment::Module($module),
                    CanonicalPreludeSegment::Type($parent),
                    CanonicalPreludeSegment::Variant($name),
                ],
                kind: CanonicalPreludeKind::Variant,
            },
        }
    };
}

macro_rules! prelude_function {
    ($name:literal) => {
        CanonicalPreludeItem {
            name: $name,
            path: CanonicalPreludeItemPath {
                package: "sai:core",
                segments: &[
                    CanonicalPreludeSegment::Module("panic"),
                    CanonicalPreludeSegment::Value($name),
                ],
                kind: CanonicalPreludeKind::Function,
            },
        }
    };
}

/// Complete non-primitive prelude item table from spec/16 §16.4.
///
/// Reserved primitive type names and the built-in `Array` constructor are
/// supplied directly as [`PreludeBinding::Primitive`] by [`build_prelude`].
pub const CANONICAL_PRELUDE_ITEMS: &[CanonicalPreludeItem] = &[
    prelude_type!("Str", "sai:core", "str"),
    prelude_type!("Slice", "sai:core", "slice"),
    prelude_type!("SliceMut", "sai:core", "slice"),
    prelude_type!("Option", "sai:core", "option"),
    prelude_variant!("Some", "option", "Option"),
    prelude_variant!("None", "option", "Option"),
    prelude_type!("Result", "sai:core", "result"),
    prelude_variant!("Ok", "result", "Result"),
    prelude_variant!("Err", "result", "Result"),
    prelude_type!("Ordering", "sai:core", "cmp"),
    prelude_type!("String", "sai:alloc", "string"),
    prelude_type!("Vec", "sai:alloc", "vec"),
    prelude_type!("Bytes", "sai:alloc", "vec"),
    prelude_type!("Box", "sai:alloc", "boxed"),
    prelude_trait!("Copy", "marker"),
    prelude_trait!("Clone", "clone"),
    prelude_trait!("Default", "default"),
    prelude_trait!("PartialEq", "cmp"),
    prelude_trait!("Eq", "cmp"),
    prelude_trait!("PartialOrd", "cmp"),
    prelude_trait!("Ord", "cmp"),
    prelude_trait!("Hash", "hash"),
    prelude_trait!("From", "convert"),
    prelude_trait!("TryFrom", "convert"),
    prelude_trait!("Add", "ops"),
    prelude_trait!("Sub", "ops"),
    prelude_trait!("Mul", "ops"),
    prelude_trait!("Div", "ops"),
    prelude_trait!("Rem", "ops"),
    prelude_trait!("Neg", "ops"),
    prelude_trait!("BitAnd", "ops"),
    prelude_trait!("BitOr", "ops"),
    prelude_trait!("BitXor", "ops"),
    prelude_trait!("BitNot", "ops"),
    prelude_trait!("Index", "ops"),
    prelude_trait!("IndexMut", "ops"),
    prelude_trait!("Fn", "ops"),
    prelude_trait!("FnMut", "ops"),
    prelude_trait!("FnOnce", "ops"),
    prelude_trait!("Drop", "ops"),
    prelude_trait!("IntoIterator", "iter"),
    prelude_trait!("Iterator", "iter"),
    prelude_trait!("FromIterator", "iter"),
    prelude_trait!("Format", "fmt"),
    prelude_trait_in_package!("ToString", "sai:alloc", "fmt"),
    prelude_trait!("Future", "future"),
    prelude_trait!("Stream", "future"),
    prelude_trait!("Arbitrary", "testing"),
    prelude_function!("panic"),
    prelude_function!("unreachable"),
];

/// Error while assembling the canonical prelude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreludeBuildError<E> {
    Package(PackageIdError),
    WrongPackageIdentity {
        expected: &'static str,
        actual: String,
    },
    CoreAndAllocAreSamePackage,
    Symbol(InternError),
    ItemLookup {
        name: &'static str,
        source: E,
    },
    MissingItem {
        name: &'static str,
        path: CanonicalPreludeItemPath,
    },
    WrongItemKind {
        name: &'static str,
        expected: CanonicalPreludeKind,
        actual: DefKind,
    },
    WrongItemPackage {
        name: &'static str,
        expected: PackageId,
        actual: PackageId,
    },
    InvalidItemResolution {
        name: &'static str,
        actual: Res,
    },
    DuplicateName(&'static str),
}

/// Build the fixed prelude from primitive bindings and canonical item lookup.
///
/// `resolve_item` is called once for each entry in
/// [`CANONICAL_PRELUDE_ITEMS`]. It should resolve the supplied path without
/// visibility checks and return the item's ordinary module resolution. The
/// result is checked against the expected category and package before it is
/// inserted. The prelude is independent of the current package's scopes.
pub fn build_prelude<E>(
    packages: &PackageInterner,
    symbols: &Interner,
    core_package: PackageId,
    alloc_package: PackageId,
    mut resolve_item: impl FnMut(&CanonicalPreludeItemPath) -> Result<Option<Res>, E>,
) -> Result<Prelude, PreludeBuildError<E>> {
    if core_package == alloc_package {
        return Err(PreludeBuildError::CoreAndAllocAreSamePackage);
    }
    let core_identity = packages
        .resolve(core_package)
        .map_err(PreludeBuildError::Package)?;
    if core_identity.as_ref() != "sai:core" {
        return Err(PreludeBuildError::WrongPackageIdentity {
            expected: "sai:core",
            actual: core_identity.to_string(),
        });
    }
    let alloc_identity = packages
        .resolve(alloc_package)
        .map_err(PreludeBuildError::Package)?;
    if alloc_identity.as_ref() != "sai:alloc" {
        return Err(PreludeBuildError::WrongPackageIdentity {
            expected: "sai:alloc",
            actual: alloc_identity.to_string(),
        });
    }
    let mut named_bindings = PRIMITIVE_BINDINGS
        .into_iter()
        .map(|(name, primitive)| (name, PreludeBinding::Primitive(primitive)))
        .collect::<Vec<_>>();

    for item in CANONICAL_PRELUDE_ITEMS {
        let resolved =
            resolve_item(&item.path).map_err(|source| PreludeBuildError::ItemLookup {
                name: item.name,
                source,
            })?;
        let Some(resolved) = resolved else {
            return Err(PreludeBuildError::MissingItem {
                name: item.name,
                path: item.path,
            });
        };
        let Res::Def(actual_kind, def_id) = resolved else {
            return Err(PreludeBuildError::InvalidItemResolution {
                name: item.name,
                actual: resolved,
            });
        };
        if !item.path.kind.accepts(actual_kind) {
            return Err(PreludeBuildError::WrongItemKind {
                name: item.name,
                expected: item.path.kind,
                actual: actual_kind,
            });
        }
        let expected_package = match item.path.package {
            "sai:core" => core_package,
            "sai:alloc" => alloc_package,
            _ => unreachable!("canonical prelude paths use only standard packages"),
        };
        if def_id.package != expected_package {
            return Err(PreludeBuildError::WrongItemPackage {
                name: item.name,
                expected: expected_package,
                actual: def_id.package,
            });
        }
        named_bindings.push((item.name, PreludeBinding::Item(resolved)));
    }

    named_bindings.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut by_symbol = HashMap::with_capacity(named_bindings.len());
    let mut entries = Vec::with_capacity(named_bindings.len());
    for (name, binding) in named_bindings {
        let symbol = symbols.intern(name).map_err(PreludeBuildError::Symbol)?;
        if by_symbol.insert(symbol, binding).is_some() {
            return Err(PreludeBuildError::DuplicateName(name));
        }
        entries.push((symbol, binding));
    }

    Ok(Prelude { by_symbol, entries })
}

impl CanonicalPreludeKind {
    fn accepts(self, actual: DefKind) -> bool {
        match self {
            Self::Type => matches!(
                actual,
                DefKind::Struct
                    | DefKind::Enum
                    | DefKind::Distinct
                    | DefKind::Refined
                    | DefKind::Alias
                    | DefKind::Resource
            ),
            Self::Trait => actual == DefKind::Trait,
            Self::Variant => actual == DefKind::Variant,
            Self::Function => actual == DefKind::Fn,
        }
    }
}

const PRIMITIVE_BINDINGS: [(&str, PrimTy); 19] = [
    ("I8", PrimTy::I8),
    ("I16", PrimTy::I16),
    ("I32", PrimTy::I32),
    ("I64", PrimTy::I64),
    ("U8", PrimTy::U8),
    ("U16", PrimTy::U16),
    ("U32", PrimTy::U32),
    ("U64", PrimTy::U64),
    ("ISize", PrimTy::ISize),
    ("USize", PrimTy::USize),
    ("F32", PrimTy::F32),
    ("F64", PrimTy::F64),
    ("Bool", PrimTy::Bool),
    ("Byte", PrimTy::Byte),
    ("Char", PrimTy::Char),
    ("Never", PrimTy::Never),
    ("Int", PrimTy::I64),
    ("UInt", PrimTy::U64),
    ("Array", PrimTy::Array),
];

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::{DefId, DefIndex};
    use sai_intern::Idx;
    use std::convert::Infallible;

    fn package_pair(packages: &PackageInterner) -> (PackageId, PackageId) {
        (
            packages.intern("sai:core").expect("core package interns"),
            packages.intern("sai:alloc").expect("alloc package interns"),
        )
    }

    fn test_resolution(
        path: &CanonicalPreludeItemPath,
        index: u32,
        core: PackageId,
        alloc: PackageId,
    ) -> Res {
        let package = match path.package {
            "sai:core" => core,
            "sai:alloc" => alloc,
            _ => panic!("unexpected canonical package"),
        };
        let kind = match path.kind {
            CanonicalPreludeKind::Type => DefKind::Alias,
            CanonicalPreludeKind::Trait => DefKind::Trait,
            CanonicalPreludeKind::Variant => DefKind::Variant,
            CanonicalPreludeKind::Function => DefKind::Fn,
        };
        Res::Def(
            kind,
            DefId::from_foreign(package, DefIndex::from_raw(index)),
        )
    }

    #[test]
    fn builds_the_complete_spec_prelude_with_primitive_aliases() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let (core, alloc) = package_pair(&packages);
        let mut lookup_count = 0_usize;

        let prelude = build_prelude(&packages, &symbols, core, alloc, |path| {
            let index = u32::try_from(lookup_count).expect("test item index fits u32");
            let resolution = test_resolution(path, index, core, alloc);
            lookup_count += 1;
            Ok::<_, Infallible>(Some(resolution))
        })
        .expect("canonical paths resolve");

        assert_eq!(lookup_count, CANONICAL_PRELUDE_ITEMS.len());
        assert_eq!(CANONICAL_PRELUDE_ITEMS.len(), 50);
        assert_eq!(prelude.len(), 69);
        let resolution = |name: &str| {
            *prelude
                .get(symbols.intern(name).expect("name interns"))
                .expect("prelude name exists")
        };
        assert_eq!(resolution("I8"), PreludeBinding::Primitive(PrimTy::I8));
        assert_eq!(
            resolution("Never"),
            PreludeBinding::Primitive(PrimTy::Never)
        );
        assert_eq!(resolution("Int"), PreludeBinding::Primitive(PrimTy::I64));
        assert_eq!(resolution("UInt"), PreludeBinding::Primitive(PrimTy::U64));
        assert_eq!(
            resolution("Array"),
            PreludeBinding::Primitive(PrimTy::Array)
        );
        assert!(matches!(
            resolution("Some"),
            PreludeBinding::Item(Res::Def(DefKind::Variant, _))
        ));
        assert!(matches!(
            resolution("Copy"),
            PreludeBinding::Item(Res::Def(DefKind::Trait, _))
        ));
        assert!(matches!(
            resolution("panic"),
            PreludeBinding::Item(Res::Def(DefKind::Fn, _))
        ));
        assert!(matches!(
            resolution("String"),
            PreludeBinding::Item(Res::Def(DefKind::Alias, _))
        ));
        assert!(matches!(
            resolution("ToString"),
            PreludeBinding::Item(Res::Def(DefKind::Trait, _))
        ));
        assert!(prelude.entries().windows(2).all(|pair| {
            let previous = symbols.resolve(pair[0].0).expect("symbol resolves");
            let next = symbols.resolve(pair[1].0).expect("symbol resolves");
            previous.as_bytes() < next.as_bytes()
        }));
        let mut actual_names = prelude
            .entries()
            .iter()
            .map(|(symbol, _)| {
                symbols
                    .resolve(*symbol)
                    .expect("prelude symbol resolves")
                    .to_string()
            })
            .collect::<Vec<_>>();
        actual_names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let mut expected_names = [
            "I8",
            "I16",
            "I32",
            "I64",
            "U8",
            "U16",
            "U32",
            "U64",
            "ISize",
            "USize",
            "F32",
            "F64",
            "Bool",
            "Byte",
            "Char",
            "Never",
            "Int",
            "UInt",
            "Array",
            "Str",
            "Slice",
            "SliceMut",
            "Option",
            "Some",
            "None",
            "Result",
            "Ok",
            "Err",
            "Ordering",
            "String",
            "Vec",
            "Bytes",
            "Box",
            "Copy",
            "Clone",
            "Default",
            "PartialEq",
            "Eq",
            "PartialOrd",
            "Ord",
            "Hash",
            "From",
            "TryFrom",
            "Add",
            "Sub",
            "Mul",
            "Div",
            "Rem",
            "Neg",
            "BitAnd",
            "BitOr",
            "BitXor",
            "BitNot",
            "Index",
            "IndexMut",
            "Fn",
            "FnMut",
            "FnOnce",
            "Drop",
            "IntoIterator",
            "Iterator",
            "FromIterator",
            "Format",
            "ToString",
            "Future",
            "Stream",
            "Arbitrary",
            "panic",
            "unreachable",
        ]
        .map(str::to_owned)
        .to_vec();
        expected_names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        assert_eq!(actual_names, expected_names);
    }

    #[test]
    fn canonical_paths_cover_the_spec_packages_and_path_namespaces() {
        assert!(CANONICAL_PRELUDE_ITEMS.iter().all(|item| {
            matches!(item.path.package, "sai:core" | "sai:alloc") && !item.path.segments.is_empty()
        }));
        let variant = CANONICAL_PRELUDE_ITEMS
            .iter()
            .find(|item| item.name == "Some")
            .expect("Option.Some is a prelude item");
        assert_eq!(
            variant.path.segments,
            [
                CanonicalPreludeSegment::Module("option"),
                CanonicalPreludeSegment::Type("Option"),
                CanonicalPreludeSegment::Variant("Some"),
            ]
        );
        let allocation_type = CANONICAL_PRELUDE_ITEMS
            .iter()
            .find(|item| item.name == "String")
            .expect("String is a prelude item");
        assert_eq!(allocation_type.path.package, "sai:alloc");
        assert_eq!(
            allocation_type.path.segments,
            [
                CanonicalPreludeSegment::Module("string"),
                CanonicalPreludeSegment::Type("String"),
            ]
        );
    }

    #[test]
    fn rejects_missing_items_wrong_kinds_and_wrong_packages() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let (core, alloc) = package_pair(&packages);

        let missing = build_prelude(&packages, &symbols, core, alloc, |_path| {
            Ok::<_, Infallible>(None)
        });
        assert!(matches!(
            missing,
            Err(PreludeBuildError::MissingItem { name: "Str", .. })
        ));

        let wrong_kind = build_prelude(&packages, &symbols, core, alloc, |path| {
            let package = if path.package == "sai:core" {
                core
            } else {
                alloc
            };
            Ok::<_, Infallible>(Some(Res::Def(
                DefKind::Fn,
                DefId::from_foreign(package, DefIndex::from_raw(0)),
            )))
        });
        assert!(matches!(
            wrong_kind,
            Err(PreludeBuildError::WrongItemKind {
                name: "Str",
                expected: CanonicalPreludeKind::Type,
                actual: DefKind::Fn
            })
        ));

        let wrong_package = build_prelude(&packages, &symbols, core, alloc, |path| {
            let package = if path.package == "sai:core" {
                alloc
            } else {
                core
            };
            Ok::<_, Infallible>(Some(test_resolution(path, 0, package, package)))
        });
        assert!(matches!(
            wrong_package,
            Err(PreludeBuildError::WrongItemPackage { name: "Str", .. })
        ));
    }

    #[test]
    fn rejects_wrong_standard_package_ids_and_preserves_lookup_errors() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let core = packages.intern("sai:core").expect("core package interns");
        let alloc = packages.intern("sai:alloc").expect("alloc package interns");
        let wrong_core = packages.intern("acme:core").expect("wrong package interns");
        let invalid_identity = build_prelude::<Infallible>(
            &packages,
            &symbols,
            wrong_core,
            alloc,
            |_path| unreachable!(),
        );
        assert!(matches!(
            invalid_identity,
            Err(PreludeBuildError::WrongPackageIdentity {
                expected: "sai:core",
                ..
            })
        ));

        let lookup_error = build_prelude(&packages, &symbols, core, alloc, |_path| {
            Err("standard package lookup failed")
        });
        assert!(matches!(
            lookup_error,
            Err(PreludeBuildError::ItemLookup {
                name: "Str",
                source: "standard package lookup failed"
            })
        ));
    }
}
