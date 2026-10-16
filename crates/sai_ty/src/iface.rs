use sai_diagnostics::Code;
use sai_hir::items::{HirOwner, InterfaceItemKind, OwnerNode, VariantShape};
use sai_hir::{OwnerId, Span};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InterfaceCheckIssueKind {
    GenericItem,
    AliasRefinement,
    ExplicitDiscriminant,
    EmptyType,
    UnsupportedVariantPayload,
}

impl InterfaceCheckIssueKind {
    pub const fn code(self) -> Code {
        match self {
            Self::GenericItem => Code::GenericInterfaceItem,
            Self::AliasRefinement | Self::UnsupportedVariantPayload => Code::WitIncompatibleType,
            Self::ExplicitDiscriminant => Code::WitExplicitDiscriminant,
            Self::EmptyType => Code::WitEmptyType,
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
        }
    }

    pub const fn primary_label(self) -> &'static str {
        match self {
            Self::GenericItem => "generic interface item",
            Self::AliasRefinement => "unsupported refinement clause",
            Self::ExplicitDiscriminant => "explicit WIT discriminant",
            Self::EmptyType => "empty WIT interface type",
            Self::UnsupportedVariantPayload => "unsupported WIT variant payload",
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
    if let InterfaceItemKind::Refined { clause, .. } = item.kind {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            kind: InterfaceCheckIssueKind::AliasRefinement,
            span: clause,
        });
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

fn is_empty_wit_type(kind: &InterfaceItemKind<'_>) -> bool {
    match kind {
        InterfaceItemKind::Struct { fields, .. } => fields.is_empty(),
        InterfaceItemKind::Enum { variants, .. } => variants.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_empty_wit_type;
    use sai_hir::items::InterfaceItemKind;
    use sai_hir::types::Generics;

    const EMPTY_GENERICS: Generics<'static> = Generics {
        params: &[],
        where_preds: &[],
    };

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
}
