//! Sparse, owner-scoped facts produced by bounded body-checking slices.
//!
//! These results are not the complete `TypeckResults` contract described by
//! the compiler implementation plan. They retain only concrete primitive
//! facts that a supported checker has established.

use crate::ty::{Ty, TyInterner};
use sai_hir::{BindingId, HirId, OwnerId};
use sai_resolve::prelude::PrimTy;
use std::collections::HashMap;

/// A HIR node supplied to a result builder belongs to another owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultOwnerMismatch {
    pub expected: OwnerId,
    pub found: OwnerId,
}

/// One owner's sparse primitive and canonical primitive type facts.
///
/// `expression_types` is keyed by the exact owner-bearing HIR identity.
/// `binding_types` is keyed by `BindingId`, which does not carry an owner;
/// `binding_nodes` retains the exact HIR identity that witnessed each
/// binding insertion. Canonical type maps are sparse mirrors derived only
/// from those already-filtered primitive facts with a caller-supplied shared
/// [`TyInterner`]; an unsupported primitive conversion remains absent.
///
/// This is intentionally not the complete `TypeckResults`: it has no
/// complete `node_types` table or type-dependent resolution tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialTypeckResults {
    owner: OwnerId,
    expression_types: HashMap<HirId, PrimTy>,
    binding_types: HashMap<BindingId, PrimTy>,
    binding_nodes: HashMap<BindingId, HirId>,
    canonical_expression_types: HashMap<HirId, Ty>,
    canonical_binding_types: HashMap<BindingId, Ty>,
}

impl PartialTypeckResults {
    pub const fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn expression_type(&self, expression: HirId) -> Option<PrimTy> {
        self.expression_types.get(&expression).copied()
    }

    pub fn binding_type(&self, binding: BindingId) -> Option<PrimTy> {
        self.binding_types.get(&binding).copied()
    }

    pub fn expression_types(&self) -> &HashMap<HirId, PrimTy> {
        &self.expression_types
    }

    pub fn binding_types(&self) -> &HashMap<BindingId, PrimTy> {
        &self.binding_types
    }

    /// Exact owner-bearing HIR node associated with a binding fact.
    pub fn binding_node(&self, binding: BindingId) -> Option<HirId> {
        self.binding_nodes.get(&binding).copied()
    }

    /// Owner-bearing HIR nodes associated with the sparse binding facts.
    pub fn binding_nodes(&self) -> &HashMap<BindingId, HirId> {
        &self.binding_nodes
    }

    /// Canonical `Ty` for one expression with a supported primitive fact.
    pub fn canonical_expression_type(&self, expression: HirId) -> Option<&Ty> {
        self.canonical_expression_types.get(&expression)
    }

    /// Canonical `Ty` for one binding with a supported primitive fact.
    pub fn canonical_binding_type(&self, binding: BindingId) -> Option<&Ty> {
        self.canonical_binding_types.get(&binding)
    }

    /// Sparse canonical expression facts derived from the primitive map.
    pub fn canonical_expression_types(&self) -> &HashMap<HirId, Ty> {
        &self.canonical_expression_types
    }

    /// Sparse canonical binding facts derived from the primitive map.
    pub fn canonical_binding_types(&self) -> &HashMap<BindingId, Ty> {
        &self.canonical_binding_types
    }

    /// Derive canonical `Ty` facts for supported primitive facts using the
    /// caller's session-wide interner.
    ///
    /// This runs after the body checker has filtered tainted facts. It never
    /// creates a fact absent from the primitive maps, and conversion failures
    /// are omitted without manufacturing an error type.
    pub fn with_canonical_primitive_types(mut self, interner: &TyInterner) -> Self {
        self.canonical_expression_types = self
            .expression_types
            .iter()
            .filter_map(|(expression, primitive)| {
                if expression.owner != self.owner {
                    return None;
                }
                let ty = crate::lower_ty::intern_primitive_ty(interner, *primitive).ok()?;
                Some((*expression, ty))
            })
            .collect();
        self.canonical_binding_types = self
            .binding_types
            .iter()
            .filter_map(|(binding, primitive)| {
                let binding_node = self.binding_nodes.get(binding)?;
                if binding_node.owner != self.owner {
                    return None;
                }
                let ty = crate::lower_ty::intern_primitive_ty(interner, *primitive).ok()?;
                Some((*binding, ty))
            })
            .collect();
        self
    }
}

/// Builds sparse primitive results for one HIR body owner.
///
/// Every expression and binding insertion must include an exact `HirId`.
/// This lets the builder reject facts from a different owner even though
/// `BindingId` itself is only a per-owner index. The caller remains
/// responsible for pairing a `BindingId` with the HIR node of that exact
/// parameter or binding pattern.
#[derive(Debug)]
pub struct PartialTypeckResultsBuilder {
    owner: OwnerId,
    expression_types: HashMap<HirId, PrimTy>,
    binding_types: HashMap<BindingId, PrimTy>,
    binding_nodes: HashMap<BindingId, HirId>,
}

