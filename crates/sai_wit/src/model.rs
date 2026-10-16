//! Parser-independent data model for a resolved WIT package.
//!
//! The model deliberately stores declarations in ordered vectors. These
//! vectors preserve WIT declaration order and are indexed with local newtypes
//! so an interface, world, and item index cannot be mixed accidentally.

use super::WitUnit;
use sai_package::version::Version;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut, Range};
use std::sync::Arc;

mod index_sealed {
    pub trait Sealed {
        fn from_raw(raw: u32) -> Self
        where
            Self: Sized;
    }
}

/// A WIT model index whose numeric value is local to one typed vector.
pub trait WitIndex: index_sealed::Sealed + Copy + Eq + std::fmt::Debug {
    const STABLE_TAG: u8;

    fn raw(self) -> u32;
}

/// A vector addressed by one WIT model index type.
///
/// It keeps the backing `Vec` private, creates indices only when appending,
/// and rejects vectors too large for the index representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitIndexVec<I: WitIndex, T> {
    values: Vec<T>,
    index: PhantomData<fn() -> I>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WitIndexVecError;

impl std::fmt::Display for WitIndexVecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WIT model vector exceeds its 32-bit index range")
    }
}

impl std::error::Error for WitIndexVecError {}

impl<I: WitIndex, T> WitIndexVec<I, T> {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            index: PhantomData,
        }
    }

    pub fn with_capacity(capacity: usize) -> Result<Self, WitIndexVecError> {
        if capacity
            .checked_sub(1)
            .is_some_and(|last| u32::try_from(last).is_err())
        {
            return Err(WitIndexVecError);
        }
        Ok(Self {
            values: Vec::with_capacity(capacity),
            index: PhantomData,
        })
    }

    pub fn try_from_vec(values: Vec<T>) -> Result<Self, WitIndexVecError> {
        if values
            .len()
            .checked_sub(1)
            .is_some_and(|last| u32::try_from(last).is_err())
        {
            return Err(WitIndexVecError);
        }
        Ok(Self {
            values,
            index: PhantomData,
        })
    }

    pub fn push(&mut self, value: T) -> Result<I, WitIndexVecError> {
        let index = u32::try_from(self.values.len()).map_err(|_| WitIndexVecError)?;
        self.values.push(value);
        Ok(index_sealed::Sealed::from_raw(index))
    }

    pub fn get(&self, index: I) -> Option<&T> {
        self.values.get(usize::try_from(index.raw()).ok()?)
    }

    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.values.get_mut(usize::try_from(index.raw()).ok()?)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.values.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.values.iter_mut()
    }

    pub fn iter_enumerated(&self) -> impl ExactSizeIterator<Item = (I, &T)> {
        self.values.iter().enumerate().map(|(index, value)| {
            (
                index_sealed::Sealed::from_raw(
                    u32::try_from(index).expect("WIT index was range-checked"),
                ),
                value,
            )
        })
    }
}

impl<I: WitIndex, T> Default for WitIndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: WitIndex, T> Index<I> for WitIndexVec<I, T> {
    type Output = T;

    fn index(&self, index: I) -> &Self::Output {
        self.get(index).expect("WIT model index is out of bounds")
    }
}

impl<I: WitIndex, T> IndexMut<I> for WitIndexVec<I, T> {
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        self.get_mut(index)
            .expect("WIT model index is out of bounds")
    }
}

impl<'a, I: WitIndex, T> IntoIterator for &'a WitIndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

/// Index of an interface in [`WitPackage::interfaces`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WitIfaceIdx(u32);

impl index_sealed::Sealed for WitIfaceIdx {
    fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

impl WitIndex for WitIfaceIdx {
    const STABLE_TAG: u8 = 0x91;

