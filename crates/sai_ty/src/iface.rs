use std::sync::Arc;

use sai_diagnostics::Code;
use sai_hir::items::{HirOwner, InterfaceItemKind, MemberRole, OwnerNode, Variant, VariantShape};
use sai_hir::types::Name;
use sai_hir::{DefId, OwnerId, Span, Symbol};
use sai_intern::{Interner, LookupError};
use sai_resolve::{
    WitNameDefinition, sai_identifier_to_wit_name, suggested_representable_name,
    wit_name_collisions,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InterfaceCheckIssueKind {
    GenericItem,
    AliasRefinement,
    ExplicitDiscriminant,
    EmptyType,
    UnsupportedVariantPayload,
    InvalidFlagsEnum,
    InvalidResourceReceiver,
    WitNameNotRepresentable,
    WitNameCollision,
}

impl InterfaceCheckIssueKind {
    pub const fn code(self) -> Code {
        match self {
            Self::GenericItem => Code::GenericInterfaceItem,
            Self::AliasRefinement | Self::UnsupportedVariantPayload => Code::WitIncompatibleType,
            Self::ExplicitDiscriminant => Code::WitExplicitDiscriminant,
            Self::EmptyType => Code::WitEmptyType,
            Self::InvalidFlagsEnum => Code::InvalidFlagsEnum,
            Self::InvalidResourceReceiver => Code::WitIncompatibleType,
            Self::WitNameNotRepresentable => Code::WitNameNotRepresentable,
            Self::WitNameCollision => Code::WitNameCollision,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::GenericItem => {
                "WIT interface items cannot have generic parameters or a `where` clause"
            }
            Self::AliasRefinement => "WIT aliases cannot have refinement clauses",
            Self::ExplicitDiscriminant => "WIT variants cannot have explicit discriminants",
            Self::EmptyType => "WIT interface types must not be empty",
            Self::UnsupportedVariantPayload => {
                "WIT variants do not support record payloads or tuples with multiple fields"
            }
            Self::InvalidFlagsEnum => {
                "flags enums require unit variants without explicit discriminants and at most 32 flags"
            }
            Self::InvalidResourceReceiver => "resource methods must borrow their resource receiver",
            Self::WitNameNotRepresentable => "SAI name cannot be represented as a WIT name",
            Self::WitNameCollision => "multiple declarations map to the same WIT name",
        }
    }

    pub const fn primary_label(self) -> &'static str {
        match self {
            Self::GenericItem => "generic interface item",
            Self::AliasRefinement => "unsupported refinement clause",
            Self::ExplicitDiscriminant => "explicit WIT discriminant",
            Self::EmptyType => "empty WIT interface type",
            Self::UnsupportedVariantPayload => "unsupported WIT variant payload",
            Self::InvalidFlagsEnum => "invalid flags enum",
            Self::InvalidResourceReceiver => "invalid resource receiver",
            Self::WitNameNotRepresentable => "not representable as a WIT name",
            Self::WitNameCollision => "colliding WIT name",
        }
    }
}

/// One local interface-check finding with its original HIR source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceCheckIssue {
    pub owner: OwnerId,
    pub kind: InterfaceCheckIssueKind,
    pub span: Span,
    pub name: Option<String>,
    pub suggestion: Option<String>,
    pub related_span: Option<Span>,
}

/// Storage width selected for a validated local WIT flags declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FlagsRepr {
    U8,
    U16,
    U32,
}

/// The validated, declaration-ordered flags data for one local interface type.
///
/// This value does not allocate flag `DefId`s. Flag names remain constants of
/// the parent flags type, as specified by spec/13 §13.9.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlagsDef {
    adt: DefId,
    flags: Arc<[(Symbol, Span)]>,
    repr: FlagsRepr,
}

impl FlagsDef {
    /// The local type definition described by this validated flags descriptor.
    pub fn adt(&self) -> DefId {
        self.adt
    }

    /// Flag names and source spans in declaration order.
    pub fn flags(&self) -> &[(Symbol, Span)] {
        &self.flags
    }

    pub(crate) const fn repr_int_ty(&self) -> crate::adt::IntTy {
        match self.repr {
            FlagsRepr::U8 => crate::adt::IntTy::U8,
            FlagsRepr::U16 => crate::adt::IntTy::U16,
            FlagsRepr::U32 => crate::adt::IntTy::U32,
        }
    }

    /// Whether this descriptor still describes the supplied owner and enum.
    pub fn matches_enum(&self, adt: DefId, variants: &[Variant<'_>]) -> bool {
        self.adt == adt
            && self.flags.len() == variants.len()
            && variants
                .iter()
                .zip(self.flags.iter())
                .all(|(variant, (flag, span))| {
                    variant.name.sym == *flag
                        && variant.name.span == *span
                        && matches!(variant.shape, VariantShape::Unit)
                        && !variant.has_explicit_discriminant
                })
    }
}

/// Results of checking one local interface item.
///
/// A valid local `@flags enum` retains its validated descriptor here so later
/// interface lowering uses the same declaration order and source identities.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InterfaceItemCheck {
    issues: Vec<InterfaceCheckIssue>,
    flags_def: Option<FlagsDef>,
}

