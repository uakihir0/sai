//! Narrow, compositional checks for directly annotated function-body locals.

use super::literal::{PrimitiveLiteralResult, check_primitive_literal, supports_primitive_literal};
use sai_hir::body::Body;
use sai_hir::expressions::{Block, Expr, ExprKind, Stmt, StmtKind};
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
/// inserts only direct built-in primitive type paths and local path resolutions
/// from the current owner analysis.
pub struct PrimitiveBodyInput<'a, 'hir> {
    pub body: &'a Body<'hir>,
    pub return_type: PrimTy,
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

/// Scalar literal classes supported by the primitive literal kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveLiteralKind {
    Integer,
    FloatingPoint,
    Bool,
    Byte,
    Character,
}

/// Check one or more immutable, annotated primitive locals followed by a tail
/// read.
///
/// Statements and expressions are checked in order. The local becomes
/// available to the tail expression only after its initializer has been
/// checked. Other valid body forms remain unsupported until their own
/// type-checking slices are implemented.
pub fn check_annotated_primitive_local_body(
    input: PrimitiveBodyInput<'_, '_>,
) -> PrimitiveBodyCheck {
    if !supports_primitive_literal(input.return_type) {
        return PrimitiveBodyCheck::Unsupported;
    }

    let ExprKind::Block(block) = input.body.value.kind else {
        return PrimitiveBodyCheck::Unsupported;
    };

    let mut context = FnCtxt {
        input,
        known_bindings: HashMap::new(),
        errors: Vec::new(),
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
    known_bindings: HashMap<BindingId, PrimTy>,
    errors: Vec<PrimitiveBodyError>,
}

impl FnCtxt<'_, '_> {
    fn check_block(&mut self, block: &Block<'_>) -> Result<(), ()> {
        if block.safety != Safety::Safe || block.stmts.is_empty() {
            return Err(());
        }

        for stmt in block.stmts {
            self.check_stmt(stmt)?;
        }

        let Some(tail) = block.tail else {
            return Err(());
        };
        self.check_expr(tail)
    }

    fn check_stmt(&mut self, stmt: &Stmt<'_>) -> Result<(), ()> {
        let StmtKind::Let {
            pat,
            ty: Some(ty),
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

        let Some(primitive) = self.input.primitive_types.get(&ty.hir_id).copied() else {
            return Err(());
        };
        if !supports_primitive_literal(primitive) {
            return Err(());
        }

        match init.kind {
            ExprKind::Lit(literal) => {
                let Some(found) = primitive_literal_kind(literal) else {
                    return Err(());
                };
                match check_primitive_literal(
                    primitive,
                    literal,
                    self.input.symbols,
                    self.input.pointer_width,
                ) {
                    PrimitiveLiteralResult::Match => {}
                    PrimitiveLiteralResult::TypeMismatch => {
                        self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralTypeMismatch {
                                expected: primitive,
                                found,
                            },
                        });
                    }
                    PrimitiveLiteralResult::OutOfRange => {
                        self.errors.push(PrimitiveBodyError {
                            node: init.hir_id,
                            kind: PrimitiveBodyErrorKind::LiteralOutOfRange {
                                expected: primitive,
                                found,
                            },
                        });
                    }
                }
            }
            ExprKind::Path(_) => {
                let found = self.check_local_path(init)?;
                if found != primitive {
                    self.errors.push(PrimitiveBodyError {
                        node: init.hir_id,
                        kind: PrimitiveBodyErrorKind::TypeMismatch {
                            expected: primitive,
                            found,
                        },
                    });
                }
            }
            _ => return Err(()),
        }

        self.known_bindings.insert(binding, primitive);
        Ok(())
    }

    fn check_expr(&mut self, expression: &Expr<'_>) -> Result<(), ()> {
        let found = self.check_local_path(expression)?;
        if found != self.input.return_type {
            self.errors.push(PrimitiveBodyError {
                node: expression.hir_id,
                kind: PrimitiveBodyErrorKind::TypeMismatch {
                    expected: self.input.return_type,
                    found,
                },
            });
        }
        Ok(())
    }

    fn check_local_path(&self, expression: &Expr<'_>) -> Result<PrimTy, ()> {
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
        let Some(primitive) = self.known_bindings.get(&binding).copied() else {
            return Err(());
        };
        Ok(primitive)
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
