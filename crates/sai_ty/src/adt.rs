//! The first semantic ADT representation.
//!
//! This module models validated local flags declarations, local-interface
//! unit enums, and payload-only templates for direct local enums. It does not
//! provide the general `adt_def(DefId)` query contract: discriminants,
//! representations, and general semantic ADT fields are not represented by
//! the payload-template type.

use crate::iface::{FlagsDef, InterfaceItemCheck, raw_flags_marker_is_valid};
use sai_hir::attrs::{ReprAttr, ReprInteger};
use sai_hir::item_tree::AttrName;
use sai_hir::items::{HirOwner, InterfaceItemKind, ItemKind, OwnerNode, Variant, VariantShape};
use sai_hir::{
    DefId, DefPathData, DefPathInterner, DisambiguatedDefPathData, LocalDefId, Symbol, Visibility,
};
use sai_intern::Interner;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The ADT kinds represented by the current semantic subset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AdtKind {
    /// An enum with unit variants.
    Enum,
    /// A validated local WIT flags type.
    Flags,
}

/// Primitive integer types used by enum discriminants or flags storage.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    /// Whether `value` can be represented by this integer type.
    pub const fn contains(self, value: i128) -> bool {
        match self {
            Self::I8 => i8::MIN as i128 <= value && value <= i8::MAX as i128,
            Self::I16 => i16::MIN as i128 <= value && value <= i16::MAX as i128,
            Self::I32 => i32::MIN as i128 <= value && value <= i32::MAX as i128,
            Self::I64 => i64::MIN as i128 <= value && value <= i64::MAX as i128,
            Self::U8 => 0 <= value && value <= u8::MAX as i128,
            Self::U16 => 0 <= value && value <= u16::MAX as i128,
            Self::U32 => 0 <= value && value <= u32::MAX as i128,
            Self::U64 => 0 <= value && value <= u64::MAX as i128,
        }
    }
}

/// Semantic field types represented by this initial ADT slice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Ty {
    Int(IntTy),
}

/// A nominal ADT definition in the currently supported semantic subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdtDef {
    def_id: DefId,
    kind: AdtKind,
    discr_ty: IntTy,
    variants: Arc<[VariantDef]>,
}

impl AdtDef {
    /// Definition identity of this ADT.
    pub const fn def_id(&self) -> DefId {
        self.def_id
    }

    /// Kind represented by this definition.
    pub const fn kind(&self) -> AdtKind {
        self.kind
    }

    /// Integer type used for enum discriminants. It is `I64` for flags ADTs.
    pub const fn discr_ty(&self) -> IntTy {
        self.discr_ty
    }

    /// Variants in declaration order.
    pub fn variants(&self) -> &[VariantDef] {
        &self.variants
    }
}

/// A variant in the currently supported semantic subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantDef {
    def_id: DefId,
    name: Symbol,
    discr: i128,
    fields: Arc<[FieldDef]>,
}

impl VariantDef {
    /// Definition identity of this variant.
    pub const fn def_id(&self) -> DefId {
        self.def_id
    }

    /// Internal name of this variant.
    pub const fn name(&self) -> Symbol {
        self.name
    }

    /// Discriminant stored in the ADT model.
    pub const fn discr(&self) -> i128 {
        self.discr
    }

    /// Fields in declaration order.
    pub fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
}

/// A field in the currently supported semantic subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldDef {
    def_id: DefId,
    name: Symbol,
    vis: Visibility,
    ty: Ty,
}

impl FieldDef {
    /// Definition identity of this field.
    pub const fn def_id(&self) -> DefId {
        self.def_id
    }

    /// Internal name of this field.
    pub const fn name(&self) -> Symbol {
        self.name
    }

    /// Visibility recorded for this field.
    pub const fn visibility(&self) -> Visibility {
        self.vis
    }

    /// Semantic type of this field.
    pub const fn ty(&self) -> Ty {
        self.ty
    }
}

/// A payload-only template for one local enum.
///
/// This is intentionally separate from [`AdtDef`]. It preserves generic
/// payload types and source shape, but does not describe enum discriminants,
/// representation, or the complete `adt_def` query contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumPayloadTemplateDef {
    owner: DefId,
    type_params: Arc<[(DefId, crate::ty::ParamTy)]>,
    variants: Arc<[LocalEnumPayloadVariantTemplateDef]>,
}

impl LocalEnumPayloadTemplateDef {
    /// Build a payload-only template from source-ordered semantic data.
    ///
    /// The caller is responsible for verifying source provenance. This
    /// constructor validates package, binder, variant, payload-identity, and
    /// template invariants before storing the supplied data.
    pub fn try_new(
        owner: DefId,
        type_params: impl Into<Arc<[(DefId, crate::ty::ParamTy)]>>,
        variants: impl Into<Arc<[LocalEnumPayloadVariantTemplateDef]>>,
    ) -> Result<Self, LocalEnumPayloadTemplateDefError> {
        let type_params = type_params.into();
        let variants = variants.into();
        if !valid_payload_template_binders(owner, &type_params) {
            return Err(LocalEnumPayloadTemplateDefError::InvalidTypeParameters);
        }

        let mut variant_ids = HashSet::with_capacity(variants.len());
        for variant in variants.iter() {
            if variant.def_id.package != owner.package
                || variant.def_id == owner
                || variant.name.is_error()
                || !variant_ids.insert(variant.def_id)
            {
                return Err(LocalEnumPayloadTemplateDefError::InvalidVariantIdentity);
            }
        }
        let mut record_field_ids = HashSet::new();
        for variant in variants.iter() {
            match &variant.payload {
                LocalEnumPayloadTemplateShape::Unit => {}
                LocalEnumPayloadTemplateShape::Tuple(payloads) => {
                    if payloads.is_empty() {
                        return Err(LocalEnumPayloadTemplateDefError::EmptyPayloadVariantShape);
                    }
                    for (index, payload) in payloads.iter().enumerate() {
                        let expected_identity = u32::try_from(index).ok().map(|index| {
                            LocalEnumPayloadTemplateIdentity::Tuple {
                                variant: variant.def_id,
                                index,
                            }
                        });
                        if Some(payload.identity) != expected_identity {
                            return Err(
                                LocalEnumPayloadTemplateDefError::InvalidTuplePayloadIdentity,
                            );
                        }
                        if !local_payload_template_matches_binders(&payload.ty, &type_params) {
                            return Err(LocalEnumPayloadTemplateDefError::InvalidPayloadTemplate);
                        }
                    }
                }
                LocalEnumPayloadTemplateShape::Record(fields) => {
                    if fields.is_empty() {
                        return Err(LocalEnumPayloadTemplateDefError::EmptyPayloadVariantShape);
                    }
                    for field in fields.iter() {
                        let LocalEnumPayloadTemplateIdentity::Record {
                            variant: field_variant,
                            field: field_def_id,
                        } = field.identity
                        else {
                            return Err(
                                LocalEnumPayloadTemplateDefError::InvalidRecordPayloadIdentity,
                            );
                        };
                        if field_variant != variant.def_id
                            || field_def_id.package != owner.package
                            || field_def_id == owner
                            || variant_ids.contains(&field_def_id)
                            || field.name.is_error()
                            || !record_field_ids.insert(field_def_id)
                        {
                            return Err(
                                LocalEnumPayloadTemplateDefError::InvalidRecordPayloadIdentity,
                            );
                        }
                        if !local_payload_template_matches_binders(&field.ty, &type_params) {
                            return Err(LocalEnumPayloadTemplateDefError::InvalidPayloadTemplate);
                        }
                    }
                }
            }
        }

        Ok(Self {
            owner,
            type_params,
            variants,
        })
    }

    /// Exact local enum identity.
    pub const fn owner(&self) -> DefId {
        self.owner
    }

    /// Exact source-ordered type binders used by payload templates.
    pub fn type_params(&self) -> &[(DefId, crate::ty::ParamTy)] {
        &self.type_params
    }

    /// Source-ordered variants and payload templates.
    pub fn variants(&self) -> &[LocalEnumPayloadVariantTemplateDef] {
        &self.variants
    }

    /// Look up a payload type by its exact tuple-position or record-field
    /// identity.
    pub fn payload_template(
        &self,
        identity: LocalEnumPayloadTemplateIdentity,
    ) -> Option<&crate::ty::Ty> {
        self.variants
            .iter()
            .find_map(|variant| match &variant.payload {
                LocalEnumPayloadTemplateShape::Unit => None,
                LocalEnumPayloadTemplateShape::Tuple(payloads) => payloads
                    .iter()
                    .find(|payload| payload.identity == identity)
                    .map(|payload| &payload.ty),
                LocalEnumPayloadTemplateShape::Record(fields) => fields
                    .iter()
                    .find(|field| field.identity == identity)
                    .map(|field| &field.ty),
            })
    }
}

/// A source-ordered enum discriminant retained even when it is invalid for
/// the selected representation or duplicates an earlier value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalEnumVariantDiscriminant {
    variant: DefId,
    value: i128,
}

impl LocalEnumVariantDiscriminant {
    /// Build a discriminant for an exact variant.
    pub const fn new(variant: DefId, value: i128) -> Self {
        Self { variant, value }
    }

    /// Exact variant identity.
    pub const fn variant(self) -> DefId {
        self.variant
    }

    /// Computed discriminant value.
    pub const fn value(self) -> i128 {
        self.value
    }
}

/// A representable discriminant semantic error attached to its later source
/// variant. Its computed value remains in either [`LocalEnumAdtDef`] or
/// [`LocalEnumAdtDefBuildError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalEnumDiscriminantIssue {
    variant: DefId,
    value: i128,
    kind: LocalEnumDiscriminantIssueKind,
}

impl LocalEnumDiscriminantIssue {
    /// Variant carrying this issue.
    pub const fn variant(self) -> DefId {
        self.variant
    }

    /// Computed value associated with this issue.
    pub const fn value(self) -> i128 {
        self.value
    }

    /// Whether the value is out of range or duplicates an earlier value.
    pub const fn kind(self) -> LocalEnumDiscriminantIssueKind {
        self.kind
    }
}

/// The semantic failure found while retaining a computed discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalEnumDiscriminantIssueKind {
    OutOfRange,
    Duplicate { previous_variant: DefId },
}

/// A typed, source-ordered enum definition for the current local-enum slice.
///
/// This composes the payload template with validated representation metadata
/// and computed discriminants. It remains separate from the general
/// [`AdtDef`] and does not implement the `adt_def` query contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumAdtDef {
    payloads: LocalEnumPayloadTemplateDef,
    repr: Option<ReprAttr>,
    discr_ty: IntTy,
    discriminants: Arc<[LocalEnumVariantDiscriminant]>,
}

/// The value type template for one exact local enum variant.
///
/// A record variant has no positional value-constructor type; record
/// construction is handled from its named fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalEnumVariantValueType {
    /// A unit variant has the enum's identity application as its value type.
    Unit(crate::ty::Ty),
    /// A tuple variant's function signature template.
    TupleConstructor(crate::ty::FnSig),
    /// A record variant cannot be used as a value or positional constructor.
    RecordHasNoValueType,
}

/// An invalid request or invariant violation while deriving a variant type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalEnumVariantTypeError {
    ForeignVariant(DefId),
    UnknownVariant(DefId),
    InvalidTypeParameters,
    InvalidPayloadShape(DefId),
    NotAdt,
    WrongEnum { expected: DefId, found: DefId },
    WrongNumberOfTypeArguments { expected: usize, found: usize },
    NotTupleVariant(DefId),
    Substitution(crate::ty::SubstitutionError),
}

