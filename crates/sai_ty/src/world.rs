//! WIT name checks for world function declarations.

use crate::boundary::{
    BoundaryTypeContext, BoundaryTypeOutcome, Incompatible, Position, boundary_type,
};
use crate::iface::{
    InterfaceCheckError, InterfaceCheckIssue, InterfaceCheckIssueKind, push_name_collisions,
    push_name_issue,
};
use sai_hir::OwnerId;
use sai_hir::items::{HirOwner, OwnerNode, WorldItemKind};
use sai_intern::Interner;

/// One incompatible parameter or result in a directly written world function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldFunctionBoundaryIssue {
    pub owner: OwnerId,
    pub position: Position,
    pub incompatible: Incompatible,
}

/// Checks the names that a directly written world function sends across the
/// WIT boundary.
///
/// Boundary type conversion is performed separately through
/// [`crate::boundary::boundary_type_with_references`].
pub fn world_function_name_check(
    owner: &HirOwner<'_>,
    symbols: &Interner,
) -> Result<Vec<InterfaceCheckIssue>, InterfaceCheckError> {
    let OwnerNode::WorldItem(item) = owner.node else {
        return Ok(Vec::new());
    };
    let (name, sig) = match item.kind {
        WorldItemKind::ImportFn { name, sig } | WorldItemKind::ExportFn { name, sig } => {
            (name, sig)
        }
        WorldItemKind::Import(_) | WorldItemKind::Export(_) | WorldItemKind::Include(_) => {
            return Ok(Vec::new());
        }
    };

    let mut issues = Vec::new();
    let _ = push_name_issue(&mut issues, item.owner_id, name, symbols)?;
    let mut parameter_names = Vec::with_capacity(sig.params.len());
    for parameter in sig.params {
        if let Some(name) = parameter.name {
            if let Some(definition) = push_name_issue(&mut issues, item.owner_id, name, symbols)? {
                parameter_names.push(definition);
            }
        } else {
            issues.push(InterfaceCheckIssue {
                owner: item.owner_id,
                kind: InterfaceCheckIssueKind::WitNameNotRepresentable,
                span: parameter.span,
                name: None,
                suggestion: None,
                related_span: None,
            });
        }
    }
    push_name_collisions(&mut issues, parameter_names);
    Ok(issues)
}

