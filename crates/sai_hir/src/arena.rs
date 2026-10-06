use crate::types::{
    Bound, EffectRef, GenericArg, GenericArgs, GenericParam, Generics, Name, Path, PathSegment,
    Provenance, Ty, WherePred,
};
use sai_intern::Arena;

/// Stable storage for HIR type nodes and their borrowed child lists.
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
}

impl Default for HirArena<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