impl PartialTypeckResultsBuilder {
    pub fn new(owner: OwnerId) -> Self {
        Self {
            owner,
            expression_types: HashMap::new(),
            binding_types: HashMap::new(),
            binding_nodes: HashMap::new(),
        }
    }

    pub const fn owner(&self) -> OwnerId {
        self.owner
    }

    /// Record a fact for an exact expression HIR ID.
    ///
    /// A duplicate insertion for the same HIR node replaces the prior fact,
    /// matching the body's existing candidate-map behavior.
    pub fn record_expression_type(
        &mut self,
        expression: HirId,
        ty: PrimTy,
    ) -> Result<(), ResultOwnerMismatch> {
        self.check_owner(expression)?;
        self.expression_types.insert(expression, ty);
        Ok(())
    }

    /// Record a fact for a binding and the exact parameter/pattern HIR node
    /// that declares it.
    ///
    /// `BindingId` carries no owner, so the caller must supply its HIR witness.
    /// The builder verifies that witness belongs to this owner and rejects a
    /// second, conflicting HIR node for the same binding index.
    pub fn record_binding_type(
        &mut self,
        binding: BindingId,
        binding_node: HirId,
        ty: PrimTy,
    ) -> Result<(), PartialTypeckResultsBuildError> {
        self.check_owner(binding_node)
            .map_err(PartialTypeckResultsBuildError::OwnerMismatch)?;
        if let Some(previous) = self.binding_nodes.get(&binding)
            && *previous != binding_node
        {
            return Err(PartialTypeckResultsBuildError::BindingNodeMismatch {
                binding,
                previous: *previous,
                found: binding_node,
            });
        }
        self.binding_nodes.insert(binding, binding_node);
        self.binding_types.insert(binding, ty);
        Ok(())
    }

    pub fn finish(self) -> PartialTypeckResults {
        PartialTypeckResults {
            owner: self.owner,
            expression_types: self.expression_types,
            binding_types: self.binding_types,
            binding_nodes: self.binding_nodes,
            canonical_expression_types: HashMap::new(),
            canonical_binding_types: HashMap::new(),
        }
    }

    fn check_owner(&self, hir_id: HirId) -> Result<(), ResultOwnerMismatch> {
        if hir_id.owner == self.owner {
            Ok(())
        } else {
            Err(ResultOwnerMismatch {
                expected: self.owner,
                found: hir_id.owner,
            })
        }
    }
}

/// A binding index was paired with more than one HIR declaration node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartialTypeckResultsBuildError {
    OwnerMismatch(ResultOwnerMismatch),
    BindingNodeMismatch {
        binding: BindingId,
        previous: HirId,
        found: HirId,
    },
}

