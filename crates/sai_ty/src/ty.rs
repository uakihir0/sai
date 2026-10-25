//! Interned semantic types shared by declaration and body checking.
//!
//! This is an early semantic type kernel. It represents primitive types,
//! tuples, references, arrays with literal or enclosing `USize` const lengths,
//! function pointers, owner-bound function type parameters, and nominal
//! identities with explicit type arguments. Signature lowering currently
//! limits arrays to the resolved built-in `Array` with either a nonnegative
//! target-fitting literal length or an exact enclosing function const parameter
//! of type `USize`. Function pointers have no effects or borrowed children and
//! may refer to their enclosing function's validated type and const parameters.
//! Generic nominal applications are limited to validated direct local records
//! and enums with region-free fields or payloads. Projections, inference
//! variables, and error types remain for later slices.

use sai_hir::{DefId, Mutability, Safety, Symbol};
use std::collections::HashMap;
use std::fmt;
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
    /// A structural array with its element type and USize length.
    Array(Ty, Const),
    /// A function pointer with a source-ordered signature and explicit safety.
    FnPtr(FnPtrSig),
    /// A type parameter in the binder of one function signature.
    Param(ParamTy),
    /// A nominal definition and its explicit type arguments.
    ///
    /// This slice supports type arguments only for validated direct local
    /// records and enums with unbounded type-only parameters whose fields or
    /// payloads are proven not to require an implicit region parameter.
    /// Interface unit enums and nongeneric local definitions use an empty
    /// argument list.
    Adt(DefId, Arc<[Ty]>),
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

/// A const parameter's position and source name in its owning function binder.
///
/// Type and const parameters share one source-order index space.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ParamConst {
    pub index: u32,
    pub name: Symbol,
}

/// A USize value retained as an array length.
///
/// A const parameter remains distinct from a literal so later substitution
/// can preserve the binder identity rather than treating it as a guessed
/// value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Const {
    Value(u128),
    Param(ParamConst),
}

/// A type or constant argument in one source-ordered generic binder.
///
/// Type and const arguments share an index space. Region arguments are not
/// represented by this type kernel and are not substituted by this API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GenericArg {
    Type(Ty),
    Const(Const),
}

/// The kind of a generic argument expected by a semantic parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenericArgKind {
    Type,
    Const,
}

impl fmt::Display for GenericArgKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type => formatter.write_str("a type"),
            Self::Const => formatter.write_str("a const"),
        }
    }
}

/// A missing or incorrectly typed argument encountered during substitution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubstitutionError {
    MissingArgument {
        index: u32,
    },
    ArgumentKindMismatch {
        index: u32,
        expected: GenericArgKind,
        found: GenericArgKind,
    },
}

impl fmt::Display for SubstitutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArgument { index } => {
                write!(formatter, "generic argument {index} is missing")
            }
            Self::ArgumentKindMismatch {
                index,
                expected,
                found,
            } => write!(
                formatter,
                "generic argument {index} must be {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for SubstitutionError {}

/// One type-only binder in a function signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FnSigTypeParam {
    pub def_id: DefId,
    pub param: ParamTy,
}

/// One const-only binder in a function signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FnSigConstParam {
    pub def_id: DefId,
    pub param: ParamConst,
}

/// A function signature binder in source order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FnSigGenericParam {
    Type(FnSigTypeParam),
    Const(FnSigConstParam),
}

/// A representable function pointer signature.
///
/// Effects are intentionally absent: HIR distinguishes an omitted effect
/// clause from an explicitly empty one, and this semantic slice cannot retain
/// either resolved effect sets or that source distinction.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FnPtrSig {
    safety: Safety,
    inputs: Arc<[Ty]>,
    output: Ty,
}

impl FnPtrSig {
    pub(crate) fn new(safety: Safety, inputs: impl Into<Arc<[Ty]>>, output: Ty) -> Self {
        Self {
            safety,
            inputs: inputs.into(),
            output,
        }
    }

    pub fn safety(&self) -> Safety {
        self.safety
    }

    pub fn inputs(&self) -> &[Ty] {
        &self.inputs
    }

    pub fn output(&self) -> &Ty {
        &self.output
    }
}

