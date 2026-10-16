use crate::expressions::Expr;
use crate::source_map::HirSourceId;
use crate::types::{EffectRef, Name, Provenance, Ty};
use crate::{BindingId, HirId, OwnerId, Safety, Span};
use std::hash::{Hash, Hasher};

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

#[derive(Clone, Copy, Debug)]
pub struct FnParam<'hir> {
    pub hir_id: HirId,
    pub name: Option<Name>,
    pub is_self: bool,
    pub mutable: bool,
    pub ty: &'hir Ty<'hir>,
    pub span: Span,
}

impl PartialEq for FnParam<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id
            && self.name == other.name
            && self.is_self == other.is_self
            && self.mutable == other.mutable
            && self.ty == other.ty
    }
}

impl Eq for FnParam<'_> {}

impl Hash for FnParam<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.name.hash(state);
        self.is_self.hash(state);
        self.mutable.hash(state);
        self.ty.hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Body<'hir> {
    pub owner: OwnerId,
    pub source_id: HirSourceId,
    pub params: &'hir [BodyParam],
    pub value: &'hir Expr<'hir>,
    pub contracts: &'hir [Contract<'hir>],
    pub invariant_preds: &'hir [&'hir Expr<'hir>],
}

impl PartialEq for Body<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.params == other.params
            && self.value == other.value
            && self.contracts == other.contracts
            && self.invariant_preds == other.invariant_preds
    }
}

impl Eq for Body<'_> {}

impl Hash for Body<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
        self.params.hash(state);
        self.value.hash(state);
        self.contracts.hash(state);
        self.invariant_preds.hash(state);
    }
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
            span: span(2),
        };
        let shifted_span_param = FnParam {
            span: span(15),
            ..param
        };
        assert_eq!(
            param, shifted_span_param,
            "source ranges do not change function-signature equality"
        );
        let mut original_param_hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&param, &mut original_param_hasher);
        let mut shifted_param_hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&shifted_span_param, &mut shifted_param_hasher);
        assert_eq!(
            original_param_hasher.finish(),
            shifted_param_hasher.finish(),
            "source ranges do not change function-signature hashing"
        );
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
            source_id: HirSourceId::default(),
            params: &body_params,
            value: &expr,
            contracts: &contracts,
            invariant_preds: &predicates,
        };
        let relowered_body = Body {
            source_id: HirSourceId::fresh(),
            ..body
        };

        assert_eq!(sig.params[0].name, Some(name));
        assert_eq!(sig.requires, &[hir_id]);
        assert_eq!(body.owner, hir_id.owner);
        assert_eq!(body.params[0].binding, BindingId::default());
        assert_eq!(body.contracts[0].kind, ContractKind::Requires);
        assert_eq!(body.invariant_preds, &[&expr]);
        assert_eq!(body, relowered_body);
        let mut body_hasher = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut body_hasher);
        let mut relowered_body_hasher = std::collections::hash_map::DefaultHasher::new();
        relowered_body.hash(&mut relowered_body_hasher);
        assert_eq!(body_hasher.finish(), relowered_body_hasher.finish());

        let other_hir_id = test_hir_id("g");
        assert_ne!(body.owner, other_hir_id.owner);
    }
}