impl InterfaceItemCheck {
    /// Return the issues found while checking this item.
    pub fn issues(&self) -> &[InterfaceCheckIssue] {
        &self.issues
    }

    /// Return the validated flags descriptor, if this item is a valid flags enum.
    pub fn flags_def(&self) -> Option<&FlagsDef> {
        self.flags_def.as_ref()
    }

    /// Split the check result into its diagnostics and validated flags descriptor.
    pub fn into_parts(self) -> (Vec<InterfaceCheckIssue>, Option<FlagsDef>) {
        (self.issues, self.flags_def)
    }
}

impl std::ops::Deref for InterfaceItemCheck {
    type Target = [InterfaceCheckIssue];

    fn deref(&self) -> &Self::Target {
        &self.issues
    }
}

impl InterfaceCheckIssue {
    pub fn message(&self) -> String {
        match (&self.kind, &self.name) {
            (InterfaceCheckIssueKind::WitNameNotRepresentable, Some(name)) => {
                format!("SAI name `{name}` cannot be represented as a WIT name")
            }
            (InterfaceCheckIssueKind::WitNameNotRepresentable, None) => {
                "unnamed parameters cannot be represented as WIT names".to_owned()
            }
            (InterfaceCheckIssueKind::WitNameCollision, Some(name)) => {
                format!("multiple declarations map to WIT name `{name}`")
            }
            _ => self.kind.message().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceCheckError {
    SymbolLookup(LookupError),
}

/// Runs the interface-item checks implemented by `sai_ty`.
///
/// The driver converts each returned source span into a snapshot-bound
/// diagnostic.
pub fn interface_item_check(
    owner: &HirOwner<'_>,
    flags_marker_accepted: bool,
    symbols: &Interner,
) -> Result<InterfaceItemCheck, InterfaceCheckError> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Ok(InterfaceItemCheck::default());
    };
    let mut issues = Vec::new();
    let mut flags_def = None;
    push_name_issue(&mut issues, item.owner_id, item.name, symbols)?;
    match item.kind {
        InterfaceItemKind::Struct { fields, .. } => {
            let mut field_names = Vec::with_capacity(fields.len());
            for field in fields {
                if let Some(definition) =
                    push_name_issue(&mut issues, item.owner_id, field.name, symbols)?
                {
                    field_names.push(definition);
                }
            }
            push_name_collisions(&mut issues, field_names);
        }
        InterfaceItemKind::Enum { variants, .. } => {
            let mut case_names = Vec::with_capacity(variants.len());
            for variant in variants {
                if let Some(definition) =
                    push_name_issue(&mut issues, item.owner_id, variant.name, symbols)?
                {
                    case_names.push(definition);
                }
            }
            push_name_collisions(&mut issues, case_names);
        }
        InterfaceItemKind::Fn { sig, .. } | InterfaceItemKind::ResourceMember { sig, .. } => {
            let mut parameter_names = Vec::with_capacity(sig.params.len());
            for param in sig.params {
                if param.is_self {
                    continue;
                }
                if let Some(name) = param.name {
                    if let Some(definition) =
                        push_name_issue(&mut issues, item.owner_id, name, symbols)?
                    {
                        parameter_names.push(definition);
                    }
                } else {
                    issues.push(InterfaceCheckIssue {
                        owner: item.owner_id,
                        kind: InterfaceCheckIssueKind::WitNameNotRepresentable,
                        span: param.span,
                        name: None,
                        suggestion: None,
                        related_span: None,
                    });
                }
            }
            push_name_collisions(&mut issues, parameter_names);
        }
        InterfaceItemKind::Alias { .. }
        | InterfaceItemKind::Refined { .. }
        | InterfaceItemKind::Resource { .. } => {}
    }
    if let Some(span) = item.generic_params_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::GenericItem,
            span,
            name: None,
            suggestion: None,
            related_span: None,
        });
    }
    if let Some(span) = item.where_clause_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::GenericItem,
            span,
            name: None,
            suggestion: None,
            related_span: None,
        });
    }
    if let InterfaceItemKind::ResourceMember {
        role: MemberRole::Method,
        sig,
        ..
    } = item.kind
    {
        if let Some(receiver) = sig.params.first().filter(|param| param.is_self) {
            if !matches!(
                receiver.ty.kind,
                sai_hir::types::TyKind::Borrow(_, _) | sai_hir::types::TyKind::Err
            ) {
                issues.push(InterfaceCheckIssue {
                    owner: item.owner_id,
                    kind: InterfaceCheckIssueKind::InvalidResourceReceiver,
                    span: receiver.ty.span,
                    name: None,
                    suggestion: None,
                    related_span: None,
                });
            }
        }
    }
    if let InterfaceItemKind::Refined { clause, .. } = item.kind {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::AliasRefinement,
            span: clause,
            name: None,
            suggestion: None,
            related_span: None,
        });
    }
    if let InterfaceItemKind::Enum { variants, .. } = item.kind
        && flags_marker_accepted
    {
        match build_flags_def(item.owner_id.def_id.to_def_id(), item.owner_id, variants) {
            Ok(definition) => flags_def = definition,
            Err(issue) => issues.push(issue),
        }
    }
    if let InterfaceItemKind::Enum { variants, .. } = item.kind {
        for variant in variants {
            if variant.has_explicit_discriminant && !flags_marker_accepted {
                issues.push(InterfaceCheckIssue {
                    owner: item.owner_id,
                    kind: InterfaceCheckIssueKind::ExplicitDiscriminant,
                    span: variant.span,
                    name: None,
                    suggestion: None,
                    related_span: None,
                });
            }
        }
    }
    if is_empty_wit_type(&item.kind) {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::EmptyType,
            span: item.name.span,
            name: None,
            suggestion: None,
            related_span: None,
        });
    }
    if let InterfaceItemKind::Enum { variants, .. } = item.kind {
        for variant in variants {
            let has_unsupported_payload = match variant.shape {
                VariantShape::Tuple(types) => types.len() >= 2,
                VariantShape::Record(_) => true,
                VariantShape::Unit => false,
            };
            if has_unsupported_payload {
                issues.push(InterfaceCheckIssue {
                    owner: item.owner_id,
                    kind: InterfaceCheckIssueKind::UnsupportedVariantPayload,
                    span: variant.payload_span.unwrap_or(variant.span),
                    name: None,
                    suggestion: None,
                    related_span: None,
                });
            }
        }
    }
    Ok(InterfaceItemCheck { issues, flags_def })
}

