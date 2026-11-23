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
use sai_hir::expressions::{
    Arm, BinOp, Block, Expr, ExprKind, Label, LoopSource, MatchSource, Stmt, StmtKind, UnOp,
};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, PatKind, PatRangeEnd};
use sai_hir::types::PathRoot;
use sai_hir::{BindingId, HirId, OwnerId, Safety, Symbol};
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
    /// `Str`-typed parameter bindings admitted by the owner-aware driver —
    /// `Str` is not a `PrimTy`, so these bind `LocalType::Str`.
    pub str_bindings: Option<&'a HashSet<BindingId>>,
    /// Local annotation nodes (`let s: Str = ..`) whose type spells `Str`.
    /// The driver resolves them textually the way `primitive_types` maps
    /// primitive annotation nodes.
    pub str_annotations: Option<&'a HashSet<HirId>>,
    pub primitive_types: &'a HashMap<HirId, PrimTy>,
    /// Direct `self.field` projection types resolved by the owner-aware
    /// driver. Other field expressions remain outside this body slice.
    pub self_field_projections: Option<&'a HashMap<HirId, PrimTy>>,
    /// Direct `self.field` projections whose names were diagnosed as
    /// unknown by the owner-aware driver. These act as error types so later
    /// local uses do not produce cascading primitive-type errors.
    pub unknown_self_fields: Option<&'a HashSet<HirId>>,
    /// Direct `self.field` projections of `Str` family (`type` decl field
    /// typed `Str` — outside `PrimTy`). A `let` binding initialized from
    /// one becomes a `Str` local.
    pub str_self_field_projections: Option<&'a HashMap<HirId, &'static str>>,
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
    /// `expected <type>, found <type>` where either side may be `Str` —
    /// `Str` is not a `PrimTy`, so `TypeMismatch` cannot name it.
    ScalarTypeMismatch {
        expected: ScalarType,
        found: ScalarType,
    },
    /// `expected <type>, found <literal>` where the expected side may be
    /// `Str`.
    ScalarLiteralTypeMismatch {
        expected: ScalarType,
        found: ScalarLiteralKind,
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
    BreakOutsideLoop,
    ContinueOutsideLoop,
    InvalidBreakValue,
    UndeclaredLabel,
    LabelShadowed,
    MissingReturnValue {
        expected: PrimTy,
    },
    EmptyRangePattern,
    NonExhaustiveMatch,
    UnreachablePattern,
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

/// A scalar type the bounded checker can name: a primitive or `Str`
/// (`Str` is `Adt(str)` — a nominal type outside `PrimTy`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarType {
    Primitive(PrimTy),
    Str,
}

/// The literal family a `Str`-position mismatch names: the primitive
/// literal kinds plus the `Str` (`"a"`) and `Slice<Byte>` (`b"a"`)
/// literals `primitive_literal_kind` excludes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarLiteralKind {
    Primitive(PrimitiveLiteralKind),
    Str,
    ByteStr,
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
    /// `Str` — a nominal type outside `PrimTy` (spec 6.8.7 admits it as a
    /// comparison operand; every other operator position stays gated).
    Str,
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
    /// `x + y`-style operands merge two variables: a child resolves to
    /// whatever type its parent resolves to.
    merged_with: Option<usize>,
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
    let mut known_bindings: HashMap<BindingId, LocalType> = input
        .initial_bindings
        .iter()
        .map(|(binding, ty)| (*binding, LocalType::Concrete(*ty)))
        .collect();
    if let Some(str_bindings) = input.str_bindings {
        for binding in str_bindings {
            known_bindings.insert(*binding, LocalType::Str);
        }
    }
    let binding_type_candidates = known_bindings.clone();
    let trusted_bindings = input
        .initial_bindings
        .keys()
        .copied()
        .chain(input.str_bindings.into_iter().flatten().copied())
        .collect();
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
        loop_stack: Vec::new(),
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
    loop_stack: Vec<LoopFrame>,
    errors: Vec<PrimitiveBodyError>,
}

/// One entry of the `Γ.loops` stack (§6.15): the enclosing loop's label and
/// whether it came from `loop` or a `while`/`for` desugaring.
struct LoopFrame {
    label: Option<Symbol>,
    source: LoopSource,
    break_state: LoopBreakState,
}