/// The lowered types of one supported partial function signature.
///
/// This is intentionally not the complete `fn_sig` model from the
/// implementation plan: safety, async output, ABI, effects, and borrow
/// relations remain explicit lowering boundaries until their representations
/// are implemented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FnSig {
    generic_params: Arc<[FnSigGenericParam]>,
    type_params: Arc<[FnSigTypeParam]>,
    const_params: Arc<[FnSigConstParam]>,
    inputs: Arc<[Ty]>,
    output: Ty,
}

impl FnSig {
    pub(crate) fn new_with_generic_params(
        generic_params: impl Into<Arc<[FnSigGenericParam]>>,
        inputs: impl Into<Arc<[Ty]>>,
        output: Ty,
    ) -> Self {
        let generic_params = generic_params.into();
        let type_params = generic_params
            .iter()
            .filter_map(|parameter| match parameter {
                FnSigGenericParam::Type(parameter) => Some(*parameter),
                FnSigGenericParam::Const(_) => None,
            })
            .collect::<Vec<_>>()
            .into();
        let const_params = generic_params
            .iter()
            .filter_map(|parameter| match parameter {
                FnSigGenericParam::Type(_) => None,
                FnSigGenericParam::Const(parameter) => Some(*parameter),
            })
            .collect::<Vec<_>>()
            .into();
        Self {
            generic_params,
            type_params,
            const_params,
            inputs: inputs.into(),
            output,
        }
    }

    /// The function's type and const binders in source order.
    pub fn generic_params(&self) -> &[FnSigGenericParam] {
        &self.generic_params
    }

    /// The function's type-only binders in source order.
    ///
    /// Each index is still its position among all type and const binders.
    pub fn type_params(&self) -> &[FnSigTypeParam] {
        &self.type_params
    }

    /// The function's const-only binders in source order.
    ///
    /// Each index is still its position among all type and const binders.
    pub fn const_params(&self) -> &[FnSigConstParam] {
        &self.const_params
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

    /// Substitute source-ordered type and const arguments through a semantic
    /// type, interning the resulting structure in this interner.
    ///
    /// `ParamTy::index` and `ParamConst::index` address the same mixed argument
    /// slice. Replacement values are inserted as-is and are not recursively
    /// substituted. The caller must provide arguments for the binder that
    /// owns `ty`, in the correct order and with the correct arity.
    ///
    /// Regions are preserved unchanged. This routine does not substitute
    /// implicit region arguments or validate binder identity.
    pub fn substitute(&self, ty: &Ty, args: &[GenericArg]) -> Result<Ty, SubstitutionError> {
        fn type_argument<'a>(
            args: &'a [GenericArg],
            index: u32,
        ) -> Result<&'a Ty, SubstitutionError> {
            match args.get(index as usize) {
                Some(GenericArg::Type(ty)) => Ok(ty),
                Some(GenericArg::Const(_)) => Err(SubstitutionError::ArgumentKindMismatch {
                    index,
                    expected: GenericArgKind::Type,
                    found: GenericArgKind::Const,
                }),
                None => Err(SubstitutionError::MissingArgument { index }),
            }
        }

        fn const_argument(args: &[GenericArg], index: u32) -> Result<Const, SubstitutionError> {
            match args.get(index as usize) {
                Some(GenericArg::Const(value)) => Ok(*value),
                Some(GenericArg::Type(_)) => Err(SubstitutionError::ArgumentKindMismatch {
                    index,
                    expected: GenericArgKind::Const,
                    found: GenericArgKind::Type,
                }),
                None => Err(SubstitutionError::MissingArgument { index }),
            }
        }

