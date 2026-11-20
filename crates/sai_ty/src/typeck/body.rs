//! Narrow, compositional checks for flat primitive-local function bodies.

use super::literal::{
    PrimitiveLiteralResult, check_primitive_literal, is_integer_primitive,
    supports_primitive_literal,
};
use super::results::{
    PartialTypeckResults, PartialTypeckResultsBuildError, PartialTypeckResultsBuilder,
};
use crate::ty::TyInterner;
use sai_hir::body::Body;
use sai_hir::expressions::{BinOp, Block, Expr, ExprKind, Stmt, StmtKind, UnOp};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, PatKind};
use sai_hir::types::PathRoot;
use sai_hir::{BindingId, HirId, OwnerId, Safety};
use sai_intern::Interner;
use sai_resolve::prelude::PrimTy;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Facts resolved by the owner-aware driver before body checking.
///
/// The maps are intentionally small and snapshot-independent. The driver
/// inserts only direct built-in primitive type paths, eligible immutable
/// parameter types, and local path resolutions from the current owner analysis.
pub struct PrimitiveBodyInput<'a, 'hir> {
    pub body: &'a Body<'hir>,
    pub return_type: PrimTy,
    pub initial_bindings: &'a HashMap<BindingId, PrimTy>,
    pub primitive_types: &'a HashMap<HirId, PrimTy>,
    /// Direct `self.field` projection types resolved by the owner-aware
    /// driver. Other field expressions remain outside this body slice.
    pub self_field_projections: Option<&'a HashMap<HirId, PrimTy>>,
    /// Direct `self.field` projections whose names were diagnosed as
    /// unknown by the owner-aware driver. These act as error types so later
    /// local uses do not produce cascading primitive-type errors.
    pub unknown_self_fields: Option<&'a HashSet<HirId>>,
    pub local_paths: &'a HashMap<HirId, BindingId>,
    /// Direct, supported function signatures resolved for call expressions
    /// in this owner's simple block.
    pub call_signatures: Option<&'a HashMap<HirId, PrimitiveCallSignature>>,
    /// `var` parameter bindings admitted by the owner-aware driver — the
    /// assignable parameters (§7.2.4).
    pub mutable_params: Option<&'a HashSet<BindingId>>,
    pub symbols: &'a Interner,
    pub pointer_width: u8,
}

/// A direct function signature projected by the owner-aware driver for one
/// call site. The call site and callee identities are both checked before use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveCallSignature {
    pub callee: HirId,
    pub parameters: Vec<PrimTy>,
    pub output: PrimTy,
}

/// Result of checking a function body in the annotated-local slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrimitiveBodyCheck {
    /// The owner or expression shape is outside this slice.
    Unsupported,
    /// The shape is supported; errors, if any, are anchored by HIR node IDs.
    /// Trustworthy type results are an owner-scoped partial projection and may
    /// be present alongside diagnostics for other expressions.
    Supported {
        errors: Vec<PrimitiveBodyError>,
        type_results: PrimitiveBodyTypeResults,
    },
}

/// Trustworthy primitive facts produced by the bounded primitive-local body
/// checker.
///
/// This is deliberately not a complete type-checking result: unsupported,
/// unresolved, and error-tainted expressions or bindings are omitted. The
/// owner is explicit because `BindingId` is only meaningful within one HIR
/// owner; each `HirId` is also checked against this owner before insertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveBodyTypeResults {
    partial_results: PartialTypeckResults,
}

impl PrimitiveBodyTypeResults {
    fn from_partial_results(partial_results: PartialTypeckResults) -> Self {
        Self { partial_results }
    }

    /// Add sparse canonical primitive type facts using the session's shared
    /// semantic interner.
    ///
    /// The underlying primitive facts are already taint-filtered. This only
    /// derives canonical types for those exact facts and cannot restore an
    /// omitted expression or binding.
    pub fn with_canonical_primitive_types(mut self, interner: &TyInterner) -> Self {
        self.partial_results = self
            .partial_results
            .with_canonical_primitive_types(interner);
        self
    }

    pub const fn owner(&self) -> OwnerId {
        self.partial_results.owner()
    }

    pub fn expression_type(&self, expression: HirId) -> Option<PrimTy> {
        self.partial_results.expression_type(expression)
    }

    pub fn binding_type(&self, binding: BindingId) -> Option<PrimTy> {
        self.partial_results.binding_type(binding)
    }

    pub fn expression_types(&self) -> &HashMap<HirId, PrimTy> {
        self.partial_results.expression_types()
    }

    pub fn binding_types(&self) -> &HashMap<BindingId, PrimTy> {
        self.partial_results.binding_types()
    }

    /// The sparse owner-scoped facts underlying this compatibility view.
    ///
    /// The returned value remains primitive-only and does not satisfy the
    /// complete `TypeckResults` contract.
    pub fn partial_results(&self) -> &PartialTypeckResults {
        &self.partial_results
    }
}

/// One semantic error found in a supported body shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrimitiveBodyError {
    pub node: HirId,
    pub kind: PrimitiveBodyErrorKind,
}

/// The semantic error kind, without snapshot-bound diagnostics or source spans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveBodyErrorKind {
    LiteralTypeMismatch {
        expected: PrimTy,
        found: PrimitiveLiteralKind,
    },
    LiteralOutOfRange {
        expected: PrimTy,
        found: PrimitiveLiteralKind,
    },
    TypeMismatch {
        expected: PrimTy,
        found: PrimTy,
    },
    OperatorNotSupported {
        operator: BinOp,
        operand: PrimitiveOperatorOperand,
    },
    OperatorTraitNotSupported {
        operator: BinOp,
        left: PrimTy,
        right: PrimTy,
    },
    DeferredOperatorTraitNotSupported {
        operator: BinOp,
        left: PrimTy,
        right: PrimTy,
    },
    UnaryOperatorNotSupported {
        operator: UnOp,
        operand: PrimTy,
    },
    WrongArgumentCount {
        expected: usize,
        found: usize,
    },
    NamedArgumentOnFunction {
        name_span: sai_hir::Span,
    },
    AssignToImmutable,
    InvalidAssignTarget,
}

