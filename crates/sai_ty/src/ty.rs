//! Interned semantic types shared by declaration and body checking.
//!
//! This is an early semantic type kernel. It represents primitive types,
//! tuples, references, owner-bound function type parameters, and nongeneric
//! nominal identities; generic arguments, function pointers, projections,
//! inference variables, and error types are added by later slices.

use sai_hir::{DefId, Mutability, Symbol};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

const TY_INTERNER_SHARDS: usize = 16;

/// A semantic type whose equality is identity within its [`TyInterner`].
///
/// The interner owns a strong reference to every value it creates, so a type
/// remains canonical for the interner's lifetime. The hash is structural to
/// remain deterministic; query fingerprints must still hash source-level
/// identities rather than this in-memory type handle.
#[derive(Clone, Debug)]
pub struct Ty(Arc<TyKind>);

impl Ty {
    /// The interned type's structure.
    pub fn kind(&self) -> &TyKind {
        &self.0
    }

    /// Whether both handles refer to the same interned type.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for Ty {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for Ty {}

impl Hash for Ty {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind().hash(state);
    }
}

/// The subset of semantic types represented by this crate slice.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TyKind {
    Int(IntTy),
    Uint(UintTy),
    Float(FloatTy),
    Bool,
    Byte,
    Char,
    Never,
    Tuple(Arc<[Ty]>),
    Ref(Region, Ty, Mutability),
    /// A type parameter in the binder of one function signature.
    Param(ParamTy),
    /// A nongeneric nominal definition.
    ///
    /// This slice constructs this form only for validated local interface
    /// unit enums and direct local records/enums whose HIR has no generic
    /// parameters or where predicates. Generic arguments will be represented
    /// when the generic substitution model is implemented.
    Adt(DefId),
}

/// Signed integer type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    ISize,
}

/// Unsigned integer type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UintTy {
    U8,
    U16,
    U32,
    U64,
    USize,
}

/// Floating-point type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FloatTy {
    F32,
    F64,
}

/// Region placeholder carried by semantic references.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Region {
    /// Region position in an item signature or type declaration.
    Sig(SigRegion),
    /// Any region inside a body.
    Erased,
}

/// A signature region position.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SigRegion {
    pub item: DefId,
    pub index: u32,
}

/// A type parameter's position and source name in its owning function binder.
///
/// The enclosing [`FnSig`] retains the corresponding local `DefId` for each
/// entry in source order. This keeps the type node compact while preserving
/// owner-local binder identity at the signature boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ParamTy {
    pub index: u32,
    pub name: Symbol,
}

/// One type-only binder in a function signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FnSigTypeParam {
    pub def_id: DefId,
    pub param: ParamTy,
}

/// The lowered types of one supported partial function signature.
///
/// This is intentionally not the complete `fn_sig` model from the
/// implementation plan: safety, async output, ABI, effects, and borrow
/// relations remain explicit lowering boundaries until their representations
/// are implemented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FnSig {
    type_params: Arc<[FnSigTypeParam]>,
    inputs: Arc<[Ty]>,
    output: Ty,
}

impl FnSig {
    pub(crate) fn new(
        type_params: impl Into<Arc<[FnSigTypeParam]>>,
        inputs: impl Into<Arc<[Ty]>>,
        output: Ty,
    ) -> Self {
        Self {
            type_params: type_params.into(),
            inputs: inputs.into(),
            output,
        }
    }

    /// The function's type-only binders in source order.
    pub fn type_params(&self) -> &[FnSigTypeParam] {
        &self.type_params
    }

    pub fn inputs(&self) -> &[Ty] {
        &self.inputs
    }

    pub fn output(&self) -> &Ty {
        &self.output
    }
}

/// Session-local canonicalization for semantic types.
///
/// Type nodes use `Arc` handles so their children can be owned without
/// self-referential storage. The strong values retained in the map keep every
/// interned node alive until the interner is dropped.
#[derive(Default)]
pub struct TyInterner {
    types: [Mutex<HashMap<TyKind, Ty>>; TY_INTERNER_SHARDS],
}