        fn substitute_inner(
            interner: &TyInterner,
            ty: &Ty,
            args: &[GenericArg],
        ) -> Result<Ty, SubstitutionError> {
            let kind = match ty.kind() {
                TyKind::Tuple(elements) => TyKind::Tuple(
                    elements
                        .iter()
                        .map(|element| substitute_inner(interner, element, args))
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                ),
                TyKind::Ref(region, inner, mutability) => TyKind::Ref(
                    *region,
                    substitute_inner(interner, inner, args)?,
                    *mutability,
                ),
                TyKind::Array(element, length) => {
                    let element = substitute_inner(interner, element, args)?;
                    let length = match length {
                        Const::Value(value) => Const::Value(*value),
                        Const::Param(parameter) => const_argument(args, parameter.index)?,
                    };
                    TyKind::Array(element, length)
                }
                TyKind::FnPtr(signature) => TyKind::FnPtr(FnPtrSig::new(
                    signature.safety(),
                    signature
                        .inputs()
                        .iter()
                        .map(|input| substitute_inner(interner, input, args))
                        .collect::<Result<Vec<_>, _>>()?,
                    substitute_inner(interner, signature.output(), args)?,
                )),
                TyKind::Param(parameter) => {
                    return Ok(
                        interner.intern(type_argument(args, parameter.index)?.kind().clone())
                    );
                }
                TyKind::Adt(def_id, type_args) => TyKind::Adt(
                    *def_id,
                    type_args
                        .iter()
                        .map(|arg| substitute_inner(interner, arg, args))
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                ),
                primitive => return Ok(interner.intern(primitive.clone())),
            };
            Ok(interner.intern(kind))
        }

        substitute_inner(self, ty, args)
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
            TyKind::Array(element, length) => {
                TyKind::Array(self.intern(element.kind().clone()), length)
            }
            TyKind::FnPtr(signature) => TyKind::FnPtr(FnPtrSig::new(
                signature.safety,
                signature
                    .inputs
                    .iter()
                    .map(|input| self.intern(input.kind().clone()))
                    .collect::<Vec<_>>(),
                self.intern(signature.output.kind().clone()),
            )),
            TyKind::Adt(def_id, args) => TyKind::Adt(
                def_id,
                args.iter()
                    .map(|arg| self.intern(arg.kind().clone()))
                    .collect::<Vec<_>>()
                    .into(),
            ),
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
    fn arrays_are_interned_by_element_type_and_length() {
        let interner = TyInterner::new();
        let element = interner.intern(TyKind::Bool);
        let array = interner.intern(TyKind::Array(element.clone(), Const::Value(4)));
        let same_array = interner.intern(TyKind::Array(element.clone(), Const::Value(4)));
        let different_length = interner.intern(TyKind::Array(element, Const::Value(5)));

        assert!(array.ptr_eq(&same_array));
        assert!(!array.ptr_eq(&different_length));
    }

    #[test]
    fn array_const_parameters_are_distinct_from_literals_and_each_other() {
        let interner = TyInterner::new();
        let symbols = sai_intern::Interner::new();
        let element = interner.intern(TyKind::Bool);
        let first_parameter = ParamConst {
            index: 0,
            name: symbols.intern("N").unwrap(),
        };
        let second_parameter = ParamConst {
            index: 1,
            name: symbols.intern("M").unwrap(),
        };
        let first = interner.intern(TyKind::Array(
            element.clone(),
            Const::Param(first_parameter),
        ));
        let equivalent = interner.intern(TyKind::Array(
            element.clone(),
            Const::Param(first_parameter),
        ));
        let other_parameter = interner.intern(TyKind::Array(
            element.clone(),
            Const::Param(second_parameter),
        ));
        let literal = interner.intern(TyKind::Array(element, Const::Value(4)));

        assert!(first.ptr_eq(&equivalent));
        assert!(!first.ptr_eq(&other_parameter));
        assert!(!first.ptr_eq(&literal));
    }

    #[test]
    fn array_interning_canonicalizes_element_types_from_another_interner() {
        let interner = TyInterner::new();
        let other = TyInterner::new();
        let local_bool = interner.intern(TyKind::Bool);
        let foreign_bool = other.intern(TyKind::Bool);
        let local_array = interner.intern(TyKind::Array(local_bool.clone(), Const::Value(8)));
        let foreign_array = interner.intern(TyKind::Array(foreign_bool, Const::Value(8)));

        assert!(local_array.ptr_eq(&foreign_array));
        let TyKind::Array(element, Const::Value(8)) = foreign_array.kind() else {
            panic!("the array retains its exact length");
        };
        assert!(element.ptr_eq(&local_bool));
    }