/// An operand description for an unsupported primitive operator diagnostic.
/// Float literals can still be unresolved when checked, so they must not be
/// reported as a concrete `F64` prematurely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveOperatorOperand {
    Concrete(PrimTy),
    FloatingPointLiteral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimitiveComparisonResult {
    Bool,
    TypeMismatch { expected: PrimTy, found: PrimTy },
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimitiveArithmeticResult {
    Type(PrimTy),
    TypeMismatch { expected: PrimTy, found: PrimTy },
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimitiveBoolOperandResult {
    Bool,
    TypeMismatch { found: PrimTy },
    Unsupported,
}

/// Scalar literal classes supported by the primitive literal kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveLiteralKind {
    Integer,
    FloatingPoint,
    Bool,
    Byte,
    Character,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalType {
    Concrete(PrimTy),
    LiteralVariable(usize),
    ErrorLiteralVariable(usize),
    Error(Option<PrimTy>),
}

#[derive(Clone, Copy, Debug)]
struct LiteralSite<'hir> {
    literal: Lit<'hir>,
    origin: HirId,
}

#[derive(Clone, Copy, Debug)]
struct PendingOperatorTraitSite {
    operator: BinOp,
    expression: HirId,
    diagnostic_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct PendingNegTraitSite {
    expression: HirId,
    diagnostic_index: usize,
}

#[derive(Clone, Debug)]
struct LiteralVariable<'hir> {
    literal: Lit<'hir>,
    kind: PrimitiveLiteralKind,
    origin: HirId,
    additional_site: Option<LiteralSite<'hir>>,
    resolved: Option<PrimTy>,
    pending_operator_traits: Vec<PendingOperatorTraitSite>,
    pending_neg_traits: Vec<PendingNegTraitSite>,
}

/// Check a flat sequence of immutable primitive locals and a local-path tail.
///
/// Direct scalar literals, plain paths to earlier bindings, comparisons
/// between concrete primitive paths or one path and one scalar literal, one
/// top-level logical operation between Bool paths, and direct arithmetic
/// between matching concrete numeric paths are accepted as initializers. A
/// Direct numeric literals are accepted on either side of `+`, `-`, `*`, and
/// `/` when paired with an eligible concrete numeric path. `%` accepts integer
/// paths and direct integer literals; float `%` returns an operator diagnostic.
/// Char-left bitwise diagnostics accept resolved numeric literal computations
/// through unannotated plain-path copies when no additional literal site exists.
/// They also defer through unannotated copies of direct integer-literal
/// addition, subtraction, multiplication, division, remainder, bitwise, and
/// wrapping-arithmetic pairs, preserving both literal sites on the shared
/// inference variable.
/// Unary `~` accepts direct integer/Byte literals, concrete integer/Byte
/// paths, and unresolved direct integer-literal paths with simple aliases.
/// Unary `-` in a direct-call argument also defers through an unresolved
/// direct integer- or float-literal local or its simple unannotated
/// plain-path alias so the parameter expectation resolves the shared
/// literal variable before the `Neg` trait check runs.
/// Unannotated numeric literals retain one inference variable across copies
/// until a coercion site or the tail constrains it. Other body forms remain
/// unsupported until their own type-checking slices are implemented.
pub fn check_primitive_local_block_body(input: PrimitiveBodyInput<'_, '_>) -> PrimitiveBodyCheck {
    if !supports_primitive_literal(input.return_type) {
        return PrimitiveBodyCheck::Unsupported;
    }
    let binding_nodes = input
        .body
        .params
        .iter()
        .map(|parameter| (parameter.binding, parameter.hir_id))
        .collect();
    let known_bindings: HashMap<BindingId, LocalType> = input
        .initial_bindings
        .iter()
        .map(|(binding, ty)| (*binding, LocalType::Concrete(*ty)))
        .collect();
    let binding_type_candidates = known_bindings.clone();
    let trusted_bindings = input.initial_bindings.keys().copied().collect();
    let mut expression_nodes = HashMap::new();
    if matches!(input.body.value.kind, ExprKind::Block(_)) {
        collect_primitive_expression_nodes(&input.body.value, &mut expression_nodes);
    }
    let mutable_bindings: HashSet<BindingId> = input
        .mutable_params
        .map(|params| params.iter().copied().collect())
        .unwrap_or_default();
    let mut context = FnCtxt {
        input,
        known_bindings,
        mutable_bindings,
        binding_type_candidates,
        binding_candidate_order: Vec::new(),
        binding_initializer_roots: HashMap::new(),
        binding_nodes,
        annotated_bindings: HashSet::new(),
        trusted_bindings,
        expression_type_candidates: RefCell::new(HashMap::new()),
        expression_nodes,
        direct_unannotated_integer_literals: HashSet::new(),
        unannotated_integer_literal_aliases: HashSet::new(),
        resolved_computed_literal_paths: HashSet::new(),
        unannotated_integer_addition_pair_paths: HashSet::new(),
        unannotated_integer_subtraction_pair_paths: HashSet::new(),
        unannotated_integer_multiplication_pair_paths: HashSet::new(),
        unannotated_integer_division_pair_paths: HashSet::new(),
        unannotated_integer_remainder_pair_paths: HashSet::new(),
        unannotated_integer_wrapping_pair_paths: HashSet::new(),
        unannotated_integer_bitwise_pair_paths: HashSet::new(),
        direct_unannotated_float_literals: HashSet::new(),
        unannotated_float_literal_aliases: HashSet::new(),
        deferred_neg_result_bindings: HashSet::new(),
        deferred_bitnot_result_bindings: HashSet::new(),
        pending_operator_trait_count: 0,
        literal_variables: Vec::new(),
        errors: Vec::new(),
    };
    let ExprKind::Block(block) = context.input.body.value.kind else {
        return PrimitiveBodyCheck::Unsupported;
    };
    if context.check_block(block).is_err() {
        return PrimitiveBodyCheck::Unsupported;
    }
    let type_results = context.primitive_body_type_results();
    PrimitiveBodyCheck::Supported {
        errors: context.errors,
        type_results,
    }
}

/// The checked result of an admissible value expression, with the
/// deferred-operator flags the `let` path uses for provenance tracking.
struct InitializerCheck {
    local_type: LocalType,
    deferred_neg_result: bool,
    deferred_bitnot_result: bool,
}

struct FnCtxt<'a, 'hir> {
    input: PrimitiveBodyInput<'a, 'hir>,
    known_bindings: HashMap<BindingId, LocalType>,
    mutable_bindings: HashSet<BindingId>,
    binding_type_candidates: HashMap<BindingId, LocalType>,
    binding_candidate_order: Vec<BindingId>,
    binding_initializer_roots: HashMap<BindingId, HirId>,
    binding_nodes: HashMap<BindingId, HirId>,
    annotated_bindings: HashSet<BindingId>,
    trusted_bindings: HashSet<BindingId>,
    expression_type_candidates: RefCell<HashMap<HirId, LocalType>>,
    expression_nodes: HashMap<HirId, Expr<'hir>>,
    direct_unannotated_integer_literals: HashSet<BindingId>,
    unannotated_integer_literal_aliases: HashSet<BindingId>,
    resolved_computed_literal_paths: HashSet<BindingId>,
    unannotated_integer_addition_pair_paths: HashSet<BindingId>,
    unannotated_integer_subtraction_pair_paths: HashSet<BindingId>,
    unannotated_integer_multiplication_pair_paths: HashSet<BindingId>,
    unannotated_integer_division_pair_paths: HashSet<BindingId>,
    unannotated_integer_remainder_pair_paths: HashSet<BindingId>,
    unannotated_integer_wrapping_pair_paths: HashSet<BindingId>,
    unannotated_integer_bitwise_pair_paths: HashSet<BindingId>,
    direct_unannotated_float_literals: HashSet<BindingId>,
    unannotated_float_literal_aliases: HashSet<BindingId>,
    deferred_neg_result_bindings: HashSet<BindingId>,
    deferred_bitnot_result_bindings: HashSet<BindingId>,
    pending_operator_trait_count: usize,
    literal_variables: Vec<LiteralVariable<'hir>>,
    errors: Vec<PrimitiveBodyError>,
}

impl<'a, 'hir> FnCtxt<'a, 'hir> {
    fn primitive_body_type_results(&self) -> PrimitiveBodyTypeResults {
        let owner = self.input.body.owner;
        let error_nodes = self
            .errors
            .iter()
            .map(|error| error.node)
            .collect::<HashSet<_>>();
        let unknown_self_fields = self.input.unknown_self_fields;
        let mut tainted_bindings = self
            .known_bindings
            .keys()
            .filter(|binding| {
                !self.trusted_bindings.contains(binding)
                    && !self.annotated_bindings.contains(binding)
            })
            .copied()
            .collect::<HashSet<_>>();
        // The supported checker visits one flat block in source order, and a
        // local path can resolve only to a parameter or an earlier binding.
        // A single pass therefore propagates taint transitively without
        // rescanning every candidate after each newly-tainted declaration.
        for binding in &self.binding_candidate_order {
            if self.annotated_bindings.contains(binding) || tainted_bindings.contains(binding) {
                continue;
            }
            let initializer = self
                .binding_initializer_roots
                .get(binding)
                .and_then(|initializer| self.expression_nodes.get(initializer));
            let initializer_is_tainted = initializer.map_or(true, |initializer| {
                expression_has_primitive_result_taint(
                    initializer,
                    &error_nodes,
                    unknown_self_fields,
                    self.input.local_paths,
                    &tainted_bindings,
                )
            });
            if initializer_is_tainted {
                tainted_bindings.insert(*binding);
            }
        }
        let expression_types: HashMap<HirId, PrimTy> = self
            .expression_type_candidates
            .borrow()
            .iter()
            .filter_map(|(expression_id, local_type)| {
                if expression_id.owner != owner {
                    return None;
                }
                let expression = self.expression_nodes.get(expression_id)?;
                if expression_has_primitive_result_taint(
                    expression,
                    &error_nodes,
                    unknown_self_fields,
                    self.input.local_paths,
                    &tainted_bindings,
                ) {
                    return None;
                }
                Some((*expression_id, self.concrete_local_type(*local_type)?))
            })
            .collect();
        let binding_types: HashMap<BindingId, PrimTy> = self
            .binding_type_candidates
            .iter()
            .filter_map(|(binding, local_type)| {
                if !self.trusted_bindings.contains(binding) {
                    return None;
                }
                if !self.annotated_bindings.contains(binding) && tainted_bindings.contains(binding)
                {
                    return None;
                }
                Some((*binding, self.concrete_local_type(*local_type)?))
            })
            .collect();
        let mut builder = PartialTypeckResultsBuilder::new(owner);
        for (expression, ty) in expression_types {
            builder
                .record_expression_type(expression, ty)
                .expect("the body checker filters expression results by exact owner");
        }
        for (binding, ty) in binding_types {
            let Some(binding_node) = self.binding_nodes.get(&binding).copied() else {
                // BindingId carries no owner, so never publish its primitive
                // type without an owner-checked parameter or pattern node.
                continue;
            };
            match builder.record_binding_type(binding, binding_node, ty) {
                Ok(()) => {}
                Err(PartialTypeckResultsBuildError::OwnerMismatch(_)) => {
                    // A binding HIR witness from another owner is not a fact
                    // of this body's partial results.
                    continue;
                }
                Err(PartialTypeckResultsBuildError::BindingNodeMismatch { .. }) => {
                    // One owner-local binding index must identify one exact
                    // parameter or pattern node.
                    continue;
                }
            }
        }
        PrimitiveBodyTypeResults::from_partial_results(builder.finish())
    }

    fn record_expression_type(&self, expression: HirId, local_type: LocalType) {
        if expression.owner == self.input.body.owner {
            self.expression_type_candidates
                .borrow_mut()
                .insert(expression, local_type);
        }
    }

    fn check_block(&mut self, block: &Block<'hir>) -> Result<(), ()> {
        if block.safety != Safety::Safe {
            return Err(());
        }
        // Only literals created inside this block may be defaulted here:
        // outer literal variables can still be constrained by sibling
        // branches or the outer tail after this block returns.
        let literal_floor = self.literal_variables.len();

        let Some(tail) = block.tail else {
            return Err(());
        };
        let is_supported_field_tail = matches!(tail.kind, ExprKind::Field { .. })
            && self
                .input
                .self_field_projections
                .is_some_and(|fields| fields.contains_key(&tail.hir_id))
            || matches!(tail.kind, ExprKind::Field { .. })
                && self
                    .input
                    .unknown_self_fields
                    .is_some_and(|fields| fields.contains(&tail.hir_id));
        let is_supported_call_tail = matches!(tail.kind, ExprKind::Call { base: None, .. })
            && self
                .input
                .call_signatures
                .is_some_and(|signatures| signatures.contains_key(&tail.hir_id));
        let is_supported_unary_tail = matches!(tail.kind, ExprKind::Unary(operator, operand) if matches!(operator, UnOp::Neg | UnOp::BitNot | UnOp::Not) && matches!(operand.kind, ExprKind::Path(_)));
        let is_supported_binary_tail = matches!(tail.kind, ExprKind::Binary(..));
        let is_supported_literal_tail = matches!(tail.kind, ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some());
        let is_supported_if_tail = matches!(tail.kind, ExprKind::If { els: Some(_), .. });
        if block.stmts.is_empty()
            && !is_supported_field_tail
            && !is_supported_call_tail
            && !is_supported_unary_tail
            && !is_supported_binary_tail
            && !is_supported_literal_tail
            && !is_supported_if_tail
        {
            return Err(());
        }
        for stmt in block.stmts {
            self.check_stmt(stmt)?;
        }

        self.check_expr(tail)?;
        self.default_unresolved_literals(literal_floor);
        Ok(())
    }

    fn check_stmt(&mut self, stmt: &Stmt<'hir>) -> Result<(), ()> {
        if let StmtKind::Expr(expression) = stmt.kind {
            return self.check_expr_stmt(expression);
        }
        if let StmtKind::Assert { cond, msg, .. } = stmt.kind {
            // `assert` conditions take the same Bool-condition gate as `if`,
            // and the optional message is a `Str` literal per N-C-20.
            self.check_if_condition(cond)?;
            if let Some(msg) = msg {
                if !matches!(msg.kind, ExprKind::Lit(Lit::Str(_))) {
                    return Err(());
                }
            }
            return Ok(());
        }
        if let StmtKind::Assign { lhs, rhs } = stmt.kind {
            return self.check_assign_stmt(lhs, rhs);
        }
        if let StmtKind::AssignOp { op, lhs, rhs } = stmt.kind {
            return self.check_assign_op_stmt(stmt.hir_id, op, lhs, rhs);
        }
        let StmtKind::Let {
            pat,
            ty,
            init,
            els: None,
        } = stmt.kind
        else {
            return Err(());
        };

        let PatKind::Binding {
            mode: mode @ (BindingMode::Value | BindingMode::Var),
            binding,
            sub: None,
            ..
        } = pat.kind
        else {
            return Err(());
        };
        self.binding_nodes.insert(binding, pat.hir_id);
        if mode == BindingMode::Var {
            self.mutable_bindings.insert(binding);
        }

        let annotation = match ty {
            Some(ty) => {
                let Some(primitive) = self.input.primitive_types.get(&ty.hir_id).copied() else {
                    return Err(());
                };
                if !supports_primitive_literal(primitive) {
                    return Err(());
                }
                Some(primitive)
            }
            None => None,
        };

        let errors_before_initializer = self.errors.len();
        let InitializerCheck {
            local_type,
            deferred_neg_result: initializer_is_deferred_neg_result,
            deferred_bitnot_result: initializer_is_deferred_bitnot_result,
        } = self.check_initializer_value(init, annotation)?;

        if initializer_is_deferred_neg_result {
            self.deferred_neg_result_bindings.insert(binding);
        }
        if initializer_is_deferred_bitnot_result {
            self.deferred_bitnot_result_bindings.insert(binding);
        }

        if annotation.is_none()
            && matches!(
                init.kind,
                ExprKind::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div, _, _)
            )
        {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    matches!(
                        variable.kind,
                        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                    ) && variable.resolved.is_some()
                        && variable.additional_site.is_none()
                }) {
                    self.resolved_computed_literal_paths.insert(binding);
                }
            }
        }

        let has_direct_integer_bitwise_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(literal)
                    if primitive_literal_kind(literal)
                        == Some(PrimitiveLiteralKind::Integer)
            ) && matches!(
                right.kind,
                ExprKind::Lit(literal)
                    if primitive_literal_kind(literal)
                        == Some(PrimitiveLiteralKind::Integer)
            )
        );
        if annotation.is_none() && has_direct_integer_bitwise_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_bitwise_pair_paths.insert(binding);
                }
            }
        }

        let has_direct_integer_addition_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::Add,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_addition_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_addition_pair_paths.insert(binding);
                }
            }
        }

        let has_direct_integer_subtraction_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::Sub,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_subtraction_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_subtraction_pair_paths
                        .insert(binding);
                }
            }
        }

        let has_direct_integer_multiplication_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::Mul,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_multiplication_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_multiplication_pair_paths
                        .insert(binding);
                }
            }
        }

        let has_direct_integer_division_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::Div,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_division_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_division_pair_paths.insert(binding);
                }
            }
        }

        let has_direct_integer_remainder_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::Rem,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_remainder_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_remainder_pair_paths
                        .insert(binding);
                }
            }
        }

        let has_direct_integer_wrapping_literal_pair = matches!(
            init.kind,
            ExprKind::Binary(
                BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul,
                left,
                right
            ) if matches!(
                left.kind,
                ExprKind::Lit(Lit::Int { .. })
            ) && matches!(
                right.kind,
                ExprKind::Lit(Lit::Int { .. })
            )
        );
        if annotation.is_none() && has_direct_integer_wrapping_literal_pair {
            if let LocalType::LiteralVariable(id) = local_type {
                if self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer
                        && variable.resolved.is_none()
                        && variable.additional_site.is_some()
                }) {
                    self.unannotated_integer_wrapping_pair_paths.insert(binding);
                }
            }
        }

        if annotation.is_none() {
            if let ExprKind::Lit(literal) = init.kind {
                match primitive_literal_kind(literal) {
                    Some(PrimitiveLiteralKind::Integer) => {
                        self.direct_unannotated_integer_literals.insert(binding);
                    }
                    Some(PrimitiveLiteralKind::FloatingPoint) => {
                        self.direct_unannotated_float_literals.insert(binding);
                    }
                    Some(
                        PrimitiveLiteralKind::Bool
                        | PrimitiveLiteralKind::Byte
                        | PrimitiveLiteralKind::Character,
                    )
                    | None => {}
                }
            }
        }
        self.record_expression_type(init.hir_id, local_type);
        if let Some(annotation) = annotation {
            self.binding_type_candidates
                .insert(binding, LocalType::Concrete(annotation));
            self.binding_candidate_order.push(binding);
            self.annotated_bindings.insert(binding);
            self.binding_initializer_roots.insert(binding, init.hir_id);
            self.trusted_bindings.insert(binding);
        } else if self.errors.len() == errors_before_initializer
            && !matches!(
                local_type,
                LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
            )
        {
            self.binding_type_candidates.insert(binding, local_type);
            self.binding_candidate_order.push(binding);
            self.binding_initializer_roots.insert(binding, init.hir_id);
            self.trusted_bindings.insert(binding);
        }
        if let Some(source_binding) = self.plain_local_path_binding(init) {
            if self.deferred_neg_result_bindings.contains(&source_binding) {
                self.deferred_neg_result_bindings.insert(binding);
            }
            if self
                .deferred_bitnot_result_bindings
                .contains(&source_binding)
            {
                self.deferred_bitnot_result_bindings.insert(binding);
            }

            let source_has_resolved_computed_literal_provenance = self
                .resolved_computed_literal_paths
                .contains(&source_binding);
            if can_track_resolved_computed_literal_alias(
                source_has_resolved_computed_literal_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.resolved_computed_literal_paths.insert(binding);
            }

            let source_has_integer_bitwise_pair_provenance = self
                .unannotated_integer_bitwise_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_bitwise_pair_alias(
                source_has_integer_bitwise_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_bitwise_pair_paths.insert(binding);
            }

            let source_has_integer_addition_pair_provenance = self
                .unannotated_integer_addition_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_addition_pair_alias(
                source_has_integer_addition_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_addition_pair_paths.insert(binding);
            }

            let source_has_integer_subtraction_pair_provenance = self
                .unannotated_integer_subtraction_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_subtraction_pair_alias(
                source_has_integer_subtraction_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_subtraction_pair_paths
                    .insert(binding);
            }

            let source_has_integer_multiplication_pair_provenance = self
                .unannotated_integer_multiplication_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_multiplication_pair_alias(
                source_has_integer_multiplication_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_multiplication_pair_paths
                    .insert(binding);
            }

            let source_has_integer_division_pair_provenance = self
                .unannotated_integer_division_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_division_pair_alias(
                source_has_integer_division_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_division_pair_paths.insert(binding);
            }

            let source_has_integer_remainder_pair_provenance = self
                .unannotated_integer_remainder_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_remainder_pair_alias(
                source_has_integer_remainder_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_remainder_pair_paths
                    .insert(binding);
            }

            let source_has_integer_wrapping_pair_provenance = self
                .unannotated_integer_wrapping_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_wrapping_pair_alias(
                source_has_integer_wrapping_pair_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_wrapping_pair_paths.insert(binding);
            }

            let source_has_integer_literal_provenance = self
                .direct_unannotated_integer_literals
                .contains(&source_binding)
                || self
                    .unannotated_integer_literal_aliases
                    .contains(&source_binding);
            if can_track_unannotated_integer_literal_alias(
                source_has_integer_literal_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_literal_aliases.insert(binding);
            }
            let source_has_float_literal_provenance = self
                .direct_unannotated_float_literals
                .contains(&source_binding)
                || self
                    .unannotated_float_literal_aliases
                    .contains(&source_binding);
            if can_track_unannotated_float_literal_alias(
                source_has_float_literal_provenance,
                matches!(init.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_float_literal_aliases.insert(binding);
            }
        }
        self.known_bindings.insert(binding, local_type);
        Ok(())
    }

    fn check_arithmetic(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        if operator == BinOp::Rem {
            return self.check_remainder(left, right, expression);
        }
        if matches!(operator, BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul) {
            return self.check_wrapping(operator, left, right, expression);
        }

        if let ExprKind::Lit(literal) = left.kind {
            let Some(kind) = primitive_literal_kind(literal) else {
                return Err(());
            };
            if matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                && kind == PrimitiveLiteralKind::Integer
                && matches!(right.kind, ExprKind::Lit(Lit::Int { .. }))
            {
                let local_type =
                    self.local_type_for_unannotated_literal(literal, kind, left.hir_id);
                let LocalType::LiteralVariable(id) = local_type else {
                    return Err(());
                };
                let ExprKind::Lit(right_literal) = right.kind else {
                    return Err(());
                };
                self.add_integer_literal_constraint(id, right_literal, right.hir_id)?;
                return Ok(local_type);
            }
            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .ok_or(())?;

            return match primitive_arithmetic_literal_left_result(operator, kind, right_type) {
                PrimitiveArithmeticResult::Type(expected) => {
                    let local_type =
                        self.local_type_for_unannotated_literal(literal, kind, left.hir_id);
                    let LocalType::LiteralVariable(id) = local_type else {
                        return Err(());
                    };
                    self.resolve_literal_variable(id, expected)?;
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                    let local_type =
                        self.local_type_for_unannotated_literal(literal, kind, left.hir_id);
                    self.errors.push(PrimitiveBodyError {
                        node: right.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                    });
                    // A failed family constraint leaves the synthesized left
                    // literal variable available to an annotation or tail.
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::Unsupported => Err(()),
            };
        }

        let left_type = self
            .check_local_path(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .ok_or(())?;

        if let ExprKind::Lit(literal) = right.kind {
            let Some(found) = primitive_literal_kind(literal) else {
                return Err(());
            };
            let Some(expected) =
                primitive_arithmetic_literal_expected_type(operator, left_type, found)
            else {
                return Err(());
            };
            self.check_annotated_literal(expected, literal, found, right.hir_id);
            // The built-in operator's result is determined by the synthesized
            // left operand, including after a literal type or range error.
            return Ok(LocalType::Concrete(left_type));
        }

        let right_type = self
            .check_local_path(right)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .ok_or(())?;

        match primitive_arithmetic_result(operator, left_type, right_type) {
            PrimitiveArithmeticResult::Type(result) => Ok(LocalType::Concrete(result)),
            PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                });
                // Primitive arithmetic synthesizes its result from the left
                // operand, so recover with that type after a right mismatch.
                Ok(LocalType::Concrete(left_type))
            }
            PrimitiveArithmeticResult::Unsupported => Err(()),
        }
    }

    fn check_remainder(
        &mut self,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        if let (
            ExprKind::Lit(left_literal @ Lit::Int { .. }),
            ExprKind::Lit(right_literal @ Lit::Int { .. }),
        ) = (left.kind, right.kind)
        {
            let local_type = self.local_type_for_unannotated_literal(
                left_literal,
                PrimitiveLiteralKind::Integer,
                left.hir_id,
            );
            let LocalType::LiteralVariable(id) = local_type else {
                return Err(());
            };
            self.add_integer_literal_constraint(id, right_literal, right.hir_id)?;
            return Ok(local_type);
        }

        if let ExprKind::Lit(literal) = left.kind {
            let Some(kind @ (PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint)) =
                primitive_literal_kind(literal)
            else {
                return Err(());
            };
            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .ok_or(())?;
            if !is_arithmetic_primitive(right_type) {
                return Err(());
            }

            let local_type = self.local_type_for_unannotated_literal(literal, kind, left.hir_id);
            if literal_family_accepts(kind, right_type) {
                let LocalType::LiteralVariable(id) = local_type else {
                    return Err(());
                };
                self.resolve_literal_variable(id, right_type)?;
                if !is_integer_primitive(right_type) {
                    self.report_unsupported_float_remainder(expression, right_type);
                }
            } else {
                let expected = match kind {
                    PrimitiveLiteralKind::Integer => PrimTy::I64,
                    PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
                    PrimitiveLiteralKind::Bool
                    | PrimitiveLiteralKind::Byte
                    | PrimitiveLiteralKind::Character => unreachable!(),
                };
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected,
                        found: right_type,
                    },
                });
                // A floating literal selects the float remainder operator
                // family even when the right path fails its family check.
                if kind == PrimitiveLiteralKind::FloatingPoint {
                    self.errors.push(PrimitiveBodyError {
                        node: expression,
                        kind: PrimitiveBodyErrorKind::OperatorNotSupported {
                            operator: BinOp::Rem,
                            operand: PrimitiveOperatorOperand::FloatingPointLiteral,
                        },
                    });
                }
            }
            return Ok(local_type);
        }

        let left_type = self
            .check_local_path(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .ok_or(())?;
        if !is_arithmetic_primitive(left_type) {
            return Err(());
        }

        if let ExprKind::Lit(literal) = right.kind {
            let Some(found) = primitive_literal_kind(literal) else {
                return Err(());
            };
            if !matches!(
                found,
                PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
            ) {
                return Err(());
            }
            self.check_annotated_literal(left_type, literal, found, right.hir_id);
        } else {
            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .ok_or(())?;
            if !is_arithmetic_primitive(right_type) {
                return Err(());
            }
            if left_type != right_type {
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: left_type,
                        found: right_type,
                    },
                });
            }
        }

        if !is_integer_primitive(left_type) {
            self.report_unsupported_float_remainder(expression, left_type);
        }
        Ok(LocalType::Concrete(left_type))
    }

    fn report_unsupported_float_remainder(&mut self, node: HirId, operand: PrimTy) {
        self.report_operator_not_supported(
            node,
            BinOp::Rem,
            PrimitiveOperatorOperand::Concrete(operand),
        );
    }

    fn check_wrapping(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        if let (
            ExprKind::Lit(left_literal @ Lit::Int { .. }),
            ExprKind::Lit(right_literal @ Lit::Int { .. }),
        ) = (left.kind, right.kind)
        {
            let local_type = self.local_type_for_unannotated_literal(
                left_literal,
                PrimitiveLiteralKind::Integer,
                left.hir_id,
            );
            let LocalType::LiteralVariable(id) = local_type else {
                return Err(());
            };
            self.add_integer_literal_constraint(id, right_literal, right.hir_id)?;
            return Ok(local_type);
        }

        if let ExprKind::Lit(literal) = left.kind {
            if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer) {
                return Err(());
            }
            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .ok_or(())?;
            let local_type = self.local_type_for_unannotated_literal(
                literal,
                PrimitiveLiteralKind::Integer,
                left.hir_id,
            );
            let LocalType::LiteralVariable(id) = local_type else {
                return Err(());
            };

            return match primitive_wrapping_literal_left_result(
                operator,
                PrimitiveLiteralKind::Integer,
                right_type,
            ) {
                PrimitiveArithmeticResult::Type(expected) => {
                    self.resolve_literal_variable(id, expected)?;
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                    self.errors.push(PrimitiveBodyError {
                        node: right.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                    });
                    // Keep the left integer literal unresolved so a later
                    // annotation or tail can still constrain its type.
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::Unsupported => Err(()),
            };
        }

        let left_type = self
            .check_local_path(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;
        if let ExprKind::Lit(literal) = right.kind {
            if !is_integer_primitive(left_type)
                || primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer)
            {
                return Err(());
            }
            self.check_annotated_literal(
                left_type,
                literal,
                PrimitiveLiteralKind::Integer,
                right.hir_id,
            );
            return Ok(LocalType::Concrete(left_type));
        }
        let right_type = self
            .check_local_path(right)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;

        // Operand typing for primitive numeric, Byte, and Bool left sides is
        // left-directed. Char reaches the missing-trait operator diagnostic
        // without a synthetic right-type mismatch.
        if left_type != right_type && left_type != PrimTy::Char {
            self.errors.push(PrimitiveBodyError {
                node: right.hir_id,
                kind: PrimitiveBodyErrorKind::TypeMismatch {
                    expected: left_type,
                    found: right_type,
                },
            });
        }
        if !is_integer_primitive(left_type) {
            self.report_operator_not_supported(
                expression,
                operator,
                PrimitiveOperatorOperand::Concrete(left_type),
            );
        }
        Ok(LocalType::Concrete(left_type))
    }

    fn check_bitwise(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        if let ExprKind::Lit(literal) = left.kind {
            if primitive_literal_kind(literal) == Some(PrimitiveLiteralKind::Byte) {
                if let ExprKind::Lit(right_literal) = right.kind {
                    let Some(kind) = primitive_literal_kind(right_literal) else {
                        return Err(());
                    };
                    if kind == PrimitiveLiteralKind::Integer {
                        self.check_annotated_literal(
                            PrimTy::Byte,
                            right_literal,
                            PrimitiveLiteralKind::Integer,
                            right.hir_id,
                        );
                        return Ok(LocalType::Concrete(PrimTy::Byte));
                    }
                    if matches!(
                        kind,
                        PrimitiveLiteralKind::Bool
                            | PrimitiveLiteralKind::Character
                            | PrimitiveLiteralKind::FloatingPoint
                    ) {
                        self.check_annotated_literal(
                            PrimTy::Byte,
                            right_literal,
                            kind,
                            right.hir_id,
                        );
                        return Ok(LocalType::Concrete(PrimTy::Byte));
                    }
                    return match primitive_bitwise_byte_literal_pair_result(operator, kind) {
                        PrimitiveArithmeticResult::Type(result) => Ok(LocalType::Concrete(result)),
                        PrimitiveArithmeticResult::TypeMismatch { .. }
                        | PrimitiveArithmeticResult::Unsupported => Err(()),
                    };
                }

                let right_type = self
                    .check_local_path(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                    .filter(|primitive| supports_primitive_literal(*primitive))
                    .ok_or(())?;

                return match primitive_bitwise_byte_literal_left_result(operator, right_type) {
                    PrimitiveArithmeticResult::Type(result) => Ok(LocalType::Concrete(result)),
                    PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                        Ok(LocalType::Concrete(PrimTy::Byte))
                    }
                    PrimitiveArithmeticResult::Unsupported => Err(()),
                };
            }

            if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer) {
                if primitive_literal_kind(literal) == Some(PrimitiveLiteralKind::Bool) {
                    let ExprKind::Lit(right_literal) = right.kind else {
                        return Err(());
                    };
                    let Some(right_kind) = primitive_literal_kind(right_literal) else {
                        return Err(());
                    };
                    if !primitive_bitwise_bool_literal_pair_supported(operator, right_kind) {
                        return Err(());
                    }

                    // Bool has no bitwise operators. Keep the direct literal
                    // pair in the supported diagnostic slice and recover it
                    // as Bool for annotations, following locals, and tails.
                    self.report_operator_not_supported(
                        expression,
                        operator,
                        PrimitiveOperatorOperand::Concrete(PrimTy::Bool),
                    );
                    return Ok(LocalType::Concrete(PrimTy::Bool));
                }
                if primitive_literal_kind(literal) == Some(PrimitiveLiteralKind::Character) {
                    let ExprKind::Lit(right_literal) = right.kind else {
                        let right_local_type = self.check_local_path(right)?;
                        let eligible_unannotated_literal_path =
                            self.is_eligible_unannotated_literal_path(right);
                        let eligible_resolved_computed_literal_path =
                            self.is_eligible_resolved_computed_literal_path(right);
                        let eligible_unannotated_integer_bitwise_pair_path =
                            self.is_eligible_unannotated_integer_bitwise_pair_path(right);
                        let eligible_unannotated_integer_addition_pair_path =
                            self.is_eligible_unannotated_integer_addition_pair_path(right);
                        let eligible_unannotated_integer_subtraction_pair_path =
                            self.is_eligible_unannotated_integer_subtraction_pair_path(right);
                        let eligible_unannotated_integer_multiplication_pair_path =
                            self.is_eligible_unannotated_integer_multiplication_pair_path(right);
                        let eligible_unannotated_integer_division_pair_path =
                            self.is_eligible_unannotated_integer_division_pair_path(right);
                        let eligible_unannotated_integer_remainder_pair_path =
                            self.is_eligible_unannotated_integer_remainder_pair_path(right);
                        let eligible_unannotated_integer_wrapping_pair_path =
                            self.is_eligible_unannotated_integer_wrapping_pair_path(right);
                        let right_type = self
                            .concrete_local_type(right_local_type)
                            .filter(|primitive| supports_primitive_literal(*primitive));
                        if let Some(right_type) = right_type {
                            if matches!(right_local_type, LocalType::LiteralVariable(_))
                                && !eligible_unannotated_literal_path
                                && !eligible_resolved_computed_literal_path
                                && !eligible_unannotated_integer_bitwise_pair_path
                                && !eligible_unannotated_integer_addition_pair_path
                                && !eligible_unannotated_integer_subtraction_pair_path
                                && !eligible_unannotated_integer_multiplication_pair_path
                                && !eligible_unannotated_integer_division_pair_path
                                && !eligible_unannotated_integer_remainder_pair_path
                                && !eligible_unannotated_integer_wrapping_pair_path
                            {
                                return Err(());
                            }
                            self.errors.push(PrimitiveBodyError {
                                node: expression,
                                kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                                    operator,
                                    left: PrimTy::Char,
                                    right: right_type,
                                },
                            });
                            return Ok(LocalType::Concrete(PrimTy::Char));
                        }

                        if !eligible_unannotated_literal_path
                            && !eligible_unannotated_integer_bitwise_pair_path
                            && !eligible_unannotated_integer_addition_pair_path
                            && !eligible_unannotated_integer_subtraction_pair_path
                            && !eligible_unannotated_integer_multiplication_pair_path
                            && !eligible_unannotated_integer_division_pair_path
                            && !eligible_unannotated_integer_remainder_pair_path
                            && !eligible_unannotated_integer_wrapping_pair_path
                        {
                            return Err(());
                        }
                        let LocalType::LiteralVariable(id) = right_local_type else {
                            return Err(());
                        };
                        let diagnostic_index =
                            self.errors.len() + self.pending_operator_trait_count;
                        self.pending_operator_trait_count += 1;
                        let Some(variable) = self.literal_variables.get_mut(id) else {
                            return Err(());
                        };
                        if !can_defer_char_literal_path_trait(
                            variable.kind,
                            variable.additional_site.is_some(),
                            eligible_unannotated_literal_path,
                            eligible_unannotated_integer_bitwise_pair_path
                                || eligible_unannotated_integer_addition_pair_path
                                || eligible_unannotated_integer_subtraction_pair_path
                                || eligible_unannotated_integer_multiplication_pair_path
                                || eligible_unannotated_integer_division_pair_path
                                || eligible_unannotated_integer_remainder_pair_path
                                || eligible_unannotated_integer_wrapping_pair_path,
                        ) {
                            return Err(());
                        }
                        variable
                            .pending_operator_traits
                            .push(PendingOperatorTraitSite {
                                operator,
                                expression,
                                diagnostic_index,
                            });
                        return Ok(LocalType::Concrete(PrimTy::Char));
                    };
                    let Some(right_kind) = primitive_literal_kind(right_literal) else {
                        return Err(());
                    };
                    if !primitive_bitwise_char_literal_pair_supported(operator, right_kind) {
                        return Err(());
                    }

                    let right_type = match right_kind {
                        PrimitiveLiteralKind::Character => PrimTy::Char,
                        PrimitiveLiteralKind::Bool => PrimTy::Bool,
                        PrimitiveLiteralKind::Byte => PrimTy::Byte,
                        PrimitiveLiteralKind::Integer => {
                            self.check_annotated_literal(
                                PrimTy::I64,
                                right_literal,
                                PrimitiveLiteralKind::Integer,
                                right.hir_id,
                            );
                            PrimTy::I64
                        }
                        PrimitiveLiteralKind::FloatingPoint => {
                            let expected =
                                primitive_bitwise_char_float_literal_rhs_type(operator).ok_or(())?;
                            self.check_annotated_literal(
                                expected,
                                right_literal,
                                PrimitiveLiteralKind::FloatingPoint,
                                right.hir_id,
                            );
                            expected
                        }
                    };

                    // Char bitwise operators use trait selection. No
                    // BitAnd, BitOr, or BitXor implementation exists for
                    // these pairs. Char-left numeric literals cannot be
                    // constrained by the failed trait obligation, so they
                    // default to I64 or F64 and are range-checked before
                    // retaining Char as the recovery result.
                    self.errors.push(PrimitiveBodyError {
                        node: expression,
                        kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                            operator,
                            left: PrimTy::Char,
                            right: right_type,
                        },
                    });
                    return Ok(LocalType::Concrete(PrimTy::Char));
                }
                return Err(());
            }
            if let ExprKind::Lit(right_literal) = right.kind {
                let Some(right_kind) = primitive_literal_kind(right_literal) else {
                    return Err(());
                };
                if !primitive_bitwise_integer_literal_pair_supported(operator, right_kind) {
                    return Err(());
                }

                let local_type = self.local_type_for_unannotated_literal(
                    literal,
                    PrimitiveLiteralKind::Integer,
                    left.hir_id,
                );
                let LocalType::LiteralVariable(id) = local_type else {
                    return Err(());
                };
                self.add_integer_literal_constraint(id, right_literal, right.hir_id)?;
                return Ok(local_type);
            }

            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .filter(|primitive| supports_primitive_literal(*primitive))
                .ok_or(())?;
            let local_type = self.local_type_for_unannotated_literal(
                literal,
                PrimitiveLiteralKind::Integer,
                left.hir_id,
            );
            let LocalType::LiteralVariable(id) = local_type else {
                return Err(());
            };

            return match primitive_bitwise_literal_left_result(operator, right_type) {
                PrimitiveArithmeticResult::Type(expected) => {
                    self.resolve_literal_variable(id, expected)?;
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                    self.errors.push(PrimitiveBodyError {
                        node: right.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                    });
                    // Keep the left integer literal unresolved so a later
                    // annotation or tail can still constrain its type.
                    Ok(local_type)
                }
                PrimitiveArithmeticResult::Unsupported => Err(()),
            };
        }

        let left_type = self
            .check_local_path(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;

        if let ExprKind::Lit(literal) = right.kind {
            match primitive_literal_kind(literal) {
                Some(PrimitiveLiteralKind::Integer) => {
                    if is_integer_primitive(left_type)
                        || matches!(
                            left_type,
                            PrimTy::Byte | PrimTy::Bool | PrimTy::F32 | PrimTy::F64
                        )
                    {
                        self.check_annotated_literal(
                            left_type,
                            literal,
                            PrimitiveLiteralKind::Integer,
                            right.hir_id,
                        );
                        if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
                            self.report_operator_not_supported(
                                expression,
                                operator,
                                PrimitiveOperatorOperand::Concrete(left_type),
                            );
                        }
                        return Ok(LocalType::Concrete(left_type));
                    }

                    if left_type == PrimTy::Char {
                        // Char follows the trait-selection rule for bitwise operators.
                        // No Char implementation can constrain this synthesized integer
                        // literal, so it defaults to I64 and is range-checked there.
                        self.check_annotated_literal(
                            PrimTy::I64,
                            literal,
                            PrimitiveLiteralKind::Integer,
                            right.hir_id,
                        );
                        self.errors.push(PrimitiveBodyError {
                            node: expression,
                            kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                                operator,
                                left: PrimTy::Char,
                                right: PrimTy::I64,
                            },
                        });
                        return Ok(LocalType::Concrete(PrimTy::Char));
                    }

                    return Err(());
                }
                Some(PrimitiveLiteralKind::Byte) => {
                    if left_type == PrimTy::Char {
                        self.errors.push(PrimitiveBodyError {
                            node: expression,
                            kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                                operator,
                                left: left_type,
                                right: PrimTy::Byte,
                            },
                        });
                    } else {
                        self.check_annotated_literal(
                            left_type,
                            literal,
                            PrimitiveLiteralKind::Byte,
                            right.hir_id,
                        );
                        if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
                            self.report_operator_not_supported(
                                expression,
                                operator,
                                PrimitiveOperatorOperand::Concrete(left_type),
                            );
                        }
                    }
                    return Ok(LocalType::Concrete(left_type));
                }
                Some(
                    kind @ (PrimitiveLiteralKind::Bool
                    | PrimitiveLiteralKind::Character
                    | PrimitiveLiteralKind::FloatingPoint),
                ) => {
                    if left_type == PrimTy::Char {
                        let right_type = match kind {
                            PrimitiveLiteralKind::Bool => PrimTy::Bool,
                            PrimitiveLiteralKind::Character => PrimTy::Char,
                            PrimitiveLiteralKind::FloatingPoint => {
                                primitive_bitwise_char_float_literal_rhs_type(operator).ok_or(())?
                            }
                            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte => {
                                unreachable!()
                            }
                        };
                        if kind == PrimitiveLiteralKind::FloatingPoint {
                            self.check_annotated_literal(right_type, literal, kind, right.hir_id);
                        }
                        self.errors.push(PrimitiveBodyError {
                            node: expression,
                            kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                                operator,
                                left: left_type,
                                right: right_type,
                            },
                        });
                        return Ok(LocalType::Concrete(left_type));
                    }
                    if left_type != PrimTy::Bool
                        && left_type != PrimTy::Byte
                        && !is_integer_primitive(left_type)
                        && !matches!(left_type, PrimTy::F32 | PrimTy::F64)
                    {
                        return Err(());
                    }
                    self.check_annotated_literal(left_type, literal, kind, right.hir_id);
                    if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
                        self.report_operator_not_supported(
                            expression,
                            operator,
                            PrimitiveOperatorOperand::Concrete(left_type),
                        );
                    }
                    return Ok(LocalType::Concrete(left_type));
                }
                _ => return Err(()),
            }
        }

        let right_type = self
            .check_local_path(right)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;

        if is_integer_primitive(left_type) || left_type == PrimTy::Byte {
            if left_type != right_type {
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: left_type,
                        found: right_type,
                    },
                });
            }
            return Ok(LocalType::Concrete(left_type));
        }

        if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
            if left_type != right_type {
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: left_type,
                        found: right_type,
                    },
                });
            }
            self.report_operator_not_supported(
                expression,
                operator,
                PrimitiveOperatorOperand::Concrete(left_type),
            );
            return Ok(LocalType::Concrete(left_type));
        }

        if left_type == PrimTy::Char {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                    operator,
                    left: left_type,
                    right: right_type,
                },
            });
            return Ok(LocalType::Concrete(left_type));
        }

        Err(())
    }

    fn check_shift(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
        allow_non_integer_rhs_mismatch: bool,
    ) -> Result<LocalType, ()> {
        let left_type = if let ExprKind::Lit(literal) = left.kind {
            let Some(kind) = primitive_literal_kind(literal) else {
                return Err(());
            };
            if !primitive_shift_left_literal_supported(kind) {
                return Err(());
            }
            match kind {
                PrimitiveLiteralKind::Byte => LocalType::Concrete(PrimTy::Byte),
                PrimitiveLiteralKind::Integer => {
                    self.local_type_for_unannotated_literal(literal, kind, left.hir_id)
                }
                _ => return Err(()),
            }
        } else {
            let local_type = self
                .check_local_path(left)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .filter(|primitive| supports_primitive_literal(*primitive))
                .ok_or(())?;
            LocalType::Concrete(local_type)
        };

        if let ExprKind::Lit(literal) = right.kind {
            let Some(kind) = primitive_literal_kind(literal) else {
                return Err(());
            };
            if kind != PrimitiveLiteralKind::Integer && !allow_non_integer_rhs_mismatch {
                return Err(());
            }
            self.check_annotated_literal(PrimTy::U32, literal, kind, right.hir_id);
        } else {
            let right_type = self
                .check_local_path(right)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .filter(|primitive| supports_primitive_literal(*primitive))
                .ok_or(())?;
            if right_type != PrimTy::U32 {
                self.errors.push(PrimitiveBodyError {
                    node: right.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: PrimTy::U32,
                        found: right_type,
                    },
                });
            }
        }

        if let Some(left_type) = self.concrete_local_type(left_type)
            && !is_bitwise_primitive(left_type)
        {
            self.report_operator_not_supported(
                expression,
                operator,
                PrimitiveOperatorOperand::Concrete(left_type),
            );
        }

        // Integer literal left operands remain unresolved until an annotation
        // or the function tail supplies their type. Unlike the shift count,
        // the left operand is not constrained by the U32 right operand.
        Ok(left_type)
    }

    fn report_operator_not_supported(
        &mut self,
        node: HirId,
        operator: BinOp,
        operand: PrimitiveOperatorOperand,
    ) {
        self.errors.push(PrimitiveBodyError {
            node,
            kind: PrimitiveBodyErrorKind::OperatorNotSupported { operator, operand },
        });
    }

    fn check_comparison(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
    ) -> Result<LocalType, ()> {
        let left_path_type = self
            .check_local_path(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type));
        let right_path_type = self
            .check_local_path(right)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type));
        let left_literal = match left.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal).map(|kind| (literal, kind)),
            _ => None,
        };
        let right_literal = match right.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal).map(|kind| (literal, kind)),
            _ => None,
        };

        match (left_path_type, right_path_type, left_literal, right_literal) {
            (Some(left_type), Some(right_type), None, None) => {
                match primitive_comparison_result(operator, left_type, right_type) {
                    PrimitiveComparisonResult::Bool => {}
                    PrimitiveComparisonResult::TypeMismatch { expected, found } => {
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                    }
                    PrimitiveComparisonResult::Unsupported => return Err(()),
                }
            }
            (Some(path_type), None, None, Some((literal, kind))) => {
                self.check_comparison_literal(
                    operator, left, path_type, right, literal, kind, false,
                )?;
            }
            (None, Some(path_type), Some((literal, kind)), None) => {
                self.check_comparison_literal(
                    operator, right, path_type, left, literal, kind, true,
                )?;
            }
            _ => return Err(()),
        }

        // A failed operand comparison still has Bool as its recovered result,
        // so later statements and the tail are checked against the expression
        // type instead of inheriting either operand's type.
        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    fn check_comparison_literal(
        &mut self,
        operator: BinOp,
        path: &'hir Expr<'hir>,
        path_type: PrimTy,
        literal_expression: &'hir Expr<'hir>,
        literal: Lit<'hir>,
        literal_kind: PrimitiveLiteralKind,
        literal_is_left: bool,
    ) -> Result<(), ()> {
        if primitive_comparison_result(operator, path_type, path_type)
            == PrimitiveComparisonResult::Unsupported
        {
            return Err(());
        }

        match primitive_comparison_literal_expected_type(literal_kind, path_type, literal_is_left) {
            Ok(expected) => self.check_annotated_literal(
                expected,
                literal,
                literal_kind,
                literal_expression.hir_id,
            ),
            Err(expected) => {
                // Report a failed left literal at its source location before
                // reporting the later path mismatch.
                self.check_annotated_literal(
                    expected,
                    literal,
                    literal_kind,
                    literal_expression.hir_id,
                );
                self.errors.push(PrimitiveBodyError {
                    node: path.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected,
                        found: path_type,
                    },
                });
            }
        }
        Ok(())
    }

    fn check_logical(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
    ) -> Result<LocalType, ()> {
        if !matches!(operator, BinOp::And | BinOp::Or) {
            return Err(());
        }

        // Typecheck in written order without lowering or reordering the
        // operands, preserving the source operator's short-circuit behavior.
        for operand in [left, right] {
            let operand_type = self.check_logical_operand(operand)?;
            self.check_logical_operand_type(operand, operand_type)?;
        }

        // Both `and` and `or` have Bool result type, including after an
        // operand mismatch, so downstream local and tail checks can recover.
        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    fn check_logical_operand(&mut self, operand: &'hir Expr<'hir>) -> Result<PrimTy, ()> {
        match operand.kind {
            // Nested logical operands recurse so `left and right and left`
            // keeps source order; comparison operands reuse the same
            // bounded comparison checker.
            ExprKind::Binary(operator, left, right)
                if matches!(operator, BinOp::And | BinOp::Or) =>
            {
                self.check_logical(operator, left, right)?;
                Ok(PrimTy::Bool)
            }
            ExprKind::Binary(operator, left, right) => {
                self.check_comparison(operator, left, right)?;
                Ok(PrimTy::Bool)
            }
            ExprKind::Lit(Lit::Bool(_)) => {
                self.record_expression_type(operand.hir_id, LocalType::Concrete(PrimTy::Bool));
                Ok(PrimTy::Bool)
            }
            // `not` operands recurse into the same operand gate so
            // `enabled and not (value >= 18)` and `not (enabled and value >= 18)`
            // stay in source order; `not not` and other unary operators still
            // reject through the path fallback below.
            ExprKind::Unary(UnOp::Not, operand) => {
                let operand_type = self.check_logical_operand(operand)?;
                self.check_logical_operand_type(operand, operand_type)?;
                Ok(PrimTy::Bool)
            }
            _ => self
                .check_local_path(operand)
                .ok()
                .and_then(|local_type| self.concrete_local_type(local_type))
                .ok_or(()),
        }
    }

    fn check_direct_call_logical(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
    ) -> Result<LocalType, ()> {
        if !matches!(operator, BinOp::And | BinOp::Or) {
            return Err(());
        }

        for operand in [left, right] {
            let operand_type = match operand.kind {
                ExprKind::Lit(Lit::Bool(_)) => {
                    self.record_expression_type(operand.hir_id, LocalType::Concrete(PrimTy::Bool));
                    PrimTy::Bool
                }
                ExprKind::Path(_) => self
                    .check_local_path(operand)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                    .ok_or(())?,
                _ => return Err(()),
            };
            self.check_logical_operand_type(operand, operand_type)?;
        }

        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    fn check_logical_operand_type(
        &mut self,
        operand: &'hir Expr<'hir>,
        operand_type: PrimTy,
    ) -> Result<(), ()> {
        match primitive_bool_operand_result(operand_type) {
            PrimitiveBoolOperandResult::Bool => {}
            PrimitiveBoolOperandResult::TypeMismatch { found } => {
                self.errors.push(PrimitiveBodyError {
                    node: operand.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: PrimTy::Bool,
                        found,
                    },
                });
            }
            PrimitiveBoolOperandResult::Unsupported => return Err(()),
        }
        Ok(())
    }

    fn concrete_local_type(&self, local_type: LocalType) -> Option<PrimTy> {
        let primitive = match local_type {
            LocalType::Concrete(primitive) => primitive,
            LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                self.literal_variables.get(id)?.resolved?
            }
            LocalType::Error(_) => return None,
        };
        supports_primitive_literal(primitive).then_some(primitive)
    }

    fn check_annotated_literal(
        &mut self,
        expected: PrimTy,
        literal: Lit<'hir>,
        found: PrimitiveLiteralKind,
        node: HirId,
    ) {
        match check_primitive_literal(
            expected,
            literal,
            self.input.symbols,
            self.input.pointer_width,
        ) {
            PrimitiveLiteralResult::Match => {
                self.record_expression_type(node, LocalType::Concrete(expected));
            }
            PrimitiveLiteralResult::TypeMismatch => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::LiteralTypeMismatch { expected, found },
                });
            }
            PrimitiveLiteralResult::OutOfRange => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::LiteralOutOfRange { expected, found },
                });
            }
        }
    }

    fn local_type_for_unannotated_literal(
        &mut self,
        literal: Lit<'hir>,
        kind: PrimitiveLiteralKind,
        origin: HirId,
    ) -> LocalType {
        let local_type = match kind {
            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint => {
                let id = self.literal_variables.len();
                self.literal_variables.push(LiteralVariable {
                    literal,
                    kind,
                    origin,
                    additional_site: None,
                    resolved: None,
                    pending_operator_traits: Vec::new(),
                    pending_neg_traits: Vec::new(),
                });
                LocalType::LiteralVariable(id)
            }
            PrimitiveLiteralKind::Bool => LocalType::Concrete(PrimTy::Bool),
            PrimitiveLiteralKind::Byte => LocalType::Concrete(PrimTy::Byte),
            PrimitiveLiteralKind::Character => LocalType::Concrete(PrimTy::Char),
        };
        self.record_expression_type(origin, local_type);
        local_type
    }

    fn check_annotated_path(
        &mut self,
        source_type: LocalType,
        expected: PrimTy,
        node: HirId,
    ) -> Result<(), ()> {
        match source_type {
            LocalType::Concrete(found) if found != expected => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                });
            }
            LocalType::Concrete(_) => {}
            LocalType::Error(_) => {}
            LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                let Some((resolved, kind)) = self
                    .literal_variables
                    .get(id)
                    .map(|variable| (variable.resolved, variable.kind))
                else {
                    return Err(());
                };
                match resolved {
                    Some(found) if found != expected => {
                        self.errors.push(PrimitiveBodyError {
                            node,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                    }
                    Some(_) => {}
                    None if literal_family_accepts(kind, expected) => {
                        self.resolve_literal_variable(id, expected)?;
                    }
                    None => {
                        self.errors.push(PrimitiveBodyError {
                            node,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected,
                                found: kind,
                            },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn check_expr_stmt(&mut self, expression: &Expr<'hir>) -> Result<(), ()> {
        // Statement-position `if`/`block` expressions unify their branch
        // tails with the return type, which does not apply to a discarded
        // result, so they stay outside this slice.
        if matches!(expression.kind, ExprKind::If { .. } | ExprKind::Block(_)) {
            return Err(());
        }
        let local_type = self.check_expr_value(expression)?;
        self.record_expression_type(expression.hir_id, local_type);
        Ok(())
    }

    fn check_expr_value(&mut self, expression: &Expr<'hir>) -> Result<LocalType, ()> {
        let local_type = if self.expression_has_unknown_field_error(expression) {
            if !matches!(expression.kind, ExprKind::Path(_) | ExprKind::Field { .. }) {
                return Err(());
            }
            self.check_error_tainted_expression(expression)?;
            if let Some(id) = self.error_tainted_literal_variable(expression) {
                LocalType::ErrorLiteralVariable(id)
            } else {
                LocalType::Error(self.known_error_tainted_result_type(expression))
            }
        } else {
            match expression.kind {
                ExprKind::Call { .. } => self.check_direct_call(expression, false)?,
                ExprKind::Field { .. } => LocalType::Concrete(
                    self.input
                        .self_field_projections
                        .and_then(|fields| fields.get(&expression.hir_id))
                        .copied()
                        .ok_or(())?,
                ),
                ExprKind::Unary(operator, operand)
                    if matches!(operator, UnOp::Neg | UnOp::BitNot | UnOp::Not)
                        && matches!(operand.kind, ExprKind::Path(_)) =>
                {
                    match operator {
                        UnOp::Neg => self.check_direct_call_neg_path(operand, expression.hir_id)?,
                        UnOp::BitNot => self.check_bitwise_not_path(operand, expression.hir_id)?,
                        _ => self.check_logical_not(operand)?,
                    }
                }
                // Binary tails share the local-initializer family checkers:
                // the tail expression's result is unified with the return
                // type the way the initializer's is unified with the
                // annotation.
                ExprKind::Binary(operator, left, right) => {
                    if matches!(
                        operator,
                        BinOp::Add
                            | BinOp::Sub
                            | BinOp::Mul
                            | BinOp::Div
                            | BinOp::Rem
                            | BinOp::WrapAdd
                            | BinOp::WrapSub
                            | BinOp::WrapMul
                    ) {
                        self.check_arithmetic(operator, left, right, expression.hir_id)?
                    } else if matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                        self.check_bitwise(operator, left, right, expression.hir_id)?
                    } else if matches!(operator, BinOp::Shl | BinOp::Shr) {
                        self.check_shift(operator, left, right, expression.hir_id, false)?
                    } else if matches!(operator, BinOp::And | BinOp::Or) {
                        self.check_logical(operator, left, right)?
                    } else {
                        self.check_comparison(operator, left, right)?
                    }
                }
                ExprKind::Lit(literal) => {
                    let Some(kind) = primitive_literal_kind(literal) else {
                        return Err(());
                    };
                    self.local_type_for_unannotated_literal(literal, kind, expression.hir_id)
                }
                // `if` tails reuse `check_block` for each branch so the
                // branch tails unify with the return type the way the
                // outer tail does. Branches with statements stay outside
                // this slice, mirroring the driver admission.
                ExprKind::If { cond, then, els } => self.check_if_tail(cond, then, els)?,
                _ => self.check_local_path(expression)?,
            }
        };
        Ok(local_type)
    }

    fn check_expr(&mut self, expression: &Expr<'hir>) -> Result<(), ()> {
        let local_type = self.check_expr_value(expression)?;
        match local_type {
            LocalType::Error(found) => {
                if let Some(found) = found {
                    self.check_concrete_tail_type(found, expression.hir_id);
                }
            }
            LocalType::Concrete(found) => {
                self.check_concrete_tail_type(found, expression.hir_id);
            }
            LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                let Some((resolved, kind)) = self
                    .literal_variables
                    .get(id)
                    .map(|variable| (variable.resolved, variable.kind))
                else {
                    return Err(());
                };
                match resolved {
                    Some(found) => self.check_concrete_tail_type(found, expression.hir_id),
                    None if literal_family_accepts(kind, self.input.return_type) => {
                        self.resolve_literal_variable(id, self.input.return_type)?;
                    }
                    None => {
                        self.errors.push(PrimitiveBodyError {
                            node: expression.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected: self.input.return_type,
                                found: kind,
                            },
                        });
                    }
                }
            }
        }
        self.record_expression_type(expression.hir_id, local_type);
        Ok(())
    }

    /// Validates an assignment place: a plain path to a preceding local whose
    /// type is already concrete. Field/tuple/index/dereference places stay
    /// gated (`Err`), a non-place target pushes `INVALID_ASSIGN_TARGET` and
    /// returns `Ok(None)`, and an immutable target pushes `ASSIGN_TO_IMMUTABLE`
    /// while still returning the place so the right side is checked too.
    fn assign_place(&mut self, lhs: &Expr<'hir>) -> Result<Option<(PrimTy,)>, ()> {
        let ExprKind::Path(_) = lhs.kind else {
            return match lhs.kind {
                ExprKind::Field { .. }
                | ExprKind::TupleField { .. }
                | ExprKind::Index(..)
                | ExprKind::Unary(UnOp::Deref, _) => Err(()),
                _ => {
                    self.errors.push(PrimitiveBodyError {
                        node: lhs.hir_id,
                        kind: PrimitiveBodyErrorKind::InvalidAssignTarget,
                    });
                    Ok(None)
                }
            };
        };
        let Some(binding) = self.plain_local_path_binding(lhs) else {
            return Err(());
        };
        let Some(local_type) = self.known_bindings.get(&binding).copied() else {
            return Err(());
        };
        let LocalType::Concrete(place_type) = local_type else {
            return Err(());
        };
        if !self.mutable_bindings.contains(&binding) {
            self.errors.push(PrimitiveBodyError {
                node: lhs.hir_id,
                kind: PrimitiveBodyErrorKind::AssignToImmutable,
            });
        }
        Ok(Some((place_type,)))
    }

    fn check_assign_stmt(&mut self, lhs: &Expr<'hir>, rhs: &Expr<'hir>) -> Result<(), ()> {
        let Some((place_type,)) = self.assign_place(lhs)? else {
            return Ok(());
        };
        self.check_initializer_value(rhs, Some(place_type))?;
        self.record_expression_type(lhs.hir_id, LocalType::Concrete(place_type));
        Ok(())
    }

    /// `place op= e` desugars to `{ let t = e; place = place op t }`
    /// (decisions.md:489). Checking `place op e` against the place type is
    /// equivalent: `t` is a fresh binding whose literal variable resolves
    /// through the operation exactly as `e` does inside `place op e`.
    fn check_assign_op_stmt(
        &mut self,
        stmt_id: HirId,
        op: BinOp,
        lhs: &'hir Expr<'hir>,
        rhs: &'hir Expr<'hir>,
    ) -> Result<(), ()> {
        let Some((place_type,)) = self.assign_place(lhs)? else {
            return Ok(());
        };
        let result = match op {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::WrapAdd
            | BinOp::WrapSub
            | BinOp::WrapMul => self.check_arithmetic(op, lhs, rhs, stmt_id)?,
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                self.check_bitwise(op, lhs, rhs, stmt_id)?
            }
            BinOp::Shl | BinOp::Shr => self.check_shift(op, lhs, rhs, stmt_id, false)?,
            BinOp::And | BinOp::Or => self.check_logical(op, lhs, rhs)?,
            _ => self.check_comparison(op, lhs, rhs)?,
        };
        self.check_annotated_path(result, place_type, stmt_id)?;
        self.record_expression_type(lhs.hir_id, LocalType::Concrete(place_type));
        self.record_expression_type(stmt_id, LocalType::Concrete(place_type));
        Ok(())
    }

    /// Checks an admissible value expression in a `let` initializer or the
    /// right-hand side of an assignment — a coercion site when `annotation`
    /// carries the expected type. The deferred-operator flags let the `let`
    /// path keep its provenance tracking; an assignment ignores them because
    /// its target already has a concrete type.
    fn check_initializer_value(
        &mut self,
        init: &Expr<'hir>,
        annotation: Option<PrimTy>,
    ) -> Result<InitializerCheck, ()> {
        let mut deferred_neg_result = false;
        let mut deferred_bitnot_result = false;
        let local_type = if self.expression_has_unknown_field_error(init) {
            self.check_error_tainted_expression(init)?;
            if let Some(id) = self.error_tainted_literal_variable(init) {
                if let Some(expected) = annotation {
                    self.check_annotated_path(
                        LocalType::LiteralVariable(id),
                        expected,
                        init.hir_id,
                    )?;
                    LocalType::Error(Some(expected))
                } else {
                    LocalType::ErrorLiteralVariable(id)
                }
            } else {
                let found = self.known_error_tainted_result_type(init);
                if let (Some(expected), Some(found)) = (annotation, found) {
                    if expected != found {
                        self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                    }
                }
                LocalType::Error(annotation.or(found))
            }
        } else {
            match init.kind {
                ExprKind::Lit(literal) => {
                    let Some(found) = primitive_literal_kind(literal) else {
                        return Err(());
                    };
                    if let Some(expected) = annotation {
                        self.check_annotated_literal(expected, literal, found, init.hir_id);
                        LocalType::Concrete(expected)
                    } else {
                        self.local_type_for_unannotated_literal(literal, found, init.hir_id)
                    }
                }
                ExprKind::Path(_) => {
                    let source_type = self.check_local_path(init)?;
                    if let Some(expected) = annotation {
                        self.check_annotated_path(source_type, expected, init.hir_id)?;
                        LocalType::Concrete(expected)
                    } else {
                        source_type
                    }
                }
                ExprKind::Field { .. } => {
                    let Some(field_type) = self
                        .input
                        .self_field_projections
                        .and_then(|fields| fields.get(&init.hir_id))
                        .copied()
                    else {
                        return Err(());
                    };
                    let source_type = LocalType::Concrete(field_type);
                    if let Some(expected) = annotation {
                        self.check_annotated_path(source_type, expected, init.hir_id)?;
                        LocalType::Concrete(expected)
                    } else {
                        source_type
                    }
                }
                ExprKind::Call { .. } => {
                    let call_type = self.check_direct_call(init, false)?;
                    if let Some(expected) = annotation {
                        self.check_annotated_path(call_type, expected, init.hir_id)?;
                        LocalType::Concrete(expected)
                    } else {
                        call_type
                    }
                }
                ExprKind::Binary(operator, left, right) => {
                    if matches!(
                        operator,
                        BinOp::Add
                            | BinOp::Sub
                            | BinOp::Mul
                            | BinOp::Div
                            | BinOp::Rem
                            | BinOp::WrapAdd
                            | BinOp::WrapSub
                            | BinOp::WrapMul
                    ) {
                        let result = self.check_arithmetic(operator, left, right, init.hir_id)?;
                        if let Some(expected) = annotation {
                            self.check_annotated_path(result, expected, init.hir_id)?;
                            // The annotation determines the local's recovered
                            // type after the coercion-site mismatch, as it does
                            // for the existing annotated path initializer.
                            LocalType::Concrete(expected)
                        } else {
                            result
                        }
                    } else if matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                        let result = self.check_bitwise(operator, left, right, init.hir_id)?;
                        if let Some(expected) = annotation {
                            self.check_annotated_path(result, expected, init.hir_id)?;
                            LocalType::Concrete(expected)
                        } else {
                            result
                        }
                    } else if matches!(operator, BinOp::Shl | BinOp::Shr) {
                        let result = self.check_shift(operator, left, right, init.hir_id, false)?;
                        if let Some(expected) = annotation {
                            self.check_annotated_path(result, expected, init.hir_id)?;
                            LocalType::Concrete(expected)
                        } else {
                            result
                        }
                    } else {
                        let result = match operator {
                            BinOp::And | BinOp::Or => self.check_logical(operator, left, right)?,
                            _ => self.check_comparison(operator, left, right)?,
                        };
                        if let Some(expected) = annotation {
                            if expected != PrimTy::Bool {
                                self.errors.push(PrimitiveBodyError {
                                    node: init.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                                        expected,
                                        found: PrimTy::Bool,
                                    },
                                });
                            }
                        }
                        result
                    }
                }
                ExprKind::Unary(UnOp::Not, operand) => {
                    // `let x = not e` admits operand-position logical shapes
                    // (path, `Bool` literal, `not`, `and`/`or`, comparison) —
                    // the same admission `if` conditions use for `not e`.
                    let operand_type = self.check_logical_operand(operand)?;
                    self.check_logical_operand_type(operand, operand_type)?;
                    if let Some(expected) = annotation {
                        if expected != PrimTy::Bool {
                            self.errors.push(PrimitiveBodyError {
                                node: init.hir_id,
                                kind: PrimitiveBodyErrorKind::TypeMismatch {
                                    expected,
                                    found: PrimTy::Bool,
                                },
                            });
                        }
                    }
                    LocalType::Concrete(PrimTy::Bool)
                }
                ExprKind::Unary(UnOp::Neg, operand) => {
                    let (result, is_deferred_neg_result) =
                        self.check_local_initializer_neg_path(operand, init.hir_id)?;
                    deferred_neg_result = is_deferred_neg_result;
                    if let Some(expected) = annotation {
                        self.check_annotated_path(result, expected, init.hir_id)?;
                        LocalType::Concrete(expected)
                    } else {
                        result
                    }
                }
                ExprKind::Unary(UnOp::BitNot, operand) => {
                    if let ExprKind::Lit(literal) = operand.kind {
                        match primitive_literal_kind(literal) {
                            Some(PrimitiveLiteralKind::Integer) => {
                                if let Some(expected) = annotation {
                                    self.check_annotated_literal(
                                        expected,
                                        literal,
                                        PrimitiveLiteralKind::Integer,
                                        operand.hir_id,
                                    );
                                    LocalType::Concrete(expected)
                                } else {
                                    self.local_type_for_unannotated_literal(
                                        literal,
                                        PrimitiveLiteralKind::Integer,
                                        operand.hir_id,
                                    )
                                }
                            }
                            Some(PrimitiveLiteralKind::Byte) => {
                                self.record_expression_type(
                                    operand.hir_id,
                                    LocalType::Concrete(PrimTy::Byte),
                                );
                                if let Some(expected) = annotation {
                                    if expected != PrimTy::Byte {
                                        self.errors.push(PrimitiveBodyError {
                                            node: init.hir_id,
                                            kind: PrimitiveBodyErrorKind::TypeMismatch {
                                                expected,
                                                found: PrimTy::Byte,
                                            },
                                        });
                                    }
                                    LocalType::Concrete(expected)
                                } else {
                                    LocalType::Concrete(PrimTy::Byte)
                                }
                            }
                            _ => return Err(()),
                        }
                    } else {
                        let (local_type, is_deferred_bitnot_result) =
                            self.check_local_initializer_bitwise_not_path(operand, init.hir_id)?;
                        deferred_bitnot_result = is_deferred_bitnot_result;
                        if let Some(expected) = annotation {
                            self.check_annotated_path(local_type, expected, init.hir_id)?;
                            LocalType::Concrete(expected)
                        } else {
                            local_type
                        }
                    }
                }
                _ => return Err(()),
            }
        };
        Ok(InitializerCheck {
            local_type,
            deferred_neg_result,
            deferred_bitnot_result,
        })
    }

    fn check_if_tail(
        &mut self,
        condition: &'hir Expr<'hir>,
        then: &'hir Block<'hir>,
        els: Option<&'hir Expr<'hir>>,
    ) -> Result<LocalType, ()> {
        let Some(els) = els else {
            return Err(());
        };
        self.check_if_condition(condition)?;
        self.check_if_branch(then)?;
        match els.kind {
            ExprKind::Block(block) => {
                self.check_if_branch(block)?;
                self.record_expression_type(
                    els.hir_id,
                    LocalType::Concrete(self.input.return_type),
                );
            }
            ExprKind::If { .. } => {
                self.check_expr(els)?;
            }
            _ => return Err(()),
        }
        Ok(LocalType::Concrete(self.input.return_type))
    }

    // Empty `if` branches are checked tail-first through `check_expr` rather
    // than `check_block`'s empty-statement gate, which deliberately excludes
    // plain `Path` tails (flat path tails belong to the direct-parameter
    // slice). Statement-bearing branches reuse `check_block` directly so
    // their `let` bindings follow the same boundary as the outer block.
    fn check_if_branch(&mut self, block: &'hir Block<'hir>) -> Result<(), ()> {
        if block.stmts.is_empty() {
            if block.safety != Safety::Safe {
                return Err(());
            }
            let Some(tail) = block.tail else {
                return Err(());
            };
            self.check_expr(tail)
        } else {
            self.check_block(block)
        }
    }

    fn check_if_condition(&mut self, condition: &'hir Expr<'hir>) -> Result<(), ()> {
        let local_type = match condition.kind {
            ExprKind::Path(_) => self.check_local_path(condition)?,
            ExprKind::Unary(UnOp::Not, operand) => match operand.kind {
                ExprKind::Path(_) => self.check_logical_not(operand)?,
                ExprKind::Binary(operator, left, right)
                    if matches!(operator, BinOp::And | BinOp::Or) =>
                {
                    self.check_logical(operator, left, right)?
                }
                ExprKind::Binary(operator, left, right)
                    if matches!(
                        operator,
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                    ) =>
                {
                    self.check_comparison(operator, left, right)?
                }
                _ => return Err(()),
            },
            ExprKind::Binary(operator, left, right)
                if matches!(operator, BinOp::And | BinOp::Or) =>
            {
                self.check_logical(operator, left, right)?
            }
            ExprKind::Binary(operator, left, right)
                if matches!(
                    operator,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) =>
            {
                self.check_comparison(operator, left, right)?
            }
            ExprKind::Lit(Lit::Bool(_)) => LocalType::Concrete(PrimTy::Bool),
            _ => return Err(()),
        };
        match local_type {
            LocalType::Concrete(PrimTy::Bool) => {
                self.record_expression_type(condition.hir_id, local_type);
                Ok(())
            }
            LocalType::Concrete(found) => {
                self.errors.push(PrimitiveBodyError {
                    node: condition.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: PrimTy::Bool,
                        found,
                    },
                });
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn check_direct_call(
        &mut self,
        expression: &Expr<'hir>,
        nested: bool,
    ) -> Result<LocalType, ()> {
        let ExprKind::Call {
            callee,
            args,
            base: None,
        } = expression.kind
        else {
            return Err(());
        };
        let signature = self
            .input
            .call_signatures
            .and_then(|signatures| signatures.get(&expression.hir_id))
            .filter(|signature| signature.callee == callee.hir_id)
            .cloned()
            .ok_or(())?;

        if args.len() != signature.parameters.len() {
            self.errors.push(PrimitiveBodyError {
                node: expression.hir_id,
                kind: PrimitiveBodyErrorKind::WrongArgumentCount {
                    expected: signature.parameters.len(),
                    found: args.len(),
                },
            });
        }

        // Check only the positional prefix shared by the source arguments and
        // the resolved signature. Unmatched extra arguments remain outside
        // this bounded checker slice, though named-argument errors are still
        // reported for every written argument.
        for (index, argument) in args.iter().enumerate() {
            if let Some(name) = argument.name {
                self.errors.push(PrimitiveBodyError {
                    node: expression.hir_id,
                    kind: PrimitiveBodyErrorKind::NamedArgumentOnFunction {
                        name_span: name.span,
                    },
                });
            }
            let Some(expected) = signature.parameters.get(index) else {
                continue;
            };
            match argument.expr.kind {
                ExprKind::Lit(literal) => {
                    let Some(found) = primitive_literal_kind(literal) else {
                        return Err(());
                    };
                    self.check_annotated_literal(*expected, literal, found, argument.expr.hir_id);
                }
                ExprKind::Path(_) => {
                    let local_type = self.check_local_path(argument.expr)?;
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Call { .. } if !nested => {
                    let local_type = self.check_direct_call(argument.expr, true)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(BinOp::Rem, left, right)
                    if is_supported_direct_call_remainder_operands(left, right) =>
                {
                    let local_type = self.check_remainder(left, right, argument.expr.hir_id)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(
                    operator @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div),
                    left,
                    right,
                ) if is_supported_direct_call_arithmetic_operands(left, right) => {
                    let local_type = self.check_direct_call_arithmetic(
                        operator,
                        left,
                        right,
                        argument.expr.hir_id,
                    )?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(
                    operator @ (BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge),
                    left,
                    right,
                ) if is_supported_direct_call_comparison_operands(left, right) => {
                    let local_type = self.check_comparison(operator, left, right)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(operator @ (BinOp::And | BinOp::Or), left, right)
                    if is_supported_direct_call_logical_operands(left, right) =>
                {
                    let local_type = self.check_direct_call_logical(operator, left, right)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(operator @ (BinOp::Shl | BinOp::Shr), left, right)
                    if is_supported_direct_call_shift_operands(left, right) =>
                {
                    let local_type =
                        self.check_shift(operator, left, right, argument.expr.hir_id, true)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(
                    operator @ (BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor),
                    left,
                    right,
                ) if is_supported_direct_call_bitwise_operands(left, right) => {
                    let local_type =
                        self.check_bitwise(operator, left, right, argument.expr.hir_id)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::Not, operand)
                    if is_supported_direct_call_not_operand(operand) =>
                {
                    let local_type = self.check_direct_call_logical_not(operand)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::BitNot, operand)
                    if is_supported_direct_call_bitwise_not_operand(operand) =>
                {
                    let local_type = match operand.kind {
                        ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_)) => {
                            self.check_direct_call_bitwise_not_literal(operand, *expected)?
                        }
                        _ => self.check_bitwise_not_path(operand, argument.expr.hir_id)?,
                    };
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::Neg, operand)
                    if is_supported_direct_call_neg_operand(operand) =>
                {
                    let local_type =
                        self.check_direct_call_neg_path(operand, argument.expr.hir_id)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                _ => return Err(()),
            }
        }
        Ok(LocalType::Concrete(signature.output))
    }

    fn check_bitwise_not_path(
        &mut self,
        operand: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        let source_type = self.check_local_path(operand)?;
        let source_binding = self.plain_local_path_binding(operand).ok_or(())?;
        if self
            .deferred_bitnot_result_bindings
            .contains(&source_binding)
        {
            return Err(());
        }
        if let LocalType::LiteralVariable(id) = source_type {
            let is_eligible_literal_source = self.is_eligible_unannotated_literal_path(operand);
            if is_eligible_literal_source
                && self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer && variable.resolved.is_none()
                })
            {
                // A direct integer literal local or its simple unannotated
                // alias can share its inference variable with this BitNot
                // result. The enclosing call's parameter expectation resolves
                // both facts and performs the range check at the source
                // literal.
                return Ok(source_type);
            }
        }

        let operand_type = self
            .concrete_local_type(source_type)
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;
        if !is_bitwise_primitive(operand_type) {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                    operator: UnOp::BitNot,
                    operand: operand_type,
                },
            });
        }
        Ok(LocalType::Concrete(operand_type))
    }

    fn check_direct_call_bitwise_not_literal(
        &mut self,
        operand: &'hir Expr<'hir>,
        expected: PrimTy,
    ) -> Result<LocalType, ()> {
        let ExprKind::Lit(literal) = operand.kind else {
            return Err(());
        };
        match primitive_literal_kind(literal) {
            Some(PrimitiveLiteralKind::Integer) => {
                self.check_annotated_literal(
                    expected,
                    literal,
                    PrimitiveLiteralKind::Integer,
                    operand.hir_id,
                );
                Ok(LocalType::Concrete(expected))
            }
            Some(PrimitiveLiteralKind::Byte) => {
                self.record_expression_type(operand.hir_id, LocalType::Concrete(PrimTy::Byte));
                Ok(LocalType::Concrete(PrimTy::Byte))
            }
            _ => Err(()),
        }
    }

    fn check_direct_call_neg_path(
        &mut self,
        operand: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        let source_type = self.check_local_path(operand)?;
        let source_binding = self.plain_local_path_binding(operand).ok_or(())?;
        if self.deferred_neg_result_bindings.contains(&source_binding) {
            return Err(());
        }
        if let LocalType::LiteralVariable(id) = source_type {
            let is_eligible_literal_source = self.is_eligible_unannotated_literal_path(operand);
            if is_eligible_literal_source
                && self.literal_variables.get(id).is_some_and(|variable| {
                    matches!(
                        variable.kind,
                        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                    ) && variable.resolved.is_none()
                })
            {
                // A direct numeric literal local or its simple unannotated
                // alias can share its inference variable with this Neg
                // result. The enclosing call's parameter expectation
                // resolves it, and the deferred `Neg` trait check rejects an
                // unsigned integer resolution at the unary expression.
                let diagnostic_index = self.errors.len() + self.pending_operator_trait_count;
                self.pending_operator_trait_count += 1;
                self.literal_variables
                    .get_mut(id)
                    .ok_or(())?
                    .pending_neg_traits
                    .push(PendingNegTraitSite {
                        expression,
                        diagnostic_index,
                    });
                return Ok(source_type);
            }
        }

        let operand_type = self
            .concrete_local_type(source_type)
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;
        if !is_neg_primitive(operand_type) {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                    operator: UnOp::Neg,
                    operand: operand_type,
                },
            });
        }
        Ok(LocalType::Concrete(operand_type))
    }

    fn check_local_initializer_neg_path(
        &mut self,
        operand: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<(LocalType, bool), ()> {
        let source_type = self.check_local_path(operand)?;
        let source_binding = self.plain_local_path_binding(operand).ok_or(())?;
        if self.deferred_neg_result_bindings.contains(&source_binding) {
            return Err(());
        }
        if let LocalType::LiteralVariable(id) = source_type {
            let eligible_literal_path = self.is_eligible_unannotated_literal_path(operand);
            if eligible_literal_path
                && self.literal_variables.get(id).is_some_and(|variable| {
                    matches!(
                        variable.kind,
                        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                    ) && variable.resolved.is_none()
                })
            {
                let diagnostic_index = self.errors.len() + self.pending_operator_trait_count;
                self.pending_operator_trait_count += 1;
                self.literal_variables
                    .get_mut(id)
                    .ok_or(())?
                    .pending_neg_traits
                    .push(PendingNegTraitSite {
                        expression,
                        diagnostic_index,
                    });
                return Ok((source_type, true));
            }
        }

        let operand_type = self
            .concrete_local_type(source_type)
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;
        if !is_neg_primitive(operand_type) {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                    operator: UnOp::Neg,
                    operand: operand_type,
                },
            });
        }
        Ok((LocalType::Concrete(operand_type), false))
    }

    fn check_local_initializer_bitwise_not_path(
        &mut self,
        operand: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<(LocalType, bool), ()> {
        let source_type = self.check_local_path(operand)?;
        let source_binding = self.plain_local_path_binding(operand).ok_or(())?;
        if self
            .deferred_bitnot_result_bindings
            .contains(&source_binding)
        {
            return Err(());
        }
        if let LocalType::LiteralVariable(id) = source_type {
            let eligible_literal_path = self.is_eligible_unannotated_literal_path(operand);
            if eligible_literal_path
                && self.literal_variables.get(id).is_some_and(|variable| {
                    variable.kind == PrimitiveLiteralKind::Integer && variable.resolved.is_none()
                })
            {
                // An unresolved integer literal can resolve only to one of
                // the integer primitives, and every such primitive
                // implements BitNot. Share its variable with the result
                // without reserving a deferred diagnostic slot.
                return Ok((source_type, true));
            }
        }

        let operand_type = self
            .concrete_local_type(source_type)
            .filter(|primitive| supports_primitive_literal(*primitive))
            .ok_or(())?;
        if !is_bitwise_primitive(operand_type) {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                    operator: UnOp::BitNot,
                    operand: operand_type,
                },
            });
        }
        Ok((LocalType::Concrete(operand_type), false))
    }

    fn check_logical_not(&mut self, operand: &'hir Expr<'hir>) -> Result<LocalType, ()> {
        let operand_type = self
            .check_local_path(operand)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .ok_or(())?;
        match primitive_bool_operand_result(operand_type) {
            PrimitiveBoolOperandResult::Bool => {}
            PrimitiveBoolOperandResult::TypeMismatch { found } => {
                self.errors.push(PrimitiveBodyError {
                    node: operand.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected: PrimTy::Bool,
                        found,
                    },
                });
            }
            PrimitiveBoolOperandResult::Unsupported => return Err(()),
        }
        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    fn check_direct_call_logical_not(
        &mut self,
        operand: &'hir Expr<'hir>,
    ) -> Result<LocalType, ()> {
        match operand.kind {
            ExprKind::Lit(Lit::Bool(_)) => {
                let bool_type = LocalType::Concrete(PrimTy::Bool);
                self.record_expression_type(operand.hir_id, bool_type);
                Ok(bool_type)
            }
            ExprKind::Path(_) => self.check_logical_not(operand),
            _ => Err(()),
        }
    }

    fn check_direct_call_arithmetic(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        if matches!(
            (left.kind, right.kind),
            (
                ExprKind::Lit(Lit::Float { .. }),
                ExprKind::Lit(Lit::Float { .. })
            )
        ) {
            if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
                return Err(());
            }
            let ExprKind::Lit(left_literal) = left.kind else {
                return Err(());
            };
            let ExprKind::Lit(right_literal) = right.kind else {
                return Err(());
            };
            let local_type = self.local_type_for_unannotated_literal(
                left_literal,
                PrimitiveLiteralKind::FloatingPoint,
                left.hir_id,
            );
            let LocalType::LiteralVariable(id) = local_type else {
                return Err(());
            };
            self.add_float_literal_constraint(id, right_literal, right.hir_id)?;
            return Ok(local_type);
        }

        self.check_arithmetic(operator, left, right, expression)
    }

    fn expression_has_unknown_field_error(&self, expression: &Expr<'hir>) -> bool {
        if self
            .input
            .unknown_self_fields
            .is_some_and(|fields| fields.contains(&expression.hir_id))
        {
            return true;
        }
        match expression.kind {
            ExprKind::Path(_) => self.resolve_local_path(expression).is_ok_and(|local_type| {
                matches!(
                    local_type,
                    LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
                )
            }),
            ExprKind::Unary(_, operand) => self.expression_has_unknown_field_error(operand),
            ExprKind::Binary(_, left, right) => {
                self.expression_has_unknown_field_error(left)
                    || self.expression_has_unknown_field_error(right)
            }
            _ => false,
        }
    }

    fn error_tainted_literal_variable(&mut self, expression: &Expr<'hir>) -> Option<usize> {
        if let ExprKind::Path(_) = expression.kind {
            let binding = self.plain_local_path_binding(expression)?;
            return match self.known_bindings.get(&binding).copied()? {
                LocalType::ErrorLiteralVariable(id) => Some(id),
                _ => None,
            };
        }

        let ExprKind::Binary(
            operator @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div),
            left,
            right,
        ) = expression.kind
        else {
            return None;
        };
        if !self.expression_has_unknown_field_error(right) {
            return None;
        }
        let ExprKind::Lit(literal) = left.kind else {
            return None;
        };
        let kind = primitive_literal_kind(literal)?;
        if !matches!(
            kind,
            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
        ) {
            return None;
        }
        let right_is_unresolved_error_literal = self
            .plain_local_path_binding(right)
            .and_then(|binding| self.known_bindings.get(&binding).copied())
            .is_some_and(|local_type| match local_type {
                LocalType::ErrorLiteralVariable(id) => self
                    .literal_variables
                    .get(id)
                    .is_some_and(|variable| variable.resolved.is_none()),
                _ => false,
            });
        if right_is_unresolved_error_literal {
            return None;
        }
        if let Some(right_type) = self.known_recovery_operand_type(right) {
            if !matches!(
                primitive_arithmetic_literal_left_result(operator, kind, right_type),
                PrimitiveArithmeticResult::TypeMismatch { .. }
            ) {
                return None;
            }
        }
        match self.local_type_for_unannotated_literal(literal, kind, left.hir_id) {
            LocalType::LiteralVariable(id) => Some(id),
            LocalType::Concrete(_) | LocalType::Error(_) | LocalType::ErrorLiteralVariable(_) => {
                None
            }
        }
    }

    fn known_error_tainted_result_type(&self, expression: &Expr<'hir>) -> Option<PrimTy> {
        match expression.kind {
            ExprKind::Path(_) => self.known_recovery_operand_type(expression),
            ExprKind::Unary(UnOp::Not, _) => Some(PrimTy::Bool),
            ExprKind::Unary(UnOp::BitNot, operand) => self.known_recovery_operand_type(operand),
            ExprKind::Binary(
                BinOp::And
                | BinOp::Or
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge,
                _,
                _,
            ) => Some(PrimTy::Bool),
            ExprKind::Binary(
                operator @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div),
                left,
                right,
            ) => {
                if self.expression_has_unknown_field_error(left) {
                    self.known_recovery_operand_type(left)
                } else if let Some(left_type) = self.known_recovery_operand_type(left) {
                    Some(left_type)
                } else if let (ExprKind::Lit(literal), Some(right_type)) =
                    (left.kind, self.known_recovery_operand_type(right))
                {
                    let literal_kind = primitive_literal_kind(literal)?;
                    match primitive_arithmetic_literal_left_result(
                        operator,
                        literal_kind,
                        right_type,
                    ) {
                        PrimitiveArithmeticResult::Type(result) => Some(result),
                        PrimitiveArithmeticResult::TypeMismatch { .. }
                        | PrimitiveArithmeticResult::Unsupported => None,
                    }
                } else {
                    None
                }
            }
            ExprKind::Binary(
                BinOp::Rem
                | BinOp::WrapAdd
                | BinOp::WrapSub
                | BinOp::WrapMul
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr,
                left,
                _,
            ) if !self.expression_has_unknown_field_error(left) => {
                self.known_recovery_operand_type(left)
            }
            _ => None,
        }
    }

    fn check_error_tainted_expression(&mut self, expression: &Expr<'hir>) -> Result<(), ()> {
        let ExprKind::Binary(operator, left, right) = expression.kind else {
            return match expression.kind {
                ExprKind::Field { .. }
                    if self
                        .input
                        .unknown_self_fields
                        .is_some_and(|fields| fields.contains(&expression.hir_id)) =>
                {
                    Ok(())
                }
                ExprKind::Path(_)
                    if self.check_local_path(expression).is_ok_and(|local_type| {
                        matches!(
                            local_type,
                            LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
                        )
                    }) =>
                {
                    Ok(())
                }
                ExprKind::Unary(UnOp::Not, operand)
                    if matches!(operand.kind, ExprKind::Path(_))
                        && self.check_local_path(operand).is_ok_and(|local_type| {
                            matches!(
                                local_type,
                                LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
                            )
                        }) =>
                {
                    if self.has_unresolved_error_literal_path(operand) {
                        return Err(());
                    }
                    if let Some(operand_type) = self.known_recovery_operand_type(operand) {
                        match primitive_bool_operand_result(operand_type) {
                            PrimitiveBoolOperandResult::Bool => {}
                            PrimitiveBoolOperandResult::TypeMismatch { found } => {
                                self.errors.push(PrimitiveBodyError {
                                    node: operand.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                                        expected: PrimTy::Bool,
                                        found,
                                    },
                                });
                            }
                            PrimitiveBoolOperandResult::Unsupported => return Err(()),
                        }
                    }
                    Ok(())
                }
                ExprKind::Unary(UnOp::BitNot, operand)
                    if matches!(operand.kind, ExprKind::Path(_))
                        && self.check_local_path(operand).is_ok_and(|local_type| {
                            matches!(
                                local_type,
                                LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
                            )
                        }) =>
                {
                    if self.has_unresolved_error_literal_path(operand) {
                        return Err(());
                    }
                    if let Some(operand_type) = self.known_recovery_operand_type(operand) {
                        if !is_bitwise_primitive(operand_type) {
                            self.errors.push(PrimitiveBodyError {
                                node: expression.hir_id,
                                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                                    operator: UnOp::BitNot,
                                    operand: operand_type,
                                },
                            });
                        }
                    }
                    Ok(())
                }
                _ => Err(()),
            };
        };
        let left_has_error = self.expression_has_unknown_field_error(left);
        let right_has_error = self.expression_has_unknown_field_error(right);
        if self.has_unresolved_error_literal_path(left)
            || self.has_unresolved_error_literal_path(right)
        {
            return Err(());
        }
        if !self.is_supported_error_tainted_binary_shape(
            operator,
            left,
            right,
            left_has_error,
            right_has_error,
        ) {
            return Err(());
        }
        if !left_has_error && !right_has_error {
            return Err(());
        }

        match operator {
            BinOp::And | BinOp::Or => {
                for operand in [left, right] {
                    self.check_error_tainted_logical_operand(operand)?;
                }
            }
            BinOp::Shl | BinOp::Shr => {
                if let Some(left_type) = self.known_recovery_operand_type(left) {
                    if !is_bitwise_primitive(left_type) {
                        self.report_operator_not_supported(
                            expression.hir_id,
                            operator,
                            PrimitiveOperatorOperand::Concrete(left_type),
                        );
                    }
                }
                if !right_has_error {
                    if let ExprKind::Lit(literal) = right.kind {
                        if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer) {
                            return Err(());
                        }
                        self.check_annotated_literal(
                            PrimTy::U32,
                            literal,
                            PrimitiveLiteralKind::Integer,
                            right.hir_id,
                        );
                    }
                }
                if let Some(right_type) = self.known_recovery_operand_type(right) {
                    if right_type != PrimTy::U32 {
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch {
                                expected: PrimTy::U32,
                                found: right_type,
                            },
                        });
                    }
                }
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                match (
                    self.known_recovery_operand_type(left),
                    self.known_recovery_operand_type(right),
                ) {
                    (Some(left_type), Some(right_type)) => {
                        match primitive_arithmetic_result(operator, left_type, right_type) {
                            PrimitiveArithmeticResult::Type(_) => {}
                            PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                                self.errors.push(PrimitiveBodyError {
                                    node: right.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                                });
                            }
                            PrimitiveArithmeticResult::Unsupported => return Err(()),
                        }
                    }
                    (Some(left_type), None) if left_has_error => {
                        if !is_arithmetic_primitive(left_type) {
                            return Err(());
                        }
                        let ExprKind::Lit(literal) = right.kind else {
                            return Ok(());
                        };
                        let Some(
                            literal_kind @ (PrimitiveLiteralKind::Integer
                            | PrimitiveLiteralKind::FloatingPoint),
                        ) = primitive_literal_kind(literal)
                        else {
                            return Err(());
                        };
                        if primitive_arithmetic_literal_expected_type(
                            operator,
                            left_type,
                            literal_kind,
                        )
                        .is_none()
                        {
                            return Err(());
                        }
                        self.check_annotated_literal(
                            left_type,
                            literal,
                            literal_kind,
                            right.hir_id,
                        );
                    }
                    (None, Some(right_type)) if right_has_error => {
                        let ExprKind::Lit(literal) = left.kind else {
                            return Ok(());
                        };
                        let Some(literal_kind) = primitive_literal_kind(literal) else {
                            return Err(());
                        };
                        match primitive_arithmetic_literal_left_result(
                            operator,
                            literal_kind,
                            right_type,
                        ) {
                            PrimitiveArithmeticResult::Type(expected) => {
                                self.check_annotated_literal(
                                    expected,
                                    literal,
                                    literal_kind,
                                    left.hir_id,
                                );
                            }
                            PrimitiveArithmeticResult::TypeMismatch { expected, found } => {
                                self.errors.push(PrimitiveBodyError {
                                    node: right.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                                });
                            }
                            PrimitiveArithmeticResult::Unsupported => return Err(()),
                        }
                    }
                    _ => {}
                }
            }
            BinOp::Rem => {
                if let (Some(left_type), Some(right_type)) = (
                    self.known_recovery_operand_type(left),
                    self.known_recovery_operand_type(right),
                ) {
                    if !is_arithmetic_primitive(left_type) || !is_arithmetic_primitive(right_type) {
                        return Err(());
                    }
                    if left_type != right_type {
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch {
                                expected: left_type,
                                found: right_type,
                            },
                        });
                    }
                    if !is_integer_primitive(left_type) {
                        self.report_unsupported_float_remainder(expression.hir_id, left_type);
                    }
                }
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if let (Some(left_type), Some(right_type)) = (
                    self.known_recovery_operand_type(left),
                    self.known_recovery_operand_type(right),
                ) {
                    match primitive_comparison_result(operator, left_type, right_type) {
                        PrimitiveComparisonResult::Bool => {}
                        PrimitiveComparisonResult::TypeMismatch { expected, found } => {
                            self.errors.push(PrimitiveBodyError {
                                node: right.hir_id,
                                kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                            });
                        }
                        PrimitiveComparisonResult::Unsupported => return Err(()),
                    }
                }
            }
            BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
                if let (Some(left_type), Some(right_type)) = (
                    self.known_recovery_operand_type(left),
                    self.known_recovery_operand_type(right),
                ) {
                    if !supports_primitive_literal(left_type)
                        || !supports_primitive_literal(right_type)
                    {
                        return Err(());
                    }
                    if left_type != right_type && left_type != PrimTy::Char {
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch {
                                expected: left_type,
                                found: right_type,
                            },
                        });
                    }
                    if !is_integer_primitive(left_type) {
                        self.report_operator_not_supported(
                            expression.hir_id,
                            operator,
                            PrimitiveOperatorOperand::Concrete(left_type),
                        );
                    }
                }
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let left_type = self.known_recovery_operand_type(left);
                let right_type = self.known_recovery_operand_type(right);
                match (left_type, right_type) {
                    (Some(left_type), Some(right_type)) => {
                        if !supports_primitive_literal(left_type)
                            || !supports_primitive_literal(right_type)
                        {
                            return Err(());
                        }
                        if is_integer_primitive(left_type) || left_type == PrimTy::Byte {
                            if left_type != right_type {
                                self.errors.push(PrimitiveBodyError {
                                    node: right.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                                        expected: left_type,
                                        found: right_type,
                                    },
                                });
                            }
                        } else if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
                            if left_type != right_type {
                                self.errors.push(PrimitiveBodyError {
                                    node: right.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                                        expected: left_type,
                                        found: right_type,
                                    },
                                });
                            }
                            self.report_operator_not_supported(
                                expression.hir_id,
                                operator,
                                PrimitiveOperatorOperand::Concrete(left_type),
                            );
                        } else if left_type == PrimTy::Char {
                            self.errors.push(PrimitiveBodyError {
                                node: expression.hir_id,
                                kind: PrimitiveBodyErrorKind::OperatorTraitNotSupported {
                                    operator,
                                    left: left_type,
                                    right: right_type,
                                },
                            });
                        } else {
                            return Err(());
                        }
                    }
                    (Some(left_type), None) if right_has_error => {
                        if !supports_primitive_literal(left_type) {
                            return Err(());
                        }
                        if matches!(left_type, PrimTy::Bool | PrimTy::F32 | PrimTy::F64) {
                            self.report_operator_not_supported(
                                expression.hir_id,
                                operator,
                                PrimitiveOperatorOperand::Concrete(left_type),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn has_unresolved_error_literal_path(&self, expression: &Expr<'hir>) -> bool {
        let ExprKind::Path(_) = expression.kind else {
            return false;
        };
        self.plain_local_path_binding(expression)
            .and_then(|binding| self.known_bindings.get(&binding).copied())
            .is_some_and(|local_type| match local_type {
                LocalType::ErrorLiteralVariable(id) => self
                    .literal_variables
                    .get(id)
                    .is_some_and(|variable| variable.resolved.is_none()),
                _ => false,
            })
    }

    fn is_supported_error_tainted_binary_shape(
        &self,
        operator: BinOp,
        left: &Expr<'hir>,
        right: &Expr<'hir>,
        left_has_error: bool,
        right_has_error: bool,
    ) -> bool {
        let is_path = |expression: &Expr<'hir>| {
            matches!(expression.kind, ExprKind::Path(_))
                && self.check_local_path(expression).is_ok()
        };
        let is_unknown_self_field = |expression: &Expr<'hir>| {
            matches!(expression.kind, ExprKind::Field { .. })
                && self
                    .input
                    .unknown_self_fields
                    .is_some_and(|fields| fields.contains(&expression.hir_id))
        };
        let is_path_or_unknown_field =
            |expression: &Expr<'hir>| is_path(expression) || is_unknown_self_field(expression);
        let is_literal = |expression: &Expr<'hir>| {
            matches!(
                expression.kind,
                ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some()
            )
        };
        let is_path_or_literal =
            |expression: &Expr<'hir>| is_path(expression) || is_literal(expression);
        let is_path_literal_or_unknown_field = |expression: &Expr<'hir>| {
            is_path_or_literal(expression) || is_unknown_self_field(expression)
        };
        let primitive_path = |expression: &Expr<'hir>| self.known_recovery_operand_type(expression);
        let literal_kind = |expression: &Expr<'hir>| match expression.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal),
            _ => None,
        };
        let clean_operand_is_arithmetic = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some_and(is_arithmetic_primitive)
                || matches!(
                    literal_kind(expression),
                    Some(PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint)
                )
        };
        let clean_operand_is_integer_arithmetic = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some_and(is_integer_primitive)
                || literal_kind(expression) == Some(PrimitiveLiteralKind::Integer)
        };
        let clean_operand_is_shift_value = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some()
                || matches!(
                    literal_kind(expression),
                    Some(PrimitiveLiteralKind::Byte | PrimitiveLiteralKind::Integer)
                )
        };
        let clean_operand_is_shift_count = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some()
                || literal_kind(expression) == Some(PrimitiveLiteralKind::Integer)
        };
        let clean_operand_is_bitwise = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some() || literal_kind(expression).is_some()
        };
        let clean_operand_is_comparable = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some() || literal_kind(expression).is_some()
        };

        let shape_is_supported = match operator {
            BinOp::And | BinOp::Or => {
                self.error_tainted_logical_operand_shape(
                    left,
                    &is_path_or_unknown_field,
                    &is_literal,
                ) && self.error_tainted_logical_operand_shape(
                    right,
                    &is_path_or_unknown_field,
                    &is_literal,
                )
            }
            BinOp::Shl | BinOp::Shr => {
                let supported_left_literal = matches!(
                    left.kind,
                    ExprKind::Lit(literal)
                        if matches!(
                            primitive_literal_kind(literal),
                            Some(PrimitiveLiteralKind::Byte | PrimitiveLiteralKind::Integer)
                        )
                );
                let supported_right_literal = matches!(
                    right.kind,
                    ExprKind::Lit(literal)
                        if primitive_literal_kind(literal)
                            == Some(PrimitiveLiteralKind::Integer)
                );
                (is_path_or_unknown_field(left) || supported_left_literal)
                    && (is_path_or_unknown_field(right) || supported_right_literal)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                (is_path_or_unknown_field(left) && is_path_or_unknown_field(right))
                    || (is_path_or_unknown_field(left) && is_literal(right))
                    || (is_literal(left) && is_path_or_unknown_field(right))
            }
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::WrapAdd
            | BinOp::WrapSub
            | BinOp::WrapMul
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor => {
                is_path_literal_or_unknown_field(left) && is_path_literal_or_unknown_field(right)
            }
        };
        if !shape_is_supported {
            return false;
        }
        if !left_has_error && !right_has_error {
            return false;
        }
        let error_operand_matches =
            |expression: &Expr<'hir>, has_error: bool, predicate: fn(PrimTy) -> bool| {
                !has_error || primitive_path(expression).map_or(true, predicate)
            };
        match operator {
            BinOp::And | BinOp::Or => [left, right]
                .into_iter()
                .zip([left_has_error, right_has_error])
                .all(|(operand, has_error)| {
                    if has_error {
                        error_operand_matches(operand, true, |ty| {
                            !matches!(
                                primitive_bool_operand_result(ty),
                                PrimitiveBoolOperandResult::Unsupported
                            )
                        })
                    } else {
                        self.error_tainted_clean_logical_operand(operand)
                    }
                }),
            BinOp::Shl | BinOp::Shr => {
                error_operand_matches(left, left_has_error, |_| true)
                    && error_operand_matches(right, right_has_error, |_| true)
                    && (left_has_error || clean_operand_is_shift_value(left))
                    && (right_has_error || clean_operand_is_shift_count(right))
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                error_operand_matches(left, left_has_error, supports_primitive_literal)
                    && error_operand_matches(right, right_has_error, supports_primitive_literal)
                    && (left_has_error || clean_operand_is_arithmetic(left))
                    && (right_has_error || clean_operand_is_arithmetic(right))
            }
            BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
                error_operand_matches(left, left_has_error, supports_primitive_literal)
                    && error_operand_matches(right, right_has_error, supports_primitive_literal)
                    && (left_has_error || clean_operand_is_integer_arithmetic(left))
                    && (right_has_error || clean_operand_is_integer_arithmetic(right))
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                error_operand_matches(left, left_has_error, supports_primitive_literal)
                    && error_operand_matches(right, right_has_error, supports_primitive_literal)
                    && (left_has_error || clean_operand_is_bitwise(left))
                    && (right_has_error || clean_operand_is_bitwise(right))
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                error_operand_matches(left, left_has_error, supports_primitive_literal)
                    && error_operand_matches(right, right_has_error, supports_primitive_literal)
                    && (left_has_error || clean_operand_is_comparable(left))
                    && (right_has_error || clean_operand_is_comparable(right))
            }
        }
    }

    // Shape gate for error-tainted `and`/`or` operands, mirroring the nested
    // operand forms `check_logical_operand` admits so a missing-field or
    // otherwise-tainted body keeps its diagnostics instead of falling back
    // to Unsupported.
    fn error_tainted_logical_operand_shape(
        &self,
        operand: &Expr<'hir>,
        is_path_or_unknown_field: &impl Fn(&Expr<'hir>) -> bool,
        is_literal: &impl Fn(&Expr<'hir>) -> bool,
    ) -> bool {
        match operand.kind {
            ExprKind::Path(_) | ExprKind::Field { .. } => is_path_or_unknown_field(operand),
            ExprKind::Lit(Lit::Bool(_)) => true,
            ExprKind::Unary(UnOp::Not, inner) => self.error_tainted_logical_operand_shape(
                inner,
                is_path_or_unknown_field,
                is_literal,
            ),
            ExprKind::Binary(inner_operator, left, right) => match inner_operator {
                BinOp::And | BinOp::Or => {
                    self.error_tainted_logical_operand_shape(
                        left,
                        is_path_or_unknown_field,
                        is_literal,
                    ) && self.error_tainted_logical_operand_shape(
                        right,
                        is_path_or_unknown_field,
                        is_literal,
                    )
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    (is_path_or_unknown_field(left) && is_path_or_unknown_field(right))
                        || (is_path_or_unknown_field(left) && is_literal(right))
                        || (is_literal(left) && is_path_or_unknown_field(right))
                }
                _ => false,
            },
            _ => false,
        }
    }

    // Whether a clean (error-free) operand of an error-tainted `and`/`or`
    // still produces `Bool` during recovery checking, mirroring the nested
    // operand forms the primary checker admits.
    fn error_tainted_clean_logical_operand(&self, operand: &Expr<'hir>) -> bool {
        match operand.kind {
            ExprKind::Lit(Lit::Bool(_)) => true,
            ExprKind::Unary(UnOp::Not, inner) => self.error_tainted_clean_logical_operand(inner),
            ExprKind::Binary(inner_operator, left, right)
                if matches!(inner_operator, BinOp::And | BinOp::Or) =>
            {
                self.error_tainted_clean_logical_operand(left)
                    && self.error_tainted_clean_logical_operand(right)
            }
            ExprKind::Binary(inner_operator, left, right)
                if matches!(
                    inner_operator,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) =>
            {
                let comparable = |expression: &Expr<'hir>| {
                    self.known_recovery_operand_type(expression).is_some()
                        || matches!(
                            expression.kind,
                            ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some()
                        )
                };
                comparable(left) && comparable(right)
            }
            _ => match self.known_recovery_operand_type(operand) {
                Some(ty) => !matches!(
                    primitive_bool_operand_result(ty),
                    PrimitiveBoolOperandResult::Unsupported
                ),
                None => false,
            },
        }
    }

    // Diagnostics for operands of an error-tainted `and`/`or`, mirroring the
    // primary `check_logical`/`check_comparison` diag for each nested leaf the
    // early-returning primary checker never reached.
    fn check_error_tainted_logical_operand(&mut self, operand: &Expr<'hir>) -> Result<(), ()> {
        match operand.kind {
            ExprKind::Lit(Lit::Bool(_)) => Ok(()),
            ExprKind::Unary(UnOp::Not, inner) => self.check_error_tainted_logical_operand(inner),
            ExprKind::Binary(inner_operator, left, right)
                if matches!(inner_operator, BinOp::And | BinOp::Or) =>
            {
                self.check_error_tainted_logical_operand(left)?;
                self.check_error_tainted_logical_operand(right)
            }
            ExprKind::Binary(inner_operator, left, right)
                if matches!(
                    inner_operator,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) =>
            {
                let operand_type = |expression: &Expr<'hir>| match expression.kind {
                    ExprKind::Lit(Lit::Bool(_)) => Some(PrimTy::Bool),
                    _ => self.known_recovery_operand_type(expression),
                };
                if let (Some(left_type), Some(right_type)) =
                    (operand_type(left), operand_type(right))
                {
                    match primitive_comparison_result(inner_operator, left_type, right_type) {
                        PrimitiveComparisonResult::Bool => {}
                        PrimitiveComparisonResult::TypeMismatch { expected, found } => {
                            self.errors.push(PrimitiveBodyError {
                                node: right.hir_id,
                                kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                            });
                        }
                        PrimitiveComparisonResult::Unsupported => return Err(()),
                    }
                }
                Ok(())
            }
            _ => {
                if let Some(operand_type) = self.known_recovery_operand_type(operand) {
                    match primitive_bool_operand_result(operand_type) {
                        PrimitiveBoolOperandResult::Bool => {}
                        PrimitiveBoolOperandResult::TypeMismatch { found } => {
                            self.errors.push(PrimitiveBodyError {
                                node: operand.hir_id,
                                kind: PrimitiveBodyErrorKind::TypeMismatch {
                                    expected: PrimTy::Bool,
                                    found,
                                },
                            });
                        }
                        PrimitiveBoolOperandResult::Unsupported => return Err(()),
                    }
                }
                Ok(())
            }
        }
    }

    fn known_recovery_operand_type(&self, expression: &Expr<'hir>) -> Option<PrimTy> {
        match expression.kind {
            ExprKind::Path(_) => self
                .check_local_path(expression)
                .ok()
                .and_then(|local_type| match local_type {
                    LocalType::Error(found) => found,
                    LocalType::ErrorLiteralVariable(id) => self.literal_variables.get(id)?.resolved,
                    _ => self.concrete_local_type(local_type),
                }),
            ExprKind::Field { .. } => self
                .input
                .self_field_projections
                .and_then(|fields| fields.get(&expression.hir_id))
                .copied(),
            _ => None,
        }
    }

    fn check_concrete_tail_type(&mut self, found: PrimTy, node: HirId) {
        if found != self.input.return_type {
            self.errors.push(PrimitiveBodyError {
                node,
                kind: PrimitiveBodyErrorKind::TypeMismatch {
                    expected: self.input.return_type,
                    found,
                },
            });
        }
    }

    fn add_integer_literal_constraint(
        &mut self,
        id: usize,
        literal: Lit<'hir>,
        origin: HirId,
    ) -> Result<(), ()> {
        if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer) {
            return Err(());
        }
        let Some(variable) = self.literal_variables.get_mut(id) else {
            return Err(());
        };
        if variable.kind != PrimitiveLiteralKind::Integer
            || variable.resolved.is_some()
            || variable.additional_site.is_some()
        {
            return Err(());
        }
        variable.additional_site = Some(LiteralSite { literal, origin });
        Ok(())
    }

    fn add_float_literal_constraint(
        &mut self,
        id: usize,
        literal: Lit<'hir>,
        origin: HirId,
    ) -> Result<(), ()> {
        if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::FloatingPoint) {
            return Err(());
        }
        let Some(variable) = self.literal_variables.get_mut(id) else {
            return Err(());
        };
        if variable.kind != PrimitiveLiteralKind::FloatingPoint
            || variable.resolved.is_some()
            || variable.additional_site.is_some()
        {
            return Err(());
        }
        variable.additional_site = Some(LiteralSite { literal, origin });
        Ok(())
    }

    fn resolve_literal_variable(&mut self, id: usize, expected: PrimTy) -> Result<(), ()> {
        let symbols = self.input.symbols;
        let pointer_width = self.input.pointer_width;
        let errors = &mut self.errors;
        let Some(variable) = self.literal_variables.get_mut(id) else {
            return Err(());
        };
        if variable.resolved.is_some() || !literal_family_accepts(variable.kind, expected) {
            return Err(());
        }
        variable.resolved = Some(expected);
        if check_primitive_literal(expected, variable.literal, symbols, pointer_width)
            == PrimitiveLiteralResult::OutOfRange
        {
            errors.push(PrimitiveBodyError {
                node: variable.origin,
                kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                    expected,
                    found: variable.kind,
                },
            });
        }
        if let Some(site) = variable.additional_site {
            if check_primitive_literal(expected, site.literal, symbols, pointer_width)
                == PrimitiveLiteralResult::OutOfRange
            {
                errors.push(PrimitiveBodyError {
                    node: site.origin,
                    kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                        expected,
                        found: variable.kind,
                    },
                });
            }
        }
        Ok(())
    }

    fn default_unresolved_literals(&mut self, literal_floor: usize) {
        let symbols = self.input.symbols;
        let pointer_width = self.input.pointer_width;
        let mut deferred_errors = Vec::new();
        let mut deferred_operator_errors = Vec::new();
        for variable in &mut self.literal_variables[literal_floor..] {
            if variable.resolved.is_none() {
                let expected = match variable.kind {
                    PrimitiveLiteralKind::Integer => PrimTy::I64,
                    PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
                    PrimitiveLiteralKind::Bool
                    | PrimitiveLiteralKind::Byte
                    | PrimitiveLiteralKind::Character => unreachable!(),
                };
                variable.resolved = Some(expected);
                if check_primitive_literal(expected, variable.literal, symbols, pointer_width)
                    == PrimitiveLiteralResult::OutOfRange
                {
                    deferred_errors.push(PrimitiveBodyError {
                        node: variable.origin,
                        kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                            expected,
                            found: variable.kind,
                        },
                    });
                }
                if let Some(site) = variable.additional_site {
                    if check_primitive_literal(expected, site.literal, symbols, pointer_width)
                        == PrimitiveLiteralResult::OutOfRange
                    {
                        deferred_errors.push(PrimitiveBodyError {
                            node: site.origin,
                            kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                                expected,
                                found: variable.kind,
                            },
                        });
                    }
                }
            }
            if let Some(right) = variable.resolved {
                deferred_operator_errors.extend(variable.pending_operator_traits.iter().map(
                    |site| {
                        (
                            site.diagnostic_index,
                            PrimitiveBodyError {
                                node: site.expression,
                                kind: PrimitiveBodyErrorKind::DeferredOperatorTraitNotSupported {
                                    operator: site.operator,
                                    left: PrimTy::Char,
                                    right,
                                },
                            },
                        )
                    },
                ));
                deferred_operator_errors.extend(
                    variable
                        .pending_neg_traits
                        .iter()
                        .filter(|_| !is_neg_primitive(right))
                        .map(|site| {
                            (
                                site.diagnostic_index,
                                PrimitiveBodyError {
                                    node: site.expression,
                                    kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                                        operator: UnOp::Neg,
                                        operand: right,
                                    },
                                },
                            )
                        }),
                );
            }
        }
        self.errors.extend(deferred_errors);
        deferred_operator_errors.sort_by_key(|(index, _)| *index);
        for (index, error) in deferred_operator_errors {
            self.errors.insert(index.min(self.errors.len()), error);
        }
    }

    fn check_local_path(&self, expression: &Expr<'hir>) -> Result<LocalType, ()> {
        let local_type = self.resolve_local_path(expression)?;
        let binding = self.plain_local_path_binding(expression).ok_or(())?;
        if self.trusted_bindings.contains(&binding) {
            self.record_expression_type(expression.hir_id, local_type);
        }
        Ok(local_type)
    }

    fn resolve_local_path(&self, expression: &Expr<'hir>) -> Result<LocalType, ()> {
        let ExprKind::Path(path) = expression.kind else {
            return Err(());
        };
        if path.root != PathRoot::Plain
            || path.segments.len() != 1
            || path.segments[0].args.is_some()
        {
            return Err(());
        }

        let Some(binding) = self.plain_local_path_binding(expression) else {
            return Err(());
        };
        let Some(local_type) = self.known_bindings.get(&binding).copied() else {
            return Err(());
        };
        Ok(local_type)
    }

    fn is_eligible_unannotated_literal_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.direct_unannotated_integer_literals.contains(&binding)
                    || self.unannotated_integer_literal_aliases.contains(&binding)
                    || self.direct_unannotated_float_literals.contains(&binding)
                    || self.unannotated_float_literal_aliases.contains(&binding)
            })
    }

    fn is_eligible_resolved_computed_literal_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| self.resolved_computed_literal_paths.contains(&binding))
    }

    fn is_eligible_unannotated_integer_bitwise_pair_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_bitwise_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_addition_pair_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_addition_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_subtraction_pair_path(
        &self,
        expression: &Expr<'hir>,
    ) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_subtraction_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_multiplication_pair_path(
        &self,
        expression: &Expr<'hir>,
    ) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_multiplication_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_division_pair_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_division_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_remainder_pair_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_remainder_pair_paths
                    .contains(&binding)
            })
    }

    fn is_eligible_unannotated_integer_wrapping_pair_path(&self, expression: &Expr<'hir>) -> bool {
        self.plain_local_path_binding(expression)
            .is_some_and(|binding| {
                self.unannotated_integer_wrapping_pair_paths
                    .contains(&binding)
            })
    }

    fn plain_local_path_binding(&self, expression: &Expr<'hir>) -> Option<BindingId> {
        let ExprKind::Path(path) = expression.kind else {
            return None;
        };
        if path.root != PathRoot::Plain
            || path.segments.len() != 1
            || path.segments[0].args.is_some()
        {
            return None;
        }
        self.input.local_paths.get(&path.hir_id).copied()
    }
}