pub(super) fn push_name_issue(
    issues: &mut Vec<InterfaceCheckIssue>,
    owner: OwnerId,
    name: Name,
    symbols: &Interner,
) -> Result<Option<WitNameDefinition>, InterfaceCheckError> {
    if name.is_error() {
        return Ok(None);
    }
    let source_name = symbols
        .resolve(name.sym)
        .map_err(InterfaceCheckError::SymbolLookup)?;
    let source_name = source_name.to_string();
    if sai_identifier_to_wit_name(&source_name).is_none() {
        issues.push(InterfaceCheckIssue {
            owner,
            kind: InterfaceCheckIssueKind::WitNameNotRepresentable,
            span: name.span,
            suggestion: suggested_representable_name(&source_name),
            name: Some(source_name.clone()),
            related_span: None,
        });
    }
    Ok(Some(WitNameDefinition {
        source_name,
        owner,
        span: name.span,
    }))
}

pub(super) fn push_name_collisions(
    issues: &mut Vec<InterfaceCheckIssue>,
    definitions: impl IntoIterator<Item = WitNameDefinition>,
) {
    for collision in wit_name_collisions(definitions) {
        issues.push(InterfaceCheckIssue {
            owner: collision.later_owner,
            kind: InterfaceCheckIssueKind::WitNameCollision,
            span: collision.later_span,
            name: Some(collision.wit_name),
            suggestion: None,
            related_span: Some(collision.earlier_span),
        });
    }
}

