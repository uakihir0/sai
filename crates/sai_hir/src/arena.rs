use crate::body::{Body, BodyParam, Contract, FnParam};
use crate::expressions::{Arg, Arm, Block, Closure, ClosureParam, Expr, FmtPiece, Stmt};
use crate::items::{
    DerivedImpl, ExternItem, FieldDef, ImplItem, InterfaceItem, Item, TraitItem, Variant, WorldItem,
};
use crate::literals::Lit;
use crate::patterns::{Pat, PatArg};
use crate::types::{
    Bound, EffectRef, GenericArg, GenericArgs, GenericParam, Generics, Name, Path, PathSegment,
    Provenance, Ty, WherePred,
};
use sai_intern::Arena;

/// Stable storage for HIR nodes and their borrowed child lists.
pub struct HirArena<'hir> {
    tys: Arena<Ty<'hir>>,
    ty_lists: Arena<Ty<'hir>>,
    paths: Arena<Path<'hir>>,
    path_segments: Arena<PathSegment<'hir>>,
    path_segment_lists: Arena<PathSegment<'hir>>,
    generic_args: Arena<GenericArgs<'hir>>,
    generic_arg_lists: Arena<GenericArg<'hir>>,
    generics: Arena<Generics<'hir>>,
    generic_params: Arena<GenericParam<'hir>>,
    generic_param_lists: Arena<GenericParam<'hir>>,
    where_preds: Arena<WherePred<'hir>>,
    where_pred_lists: Arena<WherePred<'hir>>,
    bounds: Arena<Bound<'hir>>,
    bound_lists: Arena<Bound<'hir>>,
    effect_refs: Arena<EffectRef<'hir>>,
    effect_ref_lists: Arena<EffectRef<'hir>>,
    provenances: Arena<Provenance<'hir>>,
    provenance_lists: Arena<Provenance<'hir>>,
    name_lists: Arena<Name>,
    literals: Arena<Lit<'hir>>,
    byte_strings: Arena<u8>,
    pats: Arena<Pat<'hir>>,
    pat_lists: Arena<Pat<'hir>>,
    pat_args: Arena<PatArg<'hir>>,
    pat_arg_lists: Arena<PatArg<'hir>>,
    exprs: Arena<Expr<'hir>>,
    expr_lists: Arena<Expr<'hir>>,
    args: Arena<Arg<'hir>>,
    arg_lists: Arena<Arg<'hir>>,
    arms: Arena<Arm<'hir>>,
    arm_lists: Arena<Arm<'hir>>,
    closures: Arena<Closure<'hir>>,
    closure_params: Arena<ClosureParam<'hir>>,
    closure_param_lists: Arena<ClosureParam<'hir>>,
    fmt_piece_lists: Arena<FmtPiece<'hir>>,
    blocks: Arena<Block<'hir>>,
    stmts: Arena<Stmt<'hir>>,
    stmt_lists: Arena<Stmt<'hir>>,
    fn_params: Arena<FnParam<'hir>>,
    fn_param_lists: Arena<FnParam<'hir>>,
    body: Arena<Body<'hir>>,
    body_params: Arena<BodyParam>,
    body_param_lists: Arena<BodyParam>,
    contracts: Arena<Contract<'hir>>,
    contract_lists: Arena<Contract<'hir>>,
    hir_id_lists: Arena<crate::HirId>,
    owner_id_lists: Arena<crate::OwnerId>,
    expr_ref_lists: Arena<&'hir Expr<'hir>>,
    items: Arena<Item<'hir>>,
    item_lists: Arena<Item<'hir>>,
    trait_items: Arena<TraitItem<'hir>>,
    trait_item_lists: Arena<TraitItem<'hir>>,
    impl_items: Arena<ImplItem<'hir>>,
    impl_item_lists: Arena<ImplItem<'hir>>,
    extern_items: Arena<ExternItem<'hir>>,
    extern_item_lists: Arena<ExternItem<'hir>>,
    field_defs: Arena<FieldDef<'hir>>,
    field_def_lists: Arena<FieldDef<'hir>>,
    variants: Arena<Variant<'hir>>,
    variant_lists: Arena<Variant<'hir>>,
    derived_impls: Arena<DerivedImpl>,
    derived_impl_lists: Arena<DerivedImpl>,
    interface_items: Arena<InterfaceItem<'hir>>,
    interface_item_lists: Arena<InterfaceItem<'hir>>,
    world_items: Arena<WorldItem<'hir>>,
    world_item_lists: Arena<WorldItem<'hir>>,
}