impl LocalEnumAdtDef {
    /// Build a local enum definition from its payload template, validated
    /// representation, and the exact HIR owner that supplied the template.
    ///
    /// The driver is responsible for proving that `repr` came from the
    /// current target-filtered item's `ValidatedAttrs`. This constructor
    /// checks that the HIR owner and every source-ordered variant match the
    /// payload template before calculating discriminants. If a unit-only
    /// discriminant cannot be represented, later explicit values are still
    /// checked and their computed values and issues are retained in the build
    /// error; implicit values stay unknown until an explicit value resumes the
    /// sequence.
    pub fn from_hir_owner(
        payloads: LocalEnumPayloadTemplateDef,
        repr: Option<ReprAttr>,
        hir: &HirOwner<'_>,
    ) -> Result<(Self, Vec<LocalEnumDiscriminantIssue>), LocalEnumAdtDefBuildError> {
        let invalid = LocalEnumAdtDefBuildError::single_failure;
        let OwnerNode::Item(item) = hir.node else {
            return Err(invalid(LocalEnumAdtDefError::NotModuleEnum));
        };
        if hir.id != item.owner_id || item.owner_id.def_id.to_def_id() != payloads.owner() {
            return Err(invalid(LocalEnumAdtDefError::HirOwnerMismatch));
        }
        let ItemKind::Enum { generics, variants } = item.kind else {
            return Err(invalid(LocalEnumAdtDefError::NotModuleEnum));
        };
        if !payload_template_binders_match_hir(generics, payloads.type_params()) {
            return Err(invalid(LocalEnumAdtDefError::GenericParameterMismatch));
        }
        if variants.len() != payloads.variants().len() {
            return Err(invalid(LocalEnumAdtDefError::VariantOrderMismatch));
        }

        for (variant, template) in variants.iter().zip(payloads.variants()) {
            if variant.hir_id.owner != item.owner_id
                || variant.def_id.to_def_id() != template.def_id()
                || variant.name.sym != template.name()
            {
                return Err(invalid(LocalEnumAdtDefError::VariantOrderMismatch));
            }
            let payload_shape_matches = match (&template.payload, variant.shape) {
                (LocalEnumPayloadTemplateShape::Unit, VariantShape::Unit) => true,
                (LocalEnumPayloadTemplateShape::Tuple(templates), VariantShape::Tuple(types)) => {
                    templates.len() == types.len()
                }
                (
                    LocalEnumPayloadTemplateShape::Record(templates),
                    VariantShape::Record(fields),
                ) => {
                    templates.len() == fields.len()
                        && templates.iter().zip(fields).all(|(template, field)| {
                            template.identity()
                                == LocalEnumPayloadTemplateIdentity::Record {
                                    variant: variant.def_id.to_def_id(),
                                    field: field.def_id.to_def_id(),
                                }
                                && template.name() == field.name.sym
                                && template.visibility() == field.vis
                        })
                }
                _ => false,
            };
            if !payload_shape_matches {
                return Err(invalid(LocalEnumAdtDefError::VariantOrderMismatch));
            }
        }

        let discr_ty = repr_int_ty(repr);
        let has_payload = variants
            .iter()
            .any(|variant| !matches!(variant.shape, VariantShape::Unit));
        let mut discriminants = Vec::with_capacity(variants.len());
        let mut issues = Vec::new();
        let mut failures = Vec::new();
        let mut previous: Option<i128> = None;
        let mut seen = HashMap::<i128, DefId>::with_capacity(variants.len());

        for (index, variant) in variants.iter().enumerate() {
            let variant_id = variant.def_id.to_def_id();
            let value = if has_payload {
                match i128::try_from(index) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        failures.push(LocalEnumAdtDefError::DiscriminantOverflow(variant_id));
                        None
                    }
                }
            } else {
                if variant.has_explicit_discriminant && variant.discriminant.is_none() {
                    failures.push(LocalEnumAdtDefError::MissingExplicitDiscriminant(
                        variant_id,
                    ));
                    None
                } else if !variant.has_explicit_discriminant && variant.discriminant.is_some() {
                    failures.push(LocalEnumAdtDefError::UnexpectedImplicitDiscriminant(
                        variant_id,
                    ));
                    None
                } else if variant.has_explicit_discriminant {
                    variant.discriminant
                } else {
                    match previous {
                        Some(value) => match value.checked_add(1) {
                            Some(value) => Some(value),
                            None => {
                                failures
                                    .push(LocalEnumAdtDefError::DiscriminantOverflow(variant_id));
                                None
                            }
                        },
                        None if index == 0 => Some(0),
                        // A preceding explicit value was unavailable or its
                        // implicit successor overflowed. Stay unknown until a
                        // later explicit value provides a new starting point.
                        None => None,
                    }
                }
            };
            previous = value;
            let Some(value) = value else {
                continue;
            };
            discriminants.push(LocalEnumVariantDiscriminant::new(variant_id, value));

            if !discr_ty.contains(value) {
                issues.push(LocalEnumDiscriminantIssue {
                    variant: variant_id,
                    value,
                    kind: LocalEnumDiscriminantIssueKind::OutOfRange,
                });
            }
            if let Some(previous_variant) = seen.insert(value, variant_id) {
                issues.push(LocalEnumDiscriminantIssue {
                    variant: variant_id,
                    value,
                    kind: LocalEnumDiscriminantIssueKind::Duplicate { previous_variant },
                });
            }
        }

        if !failures.is_empty() {
            let reason = failures[0];
            return Err(LocalEnumAdtDefBuildError {
                reason,
                failures,
                discriminants,
                issues,
            });
        }

        Ok((
            Self {
                payloads,
                repr,
                discr_ty,
                discriminants: discriminants.into(),
            },
            issues,
        ))
    }

    /// Payload template from the same enum owner.
    pub fn payloads(&self) -> &LocalEnumPayloadTemplateDef {
        &self.payloads
    }

    /// Validated representation argument, retaining `C` versus an integer
    /// representation even when both use the same discriminant width.
    pub const fn repr(&self) -> Option<ReprAttr> {
        self.repr
    }

    /// Integer type used to validate discriminant values.
    pub const fn discr_ty(&self) -> IntTy {
        self.discr_ty
    }

    /// Discriminants in declaration order.
    pub fn discriminants(&self) -> &[LocalEnumVariantDiscriminant] {
        &self.discriminants
    }

    /// Derive the value type template for one exact variant definition.
    ///
    /// Unit variants produce the enum applied to its identity arguments.
    /// Tuple variants produce a constructor signature whose generic binders
    /// retain the enum's exact source order and `DefId`s. Record variants
    /// explicitly report that they have no value type.
    pub fn value_type_of_variant(
        &self,
        interner: &crate::ty::TyInterner,
        variant: DefId,
    ) -> Result<LocalEnumVariantValueType, LocalEnumVariantTypeError> {
        let owner = self.payloads.owner();
        if !valid_payload_template_binders(owner, self.payloads.type_params()) {
            return Err(LocalEnumVariantTypeError::InvalidTypeParameters);
        }
        let variant = self.variant_template(variant)?;
        self.validate_variant_template(variant)?;
        let output = self.identity_type(interner);

        match variant.payload() {
            LocalEnumPayloadTemplateShape::Unit => Ok(LocalEnumVariantValueType::Unit(output)),
            LocalEnumPayloadTemplateShape::Tuple(payloads) => {
                let generic_params = self
                    .payloads
                    .type_params()
                    .iter()
                    .map(|(def_id, param)| {
                        crate::ty::FnSigGenericParam::Type(crate::ty::FnSigTypeParam {
                            def_id: *def_id,
                            param: *param,
                        })
                    })
                    .collect::<Vec<_>>();
                let inputs = payloads
                    .iter()
                    .map(|payload| payload.ty().clone())
                    .collect::<Vec<_>>();
                Ok(LocalEnumVariantValueType::TupleConstructor(
                    crate::ty::FnSig::new_with_generic_params(generic_params, inputs, output),
                ))
            }
            LocalEnumPayloadTemplateShape::Record(_) => {
                Ok(LocalEnumVariantValueType::RecordHasNoValueType)
            }
        }
    }

    /// Instantiate one tuple-variant constructor signature for an exact enum
    /// application.
    ///
    /// The enum `DefId` and type-argument arity must match this definition.
    /// Each payload template is substituted once, preserving caller-bound
    /// parameters in replacement arguments. The returned signature is
    /// specialized and therefore has no remaining enum binders.
    pub fn instantiate_tuple_variant_signature(
        &self,
        interner: &crate::ty::TyInterner,
        enum_ty: &crate::ty::Ty,
        variant: DefId,
    ) -> Result<crate::ty::FnSig, LocalEnumVariantTypeError> {
        let signature = match self.value_type_of_variant(interner, variant)? {
            LocalEnumVariantValueType::TupleConstructor(signature) => signature,
            LocalEnumVariantValueType::Unit(_)
            | LocalEnumVariantValueType::RecordHasNoValueType => {
                return Err(LocalEnumVariantTypeError::NotTupleVariant(variant));
            }
        };
        let crate::ty::TyKind::Adt(enum_def, type_args) = enum_ty.kind() else {
            return Err(LocalEnumVariantTypeError::NotAdt);
        };
        let expected_enum = self.payloads.owner();
        if *enum_def != expected_enum {
            return Err(LocalEnumVariantTypeError::WrongEnum {
                expected: expected_enum,
                found: *enum_def,
            });
        }
        if type_args.len() != self.payloads.type_params().len() {
            return Err(LocalEnumVariantTypeError::WrongNumberOfTypeArguments {
                expected: self.payloads.type_params().len(),
                found: type_args.len(),
            });
        }
        let arguments = type_args
            .iter()
            .cloned()
            .map(crate::ty::GenericArg::Type)
            .collect::<Vec<_>>();
        let inputs = signature
            .inputs()
            .iter()
            .map(|input| interner.substitute(input, &arguments))
            .collect::<Result<Vec<_>, _>>()
            .map_err(LocalEnumVariantTypeError::Substitution)?;
        Ok(crate::ty::FnSig::new_with_generic_params(
            [],
            inputs,
            enum_ty.clone(),
        ))
    }

    fn variant_template(
        &self,
        variant: DefId,
    ) -> Result<&LocalEnumPayloadVariantTemplateDef, LocalEnumVariantTypeError> {
        if variant.package != self.payloads.owner().package {
            return Err(LocalEnumVariantTypeError::ForeignVariant(variant));
        }
        self.payloads
            .variants()
            .iter()
            .find(|candidate| candidate.def_id() == variant)
            .ok_or(LocalEnumVariantTypeError::UnknownVariant(variant))
    }

    fn validate_variant_template(
        &self,
        variant: &LocalEnumPayloadVariantTemplateDef,
    ) -> Result<(), LocalEnumVariantTypeError> {
        let owner = self.payloads.owner();
        let invalid = || LocalEnumVariantTypeError::InvalidPayloadShape(variant.def_id());
        if variant.def_id().package != owner.package
            || variant.def_id() == owner
            || variant.name().is_error()
        {
            return Err(invalid());
        }
        match variant.payload() {
            LocalEnumPayloadTemplateShape::Unit => {}
            LocalEnumPayloadTemplateShape::Tuple(payloads) => {
                if payloads.is_empty()
                    || payloads.iter().enumerate().any(|(index, payload)| {
                        u32::try_from(index).ok().is_none_or(|index| {
                            payload.identity()
                                != (LocalEnumPayloadTemplateIdentity::Tuple {
                                    variant: variant.def_id(),
                                    index,
                                })
                        }) || !local_payload_template_matches_binders(
                            payload.ty(),
                            self.payloads.type_params(),
                        )
                    })
                {
                    return Err(invalid());
                }
            }
            LocalEnumPayloadTemplateShape::Record(fields) => {
                let mut field_ids = HashSet::with_capacity(fields.len());
                if fields.is_empty()
                    || fields.iter().any(|field| {
                        !matches!(
                            field.identity(),
                            LocalEnumPayloadTemplateIdentity::Record {
                                variant: field_variant,
                                field: field_id,
                            } if field_variant == variant.def_id()
                                && field_id.package == owner.package
                                && field_id != owner
                                && field_id != variant.def_id()
                                && field_ids.insert(field_id)
                        ) || field.name().is_error()
                            || !local_payload_template_matches_binders(
                                field.ty(),
                                self.payloads.type_params(),
                            )
                    })
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    fn identity_type(&self, interner: &crate::ty::TyInterner) -> crate::ty::Ty {
        let type_args = self
            .payloads
            .type_params()
            .iter()
            .map(|(_, param)| interner.intern(crate::ty::TyKind::Param(*param)))
            .collect::<Vec<_>>();
        interner.intern(crate::ty::TyKind::Adt(
            self.payloads.owner(),
            type_args.into(),
        ))
    }
}

fn repr_int_ty(repr: Option<ReprAttr>) -> IntTy {
    match repr {
        None => IntTy::I64,
        Some(ReprAttr::C) => IntTy::I32,
        Some(ReprAttr::Integer(integer)) => match integer {
            ReprInteger::I8 => IntTy::I8,
            ReprInteger::I16 => IntTy::I16,
            ReprInteger::I32 => IntTy::I32,
            ReprInteger::I64 => IntTy::I64,
            ReprInteger::U8 => IntTy::U8,
            ReprInteger::U16 => IntTy::U16,
            ReprInteger::U32 => IntTy::U32,
            ReprInteger::U64 => IntTy::U64,
        },
    }
}

/// Why a local enum's payload template could not be combined with HIR
/// discriminants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalEnumAdtDefError {
    NotModuleEnum,
    HirOwnerMismatch,
    GenericParameterMismatch,
    VariantOrderMismatch,
    MissingExplicitDiscriminant(DefId),
    UnexpectedImplicitDiscriminant(DefId),
    DiscriminantOverflow(DefId),
}

fn payload_template_binders_match_hir(
    generics: sai_hir::types::Generics<'_>,
    type_params: &[(DefId, crate::ty::ParamTy)],
) -> bool {
    generics.where_preds.is_empty()
        && generics.params.len() == type_params.len()
        && generics.params.iter().zip(type_params).enumerate().all(
            |(index, (hir_param, (def_id, param)))| {
                hir_param.def_id.to_def_id() == *def_id
                    && hir_param.name.sym == param.name
                    && u32::try_from(index).ok() == Some(param.index)
                    && matches!(
                        hir_param.kind,
                        sai_hir::types::GenericParamKind::Type { bounds } if bounds.is_empty()
                    )
            },
        )
}

/// Why an enum definition could not be built, with every representable partial
/// discriminant and semantic issue collected across the full source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumAdtDefBuildError {
    reason: LocalEnumAdtDefError,
    failures: Vec<LocalEnumAdtDefError>,
    discriminants: Vec<LocalEnumVariantDiscriminant>,
    issues: Vec<LocalEnumDiscriminantIssue>,
}