    #[test]
    fn function_pointer_interning_canonicalizes_children_and_preserves_safety() {
        let interner = TyInterner::new();
        let other = TyInterner::new();
        let local_bool = interner.intern(TyKind::Bool);
        let foreign_bool = other.intern(TyKind::Bool);
        let local_array = interner.intern(TyKind::Array(local_bool.clone(), Const::Value(3)));
        let foreign_array = other.intern(TyKind::Array(foreign_bool.clone(), Const::Value(3)));
        let local_output = interner.tuple(Arc::from([local_bool.clone()]));
        let foreign_output = other.tuple(Arc::from([foreign_bool]));

        let local = interner.intern(TyKind::FnPtr(FnPtrSig::new(
            Safety::Safe,
            Arc::from([local_array]),
            local_output,
        )));
        let foreign = interner.intern(TyKind::FnPtr(FnPtrSig::new(
            Safety::Safe,
            Arc::from([foreign_array]),
            foreign_output,
        )));
        let unsafe_signature = interner.intern(TyKind::FnPtr(FnPtrSig::new(
            Safety::Unsafe,
            Arc::from([interner.intern(TyKind::Array(local_bool.clone(), Const::Value(3)))]),
            interner.tuple(Arc::from([local_bool.clone()])),
        )));

        assert!(local.ptr_eq(&foreign));
        assert!(!local.ptr_eq(&unsafe_signature));
        let TyKind::FnPtr(signature) = foreign.kind() else {
            panic!("the type remains a function pointer");
        };
        assert_eq!(signature.safety(), Safety::Safe);
        let TyKind::Array(argument, Const::Value(3)) = signature.inputs()[0].kind() else {
            panic!("the input retains its nested array");
        };
        assert!(argument.ptr_eq(&local_bool));
        let TyKind::Tuple(output) = signature.output().kind() else {
            panic!("the output retains its nested tuple");
        };
        assert!(output[0].ptr_eq(&local_bool));
    }

    #[test]
    fn substitution_uses_the_shared_source_order_for_type_and_const_arguments() {
        let interner = TyInterner::new();
        let symbols = sai_intern::Interner::new();
        let type_parameter = interner.intern(TyKind::Param(ParamTy {
            index: 1,
            name: symbols.intern("T").unwrap(),
        }));
        let length_parameter = ParamConst {
            index: 0,
            name: symbols.intern("N").unwrap(),
        };
        let array = interner.intern(TyKind::Array(
            type_parameter,
            Const::Param(length_parameter),
        ));
        let replacement = interner.intern(TyKind::Bool);

        let substituted = interner
            .substitute(
                &array,
                &[
                    GenericArg::Const(Const::Value(11)),
                    GenericArg::Type(replacement.clone()),
                ],
            )
            .unwrap();

        let TyKind::Array(element, Const::Value(11)) = substituted.kind() else {
            panic!("the mixed const argument supplies the array length");
        };
        assert!(element.ptr_eq(&replacement));
    }

