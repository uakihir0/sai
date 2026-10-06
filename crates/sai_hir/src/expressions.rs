use crate::literals::Lit;
use crate::patterns::Pat;
use crate::types::{GenericArgs, Name, Path, Ty};
use crate::{BindingId, HirId, Mutability, Safety, Span, Symbol};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug)]
pub struct Expr<'hir> {
    pub hir_id: HirId,
    pub kind: ExprKind<'hir>,
    pub span: Span,
}

impl PartialEq for Expr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.kind == other.kind
    }
}

impl Eq for Expr<'_> {}

impl Hash for Expr<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExprKind<'hir> {
    Lit(Lit<'hir>),
    Path(Path<'hir>),
    ImplicitMember(Name),
    Field {
        base: &'hir Expr<'hir>,
        name: Name,
        args: Option<&'hir GenericArgs<'hir>>,
    },
    TupleField {
        base: &'hir Expr<'hir>,
        index: Option<u32>,
    },
    Call {
        callee: &'hir Expr<'hir>,
        args: &'hir [Arg<'hir>],
        base: Option<&'hir Expr<'hir>>,
    },
    Unary(UnOp, &'hir Expr<'hir>),
    Borrow(Mutability, &'hir Expr<'hir>),
    Binary(BinOp, &'hir Expr<'hir>, &'hir Expr<'hir>),
    Range {
        start: Option<&'hir Expr<'hir>>,
        end: Option<&'hir Expr<'hir>>,
        inclusive: bool,
    },
    Index(&'hir Expr<'hir>, &'hir Expr<'hir>),
    Try(&'hir Expr<'hir>),
    Await(&'hir Expr<'hir>),
    Tuple(&'hir [Expr<'hir>]),
    Array(&'hir [Expr<'hir>]),
    Block(&'hir Block<'hir>),
    AsyncBlock {
        def_id: crate::LocalDefId,
        capture: CaptureBy,
        block: &'hir Block<'hir>,
    },
    If {
        cond: &'hir Expr<'hir>,
        then: &'hir Block<'hir>,
        els: Option<&'hir Expr<'hir>>,
    },
    Match {
        scrutinee: &'hir Expr<'hir>,
        arms: &'hir [Arm<'hir>],
        source: MatchSource,
    },
    Loop {
        body: &'hir Block<'hir>,
        label: Option<Label>,
        source: LoopSource,
    },
    Break {
        label: Option<Label>,
        value: Option<&'hir Expr<'hir>>,
    },
    Continue {
        label: Option<Label>,
    },
    Return(Option<&'hir Expr<'hir>>),
    Closure(&'hir Closure<'hir>),
    Region {
        name: Name,
        binding: BindingId,
        block: &'hir Block<'hir>,
    },
    FormatString(&'hir [FmtPiece<'hir>]),
    ContractResult,
    ContractOld(&'hir Expr<'hir>),
    AsCast {
        operand: &'hir Expr<'hir>,
        ty: &'hir Ty<'hir>,
    },
    Err,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Arg<'hir> {
    pub name: Option<Name>,
    pub expr: &'hir Expr<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnOp {
    Neg,
    BitNot,
    Not,
    Deref,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    WrapAdd,
    WrapSub,
    WrapMul,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaptureBy {
    Move,
    Infer,
}

#[derive(Clone, Copy, Debug)]
pub struct Label {
    pub name: Symbol,
    pub span: Span,
}

impl PartialEq for Label {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Label {}

impl Hash for Label {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MatchSource {
    Normal,
    IfLet,
    WhileLet,
    ForLoop,
    ForAwait,
    Try,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LoopSource {
    Loop,
    While,
    WhileLet,
    ForLoop,
    ForAwait,
}

#[derive(Clone, Copy, Debug)]
pub struct Arm<'hir> {
    pub hir_id: HirId,
    pub pat: &'hir Pat<'hir>,
    pub guard: Option<&'hir Expr<'hir>>,
    pub body: &'hir Expr<'hir>,
    pub span: Span,
    pub synthesized: bool,
}

impl PartialEq for Arm<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id
            && self.pat == other.pat
            && self.guard == other.guard
            && self.body == other.body
            && self.synthesized == other.synthesized
    }
}

impl Eq for Arm<'_> {}

impl Hash for Arm<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.pat.hash(state);
        self.guard.hash(state);
        self.body.hash(state);
        self.synthesized.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Closure<'hir> {
    pub def_id: crate::LocalDefId,
    pub capture: CaptureBy,
    pub params: &'hir [ClosureParam<'hir>],
    pub ret: Option<&'hir Ty<'hir>>,
    pub body: &'hir Block<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClosureParam<'hir> {
    pub hir_id: HirId,
    pub binding: BindingId,
    pub name: Option<Name>,
    pub mutable: bool,
    pub ty: Option<&'hir Ty<'hir>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FmtPiece<'hir> {
    Text(Symbol),
    Arg {
        expr: &'hir Expr<'hir>,
        spec: Option<FmtSpec>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct FmtSpec {
    pub name: Symbol,
    pub arg: Option<u128>,
    pub span: Span,
}

impl PartialEq for FmtSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.arg == other.arg
    }
}

impl Eq for FmtSpec {}

impl Hash for FmtSpec {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.arg.hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Block<'hir> {
    pub hir_id: HirId,
    pub stmts: &'hir [Stmt<'hir>],
    pub tail: Option<&'hir Expr<'hir>>,
    pub safety: Safety,
    pub span: Span,
}