    fn raw(self) -> u32 {
        self.0
    }
}

impl WitIfaceIdx {
    /// Creates an interface index from its zero-based vector position.
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the zero-based vector position.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the raw zero-based index.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Index of a world in [`WitPackage::worlds`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WitWorldIdx(u32);

impl index_sealed::Sealed for WitWorldIdx {
    fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

impl WitIndex for WitWorldIdx {
    const STABLE_TAG: u8 = 0x92;

    fn raw(self) -> u32 {
        self.0
    }
}

impl WitWorldIdx {
    /// Creates a world index from its zero-based vector position.
    #[cfg(test)]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the zero-based vector position.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the raw zero-based index.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Index of an item in [`WitInterface::items`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WitItemIdx(u32);

impl index_sealed::Sealed for WitItemIdx {
    fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

impl WitIndex for WitItemIdx {
    const STABLE_TAG: u8 = 0x93;

    fn raw(self) -> u32 {
        self.0
    }
}

impl WitItemIdx {
    /// Creates an item index from its zero-based vector position.
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the zero-based vector position.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Returns the raw zero-based index.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A parser-independent copy of one resolved WIT package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackage {
    pub unit: WitUnit,
    pub name: WitPackageName,
    /// Interfaces in WIT declaration order.
    pub interfaces: WitIndexVec<WitIfaceIdx, WitInterface>,
    /// Worlds in WIT declaration order.
    pub worlds: WitIndexVec<WitWorldIdx, WitWorld>,
}

/// The namespace, name, and optional version of a WIT package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageName {
    pub namespace: String,
    pub name: String,
    pub version: Option<Version>,
}

/// A WIT interface and its members.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitInterface {
    /// The WIT name, in kebab case.
    pub name: String,
    pub docs: Option<Arc<str>>,
    pub deprecated: Option<Version>,
    /// Items in WIT order: types first, followed by functions.
    pub items: WitIndexVec<WitItemIdx, WitItem>,
    /// Absent for embedded and generated units.
    pub span: Option<WitSpan>,
}

/// A type or function declared by an interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitItem {
    pub name: String,
    pub docs: Option<Arc<str>>,
    pub deprecated: Option<Version>,
    pub kind: WitItemKind,
    pub unsupported: Option<Unsupported>,
    pub span: Option<WitSpan>,
}

/// The declaration represented by a WIT interface item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitItemKind {
    Record(Vec<WitField>),
    Variant(Vec<WitCase>),
    Enum(Vec<String>),
    Flags(Vec<String>),
    Resource(Vec<WitMember>),
    Alias(WitType),
    Used(WitTypeRef),
    Func(WitFunc),
}

/// A record field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitField {
    pub name: String,
    pub docs: Option<Arc<str>>,
    pub ty: WitType,
}

/// A variant case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitCase {
    pub name: String,
    pub docs: Option<Arc<str>>,
    pub payload: Option<WitType>,
}

/// A member function declared on a WIT resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitMember {
    pub name: String,
    pub kind: WitMemberKind,
    pub func: WitFunc,
    pub docs: Option<Arc<str>>,
    pub deprecated: Option<Version>,
    pub unsupported: Option<Unsupported>,
}

/// The kind of a WIT resource member.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitMemberKind {
    Constructor,
    Method,
    Static,
}

/// A WIT function signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitFunc {
    pub params: Vec<(String, WitType)>,
    pub result: Option<WitType>,
    pub is_async: bool,
}

/// A WIT type in a signature or type declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitType {
    Bool,
    S8,
    S16,
    S32,
    S64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Char,
    String,
    List(Box<WitType>),
    Option(Box<WitType>),
    Result {
        ok: Option<Box<WitType>>,
        err: Option<Box<WitType>>,
    },
    Tuple(Vec<WitType>),
    Named(WitTypeRef),
    Own(WitTypeRef),
    Borrow(WitTypeRef),
    Future(Option<Box<WitType>>),
    Stream(Option<Box<WitType>>),
    Unsupported(Unsupported),
}

/// A reference to a named type item in a WIT interface.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WitTypeRef {
    pub interface: WitIfaceRef,
    pub item: WitItemIdx,
}

/// A reference to an interface in a WIT unit.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WitIfaceRef {
    pub unit: WitUnit,
    pub interface: WitIfaceIdx,
}

/// A WIT world and its resolved imports and exports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitWorld {
    pub name: String,
    pub docs: Option<Arc<str>>,
    /// Imports in the order produced by `wit-parser` world elaboration.
    pub imports: Vec<WitWorldItem>,
    /// Exports in the order produced by `wit-parser` world elaboration.
    pub exports: Vec<WitWorldItem>,
    pub unsupported: Option<Unsupported>,
    /// Absent for embedded and generated units.
    pub span: Option<WitSpan>,
}

/// An interface or function exposed by a WIT world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitWorldItem {
    Interface(WitIfaceRef),
    Func {
        name: String,
        func: WitFunc,
        unsupported: Option<Unsupported>,
    },
}

/// A source file and byte range in the original WIT input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitSpan {
    pub path: String,
    pub range: Range<usize>,
}

/// A WIT construct that has no supported SAI representation yet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Unsupported {
    ErrorContext,
    FixedList,
    Map,
    NamedResults,
    Async,
    Future,
    Stream,
    WorldType,
    InlineInterface,
}

#[cfg(test)]
mod tests {
    use super::{
        Unsupported, WitCase, WitField, WitIfaceIdx, WitIfaceRef, WitIndexVec, WitInterface,
        WitItem, WitItemIdx, WitItemKind, WitPackage, WitPackageName, WitType, WitTypeRef, WitUnit,
        WitWorldIdx,
    };
    use sai_intern::Interner;
    use sai_package::version::Version;