    #[test]
    fn substitution_recurses_through_all_current_composite_types() {
        let interner = TyInterner::new();
        let symbols = sai_intern::Interner::new();
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("example:substitution#1").unwrap();
        let adt = DefId::from_foreign(package, sai_intern::DefIndex::from_raw(7));
        let parameter = |index, name| {
            interner.intern(TyKind::Param(ParamTy {
                index,
                name: symbols.intern(name).unwrap(),
            }))
        };
        let first = parameter(0, "T");
        let second = parameter(1, "U");
        let length = ParamConst {
            index: 2,
            name: symbols.intern("N").unwrap(),
        };
        let region = Region::Sig(SigRegion {
            item: adt,
            index: 4,
        });
        let array = interner.intern(TyKind::Array(
            interner.tuple(Arc::from([
                second.clone(),
                interner.intern(TyKind::Ref(region, first.clone(), Mutability::Mut)),
            ])),
            Const::Param(length),
        ));
        let function = interner.intern(TyKind::FnPtr(FnPtrSig::new(
            Safety::Unsafe,
            Arc::from([second.clone(), array.clone()]),
            first.clone(),
        )));
        let original = interner.intern(TyKind::Adt(adt, Arc::from([first, array, function])));
        let type_replacement = interner.intern(TyKind::Float(FloatTy::F64));
        let second_replacement = interner.intern(TyKind::Char);
        let expected_array = interner.intern(TyKind::Array(
            interner.tuple(Arc::from([
                second_replacement.clone(),
                interner.intern(TyKind::Ref(
                    region,
                    type_replacement.clone(),
                    Mutability::Mut,
                )),
            ])),
            Const::Value(13),
        ));
        let expected_function = interner.intern(TyKind::FnPtr(FnPtrSig::new(
            Safety::Unsafe,
            Arc::from([second_replacement.clone(), expected_array.clone()]),
            type_replacement.clone(),
        )));
        let expected = interner.intern(TyKind::Adt(
            adt,
            Arc::from([type_replacement.clone(), expected_array, expected_function]),
        ));

        let substituted = interner
            .substitute(
                &original,
                &[
                    GenericArg::Type(type_replacement),
                    GenericArg::Type(second_replacement),
                    GenericArg::Const(Const::Value(13)),
                ],
            )
            .unwrap();

        assert!(substituted.ptr_eq(&expected));
        let TyKind::Adt(actual_adt, args) = substituted.kind() else {
            panic!("the nominal type remains an ADT");
        };
        assert_eq!(*actual_adt, adt);
        let TyKind::Array(tuple, Const::Value(13)) = args[1].kind() else {
            panic!("nested array types and const parameters are substituted");
        };
        let TyKind::Tuple(elements) = tuple.kind() else {
            panic!("the array element remains a tuple");
        };
        let TyKind::Ref(actual_region, _, actual_mutability) = elements[1].kind() else {
            panic!("the reference remains a reference");
        };
        assert_eq!(*actual_region, region);
        assert_eq!(*actual_mutability, Mutability::Mut);
        let TyKind::FnPtr(signature) = args[2].kind() else {
            panic!("the ADT retains its function-pointer argument");
        };
        assert_eq!(signature.safety(), Safety::Unsafe);
    }

    #[test]
    fn substitution_interns_identity_structure_and_preserves_regions() {
        let interner = TyInterner::new();
        let other_interner = TyInterner::new();
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("example:identity#1").unwrap();
        let item = DefId::from_foreign(package, sai_intern::DefIndex::from_raw(2));
        let foreign_bool = other_interner.intern(TyKind::Bool);
        let foreign_reference = other_interner.intern(TyKind::Ref(
            Region::Sig(SigRegion { item, index: 3 }),
            foreign_bool,
            Mutability::Not,
        ));

        let result = interner.substitute(&foreign_reference, &[]).unwrap();
        let expected = interner.intern(TyKind::Ref(
            Region::Sig(SigRegion { item, index: 3 }),
            interner.intern(TyKind::Bool),
            Mutability::Not,
        ));

        assert!(result.ptr_eq(&expected));
    }

    #[test]
    fn substitution_inserts_type_replacements_without_substituting_them_again() {
        let interner = TyInterner::new();
        let symbols = sai_intern::Interner::new();
        let replacement_inner = interner.intern(TyKind::Param(ParamTy {
            index: 1,
            name: symbols.intern("U").unwrap(),
        }));
        let replacement = interner.tuple(Arc::from([replacement_inner.clone()]));
        let original = interner.intern(TyKind::Param(ParamTy {
            index: 0,
            name: symbols.intern("T").unwrap(),
        }));

        let result = interner
            .substitute(
                &original,
                &[
                    GenericArg::Type(replacement.clone()),
                    GenericArg::Type(interner.intern(TyKind::Bool)),
                ],
            )
            .unwrap();

        assert!(result.ptr_eq(&replacement));
        let TyKind::Tuple(elements) = result.kind() else {
            panic!("the replacement tuple is inserted intact");
        };
        assert!(elements[0].ptr_eq(&replacement_inner));
    }

