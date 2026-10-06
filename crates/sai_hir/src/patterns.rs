use crate::literals::Lit;
use crate::types::{Name, Path};
use crate::{BindingId, HirId, Span};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug)]
pub struct Pat<'hir> {
    pub hir_id: HirId,
    pub kind: PatKind<'hir>,
    pub span: Span,
}

impl PartialEq for Pat<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.kind == other.kind
    }
}

impl Eq for Pat<'_> {}

impl Hash for Pat<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PatKind<'hir> {
    Wild,
    Binding {
        mode: BindingMode,
        name: Name,
        binding: BindingId,
        sub: Option<&'hir Pat<'hir>>,
    },
    Lit(Lit<'hir>),
    Range {
        start: Option<PatRangeEnd<'hir>>,
        end: Option<PatRangeEnd<'hir>>,
        inclusive: bool,
    },
    Tuple(&'hir [Pat<'hir>]),
    Slice {
        before: &'hir [Pat<'hir>],
        rest: Option<Option<&'hir Pat<'hir>>>,
        after: &'hir [Pat<'hir>],
    },
    Ctor {
        path: Path<'hir>,
        args: Option<&'hir [PatArg<'hir>]>,
        has_rest: bool,
    },
    ImplicitMember {
        name: Name,
        args: Option<&'hir [PatArg<'hir>]>,
        has_rest: bool,
    },
    Or(&'hir [Pat<'hir>]),
    Err,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingMode {
    Value,
    Var,
    Borrow,
    BorrowMut,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PatArg<'hir> {
    pub name: Option<Name>,
    pub pat: &'hir Pat<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PatRangeEnd<'hir> {
    Lit(Lit<'hir>),
    Path(Path<'hir>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LangItem, PathRoot};
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, OwnerId,
        PackageInterner, SpanCtxt,
    };
    use sai_intern::{Interner, ItemLocalId};
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

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

    fn span(offset: u32, ctxt: SpanCtxt) -> Span {
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
    fn patterns_preserve_bindings_constructor_shape_and_literal_values() {
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let name = Name {
            sym: symbols.intern("Some").expect("symbol interns"),
            span: span(1, SpanCtxt::Root),
        };
        let binding = Pat {
            hir_id,
            kind: PatKind::Binding {
                mode: BindingMode::BorrowMut,
                name,
                binding: BindingId::default(),
                sub: None,
            },
            span: span(5, SpanCtxt::Root),
        };
        let path = Path {
            hir_id,
            root: PathRoot::Lang(LangItem::OptionSome),
            segments: &[],
            span: span(0, SpanCtxt::Root),
        };
        let args = [PatArg {
            name: Some(name),
            pat: &binding,
        }];
        let pattern = Pat {
            hir_id,
            kind: PatKind::Ctor {
                path,
                args: Some(&args),
                has_rest: true,
            },
            span: span(0, SpanCtxt::Root),
        };
        let moved_pattern = Pat {
            span: span(40, SpanCtxt::Desugared(crate::DesugaringKind::IfLet)),
            ..pattern
        };
        let literal = Pat {
            hir_id,
            kind: PatKind::Lit(Lit::Int {
                value: Some(12),
                negated: true,
            }),
            span: span(8, SpanCtxt::Root),
        };
        let omitted_args = PatKind::Ctor {
            path,
            args: None,
            has_rest: false,
        };
        let explicit_empty_args = PatKind::Ctor {
            path,
            args: Some(&[]),
            has_rest: false,
        };

        assert_eq!(pattern, moved_pattern);
        assert_eq!(value_hash(&pattern), value_hash(&moved_pattern));
        assert_eq!(pattern.kind, moved_pattern.kind);
        assert_ne!(omitted_args, explicit_empty_args);
        assert_eq!(
            literal.kind,
            PatKind::Lit(Lit::Int {
                value: Some(12),
                negated: true,
            })
        );
    }

    #[test]
    fn slice_pattern_distinguishes_missing_rest_from_bare_and_bound_rest() {
        let hir_id = test_hir_id();
        let symbols = Interner::new();
        let rest_name = Name {
            sym: symbols.intern("tail").expect("symbol interns"),
            span: span(2, SpanCtxt::Root),
        };
        let rest_binding = Pat {
            hir_id,
            kind: PatKind::Binding {
                mode: BindingMode::Value,
                name: rest_name,
                binding: BindingId::default(),
                sub: None,
            },
            span: span(3, SpanCtxt::Root),
        };
        let with_binding = PatKind::Slice {
            before: &[],
            rest: Some(Some(&rest_binding)),
            after: &[],
        };
        let bare_rest = PatKind::Slice {
            before: &[],
            rest: Some(None),
            after: &[],
        };
        let no_rest = PatKind::Slice {
            before: &[],
            rest: None,
            after: &[],
        };

        assert_ne!(with_binding, bare_rest);
        assert_ne!(bare_rest, no_rest);
    }
}
