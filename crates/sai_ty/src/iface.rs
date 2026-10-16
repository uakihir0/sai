use sai_diagnostics::Code;
use sai_hir::items::{HirOwner, InterfaceItemKind, MemberRole, OwnerNode, Variant, VariantShape};
use sai_hir::{OwnerId, Span};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InterfaceCheckIssueKind {
    GenericItem,
    AliasRefinement,
    ExplicitDiscriminant,
    EmptyType,
    UnsupportedVariantPayload,
    InvalidFlagsEnum,
    InvalidResourceReceiver,
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
        }
    }
}

/// One local interface-check finding with its original HIR source span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceCheckIssue {
    pub owner: OwnerId,
    pub kind: InterfaceCheckIssueKind,
    pub span: Span,
}

/// Runs the interface-item checks implemented by `sai_ty`.
///
/// The driver converts each returned source span into a snapshot-bound
/// diagnostic.
pub fn interface_item_check(
    owner: &HirOwner<'_>,
    flags_marker_accepted: bool,
) -> Vec<InterfaceCheckIssue> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Vec::new();
    };
    let mut issues = Vec::new();
    if let Some(span) = item.generic_params_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::GenericItem,
            span,
        });
    }
    if let Some(span) = item.where_clause_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::GenericItem,
            span,
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
                });
            }
        }
    }
    if let InterfaceItemKind::Refined { clause, .. } = item.kind {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::AliasRefinement,
            span: clause,
        });
    }
    let flags_invalid = match item.kind {
        InterfaceItemKind::Enum { variants, .. } if flags_marker_accepted => {
            invalid_flags_enum_issue(item.owner_id, variants)
        }
        _ => None,
    };
    if let Some(issue) = flags_invalid {
        issues.push(issue);
    }
    if let InterfaceItemKind::Enum { variants, .. } = item.kind {
        for variant in variants {
            if variant.has_explicit_discriminant && !flags_marker_accepted {
                issues.push(InterfaceCheckIssue {
                    owner: item.owner_id,
                    kind: InterfaceCheckIssueKind::ExplicitDiscriminant,
                    span: variant.span,
                });
            }
        }
    }
    if is_empty_wit_type(&item.kind) {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::EmptyType,
            span: item.name.span,
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
                });
            }
        }
    }
    issues
}

fn invalid_flags_enum_issue(
    owner: OwnerId,
    variants: &[Variant<'_>],
) -> Option<InterfaceCheckIssue> {
    if variants.is_empty() {
        return None;
    }

    let payload_span = variants
        .iter()
        .find(|variant| !matches!(variant.shape, VariantShape::Unit))
        .map(|variant| variant.payload_span.unwrap_or(variant.span));
    let discriminant_span = variants
        .iter()
        .find(|variant| variant.has_explicit_discriminant)
        .map(|variant| variant.span);
    let span = payload_span
        .or(discriminant_span)
        .or_else(|| variants.get(32).map(|variant| variant.name.span))?;

    Some(InterfaceCheckIssue {
        owner,
        kind: InterfaceCheckIssueKind::InvalidFlagsEnum,
        span,
    })
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
    use super::{InterfaceCheckIssueKind, interface_item_check, is_empty_wit_type};
    use sai_hir::body::{FnParam, FnSig};
    use sai_hir::items::{
        HirOwner, InterfaceItem, InterfaceItemKind, MemberRole, OwnerNode, Variant, VariantShape,
    };
    use sai_hir::types::{Generics, Name, Path, PathRoot, Ty, TyKind};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, Mutability,
        OwnerId, Safety, Span, SpanCtxt,
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

    fn check_enum(
        ids: &TestIds,
        variants: &[Variant<'_>],
        flags_marker_accepted: bool,
    ) -> Vec<super::InterfaceCheckIssue> {
        let item = InterfaceItem {
            owner_id: ids.owner,
            interface: ids.owner.def_id,
            name: Name {
                sym: ids.symbol,
                span: span(0, 5),
            },
            kind: InterfaceItemKind::Enum {
                generics: EMPTY_GENERICS,
                variants,
            },
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
        interface_item_check(&owner, flags_marker_accepted)
    }

    fn check_resource_member(
        role: MemberRole,
        outer_borrow: Option<Mutability>,
        error_type: bool,
    ) -> Vec<super::InterfaceCheckIssue> {
        let (ids, _symbols) = test_ids();
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
        interface_item_check(&owner, false)
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
        let (ids, _symbols) = test_ids();
        let payload = span(4, 9);
        let with_payload_and_discriminant = [
            variant(&ids, 0, VariantShape::Tuple(&[]), true, Some(payload)),
            variant(&ids, 1, VariantShape::Unit, false, None),
        ];
        let issues = check_enum(&ids, &with_payload_and_discriminant, true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issues[0].span, payload);

        let mut with_discriminant_and_33_variants = (0..33)
            .map(|index| variant(&ids, index, VariantShape::Unit, index == 7, None))
            .collect::<Vec<_>>();
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issues[0].span, with_discriminant_and_33_variants[7].span);

        with_discriminant_and_33_variants[7].has_explicit_discriminant = false;
        with_discriminant_and_33_variants[7].discriminant = None;
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(
            issues[0].span,
            with_discriminant_and_33_variants[32].name.span
        );
    }

    #[test]
    fn valid_and_empty_flags_enums_use_the_expected_interface_checks() {
        let (ids, _symbols) = test_ids();
        for count in [1, 32] {
            let variants = (0..count)
                .map(|index| variant(&ids, index, VariantShape::Unit, false, None))
                .collect::<Vec<_>>();
            let issues = check_enum(&ids, &variants, true);
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
        }

        let issues = check_enum(&ids, &[], true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::EmptyType);
        assert_eq!(issues[0].span, span(0, 5));

        let explicit = [variant(&ids, 0, VariantShape::Unit, true, None)];
        let issues = check_enum(&ids, &explicit, false);
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
}
