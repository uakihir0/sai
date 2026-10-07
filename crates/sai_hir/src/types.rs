use crate::{HirId, LocalDefId, Mutability, Safety, Span, Symbol};
use std::hash::{Hash, Hasher};

/// An interned, NFC-normalized name and its source location.
#[derive(Clone, Copy, Debug)]
pub struct Name {
    pub sym: Symbol,
    pub span: Span,
}

impl Name {
    pub const fn is_error(self) -> bool {
        self.sym.is_error()
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.sym == other.sym
    }
}

impl Eq for Name {}

impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.sym.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LangItem {
    IntoIterator,
    IntoIteratorIntoIter,
    Iterator,
    IteratorNext,
    Stream,
    StreamNext,
    Option,
    OptionSome,
    OptionNone,
    Result,
    ResultOk,
    ResultErr,
    Range,
    RangeInclusive,
    RangeFrom,
    RangeTo,
    RangeToInclusive,
    RangeFull,
    Format,
    FmtWriter,
    Panic,
    Copy,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Arbitrary,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Neg,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
}

#[derive(Clone, Copy, Debug)]
pub struct Ty<'hir> {
    pub hir_id: HirId,
    pub kind: TyKind<'hir>,
    pub span: Span,
}

impl PartialEq for Ty<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.kind == other.kind
    }
}

impl Eq for Ty<'_> {}

impl Hash for Ty<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TyKind<'hir> {
    Path(Path<'hir>),
    Borrow(Mutability, &'hir Ty<'hir>),
    Tuple(&'hir [Ty<'hir>]),
    FnPtr {
        safety: Safety,
        params: &'hir [Ty<'hir>],
        // `None` preserves an omitted `->`; it denotes the unit return type.
        ret: Option<&'hir Ty<'hir>>,
        // `None` is an omitted clause; `Some([])` is an explicit empty set.
        effects: Option<&'hir [EffectRef<'hir>]>,
    },
    Infer,
    Err,
}

#[derive(Clone, Copy, Debug)]
pub struct Path<'hir> {
    pub hir_id: HirId,
    pub root: PathRoot,
    pub segments: &'hir [PathSegment<'hir>],
    pub span: Span,
}

impl PartialEq for Path<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.root == other.root && self.segments == other.segments
    }
}

impl Eq for Path<'_> {}

impl Hash for Path<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.root.hash(state);
        self.segments.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PathRoot {
    Plain,
    SelfValue,
    SelfType,
    Lang(LangItem),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PathSegment<'hir> {
    pub hir_id: HirId,
    pub name: Name,
    pub args: Option<&'hir GenericArgs<'hir>>,
}

#[derive(Clone, Copy, Debug)]
pub struct GenericArgs<'hir> {
    pub args: &'hir [GenericArg<'hir>],
    pub span: Span,
}

impl PartialEq for GenericArgs<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.args == other.args
    }
}

impl Eq for GenericArgs<'_> {}

impl Hash for GenericArgs<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.args.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GenericArg<'hir> {
    Type(&'hir Ty<'hir>),
    Const(ConstArg),
    Binding { name: Name, ty: &'hir Ty<'hir> },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConstArg {
    Int { value: Option<u128>, negated: bool },
    Char(char),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Generics<'hir> {
    pub params: &'hir [GenericParam<'hir>],
    pub where_preds: &'hir [WherePred<'hir>],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GenericParamKind<'hir> {
    Type { bounds: &'hir [Bound<'hir>] },
    Const { ty: &'hir Ty<'hir> },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GenericParam<'hir> {
    pub def_id: LocalDefId,
    pub name: Name,
    pub kind: GenericParamKind<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WherePred<'hir> {
    pub hir_id: HirId,
    pub ty: &'hir Ty<'hir>,
    pub bounds: &'hir [Bound<'hir>],
}

#[derive(Clone, Copy, Debug)]
pub struct Bound<'hir> {
    pub hir_id: HirId,
    pub kind: BoundKind<'hir>,
    pub effects: Option<&'hir [EffectRef<'hir>]>,
    pub span: Span,
}

impl PartialEq for Bound<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.kind == other.kind && self.effects == other.effects
    }
}

impl Eq for Bound<'_> {}