impl<'hir> HirArena<'hir> {
    pub fn new() -> Self {
        Self {
            tys: Arena::new(),
            ty_lists: Arena::new(),
            paths: Arena::new(),
            path_segments: Arena::new(),
            path_segment_lists: Arena::new(),
            generic_args: Arena::new(),
            generic_arg_lists: Arena::new(),
            generics: Arena::new(),
            generic_params: Arena::new(),
            generic_param_lists: Arena::new(),
            where_preds: Arena::new(),
            where_pred_lists: Arena::new(),
            bounds: Arena::new(),
            bound_lists: Arena::new(),
            effect_refs: Arena::new(),
            effect_ref_lists: Arena::new(),
            provenances: Arena::new(),
            provenance_lists: Arena::new(),
            name_lists: Arena::new(),
            literals: Arena::new(),
            byte_strings: Arena::new(),
            pats: Arena::new(),
            pat_lists: Arena::new(),
            pat_args: Arena::new(),
            pat_arg_lists: Arena::new(),
            exprs: Arena::new(),
            expr_lists: Arena::new(),
            args: Arena::new(),
            arg_lists: Arena::new(),
            arms: Arena::new(),
            arm_lists: Arena::new(),
            closures: Arena::new(),
            closure_params: Arena::new(),
            closure_param_lists: Arena::new(),
            fmt_piece_lists: Arena::new(),
            blocks: Arena::new(),
            stmts: Arena::new(),
            stmt_lists: Arena::new(),
            fn_params: Arena::new(),
            fn_param_lists: Arena::new(),
            body: Arena::new(),
            body_params: Arena::new(),
            body_param_lists: Arena::new(),
            contracts: Arena::new(),
            contract_lists: Arena::new(),
            hir_id_lists: Arena::new(),
            owner_id_lists: Arena::new(),
            expr_ref_lists: Arena::new(),
            items: Arena::new(),
            item_lists: Arena::new(),
            trait_items: Arena::new(),
            trait_item_lists: Arena::new(),
            impl_items: Arena::new(),
            impl_item_lists: Arena::new(),
            extern_items: Arena::new(),
            extern_item_lists: Arena::new(),
            field_defs: Arena::new(),
            field_def_lists: Arena::new(),
            variants: Arena::new(),
            variant_lists: Arena::new(),
            derived_impls: Arena::new(),
            derived_impl_lists: Arena::new(),
            interface_items: Arena::new(),
            interface_item_lists: Arena::new(),
            world_items: Arena::new(),
            world_item_lists: Arena::new(),
        }
    }

    pub fn alloc_ty(&'hir self, ty: Ty<'hir>) -> &'hir Ty<'hir> {
        self.tys.alloc(ty)
    }