/// The loop's `β` (§6.16.1): each `break v` coerces `v ⇐ β`, and a bare
/// `break` unifies `β` with `()`. The primitive kernel has no `()` type, so
/// mixing bare and valued breaks reports `INVALID_BREAK_VALUE` instead of a
/// type mismatch. A literal `break` value keeps `β` as a literal variable
/// until a concrete break value resolves it or the loop ends and it
/// defaults like any other unresolved literal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopBreakState {
    Unfixed,
    Bare,
    Concrete(PrimTy),
    LiteralVariable(usize),
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
        self.check_block_expected(block, self.input.return_type)
    }

    fn check_block_expected(&mut self, block: &Block<'hir>, expected: PrimTy) -> Result<(), ()> {
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
        let is_supported_unary_tail = match tail.kind {
            ExprKind::Unary(operator, operand)
                if matches!(operator, UnOp::Neg | UnOp::BitNot | UnOp::Not) =>
            {
                self.unary_tail_operand_is_admissible(operator, operand)
            }
            _ => false,
        };
        let is_supported_binary_tail = matches!(tail.kind, ExprKind::Binary(..));
        let is_supported_literal_tail = matches!(tail.kind, ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some());
        let is_supported_if_tail = matches!(tail.kind, ExprKind::If { els: Some(_), .. });
        let is_supported_match_tail = matches!(
            tail.kind,
            ExprKind::Match {
                source: MatchSource::Normal,
                ..
            }
        );
        let is_supported_return_tail = matches!(tail.kind, ExprKind::Return(..));
        let is_supported_loop_tail = matches!(
            tail.kind,
            ExprKind::Loop {
                source: LoopSource::Loop,
                ..
            }
        );
        // A nested `{ ...; e }` block tail checks its own statements and
        // tail against the same expected type — bindings stay scoped to
        // the inner block. Statement admission is enforced by check_stmt
        // itself, so the flag only excludes non-safe inner blocks.
        let is_supported_block_tail = matches!(
            tail.kind,
            ExprKind::Block(inner) if inner.safety == Safety::Safe
        );
        let is_supported_path_tail = matches!(tail.kind, ExprKind::Path(_));
        if block.stmts.is_empty()
            && !is_supported_field_tail
            && !is_supported_call_tail
            && !is_supported_unary_tail
            && !is_supported_binary_tail
            && !is_supported_literal_tail
            && !is_supported_if_tail
            && !is_supported_match_tail
            && !is_supported_return_tail
            && !is_supported_loop_tail
            && !is_supported_block_tail
            && !is_supported_path_tail
        {
            return Err(());
        }
        for stmt in block.stmts {
            if self.check_stmt(stmt).is_err() {
                return Err(());
            }
        }

        if self.check_expr_expected(tail, expected).is_err() {
            return Err(());
        }
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
                // The message is a `Str` literal or a `Str`-typed local per
                // N-C-20 — a `Str` parameter or local path qualifies.
                let str_message = matches!(msg.kind, ExprKind::Lit(Lit::Str(_)))
                    || matches!(self.check_operand_local_type(msg), Ok(LocalType::Str));
                if !str_message {
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

        let (annotation, str_annotation) = match ty {
            Some(ty) => match self.input.primitive_types.get(&ty.hir_id).copied() {
                Some(primitive) => {
                    if !supports_primitive_literal(primitive) {
                        return Err(());
                    }
                    (Some(primitive), false)
                }
                None if self
                    .input
                    .str_annotations
                    .is_some_and(|annotations| annotations.contains(&ty.hir_id)) =>
                {
                    (None, true)
                }
                None => return Err(()),
            },
            None => (None, false),
        };

        let errors_before_initializer = self.errors.len();
        let InitializerCheck {
            local_type,
            deferred_neg_result: initializer_is_deferred_neg_result,
            deferred_bitnot_result: initializer_is_deferred_bitnot_result,
        } = if str_annotation {
            InitializerCheck {
                local_type: self.check_str_annotated_initializer(init)?,
                deferred_neg_result: false,
                deferred_bitnot_result: false,
            }
        } else {
            self.check_initializer_value(init, annotation)?
        };

        if initializer_is_deferred_neg_result {
            self.deferred_neg_result_bindings.insert(binding);
        }
        if initializer_is_deferred_bitnot_result {
            self.deferred_bitnot_result_bindings.insert(binding);
        }

        let provenance = initializer_provenance(init);

        if annotation.is_none()
            && matches!(
                provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            provenance.kind,
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
            if let ExprKind::Lit(literal) = provenance.kind {
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
        if let Some(local_type) = annotation
            .map(LocalType::Concrete)
            .or(str_annotation.then_some(LocalType::Str))
        {
            self.binding_type_candidates.insert(binding, local_type);
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
        if let Some(source_binding) = self.plain_local_path_binding(provenance) {
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
                matches!(provenance.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.resolved_computed_literal_paths.insert(binding);
            }

            let source_has_integer_bitwise_pair_provenance = self
                .unannotated_integer_bitwise_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_bitwise_pair_alias(
                source_has_integer_bitwise_pair_provenance,
                matches!(provenance.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_bitwise_pair_paths.insert(binding);
            }

            let source_has_integer_addition_pair_provenance = self
                .unannotated_integer_addition_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_addition_pair_alias(
                source_has_integer_addition_pair_provenance,
                matches!(provenance.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_addition_pair_paths.insert(binding);
            }

            let source_has_integer_subtraction_pair_provenance = self
                .unannotated_integer_subtraction_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_subtraction_pair_alias(
                source_has_integer_subtraction_pair_provenance,
                matches!(provenance.kind, ExprKind::Path(_)),
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
                matches!(provenance.kind, ExprKind::Path(_)),
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
                matches!(provenance.kind, ExprKind::Path(_)),
                annotation.is_some(),
            ) {
                self.unannotated_integer_division_pair_paths.insert(binding);
            }

            let source_has_integer_remainder_pair_provenance = self
                .unannotated_integer_remainder_pair_paths
                .contains(&source_binding);
            if can_track_unannotated_integer_remainder_pair_alias(
                source_has_integer_remainder_pair_provenance,
                matches!(provenance.kind, ExprKind::Path(_)),
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
                matches!(provenance.kind, ExprKind::Path(_)),
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
                matches!(provenance.kind, ExprKind::Path(_)),
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
                matches!(provenance.kind, ExprKind::Path(_)),
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
                && matches!(
                    kind,
                    PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                )
            {
                if let Some(LocalType::LiteralVariable(id)) =
                    self.check_operand_local_type(right).ok()
                {
                    let Some(variable) = self.literal_variables.get(id) else {
                        return Err(());
                    };
                    if variable.resolved.is_none() {
                        match (kind, variable.kind) {
                            (PrimitiveLiteralKind::Integer, PrimitiveLiteralKind::Integer) => {
                                self.add_integer_literal_constraint(id, literal, left.hir_id)?;
                            }
                            (
                                PrimitiveLiteralKind::FloatingPoint,
                                PrimitiveLiteralKind::FloatingPoint,
                            ) => {
                                self.add_float_literal_constraint(id, literal, left.hir_id)?;
                            }
                            _ => return Err(()),
                        }
                        return Ok(LocalType::LiteralVariable(id));
                    }
                }
            }
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
                .check_operand_local_type(right)
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

        let left_local_type = self.check_operand_local_type(left).ok().ok_or(())?;
        let unresolved_literal_variable = matches!(left_local_type, LocalType::LiteralVariable(id)
            if self
                .literal_variables
                .get(id)
                .is_some_and(|variable| variable.resolved.is_none()));
        if unresolved_literal_variable {
            let LocalType::LiteralVariable(id) = left_local_type else {
                return Err(());
            };
            if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
                return Err(());
            }
            if !matches!(right.kind, ExprKind::Lit(_)) {
                // `x op path` with a concretely typed right operand resolves
                // the variable to the operand's type when the family admits
                // it and the operator exists for that type pair.
                if let Some(right_type) = self
                    .check_operand_local_type(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                {
                    match primitive_arithmetic_result(operator, right_type, right_type) {
                        PrimitiveArithmeticResult::Type(_) => {
                            self.resolve_literal_variable(id, right_type)?;
                            return Ok(left_local_type);
                        }
                        _ => return Err(()),
                    }
                }
                // `x op y` with both sides unresolved literal variables of
                // one kind merges them so they resolve to a single type.
                // The left variable must be numeric: the merge defers the
                // operator check, and `+ - * /` only exist on integer and
                // floating-point primitives.
                if let Some(LocalType::LiteralVariable(other)) =
                    self.check_operand_local_type(right).ok()
                    && self.literal_variables.get(id).is_some_and(|variable| {
                        matches!(
                            variable.kind,
                            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                        )
                    })
                {
                    let Some(other_variable) = self.literal_variables.get(other) else {
                        return Err(());
                    };
                    if other_variable.resolved.is_none() {
                        self.merge_literal_variables(id, other)?;
                        return Ok(left_local_type);
                    }
                }
                return Err(());
            }
            let ExprKind::Lit(literal) = right.kind else {
                return Err(());
            };
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            match (variable.kind, primitive_literal_kind(literal)) {
                (PrimitiveLiteralKind::Integer, Some(PrimitiveLiteralKind::Integer)) => {
                    self.add_integer_literal_constraint(id, literal, right.hir_id)?;
                }
                (
                    PrimitiveLiteralKind::FloatingPoint,
                    Some(PrimitiveLiteralKind::FloatingPoint),
                ) => {
                    self.add_float_literal_constraint(id, literal, right.hir_id)?;
                }
                _ => return Err(()),
            }
            return Ok(left_local_type);
        }
        let left_type = self.concrete_local_type(left_local_type).ok_or(())?;

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

        // `p + x` with a concretely typed left operand resolves an
        // unresolved literal variable on the right to that type — the
        // symmetric rule for the left-side literal-variable branch.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(right).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                match primitive_arithmetic_result(operator, left_type, left_type) {
                    PrimitiveArithmeticResult::Type(_) => {
                        self.resolve_literal_variable(id, left_type)?;
                        return Ok(LocalType::Concrete(left_type));
                    }
                    _ => return Err(()),
                }
            }
        }

        let right_type = self
            .check_operand_local_type(right)
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
            // `1 % x` joins the literal as a constraint site on the
            // unresolved integer literal variable, like `x % 1`.
            if kind == PrimitiveLiteralKind::Integer
                && let Some(LocalType::LiteralVariable(other)) =
                    self.check_operand_local_type(right).ok()
                && self.literal_variables.get(other).is_some_and(|v| {
                    v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                })
            {
                self.add_integer_literal_constraint(other, literal, left.hir_id)?;
                return Ok(LocalType::LiteralVariable(other));
            }
            let right_type = self
                .check_operand_local_type(right)
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

        // `x % path` with a concretely typed operand resolves an unresolved
        // integer literal variable to the operand's type — the `x = x % path`
        // desugar does the same. Float variables stay gated because the
        // remainder has no float implementation.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(left).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            let variable_kind = variable.kind;
            let variable_unresolved = variable.resolved.is_none();
            if variable_unresolved {
                if let ExprKind::Lit(literal) = right.kind {
                    if variable_kind != PrimitiveLiteralKind::Integer {
                        return Err(());
                    }
                    self.add_integer_literal_constraint(id, literal, right.hir_id)?;
                    return Ok(LocalType::LiteralVariable(id));
                }
                if let Some(right_type) = self
                    .check_operand_local_type(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                {
                    if variable_kind != PrimitiveLiteralKind::Integer
                        || !is_integer_primitive(right_type)
                    {
                        return Err(());
                    }
                    self.resolve_literal_variable(id, right_type)?;
                    return Ok(LocalType::LiteralVariable(id));
                }
                // `x % y` merges two unresolved integer literal variables.
                if variable_kind == PrimitiveLiteralKind::Integer
                    && let Some(LocalType::LiteralVariable(other)) =
                        self.check_operand_local_type(right).ok()
                {
                    if self
                        .literal_variables
                        .get(other)
                        .is_some_and(|v| v.resolved.is_none())
                    {
                        self.merge_literal_variables(id, other)?;
                        return Ok(LocalType::LiteralVariable(id));
                    }
                }
                return Err(());
            }
        }

        let left_type = self
            .check_operand_local_type(left)
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
            // `p % x` resolves an unresolved integer literal variable on
            // the right to the concrete left operand's type.
            if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(right).ok()
            {
                let Some(variable) = self.literal_variables.get(id) else {
                    return Err(());
                };
                if variable.resolved.is_none() {
                    if variable.kind != PrimitiveLiteralKind::Integer
                        || !is_integer_primitive(left_type)
                    {
                        return Err(());
                    }
                    self.resolve_literal_variable(id, left_type)?;
                }
            }
            let right_type = self
                .check_operand_local_type(right)
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
            // `1 +% x` joins the literal as a constraint site on the
            // unresolved integer literal variable, like `x +% 1`.
            if let Some(LocalType::LiteralVariable(other)) =
                self.check_operand_local_type(right).ok()
                && self.literal_variables.get(other).is_some_and(|v| {
                    v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                })
            {
                self.add_integer_literal_constraint(other, literal, left.hir_id)?;
                return Ok(LocalType::LiteralVariable(other));
            }
            let right_type = self
                .check_operand_local_type(right)
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

        // `x wrapping+ path` resolves an unresolved integer literal
        // variable to the operand's type, as the wrapping desugar does.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(left).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                if variable.kind != PrimitiveLiteralKind::Integer {
                    return Err(());
                }
                if let ExprKind::Lit(literal) = right.kind {
                    self.add_integer_literal_constraint(id, literal, right.hir_id)?;
                    return Ok(LocalType::LiteralVariable(id));
                }
                if let Some(right_type) = self
                    .check_operand_local_type(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                {
                    return match primitive_wrapping_literal_left_result(
                        operator,
                        PrimitiveLiteralKind::Integer,
                        right_type,
                    ) {
                        PrimitiveArithmeticResult::Type(expected) => {
                            self.resolve_literal_variable(id, expected)?;
                            Ok(LocalType::LiteralVariable(id))
                        }
                        _ => Err(()),
                    };
                }
                // `x +% y` merges two unresolved integer literal variables.
                if let Some(LocalType::LiteralVariable(other)) =
                    self.check_operand_local_type(right).ok()
                {
                    if self
                        .literal_variables
                        .get(other)
                        .is_some_and(|v| v.resolved.is_none())
                    {
                        self.merge_literal_variables(id, other)?;
                        return Ok(LocalType::LiteralVariable(id));
                    }
                }
                return Err(());
            }
        }

        let left_type = self
            .check_operand_local_type(left)
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
        // `p +% x` resolves an unresolved integer literal variable on the
        // right to the concrete left operand's type.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(right).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                if variable.kind != PrimitiveLiteralKind::Integer
                    || !is_integer_primitive(left_type)
                {
                    return Err(());
                }
                self.resolve_literal_variable(id, left_type)?;
            }
        }
        let right_type = self
            .check_operand_local_type(right)
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
                    .check_operand_local_type(right)
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
                        let right_local_type = self.check_operand_local_type(right)?;
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

            // `1 & x` joins the literal as a constraint site on the
            // unresolved integer literal variable, like `x & 1`.
            if let Some(LocalType::LiteralVariable(other)) =
                self.check_operand_local_type(right).ok()
                && self.literal_variables.get(other).is_some_and(|v| {
                    v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                })
            {
                self.add_integer_literal_constraint(other, literal, left.hir_id)?;
                return Ok(LocalType::LiteralVariable(other));
            }

            let right_type = self
                .check_operand_local_type(right)
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

        // `x & path` resolves an unresolved integer literal variable to the
        // operand's type; a literal right side joins it as a constraint
        // site. Bool and float variables stay gated — they have no bitwise
        // operators to fall back on.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(left).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                if variable.kind != PrimitiveLiteralKind::Integer {
                    return Err(());
                }
                if let ExprKind::Lit(literal) = right.kind {
                    self.add_integer_literal_constraint(id, literal, right.hir_id)?;
                    return Ok(LocalType::LiteralVariable(id));
                }
                if let Some(right_type) = self
                    .check_operand_local_type(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                {
                    return match primitive_bitwise_literal_left_result(operator, right_type) {
                        PrimitiveArithmeticResult::Type(expected) => {
                            self.resolve_literal_variable(id, expected)?;
                            Ok(LocalType::LiteralVariable(id))
                        }
                        _ => Err(()),
                    };
                }
                // `x & y` merges two unresolved integer literal variables.
                if let Some(LocalType::LiteralVariable(other)) =
                    self.check_operand_local_type(right).ok()
                {
                    if self
                        .literal_variables
                        .get(other)
                        .is_some_and(|v| v.resolved.is_none())
                    {
                        self.merge_literal_variables(id, other)?;
                        return Ok(LocalType::LiteralVariable(id));
                    }
                }
                return Err(());
            }
        }

        let left_type = self
            .check_operand_local_type(left)
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

        // `p & x` resolves an unresolved integer literal variable on the
        // right to the concrete left operand's type.
        if let Some(LocalType::LiteralVariable(id)) = self.check_operand_local_type(right).ok() {
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                if variable.kind != PrimitiveLiteralKind::Integer
                    || (!is_integer_primitive(left_type) && left_type != PrimTy::Byte)
                {
                    return Err(());
                }
                self.resolve_literal_variable(id, left_type)?;
            }
        }

        let right_type = self
            .check_operand_local_type(right)
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
            // `x << count` keeps an unresolved integer literal variable as
            // the result — the variable resolves through an annotation or
            // the function tail, like `x % 1`.
            match self.check_operand_local_type(left) {
                Ok(local_type @ LocalType::LiteralVariable(id))
                    if self.literal_variables.get(id).is_some_and(|v| {
                        v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                    }) =>
                {
                    local_type
                }
                _ => {
                    let local_type = self
                        .check_operand_local_type(left)
                        .ok()
                        .and_then(|local_type| self.concrete_local_type(local_type))
                        .filter(|primitive| supports_primitive_literal(*primitive))
                        .ok_or(())?;
                    LocalType::Concrete(local_type)
                }
            }
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
            // `x << c` resolves an unresolved integer literal-variable
            // count to U32 — the shift count's required type — like an
            // annotated `U32` local.
            let right_type = match self.check_operand_local_type(right) {
                Ok(LocalType::LiteralVariable(id))
                    if self.literal_variables.get(id).is_some_and(|v| {
                        v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                    }) =>
                {
                    self.resolve_literal_variable(id, PrimTy::U32)?;
                    PrimTy::U32
                }
                _ => self
                    .check_operand_local_type(right)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                    .filter(|primitive| supports_primitive_literal(*primitive))
                    .ok_or(())?,
            };
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
        let left_literal = match left.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal).map(|kind| (literal, kind)),
            _ => None,
        };
        let right_literal = match right.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal).map(|kind| (literal, kind)),
            _ => None,
        };

        // An unresolved literal variable compared to a same-family literal
        // joins the literal as a constraint site on the variable, like the
        // arithmetic operand slice; the comparison itself yields Bool. Only
        // comparison operators take this path — `check_logical_operand`
        // routes every binary operand here, so an arithmetic `x + 2` must
        // keep its out-of-slice behavior rather than claim a Bool result.
        if !matches!(
            operator,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        ) {
            return Err(());
        }

        // `Str` operands compare like primitives: all six operators are
        // built-in for `Str` (spec 6.8.7 — `==` byte equality, `<`
        // lexicographic byte order). A `Str` literal or `Str`-typed path
        // qualifies; the right operand is checked against the left's type,
        // so a mismatch is always reported on the right operand except
        // when the left side itself is the non-`Str` one.
        let left_str = matches!(left.kind, ExprKind::Lit(Lit::Str(_)))
            || matches!(self.check_operand_local_type(left), Ok(LocalType::Str));
        let right_str = matches!(right.kind, ExprKind::Lit(Lit::Str(_)))
            || matches!(self.check_operand_local_type(right), Ok(LocalType::Str));
        if left_str || right_str {
            if left_str != right_str {
                // The right operand is checked against the left's type —
                // the mismatch always lands on the right.
                let expected = if left_str {
                    ScalarType::Str
                } else {
                    self.scalar_operand_scalar_type(left)?
                };
                match right.kind {
                    ExprKind::Lit(literal) => {
                        // A literal names its own family — `1 == "a"`
                        // reports `found a string literal`, `s == b"a"`
                        // `found a byte string literal` (`Slice<Byte>`).
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarLiteralTypeMismatch {
                                expected,
                                found: scalar_literal_kind(literal),
                            },
                        });
                    }
                    _ => {
                        let found = match self.check_operand_local_type(right)? {
                            LocalType::Concrete(found) => ScalarType::Primitive(found),
                            LocalType::Str => ScalarType::Str,
                            LocalType::LiteralVariable(id)
                            | LocalType::ErrorLiteralVariable(id) => {
                                let Some(variable) = self.literal_variables.get(id) else {
                                    return Err(());
                                };
                                ScalarType::Primitive(variable.resolved.unwrap_or_else(|| {
                                    literal_variable_default_type(variable.kind)
                                }))
                            }
                            // Already reported on its own — recover.
                            LocalType::Error(_) => return Ok(LocalType::Concrete(PrimTy::Bool)),
                        };
                        self.errors.push(PrimitiveBodyError {
                            node: right.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarTypeMismatch { expected, found },
                        });
                    }
                }
            }
            for operand in [left, right] {
                if matches!(operand.kind, ExprKind::Lit(Lit::Str(_)))
                    || matches!(self.check_operand_local_type(operand), Ok(LocalType::Str))
                {
                    self.record_expression_type(operand.hir_id, LocalType::Str);
                }
            }
            return Ok(LocalType::Concrete(PrimTy::Bool));
        }

        for (local_type, literal_expression, literal_entry) in [
            (
                self.check_operand_local_type(left).ok(),
                right,
                right_literal,
            ),
            (
                self.check_operand_local_type(right).ok(),
                left,
                left_literal,
            ),
        ] {
            let Some(LocalType::LiteralVariable(id)) = local_type else {
                continue;
            };
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            let variable_kind = variable.kind;
            let variable_unresolved = variable.resolved.is_none();
            if !variable_unresolved {
                continue;
            }
            let Some((literal, kind)) = literal_entry else {
                // `x < path` with a concretely typed operand resolves the
                // variable to the operand's type when the comparison is
                // defined for the pair — the same rule the arithmetic
                // operand slice uses.
                if let Some(path_type) = self
                    .check_operand_local_type(literal_expression)
                    .ok()
                    .and_then(|local_type| self.concrete_local_type(local_type))
                {
                    match primitive_comparison_result(operator, path_type, path_type) {
                        PrimitiveComparisonResult::Bool => {
                            self.resolve_literal_variable(id, path_type)?;
                            return Ok(LocalType::Concrete(PrimTy::Bool));
                        }
                        _ => return Err(()),
                    }
                }
                // `x < y` merges two unresolved literal variables of one
                // kind; the comparison yields Bool either way. Only
                // numeric variables merge — the deferred operator check
                // cannot reject `<` on Bool or Char once resolved.
                if let Some(LocalType::LiteralVariable(other)) =
                    self.check_operand_local_type(literal_expression).ok()
                    && matches!(
                        variable_kind,
                        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                    )
                    && self
                        .literal_variables
                        .get(other)
                        .is_some_and(|v| v.resolved.is_none())
                {
                    self.merge_literal_variables(id, other)?;
                    return Ok(LocalType::Concrete(PrimTy::Bool));
                }
                return Err(());
            };
            match (variable.kind, kind) {
                (PrimitiveLiteralKind::Integer, PrimitiveLiteralKind::Integer) => {
                    self.add_integer_literal_constraint(id, literal, literal_expression.hir_id)?;
                }
                (PrimitiveLiteralKind::FloatingPoint, PrimitiveLiteralKind::FloatingPoint) => {
                    self.add_float_literal_constraint(id, literal, literal_expression.hir_id)?;
                }
                _ => return Err(()),
            }
            return Ok(LocalType::Concrete(PrimTy::Bool));
        }

        // Re-derive the operand types after the literal-variable pass: an
        // operand evaluated in the loop above (e.g. a call argument in
        // `x == id(x)`) may have resolved the other side's literal
        // variable, so the types are read post-evaluation.
        let left_path_type = self
            .check_operand_local_type(left)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type));
        let right_path_type = self
            .check_operand_local_type(right)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type));

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
            ExprKind::Binary(operator, left, right)
                if matches!(
                    operator,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) =>
            {
                self.check_comparison(operator, left, right)?;
                Ok(PrimTy::Bool)
            }
            // Non-comparison binaries (`not (a + b)`, `flag and a & b`)
            // evaluate through the operand probe — their result is never
            // `Bool`, so the operand-type check reports the mismatch.
            ExprKind::Binary(..) => self.check_logical_operand_probe(operand),
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
            // `if`/`match` logical operands check each branch against the
            // `Bool` the logical position demands — `flag and if c { true }
            // else { false }` behaves like the annotated `let x: Bool = if`.
            ExprKind::If { .. } | ExprKind::Match { .. } => {
                let local_type = self.check_expr_value(operand, PrimTy::Bool)?;
                self.concrete_local_type(local_type).ok_or(())
            }
            _ => self.check_logical_operand_probe(operand),
        }
    }

    fn check_logical_operand_probe(&mut self, operand: &'hir Expr<'hir>) -> Result<PrimTy, ()> {
        let local_type = self.check_operand_local_type(operand)?;
        if let LocalType::Str = local_type {
            // `not s`/`s and x` — a `Str` operand in a `Bool` position
            // reports the `Bool` mismatch and recovers as `Bool`.
            self.errors.push(PrimitiveBodyError {
                node: operand.hir_id,
                kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                    expected: ScalarType::Primitive(PrimTy::Bool),
                    found: ScalarType::Str,
                },
            });
            return Ok(PrimTy::Bool);
        }
        if let LocalType::LiteralVariable(id) = local_type {
            // An integer or floating-point literal variable can
            // never resolve to `Bool`, so `not x` reports the
            // operand's family default rather than gating.
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            if variable.resolved.is_none() {
                return Ok(literal_variable_default_type(variable.kind));
            }
        }
        self.concrete_local_type(local_type).ok_or(())
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
                // Unresolved literal variables fall back to their family
                // default like the initializer-position operands — the
                // operand-type check then reports the `Bool` mismatch.
                ExprKind::Path(_) | ExprKind::Field { .. } => {
                    self.check_logical_operand_probe(operand)?
                }
                _ => return Err(()),
            };
            self.check_logical_operand_type(operand, operand_type)?;
        }

        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    /// `let s: Str = init` or `s = init` on a `Str` place: `Str` literals
    /// and `Str`-typed paths bind directly; a primitive initializer
    /// reports the `Str` mismatch; anything else stays outside the slice.
    fn check_str_annotated_initializer(&mut self, init: &Expr<'hir>) -> Result<LocalType, ()> {
        match init.kind {
            ExprKind::Lit(Lit::Str(_)) => {
                self.record_expression_type(init.hir_id, LocalType::Str);
                Ok(LocalType::Str)
            }
            // `let s: Str = self.text` — a `Str` field projection.
            ExprKind::Field { .. } => {
                match self
                    .input
                    .str_self_field_projections
                    .and_then(|fields| fields.get(&init.hir_id))
                    .copied()
                {
                    Some("`Str`") => {
                        self.record_expression_type(init.hir_id, LocalType::Str);
                        Ok(LocalType::Str)
                    }
                    _ => Err(()),
                }
            }
            // `let s: Str = { "a" }` or a statement-bearing block —
            // the statements check like body statements and the tail
            // must yield `Str`.
            ExprKind::Block(block) => {
                self.check_str_branch_block(block)?;
                self.record_expression_type(init.hir_id, LocalType::Str);
                Ok(LocalType::Str)
            }
            // `let s: Str = if c { "a" } else { "b" }` — the condition
            // checks like any `if` and each branch tail must yield `Str`.
            ExprKind::If { cond, then, els } => {
                self.check_if_condition(cond)?;
                self.check_str_branch_block(then)?;
                let Some(els) = els else {
                    return Err(());
                };
                self.check_str_branch_tail(els)?;
                self.record_expression_type(init.hir_id, LocalType::Str);
                Ok(LocalType::Str)
            }
            ExprKind::Lit(literal) => {
                self.errors.push(PrimitiveBodyError {
                    node: init.hir_id,
                    kind: PrimitiveBodyErrorKind::ScalarLiteralTypeMismatch {
                        expected: ScalarType::Str,
                        found: scalar_literal_kind(literal),
                    },
                });
                Ok(LocalType::Str)
            }
            _ => match self.check_local_path(init)? {
                LocalType::Str => Ok(LocalType::Str),
                LocalType::Concrete(found) => {
                    self.errors.push(PrimitiveBodyError {
                        node: init.hir_id,
                        kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                            expected: ScalarType::Str,
                            found: ScalarType::Primitive(found),
                        },
                    });
                    Ok(LocalType::Str)
                }
                LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                    let Some(variable) = self.literal_variables.get(id) else {
                        return Err(());
                    };
                    match variable.resolved {
                        Some(found) => self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                                expected: ScalarType::Str,
                                found: ScalarType::Primitive(found),
                            },
                        }),
                        None => self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarLiteralTypeMismatch {
                                expected: ScalarType::Str,
                                found: ScalarLiteralKind::Primitive(variable.kind),
                            },
                        }),
                    }
                    Ok(LocalType::Str)
                }
                LocalType::Error(_) => Ok(LocalType::Str),
            },
        }
    }

    /// Classify a non-`Str` comparison operand into the scalar type a
    /// `Str`-comparison mismatch names on its side: literals name their
    /// own type (`true` -> `Bool`, `1` -> the `I64` family default) and
    /// are still checked against it, so an out-of-range left literal
    /// reports `LiteralOutOfRange`; literal variables name their
    /// resolution or family default.
    fn scalar_operand_scalar_type(&mut self, operand: &'hir Expr<'hir>) -> Result<ScalarType, ()> {
        if let ExprKind::Lit(literal) = operand.kind {
            let Some(kind) = primitive_literal_kind(literal) else {
                return Err(());
            };
            let expected = match kind {
                PrimitiveLiteralKind::Integer => PrimTy::I64,
                PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
                PrimitiveLiteralKind::Bool => PrimTy::Bool,
                PrimitiveLiteralKind::Byte => PrimTy::Byte,
                PrimitiveLiteralKind::Character => PrimTy::Char,
            };
            self.check_annotated_literal(expected, literal, kind, operand.hir_id);
            return Ok(ScalarType::Primitive(expected));
        }
        match self.check_operand_local_type(operand)? {
            LocalType::Concrete(found) => Ok(ScalarType::Primitive(found)),
            LocalType::Str => Ok(ScalarType::Str),
            LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                let Some(variable) = self.literal_variables.get(id) else {
                    return Err(());
                };
                Ok(ScalarType::Primitive(variable.resolved.unwrap_or_else(
                    || literal_variable_default_type(variable.kind),
                )))
            }
            LocalType::Error(_) => Err(()),
        }
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
            LocalType::Str | LocalType::Error(_) => return None,
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
                    merged_with: None,
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
            LocalType::Str => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                        expected: ScalarType::Primitive(expected),
                        found: ScalarType::Str,
                    },
                });
            }
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
        // Statement-position `if`/`match` expressions discard their result;
        // branches and arms admit only statement blocks so the discarded
        // value is `()` (§6.11.1). Bare `block` statements stay gated.
        match expression.kind {
            ExprKind::If { cond, then, els } => return self.check_if_stmt(cond, then, els),
            ExprKind::Match {
                scrutinee,
                arms,
                source,
            } => return self.check_match_stmt(scrutinee, arms, source),
            ExprKind::Loop {
                body,
                label,
                source,
            } => return self.check_loop_stmt(expression.hir_id, body, label, source),
            ExprKind::Break { label, value } => {
                return self.check_break(expression.hir_id, label, value);
            }
            ExprKind::Continue { label } => return self.check_continue(expression.hir_id, label),
            ExprKind::Return(value) => return self.check_return(expression.hir_id, value),
            ExprKind::Block(_) => return Err(()),
            _ => {}
        }
        let local_type = self.check_expr_value(expression, self.input.return_type)?;
        self.record_expression_type(expression.hir_id, local_type);
        Ok(())
    }

    /// A statement-only block: every admitted statement, no tail expression.
    /// A value tail would be a discarded non-`()` value — out of scope.
    fn check_stmt_block(&mut self, block: &'hir Block<'hir>) -> Result<(), ()> {
        if block.safety != Safety::Safe {
            return Err(());
        }
        let literal_floor = self.literal_variables.len();
        for stmt in block.stmts {
            self.check_stmt(stmt)?;
        }
        // A `break`/`continue` tail (including the `while` desugaring's
        // `else { break }` block) is a statement-shaped exit; other value
        // tails would discard a non-`()` value — out of scope.
        match block.tail {
            None => {}
            Some(tail) => match tail.kind {
                ExprKind::Break { label, value } => self.check_break(tail.hir_id, label, value)?,
                ExprKind::Continue { label } => self.check_continue(tail.hir_id, label)?,
                // An `if`/`match` tail in statement position discards its
                // result like an `if`/`match` statement — the `while`
                // desugaring's then-block ends with the user's `if`.
                ExprKind::If { .. } | ExprKind::Match { .. } => self.check_expr_stmt(tail)?,
                ExprKind::Return(value) => self.check_return(tail.hir_id, value)?,
                _ => return Err(()),
            },
        }
        self.default_unresolved_literals(literal_floor);
        Ok(())
    }

    /// Statement-position `loop`/`while` (`for` desugars to iterator calls
    /// and `Option` matches that stay gated inside its body). The loop
    /// pushes a `Γ.loops` entry so `break`/`continue` inside it resolve.
    fn check_loop_stmt(
        &mut self,
        hir_id: HirId,
        body: &'hir Block<'hir>,
        label: Option<Label>,
        source: LoopSource,
    ) -> Result<(), ()> {
        self.check_loop_core(hir_id, body, label, source, LoopBreakState::Unfixed)?;
        Ok(())
    }

    /// A `loop` in value position: `β` starts at the caller's seed — the
    /// expected type when one is known (tail / annotated initializer) or
    /// `Unfixed` for a fresh `β` (unannotated initializer). Returns the
    /// frame's break state so the caller can bind `τ = β`.
    fn check_loop_value(
        &mut self,
        hir_id: HirId,
        body: &'hir Block<'hir>,
        label: Option<Label>,
        seed: LoopBreakState,
    ) -> Result<LoopBreakState, ()> {
        self.check_loop_core(hir_id, body, label, LoopSource::Loop, seed)
    }

    /// Shared body checking for statement- and value-position loops: the
    /// label-shadow diagnostic, the frame's lifetime across the body's
    /// statements and tail, and the body's literal defaulting once the
    /// frame is popped.
    fn check_loop_core(
        &mut self,
        hir_id: HirId,
        body: &'hir Block<'hir>,
        label: Option<Label>,
        source: LoopSource,
        break_state: LoopBreakState,
    ) -> Result<LoopBreakState, ()> {
        if body.safety != Safety::Safe {
            return Err(());
        }
        // §6.16.4: a label that repeats an enclosing label in the same
        // body is LABEL_SHADOWED — the inner frame still shadows for
        // resolution.
        if let Some(label) = label {
            if self
                .loop_stack
                .iter()
                .any(|frame| frame.label == Some(label.name))
            {
                self.errors.push(PrimitiveBodyError {
                    node: hir_id,
                    kind: PrimitiveBodyErrorKind::LabelShadowed,
                });
            }
        }
        self.loop_stack.push(LoopFrame {
            label: label.map(|label| label.name),
            source,
            break_state,
        });
        let literal_floor = self.literal_variables.len();
        let result = (|| {
            for stmt in body.stmts {
                self.check_stmt(stmt)?;
            }
            // The `while` desugaring ends the body with a synthetic `if`
            // tail; user `loop` bodies admit break/continue tails only —
            // a value tail would make the block non-`()`.
            match body.tail {
                None => Ok(()),
                Some(tail) => match tail.kind {
                    ExprKind::Break { label, value } => self.check_break(tail.hir_id, label, value),
                    ExprKind::Continue { label } => self.check_continue(tail.hir_id, label),
                    ExprKind::If { .. } | ExprKind::Match { .. } => self.check_expr_stmt(tail),
                    ExprKind::Return(value) => self.check_return(tail.hir_id, value),
                    _ => Err(()),
                },
            }
        })();
        let frame = self.loop_stack.pop();
        result?;
        self.default_unresolved_literals(literal_floor);
        Ok(frame
            .map(|frame| frame.break_state)
            .unwrap_or(LoopBreakState::Unfixed))
    }

    /// `break` targets the innermost loop, or the innermost loop carrying
    /// `label` (§6.16.4). A value is only valid on `loop` — `while`/`for`
    /// report `INVALID_BREAK_VALUE`; `loop` values stay gated until the
    /// loop's result type is inferred.
    fn check_break(
        &mut self,
        hir_id: HirId,
        label: Option<Label>,
        value: Option<&'hir Expr<'hir>>,
    ) -> Result<(), ()> {
        let target = match label {
            Some(label) => self
                .loop_stack
                .iter()
                .rposition(|frame| frame.label == Some(label.name)),
            None => self.loop_stack.len().checked_sub(1),
        };
        let Some(index) = target else {
            self.errors.push(PrimitiveBodyError {
                node: hir_id,
                kind: if label.is_some() {
                    PrimitiveBodyErrorKind::UndeclaredLabel
                } else {
                    PrimitiveBodyErrorKind::BreakOutsideLoop
                },
            });
            return Ok(());
        };
        if let Some(value) = value {
            if self.loop_stack[index].source != LoopSource::Loop {
                self.errors.push(PrimitiveBodyError {
                    node: value.hir_id,
                    kind: PrimitiveBodyErrorKind::InvalidBreakValue,
                });
                return Ok(());
            }
            match self.loop_stack[index].break_state {
                // `β` was already unified with `()` by a bare `break`.
                LoopBreakState::Bare => self.errors.push(PrimitiveBodyError {
                    node: value.hir_id,
                    kind: PrimitiveBodyErrorKind::InvalidBreakValue,
                }),
                LoopBreakState::Unfixed => {
                    let local_type = self.check_break_value(value)?;
                    self.record_break_value_type(index, value, local_type)?;
                }
                LoopBreakState::Concrete(expected) => {
                    self.check_expr_expected(value, expected)?;
                }
                LoopBreakState::LiteralVariable(id) => {
                    // `β`'s group may have resolved through another
                    // constraint site between breaks — behave as if `β`
                    // were concrete from the start.
                    if let Some(found) = self.literal_variable_group_type(id) {
                        self.loop_stack[index].break_state = LoopBreakState::Concrete(found);
                        self.check_expr_expected(value, found)?;
                        return Ok(());
                    }
                    let local_type = self.check_break_value(value)?;
                    self.record_break_value_type(index, value, local_type)?;
                }
            }
            return Ok(());
        }
        // A bare `break` unifies `β` with `()` — mixing it with a valued
        // break reports `INVALID_BREAK_VALUE` since `()` is not a `PrimTy`.
        match self.loop_stack[index].break_state {
            LoopBreakState::Unfixed => {
                self.loop_stack[index].break_state = LoopBreakState::Bare;
            }
            LoopBreakState::Bare => {}
            LoopBreakState::Concrete(_) | LoopBreakState::LiteralVariable(_) => {
                self.errors.push(PrimitiveBodyError {
                    node: hir_id,
                    kind: PrimitiveBodyErrorKind::InvalidBreakValue,
                });
            }
        }
        Ok(())
    }

    /// Records a checked `break` value against the frame's `β`. The
    /// state is re-read rather than assumed to match the one the `break`
    /// entered with: checking `value` can itself resolve `β` when the
    /// value is a nested `loop` containing a labeled `break` into this
    /// very frame (`break 'l loop { break 'l true, break 1 }`).
    fn record_break_value_type(
        &mut self,
        index: usize,
        value: &'hir Expr<'hir>,
        local_type: LocalType,
    ) -> Result<(), ()> {
        // A `LiteralVariable` value or state whose group already resolved
        // behaves like the concrete type it resolved to — the same
        // resolved-carry rule the `Unfixed` arm applies. Without this, a
        // group resolved mid-evaluation (e.g. a labeled `break 'l m`
        // merging `β` into an already-resolved group) would hit
        // `resolve_literal_variable`'s re-resolution refusal and gate a
        // sound program.
        let local_type = match local_type {
            LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                match self.literal_variable_group_type(id) {
                    Some(found) => LocalType::Concrete(found),
                    None => local_type,
                }
            }
            other => other,
        };
        if let LoopBreakState::LiteralVariable(id) = self.loop_stack[index].break_state
            && let Some(found) = self.literal_variable_group_type(id)
        {
            self.loop_stack[index].break_state = LoopBreakState::Concrete(found);
        }
        match self.loop_stack[index].break_state {
            LoopBreakState::Unfixed => {
                self.loop_stack[index].break_state = match local_type {
                    LocalType::Concrete(found) => LoopBreakState::Concrete(found),
                    LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                        // A break value may carry an already-resolved
                        // literal variable (`break 1 + n` where `n` has
                        // a concrete type) — `β` is fixed, not fresh.
                        match self.literal_variable_group_type(id) {
                            Some(found) => LoopBreakState::Concrete(found),
                            None => LoopBreakState::LiteralVariable(id),
                        }
                    }
                    LocalType::Error(_) => LoopBreakState::Unfixed,
                    // A `Str` break value cannot fix `β` — `LoopBreakState`
                    // has no `Str` member, so the loop stays gated.
                    LocalType::Str => return Err(()),
                };
            }
            // A bare `break` inside `value` unified `β` with `()` — this
            // valued `break` can no longer apply.
            LoopBreakState::Bare => self.errors.push(PrimitiveBodyError {
                node: value.hir_id,
                kind: PrimitiveBodyErrorKind::InvalidBreakValue,
            }),
            LoopBreakState::Concrete(expected) => match local_type {
                LocalType::Concrete(found) => {
                    if found != expected {
                        self.errors.push(PrimitiveBodyError {
                            node: value.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                    }
                }
                LocalType::Str => {
                    self.errors.push(PrimitiveBodyError {
                        node: value.hir_id,
                        kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                            expected: ScalarType::Primitive(expected),
                            found: ScalarType::Str,
                        },
                    });
                }
                LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                    match self.literal_variable_group_type(id) {
                        Some(found) => {
                            if found != expected {
                                self.errors.push(PrimitiveBodyError {
                                    node: value.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                                });
                            }
                        }
                        None => {
                            let Some(kind) =
                                self.literal_variables.get(id).map(|variable| variable.kind)
                            else {
                                return Err(());
                            };
                            if literal_family_accepts(kind, expected) {
                                self.resolve_literal_variable(id, expected)?;
                            } else {
                                self.errors.push(PrimitiveBodyError {
                                    node: value.hir_id,
                                    kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                        expected: literal_variable_default_type(kind),
                                        found: concrete_literal_family(expected),
                                    },
                                });
                            }
                        }
                    }
                }
                LocalType::Error(_) => {}
            },
            LoopBreakState::LiteralVariable(id) => match local_type {
                // A concrete break value fixes `β` — later literal
                // breaks keep flowing through it.
                LocalType::Concrete(found) => {
                    let Some(kind) = self.literal_variables.get(id).map(|variable| variable.kind)
                    else {
                        return Err(());
                    };
                    if literal_family_accepts(kind, found) {
                        self.resolve_literal_variable(id, found)?;
                        self.loop_stack[index].break_state = LoopBreakState::Concrete(found);
                    } else {
                        // `break 1` then `break true`: the value
                        // cannot join `β`'s literal family — the
                        // diagnostic names `β`'s default.
                        self.errors.push(PrimitiveBodyError {
                            node: value.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected: literal_variable_default_type(kind),
                                found: concrete_literal_family(found),
                            },
                        });
                    }
                }
                // Another literal break value joins `β`'s group so
                // all of them resolve to the same eventual type.
                LocalType::LiteralVariable(other) => {
                    let (Some(kind), Some(other_kind)) = (
                        self.literal_variables.get(id).map(|variable| variable.kind),
                        self.literal_variables
                            .get(other)
                            .map(|variable| variable.kind),
                    ) else {
                        return Err(());
                    };
                    if self.literal_variable_group_root(id)
                        == self.literal_variable_group_root(other)
                    {
                        // The same variable (or merge group) breaks
                        // twice — `β` and the value already share a
                        // group, nothing to join.
                    } else if kind == other_kind {
                        self.merge_literal_variables(id, other)?;
                    } else {
                        self.errors.push(PrimitiveBodyError {
                            node: value.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected: literal_variable_default_type(kind),
                                found: other_kind,
                            },
                        });
                    }
                }
                _ => return Err(()),
            },
        }
        Ok(())
    }

    /// A `break` value while the loop's `β` is still unfixed: value shapes
    /// that need a known expected type (`if`/`match`/`return` tails and
    /// `~literal`) stay gated; everything else flows through
    /// `check_expr_value`, where `expected` is unused by the admitted
    /// shapes (the function's return type is passed as an inert stand-in).
    fn check_break_value(&mut self, value: &'hir Expr<'hir>) -> Result<LocalType, ()> {
        match value.kind {
            // `if`/`match` break values join `β`'s literal group through
            // the same literal-join path unannotated `let` initializers
            // use — branches must still be literal tails.
            ExprKind::If { cond, then, els } => {
                return self.check_literal_join_if(cond, then, els);
            }
            ExprKind::Match {
                scrutinee,
                arms,
                source,
            } => {
                return self.check_literal_join_match(scrutinee, arms, source);
            }
            // A `loop` `break` value yields its own `τ` — the inner `β`
            // resolved or left as a literal group binds to the outer `β`
            // like an unannotated `let` initializer binds it.
            ExprKind::Loop {
                body,
                label,
                source,
            } => {
                if source != LoopSource::Loop {
                    return Err(());
                }
                return match self.check_loop_value(
                    value.hir_id,
                    body,
                    label,
                    LoopBreakState::Unfixed,
                )? {
                    LoopBreakState::Concrete(found) => Ok(LocalType::Concrete(found)),
                    LoopBreakState::LiteralVariable(id) => {
                        match self.literal_variable_group_type(id) {
                            Some(found) => Ok(LocalType::Concrete(found)),
                            None => Ok(LocalType::LiteralVariable(id)),
                        }
                    }
                    LoopBreakState::Unfixed | LoopBreakState::Bare => Err(()),
                };
            }
            ExprKind::Return(..) => {
                return Err(());
            }
            ExprKind::Unary(UnOp::BitNot, operand) if matches!(operand.kind, ExprKind::Lit(_)) => {
                return Err(());
            }
            _ => {}
        }
        self.check_expr_value(value, self.input.return_type)
    }

    /// `continue` resolves like `break`, minus the value rule.
    /// `return e` checks `e` against the declared primitive return type like a
    /// tail; a bare `return` in a non-unit body reports TYPE_MISMATCH (§6.17).
    fn check_return(&mut self, hir_id: HirId, value: Option<&'hir Expr<'hir>>) -> Result<(), ()> {
        match value {
            Some(value) => self.check_expr_expected(value, self.input.return_type),
            None => {
                self.errors.push(PrimitiveBodyError {
                    node: hir_id,
                    kind: PrimitiveBodyErrorKind::MissingReturnValue {
                        expected: self.input.return_type,
                    },
                });
                Ok(())
            }
        }
    }

    fn check_continue(&mut self, hir_id: HirId, label: Option<Label>) -> Result<(), ()> {
        let found = match label {
            Some(label) => self
                .loop_stack
                .iter()
                .any(|frame| frame.label == Some(label.name)),
            None => !self.loop_stack.is_empty(),
        };
        if !found {
            self.errors.push(PrimitiveBodyError {
                node: hir_id,
                kind: if label.is_some() {
                    PrimitiveBodyErrorKind::UndeclaredLabel
                } else {
                    PrimitiveBodyErrorKind::ContinueOutsideLoop
                },
            });
        }
        Ok(())
    }

    /// Statement-position `if`/`else if`/`else`: branches are statement-only
    /// blocks, so the expression is `()` and an `else` is optional.
    fn check_if_stmt(
        &mut self,
        condition: &'hir Expr<'hir>,
        then: &'hir Block<'hir>,
        els: Option<&'hir Expr<'hir>>,
    ) -> Result<(), ()> {
        self.check_if_condition(condition)?;
        self.check_stmt_block(then)?;
        match els {
            None => Ok(()),
            Some(els) => match els.kind {
                ExprKind::Block(block) => self.check_stmt_block(block),
                ExprKind::If { cond, then, els } => self.check_if_stmt(cond, then, els),
                _ => Err(()),
            },
        }
    }

    /// Statement-position `match`: the same scrutinee, pattern, guard and
    /// exhaustiveness admission as `check_match_tail`, but arm bodies must
    /// be statement-only blocks.
    fn check_match_stmt(
        &mut self,
        scrutinee: &'hir Expr<'hir>,
        arms: &'hir [Arm<'hir>],
        source: MatchSource,
    ) -> Result<(), ()> {
        if !matches!(source, MatchSource::Normal) {
            return Err(());
        }
        let ExprKind::Path(_) = scrutinee.kind else {
            return Err(());
        };
        let LocalType::Concrete(scrutinee_type) = self.check_local_path(scrutinee)? else {
            return Err(());
        };
        let mut catch_all = false;
        let mut bool_cover = (false, false);
        let mut arm_binding: Option<(BindingId, Option<LocalType>)> = None;
        for arm in arms {
            let check_body = |this: &mut Self| -> Result<(), ()> {
                let ExprKind::Block(block) = arm.body.kind else {
                    return Err(());
                };
                this.check_stmt_block(block)
            };
            if catch_all {
                self.errors.push(PrimitiveBodyError {
                    node: arm.pat.hir_id,
                    kind: PrimitiveBodyErrorKind::UnreachablePattern,
                });
                if let PatKind::Binding {
                    mode: BindingMode::Value,
                    binding,
                    sub: None,
                    ..
                } = arm.pat.kind
                {
                    arm_binding = Some((
                        binding,
                        self.known_bindings
                            .insert(binding, LocalType::Concrete(scrutinee_type)),
                    ));
                }
                let body_result = check_body(self);
                self.restore_arm_binding(&mut arm_binding);
                body_result?;
                continue;
            }
            self.check_match_arm_pattern(
                arm,
                scrutinee_type,
                &mut catch_all,
                &mut bool_cover,
                &mut arm_binding,
            )?;
            let body_result = arm
                .guard
                .map(|guard| self.check_if_condition(guard))
                .unwrap_or(Ok(()))
                .and_then(|_| check_body(self));
            self.restore_arm_binding(&mut arm_binding);
            body_result?;
        }
        if !catch_all && !(scrutinee_type == PrimTy::Bool && bool_cover == (true, true)) {
            self.errors.push(PrimitiveBodyError {
                node: scrutinee.hir_id,
                kind: PrimitiveBodyErrorKind::NonExhaustiveMatch,
            });
        }
        Ok(())
    }

    /// `expected` is the type the expression's value unifies with: the
    /// return type at tail positions, or the annotation at annotated
    /// initializer/assign positions.
    // Tail-position unary operand admission mirrors the driver:
    // `-`/`~` over paths or self fields (`-lit` never reaches here — the
    // parser folds it into a literal), `~` over integer/byte literals,
    // and `not` over the logical-operand shapes `check_logical_operand`
    // handles. The runtime checkers gate anything this admission covers
    // that stays unsupported, so the predicate only needs to be a
    // superset of the driver mirror.
    fn unary_tail_operand_is_admissible(&self, operator: UnOp, operand: &Expr<'hir>) -> bool {
        match (operator, operand.kind) {
            (_, ExprKind::Path(_)) => true,
            (_, ExprKind::Field { .. }) => self.self_field_leaf_is_admissible(operand),
            (UnOp::BitNot, ExprKind::Lit(literal)) => matches!(
                primitive_literal_kind(literal),
                Some(PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte)
            ),
            (UnOp::Not, _) => self.logical_operand_shape_is_admissible(operand),
            _ => false,
        }
    }

    fn self_field_leaf_is_admissible(&self, operand: &Expr<'hir>) -> bool {
        self.input
            .self_field_projections
            .is_some_and(|fields| fields.contains_key(&operand.hir_id))
            || self
                .input
                .str_self_field_projections
                .is_some_and(|fields| fields.contains_key(&operand.hir_id))
            || self
                .input
                .unknown_self_fields
                .is_some_and(|fields| fields.contains(&operand.hir_id))
    }

    // Every leaf and tree shape `check_logical_operand` can type or
    // diagnose — the driver's `primitive_logical_init_operand_shape`
    // mirror refuses anything narrower, and refusal is what publishes
    // Unsupported, so admitting a superset here is consistent.
    fn logical_operand_shape_is_admissible(&self, operand: &Expr<'hir>) -> bool {
        match operand.kind {
            ExprKind::Lit(_)
            | ExprKind::Path(_)
            | ExprKind::Block(_)
            | ExprKind::If { .. }
            | ExprKind::Match { .. } => true,
            ExprKind::Field { .. } => self.self_field_leaf_is_admissible(operand),
            ExprKind::Call { base: None, .. } => self
                .input
                .call_signatures
                .is_some_and(|signatures| signatures.contains_key(&operand.hir_id)),
            ExprKind::Unary(UnOp::Not, inner) => self.logical_operand_shape_is_admissible(inner),
            ExprKind::Binary(operator, left, right) => match operator {
                BinOp::And | BinOp::Or => {
                    self.logical_operand_shape_is_admissible(left)
                        && self.logical_operand_shape_is_admissible(right)
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    self.operand_shape_is_admissible(left)
                        && self.operand_shape_is_admissible(right)
                }
                _ => false,
            },
            _ => false,
        }
    }

    // The comparison-leaf counterpart of
    // `logical_operand_shape_is_admissible`: the driver mirror's nested
    // operand shapes — literals, paths, fields, direct calls, the
    // initializer unary shapes, nested binary trees, and blocks.
    fn operand_shape_is_admissible(&self, operand: &Expr<'hir>) -> bool {
        match operand.kind {
            ExprKind::Lit(_)
            | ExprKind::Path(_)
            | ExprKind::Block(_)
            | ExprKind::If { .. }
            | ExprKind::Match { .. } => true,
            ExprKind::Field { .. } => self.self_field_leaf_is_admissible(operand),
            ExprKind::Call { base: None, .. } => self
                .input
                .call_signatures
                .is_some_and(|signatures| signatures.contains_key(&operand.hir_id)),
            ExprKind::Unary(UnOp::Not, inner) => self.logical_operand_shape_is_admissible(inner),
            ExprKind::Unary(UnOp::Neg, inner) => {
                matches!(inner.kind, ExprKind::Path(_))
                    || (matches!(inner.kind, ExprKind::Field { .. })
                        && self.self_field_leaf_is_admissible(inner))
            }
            ExprKind::Unary(UnOp::BitNot, inner) => match inner.kind {
                ExprKind::Path(_) => true,
                ExprKind::Field { .. } => self.self_field_leaf_is_admissible(inner),
                ExprKind::Lit(literal) => matches!(
                    primitive_literal_kind(literal),
                    Some(PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte)
                ),
                _ => false,
            },
            ExprKind::Binary(_, left, right) => {
                self.operand_shape_is_admissible(left) && self.operand_shape_is_admissible(right)
            }
            _ => false,
        }
    }

    fn check_expr_value(
        &mut self,
        expression: &Expr<'hir>,
        expected: PrimTy,
    ) -> Result<LocalType, ()> {
        let local_type = if self.expression_has_unknown_field_error(expression) {
            // The shared tainted path recovers every root shape
            // `check_error_tainted_expression` supports — leaf roots
            // (`self.missing`), unary roots (`-self.missing`), and binary
            // trees (`self.missing + 1`) — so tails follow the
            // statement-position recovery.
            self.check_error_tainted_expression(expression)?;
            if let Some(id) = self.error_tainted_literal_variable(expression) {
                LocalType::ErrorLiteralVariable(id)
            } else {
                LocalType::Error(self.known_error_tainted_result_type(expression))
            }
        } else {
            match expression.kind {
                ExprKind::Call { .. } => self.check_direct_call(expression, false)?,
                ExprKind::Field { .. } => {
                    if let Some(primitive) = self
                        .input
                        .self_field_projections
                        .and_then(|fields| fields.get(&expression.hir_id))
                    {
                        LocalType::Concrete(*primitive)
                    } else {
                        match self
                            .input
                            .str_self_field_projections
                            .and_then(|fields| fields.get(&expression.hir_id))
                            .copied()
                        {
                            Some("`Str`") => LocalType::Str,
                            _ => return Err(()),
                        }
                    }
                }
                ExprKind::Block(block) => {
                    self.check_block_expected(block, expected)?;
                    LocalType::Concrete(expected)
                }
                ExprKind::Unary(UnOp::Not, operand) => {
                    // `not e` admits the same operand-position logical
                    // shapes as `let x = not e` (path, `Bool` literal,
                    // `not`, `and`/`or`, comparison).
                    let operand_type = self.check_logical_operand(operand)?;
                    self.check_logical_operand_type(operand, operand_type)?;
                    LocalType::Concrete(PrimTy::Bool)
                }
                ExprKind::Unary(UnOp::BitNot, operand)
                    if matches!(operand.kind, ExprKind::Lit(_)) =>
                {
                    // Literal `~` operands follow the let-initializer
                    // shapes: integer literals take the expected type,
                    // byte literals are `Byte`.
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
                            LocalType::Concrete(expected)
                        }
                        Some(PrimitiveLiteralKind::Byte) => {
                            self.record_expression_type(
                                operand.hir_id,
                                LocalType::Concrete(PrimTy::Byte),
                            );
                            if expected != PrimTy::Byte {
                                self.errors.push(PrimitiveBodyError {
                                    node: expression.hir_id,
                                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                                        expected,
                                        found: PrimTy::Byte,
                                    },
                                });
                            }
                            LocalType::Concrete(expected)
                        }
                        _ => return Err(()),
                    }
                }
                ExprKind::Unary(operator, operand)
                    if matches!(operator, UnOp::Neg | UnOp::BitNot)
                        && matches!(operand.kind, ExprKind::Path(_) | ExprKind::Field { .. }) =>
                {
                    match (operator, operand.kind) {
                        (UnOp::Neg, ExprKind::Path(_)) => {
                            self.check_direct_call_neg_path(operand, expression.hir_id)?
                        }
                        (_, ExprKind::Path(_)) => {
                            self.check_bitwise_not_path(operand, expression.hir_id)?
                        }
                        // `-self.f`/`~self.f` in tail position evaluate the
                        // field operand's declared type — same desugar as
                        // the unary field operands elsewhere.
                        _ => self.check_direct_call_unary_field(
                            operand,
                            operator,
                            expression.hir_id,
                        )?,
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
                ExprKind::Lit(Lit::Str(_)) => {
                    self.record_expression_type(expression.hir_id, LocalType::Str);
                    LocalType::Str
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
                ExprKind::If { cond, then, els } => {
                    self.check_if_tail(cond, then, els, expected)?
                }
                ExprKind::Match {
                    scrutinee,
                    arms,
                    source,
                } => self.check_match_tail(scrutinee, arms, source, expected)?,
                // A `return` in tail position is the never-typed exit
                // expression: the value is checked against the
                // function's return type (not the surrounding expected
                // type — an annotated `let x: Bool = if flag { return true }
                // else { false }` in an I32 body still checks `true`
                // against I32), and a bare `return` reports the same
                // missing-value diagnostic as the statement form.
                ExprKind::Return(value) => {
                    self.check_return(expression.hir_id, value)?;
                    LocalType::Concrete(expected)
                }
                // A `loop` tail yields the type `break` values fix through
                // `β` — here `β` is the expected type, so each `break v`
                // checks `v ⇐ expected` directly (§6.16.1: `β = X` when the
                // loop has a known expected type). A bare-only loop unifies
                // `β` with `()` and reports INVALID_BREAK_VALUE — `PrimTy`
                // has no `()` member to name in a TypeMismatch.
                ExprKind::Loop {
                    body,
                    label,
                    source,
                } => {
                    if source != LoopSource::Loop {
                        return Err(());
                    }
                    self.check_loop_value(
                        expression.hir_id,
                        body,
                        label,
                        LoopBreakState::Concrete(expected),
                    )?;
                    LocalType::Concrete(expected)
                }
                _ => self.check_local_path(expression)?,
            }
        };
        Ok(local_type)
    }

    fn check_expr_expected(&mut self, expression: &Expr<'hir>, expected: PrimTy) -> Result<(), ()> {
        let local_type = self.check_expr_value(expression, expected)?;
        match local_type {
            LocalType::Error(found) => {
                if let Some(found) = found {
                    self.check_concrete_tail_type(found, expected, expression.hir_id);
                }
            }
            LocalType::Str => {
                self.errors.push(PrimitiveBodyError {
                    node: expression.hir_id,
                    kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                        expected: ScalarType::Primitive(expected),
                        found: ScalarType::Str,
                    },
                });
            }
            LocalType::Concrete(found) => {
                self.check_concrete_tail_type(found, expected, expression.hir_id);
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
                    Some(found) => {
                        self.check_concrete_tail_type(found, expected, expression.hir_id)
                    }
                    None if literal_family_accepts(kind, expected) => {
                        self.resolve_literal_variable(id, expected)?;
                    }
                    None => {
                        self.errors.push(PrimitiveBodyError {
                            node: expression.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected,
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
    fn assign_place(&mut self, lhs: &Expr<'hir>) -> Result<Option<(LocalType,)>, ()> {
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
        if !matches!(
            local_type,
            LocalType::Concrete(_) | LocalType::Str | LocalType::LiteralVariable(_)
        ) {
            return Err(());
        }
        if !self.mutable_bindings.contains(&binding) {
            self.errors.push(PrimitiveBodyError {
                node: lhs.hir_id,
                kind: PrimitiveBodyErrorKind::AssignToImmutable,
            });
        }
        Ok(Some((local_type,)))
    }

    /// Simple assignment `place = e`. A concrete place checks `e` at the
    /// coercion site to the place type. An unannotated literal `var` place
    /// (`var x = 1; x = 2`) keeps the binding's shared literal variable: a
    /// literal right side joins it as an additional literal site, and a
    /// concretely typed right side resolves the variable at the assignment.
    fn check_assign_stmt(&mut self, lhs: &Expr<'hir>, rhs: &Expr<'hir>) -> Result<(), ()> {
        let Some((place_type,)) = self.assign_place(lhs)? else {
            return Ok(());
        };
        match place_type {
            LocalType::Concrete(place_type) => {
                self.check_initializer_value(rhs, Some(place_type))?;
                self.record_expression_type(lhs.hir_id, LocalType::Concrete(place_type));
                Ok(())
            }
            // `s = e` on a `Str` place desugars like `let s: Str = e` — the
            // right side is checked against `Str` at the coercion site.
            LocalType::Str => {
                self.check_str_annotated_initializer(rhs)?;
                self.record_expression_type(lhs.hir_id, LocalType::Str);
                Ok(())
            }
            LocalType::LiteralVariable(id) => {
                if let ExprKind::Lit(literal) = rhs.kind {
                    let Some(variable) = self.literal_variables.get(id) else {
                        return Err(());
                    };
                    match variable.kind {
                        PrimitiveLiteralKind::Integer => {
                            self.add_integer_literal_constraint(id, literal, rhs.hir_id)?;
                        }
                        PrimitiveLiteralKind::FloatingPoint => {
                            self.add_float_literal_constraint(id, literal, rhs.hir_id)?;
                        }
                        PrimitiveLiteralKind::Bool
                        | PrimitiveLiteralKind::Byte
                        | PrimitiveLiteralKind::Character => return Err(()),
                    }
                    self.record_expression_type(rhs.hir_id, LocalType::LiteralVariable(id));
                } else {
                    let check = self.check_initializer_value(rhs, None)?;
                    match check.local_type {
                        LocalType::Concrete(found) => {
                            // A variable already resolved by an earlier
                            // assignment coerces the right side to the
                            // resolved place type; an unresolved one
                            // resolves to the right side's type.
                            match self.literal_variables.get(id).and_then(|v| v.resolved) {
                                Some(expected) => self.check_annotated_path(
                                    LocalType::Concrete(found),
                                    expected,
                                    rhs.hir_id,
                                )?,
                                None => self.check_annotated_path(
                                    LocalType::LiteralVariable(id),
                                    found,
                                    rhs.hir_id,
                                )?,
                            }
                        }
                        // `x = y`/`x = y + 1` — the right side is itself a
                        // literal variable: the two variables share one
                        // group and resolve together, like `x op y`
                        // operand merges.
                        LocalType::LiteralVariable(other)
                        | LocalType::ErrorLiteralVariable(other) => {
                            // A resolved group on either side checks like
                            // its concrete type instead of merging.
                            match (
                                self.literal_variable_group_type(id),
                                self.literal_variable_group_type(other),
                            ) {
                                (Some(expected), Some(found)) => self.check_annotated_path(
                                    LocalType::Concrete(found),
                                    expected,
                                    rhs.hir_id,
                                )?,
                                (Some(expected), None) => self.check_annotated_path(
                                    LocalType::LiteralVariable(other),
                                    expected,
                                    rhs.hir_id,
                                )?,
                                (None, Some(found)) => self.check_annotated_path(
                                    LocalType::LiteralVariable(id),
                                    found,
                                    rhs.hir_id,
                                )?,
                                (None, None) => {
                                    if self.literal_variable_group_root(id)
                                        != self.literal_variable_group_root(other)
                                    {
                                        let (Some(kind), Some(other_kind)) = (
                                            self.literal_variables
                                                .get(id)
                                                .map(|variable| variable.kind),
                                            self.literal_variables
                                                .get(other)
                                                .map(|variable| variable.kind),
                                        ) else {
                                            return Err(());
                                        };
                                        if kind != other_kind {
                                            self.errors.push(PrimitiveBodyError {
                                                node: rhs.hir_id,
                                                kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                                    expected: literal_variable_default_type(kind),
                                                    found: other_kind,
                                                },
                                            });
                                        } else {
                                            self.merge_literal_variables(id, other)?;
                                        }
                                    }
                                }
                            }
                            self.record_expression_type(rhs.hir_id, LocalType::LiteralVariable(id));
                        }
                        _ => return Err(()),
                    }
                }
                self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                Ok(())
            }
            _ => Err(()),
        }
    }

    /// `place op= e` desugars to `{ let t = e; place = place op t }`
    /// (decisions.md:489). Checking `place op e` against the place type is
    /// equivalent: `t` is a fresh binding whose literal variable resolves
    /// through the operation exactly as `e` does inside `place op e`.
    // `x = x op t` once the desugared `let t = e` binding's type is
    // known: arithmetic and bitwise operands must equal the place type,
    // a shift count must be `U32`, logical operands `Bool`; an
    // unresolved literal-variable right side resolves to the demanded
    // type.
    fn check_assign_op_rhs_local_type(
        &mut self,
        stmt_id: HirId,
        op: BinOp,
        place: PrimTy,
        rhs: &'hir Expr<'hir>,
        rhs_local: LocalType,
    ) -> Result<(), ()> {
        let demanded = match op {
            BinOp::Shl | BinOp::Shr => PrimTy::U32,
            BinOp::And | BinOp::Or => PrimTy::Bool,
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
            | BinOp::BitXor => place,
            _ => return Err(()),
        };
        let right_type = match self.concrete_local_type(rhs_local) {
            Some(primitive) => primitive,
            None => {
                let LocalType::LiteralVariable(other) = rhs_local else {
                    return Err(());
                };
                let joinable = self.literal_variables.get(other).is_some_and(|variable| {
                    variable.resolved.is_none()
                        && match op {
                            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => matches!(
                                variable.kind,
                                PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint
                            ),
                            BinOp::And | BinOp::Or => variable.kind == PrimitiveLiteralKind::Bool,
                            _ => variable.kind == PrimitiveLiteralKind::Integer,
                        }
                });
                if !joinable {
                    return Err(());
                }
                self.resolve_literal_variable(other, demanded)?;
                demanded
            }
        };
        match op {
            BinOp::Shl | BinOp::Shr => {
                if right_type != demanded {
                    self.errors.push(PrimitiveBodyError {
                        node: rhs.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch {
                            expected: demanded,
                            found: right_type,
                        },
                    });
                }
                if is_integer_primitive(place) {
                    Ok(())
                } else {
                    Err(())
                }
            }
            BinOp::And | BinOp::Or => {
                if right_type != PrimTy::Bool {
                    self.errors.push(PrimitiveBodyError {
                        node: rhs.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch {
                            expected: PrimTy::Bool,
                            found: right_type,
                        },
                    });
                }
                if place == PrimTy::Bool {
                    Ok(())
                } else {
                    Err(())
                }
            }
            _ => {
                if right_type != place {
                    self.errors.push(PrimitiveBodyError {
                        node: rhs.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch {
                            expected: place,
                            found: right_type,
                        },
                    });
                    return Ok(());
                }
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        match primitive_arithmetic_result(op, place, right_type) {
                            PrimitiveArithmeticResult::Type(result) => {
                                if result == place {
                                    Ok(())
                                } else {
                                    self.errors.push(PrimitiveBodyError {
                                        node: stmt_id,
                                        kind: PrimitiveBodyErrorKind::TypeMismatch {
                                            expected: place,
                                            found: result,
                                        },
                                    });
                                    Ok(())
                                }
                            }
                            _ => Err(()),
                        }
                    }
                    BinOp::Rem => {
                        if is_integer_primitive(place) {
                            Ok(())
                        } else {
                            self.report_unsupported_float_remainder(stmt_id, place);
                            Ok(())
                        }
                    }
                    _ => {
                        if is_integer_primitive(place) {
                            Ok(())
                        } else {
                            Err(())
                        }
                    }
                }
            }
        }
    }

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
        // A resolved literal variable checks like its concrete type, so a
        // second `x op= e` after an earlier statement resolved `x` keeps
        // working.
        let place_type = match self.concrete_local_type(place_type) {
            Some(concrete) => LocalType::Concrete(concrete),
            None => place_type,
        };
        let LocalType::Concrete(place_type) = place_type else {
            let LocalType::LiteralVariable(id) = place_type else {
                return Err(());
            };
            // `x <<= e`/`x >>= e` desugar to `x = x << e`: the shift keeps
            // `x`'s unresolved integer literal variable as the result, so
            // the right operand is checked as a `U32` shift count exactly
            // like `x << e`.
            if matches!(op, BinOp::Shl | BinOp::Shr) {
                if !self.literal_variables.get(id).is_some_and(|variable| {
                    variable.resolved.is_none() && variable.kind == PrimitiveLiteralKind::Integer
                }) {
                    return Err(());
                }
                if let ExprKind::Lit(literal) = rhs.kind {
                    let Some(kind) = primitive_literal_kind(literal) else {
                        return Err(());
                    };
                    if kind != PrimitiveLiteralKind::Integer {
                        return Err(());
                    }
                    self.check_annotated_literal(PrimTy::U32, literal, kind, rhs.hir_id);
                } else {
                    // A non-literal count checks as the desugared `let t =
                    // e` binding: a literal variable resolves to `U32`, a
                    // concrete count must be `U32`. A count mentioning `x`
                    // — `x <<= x - 1` — can resolve `x`'s variable while
                    // it evaluates; the desugar then leaves `x = x << t`
                    // on a concrete place.
                    let rhs_local = self.check_local_path(rhs).ok().or_else(|| {
                        self.check_initializer_value(rhs, None)
                            .ok()
                            .map(|check| check.local_type)
                    });
                    if let Some(place) = self.concrete_local_type(LocalType::LiteralVariable(id)) {
                        let Some(rhs_local) = rhs_local else {
                            return Err(());
                        };
                        self.check_assign_op_rhs_local_type(stmt_id, op, place, rhs, rhs_local)?;
                        self.record_expression_type(rhs.hir_id, rhs_local);
                        self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                        self.record_expression_type(stmt_id, LocalType::Concrete(place));
                        return Ok(());
                    }
                    let right_type = match rhs_local {
                        Some(LocalType::LiteralVariable(other))
                            if self.literal_variables.get(other).is_some_and(|v| {
                                v.resolved.is_none() && v.kind == PrimitiveLiteralKind::Integer
                            }) =>
                        {
                            self.resolve_literal_variable(other, PrimTy::U32)?;
                            PrimTy::U32
                        }
                        _ => rhs_local
                            .and_then(|local_type| self.concrete_local_type(local_type))
                            .filter(|primitive| supports_primitive_literal(*primitive))
                            .ok_or(())?,
                    };
                    if right_type != PrimTy::U32 {
                        self.errors.push(PrimitiveBodyError {
                            node: rhs.hir_id,
                            kind: PrimitiveBodyErrorKind::TypeMismatch {
                                expected: PrimTy::U32,
                                found: right_type,
                            },
                        });
                    }
                    self.record_expression_type(rhs.hir_id, LocalType::Concrete(right_type));
                }
                self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                self.record_expression_type(stmt_id, LocalType::LiteralVariable(id));
                return Ok(());
            }
            // `x op= literal` on an unresolved literal variable desugars
            // to `x = x op literal`: the result keeps the variable, so the
            // literal becomes another constraint site on it, exactly as
            // `x = literal` does.
            if !matches!(
                op,
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
                    | BinOp::BitXor
            ) {
                return Err(());
            }
            if !matches!(rhs.kind, ExprKind::Lit(_)) {
                // `x op= e` with a concretely typed right operand resolves
                // the variable to the operand's type when the family
                // admits it and the operator exists for that type pair —
                // the `x = x op e` desugar does the same. With an
                // unresolved literal-variable right side the two variables
                // merge instead, like `x op y` in an initializer. Non-path
                // right sides evaluate as the desugared `let t = e`
                // binding's initializer.
                let rhs_local = self.check_local_path(rhs).ok().or_else(|| {
                    self.check_initializer_value(rhs, None)
                        .ok()
                        .map(|check| check.local_type)
                });
                let right_type = rhs_local
                    .as_ref()
                    .and_then(|local_type| self.concrete_local_type(*local_type));
                // A right side mentioning `x` — `x += x + value` — can
                // resolve the place's variable while it evaluates; the
                // desugared `let t = e` then leaves `x = x op t` on a
                // concrete place.
                if let Some(place) = self.concrete_local_type(LocalType::LiteralVariable(id)) {
                    let Some(rhs_local) = rhs_local else {
                        return Err(());
                    };
                    self.check_assign_op_rhs_local_type(stmt_id, op, place, rhs, rhs_local)?;
                    self.record_expression_type(rhs.hir_id, rhs_local);
                    self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                    self.record_expression_type(stmt_id, LocalType::Concrete(place));
                    return Ok(());
                }
                if right_type.is_none() {
                    // The merged pair resolves without an operator check,
                    // so the place's kind must admit the operator for every
                    // type it could resolve to: `+ - * /` on numeric
                    // primitives, the rest on integers only.
                    let kind_eligible =
                        self.literal_variables
                            .get(id)
                            .is_some_and(|variable| match op {
                                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => matches!(
                                    variable.kind,
                                    PrimitiveLiteralKind::Integer
                                        | PrimitiveLiteralKind::FloatingPoint
                                ),
                                _ => variable.kind == PrimitiveLiteralKind::Integer,
                            });
                    if kind_eligible
                        && let Some(LocalType::LiteralVariable(other)) = rhs_local
                        && self
                            .literal_variables
                            .get(other)
                            .is_some_and(|v| v.resolved.is_none())
                    {
                        self.merge_literal_variables(id, other)?;
                        self.record_expression_type(rhs.hir_id, LocalType::LiteralVariable(id));
                        self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                        self.record_expression_type(stmt_id, LocalType::LiteralVariable(id));
                        return Ok(());
                    }
                    return Err(());
                }
                let right_type = right_type.unwrap();
                let resolved_type = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        match primitive_arithmetic_result(op, right_type, right_type) {
                            PrimitiveArithmeticResult::Type(result) => result,
                            _ => return Err(()),
                        }
                    }
                    BinOp::Rem => {
                        if !is_integer_primitive(right_type) {
                            return Err(());
                        }
                        right_type
                    }
                    BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
                        match primitive_wrapping_literal_left_result(
                            op,
                            PrimitiveLiteralKind::Integer,
                            right_type,
                        ) {
                            PrimitiveArithmeticResult::Type(result) => result,
                            _ => return Err(()),
                        }
                    }
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        match primitive_bitwise_literal_left_result(op, right_type) {
                            PrimitiveArithmeticResult::Type(result) => result,
                            _ => return Err(()),
                        }
                    }
                    _ => return Err(()),
                };
                self.resolve_literal_variable(id, resolved_type)?;
                self.record_expression_type(rhs.hir_id, LocalType::LiteralVariable(id));
                self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
                self.record_expression_type(stmt_id, LocalType::LiteralVariable(id));
                return Ok(());
            }
            let ExprKind::Lit(literal) = rhs.kind else {
                return Err(());
            };
            let Some(variable) = self.literal_variables.get(id) else {
                return Err(());
            };
            match variable.kind {
                PrimitiveLiteralKind::Integer => {
                    self.add_integer_literal_constraint(id, literal, rhs.hir_id)?;
                }
                PrimitiveLiteralKind::FloatingPoint => {
                    // Remainder, wrapping, and bitwise operators have no
                    // float implementation; the desugared `x = x op lit`
                    // reports the operator, and the literal still joins the
                    // variable as a constraint site.
                    if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
                        self.report_operator_not_supported(
                            stmt_id,
                            op,
                            PrimitiveOperatorOperand::FloatingPointLiteral,
                        );
                    }
                    self.add_float_literal_constraint(id, literal, rhs.hir_id)?;
                }
                PrimitiveLiteralKind::Bool
                | PrimitiveLiteralKind::Byte
                | PrimitiveLiteralKind::Character => return Err(()),
            }
            self.record_expression_type(rhs.hir_id, LocalType::LiteralVariable(id));
            self.record_expression_type(lhs.hir_id, LocalType::LiteralVariable(id));
            self.record_expression_type(stmt_id, LocalType::LiteralVariable(id));
            return Ok(());
        };
        if !matches!(rhs.kind, ExprKind::Lit(_) | ExprKind::Path(_)) {
            // `x op= e` desugars to `{ let t = e; x = x op t }`: the
            // operand checkers only accept operand shapes, so the
            // binding type is checked directly.
            let rhs_local = self.check_initializer_value(rhs, None)?.local_type;
            self.check_assign_op_rhs_local_type(stmt_id, op, place_type, rhs, rhs_local)?;
            self.record_expression_type(rhs.hir_id, rhs_local);
            self.record_expression_type(lhs.hir_id, LocalType::Concrete(place_type));
            self.record_expression_type(stmt_id, LocalType::Concrete(place_type));
            return Ok(());
        }
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
    // A `Str`-yielding branch block: `unsafe` blocks stay gated, each
    // statement checks like a body statement, and the tail yields
    // `Str` through `check_str_branch_tail`.
    fn check_str_branch_block(&mut self, block: &Block<'hir>) -> Result<(), ()> {
        if block.safety != Safety::Safe {
            return Err(());
        }
        for stmt in block.stmts {
            self.check_stmt(stmt)?;
        }
        let Some(tail) = block.tail else {
            return Err(());
        };
        self.check_str_branch_tail(tail)
    }

    // `Str`-yielding tail shapes in `if`/`else` branches: a `Str`
    // literal, a `Str` local, a `Str` field projection, or a nested
    // block/`if` whose own tail qualifies. Other shapes report the
    // `Str` mismatch the primary checker would, or stay gated.
    fn check_str_branch_tail(&mut self, expression: &Expr<'hir>) -> Result<(), ()> {
        match expression.kind {
            ExprKind::Lit(Lit::Str(_)) => {
                self.record_expression_type(expression.hir_id, LocalType::Str);
                Ok(())
            }
            ExprKind::Lit(literal) => {
                self.errors.push(PrimitiveBodyError {
                    node: expression.hir_id,
                    kind: PrimitiveBodyErrorKind::ScalarLiteralTypeMismatch {
                        expected: ScalarType::Str,
                        found: scalar_literal_kind(literal),
                    },
                });
                Ok(())
            }
            ExprKind::Path(_) => {
                // A `Str` parameter/local binds in `str_bindings`, not
                // always in `known_bindings` — check the shared set
                // first so `if flag { text } else { "b" }` resolves.
                if self
                    .plain_local_path_binding(expression)
                    .is_some_and(|binding| {
                        self.input
                            .str_bindings
                            .is_some_and(|bindings| bindings.contains(&binding))
                    })
                {
                    self.record_expression_type(expression.hir_id, LocalType::Str);
                    return Ok(());
                }
                match self.check_local_path(expression)? {
                    LocalType::Str => Ok(()),
                    LocalType::Concrete(found) => {
                        self.errors.push(PrimitiveBodyError {
                            node: expression.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                                expected: ScalarType::Str,
                                found: ScalarType::Primitive(found),
                            },
                        });
                        Ok(())
                    }
                    LocalType::LiteralVariable(id) | LocalType::ErrorLiteralVariable(id) => {
                        let Some(variable) = self.literal_variables.get(id) else {
                            return Err(());
                        };
                        match variable.resolved {
                            Some(found) => self.errors.push(PrimitiveBodyError {
                                node: expression.hir_id,
                                kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                                    expected: ScalarType::Str,
                                    found: ScalarType::Primitive(found),
                                },
                            }),
                            None => self.errors.push(PrimitiveBodyError {
                                node: expression.hir_id,
                                kind: PrimitiveBodyErrorKind::ScalarLiteralTypeMismatch {
                                    expected: ScalarType::Str,
                                    found: ScalarLiteralKind::Primitive(variable.kind),
                                },
                            }),
                        }
                        Ok(())
                    }
                    LocalType::Error(_) => Ok(()),
                }
            }
            ExprKind::Field { .. } => match self
                .input
                .str_self_field_projections
                .and_then(|fields| fields.get(&expression.hir_id))
                .copied()
            {
                Some("`Str`") => {
                    self.record_expression_type(expression.hir_id, LocalType::Str);
                    Ok(())
                }
                _ => Err(()),
            },
            ExprKind::Block(block) => self.check_str_branch_block(block),
            ExprKind::If { cond, then, els } => {
                self.check_if_condition(cond)?;
                self.check_str_branch_block(then)?;
                let Some(els) = els else {
                    return Err(());
                };
                self.check_str_branch_tail(els)
            }
            _ => Err(()),
        }
    }

    // Mirror of `check_str_branch_block`'s admission: a safe block
    // whose statements are `let`s/`assert`s the checker handles and
    // whose tail is `Str`-shaped. `let` statements inside the block
    // seed the `Str` set the tail resolves against (`let t = "x"` then
    // `t` as the tail).
    fn str_branch_block_shaped_in(
        &mut self,
        block: &Block<'hir>,
        str_bindings: &HashSet<BindingId>,
    ) -> bool {
        if block.safety != Safety::Safe {
            return false;
        }
        let mut inner = str_bindings.clone();
        for stmt in block.stmts {
            match stmt.kind {
                StmtKind::Let { pat, ty, init, .. } => {
                    let str_annotated = ty.is_some_and(|ty| {
                        self.input
                            .str_annotations
                            .is_some_and(|annotations| annotations.contains(&ty.hir_id))
                    });
                    if let PatKind::Binding { binding, .. } = pat.kind {
                        if str_annotated
                            || (ty.is_none() && self.str_shaped_initializer_in(init, &inner))
                        {
                            inner.insert(binding);
                        }
                    }
                }
                StmtKind::Assert { .. } => {}
                _ => return false,
            }
        }
        block
            .tail
            .is_some_and(|tail| self.str_shaped_initializer_in(tail, &inner))
    }

    // Whether an unannotated initializer yields `Str`: a `Str` literal,
    // a `Str` local, a `Str` field projection, a block/`if` whose
    // branches all do, or a `match` over a `Str` scrutinee whose arms
    // all yield a `Str` bound path. Mirrors the driver's
    // `str_shaped_local_init` so admitted bodies never gate inside.
    fn str_shaped_initializer(&mut self, init: &Expr<'hir>) -> bool {
        match self.input.str_bindings {
            Some(bindings) => self.str_shaped_initializer_in(init, bindings),
            None => self.str_shaped_initializer_in(init, &HashSet::new()),
        }
    }

    fn str_shaped_initializer_in(
        &mut self,
        init: &Expr<'hir>,
        str_bindings: &HashSet<BindingId>,
    ) -> bool {
        match init.kind {
            ExprKind::Lit(Lit::Str(_)) => true,
            // `local_paths` is keyed by the path's id, not the
            // enclosing expression's — resolve the binding first.
            ExprKind::Path(_) => self.plain_local_path_binding(init).is_some_and(|binding| {
                // Inner `let` bindings in branch blocks live in
                // `known_bindings` after `check_stmt` runs, not in
                // the input `str_bindings` set.
                str_bindings.contains(&binding)
                    || matches!(self.known_bindings.get(&binding), Some(LocalType::Str))
            }),
            ExprKind::Field { .. } => matches!(
                self.input
                    .str_self_field_projections
                    .and_then(|fields| fields.get(&init.hir_id))
                    .copied(),
                Some("`Str`")
            ),
            ExprKind::Block(block) => self.str_branch_block_shaped_in(block, str_bindings),
            ExprKind::If { then, els, .. } => {
                self.str_branch_block_shaped_in(then, str_bindings)
                    && els.is_some_and(|els| self.str_shaped_initializer_in(els, str_bindings))
            }
            ExprKind::Match { arms, .. } => {
                !arms.is_empty()
                    && arms.iter().all(|arm| match arm.body.kind {
                        ExprKind::Path(_) => {
                            self.plain_local_path_binding(arm.body)
                                .is_some_and(|binding| {
                                    str_bindings.contains(&binding)
                                        || matches!(
                                            self.known_bindings.get(&binding),
                                            Some(LocalType::Str)
                                        )
                                })
                        }
                        _ => false,
                    })
            }
            _ => false,
        }
    }

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
                // A literal variable produced for a clean operand tree —
                // `(256 + 1) << self.missing` — resolves through the
                // annotation exactly like the primary checker resolves
                // the desugared operand's variable, so out-of-range
                // literals still report instead of hiding behind the
                // field error.
                if let (Some(expected), ExprKind::Binary(operator, left, _)) =
                    (annotation, init.kind)
                {
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
                            | BinOp::BitAnd
                            | BinOp::BitOr
                            | BinOp::BitXor
                            | BinOp::Shl
                            | BinOp::Shr
                    ) && !self.expression_has_unknown_field_error(left)
                    {
                        if let Some(id) = self.unresolved_integer_literal_variable(left) {
                            if literal_family_accepts(PrimitiveLiteralKind::Integer, expected) {
                                self.resolve_literal_variable(id, expected)?;
                            } else {
                                // `let x: Byte = (1 + 2) << self.missing`
                                // reports the annotation mismatch like the
                                // primary checker instead of gating the
                                // whole body behind the field error.
                                self.errors.push(PrimitiveBodyError {
                                    node: init.hir_id,
                                    kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                        expected,
                                        found: PrimitiveLiteralKind::Integer,
                                    },
                                });
                            }
                        }
                    }
                }
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
                ExprKind::Lit(Lit::Str(_)) => {
                    // A `Str` literal is a concrete `Str` value — never a
                    // literal variable. An annotated primitive coercion
                    // site reports the `Str` mismatch like the checker.
                    self.record_expression_type(init.hir_id, LocalType::Str);
                    if let Some(expected) = annotation {
                        self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                                expected: ScalarType::Primitive(expected),
                                found: ScalarType::Str,
                            },
                        });
                        LocalType::Concrete(expected)
                    } else {
                        LocalType::Str
                    }
                }
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
                // Annotated `if`/`match` initializers check each branch/arm
                // body against the annotation through the shared
                // expected-type path. Unannotated forms join the branch/arm
                // literal tails as sites of one shared literal variable —
                // the `var x = 1; x = 2` join — so the local resolves later
                // through the usual tail/annotation path.
                ExprKind::If { cond, then, els } => match annotation {
                    Some(expected) => {
                        self.check_if_tail(cond, then, els, expected)?;
                        LocalType::Concrete(expected)
                    }
                    // `let s = if c { "a" } else { "b" }` binds a `Str`
                    // local; other shapes join the literal group.
                    None if self.str_shaped_initializer(init) => {
                        self.check_str_annotated_initializer(init)?
                    }
                    None => self.check_literal_join_if(cond, then, els)?,
                },
                ExprKind::Match {
                    scrutinee,
                    arms,
                    source,
                } => match annotation {
                    Some(expected) => {
                        self.check_match_tail(scrutinee, arms, source, expected)?;
                        LocalType::Concrete(expected)
                    }
                    None => self.check_literal_join_match(scrutinee, arms, source)?,
                },
                // A `loop` initializer binds `τ = β`: with an annotation
                // `β` is the annotation and each `break v` checks against
                // it; without one `β` is fresh and the local joins `β`'s
                // literal group. `while`/`for` always type `()` and a
                // bare-only `loop` unifies `β` with `()` — neither can
                // bind a primitive local.
                ExprKind::Loop {
                    body,
                    label,
                    source,
                } => {
                    if source != LoopSource::Loop {
                        return Err(());
                    }
                    match annotation {
                        Some(expected) => {
                            self.check_loop_value(
                                init.hir_id,
                                body,
                                label,
                                LoopBreakState::Concrete(expected),
                            )?;
                            LocalType::Concrete(expected)
                        }
                        None => {
                            match self.check_loop_value(
                                init.hir_id,
                                body,
                                label,
                                LoopBreakState::Unfixed,
                            )? {
                                LoopBreakState::Concrete(found) => LocalType::Concrete(found),
                                LoopBreakState::LiteralVariable(id) => {
                                    match self.literal_variable_group_type(id) {
                                        Some(found) => LocalType::Concrete(found),
                                        None => LocalType::LiteralVariable(id),
                                    }
                                }
                                LoopBreakState::Unfixed | LoopBreakState::Bare => return Err(()),
                            }
                        }
                    }
                }
                ExprKind::Field { .. } => {
                    let source_type = if let Some(field_type) = self
                        .input
                        .self_field_projections
                        .and_then(|fields| fields.get(&init.hir_id))
                        .copied()
                    {
                        LocalType::Concrete(field_type)
                    } else {
                        match self
                            .input
                            .str_self_field_projections
                            .and_then(|fields| fields.get(&init.hir_id))
                            .copied()
                        {
                            Some("`Str`") => LocalType::Str,
                            _ => return Err(()),
                        }
                    };
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
                // A `{ ...; e }` block in initializer position checks its
                // statements in scope and its tail like the initializer
                // itself: the annotation (or the unannotated literal
                // machinery) applies to the tail expression directly.
                ExprKind::Block(block) => {
                    if block.safety != Safety::Safe {
                        return Err(());
                    }
                    let Some(inner_tail) = block.tail else {
                        return Err(());
                    };
                    for stmt in block.stmts {
                        self.check_stmt(stmt)?;
                    }
                    let inner = self.check_initializer_value(inner_tail, annotation)?;
                    deferred_neg_result = inner.deferred_neg_result;
                    deferred_bitnot_result = inner.deferred_bitnot_result;
                    inner.local_type
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
        expected: PrimTy,
    ) -> Result<LocalType, ()> {
        let Some(els) = els else {
            return Err(());
        };
        self.check_if_condition(condition)?;
        self.check_if_branch(then, expected)?;
        match els.kind {
            ExprKind::Block(block) => {
                self.check_if_branch(block, expected)?;
                self.record_expression_type(els.hir_id, LocalType::Concrete(expected));
            }
            ExprKind::If { .. } => {
                self.check_expr_expected(els, expected)?;
            }
            _ => return Err(()),
        }
        Ok(LocalType::Concrete(expected))
    }

    // Empty `if` branches are checked tail-first through `check_expr` rather
    // than `check_block`'s empty-statement gate, which deliberately excludes
    // plain `Path` tails (flat path tails belong to the direct-parameter
    // slice). Statement-bearing branches reuse `check_block` directly so
    // their `let` bindings follow the same boundary as the outer block.
    fn check_if_branch(&mut self, block: &'hir Block<'hir>, expected: PrimTy) -> Result<(), ()> {
        if block.stmts.is_empty() {
            if block.safety != Safety::Safe {
                return Err(());
            }
            let Some(tail) = block.tail else {
                return Err(());
            };
            self.check_expr_expected(tail, expected)
        } else {
            self.check_block_expected(block, expected)
        }
    }

    /// `match` tails admit a concrete preceding-path scrutinee and literal,
    /// wildcard, or plain `binding` patterns, with a required catch-all arm —
    /// the bounded exhaustiveness rule of this slice, since every admitted
    /// match is then trivially exhaustive (§6.22). Guards, or/range/compound
    /// patterns, arms after the catch-all (unreachable, warning-gated), and
    /// non-`match` desugarings (`if let`, loops) stay outside. Arm bodies
    /// unify with the return type through `check_expr`, like `if` branches.
    fn check_match_tail(
        &mut self,
        scrutinee: &'hir Expr<'hir>,
        arms: &'hir [Arm<'hir>],
        source: MatchSource,
        expected: PrimTy,
    ) -> Result<LocalType, ()> {
        if !matches!(source, MatchSource::Normal) {
            return Err(());
        }
        let ExprKind::Path(_) = scrutinee.kind else {
            return Err(());
        };
        let LocalType::Concrete(scrutinee_type) = self.check_local_path(scrutinee)? else {
            return Err(());
        };
        let mut catch_all = false;
        // Bool literal coverage: `match flag { true => …, false => … }` is
        // exhaustive without a catch-all (§6.22).
        let mut bool_cover = (false, false);
        let mut arm_binding: Option<(BindingId, Option<LocalType>)> = None;
        for arm in arms {
            if catch_all {
                // An arm after an unguarded catch-all can never match
                // (§6.22.4): diagnose but still check its body — the arm's
                // own pattern binding stays visible to that body.
                self.errors.push(PrimitiveBodyError {
                    node: arm.pat.hir_id,
                    kind: PrimitiveBodyErrorKind::UnreachablePattern,
                });
                if let PatKind::Binding {
                    mode: BindingMode::Value,
                    binding,
                    sub: None,
                    ..
                } = arm.pat.kind
                {
                    arm_binding = Some((
                        binding,
                        self.known_bindings
                            .insert(binding, LocalType::Concrete(scrutinee_type)),
                    ));
                }
                let body_result = self.check_expr_expected(arm.body, expected);
                self.restore_arm_binding(&mut arm_binding);
                body_result?;
                continue;
            }
            self.check_match_arm_pattern(
                arm,
                scrutinee_type,
                &mut catch_all,
                &mut bool_cover,
                &mut arm_binding,
            )?;
            // Arm patterns bind only inside their arm body — restore the
            // previous binding (or its absence) so sibling arms and the
            // enclosing scope never observe the arm-local name. A guard
            // runs inside the same scope with the binding installed.
            let body_result = arm
                .guard
                .map(|guard| self.check_if_condition(guard))
                .unwrap_or(Ok(()))
                .and_then(|_| self.check_expr_expected(arm.body, expected));
            self.restore_arm_binding(&mut arm_binding);
            body_result?;
        }
        if !catch_all && !(scrutinee_type == PrimTy::Bool && bool_cover == (true, true)) {
            self.errors.push(PrimitiveBodyError {
                node: scrutinee.hir_id,
                kind: PrimitiveBodyErrorKind::NonExhaustiveMatch,
            });
        }
        Ok(LocalType::Concrete(expected))
    }

    /// Validates one arm's pattern under the bounded exhaustiveness rule:
    /// literal patterns check against the scrutinee type, wildcard and plain
    /// `binding` patterns mark the catch-all (with the binding installed at
    /// `scrutinee_type`), guards on non-binding arms gate, and Bool literal
    /// patterns record domain coverage in `bool_cover`.
    fn check_match_arm_pattern(
        &mut self,
        arm: &'hir Arm<'hir>,
        scrutinee_type: PrimTy,
        catch_all: &mut bool,
        bool_cover: &mut (bool, bool),
        arm_binding: &mut Option<(BindingId, Option<LocalType>)>,
    ) -> Result<(), ()> {
        match arm.pat.kind {
            PatKind::Lit(literal) => {
                // A guarded arm cannot fail its pattern but its guard may,
                // so it does not count toward Bool-domain coverage.
                if arm.guard.is_none() {
                    if let Lit::Bool(value) = literal {
                        if value {
                            bool_cover.0 = true;
                        } else {
                            bool_cover.1 = true;
                        }
                    }
                }
                self.check_match_literal_pattern(literal, arm.pat.hir_id, scrutinee_type)?
            }
            // `l1 | l2 | …` alternatives must all be literals — a binding or
            // wildcard inside an or-pattern is outside this slice. Diagnosing
            // at the arm pattern's node keeps the taint walk keyed on
            // `arm.pat.hir_id` correct.
            PatKind::Or(alternatives) => {
                if alternatives.is_empty() {
                    return Err(());
                }
                for alternative in alternatives {
                    let PatKind::Lit(literal) = alternative.kind else {
                        return Err(());
                    };
                    if arm.guard.is_none() {
                        if let Lit::Bool(value) = literal {
                            if value {
                                bool_cover.0 = true;
                            } else {
                                bool_cover.1 = true;
                            }
                        }
                    }
                    self.check_match_literal_pattern(literal, arm.pat.hir_id, scrutinee_type)?;
                }
            }
            // `l1..=l2` bounds must both be literals — open ranges and
            // non-literal bounds stay gated. Diagnosing at the arm
            // pattern's node keeps the taint walk keyed on
            // `arm.pat.hir_id` correct.
            PatKind::Range {
                start,
                end,
                inclusive,
            } => {
                for bound in [start, end] {
                    let Some(PatRangeEnd::Lit(literal)) = bound else {
                        return Err(());
                    };
                    self.check_match_literal_pattern(literal, arm.pat.hir_id, scrutinee_type)?;
                }
                // §6.21.3: `a ≤ b` for `..=`, `a < b` for `..` — an empty
                // range is `EMPTY_RANGE_PATTERN`.
                if literal_range_is_empty(&start, &end, inclusive) == Some(true) {
                    self.errors.push(PrimitiveBodyError {
                        node: arm.pat.hir_id,
                        kind: PrimitiveBodyErrorKind::EmptyRangePattern,
                    });
                }
            }
            PatKind::Wild => *catch_all = arm.guard.is_none(),
            PatKind::Binding {
                mode: BindingMode::Value,
                binding,
                sub: None,
                ..
            } => {
                // A guarded binding arm is not a catch-all — the guard may
                // fail, so an unguarded catch-all arm is still required.
                *catch_all = arm.guard.is_none();
                *arm_binding = Some((
                    binding,
                    self.known_bindings
                        .insert(binding, LocalType::Concrete(scrutinee_type)),
                ));
            }
            _ => return Err(()),
        }
        self.record_expression_type(arm.pat.hir_id, LocalType::Concrete(scrutinee_type));
        Ok(())
    }

    fn check_match_literal_pattern(
        &mut self,
        literal: Lit<'hir>,
        node: HirId,
        scrutinee_type: PrimTy,
    ) -> Result<(), ()> {
        let Some(kind) = primitive_literal_kind(literal) else {
            return Err(());
        };
        match check_primitive_literal(
            scrutinee_type,
            literal,
            self.input.symbols,
            self.input.pointer_width,
        ) {
            PrimitiveLiteralResult::Match => {}
            PrimitiveLiteralResult::OutOfRange => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                        expected: scrutinee_type,
                        found: kind,
                    },
                });
            }
            PrimitiveLiteralResult::TypeMismatch => {
                self.errors.push(PrimitiveBodyError {
                    node,
                    kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                        expected: scrutinee_type,
                        found: kind,
                    },
                });
            }
        }
        Ok(())
    }

    fn restore_arm_binding(&mut self, arm_binding: &mut Option<(BindingId, Option<LocalType>)>) {
        if let Some((binding, previous)) = arm_binding.take() {
            match previous {
                Some(previous) => {
                    self.known_bindings.insert(binding, previous);
                }
                None => {
                    self.known_bindings.remove(&binding);
                }
            }
        }
    }

    /// Unannotated `let x = if flag { 1 } else { 2 }` joins both branch
    /// literal tails as sites of one shared literal variable — the
    /// `var x = 1; x = 2` join — so the local resolves later through the
    /// usual tail/annotation path. Only empty branches with a literal tail
    /// are admitted: statement-bearing branches, `else if` chains, and a
    /// missing `else` stay gated.
    fn check_literal_join_if(
        &mut self,
        condition: &'hir Expr<'hir>,
        then: &'hir Block<'hir>,
        els: Option<&'hir Expr<'hir>>,
    ) -> Result<LocalType, ()> {
        let Some(els) = els else {
            return Err(());
        };
        let ExprKind::Block(else_block) = els.kind else {
            return Err(());
        };
        self.check_if_condition(condition)?;
        let mut joined = None;
        for branch in [then, else_block] {
            let tail = Self::literal_join_branch_tail(branch)?;
            self.join_literal_site(tail, &mut joined)?;
        }
        let Some(local) = joined else {
            return Err(());
        };
        Ok(local)
    }

    fn literal_join_branch_tail(block: &'hir Block<'hir>) -> Result<&'hir Expr<'hir>, ()> {
        if block.safety != Safety::Safe || !block.stmts.is_empty() {
            return Err(());
        }
        let Some(tail) = block.tail else {
            return Err(());
        };
        if !matches!(tail.kind, ExprKind::Lit(_)) {
            return Err(());
        }
        Ok(tail)
    }

    /// Unannotated `let x = match value { 0 => 1, _ => 2 }` joins the arm
    /// body literals as sites of one shared literal variable, like the `if`
    /// join. Pattern and scrutinee admission mirrors `check_match_tail`;
    /// every arm body must be a literal tail (a third literal site or a
    /// kind mismatch gates through the shared additional-site machinery).
    fn check_literal_join_match(
        &mut self,
        scrutinee: &'hir Expr<'hir>,
        arms: &'hir [Arm<'hir>],
        source: MatchSource,
    ) -> Result<LocalType, ()> {
        if !matches!(source, MatchSource::Normal) {
            return Err(());
        }
        let ExprKind::Path(_) = scrutinee.kind else {
            return Err(());
        };
        let LocalType::Concrete(scrutinee_type) = self.check_local_path(scrutinee)? else {
            return Err(());
        };
        let mut catch_all = false;
        let mut bool_cover = (false, false);
        let mut arm_binding: Option<(BindingId, Option<LocalType>)> = None;
        let mut joined = None;
        for arm in arms {
            if catch_all {
                self.errors.push(PrimitiveBodyError {
                    node: arm.pat.hir_id,
                    kind: PrimitiveBodyErrorKind::UnreachablePattern,
                });
                if let PatKind::Binding {
                    mode: BindingMode::Value,
                    binding,
                    sub: None,
                    ..
                } = arm.pat.kind
                {
                    arm_binding = Some((
                        binding,
                        self.known_bindings
                            .insert(binding, LocalType::Concrete(scrutinee_type)),
                    ));
                }
                let body_result = self.join_literal_site(arm.body, &mut joined);
                self.restore_arm_binding(&mut arm_binding);
                body_result?;
                continue;
            }
            self.check_match_arm_pattern(
                arm,
                scrutinee_type,
                &mut catch_all,
                &mut bool_cover,
                &mut arm_binding,
            )?;
            let body_result = arm
                .guard
                .map(|guard| self.check_if_condition(guard))
                .unwrap_or(Ok(()))
                .and_then(|_| self.join_literal_site(arm.body, &mut joined));
            self.restore_arm_binding(&mut arm_binding);
            body_result?;
        }
        if !catch_all && !(scrutinee_type == PrimTy::Bool && bool_cover == (true, true)) {
            self.errors.push(PrimitiveBodyError {
                node: scrutinee.hir_id,
                kind: PrimitiveBodyErrorKind::NonExhaustiveMatch,
            });
        }
        let Some(local) = joined else {
            return Err(());
        };
        Ok(local)
    }

    /// Joins one literal tail into the shared literal variable: the first
    /// site creates it, a second site joins through the additional-site
    /// machinery, and further sites merge into the group (kind mismatches
    /// still gate). Concrete literal kinds (`Bool`/`Byte`/`Char`) gate —
    /// branch tails must agree on one concrete type, which the shared
    /// literal-variable machinery does not express yet.
    fn join_literal_site(
        &mut self,
        expression: &'hir Expr<'hir>,
        joined: &mut Option<LocalType>,
    ) -> Result<(), ()> {
        let ExprKind::Lit(literal) = expression.kind else {
            return Err(());
        };
        let Some(kind) = primitive_literal_kind(literal) else {
            return Err(());
        };
        match joined {
            None => {
                match self.local_type_for_unannotated_literal(literal, kind, expression.hir_id) {
                    local @ LocalType::LiteralVariable(_) => *joined = Some(local),
                    _ => return Err(()),
                }
            }
            Some(LocalType::LiteralVariable(id)) => match kind {
                PrimitiveLiteralKind::Integer => {
                    self.add_integer_literal_constraint(*id, literal, expression.hir_id)?;
                }
                PrimitiveLiteralKind::FloatingPoint => {
                    self.add_float_literal_constraint(*id, literal, expression.hir_id)?;
                }
                _ => return Err(()),
            },
            _ => return Err(()),
        }
        Ok(())
    }

    fn check_if_condition(&mut self, condition: &'hir Expr<'hir>) -> Result<(), ()> {
        let local_type = match condition.kind {
            ExprKind::Path(_) => self.check_local_path(condition)?,
            // A known self field in condition position — `if self.flag` —
            // reads its declared type like a local path; the shared tail
            // applies the `Bool` requirement below.
            ExprKind::Field { .. } => LocalType::Concrete(
                self.input
                    .self_field_projections
                    .and_then(|fields| fields.get(&condition.hir_id))
                    .copied()
                    .ok_or(())?,
            ),
            // A `Bool` call in condition position — `if is_ready()` or
            // `if s.is_empty()` — types like the operand-position call.
            ExprKind::Call { .. } => self.check_direct_call(condition, false)?,
            // `not e` conditions admit the same operand-position logical
            // shapes as `let x = not e` — `not not`, `not true`, and nested
            // `and`/`or`/comparison operands recurse in source order.
            ExprKind::Unary(UnOp::Not, operand) => {
                let operand_type = self.check_logical_operand(operand)?;
                self.check_logical_operand_type(operand, operand_type)?;
                LocalType::Concrete(PrimTy::Bool)
            }
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
            LocalType::Str => {
                self.errors.push(PrimitiveBodyError {
                    node: condition.hir_id,
                    kind: PrimitiveBodyErrorKind::ScalarTypeMismatch {
                        expected: ScalarType::Primitive(PrimTy::Bool),
                        found: ScalarType::Str,
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
            // Tainted argument trees (`bump(self.missing + 1)`) recover like
            // the statement-position tainted path: the argument type is
            // `Error`, the parameter unify no-ops, and the field error is
            // kept instead of gating the method.
            if self.expression_has_unknown_field_error(argument.expr) {
                self.check_error_tainted_expression(argument.expr)?;
                // Preserve the error literal variable so the parameter
                // expectation still constrains the tainted literal —
                // `let n = 300 + self.missing; take_u8(n)` must keep the
                // out-of-range diagnostic (mirrors `check_expr_value`).
                let local_type =
                    if let Some(id) = self.error_tainted_literal_variable(argument.expr) {
                        LocalType::ErrorLiteralVariable(id)
                    } else {
                        LocalType::Error(self.known_error_tainted_result_type(argument.expr))
                    };
                self.record_expression_type(argument.expr.hir_id, local_type);
                self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                continue;
            }
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
                // A known self field passes its declared type like a path
                // argument; an unknown field recovers through the tainted
                // leaf path so the `UnknownField` diagnostic survives.
                ExprKind::Field { .. } => {
                    let local_type = if let Some(field_type) = self
                        .input
                        .self_field_projections
                        .and_then(|fields| fields.get(&argument.expr.hir_id))
                        .copied()
                    {
                        LocalType::Concrete(field_type)
                    } else if self.expression_has_unknown_field_error(argument.expr) {
                        self.check_error_tainted_expression(argument.expr)?;
                        LocalType::Error(self.known_error_tainted_result_type(argument.expr))
                    } else {
                        return Err(());
                    };
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Call { .. } if !nested => {
                    let local_type = self.check_direct_call(argument.expr, true)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(BinOp::Rem, left, right)
                    if is_supported_direct_call_remainder_operands(
                        left,
                        right,
                        self.input.self_field_projections,
                    ) =>
                {
                    let local_type = self.check_remainder(left, right, argument.expr.hir_id)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(
                    operator @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div),
                    left,
                    right,
                ) if is_supported_direct_call_arithmetic_operands(
                    left,
                    right,
                    self.input.self_field_projections,
                ) =>
                {
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
                ) if is_supported_direct_call_comparison_operands(
                    left,
                    right,
                    self.input.self_field_projections,
                ) =>
                {
                    let local_type = self.check_comparison(operator, left, right)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(operator @ (BinOp::And | BinOp::Or), left, right)
                    if is_supported_direct_call_logical_operands(
                        left,
                        right,
                        self.input.self_field_projections,
                    ) =>
                {
                    let local_type = self.check_direct_call_logical(operator, left, right)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Binary(operator @ (BinOp::Shl | BinOp::Shr), left, right)
                    if is_supported_direct_call_shift_operands(
                        left,
                        right,
                        self.input.self_field_projections,
                    ) =>
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
                ) if is_supported_direct_call_bitwise_operands(
                    left,
                    right,
                    self.input.self_field_projections,
                ) =>
                {
                    let local_type =
                        self.check_bitwise(operator, left, right, argument.expr.hir_id)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::Not, operand)
                    if is_supported_direct_call_not_operand(
                        operand,
                        self.input.self_field_projections,
                    ) =>
                {
                    let local_type = self.check_direct_call_logical_not(operand)?;
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::BitNot, operand)
                    if is_supported_direct_call_bitwise_not_operand(
                        operand,
                        self.input.self_field_projections,
                    ) =>
                {
                    let local_type = match operand.kind {
                        ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_)) => {
                            self.check_direct_call_bitwise_not_literal(operand, *expected)?
                        }
                        // A field operand desugars to `let t = self.f`, so
                        // the operator check runs on the declared type
                        // without the path deferral guards.
                        ExprKind::Field { .. } => self.check_direct_call_unary_field(
                            operand,
                            UnOp::BitNot,
                            argument.expr.hir_id,
                        )?,
                        _ => self.check_bitwise_not_path(operand, argument.expr.hir_id)?,
                    };
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                ExprKind::Unary(UnOp::Neg, operand)
                    if is_supported_direct_call_neg_operand(
                        operand,
                        self.input.self_field_projections,
                    ) =>
                {
                    let local_type = if matches!(operand.kind, ExprKind::Field { .. }) {
                        self.check_direct_call_unary_field(
                            operand,
                            UnOp::Neg,
                            argument.expr.hir_id,
                        )?
                    } else {
                        self.check_direct_call_neg_path(operand, argument.expr.hir_id)?
                    };
                    self.record_expression_type(argument.expr.hir_id, local_type);
                    self.check_annotated_path(local_type, *expected, argument.expr.hir_id)?;
                }
                // `if`/`match` arguments check against the parameter type
                // through the same branch join the tail position uses —
                // `bump(if c { 1 } else { 2 })` behaves as `bump(1)` when
                // the parameter is `I32`.
                ExprKind::If { .. } | ExprKind::Match { .. } => {
                    self.check_expr_expected(argument.expr, *expected)?;
                    self.record_expression_type(
                        argument.expr.hir_id,
                        LocalType::Concrete(*expected),
                    );
                }
                // A `{ ...; e }` argument checks its inner statements and
                // tail against the parameter type like any other value
                // block — `bump({ 1 })` behaves as `bump(1)`.
                ExprKind::Block(block) => {
                    if block.safety != Safety::Safe {
                        return Err(());
                    }
                    self.check_block_expected(block, *expected)?;
                    self.record_expression_type(
                        argument.expr.hir_id,
                        LocalType::Concrete(*expected),
                    );
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
        // `-self.f` reads the field's declared type — a concrete type, so
        // the literal-variable deferral and binding cycle guard below do
        // not apply.
        if let ExprKind::Field { .. } = operand.kind {
            let operand_type = self
                .input
                .self_field_projections
                .and_then(|fields| fields.get(&operand.hir_id))
                .copied()
                .ok_or(())?;
            self.record_expression_type(operand.hir_id, LocalType::Concrete(operand_type));
            if !is_neg_primitive(operand_type) {
                self.errors.push(PrimitiveBodyError {
                    node: expression,
                    kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                        operator: UnOp::Neg,
                        operand: operand_type,
                    },
                });
            }
            return Ok((LocalType::Concrete(operand_type), false));
        }
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
        // `~self.f` reads the field's declared type — see the `Neg` arm.
        if let ExprKind::Field { .. } = operand.kind {
            let operand_type = self
                .input
                .self_field_projections
                .and_then(|fields| fields.get(&operand.hir_id))
                .copied()
                .ok_or(())?;
            self.record_expression_type(operand.hir_id, LocalType::Concrete(operand_type));
            if !is_bitwise_primitive(operand_type) {
                self.errors.push(PrimitiveBodyError {
                    node: expression,
                    kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                        operator: UnOp::BitNot,
                        operand: operand_type,
                    },
                });
            }
            return Ok((LocalType::Concrete(operand_type), false));
        }
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

    // A `self.f` operand inside `-`/`~` call arguments evaluates through
    // the operand path and applies the operator's primitive check to the
    // declared field type.
    fn check_direct_call_unary_field(
        &mut self,
        operand: &'hir Expr<'hir>,
        operator: UnOp,
        expression: HirId,
    ) -> Result<LocalType, ()> {
        let operand_type = self
            .check_operand_local_type(operand)
            .ok()
            .and_then(|local_type| self.concrete_local_type(local_type))
            .ok_or(())?;
        let supported = match operator {
            UnOp::Neg => is_neg_primitive(operand_type),
            _ => is_bitwise_primitive(operand_type),
        };
        if !supported {
            self.errors.push(PrimitiveBodyError {
                node: expression,
                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                    operator,
                    operand: operand_type,
                },
            });
        }
        Ok(LocalType::Concrete(operand_type))
    }

    fn check_logical_not(&mut self, operand: &'hir Expr<'hir>) -> Result<LocalType, ()> {
        // The shared probe falls back to the literal-variable family
        // default, so `apply_bool(not inferred)` reports the `Bool`
        // mismatch like the initializer positions.
        let operand_type = self.check_logical_operand_probe(operand)?;
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
            ExprKind::Field { .. } => {
                let operand_type = self
                    .check_operand_local_type(operand)
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

        let ExprKind::Binary(operator, left, right) = expression.kind else {
            return None;
        };
        if !self.expression_has_unknown_field_error(right) {
            return None;
        }
        if matches!(operator, BinOp::Shl | BinOp::Shr) {
            // `lit << self.missing` — the clean left subtree was already
            // evaluated as an operand by the recovery walker; reuse its
            // literal variable so the expected type still constrains the
            // literals (a `256` left of a `U8` shift reports
            // `LiteralOutOfRange` instead of defaulting to `I64`).
            return match self.check_operand_local_type(left) {
                Ok(LocalType::LiteralVariable(id)) => Some(id),
                _ => None,
            };
        }
        if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
            return None;
        }
        let ExprKind::Lit(literal) = left.kind else {
            // A nested literal-only subtree (`(256 + 1) + self.missing`)
            // keeps its literal variable the same way.
            return match self.check_operand_local_type(left) {
                Ok(LocalType::LiteralVariable(id)) => Some(id),
                _ => None,
            };
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
            LocalType::Concrete(_)
            | LocalType::Str
            | LocalType::Error(_)
            | LocalType::ErrorLiteralVariable(_) => None,
        }
    }

    fn known_error_tainted_result_type(&self, expression: &Expr<'hir>) -> Option<PrimTy> {
        match expression.kind {
            ExprKind::Path(_) => self.known_recovery_operand_type(expression),
            ExprKind::Unary(UnOp::Not, _) => Some(PrimTy::Bool),
            // `Neg`/`BitNot` both return their operand's type, so a
            // recoverable operand type is the tainted result type.
            ExprKind::Unary(UnOp::Neg | UnOp::BitNot, operand) => {
                self.known_recovery_operand_type(operand)
            }
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
                ExprKind::Unary(operator, operand)
                    if matches!(operator, UnOp::Neg | UnOp::BitNot)
                        && (match operand.kind {
                            ExprKind::Path(_) => {
                                self.check_local_path(operand).is_ok_and(|local_type| {
                                    matches!(
                                        local_type,
                                        LocalType::Error(_) | LocalType::ErrorLiteralVariable(_)
                                    )
                                })
                            }
                            // `~self.missing` recovers like the path form:
                            // the driver reports the unknown field; the
                            // operand type is unrecoverable here, so no
                            // operator check runs.
                            ExprKind::Field { .. } => self
                                .input
                                .unknown_self_fields
                                .is_some_and(|fields| fields.contains(&operand.hir_id)),
                            _ => false,
                        }) =>
                {
                    if self.has_unresolved_error_literal_path(operand) {
                        return Err(());
                    }
                    if let Some(operand_type) = self.known_recovery_operand_type(operand) {
                        let supported = match operator {
                            UnOp::Neg => is_neg_primitive(operand_type),
                            _ => is_bitwise_primitive(operand_type),
                        };
                        if !supported {
                            self.errors.push(PrimitiveBodyError {
                                node: expression.hir_id,
                                kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                                    operator,
                                    operand: operand_type,
                                },
                            });
                        }
                    }
                    Ok(())
                }
                ExprKind::Unary(UnOp::Not, operand)
                    if self.expression_has_unknown_field_error(operand)
                        && self.is_supported_error_tainted_operand(operand) =>
                {
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
        // Evaluate clean non-leaf operands through the desugared `let t =
        // e` path so the shape and diagnostic phases can type them — the
        // result lands in `expression_type_candidates`, which
        // `known_recovery_operand_type` consults. A clean operand the
        // primary checker cannot type gates the whole expression just as
        // it would untainted. Evaluated operands are remembered so the
        // diagnostic walkers below skip them: their leaf errors were
        // already emitted by the real checkers.
        let mut evaluated_clean_operands = HashSet::new();
        for operand in [left, right] {
            if !self.expression_has_unknown_field_error(operand)
                && !matches!(
                    operand.kind,
                    ExprKind::Lit(_) | ExprKind::Path(_) | ExprKind::Field { .. }
                )
                && self.is_supported_error_tainted_operand(operand)
            {
                if self.check_operand_local_type(operand).is_err() {
                    return Err(());
                }
                evaluated_clean_operands.insert(operand.hir_id);
            }
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
                    // A clean operand already evaluated through
                    // `check_operand_local_type` produced its own
                    // diagnostics — walking it again would duplicate
                    // each leaf error. The operand-position `Bool`
                    // requirement is not part of that evaluation, so it
                    // is enforced here from the recorded result type.
                    if evaluated_clean_operands.contains(&operand.hir_id) {
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
                        continue;
                    }
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
                    } else if let Some(id) = self.unresolved_integer_literal_variable(right) {
                        // An evaluated literal-variable count — `1 + 2`
                        // — resolves to `U32` exactly like the primary
                        // shift checker's count position.
                        self.resolve_literal_variable(id, PrimTy::U32)?;
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
        let is_literal = |expression: &Expr<'hir>| {
            matches!(
                expression.kind,
                ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some()
            )
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
                || self
                    .unresolved_integer_literal_variable(expression)
                    .is_some()
        };
        let clean_operand_is_shift_count = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some()
                || literal_kind(expression) == Some(PrimitiveLiteralKind::Integer)
                || self
                    .unresolved_integer_literal_variable(expression)
                    .is_some()
        };
        let clean_operand_is_bitwise = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some() || literal_kind(expression).is_some()
        };
        let clean_operand_is_comparable = |expression: &Expr<'hir>| {
            primitive_path(expression).is_some() || literal_kind(expression).is_some()
        };

        let is_supported_operand =
            |expression: &Expr<'hir>| self.is_supported_error_tainted_operand(expression);
        let shape_is_supported = match operator {
            BinOp::And | BinOp::Or => {
                self.error_tainted_logical_operand_shape(left, &is_supported_operand, &is_literal)
                    && self.error_tainted_logical_operand_shape(
                        right,
                        &is_supported_operand,
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
                (is_supported_operand(left) || supported_left_literal)
                    && (is_supported_operand(right) || supported_right_literal)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                is_supported_operand(left) && is_supported_operand(right)
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
            | BinOp::BitXor => is_supported_operand(left) && is_supported_operand(right),
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

    /// Operand admission inside an error-tainted expression: the
    /// checker-side mirror of the driver's
    /// `primitive_nested_operand_shape_is_supported` — literals, local
    /// paths, `self` fields (known or unknown), `not`/`~`/`-` over leaf
    /// operands, and nested binary trees over any primitive-local binary
    /// operator. Calls stay gated: recovery has no call operand types to
    /// evaluate them with.
    fn is_supported_error_tainted_operand(&self, expression: &Expr<'hir>) -> bool {
        match expression.kind {
            ExprKind::Lit(literal) => primitive_literal_kind(literal).is_some(),
            ExprKind::Path(_) => self.check_local_path(expression).is_ok(),
            ExprKind::Field { .. } => {
                self.input
                    .self_field_projections
                    .is_some_and(|fields| fields.contains_key(&expression.hir_id))
                    || self
                        .input
                        .unknown_self_fields
                        .is_some_and(|fields| fields.contains(&expression.hir_id))
            }
            ExprKind::Unary(UnOp::Not, operand) => self.error_tainted_logical_operand_shape(
                operand,
                &|expression| self.is_supported_error_tainted_operand(expression),
                &|expression| {
                    matches!(
                        expression.kind,
                        ExprKind::Lit(literal) if primitive_literal_kind(literal).is_some()
                    )
                },
            ),
            // Mirrors the driver's unary-operand admission exactly:
            // `-` over a local path or a self field, `~` over the same or
            // an integer/byte literal — known fields check directly,
            // unknown fields recover through the field-error diagnostic.
            ExprKind::Unary(UnOp::Neg, operand) => {
                (matches!(operand.kind, ExprKind::Path(_))
                    && self.check_local_path(operand).is_ok())
                    || (matches!(operand.kind, ExprKind::Field { .. })
                        && (self
                            .input
                            .self_field_projections
                            .is_some_and(|fields| fields.contains_key(&operand.hir_id))
                            || self
                                .input
                                .unknown_self_fields
                                .is_some_and(|fields| fields.contains(&operand.hir_id))))
            }
            ExprKind::Unary(UnOp::BitNot, operand) => match operand.kind {
                ExprKind::Lit(literal) => matches!(
                    primitive_literal_kind(literal),
                    Some(PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte)
                ),
                ExprKind::Path(_) => self.check_local_path(operand).is_ok(),
                ExprKind::Field { .. } => {
                    self.input
                        .self_field_projections
                        .is_some_and(|fields| fields.contains_key(&operand.hir_id))
                        || self
                            .input
                            .unknown_self_fields
                            .is_some_and(|fields| fields.contains(&operand.hir_id))
                }
                _ => false,
            },
            ExprKind::Binary(_, left, right) => {
                self.is_supported_error_tainted_operand(left)
                    && self.is_supported_error_tainted_operand(right)
            }
            _ => false,
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
        // A clean operand already evaluated through
        // `check_operand_local_type` answers from its recorded result
        // type — a comparison is `Bool` even when its leaves keep
        // unresolved literal variables.
        if let Some(operand_type) = self.known_recovery_operand_type(operand) {
            return !matches!(
                primitive_bool_operand_result(operand_type),
                PrimitiveBoolOperandResult::Unsupported
            );
        }
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

    /// The id of an unresolved integer literal variable recorded for an
    /// evaluated operand — e.g. `1 + 2` in shift-count position.
    fn unresolved_integer_literal_variable(&self, expression: &Expr<'hir>) -> Option<usize> {
        let LocalType::LiteralVariable(id) = self
            .expression_type_candidates
            .borrow()
            .get(&expression.hir_id)
            .copied()?
        else {
            return None;
        };
        let variable = self.literal_variables.get(id)?;
        if variable.kind == PrimitiveLiteralKind::Integer && variable.resolved.is_none() {
            Some(id)
        } else {
            None
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
            _ => self
                .expression_type_candidates
                .borrow()
                .get(&expression.hir_id)
                .copied()
                .and_then(|local_type| self.concrete_local_type(local_type)),
        }
    }

    fn check_concrete_tail_type(&mut self, found: PrimTy, expected: PrimTy, node: HirId) {
        if found != expected {
            self.errors.push(PrimitiveBodyError {
                node,
                kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
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
        let Some(variable) = self.literal_variables.get(id) else {
            return Err(());
        };
        if variable.kind != PrimitiveLiteralKind::Integer || variable.resolved.is_some() {
            return Err(());
        }
        if variable.additional_site.is_none() {
            self.literal_variables[id].additional_site = Some(LiteralSite { literal, origin });
            return Ok(());
        }
        // A third and further constraint sites join the group as merge
        // children — `1 + join(2, 3)` and three-or-more-site `match` joins
        // keep every literal in one resolution group.
        let local_type =
            self.local_type_for_unannotated_literal(literal, PrimitiveLiteralKind::Integer, origin);
        let LocalType::LiteralVariable(site) = local_type else {
            return Err(());
        };
        self.merge_literal_variables(id, site)
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
        let Some(variable) = self.literal_variables.get(id) else {
            return Err(());
        };
        if variable.kind != PrimitiveLiteralKind::FloatingPoint || variable.resolved.is_some() {
            return Err(());
        }
        if variable.additional_site.is_none() {
            self.literal_variables[id].additional_site = Some(LiteralSite { literal, origin });
            return Ok(());
        }
        let local_type = self.local_type_for_unannotated_literal(
            literal,
            PrimitiveLiteralKind::FloatingPoint,
            origin,
        );
        let LocalType::LiteralVariable(site) = local_type else {
            return Err(());
        };
        self.merge_literal_variables(id, site)
    }

    /// Links `other` into `id` so both resolve to one type — the merge an
    /// `x + y`-style operand pair needs. Same-kind unresolved variables
    /// only; a variable joins at most one parent and chains resolve
    /// recursively, so merges stay acyclic.
    fn merge_literal_variables(&mut self, id: usize, other: usize) -> Result<(), ()> {
        // Merge children link upward, so both ids are normalized to their
        // group roots first — reusing a merged variable (`x = y; z = y`,
        // `x + y` twice) merges into its group instead of being refused.
        let id = self.literal_variable_group_root(id);
        let other = self.literal_variable_group_root(other);
        if id == other {
            return Ok(());
        }
        let Some(variable) = self.literal_variables.get(id) else {
            return Err(());
        };
        let Some(other_variable) = self.literal_variables.get(other) else {
            return Err(());
        };
        if variable.kind != other_variable.kind
            || variable.resolved.is_some()
            || other_variable.resolved.is_some()
            || other_variable.merged_with.is_some()
        {
            return Err(());
        }
        // `other` may not be an ancestor of `id` — linking it back would
        // close a `a += b; b += a` cycle and infinite-loop resolution.
        let mut cursor = variable.merged_with;
        while let Some(parent) = cursor {
            if parent == other {
                return Err(());
            }
            cursor = self
                .literal_variables
                .get(parent)
                .and_then(|ancestor| ancestor.merged_with);
        }
        self.literal_variables[other].merged_with = Some(id);
        Ok(())
    }

    /// The variable's merge-group root — merge children chain upward.
    fn literal_variable_group_root(&self, mut id: usize) -> usize {
        while let Some(parent) = self
            .literal_variables
            .get(id)
            .and_then(|variable| variable.merged_with)
        {
            id = parent;
        }
        id
    }

    /// The resolved type of the variable's merge group, if any.
    fn literal_variable_group_type(&self, id: usize) -> Option<PrimTy> {
        self.literal_variables
            .get(self.literal_variable_group_root(id))
            .and_then(|variable| variable.resolved)
    }

    fn resolve_literal_variable(&mut self, id: usize, expected: PrimTy) -> Result<(), ()> {
        // A merge child resolves through its parent so the whole group ends
        // up with one type.
        if let Some(parent) = self
            .literal_variables
            .get(id)
            .and_then(|variable| variable.merged_with)
        {
            return self.resolve_literal_variable(parent, expected);
        }
        self.resolve_literal_variable_direct(id, expected)
    }

    /// Resolves `id` itself and every merge child below it — children call
    /// this directly rather than `resolve_literal_variable` so their
    /// `merged_with` redirect does not re-enter the parent resolution.
    fn resolve_literal_variable_direct(&mut self, id: usize, expected: PrimTy) -> Result<(), ()> {
        let symbols = self.input.symbols;
        let pointer_width = self.input.pointer_width;
        let errors = &mut self.errors;
        let Some(variable) = self.literal_variables.get_mut(id) else {
            return Err(());
        };
        if let Some(resolved) = variable.resolved {
            // Re-resolution to the same type is a no-op: an intervening
            // constraint site (e.g. a call argument inside `x + id(x)`)
            // may already have fixed the group to the demanded type.
            return if resolved == expected {
                Ok(())
            } else {
                Err(())
            };
        }
        if !literal_family_accepts(variable.kind, expected) {
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
        // A merged variable resolves to its parent's type; chains are
        // shallow so recursion is fine.
        let children: Vec<usize> = (0..self.literal_variables.len())
            .filter(|index| self.literal_variables[*index].merged_with == Some(id))
            .collect();
        for child in children {
            self.resolve_literal_variable_direct(child, expected)?;
        }
        Ok(())
    }

    fn default_unresolved_literals(&mut self, literal_floor: usize) {
        let symbols = self.input.symbols;
        let pointer_width = self.input.pointer_width;
        let mut deferred_errors = Vec::new();
        let mut deferred_operator_errors = Vec::new();
        let variable_count = self.literal_variables.len();
        // Literal variables carried as an enclosing loop's `β` outlive the
        // inner statement blocks they were written in — a `break 1` in one
        // `if` branch must stay unresolved until the loop itself ends so a
        // later `break 2` can still merge into it. The loop's own frame is
        // popped before its floor defaults, so each `β` lands exactly at
        // its loop's end.
        let loop_break_variables: HashSet<usize> = self
            .loop_stack
            .iter()
            .filter_map(|frame| match frame.break_state {
                LoopBreakState::LiteralVariable(id) => Some(id),
                _ => None,
            })
            .collect();
        for index in literal_floor..variable_count {
            // Merge children are handled through their group's root — the
            // root may come after a child (the right operand of `x + y`
            // merges under the left), so a per-index visit would either
            // default a child alone (leaving its parent unresolved) or
            // skip its deferred diagnostics when the group resolves at a
            // later index.
            if self.literal_variables[index].merged_with.is_some() {
                continue;
            }
            let mut group = vec![index];
            let mut cursor = 0;
            while cursor < group.len() {
                let parent = group[cursor];
                cursor += 1;
                group.extend(
                    (0..variable_count)
                        .filter(|i| self.literal_variables[*i].merged_with == Some(parent)),
                );
            }
            group.sort_unstable();
            if group
                .iter()
                .any(|member| loop_break_variables.contains(member))
            {
                continue;
            }
            if self.literal_variables[index].resolved.is_none() {
                // A group straddling the floor stays unresolved — the
                // member below the floor may still be constrained by a
                // later site (a loop-local `n += x` must not default
                // the outer `x`).
                if group.iter().any(|member| *member < literal_floor) {
                    continue;
                }
                for member in &group {
                    let variable = &mut self.literal_variables[*member];
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
            }
            for member in group {
                // Group members below this block's literal floor emit
                // their deferred diagnostics in the enclosing block's
                // pass, which visits the group again.
                if member < literal_floor {
                    continue;
                }
                let variable = &mut self.literal_variables[member];
                if let Some(right) = variable.resolved {
                    deferred_operator_errors.extend(variable.pending_operator_traits.iter().map(
                        |site| {
                            (
                                site.diagnostic_index,
                                PrimitiveBodyError {
                                    node: site.expression,
                                    kind:
                                        PrimitiveBodyErrorKind::DeferredOperatorTraitNotSupported {
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

    // A binary operand that is not a plain path evaluates like the
    // desugared `let t = e` binding its subtree stands for.
    fn check_operand_local_type(&mut self, expression: &'hir Expr<'hir>) -> Result<LocalType, ()> {
        // Literal operands stay on the literal-specific branches: an
        // `Err` here is how those branches tell literals from paths.
        if matches!(expression.kind, ExprKind::Lit(_)) {
            return Err(());
        }
        match self.check_local_path(expression) {
            Ok(local_type) => Ok(local_type),
            Err(()) => {
                // A nested operand can be reached through several operand
                // checks (e.g. once per comparison loop iteration); reuse
                // the recorded type so a literal group created by the first
                // evaluation is not orphaned by a second one.
                if let Some(local_type) = self
                    .expression_type_candidates
                    .borrow()
                    .get(&expression.hir_id)
                    .copied()
                {
                    return Ok(local_type);
                }
                let local_type = self
                    .check_initializer_value(expression, None)
                    .map(|check| check.local_type)?;
                self.record_expression_type(expression.hir_id, local_type);
                Ok(local_type)
            }
        }
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
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            collect_primitive_expression_nodes(scrutinee, nodes);
            for arm in arms {
                if let Some(guard) = arm.guard {
                    collect_primitive_expression_nodes(guard, nodes);
                }
                collect_primitive_expression_nodes(arm.body, nodes);
            }
        }
        ExprKind::Loop { body, .. } => collect_primitive_block_nodes(body, nodes),
        ExprKind::Break {
            value: Some(value), ..
        } => collect_primitive_expression_nodes(value, nodes),
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
            // Loop bodies carry the taint of their statements and exits —
            // an errored `break` value must taint the result like a body
            // error does.
            ExprKind::Loop { body, .. } => block_has_primitive_result_taint(
                body,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ),
            ExprKind::Break {
                value: Some(value), ..
            } => expression_has_primitive_result_taint(
                value,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ),
            // An error on a pattern, guard, scrutinee, or arm body taints the
            // whole `match` result — otherwise the recorded tail type stays
            // published as trustworthy.
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                expression_has_primitive_result_taint(
                    scrutinee,
                    error_nodes,
                    unknown_self_fields,
                    local_paths,
                    tainted_bindings,
                ) || arms.iter().any(|arm| {
                    error_nodes.contains(&arm.pat.hir_id)
                        || arm.guard.is_some_and(|guard| {
                            expression_has_primitive_result_taint(
                                guard,
                                error_nodes,
                                unknown_self_fields,
                                local_paths,
                                tainted_bindings,
                            )
                        })
                        || expression_has_primitive_result_taint(
                            arm.body,
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
        StmtKind::Assign { lhs, rhs } | StmtKind::AssignOp { lhs, rhs, .. } => {
            expression_has_primitive_result_taint(
                lhs,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            ) || expression_has_primitive_result_taint(
                rhs,
                error_nodes,
                unknown_self_fields,
                local_paths,
                tainted_bindings,
            )
        }
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

/// The expression a block initializer contributes for provenance: a value
/// block `{ ...; tail }` behaves as its tail for literal-local, pair, and
/// alias tracking, so `let x = { 1 }` and `let x = { let y = 1\n  y }`
/// register the same way `let x = 1` and `let x = y` do.
fn initializer_provenance<'hir>(init: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    let mut expr = init;
    while let ExprKind::Block(block) = expr.kind {
        let Some(tail) = block.tail else {
            break;
        };
        expr = tail;
    }
    expr
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

/// `Some(is_empty)` when both literal bounds are comparable same-kind
/// literals; `None` leaves non-comparable bound pairs to the per-literal
/// type checks that already ran.
pub fn literal_range_is_empty<'hir>(
    start: &Option<PatRangeEnd<'hir>>,
    end: &Option<PatRangeEnd<'hir>>,
    inclusive: bool,
) -> Option<bool> {
    let (Some(start_bound), Some(end_bound)) = (start, end) else {
        return None;
    };
    let (
        PatRangeEnd::Lit(Lit::Int {
            value: Some(start_value),
            negated: start_negated,
        }),
        PatRangeEnd::Lit(Lit::Int {
            value: Some(end_value),
            negated: end_negated,
        }),
    ) = (start_bound, end_bound)
    else {
        let (PatRangeEnd::Lit(start_literal), PatRangeEnd::Lit(end_literal)) =
            (start_bound, end_bound)
        else {
            return None;
        };
        let (start_value, end_value) = match (start_literal, end_literal) {
            (Lit::Char(start_value), Lit::Char(end_value)) => {
                (*start_value as u128, *end_value as u128)
            }
            (Lit::Byte(start_value), Lit::Byte(end_value)) => {
                (*start_value as u128, *end_value as u128)
            }
            _ => return None,
        };
        return Some(if inclusive {
            start_value > end_value
        } else {
            start_value >= end_value
        });
    };
    // Compare sign-aware without materializing an `i128`: a negated bound
    // sorts before any non-negated bound, and among negated bounds the
    // larger magnitude is the smaller value. A `u128` magnitude can exceed
    // `i128::MAX` (`-2^127`), so `-(value as i128)` would overflow. `-0`
    // equals `0`, so a zero magnitude is never treated as negated.
    let start_negated = *start_negated && *start_value != 0;
    let end_negated = *end_negated && *end_value != 0;
    let ordering = match (start_negated, end_negated) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) => end_value.cmp(start_value),
        (false, false) => start_value.cmp(end_value),
    };
    Some(if inclusive {
        ordering == std::cmp::Ordering::Greater
    } else {
        ordering != std::cmp::Ordering::Less
    })
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

/// The family default `β` resolves to when no site fixes it — used to
/// name `β` in a `v ⇐ β` mismatch diagnostic.
fn literal_variable_default_type(kind: PrimitiveLiteralKind) -> PrimTy {
    match kind {
        PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
        // Only integer and floating-point literals become variables.
        _ => PrimTy::I64,
    }
}

/// The literal family a concrete primitive belongs to, for `found` in a
/// `v ⇐ β` mismatch diagnostic.
fn concrete_literal_family(primitive: PrimTy) -> PrimitiveLiteralKind {
    match primitive {
        PrimTy::Bool => PrimitiveLiteralKind::Bool,
        PrimTy::Byte => PrimitiveLiteralKind::Byte,
        PrimTy::Char => PrimitiveLiteralKind::Character,
        PrimTy::F32 | PrimTy::F64 => PrimitiveLiteralKind::FloatingPoint,
        _ => PrimitiveLiteralKind::Integer,
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

/// Every literal family a `Str`-position mismatch can name — total, unlike
/// `primitive_literal_kind`, which excludes the `Str`/`Slice<Byte>` literals.
fn scalar_literal_kind(literal: Lit<'_>) -> ScalarLiteralKind {
    match literal {
        Lit::Int { .. } => ScalarLiteralKind::Primitive(PrimitiveLiteralKind::Integer),
        Lit::Float { .. } => ScalarLiteralKind::Primitive(PrimitiveLiteralKind::FloatingPoint),
        Lit::Bool(_) => ScalarLiteralKind::Primitive(PrimitiveLiteralKind::Bool),
        Lit::Byte(_) => ScalarLiteralKind::Primitive(PrimitiveLiteralKind::Byte),
        Lit::Char(_) => ScalarLiteralKind::Primitive(PrimitiveLiteralKind::Character),
        Lit::Str(_) => ScalarLiteralKind::Str,
        Lit::ByteStr(_) => ScalarLiteralKind::ByteStr,
    }
}

fn is_supported_direct_call_arithmetic_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_numeric_literal_or_path = |expression: &Expr<'_>| match expression.kind {
        ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) | ExprKind::Path(_) => true,
        ExprKind::Field { .. } => is_direct_call_known_self_field(expression, fields),
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

fn is_supported_direct_call_remainder_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_path_like = |expression: &Expr<'_>| {
        matches!(expression.kind, ExprKind::Path(_))
            || is_direct_call_known_self_field(expression, fields)
    };
    match (left.kind, right.kind) {
        _ if is_path_like(left) && is_path_like(right) => true,
        _ if is_path_like(left) && matches!(right.kind, ExprKind::Lit(Lit::Int { .. })) => true,
        _ if is_path_like(right) && matches!(left.kind, ExprKind::Lit(Lit::Int { .. })) => true,
        (ExprKind::Lit(Lit::Int { .. }), ExprKind::Lit(Lit::Int { .. })) => true,
        _ => false,
    }
}

fn is_supported_direct_call_comparison_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_path_like = |expression: &Expr<'_>| {
        matches!(expression.kind, ExprKind::Path(_))
            || is_direct_call_known_self_field(expression, fields)
    };
    let is_primitive_literal_or_path = |expression: &Expr<'_>| match expression.kind {
        ExprKind::Lit(literal) => primitive_literal_kind(literal).is_some(),
        _ => is_path_like(expression),
    };
    if !is_primitive_literal_or_path(left) || !is_primitive_literal_or_path(right) {
        return false;
    }

    // The bounded comparison form needs a concrete local type to constrain a
    // possible literal. Literal/literal comparisons have no such witness.
    is_path_like(left) || is_path_like(right)
}

fn is_supported_direct_call_logical_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_path_or_bool_literal = |operand: &Expr<'_>| {
        matches!(
            operand.kind,
            ExprKind::Path(_) | ExprKind::Lit(Lit::Bool(_))
        ) || is_direct_call_known_self_field(operand, fields)
    };
    is_path_or_bool_literal(left) && is_path_or_bool_literal(right)
}

fn is_supported_direct_call_shift_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_path_like = |expression: &Expr<'_>| {
        matches!(expression.kind, ExprKind::Path(_))
            || is_direct_call_known_self_field(expression, fields)
    };
    match (left.kind, right.kind) {
        _ if is_path_like(left)
            && (is_path_like(right)
                || matches!(
                    right.kind,
                    ExprKind::Lit(Lit::Byte(_))
                        | ExprKind::Lit(
                            Lit::Int { .. } | Lit::Bool(_) | Lit::Char(_) | Lit::Float { .. }
                        )
                )) =>
        {
            true
        }
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
        // A literal left operand also admits a known-field right operand —
        // the field desugars to a path leaf for the shift-count slot.
        _ if matches!(left.kind, ExprKind::Lit(Lit::Byte(_) | Lit::Int { .. }))
            && is_direct_call_known_self_field(right, fields) =>
        {
            true
        }
        _ => false,
    }
}

fn is_supported_direct_call_bitwise_operands(
    left: &Expr<'_>,
    right: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    let is_path_like = |expression: &Expr<'_>| {
        matches!(expression.kind, ExprKind::Path(_))
            || is_direct_call_known_self_field(expression, fields)
    };
    (is_path_like(left)
        && (is_path_like(right)
            || matches!(right.kind, ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_)))))
        || (matches!(left.kind, ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_)))
            && is_path_like(right))
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

// A `self.f` leaf inside a call argument desugars to `let t = self.f`, so
// it is admissible wherever a path operand is — only known (projected)
// fields qualify; unknown fields keep the tainted boundary.
fn is_direct_call_known_self_field(
    expression: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    matches!(expression.kind, ExprKind::Field { .. })
        && fields.is_some_and(|fields| fields.contains_key(&expression.hir_id))
}

fn is_supported_direct_call_not_operand(
    operand: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    matches!(
        operand.kind,
        ExprKind::Path(_) | ExprKind::Lit(Lit::Bool(_))
    ) || is_direct_call_known_self_field(operand, fields)
}

fn is_supported_direct_call_bitwise_not_operand(
    operand: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    matches!(
        operand.kind,
        ExprKind::Path(_) | ExprKind::Lit(Lit::Int { .. } | Lit::Byte(_))
    ) || is_direct_call_known_self_field(operand, fields)
}

fn is_supported_direct_call_neg_operand(
    operand: &Expr<'_>,
    fields: Option<&HashMap<HirId, PrimTy>>,
) -> bool {
    matches!(operand.kind, ExprKind::Path(_)) || is_direct_call_known_self_field(operand, fields)
}

fn primitive_shift_left_literal_supported(kind: PrimitiveLiteralKind) -> bool {
    matches!(
        kind,
        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte
    )
}
