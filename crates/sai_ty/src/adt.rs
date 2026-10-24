//! The first semantic ADT representation.
//!
//! This module currently models only validated local flags declarations. It
//! does not provide the general `adt_def(DefId)` query contract: other ADT
//! kinds and general semantic field types are not represented yet.

use crate::iface::FlagsDef;
use sai_hir::{
    DefId, DefPathData, DefPathInterner, DisambiguatedDefPathData, LocalDefId, Symbol, Visibility,
};
use sai_intern::Interner;
use std::sync::Arc;

/// The ADT kinds represented by this initial semantic model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AdtKind {
    /// A validated local WIT flags type.
    Flags,
}

/// Primitive integer types used by the hidden flags storage field.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntTy {
    U8,
    U16,
    U32,
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
                variants: Arc::from([variant]),
            },
            flags: self,
        })
    }
}
