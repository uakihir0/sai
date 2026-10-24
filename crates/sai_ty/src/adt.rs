//! The first semantic ADT representation.
//!
//! This module currently models validated local flags declarations and
//! local-interface enums whose variants carry no payload. It does not provide
//! the general `adt_def(DefId)` query contract: other ADT kinds and general
//! semantic field types are not represented yet.

use crate::iface::{FlagsDef, InterfaceItemCheck, raw_flags_marker_is_valid};
use sai_hir::item_tree::AttrName;
use sai_hir::items::{HirOwner, InterfaceItemKind, OwnerNode, Variant, VariantShape};
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
    use super::{AdtKind, IntTy, LocalUnitEnumAdtDef, LocalUnitEnumAdtDefError};
    use sai_hir::item_tree::{AttrName, RawAttr, RawAttrArg};
    use sai_hir::items::{
        HirOwner, InterfaceItem, InterfaceItemKind, OwnerNode, Variant, VariantShape,
    };
    use sai_hir::types::{Generics, Name};
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
}
