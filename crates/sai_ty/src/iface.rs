use sai_diagnostics::Code;
use sai_hir::items::{HirOwner, InterfaceItemKind, OwnerNode};
use sai_hir::{OwnerId, Span};

/// One local interface-check finding with its original HIR source span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceCheckIssue {
    pub owner: OwnerId,
    pub code: Code,
    pub span: Span,
}

/// Runs the interface-item checks implemented by `sai_ty`.
///
/// The driver converts each returned source span into a snapshot-bound
/// diagnostic.
pub fn interface_item_check(owner: &HirOwner<'_>) -> Vec<InterfaceCheckIssue> {
    let OwnerNode::InterfaceItem(item) = owner.node else {
        return Vec::new();
    };
    let mut issues = Vec::new();
    if let Some(span) = item.generic_params_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            code: Code::GenericInterfaceItem,
            span,
        });
    }
    if let Some(span) = item.where_clause_span {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            code: Code::GenericInterfaceItem,
            span,
        });
    }
    if is_empty_wit_type(&item.kind) {
        issues.push(InterfaceCheckIssue {
            owner: item.owner_id,
            code: Code::WitEmptyType,
            span: item.name.span,
        });
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
