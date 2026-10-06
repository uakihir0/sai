use crate::expressions::Expr;
use crate::types::{EffectRef, Name, Provenance, Ty};
use crate::{BindingId, HirId, OwnerId, Safety};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FnSig<'hir> {
    pub safety: Safety,
    pub is_async: bool,
    pub params: &'hir [FnParam<'hir>],
    pub ret: Option<&'hir Ty<'hir>>,
    pub borrows: Option<&'hir [Provenance<'hir>]>,
    pub effects: Option<&'hir [EffectRef<'hir>]>,
    pub requires: &'hir [HirId],
    pub ensures: &'hir [HirId],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FnParam<'hir> {
    pub hir_id: HirId,
    pub name: Option<Name>,
    pub is_self: bool,
    pub mutable: bool,
    pub ty: &'hir Ty<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Body<'hir> {
    pub owner: OwnerId,
    pub params: &'hir [BodyParam],
    pub value: &'hir Expr<'hir>,
    pub contracts: &'hir [Contract<'hir>],
    pub invariant_preds: &'hir [&'hir Expr<'hir>],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyParam {
    pub hir_id: HirId,
    pub binding: BindingId,
    pub name: Option<Name>,
    pub mutable: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Contract<'hir> {
    pub hir_id: HirId,
    pub kind: ContractKind,
    pub expr: &'hir Expr<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContractKind {
    Requires,
    Ensures,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expressions::ExprKind;
    use crate::literals::Lit;
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId, PackageInterner,
        Span, SpanCtxt,
    };
    use sai_intern::{Interner, ItemLocalId};
    use sai_vfs::{FileId, TextRange, TextSize};

    fn test_hir_id(name: &str) -> HirId {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("path interner");
        let symbol = symbols.intern(name).expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("definition path");
        HirId {
            owner: OwnerId {
                def_id: paths.intern(path).expect("definition interns"),
            },
            local_id: ItemLocalId::default(),
        }
    }

    fn span(offset: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::empty(TextSize::new(offset)),
            ctxt: SpanCtxt::Root,
        }
    }

    #[test]
    fn body_contracts_and_function_signatures_keep_owner_and_binding_ids() {
        let hir_id = test_hir_id("f");
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("x").expect("symbol interns"),
            span: span(1),
        };
        let ty = Ty {
            hir_id,
            kind: crate::types::TyKind::Infer,
            span: span(4),
        };
        let param = FnParam {
            hir_id,
            name: Some(name),
            is_self: false,
            mutable: true,
            ty: &ty,
        };
        let params = [param];
        let sig = FnSig {
            safety: Safety::Unsafe,
            is_async: true,
            params: &params,
            ret: Some(&ty),
            borrows: None,
            effects: None,
            requires: &[hir_id],
            ensures: &[],
        };
        let body_param = BodyParam {
            hir_id,
            binding: BindingId::default(),
            name: Some(name),
            mutable: true,
        };
        let body_params = [body_param];
        let expr = Expr {
            hir_id,
            kind: ExprKind::Lit(Lit::Bool(true)),
            span: span(8),
        };
        let contract = Contract {
            hir_id,
            kind: ContractKind::Requires,
            expr: &expr,
        };
        let contracts = [contract];
        let predicates = [&expr];
        let body = Body {
            owner: hir_id.owner,
            params: &body_params,
            value: &expr,
            contracts: &contracts,
            invariant_preds: &predicates,
        };

        assert_eq!(sig.params[0].name, Some(name));
        assert_eq!(sig.requires, &[hir_id]);
        assert_eq!(body.owner, hir_id.owner);
        assert_eq!(body.params[0].binding, BindingId::default());
        assert_eq!(body.contracts[0].kind, ContractKind::Requires);
        assert_eq!(body.invariant_preds, &[&expr]);

        let other_hir_id = test_hir_id("g");
        assert_ne!(body.owner, other_hir_id.owner);
    }
}