impl LocalEnumAdtDefBuildError {
    fn single_failure(reason: LocalEnumAdtDefError) -> Self {
        Self {
            reason,
            failures: vec![reason],
            discriminants: Vec::new(),
            issues: Vec::new(),
        }
    }

    /// Structural or discriminant failure that prevented construction.
    pub fn reason(&self) -> LocalEnumAdtDefError {
        self.reason
    }

    /// Every unrepresentable failure, in source order.
    pub fn failures(&self) -> &[LocalEnumAdtDefError] {
        &self.failures
    }

    /// Computed discriminants retained in source order despite a later
    /// unrepresentable value.
    pub fn discriminants(&self) -> &[LocalEnumVariantDiscriminant] {
        &self.discriminants
    }

    /// All source-ordered range/duplicate issues found during construction.
    pub fn issues(&self) -> &[LocalEnumDiscriminantIssue] {
        &self.issues
    }
}

/// A variant and its source-ordered payload shape in a local enum template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumPayloadVariantTemplateDef {
    def_id: DefId,
    name: Symbol,
    payload: LocalEnumPayloadTemplateShape,
}

impl LocalEnumPayloadVariantTemplateDef {
    /// Build a variant value for [`LocalEnumPayloadTemplateDef::try_new`].
    pub fn new(def_id: DefId, name: Symbol, payload: LocalEnumPayloadTemplateShape) -> Self {
        Self {
            def_id,
            name,
            payload,
        }
    }

    /// Exact variant definition.
    pub const fn def_id(&self) -> DefId {
        self.def_id
    }

    /// Variant name.
    pub const fn name(&self) -> Symbol {
        self.name
    }

    /// Unit, tuple, or record payload templates in source order.
    pub const fn payload(&self) -> &LocalEnumPayloadTemplateShape {
        &self.payload
    }
}

/// The unit, tuple, or record shape of a local enum variant payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalEnumPayloadTemplateShape {
    Unit,
    Tuple(Arc<[LocalEnumTuplePayloadTemplate]>),
    Record(Arc<[LocalEnumRecordPayloadTemplate]>),
}

/// Identity of a payload type template.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocalEnumPayloadTemplateIdentity {
    Tuple { variant: DefId, index: u32 },
    Record { variant: DefId, field: DefId },
}

/// A tuple payload type template, identified by its variant and position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumTuplePayloadTemplate {
    identity: LocalEnumPayloadTemplateIdentity,
    ty: crate::ty::Ty,
}

impl LocalEnumTuplePayloadTemplate {
    /// Build a tuple payload template for a specific source position.
    pub fn new(variant: DefId, index: u32, ty: crate::ty::Ty) -> Self {
        Self {
            identity: LocalEnumPayloadTemplateIdentity::Tuple { variant, index },
            ty,
        }
    }

    /// Exact variant and zero-based payload position.
    pub const fn identity(&self) -> LocalEnumPayloadTemplateIdentity {
        self.identity
    }

    /// Semantic payload type template.
    pub const fn ty(&self) -> &crate::ty::Ty {
        &self.ty
    }
}

/// A record payload type template, retaining its HIR field identity and
/// declaration metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEnumRecordPayloadTemplate {
    identity: LocalEnumPayloadTemplateIdentity,
    name: Symbol,
    visibility: Visibility,
    ty: crate::ty::Ty,
}

impl LocalEnumRecordPayloadTemplate {
    /// Build a record payload template for a specific variant field.
    pub fn new(
        variant: DefId,
        field: DefId,
        name: Symbol,
        visibility: Visibility,
        ty: crate::ty::Ty,
    ) -> Self {
        Self {
            identity: LocalEnumPayloadTemplateIdentity::Record { variant, field },
            name,
            visibility,
            ty,
        }
    }

    /// Exact variant and HIR field definition.
    pub const fn identity(&self) -> LocalEnumPayloadTemplateIdentity {
        self.identity
    }

    /// Source-level field name.
    pub const fn name(&self) -> Symbol {
        self.name
    }

    /// Field visibility from HIR.
    pub const fn visibility(&self) -> Visibility {
        self.visibility
    }

    /// Semantic payload type template.
    pub const fn ty(&self) -> &crate::ty::Ty {
        &self.ty
    }
}

/// Why a payload-only local enum template could not be constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalEnumPayloadTemplateDefError {
    InvalidTypeParameters,
    InvalidVariantIdentity,
    EmptyPayloadVariantShape,
    InvalidTuplePayloadIdentity,
    InvalidRecordPayloadIdentity,
    InvalidPayloadTemplate,
}

fn valid_payload_template_binders(
    owner: DefId,
    type_params: &[(DefId, crate::ty::ParamTy)],
) -> bool {
    let mut def_ids = HashSet::with_capacity(type_params.len());
    let mut names = HashSet::with_capacity(type_params.len());
    type_params
        .iter()
        .enumerate()
        .all(|(index, (def_id, param))| {
            def_id.package == owner.package
                && u32::try_from(index).ok() == Some(param.index)
                && !param.name.is_error()
                && def_ids.insert(*def_id)
                && names.insert(param.name)
        })
}

fn local_payload_template_matches_binders(
    template: &crate::ty::Ty,
    type_params: &[(DefId, crate::ty::ParamTy)],
) -> bool {
    let mut pending = vec![template];
    while let Some(ty) = pending.pop() {
        match ty.kind() {
            crate::ty::TyKind::Param(parameter) => {
                let Some(index) = usize::try_from(parameter.index).ok() else {
                    return false;
                };
                if type_params
                    .get(index)
                    .is_none_or(|(_, expected)| expected != parameter)
                {
                    return false;
                }
            }
            crate::ty::TyKind::Ref(_, inner, _) => pending.push(inner),
            crate::ty::TyKind::Tuple(elements) => pending.extend(elements.iter()),
            crate::ty::TyKind::Array(element, length) => {
                if matches!(length, crate::ty::Const::Param(_)) {
                    return false;
                }
                pending.push(element);
            }
            crate::ty::TyKind::FnPtr(signature) => {
                pending.extend(signature.inputs());
                pending.push(signature.output());
            }
            crate::ty::TyKind::Adt(_, arguments) => pending.extend(arguments.iter()),
            crate::ty::TyKind::Int(_)
            | crate::ty::TyKind::Uint(_)
            | crate::ty::TyKind::Float(_)
            | crate::ty::TyKind::Bool
            | crate::ty::TyKind::Byte
            | crate::ty::TyKind::Char
            | crate::ty::TyKind::Never => {}
        }
    }
    true
}

/// A validated local flags definition together with its ADT representation.
///
/// The descriptor keeps declaration-ordered flag names and spans for later
/// `FlagsConst` resolution. The `AdtDef` has one synthetic variant and one
/// hidden storage field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFlagsAdtDef {
    adt: AdtDef,
    flags: FlagsDef,
}

impl LocalFlagsAdtDef {
    /// The ADT model.
    pub fn adt_def(&self) -> &AdtDef {
        &self.adt
    }

    /// Validated flag names and source spans in declaration order.
    pub fn flags_def(&self) -> &FlagsDef {
        &self.flags
    }
}

/// A local interface enum projection, including cases that this slice cannot
/// yet represent. The owner is retained for every outcome so unsupported
/// payload or generic enums are not confused with non-enum definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalInterfaceEnumAdtProjection {
    /// A validated flags enum. Its full ADT is available through the existing
    /// `local_flags_adt_def` accessor.
    Flags { owner: DefId },
    /// An ordinary unit-only enum represented by this slice.
    Unit(LocalUnitEnumAdtDef),
    /// An enum declaration outside this slice's supported semantic subset.
    Unsupported {
        owner: DefId,
        reason: LocalEnumAdtUnsupportedReason,
    },
}

impl LocalInterfaceEnumAdtProjection {
    /// Definition identity of the source enum.
    pub fn owner(&self) -> DefId {
        match self {
            Self::Flags { owner } | Self::Unsupported { owner, .. } => *owner,
            Self::Unit(definition) => definition.adt_def().def_id(),
        }
    }
}

/// A reason a local interface enum is not represented by the unit-enum slice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocalEnumAdtUnsupportedReason {
    GenericParameters,
    Payload,
    UnvalidatedRepresentation,
    MalformedDiscriminant,
    DiscriminantOverflow,
    DiscriminantOutOfRange,
    DuplicateDiscriminant,
    InvalidVariantIdentity,
    InterfaceItemCheckFailed,
}

/// A local interface enum whose variants are all unit variants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalUnitEnumAdtDef {
    adt: AdtDef,
}

impl LocalUnitEnumAdtDef {
    /// Build an ordinary unit-enum model from a lowered interface declaration.
    ///
    /// The interface check must be the same-snapshot result produced for this
    /// HIR owner. Its opaque enum identity binds the accepted-flags decision to
    /// the exact declaration. This constructor checks the HIR shape and
    /// discriminants itself, fixes the current default representation to
    /// `I64`, and rejects a valid accepted flags declaration so it cannot be
    /// projected as an ordinary enum. Invalid and empty accepted `@flags`
    /// declarations retain ordinary-enum fallback behavior.
    ///
    /// This is intentionally narrower than a general `adt_def(DefId)` query.
    pub fn from_interface_hir(
        hir: &HirOwner<'_>,
        interface_check: &InterfaceItemCheck,
        paths: &DefPathInterner,
        symbols: &Interner,
    ) -> Result<Self, LocalUnitEnumAdtDefError> {
        let OwnerNode::InterfaceItem(item) = hir.node else {
            return Err(LocalUnitEnumAdtDefError::NotInterfaceItem);
        };
        if hir.id != item.owner_id {
            return Err(LocalUnitEnumAdtDefError::HirOwnerMismatch);
        }
        let InterfaceItemKind::Enum { generics, variants } = item.kind else {
            return Err(LocalUnitEnumAdtDefError::NotEnum);
        };
        let flags_check = interface_check
            .flags_check_for_enum(
                item.owner_id,
                variants,
                raw_flags_marker_is_valid(hir.raw_attrs),
            )
            .ok_or(LocalUnitEnumAdtDefError::InterfaceCheckMismatch)?;
        let flags_marker_accepted = flags_check.accepted_marker();
        let owner = item.owner_id.def_id;
        if !generics.params.is_empty()
            || !generics.where_preds.is_empty()
            || item.generic_params_span.is_some()
            || item.where_clause_span.is_some()
        {
            return Err(LocalUnitEnumAdtDefError::GenericParameters);
        }
        if hir
            .raw_attrs
            .iter()
            .any(|attribute| attribute.name == AttrName::Repr)
        {
            return Err(LocalUnitEnumAdtDefError::ExplicitRepresentation);
        }
        if !hir.diagnostics.is_empty() {
            return Err(LocalUnitEnumAdtDefError::InvalidHir);
        }
        if variants
            .iter()
            .any(|variant| !matches!(variant.shape, VariantShape::Unit))
        {
            return Err(LocalUnitEnumAdtDefError::Payload);
        }
        if let Some(flags_def) = interface_check.flags_def() {
            if !flags_def.matches_enum(owner.to_def_id(), variants) {
                return Err(LocalUnitEnumAdtDefError::InterfaceCheckMismatch);
            }
            return Err(LocalUnitEnumAdtDefError::ValidFlagsMustUseFlagsAdt);
        }
        if (flags_marker_accepted || flags_check.valid_raw_marker())
            && is_valid_flags_shape(variants)
        {
            return Err(LocalUnitEnumAdtDefError::ValidFlagsMustUseFlagsAdt);
        }

        let mut previous: Option<i128> = None;
        let mut seen_discriminants = HashSet::with_capacity(variants.len());
        let mut local_variants = Vec::with_capacity(variants.len());
        for variant in variants {
            let discr = match (variant.has_explicit_discriminant, variant.discriminant) {
                (true, Some(value)) => value,
                (true, None) | (false, Some(_)) => {
                    return Err(LocalUnitEnumAdtDefError::MalformedDiscriminant);
                }
                (false, None) => match previous {
                    Some(value) => value
                        .checked_add(1)
                        .ok_or(LocalUnitEnumAdtDefError::DiscriminantOverflow)?,
                    None => 0,
                },
            };
            if !IntTy::I64.contains(discr) {
                return Err(LocalUnitEnumAdtDefError::DiscriminantOutOfRange);
            }
            if !seen_discriminants.insert(discr) {
                return Err(LocalUnitEnumAdtDefError::DuplicateDiscriminant);
            }
            previous = Some(discr);
            local_variants.push((variant.def_id, variant.name.sym, discr));
        }

        Self::from_validated_parts(owner, IntTy::I64, &local_variants, paths, symbols)
    }