#[cfg(test)]
mod tests {
    use super::{PartialTypeckResultsBuildError, PartialTypeckResultsBuilder, ResultOwnerMismatch};
    use crate::ty::TyInterner;
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, OwnerId,
    };
    use sai_intern::{BindingId, Idx, Interner, ItemLocalId, PackageInterner};
    use sai_resolve::prelude::PrimTy;

    fn owners() -> (OwnerId, OwnerId) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("path interner");
        let owner = |name: &str| {
            let symbol = symbols.intern(name).expect("symbol interns");
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
            OwnerId {
                def_id: paths.intern(path).expect("definition interns"),
            }
        };
        (owner("first"), owner("second"))
    }

    fn hir_id(owner: OwnerId, local_id: u32) -> HirId {
        HirId {
            owner,
            local_id: ItemLocalId::from_raw(local_id),
        }
    }

    #[test]
    fn builder_keeps_sparse_facts_for_its_exact_owner() {
        let (owner, other_owner) = owners();
        let expression = hir_id(owner, 1);
        let binding_node = hir_id(owner, 2);
        let foreign_expression = hir_id(other_owner, 1);
        let foreign_binding_node = hir_id(other_owner, 2);
        let binding = BindingId::default();
        let mut builder = PartialTypeckResultsBuilder::new(owner);

        assert_eq!(
            builder.record_expression_type(foreign_expression, PrimTy::Bool),
            Err(ResultOwnerMismatch {
                expected: owner,
                found: other_owner,
            })
        );
        assert_eq!(
            builder.record_binding_type(binding, foreign_binding_node, PrimTy::I32),
            Err(PartialTypeckResultsBuildError::OwnerMismatch(
                ResultOwnerMismatch {
                    expected: owner,
                    found: other_owner,
                }
            ))
        );

        builder
            .record_expression_type(expression, PrimTy::I16)
            .expect("same-owner expression is accepted");
        builder
            .record_binding_type(binding, binding_node, PrimTy::I16)
            .expect("same-owner binding witness is accepted");
        let results = builder.finish();

        assert_eq!(results.owner(), owner);
        assert_eq!(results.expression_type(expression), Some(PrimTy::I16));
        assert_eq!(results.expression_type(foreign_expression), None);
        assert_eq!(results.binding_type(binding), Some(PrimTy::I16));
        assert_eq!(results.binding_node(binding), Some(binding_node));
        assert_eq!(results.expression_types().len(), 1);
        assert_eq!(results.binding_types().len(), 1);
        assert_eq!(results.binding_nodes().len(), 1);
    }

    #[test]
    fn builder_rejects_conflicting_hir_witnesses_for_one_binding_id() {
        let (owner, _) = owners();
        let binding = BindingId::default();
        let first_node = hir_id(owner, 1);
        let second_node = hir_id(owner, 2);
        let mut builder = PartialTypeckResultsBuilder::new(owner);
        builder
            .record_binding_type(binding, first_node, PrimTy::I16)
            .expect("first binding fact is accepted");

        assert_eq!(
            builder.record_binding_type(binding, second_node, PrimTy::I32),
            Err(PartialTypeckResultsBuildError::BindingNodeMismatch {
                binding,
                previous: first_node,
                found: second_node,
            })
        );
    }

    #[test]
    fn canonical_primitive_projection_is_sparse_and_uses_the_supplied_interner() {
        use crate::ty::TyKind;

        let (owner, other_owner) = owners();
        let interner = TyInterner::new();
        let same_i32 = hir_id(owner, 1);
        let repeated_i32 = hir_id(owner, 2);
        let byte = hir_id(owner, 3);
        let u8 = hir_id(owner, 4);
        let isize = hir_id(owner, 5);
        let array = hir_id(owner, 6);
        let i32_binding = BindingId::from_raw(0);
        let array_binding = BindingId::from_raw(1);
        let i32_binding_node = hir_id(owner, 7);
        let array_binding_node = hir_id(owner, 8);
        let mut builder = PartialTypeckResultsBuilder::new(owner);
        for (expression, primitive) in [
            (same_i32, PrimTy::I32),
            (repeated_i32, PrimTy::I32),
            (byte, PrimTy::Byte),
            (u8, PrimTy::U8),
            (isize, PrimTy::ISize),
            (array, PrimTy::Array),
        ] {
            builder
                .record_expression_type(expression, primitive)
                .expect("same-owner primitive fact is accepted");
        }
        builder
            .record_binding_type(i32_binding, i32_binding_node, PrimTy::I32)
            .expect("binding keeps its exact declaration witness");
        builder
            .record_binding_type(array_binding, array_binding_node, PrimTy::Array)
            .expect("unsupported conversion remains a primitive fact");
        let results = builder.finish().with_canonical_primitive_types(&interner);

        assert_eq!(results.expression_types().len(), 6);
        assert_eq!(results.canonical_expression_types().len(), 5);
        assert_eq!(results.binding_types().len(), 2);
        assert_eq!(results.canonical_binding_types().len(), 1);
        assert_eq!(
            results.canonical_expression_type(array),
            None,
            "unsupported primitive conversions stay absent from canonical facts"
        );
        assert_eq!(results.canonical_binding_type(array_binding), None);
        assert_eq!(results.binding_node(i32_binding), Some(i32_binding_node));
        assert_eq!(
            results.canonical_binding_type(i32_binding).unwrap().kind(),
            &TyKind::Int(crate::ty::IntTy::I32)
        );
        assert!(
            results
                .canonical_expression_type(same_i32)
                .unwrap()
                .ptr_eq(results.canonical_expression_type(repeated_i32).unwrap()),
            "equal primitive facts share a canonical handle"
        );
        assert!(matches!(
            results.canonical_expression_type(byte).unwrap().kind(),
            TyKind::Byte
        ));
        assert!(matches!(
            results.canonical_expression_type(u8).unwrap().kind(),
            TyKind::Uint(crate::ty::UintTy::U8)
        ));
        assert!(
            !results
                .canonical_expression_type(byte)
                .unwrap()
                .ptr_eq(results.canonical_expression_type(u8).unwrap())
        );
        assert!(matches!(
            results.canonical_expression_type(isize).unwrap().kind(),
            TyKind::Int(crate::ty::IntTy::ISize)
        ));

        let other_expression = hir_id(other_owner, 1);
        let mut other_builder = PartialTypeckResultsBuilder::new(other_owner);
        other_builder
            .record_expression_type(other_expression, PrimTy::I32)
            .expect("second owner keeps its own exact HIR ID");
        let other_results = other_builder
            .finish()
            .with_canonical_primitive_types(&interner);
        assert!(
            results.canonical_expression_type(same_i32).unwrap().ptr_eq(
                other_results
                    .canonical_expression_type(other_expression)
                    .unwrap()
            ),
            "different owners use the same supplied session interner"
        );
        assert!(
            results
                .canonical_expression_types()
                .keys()
                .all(|expression| expression.owner == owner)
        );
        assert!(
            results.canonical_binding_types().contains_key(&i32_binding),
            "canonical binding facts remain paired with the exact witnessed binding"
        );
    }
}
