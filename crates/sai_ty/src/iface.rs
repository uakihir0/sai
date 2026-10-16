use sai_diagnostics::Code;
use sai_hir::items::{HirOwner, InterfaceItemKind, MemberRole, OwnerNode, Variant, VariantShape};
use sai_hir::types::Name;
use sai_hir::{OwnerId, Span};
use sai_intern::{Interner, LookupError};
use sai_resolve::{sai_identifier_to_wit_name, suggested_representable_name};

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
}

impl InterfaceCheckIssue {
    pub fn message(&self) -> String {
        match (&self.kind, &self.name) {
            (InterfaceCheckIssueKind::WitNameNotRepresentable, Some(name)) => {
                format!("SAI name `{name}` cannot be represented as a WIT name")
            }
            (InterfaceCheckIssueKind::WitNameNotRepresentable, None) => {
                "unnamed interface parameters cannot be represented as WIT names".to_owned()
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
) -> Result<Vec<InterfaceCheckIssue>, InterfaceCheckError> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Ok(Vec::new());
    };
    let mut issues = Vec::new();
    push_name_issue(&mut issues, item.owner_id, item.name, symbols)?;
    match item.kind {
        InterfaceItemKind::Struct { fields, .. } => {
            for field in fields {
                push_name_issue(&mut issues, item.owner_id, field.name, symbols)?;
            }
        }
        InterfaceItemKind::Enum { variants, .. } => {
            for variant in variants {
                push_name_issue(&mut issues, item.owner_id, variant.name, symbols)?;
            }
        }
        InterfaceItemKind::Fn { sig, .. } | InterfaceItemKind::ResourceMember { sig, .. } => {
            for param in sig.params {
                if param.is_self {
                    continue;
                }
                if let Some(name) = param.name {
                    push_name_issue(&mut issues, item.owner_id, name, symbols)?;
                } else {
                    issues.push(InterfaceCheckIssue {
                        owner: item.owner_id,
                        kind: InterfaceCheckIssueKind::WitNameNotRepresentable,
                        span: param.span,
                        name: None,
                        suggestion: None,
                    });
                }
            }
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
        });
    }
    if let Some(span) = item.where_clause_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::GenericItem,
            span,
            name: None,
            suggestion: None,
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
                    name: None,
                    suggestion: None,
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
                });
            }
        }
    }
    Ok(issues)
}

fn push_name_issue(
    issues: &mut Vec<InterfaceCheckIssue>,
    owner: OwnerId,
    name: Name,
    symbols: &Interner,
) -> Result<(), InterfaceCheckError> {
    if name.is_error() {
        return Ok(());
    }
    let source_name = symbols
        .resolve(name.sym)
        .map_err(InterfaceCheckError::SymbolLookup)?;
    if sai_identifier_to_wit_name(&source_name).is_some() {
        return Ok(());
    }
    let source_name = source_name.to_string();
    issues.push(InterfaceCheckIssue {
        owner,
        kind: InterfaceCheckIssueKind::WitNameNotRepresentable,
        span: name.span,
        suggestion: suggested_representable_name(&source_name),
        name: Some(source_name),
    });
    Ok(())
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
        name: None,
        suggestion: None,
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
        FieldDef, HirOwner, InterfaceItem, InterfaceItemKind, MemberRole, OwnerNode, Variant,
        VariantShape,
    };
    use sai_hir::types::{Generics, Name, Path, PathRoot, Ty, TyKind};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, Mutability,
        OwnerId, Safety, Span, SpanCtxt, Visibility,
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
    ) -> Result<Vec<super::InterfaceCheckIssue>, super::InterfaceCheckError> {
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

    fn check_enum(
        ids: &TestIds,
        variants: &[Variant<'_>],
        flags_marker_accepted: bool,
        symbols: &Interner,
    ) -> Vec<super::InterfaceCheckIssue> {
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
    ) -> Vec<super::InterfaceCheckIssue> {
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

        let mut with_discriminant_and_33_variants = (0..33)
            .map(|index| variant(&ids, index, VariantShape::Unit, index == 7, None))
            .collect::<Vec<_>>();
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(issues[0].span, with_discriminant_and_33_variants[7].span);

        with_discriminant_and_33_variants[7].has_explicit_discriminant = false;
        with_discriminant_and_33_variants[7].discriminant = None;
        let issues = check_enum(&ids, &with_discriminant_and_33_variants, true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::InvalidFlagsEnum);
        assert_eq!(
            issues[0].span,
            with_discriminant_and_33_variants[32].name.span
        );
    }

    #[test]
    fn valid_and_empty_flags_enums_use_the_expected_interface_checks() {
        let (ids, symbols) = test_ids();
        for count in [1, 32] {
            let variants = (0..count)
                .map(|index| variant(&ids, index, VariantShape::Unit, false, None))
                .collect::<Vec<_>>();
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
        }

        let issues = check_enum(&ids, &[], true, &symbols);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, InterfaceCheckIssueKind::EmptyType);
        assert_eq!(issues[0].span, span(0, 5));

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