/// Checks parameter and result types of a directly written world function.
///
/// Type conversion remains identity-based and defers unresolved or incomplete
/// paths. The caller supplies original type-path resolutions and the
/// definition classifications available for this package snapshot.
pub fn world_function_boundary_check(
    owner: &HirOwner<'_>,
    context: &BoundaryTypeContext<'_>,
) -> Vec<WorldFunctionBoundaryIssue> {
    let OwnerNode::WorldItem(item) = owner.node else {
        return Vec::new();
    };
    let sig = match item.kind {
        WorldItemKind::ImportFn { sig, .. } | WorldItemKind::ExportFn { sig, .. } => sig,
        WorldItemKind::Import(_) | WorldItemKind::Export(_) | WorldItemKind::Include(_) => {
            return Vec::new();
        }
    };

    let mut issues = Vec::new();
    for parameter in sig.params {
        if let BoundaryTypeOutcome::Incompatible(incompatible) =
            boundary_type(context, item.owner_id, parameter.ty, Position::Param)
        {
            issues.push(WorldFunctionBoundaryIssue {
                owner: item.owner_id,
                position: Position::Param,
                incompatible,
            });
        }
    }
    if let Some(result) = sig.ret
        && let BoundaryTypeOutcome::Incompatible(incompatible) =
            boundary_type(context, item.owner_id, result, Position::Result)
    {
        issues.push(WorldFunctionBoundaryIssue {
            owner: item.owner_id,
            position: Position::Result,
            incompatible,
        });
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::{world_function_boundary_check, world_function_name_check};
    use crate::boundary::{BoundaryTypeContext, IncompatibleReason, Position};
    use crate::iface::InterfaceCheckIssueKind;
    use sai_hir::body::{FnParam, FnSig};
    use sai_hir::items::{HirOwner, OwnerNode, WorldItem, WorldItemKind};
    use sai_hir::types::Name;
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, ItemLocalId,
        OwnerId, Span, SpanCtxt,
    };
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_resolve::package_scopes::PathRes;
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::collections::HashMap;

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("test span is ordered"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn owner_and_symbols() -> (OwnerId, Interner) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = Interner::new();
        let name = symbols.intern("world").expect("world name interns");
        let paths =
            DefPathInterner::new(package, &packages, &symbols).expect("path interner initializes");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("world path is valid");
        let owner = OwnerId {
            def_id: paths.intern(path).expect("world path interns"),
        };
        (owner, symbols)
    }

    fn name(symbols: &Interner, value: &str, start: u32, end: u32) -> Name {
        Name {
            sym: symbols.intern(value).expect("test name interns"),
            span: span(start, end),
        }
    }

    #[test]
    fn checks_function_and_parameter_names_and_reports_later_parameter_collision() {
        let (owner, symbols) = owner_and_symbols();
        let local = owner.def_id;
        let function_name = name(&symbols, "fetch_user", 0, 10);
        let first_name = name(&symbols, "fetch_user", 11, 21);
        let second_name = name(&symbols, "FetchUser", 22, 31);
        let parameters = [
            FnParam {
                hir_id: HirId {
                    owner,
                    local_id: ItemLocalId::from_raw(1),
                },
                name: Some(first_name),
                is_self: false,
                mutable: false,
                ty: &sai_hir::types::Ty {
                    hir_id: HirId {
                        owner,
                        local_id: ItemLocalId::from_raw(2),
                    },
                    kind: sai_hir::types::TyKind::Infer,
                    span: span(32, 34),
                },
                span: span(11, 21),
            },
            FnParam {
                hir_id: HirId {
                    owner,
                    local_id: ItemLocalId::from_raw(3),
                },
                name: Some(second_name),
                is_self: false,
                mutable: false,
                ty: &sai_hir::types::Ty {
                    hir_id: HirId {
                        owner,
                        local_id: ItemLocalId::from_raw(4),
                    },
                    kind: sai_hir::types::TyKind::Infer,
                    span: span(35, 37),
                },
                span: span(22, 31),
            },
        ];
        let signature = FnSig {
            safety: sai_hir::Safety::Safe,
            is_async: false,
            params: &parameters,
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let item = WorldItem {
            owner_id: owner,
            world: local,
            kind: WorldItemKind::ImportFn {
                name: function_name,
                sig: signature,
            },
            span: span(0, 38),
        };
        let hir_owner = HirOwner {
            id: owner,
            node: OwnerNode::WorldItem(&item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        };

        let issues = world_function_name_check(&hir_owner, &symbols).expect("name lookup succeeds");
        assert_eq!(
            issues
                .iter()
                .map(|issue| (issue.kind, issue.span))
                .collect::<Vec<_>>(),
            [(InterfaceCheckIssueKind::WitNameCollision, span(22, 31))]
        );
        assert_eq!(issues[0].related_span, Some(span(11, 21)));
        assert_eq!(issues[0].name.as_deref(), Some("fetch-user"));
    }

    #[test]
    fn reports_unrepresentable_and_unnamed_world_parameters() {
        let (owner, symbols) = owner_and_symbols();
        let local = owner.def_id;
        let function_name = name(&symbols, "valid_fn", 0, 8);
        let invalid_name = name(&symbols, "userID", 9, 15);
        let parameters = [
            FnParam {
                hir_id: HirId {
                    owner,
                    local_id: ItemLocalId::from_raw(1),
                },
                name: Some(invalid_name),
                is_self: false,
                mutable: false,
                ty: &sai_hir::types::Ty {
                    hir_id: HirId {
                        owner,
                        local_id: ItemLocalId::from_raw(2),
                    },
                    kind: sai_hir::types::TyKind::Infer,
                    span: span(16, 18),
                },
                span: span(9, 15),
            },
            FnParam {
                hir_id: HirId {
                    owner,
                    local_id: ItemLocalId::from_raw(3),
                },
                name: None,
                is_self: false,
                mutable: false,
                ty: &sai_hir::types::Ty {
                    hir_id: HirId {
                        owner,
                        local_id: ItemLocalId::from_raw(4),
                    },
                    kind: sai_hir::types::TyKind::Infer,
                    span: span(19, 21),
                },
                span: span(19, 22),
            },
        ];
        let signature = FnSig {
            safety: sai_hir::Safety::Safe,
            is_async: false,
            params: &parameters,
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let item = WorldItem {
            owner_id: owner,
            world: local,
            kind: WorldItemKind::ExportFn {
                name: function_name,
                sig: signature,
            },
            span: span(0, 22),
        };
        let hir_owner = HirOwner {
            id: owner,
            node: OwnerNode::WorldItem(&item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        };

        let issues = world_function_name_check(&hir_owner, &symbols).expect("name lookup succeeds");
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .all(|issue| { issue.kind == InterfaceCheckIssueKind::WitNameNotRepresentable })
        );
        assert_eq!(issues[0].span, span(9, 15));
        assert_eq!(issues[0].name.as_deref(), Some("userID"));
        assert_eq!(issues[1].span, span(19, 22));
        assert_eq!(issues[1].name, None);
        assert_eq!(
            issues[1].message(),
            "unnamed parameters cannot be represented as WIT names"
        );
    }

    #[test]
    fn checks_world_function_parameter_and_result_positions() {
        let (owner, symbols) = owner_and_symbols();
        let function_name = name(&symbols, "transfer", 0, 8);
        let unit = sai_hir::types::Ty {
            hir_id: HirId {
                owner,
                local_id: ItemLocalId::from_raw(2),
            },
            kind: sai_hir::types::TyKind::Tuple(&[]),
            span: span(20, 22),
        };
        let parameters = [FnParam {
            hir_id: HirId {
                owner,
                local_id: ItemLocalId::from_raw(1),
            },
            name: Some(name(&symbols, "payload", 9, 16)),
            is_self: false,
            mutable: false,
            ty: &unit,
            span: span(9, 22),
        }];
        let signature = FnSig {
            safety: sai_hir::Safety::Safe,
            is_async: false,
            params: &parameters,
            ret: Some(&unit),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let item = WorldItem {
            owner_id: owner,
            world: owner.def_id,
            kind: WorldItemKind::ImportFn {
                name: function_name,
                sig: signature,
            },
            span: span(0, 22),
        };
        let hir_owner = HirOwner {
            id: owner,
            node: OwnerNode::WorldItem(&item),
            body: None,
            raw_attrs: &[],
            diagnostics: Vec::new(),
        };
        let paths = HashMap::<HirId, PathRes>::new();
        let canonical = HashMap::new();
        let definitions = HashMap::new();
        let aliases = HashMap::new();
        let context = BoundaryTypeContext {
            path_resolutions: &paths,
            canonical_types: &canonical,
            definitions: &definitions,
            aliases: &aliases,
            resource_containment: None,
            self_resource: None,
        };

        let issues = world_function_boundary_check(&hir_owner, &context);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].owner, owner);
        assert_eq!(issues[0].position, Position::Param);
        assert_eq!(issues[0].incompatible.span, span(20, 22));
        assert_eq!(
            issues[0].incompatible.reason,
            IncompatibleReason::UnitOutsideResult
        );
    }
}
