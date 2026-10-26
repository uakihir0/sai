//! Sparse, owner-scoped facts produced by bounded body-checking slices.
//!
//! These results are not the complete `TypeckResults` contract described by
//! the compiler implementation plan. They retain only concrete primitive
//! facts that a supported checker has established.

use sai_hir::{BindingId, HirId, OwnerId};
use sai_resolve::prelude::PrimTy;
use std::collections::HashMap;

/// A HIR node supplied to a result builder belongs to another owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultOwnerMismatch {
    pub expected: OwnerId,
    pub found: OwnerId,
}

/// One owner's sparse primitive type facts.
///
/// `expression_types` is keyed by the exact owner-bearing HIR identity.
/// `binding_types` is keyed by `BindingId`, which does not carry an owner;
/// `binding_nodes` retains the exact HIR identity that witnessed each
/// binding insertion.
///
/// This is intentionally not the complete `TypeckResults`: it has no
/// canonical `Ty`, no complete `node_types` table, and no type-dependent
/// resolution tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialTypeckResults {
    owner: OwnerId,
    expression_types: HashMap<HirId, PrimTy>,
    binding_types: HashMap<BindingId, PrimTy>,
    binding_nodes: HashMap<BindingId, HirId>,
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
}