    fn wasi_unit(name: &str) -> WitUnit {
        let symbols = Interner::new();
        let package = symbols.intern(name).expect("valid test package symbol");
        let version = Version::parse("0.2.3").expect("valid test version");
        WitUnit::wasi(package, &symbols, &version).expect("symbol belongs to interner")
    }

    fn interface(name: &str) -> WitInterface {
        WitInterface {
            name: name.to_owned(),
            docs: None,
            deprecated: None,
            items: WitIndexVec::new(),
            span: None,
        }
    }

    #[test]
    fn model_indices_are_typed_and_round_trip_vector_positions() {
        let iface = WitIfaceIdx::from_raw(3);
        let world = WitWorldIdx::from_raw(4);
        let item = WitItemIdx::from_raw(5);

        assert_eq!(iface.index(), 3);
        assert_eq!(iface.raw(), 3);
        assert_eq!(world.index(), 4);
        assert_eq!(world.raw(), 4);
        assert_eq!(item.index(), 5);
        assert_eq!(item.raw(), 5);
        assert_ne!(iface, WitIfaceIdx::from_raw(4));

        let mut values = WitIndexVec::<WitIfaceIdx, _>::new();
        assert_eq!(values.push("first"), Ok(WitIfaceIdx::from_raw(0)));
        assert_eq!(values.push("second"), Ok(WitIfaceIdx::from_raw(1)));
        assert_eq!(values.get(iface), None);
        assert_eq!(values.get(WitIfaceIdx::from_raw(1)), Some(&"second"));
        assert_eq!(
            values.iter_enumerated().collect::<Vec<_>>(),
            [
                (WitIfaceIdx::from_raw(0), &"first"),
                (WitIfaceIdx::from_raw(1), &"second"),
            ]
        );
    }

    #[test]
    fn model_vectors_retain_package_interface_and_item_order() {
        let item_names = ["first", "second"]
            .into_iter()
            .map(|name| WitItem {
                name: name.to_owned(),
                docs: None,
                deprecated: None,
                kind: WitItemKind::Enum(Vec::new()),
                unsupported: None,
                span: None,
            })
            .collect::<Vec<_>>();
        let mut first_interface = interface("first-interface");
        first_interface.items =
            WitIndexVec::try_from_vec(item_names).expect("two items fit the index");
        let package = WitPackage {
            unit: wasi_unit("io"),
            name: WitPackageName {
                namespace: "wasi".to_owned(),
                name: "ordered".to_owned(),
                version: Some(Version::parse("0.2.3").expect("valid test version")),
            },
            interfaces: WitIndexVec::try_from_vec(vec![
                first_interface,
                interface("second-interface"),
            ])
            .expect("two interfaces fit the index"),
            worlds: WitIndexVec::new(),
        };

        assert_eq!(
            package
                .interfaces
                .iter()
                .map(|interface| interface.name.as_str())
                .collect::<Vec<_>>(),
            ["first-interface", "second-interface"]
        );
        assert_eq!(
            package.interfaces[WitIfaceIdx::from_raw(0)]
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn nested_type_references_keep_their_explicit_interface_and_item_indices() {
        let reference = WitTypeRef {
            interface: WitIfaceRef {
                unit: wasi_unit("io"),
                interface: WitIfaceIdx::from_raw(2),
            },
            item: WitItemIdx::from_raw(7),
        };
        let ty = WitType::List(Box::new(WitType::Named(reference)));

        assert_eq!(
            ty,
            WitType::List(Box::new(WitType::Named(WitTypeRef {
                interface: WitIfaceRef {
                    unit: wasi_unit("io"),
                    interface: WitIfaceIdx::from_raw(2),
                },
                item: WitItemIdx::from_raw(7),
            })))
        );
        assert_eq!(
            WitType::Unsupported(Unsupported::ErrorContext),
            WitType::Unsupported(Unsupported::ErrorContext)
        );
    }

    #[test]
    fn aggregate_shapes_keep_fields_and_case_payloads() {
        let field = WitField {
            name: "value".to_owned(),
            docs: None,
            ty: WitType::U32,
        };
        let case = WitCase {
            name: "some".to_owned(),
            docs: None,
            payload: Some(WitType::Named(WitTypeRef {
                interface: WitIfaceRef {
                    unit: wasi_unit("io"),
                    interface: WitIfaceIdx::from_raw(0),
                },
                item: WitItemIdx::from_raw(0),
            })),
        };

        assert_eq!(field.ty, WitType::U32);
        assert!(case.payload.is_some());
    }
}