    #[test]
    fn substitution_reports_missing_and_wrong_kind_arguments() {
        let interner = TyInterner::new();
        let symbols = sai_intern::Interner::new();
        let type_parameter = interner.intern(TyKind::Param(ParamTy {
            index: 2,
            name: symbols.intern("T").unwrap(),
        }));
        let const_parameter = interner.intern(TyKind::Array(
            interner.intern(TyKind::Bool),
            Const::Param(ParamConst {
                index: 1,
                name: symbols.intern("N").unwrap(),
            }),
        ));

        assert_eq!(
            interner.substitute(&type_parameter, &[]),
            Err(SubstitutionError::MissingArgument { index: 2 })
        );
        assert_eq!(
            interner.substitute(&const_parameter, &[]),
            Err(SubstitutionError::MissingArgument { index: 1 })
        );
        assert_eq!(
            interner.substitute(
                &interner.intern(TyKind::Param(ParamTy {
                    index: 0,
                    name: symbols.intern("U").unwrap(),
                })),
                &[GenericArg::Const(Const::Value(1))],
            ),
            Err(SubstitutionError::ArgumentKindMismatch {
                index: 0,
                expected: GenericArgKind::Type,
                found: GenericArgKind::Const,
            })
        );
        assert_eq!(
            interner.substitute(
                &interner.intern(TyKind::Array(
                    interner.intern(TyKind::Bool),
                    Const::Param(ParamConst {
                        index: 0,
                        name: symbols.intern("M").unwrap(),
                    }),
                )),
                &[GenericArg::Type(interner.intern(TyKind::Bool))],
            ),
            Err(SubstitutionError::ArgumentKindMismatch {
                index: 0,
                expected: GenericArgKind::Const,
                found: GenericArgKind::Type,
            })
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
    fn generic_adt_interning_canonicalizes_arguments_from_another_interner() {
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("acme:types#1.0").expect("package interns");
        let definition = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(1));
        let interner = TyInterner::new();
        let other = TyInterner::new();
        let local_bool = interner.intern(TyKind::Bool);
        let foreign_bool = other.intern(TyKind::Bool);

        let local_application =
            interner.intern(TyKind::Adt(definition, Arc::from([local_bool.clone()])));
        let foreign_application =
            interner.intern(TyKind::Adt(definition, Arc::from([foreign_bool])));

        assert!(local_application.ptr_eq(&foreign_application));
        let TyKind::Adt(_, args) = foreign_application.kind() else {
            panic!("the nominal application remains an ADT");
        };
        assert!(args[0].ptr_eq(&local_bool));
    }

    #[test]
    fn distinct_adt_definitions_keep_distinct_semantic_identity() {
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("acme:types#1.0").expect("package interns");
        let interner = TyInterner::new();
        let first = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(1));
        let second = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(2));

        let first_ty = interner.intern(TyKind::Adt(first, Arc::from([])));
        let same_first_ty = interner.intern(TyKind::Adt(first, Arc::from([])));
        let second_ty = interner.intern(TyKind::Adt(second, Arc::from([])));

        assert!(first_ty.ptr_eq(&same_first_ty));
        assert!(!first_ty.ptr_eq(&second_ty));
        assert_ne!(first_ty.kind(), second_ty.kind());
    }

    #[test]
    fn generic_adt_applications_are_interned_by_definition_and_ordered_arguments() {
        let packages = sai_intern::PackageInterner::new();
        let package = packages.intern("acme:types#1.0").expect("package interns");
        let interner = TyInterner::new();
        let definition = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(1));
        let other_definition = DefId::from_foreign(package, sai_hir::DefIndex::from_raw(2));
        let u32_ty = interner.intern(TyKind::Uint(UintTy::U32));
        let bool_ty = interner.intern(TyKind::Bool);

        let packet_u32 = interner.intern(TyKind::Adt(definition, Arc::from([u32_ty.clone()])));
        let same_packet_u32 = interner.intern(TyKind::Adt(definition, Arc::from([u32_ty.clone()])));
        let packet_bool = interner.intern(TyKind::Adt(definition, Arc::from([bool_ty.clone()])));
        let pair_u32_bool = interner.intern(TyKind::Adt(
            definition,
            Arc::from([u32_ty.clone(), bool_ty.clone()]),
        ));
        let pair_bool_u32 = interner.intern(TyKind::Adt(definition, Arc::from([bool_ty, u32_ty])));
        let other_packet_u32 = interner.intern(TyKind::Adt(
            other_definition,
            Arc::from([interner.intern(TyKind::Uint(UintTy::U32))]),
        ));

        assert!(packet_u32.ptr_eq(&same_packet_u32));
        assert!(!packet_u32.ptr_eq(&packet_bool));
        assert!(!pair_u32_bool.ptr_eq(&pair_bool_u32));
        assert!(!packet_u32.ptr_eq(&other_packet_u32));
    }
}
