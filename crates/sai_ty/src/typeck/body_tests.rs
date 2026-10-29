use super::{
    PrimitiveBodyCheck, PrimitiveBodyError, PrimitiveBodyErrorKind, PrimitiveBodyInput,
    PrimitiveLiteralKind, check_primitive_local_block_body,
    check_primitive_local_block_body_with_assertions,
};
use sai_hir::body::Body;
use sai_hir::expressions::{Block, Expr, ExprKind, Stmt, StmtKind};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, Pat, PatKind};
use sai_hir::types::{Name, Path, PathRoot, PathSegment, Ty, TyKind};
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

#[derive(Clone, Copy)]
enum AssertionCondition {
    Literal(Lit<'static>),
    Path(Option<PrimTy>),
    PathAfterInteger { value: u128, resolve: bool },
    PathAfterDeferredNeg { literal: bool },
    LocalBool,
    UntrustedPath,
    InferredPath,
    Unsupported,
}

fn check_assertion(
    condition: AssertionCondition,
    message: bool,
    opt_in: bool,
) -> (PrimitiveBodyCheck, HirId, HirId) {
    let symbols = Interner::new();
    let packages = PackageInterner::new();
    let package = packages.intern("acme:app#1.0").unwrap();
    let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
    let name = symbols.intern("function").unwrap();
    let path = DefPath::new(
        package,
        vec![DisambiguatedDefPathData {
            data: DefPathData::ValueNs(name),
            disambiguator: 0,
        }],
        &packages,
        &symbols,
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
    let parameter_binding = BindingId::from_raw(1);
    let name = Name { sym: name, span };
    let segments = [PathSegment {
        hir_id: node(1),
        name,
        args: None,
    }];
    let path = Path {
        hir_id: node(2),
        root: PathRoot::Plain,
        segments: &segments,
        span,
    };
    let operand = Expr {
        hir_id: node(3),
        kind: ExprKind::Lit(Lit::Bool(true)),
        span,
    };
    let cond = Expr {
        hir_id: node(4),
        kind: match condition {
            AssertionCondition::Literal(literal) => ExprKind::Lit(literal),
            AssertionCondition::PathAfterDeferredNeg { literal: true } => {
                ExprKind::Lit(Lit::Bool(true))
            }
            AssertionCondition::Unsupported => {
                ExprKind::Unary(sai_hir::expressions::UnOp::Not, &operand)
            }
            _ => ExprKind::Path(path),
        },
        span,
    };
    let pat = Pat {
        hir_id: node(5),
        kind: PatKind::Binding {
            mode: BindingMode::Value,
            name,
            binding,
            sub: None,
        },
        span,
    };
    let ty = Ty {
        hir_id: node(6),
        kind: TyKind::Infer,
        span,
    };
    let initializer = Expr {
        hir_id: node(7),
        kind: ExprKind::Lit(match condition {
            AssertionCondition::LocalBool => Lit::Bool(true),
            AssertionCondition::PathAfterInteger { value, .. } => Lit::Int {
                value: Some(value),
                negated: false,
            },
            _ => Lit::Int {
                value: Some(1),
                negated: false,
            },
        }),
        span,
    };
    let local = Stmt {
        hir_id: node(8),
        kind: StmtKind::Let {
            pat: &pat,
            ty: if matches!(condition, AssertionCondition::UntrustedPath) {
                Some(&ty)
            } else {
                None
            },
            init: &initializer,
            els: None,
        },
        span,
    };
    let assertion = Stmt {
        hir_id: node(9),
        kind: StmtKind::Assert {
            cond: &cond,
            msg: message.then_some(&operand),
            cond_text: name.sym,
        },
        span,
    };
    let local_path = Path {
        hir_id: node(13),
        ..path
    };
    let local_expr = Expr {
        hir_id: node(14),
        kind: ExprKind::Path(local_path),
        span,
    };
    let resolved_pat = Pat {
        hir_id: node(15),
        kind: PatKind::Binding {
            mode: BindingMode::Value,
            name,
            binding: BindingId::from_raw(2),
            sub: None,
        },
        span,
    };
    let resolved_ty = Ty {
        hir_id: node(16),
        kind: TyKind::Infer,
        span,
    };
    let resolve_stmt = Stmt {
        hir_id: node(17),
        kind: StmtKind::Let {
            pat: &resolved_pat,
            ty: Some(&resolved_ty),
            init: &local_expr,
            els: None,
        },
        span,
    };
    let neg_path = Path {
        hir_id: node(18),
        ..path
    };
    let neg_operand = Expr {
        hir_id: node(19),
        kind: ExprKind::Path(neg_path),
        span,
    };
    let neg_expr = Expr {
        hir_id: node(20),
        kind: ExprKind::Unary(sai_hir::expressions::UnOp::Neg, &neg_operand),
        span,
    };
    let neg_pat = Pat {
        hir_id: node(21),
        kind: PatKind::Binding {
            mode: BindingMode::Value,
            name,
            binding: BindingId::from_raw(3),
            sub: None,
        },
        span,
    };
    let neg_stmt = Stmt {
        hir_id: node(22),
        kind: StmtKind::Let {
            pat: &neg_pat,
            ty: None,
            init: &neg_expr,
            els: None,
        },
        span,
    };
    let statements = if let AssertionCondition::PathAfterInteger { resolve, .. } = condition {
        if resolve {
            vec![local, resolve_stmt, assertion]
        } else {
            vec![local, assertion]
        }
    } else if matches!(condition, AssertionCondition::PathAfterDeferredNeg { .. }) {
        vec![local, neg_stmt, resolve_stmt, assertion]
    } else if matches!(
        condition,
        AssertionCondition::UntrustedPath
            | AssertionCondition::InferredPath
            | AssertionCondition::LocalBool
    ) {
        vec![local, assertion]
    } else {
        vec![assertion]
    };
    let tail = Expr {
        hir_id: node(10),
        kind: ExprKind::Lit(Lit::Bool(true)),
        span,
    };
    let block = Block {
        hir_id: node(11),
        stmts: &statements,
        tail: Some(&tail),
        safety: Safety::Safe,
        span,
    };
    let value = Expr {
        hir_id: node(12),
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
    let initial_bindings = match condition {
        AssertionCondition::Path(Some(primitive)) => HashMap::from([(binding, primitive)]),
        AssertionCondition::PathAfterInteger { .. }
        | AssertionCondition::PathAfterDeferredNeg { .. } => {
            HashMap::from([(parameter_binding, PrimTy::I32)])
        }
        _ => HashMap::new(),
    };
    let primitive_types =
        HashMap::from([(ty.hir_id, PrimTy::Bool), (resolved_ty.hir_id, PrimTy::I64)]);
    let path_binding = if matches!(
        condition,
        AssertionCondition::PathAfterInteger { .. }
            | AssertionCondition::PathAfterDeferredNeg { .. }
    ) {
        parameter_binding
    } else {
        binding
    };
    let local_paths = HashMap::from([
        (path.hir_id, path_binding),
        (local_path.hir_id, binding),
        (neg_path.hir_id, binding),
    ]);
    let input = PrimitiveBodyInput {
        body: &body,
        return_type: PrimTy::Bool,
        initial_bindings: &initial_bindings,
        primitive_types: &primitive_types,
        self_field_projections: None,
        unknown_self_fields: None,
        local_paths: &local_paths,
        call_signatures: None,
        symbols: &symbols,
        pointer_width: 64,
    };
    let result = if opt_in {
        check_primitive_local_block_body_with_assertions(input)
    } else {
        check_primitive_local_block_body(input)
    };
    (result, cond.hir_id, tail.hir_id)
}

#[test]
fn assertion_path_rejects_pending_literal_diagnostics() {
    for value in [1u128 << 63, 1] {
        let condition = AssertionCondition::PathAfterInteger {
            value,
            resolve: false,
        };
        assert_eq!(
            check_assertion(condition, false, false).0,
            PrimitiveBodyCheck::Unsupported
        );
        assert_eq!(
            check_assertion(condition, false, true).0,
            PrimitiveBodyCheck::Unsupported
        );
    }
    let condition = AssertionCondition::PathAfterInteger {
        value: 1,
        resolve: true,
    };
    let (result, cond, _) = check_assertion(condition, false, true);
    let PrimitiveBodyCheck::Supported { errors, .. } = result else {
        panic!("expected support")
    };
    assert_eq!(
        errors,
        vec![PrimitiveBodyError {
            node: cond,
            kind: PrimitiveBodyErrorKind::TypeMismatch {
                expected: PrimTy::Bool,
                found: PrimTy::I32
            },
        }]
    );
    let condition = AssertionCondition::PathAfterInteger {
        value: 1u128 << 63,
        resolve: true,
    };
    assert_eq!(
        check_assertion(condition, false, true).0,
        PrimitiveBodyCheck::Unsupported
    );
    assert_eq!(
        check_assertion(
            AssertionCondition::PathAfterDeferredNeg { literal: false },
            false,
            true
        )
        .0,
        PrimitiveBodyCheck::Unsupported
    );
    let (result, _, _) = check_assertion(
        AssertionCondition::PathAfterDeferredNeg { literal: true },
        false,
        true,
    );
    assert!(matches!(result, PrimitiveBodyCheck::Supported { errors, .. } if errors.is_empty()));
}

#[test]
fn assertion_conditions_are_checked_and_recorded_only_by_opt_in() {
    for condition in [
        AssertionCondition::Literal(Lit::Bool(false)),
        AssertionCondition::Path(Some(PrimTy::Bool)),
        AssertionCondition::LocalBool,
    ] {
        let (legacy, _, _) = check_assertion(condition, false, false);
        assert_eq!(legacy, PrimitiveBodyCheck::Unsupported);
        let (result, cond, tail) = check_assertion(condition, false, true);
        let PrimitiveBodyCheck::Supported {
            errors,
            type_results,
        } = result
        else {
            panic!("expected support")
        };
        assert!(errors.is_empty());
        assert_eq!(type_results.expression_type(cond), Some(PrimTy::Bool));
        assert_eq!(type_results.expression_type(tail), Some(PrimTy::Bool));
    }
}

#[test]
fn assertion_condition_mismatches_are_anchored_and_not_trusted() {
    for (condition, expected) in [
        (
            AssertionCondition::Literal(Lit::Int {
                value: Some(1),
                negated: false,
            }),
            PrimitiveBodyErrorKind::LiteralTypeMismatch {
                expected: PrimTy::Bool,
                found: PrimitiveLiteralKind::Integer,
            },
        ),
        (
            AssertionCondition::Path(Some(PrimTy::I32)),
            PrimitiveBodyErrorKind::TypeMismatch {
                expected: PrimTy::Bool,
                found: PrimTy::I32,
            },
        ),
    ] {
        let (result, cond, tail) = check_assertion(condition, false, true);
        let PrimitiveBodyCheck::Supported {
            errors,
            type_results,
        } = result
        else {
            panic!("expected support")
        };
        assert_eq!(
            errors,
            vec![PrimitiveBodyError {
                node: cond,
                kind: expected
            }]
        );
        assert_eq!(type_results.expression_type(cond), None);
        assert_eq!(type_results.expression_type(tail), Some(PrimTy::Bool));
    }
}

#[test]
fn assertion_slice_rejects_unchecked_forms() {
    for (condition, message) in [
        (AssertionCondition::Literal(Lit::Bool(true)), true),
        (AssertionCondition::Unsupported, false),
        (AssertionCondition::Path(None), false),
        (AssertionCondition::Path(Some(PrimTy::Never)), false),
        (AssertionCondition::UntrustedPath, false),
        (AssertionCondition::InferredPath, false),
    ] {
        assert_eq!(
            check_assertion(condition, message, true).0,
            PrimitiveBodyCheck::Unsupported
        );
    }
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
