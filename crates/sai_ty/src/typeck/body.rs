//! Narrow, compositional checks for flat primitive-local function bodies.

use super::literal::{
    PrimitiveLiteralResult, check_primitive_literal, is_integer_primitive,
    supports_primitive_literal,
};
use sai_hir::body::Body;
use sai_hir::expressions::{BinOp, Block, Expr, ExprKind, Stmt, StmtKind, UnOp};
use sai_hir::literals::Lit;
use sai_hir::patterns::{BindingMode, PatKind};
use sai_hir::types::PathRoot;
use sai_hir::{BindingId, HirId, Safety};
use sai_intern::Interner;
use sai_resolve::prelude::PrimTy;
use std::collections::HashMap;

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
    pub local_paths: &'a HashMap<HirId, BindingId>,
    pub symbols: &'a Interner,
    pub pointer_width: u8,
}

/// Result of checking a function body in the annotated-local slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrimitiveBodyCheck {
    /// The owner or expression shape is outside this slice.
    Unsupported,
    /// The shape is supported; errors, if any, are anchored by HIR node IDs.
    Supported { errors: Vec<PrimitiveBodyError> },
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
}

#[derive(Clone, Copy, Debug)]
struct LiteralVariable<'hir> {
    literal: Lit<'hir>,
    kind: PrimitiveLiteralKind,
    origin: HirId,
    resolved: Option<PrimTy>,
}

/// Check a flat sequence of immutable primitive locals and a local-path tail.
///
/// Direct scalar literals, plain paths to earlier bindings, comparisons
/// between concrete primitive paths or one path and one scalar literal, one
/// top-level logical operation between Bool paths, and direct arithmetic
/// between matching concrete numeric paths are accepted as initializers. A
/// Direct numeric literals are accepted on either side of `+`, `-`, and `*`
/// when paired with an eligible concrete numeric path.
/// Unannotated numeric literals retain one inference variable across copies
/// until a coercion site or the tail constrains it. Other body forms remain
/// unsupported until their own type-checking slices are implemented.
pub fn check_primitive_local_block_body(input: PrimitiveBodyInput<'_, '_>) -> PrimitiveBodyCheck {
    if !supports_primitive_literal(input.return_type) {
        return PrimitiveBodyCheck::Unsupported;
    }
    let known_bindings = input
        .initial_bindings
        .iter()
        .map(|(binding, ty)| (*binding, LocalType::Concrete(*ty)))
        .collect();
    let mut context = FnCtxt {
        input,
        known_bindings,
        literal_variables: Vec::new(),
        errors: Vec::new(),
    };
    let ExprKind::Block(block) = context.input.body.value.kind else {
        return PrimitiveBodyCheck::Unsupported;
    };
    if context.check_block(block).is_err() {
        return PrimitiveBodyCheck::Unsupported;
    }
    PrimitiveBodyCheck::Supported {
        errors: context.errors,
    }
}

struct FnCtxt<'a, 'hir> {
    input: PrimitiveBodyInput<'a, 'hir>,
    known_bindings: HashMap<BindingId, LocalType>,
    literal_variables: Vec<LiteralVariable<'hir>>,
    errors: Vec<PrimitiveBodyError>,
}

impl<'a, 'hir> FnCtxt<'a, 'hir> {
    fn check_block(&mut self, block: &Block<'hir>) -> Result<(), ()> {
        if block.safety != Safety::Safe || block.stmts.is_empty() {
            return Err(());
        }

        for stmt in block.stmts {
            self.check_stmt(stmt)?;
        }

        let Some(tail) = block.tail else {
            return Err(());
        };
        self.check_expr(tail)?;
        self.default_unresolved_literals();
        Ok(())
    }

    fn check_stmt(&mut self, stmt: &Stmt<'hir>) -> Result<(), ()> {
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
            mode: BindingMode::Value,
            binding,
            sub: None,
            ..
        } = pat.kind
        else {
            return Err(());
        };

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