fn collect_primitive_expression_nodes<'hir>(
    expression: &'hir Expr<'hir>,
    nodes: &mut HashMap<HirId, Expr<'hir>>,
) {
    nodes.insert(expression.hir_id, *expression);
    match expression.kind {
        ExprKind::Block(block) => collect_primitive_block_nodes(block, nodes),
        ExprKind::If { cond, then, els } => {
            collect_primitive_expression_nodes(cond, nodes);
            collect_primitive_block_nodes(then, nodes);
            if let Some(els) = els {
                collect_primitive_expression_nodes(els, nodes);
            }
        }
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Unary(_, base)
        | ExprKind::Borrow(_, base)
        | ExprKind::Try(base)
        | ExprKind::Await(base)
        | ExprKind::Return(Some(base)) => collect_primitive_expression_nodes(base, nodes),
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            collect_primitive_expression_nodes(left, nodes);
            collect_primitive_expression_nodes(right, nodes);
        }
        ExprKind::Call { callee, args, base } => {
            collect_primitive_expression_nodes(callee, nodes);
            for argument in args {
                collect_primitive_expression_nodes(argument.expr, nodes);
            }
            if let Some(base) = base {
                collect_primitive_expression_nodes(base, nodes);
            }
        }
        _ => {}
    }
}