    pub fn alloc_tys(&'hir self, tys: &[Ty<'hir>]) -> &'hir [Ty<'hir>] {
        self.ty_lists.alloc_slice(tys)
    }

    pub fn alloc_path(&'hir self, path: Path<'hir>) -> &'hir Path<'hir> {
        self.paths.alloc(path)
    }

    pub fn alloc_path_segment(&'hir self, segment: PathSegment<'hir>) -> &'hir PathSegment<'hir> {
        self.path_segments.alloc(segment)
    }

    pub fn alloc_path_segments(
        &'hir self,
        segments: &[PathSegment<'hir>],
    ) -> &'hir [PathSegment<'hir>] {
        self.path_segment_lists.alloc_slice(segments)
    }

    pub fn alloc_generic_args(&'hir self, args: GenericArgs<'hir>) -> &'hir GenericArgs<'hir> {
        self.generic_args.alloc(args)
    }

    pub fn alloc_generic_args_list(
        &'hir self,
        args: &[GenericArg<'hir>],
    ) -> &'hir [GenericArg<'hir>] {
        self.generic_arg_lists.alloc_slice(args)
    }

    pub fn alloc_generics(&'hir self, generics: Generics<'hir>) -> &'hir Generics<'hir> {
        self.generics.alloc(generics)
    }

    pub fn alloc_generic_param(&'hir self, param: GenericParam<'hir>) -> &'hir GenericParam<'hir> {
        self.generic_params.alloc(param)
    }

    pub fn alloc_generic_params(
        &'hir self,
        params: &[GenericParam<'hir>],
    ) -> &'hir [GenericParam<'hir>] {
        self.generic_param_lists.alloc_slice(params)
    }

    pub fn alloc_where_pred(&'hir self, pred: WherePred<'hir>) -> &'hir WherePred<'hir> {
        self.where_preds.alloc(pred)
    }

    pub fn alloc_where_preds(&'hir self, preds: &[WherePred<'hir>]) -> &'hir [WherePred<'hir>] {
        self.where_pred_lists.alloc_slice(preds)
    }

    pub fn alloc_bound(&'hir self, bound: Bound<'hir>) -> &'hir Bound<'hir> {
        self.bounds.alloc(bound)
    }

    pub fn alloc_bounds(&'hir self, bounds: &[Bound<'hir>]) -> &'hir [Bound<'hir>] {
        self.bound_lists.alloc_slice(bounds)
    }

    pub fn alloc_effect_ref(&'hir self, effect: EffectRef<'hir>) -> &'hir EffectRef<'hir> {
        self.effect_refs.alloc(effect)
    }

    pub fn alloc_effect_refs(&'hir self, effects: &[EffectRef<'hir>]) -> &'hir [EffectRef<'hir>] {
        self.effect_ref_lists.alloc_slice(effects)
    }

    pub fn alloc_provenance(&'hir self, provenance: Provenance<'hir>) -> &'hir Provenance<'hir> {
        self.provenances.alloc(provenance)
    }

    pub fn alloc_provenances(
        &'hir self,
        provenances: &[Provenance<'hir>],
    ) -> &'hir [Provenance<'hir>] {
        self.provenance_lists.alloc_slice(provenances)
    }

    pub fn alloc_names(&'hir self, names: &[Name]) -> &'hir [Name] {
        self.name_lists.alloc_slice(names)
    }

    pub fn alloc_lit(&'hir self, literal: Lit<'hir>) -> &'hir Lit<'hir> {
        self.literals.alloc(literal)
    }

    pub fn alloc_bytes(&'hir self, bytes: &[u8]) -> &'hir [u8] {
        self.byte_strings.alloc_slice(bytes)
    }

    /// Copy UTF-8 text into arena-backed storage.
    pub fn alloc_str(&'hir self, text: &str) -> &'hir str {
        std::str::from_utf8(self.byte_strings.alloc_slice(text.as_bytes()))
            .expect("text copied from a valid UTF-8 string")
    }

    pub fn alloc_pat(&'hir self, pat: Pat<'hir>) -> &'hir Pat<'hir> {
        self.pats.alloc(pat)
    }

    pub fn alloc_pats(&'hir self, pats: &[Pat<'hir>]) -> &'hir [Pat<'hir>] {
        self.pat_lists.alloc_slice(pats)
    }

    pub fn alloc_pat_arg(&'hir self, arg: PatArg<'hir>) -> &'hir PatArg<'hir> {
        self.pat_args.alloc(arg)
    }

    pub fn alloc_pat_args(&'hir self, args: &[PatArg<'hir>]) -> &'hir [PatArg<'hir>] {
        self.pat_arg_lists.alloc_slice(args)
    }

    pub fn alloc_expr(&'hir self, expr: Expr<'hir>) -> &'hir Expr<'hir> {
        self.exprs.alloc(expr)
    }

    pub fn alloc_exprs(&'hir self, exprs: &[Expr<'hir>]) -> &'hir [Expr<'hir>] {
        self.expr_lists.alloc_slice(exprs)
    }

    pub fn alloc_arg(&'hir self, arg: Arg<'hir>) -> &'hir Arg<'hir> {
        self.args.alloc(arg)
    }

    pub fn alloc_args(&'hir self, args: &[Arg<'hir>]) -> &'hir [Arg<'hir>] {
        self.arg_lists.alloc_slice(args)
    }

    pub fn alloc_arm(&'hir self, arm: Arm<'hir>) -> &'hir Arm<'hir> {
        self.arms.alloc(arm)
    }

    pub fn alloc_arms(&'hir self, arms: &[Arm<'hir>]) -> &'hir [Arm<'hir>] {
        self.arm_lists.alloc_slice(arms)
    }

    pub fn alloc_closure(&'hir self, closure: Closure<'hir>) -> &'hir Closure<'hir> {
        self.closures.alloc(closure)
    }

    pub fn alloc_closure_param(&'hir self, param: ClosureParam<'hir>) -> &'hir ClosureParam<'hir> {
        self.closure_params.alloc(param)
    }

    pub fn alloc_closure_params(
        &'hir self,
        params: &[ClosureParam<'hir>],
    ) -> &'hir [ClosureParam<'hir>] {
        self.closure_param_lists.alloc_slice(params)
    }

    pub fn alloc_fmt_pieces(&'hir self, pieces: &[FmtPiece<'hir>]) -> &'hir [FmtPiece<'hir>] {
        self.fmt_piece_lists.alloc_slice(pieces)
    }

    pub fn alloc_block(&'hir self, block: Block<'hir>) -> &'hir Block<'hir> {
        self.blocks.alloc(block)
    }

    pub fn alloc_stmt(&'hir self, stmt: Stmt<'hir>) -> &'hir Stmt<'hir> {
        self.stmts.alloc(stmt)
    }

    pub fn alloc_stmts(&'hir self, stmts: &[Stmt<'hir>]) -> &'hir [Stmt<'hir>] {
        self.stmt_lists.alloc_slice(stmts)
    }

    pub fn alloc_fn_param(&'hir self, param: FnParam<'hir>) -> &'hir FnParam<'hir> {
        self.fn_params.alloc(param)
    }

    pub fn alloc_fn_params(&'hir self, params: &[FnParam<'hir>]) -> &'hir [FnParam<'hir>] {
        self.fn_param_lists.alloc_slice(params)
    }

    pub fn alloc_body(&'hir self, body: Body<'hir>) -> &'hir Body<'hir> {
        self.body.alloc(body)
    }

    pub fn alloc_body_param(&'hir self, param: BodyParam) -> &'hir BodyParam {
        self.body_params.alloc(param)
    }

    pub fn alloc_body_params(&'hir self, params: &[BodyParam]) -> &'hir [BodyParam] {
        self.body_param_lists.alloc_slice(params)
    }

    pub fn alloc_contract(&'hir self, contract: Contract<'hir>) -> &'hir Contract<'hir> {
        self.contracts.alloc(contract)
    }

    pub fn alloc_contracts(&'hir self, contracts: &[Contract<'hir>]) -> &'hir [Contract<'hir>] {
        self.contract_lists.alloc_slice(contracts)
    }

    pub fn alloc_hir_ids(&'hir self, ids: &[crate::HirId]) -> &'hir [crate::HirId] {
        self.hir_id_lists.alloc_slice(ids)
    }

    pub fn alloc_owner_ids(&'hir self, ids: &[crate::OwnerId]) -> &'hir [crate::OwnerId] {
        self.owner_id_lists.alloc_slice(ids)
    }

    pub fn alloc_expr_refs(&'hir self, exprs: &[&'hir Expr<'hir>]) -> &'hir [&'hir Expr<'hir>] {
        self.expr_ref_lists.alloc_slice(exprs)
    }

    pub fn alloc_item(&'hir self, item: Item<'hir>) -> &'hir Item<'hir> {
        self.items.alloc(item)
    }

    pub fn alloc_items(&'hir self, items: &[Item<'hir>]) -> &'hir [Item<'hir>] {
        self.item_lists.alloc_slice(items)
    }

    pub fn alloc_trait_item(&'hir self, item: TraitItem<'hir>) -> &'hir TraitItem<'hir> {
        self.trait_items.alloc(item)
    }

    pub fn alloc_trait_items(&'hir self, items: &[TraitItem<'hir>]) -> &'hir [TraitItem<'hir>] {
        self.trait_item_lists.alloc_slice(items)
    }

    pub fn alloc_impl_item(&'hir self, item: ImplItem<'hir>) -> &'hir ImplItem<'hir> {
        self.impl_items.alloc(item)
    }

    pub fn alloc_impl_items(&'hir self, items: &[ImplItem<'hir>]) -> &'hir [ImplItem<'hir>] {
        self.impl_item_lists.alloc_slice(items)
    }

    pub fn alloc_extern_item(&'hir self, item: ExternItem<'hir>) -> &'hir ExternItem<'hir> {
        self.extern_items.alloc(item)
    }

    pub fn alloc_extern_items(&'hir self, items: &[ExternItem<'hir>]) -> &'hir [ExternItem<'hir>] {
        self.extern_item_lists.alloc_slice(items)
    }

    pub fn alloc_field_def(&'hir self, field: FieldDef<'hir>) -> &'hir FieldDef<'hir> {
        self.field_defs.alloc(field)
    }

    pub fn alloc_field_defs(&'hir self, fields: &[FieldDef<'hir>]) -> &'hir [FieldDef<'hir>] {
        self.field_def_lists.alloc_slice(fields)
    }

    pub fn alloc_variant(&'hir self, variant: Variant<'hir>) -> &'hir Variant<'hir> {
        self.variants.alloc(variant)
    }

    pub fn alloc_variants(&'hir self, variants: &[Variant<'hir>]) -> &'hir [Variant<'hir>] {
        self.variant_lists.alloc_slice(variants)
    }

    pub fn alloc_derived_impl(&'hir self, derived: DerivedImpl) -> &'hir DerivedImpl {
        self.derived_impls.alloc(derived)
    }

    pub fn alloc_derived_impls(&'hir self, derived: &[DerivedImpl]) -> &'hir [DerivedImpl] {
        self.derived_impl_lists.alloc_slice(derived)
    }

    pub fn alloc_interface_item(
        &'hir self,
        item: InterfaceItem<'hir>,
    ) -> &'hir InterfaceItem<'hir> {
        self.interface_items.alloc(item)
    }

    pub fn alloc_interface_items(
        &'hir self,
        items: &[InterfaceItem<'hir>],
    ) -> &'hir [InterfaceItem<'hir>] {
        self.interface_item_lists.alloc_slice(items)
    }

    pub fn alloc_world_item(&'hir self, item: WorldItem<'hir>) -> &'hir WorldItem<'hir> {
        self.world_items.alloc(item)
    }

    pub fn alloc_world_items(&'hir self, items: &[WorldItem<'hir>]) -> &'hir [WorldItem<'hir>] {
        self.world_item_lists.alloc_slice(items)
    }
}