        let local_type = match init.kind {
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
            ExprKind::Binary(operator, left, right) => {
                if matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                    let result = self.check_arithmetic(operator, left, right)?;
                    if let Some(expected) = annotation {
                        self.check_annotated_path(result, expected, init.hir_id)?;
                        // The annotation determines the local's recovered
                        // type after the coercion-site mismatch, as it does
                        // for the existing annotated path initializer.
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
            _ => return Err(()),
        };

        self.known_bindings.insert(binding, local_type);
        Ok(())
    }

    fn check_arithmetic(
        &mut self,
        operator: BinOp,
        left: &'hir Expr<'hir>,
        right: &'hir Expr<'hir>,
    ) -> Result<LocalType, ()> {
        if let ExprKind::Lit(literal) = left.kind {
            let Some(kind) = primitive_literal_kind(literal) else {
                return Err(());
            };
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
                self.errors.push(PrimitiveBodyError {
                    node: path.hir_id,
                    kind: PrimitiveBodyErrorKind::TypeMismatch {
                        expected,
                        found: path_type,
                    },
                });
                // Even when a left numeric literal cannot unify with the
                // path's family, its default type can independently fail
                // range checking. Keep that diagnostic on the literal.
                self.check_annotated_literal(
                    expected,
                    literal,
                    literal_kind,
                    literal_expression.hir_id,
                );
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
        }

        // Both `and` and `or` have Bool result type, including after an
        // operand mismatch, so downstream local and tail checks can recover.
        Ok(LocalType::Concrete(PrimTy::Bool))
    }

    fn concrete_local_type(&self, local_type: LocalType) -> Option<PrimTy> {
        let primitive = match local_type {
            LocalType::Concrete(primitive) => primitive,
            LocalType::LiteralVariable(id) => self.literal_variables.get(id)?.resolved?,
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
            PrimitiveLiteralResult::Match => {}
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
        match kind {
            PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::FloatingPoint => {
                let id = self.literal_variables.len();
                self.literal_variables.push(LiteralVariable {
                    literal,
                    kind,
                    origin,
                    resolved: None,
                });
                LocalType::LiteralVariable(id)
            }
            PrimitiveLiteralKind::Bool => LocalType::Concrete(PrimTy::Bool),
            PrimitiveLiteralKind::Byte => LocalType::Concrete(PrimTy::Byte),
            PrimitiveLiteralKind::Character => LocalType::Concrete(PrimTy::Char),
        }
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
            LocalType::LiteralVariable(id) => {
                let Some(variable) = self.literal_variables.get(id).copied() else {
                    return Err(());
                };
                match variable.resolved {
                    Some(found) if found != expected => {
                        self.errors.push(PrimitiveBodyError {
                            node,
                            kind: PrimitiveBodyErrorKind::TypeMismatch { expected, found },
                        });
                    }
                    Some(_) => {}
                    None if literal_family_accepts(variable.kind, expected) => {
                        self.resolve_literal_variable(id, expected)?;
                    }
                    None => {
                        self.errors.push(PrimitiveBodyError {
                            node,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected,
                                found: variable.kind,
                            },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn check_expr(&mut self, expression: &Expr<'hir>) -> Result<(), ()> {
        let local_type = self.check_local_path(expression)?;
        match local_type {
            LocalType::Concrete(found) => {
                self.check_concrete_tail_type(found, expression.hir_id);
            }
            LocalType::LiteralVariable(id) => {
                let Some(variable) = self.literal_variables.get(id).copied() else {
                    return Err(());
                };
                match variable.resolved {
                    Some(found) => self.check_concrete_tail_type(found, expression.hir_id),
                    None if literal_family_accepts(variable.kind, self.input.return_type) => {
                        self.resolve_literal_variable(id, self.input.return_type)?;
                    }
                    None => {
                        self.errors.push(PrimitiveBodyError {
                            node: expression.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected: self.input.return_type,
                                found: variable.kind,
                            },
                        });
                    }
                }
            }
        }
        Ok(())
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

    fn resolve_literal_variable(&mut self, id: usize, expected: PrimTy) -> Result<(), ()> {
        let Some(variable) = self.literal_variables.get_mut(id) else {
            return Err(());
        };
        if variable.resolved.is_some() || !literal_family_accepts(variable.kind, expected) {
            return Err(());
        }
        variable.resolved = Some(expected);
        if check_primitive_literal(
            expected,
            variable.literal,
            self.input.symbols,
            self.input.pointer_width,
        ) == PrimitiveLiteralResult::OutOfRange
        {
            self.errors.push(PrimitiveBodyError {
                node: variable.origin,
                kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                    expected,
                    found: variable.kind,
                },
            });
        }
        Ok(())
    }

    fn default_unresolved_literals(&mut self) {
        for id in 0..self.literal_variables.len() {
            let variable = self.literal_variables[id];
            if variable.resolved.is_some() {
                continue;
            }
            let expected = match variable.kind {
                PrimitiveLiteralKind::Integer => PrimTy::I64,
                PrimitiveLiteralKind::FloatingPoint => PrimTy::F64,
                PrimitiveLiteralKind::Bool
                | PrimitiveLiteralKind::Byte
                | PrimitiveLiteralKind::Character => unreachable!(),
            };
            self.literal_variables[id].resolved = Some(expected);
            if check_primitive_literal(
                expected,
                variable.literal,
                self.input.symbols,
                self.input.pointer_width,
            ) == PrimitiveLiteralResult::OutOfRange
            {
                self.errors.push(PrimitiveBodyError {
                    node: variable.origin,
                    kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                        expected,
                        found: variable.kind,
                    },
                });
            }
        }
    }

    fn check_local_path(&self, expression: &Expr<'hir>) -> Result<LocalType, ()> {
        let ExprKind::Path(path) = expression.kind else {
            return Err(());
        };
        if path.root != PathRoot::Plain
            || path.segments.len() != 1
            || path.segments[0].args.is_some()
        {
            return Err(());
        }

        let Some(binding) = self.input.local_paths.get(&path.hir_id).copied() else {
            return Err(());
        };
        let Some(local_type) = self.known_bindings.get(&binding).copied() else {
            return Err(());
        };
        Ok(local_type)
    }
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
    if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul)
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

const fn is_arithmetic_primitive(primitive: PrimTy) -> bool {
    is_integer_primitive(primitive) || matches!(primitive, PrimTy::F32 | PrimTy::F64)
}

fn primitive_arithmetic_literal_expected_type(
    operator: BinOp,
    left: PrimTy,
    literal: PrimitiveLiteralKind,
) -> Option<PrimTy> {
    (matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul)
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
    if !matches!(operator, BinOp::Add | BinOp::Sub | BinOp::Mul)
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
        PrimitiveLiteralKind, primitive_arithmetic_literal_expected_type,
        primitive_arithmetic_literal_left_result, primitive_arithmetic_result,
        primitive_bool_operand_result, primitive_comparison_literal_expected_type,
        primitive_comparison_result,
    };
    use sai_hir::expressions::BinOp;
    use sai_resolve::prelude::PrimTy;

    const ARITHMETIC_OPERATORS: [BinOp; 3] = [BinOp::Add, BinOp::Sub, BinOp::Mul];

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
        for unsupported in [BinOp::Div, BinOp::Rem, BinOp::And, BinOp::Eq] {
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
        for operator in [BinOp::Div, BinOp::Rem, BinOp::And, BinOp::Eq] {
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
        for operator in [BinOp::Div, BinOp::Rem, BinOp::And, BinOp::Eq] {
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