fn collect_primitive_block_nodes<'hir>(
    block: &'hir Block<'hir>,
    nodes: &mut HashMap<HirId, Expr<'hir>>,
) {
    for stmt in block.stmts {
        match stmt.kind {
            StmtKind::Let { init, .. } | StmtKind::Expr(init) => {
                collect_primitive_expression_nodes(init, nodes)
            }
            StmtKind::Assert { cond, msg, .. } => {
                collect_primitive_expression_nodes(cond, nodes);
                if let Some(msg) = msg {
                    collect_primitive_expression_nodes(msg, nodes);
                }
            }
            StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
                collect_primitive_expression_nodes(lhs, nodes);
                collect_primitive_expression_nodes(rhs, nodes);
            }
        };
    }
    if let Some(tail) = block.tail {
        collect_primitive_expression_nodes(tail, nodes);
    }
}

fn expression_has_primitive_result_taint(
    expression: &Expr<'_>,
    error_nodes: &HashSet<HirId>,
    unknown_self_fields: Option<&HashSet<HirId>>,
    local_paths: &HashMap<HirId, BindingId>,
    tainted_bindings: &HashSet<BindingId>,
) -> bool {
    error_nodes.contains(&expression.hir_id)
        || unknown_self_fields.is_some_and(|fields| fields.contains(&expression.hir_id))
        || match expression.kind {
            ExprKind::Path(path) => local_paths
                .get(&path.hir_id)
                .is_some_and(|binding| tainted_bindings.contains(binding)),
            ExprKind::Field { base, .. }
            | ExprKind::TupleField { base, .. }
            | ExprKind::Unary(_, base)
            | ExprKind::Borrow(_, base)
            | ExprKind::Try(base)
            | ExprKind::Await(base)
            | ExprKind::Return(Some(base)) => expression_has_primitive_result_taint(
                base,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ),
            ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                expression_has_primitive_result_taint(
                    left,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                ) || expression_has_primitive_result_taint(
                    right,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                )
            }
            ExprKind::Call { callee, args, base } => {
                expression_has_primitive_result_taint(
                    callee,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                ) || args.iter().any(|argument| {
                    expression_has_primitive_result_taint(
                        argument.expr,
                        error_nodes,
                        unknown_self_fields,
                        local_paths,
                        tainted_bindings,
                    )
                }) || base.is_some_and(|base| {
                    expression_has_primitive_result_taint(
                        base,
                        error_nodes,
                        unknown_self_fields,
                        local_paths,
                        tainted_bindings,
                    )
                })
            }
            ExprKind::Block(block) => block_has_primitive_result_taint(
                block,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ),
            ExprKind::If { cond, then, els } => {
                expression_has_primitive_result_taint(
                    cond,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                ) || block_has_primitive_result_taint(
                    then,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                ) || els.is_some_and(|els| {
                    expression_has_primitive_result_taint(
                        els,
                        error_nodes,
                        unknown_self_fields,
                        local_paths,
                        tainted_bindings,
                    )
                })
            }
            _ => false,
        }
}