    /// Build from checked source-ordered identities. Kept private so public
    /// callers must provide the HIR declaration that proves variant shape.
    fn from_validated_parts(
        owner: LocalDefId,
        discr_ty: IntTy,
        variants: &[(LocalDefId, Symbol, i128)],
        paths: &DefPathInterner,
        symbols: &Interner,
    ) -> Result<Self, LocalUnitEnumAdtDefError> {
        if paths.package() != owner.package() {
            return Err(LocalUnitEnumAdtDefError::OwnerDefinitionPathMismatch);
        }
        let owner_path = paths
            .resolve(owner)
            .map_err(|_| LocalUnitEnumAdtDefError::OwnerDefinitionPathMismatch)?
            .ok_or(LocalUnitEnumAdtDefError::OwnerDefinitionPathMismatch)?;
        if owner_path.package() != owner.package()
            || !matches!(
                owner_path.data().last(),
                Some(DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(_),
                    ..
                })
            )
        {
            return Err(LocalUnitEnumAdtDefError::OwnerDefinitionPathMismatch);
        }

        let mut seen_ids = HashSet::with_capacity(variants.len());
        let mut seen_discriminants = HashSet::with_capacity(variants.len());
        let mut occurrences = HashMap::<Symbol, u32>::new();
        let mut lowered_variants = Vec::with_capacity(variants.len());
        for (variant, name, discr) in variants {
            if !discr_ty.contains(*discr) {
                return Err(LocalUnitEnumAdtDefError::DiscriminantOutOfRange);
            }
            if !seen_discriminants.insert(*discr) {
                return Err(LocalUnitEnumAdtDefError::DuplicateDiscriminant);
            }
            if variant.package() != owner.package() {
                return Err(LocalUnitEnumAdtDefError::VariantDefinitionPackageMismatch);
            }
            let variant_id = variant.to_def_id();
            if variant_id == owner.to_def_id() {
                return Err(LocalUnitEnumAdtDefError::VariantAliasesOwner);
            }
            if !seen_ids.insert(variant_id) {
                return Err(LocalUnitEnumAdtDefError::DuplicateVariantDefinition);
            }
            symbols
                .resolve(*name)
                .map_err(|_| LocalUnitEnumAdtDefError::InvalidSymbol)?;
            let disambiguator = occurrences.entry(*name).or_default();
            let expected_component = DisambiguatedDefPathData {
                data: DefPathData::Variant(*name),
                disambiguator: *disambiguator,
            };
            *disambiguator = disambiguator
                .checked_add(1)
                .ok_or(LocalUnitEnumAdtDefError::TooManySameNamedVariants)?;
            let variant_path = paths
                .resolve(*variant)
                .map_err(|_| LocalUnitEnumAdtDefError::VariantDefinitionPathMismatch)?
                .ok_or(LocalUnitEnumAdtDefError::VariantDefinitionPathMismatch)?;
            if variant_path.package() != owner.package()
                || variant_path.data().len() != owner_path.data().len() + 1
                || variant_path.data().get(..owner_path.data().len()) != Some(owner_path.data())
                || variant_path.data().last() != Some(&expected_component)
            {
                return Err(LocalUnitEnumAdtDefError::VariantDefinitionPathMismatch);
            }
            lowered_variants.push(VariantDef {
                def_id: variant_id,
                name: *name,
                discr: *discr,
                fields: Arc::from([]),
            });
        }

        Ok(Self {
            adt: AdtDef {
                def_id: owner.to_def_id(),
                kind: AdtKind::Enum,
                discr_ty,
                variants: lowered_variants.into(),
            },
        })
    }

    /// The enum ADT model.
    pub fn adt_def(&self) -> &AdtDef {
        &self.adt
    }
}

fn is_valid_flags_shape(variants: &[Variant<'_>]) -> bool {
    (1..=32).contains(&variants.len())
        && variants.iter().all(|variant| {
            matches!(variant.shape, VariantShape::Unit)
                && !variant.has_explicit_discriminant
                && variant.discriminant.is_none()
        })
}

/// Why a local unit enum could not be assembled from verified DefPaths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalUnitEnumAdtDefError {
    NotInterfaceItem,
    HirOwnerMismatch,
    NotEnum,
    GenericParameters,
    ExplicitRepresentation,
    InvalidHir,
    Payload,
    InterfaceCheckMismatch,
    ValidFlagsMustUseFlagsAdt,
    MalformedDiscriminant,
    DiscriminantOverflow,
    OwnerDefinitionPathMismatch,
    VariantDefinitionPackageMismatch,
    VariantDefinitionPathMismatch,
    VariantAliasesOwner,
    DuplicateVariantDefinition,
    TooManySameNamedVariants,
    DiscriminantOutOfRange,
    DuplicateDiscriminant,
    InvalidSymbol,
}

/// Why a validated flags descriptor could not be turned into an ADT model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFlagsAdtDefError {
    OwnerMismatch,
    OwnerDefinitionPathMismatch,
    InvalidFlagsCount,
    ChildDefinitionPackageMismatch,
    ChildDefinitionPathMismatch,
    ChildDefinitionAliasesParent,
    ChildDefinitionsAliasEachOther,
    InvalidSyntheticNames,
    InvalidSymbol,
}

impl FlagsDef {
    /// Build the flags-only ADT model using already-interned real child IDs.
    ///
    /// The constructor validates the owner and child paths against `paths`,
    /// including the reserved synthetic names and exact parent relationships.
    pub fn into_local_adt_def(
        self,
        owner: LocalDefId,
        variant: LocalDefId,
        field: LocalDefId,
        variant_name: Symbol,
        bits_name: Symbol,
        paths: &DefPathInterner,
        symbols: &Interner,
    ) -> Result<LocalFlagsAdtDef, LocalFlagsAdtDefError> {
        let owner_id = owner.to_def_id();
        if self.adt() != owner_id {
            return Err(LocalFlagsAdtDefError::OwnerMismatch);
        }
        if !(1..=32).contains(&self.flags().len()) {
            return Err(LocalFlagsAdtDefError::InvalidFlagsCount);
        }
        if variant.package() != owner.package() || field.package() != owner.package() {
            return Err(LocalFlagsAdtDefError::ChildDefinitionPackageMismatch);
        }
        if paths.package() != owner.package() {
            return Err(LocalFlagsAdtDefError::OwnerDefinitionPathMismatch);
        }

        let owner_path = paths
            .resolve(owner)
            .map_err(|_| LocalFlagsAdtDefError::OwnerDefinitionPathMismatch)?
            .ok_or(LocalFlagsAdtDefError::OwnerDefinitionPathMismatch)?;
        if owner_path.package() != owner.package()
            || !matches!(
                owner_path.data().last(),
                Some(DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(_),
                    ..
                })
            )
        {
            return Err(LocalFlagsAdtDefError::OwnerDefinitionPathMismatch);
        }

        let variant_id = variant.to_def_id();
        let field_id = field.to_def_id();
        if variant_id == owner_id || field_id == owner_id {
            return Err(LocalFlagsAdtDefError::ChildDefinitionAliasesParent);
        }
        if variant_id == field_id {
            return Err(LocalFlagsAdtDefError::ChildDefinitionsAliasEachOther);
        }
        let variant_text = symbols
            .resolve(variant_name)
            .map_err(|_| LocalFlagsAdtDefError::InvalidSymbol)?;
        let bits_text = symbols
            .resolve(bits_name)
            .map_err(|_| LocalFlagsAdtDefError::InvalidSymbol)?;
        if variant_text.as_ref() != "$flags" || bits_text.as_ref() != "$bits" {
            return Err(LocalFlagsAdtDefError::InvalidSyntheticNames);
        }
        if self
            .flags()
            .iter()
            .any(|(flag, _)| symbols.resolve(*flag).is_err())
        {
            return Err(LocalFlagsAdtDefError::InvalidSymbol);
        }

        let variant_path = paths
            .resolve(variant)
            .map_err(|_| LocalFlagsAdtDefError::ChildDefinitionPathMismatch)?
            .ok_or(LocalFlagsAdtDefError::ChildDefinitionPathMismatch)?;
        let field_path = paths
            .resolve(field)
            .map_err(|_| LocalFlagsAdtDefError::ChildDefinitionPathMismatch)?
            .ok_or(LocalFlagsAdtDefError::ChildDefinitionPathMismatch)?;
        let variant_component = DisambiguatedDefPathData {
            data: DefPathData::Variant(variant_name),
            disambiguator: 0,
        };
        let field_component = DisambiguatedDefPathData {
            data: DefPathData::Field(bits_name),
            disambiguator: 0,
        };
        if variant_path.package() != owner.package()
            || variant_path.data().len() != owner_path.data().len() + 1
            || variant_path.data().get(..owner_path.data().len()) != Some(owner_path.data())
            || variant_path.data().last() != Some(&variant_component)
            || field_path.package() != owner.package()
            || field_path.data().len() != variant_path.data().len() + 1
            || field_path.data().get(..variant_path.data().len()) != Some(variant_path.data())
            || field_path.data().last() != Some(&field_component)
        {
            return Err(LocalFlagsAdtDefError::ChildDefinitionPathMismatch);
        }

        let field = FieldDef {
            def_id: field_id,
            name: bits_name,
            vis: Visibility::Private,
            ty: Ty::Int(self.repr_int_ty()),
        };
        let variant = VariantDef {
            def_id: variant_id,
            name: variant_name,
            discr: 0,
            fields: Arc::from([field]),
        };
        Ok(LocalFlagsAdtDef {
            adt: AdtDef {
                def_id: owner_id,
                kind: AdtKind::Flags,
                discr_ty: IntTy::I64,
                variants: Arc::from([variant]),
            },
            flags: self,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdtKind, IntTy, LocalEnumAdtDef, LocalEnumAdtDefError, LocalEnumDiscriminantIssueKind,
        LocalEnumPayloadTemplateDef, LocalEnumPayloadTemplateDefError,
        LocalEnumPayloadTemplateIdentity, LocalEnumPayloadTemplateShape,
        LocalEnumPayloadVariantTemplateDef, LocalEnumRecordPayloadTemplate,
        LocalEnumTuplePayloadTemplate, LocalEnumVariantTypeError, LocalEnumVariantValueType,
        LocalUnitEnumAdtDef, LocalUnitEnumAdtDefError, Visibility,
    };
    use sai_hir::attrs::{ReprAttr, ReprInteger};
    use sai_hir::item_tree::{AttrName, RawAttr, RawAttrArg};
    use sai_hir::items::{
        HirOwner, InterfaceItem, InterfaceItemKind, Item, ItemKind, OwnerNode, Variant,
        VariantShape,
    };
    use sai_hir::types::{
        GenericParam, GenericParamKind, Generics, Name, Ty as HirTy, TyKind as HirTyKind,
    };
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, ItemLocalId,
        LocalDefId, OwnerId, Span, SpanCtxt,
    };
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_syntax::ast::{self, AstNode};
    use sai_vfs::{FileId, SnapshotFile, SourceFile, TextRange, TextSize, WorkspaceSnapshot};
    use std::sync::Arc;

    struct TestIds {
        owner: LocalDefId,
        paths: DefPathInterner,
        packages: PackageInterner,
        symbols: Interner,
    }

    fn test_ids() -> TestIds {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:enum#1.0").unwrap();
        let symbols = Interner::new();
        let owner_name = symbols.intern("State").unwrap();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner_path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(owner_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let owner = paths.intern(owner_path).unwrap();
        TestIds {
            owner,
            paths,
            packages,
            symbols,
        }
    }

    fn span() -> Span {
        Span {
            file: FileId::new(0),
            range: TextRange::empty(TextSize::new(0)),
            ctxt: SpanCtxt::Root,
        }
    }

    fn unit_variant(
        owner: LocalDefId,
        def_id: LocalDefId,
        symbol: sai_hir::Symbol,
        local_id: u32,
        discriminant: Option<i128>,
        has_explicit_discriminant: bool,
    ) -> Variant<'static> {
        let span = span();
        Variant {
            hir_id: HirId {
                owner: OwnerId { def_id: owner },
                local_id: ItemLocalId::from_raw(local_id),
            },
            def_id,
            name: Name { sym: symbol, span },
            shape: VariantShape::Unit,
            discriminant,
            has_explicit_discriminant,
            span,
            payload_span: None,
        }
    }