impl PartialEq for Block<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id
            && self.stmts == other.stmts
            && self.tail == other.tail
            && self.safety == other.safety
    }
}

impl Eq for Block<'_> {}

impl Hash for Block<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.stmts.hash(state);
        self.tail.hash(state);
        self.safety.hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stmt<'hir> {
    pub hir_id: HirId,
    pub kind: StmtKind<'hir>,
    pub span: Span,
}

impl PartialEq for Stmt<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.hir_id == other.hir_id && self.kind == other.kind
    }
}

impl Eq for Stmt<'_> {}

impl Hash for Stmt<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hir_id.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StmtKind<'hir> {
    Let {
        pat: &'hir Pat<'hir>,
        ty: Option<&'hir Ty<'hir>>,
        init: &'hir Expr<'hir>,
        els: Option<&'hir Block<'hir>>,
    },
    Assign {
        lhs: &'hir Expr<'hir>,
        rhs: &'hir Expr<'hir>,
    },
    AssignOp {
        op: BinOp,
        lhs: &'hir Expr<'hir>,
        rhs: &'hir Expr<'hir>,
    },
    Assert {
        cond: &'hir Expr<'hir>,
        msg: Option<&'hir Expr<'hir>>,
        cond_text: Symbol,
    },
    Expr(&'hir Expr<'hir>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::PatKind;
    use crate::types::PathRoot;
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId, PackageInterner,
        SpanCtxt,
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
    fn expression_blocks_and_statements_ignore_spans_but_keep_ids() {
        let hir_id = test_hir_id();
        let literal = Expr {
            hir_id,
            kind: ExprKind::Lit(Lit::Int {
                value: Some(4),
                negated: false,
            }),
            span: span(3, SpanCtxt::Root),
        };
        let stmt = Stmt {
            hir_id,
            kind: StmtKind::Expr(&literal),
            span: span(2, SpanCtxt::Root),
        };
        let stmts = [stmt];
        let block = Block {
            hir_id,
            stmts: &stmts,
            tail: Some(&literal),
            safety: Safety::Safe,
            span: span(0, SpanCtxt::Root),
        };
        let moved_block = Block {
            span: span(20, SpanCtxt::Desugared(crate::DesugaringKind::IfLet)),
            ..block
        };
        let moved_stmt = Stmt {
            span: span(19, SpanCtxt::Desugared(crate::DesugaringKind::IfLet)),
            ..stmt
        };
        let moved_expr = Expr {
            span: span(18, SpanCtxt::Desugared(crate::DesugaringKind::IfLet)),
            ..literal
        };

        assert_eq!(block, moved_block);
        assert_eq!(value_hash(&block), value_hash(&moved_block));
        assert_eq!(stmt, moved_stmt);
        assert_eq!(value_hash(&stmt), value_hash(&moved_stmt));
        assert_eq!(literal, moved_expr);
        assert_eq!(value_hash(&literal), value_hash(&moved_expr));
    }

    #[test]
    fn labels_and_format_specs_compare_semantics_without_source_spans() {
        let symbols = Interner::new();
        let symbol = symbols.intern("loop_label").expect("symbol interns");
        let label = Label {
            name: symbol,
            span: span(1, SpanCtxt::Root),
        };
        let moved_label = Label {
            span: span(22, SpanCtxt::Desugared(crate::DesugaringKind::WhileLoop)),
            ..label
        };
        let spec = FmtSpec {
            name: symbols.intern("hex").expect("symbol interns"),
            arg: Some(16),
            span: span(4, SpanCtxt::Root),
        };
        let moved_spec = FmtSpec {
            span: span(25, SpanCtxt::Desugared(crate::DesugaringKind::FormatString)),
            ..spec
        };

        assert_eq!(label, moved_label);
        assert_eq!(value_hash(&label), value_hash(&moved_label));
        assert_eq!(spec, moved_spec);
        assert_eq!(value_hash(&spec), value_hash(&moved_spec));
    }

    #[test]
    fn expression_kind_covers_lang_paths_binary_ops_and_synthesized_arms() {
        let hir_id = test_hir_id();
        let operand = Expr {
            hir_id,
            kind: ExprKind::Err,
            span: span(1, SpanCtxt::Root),
        };
        let path = Path {
            hir_id,
            root: PathRoot::Lang(crate::types::LangItem::Panic),
            segments: &[],
            span: span(0, SpanCtxt::Root),
        };
        let call = ExprKind::Call {
            callee: &Expr {
                kind: ExprKind::Path(path),
                ..operand
            },
            args: &[Arg {
                name: None,
                expr: &operand,
            }],
            base: None,
        };
        let binary = ExprKind::Binary(BinOp::And, &operand, &operand);
        let pat = Pat {
            hir_id,
            kind: PatKind::Wild,
            span: span(2, SpanCtxt::Root),
        };
        let arms = [Arm {
            hir_id,
            pat: &pat,
            guard: None,
            body: &operand,
            span: span(3, SpanCtxt::Desugared(crate::DesugaringKind::IfLet)),
            synthesized: true,
        }];
        let match_expr = ExprKind::Match {
            scrutinee: &operand,
            arms: &arms,
            source: MatchSource::IfLet,
        };

        assert!(matches!(call, ExprKind::Call { .. }));
        assert_eq!(binary, ExprKind::Binary(BinOp::And, &operand, &operand));
        assert!(
            matches!(match_expr, ExprKind::Match { arms, source: MatchSource::IfLet, .. } if arms[0].synthesized)
        );
    }
}
