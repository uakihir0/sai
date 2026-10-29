use super::{
    PrimitiveBodyCheck, PrimitiveBodyError, PrimitiveBodyErrorKind, PrimitiveBodyInput,
    PrimitiveLiteralKind, check_primitive_local_block_body,
};
use sai_hir::body::Body;
use sai_hir::expressions::{Block, Expr, ExprKind, Stmt, StmtKind};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, Pat, PatKind};
use sai_hir::types::{Name, Ty, TyKind};
use sai_hir::{
    DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, FileId, HirId, OwnerId,
    Safety, Span, SpanCtxt,
};
use sai_intern::{BindingId, Idx, Interner, ItemLocalId, PackageInterner};
use sai_resolve::prelude::PrimTy;
use sai_vfs::{TextRange, TextSize};
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum Statement {
    None,
    Local,
    UnannotatedInteger,
    AnnotatedMismatch,
    UnannotatedOverflow,
    Unsupported,
}

fn check_tail(
    symbols: &Interner,
    literal: Lit<'_>,
    result: PrimTy,
    pointer_width: u8,
    statement: Statement,
    safety: Safety,
) -> (PrimitiveBodyCheck, HirId, HirId, BindingId) {
    let packages = PackageInterner::new();
    let package = packages.intern("acme:app#1.0").unwrap();
    let paths = DefPathInterner::new(package, &packages, symbols).unwrap();
    let path = DefPath::new(
        package,
        vec![DisambiguatedDefPathData {
            data: DefPathData::ValueNs(symbols.intern("function").unwrap()),
            disambiguator: 0,
        }],
        &packages,
        symbols,
    )
    .unwrap();
    let owner = OwnerId {
        def_id: paths.intern(path).unwrap(),
    };
    let node = |index| HirId {
        owner,
        local_id: ItemLocalId::from_raw(index),
    };
    let span = Span {
        file: FileId::new(1),
        range: TextRange::empty(TextSize::new(0)),
        ctxt: SpanCtxt::Root,
    };
    let binding = BindingId::from_raw(0);
    let pattern = Pat {
        hir_id: node(1),
        kind: PatKind::Binding {
            mode: BindingMode::Value,
            name: Name {
                sym: symbols.intern("local").unwrap(),
                span,
            },
            binding,
            sub: None,
        },
        span,
    };
    let annotation = Ty {
        hir_id: node(2),
        kind: TyKind::Infer,
        span,
    };
    let initializer = Expr {
        hir_id: node(3),
        kind: ExprKind::Lit(match statement {
            Statement::UnannotatedInteger | Statement::AnnotatedMismatch => Lit::Int {
                value: Some(42),
                negated: false,
            },
            Statement::UnannotatedOverflow => Lit::Int {
                value: Some(1u128 << 63),
                negated: false,
            },
            _ => Lit::Bool(true),
        }),
        span,
    };
    let stmt = Stmt {
        hir_id: node(4),
        kind: match statement {
            Statement::Local
            | Statement::UnannotatedInteger
            | Statement::AnnotatedMismatch
            | Statement::UnannotatedOverflow
            | Statement::None => StmtKind::Let {
                pat: &pattern,
                ty: if matches!(
                    statement,
                    Statement::UnannotatedInteger | Statement::UnannotatedOverflow
                ) {
                    None
                } else {
                    Some(&annotation)
                },
                init: &initializer,
                els: None,
            },
            Statement::Unsupported => StmtKind::Expr(&initializer),
        },
        span,
    };
    let tail = Expr {
        hir_id: node(5),
        kind: ExprKind::Lit(literal),
        span,
    };
    let statements = [stmt];
    let block = Block {
        hir_id: node(6),
        stmts: if matches!(statement, Statement::None) {
            &[]
        } else {
            &statements
        },
        tail: Some(&tail),
        safety,
        span,
    };
    let value = Expr {
        hir_id: node(7),
        kind: ExprKind::Block(&block),
        span,
    };
    let body = Body {
        owner,
        source_id: Default::default(),
        params: &[],
        value: &value,
        contracts: &[],
        invariant_preds: &[],
    };
    let primitive_types = HashMap::from([(annotation.hir_id, PrimTy::Bool)]);
    let check = check_primitive_local_block_body(PrimitiveBodyInput {
        body: &body,
        return_type: result,
        initial_bindings: &HashMap::new(),
        primitive_types: &primitive_types,
        self_field_projections: None,
        unknown_self_fields: None,
        local_paths: &HashMap::new(),
        call_signatures: None,
        symbols,
        pointer_width,
    });
    (check, tail.hir_id, initializer.hir_id, binding)
}

#[test]
fn literal_tails_use_return_type_and_preserve_local_facts() {
    let symbols = Interner::new();
    let float = symbols.intern("1.5").unwrap();
    for (literal, expected) in [
        (
            Lit::Int {
                value: Some(127),
                negated: false,
            },
            PrimTy::I8,
        ),
        (
            Lit::Int {
                value: Some(128),
                negated: true,
            },
            PrimTy::I8,
        ),
        (
            Lit::Float {
                text: float,
                negated: false,
            },
            PrimTy::F32,
        ),
        (Lit::Bool(true), PrimTy::Bool),
        (Lit::Byte(3), PrimTy::Byte),
        (Lit::Char('a'), PrimTy::Char),
    ] {
        for statement in [Statement::None, Statement::Local] {
            let (check, tail, initializer, binding) =
                check_tail(&symbols, literal, expected, 64, statement, Safety::Safe);
            let PrimitiveBodyCheck::Supported {
                errors,
                type_results,
            } = check
            else {
                panic!("literal tail should be supported");
            };
            assert!(errors.is_empty(), "{literal:?}: {errors:?}");
            assert_eq!(type_results.expression_type(tail), Some(expected));
            if matches!(statement, Statement::Local) {
                assert_eq!(
                    type_results.expression_type(initializer),
                    Some(PrimTy::Bool)
                );
                assert_eq!(type_results.binding_type(binding), Some(PrimTy::Bool));
            }
        }
    }
}