    fn enum_item<'hir>(
        owner: LocalDefId,
        name: sai_hir::Symbol,
        variants: &'hir [Variant<'hir>],
    ) -> InterfaceItem<'hir> {
        let span = span();
        InterfaceItem {
            owner_id: OwnerId { def_id: owner },
            interface: owner,
            name: Name { sym: name, span },
            kind: InterfaceItemKind::Enum {
                generics: Generics {
                    params: &[],
                    where_preds: &[],
                },
                variants,
            },
            span,
            generic_params_span: None,
            where_clause_span: None,
        }
    }

    fn hir_owner<'hir>(item: &'hir InterfaceItem<'hir>) -> HirOwner<'hir> {
        HirOwner {
            id: item.owner_id,
            node: OwnerNode::InterfaceItem(item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        }
    }

    fn module_enum_item<'hir>(
        owner: LocalDefId,
        name: sai_hir::Symbol,
        variants: &'hir [Variant<'hir>],
    ) -> Item<'hir> {
        let span = span();
        Item {
            owner_id: OwnerId { def_id: owner },
            name: Name { sym: name, span },
            vis: Visibility::Pub,
            kind: ItemKind::Enum {
                generics: Generics {
                    params: &[],
                    where_preds: &[],
                },
                variants,
            },
            span,
        }
    }

    fn module_hir_owner<'hir>(item: &'hir Item<'hir>) -> HirOwner<'hir> {
        HirOwner {
            id: item.owner_id,
            node: OwnerNode::Item(item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        }
    }

    fn unit_payload_template(
        owner: LocalDefId,
        variants: &[Variant<'_>],
    ) -> LocalEnumPayloadTemplateDef {
        LocalEnumPayloadTemplateDef::try_new(
            owner.to_def_id(),
            Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
            variants
                .iter()
                .map(|variant| {
                    LocalEnumPayloadVariantTemplateDef::new(
                        variant.def_id.to_def_id(),
                        variant.name.sym,
                        LocalEnumPayloadTemplateShape::Unit,
                    )
                })
                .collect::<Vec<_>>(),
        )
        .expect("unit enum HIR has a valid payload template")
    }

    fn raw_flags_attr(source: &str, with_argument: bool) -> RawAttr {
        let file = FileId::new(77);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, source, false).expect("valid attribute source")),
            "src/lib.sai",
            None,
            None,
        )])
        .expect("valid snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("attribute source parses");
        let attribute = parsed
            .root
            .descendants()
            .find_map(ast::Attribute::cast)
            .expect("source contains @flags");
        let span = sai_vfs::Span::new(file, attribute.syntax().text_range());
        RawAttr {
            name: AttrName::Flags,
            args: with_argument.then(|| {
                vec![RawAttrArg::Int {
                    value: Some(1),
                    span,
                }]
            }),
            span,
            ptr: ast::AstPtr::new(file, &attribute).expect("attribute pointer is valid"),
            has_error: false,
        }
    }

    fn enum_check(
        hir: &HirOwner<'_>,
        flags_marker_accepted: bool,
        symbols: &Interner,
    ) -> super::super::iface::InterfaceItemCheck {
        crate::iface::interface_item_check(hir, flags_marker_accepted, symbols)
            .expect("test enum names resolve")
    }

    #[test]
    fn local_unit_enum_keeps_variant_order_identity_and_discriminants() {
        let ids = test_ids();
        let ready = ids.symbols.intern("Ready").unwrap();
        let waiting = ids.symbols.intern("Waiting").unwrap();
        let variant_ids = ids
            .paths
            .intern_variants(ids.owner, &[(ready, 0), (waiting, 0), (ready, 1)])
            .unwrap();
        let inputs = [
            (variant_ids[0], ready, -1),
            (variant_ids[1], waiting, 0),
            (variant_ids[2], ready, 1),
        ];

        let definition = LocalUnitEnumAdtDef::from_validated_parts(
            ids.owner,
            IntTy::I64,
            &inputs,
            &ids.paths,
            &ids.symbols,
        )
        .unwrap();

        let adt = definition.adt_def();
        assert_eq!(adt.kind(), AdtKind::Enum);
        assert_eq!(adt.discr_ty(), IntTy::I64);
        assert_eq!(adt.variants().len(), 3);
        assert_eq!(
            adt.variants()
                .iter()
                .map(|variant| (variant.def_id(), variant.name(), variant.discr()))
                .collect::<Vec<_>>(),
            vec![
                (variant_ids[0].to_def_id(), ready, -1),
                (variant_ids[1].to_def_id(), waiting, 0),
                (variant_ids[2].to_def_id(), ready, 1),
            ]
        );
        assert!(
            adt.variants()
                .iter()
                .all(|variant| variant.fields().is_empty())
        );
    }

    #[test]
    fn interface_hir_constructor_proves_unit_shape_and_preserves_flags_fallback() {
        let ids = test_ids();
        let state = ids.symbols.intern("State").unwrap();
        let ready = ids.symbols.intern("Ready").unwrap();
        let waiting = ids.symbols.intern("Waiting").unwrap();
        let variants_ids = ids
            .paths
            .intern_variants(ids.owner, &[(ready, 0), (waiting, 0)])
            .unwrap();
        let variants = [
            unit_variant(ids.owner, variants_ids[0], ready, 1, None, false),
            unit_variant(ids.owner, variants_ids[1], waiting, 2, None, false),
        ];
        let item = enum_item(ids.owner, state, &variants);
        let hir = hir_owner(&item);
        let ordinary_check = enum_check(&hir, false, &ids.symbols);
        let definition = LocalUnitEnumAdtDef::from_interface_hir(
            &hir,
            &ordinary_check,
            &ids.paths,
            &ids.symbols,
        )
        .expect("ordinary unit enum is validated from HIR");
        assert_eq!(
            definition
                .adt_def()
                .variants()
                .iter()
                .map(|variant| variant.discr())
                .collect::<Vec<_>>(),
            [0, 1]
        );
        let valid_flags_check = enum_check(&hir, true, &ids.symbols);
        assert_eq!(
            LocalUnitEnumAdtDef::from_interface_hir(
                &hir,
                &valid_flags_check,
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::ValidFlagsMustUseFlagsAdt),
            "an accepted flags declaration cannot be independently projected as Unit"
        );

        let raw_flags_attrs = [raw_flags_attr(
            "@flags enum State { Ready, Waiting }",
            false,
        )];
        let raw_flags_hir = HirOwner {
            raw_attrs: &raw_flags_attrs,
            ..hir_owner(&item)
        };
        let unaccepted_raw_flags_check = enum_check(&raw_flags_hir, false, &ids.symbols);
        assert_eq!(
            LocalUnitEnumAdtDef::from_interface_hir(
                &raw_flags_hir,
                &unaccepted_raw_flags_check,
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::ValidFlagsMustUseFlagsAdt),
            "false checker acceptance cannot suppress a valid raw @flags marker into Unit"
        );

        let empty_item = enum_item(ids.owner, state, &[]);
        let empty_flags_attrs = [raw_flags_attr("@flags enum State {}", false)];
        let empty_hir = HirOwner {
            raw_attrs: &empty_flags_attrs,
            ..hir_owner(&empty_item)
        };
        let empty_check = enum_check(&empty_hir, false, &ids.symbols);
        let empty = LocalUnitEnumAdtDef::from_interface_hir(
            &empty_hir,
            &empty_check,
            &ids.paths,
            &ids.symbols,
        )
        .expect("empty flags preserve ordinary-enum fallback");
        assert!(empty.adt_def().variants().is_empty());

        let malformed_flags_attrs = [raw_flags_attr(
            "@flags(1) enum State { Ready, Waiting }",
            true,
        )];
        let malformed_flags_hir = HirOwner {
            raw_attrs: &malformed_flags_attrs,
            ..hir_owner(&item)
        };
        let malformed_flags_check = enum_check(&malformed_flags_hir, false, &ids.symbols);
        let malformed_fallback = LocalUnitEnumAdtDef::from_interface_hir(
            &malformed_flags_hir,
            &malformed_flags_check,
            &ids.paths,
            &ids.symbols,
        )
        .expect("malformed marker arguments preserve ordinary-enum fallback");
        assert_eq!(
            malformed_fallback
                .adt_def()
                .variants()
                .iter()
                .map(|variant| variant.discr())
                .collect::<Vec<_>>(),
            [0, 1]
        );

        let explicit = [unit_variant(
            ids.owner,
            variants_ids[0],
            ready,
            1,
            Some(7),
            true,
        )];
        let invalid_flags_item = enum_item(ids.owner, state, &explicit);
        let invalid_flags_attrs = [raw_flags_attr("@flags enum State { Ready = 7 }", false)];
        let invalid_flags_hir = HirOwner {
            raw_attrs: &invalid_flags_attrs,
            ..hir_owner(&invalid_flags_item)
        };
        let invalid_flags_check = enum_check(&invalid_flags_hir, false, &ids.symbols);
        let fallback = LocalUnitEnumAdtDef::from_interface_hir(
            &invalid_flags_hir,
            &invalid_flags_check,
            &ids.paths,
            &ids.symbols,
        )
        .expect("explicitly discriminated flags preserve ordinary-enum fallback");
        assert_eq!(fallback.adt_def().variants()[0].discr(), 7);
    }

    #[test]
    fn interface_hir_constructor_rejects_payload_and_mismatched_owner() {
        let ids = test_ids();
        let state = ids.symbols.intern("State").unwrap();
        let item_name = ids.symbols.intern("Item").unwrap();
        let [variant_id] = ids
            .paths
            .intern_variants(ids.owner, &[(item_name, 0)])
            .unwrap()[..]
        else {
            unreachable!()
        };
        let payload = [Variant {
            shape: VariantShape::Tuple(&[]),
            ..unit_variant(ids.owner, variant_id, item_name, 1, None, false)
        }];
        let payload_item = enum_item(ids.owner, state, &payload);
        let payload_hir = hir_owner(&payload_item);
        let payload_check = enum_check(&payload_hir, false, &ids.symbols);
        assert_eq!(
            LocalUnitEnumAdtDef::from_interface_hir(
                &payload_hir,
                &payload_check,
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::Payload)
        );

        let unit = [unit_variant(
            ids.owner, variant_id, item_name, 1, None, false,
        )];
        let unit_item = enum_item(ids.owner, state, &unit);
        let unit_hir = hir_owner(&unit_item);
        let unit_check = enum_check(&unit_hir, false, &ids.symbols);
        assert_eq!(
            LocalUnitEnumAdtDef::from_interface_hir(
                &unit_hir,
                &payload_check,
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::InterfaceCheckMismatch),
            "a check from another HIR declaration cannot authorize projection"
        );
        let mut mismatched_hir = unit_hir;
        mismatched_hir.id = OwnerId {
            def_id: ids
                .paths
                .intern(
                    DefPath::new(
                        ids.owner.package(),
                        vec![DisambiguatedDefPathData {
                            data: DefPathData::TypeNs(state),
                            disambiguator: 1,
                        }],
                        &ids.packages,
                        &ids.symbols,
                    )
                    .unwrap(),
                )
                .unwrap(),
        };
        assert_eq!(
            LocalUnitEnumAdtDef::from_interface_hir(
                &mismatched_hir,
                &unit_check,
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::HirOwnerMismatch)
        );
    }

    #[test]
    fn local_unit_enum_constructor_rejects_wrong_parent_and_duplicate_ids() {
        let ids = test_ids();
        let ready = ids.symbols.intern("Ready").unwrap();
        let other_name = ids.symbols.intern("Other").unwrap();
        let orphan_path = DefPath::new(
            ids.owner.package(),
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(other_name),
                disambiguator: 0,
            }],
            &ids.packages,
            &ids.symbols,
        )
        .unwrap();
        let orphan = ids.paths.intern(orphan_path).unwrap();
        assert_eq!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &[(orphan, ready, 0)],
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::VariantDefinitionPathMismatch)
        );

        let [variant] = ids.paths.intern_variants(ids.owner, &[(ready, 0)]).unwrap()[..] else {
            unreachable!()
        };
        assert_eq!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &[(variant, ready, 0), (variant, ready, 1)],
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::DuplicateVariantDefinition)
        );
    }

    #[test]
    fn integer_discriminant_ranges_cover_all_fixed_width_repr_types() {
        for (ty, min, max) in [
            (IntTy::I8, i8::MIN as i128, i8::MAX as i128),
            (IntTy::I16, i16::MIN as i128, i16::MAX as i128),
            (IntTy::I32, i32::MIN as i128, i32::MAX as i128),
            (IntTy::I64, i64::MIN as i128, i64::MAX as i128),
            (IntTy::U8, 0, u8::MAX as i128),
            (IntTy::U16, 0, u16::MAX as i128),
            (IntTy::U32, 0, u32::MAX as i128),
            (IntTy::U64, 0, u64::MAX as i128),
        ] {
            assert!(ty.contains(min), "{ty:?} must contain its minimum");
            assert!(ty.contains(max), "{ty:?} must contain its maximum");
            if min > i128::MIN {
                assert!(!ty.contains(min - 1), "{ty:?} accepted a value below range");
            }
            if max < i128::MAX {
                assert!(!ty.contains(max + 1), "{ty:?} accepted a value above range");
            }
        }
    }

    #[test]
    fn local_unit_enum_constructor_checks_repr_boundaries_and_discriminant_uniqueness() {
        let ids = test_ids();
        let minimum = ids.symbols.intern("Minimum").unwrap();
        let maximum = ids.symbols.intern("Maximum").unwrap();
        let variant_ids = ids
            .paths
            .intern_variants(ids.owner, &[(minimum, 0), (maximum, 0)])
            .unwrap();

        let boundary_variants = [
            (variant_ids[0], minimum, i64::MIN as i128),
            (variant_ids[1], maximum, i64::MAX as i128),
        ];
        assert!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &boundary_variants,
                &ids.paths,
                &ids.symbols,
            )
            .is_ok()
        );

        assert_eq!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &[(variant_ids[0], minimum, i64::MIN as i128 - 1)],
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::DiscriminantOutOfRange)
        );
        assert_eq!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &[(variant_ids[0], minimum, i64::MAX as i128 + 1)],
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::DiscriminantOutOfRange)
        );
        assert_eq!(
            LocalUnitEnumAdtDef::from_validated_parts(
                ids.owner,
                IntTy::I64,
                &[(variant_ids[0], minimum, 4), (variant_ids[1], maximum, 4),],
                &ids.paths,
                &ids.symbols,
            ),
            Err(LocalUnitEnumAdtDefError::DuplicateDiscriminant)
        );
    }

    #[test]
    fn local_enum_payload_template_preserves_shapes_binders_and_field_metadata() {
        let ids = test_ids();
        let type_name = ids.symbols.intern("T").unwrap();
        let value_name = ids.symbols.intern("Value").unwrap();
        let named_name = ids.symbols.intern("Named").unwrap();
        let empty_name = ids.symbols.intern("Empty").unwrap();
        let field_name = ids.symbols.intern("item").unwrap();
        let variant_ids = ids
            .paths
            .intern_variants(
                ids.owner,
                &[(value_name, 0), (named_name, 0), (empty_name, 0)],
            )
            .unwrap();
        let owner_path = ids.paths.resolve(ids.owner).unwrap().unwrap();
        let mut binder_components = owner_path.data().to_vec();
        binder_components.push(DisambiguatedDefPathData {
            data: DefPathData::TypeParam(type_name),
            disambiguator: 0,
        });
        let binder = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    binder_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();
        let named_path = ids.paths.resolve(variant_ids[1]).unwrap().unwrap();
        let mut field_components = named_path.data().to_vec();
        field_components.push(DisambiguatedDefPathData {
            data: DefPathData::Field(field_name),
            disambiguator: 0,
        });
        let field = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    field_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();

        let interner = crate::ty::TyInterner::new();
        let parameter = crate::ty::ParamTy {
            index: 0,
            name: type_name,
        };
        let parameter_ty = interner.intern(crate::ty::TyKind::Param(parameter));
        let bool_ty = interner.intern(crate::ty::TyKind::Bool);
        let tuple_ty = interner.tuple([parameter_ty.clone(), bool_ty]);
        let value_id = variant_ids[0].to_def_id();
        let named_id = variant_ids[1].to_def_id();
        let empty_id = variant_ids[2].to_def_id();
        let record_identity = LocalEnumPayloadTemplateIdentity::Record {
            variant: named_id,
            field: field.to_def_id(),
        };
        let definition = LocalEnumPayloadTemplateDef::try_new(
            ids.owner.to_def_id(),
            Arc::from([(binder.to_def_id(), parameter)]),
            Arc::from([
                LocalEnumPayloadVariantTemplateDef::new(
                    value_id,
                    value_name,
                    LocalEnumPayloadTemplateShape::Tuple(Arc::from([
                        LocalEnumTuplePayloadTemplate::new(value_id, 0, tuple_ty.clone()),
                    ])),
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    named_id,
                    named_name,
                    LocalEnumPayloadTemplateShape::Record(Arc::from([
                        LocalEnumRecordPayloadTemplate::new(
                            named_id,
                            field.to_def_id(),
                            field_name,
                            Visibility::Pub,
                            parameter_ty.clone(),
                        ),
                    ])),
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    empty_id,
                    empty_name,
                    LocalEnumPayloadTemplateShape::Unit,
                ),
            ]),
        )
        .expect("validated source-ordered data builds a payload-only definition");

        assert_eq!(definition.owner(), ids.owner.to_def_id());
        assert_eq!(
            definition.type_params(),
            &[(binder.to_def_id(), parameter)],
            "the exact source-ordered binder identity is retained"
        );
        assert_eq!(definition.variants().len(), 3);
        let LocalEnumPayloadTemplateShape::Tuple(tuple) = definition.variants()[0].payload() else {
            panic!("Value keeps its tuple shape")
        };
        assert_eq!(
            tuple[0].identity(),
            LocalEnumPayloadTemplateIdentity::Tuple {
                variant: value_id,
                index: 0,
            }
        );
        assert!(tuple[0].ty().ptr_eq(&tuple_ty));
        let LocalEnumPayloadTemplateShape::Record(record) = definition.variants()[1].payload()
        else {
            panic!("Named keeps its record shape")
        };
        assert_eq!(record[0].identity(), record_identity);
        assert_eq!(record[0].name(), field_name);
        assert_eq!(record[0].visibility(), Visibility::Pub);
        assert!(
            definition
                .payload_template(record_identity)
                .is_some_and(|ty| ty.ptr_eq(&parameter_ty)),
            "record payload lookup uses the exact variant and field IDs"
        );
        assert!(matches!(
            definition.variants()[2].payload(),
            LocalEnumPayloadTemplateShape::Unit
        ));
    }

    #[test]
    fn local_enum_payload_template_rejects_forged_binders_and_positions() {
        let ids = test_ids();
        let type_name = ids.symbols.intern("T").unwrap();
        let value_name = ids.symbols.intern("Value").unwrap();
        let variant = ids
            .paths
            .intern_variants(ids.owner, &[(value_name, 0)])
            .unwrap()[0];
        let owner_path = ids.paths.resolve(ids.owner).unwrap().unwrap();
        let mut binder_components = owner_path.data().to_vec();
        binder_components.push(DisambiguatedDefPathData {
            data: DefPathData::TypeParam(type_name),
            disambiguator: 0,
        });
        let binder = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    binder_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();
        let interner = crate::ty::TyInterner::new();
        let parameter = crate::ty::ParamTy {
            index: 0,
            name: type_name,
        };
        let ty = interner.intern(crate::ty::TyKind::Param(parameter));
        let identity = |index| {
            LocalEnumPayloadVariantTemplateDef::new(
                variant.to_def_id(),
                value_name,
                LocalEnumPayloadTemplateShape::Tuple(Arc::from([
                    LocalEnumTuplePayloadTemplate::new(variant.to_def_id(), index, ty.clone()),
                ])),
            )
        };
        assert_eq!(
            LocalEnumPayloadTemplateDef::try_new(
                ids.owner.to_def_id(),
                Arc::from([(
                    binder.to_def_id(),
                    crate::ty::ParamTy {
                        index: 1,
                        name: type_name,
                    },
                )]),
                Arc::from([identity(0)]),
            ),
            Err(LocalEnumPayloadTemplateDefError::InvalidTypeParameters),
            "binder indices and names must match the source-order parameter records"
        );
        let exact_binder = crate::ty::ParamTy {
            index: 0,
            name: type_name,
        };
        assert_eq!(
            LocalEnumPayloadTemplateDef::try_new(
                ids.owner.to_def_id(),
                Arc::from([(binder.to_def_id(), exact_binder)]),
                Arc::from([identity(1)]),
            ),
            Err(LocalEnumPayloadTemplateDefError::InvalidTuplePayloadIdentity),
            "tuple identities are the exact variant plus contiguous zero-based index"
        );

        assert_eq!(
            LocalEnumPayloadTemplateDef::try_new(
                ids.owner.to_def_id(),
                Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
                Arc::from([LocalEnumPayloadVariantTemplateDef::new(
                    variant.to_def_id(),
                    value_name,
                    LocalEnumPayloadTemplateShape::Tuple(Arc::from([])),
                )]),
            ),
            Err(LocalEnumPayloadTemplateDefError::EmptyPayloadVariantShape),
            "tuple variants require at least one payload"
        );
        assert_eq!(
            LocalEnumPayloadTemplateDef::try_new(
                ids.owner.to_def_id(),
                Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
                Arc::from([LocalEnumPayloadVariantTemplateDef::new(
                    variant.to_def_id(),
                    value_name,
                    LocalEnumPayloadTemplateShape::Record(Arc::from([])),
                )]),
            ),
            Err(LocalEnumPayloadTemplateDefError::EmptyPayloadVariantShape),
            "record variants require at least one field"
        );
    }

    #[test]
    fn local_enum_variant_value_types_preserve_identity_and_instantiate_once() {
        let ids = test_ids();
        let type_names = [
            ids.symbols.intern("T").unwrap(),
            ids.symbols.intern("U").unwrap(),
        ];
        let variant_names = [
            ids.symbols.intern("Empty").unwrap(),
            ids.symbols.intern("Pair").unwrap(),
            ids.symbols.intern("Named").unwrap(),
        ];
        let local_variant_ids = ids
            .paths
            .intern_variants(ids.owner, &variant_names.map(|name| (name, 0)))
            .unwrap();
        let variant_ids = local_variant_ids
            .iter()
            .copied()
            .into_iter()
            .map(|id| id.to_def_id())
            .collect::<Vec<_>>();
        let binder_ids = type_names.map(|name| {
            let owner_path = ids.paths.resolve(ids.owner).unwrap().unwrap();
            let mut components = owner_path.data().to_vec();
            components.push(DisambiguatedDefPathData {
                data: DefPathData::TypeParam(name),
                disambiguator: 0,
            });
            ids.paths
                .intern(
                    DefPath::new(ids.owner.package(), components, &ids.packages, &ids.symbols)
                        .unwrap(),
                )
                .unwrap()
                .to_def_id()
        });
        let parameters = [
            crate::ty::ParamTy {
                index: 0,
                name: type_names[0],
            },
            crate::ty::ParamTy {
                index: 1,
                name: type_names[1],
            },
        ];
        let interner = crate::ty::TyInterner::new();
        let t_ty = interner.intern(crate::ty::TyKind::Param(parameters[0]));
        let u_ty = interner.intern(crate::ty::TyKind::Param(parameters[1]));
        let nested_ty = interner.tuple([u_ty.clone(), t_ty.clone()]);
        let named_field_name = ids.symbols.intern("value").unwrap();
        let named_path = ids.paths.resolve(local_variant_ids[2]).unwrap().unwrap();
        let mut field_components = named_path.data().to_vec();
        field_components.push(DisambiguatedDefPathData {
            data: DefPathData::Field(named_field_name),
            disambiguator: 0,
        });
        let field_id = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    field_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap()
            .to_def_id();
        let payloads = LocalEnumPayloadTemplateDef::try_new(
            ids.owner.to_def_id(),
            Arc::from([
                (binder_ids[0], parameters[0]),
                (binder_ids[1], parameters[1]),
            ]),
            Arc::from([
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[0],
                    variant_names[0],
                    LocalEnumPayloadTemplateShape::Unit,
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[1],
                    variant_names[1],
                    LocalEnumPayloadTemplateShape::Tuple(Arc::from([
                        LocalEnumTuplePayloadTemplate::new(variant_ids[1], 0, t_ty.clone()),
                        LocalEnumTuplePayloadTemplate::new(variant_ids[1], 1, nested_ty.clone()),
                    ])),
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[2],
                    variant_names[2],
                    LocalEnumPayloadTemplateShape::Record(Arc::from([
                        LocalEnumRecordPayloadTemplate::new(
                            variant_ids[2],
                            field_id,
                            named_field_name,
                            Visibility::Pub,
                            u_ty.clone(),
                        ),
                    ])),
                ),
            ]),
        )
        .unwrap();
        let definition = LocalEnumAdtDef {
            payloads,
            repr: None,
            discr_ty: IntTy::I64,
            discriminants: Arc::from([]),
        };

        let identity_enum = interner.intern(crate::ty::TyKind::Adt(
            ids.owner.to_def_id(),
            Arc::from([t_ty.clone(), u_ty.clone()]),
        ));
        assert_eq!(
            definition.value_type_of_variant(&interner, variant_ids[0]),
            Ok(LocalEnumVariantValueType::Unit(identity_enum.clone()))
        );
        let LocalEnumVariantValueType::TupleConstructor(template_signature) = definition
            .value_type_of_variant(&interner, variant_ids[1])
            .unwrap()
        else {
            panic!("the Pair variant has a tuple constructor signature")
        };
        assert_eq!(
            template_signature.generic_params(),
            &[
                crate::ty::FnSigGenericParam::Type(crate::ty::FnSigTypeParam {
                    def_id: binder_ids[0],
                    param: parameters[0],
                }),
                crate::ty::FnSigGenericParam::Type(crate::ty::FnSigTypeParam {
                    def_id: binder_ids[1],
                    param: parameters[1],
                }),
            ],
            "constructor binder metadata keeps the exact enum source order and DefIds"
        );
        assert_eq!(template_signature.inputs(), &[t_ty.clone(), nested_ty]);
        assert_eq!(template_signature.output(), &identity_enum);
        assert_eq!(
            definition.value_type_of_variant(&interner, variant_ids[2]),
            Ok(LocalEnumVariantValueType::RecordHasNoValueType)
        );

        let caller_name = ids.symbols.intern("Caller").unwrap();
        let caller_param = interner.intern(crate::ty::TyKind::Param(crate::ty::ParamTy {
            index: 0,
            name: caller_name,
        }));
        let concrete_enum = interner.intern(crate::ty::TyKind::Adt(
            ids.owner.to_def_id(),
            Arc::from([
                caller_param.clone(),
                interner.intern(crate::ty::TyKind::Bool),
            ]),
        ));
        let instantiated = definition
            .instantiate_tuple_variant_signature(&interner, &concrete_enum, variant_ids[1])
            .unwrap();
        assert!(instantiated.generic_params().is_empty());
        assert_eq!(instantiated.inputs()[0], caller_param);
        assert_eq!(
            instantiated.inputs()[1],
            interner.tuple([
                interner.intern(crate::ty::TyKind::Bool),
                caller_param.clone(),
            ]),
            "substitution descends through template structure once and does not resubstitute caller parameters"
        );
        assert_eq!(instantiated.output(), &concrete_enum);

        let unknown_variant = ids.owner.to_def_id();
        assert_eq!(
            definition.value_type_of_variant(&interner, unknown_variant),
            Err(LocalEnumVariantTypeError::UnknownVariant(unknown_variant))
        );
        assert_eq!(
            definition.instantiate_tuple_variant_signature(
                &interner,
                &concrete_enum,
                variant_ids[0],
            ),
            Err(LocalEnumVariantTypeError::NotTupleVariant(variant_ids[0]))
        );
        let wrong_enum = interner.intern(crate::ty::TyKind::Adt(
            variant_ids[0],
            Arc::from([caller_param.clone(), caller_param.clone()]),
        ));
        assert_eq!(
            definition.instantiate_tuple_variant_signature(&interner, &wrong_enum, variant_ids[1],),
            Err(LocalEnumVariantTypeError::WrongEnum {
                expected: ids.owner.to_def_id(),
                found: variant_ids[0],
            })
        );
        let wrong_arity = interner.intern(crate::ty::TyKind::Adt(
            ids.owner.to_def_id(),
            Arc::from([caller_param.clone()]),
        ));
        assert_eq!(
            definition
                .instantiate_tuple_variant_signature(&interner, &wrong_arity, variant_ids[1],),
            Err(LocalEnumVariantTypeError::WrongNumberOfTypeArguments {
                expected: 2,
                found: 1,
            })
        );
        assert_eq!(
            definition.instantiate_tuple_variant_signature(
                &interner,
                &caller_param,
                variant_ids[1],
            ),
            Err(LocalEnumVariantTypeError::NotAdt)
        );
    }

    #[test]
    fn local_enum_variant_value_type_rejects_foreign_and_malformed_variants() {
        let ids = test_ids();
        let names = [ids.symbols.intern("Value").unwrap()];
        let variant_id = ids
            .paths
            .intern_variants(ids.owner, &[(names[0], 0)])
            .unwrap()[0]
            .to_def_id();
        let payloads = LocalEnumPayloadTemplateDef::try_new(
            ids.owner.to_def_id(),
            Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
            Arc::from([LocalEnumPayloadVariantTemplateDef::new(
                variant_id,
                names[0],
                LocalEnumPayloadTemplateShape::Tuple(Arc::from([
                    LocalEnumTuplePayloadTemplate::new(
                        variant_id,
                        0,
                        crate::ty::TyInterner::new().intern(crate::ty::TyKind::Bool),
                    ),
                ])),
            )]),
        )
        .unwrap();
        let interner = crate::ty::TyInterner::new();
        let definition = LocalEnumAdtDef {
            payloads,
            repr: None,
            discr_ty: IntTy::I64,
            discriminants: Arc::from([]),
        };

        let foreign_package = ids.packages.intern("acme:foreign#1.0").unwrap();
        let foreign_paths =
            DefPathInterner::new(foreign_package, &ids.packages, &ids.symbols).unwrap();
        let foreign_owner = foreign_paths
            .intern(
                DefPath::new(
                    foreign_package,
                    vec![DisambiguatedDefPathData {
                        data: DefPathData::TypeNs(ids.symbols.intern("Foreign").unwrap()),
                        disambiguator: 0,
                    }],
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();
        let foreign_variant = foreign_paths
            .intern_variants(foreign_owner, &[(ids.symbols.intern("Value").unwrap(), 0)])
            .unwrap()[0]
            .to_def_id();
        assert_eq!(
            definition.value_type_of_variant(&interner, foreign_variant),
            Err(LocalEnumVariantTypeError::ForeignVariant(foreign_variant))
        );

        let LocalEnumPayloadTemplateDef {
            owner,
            type_params,
            variants,
        } = definition.payloads;
        let mut variants = variants.to_vec();
        let LocalEnumPayloadTemplateShape::Tuple(payloads) = variants[0].payload() else {
            panic!("Value has a tuple payload")
        };
        let malformed_payloads = Arc::from([LocalEnumTuplePayloadTemplate::new(
            variant_id,
            1,
            payloads[0].ty().clone(),
        )]);
        variants[0].payload = LocalEnumPayloadTemplateShape::Tuple(malformed_payloads);
        let malformed = LocalEnumAdtDef {
            payloads: LocalEnumPayloadTemplateDef {
                owner,
                type_params,
                variants: variants.into(),
            },
            repr: None,
            discr_ty: IntTy::I64,
            discriminants: Arc::from([]),
        };
        assert_eq!(
            malformed.value_type_of_variant(&interner, variant_id),
            Err(LocalEnumVariantTypeError::InvalidPayloadShape(variant_id)),
            "variant type construction revalidates tuple identity and order"
        );
    }

    #[test]
    fn local_enum_adt_requires_payload_binders_from_the_exact_hir_owner() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("State").unwrap();
        let type_name = ids.symbols.intern("T").unwrap();
        let variant_name = ids.symbols.intern("Value").unwrap();
        let variant_id = ids
            .paths
            .intern_variants(ids.owner, &[(variant_name, 0)])
            .unwrap()[0];
        let mut owner_type_components = ids
            .paths
            .resolve(ids.owner)
            .unwrap()
            .unwrap()
            .data()
            .to_vec();
        owner_type_components.push(DisambiguatedDefPathData {
            data: DefPathData::TypeParam(type_name),
            disambiguator: 0,
        });
        let exact_binder = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    owner_type_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();

        let other_type_name = ids.symbols.intern("Other").unwrap();
        let other_owner = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    vec![DisambiguatedDefPathData {
                        data: DefPathData::TypeNs(other_type_name),
                        disambiguator: 0,
                    }],
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();
        let mut other_binder_components = ids
            .paths
            .resolve(other_owner)
            .unwrap()
            .unwrap()
            .data()
            .to_vec();
        other_binder_components.push(DisambiguatedDefPathData {
            data: DefPathData::TypeParam(type_name),
            disambiguator: 0,
        });
        let same_package_other_binder = ids
            .paths
            .intern(
                DefPath::new(
                    ids.owner.package(),
                    other_binder_components,
                    &ids.packages,
                    &ids.symbols,
                )
                .unwrap(),
            )
            .unwrap();

        let variants = [unit_variant(
            ids.owner,
            variant_id,
            variant_name,
            1,
            None,
            false,
        )];
        let generic_parameters = [GenericParam {
            def_id: exact_binder,
            name: Name {
                sym: type_name,
                span: span(),
            },
            kind: GenericParamKind::Type { bounds: &[] },
        }];
        let item_span = span();
        let item = Item {
            owner_id: OwnerId { def_id: ids.owner },
            name: Name {
                sym: enum_name,
                span: item_span,
            },
            vis: Visibility::Pub,
            kind: ItemKind::Enum {
                generics: Generics {
                    params: &generic_parameters,
                    where_preds: &[],
                },
                variants: &variants,
            },
            span: item_span,
        };
        let hir = module_hir_owner(&item);
        let parameter = crate::ty::ParamTy {
            index: 0,
            name: type_name,
        };
        let template_for = |binder: LocalDefId| {
            LocalEnumPayloadTemplateDef::try_new(
                ids.owner.to_def_id(),
                Arc::from([(binder.to_def_id(), parameter)]),
                Arc::from([LocalEnumPayloadVariantTemplateDef::new(
                    variant_id.to_def_id(),
                    variant_name,
                    LocalEnumPayloadTemplateShape::Unit,
                )]),
            )
            .unwrap()
        };

        assert!(
            LocalEnumAdtDef::from_hir_owner(template_for(exact_binder), None, &hir,).is_ok(),
            "a matching source-order binder DefId composes successfully"
        );
        let error =
            LocalEnumAdtDef::from_hir_owner(template_for(same_package_other_binder), None, &hir)
                .expect_err("same-package binders from a different HIR owner are rejected");
        assert_eq!(
            error.reason(),
            super::LocalEnumAdtDefError::GenericParameterMismatch
        );
    }

    #[test]
    fn local_enum_adt_maps_and_retains_validated_repr_choices() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("State").unwrap();
        let ready = ids.symbols.intern("Ready").unwrap();
        let variant_id = ids.paths.intern_variants(ids.owner, &[(ready, 0)]).unwrap()[0];
        let variants = [unit_variant(ids.owner, variant_id, ready, 1, None, false)];
        let payloads = unit_payload_template(ids.owner, &variants);
        let item = module_enum_item(ids.owner, enum_name, &variants);
        let hir = module_hir_owner(&item);
        let cases = [
            (None, IntTy::I64),
            (Some(ReprAttr::C), IntTy::I32),
            (Some(ReprAttr::Integer(ReprInteger::I8)), IntTy::I8),
            (Some(ReprAttr::Integer(ReprInteger::I16)), IntTy::I16),
            (Some(ReprAttr::Integer(ReprInteger::I32)), IntTy::I32),
            (Some(ReprAttr::Integer(ReprInteger::I64)), IntTy::I64),
            (Some(ReprAttr::Integer(ReprInteger::U8)), IntTy::U8),
            (Some(ReprAttr::Integer(ReprInteger::U16)), IntTy::U16),
            (Some(ReprAttr::Integer(ReprInteger::U32)), IntTy::U32),
            (Some(ReprAttr::Integer(ReprInteger::U64)), IntTy::U64),
        ];

        for (repr, expected_discr_ty) in cases {
            let (definition, issues) =
                LocalEnumAdtDef::from_hir_owner(payloads.clone(), repr, &hir)
                    .expect("the exact HIR owner builds an enum definition");
            assert_eq!(definition.repr(), repr);
            assert_eq!(definition.discr_ty(), expected_discr_ty);
            assert_eq!(definition.discriminants().len(), 1);
            assert_eq!(
                definition.discriminants()[0].variant(),
                variant_id.to_def_id()
            );
            assert_eq!(definition.discriminants()[0].value(), 0);
            assert!(issues.is_empty());
        }

        let (c_definition, _) =
            LocalEnumAdtDef::from_hir_owner(payloads.clone(), Some(ReprAttr::C), &hir).unwrap();
        let (integer_definition, _) = LocalEnumAdtDef::from_hir_owner(
            payloads,
            Some(ReprAttr::Integer(ReprInteger::I32)),
            &hir,
        )
        .unwrap();
        assert_eq!(c_definition.discr_ty(), integer_definition.discr_ty());
        assert_ne!(c_definition.repr(), integer_definition.repr());
    }

    #[test]
    fn local_enum_adt_computes_unit_discriminants_in_source_order() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("State").unwrap();
        let names =
            ["First", "Next", "Reset", "AfterReset"].map(|name| ids.symbols.intern(name).unwrap());
        let variant_ids = ids
            .paths
            .intern_variants(ids.owner, &names.map(|name| (name, 0)))
            .unwrap();
        let variants = [
            unit_variant(ids.owner, variant_ids[0], names[0], 1, Some(5), true),
            unit_variant(ids.owner, variant_ids[1], names[1], 2, None, false),
            unit_variant(ids.owner, variant_ids[2], names[2], 3, Some(-2), true),
            unit_variant(ids.owner, variant_ids[3], names[3], 4, None, false),
        ];
        let payloads = unit_payload_template(ids.owner, &variants);
        let item = module_enum_item(ids.owner, enum_name, &variants);
        let hir = module_hir_owner(&item);

        let (definition, issues) = LocalEnumAdtDef::from_hir_owner(payloads, None, &hir).unwrap();

        assert!(issues.is_empty());
        assert_eq!(
            definition
                .discriminants()
                .iter()
                .map(|discriminant| discriminant.value())
                .collect::<Vec<_>>(),
            [5, 6, -2, -1]
        );
    }

    #[test]
    fn local_enum_adt_keeps_out_of_range_and_duplicate_values() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("State").unwrap();
        let names =
            ["TooLarge", "Repeated", "Negative"].map(|name| ids.symbols.intern(name).unwrap());
        let variant_ids = ids
            .paths
            .intern_variants(ids.owner, &names.map(|name| (name, 0)))
            .unwrap();
        let variants = [
            unit_variant(ids.owner, variant_ids[0], names[0], 1, Some(256), true),
            unit_variant(ids.owner, variant_ids[1], names[1], 2, Some(256), true),
            unit_variant(ids.owner, variant_ids[2], names[2], 3, Some(-1), true),
        ];
        let payloads = unit_payload_template(ids.owner, &variants);
        let item = module_enum_item(ids.owner, enum_name, &variants);
        let hir = module_hir_owner(&item);

        let (definition, issues) = LocalEnumAdtDef::from_hir_owner(
            payloads,
            Some(ReprAttr::Integer(ReprInteger::U8)),
            &hir,
        )
        .unwrap();

        assert_eq!(
            definition
                .discriminants()
                .iter()
                .map(|discriminant| (discriminant.variant(), discriminant.value()))
                .collect::<Vec<_>>(),
            [
                (variant_ids[0].to_def_id(), 256),
                (variant_ids[1].to_def_id(), 256),
                (variant_ids[2].to_def_id(), -1),
            ]
        );
        assert_eq!(
            issues
                .iter()
                .map(|issue| (issue.variant(), issue.value(), issue.kind()))
                .collect::<Vec<_>>(),
            [
                (
                    variant_ids[0].to_def_id(),
                    256,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    variant_ids[1].to_def_id(),
                    256,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    variant_ids[1].to_def_id(),
                    256,
                    LocalEnumDiscriminantIssueKind::Duplicate {
                        previous_variant: variant_ids[0].to_def_id(),
                    },
                ),
                (
                    variant_ids[2].to_def_id(),
                    -1,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
            ]
        );
    }

    #[test]
    fn local_enum_adt_rejects_reordered_variant_templates() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("State").unwrap();
        let names = ["First", "Second"].map(|name| ids.symbols.intern(name).unwrap());
        let variant_ids = ids
            .paths
            .intern_variants(ids.owner, &names.map(|name| (name, 0)))
            .unwrap();
        let variants = [
            unit_variant(ids.owner, variant_ids[0], names[0], 1, None, false),
            unit_variant(ids.owner, variant_ids[1], names[1], 2, None, false),
        ];
        let reordered_payloads = LocalEnumPayloadTemplateDef::try_new(
            ids.owner.to_def_id(),
            Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
            Arc::from([
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[1].to_def_id(),
                    names[1],
                    LocalEnumPayloadTemplateShape::Unit,
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[0].to_def_id(),
                    names[0],
                    LocalEnumPayloadTemplateShape::Unit,
                ),
            ]),
        )
        .unwrap();
        let item = module_enum_item(ids.owner, enum_name, &variants);
        let hir = module_hir_owner(&item);

        let error = LocalEnumAdtDef::from_hir_owner(reordered_payloads, None, &hir)
            .expect_err("the exact HIR variant order must match the payload template");

        assert_eq!(
            error.reason(),
            super::LocalEnumAdtDefError::VariantOrderMismatch
        );
    }

    #[test]
    fn local_enum_adt_uses_ordinals_for_data_enums_and_fails_closed_on_lost_values() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("Mixed").unwrap();
        let unit_name = ids.symbols.intern("Unit").unwrap();
        let value_name = ids.symbols.intern("Value").unwrap();
        let tail_name = ids.symbols.intern("Tail").unwrap();
        let field_name = ids.symbols.intern("payload").unwrap();
        let variant_ids = ids
            .paths
            .intern_variants(
                ids.owner,
                &[(unit_name, 0), (value_name, 0), (tail_name, 0)],
            )
            .unwrap();
        let field_id = ids
            .paths
            .intern_fields_for_parents(&[(variant_ids[1], field_name, 0)])
            .unwrap()[0];
        let field_span = span();
        let hir_ty = HirTy {
            hir_id: HirId {
                owner: OwnerId { def_id: ids.owner },
                local_id: ItemLocalId::from_raw(10),
            },
            kind: HirTyKind::Err,
            span: field_span,
        };
        let hir_field = sai_hir::items::FieldDef {
            hir_id: HirId {
                owner: OwnerId { def_id: ids.owner },
                local_id: ItemLocalId::from_raw(9),
            },
            def_id: field_id,
            vis: Visibility::Pub,
            name: Name {
                sym: field_name,
                span: field_span,
            },
            ty: &hir_ty,
        };
        let hir_fields = [hir_field];
        let variants = [
            unit_variant(ids.owner, variant_ids[0], unit_name, 1, Some(99), true),
            Variant {
                hir_id: HirId {
                    owner: OwnerId { def_id: ids.owner },
                    local_id: ItemLocalId::from_raw(2),
                },
                def_id: variant_ids[1],
                name: Name {
                    sym: value_name,
                    span: span(),
                },
                shape: VariantShape::Record(&hir_fields),
                discriminant: None,
                has_explicit_discriminant: true,
                span: span(),
                payload_span: Some(span()),
            },
            unit_variant(ids.owner, variant_ids[2], tail_name, 3, Some(-20), true),
        ];
        let semantic_types = crate::ty::TyInterner::new();
        let payload_ty = semantic_types.intern(crate::ty::TyKind::Int(crate::ty::IntTy::I32));
        let payloads = LocalEnumPayloadTemplateDef::try_new(
            ids.owner.to_def_id(),
            Arc::<[(sai_hir::DefId, crate::ty::ParamTy)]>::from([]),
            Arc::from([
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[0].to_def_id(),
                    unit_name,
                    LocalEnumPayloadTemplateShape::Unit,
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[1].to_def_id(),
                    value_name,
                    LocalEnumPayloadTemplateShape::Record(Arc::from([
                        LocalEnumRecordPayloadTemplate::new(
                            variant_ids[1].to_def_id(),
                            field_id.to_def_id(),
                            field_name,
                            Visibility::Pub,
                            payload_ty,
                        ),
                    ])),
                ),
                LocalEnumPayloadVariantTemplateDef::new(
                    variant_ids[2].to_def_id(),
                    tail_name,
                    LocalEnumPayloadTemplateShape::Unit,
                ),
            ]),
        )
        .unwrap();
        let item = module_enum_item(ids.owner, enum_name, &variants);
        let hir = module_hir_owner(&item);

        let (definition, issues) = LocalEnumAdtDef::from_hir_owner(payloads, None, &hir).unwrap();

        assert!(issues.is_empty());
        assert_eq!(
            definition
                .discriminants()
                .iter()
                .map(|discriminant| discriminant.value())
                .collect::<Vec<_>>(),
            [0, 1, 2],
            "explicit literals in a data enum do not replace declaration ordinals"
        );

        let missing_name = ids.symbols.intern("Missing").unwrap();
        let missing_id = ids
            .paths
            .intern_variants(ids.owner, &[(missing_name, 0)])
            .unwrap()[0];
        let missing = [unit_variant(
            ids.owner,
            missing_id,
            missing_name,
            1,
            None,
            true,
        )];
        let missing_payloads = unit_payload_template(ids.owner, &missing);
        let missing_item = module_enum_item(ids.owner, enum_name, &missing);
        let missing_hir = module_hir_owner(&missing_item);
        let error = LocalEnumAdtDef::from_hir_owner(missing_payloads, None, &missing_hir)
            .expect_err("a missing explicit value is not an implicit discriminant");
        assert_eq!(
            error.reason(),
            super::LocalEnumAdtDefError::MissingExplicitDiscriminant(missing_id.to_def_id())
        );

        let overflow_names = ["Max", "Overflow"].map(|name| ids.symbols.intern(name).unwrap());
        let overflow_ids = ids
            .paths
            .intern_variants(ids.owner, &overflow_names.map(|name| (name, 0)))
            .unwrap();
        let overflow = [
            unit_variant(
                ids.owner,
                overflow_ids[0],
                overflow_names[0],
                1,
                Some(i128::MAX),
                true,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[1],
                overflow_names[1],
                2,
                None,
                false,
            ),
        ];
        let overflow_payloads = unit_payload_template(ids.owner, &overflow);
        let overflow_item = module_enum_item(ids.owner, enum_name, &overflow);
        let overflow_hir = module_hir_owner(&overflow_item);
        let error = LocalEnumAdtDef::from_hir_owner(overflow_payloads, None, &overflow_hir)
            .expect_err("implicit arithmetic must not wrap or invent an i128 value");
        assert_eq!(
            error.reason(),
            super::LocalEnumAdtDefError::DiscriminantOverflow(overflow_ids[1].to_def_id())
        );
        assert_eq!(
            error.issues().len(),
            1,
            "diagnostics for earlier computed values remain available when later arithmetic fails"
        );
        assert_eq!(
            error.issues()[0].kind(),
            LocalEnumDiscriminantIssueKind::OutOfRange
        );
    }

    #[test]
    fn local_enum_adt_retains_partial_values_and_issues_after_unavailable_values() {
        let ids = test_ids();
        let enum_name = ids.symbols.intern("Recovery").unwrap();
        let missing_names = ["Lost", "Dependent", "Large", "Repeated", "After"]
            .map(|name| ids.symbols.intern(name).unwrap());
        let missing_ids = ids
            .paths
            .intern_variants(ids.owner, &missing_names.map(|name| (name, 0)))
            .unwrap();
        let missing_variants = [
            unit_variant(ids.owner, missing_ids[0], missing_names[0], 1, None, true),
            unit_variant(ids.owner, missing_ids[1], missing_names[1], 2, None, false),
            unit_variant(
                ids.owner,
                missing_ids[2],
                missing_names[2],
                3,
                Some(300),
                true,
            ),
            unit_variant(
                ids.owner,
                missing_ids[3],
                missing_names[3],
                4,
                Some(300),
                true,
            ),
            unit_variant(ids.owner, missing_ids[4], missing_names[4], 5, None, false),
        ];
        let missing_payloads = unit_payload_template(ids.owner, &missing_variants);
        let missing_item = module_enum_item(ids.owner, enum_name, &missing_variants);
        let missing_hir = module_hir_owner(&missing_item);
        let missing_error = LocalEnumAdtDef::from_hir_owner(
            missing_payloads,
            Some(ReprAttr::Integer(ReprInteger::U8)),
            &missing_hir,
        )
        .expect_err("the lost explicit value keeps the enum unsupported");

        assert_eq!(
            missing_error.failures(),
            &[LocalEnumAdtDefError::MissingExplicitDiscriminant(
                missing_ids[0].to_def_id()
            )]
        );
        assert_eq!(
            missing_error
                .discriminants()
                .iter()
                .map(|value| (value.variant(), value.value()))
                .collect::<Vec<_>>(),
            [
                (missing_ids[2].to_def_id(), 300),
                (missing_ids[3].to_def_id(), 300),
                (missing_ids[4].to_def_id(), 301),
            ],
            "unknown implicit successors stay absent until an explicit value resets the sequence"
        );
        assert_eq!(
            missing_error
                .issues()
                .iter()
                .map(|issue| (issue.variant(), issue.value(), issue.kind()))
                .collect::<Vec<_>>(),
            [
                (
                    missing_ids[2].to_def_id(),
                    300,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    missing_ids[3].to_def_id(),
                    300,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    missing_ids[3].to_def_id(),
                    300,
                    LocalEnumDiscriminantIssueKind::Duplicate {
                        previous_variant: missing_ids[2].to_def_id(),
                    },
                ),
                (
                    missing_ids[4].to_def_id(),
                    301,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
            ]
        );

        let overflow_names = [
            "Max",
            "Overflow",
            "Dependent",
            "RepeatedMax",
            "Large",
            "After",
        ]
        .map(|name| ids.symbols.intern(name).unwrap());
        let overflow_ids = ids
            .paths
            .intern_variants(ids.owner, &overflow_names.map(|name| (name, 0)))
            .unwrap();
        let overflow_variants = [
            unit_variant(
                ids.owner,
                overflow_ids[0],
                overflow_names[0],
                1,
                Some(i128::MAX),
                true,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[1],
                overflow_names[1],
                2,
                None,
                false,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[2],
                overflow_names[2],
                3,
                None,
                false,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[3],
                overflow_names[3],
                4,
                Some(i128::MAX),
                true,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[4],
                overflow_names[4],
                5,
                Some(300),
                true,
            ),
            unit_variant(
                ids.owner,
                overflow_ids[5],
                overflow_names[5],
                6,
                None,
                false,
            ),
        ];
        let overflow_payloads = unit_payload_template(ids.owner, &overflow_variants);
        let overflow_item = module_enum_item(ids.owner, enum_name, &overflow_variants);
        let overflow_hir = module_hir_owner(&overflow_item);
        let overflow_error = LocalEnumAdtDef::from_hir_owner(
            overflow_payloads,
            Some(ReprAttr::Integer(ReprInteger::U8)),
            &overflow_hir,
        )
        .expect_err("the overflow keeps the enum unsupported");

        assert_eq!(
            overflow_error.failures(),
            &[LocalEnumAdtDefError::DiscriminantOverflow(
                overflow_ids[1].to_def_id()
            )]
        );
        assert_eq!(
            overflow_error
                .discriminants()
                .iter()
                .map(|value| (value.variant(), value.value()))
                .collect::<Vec<_>>(),
            [
                (overflow_ids[0].to_def_id(), i128::MAX),
                (overflow_ids[3].to_def_id(), i128::MAX),
                (overflow_ids[4].to_def_id(), 300),
                (overflow_ids[5].to_def_id(), 301),
            ],
            "explicit values resume the sequence after an overflow"
        );
        assert_eq!(
            overflow_error
                .issues()
                .iter()
                .map(|issue| (issue.variant(), issue.value(), issue.kind()))
                .collect::<Vec<_>>(),
            [
                (
                    overflow_ids[0].to_def_id(),
                    i128::MAX,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    overflow_ids[3].to_def_id(),
                    i128::MAX,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    overflow_ids[3].to_def_id(),
                    i128::MAX,
                    LocalEnumDiscriminantIssueKind::Duplicate {
                        previous_variant: overflow_ids[0].to_def_id(),
                    },
                ),
                (
                    overflow_ids[4].to_def_id(),
                    300,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
                (
                    overflow_ids[5].to_def_id(),
                    301,
                    LocalEnumDiscriminantIssueKind::OutOfRange,
                ),
            ]
        );
    }
}