impl TyInterner {
    pub fn new() -> Self {
        Self {
            types: std::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }

    /// Intern a semantic type, returning the canonical handle for its shape.
    pub fn intern(&self, kind: TyKind) -> Ty {
        let kind = self.canonicalize_children(kind);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        kind.hash(&mut hasher);
        let shard_index = (hasher.finish() as usize) % TY_INTERNER_SHARDS;
        let mut types = self.types[shard_index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ty) = types.get(&kind) {
            return ty.clone();
        }

        let ty = Ty(Arc::new(kind.clone()));
        types.insert(kind, ty.clone());
        ty
    }

    fn canonicalize_children(&self, kind: TyKind) -> TyKind {
        match kind {
            TyKind::Tuple(elements) => TyKind::Tuple(
                elements
                    .iter()
                    .map(|element| self.intern(element.kind().clone()))
                    .collect::<Vec<_>>()
                    .into(),
            ),
            TyKind::Ref(region, inner, mutability) => {
                TyKind::Ref(region, self.intern(inner.kind().clone()), mutability)
            }
            kind => kind,
        }
    }

    /// Intern a tuple in declaration order. An empty tuple is the unit type.
    pub fn tuple(&self, elements: impl Into<Arc<[Ty]>>) -> Ty {
        self.intern(TyKind::Tuple(elements.into()))
    }

    /// Number of distinct type shapes retained by this interner.
    pub fn len(&self) -> usize {
        self.types
            .iter()
            .map(|shard| {
                shard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len()
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_intern::Idx;
    use std::collections::hash_map::DefaultHasher;

    fn hash(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn interning_is_structural_and_pointer_equal() {
        let interner = TyInterner::new();
        let first = interner.intern(TyKind::Int(IntTy::I64));
        let second = interner.intern(TyKind::Int(IntTy::I64));
        let different = interner.intern(TyKind::Int(IntTy::I32));

        assert!(first.ptr_eq(&second));
        assert_eq!(first, second);
        assert_ne!(first, different);
        assert_eq!(hash(&first), hash(&second));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn nested_tuple_and_reference_types_reuse_interned_children() {
        let interner = TyInterner::new();
        let boolean = interner.intern(TyKind::Bool);
        let tuple = interner.tuple(Arc::from([boolean.clone()]));
        let reference =
            interner.intern(TyKind::Ref(Region::Erased, tuple.clone(), Mutability::Mut));
        let equivalent = interner.intern(TyKind::Ref(Region::Erased, tuple, Mutability::Mut));

        assert!(reference.ptr_eq(&equivalent));
        assert_eq!(
            reference.kind(),
            &TyKind::Ref(
                Region::Erased,
                interner.tuple(Arc::from([boolean])),
                Mutability::Mut
            )
        );
    }

    #[test]
    fn concurrent_interning_publishes_one_canonical_type() {
        let interner = Arc::new(TyInterner::new());
        let handles = (0..16)
            .map(|_| {
                let interner = Arc::clone(&interner);
                std::thread::spawn(move || interner.intern(TyKind::Uint(UintTy::U64)))
            })
            .collect::<Vec<_>>();
        let values = handles
            .into_iter()
            .map(|handle| handle.join().expect("intern thread completes"))
            .collect::<Vec<_>>();

        assert!(values.iter().all(|value| value.ptr_eq(&values[0])));
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn interning_canonicalizes_children_from_another_interner() {
        let interner = TyInterner::new();
        let other = TyInterner::new();
        let local_child = interner.intern(TyKind::Bool);
        let foreign_child = other.intern(TyKind::Bool);
        let local_parent = interner.tuple(Arc::from([local_child]));
        let foreign_parent = interner.intern(TyKind::Tuple(Arc::from([foreign_child])));

        assert!(local_parent.ptr_eq(&foreign_parent));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn distinct_adt_definitions_keep_distinct_semantic_identity() {
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("acme:types#1.0").expect("package interns");
        let interner = TyInterner::new();
        let first = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(1));
        let second = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(2));

        let first_ty = interner.intern(TyKind::Adt(first));
        let same_first_ty = interner.intern(TyKind::Adt(first));
        let second_ty = interner.intern(TyKind::Adt(second));

        assert!(first_ty.ptr_eq(&same_first_ty));
        assert!(!first_ty.ptr_eq(&second_ty));
        assert_ne!(first_ty.kind(), second_ty.kind());
    }
}