fn block_has_primitive_result_taint(
    block: &Block<'_>,
    error_nodes: &HashSet<HirId>,
    unknown_self_fields: Option<&HashSet<HirId>>,
    local_paths: &HashMap<HirId, BindingId>,
    tainted_bindings: &HashSet<BindingId>,
) -> bool {
    block.stmts.iter().any(|stmt| match stmt.kind {
        StmtKind::Let { init, .. } | StmtKind::Expr(init) => expression_has_primitive_result_taint(
            init,
            error_nodes,
            unknown_self_fields,
            local_paths,
            tainted_bindings,
        ),
        StmtKind::Assert { cond, msg, .. } => {
            expression_has_primitive_result_taint(
                cond,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ) || msg.is_some_and(|msg| {
                expression_has_primitive_result_taint(
                    msg,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                )
            })
        }
        _ => false,
    }) || block.tail.is_some_and(|tail| {
        expression_has_primitive_result_taint(
            tail,
            error_nodes,
            unknown_self_fields,
            local_paths,
            tainted_bindings,
        )
    })
}

fn can_track_unannotated_float_literal_alias(
    source_has_float_literal_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_float_literal_provenance && initializer_is_plain_path && !alias_has_annotation
}

fn can_track_unannotated_integer_literal_alias(
    source_has_integer_literal_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_literal_provenance && initializer_is_plain_path && !alias_has_annotation
}