#[test]
fn literal_tail_does_not_constrain_an_unrelated_local() {
    let symbols = Interner::new();
    let (check, tail, initializer, binding) = check_tail(
        &symbols,
        Lit::Bool(true),
        PrimTy::Bool,
        64,
        Statement::UnannotatedInteger,
        Safety::Safe,
    );
    let PrimitiveBodyCheck::Supported {
        errors,
        type_results,
    } = check
    else {
        panic!("literal tail should be supported");
    };
    assert!(errors.is_empty());
    assert_eq!(type_results.expression_type(tail), Some(PrimTy::Bool));
    assert_eq!(type_results.expression_type(initializer), Some(PrimTy::I64));
    assert_eq!(type_results.binding_type(binding), Some(PrimTy::I64));
}

#[test]
fn literal_tail_remains_trustworthy_after_local_errors() {
    let symbols = Interner::new();
    for (statement, kind, expected_binding) in [
        (
            Statement::AnnotatedMismatch,
            PrimitiveBodyErrorKind::LiteralTypeMismatch {
                expected: PrimTy::Bool,
                found: PrimitiveLiteralKind::Integer,
            },
            Some(PrimTy::Bool),
        ),
        (
            Statement::UnannotatedOverflow,
            PrimitiveBodyErrorKind::LiteralOutOfRange {
                expected: PrimTy::I64,
                found: PrimitiveLiteralKind::Integer,
            },
            None,
        ),
    ] {
        let (check, tail, initializer, binding) = check_tail(
            &symbols,
            Lit::Bool(true),
            PrimTy::Bool,
            64,
            statement,
            Safety::Safe,
        );
        let PrimitiveBodyCheck::Supported {
            errors,
            type_results,
        } = check
        else {
            panic!("literal tail should be supported");
        };
        assert_eq!(
            errors,
            vec![PrimitiveBodyError {
                node: initializer,
                kind
            }]
        );
        assert_eq!(type_results.expression_type(initializer), None);
        assert_eq!(type_results.binding_type(binding), expected_binding);
        assert_eq!(type_results.expression_type(tail), Some(PrimTy::Bool));
    }
}

#[test]
fn literal_tail_errors_are_anchored_and_do_not_publish_type_facts() {
    let symbols = Interner::new();
    let float = symbols.intern("1e100").unwrap();
    for (literal, expected, width, kind) in [
        (
            Lit::Int {
                value: Some(1),
                negated: false,
            },
            PrimTy::F64,
            64,
            PrimitiveBodyErrorKind::LiteralTypeMismatch {
                expected: PrimTy::F64,
                found: PrimitiveLiteralKind::Integer,
            },
        ),
        (
            Lit::Int {
                value: Some(128),
                negated: false,
            },
            PrimTy::I8,
            64,
            PrimitiveBodyErrorKind::LiteralOutOfRange {
                expected: PrimTy::I8,
                found: PrimitiveLiteralKind::Integer,
            },
        ),
        (
            Lit::Int {
                value: Some(1u128 << 32),
                negated: false,
            },
            PrimTy::USize,
            32,
            PrimitiveBodyErrorKind::LiteralOutOfRange {
                expected: PrimTy::USize,
                found: PrimitiveLiteralKind::Integer,
            },
        ),
        (
            Lit::Float {
                text: float,
                negated: false,
            },
            PrimTy::F32,
            64,
            PrimitiveBodyErrorKind::LiteralOutOfRange {
                expected: PrimTy::F32,
                found: PrimitiveLiteralKind::FloatingPoint,
            },
        ),
    ] {
        let (check, tail, initializer, binding) = check_tail(
            &symbols,
            literal,
            expected,
            width,
            Statement::Local,
            Safety::Safe,
        );
        let PrimitiveBodyCheck::Supported {
            errors,
            type_results,
        } = check
        else {
            panic!("literal tail should be supported");
        };
        assert_eq!(errors, vec![PrimitiveBodyError { node: tail, kind }]);
        assert_eq!(type_results.expression_type(tail), None);
        assert_eq!(
            type_results.expression_type(initializer),
            Some(PrimTy::Bool)
        );
        assert_eq!(type_results.binding_type(binding), Some(PrimTy::Bool));
    }
}

#[test]
fn literal_tails_do_not_open_unsupported_shapes() {
    let symbols = Interner::new();
    let text = symbols.intern("text").unwrap();
    for (literal, statement, safety) in [
        (Lit::Str(text), Statement::None, Safety::Safe),
        (Lit::Str(text), Statement::Local, Safety::Safe),
        (Lit::Bool(true), Statement::Unsupported, Safety::Safe),
        (Lit::Bool(true), Statement::None, Safety::Unsafe),
    ] {
        let (check, _, _, _) = check_tail(&symbols, literal, PrimTy::Bool, 64, statement, safety);
        assert_eq!(check, PrimitiveBodyCheck::Unsupported);
    }
}