fn build_flags_def(
    adt: DefId,
    owner: OwnerId,
    variants: &[Variant<'_>],
) -> Result<Option<FlagsDef>, InterfaceCheckIssue> {
    if variants.is_empty() {
        return Ok(None);
    }

    let payload_span = variants
        .iter()
        .find(|variant| !matches!(variant.shape, VariantShape::Unit))
        .map(|variant| variant.payload_span.unwrap_or(variant.span));
    let discriminant_span = variants
        .iter()
        .find(|variant| variant.has_explicit_discriminant)
        .map(|variant| variant.span);
    let invalid_span = payload_span
        .or(discriminant_span)
        .or_else(|| variants.get(32).map(|variant| variant.name.span));
    if let Some(span) = invalid_span {
        return Err(InterfaceCheckIssue {
            owner,
            kind: InterfaceCheckIssueKind::InvalidFlagsEnum,
            span,
            name: None,
            suggestion: None,
            related_span: None,
        });
    }

    let repr = match variants.len() {
        1..=8 => FlagsRepr::U8,
        9..=16 => FlagsRepr::U16,
        17..=32 => FlagsRepr::U32,
        _ => unreachable!("the 33rd flag is rejected above"),
    };
    let flags = variants
        .iter()
        .map(|variant| (variant.name.sym, variant.name.span))
        .collect::<Vec<_>>()
        .into();
    Ok(Some(FlagsDef { adt, flags, repr }))
}

fn is_empty_wit_type(kind: &InterfaceItemKind<'_>) -> bool {
    match kind {
        InterfaceItemKind::Struct { fields, .. } => fields.is_empty(),
        InterfaceItemKind::Enum { variants, .. } => variants.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FlagsRepr, InterfaceCheckIssueKind, build_flags_def, interface_item_check,
        is_empty_wit_type,
    };
    use sai_hir::body::{FnParam, FnSig};
    use sai_hir::items::{
        FieldDef, HirOwner, InterfaceItem, InterfaceItemKind, MemberRole, OwnerNode, Variant,
        VariantShape,
    };
    use sai_hir::types::{Generics, Name, Path, PathRoot, Ty, TyKind};
    use sai_hir::{
        DefId, DefIndex, DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId,
        Mutability, OwnerId, Safety, Span, SpanCtxt, Visibility,
    };
    use sai_intern::{Interner, ItemLocalId, PackageInterner};
    use sai_vfs::{FileId, TextRange, TextSize};

    const EMPTY_GENERICS: Generics<'static> = Generics {
        params: &[],
        where_preds: &[],
    };

    struct TestIds {
        owner: OwnerId,
        hir: HirId,
        symbol: sai_hir::Symbol,
        paths: DefPathInterner,
        packages: PackageInterner,
    }

    fn test_ids() -> (TestIds, Interner) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:flags#1.0").expect("package interns");
        let symbols = Interner::new();
        let symbol = symbols.intern("Flags").expect("symbol interns");
        let paths =
            DefPathInterner::new(package, &packages, &symbols).expect("path interner builds");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("definition path builds");
        let def_id = paths.intern(path).expect("definition interns");
        let owner = OwnerId { def_id };
        (
            TestIds {
                owner,
                hir: HirId {
                    owner,
                    local_id: ItemLocalId::default(),
                },
                symbol,
                paths,
                packages,
            },
            symbols,
        )
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("valid test span"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn name(symbols: &Interner, value: &str, start: u32, end: u32) -> Name {
        Name {
            sym: symbols.intern(value).expect("test name interns"),
            span: span(start, end),
        }
    }

    fn check_item_kind<'hir>(
        ids: &TestIds,
        item_name: Name,
        kind: InterfaceItemKind<'hir>,
        flags_marker_accepted: bool,
        symbols: &Interner,
    ) -> Result<super::InterfaceItemCheck, super::InterfaceCheckError> {
        let item = InterfaceItem {
            owner_id: ids.owner,
            interface: ids.owner.def_id,
            name: item_name,
            kind,
            span: span(0, 1000),
            generic_params_span: None,
            where_clause_span: None,
        };
        let owner = HirOwner {
            id: ids.owner,
            node: OwnerNode::InterfaceItem(&item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        };
        super::interface_item_check(&owner, flags_marker_accepted, symbols)
    }

    fn variant(
        ids: &TestIds,
        index: u32,
        shape: VariantShape<'static>,
        has_explicit_discriminant: bool,
        payload_span: Option<Span>,
    ) -> Variant<'static> {
        let start = index * 10;
        Variant {
            hir_id: ids.hir,
            def_id: ids.owner.def_id,
            name: Name {
                sym: ids.symbol,
                span: span(start, start + 2),
            },
            shape,
            discriminant: has_explicit_discriminant.then_some(i128::from(index)),
            has_explicit_discriminant,
            span: span(start, start + 8),
            payload_span,
        }
    }

    fn flags_variants(ids: &TestIds, symbols: &Interner, count: u32) -> Vec<Variant<'static>> {
        (0..count)
            .map(|index| {
                let mut variant = variant(ids, index, VariantShape::Unit, false, None);
                variant.name.sym = symbols
                    .intern(&format!("Flag{index}"))
                    .expect("flag name interns");
                variant
            })
            .collect()
    }

    fn check_enum(
        ids: &TestIds,
        variants: &[Variant<'_>],
        flags_marker_accepted: bool,
        symbols: &Interner,
    ) -> super::InterfaceItemCheck {
        check_item_kind(
            ids,
            Name {
                sym: ids.symbol,
                span: span(0, 5),
            },
            InterfaceItemKind::Enum {
                generics: EMPTY_GENERICS,
                variants,
            },
            flags_marker_accepted,
            symbols,
        )
        .expect("HIR symbols resolve")
    }

    fn check_resource_member(
        role: MemberRole,
        outer_borrow: Option<Mutability>,
        error_type: bool,
    ) -> super::InterfaceItemCheck {
        let (ids, symbols) = test_ids();
        let inner_ty = Ty {
            hir_id: ids.hir,
            kind: TyKind::Path(Path {
                hir_id: ids.hir,
                root: PathRoot::SelfType,
                segments: &[],
                span: span(35, 39),
            }),
            span: span(35, 39),
        };
        let receiver_ty = Ty {
            hir_id: ids.hir,
            kind: if error_type {
                TyKind::Err
            } else if let Some(mutability) = outer_borrow {
                TyKind::Borrow(mutability, &inner_ty)
            } else {
                TyKind::Tuple(&[])
            },
            span: span(30, 34),
        };
        let receiver = FnParam {
            hir_id: ids.hir,
            name: Some(Name {
                sym: ids.symbol,
                span: span(20, 24),
            }),
            is_self: true,
            mutable: false,
            ty: &receiver_ty,
            span: span(20, 34),
        };
        let params = [receiver];
        let sig = FnSig {
            safety: Safety::Safe,
            is_async: false,
            params: &params,
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let item = InterfaceItem {
            owner_id: ids.owner,
            interface: ids.owner.def_id,
            name: Name {
                sym: ids.symbol,
                span: span(0, 5),
            },
            kind: InterfaceItemKind::ResourceMember {
                resource: ids.owner.def_id,
                role,
                sig,
                generics: EMPTY_GENERICS,
            },
            span: span(0, 100),
            generic_params_span: None,
            where_clause_span: None,
        };
        let owner = HirOwner {
            id: ids.owner,
            node: OwnerNode::InterfaceItem(&item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        };
        interface_item_check(&owner, false, &symbols).expect("HIR symbols resolve")
    }

    #[test]
    fn empty_interface_records_and_enums_are_incompatible_with_wit() {
        assert!(is_empty_wit_type(&InterfaceItemKind::Struct {
            generics: EMPTY_GENERICS,
            fields: &[],
        }));
        assert!(is_empty_wit_type(&InterfaceItemKind::Enum {
            generics: EMPTY_GENERICS,
            variants: &[],
        }));
    }

    #[test]
    fn non_type_interface_items_are_not_empty_types() {
        assert!(!is_empty_wit_type(&InterfaceItemKind::Resource {
            members: &[],
        }));
    }

    #[test]
    fn flags_enum_validation_uses_payload_discriminant_and_count_precedence() {
        let (ids, symbols) = test_ids();
        let payload = span(4, 9);
        let with_payload_and_discriminant = [
            variant(&ids, 0, VariantShape::Tuple(&[]), true, Some(payload)),
            variant(&ids, 1, VariantShape::Unit, false, None),
        ];
        let issues = check_enum(&ids, &with_payload_and_discriminant, true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issues[0].span, payload);
        assert!(issues.flags_def().is_none());

        let mut with_discriminant_and_33_variants = (0..33)
            .map(|index| variant(&ids, index, VariantShape::Unit, index == 7, None))
            .collect::<Vec<_>>();
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issues[0].span, with_discriminant_and_33_variants[7].span);
        assert!(issues.flags_def().is_none());

        with_discriminant_and_33_variants[7].has_explicit_discriminant = false;
        with_discriminant_and_33_variants[7].discriminant = None;
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(
            issues[0].span,
            with_discriminant_and_33_variants[32].name.span
        );
        assert!(issues.flags_def().is_none());
    }

    #[test]
    fn valid_and_empty_flags_enums_use_the_expected_interface_checks() {
        let (ids, symbols) = test_ids();
        for count in [1, 32] {
            let variants = flags_variants(&ids, &symbols, count);
            let issues = check_enum(&ids, &variants, true, &symbols);
            assert!(
                issues
                    .iter()
                    .all(|issue| issue.kind != InterfaceCheckIssueKind::InvalidFlagsEnum)
            );
            assert!(
                issues
                    .iter()
                    .all(|issue| issue.kind != InterfaceCheckIssueKind::EmptyType)
            );
            let flags_def = issues
                .flags_def()
                .expect("valid flags checks retain their descriptor");
            assert_eq!(flags_def.adt(), ids.owner.def_id.to_def_id());
            assert_eq!(flags_def.flags().len(), count as usize);
            for (index, (flag, flag_span)) in flags_def.flags().iter().enumerate() {
                assert_eq!(
                    symbols.resolve(*flag).expect("flag name resolves").as_ref(),
                    format!("Flag{index}")
                );
                assert_eq!(*flag_span, variants[index].name.span);
            }
        }

        let issues = check_enum(&ids, &[], true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::EmptyType);
        assert_eq!(issues[0].span, span(0, 5));
        assert!(issues.flags_def().is_none());

        let explicit = [variant(&ids, 0, VariantShape::Unit, true, None)];
        let issues = check_enum(&ids, &explicit, false, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].kind,
            InterfaceCheckIssueKind::ExplicitDiscriminant
        );
        assert_eq!(issues[0].span, explicit[0].span);
    }

    #[test]
    fn resource_methods_require_an_outer_borrow_and_suppress_error_types() {
        for mutability in [Mutability::Not, Mutability::Mut] {
            let issues = check_resource_member(MemberRole::Method, Some(mutability), false);
            assert!(issues.is_empty(), "borrowed self receivers are accepted");
        }

        let issues = check_resource_member(MemberRole::Method, None, false);
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].kind,
            InterfaceCheckIssueKind::InvalidResourceReceiver
        );
        assert_eq!(issues[0].span, span(30, 34));

        let issues = check_resource_member(MemberRole::Method, None, true);
        assert!(issues.is_empty(), "an existing type error is not cascaded");

        for role in [MemberRole::Constructor, MemberRole::Static] {
            let issues = check_resource_member(role, None, false);
            assert!(
                issues.is_empty(),
                "only methods have a resource receiver rule"
            );
        }
    }

    #[test]
    fn wit_name_check_reports_item_and_field_names_but_skips_error_names() {
        let (ids, symbols) = test_ids();
        let ty = Ty {
            hir_id: ids.hir,
            kind: TyKind::Err,
            span: span(50, 53),
        };
        let fields = [
            FieldDef {
                hir_id: ids.hir,
                def_id: ids.owner.def_id,
                vis: Visibility::Pub,
                name: name(&symbols, "User_Id", 10, 17),
                ty: &ty,
            },
            FieldDef {
                hir_id: ids.hir,
                def_id: ids.owner.def_id,
                vis: Visibility::Pub,
                name: Name {
                    sym: sai_hir::Symbol::ERROR,
                    span: span(20, 25),
                },
                ty: &ty,
            },
        ];
        let issues = check_item_kind(
            &ids,
            name(&symbols, "Record", 0, 6),
            InterfaceItemKind::Struct {
                generics: EMPTY_GENERICS,
                fields: &fields,
            },
            false,
            &symbols,
        )
        .expect("error names do not require an interner lookup");

        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].kind,
            InterfaceCheckIssueKind::WitNameNotRepresentable
        );
        assert_eq!(issues[0].span, span(10, 17));
        assert_eq!(issues[0].name.as_deref(), Some("User_Id"));
        assert_eq!(issues[0].suggestion.as_deref(), Some("UserId"));
        assert_eq!(
            issues[0].message(),
            "SAI name `User_Id` cannot be represented as a WIT name"
        );
    }

    #[test]
    fn flags_definition_preserves_bit_order_and_selects_the_smallest_repr() {
        let (ids, symbols) = test_ids();
        for (count, repr) in [
            (1, FlagsRepr::U8),
            (8, FlagsRepr::U8),
            (9, FlagsRepr::U16),
            (16, FlagsRepr::U16),
            (17, FlagsRepr::U32),
            (32, FlagsRepr::U32),
        ] {
            let variants = flags_variants(&ids, &symbols, count);
            let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
                .expect("valid flags declaration")
                .expect("nonempty flags declaration builds a definition");

            assert_eq!(flags.adt, ids.owner.def_id.to_def_id());
            assert!(flags.matches_enum(ids.owner.def_id.to_def_id(), &variants));
            assert_eq!(flags.repr, repr);
            assert_eq!(flags.flags.len(), count as usize);
            for (index, (flag, flag_span)) in flags.flags.iter().enumerate() {
                assert_eq!(
                    symbols.resolve(*flag).expect("flag name resolves").as_ref(),
                    format!("Flag{index}")
                );
                assert_eq!(*flag_span, variants[index].name.span);
            }
        }
    }

    #[test]
    fn flags_descriptor_builds_one_hidden_storage_variant_for_each_repr_boundary() {
        let (ids, symbols) = test_ids();
        let variant_name = symbols.intern("$flags").expect("synthetic variant interns");
        let bits_name = symbols.intern("$bits").expect("hidden field interns");

        for (count, repr) in [
            (1, crate::adt::IntTy::U8),
            (8, crate::adt::IntTy::U8),
            (9, crate::adt::IntTy::U16),
            (16, crate::adt::IntTy::U16),
            (17, crate::adt::IntTy::U32),
            (32, crate::adt::IntTy::U32),
        ] {
            let variants = flags_variants(&ids, &symbols, count);
            let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
                .expect("valid flags declaration")
                .expect("nonempty flags declaration builds a definition");
            let variant_id = ids
                .paths
                .intern_variants(ids.owner.def_id, &[(variant_name, 0)])
                .expect("synthetic variant path interns")[0];
            let field_id = ids
                .paths
                .intern_fields_for_parents(&[(variant_id, bits_name, 0)])
                .expect("hidden field path interns")[0];

            let local = flags
                .into_local_adt_def(
                    ids.owner.def_id,
                    variant_id,
                    field_id,
                    variant_name,
                    bits_name,
                    &ids.paths,
                    &symbols,
                )
                .expect("validated flags descriptor lowers");
            let adt = local.adt_def();
            assert_eq!(adt.def_id(), ids.owner.def_id.to_def_id());
            assert_eq!(adt.kind(), crate::adt::AdtKind::Flags);
            assert_eq!(adt.variants().len(), 1);
            let variant = &adt.variants()[0];
            assert_eq!(variant.def_id(), variant_id.to_def_id());
            assert_eq!(variant.name(), variant_name);
            assert_eq!(variant.discr(), 0);
            assert_eq!(variant.fields().len(), 1);
            let field = &variant.fields()[0];
            assert_eq!(field.def_id(), field_id.to_def_id());
            assert_eq!(field.name(), bits_name);
            assert_eq!(field.visibility(), Visibility::Private);
            assert_eq!(field.ty(), crate::adt::Ty::Int(repr));
            assert_eq!(local.flags_def().flags().len(), count as usize);
            assert_eq!(
                local
                    .flags_def()
                    .flags()
                    .iter()
                    .map(|(name, _)| symbols.resolve(*name).expect("flag name resolves"))
                    .collect::<Vec<_>>(),
                (0..count)
                    .map(|index| symbols
                        .intern(&format!("Flag{index}"))
                        .expect("flag symbol interns"))
                    .map(|symbol| symbols.resolve(symbol).expect("flag name resolves"))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn flags_adt_constructor_rejects_wrong_owner_and_aliased_child_ids() {
        let (ids, symbols) = test_ids();
        let variants = flags_variants(&ids, &symbols, 2);
        let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
            .expect("valid flags declaration")
            .expect("nonempty flags declaration builds a definition");
        let variant_name = symbols.intern("$flags").expect("synthetic variant interns");
        let bits_name = symbols.intern("$bits").expect("hidden field interns");
        let variant_id = ids
            .paths
            .intern_variants(ids.owner.def_id, &[(variant_name, 0)])
            .expect("synthetic variant path interns")[0];
        let field_id = ids
            .paths
            .intern_fields_for_parents(&[(variant_id, bits_name, 0)])
            .expect("hidden field path interns")[0];

        assert_eq!(
            flags.clone().into_local_adt_def(
                variant_id,
                variant_id,
                field_id,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols
            ),
            Err(crate::adt::LocalFlagsAdtDefError::OwnerMismatch)
        );
        assert_eq!(
            flags.into_local_adt_def(
                ids.owner.def_id,
                variant_id,
                variant_id,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionsAliasEachOther)
        );
    }

    #[test]
    fn flags_adt_constructor_rejects_unrelated_or_foreign_interner_paths() {
        let (ids, symbols) = test_ids();
        let variants = flags_variants(&ids, &symbols, 2);
        let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
            .expect("valid flags declaration")
            .expect("nonempty flags declaration builds a definition");
        let variant_name = symbols.intern("$flags").expect("synthetic variant interns");
        let bits_name = symbols.intern("$bits").expect("hidden field interns");

        let unrelated = symbols.intern("Unrelated").expect("unrelated name interns");
        let unrelated_path = DefPath::new(
            ids.paths.package(),
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(unrelated),
                disambiguator: 0,
            }],
            &ids.packages,
            &symbols,
        )
        .expect("unrelated owner path builds");
        let unrelated_owner = ids
            .paths
            .intern(unrelated_path)
            .expect("unrelated owner interns");
        let unrelated_variant = ids
            .paths
            .intern_variants(unrelated_owner, &[(variant_name, 0)])
            .expect("unrelated variant interns")[0];
        let unrelated_field = ids
            .paths
            .intern_fields_for_parents(&[(unrelated_variant, bits_name, 0)])
            .expect("unrelated field interns")[0];
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                unrelated_variant,
                unrelated_field,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionPathMismatch)
        );

        let same_package_paths = DefPathInterner::new(ids.paths.package(), &ids.packages, &symbols)
            .expect("second same-package interner builds");
        let local_variant = ids
            .paths
            .intern_variants(ids.owner.def_id, &[(variant_name, 0)])
            .expect("local variant interns")[0];
        let local_field = ids
            .paths
            .intern_fields_for_parents(&[(local_variant, bits_name, 0)])
            .expect("local field interns")[0];
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                local_variant,
                local_field,
                variant_name,
                bits_name,
                &same_package_paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::OwnerDefinitionPathMismatch)
        );
        let nonzero_variant = ids
            .paths
            .intern_variants(ids.owner.def_id, &[(variant_name, 1)])
            .expect("nonzero variant disambiguator interns")[0];
        let nonzero_variant_field = ids
            .paths
            .intern_fields_for_parents(&[(nonzero_variant, bits_name, 0)])
            .expect("field beneath nonzero variant interns")[0];
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                nonzero_variant,
                nonzero_variant_field,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionPathMismatch)
        );
        let nonzero_field = ids
            .paths
            .intern_fields_for_parents(&[(local_variant, bits_name, 1)])
            .expect("nonzero field disambiguator interns")[0];
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                local_variant,
                nonzero_field,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionPathMismatch)
        );
        let same_package_owner_path = ids
            .paths
            .resolve(ids.owner.def_id)
            .expect("original owner resolves")
            .expect("original owner is interned");
        let same_package_owner = same_package_paths
            .intern(same_package_owner_path)
            .expect("same-package owner interns separately");
        let same_package_variant = same_package_paths
            .intern_variants(same_package_owner, &[(variant_name, 0)])
            .expect("same-package variant interns")[0];
        let same_package_field = same_package_paths
            .intern_fields_for_parents(&[(same_package_variant, bits_name, 0)])
            .expect("same-package field interns")[0];
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                same_package_variant,
                same_package_field,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionPathMismatch)
        );

        let other_package = ids
            .packages
            .intern("acme:other-flags#1.0")
            .expect("other package interns");
        let other_paths = DefPathInterner::new(other_package, &ids.packages, &symbols)
            .expect("other-package interner builds");
        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                local_variant,
                local_field,
                variant_name,
                bits_name,
                &other_paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::OwnerDefinitionPathMismatch)
        );
        let other_name = symbols.intern("OtherFlags").expect("other name interns");
        let other_owner_path = DefPath::new(
            other_package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(other_name),
                disambiguator: 0,
            }],
            &ids.packages,
            &symbols,
        )
        .expect("other owner path builds");
        let other_owner = other_paths
            .intern(other_owner_path)
            .expect("other owner interns");
        let other_variant = other_paths
            .intern_variants(other_owner, &[(variant_name, 0)])
            .expect("other variant interns")[0];
        let other_field = other_paths
            .intern_fields_for_parents(&[(other_variant, bits_name, 0)])
            .expect("other field interns")[0];
        assert_eq!(
            flags.into_local_adt_def(
                ids.owner.def_id,
                other_variant,
                other_field,
                variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::ChildDefinitionPackageMismatch)
        );
    }

    #[test]
    fn flags_adt_constructor_rejects_non_reserved_synthetic_names() {
        let (ids, symbols) = test_ids();
        let variants = flags_variants(&ids, &symbols, 2);
        let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
            .expect("valid flags declaration")
            .expect("nonempty flags declaration builds a definition");
        let variant_name = symbols.intern("$flags").expect("synthetic variant interns");
        let bits_name = symbols.intern("$bits").expect("hidden field interns");
        let variant_id = ids
            .paths
            .intern_variants(ids.owner.def_id, &[(variant_name, 0)])
            .expect("synthetic variant path interns")[0];
        let field_id = ids
            .paths
            .intern_fields_for_parents(&[(variant_id, bits_name, 0)])
            .expect("hidden field path interns")[0];
        let visible_name = symbols.intern("bits").expect("visible name interns");
        let visible_variant_name = symbols
            .intern("flags")
            .expect("visible variant name interns");

        assert_eq!(
            flags.clone().into_local_adt_def(
                ids.owner.def_id,
                variant_id,
                field_id,
                variant_name,
                visible_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::InvalidSyntheticNames)
        );
        assert_eq!(
            flags.into_local_adt_def(
                ids.owner.def_id,
                variant_id,
                field_id,
                visible_variant_name,
                bits_name,
                &ids.paths,
                &symbols,
            ),
            Err(crate::adt::LocalFlagsAdtDefError::InvalidSyntheticNames)
        );
    }

    #[test]
    fn flags_descriptor_rejects_a_different_owner_or_enum_source() {
        let (ids, symbols) = test_ids();
        let variants = flags_variants(&ids, &symbols, 2);
        let flags = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &variants)
            .expect("valid flags declaration")
            .expect("nonempty flags declaration builds a definition");
        let foreign_package = PackageInterner::new()
            .intern("other:flags#1.0")
            .expect("foreign package interns");
        let foreign_adt = DefId::from_foreign(foreign_package, DefIndex::default());

        assert!(!flags.matches_enum(foreign_adt, &variants));

        let mut changed_variants = variants.clone();
        changed_variants[0].name.span = span(100, 102);
        assert!(!flags.matches_enum(ids.owner.def_id.to_def_id(), &changed_variants));

        let mut payload_variants = variants;
        payload_variants[0].shape = VariantShape::Tuple(&[]);
        assert!(!flags.matches_enum(ids.owner.def_id.to_def_id(), &payload_variants));
    }

    #[test]
    fn invalid_and_empty_flags_definitions_do_not_create_partial_flags_data() {
        let (ids, symbols) = test_ids();

        assert!(
            build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &[])
                .expect("empty flags are handled by WIT_EMPTY_TYPE")
                .is_none()
        );

        let payload_span = span(4, 9);
        let invalid_payload = [variant(
            &ids,
            0,
            VariantShape::Tuple(&[]),
            true,
            Some(payload_span),
        )];
        let issue = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &invalid_payload)
            .expect_err("payload-bearing flags are rejected");
        assert_eq!(issue.kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issue.span, payload_span);

        let invalid_count = flags_variants(&ids, &symbols, 33);
        let issue = build_flags_def(ids.owner.def_id.to_def_id(), ids.owner, &invalid_count)
            .expect_err("the 33rd flag is rejected");
        assert_eq!(issue.kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issue.span, invalid_count[32].name.span);
    }

    #[test]
    fn interface_local_name_collisions_retain_the_first_declaration_span() {
        let (ids, symbols) = test_ids();
        let ty = Ty {
            hir_id: ids.hir,
            kind: TyKind::Err,
            span: span(60, 63),
        };
        let fields = [
            FieldDef {
                hir_id: ids.hir,
                def_id: ids.owner.def_id,
                vis: Visibility::Pub,
                name: name(&symbols, "fetch_user", 10, 20),
                ty: &ty,
            },
            FieldDef {
                hir_id: ids.hir,
                def_id: ids.owner.def_id,
                vis: Visibility::Pub,
                name: name(&symbols, "FetchUser", 30, 39),
                ty: &ty,
            },
            FieldDef {
                hir_id: ids.hir,
                def_id: ids.owner.def_id,
                vis: Visibility::Pub,
                name: name(&symbols, "FetchUser", 40, 49),
                ty: &ty,
            },
        ];
        let issues = check_item_kind(
            &ids,
            name(&symbols, "Record", 0, 6),
            InterfaceItemKind::Struct {
                generics: EMPTY_GENERICS,
                fields: &fields,
            },
            false,
            &symbols,
        )
        .expect("HIR symbols resolve");

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::WitNameCollision);
        assert_eq!(issues[0].name.as_deref(), Some("fetch-user"));
        assert_eq!(issues[0].span, span(30, 39));
        assert_eq!(issues[0].related_span, Some(span(10, 20)));
        assert_eq!(
            issues[0].message(),
            "multiple declarations map to WIT name `fetch-user`"
        );
    }

    #[test]
    fn wit_name_check_does_not_report_fields_of_unsupported_record_variant_payloads() {
        let (ids, symbols) = test_ids();
        let ty = Ty {
            hir_id: ids.hir,
            kind: TyKind::Err,
            span: span(50, 53),
        };
        let fields = [FieldDef {
            hir_id: ids.hir,
            def_id: ids.owner.def_id,
            vis: Visibility::Pub,
            name: name(&symbols, "User_Id", 10, 17),
            ty: &ty,
        }];
        let variants = [Variant {
            shape: VariantShape::Record(&fields),
            ..variant(&ids, 0, VariantShape::Unit, false, None)
        }];
        let issues = check_enum(&ids, &variants, false, &symbols);

        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].kind,
            InterfaceCheckIssueKind::UnsupportedVariantPayload
        );
        assert_eq!(issues[0].span, variants[0].span);
    }

    #[test]
    fn wit_name_check_skips_error_item_names_before_resolving_them() {
        let (ids, symbols) = test_ids();
        let issues = check_item_kind(
            &ids,
            Name {
                sym: sai_hir::Symbol::ERROR,
                span: span(0, 5),
            },
            InterfaceItemKind::Resource { members: &[] },
            false,
            &symbols,
        )
        .expect("error item name does not require an interner lookup");

        assert!(issues.is_empty());
    }
}