fn can_track_resolved_computed_literal_alias(
    source_has_resolved_computed_literal_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_resolved_computed_literal_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_bitwise_pair_alias(
    source_has_integer_bitwise_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_bitwise_pair_provenance && initializer_is_plain_path && !alias_has_annotation
}

fn can_track_unannotated_integer_addition_pair_alias(
    source_has_integer_addition_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_addition_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_wrapping_pair_alias(
    source_has_integer_wrapping_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_wrapping_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_subtraction_pair_alias(
    source_has_integer_subtraction_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_subtraction_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_multiplication_pair_alias(
    source_has_integer_multiplication_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_multiplication_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_division_pair_alias(
    source_has_integer_division_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_division_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn can_track_unannotated_integer_remainder_pair_alias(
    source_has_integer_remainder_pair_provenance: bool,
    initializer_is_plain_path: bool,
    alias_has_annotation: bool,
) -> bool {
    source_has_integer_remainder_pair_provenance
        && initializer_is_plain_path
        && !alias_has_annotation
}

fn primitive_comparison_result(
    operator: BinOp,
    left: PrimTy,
    right: PrimTy,
) -> PrimitiveComparisonResult {
    if !matches!(
        operator,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) || !supports_primitive_literal(left)
        || !supports_primitive_literal(right)
    {
        return PrimitiveComparisonResult::Unsupported;
    }
    if left != right {
        return PrimitiveComparisonResult::TypeMismatch {
            expected: left,
            found: right,
        };
    }
    PrimitiveComparisonResult::Bool
}

fn primitive_arithmetic_result(
    operator: BinOp,
    left: PrimTy,
    right: PrimTy,
) -> PrimitiveArithmeticResult {
    if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
        || !is_arithmetic_primitive(left)
        || !is_arithmetic_primitive(right)
    {
        return PrimitiveArithmeticResult::Unsupported;
    }
    if left == right {
        PrimitiveArithmeticResult::Type(left)
    } else {
        PrimitiveArithmeticResult::TypeMismatch {
            expected: left,
            found: right,
        }
    }
}

fn primitive_wrapping_literal_left_result(
    operator: BinOp,
    literal: PrimitiveLiteralKind,
    right: PrimTy,
) -> PrimitiveArithmeticResult {
    if !matches!(operator, BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul)
        || literal != PrimitiveLiteralKind::Integer
        || !supports_primitive_literal(right)
    {
        return PrimitiveArithmeticResult::Unsupported;
    }

    if is_integer_primitive(right) {
        PrimitiveArithmeticResult::Type(right)
    } else {
        PrimitiveArithmeticResult::TypeMismatch {
            expected: PrimTy::I64,
            found: right,
        }
    }
}

fn primitive_bitwise_literal_left_result(
    operator: BinOp,
    right: PrimTy,
) -> PrimitiveArithmeticResult {
    if !matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        || !supports_primitive_literal(right)
    {
        return PrimitiveArithmeticResult::Unsupported;
    }

    if is_integer_primitive(right) {
        PrimitiveArithmeticResult::Type(right)
    } else {
        PrimitiveArithmeticResult::TypeMismatch {
            expected: PrimTy::I64,
            found: right,
        }
    }
}

fn primitive_bitwise_integer_literal_pair_supported(
    operator: BinOp,
    right: PrimitiveLiteralKind,
) -> bool {
    matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        && right == PrimitiveLiteralKind::Integer
}

fn primitive_bitwise_bool_literal_pair_supported(
    operator: BinOp,
    right: PrimitiveLiteralKind,
) -> bool {
    matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        && right == PrimitiveLiteralKind::Bool
}

fn primitive_bitwise_char_literal_pair_supported(
    operator: BinOp,
    right: PrimitiveLiteralKind,
) -> bool {
    matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        && matches!(
            right,
            PrimitiveLiteralKind::Character
                | PrimitiveLiteralKind::Bool
                | PrimitiveLiteralKind::Byte
                | PrimitiveLiteralKind::Integer
                | PrimitiveLiteralKind::FloatingPoint
        )
}

fn primitive_bitwise_byte_literal_left_result(
    operator: BinOp,
    right: PrimTy,
) -> PrimitiveArithmeticResult {
    if !matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        || !supports_primitive_literal(right)
    {
        return PrimitiveArithmeticResult::Unsupported;
    }

    if right == PrimTy::Byte {
        PrimitiveArithmeticResult::Type(PrimTy::Byte)
    } else {
        PrimitiveArithmeticResult::TypeMismatch {
            expected: PrimTy::Byte,
            found: right,
        }
    }
}

fn primitive_bitwise_byte_literal_pair_result(
    operator: BinOp,
    right: PrimitiveLiteralKind,
) -> PrimitiveArithmeticResult {
    if right != PrimitiveLiteralKind::Byte {
        return PrimitiveArithmeticResult::Unsupported;
    }

    primitive_bitwise_byte_literal_left_result(operator, PrimTy::Byte)
}

fn primitive_bitwise_char_float_literal_rhs_type(operator: BinOp) -> Option<PrimTy> {
    matches!(operator, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor).then_some(PrimTy::F64)
}

fn can_defer_char_literal_path_trait(
    kind: PrimitiveLiteralKind,
    has_additional_site: bool,
    is_eligible_unannotated_numeric_literal_path: bool,
    is_eligible_unannotated_integer_literal_pair_path: bool,
) -> bool {
    match (kind, has_additional_site) {
        (PrimitiveLiteralKind::Integer, true) => is_eligible_unannotated_integer_literal_pair_path,
        (PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint, false) => {
            is_eligible_unannotated_numeric_literal_path
        }
        _ => false,
    }
}

const fn is_bitwise_primitive(primitive: PrimTy) -> bool {
    is_integer_primitive(primitive) || matches!(primitive, PrimTy::Byte)
}

const fn is_neg_primitive(primitive: PrimTy) -> bool {
    matches!(
        primitive,
        PrimTy::I8
            | PrimTy::I16
            | PrimTy::I32
            | PrimTy::I64
            | PrimTy::ISize
            | PrimTy::F32
            | PrimTy::F64
    )
}

const fn is_arithmetic_primitive(primitive: PrimTy) -> bool {
    is_integer_primitive(primitive) || matches!(primitive, PrimTy::F32 | PrimTy::F64)
}

fn primitive_arithmetic_literal_expected_type(
    operator: BinOp,
    left: PrimTy,
    literal: PrimitiveLiteralKind,
) -> Option<PrimTy> {
    (matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
        && is_arithmetic_primitive(left)
        && matches!(
            literal,
            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
        ))
    .then_some(left)
}

fn primitive_arithmetic_literal_left_result(
    operator: BinOp,
    literal: PrimitiveLiteralKind,
    right: PrimTy,
) -> PrimitiveArithmeticResult {
    if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
        || !is_arithmetic_primitive(right)
        || !matches!(
            literal,
            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
        )
    {
        return PrimitiveArithmeticResult::Unsupported;
    }

    if literal_family_accepts(literal, right) {
        PrimitiveArithmeticResult::Type(right)
    } else {
        let expected = match literal {
            PrimitiveLiteralKind::Integer => PrimTy::I64,
            PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
            PrimitiveLiteralKind::Bool
            | PrimitiveLiteralKind::Byte
            | PrimitiveLiteralKind::Character => unreachable!(),
        };
        PrimitiveArithmeticResult::TypeMismatch {
            expected,
            found: right,
        }
    }
}

fn primitive_comparison_literal_expected_type(
    kind: PrimitiveLiteralKind,
    path_type: PrimTy,
    literal_is_left: bool,
) -> Result<PrimTy, PrimTy> {
    if !literal_is_left {
        return Ok(path_type);
    }

    let default_type = match kind {
        PrimitiveLiteralKind::Integer => PrimTy::I64,
        PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
        PrimitiveLiteralKind::Bool => PrimTy::Bool,
        PrimitiveLiteralKind::Byte => PrimTy::Byte,
        PrimitiveLiteralKind::Character => PrimTy::Char,
    };
    let family_accepts = match kind {
        PrimitiveLiteralKind::Integer => is_integer_primitive(path_type),
        PrimitiveLiteralKind::FloatingPoint => matches!(path_type, PrimTy::F32 | PrimTy::F64),
        PrimitiveLiteralKind::Bool => path_type == PrimTy::Bool,
        PrimitiveLiteralKind::Byte => path_type == PrimTy::Byte,
        PrimitiveLiteralKind::Character => path_type == PrimTy::Char,
    };
    if family_accepts {
        Ok(path_type)
    } else {
        Err(default_type)
    }
}

fn primitive_bool_operand_result(found: PrimTy) -> PrimitiveBoolOperandResult {
    if found == PrimTy::Bool {
        PrimitiveBoolOperandResult::Bool
    } else if supports_primitive_literal(found) {
        PrimitiveBoolOperandResult::TypeMismatch { found }
    } else {
        PrimitiveBoolOperandResult::Unsupported
    }
}

fn literal_family_accepts(kind: PrimitiveLiteralKind, expected: PrimTy) -> bool {
    match kind {
        PrimitiveLiteralKind::Integer => is_integer_primitive(expected),
        PrimitiveLiteralKind::FloatingPoint => matches!(expected, PrimTy::F32 | PrimTy::F64),
        PrimitiveLiteralKind::Bool
        | PrimitiveLiteralKind::Byte
        | PrimitiveLiteralKind::Character => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PrimitiveArithmeticResult, PrimitiveBoolOperandResult, PrimitiveComparisonResult,
        PrimitiveLiteralKind, can_defer_char_literal_path_trait,
        can_track_resolved_computed_literal_alias, can_track_unannotated_float_literal_alias,
        can_track_unannotated_integer_addition_pair_alias,
        can_track_unannotated_integer_bitwise_pair_alias,
        can_track_unannotated_integer_division_pair_alias,
        can_track_unannotated_integer_literal_alias,
        can_track_unannotated_integer_multiplication_pair_alias,
        can_track_unannotated_integer_remainder_pair_alias,
        can_track_unannotated_integer_subtraction_pair_alias,
        can_track_unannotated_integer_wrapping_pair_alias,
        primitive_arithmetic_literal_expected_type, primitive_arithmetic_literal_left_result,
        primitive_arithmetic_result, primitive_bitwise_bool_literal_pair_supported,
        primitive_bitwise_byte_literal_left_result, primitive_bitwise_byte_literal_pair_result,
        primitive_bitwise_char_float_literal_rhs_type,
        primitive_bitwise_char_literal_pair_supported,
        primitive_bitwise_integer_literal_pair_supported, primitive_bool_operand_result,
        primitive_comparison_literal_expected_type, primitive_comparison_result,
        primitive_shift_left_literal_supported,
    };
    use sai_hir::expressions::BinOp;
    use sai_resolve::prelude::PrimTy;

    const ARITHMETIC_OPERATORS: [BinOp; 4] = [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div];

    const COMPARISON_OPERATORS: [BinOp; 6] = [
        BinOp::Eq,
        BinOp::Ne,
        BinOp::Lt,
        BinOp::Le,
        BinOp::Gt,
        BinOp::Ge,
    ];

    const SUPPORTED_PRIMITIVES: [PrimTy; 15] = [
        PrimTy::I8,
        PrimTy::I16,
        PrimTy::I32,
        PrimTy::I64,
        PrimTy::U8,
        PrimTy::U16,
        PrimTy::U32,
        PrimTy::U64,
        PrimTy::ISize,
        PrimTy::USize,
        PrimTy::F32,
        PrimTy::F64,
        PrimTy::Bool,
        PrimTy::Byte,
        PrimTy::Char,
    ];

    #[test]
    fn arithmetic_accepts_matching_integer_and_float_primitives_only() {
        let arithmetic_primitives = [
            PrimTy::I8,
            PrimTy::I16,
            PrimTy::I32,
            PrimTy::I64,
            PrimTy::U8,
            PrimTy::U16,
            PrimTy::U32,
            PrimTy::U64,
            PrimTy::ISize,
            PrimTy::USize,
            PrimTy::F32,
            PrimTy::F64,
        ];
        for operator in ARITHMETIC_OPERATORS {
            for primitive in arithmetic_primitives {
                assert_eq!(
                    primitive_arithmetic_result(operator, primitive, primitive),
                    PrimitiveArithmeticResult::Type(primitive),
                    "{operator:?} on {primitive:?}"
                );
            }
            for (left, right) in [(PrimTy::I32, PrimTy::U32), (PrimTy::F32, PrimTy::F64)] {
                assert_eq!(
                    primitive_arithmetic_result(operator, left, right),
                    PrimitiveArithmeticResult::TypeMismatch {
                        expected: left,
                        found: right,
                    },
                    "{operator:?} on {left:?} and {right:?}"
                );
            }
            for primitive in [PrimTy::Bool, PrimTy::Byte, PrimTy::Char, PrimTy::Never] {
                assert_eq!(
                    primitive_arithmetic_result(operator, primitive, primitive),
                    PrimitiveArithmeticResult::Unsupported,
                    "{operator:?} on {primitive:?}"
                );
            }
            assert_eq!(
                primitive_arithmetic_result(operator, PrimTy::I32, PrimTy::Byte),
                PrimitiveArithmeticResult::Unsupported
            );
        }
        for unsupported in [BinOp::Rem, BinOp::And, BinOp::Eq] {
            assert_eq!(
                primitive_arithmetic_result(unsupported, PrimTy::I32, PrimTy::I32),
                PrimitiveArithmeticResult::Unsupported,
                "{unsupported:?}"
            );
        }
    }

    #[test]
    fn arithmetic_accepts_only_numeric_literals_against_concrete_numeric_left_types() {
        let arithmetic_primitives = [
            PrimTy::I8,
            PrimTy::I16,
            PrimTy::I32,
            PrimTy::I64,
            PrimTy::U8,
            PrimTy::U16,
            PrimTy::U32,
            PrimTy::U64,
            PrimTy::ISize,
            PrimTy::USize,
            PrimTy::F32,
            PrimTy::F64,
        ];
        for operator in ARITHMETIC_OPERATORS {
            for primitive in arithmetic_primitives {
                assert_eq!(
                    primitive_arithmetic_literal_expected_type(
                        operator,
                        primitive,
                        PrimitiveLiteralKind::Integer
                    ),
                    Some(primitive),
                    "integer literal on the right of {primitive:?} {operator:?}"
                );
                assert_eq!(
                    primitive_arithmetic_literal_expected_type(
                        operator,
                        primitive,
                        PrimitiveLiteralKind::FloatingPoint
                    ),
                    Some(primitive),
                    "float literal on the right of {primitive:?} {operator:?}"
                );
                for literal in [
                    PrimitiveLiteralKind::Bool,
                    PrimitiveLiteralKind::Byte,
                    PrimitiveLiteralKind::Character,
                ] {
                    assert_eq!(
                        primitive_arithmetic_literal_expected_type(operator, primitive, literal),
                        None,
                        "{literal:?} on the right of {primitive:?} {operator:?}"
                    );
                }
            }
            for primitive in [PrimTy::Bool, PrimTy::Byte, PrimTy::Char, PrimTy::Never] {
                assert_eq!(
                    primitive_arithmetic_literal_expected_type(
                        operator,
                        primitive,
                        PrimitiveLiteralKind::Integer
                    ),
                    None,
                    "non-numeric {primitive:?} left type for {operator:?}"
                );
            }
        }
        for operator in [BinOp::Rem, BinOp::And, BinOp::Eq] {
            assert_eq!(
                primitive_arithmetic_literal_expected_type(
                    operator,
                    PrimTy::I32,
                    PrimitiveLiteralKind::Integer
                ),
                None,
                "numeric literals remain gated for {operator:?}"
            );
        }
    }

    #[test]
    fn arithmetic_left_literals_unify_with_concrete_paths_by_family() {
        let integer_types = [
            PrimTy::I8,
            PrimTy::I16,
            PrimTy::I32,
            PrimTy::I64,
            PrimTy::U8,
            PrimTy::U16,
            PrimTy::U32,
            PrimTy::U64,
            PrimTy::ISize,
            PrimTy::USize,
        ];
        for operator in ARITHMETIC_OPERATORS {
            for primitive in integer_types {
                assert_eq!(
                    primitive_arithmetic_literal_left_result(
                        operator,
                        PrimitiveLiteralKind::Integer,
                        primitive
                    ),
                    PrimitiveArithmeticResult::Type(primitive),
                    "integer literal on the left of {primitive:?} {operator:?}"
                );
            }
            for primitive in [PrimTy::F32, PrimTy::F64] {
                assert_eq!(
                    primitive_arithmetic_literal_left_result(
                        operator,
                        PrimitiveLiteralKind::FloatingPoint,
                        primitive
                    ),
                    PrimitiveArithmeticResult::Type(primitive),
                    "float literal on the left of {primitive:?} {operator:?}"
                );
            }
            assert_eq!(
                primitive_arithmetic_literal_left_result(
                    operator,
                    PrimitiveLiteralKind::Integer,
                    PrimTy::F32
                ),
                PrimitiveArithmeticResult::TypeMismatch {
                    expected: PrimTy::I64,
                    found: PrimTy::F32,
                }
            );
            assert_eq!(
                primitive_arithmetic_literal_left_result(
                    operator,
                    PrimitiveLiteralKind::FloatingPoint,
                    PrimTy::U8
                ),
                PrimitiveArithmeticResult::TypeMismatch {
                    expected: PrimTy::F64,
                    found: PrimTy::U8,
                }
            );
            for literal in [
                PrimitiveLiteralKind::Bool,
                PrimitiveLiteralKind::Byte,
                PrimitiveLiteralKind::Character,
            ] {
                assert_eq!(
                    primitive_arithmetic_literal_left_result(operator, literal, PrimTy::I32),
                    PrimitiveArithmeticResult::Unsupported,
                    "{literal:?} literal on the left of {operator:?}"
                );
            }
            for primitive in [PrimTy::Bool, PrimTy::Byte, PrimTy::Char, PrimTy::Never] {
                assert_eq!(
                    primitive_arithmetic_literal_left_result(
                        operator,
                        PrimitiveLiteralKind::Integer,
                        primitive
                    ),
                    PrimitiveArithmeticResult::Unsupported,
                    "non-numeric {primitive:?} path on the right of {operator:?}"
                );
            }
        }
        for operator in [BinOp::Rem, BinOp::And, BinOp::Eq] {
            assert_eq!(
                primitive_arithmetic_literal_left_result(
                    operator,
                    PrimitiveLiteralKind::Integer,
                    PrimTy::I32
                ),
                PrimitiveArithmeticResult::Unsupported,
                "left literals remain gated for {operator:?}"
            );
        }
    }

    #[test]
    fn every_comparison_operator_accepts_each_supported_primitive_type() {
        for operator in COMPARISON_OPERATORS {
            for primitive in SUPPORTED_PRIMITIVES {
                assert_eq!(
                    primitive_comparison_result(operator, primitive, primitive),
                    PrimitiveComparisonResult::Bool,
                    "{operator:?} on {primitive:?}"
                );
            }
        }
    }

    #[test]
    fn primitive_comparison_rejects_mismatched_types_and_non_comparison_operators() {
        for operator in COMPARISON_OPERATORS {
            assert_eq!(
                primitive_comparison_result(operator, PrimTy::I32, PrimTy::U32),
                PrimitiveComparisonResult::TypeMismatch {
                    expected: PrimTy::I32,
                    found: PrimTy::U32,
                }
            );
        }
        assert_eq!(
            primitive_comparison_result(BinOp::Add, PrimTy::I32, PrimTy::I32),
            PrimitiveComparisonResult::Unsupported
        );
        assert_eq!(
            primitive_comparison_result(BinOp::Eq, PrimTy::Never, PrimTy::Never),
            PrimitiveComparisonResult::Unsupported
        );
    }

    #[test]
    fn comparison_literals_follow_operand_order_and_literal_family() {
        for (kind, matching_path, other_path) in [
            (super::PrimitiveLiteralKind::Bool, PrimTy::Bool, PrimTy::I32),
            (
                super::PrimitiveLiteralKind::Character,
                PrimTy::Char,
                PrimTy::I32,
            ),
            (super::PrimitiveLiteralKind::Byte, PrimTy::Byte, PrimTy::I32),
            (
                super::PrimitiveLiteralKind::Integer,
                PrimTy::I8,
                PrimTy::F32,
            ),
            (
                super::PrimitiveLiteralKind::FloatingPoint,
                PrimTy::F32,
                PrimTy::I8,
            ),
        ] {
            assert_eq!(
                primitive_comparison_literal_expected_type(kind, matching_path, true),
                Ok(matching_path),
                "{kind:?} literal on the left"
            );
            assert_eq!(
                primitive_comparison_literal_expected_type(kind, matching_path, false),
                Ok(matching_path),
                "{kind:?} literal on the right"
            );
            let expected_default = match kind {
                super::PrimitiveLiteralKind::Integer => PrimTy::I64,
                super::PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
                super::PrimitiveLiteralKind::Bool => PrimTy::Bool,
                super::PrimitiveLiteralKind::Byte => PrimTy::Byte,
                super::PrimitiveLiteralKind::Character => PrimTy::Char,
            };
            assert_eq!(
                primitive_comparison_literal_expected_type(kind, other_path, true),
                Err(expected_default),
                "{kind:?} literal on the left with incompatible path type"
            );
            assert_eq!(
                primitive_comparison_literal_expected_type(kind, other_path, false),
                Ok(other_path),
                "{kind:?} literal on the right is checked against the path"
            );
        }

        for integer_type in [
            PrimTy::I8,
            PrimTy::I16,
            PrimTy::I32,
            PrimTy::I64,
            PrimTy::U8,
            PrimTy::U16,
            PrimTy::U32,
            PrimTy::U64,
            PrimTy::ISize,
            PrimTy::USize,
        ] {
            assert_eq!(
                primitive_comparison_literal_expected_type(
                    super::PrimitiveLiteralKind::Integer,
                    integer_type,
                    true,
                ),
                Ok(integer_type)
            );
        }
        assert_eq!(
            primitive_comparison_literal_expected_type(
                super::PrimitiveLiteralKind::FloatingPoint,
                PrimTy::F64,
                true,
            ),
            Ok(PrimTy::F64)
        );
    }

    #[test]
    fn logical_and_not_operands_require_bool_and_reject_unsupported_types() {
        assert_eq!(
            primitive_bool_operand_result(PrimTy::Bool),
            PrimitiveBoolOperandResult::Bool
        );
        for primitive in SUPPORTED_PRIMITIVES
            .into_iter()
            .filter(|ty| *ty != PrimTy::Bool)
        {
            assert_eq!(
                primitive_bool_operand_result(primitive),
                PrimitiveBoolOperandResult::TypeMismatch { found: primitive },
                "{primitive:?}"
            );
        }
        assert_eq!(
            primitive_bool_operand_result(PrimTy::Never),
            PrimitiveBoolOperandResult::Unsupported
        );
    }

    #[test]
    fn shift_left_accepts_only_integer_and_byte_literal_kinds() {
        for kind in [PrimitiveLiteralKind::Integer, PrimitiveLiteralKind::Byte] {
            assert!(
                primitive_shift_left_literal_supported(kind),
                "{kind:?} literals are supported on the left of a shift"
            );
        }
        for kind in [
            PrimitiveLiteralKind::FloatingPoint,
            PrimitiveLiteralKind::Bool,
            PrimitiveLiteralKind::Character,
        ] {
            assert!(
                !primitive_shift_left_literal_supported(kind),
                "{kind:?} literals remain unsupported on the left of a shift"
            );
        }
    }

    #[test]
    fn byte_literal_bitwise_left_requires_a_byte_path_for_each_bitwise_operator() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert_eq!(
                primitive_bitwise_byte_literal_left_result(operator, PrimTy::Byte),
                PrimitiveArithmeticResult::Type(PrimTy::Byte),
                "{operator:?} accepts a Byte right path"
            );
            for right in SUPPORTED_PRIMITIVES
                .into_iter()
                .filter(|primitive| *primitive != PrimTy::Byte)
            {
                assert_eq!(
                    primitive_bitwise_byte_literal_left_result(operator, right),
                    PrimitiveArithmeticResult::TypeMismatch {
                        expected: PrimTy::Byte,
                        found: right,
                    },
                    "{operator:?} with a {right:?} right path"
                );
            }
        }

        assert_eq!(
            primitive_bitwise_byte_literal_left_result(BinOp::Add, PrimTy::Byte),
            PrimitiveArithmeticResult::Unsupported
        );
        assert_eq!(
            primitive_bitwise_byte_literal_left_result(BinOp::BitXor, PrimTy::Never),
            PrimitiveArithmeticResult::Unsupported
        );
    }

    #[test]
    fn byte_literal_bitwise_pairs_require_byte_literals_on_both_sides() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert_eq!(
                primitive_bitwise_byte_literal_pair_result(operator, PrimitiveLiteralKind::Byte),
                PrimitiveArithmeticResult::Type(PrimTy::Byte),
                "{operator:?} accepts a direct Byte/Byte pair"
            );
            for right in [
                PrimitiveLiteralKind::Integer,
                PrimitiveLiteralKind::FloatingPoint,
                PrimitiveLiteralKind::Bool,
                PrimitiveLiteralKind::Character,
            ] {
                assert_eq!(
                    primitive_bitwise_byte_literal_pair_result(operator, right),
                    PrimitiveArithmeticResult::Unsupported,
                    "{operator:?} keeps Byte/{right:?} literal pairs gated"
                );
            }
        }

        assert_eq!(
            primitive_bitwise_byte_literal_pair_result(BinOp::Add, PrimitiveLiteralKind::Byte),
            PrimitiveArithmeticResult::Unsupported
        );
    }

    #[test]
    fn integer_literal_bitwise_pairs_accept_only_integer_literals_for_bitwise_operators() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert!(
                primitive_bitwise_integer_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::Integer
                ),
                "{operator:?} accepts an integer/integer literal pair"
            );
            for right in [
                PrimitiveLiteralKind::FloatingPoint,
                PrimitiveLiteralKind::Bool,
                PrimitiveLiteralKind::Byte,
                PrimitiveLiteralKind::Character,
            ] {
                assert!(
                    !primitive_bitwise_integer_literal_pair_supported(operator, right),
                    "{operator:?} keeps integer/{right:?} literal pairs gated"
                );
            }
        }

        for operator in [BinOp::Add, BinOp::Shl, BinOp::Shr, BinOp::Eq] {
            assert!(
                !primitive_bitwise_integer_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::Integer
                ),
                "{operator:?} integer literal pairs remain unsupported"
            );
        }
    }

    #[test]
    fn bool_literal_bitwise_pairs_accept_only_bool_literals_for_bitwise_operators() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert!(
                primitive_bitwise_bool_literal_pair_supported(operator, PrimitiveLiteralKind::Bool),
                "{operator:?} accepts a Bool/Bool literal pair for diagnostics"
            );
            for right in [
                PrimitiveLiteralKind::Integer,
                PrimitiveLiteralKind::FloatingPoint,
                PrimitiveLiteralKind::Byte,
                PrimitiveLiteralKind::Character,
            ] {
                assert!(
                    !primitive_bitwise_bool_literal_pair_supported(operator, right),
                    "{operator:?} keeps Bool/{right:?} literal pairs gated"
                );
            }
        }

        for operator in [BinOp::Add, BinOp::Shl, BinOp::Shr, BinOp::Eq] {
            assert!(
                !primitive_bitwise_bool_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::Bool
                ),
                "{operator:?} Bool literal pairs remain unsupported"
            );
        }
    }

    #[test]
    fn char_literal_bitwise_pairs_accept_all_direct_scalar_literals_for_bitwise_operators() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert!(
                primitive_bitwise_char_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::Character
                ),
                "{operator:?} accepts a Char/Char literal pair for trait diagnostics"
            );
            assert!(
                primitive_bitwise_char_literal_pair_supported(operator, PrimitiveLiteralKind::Bool),
                "{operator:?} accepts a Char/Bool literal pair for trait diagnostics"
            );
            assert!(
                primitive_bitwise_char_literal_pair_supported(operator, PrimitiveLiteralKind::Byte),
                "{operator:?} accepts a Char/Byte literal pair for trait diagnostics"
            );
            assert!(
                primitive_bitwise_char_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::Integer
                ),
                "{operator:?} accepts a Char/integer literal pair for trait diagnostics"
            );
            assert!(
                primitive_bitwise_char_literal_pair_supported(
                    operator,
                    PrimitiveLiteralKind::FloatingPoint
                ),
                "{operator:?} accepts a Char/float literal pair for trait diagnostics"
            );
        }

        for operator in [BinOp::Add, BinOp::Shl, BinOp::Shr, BinOp::Eq] {
            for right in [
                PrimitiveLiteralKind::Character,
                PrimitiveLiteralKind::Bool,
                PrimitiveLiteralKind::Byte,
                PrimitiveLiteralKind::Integer,
                PrimitiveLiteralKind::FloatingPoint,
            ] {
                assert!(
                    !primitive_bitwise_char_literal_pair_supported(operator, right),
                    "{operator:?} Char/{right:?} literal pairs remain unsupported"
                );
            }
        }
    }

    #[test]
    fn char_bitwise_float_literal_rhs_defaults_to_f64_for_each_operator() {
        for operator in [BinOp::BitAnd, BinOp::BitOr, BinOp::BitXor] {
            assert_eq!(
                primitive_bitwise_char_float_literal_rhs_type(operator),
                Some(PrimTy::F64),
                "{operator:?} leaves a Char-right float literal unconstrained"
            );
        }
        assert_eq!(
            primitive_bitwise_char_float_literal_rhs_type(BinOp::Add),
            None
        );
    }

    #[test]
    fn char_path_trait_deferral_requires_eligible_literal_provenance() {
        assert!(can_defer_char_literal_path_trait(
            PrimitiveLiteralKind::Integer,
            false,
            true,
            false
        ));
        assert!(can_defer_char_literal_path_trait(
            PrimitiveLiteralKind::FloatingPoint,
            false,
            true,
            false
        ));
        for kind in [
            PrimitiveLiteralKind::Bool,
            PrimitiveLiteralKind::Byte,
            PrimitiveLiteralKind::Character,
        ] {
            assert!(
                !can_defer_char_literal_path_trait(kind, false, true, false),
                "{kind:?} literal locals remain gated"
            );
        }
        assert!(
            !can_defer_char_literal_path_trait(PrimitiveLiteralKind::Integer, false, false, false),
            "a path without direct-literal provenance remains ineligible"
        );
        assert!(
            can_defer_char_literal_path_trait(PrimitiveLiteralKind::Integer, true, false, true),
            "a direct integer-literal bitwise pair may defer with its additional source site"
        );
        assert!(
            !can_defer_char_literal_path_trait(PrimitiveLiteralKind::Integer, true, true, false),
            "other computed integer literal variables with another source site remain gated"
        );
        assert!(
            !can_defer_char_literal_path_trait(
                PrimitiveLiteralKind::FloatingPoint,
                true,
                true,
                true
            ),
            "float literal variables with another source site remain gated"
        );
    }

    #[test]
    fn float_literal_alias_tracking_propagates_only_unannotated_plain_paths() {
        assert!(
            can_track_unannotated_float_literal_alias(true, true, false),
            "a direct literal or already tracked alias can source provenance"
        );
        assert!(
            !can_track_unannotated_float_literal_alias(true, true, true),
            "an annotated concrete copy follows the existing concrete-path route"
        );
        assert!(
            !can_track_unannotated_float_literal_alias(false, true, false),
            "a source without literal provenance cannot start an alias chain"
        );
        assert!(
            !can_track_unannotated_float_literal_alias(true, false, false),
            "computed initializers do not propagate literal provenance"
        );
    }

    #[test]
    fn integer_literal_alias_tracking_propagates_only_unannotated_plain_paths() {
        assert!(
            can_track_unannotated_integer_literal_alias(true, true, false),
            "a direct integer literal or tracked alias can source provenance"
        );
        assert!(
            !can_track_unannotated_integer_literal_alias(true, true, true),
            "an annotated concrete copy is not tracked as an inferred alias"
        );
        assert!(
            !can_track_unannotated_integer_literal_alias(false, true, false),
            "computed literals and other sources without provenance cannot start a chain"
        );
        assert!(
            !can_track_unannotated_integer_literal_alias(true, false, false),
            "computed initializers do not propagate integer-literal provenance"
        );
    }

    #[test]
    fn resolved_computed_literal_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_resolved_computed_literal_alias(true, true, false),
            "an eligible resolved computation can source unannotated path copies"
        );
        assert!(
            !can_track_resolved_computed_literal_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_resolved_computed_literal_alias(false, true, false),
            "other resolved literal variables do not gain computed provenance"
        );
        assert!(
            !can_track_resolved_computed_literal_alias(true, false, false),
            "computed expressions do not propagate path-alias provenance"
        );
    }

    #[test]
    fn integer_bitwise_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_bitwise_pair_alias(true, true, false),
            "a direct integer-literal bitwise pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_bitwise_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_bitwise_pair_alias(false, true, false),
            "other computed expressions do not gain bitwise-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_bitwise_pair_alias(true, false, false),
            "computed initializers do not propagate bitwise-pair provenance"
        );
    }

    #[test]
    fn integer_addition_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_addition_pair_alias(true, true, false),
            "a direct integer-literal addition pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_addition_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_addition_pair_alias(false, true, false),
            "other computed expressions do not gain addition-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_addition_pair_alias(true, false, false),
            "computed initializers do not propagate addition-pair provenance"
        );
    }

    #[test]
    fn integer_wrapping_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_wrapping_pair_alias(true, true, false),
            "a direct integer-literal wrapping pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_wrapping_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_wrapping_pair_alias(false, true, false),
            "other computed expressions do not gain wrapping-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_wrapping_pair_alias(true, false, false),
            "computed initializers do not propagate wrapping-pair provenance"
        );
    }

    #[test]
    fn integer_subtraction_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_subtraction_pair_alias(true, true, false),
            "a direct integer-literal subtraction pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_subtraction_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_subtraction_pair_alias(false, true, false),
            "other computed expressions do not gain subtraction-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_subtraction_pair_alias(true, false, false),
            "computed initializers do not propagate subtraction-pair provenance"
        );
    }

    #[test]
    fn integer_multiplication_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_multiplication_pair_alias(true, true, false),
            "a direct integer-literal multiplication pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_multiplication_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_multiplication_pair_alias(false, true, false),
            "other computed expressions do not gain multiplication-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_multiplication_pair_alias(true, false, false),
            "computed initializers do not propagate multiplication-pair provenance"
        );
    }

    #[test]
    fn integer_division_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_division_pair_alias(true, true, false),
            "a direct integer-literal division pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_division_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_division_pair_alias(false, true, false),
            "other computed expressions do not gain division-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_division_pair_alias(true, false, false),
            "computed initializers do not propagate division-pair provenance"
        );
    }

    #[test]
    fn integer_remainder_pair_alias_tracking_requires_provenance_and_plain_paths() {
        assert!(
            can_track_unannotated_integer_remainder_pair_alias(true, true, false),
            "a direct integer-literal remainder pair can source unannotated copies"
        );
        assert!(
            !can_track_unannotated_integer_remainder_pair_alias(true, true, true),
            "annotated copies use the existing concrete-path behavior"
        );
        assert!(
            !can_track_unannotated_integer_remainder_pair_alias(false, true, false),
            "other computed expressions do not gain remainder-pair provenance"
        );
        assert!(
            !can_track_unannotated_integer_remainder_pair_alias(true, false, false),
            "computed initializers do not propagate remainder-pair provenance"
        );
    }
}