impl Default for HirArena<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::FnSig;
    use crate::expressions::{CaptureBy, ExprKind, FmtSpec, MatchSource, StmtKind};
    use crate::patterns::PatKind;
    use crate::types::{
        BoundKind, ConstArg, FnTraitKind, GenericArg, GenericParamKind, LangItem, PathRoot, TyKind,
    };
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, OwnerId,
        PackageInterner, Span, SpanCtxt,
    };
    use sai_intern::{Interner, ItemLocalId};
    use sai_vfs::{FileId, TextRange, TextSize};

    fn test_hir_id() -> HirId {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("path interner");
        let symbol = symbols.intern("item").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
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
    fn arena_stores_nested_type_nodes_and_child_lists_for_its_lifetime() {
        let arena = HirArena::new();
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let option_name = Name {
            sym: symbols.intern("Option").expect("symbol interns"),
            span: span(1),
        };
        let type_name = arena.alloc_names(&[option_name]);
        let inner = arena.alloc_ty(Ty {
            hir_id,
            kind: TyKind::Infer,
            span: span(7),
        });
        let arguments = arena.alloc_generic_args_list(&[
            GenericArg::Type(inner),
            GenericArg::Const(ConstArg::Bool(true)),
        ]);
        let generic_args = arena.alloc_generic_args(GenericArgs {
            args: arguments,
            span: span(6),
        });
        let segment = arena.alloc_path_segment(PathSegment {
            hir_id,
            name: type_name[0],
            args: Some(generic_args),
        });
        let segments = arena.alloc_path_segments(&[*segment]);
        let path = arena.alloc_path(Path {
            hir_id,
            root: PathRoot::Lang(LangItem::Option),
            segments,
            span: span(0),
        });
        let outer = arena.alloc_ty(Ty {
            hir_id,
            kind: TyKind::Path(*path),
            span: span(0),
        });
        let first_address = outer as *const Ty<'_>;
        let parameter_bounds = arena.alloc_bounds(&[Bound {
            hir_id,
            kind: BoundKind::FnSugar {
                kind: FnTraitKind::FnMut,
                params: arena.alloc_tys(&[*inner]),
                ret: Some(outer),
            },
            effects: None,
            span: span(8),
        }]);
        let parameter = arena.alloc_generic_param(GenericParam {
            def_id: hir_id.owner.def_id,
            name: option_name,
            kind: GenericParamKind::Type {
                bounds: parameter_bounds,
            },
        });
        let params = arena.alloc_generic_params(&[*parameter]);
        let where_pred = arena.alloc_where_pred(WherePred {
            hir_id,
            ty: outer,
            bounds: parameter_bounds,
        });
        let where_preds = arena.alloc_where_preds(&[*where_pred]);
        let generics = arena.alloc_generics(Generics {
            params,
            where_preds,
        });
        let effect = arena.alloc_effect_ref(EffectRef {
            hir_id,
            segments: type_name,
        });
        let effects = arena.alloc_effect_refs(&[*effect]);
        let provenance = arena.alloc_provenance(Provenance {
            root: option_name,
            fields: type_name,
        });
        let provenances = arena.alloc_provenances(&[*provenance]);

        for offset in 0..128 {
            arena.alloc_ty(Ty {
                hir_id,
                kind: TyKind::Tuple(&[]),
                span: span(offset),
            });
        }

        assert_eq!(first_address, outer as *const Ty<'_>);
        assert_eq!(outer.kind, TyKind::Path(*path));
        assert_eq!(generics.params.len(), 1);
        assert_eq!(generics.where_preds.len(), 1);
        assert_eq!(effects[0].segments, &[option_name]);
        assert_eq!(provenances[0].fields, &[option_name]);
    }

    #[test]
    fn arena_stores_pattern_nodes_and_byte_string_payloads() {
        let arena = HirArena::new();
        let hir_id = test_hir_id();
        let bytes = arena.alloc_bytes(&[0, 255, 128]);
        let literal = arena.alloc_lit(Lit::ByteStr(bytes));
        let pattern = arena.alloc_pat(Pat {
            hir_id,
            kind: PatKind::Lit(*literal),
            span: span(0),
        });
        let patterns = arena.alloc_pats(&[*pattern]);

        for value in 0..128_u8 {
            arena.alloc_bytes(&[value; 4]);
        }

        assert_eq!(patterns[0].kind, PatKind::Lit(Lit::ByteStr(&[0, 255, 128])));
        assert_eq!(bytes, &[0, 255, 128]);
    }

    #[test]
    fn arena_stores_expression_block_statement_arm_and_closure_graphs() {
        let arena = HirArena::new();
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let literal = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            }),
            span: span(3),
        });
        let tuple_items = arena.alloc_exprs(&[*literal, *literal]);
        let tuple = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::Tuple(tuple_items),
            span: span(2),
        });
        let argument = arena.alloc_arg(Arg {
            name: None,
            expr: tuple,
        });
        let arguments = arena.alloc_args(&[*argument]);
        let stmt = arena.alloc_stmt(Stmt {
            hir_id,
            kind: StmtKind::Assert {
                cond: tuple,
                msg: Some(literal),
                cond_text: symbols.intern("1 == 1").expect("symbol interns"),
            },
            span: span(1),
        });
        let stmts = arena.alloc_stmts(&[*stmt]);
        let block = arena.alloc_block(Block {
            hir_id,
            stmts,
            tail: Some(tuple),
            safety: crate::Safety::Safe,
            span: span(0),
        });
        let pat = arena.alloc_pat(Pat {
            hir_id,
            kind: PatKind::Wild,
            span: span(4),
        });
        let arm = arena.alloc_arm(Arm {
            hir_id,
            pat,
            guard: None,
            body: tuple,
            span: span(4),
            synthesized: true,
        });
        let arms = arena.alloc_arms(&[*arm]);
        let match_expr = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::Match {
                scrutinee: tuple,
                arms,
                source: MatchSource::IfLet,
            },
            span: span(4),
        });
        let closure_param = arena.alloc_closure_param(ClosureParam {
            hir_id,
            binding: crate::BindingId::default(),
            name: None,
            mutable: false,
            ty: None,
        });
        let closure_params = arena.alloc_closure_params(&[*closure_param]);
        let closure = arena.alloc_closure(Closure {
            def_id: hir_id.owner.def_id,
            capture: CaptureBy::Move,
            params: closure_params,
            ret: None,
            body: block,
        });
        let closure_expr = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::Closure(closure),
            span: span(5),
        });
        let fmt_pieces = arena.alloc_fmt_pieces(&[
            FmtPiece::Text(symbols.intern("value=").expect("symbol interns")),
            FmtPiece::Arg {
                expr: tuple,
                spec: Some(FmtSpec {
                    name: symbols.intern("hex").expect("symbol interns"),
                    arg: Some(16),
                    span: span(6),
                }),
            },
        ]);
        let fmt_expr = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::FormatString(fmt_pieces),
            span: span(6),
        });

        for offset in 0..128 {
            arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Err,
                span: span(offset),
            });
        }

        assert_eq!(tuple_items.len(), 2);
        assert_eq!(arguments[0].expr, tuple);
        assert_eq!(block.stmts.len(), 1);
        assert!(arms[0].synthesized);
        assert_eq!(closure.params.len(), 1);
        assert_eq!(fmt_pieces.len(), 2);
        assert!(matches!(match_expr.kind, ExprKind::Match { .. }));
        assert!(matches!(closure_expr.kind, ExprKind::Closure(_)));
        assert!(matches!(fmt_expr.kind, ExprKind::FormatString(_)));
    }

    #[test]
    fn arena_stores_function_signatures_and_body_contract_lists() {
        let arena = HirArena::new();
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("arg").expect("symbol interns"),
            span: span(2),
        };
        let ty = arena.alloc_ty(Ty {
            hir_id,
            kind: TyKind::Infer,
            span: span(3),
        });
        let fn_param = arena.alloc_fn_param(FnParam {
            hir_id,
            name: Some(name),
            is_self: false,
            mutable: true,
            ty,
        });
        let fn_params = arena.alloc_fn_params(&[*fn_param]);
        let required = arena.alloc_hir_ids(&[hir_id]);
        let signature = FnSig {
            safety: crate::Safety::Unsafe,
            is_async: true,
            params: fn_params,
            ret: Some(ty),
            borrows: None,
            effects: None,
            requires: required,
            ensures: &[],
        };
        let expression = arena.alloc_expr(Expr {
            hir_id,
            kind: ExprKind::Lit(Lit::Bool(true)),
            span: span(5),
        });
        let body_param = arena.alloc_body_param(BodyParam {
            hir_id,
            binding: crate::BindingId::default(),
            name: Some(name),
            mutable: true,
        });
        let body_params = arena.alloc_body_params(&[*body_param]);
        let contract = arena.alloc_contract(Contract {
            hir_id,
            kind: crate::body::ContractKind::Ensures,
            expr: expression,
        });
        let contracts = arena.alloc_contracts(&[*contract]);
        let invariant_preds = arena.alloc_expr_refs(&[expression]);
        let body = arena.alloc_body(Body {
            owner: hir_id.owner,
            params: body_params,
            value: expression,
            contracts,
            invariant_preds,
        });

        assert_eq!(signature.params[0].name, Some(name));
        assert_eq!(signature.requires, &[hir_id]);
        assert_eq!(body.params[0].binding, crate::BindingId::default());
        assert_eq!(body.contracts[0].kind, crate::body::ContractKind::Ensures);
        assert_eq!(body.invariant_preds, &[expression]);
    }
}
