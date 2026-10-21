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
    OperatorNotSupported {
        operator: BinOp,
        operand: PrimitiveOperatorOperand,
    },
    OperatorTraitNotSupported {
        operator: BinOp,
        left: PrimTy,
        right: PrimTy,
    },
    UnaryOperatorNotSupported {
        operator: UnOp,
        operand: PrimTy,
    },
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
/// Direct numeric literals are accepted on either side of `+`, `-`, `*`, and
/// `/` when paired with an eligible concrete numeric path. `%` accepts integer
/// paths and direct integer literals; float `%` returns an operator diagnostic.
/// Unary `~` accepts direct integer/Byte literals and concrete integer/Byte paths.
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
                    let result = self.check_shift(operator, left, right, init.hir_id)?;
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
                    let operand_type = self
                        .check_local_path(operand)
                        .ok()
                        .and_then(|local_type| self.concrete_local_type(local_type))
                        .filter(|primitive| supports_primitive_literal(*primitive))
                        .ok_or(())?;
                    if !is_bitwise_primitive(operand_type) {
                        self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::UnaryOperatorNotSupported {
                                operator: UnOp::BitNot,
                                operand: operand_type,
                            },
                        });
                    }
                    if let Some(expected) = annotation {
                        if expected != operand_type {
                            self.errors.push(PrimitiveBodyError {
                                node: init.hir_id,
                                kind: PrimitiveBodyErrorKind::TypeMismatch {
                                    expected,
                                    found: operand_type,
                                },
                            });
                        }
                        LocalType::Concrete(expected)
                    } else {
                        LocalType::Concrete(operand_type)
                    }
                }
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
                return Err(());
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

                    // Keep Char's bitwise rule gated until operator-trait inference is
                    // available for an integer literal right operand.
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
                    if left_type != PrimTy::Bool
                        && left_type != PrimTy::Byte
                        && !is_integer_primitive(left_type)
                    {
                        return Err(());
                    }
                    self.check_annotated_literal(left_type, literal, kind, right.hir_id);
                    if left_type == PrimTy::Bool {
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
            if primitive_literal_kind(literal) != Some(PrimitiveLiteralKind::Integer) {
                return Err(());
            }
            self.check_annotated_literal(
                PrimTy::U32,
                literal,
                PrimitiveLiteralKind::Integer,
                right.hir_id,
            );
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

const fn is_bitwise_primitive(primitive: PrimTy) -> bool {
    is_integer_primitive(primitive) || matches!(primitive, PrimTy::Byte)
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
        PrimitiveLiteralKind, primitive_arithmetic_literal_expected_type,
        primitive_arithmetic_literal_left_result, primitive_arithmetic_result,
        primitive_bitwise_byte_literal_left_result, primitive_bitwise_byte_literal_pair_result,
        primitive_bool_operand_result, primitive_comparison_literal_expected_type,
        primitive_comparison_result, primitive_shift_left_literal_supported,
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

fn primitive_shift_left_literal_supported(kind: PrimitiveLiteralKind) -> bool {
    matches!(
        kind,
        PrimitiveLiteralKind::Integer | PrimitiveLiteralKind::Byte
    )
}