fn primitive_literal_kind(literal: Lit<'_>) -> Option<PrimitiveLiteralKind> {
    match literal {
        Lit::Int { .. } => Some(PrimitiveLiteralKind::Integer),
        Lit::Float { .. } => Some(PrimitiveLiteralKind::FloatingPoint),
        Lit::Bool(_) => Some(PrimitiveLiteralKind::Bool),
        Lit::Byte(_) => Some(PrimitiveLiteralKind::Byte),
        Lit::Char(_) => Some(PrimitiveLiteralKind::Character),
        Lit::Str(_) | Lit::ByteStr(_) => None,
    }
}

fn is_supported_direct_call_arithmetic_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    let is_numeric_literal_or_path = |expression: &Expr<'_>| match expression.kind {
        ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) | ExprKind::Path(_) => true,
        _ => false,
    };
    if !is_numeric_literal_or_path(left) || !is_numeric_literal_or_path(right) {
        return false;
    }

    // This call-argument slice can constrain same-kind numeric literal pairs;
    // mixed integer/float literal pairs remain outside its bounded inference.
    if matches!(left.kind, ExprKind::Lit(_)) && matches!(right.kind, ExprKind::Lit(_)) {
        return matches!(
            (left.kind, right.kind),
            (
                ExprKind::Lit(Lit::Int { .. }),
                ExprKind::Lit(Lit::Int { .. })
            ) | (
                ExprKind::Lit(Lit::Float { .. }),
                ExprKind::Lit(Lit::Float { .. })
            )
        );
    }
    true
}