impl Hash for Bound<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.kind.hash(state);
        self.effects.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BoundKind<'hir> {
    Trait(Path<'hir>),
    FnSugar {
        kind: FnTraitKind,
        params: &'hir [Ty<'hir>],
        ret: Option<&'hir Ty<'hir>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FnTraitKind {
    Fn,
    FnMut,
    FnOnce,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EffectRef<'hir> {
    pub hir_id: HirId,
    pub segments: &'hir [Name],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Provenance<'hir> {
    pub root: Name,
    pub fields: &'hir [Name],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId, PackageInterner,
    };
    use sai_intern::Interner;
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;

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
        let owner = OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        };
        HirId {
            owner,
            local_id: crate::ItemLocalId::default(),
        }
    }

    fn span(offset: u32, ctxt: crate::SpanCtxt) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::empty(TextSize::new(offset)),
            ctxt,
        }
    }

    fn value_hash(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn error_names_and_source_spans_are_separate_from_name_equality() {
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("value").expect("symbol interns"),
            span: span(0, crate::SpanCtxt::Root),
        };
        let moved_name = Name {
            span: span(
                12,
                crate::SpanCtxt::Desugared(crate::DesugaringKind::Assert),
            ),
            ..name
        };
        let error_name = Name {
            sym: Symbol::ERROR,
            span: span(20, crate::SpanCtxt::Root),
        };

        assert_eq!(name, moved_name);
        assert_eq!(value_hash(&name), value_hash(&moved_name));
        assert_ne!(name.span, moved_name.span);
        assert!(error_name.is_error());
        assert!(!name.is_error());
    }

    #[test]
    fn type_paths_and_generic_arguments_are_copyable_and_ignore_spans_in_equality() {
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("Option").expect("symbol interns"),
            span: span(2, crate::SpanCtxt::Root),
        };
        let inner = Ty {
            hir_id,
            kind: TyKind::Infer,
            span: span(9, crate::SpanCtxt::Root),
        };
        let args = [
            GenericArg::Type(&inner),
            GenericArg::Const(ConstArg::Bool(true)),
        ];
        let generic_args = GenericArgs {
            args: &args,
            span: span(8, crate::SpanCtxt::Root),
        };
        let segments = [PathSegment {
            hir_id,
            name,
            args: Some(&generic_args),
        }];
        let path = Path {
            hir_id,
            root: PathRoot::Lang(LangItem::Option),
            segments: &segments,
            span: span(1, crate::SpanCtxt::Root),
        };
        let moved_path = Path {
            span: span(24, crate::SpanCtxt::Desugared(crate::DesugaringKind::TryOp)),
            ..path
        };
        let moved_args = GenericArgs {
            span: span(18, crate::SpanCtxt::Root),
            ..generic_args
        };
        let ty = Ty {
            hir_id,
            kind: TyKind::Path(moved_path),
            span: span(1, crate::SpanCtxt::Root),
        };
        let moved_ty = Ty {
            span: span(30, crate::SpanCtxt::Desugared(crate::DesugaringKind::TryOp)),
            ..ty
        };
        let copied_ty = ty;

        assert_eq!(ty, moved_ty);
        assert_eq!(value_hash(&ty), value_hash(&moved_ty));
        assert_eq!(ty, copied_ty);
        assert_eq!(path, moved_path);
        assert_eq!(value_hash(&generic_args), value_hash(&moved_args));
        assert_eq!(generic_args.args.len(), 2);
        assert_eq!(args[1], GenericArg::Const(ConstArg::Bool(true)));
    }

    #[test]
    fn bounds_and_generic_parameters_preserve_source_level_structure() {
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("T").expect("symbol interns"),
            span: span(3, crate::SpanCtxt::Root),
        };
        let ty = Ty {
            hir_id,
            kind: TyKind::Err,
            span: span(5, crate::SpanCtxt::Root),
        };
        let effects = [EffectRef {
            hir_id,
            segments: &[name],
        }];
        let bound = Bound {
            hir_id,
            kind: BoundKind::FnSugar {
                kind: FnTraitKind::FnMut,
                params: &[ty],
                ret: Some(&ty),
            },
            effects: Some(&effects),
            span: span(4, crate::SpanCtxt::Root),
        };
        let moved_bound = Bound {
            span: span(
                25,
                crate::SpanCtxt::Desugared(crate::DesugaringKind::ForLoop),
            ),
            ..bound
        };
        let bounds = [bound];
        let parameter = GenericParam {
            def_id: hir_id.owner.def_id,
            name,
            kind: GenericParamKind::Type { bounds: &bounds },
        };
        let params = [parameter];
        let generics = Generics {
            params: &params,
            where_preds: &[],
        };

        assert_eq!(
            generics.params[0].kind,
            GenericParamKind::Type { bounds: &bounds }
        );
        assert_eq!(bounds[0].kind, bound.kind);
        assert_eq!(bound, moved_bound);
        assert_eq!(value_hash(&bound), value_hash(&moved_bound));
        assert_eq!(bounds[0].effects.unwrap()[0].segments, &[name]);
    }
}
