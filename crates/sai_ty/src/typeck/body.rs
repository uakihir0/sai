//! Narrow, compositional checks for flat primitive-local function bodies.

use super::literal::{
    PrimitiveLiteralResult, check_primitive_literal, is_integer_primitive,
    supports_primitive_literal,
};
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
/// Direct scalar literals and plain paths to earlier locals are accepted as
/// initializers. Unannotated numeric literals retain one inference variable
/// across copies until a coercion site or the tail constrains it. Other body
/// forms remain unsupported until their own type-checking slices are
/// implemented.
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
            _ => return Err(()),
        };

        self.known_bindings.insert(binding, local_type);
        Ok(())
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

fn literal_family_accepts(kind: PrimitiveLiteralKind, expected: PrimTy) -> bool {
    match kind {
        PrimitiveLiteralKind::Integer => is_integer_primitive(expected),
        PrimitiveLiteralKind::FloatingPoint => matches!(expected, PrimTy::F32 | PrimTy::F64),
        PrimitiveLiteralKind::Bool
        | PrimitiveLiteralKind::Byte
        | PrimitiveLiteralKind::Character => false,
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