fn is_supported_direct_call_remainder_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    matches!(
        (left.kind, right.kind),
        (
            ExprKind::Path(_),
            ExprKind::Lit(Lit::Int { .. }) | ExprKind::Path(_)
        ) | (ExprKind::Lit(Lit::Int { .. }), ExprKind::Path(_))
            | (
                ExprKind::Lit(Lit::Int { .. }),
                ExprKind::Lit(Lit::Int { .. })
            )
    )
}

fn is_supported_direct_call_comparison_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    let is_primitive_literal_or_path = |expression: &Expr<'_>| match expression.kind {
        ExprKind::Lit(literal) => primitive_literal_kind(literal).is_some(),
        ExprKind::Path(_) => true,
        _ => false,
    };
    if !is_primitive_literal_or_path(left) || !is_primitive_literal_or_path(right) {
        return false;
    }

    // The bounded comparison form needs a concrete local type to constrain a
    // possible literal. Literal/literal comparisons have no such witness.
    matches!(left.kind, ExprKind::Path(_)) || matches!(right.kind, ExprKind::Path(_))
}

fn is_supported_direct_call_logical_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    let is_path_or_bool_literal = |operand: &Expr<'_>| {
        matches!(
            operand.kind,
            ExprKind::Path(_) | ExprKind::Lit(Lit::Bool(_))
        )
    };
    is_path_or_bool_literal(left) && is_path_or_bool_literal(right)
}

fn is_supported_direct_call_shift_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    match (left.kind, right.kind) {
        (
            ExprKind::Path(_),
            ExprKind::Path(_)
            | ExprKind::Lit(Lit::Byte(_))
            | ExprKind::Lit(Lit::Int { .. } | Lit::Bool(_) | Lit::Char(_) | Lit::Float { .. }),
        ) => true,
        (
            ExprKind::Lit(Lit::Byte(_)),
            ExprKind::Path(_)
            | ExprKind::Lit(Lit::Byte(_))
            | ExprKind::Lit(Lit::Int { .. } | Lit::Bool(_) | Lit::Char(_) | Lit::Float { .. }),
        ) => true,
        (
            ExprKind::Lit(Lit::Int { .. }),
            ExprKind::Path(_)
            | ExprKind::Lit(
                Lit::Int { .. } | Lit::Byte(_) | Lit::Bool(_) | Lit::Char(_) | Lit::Float { .. },
            ),
        ) => true,
        _ => false,
    }
}

fn is_supported_direct_call_bitwise_operands(left: &Expr<'_>, right: &Expr<'_>) -> bool {
    (matches!(left.kind, ExprKind::Path(_))
        && matches!(
            right.kind,
            ExprKind::Path(_) | ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_))
        ))
        || (matches!(left.kind, ExprKind::Lit(Lit::Int { .. }))
            && matches!(right.kind, ExprKind::Path(_)))
        || (matches!(left.kind, ExprKind::Lit(Lit::Byte(_)))
            && matches!(right.kind, ExprKind::Path(_)))
        || matches!(
            (left.kind, right.kind),
            (
                ExprKind::Lit(Lit::Int { .. }),
                ExprKind::Lit(Lit::Int { .. })
            )
        )
        || matches!(
            (left.kind, right.kind),
            (ExprKind::Lit(Lit::Byte(_)), ExprKind::Lit(Lit::Byte(_)))
        )
        || matches!(
            (left.kind, right.kind),
            (ExprKind::Lit(Lit::Byte(_)), ExprKind::Lit(Lit::Int { .. }))
        )
        || matches!(
            (left.kind, right.kind),
            (
                ExprKind::Lit(Lit::Byte(_)),
                ExprKind::Lit(Lit::Bool(_) | Lit::Char(_) | Lit::Float { .. })
            )
        )
}

fn is_supported_direct_call_not_operand(operand: &Expr<'_>) -> bool {
    matches!(
        operand.kind,
        ExprKind::Path(_) | ExprKind::Lit(Lit::Bool(_))
    )
}

fn is_supported_direct_call_bitwise_not_operand(operand: &Expr<'_>) -> bool {
    matches!(
        operand.kind,
        ExprKind::Path(_) | ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_))
    )
}

fn is_supported_direct_call_neg_operand(operand: &Expr<'_>) -> bool {
    matches!(operand.kind, ExprKind::Path(_))
}

fn primitive_shift_left_literal_supported(kind: PrimitiveLiteralKind) -> bool {
    matches!(
        kind,
        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte
    )
}
