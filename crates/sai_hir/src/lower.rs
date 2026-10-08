use crate::Symbol;
use crate::arena::HirArena;
use crate::body::{Body, BodyParam, Contract, ContractKind, FnParam, FnSig};
use crate::expressions::{
    Arg, Arm as HirArm, BinOp, Block as HirBlock, CaptureBy, Closure, ClosureParam, Expr, ExprKind,
    FmtPiece, FmtSpec as HirFmtSpec, Label as HirLabel, LoopSource as HirLoopSource,
    MatchSource as HirMatchSource, Stmt as HirStmt, StmtKind, UnOp,
};
use crate::item_tree::{ItemTree, ItemTreeError, ItemTreeKind, RawAttr, RawAttrArg};
use crate::items::{FieldDef, HirOwner, Item, ItemKind, OwnerNode};
use crate::literals::Lit;
use crate::patterns::{BindingMode as HirBindingMode, Pat, PatArg, PatKind, PatRangeEnd};
use crate::source_map::{HirSourceMap, HirSourceMapBuilder, HirSourceMapError};
use crate::types::{
    Bound as HirBound, BoundKind as HirBoundKind, ConstArg as HirConstArg,
    EffectRef as HirEffectRef, FnTraitKind, GenericArg as HirGenericArg,
    GenericArgs as HirGenericArgs, GenericParam as HirGenericParam,
    GenericParamKind as HirGenericParamKind, Generics as HirGenerics, LangItem, Name, Path,
    PathRoot, PathSegment, Provenance, Ty as HirTy, TyKind as HirTyKind, WherePred as HirWherePred,
};
use crate::{
    BindingId, DefPath, DefPathData, DefPathInterner, DefPathInternerError, DesugaringKind,
    LocalDefId, Mutability, OwnerId, Safety, Span, SpanCtxt,
};
use sai_diagnostics::Code;
use sai_intern::{InternError, Interner};
use sai_syntax::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxNodePtr, SyntaxToken, TokenAtOffset,
    ast::{self, AstNode, AstToken},
};
use std::collections::{HashMap, HashSet};

/// Error returned when lowering cannot allocate source data or encounters a
/// syntax form whose HIR conversion has not been implemented yet.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirLoweringError {
    SourceMap(HirSourceMapError),
    Interner(InternError),
    DefPath(DefPathInternerError),
    DefPathInternerRequired,
    ItemTree(ItemTreeError),
    ItemTreeEntryMismatch,
    UnsupportedSyntax(SyntaxKind),
}

/// Inputs shared by owner-level lowering for one parsed workspace snapshot.
pub struct HirLoweringContext<'source, 'hir> {
    ast_ids: &'source ast::AstIdMap,
    snapshot: &'source sai_vfs::WorkspaceSnapshot,
    root: &'source SyntaxNode,
    arena: &'hir HirArena<'hir>,
    symbols: &'source Interner,
    def_paths: &'source DefPathInterner,
}

impl<'source, 'hir> HirLoweringContext<'source, 'hir> {
    pub fn new(
        ast_ids: &'source ast::AstIdMap,
        snapshot: &'source sai_vfs::WorkspaceSnapshot,
        root: &'source SyntaxNode,
        arena: &'hir HirArena<'hir>,
        symbols: &'source Interner,
        def_paths: &'source DefPathInterner,
    ) -> Self {
        Self {
            ast_ids,
            snapshot,
            root,
            arena,
            symbols,
            def_paths,
        }
    }
}

/// Lower a function declaration's signature and body within its HIR owner.
///
/// The source-map builder must be rooted at `declaration`. An async function
/// with a body, or any function whose contracts or body contain closures or
/// async blocks, requires the same definition-path interner that interned the
/// owner. Function bodies and contract expressions share the owner's HIR and
/// binding-ID spaces. This compatibility entry point preserves its historical
/// behavior and does not lower generic bounds or `where` predicates; use
/// `lower_function_with_generics` when the owning item needs generic HIR.
pub fn lower_function<'hir>(
    declaration: &ast::FnDecl,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(FnSig<'hir>, Option<&'hir Body<'hir>>), HirLoweringError> {
    let (signature, _, body) =
        lower_function_inner(declaration, arena, symbols, source_map, false)?;
    Ok((signature, body))
}

/// Lower a function declaration's signature, generics, and body in one owner.
///
/// Generic parameter bounds are lowered before the signature, `where`
/// predicates after the return type, and the remaining clauses in written
/// order, so HIR IDs follow source order. Async functions with bodies use the
/// Stage 5 synthetic async-block wrapper.
pub fn lower_function_with_generics<'hir>(
    declaration: &ast::FnDecl,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<
    (
        FnSig<'hir>,
        &'hir HirGenerics<'hir>,
        Option<&'hir Body<'hir>>,
    ),
    HirLoweringError,
> {
    lower_function_inner(declaration, arena, symbols, source_map, true)
}

/// Lower one top-level function entry from the current item tree into a
/// complete HIR owner and its source map.
///
/// `expected_path` must be the complete path derived for this exact item,
/// including its enclosing path and sibling disambiguator. The resolved owner
/// must match it exactly and end in this item's value-namespace symbol. The
/// item tree, AST ID map, snapshot, and root must all describe the same file
/// and snapshot.
pub fn lower_function_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Function, context)?;
    let function = ast::FnDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(function.syntax());
    validate_current_item_metadata(entry, function.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::ValueNs(entry.name),
        context,
    )?;

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        function.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        function.name(),
        function.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let (sig, generics, body) =
        lower_function_with_generics(&function, context.arena, context.symbols, &mut source_map)?;
    let span = source_map.span_for(function.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Fn {
            sig,
            generics: *generics,
            has_body: function.body().is_some(),
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body,
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level type alias from the current item tree.
///
/// Generic parameter definitions are interned below the complete type-item
/// path. A diagnosed missing alias type recovers as `TyKind::Err`; refined
/// aliases and record declarations are handled by their own item kinds.
pub fn lower_alias_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Alias, context)?;
    let declaration = ast::TypeDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::TypeNs(entry.name),
        context,
    )?;
    if declaration.struct_def().is_some() {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let alias_definition = declaration
        .alias_def()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    if alias_definition.refinement().is_some() {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    ensure_supported_generic_params(declaration.generic_params())?;
    let alias_type = alias_definition.ty();
    if let Some(ty) = &alias_type {
        ensure_supported_type(ty)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let pending_generic_params =
        prepare_generic_params(declaration.generic_params(), context.symbols, &source_map)?;
    let generic_params = lower_generic_params(
        pending_generic_params,
        context.arena,
        context.symbols,
        &mut source_map,
    )?;
    let ty = match alias_type {
        Some(ty) => lower_type(&ty, context.arena, context.symbols, &mut source_map)?,
        None => {
            let error_syntax = direct_error_child(alias_definition.syntax())
                .or_else(|| direct_error_child(declaration.syntax()))
                .unwrap_or_else(|| declaration.syntax().clone());
            lower_error_type(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let generics = context.arena.alloc_generics(HirGenerics {
        params: context.arena.alloc_generic_params(&generic_params),
        where_preds: &[],
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Alias {
            generics: *generics,
            ty,
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: None,
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level refined type from the current item tree.
///
/// The base type is stored on `ItemKind::Refined`; the predicate is the body
/// value and sees `self` through the refined owner's semantic context. Generic
/// parameter syntax remains in ItemTree for `GENERIC_REFINED_TYPE` validation,
/// because refined HIR has no generic-parameter field.
pub fn lower_refined_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Refined, context)?;
    let declaration = ast::TypeDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::TypeNs(entry.name),
        context,
    )?;
    if declaration.struct_def().is_some() {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let definition = declaration
        .alias_def()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let refinement = definition
        .refinement()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let base_type = definition.ty();
    if let Some(base_type) = &base_type {
        ensure_supported_type(base_type)?;
    }
    let predicate = refinement.expression();
    if let Some(predicate) = &predicate {
        ensure_supported_expr(predicate)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    prepare_nested_definition_ids(declaration.syntax(), &mut source_map)?;

    let base = match base_type {
        Some(base_type) => lower_type(&base_type, context.arena, context.symbols, &mut source_map)?,
        None => {
            let error_syntax = direct_error_child(definition.syntax())
                .unwrap_or_else(|| definition.syntax().clone());
            lower_error_type(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let value = match predicate {
        Some(predicate) => lower_expr_in_prepared_owner(
            &predicate,
            context.arena,
            context.symbols,
            &mut source_map,
        )?,
        None => {
            let error_syntax = direct_error_child(refinement.syntax())
                .unwrap_or_else(|| refinement.syntax().clone());
            lower_error_expr(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let body = context.arena.alloc_body(Body {
        owner,
        params: &[],
        value,
        contracts: &[],
        invariant_preds: context.arena.alloc_expr_refs(&[]),
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Refined { base },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: Some(body),
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level distinct type from the current item tree.
///
/// A refinement predicate, when present, becomes the body value and sees
/// `self` through the distinct owner's semantic context.
pub fn lower_distinct_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Distinct, context)?;
    let declaration =
        ast::DistinctDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::TypeNs(entry.name),
        context,
    )?;
    let base_type = declaration.base_type();
    if let Some(base_type) = &base_type {
        ensure_supported_type(base_type)?;
    }
    ensure_supported_generic_params(declaration.generic_params())?;
    let refinement = declaration.refinement();
    let has_predicate = refinement.is_some();
    let predicate = refinement.as_ref().and_then(|clause| clause.expression());
    if let Some(predicate) = &predicate {
        ensure_supported_expr(predicate)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let pending_generic_params =
        prepare_generic_params(declaration.generic_params(), context.symbols, &source_map)?;
    if refinement.is_some() {
        prepare_nested_definition_ids(declaration.syntax(), &mut source_map)?;
    }
    let generic_params = lower_generic_params(
        pending_generic_params,
        context.arena,
        context.symbols,
        &mut source_map,
    )?;
    let generics = context.arena.alloc_generics(HirGenerics {
        params: context.arena.alloc_generic_params(&generic_params),
        where_preds: &[],
    });
    let base = match base_type {
        Some(base_type) => lower_type(&base_type, context.arena, context.symbols, &mut source_map)?,
        None => {
            let error_syntax = direct_error_child(declaration.syntax())
                .unwrap_or_else(|| declaration.syntax().clone());
            lower_error_type(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let body = match (refinement, predicate) {
        (Some(_), Some(predicate)) => {
            let value = lower_expr_in_prepared_owner(
                &predicate,
                context.arena,
                context.symbols,
                &mut source_map,
            )?;
            Some(context.arena.alloc_body(Body {
                owner,
                params: &[],
                value,
                contracts: &[],
                invariant_preds: context.arena.alloc_expr_refs(&[]),
            }))
        }
        (Some(refinement), None) => {
            let error_syntax = direct_error_child(refinement.syntax())
                .unwrap_or_else(|| refinement.syntax().clone());
            let value = lower_error_expr(&error_syntax, context.arena, &mut source_map)?;
            Some(context.arena.alloc_body(Body {
                owner,
                params: &[],
                value,
                contracts: &[],
                invariant_preds: context.arena.alloc_expr_refs(&[]),
            }))
        }
        (None, None) => None,
        (None, Some(_)) => return Err(HirLoweringError::ItemTreeEntryMismatch),
    };
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Distinct {
            generics: *generics,
            pub_ctor: declaration.inner_pub_token().is_some(),
            base,
            has_predicate,
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body,
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level record declaration from the current item tree.
///
/// Generic parameter definitions and field definitions are interned below the
/// complete type-item path. Diagnosed missing field types recover as
/// `TyKind::Err`; the record itself has no body.
pub fn lower_struct_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Struct, context)?;
    let declaration = ast::TypeDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::TypeNs(entry.name),
        context,
    )?;
    if declaration.alias_def().is_some() {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let definition = declaration
        .struct_def()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let fields = definition
        .fields()
        .map(|list| list.fields().collect::<Vec<_>>())
        .unwrap_or_default();
    ensure_supported_generic_params(declaration.generic_params())?;
    if let Some(where_clause) = definition.where_clause() {
        ensure_supported_where_clause(&where_clause)?;
    }
    for field in &fields {
        if let Some(ty) = field.ty() {
            ensure_supported_type(&ty)?;
        }
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let pending_generic_params =
        prepare_generic_params(declaration.generic_params(), context.symbols, &source_map)?;
    let pending_fields = prepare_field_defs(fields, context.symbols, &source_map)?;
    let generic_params = lower_generic_params(
        pending_generic_params,
        context.arena,
        context.symbols,
        &mut source_map,
    )?;
    let where_preds = definition
        .where_clause()
        .map(|clause| lower_where_clause(&clause, context.arena, context.symbols, &mut source_map))
        .transpose()?
        .unwrap_or_default();
    let fields = lower_field_defs(
        pending_fields,
        context.arena,
        context.symbols,
        &mut source_map,
    )?;
    let generics = context.arena.alloc_generics(HirGenerics {
        params: context.arena.alloc_generic_params(&generic_params),
        where_preds: context.arena.alloc_where_preds(&where_preds),
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Struct {
            generics: *generics,
            fields: context.arena.alloc_field_defs(&fields),
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: None,
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level constant entry from the current item tree.
///
/// A diagnosed missing type or initializer is retained as an error HIR node so
/// the owner remains available to later stages. As with function owners, the
/// caller must supply the exact already-interned definition path for this
/// declaration, including its parent and sibling disambiguator. `expected_path`
/// is checked against the path resolved from `owner`.
pub fn lower_const_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Const, context)?;
    let declaration =
        ast::ConstDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::ValueNs(entry.name),
        context,
    )?;

    if let Some(ty) = declaration.ty() {
        ensure_supported_type(&ty)?;
    }
    if let Some(initializer) = declaration.initializer() {
        ensure_supported_expr(&initializer)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let ty = match declaration.ty() {
        Some(ty) => lower_type(&ty, context.arena, context.symbols, &mut source_map)?,
        None => lower_error_type(
            &direct_error_child(declaration.syntax())
                .unwrap_or_else(|| declaration.syntax().clone()),
            context.arena,
            &mut source_map,
        )?,
    };
    let value = match declaration.initializer() {
        Some(initializer) => lower_expr(
            &initializer,
            context.arena,
            context.symbols,
            &mut source_map,
        )?,
        None => lower_error_expr(
            &direct_error_child_from_end(declaration.syntax())
                .unwrap_or_else(|| declaration.syntax().clone()),
            context.arena,
            &mut source_map,
        )?,
    };
    let body = context.arena.alloc_body(Body {
        owner,
        params: &[],
        value,
        contracts: &[],
        invariant_preds: &[],
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Const { ty },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: Some(body),
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level invariant declaration from the current item tree.
///
/// Invariant expressions belong to one owner, so nested closure and async
/// definition IDs are prepared over the entire declaration before any
/// predicate is lowered.
pub fn lower_invariant_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Invariant, context)?;
    let declaration =
        ast::InvariantDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::Invariant(entry.name),
        context,
    )?;

    let predicates = declaration
        .predicates()
        .map(|list| list.expressions().collect::<Vec<_>>())
        .unwrap_or_default();
    for predicate in &predicates {
        ensure_supported_expr(predicate)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    prepare_nested_definition_ids(declaration.syntax(), &mut source_map)?;

    let target_syntax = declaration
        .name()
        .map(|name| name.syntax().clone())
        .unwrap_or_else(|| declaration.syntax().clone());
    let (path_hir_id, path_span) = source_map.add_node_with_span(&target_syntax, SpanCtxt::Root)?;
    let (segment_hir_id, _) = source_map.add_node_with_span(&target_syntax, SpanCtxt::Root)?;
    let target = Path {
        hir_id: path_hir_id,
        root: PathRoot::Plain,
        segments: context.arena.alloc_path_segments(&[PathSegment {
            hir_id: segment_hir_id,
            name,
            args: None,
        }]),
        span: path_span,
    };
    let lowered_predicates = predicates
        .iter()
        .map(|predicate| {
            lower_expr_in_prepared_owner(predicate, context.arena, context.symbols, &mut source_map)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (value_hir_id, value_span) =
        source_map.add_node_with_span(declaration.syntax(), SpanCtxt::Root)?;
    let value = context.arena.alloc_expr(Expr {
        hir_id: value_hir_id,
        kind: ExprKind::Tuple(context.arena.alloc_exprs(&[])),
        span: value_span,
    });
    let body = context.arena.alloc_body(Body {
        owner,
        params: &[],
        value,
        contracts: &[],
        invariant_preds: context.arena.alloc_expr_refs(&lowered_predicates),
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Invariant { target },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: Some(body),
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level test declaration from the current item tree.
///
/// Test names are decoded string literals and form their own DefPath
/// component. Nested closure and async-block paths are prepared over the whole
/// test declaration before its body is lowered.
pub fn lower_test_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) = resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Test, context)?;
    let declaration = ast::TestDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;

    let name_token = declaration
        .name()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let name_value = name_token.value().into_owned();
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::Test(std::sync::Arc::<str>::from(name_value.as_str())),
        context,
    )?;
    if let Some(body) = declaration.body() {
        ensure_supported_block(&body)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name_sym = context.symbols.intern(&name_value)?;
    if name_sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let token = name_token.syntax();
    let parent_span = source_map.span_for(token.parent(), SpanCtxt::Root)?;
    let name_span = Span {
        range: token.text_range(),
        ..parent_span
    };
    let name = Name {
        sym: name_sym,
        span: name_span,
    };
    prepare_nested_definition_ids(declaration.syntax(), &mut source_map)?;
    let value = match declaration.body() {
        Some(body) => lower_block_as_expr(
            declaration.syntax(),
            &body,
            context.arena,
            context.symbols,
            &mut source_map,
        )?,
        None => {
            let error_syntax = direct_error_child_from_end(declaration.syntax())
                .unwrap_or_else(|| declaration.syntax().clone());
            lower_error_expr(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let body = context.arena.alloc_body(Body {
        owner,
        params: &[],
        value,
        contracts: &[],
        invariant_preds: &[],
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Test {
            name: context.arena.alloc_str(&name_value),
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: Some(body),
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level property declaration from the current item tree.
///
/// Property parameters retain their declared types in `ItemKind::Property`
/// and receive corresponding body bindings. The property body and parameter
/// types are preflighted before HIR allocation; nested closure and async-block
/// paths are prepared over the whole declaration.
pub fn lower_property_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Property, context)?;
    let declaration =
        ast::PropertyDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;

    let name_token = declaration
        .name()
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let name_value = name_token.value().into_owned();
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::Property(std::sync::Arc::<str>::from(name_value.as_str())),
        context,
    )?;
    let params = declaration
        .params()
        .map(|list| {
            list.params()
                .map(FunctionParamSyntax::Param)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for param in &params {
        if let Some(ty) = param.ty() {
            ensure_supported_type(&ty)?;
        }
    }
    if let Some(body) = declaration.body() {
        ensure_supported_block(&body)?;
    }

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name_sym = context.symbols.intern(&name_value)?;
    if name_sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let token = name_token.syntax();
    let parent_span = source_map.span_for(token.parent(), SpanCtxt::Root)?;
    let name_span = Span {
        range: token.text_range(),
        ..parent_span
    };
    let name = Name {
        sym: name_sym,
        span: name_span,
    };
    prepare_nested_definition_ids(declaration.syntax(), &mut source_map)?;

    let mut lowered_params = Vec::with_capacity(params.len());
    let mut pending_body_params = Vec::with_capacity(params.len());
    for param in params {
        let (lowered, body_param) =
            lower_function_param(param, context.arena, context.symbols, &mut source_map)?;
        lowered_params.push(lowered);
        pending_body_params.push(body_param);
    }
    let body_params = pending_body_params
        .into_iter()
        .map(|pending| lower_body_param(pending, &mut source_map))
        .collect::<Result<Vec<_>, _>>()?;
    let value = match declaration.body() {
        Some(body) => lower_block_as_expr(
            declaration.syntax(),
            &body,
            context.arena,
            context.symbols,
            &mut source_map,
        )?,
        None => {
            let error_syntax = direct_error_child_from_end(declaration.syntax())
                .unwrap_or_else(|| declaration.syntax().clone());
            lower_error_expr(&error_syntax, context.arena, &mut source_map)?
        }
    };
    let body = context.arena.alloc_body(Body {
        owner,
        params: context.arena.alloc_body_params(&body_params),
        value,
        contracts: &[],
        invariant_preds: &[],
    });
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Property {
            name: context.arena.alloc_str(&name_value),
            params: context.arena.alloc_fn_params(&lowered_params),
        },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: Some(body),
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

/// Lower one top-level effect declaration from the current item tree.
///
/// An omitted or parser-diagnosed malformed `covers` set recovers as empty:
/// the HIR has no error effect reference, and retaining only a valid-looking
/// prefix could grant capabilities not present in the complete declaration.
/// `expected_path` is checked against the path resolved from `owner`.
pub fn lower_effect_owner<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    item_tree: &'hir ItemTree,
    item_id: ast::ErasedAstId,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(HirOwner<'hir>, HirSourceMap), HirLoweringError> {
    let (entry, syntax) =
        resolve_item_tree_entry(item_tree, item_id, ItemTreeKind::Effect, context)?;
    let declaration =
        ast::EffectDecl::cast(syntax).ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let visibility = item_visibility(declaration.syntax());
    validate_current_item_metadata(entry, declaration.attrs(), visibility, context)?;
    validate_owner_def_path(
        owner,
        expected_path,
        DefPathData::Effect(entry.name),
        context,
    )?;

    let mut source_map = HirSourceMapBuilder::new(
        owner,
        context.ast_ids,
        context.snapshot,
        context.root,
        declaration.syntax(),
    )?
    .with_def_path_interner(context.def_paths);
    let name = lower_item_name(
        declaration.name(),
        declaration.syntax(),
        context.symbols,
        &source_map,
    )?;
    if name.sym != entry.name {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    let covers = match declaration
        .covers()
        .and_then(|clause| clause.set())
        .filter(effect_set_is_well_formed)
    {
        Some(set) => lower_effect_set(&set, context.arena, context.symbols, &mut source_map)?,
        None => context.arena.alloc_effect_refs(&[]),
    };
    let span = source_map.span_for(declaration.syntax(), SpanCtxt::Root)?;
    let item = context.arena.alloc_item(Item {
        owner_id: owner,
        name,
        vis: visibility,
        kind: ItemKind::Effect { covers },
        span,
    });
    let source_map = source_map.finish();
    let diagnostics = source_map.diagnostics().to_vec();
    Ok((
        HirOwner {
            id: owner,
            node: OwnerNode::Item(item),
            body: None,
            raw_attrs: &entry.raw_attrs,
            diagnostics,
        },
        source_map,
    ))
}

fn resolve_item_tree_entry<'tree, 'hir>(
    item_tree: &'tree ItemTree,
    item_id: ast::ErasedAstId,
    kind: ItemTreeKind,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(&'tree crate::item_tree::ItemTreeEntry, SyntaxNode), HirLoweringError> {
    if !item_tree.matches_ast_ids(context.ast_ids) {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    if item_tree.file != context.ast_ids.file()
        || !context.ast_ids.matches_root(item_tree.file, context.root)
    {
        return Err(HirSourceMapError::AstIdMapMismatch.into());
    }
    if !context.ast_ids.matches_snapshot(context.snapshot) {
        return Err(HirSourceMapError::WorkspaceSnapshotMismatch.into());
    }
    let entry = item_tree
        .items
        .iter()
        .find(|entry| entry.ast_id == item_id)
        .filter(|entry| entry.kind == kind)
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    let syntax = context
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(item_tree.file, context.root))
        .ok_or(HirLoweringError::ItemTreeEntryMismatch)?;
    Ok((entry, syntax))
}

fn item_visibility(syntax: &SyntaxNode) -> crate::Visibility {
    syntax.children().find_map(ast::Visibility::cast).map_or(
        crate::Visibility::Private,
        |visibility| {
            if visibility.export_token().is_some() {
                crate::Visibility::Export
            } else if visibility.pub_token().is_some() {
                crate::Visibility::Pub
            } else {
                crate::Visibility::Private
            }
        },
    )
}

fn validate_current_item_metadata<'hir>(
    entry: &crate::item_tree::ItemTreeEntry,
    attrs: impl Iterator<Item = ast::Attribute>,
    visibility: crate::Visibility,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(), HirLoweringError> {
    let source = context
        .snapshot
        .file(context.ast_ids.file())
        .ok_or(HirSourceMapError::WorkspaceSnapshotMismatch)?
        .source()
        .text();
    let current_raw_attrs =
        crate::item_tree::raw_attrs(context.ast_ids.file(), source, attrs, context.symbols)?;
    if !raw_attrs_match(&current_raw_attrs, &entry.raw_attrs) || visibility != entry.vis {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    Ok(())
}

fn validate_owner_def_path<'hir>(
    owner: OwnerId,
    expected_path: &DefPath,
    expected_data: DefPathData,
    context: &HirLoweringContext<'_, 'hir>,
) -> Result<(), HirLoweringError> {
    let owner_path = context
        .def_paths
        .resolve(owner.def_id)?
        .ok_or(DefPathInternerError::ParentDefinitionNotFound)?;
    if owner_path != *expected_path
        || !matches!(
            expected_path.data().last(),
            Some(component) if component.data == expected_data
        )
    {
        return Err(HirLoweringError::ItemTreeEntryMismatch);
    }
    Ok(())
}

fn raw_attrs_match(current: &[RawAttr], item_tree: &[RawAttr]) -> bool {
    current.len() == item_tree.len()
        && current.iter().zip(item_tree).all(|(current, item_tree)| {
            current == item_tree
                && current.span == item_tree.span
                && current.ptr.syntax_node_ptr() == item_tree.ptr.syntax_node_ptr()
                && raw_attr_args_match(&current.args, &item_tree.args)
        })
}

fn raw_attr_args_match(
    current: &Option<Vec<RawAttrArg>>,
    item_tree: &Option<Vec<RawAttrArg>>,
) -> bool {
    match (current, item_tree) {
        (None, None) => true,
        (Some(current), Some(item_tree)) => {
            current.len() == item_tree.len()
                && current
                    .iter()
                    .zip(item_tree)
                    .all(|(current, item_tree)| raw_attr_arg_match(current, item_tree))
        }
        _ => false,
    }
}

fn raw_attr_arg_match(current: &RawAttrArg, item_tree: &RawAttrArg) -> bool {
    match (current, item_tree) {
        (
            RawAttrArg::Word {
                name: current_name,
                raw: current_raw,
                args: current_args,
                span: current_span,
            },
            RawAttrArg::Word {
                name: item_tree_name,
                raw: item_tree_raw,
                args: item_tree_args,
                span: item_tree_span,
            },
        ) => {
            current_name == item_tree_name
                && current_raw == item_tree_raw
                && current_span == item_tree_span
                && raw_attr_args_match(current_args, item_tree_args)
        }
        (
            RawAttrArg::Str {
                value: current_value,
                span: current_span,
            },
            RawAttrArg::Str {
                value: item_tree_value,
                span: item_tree_span,
            },
        ) => current_value == item_tree_value && current_span == item_tree_span,
        (
            RawAttrArg::Int {
                value: current_value,
                span: current_span,
            },
            RawAttrArg::Int {
                value: item_tree_value,
                span: item_tree_span,
            },
        ) => current_value == item_tree_value && current_span == item_tree_span,
        (
            RawAttrArg::KeyValue {
                key: current_key,
                raw: current_raw,
                value: current_value,
                value_span: current_value_span,
                span: current_span,
            },
            RawAttrArg::KeyValue {
                key: item_tree_key,
                raw: item_tree_raw,
                value: item_tree_value,
                value_span: item_tree_value_span,
                span: item_tree_span,
            },
        ) => {
            current_key == item_tree_key
                && current_raw == item_tree_raw
                && current_value == item_tree_value
                && current_value_span == item_tree_value_span
                && current_span == item_tree_span
        }
        _ => false,
    }
}

fn lower_function_inner<'hir>(
    declaration: &ast::FnDecl,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    lower_generics: bool,
) -> Result<
    (
        FnSig<'hir>,
        &'hir HirGenerics<'hir>,
        Option<&'hir Body<'hir>>,
    ),
    HirLoweringError,
> {
    if source_map.pointer_for(declaration.syntax())?
        != source_map.pointer_for(source_map.owner_syntax())?
    {
        return Err(HirLoweringError::UnsupportedSyntax(
            declaration.syntax().kind(),
        ));
    }
    if lower_generics {
        ensure_supported_generics(declaration)?;
    }
    let params = declaration
        .params()
        .map(|list| {
            list.syntax()
                .children()
                .filter_map(|child| {
                    ast::Param::cast(child.clone())
                        .map(FunctionParamSyntax::Param)
                        .or_else(|| ast::SelfParam::cast(child).map(FunctionParamSyntax::SelfParam))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let ret_type = declaration.ret_type().and_then(|ret| ret.ty());
    let contracts = declaration
        .syntax()
        .children()
        .filter_map(|child| {
            ast::RequiresClause::cast(child.clone())
                .map(FunctionContractSyntax::Requires)
                .or_else(|| ast::EnsuresClause::cast(child).map(FunctionContractSyntax::Ensures))
        })
        .collect::<Vec<_>>();
    let body = declaration.body();
    let has_body_owner = body.is_some() || !contracts.is_empty();

    for param in &params {
        if let Some(ty) = param.ty() {
            ensure_supported_type(&ty)?;
        }
    }
    if let Some(ret_type) = &ret_type {
        ensure_supported_type(ret_type)?;
    }
    for contract in &contracts {
        if let Some(expression) = contract.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    if let Some(body) = &body {
        ensure_supported_block(body)?;
    }

    let pending_generic_params = if lower_generics {
        prepare_generic_params(declaration.generic_params(), symbols, source_map)?
    } else {
        Vec::new()
    };

    // Assign closure and async-block identities across the complete owner
    // before lowering any child. This also makes contracts and body expressions
    // agree on the same owner-wide preorder.
    let async_fn_body_syntax = if declaration.async_token().is_some() {
        body.as_ref().map(|body| body.syntax().clone())
    } else {
        None
    };
    let async_fn_def_id = prepare_nested_definition_ids_with_async_fn_body(
        declaration.syntax(),
        async_fn_body_syntax.as_ref(),
        source_map,
        true,
    )?;

    let lowered_generic_params =
        lower_generic_params(pending_generic_params, arena, symbols, source_map)?;

    let mut lowered_params = Vec::with_capacity(params.len());
    let mut pending_body_params = Vec::with_capacity(params.len());
    for param in params {
        let (lowered, body_param) = lower_function_param(param, arena, symbols, source_map)?;
        lowered_params.push(lowered);
        pending_body_params.push(body_param);
    }

    let ret = match declaration.ret_type() {
        Some(ret) => match ret.ty() {
            Some(ty) => Some(lower_type_inner(&ty, arena, symbols, source_map)?),
            None => Some(lower_error_type(ret.syntax(), arena, source_map)?),
        },
        None => None,
    };

    let mut lowered_where_preds = Vec::new();
    let mut borrows = None;
    let mut effects = None;
    let mut borrows_seen = false;
    let mut effects_seen = false;
    let mut lowered_contracts = Vec::with_capacity(contracts.len());
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    for child in declaration.syntax().children() {
        if lower_generics && let Some(clause) = ast::WhereClause::cast(child.clone()) {
            lowered_where_preds.extend(lower_where_clause(&clause, arena, symbols, source_map)?);
        } else if let Some(clause) = ast::BorrowsClause::cast(child.clone()) {
            if !borrows_seen {
                borrows_seen = true;
                if borrows_clause_is_well_formed(&clause) {
                    borrows = Some(lower_provenances(&clause, arena, symbols, source_map)?);
                }
            }
        } else if let Some(clause) = ast::EffectsClause::cast(child.clone()) {
            if !effects_seen {
                effects_seen = true;
                effects = lower_function_effects(&clause, arena, symbols, source_map)?;
            }
        } else if let Some(clause) = ast::RequiresClause::cast(child.clone()) {
            let (contract, expression_hir_id) = lower_function_contract(
                FunctionContractSyntax::Requires(clause),
                arena,
                symbols,
                source_map,
            )?;
            requires.push(expression_hir_id);
            lowered_contracts.push(contract);
        } else if let Some(clause) = ast::EnsuresClause::cast(child) {
            let (contract, expression_hir_id) = lower_function_contract(
                FunctionContractSyntax::Ensures(clause),
                arena,
                symbols,
                source_map,
            )?;
            ensures.push(expression_hir_id);
            lowered_contracts.push(contract);
        }
    }
    let generics = arena.alloc_generics(HirGenerics {
        params: arena.alloc_generic_params(&lowered_generic_params),
        where_preds: arena.alloc_where_preds(&lowered_where_preds),
    });

    let body_params = if has_body_owner {
        pending_body_params
            .into_iter()
            .map(|pending| lower_body_param(pending, source_map))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let body_value = match body {
        Some(body) if let Some(async_fn_def_id) = async_fn_def_id => {
            Some(lower_async_function_body(
                declaration,
                &body,
                async_fn_def_id,
                arena,
                symbols,
                source_map,
            )?)
        }
        Some(body) => Some(lower_block_as_expr(
            body.syntax(),
            &body,
            arena,
            symbols,
            source_map,
        )?),
        None if !lowered_contracts.is_empty() => {
            let (hir_id, span) =
                source_map.add_node_with_span(declaration.syntax(), SpanCtxt::Root)?;
            Some(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Tuple(arena.alloc_exprs(&[])),
                span,
            }))
        }
        None => None,
    };

    let signature = FnSig {
        safety: if declaration.unsafe_token().is_some() {
            Safety::Unsafe
        } else {
            Safety::Safe
        },
        is_async: declaration.async_token().is_some(),
        params: arena.alloc_fn_params(&lowered_params),
        ret,
        borrows,
        effects,
        requires: arena.alloc_hir_ids(&requires),
        ensures: arena.alloc_hir_ids(&ensures),
    };

    let Some(body_value) = body_value else {
        return Ok((signature, generics, None));
    };
    let body = arena.alloc_body(Body {
        owner: source_map.owner(),
        params: arena.alloc_body_params(&body_params),
        value: body_value,
        contracts: arena.alloc_contracts(&lowered_contracts),
        invariant_preds: arena.alloc_expr_refs(&[]),
    });
    Ok((signature, generics, Some(body)))
}

fn lower_async_function_body<'hir>(
    declaration: &ast::FnDecl,
    source_body: &ast::Block,
    def_id: LocalDefId,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let span_context = SpanCtxt::Desugared(DesugaringKind::AsyncFn);
    let (value_hir_id, value_span) =
        source_map.add_node_with_span(declaration.syntax(), span_context)?;
    let (outer_block_hir_id, outer_block_span) =
        source_map.add_node_with_span(declaration.syntax(), span_context)?;
    let (async_expr_hir_id, async_expr_span) =
        source_map.add_node_with_span(declaration.syntax(), span_context)?;
    let block = lower_block(source_body, arena, symbols, source_map)?;
    let async_expr = arena.alloc_expr(Expr {
        hir_id: async_expr_hir_id,
        kind: ExprKind::AsyncBlock {
            def_id,
            capture: CaptureBy::Move,
            block,
        },
        span: async_expr_span,
    });
    let outer_block = arena.alloc_block(HirBlock {
        hir_id: outer_block_hir_id,
        stmts: arena.alloc_stmts(&[]),
        tail: Some(async_expr),
        safety: Safety::Safe,
        span: outer_block_span,
    });
    Ok(arena.alloc_expr(Expr {
        hir_id: value_hir_id,
        kind: ExprKind::Block(outer_block),
        span: value_span,
    }))
}

enum FunctionParamSyntax {
    Param(ast::Param),
    SelfParam(ast::SelfParam),
}

struct PendingBodyParam {
    syntax: SyntaxNode,
    binding_syntax: SyntaxNode,
    name: Option<Name>,
    mutable: bool,
}

impl FunctionParamSyntax {
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Param(param) => param.syntax(),
            Self::SelfParam(param) => param.syntax(),
        }
    }

    fn ty(&self) -> Option<ast::Type> {
        match self {
            Self::Param(param) => param.ty(),
            Self::SelfParam(param) => param.ty(),
        }
    }
}

enum FunctionContractSyntax {
    Requires(ast::RequiresClause),
    Ensures(ast::EnsuresClause),
}

impl FunctionContractSyntax {
    fn expression(&self) -> Option<ast::Expr> {
        match self {
            Self::Requires(clause) => clause.expression(),
            Self::Ensures(clause) => clause.expression(),
        }
    }
}

fn lower_function_param<'hir>(
    param: FunctionParamSyntax,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(FnParam<'hir>, PendingBodyParam), HirLoweringError> {
    let (hir_id, _) = source_map.add_node_with_span(param.syntax(), SpanCtxt::Root)?;
    let (name, binding_syntax, is_self, mutable) = match &param {
        FunctionParamSyntax::Param(param) => {
            let source_name = param.name();
            let name_token = source_name.as_ref().and_then(ast::Name::token);
            let binding_syntax = source_name
                .as_ref()
                .map(|name| name.syntax().clone())
                .or_else(|| direct_error_child(param.syntax()))
                .unwrap_or_else(|| param.syntax().clone());
            let name = match (source_name, name_token) {
                (Some(_), Some(token)) if token.kind() == SyntaxKind::UNDERSCORE => None,
                (Some(name), token) => Some(lower_name_spelling(
                    name.syntax(),
                    token.as_ref().map(|token| token.text()),
                    symbols,
                    source_map,
                )?),
                (None, _) => Some(Name {
                    sym: Symbol::ERROR,
                    span: source_map.span_for(&binding_syntax, SpanCtxt::Root)?,
                }),
            };
            (name, binding_syntax, false, param.is_mutable())
        }
        FunctionParamSyntax::SelfParam(param) => {
            let binding_syntax = param.syntax().clone();
            let mut span = source_map.span_for(param.syntax(), SpanCtxt::Root)?;
            if let Some(token) = direct_token(param.syntax(), SyntaxKind::SELF_KW) {
                span.range = token.text_range();
            }
            (
                Some(Name {
                    sym: symbols.intern("self")?,
                    span,
                }),
                binding_syntax,
                true,
                param.is_mutable(),
            )
        }
    };
    let ty = match param.ty() {
        Some(ty) => lower_type_inner(&ty, arena, symbols, source_map)?,
        None => lower_error_type(param.syntax(), arena, source_map)?,
    };
    let lowered = FnParam {
        hir_id,
        name,
        is_self,
        mutable,
        ty,
    };
    let body_param = PendingBodyParam {
        syntax: param.syntax().clone(),
        binding_syntax,
        name,
        mutable,
    };
    Ok((lowered, body_param))
}

fn lower_body_param(
    pending: PendingBodyParam,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<BodyParam, HirLoweringError> {
    let hir_id = source_map.add_node(&pending.syntax)?;
    let binding = source_map.add_binding(&pending.binding_syntax)?;
    Ok(BodyParam {
        hir_id,
        binding,
        name: pending.name,
        mutable: pending.mutable,
    })
}

fn lower_provenances<'hir>(
    clause: &ast::BorrowsClause,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<&'hir [Provenance<'hir>], HirLoweringError> {
    let mut lowered = Vec::new();
    for provenance in clause.provenances() {
        let segments = provenance
            .segments()
            .map(|segment| {
                if segment
                    .syntax()
                    .first_token()
                    .is_some_and(|token| token.kind() == SyntaxKind::SELF_KW)
                {
                    Ok(Name {
                        sym: symbols.intern("self")?,
                        span: source_map.span_for(segment.syntax(), SpanCtxt::Root)?,
                    })
                } else {
                    lower_name_ref(&segment, symbols, source_map)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (root, fields) = match segments.split_first() {
            Some((root, fields)) => (*root, fields),
            None => (
                Name {
                    sym: Symbol::ERROR,
                    span: source_map.span_for(provenance.syntax(), SpanCtxt::Root)?,
                },
                &[] as &[Name],
            ),
        };
        lowered.push(Provenance {
            root,
            fields: arena.alloc_names(fields),
        });
    }
    Ok(arena.alloc_provenances(&lowered))
}

fn lower_item_name(
    name: Option<ast::Name>,
    fallback_syntax: &SyntaxNode,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    let Some(name) = name else {
        return lower_name_spelling(fallback_syntax, None, symbols, source_map);
    };
    let written = name.token().map(|token| token.text().to_owned());
    lower_name_spelling(name.syntax(), written.as_deref(), symbols, source_map)
}

struct PendingGenericParam {
    param: ast::GenericParam,
    name: Name,
    def_id: LocalDefId,
}

struct PendingFieldDef {
    field: ast::FieldDecl,
    name: Name,
    def_id: LocalDefId,
}

fn prepare_generic_params(
    generic_params: Option<ast::GenericParamList>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Vec<PendingGenericParam>, HirLoweringError> {
    let params = generic_params
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    if params.is_empty() {
        return Ok(Vec::new());
    }
    let def_paths = source_map
        .def_path_interner()
        .ok_or(HirLoweringError::DefPathInternerRequired)?;

    let mut pending = Vec::with_capacity(params.len());
    let mut disambiguators = HashMap::<DefPathData, u32>::new();
    for param in params {
        let (name, is_const) = match &param {
            ast::GenericParam::Type(param) => (
                lower_item_name(param.name(), param.syntax(), symbols, source_map)?,
                false,
            ),
            ast::GenericParam::Const(param) => (
                lower_item_name(param.name(), param.syntax(), symbols, source_map)?,
                true,
            ),
        };
        let component = if is_const {
            DefPathData::ConstParam(name.sym)
        } else {
            DefPathData::TypeParam(name.sym)
        };
        let disambiguator = disambiguators.entry(component).or_default();
        let ordinal = *disambiguator;
        *disambiguator = disambiguator
            .checked_add(1)
            .ok_or(DefPathInternerError::TooManyDefinitions)?;
        pending.push((param, name, is_const, ordinal));
    }

    let path_parameters = pending
        .iter()
        .map(|(_, name, is_const, disambiguator)| (name.sym, *is_const, *disambiguator))
        .collect::<Vec<_>>();
    let def_ids = def_paths.intern_generic_params(source_map.owner().def_id, &path_parameters)?;
    Ok(pending
        .into_iter()
        .zip(def_ids)
        .map(|((param, name, _, _), def_id)| PendingGenericParam {
            param,
            name,
            def_id,
        })
        .collect())
}

fn prepare_field_defs(
    fields: Vec<ast::FieldDecl>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Vec<PendingFieldDef>, HirLoweringError> {
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let def_paths = source_map
        .def_path_interner()
        .ok_or(HirLoweringError::DefPathInternerRequired)?;
    let mut pending = Vec::with_capacity(fields.len());
    let mut disambiguators = HashMap::<Symbol, u32>::new();
    for field in fields {
        let name = lower_item_name(field.name(), field.syntax(), symbols, source_map)?;
        let disambiguator = disambiguators.entry(name.sym).or_default();
        let ordinal = *disambiguator;
        *disambiguator = disambiguator
            .checked_add(1)
            .ok_or(DefPathInternerError::TooManyDefinitions)?;
        pending.push((field, name, ordinal));
    }
    let field_paths = pending
        .iter()
        .map(|(_, name, disambiguator)| (name.sym, *disambiguator))
        .collect::<Vec<_>>();
    let def_ids = def_paths.intern_fields(source_map.owner().def_id, &field_paths)?;
    Ok(pending
        .into_iter()
        .zip(def_ids)
        .map(|((field, name, _), def_id)| PendingFieldDef {
            field,
            name,
            def_id,
        })
        .collect())
}

fn lower_field_defs<'hir>(
    pending_fields: Vec<PendingFieldDef>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Vec<FieldDef<'hir>>, HirLoweringError> {
    pending_fields
        .into_iter()
        .map(
            |PendingFieldDef {
                 field,
                 name,
                 def_id,
             }| {
                let hir_id = source_map.add_node(field.syntax())?;
                let ty = match field.ty() {
                    Some(ty) => lower_type(&ty, arena, symbols, source_map)?,
                    None => {
                        let error_syntax = direct_error_child(field.syntax())
                            .unwrap_or_else(|| field.syntax().clone());
                        lower_error_type(&error_syntax, arena, source_map)?
                    }
                };
                Ok(FieldDef {
                    hir_id,
                    def_id,
                    vis: if field.pub_token().is_some() {
                        crate::Visibility::Pub
                    } else {
                        crate::Visibility::Private
                    },
                    name,
                    ty,
                })
            },
        )
        .collect()
}

fn lower_generic_params<'hir>(
    pending_params: Vec<PendingGenericParam>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Vec<HirGenericParam<'hir>>, HirLoweringError> {
    pending_params
        .into_iter()
        .map(
            |PendingGenericParam {
                 param,
                 name,
                 def_id,
             }| {
                let kind = match param {
                    ast::GenericParam::Type(param) => {
                        let bounds = param
                            .bounds()
                            .map(|bounds| lower_bounds(&bounds, arena, symbols, source_map))
                            .transpose()?
                            .unwrap_or_default();
                        HirGenericParamKind::Type {
                            bounds: arena.alloc_bounds(&bounds),
                        }
                    }
                    ast::GenericParam::Const(param) => {
                        let ty = match param.ty() {
                            Some(ty) => lower_type_inner(&ty, arena, symbols, source_map)?,
                            None => lower_error_type(param.syntax(), arena, source_map)?,
                        };
                        HirGenericParamKind::Const { ty }
                    }
                };
                Ok(HirGenericParam { def_id, name, kind })
            },
        )
        .collect()
}

fn lower_where_clause<'hir>(
    where_clause: &ast::WhereClause,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Vec<HirWherePred<'hir>>, HirLoweringError> {
    let mut lowered = Vec::new();
    for predicate in where_clause.predicates() {
        let hir_id = source_map.add_node(predicate.syntax())?;
        let ty = match predicate.ty() {
            Some(ty) => lower_type_inner(&ty, arena, symbols, source_map)?,
            None => lower_error_type(predicate.syntax(), arena, source_map)?,
        };
        let bounds = predicate
            .bounds()
            .map(|bound| lower_bound(bound, arena, symbols, source_map))
            .collect::<Result<Vec<_>, _>>()?;
        lowered.push(HirWherePred {
            hir_id,
            ty,
            bounds: arena.alloc_bounds(&bounds),
        });
    }
    Ok(lowered)
}

fn lower_function_contract<'hir>(
    contract: FunctionContractSyntax,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(Contract<'hir>, crate::HirId), HirLoweringError> {
    let (syntax, kind, expression) = match contract {
        FunctionContractSyntax::Requires(clause) => (
            clause.syntax().clone(),
            ContractKind::Requires,
            clause.expression(),
        ),
        FunctionContractSyntax::Ensures(clause) => (
            clause.syntax().clone(),
            ContractKind::Ensures,
            clause.expression(),
        ),
    };
    let (hir_id, _) = source_map.add_node_with_span(&syntax, SpanCtxt::Root)?;
    let expression = match expression {
        Some(expression) => lower_expr_in_prepared_owner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(&syntax).unwrap_or_else(|| syntax.clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let expression_hir_id = expression.hir_id;
    Ok((
        Contract {
            hir_id,
            kind,
            expr: expression,
        },
        expression_hir_id,
    ))
}

fn lower_bounds<'hir>(
    bounds: &ast::BoundList,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Vec<HirBound<'hir>>, HirLoweringError> {
    bounds
        .bounds()
        .map(|bound| lower_bound(bound, arena, symbols, source_map))
        .collect()
}

fn lower_bound<'hir>(
    bound: ast::Bound,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirBound<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(bound.syntax(), SpanCtxt::Root)?;
    let (kind, effects_clause) = match bound {
        ast::Bound::Trait(bound) => {
            let path = match bound.path() {
                Some(path) if contains_error_node(path.syntax()) => lower_path(
                    path.syntax(),
                    Vec::new(),
                    PathRoot::Plain,
                    arena,
                    symbols,
                    source_map,
                )?,
                Some(path) => lower_path(
                    path.syntax(),
                    path.segments().collect(),
                    PathRoot::Plain,
                    arena,
                    symbols,
                    source_map,
                )?,
                None => lower_path(
                    bound.syntax(),
                    Vec::new(),
                    PathRoot::Plain,
                    arena,
                    symbols,
                    source_map,
                )?,
            };
            (HirBoundKind::Trait(*path), bound.effects())
        }
        ast::Bound::FnSugar(bound) => {
            let kind = lower_fn_trait_kind(&bound);
            let params = lower_fn_type_params(bound.syntax(), arena, symbols, source_map)?;
            let ret = match bound.ret_type() {
                Some(ty) => Some(lower_type_inner(&ty, arena, symbols, source_map)?),
                None if direct_token(bound.syntax(), SyntaxKind::THIN_ARROW).is_some() => {
                    let error =
                        direct_error_child_after_token(bound.syntax(), SyntaxKind::THIN_ARROW)
                            .unwrap_or_else(|| bound.syntax().clone());
                    Some(lower_error_type(&error, arena, source_map)?)
                }
                None => None,
            };
            (
                HirBoundKind::FnSugar {
                    kind,
                    params: arena.alloc_tys(&params),
                    ret,
                },
                bound.effects(),
            )
        }
    };
    let effects = effects_clause
        .as_ref()
        .map(|clause| lower_function_effects(clause, arena, symbols, source_map))
        .transpose()?
        .flatten();
    Ok(HirBound {
        hir_id,
        kind,
        effects,
        span,
    })
}

fn lower_fn_type_params<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Vec<HirTy<'hir>>, HirLoweringError> {
    let mut lowered = Vec::new();
    let mut in_params = false;
    let mut has_parameter = false;
    for element in syntax.children_with_tokens() {
        match element {
            SyntaxElement::Node(node) if in_params => {
                if node.kind() == SyntaxKind::ERROR {
                    if !is_missing_close_delimiter_error(syntax, &node, SyntaxKind::R_PAREN)
                        || error_is_before_comma(syntax, &node)
                    {
                        lowered.push(*lower_error_type(&node, arena, source_map)?);
                        has_parameter = true;
                    }
                } else if let Some(ty) = ast::Type::cast(node) {
                    lowered.push(*lower_type_inner(&ty, arena, symbols, source_map)?);
                    has_parameter = true;
                }
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::L_PAREN && !in_params => {
                in_params = true;
            }
            SyntaxElement::Token(token) if in_params && token.kind() == SyntaxKind::R_PAREN => {
                break;
            }
            SyntaxElement::Token(token) if in_params && token.kind() == SyntaxKind::THIN_ARROW => {
                break;
            }
            SyntaxElement::Token(token) if in_params && token.kind() == SyntaxKind::COMMA => {
                if !has_parameter {
                    lowered.push(*lower_error_type(syntax, arena, source_map)?);
                }
                has_parameter = false;
            }
            SyntaxElement::Token(_) | SyntaxElement::Node(_) => {}
        }
    }
    Ok(lowered)
}

fn error_is_before_comma(syntax: &SyntaxNode, error: &SyntaxNode) -> bool {
    for ancestor in syntax.ancestors() {
        match ancestor.token_at_offset(error.text_range().start()) {
            TokenAtOffset::Single(token) if token.kind() == SyntaxKind::COMMA => return true,
            TokenAtOffset::Between(left, right)
                if left.kind() == SyntaxKind::COMMA || right.kind() == SyntaxKind::COMMA =>
            {
                return true;
            }
            TokenAtOffset::None | TokenAtOffset::Single(_) | TokenAtOffset::Between(_, _) => {}
        }
    }
    false
}

fn lower_fn_trait_kind(bound: &ast::FnSugarBound) -> FnTraitKind {
    let written = bound
        .name()
        .and_then(|name| name.syntax().first_token())
        .map(|token| token.text().to_owned());
    match written.as_deref() {
        Some("Fn") => FnTraitKind::Fn,
        Some("FnMut") => FnTraitKind::FnMut,
        // `FnOnce` is also the deterministic recovery for a malformed
        // function-sugar name: it promises the least callability.
        Some("FnOnce") | None | Some(_) => FnTraitKind::FnOnce,
    }
}

fn lower_function_effects<'hir>(
    clause: &ast::EffectsClause,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Option<&'hir [HirEffectRef<'hir>]>, HirLoweringError> {
    let Some(set) = clause.set().filter(effect_set_is_well_formed) else {
        return Ok(None);
    };
    lower_effect_set(&set, arena, symbols, source_map).map(Some)
}

impl From<HirSourceMapError> for HirLoweringError {
    fn from(error: HirSourceMapError) -> Self {
        Self::SourceMap(error)
    }
}

impl From<ItemTreeError> for HirLoweringError {
    fn from(error: ItemTreeError) -> Self {
        Self::ItemTree(error)
    }
}

impl From<InternError> for HirLoweringError {
    fn from(error: InternError) -> Self {
        Self::Interner(error)
    }
}

impl From<DefPathInternerError> for HirLoweringError {
    fn from(error: DefPathInternerError) -> Self {
        Self::DefPath(error)
    }
}

/// Lower one syntax literal to a HIR expression and record its source span.
///
/// The source-map builder validates that the syntax node belongs to its
/// snapshot-bound file root before assigning the HIR ID. Malformed literals
/// become `ExprKind::Err` because their lexical or parser diagnostic already
/// exists.
pub fn lower_literal<'hir>(
    literal: &ast::LiteralExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    lower_literal_with_sign(literal, literal.syntax(), false, arena, symbols, source_map)
}

fn lower_format_string_expr<'hir>(
    format_string: &ast::FStringExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(format_string.syntax(), SpanCtxt::Root)?;
    let mut interpolation_nodes = format_string
        .syntax()
        .children()
        .filter_map(ast::FStringInterp::cast);
    let mut pieces = Vec::new();
    for piece in format_string.pieces() {
        match piece {
            ast::FStringPiece::Text(text) => {
                pieces.push(FmtPiece::Text(symbols.intern(text.as_ref())?));
            }
            ast::FStringPiece::Interp { expr, spec } => {
                let interpolation = interpolation_nodes
                    .next()
                    .expect("each typed f-string interpolation has a syntax node");
                let interpolation_error = interpolation
                    .syntax()
                    .descendants()
                    .find(|node| node.kind() == SyntaxKind::ERROR);
                let expr = match (expr, interpolation_error) {
                    (_, Some(error)) => lower_error_expr(&error, arena, source_map)?,
                    (Some(expr), None) => lower_expr_inner(&expr, arena, symbols, source_map)?,
                    (None, None) => lower_error_expr(interpolation.syntax(), arena, source_map)?,
                };
                let spec = spec
                    .map(|spec| -> Result<HirFmtSpec, HirLoweringError> {
                        Ok(HirFmtSpec {
                            name: symbols.intern(&sai_syntax::normalize_identifier(spec.name()))?,
                            arg: spec.arg(),
                            span: Span {
                                file: span.file,
                                range: spec.syntax().text_range(),
                                ctxt: SpanCtxt::Root,
                            },
                        })
                    })
                    .transpose()?;
                pieces.push(FmtPiece::Arg { expr, spec });
            }
        }
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::FormatString(arena.alloc_fmt_pieces(&pieces)),
        span,
    }))
}

fn lower_closure_expr<'hir>(
    closure: &ast::ClosureExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(params), Some(body)) = (closure.params(), closure.body()) else {
        return lower_error_expr(closure.syntax(), arena, source_map);
    };
    let params = params.params().collect::<Vec<_>>();

    let (hir_id, span) = source_map.add_node_with_span(closure.syntax(), SpanCtxt::Root)?;
    let def_id = source_map
        .nested_definition_for_syntax(closure.syntax())?
        .ok_or(HirSourceMapError::NestedDefinitionIdMismatch)?;
    let mut lowered_params = Vec::with_capacity(params.len());
    for param in params {
        let (param_hir_id, _) = source_map.add_node_with_span(param.syntax(), SpanCtxt::Root)?;
        let source_name = param.name();
        let name_token = source_name.as_ref().and_then(ast::Name::token);
        let binding_syntax = source_name
            .as_ref()
            .map(|name| name.syntax().clone())
            .or_else(|| direct_error_child(param.syntax()))
            .unwrap_or_else(|| param.syntax().clone());
        let (binding, name_span) =
            source_map.add_binding_with_span(&binding_syntax, SpanCtxt::Root)?;
        let name = match (source_name, name_token) {
            (Some(_), Some(token)) if token.kind() == SyntaxKind::UNDERSCORE => None,
            (Some(name), token) => {
                let hir_name = lower_name_spelling(
                    name.syntax(),
                    token.as_ref().map(|token| token.text()),
                    symbols,
                    source_map,
                )?;
                Some(hir_name)
            }
            (None, _) => Some(Name {
                sym: Symbol::ERROR,
                span: name_span,
            }),
        };
        let ty = match param.ty() {
            Some(ty) => Some(lower_type_inner(&ty, arena, symbols, source_map)?),
            None if has_direct_token(param.syntax(), SyntaxKind::COLON) => {
                Some(lower_error_type(param.syntax(), arena, source_map)?)
            }
            None => None,
        };
        lowered_params.push(ClosureParam {
            hir_id: param_hir_id,
            binding,
            name,
            mutable: has_direct_token(param.syntax(), SyntaxKind::VAR_KW),
            ty,
        });
    }

    let ret = closure
        .ret_type()
        .map(|ret| match ret.ty() {
            Some(ty) => lower_type_inner(&ty, arena, symbols, source_map),
            None => lower_error_type(ret.syntax(), arena, source_map),
        })
        .transpose()?;
    let body = lower_block(&body, arena, symbols, source_map)?;
    let closure = arena.alloc_closure(Closure {
        def_id,
        capture: if has_direct_token(closure.syntax(), SyntaxKind::MOVE_KW) {
            CaptureBy::Move
        } else {
            CaptureBy::Infer
        },
        params: arena.alloc_closure_params(&lowered_params),
        ret,
        body,
    });
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Closure(closure),
        span,
    }))
}

fn lower_async_block_expr<'hir>(
    async_block: &ast::AsyncBlockExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(body) = async_block.body() else {
        return lower_error_expr(async_block.syntax(), arena, source_map);
    };
    let def_id = source_map
        .nested_definition_for_syntax(async_block.syntax())?
        .ok_or(HirSourceMapError::NestedDefinitionIdMismatch)?;
    let (hir_id, span) = source_map.add_node_with_span(async_block.syntax(), SpanCtxt::Root)?;
    let block = lower_block(&body, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::AsyncBlock {
            def_id,
            capture: if has_direct_token(async_block.syntax(), SyntaxKind::MOVE_KW) {
                CaptureBy::Move
            } else {
                CaptureBy::Infer
            },
            block,
        },
        span,
    }))
}

fn lower_literal_with_sign<'hir>(
    literal: &ast::LiteralExpr,
    expression_syntax: &SyntaxNode,
    negated: bool,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(expression_syntax, SpanCtxt::Root)?;
    let kind = if contains_error_node(expression_syntax) {
        ExprKind::Err
    } else {
        literal
            .token()
            .map(|token| decode_literal(token, arena, symbols, true, negated))
            .transpose()?
            .flatten()
            .map(ExprKind::Lit)
            .unwrap_or(ExprKind::Err)
    };
    Ok(arena.alloc_expr(Expr { hir_id, kind, span }))
}

/// Lower one syntax type and record its source spans.
///
/// Parentheses and `owned` are transparent in HIR. Missing or error-marked
/// types become `TyKind::Err`, leaving parser diagnostics authoritative.
pub fn lower_type<'hir>(
    ty: &ast::Type,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirTy<'hir>, HirLoweringError> {
    ensure_supported_type(ty)?;
    lower_type_inner(ty, arena, symbols, source_map)
}

fn lower_type_inner<'hir>(
    ty: &ast::Type,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirTy<'hir>, HirLoweringError> {
    if !matches!(ty, ast::Type::TupleType(_)) && direct_error_child(ty.syntax()).is_some() {
        return lower_error_type(ty.syntax(), arena, source_map);
    }
    match ty {
        ast::Type::PathType(path) => lower_path_type(
            path.syntax(),
            path.segments().collect(),
            PathRoot::Plain,
            arena,
            symbols,
            source_map,
        ),
        ast::Type::SelfType(path) => lower_path_type(
            path.syntax(),
            path.segments().collect(),
            PathRoot::SelfType,
            arena,
            symbols,
            source_map,
        ),
        ast::Type::BorrowType(borrow) => {
            let Some(inner) = borrow.ty() else {
                return lower_error_type(borrow.syntax(), arena, source_map);
            };
            let (hir_id, span) = source_map.add_node_with_span(borrow.syntax(), SpanCtxt::Root)?;
            let mutability = if borrow.mut_token().is_some() {
                Mutability::Mut
            } else {
                Mutability::Not
            };
            let inner = lower_type_inner(&inner, arena, symbols, source_map)?;
            Ok(arena.alloc_ty(HirTy {
                hir_id,
                kind: HirTyKind::Borrow(mutability, inner),
                span,
            }))
        }
        ast::Type::OwnedType(owned) => match owned.ty() {
            Some(inner) => lower_type_inner(&inner, arena, symbols, source_map),
            None => lower_error_type(owned.syntax(), arena, source_map),
        },
        ast::Type::TupleType(tuple) => {
            let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
            let mut elements = Vec::new();
            for child in tuple.syntax().children() {
                if child.kind() == SyntaxKind::ERROR {
                    if is_missing_tuple_close_error(tuple, &child) {
                        continue;
                    }
                    elements.push(*lower_error_type(&child, arena, source_map)?);
                } else if let Some(element) = ast::Type::cast(child) {
                    elements.push(*lower_type_inner(&element, arena, symbols, source_map)?);
                }
            }
            let comma_error = boundary_comma_error(tuple.syntax());
            let unconsumed_commas = comma_error.as_ref().map_or(0, |error| {
                unconsumed_commas_at_error_boundary(tuple.syntax(), error)
            });
            let represented_error_slots = tuple
                .syntax()
                .children()
                .filter(|child| {
                    child.kind() == SyntaxKind::ERROR
                        && child.text_range().is_empty()
                        && comma_error.as_ref().is_some_and(|error| {
                            child.text_range().start() == error.text_range().start()
                        })
                        && !is_missing_tuple_close_error(tuple, child)
                })
                .count();
            if let Some(error) = comma_error.as_ref() {
                for _ in represented_error_slots..unconsumed_commas {
                    elements.push(*lower_error_type(error, arena, source_map)?);
                }
            }
            Ok(arena.alloc_ty(HirTy {
                hir_id,
                kind: HirTyKind::Tuple(arena.alloc_tys(&elements)),
                span,
            }))
        }
        ast::Type::ParenType(paren) => match paren.ty() {
            Some(inner) => lower_type_inner(&inner, arena, symbols, source_map),
            None => lower_error_type(paren.syntax(), arena, source_map),
        },
        ast::Type::InferType(infer) => {
            let (hir_id, span) = source_map.add_node_with_span(infer.syntax(), SpanCtxt::Root)?;
            Ok(arena.alloc_ty(HirTy {
                hir_id,
                kind: HirTyKind::Infer,
                span,
            }))
        }
        ast::Type::FnPtrType(function) => {
            let effects_set = match function
                .syntax()
                .children()
                .find_map(ast::EffectsClause::cast)
            {
                Some(clause) => match clause.set() {
                    Some(set) if effect_set_is_well_formed(&set) => Some(set),
                    _ => return lower_error_type(function.syntax(), arena, source_map),
                },
                None => None,
            };
            let (hir_id, span) =
                source_map.add_node_with_span(function.syntax(), SpanCtxt::Root)?;
            let params = lower_fn_type_params(function.syntax(), arena, symbols, source_map)?;
            let ret = function
                .ret_type()
                .map(|ret| lower_type_inner(&ret, arena, symbols, source_map))
                .transpose()?;
            let effects = effects_set
                .as_ref()
                .map(|effects| lower_effect_set(effects, arena, symbols, source_map))
                .transpose()?;
            Ok(arena.alloc_ty(HirTy {
                hir_id,
                kind: HirTyKind::FnPtr {
                    safety: if function.unsafe_token().is_some() {
                        Safety::Unsafe
                    } else {
                        Safety::Safe
                    },
                    params: arena.alloc_tys(&params),
                    ret,
                    effects,
                },
                span,
            }))
        }
    }
}

fn lower_error_type<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirTy<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    Ok(arena.alloc_ty(HirTy {
        hir_id,
        kind: HirTyKind::Err,
        span,
    }))
}

fn lower_path_type<'hir>(
    syntax: &SyntaxNode,
    path_segments: Vec<ast::PathSegment>,
    root: PathRoot,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirTy<'hir>, HirLoweringError> {
    if root == PathRoot::Plain && path_segments.is_empty() {
        return lower_error_type(syntax, arena, source_map);
    }
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let path = lower_path(syntax, path_segments, root, arena, symbols, source_map)?;
    Ok(arena.alloc_ty(HirTy {
        hir_id,
        kind: HirTyKind::Path(*path),
        span,
    }))
}

fn lower_path<'hir>(
    syntax: &SyntaxNode,
    path_segments: Vec<ast::PathSegment>,
    root: PathRoot,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Path<'hir>, HirLoweringError> {
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let mut segments = Vec::new();
    for segment in path_segments {
        let (segment_hir_id, _) =
            source_map.add_node_with_span(segment.syntax(), SpanCtxt::Root)?;
        let name = match segment.name() {
            Some(name) => lower_name_ref(&name, symbols, source_map)?,
            None => lower_name_spelling(segment.syntax(), None, symbols, source_map)?,
        };
        let args = segment
            .generic_args()
            .map(|args| lower_generic_args(&args, arena, symbols, source_map))
            .transpose()?;
        segments.push(PathSegment {
            hir_id: segment_hir_id,
            name,
            args,
        });
    }
    Ok(arena.alloc_path(Path {
        hir_id: path_hir_id,
        root,
        segments: arena.alloc_path_segments(&segments),
        span: path_span,
    }))
}

fn lower_generic_args<'hir>(
    args: &ast::GenericArgList,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirGenericArgs<'hir>, HirLoweringError> {
    let mut lowered = Vec::new();
    let mut has_argument_in_slot = false;
    let mut has_any_argument = false;
    let mut trailing_comma = false;
    for element in args.syntax().children_with_tokens() {
        match element {
            SyntaxElement::Node(child) => {
                if let Some(type_arg) = ast::TypeArg::cast(child.clone()) {
                    let ty = match type_arg.ty() {
                        Some(ty) => lower_type_inner(&ty, arena, symbols, source_map)?,
                        None => lower_error_type(type_arg.syntax(), arena, source_map)?,
                    };
                    lowered.push(HirGenericArg::Type(ty));
                    has_argument_in_slot = true;
                    has_any_argument = true;
                    trailing_comma = false;
                } else if let Some(const_arg) = ast::ConstArg::cast(child.clone()) {
                    match lower_const_arg(&const_arg) {
                        Some(const_arg) => lowered.push(HirGenericArg::Const(const_arg)),
                        None => lowered.push(HirGenericArg::Type(lower_error_type(
                            const_arg.syntax(),
                            arena,
                            source_map,
                        )?)),
                    }
                    has_argument_in_slot = true;
                    has_any_argument = true;
                    trailing_comma = false;
                } else if let Some(binding) = ast::AssocTypeBinding::cast(child.clone()) {
                    let name = match binding.name() {
                        Some(name) => lower_name_ref(&name, symbols, source_map)?,
                        None => lower_name_spelling(binding.syntax(), None, symbols, source_map)?,
                    };
                    let ty = match binding.ty() {
                        Some(ty) => lower_type_inner(&ty, arena, symbols, source_map)?,
                        None => lower_error_type(binding.syntax(), arena, source_map)?,
                    };
                    lowered.push(HirGenericArg::Binding { name, ty });
                    has_argument_in_slot = true;
                    has_any_argument = true;
                    trailing_comma = false;
                } else if child.kind() == SyntaxKind::ERROR {
                    if is_missing_generic_close_error(args, &child) {
                        continue;
                    }
                    lowered.push(HirGenericArg::Type(lower_error_type(
                        &child, arena, source_map,
                    )?));
                    has_argument_in_slot = true;
                    has_any_argument = true;
                    trailing_comma = false;
                }
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => {
                if !has_argument_in_slot {
                    // Some parser recovery paths report a missing argument but
                    // do not keep an ERROR node at that source position.
                    lowered.push(HirGenericArg::Type(lower_error_type(
                        args.syntax(),
                        arena,
                        source_map,
                    )?));
                    has_any_argument = true;
                }
                has_argument_in_slot = false;
                trailing_comma = true;
            }
            SyntaxElement::Token(_) => {}
        }
    }
    let comma_error = boundary_comma_error(args.syntax());
    let unconsumed_commas = comma_error.as_ref().map_or(0, |error| {
        unconsumed_commas_at_error_boundary(args.syntax(), error)
    });
    let represented_error_slots = args
        .syntax()
        .children()
        .filter(|child| {
            child.kind() == SyntaxKind::ERROR
                && child.text_range().is_empty()
                && comma_error
                    .as_ref()
                    .is_some_and(|error| child.text_range().start() == error.text_range().start())
                && !is_missing_generic_close_error(args, child)
        })
        .count();
    if let Some(error) = comma_error.as_ref() {
        for _ in represented_error_slots..unconsumed_commas {
            lowered.push(HirGenericArg::Type(lower_error_type(
                error, arena, source_map,
            )?));
        }
    }
    if !has_any_argument && !trailing_comma {
        lowered.push(HirGenericArg::Type(lower_error_type(
            args.syntax(),
            arena,
            source_map,
        )?));
    }
    let span = source_map.span_for(args.syntax(), SpanCtxt::Root)?;
    Ok(arena.alloc_generic_args(HirGenericArgs {
        args: arena.alloc_generic_args_list(&lowered),
        span,
    }))
}

fn lower_const_arg(arg: &ast::ConstArg) -> Option<HirConstArg> {
    let negated = arg.minus_token().is_some();
    let token = arg.value_token()?;
    if token.has_error() {
        return None;
    }
    Some(match token.kind() {
        SyntaxKind::INT => HirConstArg::Int {
            value: ast::IntLit::cast(token).and_then(|literal| literal.value()),
            negated,
        },
        SyntaxKind::CHAR => HirConstArg::Char(ast::CharLit::cast(token)?.value()),
        SyntaxKind::TRUE_KW => HirConstArg::Bool(true),
        SyntaxKind::FALSE_KW => HirConstArg::Bool(false),
        _ => {
            return None;
        }
    })
}

fn lower_effect_set<'hir>(
    set: &ast::EffectSet,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir [HirEffectRef<'hir>], HirLoweringError> {
    let mut effects = Vec::new();
    for effect in set.effects() {
        let (hir_id, _) = source_map.add_node_with_span(effect.syntax(), SpanCtxt::Root)?;
        let segments = effect
            .segments()
            .map(|segment| lower_name_ref(&segment, symbols, source_map))
            .collect::<Result<Vec<_>, _>>()?;
        effects.push(HirEffectRef {
            hir_id,
            segments: arena.alloc_names(&segments),
        });
    }
    Ok(arena.alloc_effect_refs(&effects))
}

fn effect_set_is_well_formed(set: &ast::EffectSet) -> bool {
    !contains_error_node(set.syntax())
        && has_direct_token(set.syntax(), SyntaxKind::L_BRACE)
        && has_direct_token(set.syntax(), SyntaxKind::R_BRACE)
        && set.effects().all(effect_ref_is_well_formed)
}

fn effect_ref_is_well_formed(effect: ast::EffectRef) -> bool {
    !contains_error_node(effect.syntax())
        && effect
            .segments()
            .map(|segment| {
                let Some(token) = segment.syntax().first_token() else {
                    return false;
                };
                let written = token.text();
                !token.has_error()
                    && !invalid_raw_identifier(written)
                    && (written.starts_with("r#") || !sai_syntax::is_reserved_identifier(written))
            })
            .reduce(|all, segment| all && segment)
            .unwrap_or(false)
}

fn borrows_clause_is_well_formed(clause: &ast::BorrowsClause) -> bool {
    !contains_error_node(clause.syntax())
        && has_direct_token(clause.syntax(), SyntaxKind::L_BRACE)
        && has_direct_token(clause.syntax(), SyntaxKind::R_BRACE)
        && clause.provenances().all(|provenance| {
            !contains_error_node(provenance.syntax()) && provenance.segments().next().is_some()
        })
}

/// Lower the currently supported expression forms and record their source spans.
///
/// Valid but not-yet-supported expressions are rejected before any HIR nodes
/// or source-map entries are allocated.
pub fn lower_expr<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    ensure_supported_expr(expression)?;
    if should_lower_expression_as_error(expression) {
        return lower_error_expr(expression.syntax(), arena, source_map);
    }
    prepare_nested_definition_ids(expression.syntax(), source_map)?;
    lower_expr_inner(expression, arena, symbols, source_map)
}

fn lower_expr_in_prepared_owner<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    ensure_supported_expr(expression)?;
    if should_lower_expression_as_error(expression) {
        return lower_error_expr(expression.syntax(), arena, source_map);
    }
    lower_expr_inner(expression, arena, symbols, source_map)
}

fn should_lower_expression_as_error(expression: &ast::Expr) -> bool {
    contains_error_node(expression.syntax())
        && !contains_call_arglist_error(expression.syntax())
        && !errors_are_in_recoverable_block_statements(expression.syntax())
        && !errors_are_recoverable_closure_contexts(expression.syntax())
        && !errors_are_in_recoverable_format_interpolations(expression.syntax())
        && !errors_are_in_casts(expression.syntax())
}

fn prepare_nested_definition_ids(
    expression_syntax: &SyntaxNode,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(), HirLoweringError> {
    prepare_nested_definition_ids_with_async_fn_body(expression_syntax, None, source_map, false)?;
    Ok(())
}

fn prepare_nested_definition_ids_with_async_fn_body(
    expression_syntax: &SyntaxNode,
    async_fn_body_syntax: Option<&SyntaxNode>,
    source_map: &mut HirSourceMapBuilder<'_>,
    require_interner: bool,
) -> Result<Option<LocalDefId>, HirLoweringError> {
    let owner_pointer = source_map.pointer_for(source_map.owner_syntax())?;
    let async_fn_body_pointer = async_fn_body_syntax
        .map(|syntax| source_map.pointer_for(syntax))
        .transpose()?;
    let mut nested_definitions = Vec::new();
    for node in source_map.owner_syntax().descendants() {
        if is_nested_definition_kind(node.kind())
            && nested_definition_belongs_to_owner(&node, &owner_pointer, source_map)?
        {
            let pointer = source_map.pointer_for(&node)?;
            nested_definitions.push((node, pointer));
        }
    }
    let nested_definition_pointers = nested_definitions
        .iter()
        .map(|(_, pointer)| pointer.clone())
        .collect::<HashSet<_>>();
    let expression_pointer = source_map.pointer_for(expression_syntax)?;
    let mut first_owned_definition_kind = None;
    for node in expression_syntax
        .descendants()
        .filter(|node| is_nested_definition_kind(node.kind()))
    {
        let pointer = source_map.pointer_for(&node)?;
        if nested_definition_pointers.contains(&pointer) {
            first_owned_definition_kind.get_or_insert(node.kind());
            continue;
        }
        if pointer == expression_pointer
            || !nested_definition_is_in_foreign_owner(&node, &owner_pointer, source_map)?
        {
            return Err(HirLoweringError::UnsupportedSyntax(node.kind()));
        }
    }
    if first_owned_definition_kind.is_none() && async_fn_body_pointer.is_none() {
        return Ok(None);
    };
    let Some(def_paths) = source_map.def_path_interner() else {
        return Err(if require_interner {
            HirLoweringError::DefPathInternerRequired
        } else {
            HirLoweringError::UnsupportedSyntax(
                first_owned_definition_kind.unwrap_or(SyntaxKind::FN_DECL),
            )
        });
    };
    let async_fn_def_id = async_fn_body_pointer
        .as_ref()
        .map(|_| {
            def_paths
                .intern_nested(source_map.owner().def_id, DefPathData::AsyncBlock, 0)
                .map_err(HirLoweringError::from)
        })
        .transpose()?;
    let disambiguator_offset = usize::from(async_fn_def_id.is_some());
    let mut ids_by_syntax = HashMap::<SyntaxNodePtr, LocalDefId>::new();

    for (ordinal, (node, pointer)) in nested_definitions.iter().enumerate() {
        let ordinal = ordinal
            .checked_add(disambiguator_offset)
            .ok_or(DefPathInternerError::TooManyDefinitions)?;
        let disambiguator =
            u32::try_from(ordinal).map_err(|_| DefPathInternerError::TooManyDefinitions)?;
        let component = match node.kind() {
            SyntaxKind::CLOSURE_EXPR => DefPathData::Closure,
            SyntaxKind::ASYNC_BLOCK_EXPR => DefPathData::AsyncBlock,
            _ => unreachable!("only nested definition nodes are collected"),
        };

        let mut ancestor = node.parent();
        let parent = if pointer == &owner_pointer {
            source_map.owner().def_id
        } else {
            loop {
                let Some(candidate) = ancestor else {
                    break source_map.owner().def_id;
                };
                let candidate_pointer = source_map.pointer_for(&candidate)?;
                if async_fn_body_pointer.as_ref() == Some(&candidate_pointer) {
                    break async_fn_def_id.ok_or(HirSourceMapError::NestedDefinitionIdMismatch)?;
                }
                if matches!(
                    candidate.kind(),
                    SyntaxKind::CLOSURE_EXPR | SyntaxKind::ASYNC_BLOCK_EXPR
                ) {
                    if candidate_pointer == owner_pointer {
                        break source_map.owner().def_id;
                    }
                    break ids_by_syntax
                        .get(&candidate_pointer)
                        .copied()
                        .ok_or(HirSourceMapError::NestedDefinitionIdMismatch)?;
                }
                if candidate_pointer == owner_pointer {
                    break source_map.owner().def_id;
                }
                ancestor = candidate.parent();
            }
        };

        let id = def_paths.intern_nested(parent, component, disambiguator)?;
        source_map.record_nested_definition(node, id)?;
        ids_by_syntax.insert(pointer.clone(), id);
    }
    Ok(async_fn_def_id)
}

fn is_nested_definition_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::CLOSURE_EXPR | SyntaxKind::ASYNC_BLOCK_EXPR
    )
}

fn nested_definition_belongs_to_owner(
    node: &SyntaxNode,
    owner_pointer: &SyntaxNodePtr,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<bool, HirLoweringError> {
    let mut ancestor = Some(node.clone());
    while let Some(candidate) = ancestor {
        let pointer = source_map.pointer_for(&candidate)?;
        if &pointer == owner_pointer {
            return Ok(true);
        }
        if is_hir_owner_syntax(candidate.kind()) {
            return Ok(false);
        }
        ancestor = candidate.parent();
    }
    Ok(false)
}

fn nested_definition_is_in_foreign_owner(
    node: &SyntaxNode,
    owner_pointer: &SyntaxNodePtr,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<bool, HirLoweringError> {
    let mut ancestor = Some(node.clone());
    while let Some(candidate) = ancestor {
        let pointer = source_map.pointer_for(&candidate)?;
        if &pointer == owner_pointer {
            return Ok(false);
        }
        if is_hir_owner_syntax(candidate.kind()) {
            return Ok(true);
        }
        ancestor = candidate.parent();
    }
    Ok(false)
}

fn is_hir_owner_syntax(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FN_DECL
            | SyntaxKind::TYPE_DECL
            | SyntaxKind::ENUM_DECL
            | SyntaxKind::DISTINCT_DECL
            | SyntaxKind::RESOURCE_DECL
            | SyntaxKind::TRAIT_DECL
            | SyntaxKind::ASSOC_TYPE_DECL
            | SyntaxKind::IMPL_DECL
            | SyntaxKind::ASSOC_TYPE_DEF
            | SyntaxKind::CONST_DECL
            | SyntaxKind::INVARIANT_DECL
            | SyntaxKind::EFFECT_DECL
            | SyntaxKind::INTERFACE_DECL
            | SyntaxKind::IFACE_RESOURCE
            | SyntaxKind::WORLD_DECL
            | SyntaxKind::WORLD_IMPORT
            | SyntaxKind::WORLD_EXPORT
            | SyntaxKind::EXTERN_MODULE
            | SyntaxKind::TEST_DECL
            | SyntaxKind::PROPERTY_DECL
    )
}

fn lower_expr_inner<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    match expression {
        ast::Expr::Path(path) if is_contract_result(path) => {
            lower_contract_result(path, arena, source_map)
        }
        ast::Expr::Call(call)
            if is_inside_ensures(call.syntax()) && is_contract_old_callee(call) =>
        {
            lower_contract_old(call, arena, symbols, source_map)
        }
        ast::Expr::Literal(literal) => lower_literal(literal, arena, symbols, source_map),
        ast::Expr::FormatString(format_string) => {
            lower_format_string_expr(format_string, arena, symbols, source_map)
        }
        ast::Expr::Closure(closure) => lower_closure_expr(closure, arena, symbols, source_map),
        ast::Expr::AsyncBlock(async_block) => {
            lower_async_block_expr(async_block, arena, symbols, source_map)
        }
        ast::Expr::Binary(binary) => lower_binary_expr(binary, arena, symbols, source_map),
        ast::Expr::Prefix(prefix) => lower_prefix_expr(prefix, arena, symbols, source_map),
        ast::Expr::Range(range) => lower_range_expr(range, arena, symbols, source_map),
        ast::Expr::Index(index) => lower_index_expr(index, arena, symbols, source_map),
        ast::Expr::Try(try_expr) => lower_try_expr(try_expr, arena, symbols, source_map),
        ast::Expr::Await(await_expr) => lower_await_expr(await_expr, arena, symbols, source_map),
        ast::Expr::Call(call) => lower_call_expr(call, arena, symbols, source_map),
        ast::Expr::Return(return_expr) => {
            lower_return_expr(return_expr, arena, symbols, source_map)
        }
        ast::Expr::Break(break_expr) => lower_break_expr(break_expr, arena, symbols, source_map),
        ast::Expr::Continue(continue_expr) => {
            lower_continue_expr(continue_expr, arena, symbols, source_map)
        }
        ast::Expr::Block(block_expr) => lower_block_expr(block_expr, arena, symbols, source_map),
        ast::Expr::UnsafeBlock(block_expr) => {
            lower_unsafe_block_expr(block_expr, arena, symbols, source_map)
        }
        ast::Expr::Region(region_expr) => {
            lower_region_expr(region_expr, arena, symbols, source_map)
        }
        ast::Expr::If(if_expr) => lower_if_expr(if_expr, arena, symbols, source_map),
        ast::Expr::Match(match_expr) => lower_match_expr(match_expr, arena, symbols, source_map),
        ast::Expr::Loop(loop_expr) => lower_loop_expr(loop_expr, arena, symbols, source_map),
        ast::Expr::While(while_expr) => lower_while_expr(while_expr, arena, symbols, source_map),
        ast::Expr::For(for_expr) => lower_for_expr(for_expr, arena, symbols, source_map),
        ast::Expr::Cast(cast) => lower_cast_expr(cast, arena, symbols, source_map),
        ast::Expr::Path(path) => lower_path_expr(path, arena, symbols, source_map),
        ast::Expr::SelfValue(self_value) => {
            lower_root_path_expr(self_value.syntax(), PathRoot::SelfValue, arena, source_map)
        }
        ast::Expr::SelfType(self_type) => {
            lower_root_path_expr(self_type.syntax(), PathRoot::SelfType, arena, source_map)
        }
        ast::Expr::ImplicitMember(member) => {
            lower_implicit_member_expr(member, arena, symbols, source_map)
        }
        ast::Expr::Field(field) => lower_field_expr(field, arena, symbols, source_map),
        ast::Expr::TupleIndex(index) => lower_tuple_index_expr(index, arena, symbols, source_map),
        ast::Expr::Unit(unit) => {
            let (hir_id, span) = source_map.add_node_with_span(unit.syntax(), SpanCtxt::Root)?;
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Tuple(arena.alloc_exprs(&[])),
                span,
            }))
        }
        ast::Expr::Paren(paren) => match paren.expression() {
            Some(inner) => lower_expr_inner(&inner, arena, symbols, source_map),
            None => lower_error_expr(paren.syntax(), arena, source_map),
        },
        ast::Expr::Tuple(tuple) => {
            let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
            let mut elements = Vec::new();
            for element in collect_expr_list_elements(tuple.syntax()) {
                let lowered = match element {
                    ExprListElementSyntax::Expression(expression) => {
                        lower_expr_inner(&expression, arena, symbols, source_map)?
                    }
                    ExprListElementSyntax::Error(syntax) => {
                        lower_error_expr(&syntax, arena, source_map)?
                    }
                };
                elements.push(*lowered);
            }
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Tuple(arena.alloc_exprs(&elements)),
                span,
            }))
        }
        ast::Expr::Array(array) => {
            let (hir_id, span) = source_map.add_node_with_span(array.syntax(), SpanCtxt::Root)?;
            let mut elements = Vec::new();
            for element in collect_expr_list_elements(array.syntax()) {
                let lowered = match element {
                    ExprListElementSyntax::Expression(expression) => {
                        lower_expr_inner(&expression, arena, symbols, source_map)?
                    }
                    ExprListElementSyntax::Error(syntax) => {
                        lower_error_expr(&syntax, arena, source_map)?
                    }
                };
                elements.push(*lowered);
            }
            Ok(arena.alloc_expr(Expr {
                hir_id,
                kind: ExprKind::Array(arena.alloc_exprs(&elements)),
                span,
            }))
        }
        _ if contains_error_node(expression.syntax()) => {
            lower_error_expr(expression.syntax(), arena, source_map)
        }
        _ => Err(HirLoweringError::UnsupportedSyntax(
            expression.syntax().kind(),
        )),
    }
}

fn lower_contract_result<'hir>(
    path: &ast::PathExpr,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(path.syntax(), SpanCtxt::Root)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::ContractResult,
        span,
    }))
}

fn lower_contract_old<'hir>(
    call: &ast::CallExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(args) = collect_call_args(call) else {
        return lower_error_expr(call.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(call.syntax(), SpanCtxt::Root)?;
    let mut lowered_args = Vec::with_capacity(args.len());
    let mut base = None;
    for arg in args {
        match arg {
            CallArgSyntax::Positional(expression) => {
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::PositionalError(syntax) => {
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::Named { name, expression } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::NamedError { name, syntax } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::Base(expression) => {
                base = Some(lower_expr_inner(&expression, arena, symbols, source_map)?);
            }
            CallArgSyntax::BaseError(syntax) => {
                base = Some(lower_error_expr(&syntax, arena, source_map)?);
            }
        }
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::ContractOld {
            args: arena.alloc_args(&lowered_args),
            base,
        },
        span,
    }))
}

fn is_contract_result(path: &ast::PathExpr) -> bool {
    is_inside_ensures(path.syntax())
        && path.generic_args().is_none()
        && path.name().is_some_and(|name| {
            name.syntax()
                .first_token()
                .is_some_and(|token| !token.has_error() && token.text() == "result")
        })
}

fn is_contract_old_callee(call: &ast::CallExpr) -> bool {
    let Some(ast::Expr::Path(path)) = call.callee() else {
        return false;
    };
    path.generic_args().is_none()
        && path.name().is_some_and(|name| {
            name.syntax()
                .first_token()
                .is_some_and(|token| !token.has_error() && token.text() == "old")
        })
}

fn is_inside_ensures(syntax: &SyntaxNode) -> bool {
    let mut ancestor = syntax.parent();
    while let Some(node) = ancestor {
        if node.kind() == SyntaxKind::ENSURES_CLAUSE {
            return true;
        }
        if is_hir_owner_syntax(node.kind()) || is_nested_definition_kind(node.kind()) {
            return false;
        }
        ancestor = node.parent();
    }
    false
}

fn lower_path_expr<'hir>(
    path: &ast::PathExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(name) = path.name() else {
        return lower_error_expr(path.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(path.syntax(), SpanCtxt::Root)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(path.syntax(), SpanCtxt::Root)?;
    let (segment_hir_id, _) = source_map.add_node_with_span(name.syntax(), SpanCtxt::Root)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    let args = path
        .generic_args()
        .map(|args| lower_generic_args(&args, arena, symbols, source_map))
        .transpose()?;
    let segment = PathSegment {
        hir_id: segment_hir_id,
        name,
        args,
    };
    let path = Path {
        hir_id: path_hir_id,
        root: PathRoot::Plain,
        segments: arena.alloc_path_segments(&[segment]),
        span: path_span,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(path),
        span,
    }))
}

fn lower_root_path_expr<'hir>(
    syntax: &SyntaxNode,
    root: PathRoot,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let path = Path {
        hir_id: path_hir_id,
        root,
        segments: arena.alloc_path_segments(&[]),
        span: path_span,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(path),
        span,
    }))
}

fn lower_implicit_member_expr<'hir>(
    member: &ast::ImplicitMemberExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(name) = member.name() else {
        return lower_error_expr(member.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(member.syntax(), SpanCtxt::Root)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::ImplicitMember(name),
        span,
    }))
}

fn lower_field_expr<'hir>(
    field: &ast::FieldExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(base), Some(name)) = (field.base(), field.name()) else {
        return lower_error_expr(field.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(field.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    let args = direct_generic_args(field.syntax())
        .map(|args| lower_generic_args(&args, arena, symbols, source_map))
        .transpose()?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Field { base, name, args },
        span,
    }))
}

fn lower_tuple_index_expr<'hir>(
    index: &ast::TupleIndexExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(base) = index.base() else {
        return lower_error_expr(index.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(index.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::TupleField {
            base,
            index: index.index(),
        },
        span,
    }))
}

fn lower_binary_expr<'hir>(
    binary: &ast::BinExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(lhs), Some(rhs), Some((_, op))) = (binary.lhs(), binary.rhs(), binary.op()) else {
        return lower_error_expr(binary.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(binary.syntax(), SpanCtxt::Root)?;
    let lhs = lower_expr_inner(&lhs, arena, symbols, source_map)?;
    let rhs = lower_expr_inner(&rhs, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Binary(lower_bin_op(op), lhs, rhs),
        span,
    }))
}

fn lower_prefix_expr<'hir>(
    prefix: &ast::PrefixExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some((_, op)), Some(operand)) = (prefix.op(), prefix.operand()) else {
        return lower_error_expr(prefix.syntax(), arena, source_map);
    };
    if op == ast::PrefixOp::Neg
        && let ast::Expr::Literal(ref literal) = operand
        && matches!(
            literal.token(),
            Some(ast::LiteralToken::Int(_) | ast::LiteralToken::Float(_))
        )
    {
        return lower_literal_with_sign(literal, prefix.syntax(), true, arena, symbols, source_map);
    }

    let (hir_id, span) = source_map.add_node_with_span(prefix.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    let kind = match op {
        ast::PrefixOp::Neg => ExprKind::Unary(UnOp::Neg, operand),
        ast::PrefixOp::BitNot => ExprKind::Unary(UnOp::BitNot, operand),
        ast::PrefixOp::Deref => ExprKind::Unary(UnOp::Deref, operand),
        ast::PrefixOp::Not => ExprKind::Unary(UnOp::Not, operand),
        ast::PrefixOp::Borrow => ExprKind::Borrow(Mutability::Not, operand),
        ast::PrefixOp::BorrowMut => ExprKind::Borrow(Mutability::Mut, operand),
    };
    Ok(arena.alloc_expr(Expr { hir_id, kind, span }))
}

fn lower_range_expr<'hir>(
    range: &ast::RangeExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(inclusive) = range_inclusive(range.syntax()) else {
        return lower_error_expr(range.syntax(), arena, source_map);
    };
    if inclusive && range.end().is_none() {
        return lower_error_expr(range.syntax(), arena, source_map);
    }
    let (hir_id, span) = source_map.add_node_with_span(range.syntax(), SpanCtxt::Root)?;
    let start = range
        .start()
        .map(|start| lower_expr_inner(&start, arena, symbols, source_map))
        .transpose()?;
    let end = range
        .end()
        .map(|end| lower_expr_inner(&end, arena, symbols, source_map))
        .transpose()?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Range {
            start,
            end,
            inclusive,
        },
        span,
    }))
}

fn range_inclusive(syntax: &SyntaxNode) -> Option<bool> {
    syntax
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2 => Some(false),
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2EQ => Some(true),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
}

fn lower_index_expr<'hir>(
    index: &ast::IndexExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(base), Some(index_expr)) = (index.base(), index.index()) else {
        return lower_error_expr(index.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(index.syntax(), SpanCtxt::Root)?;
    let base = lower_expr_inner(&base, arena, symbols, source_map)?;
    let index_expr = lower_expr_inner(&index_expr, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Index(base, index_expr),
        span,
    }))
}

fn lower_try_expr<'hir>(
    try_expr: &ast::TryExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(operand) = try_expr.operand() else {
        return lower_error_expr(try_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(try_expr.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Try(operand),
        span,
    }))
}

fn lower_await_expr<'hir>(
    await_expr: &ast::AwaitExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(operand) = await_expr.operand() else {
        return lower_error_expr(await_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(await_expr.syntax(), SpanCtxt::Root)?;
    let operand = lower_expr_inner(&operand, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Await(operand),
        span,
    }))
}

enum CallArgSyntax {
    Positional(ast::Expr),
    PositionalError(SyntaxNode),
    Named {
        name: ast::NameRef,
        expression: ast::Expr,
    },
    NamedError {
        name: ast::NameRef,
        syntax: SyntaxNode,
    },
    Base(ast::Expr),
    BaseError(SyntaxNode),
}

enum ExprListElementSyntax {
    Expression(ast::Expr),
    Error(SyntaxNode),
}

enum BlockStatementSyntax {
    Statement(ast::Stmt),
    Error(SyntaxNode),
}

enum MatchArmSyntax {
    Arm(ast::MatchArm),
    Error(SyntaxNode),
}

fn collect_expr_list_elements(syntax: &SyntaxNode) -> Vec<ExprListElementSyntax> {
    let mut elements = Vec::new();
    for child in syntax.children() {
        if let Some(expression) = ast::Expr::cast(child.clone()) {
            elements.push(ExprListElementSyntax::Expression(expression));
        } else if child.kind() == SyntaxKind::ERROR && !contains_attribute(&child) {
            elements.push(ExprListElementSyntax::Error(child));
        }
    }
    elements
}

fn collect_block_statements(syntax: &SyntaxNode) -> Vec<BlockStatementSyntax> {
    let mut statements = Vec::new();
    for child in syntax.children() {
        if let Some(statement) = ast::Stmt::cast(child.clone()) {
            statements.push(BlockStatementSyntax::Statement(statement));
        } else if child.kind() == SyntaxKind::ERROR {
            statements.push(BlockStatementSyntax::Error(child));
        }
    }
    statements
}

fn collect_match_arms(match_expr: &ast::MatchExpr) -> Vec<MatchArmSyntax> {
    match match_expr.arms() {
        Some(arms) => arms
            .syntax()
            .children()
            .filter_map(|child| {
                ast::MatchArm::cast(child.clone())
                    .map(MatchArmSyntax::Arm)
                    .or_else(|| {
                        (child.kind() == SyntaxKind::ERROR)
                            .then_some(child)
                            .map(MatchArmSyntax::Error)
                    })
            })
            .collect(),
        None => {
            let mut errors = match_expr
                .syntax()
                .children()
                .filter(|child| child.kind() == SyntaxKind::ERROR)
                .collect::<Vec<_>>();
            if match_expr.scrutinee().is_none()
                && let Some(scrutinee_error) =
                    errors.iter().position(|error| !contains_attribute(error))
            {
                errors.remove(scrutinee_error);
            }
            if errors.is_empty() {
                vec![MatchArmSyntax::Error(match_expr.syntax().clone())]
            } else {
                errors.into_iter().map(MatchArmSyntax::Error).collect()
            }
        }
    }
}

fn collect_call_args(call: &ast::CallExpr) -> Option<Vec<CallArgSyntax>> {
    let args = call.args()?;
    let mut collected = Vec::new();
    let mut has_base = false;
    for child in args.syntax().children() {
        if let Some(arg) = ast::Arg::cast(child.clone()) {
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Positional(expression));
            } else {
                collected.push(CallArgSyntax::PositionalError(direct_error_child(
                    arg.syntax(),
                )?));
            }
        } else if let Some(arg) = ast::NamedArg::cast(child.clone()) {
            let name = arg.name()?;
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Named { name, expression });
            } else {
                collected.push(CallArgSyntax::NamedError {
                    name,
                    syntax: direct_error_child(arg.syntax())?,
                });
            }
        } else if let Some(arg) = ast::BaseArg::cast(child.clone()) {
            if has_base {
                return None;
            }
            has_base = true;
            if let Some(expression) = arg.expression() {
                collected.push(CallArgSyntax::Base(expression));
            } else {
                collected.push(CallArgSyntax::BaseError(direct_error_child(arg.syntax())?));
            }
        } else if child.kind() == SyntaxKind::ERROR && !contains_attribute(&child) {
            // Recovery creates no Arg node for an empty expression such as
            // `call(, value)`. Keep its position so later stages see an Err
            // argument instead of a call with a silently shortened list.
            collected.push(CallArgSyntax::PositionalError(child));
        }
    }
    Some(collected)
}

fn call_argument_expressions(call: &ast::CallExpr) -> Vec<ast::Expr> {
    let Some(args) = call.args() else {
        return Vec::new();
    };
    args.syntax()
        .children()
        .filter_map(|child| {
            ast::Arg::cast(child.clone())
                .and_then(|arg| arg.expression())
                .or_else(|| ast::NamedArg::cast(child.clone()).and_then(|arg| arg.expression()))
                .or_else(|| ast::BaseArg::cast(child).and_then(|arg| arg.expression()))
        })
        .collect()
}

fn direct_error_child(syntax: &SyntaxNode) -> Option<SyntaxNode> {
    syntax
        .children()
        .find(|child| child.kind() == SyntaxKind::ERROR && !contains_attribute(child))
}

fn is_missing_tuple_close_error(tuple: &ast::TupleType, error: &SyntaxNode) -> bool {
    if !is_missing_close_delimiter_error(tuple.syntax(), error, SyntaxKind::R_PAREN) {
        return false;
    }
    let item_count = tuple.elements().count()
        + tuple
            .syntax()
            .children()
            .filter(|child| child.kind() == SyntaxKind::ERROR)
            .count()
            .saturating_sub(1);
    let comma_count = tuple
        .syntax()
        .children_with_tokens()
        .filter(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA))
        .count()
        + unconsumed_commas_at_error_boundary(tuple.syntax(), error);
    comma_count <= item_count
}

fn is_missing_generic_close_error(args: &ast::GenericArgList, error: &SyntaxNode) -> bool {
    if !is_missing_close_delimiter_error(args.syntax(), error, SyntaxKind::GT) {
        return false;
    }
    let children = args.syntax().children().collect::<Vec<_>>();
    let item_count = children
        .iter()
        .filter(|child| {
            ast::TypeArg::can_cast(child.kind())
                || ast::ConstArg::can_cast(child.kind())
                || ast::AssocTypeBinding::can_cast(child.kind())
                || child.kind() == SyntaxKind::ERROR
        })
        .count()
        .saturating_sub(1);
    let comma_count = args
        .syntax()
        .children_with_tokens()
        .filter(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA))
        .count()
        + unconsumed_commas_at_error_boundary(args.syntax(), error);
    comma_count <= item_count
}

fn unconsumed_commas_at_error_boundary(syntax: &SyntaxNode, error: &SyntaxNode) -> usize {
    let Some(mut token) = token_at_error_boundary(syntax, error) else {
        return 0;
    };
    if comma_belongs_to_enclosing_list(&token, syntax) {
        return 0;
    }
    let mut commas = 0;
    loop {
        if token.kind() != SyntaxKind::COMMA {
            break;
        }
        commas += 1;
        let Some(next) = next_syntax_token(&token) else {
            break;
        };
        if comma_belongs_to_enclosing_list(&next, syntax) {
            break;
        }
        token = next;
    }
    commas
}

fn comma_belongs_to_enclosing_list(token: &SyntaxToken, syntax: &SyntaxNode) -> bool {
    // Commas consumed as list separators are direct children of the owning
    // syntax node; recovery-wrapped commas still describe an inner empty slot.
    let owner = token.parent();
    token.kind() == SyntaxKind::COMMA
        && owner.kind() != SyntaxKind::ERROR
        && (owner.kind() != syntax.kind() || owner.text_range() != syntax.text_range())
}

fn boundary_comma_error(syntax: &SyntaxNode) -> Option<SyntaxNode> {
    syntax.children().find(|child| {
        child.kind() == SyntaxKind::ERROR
            && child.text_range().is_empty()
            && child.text_range().start() >= syntax.text_range().end()
            && token_at_error_boundary(syntax, child)
                .is_some_and(|token| token.kind() == SyntaxKind::COMMA)
    })
}

fn token_at_error_boundary(syntax: &SyntaxNode, error: &SyntaxNode) -> Option<SyntaxToken> {
    let boundary = error.text_range().start();
    let root = syntax.ancestors().last()?;
    match root.token_at_offset(boundary) {
        TokenAtOffset::Single(token) if token.text_range().start() == boundary => Some(token),
        TokenAtOffset::Between(_, right) if right.text_range().start() == boundary => Some(right),
        TokenAtOffset::Between(left, _) if left.text_range().start() == boundary => Some(left),
        _ => None,
    }
}

fn next_syntax_token(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut parent = token.parent().clone();
    let mut index = token.index_in_parent();
    loop {
        if let Some(next) = parent
            .children_with_tokens()
            .skip(index as usize + 1)
            .find_map(|element| match element {
                SyntaxElement::Token(token) => Some(token),
                SyntaxElement::Node(node) => node.first_token(),
            })
        {
            return Some(next);
        }
        index = parent.index_in_parent()?;
        parent = parent.parent()?;
    }
}

fn is_missing_close_delimiter_error(
    syntax: &SyntaxNode,
    error: &SyntaxNode,
    close_kind: SyntaxKind,
) -> bool {
    if !error.text_range().is_empty()
        || error.text_range().start() < syntax.text_range().end()
        || syntax.children_with_tokens().any(
            |element| matches!(element, SyntaxElement::Token(token) if token.kind() == close_kind),
        )
    {
        return false;
    }
    error.text_range().start() == syntax.text_range().end()
        || token_at_error_boundary(syntax, error).is_some_and(|token| {
            matches!(
                token.kind(),
                SyntaxKind::COMMA
                    | SyntaxKind::EOF
                    | SyntaxKind::GT
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::R_BRACE
                    | SyntaxKind::R_BRACKET
                    | SyntaxKind::R_PAREN
            )
        })
}

fn direct_error_child_from_end(syntax: &SyntaxNode) -> Option<SyntaxNode> {
    syntax
        .children()
        .filter(|child| child.kind() == SyntaxKind::ERROR && !contains_attribute(child))
        .collect::<Vec<_>>()
        .into_iter()
        .next_back()
}

fn contains_attribute(syntax: &SyntaxNode) -> bool {
    syntax
        .descendants()
        .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
}

fn contains_call_arglist_error(syntax: &SyntaxNode) -> bool {
    let root_call = ast::CallExpr::cast(syntax.clone());
    root_call
        .into_iter()
        .chain(syntax.descendants().filter_map(ast::CallExpr::cast))
        .any(|call| {
            call.args()
                .is_some_and(|args| contains_error_node(args.syntax()))
        })
}

fn lower_call_expr<'hir>(
    call: &ast::CallExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(callee), Some(args)) = (call.callee(), collect_call_args(call)) else {
        return lower_error_expr(call.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(call.syntax(), SpanCtxt::Root)?;
    let callee = lower_expr_inner(&callee, arena, symbols, source_map)?;
    let mut lowered_args = Vec::with_capacity(args.len());
    let mut base = None;
    for arg in args {
        match arg {
            CallArgSyntax::Positional(expression) => {
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::PositionalError(syntax) => {
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: None,
                    expr: expression,
                });
            }
            CallArgSyntax::Named { name, expression } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_expr_inner(&expression, arena, symbols, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::NamedError { name, syntax } => {
                let name = lower_name_ref(&name, symbols, source_map)?;
                let expression = lower_error_expr(&syntax, arena, source_map)?;
                lowered_args.push(Arg {
                    name: Some(name),
                    expr: expression,
                });
            }
            CallArgSyntax::Base(expression) => {
                base = Some(lower_expr_inner(&expression, arena, symbols, source_map)?);
            }
            CallArgSyntax::BaseError(syntax) => {
                base = Some(lower_error_expr(&syntax, arena, source_map)?);
            }
        }
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Call {
            callee,
            args: arena.alloc_args(&lowered_args),
            base,
        },
        span,
    }))
}

fn lower_return_expr<'hir>(
    return_expr: &ast::ReturnExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(return_expr.syntax(), SpanCtxt::Root)?;
    let value = lower_optional_expr_value(
        return_expr.syntax(),
        return_expr.value(),
        arena,
        symbols,
        source_map,
    )?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Return(value),
        span,
    }))
}

fn lower_break_expr<'hir>(
    break_expr: &ast::BreakExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(break_expr.syntax(), SpanCtxt::Root)?;
    let label = lower_label(break_expr.label(), symbols, source_map)?;
    let value = lower_optional_expr_value(
        break_expr.syntax(),
        break_expr.value(),
        arena,
        symbols,
        source_map,
    )?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Break { label, value },
        span,
    }))
}

fn lower_optional_expr_value<'hir>(
    syntax: &SyntaxNode,
    value: Option<ast::Expr>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Option<&'hir Expr<'hir>>, HirLoweringError> {
    if let Some(value) = value {
        Ok(Some(lower_expr_inner(&value, arena, symbols, source_map)?))
    } else if let Some(error) = direct_error_child(syntax) {
        Ok(Some(lower_error_expr(&error, arena, source_map)?))
    } else {
        Ok(None)
    }
}

fn lower_continue_expr<'hir>(
    continue_expr: &ast::ContinueExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(continue_expr.syntax(), SpanCtxt::Root)?;
    let label = lower_label(continue_expr.label(), symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Continue { label },
        span,
    }))
}

fn lower_block_expr<'hir>(
    block_expr: &ast::BlockExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(block) = block_expr.block() else {
        return lower_error_expr(block_expr.syntax(), arena, source_map);
    };
    lower_block_as_expr(block_expr.syntax(), &block, arena, symbols, source_map)
}

fn lower_unsafe_block_expr<'hir>(
    block_expr: &ast::UnsafeBlockExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(block) = block_expr.body() else {
        return lower_error_expr(block_expr.syntax(), arena, source_map);
    };
    lower_block_as_expr_with_safety(
        block_expr.syntax(),
        &block,
        Safety::Unsafe,
        arena,
        symbols,
        source_map,
    )
}

fn lower_region_expr<'hir>(
    region_expr: &ast::RegionExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(region_name), Some(body)) = (region_expr.name(), region_expr.body()) else {
        return lower_error_expr(region_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(region_expr.syntax(), SpanCtxt::Root)?;
    let name = match region_name.token() {
        Some(token) => lower_name_spelling(
            region_name.syntax(),
            Some(token.text()),
            symbols,
            source_map,
        )?,
        None => lower_name_spelling(region_name.syntax(), None, symbols, source_map)?,
    };
    let binding = lower_binding_id(region_name.syntax(), source_map)?;
    let block = lower_block(&body, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Region {
            name,
            binding,
            block,
        },
        span,
    }))
}

fn lower_block_as_expr<'hir>(
    syntax: &SyntaxNode,
    block: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    lower_block_as_expr_with_safety(syntax, block, Safety::Safe, arena, symbols, source_map)
}

fn lower_block_as_expr_with_safety<'hir>(
    syntax: &SyntaxNode,
    block: &ast::Block,
    safety: Safety,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let block = lower_block_with_safety(block, safety, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Block(block),
        span,
    }))
}

fn lower_if_expr<'hir>(
    if_expr: &ast::IfExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(then_branch) = if_expr.then_branch() else {
        return lower_error_expr(if_expr.syntax(), arena, source_map);
    };
    if let Some(let_condition) = if_expr
        .condition()
        .and_then(|condition| condition.let_condition())
    {
        return lower_if_let_expr(
            if_expr,
            &let_condition,
            &then_branch,
            arena,
            symbols,
            source_map,
        );
    }
    let (hir_id, span) = source_map.add_node_with_span(if_expr.syntax(), SpanCtxt::Root)?;
    let cond = match if_expr.condition() {
        Some(condition) => match condition.expression() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
            None => match direct_error_child(condition.syntax()) {
                Some(error) => lower_error_expr(&error, arena, source_map)?,
                None => lower_error_expr(condition.syntax(), arena, source_map)?,
            },
        },
        None => lower_error_expr(if_expr.syntax(), arena, source_map)?,
    };
    let then_branch = lower_block(&then_branch, arena, symbols, source_map)?;
    let els = if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            Some(lower_block_as_expr(
                block.syntax(),
                &block,
                arena,
                symbols,
                source_map,
            )?)
        } else if let Some(else_if) = else_branch.else_if() {
            Some(lower_if_expr(&else_if, arena, symbols, source_map)?)
        } else {
            let error = direct_error_child(else_branch.syntax())
                .unwrap_or_else(|| else_branch.syntax().clone());
            Some(lower_error_expr(&error, arena, source_map)?)
        }
    } else {
        None
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::If {
            cond,
            then: then_branch,
            els,
        },
        span,
    }))
}

fn lower_loop_expr<'hir>(
    loop_expr: &ast::LoopExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(body) = loop_expr.body() else {
        return lower_error_expr(loop_expr.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(loop_expr.syntax(), SpanCtxt::Root)?;
    let label = lower_label(
        loop_expr.label().and_then(|label| label.label()),
        symbols,
        source_map,
    )?;
    let body = lower_block(&body, arena, symbols, source_map)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Loop {
            body,
            label,
            source: HirLoopSource::Loop,
        },
        span,
    }))
}

fn lower_while_expr<'hir>(
    while_expr: &ast::WhileExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let Some(body) = while_expr.body() else {
        return lower_error_expr(while_expr.syntax(), arena, source_map);
    };
    let Some(condition) = while_expr.condition() else {
        return lower_error_expr(while_expr.syntax(), arena, source_map);
    };

    let let_condition = condition.let_condition();
    let desugaring = if let_condition.is_some() {
        DesugaringKind::WhileLet
    } else {
        DesugaringKind::WhileLoop
    };
    let ctxt = SpanCtxt::Desugared(desugaring);
    let (hir_id, span) = source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
    let label = lower_label(
        while_expr.label().and_then(|label| label.label()),
        symbols,
        source_map,
    )?;
    let (loop_body_hir_id, loop_body_span) =
        source_map.add_node_with_span(while_expr.syntax(), ctxt)?;

    let tail = if let Some(let_condition) = let_condition {
        let (match_hir_id, match_span) =
            source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
        let scrutinee = match let_condition.initializer() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
            None => {
                let error = direct_error_child_from_end(let_condition.syntax())
                    .unwrap_or_else(|| let_condition.syntax().clone());
                lower_error_expr(&error, arena, source_map)?
            }
        };

        let (user_arm_hir_id, user_arm_span) =
            source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
        let pattern = match let_condition.pattern() {
            Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
            None => {
                let error = direct_error_child(let_condition.syntax())
                    .unwrap_or_else(|| let_condition.syntax().clone());
                lower_error_pattern(&error, arena, source_map)?
            }
        };
        let user_body = lower_block_as_expr(body.syntax(), &body, arena, symbols, source_map)?;
        let user_arm = HirArm {
            hir_id: user_arm_hir_id,
            pat: pattern,
            guard: None,
            body: user_body,
            span: user_arm_span,
            synthesized: false,
        };

        let (fallback_arm_hir_id, fallback_arm_span) =
            source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
        let (wildcard_hir_id, wildcard_span) =
            source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
        let wildcard = arena.alloc_pat(Pat {
            hir_id: wildcard_hir_id,
            kind: PatKind::Wild,
            span: wildcard_span,
        });
        let fallback_body = lower_synthetic_break(while_expr.syntax(), ctxt, arena, source_map)?;
        let fallback_arm = HirArm {
            hir_id: fallback_arm_hir_id,
            pat: wildcard,
            guard: None,
            body: fallback_body,
            span: fallback_arm_span,
            synthesized: true,
        };
        arena.alloc_expr(Expr {
            hir_id: match_hir_id,
            kind: ExprKind::Match {
                scrutinee,
                arms: arena.alloc_arms(&[user_arm, fallback_arm]),
                source: HirMatchSource::WhileLet,
            },
            span: match_span,
        })
    } else {
        let (if_hir_id, if_span) = source_map.add_node_with_span(while_expr.syntax(), ctxt)?;
        let cond = match condition.expression() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
            None => {
                let error = direct_error_child(condition.syntax())
                    .unwrap_or_else(|| condition.syntax().clone());
                lower_error_expr(&error, arena, source_map)?
            }
        };
        let then_branch = lower_block(&body, arena, symbols, source_map)?;
        let else_branch =
            lower_synthetic_break_block_expr(while_expr.syntax(), ctxt, arena, source_map)?;
        arena.alloc_expr(Expr {
            hir_id: if_hir_id,
            kind: ExprKind::If {
                cond,
                then: then_branch,
                els: Some(else_branch),
            },
            span: if_span,
        })
    };

    let loop_body = arena.alloc_block(HirBlock {
        hir_id: loop_body_hir_id,
        stmts: arena.alloc_stmts(&[]),
        tail: Some(tail),
        safety: Safety::Safe,
        span: loop_body_span,
    });
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Loop {
            body: loop_body,
            label,
            source: if desugaring == DesugaringKind::WhileLet {
                HirLoopSource::WhileLet
            } else {
                HirLoopSource::While
            },
        },
        span,
    }))
}

fn lower_synthetic_break<'hir>(
    syntax: &SyntaxNode,
    ctxt: SpanCtxt,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, ctxt)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Break {
            label: None,
            value: None,
        },
        span,
    }))
}

fn lower_synthetic_break_block_expr<'hir>(
    syntax: &SyntaxNode,
    ctxt: SpanCtxt,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (expr_hir_id, expr_span) = source_map.add_node_with_span(syntax, ctxt)?;
    let (block_hir_id, block_span) = source_map.add_node_with_span(syntax, ctxt)?;
    let break_expr = lower_synthetic_break(syntax, ctxt, arena, source_map)?;
    let block = arena.alloc_block(HirBlock {
        hir_id: block_hir_id,
        stmts: arena.alloc_stmts(&[]),
        tail: Some(break_expr),
        safety: Safety::Safe,
        span: block_span,
    });
    Ok(arena.alloc_expr(Expr {
        hir_id: expr_hir_id,
        kind: ExprKind::Block(block),
        span: expr_span,
    }))
}

fn lower_for_expr<'hir>(
    for_expr: &ast::ForExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (Some(pattern), Some(iterable), Some(body)) =
        (for_expr.pattern(), for_expr.iterable(), for_expr.body())
    else {
        return lower_error_expr(for_expr.syntax(), arena, source_map);
    };

    let await_token = for_expr
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::AWAIT_KW => Some(token),
            _ => None,
        });
    let for_await = await_token.is_some();
    let ctxt = SpanCtxt::Desugared(if for_await {
        DesugaringKind::ForAwait
    } else {
        DesugaringKind::ForLoop
    });
    let (match_hir_id, match_span) = source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let iterable_syntax = iterable.syntax();
    let outer_scrutinee = if for_await {
        lower_expr_inner(&iterable, arena, symbols, source_map)?
    } else {
        let (iter_call_hir_id, iter_call_span) =
            source_map.add_node_with_span(iterable_syntax, ctxt)?;
        let iter_callee = lower_synthetic_lang_path_expr(
            iterable_syntax,
            ctxt,
            LangItem::IntoIteratorIntoIter,
            arena,
            source_map,
        )?;
        let iterable = lower_expr_inner(&iterable, arena, symbols, source_map)?;
        arena.alloc_expr(Expr {
            hir_id: iter_call_hir_id,
            kind: ExprKind::Call {
                callee: iter_callee,
                args: arena.alloc_args(&[Arg {
                    name: None,
                    expr: iterable,
                }]),
                base: None,
            },
            span: iter_call_span,
        })
    };

    let (outer_arm_hir_id, outer_arm_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let (iter_pat_hir_id, iter_pat_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let iter_symbol = symbols.intern(if for_await { "%stream" } else { "%iter" })?;
    let iter_name_span = source_map.span_for(for_expr.syntax(), ctxt)?;
    let iter_name = Name {
        sym: iter_symbol,
        span: iter_name_span,
    };
    let iter_binding = source_map.add_binding(for_expr.syntax())?;
    let iter_pat = arena.alloc_pat(Pat {
        hir_id: iter_pat_hir_id,
        kind: PatKind::Binding {
            mode: HirBindingMode::Var,
            name: iter_name,
            binding: iter_binding,
            sub: None,
        },
        span: iter_pat_span,
    });

    let (loop_hir_id, loop_span) = source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let label = lower_label(
        for_expr.label().and_then(|label| label.label()),
        symbols,
        source_map,
    )?;
    let (loop_body_hir_id, loop_body_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let (next_match_hir_id, next_match_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let next_value = if for_await {
        let (await_hir_id, mut await_span) =
            source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
        await_span.range = await_token
            .as_ref()
            .expect("for-await syntax has its await token")
            .text_range();
        let (next_call_hir_id, next_call_span) =
            source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
        let next_callee = lower_synthetic_lang_path_expr(
            for_expr.syntax(),
            ctxt,
            LangItem::StreamNext,
            arena,
            source_map,
        )?;
        let (borrow_hir_id, borrow_span) =
            source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
        let stream_ref = lower_synthetic_local_path_expr(
            for_expr.syntax(),
            ctxt,
            iter_symbol,
            arena,
            source_map,
        )?;
        let stream_borrow = arena.alloc_expr(Expr {
            hir_id: borrow_hir_id,
            kind: ExprKind::Borrow(Mutability::Mut, stream_ref),
            span: borrow_span,
        });
        let next_call = arena.alloc_expr(Expr {
            hir_id: next_call_hir_id,
            kind: ExprKind::Call {
                callee: next_callee,
                args: arena.alloc_args(&[Arg {
                    name: None,
                    expr: stream_borrow,
                }]),
                base: None,
            },
            span: next_call_span,
        });
        arena.alloc_expr(Expr {
            hir_id: await_hir_id,
            kind: ExprKind::Await(next_call),
            span: await_span,
        })
    } else {
        let (next_call_hir_id, next_call_span) =
            source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
        let next_callee = lower_synthetic_lang_path_expr(
            for_expr.syntax(),
            ctxt,
            LangItem::IteratorNext,
            arena,
            source_map,
        )?;
        let (borrow_hir_id, borrow_span) =
            source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
        let iter_ref = lower_synthetic_local_path_expr(
            for_expr.syntax(),
            ctxt,
            iter_symbol,
            arena,
            source_map,
        )?;
        let iter_borrow = arena.alloc_expr(Expr {
            hir_id: borrow_hir_id,
            kind: ExprKind::Borrow(Mutability::Mut, iter_ref),
            span: borrow_span,
        });
        arena.alloc_expr(Expr {
            hir_id: next_call_hir_id,
            kind: ExprKind::Call {
                callee: next_callee,
                args: arena.alloc_args(&[Arg {
                    name: None,
                    expr: iter_borrow,
                }]),
                base: None,
            },
            span: next_call_span,
        })
    };

    let (some_arm_hir_id, some_arm_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let some_pat = lower_synthetic_lang_ctor_pat(
        for_expr.syntax(),
        ctxt,
        LangItem::OptionSome,
        Some(&pattern),
        arena,
        symbols,
        source_map,
    )?;
    let user_body = lower_block_as_expr(body.syntax(), &body, arena, symbols, source_map)?;
    let some_arm = HirArm {
        hir_id: some_arm_hir_id,
        pat: some_pat,
        guard: None,
        body: user_body,
        span: some_arm_span,
        synthesized: false,
    };

    let (none_arm_hir_id, none_arm_span) =
        source_map.add_node_with_span(for_expr.syntax(), ctxt)?;
    let none_pat = lower_synthetic_lang_ctor_pat(
        for_expr.syntax(),
        ctxt,
        LangItem::OptionNone,
        None,
        arena,
        symbols,
        source_map,
    )?;
    let none_body = lower_synthetic_break(for_expr.syntax(), ctxt, arena, source_map)?;
    let none_arm = HirArm {
        hir_id: none_arm_hir_id,
        pat: none_pat,
        guard: None,
        body: none_body,
        span: none_arm_span,
        synthesized: true,
    };

    let next_match = arena.alloc_expr(Expr {
        hir_id: next_match_hir_id,
        kind: ExprKind::Match {
            scrutinee: next_value,
            arms: arena.alloc_arms(&[some_arm, none_arm]),
            source: if for_await {
                HirMatchSource::ForAwait
            } else {
                HirMatchSource::ForLoop
            },
        },
        span: next_match_span,
    });
    let loop_body = arena.alloc_block(HirBlock {
        hir_id: loop_body_hir_id,
        stmts: arena.alloc_stmts(&[]),
        tail: Some(next_match),
        safety: Safety::Safe,
        span: loop_body_span,
    });
    let loop_expr = arena.alloc_expr(Expr {
        hir_id: loop_hir_id,
        kind: ExprKind::Loop {
            body: loop_body,
            label,
            source: if for_await {
                HirLoopSource::ForAwait
            } else {
                HirLoopSource::ForLoop
            },
        },
        span: loop_span,
    });
    let outer_arm = HirArm {
        hir_id: outer_arm_hir_id,
        pat: iter_pat,
        guard: None,
        body: loop_expr,
        span: outer_arm_span,
        synthesized: false,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id: match_hir_id,
        kind: ExprKind::Match {
            scrutinee: outer_scrutinee,
            arms: arena.alloc_arms(&[outer_arm]),
            source: if for_await {
                HirMatchSource::ForAwait
            } else {
                HirMatchSource::ForLoop
            },
        },
        span: match_span,
    }))
}

fn lower_synthetic_lang_path_expr<'hir>(
    syntax: &SyntaxNode,
    ctxt: SpanCtxt,
    item: LangItem,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, ctxt)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, ctxt)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(Path {
            hir_id: path_hir_id,
            root: PathRoot::Lang(item),
            segments: arena.alloc_path_segments(&[]),
            span: path_span,
        }),
        span,
    }))
}

fn lower_synthetic_local_path_expr<'hir>(
    syntax: &SyntaxNode,
    ctxt: SpanCtxt,
    symbol: Symbol,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, ctxt)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, ctxt)?;
    let (segment_hir_id, _) = source_map.add_node_with_span(syntax, ctxt)?;
    let segment = PathSegment {
        hir_id: segment_hir_id,
        name: Name {
            sym: symbol,
            span: source_map.span_for(syntax, ctxt)?,
        },
        args: None,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Path(Path {
            hir_id: path_hir_id,
            root: PathRoot::Plain,
            segments: arena.alloc_path_segments(&[segment]),
            span: path_span,
        }),
        span,
    }))
}

fn lower_synthetic_lang_ctor_pat<'hir>(
    syntax: &SyntaxNode,
    ctxt: SpanCtxt,
    item: LangItem,
    argument: Option<&ast::Pattern>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, ctxt)?;
    let (path_hir_id, path_span) = source_map.add_node_with_span(syntax, ctxt)?;
    let path = Path {
        hir_id: path_hir_id,
        root: PathRoot::Lang(item),
        segments: arena.alloc_path_segments(&[]),
        span: path_span,
    };
    let args = if let Some(argument) = argument {
        let pat = lower_pattern(argument, arena, symbols, source_map)?;
        Some(arena.alloc_pat_args(&[PatArg { name: None, pat }]))
    } else {
        None
    };
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Ctor {
            path,
            args,
            has_rest: false,
        },
        span,
    }))
}

fn lower_cast_expr<'hir>(
    cast: &ast::CastExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(cast.syntax(), SpanCtxt::Root)?;
    let operand = match cast.expression() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(cast.syntax()).unwrap_or_else(|| cast.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let ty = match cast.ty() {
        Some(ty) => lower_type(&ty, arena, symbols, source_map)?,
        None => {
            let error =
                direct_error_child_from_end(cast.syntax()).unwrap_or_else(|| cast.syntax().clone());
            lower_error_type(&error, arena, source_map)?
        }
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::AsCast { operand, ty },
        span,
    }))
}

fn lower_if_let_expr<'hir>(
    if_expr: &ast::IfExpr,
    condition: &ast::LetCondition,
    then_branch: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let desugared = SpanCtxt::Desugared(DesugaringKind::IfLet);
    let (hir_id, span) = source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let scrutinee = match condition.initializer() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_from_end(condition.syntax())
                .unwrap_or_else(|| condition.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let (arm_hir_id, arm_span) = source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let pattern = match condition.pattern() {
        Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(condition.syntax())
                .unwrap_or_else(|| condition.syntax().clone());
            lower_error_pattern(&error, arena, source_map)?
        }
    };
    let body = lower_block_as_expr(
        then_branch.syntax(),
        then_branch,
        arena,
        symbols,
        source_map,
    )?;
    let user_arm = HirArm {
        hir_id: arm_hir_id,
        pat: pattern,
        guard: None,
        body,
        span: arm_span,
        synthesized: false,
    };

    let (fallback_arm_hir_id, fallback_arm_span) =
        source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let (wildcard_hir_id, wildcard_span) =
        source_map.add_node_with_span(if_expr.syntax(), desugared)?;
    let wildcard = arena.alloc_pat(Pat {
        hir_id: wildcard_hir_id,
        kind: PatKind::Wild,
        span: wildcard_span,
    });
    let fallback_body = if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            lower_block_as_expr(block.syntax(), &block, arena, symbols, source_map)?
        } else if let Some(else_if) = else_branch.else_if() {
            lower_if_expr(&else_if, arena, symbols, source_map)?
        } else {
            let error = direct_error_child(else_branch.syntax())
                .unwrap_or_else(|| else_branch.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    } else {
        let (empty_hir_id, empty_span) =
            source_map.add_node_with_span(if_expr.syntax(), desugared)?;
        arena.alloc_expr(Expr {
            hir_id: empty_hir_id,
            kind: ExprKind::Tuple(arena.alloc_exprs(&[])),
            span: empty_span,
        })
    };
    let fallback_arm = HirArm {
        hir_id: fallback_arm_hir_id,
        pat: wildcard,
        guard: None,
        body: fallback_body,
        span: fallback_arm_span,
        synthesized: true,
    };
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Match {
            scrutinee,
            arms: arena.alloc_arms(&[user_arm, fallback_arm]),
            source: HirMatchSource::IfLet,
        },
        span,
    }))
}

fn lower_match_expr<'hir>(
    match_expr: &ast::MatchExpr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(match_expr.syntax(), SpanCtxt::Root)?;
    let scrutinee = match match_expr.scrutinee() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(match_expr.syntax())
                .unwrap_or_else(|| match_expr.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let mut arms = Vec::new();
    for arm in collect_match_arms(match_expr) {
        arms.push(match arm {
            MatchArmSyntax::Arm(arm) => lower_match_arm(&arm, arena, symbols, source_map)?,
            MatchArmSyntax::Error(error) => {
                let (arm_hir_id, arm_span) =
                    source_map.add_node_with_span(&error, SpanCtxt::Root)?;
                let pattern = lower_error_pattern(&error, arena, source_map)?;
                let body = lower_error_expr(&error, arena, source_map)?;
                HirArm {
                    hir_id: arm_hir_id,
                    pat: pattern,
                    guard: None,
                    body,
                    span: arm_span,
                    synthesized: false,
                }
            }
        });
    }
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Match {
            scrutinee,
            arms: arena.alloc_arms(&arms),
            source: HirMatchSource::Normal,
        },
        span,
    }))
}

fn lower_match_arm<'hir>(
    arm: &ast::MatchArm,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirArm<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(arm.syntax(), SpanCtxt::Root)?;
    let pattern = match arm.pattern() {
        Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child(arm.syntax()).unwrap_or_else(|| arm.syntax().clone());
            lower_error_pattern(&error, arena, source_map)?
        }
    };
    let guard = arm
        .guard()
        .map(|guard| match guard.expression() {
            Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map),
            None => {
                let error =
                    direct_error_child(guard.syntax()).unwrap_or_else(|| guard.syntax().clone());
                lower_error_expr(&error, arena, source_map)
            }
        })
        .transpose()?;
    let body = match arm.expression() {
        Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
        None => {
            let error =
                direct_error_child_from_end(arm.syntax()).unwrap_or_else(|| arm.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    Ok(HirArm {
        hir_id,
        pat: pattern,
        guard,
        body,
        span,
        synthesized: false,
    })
}

fn lower_block<'hir>(
    block: &ast::Block,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirBlock<'hir>, HirLoweringError> {
    lower_block_with_safety(block, Safety::Safe, arena, symbols, source_map)
}

fn lower_block_with_safety<'hir>(
    block: &ast::Block,
    safety: Safety,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirBlock<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(block.syntax(), SpanCtxt::Root)?;
    let entries = collect_block_statements(block.syntax());
    let entry_count = entries.len();
    let mut statements = Vec::new();
    let mut tail = None;
    let last_is_expression = matches!(
        entries.last(),
        Some(BlockStatementSyntax::Statement(ast::Stmt::Expr(_)))
    );

    for (index, entry) in entries.into_iter().enumerate() {
        match entry {
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement))
                if last_is_expression && index + 1 == entry_count =>
            {
                tail = Some(match expression_statement.expression() {
                    Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
                    None => match direct_error_child(expression_statement.syntax()) {
                        Some(error) => lower_error_expr(&error, arena, source_map)?,
                        None => lower_error_expr(expression_statement.syntax(), arena, source_map)?,
                    },
                });
            }
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement)) => {
                let (stmt_hir_id, stmt_span) =
                    source_map.add_node_with_span(expression_statement.syntax(), SpanCtxt::Root)?;
                let expression = match expression_statement.expression() {
                    Some(expression) => lower_expr_inner(&expression, arena, symbols, source_map)?,
                    None => match direct_error_child(expression_statement.syntax()) {
                        Some(error) => lower_error_expr(&error, arena, source_map)?,
                        None => lower_error_expr(expression_statement.syntax(), arena, source_map)?,
                    },
                };
                statements.push(HirStmt {
                    hir_id: stmt_hir_id,
                    kind: StmtKind::Expr(expression),
                    span: stmt_span,
                });
            }
            BlockStatementSyntax::Statement(ast::Stmt::Let(let_statement)) => {
                statements.push(lower_let_statement(
                    &let_statement,
                    arena,
                    symbols,
                    source_map,
                )?);
            }
            BlockStatementSyntax::Statement(ast::Stmt::Var(var_statement)) => {
                statements.push(lower_var_statement(
                    &var_statement,
                    arena,
                    symbols,
                    source_map,
                )?);
            }
            BlockStatementSyntax::Statement(ast::Stmt::Assign(assignment)) => {
                statements.push(lower_assignment_statement(
                    &assignment,
                    arena,
                    symbols,
                    source_map,
                )?);
            }
            BlockStatementSyntax::Statement(ast::Stmt::Assert(assertion)) => {
                statements.push(lower_assert_statement(
                    &assertion, arena, symbols, source_map,
                )?);
            }
            BlockStatementSyntax::Error(error) => {
                let (stmt_hir_id, stmt_span) =
                    source_map.add_node_with_span(&error, SpanCtxt::Root)?;
                let expression = lower_error_expr(&error, arena, source_map)?;
                statements.push(HirStmt {
                    hir_id: stmt_hir_id,
                    kind: StmtKind::Expr(expression),
                    span: stmt_span,
                });
            }
        }
    }

    Ok(arena.alloc_block(HirBlock {
        hir_id,
        stmts: arena.alloc_stmts(&statements),
        tail,
        safety,
        span,
    }))
}

fn lower_let_statement<'hir>(
    statement: &ast::LetStmt,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirStmt<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(statement.syntax(), SpanCtxt::Root)?;
    let pat = match statement.pattern() {
        Some(pattern) => lower_pattern(&pattern, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_before_token(statement.syntax(), SyntaxKind::EQ)
                .unwrap_or_else(|| statement.syntax().clone());
            lower_error_pattern(&error, arena, source_map)?
        }
    };
    let ty = if let Some(ty) = statement.ty() {
        Some(lower_type_inner(&ty, arena, symbols, source_map)?)
    } else if has_direct_token(statement.syntax(), SyntaxKind::COLON) {
        let error = direct_error_child_between_tokens(
            statement.syntax(),
            SyntaxKind::COLON,
            SyntaxKind::EQ,
        )
        .unwrap_or_else(|| statement.syntax().clone());
        Some(lower_error_type(&error, arena, source_map)?)
    } else {
        None
    };
    let init = lower_statement_initializer(
        statement.syntax(),
        statement.initializer(),
        arena,
        symbols,
        source_map,
    )?;
    let els = statement
        .else_clause()
        .map(|else_clause| {
            if let Some(block) = else_clause.block() {
                lower_block(&block, arena, symbols, source_map)
            } else {
                lower_error_block(else_clause.syntax(), arena, source_map)
            }
        })
        .transpose()?;
    Ok(HirStmt {
        hir_id,
        kind: StmtKind::Let { pat, ty, init, els },
        span,
    })
}

fn lower_var_statement<'hir>(
    statement: &ast::VarStmt,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirStmt<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(statement.syntax(), SpanCtxt::Root)?;
    let pat = if let Some(name) = statement.name() {
        let ctxt = SpanCtxt::Desugared(DesugaringKind::VarStmt);
        let (pat_hir_id, pat_span) = source_map.add_node_with_span(statement.syntax(), ctxt)?;
        let (binding, name_span) = source_map.add_binding_with_span(name.syntax(), ctxt)?;
        let token = name.token();
        let mut hir_name = lower_name_spelling(
            name.syntax(),
            token.as_ref().map(|token| token.text()),
            symbols,
            source_map,
        )?;
        hir_name.span = name_span;
        arena.alloc_pat(Pat {
            hir_id: pat_hir_id,
            kind: PatKind::Binding {
                mode: HirBindingMode::Var,
                name: hir_name,
                binding,
                sub: None,
            },
            span: pat_span,
        })
    } else {
        let error = direct_error_child_before_token(statement.syntax(), SyntaxKind::EQ)
            .unwrap_or_else(|| statement.syntax().clone());
        lower_error_pattern(&error, arena, source_map)?
    };
    let ty = if let Some(ty) = statement.ty() {
        Some(lower_type_inner(&ty, arena, symbols, source_map)?)
    } else if has_direct_token(statement.syntax(), SyntaxKind::COLON) {
        let error = direct_error_child_between_tokens(
            statement.syntax(),
            SyntaxKind::COLON,
            SyntaxKind::EQ,
        )
        .unwrap_or_else(|| statement.syntax().clone());
        Some(lower_error_type(&error, arena, source_map)?)
    } else {
        None
    };
    let init = lower_statement_initializer(
        statement.syntax(),
        statement.initializer(),
        arena,
        symbols,
        source_map,
    )?;
    Ok(HirStmt {
        hir_id,
        kind: StmtKind::Let {
            pat,
            ty,
            init,
            els: None,
        },
        span,
    })
}

fn lower_statement_initializer<'hir>(
    statement: &SyntaxNode,
    initializer: Option<ast::Expr>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    match initializer {
        Some(initializer) => lower_recovered_expr(&initializer, arena, symbols, source_map),
        None => {
            let error = if has_direct_token(statement, SyntaxKind::EQ) {
                direct_error_child_after_token(statement, SyntaxKind::EQ)
            } else {
                direct_error_child_from_end(statement)
            }
            .unwrap_or_else(|| statement.clone());
            lower_error_expr(&error, arena, source_map)
        }
    }
}

fn lower_assignment_statement<'hir>(
    statement: &ast::AssignStmt,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirStmt<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(statement.syntax(), SpanCtxt::Root)?;
    let Some((operator_token, operator)) = statement.op() else {
        let expression = lower_error_expr(statement.syntax(), arena, source_map)?;
        return Ok(HirStmt {
            hir_id,
            kind: StmtKind::Expr(expression),
            span,
        });
    };
    let lhs = match statement.lhs() {
        Some(lhs) => lower_recovered_expr(&lhs, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_before_token(statement.syntax(), operator_token.kind())
                .unwrap_or_else(|| statement.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let rhs = match statement.rhs() {
        Some(rhs) => lower_recovered_expr(&rhs, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_after_token(statement.syntax(), operator_token.kind())
                .unwrap_or_else(|| statement.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let kind = match operator {
        ast::AssignOp::Set => StmtKind::Assign { lhs, rhs },
        ast::AssignOp::Add => StmtKind::AssignOp {
            op: BinOp::Add,
            lhs,
            rhs,
        },
        ast::AssignOp::Sub => StmtKind::AssignOp {
            op: BinOp::Sub,
            lhs,
            rhs,
        },
        ast::AssignOp::Mul => StmtKind::AssignOp {
            op: BinOp::Mul,
            lhs,
            rhs,
        },
        ast::AssignOp::Div => StmtKind::AssignOp {
            op: BinOp::Div,
            lhs,
            rhs,
        },
        ast::AssignOp::Rem => StmtKind::AssignOp {
            op: BinOp::Rem,
            lhs,
            rhs,
        },
        ast::AssignOp::WrappingAdd => StmtKind::AssignOp {
            op: BinOp::WrapAdd,
            lhs,
            rhs,
        },
        ast::AssignOp::WrappingSub => StmtKind::AssignOp {
            op: BinOp::WrapSub,
            lhs,
            rhs,
        },
        ast::AssignOp::WrappingMul => StmtKind::AssignOp {
            op: BinOp::WrapMul,
            lhs,
            rhs,
        },
        ast::AssignOp::BitAnd => StmtKind::AssignOp {
            op: BinOp::BitAnd,
            lhs,
            rhs,
        },
        ast::AssignOp::BitOr => StmtKind::AssignOp {
            op: BinOp::BitOr,
            lhs,
            rhs,
        },
        ast::AssignOp::BitXor => StmtKind::AssignOp {
            op: BinOp::BitXor,
            lhs,
            rhs,
        },
        ast::AssignOp::Shl => StmtKind::AssignOp {
            op: BinOp::Shl,
            lhs,
            rhs,
        },
        ast::AssignOp::Shr => StmtKind::AssignOp {
            op: BinOp::Shr,
            lhs,
            rhs,
        },
    };
    Ok(HirStmt { hir_id, kind, span })
}

fn lower_assert_statement<'hir>(
    statement: &ast::AssertStmt,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<HirStmt<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(statement.syntax(), SpanCtxt::Root)?;
    let cond = match statement.condition() {
        Some(condition) => lower_recovered_expr(&condition, arena, symbols, source_map)?,
        None => {
            let error = direct_error_child_before_token(statement.syntax(), SyntaxKind::COMMA)
                .unwrap_or_else(|| statement.syntax().clone());
            lower_error_expr(&error, arena, source_map)?
        }
    };
    let msg = match statement.message() {
        Some(message) => Some(lower_recovered_expr(&message, arena, symbols, source_map)?),
        None if has_direct_token(statement.syntax(), SyntaxKind::COMMA) => {
            let error = direct_error_child_after_token(statement.syntax(), SyntaxKind::COMMA)
                .unwrap_or_else(|| statement.syntax().clone());
            Some(lower_error_expr(&error, arena, source_map)?)
        }
        None => None,
    };
    let cond_text = statement.condition().map_or_else(
        || Ok(String::new()),
        |condition| {
            source_map
                .text_for_syntax(condition.syntax())
                .map(str::to_owned)
        },
    )?;
    let cond_text = symbols.intern(cond_text.trim())?;
    Ok(HirStmt {
        hir_id,
        kind: StmtKind::Assert {
            cond,
            msg,
            cond_text,
        },
        span,
    })
}

fn lower_recovered_expr<'hir>(
    expression: &ast::Expr,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    if contains_error_node(expression.syntax()) && !contains_call_arglist_error(expression.syntax())
    {
        lower_error_expr(expression.syntax(), arena, source_map)
    } else {
        lower_expr_inner(expression, arena, symbols, source_map)
    }
}

fn lower_error_block<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir HirBlock<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    let error = direct_error_child(syntax).unwrap_or_else(|| syntax.clone());
    let tail = lower_error_expr(&error, arena, source_map)?;
    Ok(arena.alloc_block(HirBlock {
        hir_id,
        stmts: arena.alloc_stmts(&[]),
        tail: Some(tail),
        safety: Safety::Safe,
        span,
    }))
}

fn has_direct_token(syntax: &SyntaxNode, kind: SyntaxKind) -> bool {
    syntax
        .children_with_tokens()
        .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == kind))
}

fn direct_token(syntax: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    syntax
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == kind => Some(token),
            _ => None,
        })
}

fn direct_error_child_before_token(syntax: &SyntaxNode, before: SyntaxKind) -> Option<SyntaxNode> {
    let mut errors = syntax.children_with_tokens().take_while(
        |element| !matches!(element, SyntaxElement::Token(token) if token.kind() == before),
    );
    errors.find_map(|element| match element {
        SyntaxElement::Node(node) if node.kind() == SyntaxKind::ERROR => Some(node),
        _ => None,
    })
}

fn direct_error_child_after_token(syntax: &SyntaxNode, after: SyntaxKind) -> Option<SyntaxNode> {
    let mut after_token = false;
    for element in syntax.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind() == after => after_token = true,
            SyntaxElement::Node(node) if after_token && node.kind() == SyntaxKind::ERROR => {
                return Some(node);
            }
            _ => {}
        }
    }
    None
}

fn direct_error_child_between_tokens(
    syntax: &SyntaxNode,
    after: SyntaxKind,
    before: SyntaxKind,
) -> Option<SyntaxNode> {
    let mut after_token = false;
    for element in syntax.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if after_token && token.kind() == before => return None,
            SyntaxElement::Token(token) if token.kind() == after => after_token = true,
            SyntaxElement::Node(node) if after_token && node.kind() == SyntaxKind::ERROR => {
                return Some(node);
            }
            _ => {}
        }
    }
    None
}

fn lower_label(
    label: Option<ast::Label>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Option<HirLabel>, HirLoweringError> {
    label
        .map(|label| {
            let token = label.syntax();
            let parent_span = source_map.span_for(token.parent(), SpanCtxt::Root)?;
            let text = token.text().strip_prefix('\'').unwrap_or(token.text());
            let normalized = sai_syntax::normalize_identifier(text);
            Ok(HirLabel {
                name: symbols.intern(&normalized)?,
                span: Span {
                    range: token.text_range(),
                    ..parent_span
                },
            })
        })
        .transpose()
}

fn lower_bin_op(op: ast::BinOp) -> BinOp {
    match op {
        ast::BinOp::Or => BinOp::Or,
        ast::BinOp::And => BinOp::And,
        ast::BinOp::Eq => BinOp::Eq,
        ast::BinOp::Ne => BinOp::Ne,
        ast::BinOp::Lt => BinOp::Lt,
        ast::BinOp::Le => BinOp::Le,
        ast::BinOp::Gt => BinOp::Gt,
        ast::BinOp::Ge => BinOp::Ge,
        ast::BinOp::BitOr => BinOp::BitOr,
        ast::BinOp::BitXor => BinOp::BitXor,
        ast::BinOp::BitAnd => BinOp::BitAnd,
        ast::BinOp::Shl => BinOp::Shl,
        ast::BinOp::Shr => BinOp::Shr,
        ast::BinOp::Add => BinOp::Add,
        ast::BinOp::Sub => BinOp::Sub,
        ast::BinOp::WrappingAdd => BinOp::WrapAdd,
        ast::BinOp::WrappingSub => BinOp::WrapSub,
        ast::BinOp::Mul => BinOp::Mul,
        ast::BinOp::Div => BinOp::Div,
        ast::BinOp::Rem => BinOp::Rem,
        ast::BinOp::WrappingMul => BinOp::WrapMul,
    }
}

fn lower_error_expr<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Expr<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    Ok(arena.alloc_expr(Expr {
        hir_id,
        kind: ExprKind::Err,
        span,
    }))
}

fn ensure_supported_expr(expression: &ast::Expr) -> Result<(), HirLoweringError> {
    match expression {
        ast::Expr::Literal(_) | ast::Expr::SelfValue(_) | ast::Expr::SelfType(_) => Ok(()),
        ast::Expr::Closure(closure) => {
            if let Some(params) = closure.params() {
                for param in params.params() {
                    if let Some(ty) = param.ty() {
                        ensure_supported_type(&ty)?;
                    }
                }
            }
            if let Some(ret) = closure.ret_type().and_then(|ret| ret.ty()) {
                ensure_supported_type(&ret)?;
            }
            if let Some(body) = closure.body() {
                ensure_supported_block(&body)?;
            }
            Ok(())
        }
        ast::Expr::AsyncBlock(async_block) => match async_block.body() {
            Some(body) => ensure_supported_block(&body),
            None => Ok(()),
        },
        ast::Expr::Binary(binary) => {
            if let Some(lhs) = binary.lhs() {
                ensure_supported_expr(&lhs)?;
            }
            if let Some(rhs) = binary.rhs() {
                ensure_supported_expr(&rhs)?;
            }
            Ok(())
        }
        ast::Expr::Prefix(prefix) => match prefix.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Range(range) => {
            if let Some(start) = range.start() {
                ensure_supported_expr(&start)?;
            }
            if let Some(end) = range.end() {
                ensure_supported_expr(&end)?;
            }
            Ok(())
        }
        ast::Expr::Index(index) => {
            if let Some(base) = index.base() {
                ensure_supported_expr(&base)?;
            }
            if let Some(index_expr) = index.index() {
                ensure_supported_expr(&index_expr)?;
            }
            Ok(())
        }
        ast::Expr::Try(try_expr) => match try_expr.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Await(await_expr) => match await_expr.operand() {
            Some(operand) => ensure_supported_expr(&operand),
            None => Ok(()),
        },
        ast::Expr::Return(return_expr) => match return_expr.value() {
            Some(value) => ensure_supported_expr(&value),
            None => Ok(()),
        },
        ast::Expr::Break(break_expr) => match break_expr.value() {
            Some(value) => ensure_supported_expr(&value),
            None => Ok(()),
        },
        ast::Expr::Continue(_) => Ok(()),
        ast::Expr::Call(call) => {
            if let Some(callee) = call.callee() {
                ensure_supported_expr(&callee)?;
            }
            for expression in call_argument_expressions(call) {
                ensure_supported_expr(&expression)?;
            }
            Ok(())
        }
        ast::Expr::Path(path) => {
            if let Some(args) = path.generic_args() {
                ensure_supported_generic_args(&args)?;
            }
            Ok(())
        }
        ast::Expr::Field(field) => {
            if let Some(args) = direct_generic_args(field.syntax()) {
                ensure_supported_generic_args(&args)?;
            }
            match field.base() {
                Some(base) => ensure_supported_expr(&base),
                None => Ok(()),
            }
        }
        ast::Expr::TupleIndex(index) => match index.base() {
            Some(base) => ensure_supported_expr(&base),
            None => Ok(()),
        },
        ast::Expr::ImplicitMember(_) | ast::Expr::Unit(_) => Ok(()),
        ast::Expr::Paren(paren) => match paren.expression() {
            Some(inner) => ensure_supported_expr(&inner),
            None => Ok(()),
        },
        ast::Expr::Tuple(tuple) => {
            for element in tuple.elements() {
                ensure_supported_expr(&element)?;
            }
            Ok(())
        }
        ast::Expr::Array(array) => {
            for element in array.elements() {
                ensure_supported_expr(&element)?;
            }
            Ok(())
        }
        ast::Expr::FormatString(format_string) => {
            for piece in format_string.pieces() {
                if let ast::FStringPiece::Interp {
                    expr: Some(expression),
                    ..
                } = piece
                {
                    ensure_supported_expr(&expression)?;
                }
            }
            Ok(())
        }
        ast::Expr::Block(block_expr) => match block_expr.block() {
            Some(block) => ensure_supported_block(&block),
            None => Ok(()),
        },
        ast::Expr::UnsafeBlock(block_expr) => match block_expr.body() {
            Some(block) => ensure_supported_block(&block),
            None => Ok(()),
        },
        ast::Expr::Region(region_expr) => match region_expr.body() {
            Some(block) => ensure_supported_block(&block),
            None => Ok(()),
        },
        ast::Expr::If(if_expr) => ensure_supported_if_expr(if_expr),
        ast::Expr::Match(match_expr) => ensure_supported_match_expr(match_expr),
        ast::Expr::Loop(loop_expr) => {
            if let Some(body) = loop_expr.body() {
                ensure_supported_block(&body)?;
            }
            Ok(())
        }
        ast::Expr::While(while_expr) => ensure_supported_while_expr(while_expr),
        ast::Expr::For(for_expr) => {
            if let Some(pattern) = for_expr.pattern() {
                ensure_supported_pattern(&pattern)?;
            }
            if let Some(iterable) = for_expr.iterable() {
                ensure_supported_expr(&iterable)?;
            }
            if let Some(body) = for_expr.body() {
                ensure_supported_block(&body)?;
            }
            Ok(())
        }
        ast::Expr::Cast(cast) => {
            if let Some(operand) = cast.expression() {
                ensure_supported_expr(&operand)?;
            }
            if let Some(ty) = cast.ty() {
                ensure_supported_type(&ty)?;
            }
            Ok(())
        }
        _ if contains_error_node(expression.syntax()) => Ok(()),
        _ => Err(HirLoweringError::UnsupportedSyntax(
            expression.syntax().kind(),
        )),
    }
}

fn ensure_supported_block(block: &ast::Block) -> Result<(), HirLoweringError> {
    for entry in collect_block_statements(block.syntax()) {
        match entry {
            BlockStatementSyntax::Statement(ast::Stmt::Expr(expression_statement)) => {
                if let Some(expression) = expression_statement.expression() {
                    ensure_supported_expr(&expression)?;
                }
            }
            BlockStatementSyntax::Statement(ast::Stmt::Let(statement)) => {
                if let Some(pattern) = statement.pattern() {
                    ensure_supported_pattern(&pattern)?;
                }
                if let Some(ty) = statement.ty() {
                    ensure_supported_type(&ty)?;
                }
                if let Some(initializer) = statement.initializer() {
                    ensure_supported_expr(&initializer)?;
                }
                if let Some(block) = statement.else_clause().and_then(|clause| clause.block()) {
                    ensure_supported_block(&block)?;
                }
            }
            BlockStatementSyntax::Statement(ast::Stmt::Var(statement)) => {
                if let Some(ty) = statement.ty() {
                    ensure_supported_type(&ty)?;
                }
                if let Some(initializer) = statement.initializer() {
                    ensure_supported_expr(&initializer)?;
                }
            }
            BlockStatementSyntax::Statement(ast::Stmt::Assign(statement)) => {
                if let Some(lhs) = statement.lhs() {
                    ensure_supported_expr(&lhs)?;
                }
                if let Some(rhs) = statement.rhs() {
                    ensure_supported_expr(&rhs)?;
                }
            }
            BlockStatementSyntax::Statement(ast::Stmt::Assert(statement)) => {
                if let Some(condition) = statement.condition() {
                    ensure_supported_expr(&condition)?;
                }
                if let Some(message) = statement.message() {
                    ensure_supported_expr(&message)?;
                }
            }
            BlockStatementSyntax::Error(_) => {}
        }
    }
    Ok(())
}

fn ensure_supported_if_expr(if_expr: &ast::IfExpr) -> Result<(), HirLoweringError> {
    if let Some(condition) = if_expr.condition() {
        if let Some(let_condition) = condition.let_condition() {
            if let Some(pattern) = let_condition.pattern() {
                ensure_supported_pattern(&pattern)?;
            }
            if let Some(expression) = let_condition.initializer() {
                ensure_supported_expr(&expression)?;
            }
        } else if let Some(expression) = condition.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    if let Some(then_branch) = if_expr.then_branch() {
        ensure_supported_block(&then_branch)?;
    }
    if let Some(else_branch) = if_expr.else_branch() {
        if let Some(block) = else_branch.block() {
            ensure_supported_block(&block)?;
        } else if let Some(else_if) = else_branch.else_if() {
            ensure_supported_if_expr(&else_if)?;
        }
    }
    Ok(())
}

fn ensure_supported_while_expr(while_expr: &ast::WhileExpr) -> Result<(), HirLoweringError> {
    if let Some(condition) = while_expr.condition() {
        if let Some(let_condition) = condition.let_condition() {
            if let Some(pattern) = let_condition.pattern() {
                ensure_supported_pattern(&pattern)?;
            }
            if let Some(expression) = let_condition.initializer() {
                ensure_supported_expr(&expression)?;
            }
        } else if let Some(expression) = condition.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    if let Some(body) = while_expr.body() {
        ensure_supported_block(&body)?;
    }
    Ok(())
}

fn ensure_supported_type(ty: &ast::Type) -> Result<(), HirLoweringError> {
    match ty {
        ast::Type::PathType(path) => {
            for segment in path.segments() {
                if let Some(args) = segment.generic_args() {
                    ensure_supported_generic_args(&args)?;
                }
            }
        }
        ast::Type::SelfType(path) => {
            for segment in path.segments() {
                if let Some(args) = segment.generic_args() {
                    ensure_supported_generic_args(&args)?;
                }
            }
        }
        ast::Type::BorrowType(borrow) => {
            if let Some(inner) = borrow.ty() {
                ensure_supported_type(&inner)?;
            }
        }
        ast::Type::OwnedType(owned) => {
            if let Some(inner) = owned.ty() {
                ensure_supported_type(&inner)?;
            }
        }
        ast::Type::TupleType(tuple) => {
            for element in tuple.elements() {
                ensure_supported_type(&element)?;
            }
        }
        ast::Type::ParenType(paren) => {
            if let Some(inner) = paren.ty() {
                ensure_supported_type(&inner)?;
            }
        }
        ast::Type::InferType(_) => {}
        ast::Type::FnPtrType(function) => {
            for param in function.params() {
                ensure_supported_type(&param)?;
            }
            if let Some(ret) = function.ret_type() {
                ensure_supported_type(&ret)?;
            }
        }
    }
    Ok(())
}

fn ensure_supported_generics(declaration: &ast::FnDecl) -> Result<(), HirLoweringError> {
    ensure_supported_generic_params(declaration.generic_params())?;
    for where_clause in declaration
        .syntax()
        .children()
        .filter_map(ast::WhereClause::cast)
    {
        ensure_supported_where_clause(&where_clause)?;
    }
    Ok(())
}

fn ensure_supported_generic_params(
    generic_params: Option<ast::GenericParamList>,
) -> Result<(), HirLoweringError> {
    if let Some(params) = generic_params {
        for param in params.params() {
            match param {
                ast::GenericParam::Type(param) => {
                    if let Some(bounds) = param.bounds() {
                        for bound in bounds.bounds() {
                            ensure_supported_bound(bound)?;
                        }
                    }
                }
                ast::GenericParam::Const(param) => {
                    if let Some(ty) = param.ty() {
                        ensure_supported_type(&ty)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn ensure_supported_where_clause(where_clause: &ast::WhereClause) -> Result<(), HirLoweringError> {
    for predicate in where_clause.predicates() {
        if let Some(ty) = predicate.ty() {
            ensure_supported_type(&ty)?;
        }
        for bound in predicate.bounds() {
            ensure_supported_bound(bound)?;
        }
    }
    Ok(())
}

fn ensure_supported_bound(bound: ast::Bound) -> Result<(), HirLoweringError> {
    match bound {
        ast::Bound::Trait(bound) => {
            if let Some(path) = bound.path() {
                ensure_supported_type(&ast::Type::PathType(path))?;
            }
        }
        ast::Bound::FnSugar(bound) => {
            for ty in bound.params() {
                ensure_supported_type(&ty)?;
            }
            if let Some(ty) = bound.ret_type() {
                ensure_supported_type(&ty)?;
            }
        }
    }
    Ok(())
}

fn ensure_supported_generic_args(args: &ast::GenericArgList) -> Result<(), HirLoweringError> {
    for child in args.syntax().children() {
        if let Some(type_arg) = ast::TypeArg::cast(child.clone()) {
            if let Some(ty) = type_arg.ty() {
                ensure_supported_type(&ty)?;
            }
        } else if let Some(binding) = ast::AssocTypeBinding::cast(child)
            && let Some(ty) = binding.ty()
        {
            ensure_supported_type(&ty)?;
        }
    }
    Ok(())
}

fn ensure_supported_match_expr(match_expr: &ast::MatchExpr) -> Result<(), HirLoweringError> {
    if let Some(scrutinee) = match_expr.scrutinee() {
        ensure_supported_expr(&scrutinee)?;
    }
    for arm in collect_match_arms(match_expr) {
        let MatchArmSyntax::Arm(arm) = arm else {
            continue;
        };
        if let Some(pattern) = arm.pattern() {
            ensure_supported_pattern(&pattern)?;
        }
        if let Some(guard) = arm.guard()
            && let Some(expression) = guard.expression()
        {
            ensure_supported_expr(&expression)?;
        }
        if let Some(expression) = arm.expression() {
            ensure_supported_expr(&expression)?;
        }
    }
    Ok(())
}

fn direct_generic_args(syntax: &SyntaxNode) -> Option<ast::GenericArgList> {
    syntax.children().find_map(ast::GenericArgList::cast)
}

/// Lower a literal pattern, preserving its direct minus sign for integer
/// patterns and mapping parser-rejected float patterns to `PatKind::Err`.
pub fn lower_literal_pat<'hir>(
    literal: &ast::LiteralPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(literal.syntax(), SpanCtxt::Root)?;
    let negative = literal.minus_token().is_some();
    let kind = if contains_error_node(literal.syntax()) {
        PatKind::Err
    } else {
        literal
            .value_token()
            .and_then(ast::LiteralToken::cast)
            .map(|token| decode_literal(token, arena, symbols, false, negative))
            .transpose()?
            .flatten()
            .map(PatKind::Lit)
            .unwrap_or(PatKind::Err)
    };
    Ok(arena.alloc_pat(Pat { hir_id, kind, span }))
}

/// Lower a range pattern with literal or constant-path endpoints.
///
/// Invalid endpoint syntax becomes `PatKind::Err`.
pub fn lower_range_pat<'hir>(
    range: &ast::RangePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(range.syntax()) {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }

    let start_syntax = range.start();
    let end_syntax = range.end();
    let has_invalid_path = [start_syntax.as_ref(), end_syntax.as_ref()]
        .into_iter()
        .flatten()
        .any(|endpoint| match endpoint {
            ast::RangePatEndpoint::ConstantPath(path) => const_path_is_invalid(path.clone()),
            ast::RangePatEndpoint::Literal(_) => false,
        });
    if has_invalid_path {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }
    let inclusive = range.syntax().children_with_tokens().any(|element| {
        matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOT2EQ)
    });
    let has_start = start_syntax.is_some();
    let has_end = end_syntax.is_some();
    let start = start_syntax
        .map(|endpoint| prepare_range_endpoint(endpoint, arena, symbols))
        .transpose()?
        .flatten();
    let end = end_syntax
        .map(|endpoint| prepare_range_endpoint(endpoint, arena, symbols))
        .transpose()?
        .flatten();
    let invalid_shape = if inclusive { !has_end } else { !has_start };
    let invalid_endpoint = (has_start && start.is_none()) || (has_end && end.is_none());
    if invalid_shape || invalid_endpoint {
        return lower_error_pattern(range.syntax(), arena, source_map);
    }

    let (hir_id, span) = source_map.add_node_with_span(range.syntax(), SpanCtxt::Root)?;
    let start = start
        .map(|endpoint| lower_prepared_range_endpoint(endpoint, arena, symbols, source_map))
        .transpose()?;
    let end = end
        .map(|endpoint| lower_prepared_range_endpoint(endpoint, arena, symbols, source_map))
        .transpose()?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Range {
            start,
            end,
            inclusive,
        },
        span,
    }))
}

/// Lower the currently supported simple and recursive pattern forms.
///
/// Unsupported, otherwise-valid pattern kinds are returned explicitly so a
/// caller cannot mistake an unfinished lowering case for a source error.
pub fn lower_pattern<'hir>(
    pattern: &ast::Pattern,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    ensure_supported_pattern(pattern)?;
    if contains_error_node(pattern.syntax()) {
        return lower_error_pattern(pattern.syntax(), arena, source_map);
    }
    plan_pattern_bindings(pattern, symbols, source_map, None)?;
    lower_pattern_inner(pattern, arena, symbols, source_map)
}

fn lower_pattern_inner<'hir>(
    pattern: &ast::Pattern,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    match pattern {
        ast::Pattern::Wildcard(wildcard) => {
            let (hir_id, span) =
                source_map.add_node_with_span(wildcard.syntax(), SpanCtxt::Root)?;
            Ok(arena.alloc_pat(Pat {
                hir_id,
                kind: PatKind::Wild,
                span,
            }))
        }
        ast::Pattern::Literal(literal) => lower_literal_pat(literal, arena, symbols, source_map),
        ast::Pattern::Range(range) => lower_range_pat(range, arena, symbols, source_map),
        ast::Pattern::Tuple(tuple) => lower_tuple_pattern(tuple, arena, symbols, source_map),
        ast::Pattern::Slice(slice) => lower_slice_pattern(slice, arena, symbols, source_map),
        ast::Pattern::Binding(binding) => {
            lower_binding_pattern(binding, arena, symbols, source_map)
        }
        ast::Pattern::At(at) => lower_at_pattern(at, arena, symbols, source_map),
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_some_and(const_path_is_invalid) =>
        {
            lower_error_pattern(constructor.syntax(), arena, source_map)
        }
        ast::Pattern::Constructor(constructor) => {
            lower_constructor_pattern(constructor, arena, symbols, source_map)
        }
        ast::Pattern::Or(or) => lower_or_pattern(or, arena, symbols, source_map),
        ast::Pattern::ImplicitMember(member) => {
            lower_implicit_member_pattern(member, arena, symbols, source_map)
        }
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => lower_pattern_inner(&inner, arena, symbols, source_map),
            None => lower_error_pattern(paren.syntax(), arena, source_map),
        },
    }
}

fn lower_error_pattern<'hir>(
    syntax: &SyntaxNode,
    arena: &'hir HirArena<'hir>,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax, SpanCtxt::Root)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Err,
        span,
    }))
}

fn lower_or_pattern<'hir>(
    or: &ast::OrPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let alternatives = flattened_or_alternatives(or);
    match alternatives.as_slice() {
        [] => lower_error_pattern(or.syntax(), arena, source_map),
        [only] => lower_pattern_inner(only, arena, symbols, source_map),
        _ => {
            let (hir_id, span) = source_map.add_node_with_span(or.syntax(), SpanCtxt::Root)?;
            let alternatives = alternatives
                .iter()
                .map(|alternative| {
                    lower_pattern_inner(alternative, arena, symbols, source_map).copied()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(arena.alloc_pat(Pat {
                hir_id,
                kind: PatKind::Or(arena.alloc_pats(&alternatives)),
                span,
            }))
        }
    }
}

fn lower_tuple_pattern<'hir>(
    tuple: &ast::TuplePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(tuple.syntax(), SpanCtxt::Root)?;
    let elements = tuple
        .elements()
        .map(|element| lower_pattern_inner(&element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let elements = arena.alloc_pats(&elements);
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Tuple(elements),
        span,
    }))
}

fn lower_slice_pattern<'hir>(
    slice: &ast::SlicePat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(slice.syntax()) {
        return lower_error_pattern(slice.syntax(), arena, source_map);
    }

    let rests = slice.rest_patterns().collect::<Vec<_>>();
    if rests.len() > 1 {
        return lower_error_pattern(slice.syntax(), arena, source_map);
    }
    let elements = slice.elements().collect::<Vec<_>>();
    let split = rests.first().map_or(elements.len(), |rest| {
        let rest_start = rest.syntax().text_range().start();
        elements
            .iter()
            .position(|element| element.syntax().text_range().start() > rest_start)
            .unwrap_or(elements.len())
    });

    let (hir_id, span) = source_map.add_node_with_span(slice.syntax(), SpanCtxt::Root)?;
    let before = elements[..split]
        .iter()
        .map(|element| lower_pattern_inner(element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let rest = match rests.first() {
        None => None,
        Some(rest) => Some(
            rest.binding()
                .map(|binding| lower_binding_pattern(&binding, arena, symbols, source_map))
                .transpose()?,
        ),
    };
    let after = elements[split..]
        .iter()
        .map(|element| lower_pattern_inner(element, arena, symbols, source_map).copied())
        .collect::<Result<Vec<_>, _>>()?;
    let before = arena.alloc_pats(&before);
    let after = arena.alloc_pats(&after);
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Slice {
            before,
            rest,
            after,
        },
        span,
    }))
}

fn lower_implicit_member_pattern<'hir>(
    member: &ast::ImplicitMemberPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(member.syntax()) {
        return lower_error_pattern(member.syntax(), arena, source_map);
    }
    let Some(name) = member.name() else {
        return lower_error_pattern(member.syntax(), arena, source_map);
    };
    let args = match member.args() {
        Some(args) => match collect_ctor_pat_args(&args) {
            Some((args, has_rest)) => Some((args, has_rest)),
            None => return lower_error_pattern(member.syntax(), arena, source_map),
        },
        None => None,
    };

    let (hir_id, span) = source_map.add_node_with_span(member.syntax(), SpanCtxt::Root)?;
    let name = lower_name_ref(&name, symbols, source_map)?;
    let (args, has_rest) = match args {
        Some((args, has_rest)) => (
            Some(lower_ctor_pat_args(&args, arena, symbols, source_map)?),
            has_rest,
        ),
        None => (None, false),
    };
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::ImplicitMember {
            name,
            args,
            has_rest,
        },
        span,
    }))
}

fn lower_constructor_pattern<'hir>(
    constructor: &ast::CtorPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(constructor.syntax()) {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    }
    let Some(path) = constructor.path() else {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    };
    if path.segments().next().is_none() {
        return lower_error_pattern(constructor.syntax(), arena, source_map);
    }
    let args = match constructor.args() {
        Some(args) => match collect_ctor_pat_args(&args) {
            Some((args, has_rest)) => Some((args, has_rest)),
            None => return lower_error_pattern(constructor.syntax(), arena, source_map),
        },
        None => None,
    };

    let (hir_id, span) = source_map.add_node_with_span(constructor.syntax(), SpanCtxt::Root)?;
    let path = lower_const_path(&path, arena, symbols, source_map)?;
    let (args, has_rest) = match args {
        Some((args, has_rest)) => (
            Some(lower_ctor_pat_args(&args, arena, symbols, source_map)?),
            has_rest,
        ),
        None => (None, false),
    };
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Ctor {
            path,
            args,
            has_rest,
        },
        span,
    }))
}

fn lower_const_path<'hir>(
    syntax: &ast::ConstPath,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<Path<'hir>, HirLoweringError> {
    let (hir_id, span) = source_map.add_node_with_span(syntax.syntax(), SpanCtxt::Root)?;
    let mut segments = Vec::new();
    for segment in syntax.segments() {
        let (hir_id, _) = source_map.add_node_with_span(segment.syntax(), SpanCtxt::Root)?;
        segments.push(PathSegment {
            hir_id,
            name: lower_name_ref(&segment, symbols, source_map)?,
            args: None,
        });
    }
    Ok(Path {
        hir_id,
        root: PathRoot::Plain,
        segments: arena.alloc_path_segments(&segments),
        span,
    })
}

#[derive(Clone)]
struct PlannedBinding {
    symbol: Symbol,
    mode: HirBindingMode,
    binding: BindingId,
    occurrences: Vec<ast::BindingPat>,
}

fn plan_pattern_bindings(
    pattern: &ast::Pattern,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    match pattern {
        ast::Pattern::Binding(binding) => plan_binding(binding, symbols, source_map, shared),
        ast::Pattern::At(at) => {
            let (Some(binding), Some(inner)) = (at.binding(), at.pattern()) else {
                return Ok(Vec::new());
            };
            if lower_binding_mode(&binding).is_none() {
                return Ok(Vec::new());
            }
            let mut planned = plan_binding(&binding, symbols, source_map, shared)?;
            let inner = plan_pattern_bindings(&inner, symbols, source_map, shared)?;
            merge_planned_bindings(&mut planned, inner, symbols, source_map)?;
            Ok(planned)
        }
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => plan_pattern_bindings(&inner, symbols, source_map, shared),
            None => Ok(Vec::new()),
        },
        ast::Pattern::Tuple(tuple) => {
            let mut planned = Vec::new();
            for element in tuple.elements() {
                let element = plan_pattern_bindings(&element, symbols, source_map, shared)?;
                merge_planned_bindings(&mut planned, element, symbols, source_map)?;
            }
            Ok(planned)
        }
        ast::Pattern::Slice(slice) if slice.rest_patterns().count() > 1 => Ok(Vec::new()),
        ast::Pattern::Slice(slice) => {
            let mut planned = Vec::new();
            for child in slice.syntax().children() {
                let child_bindings = if let Some(pattern) = ast::Pattern::cast(child.clone()) {
                    plan_pattern_bindings(&pattern, symbols, source_map, shared)?
                } else if let Some(rest) = ast::RestPat::cast(child) {
                    match rest.binding() {
                        Some(binding) => plan_binding(&binding, symbols, source_map, shared)?,
                        None => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                merge_planned_bindings(&mut planned, child_bindings, symbols, source_map)?;
            }
            Ok(planned)
        }
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_none_or(const_path_is_invalid) =>
        {
            Ok(Vec::new())
        }
        ast::Pattern::Constructor(constructor) => {
            let Some(args) = constructor.args() else {
                return Ok(Vec::new());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(Vec::new());
            };
            plan_ctor_args(&args, symbols, source_map, shared)
        }
        ast::Pattern::ImplicitMember(member) => {
            if member.name().is_none() {
                return Ok(Vec::new());
            }
            let Some(args) = member.args() else {
                return Ok(Vec::new());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(Vec::new());
            };
            plan_ctor_args(&args, symbols, source_map, shared)
        }
        ast::Pattern::Or(or) => plan_or_bindings(or, symbols, source_map, shared),
        ast::Pattern::Wildcard(_) | ast::Pattern::Literal(_) | ast::Pattern::Range(_) => {
            Ok(Vec::new())
        }
    }
}

fn plan_binding(
    binding_pat: &ast::BindingPat,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let Some(mode) = lower_binding_mode(binding_pat) else {
        return Ok(Vec::new());
    };
    let name = lower_binding_name(binding_pat, symbols, source_map)?;
    if name.sym.is_error() {
        return Ok(Vec::new());
    }
    let binding = match shared
        .and_then(|bindings| bindings.iter().find(|planned| planned.symbol == name.sym))
    {
        Some(planned) => {
            source_map.add_binding_alias(binding_pat.syntax(), planned.binding)?;
            planned.binding
        }
        None => {
            source_map
                .add_binding_with_span(binding_pat.syntax(), SpanCtxt::Root)?
                .0
        }
    };
    Ok(vec![PlannedBinding {
        symbol: name.sym,
        mode,
        binding,
        occurrences: vec![binding_pat.clone()],
    }])
}

fn plan_ctor_args(
    args: &[CtorPatArgSyntax],
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let mut planned = Vec::new();
    for arg in args {
        let pattern = match arg {
            CtorPatArgSyntax::Positional(pattern) | CtorPatArgSyntax::Named { pattern, .. } => {
                pattern
            }
        };
        let bindings = plan_pattern_bindings(pattern, symbols, source_map, shared)?;
        merge_planned_bindings(&mut planned, bindings, symbols, source_map)?;
    }
    Ok(planned)
}

fn merge_planned_bindings(
    planned: &mut Vec<PlannedBinding>,
    incoming: Vec<PlannedBinding>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<(), HirLoweringError> {
    for mut binding in incoming {
        let duplicate = planned
            .iter()
            .find(|existing| existing.symbol == binding.symbol);
        if let Some(existing) = duplicate {
            if existing.binding == binding.binding {
                let mut occurrences = binding.occurrences.iter();
                let first = occurrences
                    .next()
                    .expect("a planned binding has at least one syntax occurrence");
                binding.binding = source_map
                    .add_binding_with_span(first.syntax(), SpanCtxt::Root)?
                    .0;
                for occurrence in occurrences {
                    source_map.add_binding_alias(occurrence.syntax(), binding.binding)?;
                }
            }
            let duplicate_syntax = binding
                .occurrences
                .first()
                .expect("a planned binding has at least one syntax occurrence");
            let name = symbols
                .resolve(binding.symbol)
                .map(|name| name.to_string())
                .unwrap_or_else(|_| "<error>".to_owned());
            source_map.push_error_diagnostic_for_node(
                Code::DuplicateBinding,
                format!("`{name}` is bound more than once in this pattern"),
                duplicate_syntax.syntax(),
            )?;
        } else {
            planned.push(binding);
        }
    }
    Ok(())
}

fn plan_or_bindings(
    or: &ast::OrPat,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
    shared: Option<&[PlannedBinding]>,
) -> Result<Vec<PlannedBinding>, HirLoweringError> {
    let alternatives = flattened_or_alternatives(or);
    let Some(first) = alternatives.first() else {
        return Ok(Vec::new());
    };
    let first_bindings = plan_pattern_bindings(first, symbols, source_map, shared)?;
    let mut planned = first_bindings.clone();
    let mut mismatch = None;
    for alternative in alternatives.iter().skip(1) {
        let bindings = plan_pattern_bindings(alternative, symbols, source_map, Some(&planned))?;
        if mismatch.is_none() {
            mismatch = first_binding_mismatch(&first_bindings, &bindings);
        }
        for alternative_binding in bindings {
            if let Some(planned_binding) = planned
                .iter_mut()
                .find(|binding| binding.symbol == alternative_binding.symbol)
            {
                planned_binding
                    .occurrences
                    .extend(alternative_binding.occurrences.iter().cloned());
            } else {
                planned.push(alternative_binding);
            }
        }
    }
    if let Some(symbol) = mismatch {
        let name = symbols
            .resolve(symbol)
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "<error>".to_owned());
        source_map.push_error_diagnostic_for_node(
            Code::OrPatternBindingMismatch,
            format!("or-pattern alternatives do not bind `{name}` consistently"),
            or.syntax(),
        )?;
    }
    Ok(planned)
}

fn first_binding_mismatch(
    expected: &[PlannedBinding],
    actual: &[PlannedBinding],
) -> Option<Symbol> {
    for binding in expected {
        match actual
            .iter()
            .find(|candidate| candidate.symbol == binding.symbol)
        {
            Some(candidate) if candidate.mode == binding.mode => {}
            _ => return Some(binding.symbol),
        }
    }
    actual
        .iter()
        .find(|binding| {
            !expected
                .iter()
                .any(|candidate| candidate.symbol == binding.symbol)
        })
        .map(|binding| binding.symbol)
}

fn flattened_or_alternatives(or: &ast::OrPat) -> Vec<ast::Pattern> {
    fn flatten(pattern: &ast::Pattern, alternatives: &mut Vec<ast::Pattern>) {
        match pattern {
            ast::Pattern::Or(or) => {
                for alternative in or.alternatives() {
                    flatten(&alternative, alternatives);
                }
            }
            ast::Pattern::Parenthesized(paren) => match paren.pattern() {
                Some(inner) => flatten(&inner, alternatives),
                None => alternatives.push(pattern.clone()),
            },
            _ => alternatives.push(pattern.clone()),
        }
    }

    let mut alternatives = Vec::new();
    for alternative in or.alternatives() {
        flatten(&alternative, &mut alternatives);
    }
    alternatives
}

fn ensure_supported_pattern(pattern: &ast::Pattern) -> Result<(), HirLoweringError> {
    if contains_error_node(pattern.syntax()) {
        return Ok(());
    }

    match pattern {
        ast::Pattern::Wildcard(_) | ast::Pattern::Literal(_) | ast::Pattern::Binding(_) => Ok(()),
        ast::Pattern::Range(_) => Ok(()),
        ast::Pattern::Parenthesized(paren) => match paren.pattern() {
            Some(inner) => ensure_supported_pattern(&inner),
            None => Ok(()),
        },
        ast::Pattern::At(at) => match (at.binding(), at.pattern()) {
            (Some(binding), Some(inner)) if lower_binding_mode(&binding).is_some() => {
                ensure_supported_pattern(&inner)
            }
            _ => Ok(()),
        },
        ast::Pattern::Tuple(tuple) => {
            for element in tuple.elements() {
                ensure_supported_pattern(&element)?;
            }
            Ok(())
        }
        ast::Pattern::Slice(slice) if slice.rest_patterns().count() > 1 => Ok(()),
        ast::Pattern::Slice(slice) => {
            for element in slice.elements() {
                ensure_supported_pattern(&element)?;
            }
            Ok(())
        }
        ast::Pattern::Or(or) => {
            for alternative in flattened_or_alternatives(or) {
                ensure_supported_pattern(&alternative)?;
            }
            Ok(())
        }
        ast::Pattern::ImplicitMember(member) => {
            let Some(args) = member.args() else {
                return Ok(());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(());
            };
            for arg in args {
                let pattern = match arg {
                    CtorPatArgSyntax::Positional(pattern)
                    | CtorPatArgSyntax::Named { pattern, .. } => pattern,
                };
                ensure_supported_pattern(&pattern)?;
            }
            Ok(())
        }
        ast::Pattern::Constructor(constructor)
            if constructor.path().is_some_and(const_path_is_invalid) =>
        {
            Ok(())
        }
        ast::Pattern::Constructor(constructor) => {
            let Some(args) = constructor.args() else {
                return Ok(());
            };
            let Some((args, _)) = collect_ctor_pat_args(&args) else {
                return Ok(());
            };
            for arg in args {
                let pattern = match arg {
                    CtorPatArgSyntax::Positional(pattern)
                    | CtorPatArgSyntax::Named { pattern, .. } => pattern,
                };
                ensure_supported_pattern(&pattern)?;
            }
            Ok(())
        }
    }
}

enum CtorPatArgSyntax {
    Positional(ast::Pattern),
    Named {
        name: ast::NameRef,
        pattern: ast::Pattern,
    },
}

fn collect_ctor_pat_args(args: &ast::CtorPatArgs) -> Option<(Vec<CtorPatArgSyntax>, bool)> {
    let rests = args.rest_patterns().count();
    if rests > 1 {
        return None;
    }
    let mut lowered = Vec::new();
    for child in args.syntax().children() {
        if let Some(pattern) = ast::Pattern::cast(child.clone()) {
            lowered.push(CtorPatArgSyntax::Positional(pattern));
        } else if let Some(named) = ast::NamedPatArg::cast(child) {
            let (Some(name), Some(pattern)) = (named.name(), named.pattern()) else {
                return None;
            };
            lowered.push(CtorPatArgSyntax::Named { name, pattern });
        }
    }
    Some((lowered, rests == 1))
}

fn lower_ctor_pat_args<'hir>(
    args: &[CtorPatArgSyntax],
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir [PatArg<'hir>], HirLoweringError> {
    let mut lowered = Vec::with_capacity(args.len());
    for arg in args {
        match arg {
            CtorPatArgSyntax::Positional(pattern) => {
                let pattern = lower_pattern_inner(pattern, arena, symbols, source_map)?;
                lowered.push(PatArg {
                    name: None,
                    pat: pattern,
                });
            }
            CtorPatArgSyntax::Named { name, pattern } => {
                let name = lower_name_ref(name, symbols, source_map)?;
                let pattern = lower_pattern_inner(pattern, arena, symbols, source_map)?;
                lowered.push(PatArg {
                    name: Some(name),
                    pat: pattern,
                });
            }
        }
    }
    Ok(arena.alloc_pat_args(&lowered))
}

fn lower_binding_pattern<'hir>(
    binding_pat: &ast::BindingPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(binding_pat.syntax()) {
        return lower_error_pattern(binding_pat.syntax(), arena, source_map);
    }
    let Some(mode) = lower_binding_mode(binding_pat) else {
        return lower_error_pattern(binding_pat.syntax(), arena, source_map);
    };
    let (hir_id, span) = source_map.add_node_with_span(binding_pat.syntax(), SpanCtxt::Root)?;
    let name = lower_binding_name(binding_pat, symbols, source_map)?;
    let binding = lower_binding_id(binding_pat.syntax(), source_map)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Binding {
            mode,
            name,
            binding,
            sub: None,
        },
        span,
    }))
}

fn lower_at_pattern<'hir>(
    at: &ast::AtPat,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<&'hir Pat<'hir>, HirLoweringError> {
    if contains_error_node(at.syntax()) {
        return lower_error_pattern(at.syntax(), arena, source_map);
    }
    let (Some(binding_pat), Some(inner)) = (at.binding(), at.pattern()) else {
        return lower_error_pattern(at.syntax(), arena, source_map);
    };
    let Some(mode) = lower_binding_mode(&binding_pat) else {
        return lower_error_pattern(at.syntax(), arena, source_map);
    };

    let (hir_id, span) = source_map.add_node_with_span(at.syntax(), SpanCtxt::Root)?;
    let name = lower_binding_name(&binding_pat, symbols, source_map)?;
    let binding = lower_binding_id(binding_pat.syntax(), source_map)?;
    let sub = lower_pattern_inner(&inner, arena, symbols, source_map)?;
    Ok(arena.alloc_pat(Pat {
        hir_id,
        kind: PatKind::Binding {
            mode,
            name,
            binding,
            sub: Some(sub),
        },
        span,
    }))
}

fn lower_binding_id(
    syntax: &SyntaxNode,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<BindingId, HirLoweringError> {
    match source_map.binding_for_syntax(syntax)? {
        Some(binding) => Ok(binding),
        None => Ok(source_map.add_binding(syntax)?),
    }
}

fn lower_binding_mode(binding_pat: &ast::BindingPat) -> Option<HirBindingMode> {
    let Some(mode) = binding_pat.mode() else {
        return Some(HirBindingMode::Value);
    };
    match mode.kind()? {
        ast::BindingModeKind::Var => Some(HirBindingMode::Var),
        ast::BindingModeKind::Borrow => Some(HirBindingMode::Borrow),
        ast::BindingModeKind::BorrowMut => Some(HirBindingMode::BorrowMut),
    }
}

fn lower_binding_name(
    binding_pat: &ast::BindingPat,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    let Some(name) = binding_pat.name() else {
        return lower_name_spelling(binding_pat.syntax(), None, symbols, source_map);
    };
    let Some(token) = name.token() else {
        return lower_name_spelling(name.syntax(), None, symbols, source_map);
    };

    lower_name_spelling(name.syntax(), Some(token.text()), symbols, source_map)
}

fn lower_name_ref(
    name: &ast::NameRef,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    match name.syntax().first_token() {
        Some(token) => lower_name_spelling(name.syntax(), Some(token.text()), symbols, source_map),
        None => lower_name_spelling(name.syntax(), None, symbols, source_map),
    }
}

fn lower_name_spelling(
    syntax: &SyntaxNode,
    written: Option<&str>,
    symbols: &Interner,
    source_map: &HirSourceMapBuilder<'_>,
) -> Result<Name, HirLoweringError> {
    let span = source_map.span_for(syntax, SpanCtxt::Root)?;
    let Some(written) = written else {
        return Ok(Name {
            sym: Symbol::ERROR,
            span,
        });
    };
    let raw_name = written.strip_prefix("r#");
    let identifier = raw_name.unwrap_or(written);
    let invalid_raw = invalid_raw_identifier(written);
    let reserved = raw_name.is_none() && sai_syntax::is_reserved_identifier(identifier);
    let sym = if invalid_raw || reserved {
        Symbol::ERROR
    } else {
        let normalized = sai_syntax::normalize_identifier(identifier);
        symbols.intern(&normalized)?
    };
    Ok(Name { sym, span })
}

fn invalid_raw_identifier(written: &str) -> bool {
    matches!(written.strip_prefix("r#"), Some("self" | "Self" | "_"))
}

fn const_path_has_invalid_raw_identifier(path: ast::ConstPath) -> bool {
    path.segments().any(|segment| {
        segment
            .syntax()
            .first_token()
            .is_some_and(|token| invalid_raw_identifier(token.text()))
    })
}

fn const_path_is_invalid(path: ast::ConstPath) -> bool {
    path.segments().next().is_none() || const_path_has_invalid_raw_identifier(path)
}

enum PreparedRangeEndpoint<'hir> {
    Literal(Lit<'hir>),
    Path(ast::ConstPath),
}

fn prepare_range_endpoint<'hir>(
    endpoint: ast::RangePatEndpoint,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
) -> Result<Option<PreparedRangeEndpoint<'hir>>, HirLoweringError> {
    match endpoint {
        ast::RangePatEndpoint::Literal(literal) => {
            if contains_error_node(literal.syntax()) {
                return Ok(None);
            }
            let negative = literal.minus_token().is_some();
            let decoded = literal
                .value_token()
                .and_then(ast::LiteralToken::cast)
                .map(|token| decode_literal(token, arena, symbols, false, negative))
                .transpose()?
                .flatten();
            Ok(decoded.map(PreparedRangeEndpoint::Literal))
        }
        ast::RangePatEndpoint::ConstantPath(path) => Ok(Some(PreparedRangeEndpoint::Path(path))),
    }
}

fn lower_prepared_range_endpoint<'hir>(
    endpoint: PreparedRangeEndpoint<'hir>,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    source_map: &mut HirSourceMapBuilder<'_>,
) -> Result<PatRangeEnd<'hir>, HirLoweringError> {
    match endpoint {
        PreparedRangeEndpoint::Literal(literal) => Ok(PatRangeEnd::Lit(literal)),
        PreparedRangeEndpoint::Path(path) => Ok(PatRangeEnd::Path(lower_const_path(
            &path, arena, symbols, source_map,
        )?)),
    }
}

fn decode_literal<'hir>(
    token: ast::LiteralToken,
    arena: &'hir HirArena<'hir>,
    symbols: &Interner,
    allow_float: bool,
    negated: bool,
) -> Result<Option<Lit<'hir>>, HirLoweringError> {
    let literal = match token {
        ast::LiteralToken::Int(token) if !token.syntax().has_error() => Some(Lit::Int {
            value: token.value(),
            negated,
        }),
        ast::LiteralToken::Float(token) if allow_float && !token.syntax().has_error() => {
            let text = token.syntax().text().replace('_', "");
            Some(Lit::Float {
                text: symbols.intern(&text)?,
                negated,
            })
        }
        ast::LiteralToken::String(token) if !negated => {
            Some(Lit::Str(symbols.intern(token.value().as_ref())?))
        }
        ast::LiteralToken::ByteString(token) if !negated => {
            Some(Lit::ByteStr(arena.alloc_bytes(token.value().as_ref())))
        }
        ast::LiteralToken::Char(token) if !negated => Some(Lit::Char(token.value())),
        ast::LiteralToken::Byte(token) if !negated => Some(Lit::Byte(token.value())),
        ast::LiteralToken::True(_) if !negated => Some(Lit::Bool(true)),
        ast::LiteralToken::False(_) if !negated => Some(Lit::Bool(false)),
        _ => None,
    };
    Ok(literal)
}

fn contains_error_node(syntax: &SyntaxNode) -> bool {
    syntax
        .descendants()
        .any(|node| node.kind() == SyntaxKind::ERROR)
}

fn errors_are_in_recoverable_block_statements(syntax: &SyntaxNode) -> bool {
    let errors = syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::ERROR)
        .collect::<Vec<_>>();
    !errors.is_empty() && errors.iter().all(error_is_in_recoverable_block_statement)
}

fn errors_are_recoverable_closure_contexts(syntax: &SyntaxNode) -> bool {
    let errors = syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::ERROR)
        .collect::<Vec<_>>();
    !errors.is_empty()
        && errors.iter().any(error_is_in_closure_params)
        && errors.iter().all(|error| {
            error_is_in_closure_params(error)
                || error_is_in_recoverable_block_statement(error)
                || error_is_in_call_arglist(error)
                || error_is_in_recoverable_format_interpolation(error, syntax)
                || error_belongs_to_cast(error, syntax)
        })
}

fn error_is_in_closure_params(error: &SyntaxNode) -> bool {
    error.ancestors().any(|ancestor| {
        matches!(
            ancestor.kind(),
            SyntaxKind::CLOSURE_PARAM | SyntaxKind::CLOSURE_PARAM_LIST
        )
    })
}

fn error_is_in_recoverable_block_statement(error: &SyntaxNode) -> bool {
    error.ancestors().any(|ancestor| {
        matches!(
            ancestor.kind(),
            SyntaxKind::LET_STMT
                | SyntaxKind::VAR_STMT
                | SyntaxKind::ASSIGN_STMT
                | SyntaxKind::ASSERT_STMT
        )
    }) || is_direct_block_error(error)
}

fn error_is_in_call_arglist(error: &SyntaxNode) -> bool {
    error
        .ancestors()
        .any(|ancestor| ancestor.kind() == SyntaxKind::ARG_LIST)
}

fn errors_are_in_casts(syntax: &SyntaxNode) -> bool {
    let errors = syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::ERROR)
        .collect::<Vec<_>>();
    !errors.is_empty()
        && errors
            .iter()
            .all(|error| error_belongs_to_cast(error, syntax))
}

fn errors_are_in_recoverable_format_interpolations(syntax: &SyntaxNode) -> bool {
    let errors = syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::ERROR)
        .collect::<Vec<_>>();
    !errors.is_empty()
        && errors
            .iter()
            .all(|error| error_is_in_recoverable_format_interpolation(error, syntax))
}

fn error_is_in_recoverable_format_interpolation(error: &SyntaxNode, syntax: &SyntaxNode) -> bool {
    let mut ancestor = Some(error.clone());
    let mut in_interpolation = false;
    let mut in_format_string = false;
    while let Some(node) = ancestor {
        in_interpolation |= node.kind() == SyntaxKind::FSTRING_INTERP;
        in_format_string |= node.kind() == SyntaxKind::FSTRING_EXPR;
        if node.kind() == syntax.kind() && node.text_range() == syntax.text_range() {
            return in_interpolation && in_format_string;
        }
        ancestor = node.parent();
    }
    false
}

fn error_belongs_to_cast(error: &SyntaxNode, root: &SyntaxNode) -> bool {
    let mut ancestor = Some(error.clone());
    while let Some(node) = ancestor {
        if node.kind() == root.kind() && node.text_range() == root.text_range() {
            return node.kind() == SyntaxKind::CAST_EXPR;
        }
        if node.kind() == SyntaxKind::CAST_EXPR {
            return true;
        }
        ancestor = node.parent();
    }
    false
}

fn is_direct_block_error(error: &SyntaxNode) -> bool {
    error.ancestors().any(|ancestor| {
        ancestor.kind() == SyntaxKind::ERROR
            && ancestor
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::BLOCK)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId};
    use sai_intern::PackageInterner;
    use sai_syntax::{SyntaxKind, ast::AstNode};
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};
    use std::sync::Arc;

    fn parse_file(
        file: crate::FileId,
        source: &str,
    ) -> (
        WorkspaceSnapshot,
        sai_syntax::Parse,
        sai_syntax::ast::AstIdMap,
    ) {
        let source = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, Some(1))])
                .expect("valid snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("parse succeeds");
        let ast_ids = sai_syntax::ast::AstIdMap::new(file, &parsed.root).expect("AST ID map");
        (snapshot, parsed, ast_ids)
    }

    fn owner_id(symbols: &Interner) -> OwnerId {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:lowering#1.0")
            .expect("package interns");
        let paths = DefPathInterner::new(package, &packages, symbols).expect("path interner");
        let name = symbols.intern("f").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(name),
                disambiguator: 0,
            }],
            &packages,
            symbols,
        )
        .expect("definition path");
        OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        }
    }

    fn owner_context(symbols: &Interner) -> (PackageInterner, DefPathInterner, OwnerId) {
        let name = symbols.intern("f").expect("symbol interns");
        owner_context_with_symbol(symbols, name)
    }

    fn owner_context_with_symbol(
        symbols: &Interner,
        name: Symbol,
    ) -> (PackageInterner, DefPathInterner, OwnerId) {
        owner_context_with_data(symbols, DefPathData::ValueNs(name))
    }

    fn owner_context_with_data(
        symbols: &Interner,
        data: DefPathData,
    ) -> (PackageInterner, DefPathInterner, OwnerId) {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:closure-lowering#1.0")
            .expect("package interns");
        let paths = DefPathInterner::new(package, &packages, symbols).expect("path interner");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data,
                disambiguator: 0,
            }],
            &packages,
            symbols,
        )
        .expect("definition path");
        let owner = OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        };
        (packages, paths, owner)
    }

    fn owner_def_path(paths: &DefPathInterner, owner: OwnerId) -> DefPath {
        paths
            .resolve(owner.def_id)
            .expect("owner path resolves")
            .expect("owner path exists")
    }

    fn function_node(root: &sai_syntax::SyntaxNode) -> sai_syntax::SyntaxNode {
        root.descendants()
            .find(|node| node.kind() == SyntaxKind::FN_DECL)
            .expect("function declaration")
    }

    #[test]
    fn lowers_nested_borrow_tuple_owned_and_self_types() {
        let file = crate::FileId::new(114);
        let source = "fn f(a: borrow mut (I32, owned U8), b: Self.Nested) {}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration AST");
        let params = declaration
            .params()
            .expect("parameter list")
            .params()
            .collect::<Vec<_>>();
        let first_type = params[0].ty().expect("borrow tuple type");
        let second_type = params[1].ty().expect("Self-qualified type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered = lower_type(&first_type, &arena, &symbols, &mut source_map)
            .expect("nested borrow type lowers");
        let HirTyKind::Borrow(Mutability::Mut, tuple) = lowered.kind else {
            panic!("the outer type is a mutable borrow");
        };
        let HirTyKind::Tuple(elements) = tuple.kind else {
            panic!("the borrowed type is a tuple");
        };
        assert_eq!(elements.len(), 2);
        let HirTyKind::Path(first_path) = elements[0].kind else {
            panic!("the first tuple element is a path type");
        };
        let HirTyKind::Path(owned_path) = elements[1].kind else {
            panic!("owned is erased to its inner path type");
        };
        assert_eq!(
            symbols
                .resolve(owned_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "U8"
        );
        assert_eq!(
            elements[1].span.range,
            params[0]
                .ty()
                .unwrap()
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::PATH_TYPE
                    && node.text_range().start() > elements[0].span.range.start())
                .expect("the owned inner path syntax")
                .text_range()
        );

        let self_type = lower_type(&second_type, &arena, &symbols, &mut source_map)
            .expect("Self path type lowers");
        let HirTyKind::Path(self_path) = self_type.kind else {
            panic!("Self type lowers to a path");
        };
        assert_eq!(self_path.root, PathRoot::SelfType);
        assert_eq!(self_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(self_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "Nested"
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                tuple.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                first_path.hir_id.local_id.raw(),
                first_path.segments[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                owned_path.hir_id.local_id.raw(),
                owned_path.segments[0].hir_id.local_id.raw(),
                self_type.hir_id.local_id.raw(),
                self_path.hir_id.local_id.raw(),
                self_path.segments[0].hir_id.local_id.raw(),
            ],
            std::array::from_fn::<_, 11, _>(|index| index as u32 + 1)
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 12);
    }

    #[test]
    fn lowers_mixed_generic_arguments_and_function_pointer_effects() {
        let file = crate::FileId::new(115);
        let source = concat!(
            "fn f<T: pkg.Trait<I32, U8, -3, 4, 'c', true, false, Item = U8, Other = I32>>(",
            "a: pkg.Foo<I32, -3, 4, 'c', true, false>, ",
            "callback: unsafe fn(I32, U8) -> Bool effects { io }, ",
            "pure: fn(), ",
            "empty: fn() effects {}) {}\n"
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration AST");
        let params = declaration
            .params()
            .expect("parameter list")
            .params()
            .collect::<Vec<_>>();
        let path_type = params[0].ty().expect("generic path type");
        let function_type = params[1].ty().expect("function pointer type");
        let pure_function_type = params[2].ty().expect("pure function pointer type");
        let empty_effects_type = params[3].ty().expect("explicit empty effect set");
        let trait_path = function
            .descendants()
            .filter_map(ast::PathType::cast)
            .find(|path| {
                path.segments().any(|segment| {
                    segment
                        .generic_args()
                        .is_some_and(|args| args.assoc_type_bindings().count() == 2)
                })
            })
            .expect("trait bound path with associated type arguments");
        let trait_type =
            ast::Type::cast(trait_path.syntax().clone()).expect("trait bound path type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered_path = lower_type(&path_type, &arena, &symbols, &mut source_map)
            .expect("generic path type lowers");
        let HirTyKind::Path(path) = lowered_path.kind else {
            panic!("generic path lowers to a path type");
        };
        assert_eq!(path.root, PathRoot::Plain);
        assert_eq!(path.segments.len(), 2);
        let args = path.segments[1].args.expect("generic arguments");
        assert_eq!(args.args.len(), 6);
        assert!(matches!(args.args[0], HirGenericArg::Type(_)));
        assert_eq!(
            args.args[1],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(3),
                negated: true,
            })
        );
        assert_eq!(
            args.args[2],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(4),
                negated: false,
            })
        );
        assert_eq!(args.args[3], HirGenericArg::Const(HirConstArg::Char('c')));
        assert_eq!(args.args[4], HirGenericArg::Const(HirConstArg::Bool(true)));
        assert_eq!(args.args[5], HirGenericArg::Const(HirConstArg::Bool(false)));
        assert_eq!(
            args.span.range,
            path_type
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::GENERIC_ARG_LIST)
                .expect("generic argument list")
                .text_range()
        );

        let lowered_trait = lower_type(&trait_type, &arena, &symbols, &mut source_map)
            .expect("trait bound type lowers");
        let HirTyKind::Path(trait_path) = lowered_trait.kind else {
            panic!("trait bound lowers to a path type");
        };
        let trait_args = trait_path.segments[1].args.expect("trait arguments");
        assert_eq!(trait_args.args.len(), 9);
        assert!(matches!(
            trait_args.args[7],
            HirGenericArg::Binding { name, .. }
                if symbols.resolve(name.sym).unwrap().as_ref() == "Item"
        ));
        assert!(matches!(
            trait_args.args[8],
            HirGenericArg::Binding { name, .. }
                if symbols.resolve(name.sym).unwrap().as_ref() == "Other"
        ));

        let lowered_function = lower_type(&function_type, &arena, &symbols, &mut source_map)
            .expect("function pointer type lowers");
        let HirTyKind::FnPtr {
            safety,
            params,
            ret,
            effects,
        } = lowered_function.kind
        else {
            panic!("function pointer lowers to TyKind::FnPtr");
        };
        assert_eq!(safety, Safety::Unsafe);
        assert_eq!(params.len(), 2);
        assert!(matches!(ret.map(|ty| ty.kind), Some(HirTyKind::Path(_))));
        let effects = effects.expect("effect set");
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(effects[0].segments[0].sym)
                .unwrap()
                .as_ref(),
            "io"
        );
        assert_eq!(lowered_function.span.ctxt, SpanCtxt::Root);
        let lowered_pure_function =
            lower_type(&pure_function_type, &arena, &symbols, &mut source_map)
                .expect("pure function pointer type lowers");
        assert!(matches!(
            lowered_pure_function.kind,
            HirTyKind::FnPtr {
                safety: Safety::Safe,
                params,
                ret: None,
                effects: None,
            } if params.is_empty()
        ));
        let lowered_empty_effects =
            lower_type(&empty_effects_type, &arena, &symbols, &mut source_map)
                .expect("explicit empty effect set lowers");
        assert!(matches!(
            lowered_empty_effects.kind,
            HirTyKind::FnPtr {
                effects: Some(effects),
                ..
            } if effects.is_empty()
        ));
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn lowers_unit_grouped_and_inference_types() {
        let file = crate::FileId::new(118);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f(unit: (), grouped: (U8), inferred: _) {}\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration AST");
        let params = declaration
            .params()
            .expect("parameter list")
            .params()
            .collect::<Vec<_>>();
        let unit_type = params[0].ty().expect("unit type");
        let grouped_type = params[1].ty().expect("grouped type");
        let inferred_type = params[2].ty().expect("inference type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let unit =
            lower_type(&unit_type, &arena, &symbols, &mut source_map).expect("unit type lowers");
        assert!(matches!(unit.kind, HirTyKind::Tuple(elements) if elements.is_empty()));
        let grouped = lower_type(&grouped_type, &arena, &symbols, &mut source_map)
            .expect("grouped type lowers");
        let HirTyKind::Path(path) = grouped.kind else {
            panic!("parentheses are transparent in type HIR");
        };
        assert_eq!(
            symbols.resolve(path.segments[0].name.sym).unwrap().as_ref(),
            "U8"
        );
        assert_eq!(
            grouped.span.range,
            grouped_type
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::PATH_TYPE)
                .expect("inner path type")
                .text_range()
        );
        let inferred = lower_type(&inferred_type, &arena, &symbols, &mut source_map)
            .expect("inference type lowers");
        assert_eq!(inferred.kind, HirTyKind::Infer);
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn malformed_function_pointer_effects_make_the_type_err() {
        for (index, source) in [
            "fn f(callback: fn() effects) {}\n",
            "fn f(callback: fn() effects { io, 123, net }) {}\n",
            "fn f(callback: fn() effects { io. }) {}\n",
            "fn f(callback: fn() effects { io, null }) {}\n",
        ]
        .into_iter()
        .enumerate()
        {
            let file = crate::FileId::new(119 + index as u32);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source}");
            let function = function_node(&parsed.root);
            let function_type = function
                .descendants()
                .find_map(ast::FnPtrType::cast)
                .expect("function pointer type");
            let ty = ast::Type::cast(function_type.syntax().clone()).expect("type union member");
            let arena = HirArena::new();
            let symbols = Interner::new();
            let owner = owner_id(&symbols);
            let mut source_map =
                HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                    .expect("source map builder");

            let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
                .expect("malformed effect syntax lowers to an error type");
            assert_eq!(lowered.kind, HirTyKind::Err, "{source}");
            assert_eq!(source_map.finish().node_to_syntax().len(), 2, "{source}");
        }
    }

    #[test]
    fn malformed_generic_argument_preserves_valid_siblings() {
        let file = crate::FileId::new(116);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f(value: Foo<I32, borrow, U8>) {}\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration AST");
        let ty = declaration
            .params()
            .and_then(|params| params.params().next())
            .and_then(|param| param.ty())
            .expect("recovered type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_type(&ty, &arena, &symbols, &mut source_map).expect("recovered type lowers");
        let HirTyKind::Path(path) = lowered.kind else {
            panic!("a malformed generic argument preserves the path type");
        };
        let args = path.segments[0].args.expect("generic argument list").args;
        assert_eq!(args.len(), 3);
        assert!(matches!(
            args[0],
            HirGenericArg::Type(ty)
                if matches!(ty.kind, HirTyKind::Path(path)
                    if symbols.resolve(path.segments[0].name.sym).unwrap().as_ref() == "I32")
        ));
        assert!(matches!(args[1], HirGenericArg::Type(ty) if ty.kind == HirTyKind::Err));
        assert!(matches!(
            args[2],
            HirGenericArg::Type(ty)
                if matches!(ty.kind, HirTyKind::Path(path)
                    if symbols.resolve(path.segments[0].name.sym).unwrap().as_ref() == "U8")
        ));
        assert_eq!(lowered.span.range, ty.syntax().text_range());
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn empty_generic_argument_list_retains_an_error_placeholder() {
        for (index, source) in ["fn f(value: Foo<>) {}\n", "fn f(value: Foo<,>) {}\n"]
            .into_iter()
            .enumerate()
        {
            let file = crate::FileId::new(122 + index as u32);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source}");
            let function = function_node(&parsed.root);
            let declaration =
                ast::FnDecl::cast(function.clone()).expect("function declaration AST");
            let ty = declaration
                .params()
                .and_then(|params| params.params().next())
                .and_then(|param| param.ty())
                .expect("recovered type");
            let arena = HirArena::new();
            let symbols = Interner::new();
            let owner = owner_id(&symbols);
            let mut source_map =
                HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                    .expect("source map builder");

            let lowered =
                lower_type(&ty, &arena, &symbols, &mut source_map).expect("recovered type lowers");
            let HirTyKind::Path(path) = lowered.kind else {
                panic!("a recovered empty generic list preserves its path type");
            };
            let args = path.segments[0].args.expect("generic arguments").args;
            assert_eq!(args.len(), 1, "{source}");
            assert!(matches!(
                args[0],
                HirGenericArg::Type(ty) if ty.kind == HirTyKind::Err
            ));
            assert!(source_map.finish().node_to_syntax().len() > 2, "{source}");
        }
    }

    #[test]
    fn malformed_const_argument_is_err_but_overflow_remains_decodable() {
        for (index, (source, expect_parse_error)) in [
            ("fn f(value: Foo<1__2>) {}\n", true),
            (
                "fn f(value: Foo<340282366920938463463374607431768211456>) {}\n",
                false,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let file = crate::FileId::new(126 + index as u32);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert_eq!(!parsed.errors.is_empty(), expect_parse_error, "{source}");
            let function = function_node(&parsed.root);
            let declaration =
                ast::FnDecl::cast(function.clone()).expect("function declaration AST");
            let ty = declaration
                .params()
                .and_then(|params| params.params().next())
                .and_then(|param| param.ty())
                .expect("path type");
            let arena = HirArena::new();
            let symbols = Interner::new();
            let owner = owner_id(&symbols);
            let mut source_map =
                HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                    .expect("source map builder");

            let lowered =
                lower_type(&ty, &arena, &symbols, &mut source_map).expect("path type lowers");
            let HirTyKind::Path(path) = lowered.kind else {
                panic!("constant argument recovery preserves the generic path");
            };
            let args = path.segments[0].args.expect("generic arguments").args;
            assert_eq!(args.len(), 1);
            if expect_parse_error {
                assert!(matches!(
                    args[0],
                    HirGenericArg::Type(ty) if ty.kind == HirTyKind::Err
                ));
            } else {
                assert_eq!(
                    args[0],
                    HirGenericArg::Const(HirConstArg::Int {
                        value: None,
                        negated: false,
                    })
                );
            }
            assert!(source_map.finish().node_to_syntax().len() > 2);
        }
    }

    #[test]
    fn malformed_tuple_element_does_not_discard_valid_siblings() {
        let file = crate::FileId::new(117);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(value: (I32, , U8)) {}\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration AST");
        let ty = declaration
            .params()
            .and_then(|params| params.params().next())
            .and_then(|param| param.ty())
            .expect("recovered tuple type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
            .expect("recovered tuple type lowers");
        let HirTyKind::Tuple(elements) = lowered.kind else {
            panic!("the malformed syntax still has a tuple type");
        };
        assert_eq!(elements.len(), 3);
        assert!(matches!(elements[0].kind, HirTyKind::Path(_)));
        assert_eq!(elements[1].kind, HirTyKind::Err);
        let HirTyKind::Path(last) = elements[2].kind else {
            panic!("the final tuple element remains a path type");
        };
        assert_eq!(
            symbols.resolve(last.segments[0].name.sym).unwrap().as_ref(),
            "U8"
        );
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn missing_type_delimiters_do_not_create_phantom_children() {
        for (index, (source, expect_tuple, expected_count)) in [
            ("type Pair = (I32, U8", true, 2),
            ("type Pair = (U8,,,", true, 3),
            ("type Pair = (U8, , ,", true, 3),
            ("type Boxed = Foo<U8", false, 1),
            ("type Boxed = Foo<U8,,", false, 2),
            ("type Boxed = Foo<U8,,,", false, 3),
            ("type Boxed = Foo<U8, , ,", false, 3),
        ]
        .into_iter()
        .enumerate()
        {
            let file = crate::FileId::new(124 + index as u32);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source}");
            let type_decl = parsed
                .root
                .descendants()
                .find_map(ast::TypeDecl::cast)
                .expect("type declaration");
            let ty = type_decl
                .alias_def()
                .and_then(|alias| alias.ty())
                .expect("aliased type");
            let arena = HirArena::new();
            let symbols = Interner::new();
            let owner = owner_id(&symbols);
            let mut source_map = HirSourceMapBuilder::new(
                owner,
                &ast_ids,
                &snapshot,
                &parsed.root,
                type_decl.syntax(),
            )
            .expect("source map builder");

            let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
                .expect("type with a missing closing delimiter lowers");
            if expect_tuple {
                let HirTyKind::Tuple(elements) = lowered.kind else {
                    panic!("the missing tuple delimiter retains its tuple type");
                };
                assert_eq!(elements.len(), expected_count, "{source}");
                if expected_count > 2 {
                    assert!(
                        elements[1..]
                            .iter()
                            .all(|ty| ty.kind == HirTyKind::Err && ty.span.range.is_empty())
                    );
                }
            } else {
                let HirTyKind::Path(path) = lowered.kind else {
                    panic!("the missing generic delimiter retains its path type");
                };
                let args = path.segments[0].args.expect("generic args").args;
                assert_eq!(args.len(), expected_count, "{source}");
                if expected_count > 1 {
                    assert!(
                        args[1..].iter().all(
                            |arg| matches!(arg, HirGenericArg::Type(ty) if ty.kind == HirTyKind::Err)
                        ),
                        "{source}"
                    );
                    assert!(
                        args[1..]
                            .iter()
                            .all(|arg| matches!(arg, HirGenericArg::Type(ty)
                            if ty.kind == HirTyKind::Err && ty.span.range.is_empty())),
                        "{source}"
                    );
                }
            }
            assert!(source_map.finish().node_to_syntax().len() > 2);
        }
    }

    #[test]
    fn missing_inner_generic_delimiter_does_not_consume_outer_separator() {
        let file = crate::FileId::new(130);
        let (snapshot, parsed, ast_ids) = parse_file(file, "type X = Outer<Inner<U8,, T>\n");
        assert!(!parsed.errors.is_empty());
        let type_decl = parsed
            .root
            .descendants()
            .find_map(ast::TypeDecl::cast)
            .expect("type declaration");
        let ty = type_decl
            .alias_def()
            .and_then(|alias| alias.ty())
            .expect("aliased type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, type_decl.syntax())
                .expect("source map builder");

        let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
            .expect("nested type with a missing delimiter lowers");
        let HirTyKind::Path(outer) = lowered.kind else {
            panic!("the outer generic path is retained");
        };
        let outer_args = outer.segments[0].args.expect("outer generic arguments");
        assert_eq!(outer_args.args.len(), 2);
        let HirGenericArg::Type(inner_ty) = outer_args.args[0] else {
            panic!("the first outer argument is the inner type");
        };
        let HirTyKind::Path(inner) = inner_ty.kind else {
            panic!("the inner generic path is retained");
        };
        let inner_args = inner.segments[0].args.expect("inner generic arguments");
        assert_eq!(inner_args.args.len(), 1);
        assert!(matches!(
            inner_args.args[0],
            HirGenericArg::Type(ty) if matches!(ty.kind, HirTyKind::Path(_))
        ));
        assert!(matches!(outer_args.args[1], HirGenericArg::Type(_)));
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn missing_generic_delimiter_does_not_consume_function_pointer_parameter_separator() {
        let file = crate::FileId::new(131);
        let (snapshot, parsed, ast_ids) = parse_file(file, "type F = fn(Foo<U8, , Bar)\n");
        assert!(!parsed.errors.is_empty());
        let type_decl = parsed
            .root
            .descendants()
            .find_map(ast::TypeDecl::cast)
            .expect("type declaration");
        let ty = type_decl
            .alias_def()
            .and_then(|alias| alias.ty())
            .expect("aliased type");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, type_decl.syntax())
                .expect("source map builder");

        let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
            .expect("function pointer with a recovered generic type lowers");
        let HirTyKind::FnPtr { params, .. } = lowered.kind else {
            panic!("the type remains a function pointer");
        };
        assert_eq!(params.len(), 2);
        let HirTyKind::Path(first) = params[0].kind else {
            panic!("the first parameter remains the generic path");
        };
        assert_eq!(first.segments[0].args.expect("generic args").args.len(), 1);
        let HirTyKind::Path(second) = params[1].kind else {
            panic!("the second parameter remains a path");
        };
        assert_eq!(
            symbols
                .resolve(second.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "Bar"
        );
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn missing_generic_delimiter_does_not_consume_variant_payload_separator() {
        for (index, source) in [
            "enum E { V(Foo<U8, , Bar) }\n",
            "enum E { V(first: Foo<U8, , second: Bar) }\n",
        ]
        .into_iter()
        .enumerate()
        {
            let file = crate::FileId::new(132 + index as u32);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source}");
            let enum_decl = parsed
                .root
                .descendants()
                .find_map(ast::EnumDecl::cast)
                .expect("enum declaration");
            let foo = enum_decl
                .syntax()
                .descendants()
                .filter_map(ast::PathType::cast)
                .find(|path| {
                    path.segments()
                        .next()
                        .and_then(|segment| segment.name())
                        .and_then(|name| name.syntax().first_token())
                        .is_some_and(|token| token.text() == "Foo")
                })
                .expect("Foo type in variant payload");
            assert!(
                enum_decl
                    .syntax()
                    .descendants()
                    .filter_map(ast::PathType::cast)
                    .any(|path| {
                        path.segments()
                            .next()
                            .and_then(|segment| segment.name())
                            .and_then(|name| name.syntax().first_token())
                            .is_some_and(|token| token.text() == "Bar")
                    }),
                "the following payload type survives: {source}"
            );
            let ty = ast::Type::cast(foo.syntax().clone()).expect("type union member");
            let arena = HirArena::new();
            let symbols = Interner::new();
            let owner = owner_id(&symbols);
            let mut source_map = HirSourceMapBuilder::new(
                owner,
                &ast_ids,
                &snapshot,
                &parsed.root,
                enum_decl.syntax(),
            )
            .expect("source map builder");

            let lowered = lower_type(&ty, &arena, &symbols, &mut source_map)
                .expect("recovered generic path lowers");
            let HirTyKind::Path(path) = lowered.kind else {
                panic!("Foo remains a path type");
            };
            assert_eq!(
                path.segments[0].args.expect("generic arguments").args.len(),
                1,
                "{source}"
            );
            assert!(source_map.finish().node_to_syntax().len() > 2);
        }
    }

    #[test]
    fn missing_generic_delimiter_does_not_consume_where_predicate_separator() {
        let file = crate::FileId::new(134);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() where Foo<U8, , T: Trait {}\n");
        assert!(!parsed.errors.is_empty());
        let function = parsed
            .root
            .descendants()
            .find_map(ast::FnDecl::cast)
            .expect("function declaration");
        let foo = function
            .syntax()
            .descendants()
            .filter_map(ast::PathType::cast)
            .find(|path| {
                path.segments()
                    .next()
                    .and_then(|segment| segment.name())
                    .and_then(|name| name.syntax().first_token())
                    .is_some_and(|token| token.text() == "Foo")
            })
            .expect("Foo type in where predicate");
        assert!(
            function
                .syntax()
                .descendants()
                .filter_map(ast::PathType::cast)
                .any(|path| {
                    path.segments()
                        .next()
                        .and_then(|segment| segment.name())
                        .and_then(|name| name.syntax().first_token())
                        .is_some_and(|token| token.text() == "T")
                }),
            "the next where predicate's type survives"
        );
        let ty = ast::Type::cast(foo.syntax().clone()).expect("type union member");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, function.syntax())
                .expect("source map builder");

        let lowered =
            lower_type(&ty, &arena, &symbols, &mut source_map).expect("recovered type lowers");
        let HirTyKind::Path(path) = lowered.kind else {
            panic!("Foo remains a path type");
        };
        assert_eq!(path.segments[0].args.expect("generic args").args.len(), 1);
        assert!(source_map.finish().node_to_syntax().len() > 2);
    }

    #[test]
    fn lowers_decoded_literals_and_records_source_spans() {
        let file = crate::FileId::new(61);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r#"fn f() { (42, 340282366920938463463374607431768211455, 340282366920938463463374607431768211456, 1.5_0, "text\n", b"bytes\x21", 'x', b'y', true, false) }
"#,
        );
        let function = function_node(&parsed.root);
        let literals = function
            .descendants()
            .filter_map(ast::LiteralExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(literals.len(), 10);
        assert!(!contains_error_node(literals[2].syntax()));

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = literals
            .iter()
            .map(|literal| lower_literal(literal, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literals lower");

        assert_eq!(
            lowered[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(42),
                negated: false,
            })
        );
        assert_eq!(
            lowered[1].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(u128::MAX),
                negated: false,
            })
        );
        assert_eq!(
            lowered[2].kind,
            ExprKind::Lit(Lit::Int {
                value: None,
                negated: false,
            })
        );
        assert_eq!(
            lowered[3].kind,
            ExprKind::Lit(Lit::Float {
                text: symbols.intern("1.50").expect("float spelling interns"),
                negated: false,
            })
        );
        let decoded_string = match lowered[4].kind {
            ExprKind::Lit(Lit::Str(value)) => symbols.resolve(value).expect("string resolves"),
            _ => panic!("string literal lowers to Lit::Str"),
        };
        assert_eq!(decoded_string.as_ref(), "text\n");
        assert_eq!(lowered[5].kind, ExprKind::Lit(Lit::ByteStr(b"bytes!")));
        assert_eq!(lowered[6].kind, ExprKind::Lit(Lit::Char('x')));
        assert_eq!(lowered[7].kind, ExprKind::Lit(Lit::Byte(b'y')));
        assert_eq!(lowered[8].kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[9].kind, ExprKind::Lit(Lit::Bool(false)));
        assert!(
            lowered
                .iter()
                .enumerate()
                .all(|(index, expr)| expr.hir_id.local_id.raw() == index as u32 + 1)
        );
        for (expr, syntax) in lowered.iter().zip(&literals) {
            assert_eq!(expr.span.range, syntax.syntax().text_range());
            assert_eq!(expr.span.file, file);
            assert_eq!(expr.span.ctxt, SpanCtxt::Root);
        }

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), literals.len() + 1);
    }

    #[test]
    fn lowers_format_strings_with_decoded_text_specs_and_source_spans() {
        let file = crate::FileId::new(197);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r###"fn f(name: Str, age: I32) { f"Hello {{name}} {name}, age {age:pad(3)}!" }
"###,
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let format_string = function
            .descendants()
            .find_map(ast::FStringExpr::cast)
            .expect("format string expression");
        let expression =
            ast::Expr::cast(format_string.syntax().clone()).expect("format string expression AST");
        let source_interpolations = format_string
            .syntax()
            .children()
            .filter_map(ast::FStringInterp::cast)
            .collect::<Vec<_>>();
        let source_spec = format_string
            .syntax()
            .descendants()
            .find_map(|node| {
                node.children_with_tokens()
                    .find_map(|element| match element {
                        SyntaxElement::Token(token) => ast::FormatSpec::cast(token),
                        SyntaxElement::Node(_) => None,
                    })
            })
            .expect("format spec token");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("format lowers");
        let ExprKind::FormatString(pieces) = lowered.kind else {
            panic!("format string remains an ExprKind::FormatString");
        };

        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        assert_eq!(lowered.span.range, format_string.syntax().text_range());
        assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        assert_eq!(pieces.len(), 5);
        let FmtPiece::Text(text) = pieces[0] else {
            panic!("the first piece is decoded text");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), "Hello {name} ");
        let FmtPiece::Arg {
            expr: first_arg,
            spec: None,
        } = pieces[1]
        else {
            panic!("the first interpolation has no format spec");
        };
        assert_eq!(first_arg.span.ctxt, SpanCtxt::Root);
        assert_eq!(
            first_arg.span.range,
            source_interpolations[0]
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::PATH_EXPR)
                .expect("first interpolation expression")
                .text_range()
        );
        let FmtPiece::Text(text) = pieces[2] else {
            panic!("the middle piece is text");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), ", age ");
        let FmtPiece::Arg {
            expr: second_arg,
            spec: Some(spec),
        } = pieces[3]
        else {
            panic!("the second interpolation carries its format spec");
        };
        assert_eq!(spec.name, symbols.intern("pad").unwrap());
        assert_eq!(spec.arg, Some(3));
        assert_eq!(spec.span.file, file);
        assert_eq!(spec.span.range, source_spec.syntax().text_range());
        assert_eq!(spec.span.ctxt, SpanCtxt::Root);
        assert_eq!(second_arg.span.ctxt, SpanCtxt::Root);
        let FmtPiece::Text(text) = pieces[4] else {
            panic!("the final piece is text");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), "!");

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 8);
    }

    #[test]
    fn format_string_preflight_rejects_unsupported_interpolations_before_allocation() {
        let file = crate::FileId::new(198);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { f\"{fn() {}}\" }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let format_string = function
            .descendants()
            .find_map(ast::FStringExpr::cast)
            .expect("format string expression");
        let expression =
            ast::Expr::cast(format_string.syntax().clone()).expect("format string expression AST");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        assert!(matches!(
            lower_expr(&expression, &arena, &symbols, &mut source_map),
            Err(HirLoweringError::UnsupportedSyntax(
                SyntaxKind::CLOSURE_EXPR
            ))
        ));
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 1);
        assert!(map.binding_to_syntax().is_empty());
    }

    #[test]
    fn malformed_format_string_text_keeps_the_ast_decoder_recovery_value() {
        let file = crate::FileId::new(199);
        let (snapshot, parsed, ast_ids) = parse_file(file, r#"fn f() { f"bad\q" }"#);
        let function = function_node(&parsed.root);
        let format_string = function
            .descendants()
            .find_map(ast::FStringExpr::cast)
            .expect("format string expression");
        let expression =
            ast::Expr::cast(format_string.syntax().clone()).expect("format string expression AST");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("format lowers");
        let ExprKind::FormatString(pieces) = lowered.kind else {
            panic!("lexical text errors do not discard the format string");
        };
        assert!(matches!(
            pieces,
            [FmtPiece::Text(text)] if symbols.resolve(*text).unwrap().as_ref() == "bad\u{fffd}"
        ));
    }

    #[test]
    fn malformed_format_interpolation_keeps_its_sibling_pieces() {
        let file = crate::FileId::new(200);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r#"fn f(value: I32) { f"before {1 + } after {value} end" }"#,
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let format_string = function
            .descendants()
            .find_map(ast::FStringExpr::cast)
            .expect("format string expression");
        let interpolation_errors = format_string
            .syntax()
            .descendants()
            .filter(|node| {
                node.kind() == SyntaxKind::ERROR
                    && node
                        .ancestors()
                        .any(|ancestor| ancestor.kind() == SyntaxKind::FSTRING_INTERP)
            })
            .collect::<Vec<_>>();
        assert_eq!(interpolation_errors.len(), 1);
        let expression =
            ast::Expr::cast(format_string.syntax().clone()).expect("format string expression AST");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("format recovers");
        let ExprKind::FormatString(pieces) = lowered.kind else {
            panic!("the outer format string is preserved after an interpolation error");
        };
        assert_eq!(pieces.len(), 5);
        let FmtPiece::Text(text) = pieces[0] else {
            panic!("the leading text is retained");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), "before ");
        let FmtPiece::Arg { expr, .. } = pieces[1] else {
            panic!("the malformed interpolation remains an argument");
        };
        assert_eq!(expr.kind, ExprKind::Err);
        assert_eq!(expr.span.range, interpolation_errors[0].text_range());
        let FmtPiece::Text(text) = pieces[2] else {
            panic!("text after the malformed interpolation is retained");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), " after ");
        let FmtPiece::Arg { expr, .. } = pieces[3] else {
            panic!("the later valid interpolation remains an argument");
        };
        assert!(matches!(expr.kind, ExprKind::Path(_)));
        let FmtPiece::Text(text) = pieces[4] else {
            panic!("the trailing text is retained");
        };
        assert_eq!(symbols.resolve(text).unwrap().as_ref(), " end");

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 6);
    }

    #[test]
    fn lowers_unit_parenthesized_and_tuple_expressions() {
        let file = crate::FileId::new(72);
        let source = "fn f() { ((), (1, ((true,)))) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("outer tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("supported tuple expression lowers");

        fn tuple_elements<'hir>(expression: &Expr<'hir>) -> &'hir [Expr<'hir>] {
            match expression.kind {
                ExprKind::Tuple(elements) => elements,
                _ => panic!("unit and tuple expressions lower to ExprKind::Tuple"),
            }
        }
        let outer = tuple_elements(lowered);
        assert_eq!(outer.len(), 2);
        assert!(tuple_elements(&outer[0]).is_empty());
        assert_eq!(
            outer[0].span.range,
            function
                .descendants()
                .find(|node| node.kind() == SyntaxKind::UNIT_EXPR)
                .expect("unit expression syntax")
                .text_range()
        );

        let inner = tuple_elements(&outer[1]);
        assert_eq!(inner.len(), 2);
        assert_eq!(
            inner[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let singleton = tuple_elements(&inner[1]);
        assert_eq!(singleton.len(), 1);
        assert_eq!(singleton[0].kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(
            lowered.hir_id.local_id.raw(),
            1,
            "the outer tuple receives its ID before its children"
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                outer[0].hir_id.local_id.raw(),
                outer[1].hir_id.local_id.raw(),
                inner[0].hir_id.local_id.raw(),
                inner[1].hir_id.local_id.raw(),
                singleton[0].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(lowered.span.range, tuple.syntax().text_range());
        let paren = function
            .descendants()
            .find_map(ast::ParenExpr::cast)
            .expect("parenthesized expression syntax");
        let paren_pointer =
            sai_syntax::SyntaxNodePtr::new(file, paren.syntax()).expect("parenthesis pointer");
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 7);
        assert!(
            map.nodes_for_syntax(&paren_pointer).is_empty(),
            "parentheses reuse their contents without a separate HIR node"
        );
    }

    #[test]
    fn lowers_arrays_with_source_order_parent_first_ids_and_spans() {
        let file = crate::FileId::new(88);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { [1, [2], 3] }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("array lowers");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("array syntax lowers to ExprKind::Array");
        };
        assert_eq!(elements.len(), 3);
        assert_eq!(
            elements[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let ExprKind::Array(nested) = elements[1].kind else {
            panic!("nested array remains an array expression");
        };
        assert_eq!(
            nested[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(
            elements[2].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                nested[0].hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5]
        );
        assert_eq!(lowered.span.range, array.syntax().text_range());
        assert_eq!(
            elements[1].span.range,
            array.elements().nth(1).unwrap().syntax().text_range()
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 6);
    }

    #[test]
    fn array_recovery_keeps_missing_elements_and_lowers_generic_fields() {
        let file = crate::FileId::new(89);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { [, call(, 2)] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let array_error = array
            .syntax()
            .children()
            .find(|child| child.kind() == SyntaxKind::ERROR)
            .expect("missing array element recovery node");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("array recovers");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("the array is preserved around missing elements");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].kind, ExprKind::Err);
        let ExprKind::Call { args, .. } = elements[1].kind else {
            panic!("the valid call sibling remains in the array");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        let error_ptr = sai_syntax::SyntaxNodePtr::new(file, &array_error).expect("error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&error_ptr).len(), 1);
        assert_eq!(elements[0].span.range, array_error.text_range());

        let file = crate::FileId::new(90);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { [make(..base1, ..base2, value.convert<U32>)] }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("a malformed call remains local to its array element");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("the array is retained around the malformed call");
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].kind, ExprKind::Err);
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(91);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { [[value.convert<U32>, ,], call(, 2)] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("nested generic fields and missing elements recover");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("the outer array is retained");
        };
        assert_eq!(elements.len(), 2);
        assert!(matches!(elements[0].kind, ExprKind::Array(_)));
        assert!(matches!(elements[1].kind, ExprKind::Call { .. }));
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(92);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { [(@inline value).convert<U32>, call(, 2)] }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let array = function
            .descendants()
            .find_map(ast::ArrayExpr::cast)
            .expect("outer array expression");
        let expression =
            ast::Expr::cast(array.syntax().clone()).expect("array is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the parenthesized field value and recovered call lower");
        let ExprKind::Array(elements) = lowered.kind else {
            panic!("the array is retained");
        };
        assert_eq!(elements.len(), 2);
        assert!(matches!(elements[0].kind, ExprKind::Field { .. }));
        assert!(matches!(elements[1].kind, ExprKind::Call { .. }));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_return_break_continue_and_generic_field_values() {
        let file = crate::FileId::new(93);
        let source = "fn f() { (return, return 1, break, break 'outer 2, continue 'outer) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("control-transfer expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the outer expression is a tuple");
        };
        assert_eq!(elements.len(), 5);
        assert_eq!(elements[0].kind, ExprKind::Return(None));
        let ExprKind::Return(Some(return_value)) = elements[1].kind else {
            panic!("return keeps its optional value");
        };
        assert_eq!(
            return_value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            elements[2].kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        );
        let ExprKind::Break {
            label: Some(break_label),
            value: Some(break_value),
        } = elements[3].kind
        else {
            panic!("labeled break retains its label and value");
        };
        assert_eq!(
            symbols
                .resolve(break_label.name)
                .expect("label name resolves")
                .as_ref(),
            "outer"
        );
        assert_eq!(
            break_value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        let ExprKind::Continue {
            label: Some(continue_label),
        } = elements[4].kind
        else {
            panic!("labeled continue retains its label");
        };
        assert_eq!(continue_label.name, break_label.name);

        let label_tokens = function
            .descendants()
            .filter_map(ast::BreakExpr::cast)
            .filter_map(|break_expr| break_expr.label())
            .chain(
                function
                    .descendants()
                    .filter_map(ast::ContinueExpr::cast)
                    .filter_map(|continue_expr| continue_expr.label()),
            )
            .collect::<Vec<_>>();
        assert_eq!(label_tokens.len(), 2);
        for (label, hir_label) in label_tokens.iter().zip([break_label, continue_label]) {
            assert_eq!(hir_label.span.file, file);
            assert_eq!(hir_label.span.range, label.syntax().text_range());
            assert_eq!(hir_label.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                return_value.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                break_value.hir_id.local_id.raw(),
                elements[4].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 9);

        let file = crate::FileId::new(94);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { return value.convert<U32> }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let return_expr = function
            .descendants()
            .find_map(ast::ReturnExpr::cast)
            .expect("return expression");
        let expression =
            ast::Expr::cast(return_expr.syntax().clone()).expect("return is an expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("return lowers");
        let ExprKind::Return(Some(value)) = lowered.kind else {
            panic!("the return value remains in HIR");
        };
        assert!(matches!(
            value.kind,
            ExprKind::Field { args: Some(args), .. } if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(95);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { (return 'oops:, break 'oops:, call(, 2)) }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("recovery tuple");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the outer tuple preserves local recovery");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the recovery parent remains a tuple");
        };
        assert_eq!(elements.len(), 3);
        assert!(matches!(
            elements[0].kind,
            ExprKind::Return(Some(value)) if value.kind == ExprKind::Err
        ));
        assert!(matches!(
            elements[1].kind,
            ExprKind::Break {
                label: _,
                value: Some(value),
            } if value.kind == ExprKind::Err
        ));
        assert!(matches!(
            elements[2].kind,
            ExprKind::Call { args, .. }
                if args.first().is_some_and(|arg| arg.expr.kind == ExprKind::Err)
        ));
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn lowers_expression_blocks_with_source_ordered_statements_and_tail() {
        let file = crate::FileId::new(96);
        let source = "fn f() { ({}, { 1\n 2 }) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("blocks lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the tuple holds both block expressions");
        };
        assert_eq!(elements.len(), 2);

        let ExprKind::Block(empty_block) = elements[0].kind else {
            panic!("the first element is an empty block");
        };
        assert!(empty_block.stmts.is_empty());
        assert!(empty_block.tail.is_none());
        assert_eq!(empty_block.safety, Safety::Safe);

        let ExprKind::Block(block) = elements[1].kind else {
            panic!("the second element is a block");
        };
        assert_eq!(block.stmts.len(), 1);
        let StmtKind::Expr(statement_expr) = block.stmts[0].kind else {
            panic!("the preceding expression statement remains in the block");
        };
        assert_eq!(
            statement_expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let tail = block.tail.expect("the final expression is the block tail");
        assert_eq!(
            tail.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                empty_block.hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                block.hir_id.local_id.raw(),
                block.stmts[0].hir_id.local_id.raw(),
                statement_expr.hir_id.local_id.raw(),
                tail.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(block.span.file, file);
        assert_eq!(block.span.ctxt, SpanCtxt::Root);
        assert_eq!(source_map.finish().node_to_syntax().len(), 9);
    }

    #[test]
    fn lowers_unsafe_blocks_with_unsafe_block_safety() {
        let file = crate::FileId::new(190);
        let source = "fn f() { unsafe { 1 } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let unsafe_block = function
            .descendants()
            .find_map(ast::UnsafeBlockExpr::cast)
            .expect("unsafe block expression");
        let expression =
            ast::Expr::cast(unsafe_block.syntax().clone()).expect("unsafe block is an expression");
        let body = unsafe_block.body().expect("unsafe block body");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("unsafe block lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("unsafe block expression maps to a block");
        };
        assert_eq!(block.safety, Safety::Unsafe);
        assert_eq!(block.span.range, body.syntax().text_range());
        assert_eq!(block.span.ctxt, SpanCtxt::Root);
        assert_eq!(lowered.span.range, unsafe_block.syntax().text_range());
        assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        assert_eq!(block.hir_id.local_id.raw(), 2);
        let tail = block.tail.expect("literal is the block tail");
        assert!(matches!(
            tail.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        ));
        assert_eq!(tail.hir_id.local_id.raw(), 3);
        assert_eq!(source_map.finish().node_to_syntax().len(), 4);
    }

    #[test]
    fn unsafe_blocks_lower_generic_field_values() {
        let file = crate::FileId::new(191);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { unsafe { source.convert<U32> } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let unsafe_block = function
            .descendants()
            .find_map(ast::UnsafeBlockExpr::cast)
            .expect("unsafe block expression");
        let expression =
            ast::Expr::cast(unsafe_block.syntax().clone()).expect("unsafe block is an expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("unsafe block lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("unsafe block lowers to a HIR block expression");
        };
        assert_eq!(block.safety, Safety::Unsafe);
        assert!(matches!(
            block.tail.map(|tail| tail.kind),
            Some(ExprKind::Field { args: Some(args), .. }) if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_region_blocks_with_a_source_mapped_binding() {
        let file = crate::FileId::new(192);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { region arena { 1 } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let region_expr = function
            .descendants()
            .find_map(ast::RegionExpr::cast)
            .expect("region expression");
        let expression =
            ast::Expr::cast(region_expr.syntax().clone()).expect("region is an expression");
        let region_name = region_expr.name().expect("region binding name");
        let body = region_expr.body().expect("region body");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("region lowers");
        let ExprKind::Region {
            name,
            binding,
            block,
        } = lowered.kind
        else {
            panic!("region syntax maps to a region expression");
        };
        assert_eq!(
            symbols
                .resolve(name.sym)
                .expect("interned region name")
                .as_ref(),
            "arena"
        );
        assert_eq!(name.span.range, region_name.syntax().text_range());
        assert_eq!(name.span.ctxt, SpanCtxt::Root);
        assert_eq!(binding.raw(), 0);
        assert_eq!(block.safety, Safety::Safe);
        assert_eq!(block.span.range, body.syntax().text_range());
        assert_eq!(lowered.span.range, region_expr.syntax().text_range());
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        assert_eq!(block.hir_id.local_id.raw(), 2);
        let tail = block.tail.expect("literal is the region tail");
        assert!(matches!(
            tail.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        ));
        assert_eq!(tail.hir_id.local_id.raw(), 3);
        let region_name_ptr =
            sai_syntax::SyntaxNodePtr::new(file, region_name.syntax()).expect("region name ptr");
        let source_map = source_map.finish();
        assert_eq!(source_map.node_to_syntax().len(), 4);
        assert_eq!(source_map.binding_to_syntax().len(), 1);
        assert_eq!(
            source_map.syntax_for_binding(binding),
            Some(&region_name_ptr)
        );
    }

    #[test]
    fn region_blocks_lower_generic_field_values() {
        let file = crate::FileId::new(193);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { region arena { source.convert<U32> } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let region_expr = function
            .descendants()
            .find_map(ast::RegionExpr::cast)
            .expect("region expression");
        let expression =
            ast::Expr::cast(region_expr.syntax().clone()).expect("region is an expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("region lowers");
        let ExprKind::Region { block, .. } = lowered.kind else {
            panic!("the region expression is retained");
        };
        assert!(matches!(
            block.tail.map(|tail| tail.kind),
            Some(ExprKind::Field { args: Some(args), .. }) if args.args.len() == 1
        ));
        let map = source_map.finish();
        assert!(map.node_to_syntax().len() > 1);
        assert_eq!(map.binding_to_syntax().len(), 1);
    }

    #[test]
    fn lowers_for_loops_with_iterator_protocol_and_synthetic_spans() {
        let file = crate::FileId::new(194);
        let source = "fn f() { 'outer: for item in items {} }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let for_expr = function
            .descendants()
            .find_map(ast::ForExpr::cast)
            .expect("for expression");
        let expression =
            ast::Expr::cast(for_expr.syntax().clone()).expect("for expression variant");
        let pattern = for_expr.pattern().expect("loop pattern");
        let body = for_expr.body().expect("loop body");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("for lowers");
        let ExprKind::Match {
            scrutinee,
            arms: outer_arms,
            source: HirMatchSource::ForLoop,
        } = lowered.kind
        else {
            panic!("for lowers to the outer iterator match");
        };
        assert_eq!(
            lowered.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        let ExprKind::Call {
            callee,
            args: iter_args,
            base: None,
        } = scrutinee.kind
        else {
            panic!("the outer match scrutinee calls into_iter");
        };
        let ExprKind::Path(path) = callee.kind else {
            panic!("into_iter is a lang-item path");
        };
        assert_eq!(path.root, PathRoot::Lang(LangItem::IntoIteratorIntoIter));
        assert!(path.segments.is_empty());
        let iterable_syntax = for_expr.iterable().expect("loop iterable");
        let iterable_range = iterable_syntax.syntax().text_range();
        assert_eq!(scrutinee.span.range, iterable_range);
        assert_eq!(callee.span.range, iterable_range);
        assert_eq!(path.span.range, iterable_range);
        assert_eq!(scrutinee.hir_id.local_id.raw(), 2);
        assert_eq!(callee.hir_id.local_id.raw(), 3);
        assert_eq!(path.hir_id.local_id.raw(), 4);
        assert_eq!(iter_args.len(), 1);
        let iterable = iter_args[0].expr;
        assert_eq!(iterable.span.ctxt, SpanCtxt::Root);
        assert_eq!(iterable.hir_id.local_id.raw(), 5);
        assert_eq!(outer_arms.len(), 1);
        let outer_arm = outer_arms[0];
        assert!(!outer_arm.synthesized);
        assert_eq!(outer_arm.hir_id.local_id.raw(), 8);
        assert_eq!(
            outer_arm.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        let PatKind::Binding {
            mode: HirBindingMode::Var,
            name: iter_name,
            binding: iter_binding,
            sub: None,
        } = outer_arm.pat.kind
        else {
            panic!("the outer arm binds the hidden mutable iterator");
        };
        assert_eq!(symbols.resolve(iter_name.sym).unwrap().as_ref(), "%iter");
        assert_eq!(
            iter_name.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(iter_name.span.range, for_expr.syntax().text_range());
        assert_eq!(iter_binding.raw(), 0);

        let ExprKind::Loop {
            body: loop_body,
            label: Some(label),
            source: HirLoopSource::ForLoop,
        } = outer_arm.body.kind
        else {
            panic!("the outer arm evaluates the labeled iterator loop");
        };
        assert_eq!(
            outer_arm.body.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(symbols.resolve(label.name).unwrap().as_ref(), "outer");
        assert_eq!(label.span.ctxt, SpanCtxt::Root);
        assert_eq!(
            loop_body.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        let next_match = loop_body.tail.expect("loop tail is the next match");
        assert_eq!(
            next_match.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        let ExprKind::Match {
            scrutinee: next_call,
            arms: next_arms,
            source: HirMatchSource::ForLoop,
        } = next_match.kind
        else {
            panic!("the loop tail matches Iterator.next");
        };
        let ExprKind::Call {
            callee: next_callee,
            args: next_args,
            base: None,
        } = next_call.kind
        else {
            panic!("the next match scrutinee calls Iterator.next");
        };
        let ExprKind::Path(next_path) = next_callee.kind else {
            panic!("Iterator.next is a lang-item path");
        };
        assert_eq!(next_path.root, PathRoot::Lang(LangItem::IteratorNext));
        assert_eq!(
            next_path.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(next_args.len(), 1);
        let ExprKind::Borrow(Mutability::Mut, iter_ref) = next_args[0].expr.kind else {
            panic!("Iterator.next receives a mutable borrow");
        };
        let ExprKind::Path(iter_path) = iter_ref.kind else {
            panic!("the mutable borrow refers to the hidden iterator");
        };
        assert_eq!(iter_path.root, PathRoot::Plain);
        assert_eq!(
            iter_path.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(iter_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(iter_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "%iter"
        );
        assert_eq!(next_arms.len(), 2);
        let some_arm = next_arms[0];
        assert!(!some_arm.synthesized);
        let PatKind::Ctor {
            path: some_path,
            args: Some(some_args),
            has_rest: false,
        } = some_arm.pat.kind
        else {
            panic!("the first next arm matches Option.Some");
        };
        assert_eq!(some_path.root, PathRoot::Lang(LangItem::OptionSome));
        assert_eq!(
            some_path.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );
        assert_eq!(some_args.len(), 1);
        let PatKind::Binding {
            mode: HirBindingMode::Value,
            binding: item_binding,
            ..
        } = some_args[0].pat.kind
        else {
            panic!("the source loop pattern is nested in Option.Some");
        };
        assert_eq!(item_binding.raw(), 1);
        assert_eq!(
            some_args[0].pat.span,
            Span {
                file,
                range: pattern.syntax().text_range(),
                ctxt: SpanCtxt::Root,
            }
        );
        let ExprKind::Block(user_block) = some_arm.body.kind else {
            panic!("the user block remains the Some arm body");
        };
        assert_eq!(some_arm.body.span.range, body.syntax().text_range());
        assert_eq!(some_arm.body.span.ctxt, SpanCtxt::Root);
        assert_eq!(user_block.safety, Safety::Safe);

        let none_arm = next_arms[1];
        assert!(none_arm.synthesized);
        let PatKind::Ctor {
            path: none_path,
            args: None,
            has_rest: false,
        } = none_arm.pat.kind
        else {
            panic!("the fallback matches Option.None without arguments");
        };
        assert_eq!(none_path.root, PathRoot::Lang(LangItem::OptionNone));
        assert!(matches!(
            none_arm.body.kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        ));
        assert_eq!(
            none_arm.pat.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::ForLoop)
        );

        let for_ptr =
            sai_syntax::SyntaxNodePtr::new(file, for_expr.syntax()).expect("for syntax pointer");
        let source_map = source_map.finish();
        assert_eq!(source_map.syntax_for_binding(iter_binding), Some(&for_ptr));
        assert_eq!(
            source_map.syntax_for_binding(item_binding).unwrap().kind(),
            SyntaxKind::BINDING_PAT
        );
        assert_eq!(source_map.diagnostics().len(), 0);
    }

    #[test]
    fn lowers_for_await_loops_with_stream_protocol_and_synthetic_spans() {
        let file = crate::FileId::new(211);
        let source = "fn f() { 'outer: for await item in stream { item } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let for_expr = function
            .descendants()
            .find_map(ast::ForExpr::cast)
            .expect("for-await expression");
        let expression = ast::Expr::cast(for_expr.syntax().clone()).unwrap();
        let pattern = for_expr.pattern().expect("loop pattern");
        let iterable = for_expr.iterable().expect("stream expression");
        let body = for_expr.body().expect("loop body");
        let await_token_range = for_expr
            .syntax()
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::AWAIT_KW => {
                    Some(token.text_range())
                }
                _ => None,
            })
            .expect("for-await keyword");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("for await lowers");
        let ctxt = SpanCtxt::Desugared(DesugaringKind::ForAwait);
        let ExprKind::Match {
            scrutinee: outer_scrutinee,
            arms: outer_arms,
            source: HirMatchSource::ForAwait,
        } = lowered.kind
        else {
            panic!("for await lowers to the outer stream match");
        };
        assert_eq!(lowered.span.ctxt, ctxt);
        let ExprKind::Path(stream_path) = outer_scrutinee.kind else {
            panic!("the source stream is matched directly");
        };
        assert_eq!(outer_scrutinee.span.ctxt, SpanCtxt::Root);
        assert_eq!(stream_path.root, PathRoot::Plain);
        assert_eq!(stream_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(stream_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "stream"
        );
        assert_eq!(outer_arms.len(), 1);
        let outer_arm = outer_arms[0];
        assert!(!outer_arm.synthesized);
        let PatKind::Binding {
            mode: HirBindingMode::Var,
            name: stream_name,
            binding: stream_binding,
            sub: None,
        } = outer_arm.pat.kind
        else {
            panic!("the outer arm binds the hidden mutable stream");
        };
        assert_eq!(
            symbols.resolve(stream_name.sym).unwrap().as_ref(),
            "%stream"
        );
        assert_eq!(stream_name.span.ctxt, ctxt);
        assert_eq!(outer_arm.span.ctxt, ctxt);
        assert_eq!(outer_arm.pat.span.ctxt, ctxt);
        let ExprKind::Loop {
            body: loop_body,
            label: Some(label),
            source: HirLoopSource::ForAwait,
        } = outer_arm.body.kind
        else {
            panic!("the outer arm evaluates the labeled stream loop");
        };
        assert_eq!(symbols.resolve(label.name).unwrap().as_ref(), "outer");
        assert_eq!(label.span.ctxt, SpanCtxt::Root);
        assert_eq!(outer_arm.body.span.ctxt, ctxt);
        assert_eq!(loop_body.span.ctxt, ctxt);
        let next_match = loop_body.tail.expect("loop tail is the stream-next match");
        let ExprKind::Match {
            scrutinee: awaited_next,
            arms: next_arms,
            source: HirMatchSource::ForAwait,
        } = next_match.kind
        else {
            panic!("the loop tail matches the awaited stream result");
        };
        assert_eq!(next_match.span.ctxt, ctxt);
        let ExprKind::Await(next_call) = awaited_next.kind else {
            panic!("StreamNext is awaited");
        };
        assert_eq!(awaited_next.span.ctxt, ctxt);
        assert_eq!(awaited_next.span.range, await_token_range);
        let ExprKind::Call {
            callee: next_callee,
            args: next_args,
            base: None,
        } = next_call.kind
        else {
            panic!("the await operand calls StreamNext");
        };
        assert_eq!(next_call.span.ctxt, ctxt);
        let ExprKind::Path(next_path) = next_callee.kind else {
            panic!("StreamNext is a lang-item path");
        };
        assert_eq!(next_path.root, PathRoot::Lang(LangItem::StreamNext));
        assert_eq!(next_callee.span.ctxt, ctxt);
        assert_eq!(next_path.span.ctxt, ctxt);
        assert_eq!(next_args.len(), 1);
        let ExprKind::Borrow(Mutability::Mut, stream_ref) = next_args[0].expr.kind else {
            panic!("StreamNext receives a mutable borrow");
        };
        assert_eq!(next_args[0].expr.span.ctxt, ctxt);
        let ExprKind::Path(stream_ref_path) = stream_ref.kind else {
            panic!("the mutable borrow refers to the hidden stream");
        };
        assert_eq!(stream_ref_path.root, PathRoot::Plain);
        assert_eq!(stream_ref.span.ctxt, ctxt);
        assert_eq!(stream_ref_path.span.ctxt, ctxt);
        assert_eq!(stream_ref_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(stream_ref_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "%stream"
        );

        assert_eq!(next_arms.len(), 2);
        let some_arm = next_arms[0];
        assert!(!some_arm.synthesized);
        let PatKind::Ctor {
            path: some_path,
            args: Some(some_args),
            has_rest: false,
        } = some_arm.pat.kind
        else {
            panic!("the first stream-next arm matches Option.Some");
        };
        assert_eq!(some_path.root, PathRoot::Lang(LangItem::OptionSome));
        assert_eq!(some_arm.span.ctxt, ctxt);
        assert_eq!(some_arm.pat.span.ctxt, ctxt);
        assert_eq!(some_path.span.ctxt, ctxt);
        assert_eq!(some_args.len(), 1);
        assert_eq!(
            some_args[0].pat.span,
            Span {
                file,
                range: pattern.syntax().text_range(),
                ctxt: SpanCtxt::Root,
            }
        );
        let PatKind::Binding {
            binding: item_binding,
            ..
        } = some_args[0].pat.kind
        else {
            panic!("the source loop pattern is nested under Option.Some");
        };
        let ExprKind::Block(user_block) = some_arm.body.kind else {
            panic!("the source block remains the Some arm body");
        };
        assert_eq!(some_arm.body.span.range, body.syntax().text_range());
        assert_eq!(some_arm.body.span.ctxt, SpanCtxt::Root);
        assert_eq!(user_block.safety, Safety::Safe);

        let none_arm = next_arms[1];
        assert!(none_arm.synthesized);
        let PatKind::Ctor {
            path: none_path,
            args: None,
            has_rest: false,
        } = none_arm.pat.kind
        else {
            panic!("the fallback matches Option.None without arguments");
        };
        assert_eq!(none_path.root, PathRoot::Lang(LangItem::OptionNone));
        assert_eq!(none_arm.span.ctxt, ctxt);
        assert_eq!(none_arm.pat.span.ctxt, ctxt);
        assert_eq!(none_path.span.ctxt, ctxt);
        assert!(matches!(
            none_arm.body.kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        ));

        let for_ptr = SyntaxNodePtr::new(file, for_expr.syntax()).expect("for syntax pointer");
        let iterable_ptr =
            SyntaxNodePtr::new(file, iterable.syntax()).expect("stream expression pointer");
        let source_map = source_map.finish();
        assert_eq!(
            source_map.syntax_for_binding(stream_binding),
            Some(&for_ptr)
        );
        assert_eq!(
            source_map.syntax_for_node(awaited_next.hir_id),
            Some(&for_ptr)
        );
        assert_eq!(
            source_map.syntax_for_binding(item_binding).unwrap().kind(),
            SyntaxKind::BINDING_PAT
        );
        assert_eq!(
            source_map.syntax_for_node(outer_scrutinee.hir_id),
            Some(&iterable_ptr)
        );
        assert_eq!(source_map.diagnostics().len(), 0);
    }

    #[test]
    fn for_await_lowers_generic_field_values_in_the_body() {
        let file = crate::FileId::new(212);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { for await item in stream { item.convert<U32> } }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let for_expr = function
            .descendants()
            .find_map(ast::ForExpr::cast)
            .expect("for-await expression");
        let expression = ast::Expr::cast(for_expr.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("for await lowers");
        assert!(matches!(
            lowered.kind,
            ExprKind::Match {
                source: HirMatchSource::ForAwait,
                ..
            }
        ));
        let source_map = source_map.finish();
        assert!(source_map.node_to_syntax().len() > 1);
        assert_eq!(source_map.binding_to_syntax().len(), 2);
    }

    #[test]
    fn for_loops_lower_generic_field_values_in_the_body() {
        let file = crate::FileId::new(195);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { for item in items { source.convert<U32> } }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let for_expr = function
            .descendants()
            .find_map(ast::ForExpr::cast)
            .expect("for expression");
        let expression =
            ast::Expr::cast(for_expr.syntax().clone()).expect("for expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("for loop lowers");
        assert!(matches!(
            lowered.kind,
            ExprKind::Match {
                source: HirMatchSource::ForLoop,
                ..
            }
        ));
        let source_map = source_map.finish();
        assert!(source_map.node_to_syntax().len() > 1);
        assert_eq!(source_map.binding_to_syntax().len(), 2);
    }

    #[test]
    fn block_recovery_keeps_call_errors_and_lowers_generic_fields_in_loops() {
        let file = crate::FileId::new(97);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { { call(, 2) } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument recovery preserves its containing block");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the block is preserved");
        };
        let call = block.tail.expect("the final call is the block tail");
        let ExprKind::Call { args, .. } = call.kind else {
            panic!("the block tail is the call");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let file = crate::FileId::new(98);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { { for item in x { value.convert<U32> }\n x } }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("nested block lowers");
        assert!(matches!(lowered.kind, ExprKind::Block(_)));
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(99);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { { call(, 2)\n@allow(foo)\nfn inner() {}\n} }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let block_syntax = block_expr.block().expect("block syntax");
        let recovered_error = block_syntax
            .syntax()
            .children()
            .find(|child| child.kind() == SyntaxKind::ERROR)
            .expect("attributed item recovery node");
        assert!(contains_attribute(&recovered_error));

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the block retains call and statement recovery");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the block is preserved");
        };
        assert_eq!(block.stmts.len(), 2);
        assert!(block.tail.is_none());
        let StmtKind::Expr(call) = block.stmts[0].kind else {
            panic!("the earlier expression remains a statement");
        };
        assert!(matches!(call.kind, ExprKind::Call { .. }));
        let StmtKind::Expr(error_expr) = block.stmts[1].kind else {
            panic!("the attributed error remains an error statement");
        };
        assert_eq!(error_expr.kind, ExprKind::Err);
        assert_eq!(error_expr.span.range, recovered_error.text_range());
        let error_ptr =
            sai_syntax::SyntaxNodePtr::new(file, &recovered_error).expect("error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&error_ptr).len(), 2);
    }

    #[test]
    fn lowers_let_and_var_statements_with_bindings_and_source_context() {
        let file = crate::FileId::new(137);
        let source = "fn f() { { let (left, right): (I32, I32) = pair\n var cursor: I32 = left\n cursor } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("statements lower");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the expression is a block");
        };
        assert_eq!(block.stmts.len(), 2);
        assert!(block.tail.is_some());
        let StmtKind::Let {
            pat,
            ty: Some(ty),
            init,
            els: None,
        } = block.stmts[0].kind
        else {
            panic!("the first statement is an annotated let");
        };
        let PatKind::Tuple(elements) = pat.kind else {
            panic!("the let retains its tuple pattern");
        };
        assert_eq!(elements.len(), 2);
        assert!(elements.iter().all(|element| matches!(
            element.kind,
            PatKind::Binding {
                mode: HirBindingMode::Value,
                sub: None,
                ..
            }
        )));
        assert!(matches!(ty.kind, HirTyKind::Tuple(types) if types.len() == 2));
        assert!(matches!(init.kind, ExprKind::Path(_)));
        assert_eq!(block.stmts[0].span.ctxt, SpanCtxt::Root);
        assert_eq!(pat.span.ctxt, SpanCtxt::Root);

        let StmtKind::Let {
            pat: var_pat,
            ty: Some(var_ty),
            init: var_init,
            els: None,
        } = block.stmts[1].kind
        else {
            panic!("var lowers to a let-shaped HIR statement");
        };
        let PatKind::Binding {
            mode: HirBindingMode::Var,
            name,
            binding,
            sub: None,
        } = var_pat.kind
        else {
            panic!("var creates a mutable binding pattern");
        };
        assert_eq!(symbols.resolve(name.sym).unwrap().as_ref(), "cursor");
        assert!(matches!(var_ty.kind, HirTyKind::Path(_)));
        assert!(matches!(var_init.kind, ExprKind::Path(_)));
        assert_eq!(
            var_pat.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::VarStmt)
        );
        assert_eq!(name.span.ctxt, SpanCtxt::Desugared(DesugaringKind::VarStmt));
        assert_eq!(block.stmts[1].span.ctxt, SpanCtxt::Root);
        let var_statement = function
            .descendants()
            .find_map(ast::VarStmt::cast)
            .expect("var statement syntax");
        let var_name = var_statement.name().expect("var binding name");
        let var_statement_ptr =
            sai_syntax::SyntaxNodePtr::new(file, var_statement.syntax()).expect("statement ptr");
        let var_name_ptr =
            sai_syntax::SyntaxNodePtr::new(file, var_name.syntax()).expect("name ptr");
        let map = source_map.finish();
        assert_eq!(map.binding_to_syntax().len(), 3);
        assert_eq!(
            map.syntax_for_node(var_pat.hir_id),
            Some(&var_statement_ptr)
        );
        assert_eq!(map.syntax_for_binding(binding), Some(&var_name_ptr));
    }

    #[test]
    fn lowers_let_else_and_preserves_missing_annotation_and_initializer_errors() {
        let file = crate::FileId::new(138);
        let source = "fn f() { { let value = input else { fallback() }\n value } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("let-else lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the expression is a block");
        };
        let StmtKind::Let {
            els: Some(else_block),
            init,
            ..
        } = block.stmts[0].kind
        else {
            panic!("the let-else clause remains present");
        };
        assert!(matches!(init.kind, ExprKind::Path(_)));
        assert!(matches!(
            else_block.tail.map(|tail| tail.kind),
            Some(ExprKind::Call { .. })
        ));
        assert!(block.tail.is_some());

        let file = crate::FileId::new(139);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { { let x: = 1\n let y\n y } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("recovery lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the recovered expression is a block");
        };
        assert_eq!(block.stmts.len(), 2);
        let StmtKind::Let {
            ty: Some(ty), init, ..
        } = block.stmts[0].kind
        else {
            panic!("the first statement remains a let with an explicit annotation");
        };
        assert_eq!(ty.kind, HirTyKind::Err);
        assert!(matches!(
            init.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        ));
        let StmtKind::Let { init, ty: None, .. } = block.stmts[1].kind else {
            panic!("the second statement is an unannotated let");
        };
        assert_eq!(init.kind, ExprKind::Err);
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn let_initializer_keeps_recoverable_call_arguments() {
        let file = crate::FileId::new(141);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { { let value = call(, 2)\n value } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("let recovers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the containing block survives initializer recovery");
        };
        let StmtKind::Let { init, .. } = block.stmts[0].kind else {
            panic!("the first statement is a let");
        };
        let ExprKind::Call { args, .. } = init.kind else {
            panic!("the initializer call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert!(matches!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        ));
    }

    #[test]
    fn let_statement_lowers_generic_field_initializers() {
        let file = crate::FileId::new(140);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { { let value = source.convert<U32>\n value } }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("block lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the enclosing block is retained");
        };
        let StmtKind::Let { init, .. } = block.stmts[0].kind else {
            panic!("the let initializer is retained");
        };
        assert!(matches!(
            init.kind,
            ExprKind::Field { args: Some(args), .. } if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_simple_compound_assignments_and_assertions() {
        let file = crate::FileId::new(142);
        let operators = [
            ("=", None),
            ("+=", Some(BinOp::Add)),
            ("-=", Some(BinOp::Sub)),
            ("*=", Some(BinOp::Mul)),
            ("/=", Some(BinOp::Div)),
            ("%=", Some(BinOp::Rem)),
            ("+%=", Some(BinOp::WrapAdd)),
            ("-%=", Some(BinOp::WrapSub)),
            ("*%=", Some(BinOp::WrapMul)),
            ("&=", Some(BinOp::BitAnd)),
            ("|=", Some(BinOp::BitOr)),
            ("^=", Some(BinOp::BitXor)),
            ("<<=", Some(BinOp::Shl)),
            (">>=", Some(BinOp::Shr)),
        ];
        let assignments = operators
            .iter()
            .map(|(operator, _)| format!("target {operator} value"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!(
            "fn f() {{ {{\n{assignments}\nassert   left /* condition */ == right   , \"failed\"\nassert ready\n}} }}\n"
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, &source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("assignment and assert statements lower");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the expression is a block");
        };
        assert_eq!(block.stmts.len(), operators.len() + 2);
        assert!(block.tail.is_none());
        for (statement, (_, expected_op)) in block.stmts.iter().zip(operators) {
            assert_eq!(statement.span.ctxt, SpanCtxt::Root);
            match (statement.kind, expected_op) {
                (StmtKind::Assign { lhs, rhs }, None) => {
                    assert!(statement.hir_id.local_id < lhs.hir_id.local_id);
                    assert!(lhs.hir_id.local_id < rhs.hir_id.local_id);
                    assert!(matches!(lhs.kind, ExprKind::Path(_)));
                    assert!(matches!(rhs.kind, ExprKind::Path(_)));
                }
                (StmtKind::AssignOp { op, lhs, rhs }, Some(expected_op)) => {
                    assert_eq!(op, expected_op);
                    assert!(statement.hir_id.local_id < lhs.hir_id.local_id);
                    assert!(lhs.hir_id.local_id < rhs.hir_id.local_id);
                    assert!(matches!(lhs.kind, ExprKind::Path(_)));
                    assert!(matches!(rhs.kind, ExprKind::Path(_)));
                }
                (kind, expected) => {
                    panic!("unexpected assignment lowering {kind:?} for {expected:?}")
                }
            }
        }
        let StmtKind::Assert {
            cond,
            msg: Some(msg),
            cond_text,
        } = block.stmts[operators.len()].kind
        else {
            panic!("the first assert has a message");
        };
        assert!(block.stmts[operators.len()].hir_id.local_id < cond.hir_id.local_id);
        assert!(cond.hir_id.local_id < msg.hir_id.local_id);
        assert!(matches!(cond.kind, ExprKind::Binary { .. }));
        assert_eq!(
            symbols.resolve(cond_text).unwrap().as_ref(),
            "left /* condition */ == right"
        );
        assert!(matches!(
            msg.kind,
            ExprKind::Lit(Lit::Str(text))
                if symbols.resolve(text).unwrap().as_ref() == "failed"
        ));
        let StmtKind::Assert {
            cond,
            msg: None,
            cond_text,
        } = block.stmts[operators.len() + 1].kind
        else {
            panic!("the second assert uses its default message");
        };
        assert!(block.stmts[operators.len() + 1].hir_id.local_id < cond.hir_id.local_id);
        assert!(matches!(cond.kind, ExprKind::Path(_)));
        assert_eq!(symbols.resolve(cond_text).unwrap().as_ref(), "ready");
    }

    #[test]
    fn malformed_assert_condition_is_retained_as_an_error_expression() {
        let file = crate::FileId::new(143);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { { assert , \"message\"\n 1 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("assert recovery preserves the block");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the recovered expression is a block");
        };
        assert_eq!(block.stmts.len(), 1);
        assert!(matches!(
            block.tail.map(|tail| tail.kind),
            Some(ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            }))
        ));
        let StmtKind::Assert {
            cond,
            msg: Some(msg),
            cond_text,
        } = block.stmts[0].kind
        else {
            panic!("the recovered assert keeps its message");
        };
        assert_eq!(cond.kind, ExprKind::Err);
        assert!(matches!(
            msg.kind,
            ExprKind::Lit(Lit::Str(text))
                if symbols.resolve(text).unwrap().as_ref() == "message"
        ));
        assert_eq!(symbols.resolve(cond_text).unwrap().as_ref(), "");
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn missing_assert_message_is_retained_as_an_error_expression() {
        let file = crate::FileId::new(145);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { { assert ready, } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("assert recovery preserves the block");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the recovered expression is a block");
        };
        let StmtKind::Assert {
            cond,
            msg: Some(msg),
            cond_text,
        } = block.stmts[0].kind
        else {
            panic!("the missing message occupies its own error slot");
        };
        assert!(matches!(cond.kind, ExprKind::Path(_)));
        assert_eq!(msg.kind, ExprKind::Err);
        assert_eq!(symbols.resolve(cond_text).unwrap().as_ref(), "ready");
        assert!(block.tail.is_none());
    }

    #[test]
    fn direct_block_errors_preserve_otherwise_lowerable_blocks() {
        let file = crate::FileId::new(146);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { { @allow(foo)\n fn inner() {} } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the block directly retains its recovery node");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the block expression is retained");
        };
        assert_eq!(block.stmts.len(), 1);
        assert!(block.tail.is_none());
        let StmtKind::Expr(error) = block.stmts[0].kind else {
            panic!("a direct block error becomes an error statement");
        };
        assert_eq!(error.kind, ExprKind::Err);
    }

    #[test]
    fn assignment_lowers_generic_field_operands() {
        let file = crate::FileId::new(144);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { { target = source.convert<U32>\n value } }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let block_expr = function
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .expect("block expression");
        let expression =
            ast::Expr::cast(block_expr.syntax().clone()).expect("block is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("block lowers");
        let ExprKind::Block(block) = lowered.kind else {
            panic!("the enclosing block is retained");
        };
        let StmtKind::Assign { rhs, .. } = block.stmts[0].kind else {
            panic!("the assignment is retained");
        };
        assert!(matches!(
            rhs.kind,
            ExprKind::Field { args: Some(args), .. } if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_plain_if_else_and_else_if_expressions() {
        let file = crate::FileId::new(100);
        let source = "fn f() { (if true { 1 } else { 2 }, if false { 3 } else if true { 4 } else { 5 }, if false { 6 }) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("plain if expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the tuple holds three if expressions");
        };
        assert_eq!(elements.len(), 3);

        let ExprKind::If {
            cond: first_cond,
            then: first_then,
            els: Some(first_else),
        } = elements[0].kind
        else {
            panic!("the first if has an else block");
        };
        assert_eq!(first_cond.kind, ExprKind::Lit(Lit::Bool(true)));
        assert!(first_then.stmts.is_empty());
        assert_eq!(
            first_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let ExprKind::Block(first_else_block) = first_else.kind else {
            panic!("else blocks are represented as block expressions");
        };
        assert_eq!(
            first_else_block.tail.expect("else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let ExprKind::If {
            cond: second_cond,
            then: second_then,
            els: Some(second_else),
        } = elements[1].kind
        else {
            panic!("the second if has an else-if");
        };
        assert_eq!(second_cond.kind, ExprKind::Lit(Lit::Bool(false)));
        assert_eq!(
            second_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        let ExprKind::If {
            cond: else_if_cond,
            then: else_if_then,
            els: Some(else_if_else),
        } = second_else.kind
        else {
            panic!("else-if lowers recursively as an if expression");
        };
        assert_eq!(else_if_cond.kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(
            else_if_then.tail.expect("else-if then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(4),
                negated: false,
            })
        );
        let ExprKind::Block(else_if_else_block) = else_if_else.kind else {
            panic!("the nested else block is preserved");
        };
        assert_eq!(
            else_if_else_block.tail.expect("else-if else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(5),
                negated: false,
            })
        );

        let ExprKind::If {
            cond: third_cond,
            then: third_then,
            els: None,
        } = elements[2].kind
        else {
            panic!("the third if has no else branch");
        };
        assert_eq!(third_cond.kind, ExprKind::Lit(Lit::Bool(false)));
        assert_eq!(
            third_then.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(6),
                negated: false,
            })
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                first_cond.hir_id.local_id.raw(),
                first_then.hir_id.local_id.raw(),
                first_then.tail.unwrap().hir_id.local_id.raw(),
                first_else.hir_id.local_id.raw(),
                first_else_block.hir_id.local_id.raw(),
                first_else_block.tail.unwrap().hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                second_cond.hir_id.local_id.raw(),
                second_then.hir_id.local_id.raw(),
                second_then.tail.unwrap().hir_id.local_id.raw(),
                second_else.hir_id.local_id.raw(),
                else_if_cond.hir_id.local_id.raw(),
                else_if_then.hir_id.local_id.raw(),
                else_if_then.tail.unwrap().hir_id.local_id.raw(),
                else_if_else.hir_id.local_id.raw(),
                else_if_else_block.hir_id.local_id.raw(),
                else_if_else_block.tail.unwrap().hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                third_cond.hir_id.local_id.raw(),
                third_then.hir_id.local_id.raw(),
                third_then.tail.unwrap().hir_id.local_id.raw(),
            ],
            std::array::from_fn::<_, 23, _>(|index| index as u32 + 1)
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 24);
    }

    #[test]
    fn lowers_loop_and_while_with_specified_hir_shapes() {
        let file = crate::FileId::new(100);
        let source = concat!(
            "fn f(value: I32) {\n",
            "'outer:\n",
            "loop { break 'outer }\n",
            "'again:\n",
            "while true { value }\n",
            "}\n",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);

        let loop_syntax = function
            .descendants()
            .find_map(ast::LoopExpr::cast)
            .expect("loop expression");
        let loop_expression =
            ast::Expr::cast(loop_syntax.syntax().clone()).expect("loop expression variant");
        let mut loop_source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("loop source map");
        let lowered_loop = lower_expr(&loop_expression, &arena, &symbols, &mut loop_source_map)
            .expect("loop lowers");
        let ExprKind::Loop {
            body,
            label: Some(label),
            source: HirLoopSource::Loop,
        } = lowered_loop.kind
        else {
            panic!("the source loop retains its label and loop kind");
        };
        assert_eq!(symbols.resolve(label.name).unwrap().as_ref(), "outer");
        let ExprKind::Break {
            label: Some(break_label),
            value: None,
        } = body.tail.expect("loop body tail").kind
        else {
            panic!("the loop body contains its labeled break");
        };
        assert_eq!(symbols.resolve(break_label.name).unwrap().as_ref(), "outer");
        let loop_source_map = loop_source_map.finish();
        assert_eq!(loop_source_map.node_to_syntax().len(), 4);
        let loop_pointer =
            sai_syntax::SyntaxNodePtr::new(file, loop_syntax.syntax()).expect("loop pointer");
        assert_eq!(
            loop_source_map
                .nodes_for_syntax(&loop_pointer)
                .iter()
                .map(|local_id| local_id.raw())
                .collect::<Vec<_>>(),
            [1]
        );

        let while_syntax = function
            .descendants()
            .find_map(ast::WhileExpr::cast)
            .expect("while expression");
        let while_expression =
            ast::Expr::cast(while_syntax.syntax().clone()).expect("while expression variant");
        let mut while_source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("while source map");
        let lowered_while = lower_expr(&while_expression, &arena, &symbols, &mut while_source_map)
            .expect("while lowers");
        let ExprKind::Loop {
            body: while_body,
            label: Some(while_label),
            source: HirLoopSource::While,
        } = lowered_while.kind
        else {
            panic!("labeled while lowers to a labeled loop with While source");
        };
        let ctxt = SpanCtxt::Desugared(DesugaringKind::WhileLoop);
        assert_eq!(symbols.resolve(while_label.name).unwrap().as_ref(), "again");
        assert_eq!(lowered_while.span.ctxt, ctxt);
        assert_eq!(while_body.span, lowered_while.span);
        let ExprKind::If {
            cond,
            then,
            els: Some(else_expr),
        } = while_body.tail.expect("synthetic loop body tail").kind
        else {
            panic!("while lowers to an if in the loop body");
        };
        assert_eq!(cond.kind, ExprKind::Lit(Lit::Bool(true)));
        assert_eq!(cond.span.ctxt, SpanCtxt::Root);
        assert_eq!(then.span.ctxt, SpanCtxt::Root);
        assert_eq!(while_body.tail.unwrap().span.ctxt, ctxt);
        let then_tail = then.tail.expect("user while body tail");
        assert!(matches!(then_tail.kind, ExprKind::Path(_)));
        let ExprKind::Path(path) = then_tail.kind else {
            unreachable!();
        };
        assert_eq!(
            symbols.resolve(path.segments[0].name.sym).unwrap().as_ref(),
            "value"
        );
        let ExprKind::Block(else_block) = else_expr.kind else {
            panic!("the false branch is a synthesized block");
        };
        assert_eq!(else_expr.span.ctxt, ctxt);
        assert_eq!(else_block.span.ctxt, ctxt);
        let synthesized_break = else_block.tail.expect("synthesized break");
        assert_eq!(synthesized_break.span.ctxt, ctxt);
        assert_eq!(
            synthesized_break.kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        );
        assert_eq!(
            [
                lowered_while.hir_id.local_id.raw(),
                while_body.hir_id.local_id.raw(),
                while_body.tail.unwrap().hir_id.local_id.raw(),
                cond.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4]
        );
        let source_map = while_source_map.finish();
        assert_eq!(source_map.node_to_syntax().len(), 12);
        let while_pointer =
            sai_syntax::SyntaxNodePtr::new(file, while_syntax.syntax()).expect("while pointer");
        assert_eq!(
            source_map
                .nodes_for_syntax(&while_pointer)
                .iter()
                .map(|local_id| local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 9, 10, 11]
        );
    }

    #[test]
    fn lowers_while_let_to_match_with_synthesized_break_arm() {
        let file = crate::FileId::new(100);
        let source = "fn f(value: Option<I32>) { while let Some(item) = value { item } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let while_syntax = function
            .descendants()
            .find_map(ast::WhileExpr::cast)
            .expect("while-let expression");
        let expression =
            ast::Expr::cast(while_syntax.syntax().clone()).expect("while expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("while-let lowers");
        let ctxt = SpanCtxt::Desugared(DesugaringKind::WhileLet);
        assert_eq!(lowered.span.ctxt, ctxt);
        let ExprKind::Loop {
            body,
            source: HirLoopSource::WhileLet,
            ..
        } = lowered.kind
        else {
            panic!("while-let lowers to a loop");
        };
        assert_eq!(body.span.ctxt, ctxt);
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::WhileLet,
        } = body.tail.expect("loop body match").kind
        else {
            panic!("while-let loop body is a match");
        };
        assert!(matches!(scrutinee.kind, ExprKind::Path(_)));
        assert_eq!(arms.len(), 2);
        assert!(!arms[0].synthesized);
        assert!(matches!(arms[0].pat.kind, PatKind::Ctor { .. }));
        assert_eq!(arms[0].pat.span.ctxt, SpanCtxt::Root);
        assert_eq!(arms[0].body.span.ctxt, SpanCtxt::Root);
        assert!(arms[1].synthesized);
        assert_eq!(arms[1].span.ctxt, ctxt);
        assert_eq!(arms[1].pat.kind, PatKind::Wild);
        assert_eq!(arms[1].pat.span.ctxt, ctxt);
        assert_eq!(
            arms[1].body.kind,
            ExprKind::Break {
                label: None,
                value: None,
            }
        );
        assert_eq!(arms[1].body.span.ctxt, ctxt);
        let source_map = source_map.finish();
        let while_pointer =
            sai_syntax::SyntaxNodePtr::new(file, while_syntax.syntax()).expect("while pointer");
        assert_eq!(
            source_map
                .nodes_for_syntax(&while_pointer)
                .iter()
                .map(|local_id| local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 7, 17, 18, 19]
        );
    }

    #[test]
    fn while_loop_lowers_generic_field_values_in_the_body() {
        let file = crate::FileId::new(100);
        let source = "fn f(value: I32) { while true { value.convert<U32> } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let while_syntax = function
            .descendants()
            .find_map(ast::WhileExpr::cast)
            .expect("while expression");
        let expression =
            ast::Expr::cast(while_syntax.syntax().clone()).expect("while expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("while lowers");
        assert!(matches!(
            lowered.kind,
            ExprKind::Loop {
                source: HirLoopSource::While,
                ..
            }
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_cast_recovery_and_retains_a_missing_target_type() {
        let file = crate::FileId::new(100);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(value: I32) { value as U8 }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let cast = function
            .descendants()
            .find_map(ast::CastExpr::cast)
            .expect("cast expression");
        let expression = ast::Expr::cast(cast.syntax().clone()).expect("cast expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("cast recovery lowers");
        let ExprKind::AsCast { operand, ty } = lowered.kind else {
            panic!("a cast remains in HIR for type checking");
        };
        assert!(matches!(operand.kind, ExprKind::Path(_)));
        assert!(matches!(ty.kind, HirTyKind::Path(_)));
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        assert_eq!(operand.hir_id.local_id.raw(), 2);
        assert_eq!(ty.hir_id.local_id.raw(), 5);
        assert_eq!(source_map.finish().node_to_syntax().len(), 8);

        let file = crate::FileId::new(101);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(value: I32) { value as }\n");
        assert!(
            !parsed.errors.is_empty(),
            "the parser owns the missing-type diagnostic"
        );
        let function = function_node(&parsed.root);
        let cast = function
            .descendants()
            .find_map(ast::CastExpr::cast)
            .expect("recovered cast expression");
        assert!(cast.ty().is_none());
        let expression = ast::Expr::cast(cast.syntax().clone()).expect("cast expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the cast survives parser recovery");
        let ExprKind::AsCast { operand, ty } = lowered.kind else {
            panic!("the outer cast is preserved");
        };
        assert!(matches!(operand.kind, ExprKind::Path(_)));
        assert_eq!(ty.kind, HirTyKind::Err);
        assert_eq!(
            ty.span.range,
            direct_error_child_from_end(cast.syntax())
                .expect("the parser's missing-type error")
                .text_range()
        );
        let source_map = source_map.finish();
        assert!(source_map.diagnostics().is_empty());
    }

    #[test]
    fn cast_preflight_lowers_explicit_generic_call_operands() {
        let file = crate::FileId::new(100);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { value<I32>() as U8 }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let cast = function
            .descendants()
            .find_map(ast::CastExpr::cast)
            .expect("cast expression");
        let expression = ast::Expr::cast(cast.syntax().clone()).expect("cast expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("generic call operand lowers through a cast");
        let ExprKind::AsCast { operand, .. } = lowered.kind else {
            panic!("the expression remains a cast");
        };
        let ExprKind::Call { callee, .. } = operand.kind else {
            panic!("the cast operand remains a call");
        };
        let ExprKind::Path(path) = callee.kind else {
            panic!("the generic function callee remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn cast_recovery_does_not_discard_its_parent_expression() {
        let file = crate::FileId::new(100);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(value: I32) { value as + 1 }\n");
        assert!(
            !parsed.errors.is_empty(),
            "the parser diagnoses the missing type"
        );
        let function = function_node(&parsed.root);
        let binary = function
            .descendants()
            .find_map(ast::BinExpr::cast)
            .expect("binary expression");
        let expression = ast::Expr::cast(binary.syntax().clone()).expect("binary expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the enclosing expression retains cast recovery");
        let ExprKind::Binary(BinOp::Add, left, right) = lowered.kind else {
            panic!("the binary expression is preserved");
        };
        let ExprKind::AsCast { operand, ty } = left.kind else {
            panic!("the recovered cast remains the left operand");
        };
        assert!(matches!(operand.kind, ExprKind::Path(_)));
        assert_eq!(ty.kind, HirTyKind::Err);
        assert_eq!(
            right.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
    }

    #[test]
    fn cast_recovery_does_not_escape_the_lowered_syntax_subtree() {
        let file = crate::FileId::new(100);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f(value: I32) { (value +) as U8 }\n");
        assert!(
            !parsed.errors.is_empty(),
            "the parser diagnoses the missing operand"
        );
        let function = function_node(&parsed.root);
        let cast = function
            .descendants()
            .find_map(ast::CastExpr::cast)
            .expect("outer cast expression");
        let binary = function
            .descendants()
            .find_map(ast::BinExpr::cast)
            .expect("malformed binary operand");
        assert!(contains_error_node(binary.syntax()));
        assert!(!errors_are_in_casts(binary.syntax()));
        let expression = ast::Expr::cast(binary.syntax().clone()).expect("binary expression");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map");

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("the malformed binary is recoverable on its own");
        assert_eq!(lowered.kind, ExprKind::Err);
        assert!(cast.ty().is_some());
    }

    #[test]
    fn lowers_if_let_to_match_with_desugared_synthetic_fallback() {
        let file = crate::FileId::new(111);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { if let 0 = value { 1 } }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("if-let lowers to a match");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::IfLet,
        } = lowered.kind
        else {
            panic!("if-let lowers to its dedicated match source");
        };
        assert!(matches!(scrutinee.kind, ExprKind::Path(_)));
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[0].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(0),
                negated: false,
            })
        );
        assert!(!arms[0].synthesized);
        let ExprKind::Block(then_block) = arms[0].body.kind else {
            panic!("the first arm body is the source then block");
        };
        assert_eq!(
            then_block.tail.expect("then tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert!(arms[1].synthesized);
        assert_eq!(arms[1].pat.kind, PatKind::Wild);
        assert_eq!(arms[1].body.kind, ExprKind::Tuple(&[]));

        let desugared = SpanCtxt::Desugared(DesugaringKind::IfLet);
        let source_range = if_expr.syntax().text_range();
        assert_eq!(lowered.span.range, source_range);
        assert_eq!(lowered.span.ctxt, desugared);
        assert_eq!(arms[0].span.range, source_range);
        assert_eq!(arms[0].span.ctxt, desugared);
        assert_eq!(arms[1].span.range, source_range);
        assert_eq!(arms[1].span.ctxt, desugared);
        assert_eq!(arms[1].pat.span.range, source_range);
        assert_eq!(arms[1].pat.span.ctxt, desugared);
        assert_eq!(arms[1].body.span.range, source_range);
        assert_eq!(arms[1].body.span.ctxt, desugared);
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                scrutinee.hir_id.local_id.raw(),
                match scrutinee.kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match scrutinee.kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[0].hir_id.local_id.raw(),
                arms[0].pat.hir_id.local_id.raw(),
                arms[0].body.hir_id.local_id.raw(),
                then_block.hir_id.local_id.raw(),
                then_block.tail.unwrap().hir_id.local_id.raw(),
                arms[1].hir_id.local_id.raw(),
                arms[1].pat.hir_id.local_id.raw(),
                arms[1].body.hir_id.local_id.raw(),
            ],
            std::array::from_fn::<_, 12, _>(|index| index as u32 + 1)
        );
        let if_pointer =
            sai_syntax::SyntaxNodePtr::new(file, if_expr.syntax()).expect("if expression pointer");
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 13);
        assert_eq!(map.nodes_for_syntax(&if_pointer).len(), 5);
    }

    #[test]
    fn lowers_else_if_let_inside_the_synthesized_fallback() {
        let file = crate::FileId::new(112);
        let source = "fn f() { if let 0 = value {} else if let 1 = other {} else { 2 } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("outer if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("nested if-let lowers");
        let ExprKind::Match {
            arms,
            source: HirMatchSource::IfLet,
            ..
        } = lowered.kind
        else {
            panic!("outer if-let lowers to a match");
        };
        assert_eq!(arms.len(), 2);
        assert!(arms[1].synthesized);
        let ExprKind::Match {
            arms: else_if_arms,
            source: HirMatchSource::IfLet,
            ..
        } = arms[1].body.kind
        else {
            panic!("else-if let recursively lowers to a match");
        };
        assert_eq!(else_if_arms.len(), 2);
        assert!(!else_if_arms[0].synthesized);
        assert!(else_if_arms[1].synthesized);
        let ExprKind::Block(else_block) = else_if_arms[1].body.kind else {
            panic!("the final else block is retained");
        };
        assert_eq!(
            else_block.tail.expect("else tail").kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
    }

    #[test]
    fn if_let_lowers_explicit_generic_method_call_scrutinees() {
        let file = crate::FileId::new(113);
        let source = "fn f() { if let _ = value.convert<U32>() {} }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("generic method call scrutinee lowers");
        let ExprKind::Match {
            scrutinee,
            source: HirMatchSource::IfLet,
            ..
        } = lowered.kind
        else {
            panic!("if-let retains its dedicated match source");
        };
        let ExprKind::Call { callee, .. } = scrutinee.kind else {
            panic!("if-let scrutinee is a call");
        };
        let ExprKind::Field {
            args: Some(args), ..
        } = callee.kind
        else {
            panic!("the method callee retains generic arguments");
        };
        assert_eq!(args.args.len(), 1);
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn if_recovery_is_preserved_with_generic_field_else_values() {
        let file = crate::FileId::new(101);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { if call(, true) { 1 } else { 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument errors do not discard the if");
        let ExprKind::If { cond, .. } = lowered.kind else {
            panic!("the if expression is preserved");
        };
        let ExprKind::Call { args, .. } = cond.kind else {
            panic!("the condition call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);

        let file = crate::FileId::new(102);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { if let x = call(, 1) { 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if-let expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call argument errors preserve if-let");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::IfLet,
        } = lowered.kind
        else {
            panic!("if-let lowers to its dedicated match source");
        };
        let ExprKind::Call { args, .. } = scrutinee.kind else {
            panic!("the scrutinee call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(arms.len(), 2);
        assert!(matches!(
            arms[0].pat.kind,
            PatKind::Binding {
                mode: HirBindingMode::Value,
                ..
            }
        ));
        assert!(!arms[0].synthesized);
        assert!(arms[1].synthesized);
        assert_eq!(arms[1].pat.kind, PatKind::Wild);
        assert_eq!(arms[1].body.kind, ExprKind::Tuple(&[]));

        let file = crate::FileId::new(103);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { 1 } else { value.convert<U32> } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression");
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("if recovery preserves its generic field else branch");
        let ExprKind::If {
            cond,
            els: Some(els),
            ..
        } = lowered.kind
        else {
            panic!("the recovered if expression retains its else branch");
        };
        let ExprKind::Call { args, .. } = cond.kind else {
            panic!("the recovered condition remains a call");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        let ExprKind::Block(else_block) = els.kind else {
            panic!("the else block is retained");
        };
        assert!(matches!(
            else_block.tail.map(|tail| tail.kind),
            Some(ExprKind::Field { args: Some(args), .. }) if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(104);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { (value.convert<U32>() +) } else { 0 } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression with a recovered binary");
        let then_block = if_expr.then_branch().expect("then block");
        let then_tail = then_block
            .statements()
            .last()
            .and_then(|statement| match statement {
                ast::Stmt::Expr(statement) => statement.expression(),
                _ => None,
            })
            .expect("recovered binary expression statement");
        let ast::Expr::Paren(paren) = then_tail else {
            panic!("the malformed binary retains its parentheses");
        };
        let ast::Expr::Binary(binary) = paren.expression().expect("binary expression") else {
            panic!("the parenthesized expression is binary");
        };
        assert!(binary.rhs().is_none());
        let ast::Expr::Call(call) = binary.lhs().expect("binary left operand") else {
            panic!("the binary left operand is a call");
        };
        let ast::Expr::Field(callee) = call.callee().expect("call callee") else {
            panic!("the call callee is a field expression");
        };
        assert!(direct_generic_args(callee.syntax()).is_some());
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("generic method calls lower inside recoverable if branches");
        let ExprKind::If { cond, then, .. } = lowered.kind else {
            panic!("the if is retained around the recovered block");
        };
        let ExprKind::Call { args, .. } = cond.kind else {
            panic!("the if condition call is preserved");
        };
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(then.tail.expect("then tail").kind, ExprKind::Err);
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(105);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { if call(, true) { value.convert<U32>()[] } else { 0 } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let if_expr = function
            .descendants()
            .find_map(ast::IfExpr::cast)
            .expect("if expression with a recovered index");
        let then_block = if_expr.then_branch().expect("then block");
        let then_tail = then_block
            .statements()
            .last()
            .and_then(|statement| match statement {
                ast::Stmt::Expr(statement) => statement.expression(),
                _ => None,
            })
            .expect("recovered index expression statement");
        let ast::Expr::Index(index) = then_tail else {
            panic!("the recovered expression is an index");
        };
        assert!(index.base().is_some());
        assert!(index.index().is_none());
        assert!(
            index
                .base()
                .is_some_and(|base| matches!(base, ast::Expr::Call(ref call)
                    if call.callee().is_some_and(|callee| matches!(callee,
                        ast::Expr::Field(ref field)
                            if direct_generic_args(field.syntax()).is_some()))))
        );
        let expression =
            ast::Expr::cast(if_expr.syntax().clone()).expect("if is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("generic method callee lowers through recovered index syntax");
        let ExprKind::If { cond, then, .. } = lowered.kind else {
            panic!("the if remains around the recovered index");
        };
        let ExprKind::Call { args, .. } = cond.kind else {
            panic!("the condition call remains available");
        };
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(then.tail.expect("then tail").kind, ExprKind::Err);
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_match_arms_with_patterns_guards_and_source_ordered_ids() {
        let file = crate::FileId::new(106);
        let source = "fn f() { match value { 0 => 1, 1 if test => 2, _ => 3 } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("match lowers");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = lowered.kind
        else {
            panic!("the expression lowers to a normal match");
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(scrutinee.kind, ExprKind::Path(_)));
        assert_eq!(
            arms[0].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(0),
                negated: false,
            })
        );
        assert!(arms[0].guard.is_none());
        assert_eq!(
            arms[0].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            arms[1].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert!(matches!(
            arms[1].guard.map(|guard| guard.kind),
            Some(ExprKind::Path(_))
        ));
        assert_eq!(
            arms[1].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(arms[2].pat.kind, PatKind::Wild);
        assert_eq!(
            arms[2].body.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(3),
                negated: false,
            })
        );
        assert!(arms.iter().all(|arm| !arm.synthesized));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                scrutinee.hir_id.local_id.raw(),
                match scrutinee.kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match scrutinee.kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[0].hir_id.local_id.raw(),
                arms[0].pat.hir_id.local_id.raw(),
                arms[0].body.hir_id.local_id.raw(),
                arms[1].hir_id.local_id.raw(),
                arms[1].pat.hir_id.local_id.raw(),
                arms[1].guard.unwrap().hir_id.local_id.raw(),
                match arms[1].guard.unwrap().kind {
                    ExprKind::Path(path) => path.hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                match arms[1].guard.unwrap().kind {
                    ExprKind::Path(path) => path.segments[0].hir_id.local_id.raw(),
                    _ => unreachable!(),
                },
                arms[1].body.hir_id.local_id.raw(),
                arms[2].hir_id.local_id.raw(),
                arms[2].pat.hir_id.local_id.raw(),
                arms[2].body.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 17);
    }

    #[test]
    fn match_recovery_keeps_arms_and_lowers_generic_field_values() {
        let file = crate::FileId::new(107);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { match call(, true) { _ =>, 1 => 2 } }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("scrutinee call errors preserve the match");
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = lowered.kind
        else {
            panic!("the match is preserved");
        };
        let ExprKind::Call { args, .. } = scrutinee.kind else {
            panic!("the scrutinee call is preserved");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(
            arms[1].pat.kind,
            PatKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );

        let file = crate::FileId::new(108);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match call(, true) { _ => value.convert<U32> } }\n",
        );
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("match recovery retains its generic field arm");
        let ExprKind::Match { arms, .. } = lowered.kind else {
            panic!("the match expression is retained");
        };
        assert_eq!(arms.len(), 1);
        assert!(matches!(
            arms[0].body.kind,
            ExprKind::Field { args: Some(args), .. } if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn missing_match_scrutinee_and_arm_list_keep_distinct_error_nodes() {
        let file = crate::FileId::new(109);
        let source = "fn f() { (match, call(, 2)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        assert!(match_expr.scrutinee().is_none());
        assert!(match_expr.arms().is_none());
        let errors = match_expr
            .syntax()
            .children()
            .filter(|child| child.kind() == SyntaxKind::ERROR)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 2);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let tuple_expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&tuple_expression, &arena, &symbols, &mut source_map)
            .expect("the sibling call keeps the tuple recoverable");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the outer tuple is preserved");
        };
        let ExprKind::Match {
            scrutinee,
            arms,
            source: HirMatchSource::Normal,
        } = elements[0].kind
        else {
            panic!("the malformed match is preserved");
        };
        assert_eq!(scrutinee.kind, ExprKind::Err);
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].pat.kind, PatKind::Err);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(scrutinee.span.range, errors[0].text_range());
        assert_eq!(arms[0].span.range, errors[1].text_range());
        let first_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[0]).expect("scrutinee error pointer");
        let second_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[1]).expect("arm error pointer");
        let map = source_map.finish();
        assert_eq!(map.nodes_for_syntax(&first_error).len(), 1);
        assert_eq!(map.nodes_for_syntax(&second_error).len(), 3);
    }

    #[test]
    fn missing_match_body_uses_its_own_recovery_node() {
        let file = crate::FileId::new(110);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { match call(, true) { _ } }\n");
        let function = function_node(&parsed.root);
        assert!(!parsed.errors.is_empty());
        let match_expr = function
            .descendants()
            .find_map(ast::MatchExpr::cast)
            .expect("match expression");
        let arm = function
            .descendants()
            .find_map(ast::MatchArm::cast)
            .expect("match arm");
        let errors = arm
            .syntax()
            .children()
            .filter(|child| child.kind() == SyntaxKind::ERROR)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].text_range(), errors[1].text_range());
        let expression =
            ast::Expr::cast(match_expr.syntax().clone()).expect("match is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("a missing body is represented as an error expression");
        let ExprKind::Match { arms, .. } = lowered.kind else {
            panic!("the match is preserved: {lowered:?}");
        };
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].pat.kind, PatKind::Wild);
        assert_eq!(arms[0].body.kind, ExprKind::Err);
        assert_eq!(arms[0].body.span.range, errors[1].text_range());
        let arrow_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[0]).expect("arrow error pointer");
        let body_error =
            sai_syntax::SyntaxNodePtr::new(file, &errors[1]).expect("body error pointer");
        let map = source_map.finish();
        assert!(map.nodes_for_syntax(&arrow_error).is_empty());
        assert_eq!(map.nodes_for_syntax(&body_error).len(), 1);
    }

    #[test]
    fn lowers_generic_field_arguments_in_value_position() {
        let file = crate::FileId::new(73);
        let source = "fn f() { (1, value.convert<U32, 5>) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let field = function
            .descendants()
            .find_map(ast::FieldExpr::cast)
            .expect("generic field expression");
        let generic_args_syntax =
            direct_generic_args(field.syntax()).expect("field has explicit generic arguments");
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("tuple lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        let ExprKind::Field {
            args: Some(args), ..
        } = elements[1].kind
        else {
            panic!("generic field arguments remain on the field expression");
        };
        assert_eq!(args.args.len(), 2);
        assert!(matches!(
            args.args[0],
            HirGenericArg::Type(ty) if matches!(ty.kind, HirTyKind::Path(_))
        ));
        assert_eq!(
            args.args[1],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(5),
                negated: false,
            })
        );
        assert_eq!(args.span.range, generic_args_syntax.syntax().text_range());
        let source_ty = generic_args_syntax
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::PATH_TYPE)
            .expect("generic type argument syntax");
        let source_ty_ptr =
            SyntaxNodePtr::new(file, &source_ty).expect("generic type syntax pointer");
        let HirGenericArg::Type(lowered_ty) = args.args[0] else {
            panic!("first generic argument is a type");
        };
        let map = source_map.finish();
        assert!(map.node_to_syntax().len() > 1);
        assert_eq!(map.syntax_for_node(lowered_ty.hir_id), Some(&source_ty_ptr));
    }

    #[test]
    fn lowers_standalone_generic_path_arguments_in_value_position() {
        let file = crate::FileId::new(75);
        let source = "fn f() { (1, value<I32, 5>) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("tuple lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        let ExprKind::Path(path) = elements[1].kind else {
            panic!("generic function item remains a path value");
        };
        let args = path.segments[0].args.expect("explicit generic arguments");
        assert_eq!(args.args.len(), 2);
        assert!(matches!(
            args.args[0],
            HirGenericArg::Type(ty) if matches!(ty.kind, HirTyKind::Path(_))
        ));
        assert_eq!(
            args.args[1],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(5),
                negated: false,
            })
        );
        let map = source_map.finish();
        assert!(map.node_to_syntax().len() > 1);
        let source_ty = tuple
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::PATH_TYPE)
            .expect("generic type argument syntax");
        let source_ty_ptr =
            SyntaxNodePtr::new(file, &source_ty).expect("generic type syntax pointer");
        let HirGenericArg::Type(lowered_ty) = args.args[0] else {
            panic!("first generic argument is a type");
        };
        assert_eq!(map.syntax_for_node(lowered_ty.hir_id), Some(&source_ty_ptr));
    }

    #[test]
    fn lowers_plain_self_and_implicit_member_expression_paths() {
        let file = crate::FileId::new(74);
        let source = "fn f() { (value, self, Self, .Variant) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("path expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);

        let ExprKind::Path(value_path) = elements[0].kind else {
            panic!("plain name lowers to a path");
        };
        assert_eq!(value_path.root, PathRoot::Plain);
        assert_eq!(value_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(value_path.segments[0].name.sym)
                .expect("path name resolves")
                .as_ref(),
            "value"
        );
        assert_eq!(value_path.segments[0].args, None);

        let ExprKind::Path(self_path) = elements[1].kind else {
            panic!("self lowers to a path");
        };
        assert_eq!(self_path.root, PathRoot::SelfValue);
        assert!(self_path.segments.is_empty());

        let ExprKind::Path(self_type_path) = elements[2].kind else {
            panic!("Self lowers to a path");
        };
        assert_eq!(self_type_path.root, PathRoot::SelfType);
        assert!(self_type_path.segments.is_empty());

        let ExprKind::ImplicitMember(variant) = elements[3].kind else {
            panic!("implicit member lowers to its dedicated expression");
        };
        assert_eq!(
            symbols
                .resolve(variant.sym)
                .expect("implicit member name resolves")
                .as_ref(),
            "Variant"
        );
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                value_path.hir_id.local_id.raw(),
                value_path.segments[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                self_path.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                self_type_path.hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );

        let map = source_map.finish();
        let path_span = map
            .syntax_for_node(value_path.hir_id)
            .expect("path has a source mapping")
            .resolve(file, &parsed.root)
            .expect("path syntax resolves")
            .text_range();
        let segment_span = map
            .syntax_for_node(value_path.segments[0].hir_id)
            .expect("segment has a source mapping")
            .resolve(file, &parsed.root)
            .expect("segment syntax resolves")
            .text_range();
        assert_eq!(path_span, elements[0].span.range);
        assert_eq!(segment_span, value_path.segments[0].name.span.range);
        assert_eq!(map.node_to_syntax().len(), 10);
    }

    #[test]
    fn lowers_nested_field_and_tuple_index_expressions() {
        let file = crate::FileId::new(76);
        let source = "fn f() { (value.field.other, value.0, value.00, value.999999999999) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidTupleIndex)
                .count(),
            1
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("field and tuple-index expressions lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);

        let ExprKind::Field {
            base: outer_base,
            name: outer_name,
            args: None,
        } = elements[0].kind
        else {
            panic!("outer field expression is lowered");
        };
        assert_eq!(symbols.resolve(outer_name.sym).unwrap().as_ref(), "other");
        let ExprKind::Field {
            base: inner_base,
            name: inner_name,
            args: None,
        } = outer_base.kind
        else {
            panic!("field chain is represented as nested field expressions");
        };
        assert_eq!(symbols.resolve(inner_name.sym).unwrap().as_ref(), "field");
        let ExprKind::Path(path) = inner_base.kind else {
            panic!("field chain starts with a plain path");
        };
        assert_eq!(path.root, PathRoot::Plain);
        assert_eq!(path.segments.len(), 1);

        let ExprKind::TupleField {
            base: tuple_base,
            index: Some(0),
        } = elements[1].kind
        else {
            panic!("valid tuple index is retained");
        };
        assert!(matches!(tuple_base.kind, ExprKind::Path(_)));
        let ExprKind::TupleField {
            base: invalid_index_base,
            index: None,
        } = elements[2].kind
        else {
            panic!("parser-diagnosed invalid index keeps its recovery value");
        };
        assert!(matches!(invalid_index_base.kind, ExprKind::Path(_)));
        let ExprKind::TupleField {
            base: overflowing_index_base,
            index: None,
        } = elements[3].kind
        else {
            panic!("an index outside u32 has no decoded HIR index");
        };
        assert!(matches!(overflowing_index_base.kind, ExprKind::Path(_)));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                outer_base.hir_id.local_id.raw(),
                inner_base.hir_id.local_id.raw(),
                path.hir_id.local_id.raw(),
                path.segments[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                tuple_base.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                invalid_index_base.hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                overflowing_index_base.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 15, 16]
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 19);
        assert!(
            map.diagnostics().is_empty(),
            "the parser's INVALID_TUPLE_INDEX diagnostic remains authoritative"
        );
    }

    #[test]
    fn lowers_all_binary_operators_with_parent_first_ids() {
        let cases = [
            ("or", BinOp::Or),
            ("and", BinOp::And),
            ("==", BinOp::Eq),
            ("!=", BinOp::Ne),
            ("<", BinOp::Lt),
            ("<=", BinOp::Le),
            (">", BinOp::Gt),
            (">=", BinOp::Ge),
            ("|", BinOp::BitOr),
            ("^", BinOp::BitXor),
            ("&", BinOp::BitAnd),
            ("<<", BinOp::Shl),
            (">>", BinOp::Shr),
            ("+", BinOp::Add),
            ("-", BinOp::Sub),
            ("+%", BinOp::WrapAdd),
            ("-%", BinOp::WrapSub),
            ("*", BinOp::Mul),
            ("/", BinOp::Div),
            ("%", BinOp::Rem),
            ("*%", BinOp::WrapMul),
            ("||", BinOp::Or),
            ("&&", BinOp::And),
        ];
        let source = format!(
            "fn f() {{ ({}) }}\n",
            cases
                .iter()
                .map(|(operator, _)| format!("left {operator} right"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let file = crate::FileId::new(77);
        let (snapshot, parsed, ast_ids) = parse_file(file, &source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::UseAndOr, Code::UseAndOr]
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("binary lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), cases.len());
        assert_eq!(lowered.hir_id.local_id.raw(), 1);
        for (index, (element, (_, expected_op))) in elements.iter().zip(cases).enumerate() {
            let ExprKind::Binary(op, lhs, rhs) = element.kind else {
                panic!("tuple element {index} is binary");
            };
            assert_eq!(op, expected_op, "operator at tuple element {index}");
            assert!(matches!(lhs.kind, ExprKind::Path(_)));
            assert!(matches!(rhs.kind, ExprKind::Path(_)));
            assert_eq!(element.hir_id.local_id.raw(), 2 + index as u32 * 7);
            assert_eq!(lhs.hir_id.local_id.raw(), element.hir_id.local_id.raw() + 1);
            assert_eq!(rhs.hir_id.local_id.raw(), element.hir_id.local_id.raw() + 4);
        }
        assert_eq!(
            source_map.finish().node_to_syntax().len(),
            2 + cases.len() * 7,
            "the source map also contains the owner node"
        );
    }

    #[test]
    fn lowers_prefix_operators_and_folds_negative_numeric_literals() {
        let file = crate::FileId::new(78);
        let source = "fn f() { (-12, -1.25, -value, ~mask, *pointer, not condition, borrow value, borrow mut value, -true) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("prefixes lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 9);
        assert_eq!(
            elements[0].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(12),
                negated: true,
            })
        );
        assert_eq!(
            elements[1].kind,
            ExprKind::Lit(Lit::Float {
                text: symbols.intern("1.25").expect("float spelling interns"),
                negated: true,
            })
        );
        assert!(matches!(
            elements[2].kind,
            ExprKind::Unary(UnOp::Neg, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[3].kind,
            ExprKind::Unary(UnOp::BitNot, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[4].kind,
            ExprKind::Unary(UnOp::Deref, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[5].kind,
            ExprKind::Unary(UnOp::Not, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[6].kind,
            ExprKind::Borrow(Mutability::Not, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[7].kind,
            ExprKind::Borrow(Mutability::Mut, operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[8].kind,
            ExprKind::Unary(UnOp::Neg, operand)
                if operand.kind == ExprKind::Lit(Lit::Bool(true))
        ));

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 30);
        let negative_float = map
            .syntax_for_node(elements[1].hir_id)
            .expect("folded float has a source mapping")
            .resolve(file, &parsed.root)
            .expect("negative float syntax resolves");
        assert_eq!(negative_float.text_range(), elements[1].span.range);
        assert_eq!(
            &source[negative_float.text_range().start().to_usize()
                ..negative_float.text_range().end().to_usize()],
            "-1.25",
            "a folded negative literal maps to the whole prefix expression"
        );
    }

    #[test]
    fn lowers_all_six_range_expression_forms() {
        let file = crate::FileId::new(80);
        let source = "fn f() { (start..end, start..=end, start.., ..end, ..=end, ..) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("all range expression forms lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 6);
        let expected = [
            (true, true, false),
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, true, true),
            (false, false, false),
        ];
        for (index, (range, (has_start, has_end, inclusive))) in
            elements.iter().zip(expected).enumerate()
        {
            let ExprKind::Range {
                start,
                end,
                inclusive: actual_inclusive,
            } = range.kind
            else {
                panic!("tuple element {index} is a range");
            };
            assert_eq!(start.is_some(), has_start, "range {index} start");
            assert_eq!(end.is_some(), has_end, "range {index} end");
            assert_eq!(actual_inclusive, inclusive, "range {index} inclusivity");
            for endpoint in start.into_iter().chain(end) {
                assert!(matches!(endpoint.kind, ExprKind::Path(_)));
            }
        }
        let ExprKind::Range {
            start: Some(first_start),
            end: Some(first_end),
            inclusive: false,
        } = elements[0].kind
        else {
            panic!("first range has two exclusive endpoints");
        };
        assert_eq!(elements[0].hir_id.local_id.raw(), 2);
        assert_eq!(first_start.hir_id.local_id.raw(), 3);
        assert_eq!(first_end.hir_id.local_id.raw(), 6);
        assert_eq!(elements[1].hir_id.local_id.raw(), 9);
        assert_eq!(elements[2].hir_id.local_id.raw(), 16);
        assert_eq!(elements[3].hir_id.local_id.raw(), 20);
        assert_eq!(elements[4].hir_id.local_id.raw(), 24);
        assert_eq!(elements[5].hir_id.local_id.raw(), 28);

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 29);
        let first_range_syntax = map
            .syntax_for_node(elements[0].hir_id)
            .expect("range has a source mapping")
            .resolve(file, &parsed.root)
            .expect("range syntax resolves");
        assert_eq!(first_range_syntax.text_range(), elements[0].span.range);
        assert_eq!(
            &source[first_range_syntax.text_range().start().to_usize()
                ..first_range_syntax.text_range().end().to_usize()],
            "start..end"
        );
    }

    #[test]
    fn range_recovery_preserves_diagnostics_and_lowers_generic_endpoints() {
        let file = crate::FileId::new(81);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { left..value<I32> }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("range expression");
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("range lowers");
        let ExprKind::Range {
            start: Some(_),
            end: Some(end),
            inclusive: false,
        } = lowered.kind
        else {
            panic!("the range retains both endpoints");
        };
        let ExprKind::Path(path) = end.kind else {
            panic!("the generic endpoint remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(82);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { value..= }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("recovered range expression");
        assert!(contains_error_node(range.syntax()));
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("parser-recovered range lowers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert!(map.diagnostics().is_empty());
        assert_eq!(map.node_to_syntax().len(), 2);
    }

    #[test]
    fn chained_range_diagnostic_does_not_prevent_hir_recovery() {
        let file = crate::FileId::new(83);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { left..middle..right }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::ChainedRange]
        );
        let function = function_node(&parsed.root);
        let range = function
            .descendants()
            .find_map(ast::RangeExpr::cast)
            .expect("outer range expression");
        let expression =
            ast::Expr::cast(range.syntax().clone()).expect("range is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("range lowers");
        let ExprKind::Range {
            start: Some(start),
            end: Some(end),
            inclusive: false,
        } = lowered.kind
        else {
            panic!("outer range retains both endpoints");
        };
        assert!(matches!(
            start.kind,
            ExprKind::Range {
                start: Some(_),
                end: Some(_),
                inclusive: false,
            }
        ));
        assert!(matches!(end.kind, ExprKind::Path(_)));
        assert!(source_map.finish().diagnostics().is_empty());
    }

    #[test]
    fn lowers_index_try_and_await_postfix_expressions() {
        let file = crate::FileId::new(84);
        let source = "fn f() { (array[index], optional?, future.await, nested[a][b]?) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("postfix lowers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 4);
        assert!(matches!(
            elements[0].kind,
            ExprKind::Index(base, index)
                if matches!(base.kind, ExprKind::Path(_))
                    && matches!(index.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[1].kind,
            ExprKind::Try(operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        assert!(matches!(
            elements[2].kind,
            ExprKind::Await(operand) if matches!(operand.kind, ExprKind::Path(_))
        ));
        let ExprKind::Try(outer_operand) = elements[3].kind else {
            panic!("postfix question mark wraps the indexed expression");
        };
        let ExprKind::Index(outer_base, outer_index) = outer_operand.kind else {
            panic!("outer index is preserved");
        };
        let ExprKind::Index(inner_base, inner_index) = outer_base.kind else {
            panic!("nested index remains left-associated");
        };
        assert!(matches!(inner_base.kind, ExprKind::Path(_)));
        assert!(matches!(inner_index.kind, ExprKind::Path(_)));
        assert!(matches!(outer_index.kind, ExprKind::Path(_)));
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                elements[3].hir_id.local_id.raw(),
                outer_operand.hir_id.local_id.raw(),
                outer_base.hir_id.local_id.raw(),
                inner_base.hir_id.local_id.raw(),
                inner_index.hir_id.local_id.raw(),
                outer_index.hir_id.local_id.raw(),
            ],
            [1, 2, 9, 13, 17, 18, 19, 20, 23, 26]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 29);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn postfix_recovery_lowers_generic_index_expressions() {
        let file = crate::FileId::new(85);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { array[value<I32>] }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let index = function
            .descendants()
            .find_map(ast::IndexExpr::cast)
            .expect("index expression");
        let expression =
            ast::Expr::cast(index.syntax().clone()).expect("index is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("index lowers");
        let ExprKind::Index(_, index) = lowered.kind else {
            panic!("the postfix expression remains an index");
        };
        let ExprKind::Path(path) = index.kind else {
            panic!("the generic index remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(86);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { array[] }\n");
        assert!(!parsed.errors.is_empty());
        let function = function_node(&parsed.root);
        let index = function
            .descendants()
            .find_map(ast::IndexExpr::cast)
            .expect("recovered index expression");
        assert!(contains_error_node(index.syntax()));
        let expression =
            ast::Expr::cast(index.syntax().clone()).expect("index is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("index recovers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 2);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn lowers_calls_with_source_ordered_positional_named_and_base_arguments() {
        let file = crate::FileId::new(87);
        let source = "fn f() { (factory(1, named: value, ..base), object.method(first: 2, value), .Variant(3)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple expression");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("calls lower");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("outer expression is a tuple");
        };
        assert_eq!(elements.len(), 3);

        let ExprKind::Call {
            callee: first_callee,
            args: first_args,
            base: Some(base),
        } = elements[0].kind
        else {
            panic!("the first call retains its record-update base");
        };
        assert!(matches!(first_callee.kind, ExprKind::Path(_)));
        assert_eq!(first_args.len(), 2);
        assert_eq!(first_args[0].name, None);
        assert_eq!(
            first_args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            symbols
                .resolve(first_args[1].name.expect("named argument").sym)
                .expect("argument name resolves")
                .as_ref(),
            "named"
        );
        assert!(matches!(first_args[1].expr.kind, ExprKind::Path(_)));
        assert!(matches!(base.kind, ExprKind::Path(_)));

        let ExprKind::Call {
            callee: method,
            args: method_args,
            base: None,
        } = elements[1].kind
        else {
            panic!("the method call has no base argument");
        };
        assert!(matches!(method.kind, ExprKind::Field { .. }));
        assert_eq!(method_args.len(), 2);
        assert_eq!(
            symbols
                .resolve(method_args[0].name.expect("first named argument").sym)
                .expect("argument name resolves")
                .as_ref(),
            "first"
        );
        assert_eq!(
            method_args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );
        assert_eq!(method_args[1].name, None);
        assert!(matches!(method_args[1].expr.kind, ExprKind::Path(_)));

        let ExprKind::Call {
            callee: variant,
            args: variant_args,
            base: None,
        } = elements[2].kind
        else {
            panic!("implicit variant construction is represented as a call");
        };
        let ExprKind::ImplicitMember(variant_name) = variant.kind else {
            panic!("call callee is the implicit member");
        };
        assert_eq!(
            symbols
                .resolve(variant_name.sym)
                .expect("variant name resolves")
                .as_ref(),
            "Variant"
        );
        assert_eq!(variant_args.len(), 1);
        assert_eq!(
            [
                lowered.hir_id.local_id.raw(),
                elements[0].hir_id.local_id.raw(),
                first_callee.hir_id.local_id.raw(),
                first_args[0].expr.hir_id.local_id.raw(),
                first_args[1].expr.hir_id.local_id.raw(),
                base.hir_id.local_id.raw(),
                elements[1].hir_id.local_id.raw(),
                method.hir_id.local_id.raw(),
                elements[2].hir_id.local_id.raw(),
                variant.hir_id.local_id.raw(),
                variant_args[0].expr.hir_id.local_id.raw(),
            ],
            [1, 2, 3, 6, 7, 10, 13, 14, 22, 23, 24]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 25);
        let call_syntax = map
            .syntax_for_node(elements[0].hir_id)
            .expect("call has a source mapping")
            .resolve(file, &parsed.root)
            .expect("call syntax resolves");
        assert_eq!(call_syntax.text_range(), elements[0].span.range);
        assert_eq!(
            &source[call_syntax.text_range().start().to_usize()
                ..call_syntax.text_range().end().to_usize()],
            "factory(1, named: value, ..base)"
        );
    }

    #[test]
    fn explicit_generic_call_arguments_and_multiple_base_recovery() {
        let file = crate::FileId::new(88);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { factory<I32>(value) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call expression");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("explicit generic call arguments lower");
        let ExprKind::Call { callee, .. } = lowered.kind else {
            panic!("generic function invocation remains a call");
        };
        let ExprKind::Path(path) = callee.kind else {
            panic!("generic function callee remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);

        let file = crate::FileId::new(89);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { factory(..first, ..second) }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast]
        );
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("recovered call expression");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("call recovers");
        assert_eq!(lowered.kind, ExprKind::Err);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 2);
        assert!(map.diagnostics().is_empty());
    }

    #[test]
    fn call_argument_recovery_lowers_generic_paths_and_keeps_valid_arguments() {
        let file = crate::FileId::new(90);
        let source = "fn f() { (call(, 1), 2) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::ExpectedExpression]
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple containing the recovered call");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("call recovers");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("a malformed argument does not discard its enclosing tuple");
        };
        assert_eq!(elements.len(), 2);
        let ExprKind::Call {
            args, base: None, ..
        } = elements[0].kind
        else {
            panic!("the call retains its HIR node");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        assert_eq!(
            args[1].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        assert_eq!(
            elements[1].kind,
            ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            })
        );

        let map = source_map.finish();
        let error_syntax = map
            .syntax_for_node(args[0].expr.hir_id)
            .expect("recovered argument has a source mapping")
            .resolve(file, &parsed.root)
            .expect("recovered argument syntax resolves");
        assert_eq!(error_syntax.kind(), SyntaxKind::ERROR);
        assert!(error_syntax.text_range().is_empty());
        assert!(map.diagnostics().is_empty());

        let file = crate::FileId::new(92);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { call(@inline 1) }\n");
        assert_eq!(
            parsed
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidAttributeTarget]
        );
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call with an invalid argument attribute");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("call survives an invalid argument attribute");
        let ExprKind::Call { args, .. } = lowered.kind else {
            panic!("an attribute error is not an argument");
        };
        assert_eq!(args.len(), 1);
        assert_eq!(
            args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );

        let file = crate::FileId::new(93);
        let source = "fn f() { (call(named: @inline), call(..@inline)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            2
        );
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedExpression)
                .count(),
            2
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple containing incomplete named and base arguments");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("incomplete named and base arguments recover");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("incomplete arguments stay local to their calls");
        };
        let ExprKind::Call {
            args: named_args,
            base: None,
            ..
        } = elements[0].kind
        else {
            panic!("first expression is the named-argument call");
        };
        assert_eq!(named_args.len(), 1);
        assert!(named_args[0].name.is_some());
        assert_eq!(named_args[0].expr.kind, ExprKind::Err);

        let ExprKind::Call {
            args: base_args,
            base: Some(base),
            ..
        } = elements[1].kind
        else {
            panic!("second expression is the base-argument call");
        };
        assert!(base_args.is_empty());
        assert_eq!(base.kind, ExprKind::Err);

        let map = source_map.finish();
        for error_expr in [named_args[0].expr, base] {
            let syntax = map
                .syntax_for_node(error_expr.hir_id)
                .expect("missing argument expression has a source mapping")
                .resolve(file, &parsed.root)
                .expect("missing argument syntax resolves");
            assert_eq!(syntax.kind(), SyntaxKind::ERROR);
            assert!(
                syntax.text_range().is_empty(),
                "attribute recovery must not steal the missing-expression source map"
            );
        }

        let file = crate::FileId::new(94);
        let source = "fn f() { (, call(@inline 1)) }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
        );
        let function = function_node(&parsed.root);
        let tuple = function
            .descendants()
            .find_map(ast::TupleExpr::cast)
            .expect("tuple with a missing element and an attributed call");
        let expression =
            ast::Expr::cast(tuple.syntax().clone()).expect("tuple is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("tuple and nested call recover independently");
        let ExprKind::Tuple(elements) = lowered.kind else {
            panic!("the enclosing tuple remains available");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].kind, ExprKind::Err);
        let ExprKind::Call { args, .. } = elements[1].kind else {
            panic!("the valid call remains available");
        };
        assert_eq!(args.len(), 1);
        assert_eq!(
            args[0].expr.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        );
        let map = source_map.finish();
        let missing_element = map
            .syntax_for_node(elements[0].hir_id)
            .expect("missing tuple element has a source mapping")
            .resolve(file, &parsed.root)
            .expect("missing tuple element syntax resolves");
        assert_eq!(missing_element.kind(), SyntaxKind::ERROR);
        assert!(missing_element.text_range().is_empty());

        let file = crate::FileId::new(91);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { call(, value<I32>) }\n");
        let function = function_node(&parsed.root);
        let call = function
            .descendants()
            .find_map(ast::CallExpr::cast)
            .expect("call with a valid unsupported sibling argument");
        let expression =
            ast::Expr::cast(call.syntax().clone()).expect("call is an expression variant");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("call recovers");
        let ExprKind::Call { args, .. } = lowered.kind else {
            panic!("the recovered expression remains a call");
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].expr.kind, ExprKind::Err);
        let ExprKind::Path(path) = args[1].expr.kind else {
            panic!("the valid generic call argument remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_generic_paths_as_binary_operands() {
        let file = crate::FileId::new(79);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { (left + value<I32>) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let binary = function
            .descendants()
            .find_map(ast::BinExpr::cast)
            .expect("binary expression");
        let expression = ast::Expr::cast(binary.syntax().clone())
            .expect("binary expression is an expression variant");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("binary lowers");
        let ExprKind::Binary(_, _, rhs) = lowered.kind else {
            panic!("outer expression is binary");
        };
        let ExprKind::Path(path) = rhs.kind else {
            panic!("generic operand remains a path");
        };
        assert!(path.segments[0].args.is_some());
        assert!(source_map.finish().node_to_syntax().len() > 1);
    }

    #[test]
    fn lowers_malformed_literal_to_error_without_cascading() {
        let file = crate::FileId::new(62);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { 1__2 }\n");
        let function = function_node(&parsed.root);
        let literal = function
            .descendants()
            .find_map(ast::LiteralExpr::cast)
            .expect("recovered literal");
        assert!(literal.token().expect("integer token").syntax().has_error());

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_literal(&literal, &arena, &symbols, &mut source_map)
            .expect("recovered literals lower");
        assert_eq!(lowered.kind, ExprKind::Err);
    }

    #[test]
    fn retains_recovery_values_for_error_marked_text_and_character_literals() {
        let file = crate::FileId::new(63);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, r#"fn f() { ("\q", b"\q", '\q', b'€') }"#);
        let function = function_node(&parsed.root);
        let literals = function
            .descendants()
            .filter_map(ast::LiteralExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(literals.len(), 4);
        assert!(
            literals
                .iter()
                .all(|literal| !contains_error_node(literal.syntax()))
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = literals
            .iter()
            .map(|literal| lower_literal(literal, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("error-marked literals lower");

        assert_eq!(
            lowered[0].kind,
            ExprKind::Lit(Lit::Str(
                symbols.intern("\u{fffd}").expect("replacement interns")
            ))
        );
        assert_eq!(
            lowered[1].kind,
            ExprKind::Lit(Lit::ByteStr("\u{fffd}".as_bytes()))
        );
        assert_eq!(lowered[2].kind, ExprKind::Lit(Lit::Char('\u{fffd}')));
        assert_eq!(lowered[3].kind, ExprKind::Lit(Lit::Byte(0)));
    }

    #[test]
    fn lowers_literal_patterns_and_preserves_negative_integer_values() {
        let file = crate::FileId::new(64);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            r#"fn f() { match value { -42 => (), 340282366920938463463374607431768211456 => (), "text\n" => (), b"bytes\x21" => (), 'x' => (), b'y' => (), true => (), false => (), 1__2 => (), 1.5 => (), -2.5 => () } }"#,
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::LiteralPat::cast)
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 11);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::FloatPattern)
                .count(),
            2
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_literal_pat(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literal patterns lower");

        assert_eq!(
            lowered[0].kind,
            PatKind::Lit(Lit::Int {
                value: Some(42),
                negated: true,
            })
        );
        assert_eq!(
            lowered[1].kind,
            PatKind::Lit(Lit::Int {
                value: None,
                negated: false,
            })
        );
        let decoded_string = match lowered[2].kind {
            PatKind::Lit(Lit::Str(value)) => symbols.resolve(value).expect("string resolves"),
            _ => panic!("string pattern lowers to PatKind::Lit"),
        };
        assert_eq!(decoded_string.as_ref(), "text\n");
        assert_eq!(lowered[3].kind, PatKind::Lit(Lit::ByteStr(b"bytes!")));
        assert_eq!(lowered[4].kind, PatKind::Lit(Lit::Char('x')));
        assert_eq!(lowered[5].kind, PatKind::Lit(Lit::Byte(b'y')));
        assert_eq!(lowered[6].kind, PatKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[7].kind, PatKind::Lit(Lit::Bool(false)));
        assert_eq!(lowered[8].kind, PatKind::Err);
        assert_eq!(lowered[9].kind, PatKind::Err);
        assert_eq!(lowered[10].kind, PatKind::Err);
        assert!(
            lowered
                .iter()
                .enumerate()
                .all(|(index, pattern)| pattern.hir_id.local_id.raw() == index as u32 + 1)
        );
        for (pattern, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(pattern.span.range, syntax.syntax().text_range());
            assert_eq!(pattern.span.file, file);
            assert_eq!(pattern.span.ctxt, SpanCtxt::Root);
        }

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), patterns.len() + 1);
    }

    #[test]
    fn lowers_wildcard_and_strips_parenthesized_patterns() {
        let file = crate::FileId::new(65);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match value { _ => (), (true) => (), ((_)) => () } }\n",
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 3);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..3]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("supported patterns lower");

        assert_eq!(lowered[0].kind, PatKind::Wild);
        assert_eq!(lowered[1].kind, PatKind::Lit(Lit::Bool(true)));
        assert_eq!(lowered[2].kind, PatKind::Wild);
        assert_eq!(lowered[0].span.range, patterns[0].syntax().text_range());
        let ast::Pattern::Parenthesized(paren) = &patterns[1] else {
            panic!("second pattern is parenthesized");
        };
        let inner = paren.pattern().expect("parenthesized literal");
        assert_eq!(lowered[1].span.range, inner.syntax().text_range());
        let ast::Pattern::Parenthesized(outer) = &patterns[2] else {
            panic!("third pattern is parenthesized");
        };
        let ast::Pattern::Parenthesized(inner) = outer.pattern().expect("inner parentheses") else {
            panic!("third pattern has nested parentheses");
        };
        let wildcard = inner.pattern().expect("nested wildcard");
        assert_eq!(lowered[2].span.range, wildcard.syntax().text_range());
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 4);
    }

    #[test]
    fn lowers_open_inclusive_literal_and_constant_path_range_patterns() {
        let file = crate::FileId::new(66);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() { match value { 1..=4 => (), ..=8 => (), 9.. => (), 'a'..'z' => (), b'A'..=b'Z' => (), 1..2 => (), -2..=-1 => (), 1.5..=pkg.Status.High => (), pkg.Status.Low..=pkg.Status.High => () } }\n",
        );
        let function = function_node(&parsed.root);
        let ranges = function
            .descendants()
            .filter_map(ast::RangePat::cast)
            .collect::<Vec<_>>();
        assert_eq!(ranges.len(), 9);
        assert_eq!(
            parsed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == sai_diagnostics::Code::FloatPattern)
                .count(),
            1
        );

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let patterns = ranges
            .iter()
            .map(|range| ast::Pattern::cast(range.syntax().clone()).expect("range pattern variant"))
            .collect::<Vec<_>>();
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("literal range patterns lower");

        assert_eq!(
            lowered[0].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: false,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(4),
                    negated: false,
                })),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[1].kind,
            PatKind::Range {
                start: None,
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(8),
                    negated: false,
                })),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[2].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(9),
                    negated: false,
                })),
                end: None,
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[3].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Char('a'))),
                end: Some(PatRangeEnd::Lit(Lit::Char('z'))),
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[4].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Byte(b'A'))),
                end: Some(PatRangeEnd::Lit(Lit::Byte(b'Z'))),
                inclusive: true,
            }
        );
        assert_eq!(
            lowered[5].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: false,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(2),
                    negated: false,
                })),
                inclusive: false,
            }
        );
        assert_eq!(
            lowered[6].kind,
            PatKind::Range {
                start: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(2),
                    negated: true,
                })),
                end: Some(PatRangeEnd::Lit(Lit::Int {
                    value: Some(1),
                    negated: true,
                })),
                inclusive: true,
            }
        );
        assert_eq!(lowered[7].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        for (pattern, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(pattern.span.range, syntax.syntax().text_range());
            assert_eq!(pattern.span.file, file);
            assert_eq!(pattern.span.ctxt, SpanCtxt::Root);
        }
        let (start_path, end_path) = match lowered[8].kind {
            PatKind::Range {
                start: Some(PatRangeEnd::Path(start_path)),
                end: Some(PatRangeEnd::Path(end_path)),
                inclusive: true,
            } => (start_path, end_path),
            _ => panic!("constant-path range lowers to two HIR paths"),
        };
        let path_names = |path: Path<'_>| {
            path.segments
                .iter()
                .map(|segment| {
                    symbols
                        .resolve(segment.name.sym)
                        .expect("path segment symbol resolves")
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(path_names(start_path).join("."), "pkg.Status.Low");
        assert_eq!(path_names(end_path).join("."), "pkg.Status.High");
        assert_eq!(start_path.root, PathRoot::Plain);
        assert_eq!(end_path.root, PathRoot::Plain);
        assert_eq!(start_path.hir_id.local_id.raw(), 10);
        assert_eq!(start_path.segments[0].hir_id.local_id.raw(), 11);
        assert_eq!(end_path.hir_id.local_id.raw(), 14);
        assert_eq!(end_path.segments[0].hir_id.local_id.raw(), 15);

        let ast::RangePatEndpoint::ConstantPath(start_syntax) =
            ranges[8].start().expect("start path syntax")
        else {
            panic!("range starts with a constant path");
        };
        let ast::RangePatEndpoint::ConstantPath(end_syntax) =
            ranges[8].end().expect("end path syntax")
        else {
            panic!("range ends with a constant path");
        };

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 18);
        assert_eq!(
            map.syntax_for_node(start_path.hir_id)
                .expect("start path source mapping")
                .resolve(file, &parsed.root)
                .expect("start path syntax resolves")
                .text_range(),
            start_syntax.syntax().text_range()
        );
        assert_eq!(
            map.syntax_for_node(end_path.segments[0].hir_id)
                .expect("end path segment source mapping")
                .resolve(file, &parsed.root)
                .expect("end path segment syntax resolves")
                .text_range(),
            end_syntax
                .segments()
                .next()
                .expect("first end path segment")
                .syntax()
                .text_range()
        );
    }

    #[test]
    fn lowers_binding_modes_at_patterns_and_normalized_names() {
        let file = crate::FileId::new(67);
        let source = "fn f() { match value { item => (), var mutable => (), borrow borrowed => (), borrow mut mutable_ref => (), named @ _ => (), outer @ (inner) => (), null => (), r#null => (), r#self => (), cafe\u{301} => (), r#Self => (), r#Self..=4 => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::NoNull)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidRawIdent)
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::IdentNotNfc)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 12);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("binding patterns lower");

        let modes = lowered[..4]
            .iter()
            .map(|pattern| match pattern.kind {
                PatKind::Binding { mode, .. } => mode,
                _ => panic!("simple binding pattern lowers to PatKind::Binding"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            [
                HirBindingMode::Value,
                HirBindingMode::Var,
                HirBindingMode::Borrow,
                HirBindingMode::BorrowMut,
            ]
        );
        fn binding_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (Name, crate::BindingId, Option<&'hir Pat<'hir>>) {
            match pattern.kind {
                PatKind::Binding {
                    name, binding, sub, ..
                } => (name, binding, sub),
                _ => panic!("binding pattern lowers to PatKind::Binding"),
            }
        }
        assert_eq!(
            ["item", "mutable", "borrowed", "mutable_ref"]
                .into_iter()
                .enumerate()
                .map(|(index, expected)| {
                    symbols
                        .resolve(binding_parts(lowered[index]).0.sym)
                        .expect("binding symbol resolves")
                        .to_string()
                        == expected
                })
                .collect::<Vec<_>>(),
            [true, true, true, true]
        );
        let (named, named_binding, named_sub) = binding_parts(lowered[4]);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 4);
        assert_eq!(
            named_sub.expect("at pattern has subpattern").kind,
            PatKind::Wild
        );
        let (outer, outer_binding, outer_sub) = binding_parts(lowered[5]);
        assert_eq!(symbols.resolve(outer.sym).unwrap().as_ref(), "outer");
        assert_eq!(outer_binding.raw(), 5);
        let inner = outer_sub.expect("at pattern has subpattern");
        let (inner_name, inner_binding, inner_sub) = binding_parts(inner);
        assert_eq!(symbols.resolve(inner_name.sym).unwrap().as_ref(), "inner");
        assert_eq!(inner_binding.raw(), 6);
        assert_eq!(inner_sub, None);
        assert_eq!(
            named_sub
                .expect("at pattern has subpattern")
                .hir_id
                .local_id
                .raw(),
            6
        );
        assert_eq!(inner.hir_id.local_id.raw(), 8);
        assert!(binding_parts(lowered[6]).0.sym.is_error());
        assert_eq!(
            symbols
                .resolve(binding_parts(lowered[7]).0.sym)
                .unwrap()
                .as_ref(),
            "null"
        );
        assert!(binding_parts(lowered[8]).0.sym.is_error());
        assert_eq!(
            symbols
                .resolve(binding_parts(lowered[9]).0.sym)
                .unwrap()
                .as_ref(),
            "café"
        );
        assert_eq!(lowered[10].kind, PatKind::Err);
        assert_eq!(lowered[11].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 7, 9, 10, 11, 12, 13, 14]
        );
        let ast::Pattern::At(outer_at) = &patterns[5] else {
            panic!("sixth pattern is an at pattern");
        };
        let name_syntax = outer_at
            .binding()
            .and_then(|binding| binding.name())
            .expect("at pattern name");
        assert_eq!(lowered[5].span.range, outer_at.syntax().text_range());
        assert_eq!(
            binding_parts(lowered[5]).0.span.range,
            name_syntax.syntax().text_range()
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 15);
        assert_eq!(map.binding_to_syntax().len(), 11);

        fn collect_bindings(pattern: &ast::Pattern, bindings: &mut Vec<ast::BindingPat>) {
            match pattern {
                ast::Pattern::Binding(binding) => bindings.push(binding.clone()),
                ast::Pattern::At(at) => {
                    if let Some(binding) = at.binding() {
                        bindings.push(binding);
                    }
                    if let Some(inner) = at.pattern() {
                        collect_bindings(&inner, bindings);
                    }
                }
                ast::Pattern::Parenthesized(paren) => {
                    if let Some(inner) = paren.pattern() {
                        collect_bindings(&inner, bindings);
                    }
                }
                _ => {}
            }
        }
        let mut expected_bindings = Vec::new();
        for pattern in &patterns {
            collect_bindings(pattern, &mut expected_bindings);
        }
        assert_eq!(expected_bindings.len(), map.binding_to_syntax().len());
        for (expected_id, ((binding_id, pointer), expected)) in map
            .binding_to_syntax()
            .iter_enumerated()
            .zip(expected_bindings)
            .enumerate()
        {
            assert_eq!(binding_id.raw() as usize, expected_id);
            assert_eq!(
                pointer
                    .resolve(file, &parsed.root)
                    .expect("binding pointer resolves")
                    .text_range(),
                expected.syntax().text_range()
            );
        }
    }

    #[test]
    fn lowers_or_patterns_shares_bindings_and_reports_mismatches_and_duplicates() {
        let file = crate::FileId::new(69);
        let source = "fn f() { match value { Pair(first, second) | Pair(second, first) => (), Pair(var mode) | Pair(mode) => (), Pair(left) | Pair(right) => (), Pair(duplicate, duplicate) => (), ((nested | nested) | nested) => (), | leading | leading => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 6);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("or-patterns lower");

        fn alternatives<'hir>(pattern: &Pat<'hir>) -> &'hir [Pat<'hir>] {
            match pattern.kind {
                PatKind::Or(alternatives) => alternatives,
                _ => panic!("or-pattern lowers to PatKind::Or"),
            }
        }
        fn binding(pattern: &Pat<'_>) -> (Symbol, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name.sym, binding, mode),
                _ => panic!("binding pattern lowers to PatKind::Binding"),
            }
        }
        fn constructor_args<'hir>(pattern: &Pat<'hir>) -> &'hir [PatArg<'hir>] {
            match pattern.kind {
                PatKind::Ctor {
                    args: Some(args), ..
                } => args,
                _ => panic!("constructor pattern has arguments"),
            }
        }

        let valid_alternatives = alternatives(lowered[0]);
        assert_eq!(valid_alternatives.len(), 2);
        let first_args = constructor_args(&valid_alternatives[0]);
        let second_args = constructor_args(&valid_alternatives[1]);
        assert_eq!(binding(first_args[0].pat).1.raw(), 0);
        assert_eq!(binding(first_args[1].pat).1.raw(), 1);
        assert_eq!(binding(second_args[0].pat).1.raw(), 1);
        assert_eq!(binding(second_args[1].pat).1.raw(), 0);

        let mode_alternatives = alternatives(lowered[1]);
        assert_eq!(
            binding(constructor_args(&mode_alternatives[0])[0].pat)
                .1
                .raw(),
            2
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[1])[0].pat)
                .1
                .raw(),
            2
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[0])[0].pat).2,
            HirBindingMode::Var
        );
        assert_eq!(
            binding(constructor_args(&mode_alternatives[1])[0].pat).2,
            HirBindingMode::Value
        );

        let name_alternatives = alternatives(lowered[2]);
        assert_eq!(
            binding(constructor_args(&name_alternatives[0])[0].pat)
                .1
                .raw(),
            3
        );
        assert_eq!(
            binding(constructor_args(&name_alternatives[1])[0].pat)
                .1
                .raw(),
            4
        );

        let duplicate_args = constructor_args(lowered[3]);
        assert_eq!(binding(duplicate_args[0].pat).1.raw(), 5);
        assert_eq!(binding(duplicate_args[1].pat).1.raw(), 6);

        let nested_alternatives = alternatives(lowered[4]);
        assert_eq!(nested_alternatives.len(), 3);
        assert_eq!(
            nested_alternatives
                .iter()
                .map(|pattern| binding(pattern).1.raw())
                .collect::<Vec<_>>(),
            [7, 7, 7]
        );

        let leading_alternatives = alternatives(lowered[5]);
        assert_eq!(leading_alternatives.len(), 2);
        assert_eq!(
            leading_alternatives
                .iter()
                .map(|pattern| binding(pattern).1.raw())
                .collect::<Vec<_>>(),
            [8, 8]
        );

        let map = source_map.finish();
        assert_eq!(map.binding_to_syntax().len(), 9);
        let duplicate_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 1);
        let duplicate_binding = ast::BindingPat::cast(
            patterns[3]
                .syntax()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::BINDING_PAT)
                .nth(1)
                .expect("second duplicate binding")
                .clone(),
        )
        .expect("binding syntax");
        assert_eq!(
            duplicate_diagnostics[0]
                .primary()
                .expect("duplicate diagnostic location")
                .span
                .range,
            duplicate_binding.syntax().text_range()
        );

        let mismatch_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::OrPatternBindingMismatch)
            .collect::<Vec<_>>();
        assert_eq!(mismatch_diagnostics.len(), 2);
        for ((diagnostic, pattern), expected_name) in mismatch_diagnostics
            .into_iter()
            .zip([&patterns[1], &patterns[2]])
            .zip(["mode", "left"])
        {
            assert_eq!(diagnostic.severity, sai_diagnostics::Severity::Error);
            assert!(diagnostic.message.contains(&format!("`{expected_name}`")));
            assert_eq!(
                diagnostic
                    .primary()
                    .expect("mismatch diagnostic location")
                    .span
                    .range,
                pattern.syntax().text_range()
            );
        }

        let all_binding_syntax = function
            .descendants()
            .filter_map(ast::BindingPat::cast)
            .collect::<Vec<_>>();
        let canonical_binding_indexes = [0, 1, 4, 6, 7, 8, 9, 10, 13];
        for ((_, pointer), expected_index) in map
            .binding_to_syntax()
            .iter_enumerated()
            .zip(canonical_binding_indexes)
        {
            let expected =
                sai_syntax::SyntaxNodePtr::new(file, all_binding_syntax[expected_index].syntax())
                    .expect("expected binding pointer");
            assert_eq!(pointer, &expected);
        }
    }

    #[test]
    fn keeps_nested_or_aliases_together_when_reporting_duplicate_bindings() {
        let file = crate::FileId::new(70);
        let source = "fn f() { match value { Pair(existing, Pair(existing | existing)) | Pair(existing, Pair(existing | existing)) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let pattern = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .find_map(|arm| arm.pattern())
            .expect("match arm pattern");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_pattern(&pattern, &arena, &symbols, &mut source_map)
            .expect("nested or-pattern lowers");
        let PatKind::Or(outer_alternatives) = lowered.kind else {
            panic!("outer pattern lowers to an or-pattern");
        };
        assert_eq!(outer_alternatives.len(), 2);
        let nested_binding_ids = outer_alternatives
            .iter()
            .map(|alternative| {
                let PatKind::Ctor {
                    args: Some(args), ..
                } = alternative.kind
                else {
                    panic!("outer alternative is a constructor");
                };
                let PatKind::Ctor {
                    args: Some(nested_args),
                    ..
                } = args[1].pat.kind
                else {
                    panic!("second constructor is nested");
                };
                let PatKind::Or(inner_alternatives) = nested_args[0].pat.kind else {
                    panic!("nested constructor argument is an or-pattern");
                };
                inner_alternatives
                    .iter()
                    .map(|pattern| match pattern.kind {
                        PatKind::Binding { binding, .. } => binding.raw(),
                        _ => panic!("nested or alternative is a binding"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(nested_binding_ids[0][0], nested_binding_ids[0][1]);
        assert_eq!(nested_binding_ids[1][0], nested_binding_ids[1][1]);

        let map = source_map.finish();
        assert_eq!(
            map.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
                .count(),
            2
        );
    }

    #[test]
    fn checks_bindings_from_later_or_alternatives_against_sibling_patterns() {
        let file = crate::FileId::new(71);
        let source = "fn f() { match value { (left | right | right, right) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let pattern = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .find_map(|arm| arm.pattern())
            .expect("match arm pattern");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = lower_pattern(&pattern, &arena, &symbols, &mut source_map)
            .expect("tuple with an or-pattern lowers");
        let PatKind::Tuple(elements) = lowered.kind else {
            panic!("parenthesized comma pattern lowers to a tuple");
        };
        let PatKind::Or(alternatives) = elements[0].kind else {
            panic!("first tuple element is an or-pattern");
        };
        let alternative_bindings = alternatives
            .iter()
            .map(|pattern| match pattern.kind {
                PatKind::Binding { binding, .. } => binding.raw(),
                _ => panic!("or alternative is a binding"),
            })
            .collect::<Vec<_>>();
        assert_eq!(alternative_bindings, [0, 1, 1]);
        let PatKind::Binding {
            binding: trailing_binding,
            ..
        } = elements[1].kind
        else {
            panic!("second tuple element is a binding");
        };
        assert_eq!(trailing_binding.raw(), 2);

        let map = source_map.finish();
        let duplicate_diagnostics = map
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::DuplicateBinding)
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 1);
        assert_eq!(map.binding_to_syntax().len(), 3);
        assert_eq!(
            duplicate_diagnostics[0]
                .primary()
                .expect("duplicate diagnostic location")
                .span
                .range,
            elements[1].span.range
        );
        let binding_syntaxes = function
            .descendants()
            .filter_map(ast::BindingPat::cast)
            .collect::<Vec<_>>();
        for ((_, pointer), expected_index) in
            map.binding_to_syntax().iter_enumerated().zip([0, 1, 3])
        {
            let expected =
                sai_syntax::SyntaxNodePtr::new(file, binding_syntaxes[expected_index].syntax())
                    .expect("expected binding pointer");
            assert_eq!(pointer, &expected);
        }
        assert_eq!(
            map.diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::OrPatternBindingMismatch)
                .count(),
            1
        );
    }

    #[test]
    fn lowers_tuple_patterns_and_preserves_nested_bindings() {
        let file = crate::FileId::new(68);
        let source = "fn f() { match value { () => (), (only,) => (), (first, (second,), var third,) => () } }";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 3);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..3]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("tuple patterns lower");

        fn tuple_elements<'hir>(pattern: &Pat<'hir>) -> &'hir [Pat<'hir>] {
            match pattern.kind {
                PatKind::Tuple(elements) => elements,
                _ => panic!("tuple pattern lowers to PatKind::Tuple"),
            }
        }
        assert!(tuple_elements(lowered[0]).is_empty());
        let singleton = tuple_elements(lowered[1]);
        assert_eq!(singleton.len(), 1);
        let binding_parts = |pattern: &Pat<'_>| match pattern.kind {
            PatKind::Binding {
                name,
                binding,
                mode,
                ..
            } => (name, binding, mode),
            _ => panic!("tuple element lowers to PatKind::Binding"),
        };
        let (only_name, only_binding, _) = binding_parts(&singleton[0]);
        assert_eq!(symbols.resolve(only_name.sym).unwrap().as_ref(), "only");
        assert_eq!(only_binding.raw(), 0);

        let nested = tuple_elements(lowered[2]);
        assert_eq!(nested.len(), 3);
        let (first_name, first_binding, _) = binding_parts(&nested[0]);
        assert_eq!(symbols.resolve(first_name.sym).unwrap().as_ref(), "first");
        assert_eq!(first_binding.raw(), 1);
        let second = tuple_elements(&nested[1]);
        assert_eq!(second.len(), 1);
        let (second_name, second_binding, _) = binding_parts(&second[0]);
        assert_eq!(symbols.resolve(second_name.sym).unwrap().as_ref(), "second");
        assert_eq!(second_binding.raw(), 2);
        let (third_name, third_binding, third_mode) = binding_parts(&nested[2]);
        assert_eq!(symbols.resolve(third_name.sym).unwrap().as_ref(), "third");
        assert_eq!(third_binding.raw(), 3);
        assert_eq!(third_mode, HirBindingMode::Var);

        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(lowered[0].span.range, patterns[0].syntax().text_range());
        assert_eq!(
            singleton[0].span.range,
            patterns[1]
                .syntax()
                .descendants()
                .find(|node| node.kind() == SyntaxKind::BINDING_PAT)
                .expect("singleton binding syntax")
                .text_range()
        );
        assert_eq!(
            second[0].hir_id.local_id.raw(),
            7,
            "nested tuple children receive owner-local IDs in source pre-order"
        );

        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 9);
        assert_eq!(map.binding_to_syntax().len(), 4);
    }

    #[test]
    fn lowers_slice_patterns_and_recovers_duplicate_rest_patterns() {
        let file = crate::FileId::new(69);
        let source = concat!(
            "fn f() { match value { ",
            "[] => (), ",
            "[first, .., last] => (), ",
            "[..borrow mut rest, after] => (), ",
            "[before, ..var tail] => (), ",
            "[.., ..] => (), ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::MultipleRestPatterns)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 5);
        let ast::Pattern::Slice(duplicate_rest) = &patterns[4] else {
            panic!("fifth pattern is a slice");
        };
        assert_eq!(duplicate_rest.rest_patterns().count(), 2);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..5]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("slice patterns lower");

        fn slice_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (
            &'hir [Pat<'hir>],
            Option<Option<&'hir Pat<'hir>>>,
            &'hir [Pat<'hir>],
        ) {
            match pattern.kind {
                PatKind::Slice {
                    before,
                    rest,
                    after,
                } => (before, rest, after),
                _ => panic!("slice pattern lowers to PatKind::Slice"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("slice element lowers to PatKind::Binding"),
            }
        }

        assert_eq!(slice_parts(lowered[0]), (&[][..], None, &[][..]));
        let (before, rest, after) = slice_parts(lowered[1]);
        assert_eq!(before.len(), 1);
        assert_eq!(rest, Some(None));
        assert_eq!(after.len(), 1);
        assert_eq!(
            symbols
                .resolve(binding_parts(&before[0]).0.sym)
                .unwrap()
                .as_ref(),
            "first"
        );
        let (_, first_binding, _) = binding_parts(&before[0]);
        assert_eq!(first_binding.raw(), 0);
        let (_, last_binding, _) = binding_parts(&after[0]);
        assert_eq!(last_binding.raw(), 1);
        assert_eq!(
            symbols
                .resolve(binding_parts(&after[0]).0.sym)
                .unwrap()
                .as_ref(),
            "last"
        );
        assert_eq!(before[0].hir_id.local_id.raw(), 3);
        assert_eq!(after[0].hir_id.local_id.raw(), 4);

        let (before, rest, after) = slice_parts(lowered[2]);
        assert!(before.is_empty());
        let rest = rest
            .expect("bound rest is present")
            .expect("rest binds a name");
        let (rest_name, rest_binding, rest_mode) = binding_parts(rest);
        assert_eq!(symbols.resolve(rest_name.sym).unwrap().as_ref(), "rest");
        assert_eq!(rest_binding.raw(), 2);
        assert_eq!(rest_mode, HirBindingMode::BorrowMut);
        assert_eq!(after.len(), 1);
        let (_, after_binding, _) = binding_parts(&after[0]);
        assert_eq!(after_binding.raw(), 3);
        assert_eq!(
            symbols
                .resolve(binding_parts(&after[0]).0.sym)
                .unwrap()
                .as_ref(),
            "after"
        );
        assert_eq!(rest.hir_id.local_id.raw(), 6);
        assert_eq!(after[0].hir_id.local_id.raw(), 7);

        let (before, rest, after) = slice_parts(lowered[3]);
        assert_eq!(before.len(), 1);
        let tail = rest
            .expect("bound rest is present")
            .expect("rest binds a name");
        let (tail_name, tail_binding, tail_mode) = binding_parts(tail);
        assert_eq!(symbols.resolve(tail_name.sym).unwrap().as_ref(), "tail");
        assert_eq!(tail_binding.raw(), 5);
        assert_eq!(tail_mode, HirBindingMode::Var);
        assert!(after.is_empty());
        let (_, before_binding, _) = binding_parts(&before[0]);
        assert_eq!(before_binding.raw(), 4);
        assert_eq!(
            symbols
                .resolve(binding_parts(&before[0]).0.sym)
                .unwrap()
                .as_ref(),
            "before"
        );
        assert_eq!(before[0].hir_id.local_id.raw(), 9);
        assert_eq!(tail.hir_id.local_id.raw(), 10);

        assert_eq!(lowered[4].kind, PatKind::Err);
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 5, 8, 11]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(lowered[1].span.range, patterns[1].syntax().text_range());
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 12);
        assert_eq!(map.binding_to_syntax().len(), 6);
    }

    #[test]
    fn lowers_implicit_member_patterns_and_preserves_argument_order() {
        let file = crate::FileId::new(70);
        let source = concat!(
            "fn f() { match value { ",
            ".Some => (), ",
            ".Some() => (), ",
            ".Some(..) => (), ",
            ".Some(field: var named, positional, ..) => (), ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 4);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..4]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("implicit member patterns lower");

        fn implicit_parts<'hir>(pattern: &Pat<'hir>) -> (Name, Option<&'hir [PatArg<'hir>]>, bool) {
            match pattern.kind {
                PatKind::ImplicitMember {
                    name,
                    args,
                    has_rest,
                } => (name, args, has_rest),
                _ => panic!("implicit member lowers to PatKind::ImplicitMember"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("argument pattern lowers to PatKind::Binding"),
            }
        }

        let (name, args, has_rest) = implicit_parts(lowered[0]);
        assert_eq!(symbols.resolve(name.sym).unwrap().as_ref(), "Some");
        assert!(args.is_none());
        assert!(!has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[1]);
        assert_eq!(args.expect("explicit empty args"), &[]);
        assert!(!has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[2]);
        assert_eq!(args.expect("explicit rest args"), &[]);
        assert!(has_rest);
        let (_, args, has_rest) = implicit_parts(lowered[3]);
        let args = args.expect("explicit arguments");
        assert_eq!(args.len(), 2);
        assert!(has_rest);
        assert_eq!(
            symbols
                .resolve(args[0].name.expect("named argument").sym)
                .unwrap()
                .as_ref(),
            "field"
        );
        assert_eq!(args[1].name, None);
        let (named, named_binding, named_mode) = binding_parts(args[0].pat);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 0);
        assert_eq!(named_mode, HirBindingMode::Var);
        let (positional, positional_binding, positional_mode) = binding_parts(args[1].pat);
        assert_eq!(
            symbols.resolve(positional.sym).unwrap().as_ref(),
            "positional"
        );
        assert_eq!(positional_binding.raw(), 1);
        assert_eq!(positional_mode, HirBindingMode::Value);
        let ast::Pattern::ImplicitMember(mixed) = &patterns[3] else {
            panic!("fourth pattern is an implicit member");
        };
        let named_arg_syntax = mixed
            .args()
            .and_then(|args| args.named_args().next())
            .expect("named argument syntax");
        assert_eq!(
            args[0].name.expect("named argument").span.range,
            named_arg_syntax
                .name()
                .expect("named argument name")
                .syntax()
                .text_range()
        );
        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        assert_eq!(args[0].pat.hir_id.local_id.raw(), 5);
        assert_eq!(args[1].pat.hir_id.local_id.raw(), 6);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 7);
        assert_eq!(map.binding_to_syntax().len(), 2);
    }

    #[test]
    fn lowers_constructor_paths_arguments_and_source_mappings() {
        let file = crate::FileId::new(71);
        let source = concat!(
            "fn f() { match value { ",
            "Some => (), ",
            "Some() => (), ",
            "http.Status.Ok(code: var named, payload, ..) => (), ",
            "app.Result.Left(.Ready) => (), ",
            "r#Self() => () ",
            "} }",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == sai_diagnostics::Code::InvalidRawIdent)
        );
        let function = function_node(&parsed.root);
        let patterns = function
            .descendants()
            .filter_map(ast::MatchArm::cast)
            .filter_map(|arm| arm.pattern())
            .collect::<Vec<_>>();
        assert_eq!(patterns.len(), 5);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");
        let lowered = patterns[..4]
            .iter()
            .map(|pattern| lower_pattern(pattern, &arena, &symbols, &mut source_map))
            .collect::<Result<Vec<_>, _>>()
            .expect("constructor patterns lower");
        let invalid_raw = lower_pattern(&patterns[4], &arena, &symbols, &mut source_map)
            .expect("invalid raw constructor lowers to an error pattern");

        fn ctor_parts<'hir>(
            pattern: &Pat<'hir>,
        ) -> (Path<'hir>, Option<&'hir [PatArg<'hir>]>, bool) {
            match pattern.kind {
                PatKind::Ctor {
                    path,
                    args,
                    has_rest,
                } => (path, args, has_rest),
                _ => panic!("constructor pattern lowers to PatKind::Ctor"),
            }
        }
        fn binding_parts(pattern: &Pat<'_>) -> (Name, crate::BindingId, HirBindingMode) {
            match pattern.kind {
                PatKind::Binding {
                    name,
                    binding,
                    mode,
                    ..
                } => (name, binding, mode),
                _ => panic!("constructor argument lowers to PatKind::Binding"),
            }
        }
        fn path_names<'hir>(path: &Path<'hir>, symbols: &Interner) -> Vec<String> {
            path.segments
                .iter()
                .map(|segment| {
                    symbols
                        .resolve(segment.name.sym)
                        .expect("path segment symbol resolves")
                        .to_string()
                })
                .collect()
        }

        let (first_path, args, has_rest) = ctor_parts(lowered[0]);
        assert_eq!(first_path.root, PathRoot::Plain);
        assert_eq!(path_names(&first_path, &symbols).join("."), "Some");
        assert!(args.is_none());
        assert!(!has_rest);

        let (path, args, has_rest) = ctor_parts(lowered[1]);
        assert_eq!(path_names(&path, &symbols).join("."), "Some");
        assert_eq!(args.expect("explicit empty args"), &[]);
        assert!(!has_rest);

        let (qualified_path, args, has_rest) = ctor_parts(lowered[2]);
        assert_eq!(
            path_names(&qualified_path, &symbols).join("."),
            "http.Status.Ok"
        );
        let args = args.expect("explicit constructor arguments");
        assert_eq!(args.len(), 2);
        assert!(has_rest);
        assert_eq!(
            symbols
                .resolve(args[0].name.expect("named argument").sym)
                .unwrap()
                .as_ref(),
            "code"
        );
        assert_eq!(args[1].name, None);
        let (named, named_binding, named_mode) = binding_parts(args[0].pat);
        assert_eq!(symbols.resolve(named.sym).unwrap().as_ref(), "named");
        assert_eq!(named_binding.raw(), 0);
        assert_eq!(named_mode, HirBindingMode::Var);
        let (positional, positional_binding, positional_mode) = binding_parts(args[1].pat);
        assert_eq!(symbols.resolve(positional.sym).unwrap().as_ref(), "payload");
        assert_eq!(positional_binding.raw(), 1);
        assert_eq!(positional_mode, HirBindingMode::Value);

        let (nested_path, args, has_rest) = ctor_parts(lowered[3]);
        assert_eq!(
            path_names(&nested_path, &symbols).join("."),
            "app.Result.Left"
        );
        let args = args.expect("nested implicit-member argument");
        assert_eq!(args.len(), 1);
        assert!(!has_rest);
        assert!(matches!(
            args[0].pat.kind,
            PatKind::ImplicitMember { name, .. }
                if symbols.resolve(name.sym).unwrap().as_ref() == "Ready"
        ));
        assert_eq!(invalid_raw.kind, PatKind::Err);
        assert_eq!(
            invalid_raw.hir_id.local_id.raw(),
            20,
            "unsupported nested patterns are rejected before allocating the constructor"
        );

        assert_eq!(
            lowered
                .iter()
                .map(|pattern| pattern.hir_id.local_id.raw())
                .collect::<Vec<_>>(),
            [1, 4, 7, 14]
        );
        for (lowered, syntax) in lowered.iter().zip(&patterns[..4]) {
            assert_eq!(lowered.span.range, syntax.syntax().text_range());
            assert_eq!(lowered.span.ctxt, SpanCtxt::Root);
        }
        let ast::Pattern::Constructor(qualified_syntax) = &patterns[2] else {
            panic!("third arm is a constructor");
        };
        let syntax_path = qualified_syntax.path().expect("qualified constructor path");
        assert_eq!(qualified_path.span.range, syntax_path.syntax().text_range());
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 21);
        assert_eq!(map.binding_to_syntax().len(), 2);
        assert_eq!(
            map.syntax_for_node(qualified_path.hir_id)
                .expect("path source mapping")
                .resolve(file, &parsed.root)
                .expect("path syntax resolves")
                .text_range(),
            syntax_path.syntax().text_range()
        );
        assert_eq!(
            map.syntax_for_node(qualified_path.segments[0].hir_id)
                .expect("path segment source mapping")
                .resolve(file, &parsed.root)
                .expect("segment syntax resolves")
                .text_range(),
            syntax_path
                .segments()
                .next()
                .expect("first path segment")
                .syntax()
                .text_range()
        );
    }

    #[test]
    fn lowers_closures_with_lexical_paths_parameters_and_source_mappings() {
        let file = crate::FileId::new(201);
        let source = "fn f() {\n    move fn(var x: I32, _: U8) -> I32 { fn(y) { x } }\n    async {}\n    fn() {}\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let closures = function
            .descendants()
            .filter_map(ast::ClosureExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 3);
        let async_block = function
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ASYNC_BLOCK_EXPR)
            .expect("async block contributes to nested-definition preorder");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let outer_expr = ast::Expr::cast(closures[0].syntax().clone()).unwrap();
        let outer = lower_expr(&outer_expr, &arena, &symbols, &mut source_map)
            .expect("outer closure lowers");
        let outer_hir_id = outer.hir_id;
        let outer_span = outer.span;
        let ExprKind::Closure(outer) = outer.kind else {
            panic!("closure expression is preserved");
        };
        assert_eq!(outer.capture, CaptureBy::Move);
        assert_eq!(outer.params.len(), 2);
        assert!(outer.params[0].mutable);
        assert_eq!(
            symbols
                .resolve(outer.params[0].name.expect("named parameter").sym)
                .unwrap()
                .as_ref(),
            "x"
        );
        assert!(!outer.params[1].mutable);
        assert!(outer.params[1].name.is_none(), "underscore is a wildcard");
        assert!(outer.params[0].ty.is_some());
        assert!(outer.params[1].ty.is_some());
        assert!(outer.ret.is_some());
        assert_eq!(outer.body.safety, Safety::Safe);

        let nested = outer
            .body
            .tail
            .expect("nested closure is the outer closure's tail");
        let nested_hir_id = nested.hir_id;
        let ExprKind::Closure(nested) = nested.kind else {
            panic!("nested closure is preserved");
        };
        assert_eq!(nested.capture, CaptureBy::Infer);
        assert_eq!(nested.params.len(), 1);
        assert_eq!(
            symbols
                .resolve(nested.params[0].name.expect("nested parameter").sym)
                .unwrap()
                .as_ref(),
            "y"
        );

        let sibling_expr = ast::Expr::cast(closures[2].syntax().clone()).unwrap();
        let sibling = lower_expr(&sibling_expr, &arena, &symbols, &mut source_map)
            .expect("sibling closure lowers");
        let ExprKind::Closure(sibling) = sibling.kind else {
            panic!("sibling closure is preserved");
        };
        assert_eq!(sibling.capture, CaptureBy::Infer);
        assert!(sibling.params.is_empty());

        let outer_path = paths.resolve(outer.def_id).unwrap().unwrap();
        assert_eq!(
            outer_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            })
        );
        let nested_path = paths.resolve(nested.def_id).unwrap().unwrap();
        assert_eq!(
            nested_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 1,
            })
        );
        assert_eq!(
            nested_path.data()[nested_path.data().len() - 2],
            DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            }
        );
        let sibling_path = paths.resolve(sibling.def_id).unwrap().unwrap();
        assert_eq!(
            sibling_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 3,
            })
        );
        let async_id = source_map
            .nested_definition_for_syntax(&async_block)
            .unwrap()
            .expect("async block path was assigned");
        assert_eq!(
            paths.resolve(async_id).unwrap().unwrap().data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::AsyncBlock,
                disambiguator: 2,
            })
        );

        let outer_ptr =
            SyntaxNodePtr::new(file, closures[0].syntax()).expect("outer closure pointer");
        let outer_ast_params = closures[0].params().unwrap().params().collect::<Vec<_>>();
        let first_param_ptr =
            SyntaxNodePtr::new(file, outer_ast_params[0].name().unwrap().syntax())
                .expect("first parameter name pointer");
        let wildcard_ptr = SyntaxNodePtr::new(file, outer_ast_params[1].name().unwrap().syntax())
            .expect("wildcard parameter name pointer");
        let first_param_node_ptr =
            SyntaxNodePtr::new(file, outer_ast_params[0].syntax()).expect("parameter pointer");
        let first_param_type_ptr =
            SyntaxNodePtr::new(file, outer_ast_params[0].ty().unwrap().syntax())
                .expect("parameter type pointer");
        let ret_type_ptr =
            SyntaxNodePtr::new(file, closures[0].ret_type().unwrap().ty().unwrap().syntax())
                .expect("closure return type pointer");
        let body_ptr =
            SyntaxNodePtr::new(file, closures[0].body().unwrap().syntax()).expect("body pointer");
        let nested_ptr =
            SyntaxNodePtr::new(file, closures[1].syntax()).expect("nested closure pointer");
        let map = source_map.finish();
        assert_eq!(map.syntax_for_node(outer_hir_id), Some(&outer_ptr));
        assert_eq!(
            map.syntax_for_node(outer.params[0].hir_id)
                .expect("first parameter HIR source"),
            &first_param_node_ptr
        );
        assert_eq!(
            map.syntax_for_node(outer.params[0].ty.unwrap().hir_id),
            Some(&first_param_type_ptr)
        );
        assert_eq!(
            map.syntax_for_node(outer.ret.unwrap().hir_id),
            Some(&ret_type_ptr)
        );
        assert_eq!(map.syntax_for_node(outer.body.hir_id), Some(&body_ptr));
        assert_eq!(
            map.syntax_for_binding(outer.params[0].binding),
            Some(&first_param_ptr)
        );
        assert_eq!(
            map.syntax_for_binding(outer.params[1].binding),
            Some(&wildcard_ptr)
        );
        assert_eq!(map.syntax_for_node(nested_hir_id), Some(&nested_ptr));
        assert_eq!(outer_span.range, closures[0].syntax().text_range());
        assert_eq!(outer_span.ctxt, SpanCtxt::Root);
        assert_eq!(
            outer.params[0].name.unwrap().span.range,
            outer_ast_params[0].name().unwrap().syntax().text_range()
        );
        assert_eq!(outer.params[0].name.unwrap().span.ctxt, SpanCtxt::Root);
    }

    #[test]
    fn lowers_async_blocks_with_capture_paths_and_source_mappings() {
        let file = crate::FileId::new(208);
        let source =
            "fn f() {\n    async { 1 }\n    async move { 2 }\n    fn() { async { 3 } }\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let async_blocks = function
            .descendants()
            .filter_map(ast::AsyncBlockExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(async_blocks.len(), 3);
        let closure = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure with a nested async block");

        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);
        let mut lowered = Vec::new();
        for async_block in &async_blocks {
            let expression = ast::Expr::cast(async_block.syntax().clone()).unwrap();
            lowered.push(
                lower_expr(&expression, &arena, &symbols, &mut source_map)
                    .expect("async block lowers"),
            );
        }
        let closure_expression = ast::Expr::cast(closure.syntax().clone()).unwrap();
        let lowered_closure = lower_expr(&closure_expression, &arena, &symbols, &mut source_map)
            .expect("closure containing an async block lowers");
        let ExprKind::Closure(lowered_closure) = lowered_closure.kind else {
            panic!("the closure remains a closure");
        };
        let nested_async = lowered_closure.body.tail.expect("nested async block");

        let ExprKind::AsyncBlock {
            def_id: first_def_id,
            capture: CaptureBy::Infer,
            block: first_body,
        } = lowered[0].kind
        else {
            panic!("plain async block infers its capture mode");
        };
        let ExprKind::AsyncBlock {
            def_id: second_def_id,
            capture: CaptureBy::Move,
            block: second_body,
        } = lowered[1].kind
        else {
            panic!("move async block captures by move");
        };
        assert!(matches!(
            first_body.tail.map(|tail| tail.kind),
            Some(ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            }))
        ));
        assert!(matches!(
            second_body.tail.map(|tail| tail.kind),
            Some(ExprKind::Lit(Lit::Int {
                value: Some(2),
                negated: false,
            }))
        ));
        assert_eq!(first_body.safety, Safety::Safe);
        assert_eq!(second_body.safety, Safety::Safe);

        for (ordinal, def_id) in [(0, first_def_id), (1, second_def_id)] {
            let path = paths.resolve(def_id).unwrap().unwrap();
            assert_eq!(
                path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::AsyncBlock,
                    disambiguator: ordinal,
                })
            );
        }
        let ExprKind::AsyncBlock {
            def_id: nested_def_id,
            capture: CaptureBy::Infer,
            ..
        } = nested_async.kind
        else {
            panic!("nested async block uses inferred capture");
        };
        let nested_path = paths.resolve(nested_def_id).unwrap().unwrap();
        assert_eq!(
            nested_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::AsyncBlock,
                disambiguator: 3,
            })
        );
        assert_eq!(
            nested_path.data()[nested_path.data().len() - 2],
            DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 2,
            }
        );
        let first_ptr =
            SyntaxNodePtr::new(file, async_blocks[0].syntax()).expect("first async pointer");
        let first_body_ptr = SyntaxNodePtr::new(file, async_blocks[0].body().unwrap().syntax())
            .expect("first async body pointer");
        let map = source_map.finish();
        assert_eq!(map.syntax_for_node(lowered[0].hir_id), Some(&first_ptr));
        assert_eq!(
            map.syntax_for_node(first_body.hir_id),
            Some(&first_body_ptr)
        );
    }

    #[test]
    fn async_block_lowering_requires_definition_paths_before_allocating_hir() {
        let file = crate::FileId::new(209);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { async {} }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let async_block = function
            .descendants()
            .find_map(ast::AsyncBlockExpr::cast)
            .expect("async block");
        let expression = ast::Expr::cast(async_block.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        assert_eq!(
            lower_expr(&expression, &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::ASYNC_BLOCK_EXPR)
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn async_block_lowers_generic_field_values_in_the_body() {
        let file = crate::FileId::new(210);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { async { value.convert<U32> } }\n");
        let function = function_node(&parsed.root);
        let async_block = function
            .descendants()
            .find_map(ast::AsyncBlockExpr::cast)
            .expect("async block");
        let expression = ast::Expr::cast(async_block.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("async block with a generic field lowers");
        let ExprKind::AsyncBlock { block, .. } = lowered.kind else {
            panic!("async block retains its HIR form");
        };
        assert!(matches!(
            block.tail.map(|tail| tail.kind),
            Some(ExprKind::Field { args: Some(args), .. }) if args.args.len() == 1
        ));
        assert!(source_map.finish().node_to_syntax().len() > 1);
        assert_eq!(paths.len().unwrap(), 2, "the async block path is interned");
    }

    #[test]
    fn closure_lowering_requires_definition_paths_before_allocating_hir() {
        let file = crate::FileId::new(202);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { fn() {} }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let closure = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure expression");
        let expression = ast::Expr::cast(closure.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        assert_eq!(
            lower_expr(&expression, &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::CLOSURE_EXPR)
        );
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 1);
        assert!(map.binding_to_syntax().is_empty());
    }

    #[test]
    fn closure_lowers_generic_field_values_in_the_body() {
        let file = crate::FileId::new(203);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { fn() { value.convert<U32> } }\n");
        let function = function_node(&parsed.root);
        let closure = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure expression");
        let expression = ast::Expr::cast(closure.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered =
            lower_expr(&expression, &arena, &symbols, &mut source_map).expect("closure lowers");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("the closure retains its HIR form");
        };
        assert!(matches!(
            closure.body.tail.map(|tail| tail.kind),
            Some(ExprKind::Field { args: Some(args), .. }) if args.args.len() == 1
        ));
        let map = source_map.finish();
        assert!(map.node_to_syntax().len() > 1);
        assert!(map.binding_to_syntax().is_empty());
        assert_eq!(paths.len().unwrap(), 2, "the closure path is interned");
    }

    #[test]
    fn closure_parameter_name_recovery_keeps_the_closure_body() {
        let file = crate::FileId::new(204);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { fn(var) { 7 } }\n");
        assert!(!parsed.errors.is_empty(), "the missing name is diagnosed");
        let function = function_node(&parsed.root);
        let closure = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure expression");
        let param = closure
            .params()
            .unwrap()
            .params()
            .next()
            .expect("recovered closure parameter");
        let expression = ast::Expr::cast(closure.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("closure recovers its diagnosed parameter name");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("the valid closure body is retained");
        };
        assert_eq!(closure.params.len(), 1);
        assert!(closure.params[0].mutable);
        assert_eq!(
            closure.params[0].name.map(|name| name.sym),
            Some(Symbol::ERROR)
        );
        assert!(matches!(
            closure.body.tail.map(|tail| tail.kind),
            Some(ExprKind::Lit(Lit::Int {
                value: Some(7),
                negated: false,
            }))
        ));

        let binding_syntax = param
            .name()
            .map(|name| name.syntax().clone())
            .or_else(|| direct_error_child(param.syntax()))
            .unwrap_or_else(|| param.syntax().clone());
        let binding_ptr =
            SyntaxNodePtr::new(file, &binding_syntax).expect("recovered binding pointer");
        let map = source_map.finish();
        assert_eq!(
            map.syntax_for_binding(closure.params[0].binding),
            Some(&binding_ptr)
        );
    }

    #[test]
    fn closure_parameter_separator_recovery_keeps_later_parameters_and_body() {
        let file = crate::FileId::new(206);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() { fn(x y: I32) { x } }\n");
        assert!(!parsed.errors.is_empty(), "the missing comma is diagnosed");
        let function = function_node(&parsed.root);
        let closure_syntax = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure expression");
        let params = closure_syntax
            .params()
            .unwrap()
            .params()
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 2);
        let expression = ast::Expr::cast(closure_syntax.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("closure recovers its missing parameter separator");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("the closure is preserved");
        };
        assert_eq!(closure.params.len(), 2);
        assert_eq!(
            symbols
                .resolve(closure.params[1].name.expect("second parameter").sym)
                .unwrap()
                .as_ref(),
            "y"
        );
        assert!(matches!(
            closure.body.tail.map(|tail| tail.kind),
            Some(ExprKind::Path(_))
        ));
        assert_eq!(source_map.finish().binding_to_syntax().len(), 2);
    }

    #[test]
    fn closure_parameter_and_body_recovery_compose() {
        let file = crate::FileId::new(207);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() { fn(x y: I32) { let value: = 1\n value } }\n");
        assert!(
            parsed.errors.len() >= 2,
            "the missing parameter separator and type annotation are diagnosed"
        );
        let function = function_node(&parsed.root);
        let closure_syntax = function
            .descendants()
            .find_map(ast::ClosureExpr::cast)
            .expect("closure expression");
        let expression = ast::Expr::cast(closure_syntax.syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("recoverable parameter and body errors compose");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("the closure is preserved");
        };
        assert_eq!(closure.params.len(), 2);
        assert_eq!(closure.body.stmts.len(), 1);
        let StmtKind::Let { ty, init, .. } = closure.body.stmts[0].kind else {
            panic!("the closure body keeps its recoverable let statement");
        };
        assert!(matches!(ty.map(|ty| ty.kind), Some(HirTyKind::Err)));
        assert!(matches!(
            init.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            })
        ));
        assert!(matches!(
            closure.body.tail.map(|tail| tail.kind),
            Some(ExprKind::Path(_))
        ));
    }

    #[test]
    fn closure_paths_ignore_nested_item_syntax_outside_the_owner() {
        let file = crate::FileId::new(205);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() {\n    fn hidden() { fn() {} }\n    fn() {}\n}\n",
        );
        assert!(!parsed.errors.is_empty(), "local items are diagnosed");
        let function = function_node(&parsed.root);
        let closures = function
            .descendants()
            .filter_map(ast::ClosureExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 2);
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let hidden_expr = ast::Expr::cast(closures[0].syntax().clone()).unwrap();
        assert_eq!(
            lower_expr(&hidden_expr, &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::CLOSURE_EXPR)
        );
        assert_eq!(paths.len().unwrap(), 1);
        assert_eq!(
            source_map
                .nested_definition_for_syntax(closures[0].syntax())
                .unwrap(),
            None
        );

        let source_expr = ast::Expr::cast(closures[1].syntax().clone()).unwrap();
        let lowered = lower_expr(&source_expr, &arena, &symbols, &mut source_map)
            .expect("closure in the current owner lowers");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("source closure is preserved");
        };
        assert_eq!(
            paths
                .resolve(closure.def_id)
                .unwrap()
                .unwrap()
                .data()
                .last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            })
        );
    }

    #[test]
    fn nested_item_errors_do_not_block_the_enclosing_closure_body() {
        let file = crate::FileId::new(207);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f() {\n    fn() {\n        fn hidden() { fn() {} }\n        1\n    }\n}\n",
        );
        assert!(!parsed.errors.is_empty(), "the local item is diagnosed");
        let function = function_node(&parsed.root);
        let closures = function
            .descendants()
            .filter_map(ast::ClosureExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 2);
        let expression = ast::Expr::cast(closures[0].syntax().clone()).unwrap();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let lowered = lower_expr(&expression, &arena, &symbols, &mut source_map)
            .expect("outer closure lowers around the diagnosed local item");
        let ExprKind::Closure(closure) = lowered.kind else {
            panic!("outer closure is preserved");
        };
        assert!(matches!(
            closure.body.tail.map(|tail| tail.kind),
            Some(ExprKind::Lit(Lit::Int {
                value: Some(1),
                negated: false,
            }))
        ));
        assert_eq!(
            source_map
                .nested_definition_for_syntax(closures[1].syntax())
                .unwrap(),
            None,
            "the local item's closure is outside the enclosing owner's paths"
        );
        assert_eq!(paths.len().unwrap(), 2);
    }

    #[test]
    fn lowers_function_generic_parameters_bounds_and_where_predicates() {
        let file = crate::FileId::new(219);
        let source = concat!(
            "fn inspect<U: Clone + Fn(I32) -> I32 + Send, const N: U32, U>(value: U) -> Bool\n",
            "where U: Clone + (FnMut() -> Bool effects { io }) + Send, ",
            "I32: (Copy effects {}), Bool: Fn()\n{}\n",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, generics, _) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("function generics, signature, and body lower");
        assert_eq!(generics.params.len(), 3);
        assert_eq!(generics.where_preds.len(), 3);
        let return_type = signature.ret.expect("written return type");
        assert!(
            signature.params[0].hir_id.local_id.raw() < return_type.hir_id.local_id.raw()
                && return_type.hir_id.local_id.raw()
                    < generics.where_preds[0].hir_id.local_id.raw(),
            "parameter, return, then where nodes follow source order"
        );

        let first = generics.params[0];
        assert_eq!(symbols.resolve(first.name.sym).unwrap().as_ref(), "U");
        let HirGenericParamKind::Type { bounds } = first.kind else {
            panic!("the first parameter is a type parameter");
        };
        assert!(
            bounds[0].hir_id.local_id.raw() < signature.params[0].hir_id.local_id.raw(),
            "generic bounds precede function signature nodes"
        );
        assert_eq!(bounds.len(), 3);
        assert!(matches!(bounds[0].kind, HirBoundKind::Trait(_)));
        let HirBoundKind::FnSugar {
            kind: FnTraitKind::Fn,
            params,
            ret: Some(ret),
        } = bounds[1].kind
        else {
            panic!("the second bound is an Fn bound with a return type");
        };
        assert_eq!(params.len(), 1);
        assert!(bounds[1].effects.is_none());
        let HirTyKind::Path(param_path) = params[0].kind else {
            panic!("Fn parameter lowers to a path type");
        };
        assert_eq!(
            symbols
                .resolve(param_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "I32"
        );
        let HirTyKind::Path(ret_path) = ret.kind else {
            panic!("Fn return lowers to a path type");
        };
        assert_eq!(
            symbols
                .resolve(ret_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "I32"
        );
        assert!(matches!(bounds[2].kind, HirBoundKind::Trait(_)));

        let second = generics.params[1];
        assert_eq!(symbols.resolve(second.name.sym).unwrap().as_ref(), "N");
        let HirGenericParamKind::Const { ty } = second.kind else {
            panic!("the second parameter is a const parameter");
        };
        assert!(matches!(ty.kind, HirTyKind::Path(_)));
        let third = generics.params[2];
        assert!(matches!(
            third.kind,
            HirGenericParamKind::Type { bounds } if bounds.is_empty()
        ));

        let first_predicate = generics.where_preds[0];
        let HirTyKind::Path(subject_path) = first_predicate.ty.kind else {
            panic!("the first where subject is a path type");
        };
        assert_eq!(
            symbols
                .resolve(subject_path.segments[0].name.sym)
                .unwrap()
                .as_ref(),
            "U"
        );
        assert_eq!(first_predicate.bounds.len(), 3);
        assert!(matches!(
            first_predicate.bounds[0].kind,
            HirBoundKind::Trait(_)
        ));
        let HirBoundKind::FnSugar {
            kind: FnTraitKind::FnMut,
            params,
            ret: Some(ret),
        } = first_predicate.bounds[1].kind
        else {
            panic!("the middle where bound is FnMut");
        };
        assert!(params.is_empty());
        assert!(matches!(ret.kind, HirTyKind::Path(_)));
        let effects = first_predicate.bounds[1].effects.expect("FnMut effect cap");
        assert_eq!(effects.len(), 1);
        assert_eq!(
            symbols
                .resolve(effects[0].segments[0].sym)
                .unwrap()
                .as_ref(),
            "io"
        );
        assert!(matches!(
            first_predicate.bounds[2].kind,
            HirBoundKind::Trait(_)
        ));

        let empty_cap = generics.where_preds[1].bounds[0]
            .effects
            .expect("explicit empty cap is retained");
        assert!(empty_cap.is_empty());
        assert!(matches!(
            generics.where_preds[2].bounds[0].kind,
            HirBoundKind::FnSugar {
                kind: FnTraitKind::Fn,
                params,
                ret: None,
            } if params.is_empty()
        ));

        let first_path = paths.resolve(first.def_id).unwrap().unwrap();
        assert!(matches!(
            first_path.data().last().map(|component| &component.data),
            Some(DefPathData::TypeParam(_))
        ));
        assert_eq!(
            first_path.data().last().unwrap().disambiguator,
            0,
            "the first same-named type parameter has disambiguator zero"
        );
        let const_path = paths.resolve(second.def_id).unwrap().unwrap();
        assert!(matches!(
            const_path.data().last().map(|component| &component.data),
            Some(DefPathData::ConstParam(_))
        ));
        let duplicate_path = paths.resolve(third.def_id).unwrap().unwrap();
        assert_eq!(
            duplicate_path.data().last().unwrap().disambiguator,
            1,
            "same-named parameters of the same kind remain distinct"
        );
        assert_ne!(first.def_id, third.def_id);

        let lowered_map = source_map.finish();
        assert_eq!(
            lowered_map
                .syntax_for_node(first_predicate.bounds[1].hir_id)
                .unwrap()
                .text_range(),
            first_predicate.bounds[1].span.range
        );
        assert!(
            lowered_map.node_to_syntax().len() > generics.params.len(),
            "the source map includes bound and type nodes as well as the owner"
        );
    }

    #[test]
    fn generic_lowering_rejects_a_foreign_owner_before_allocating_hir() {
        let file = crate::FileId::new(220);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f<T: Clone>() {}\nfn g<U: Copy>() {}\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let functions = parsed
            .root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FN_DECL)
            .collect::<Vec<_>>();
        let first = functions[0].clone();
        let second = ast::FnDecl::cast(functions[1].clone()).expect("second function");
        let declaration = ast::FnDecl::cast(first.clone()).expect("first function");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &first)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        assert_eq!(
            lower_function_with_generics(&second, &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::UnsupportedSyntax(SyntaxKind::FN_DECL)
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
        assert_eq!(paths.len().unwrap(), 1);

        let other_packages = PackageInterner::new();
        let other_package = other_packages
            .intern("acme:other#1.0")
            .expect("package interns");
        let other_paths = DefPathInterner::new(other_package, &other_packages, &symbols)
            .expect("other path interner");
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &first)
                .expect("source map builder");
        assert_eq!(
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .unwrap_err(),
            HirLoweringError::DefPathInternerRequired
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);

        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &first)
                .expect("source map builder")
                .with_def_path_interner(&other_paths);
        assert_eq!(
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .unwrap_err(),
            HirLoweringError::DefPath(DefPathInternerError::ForeignDefinitionId)
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn malformed_generic_parameter_types_recover_as_error_types() {
        let file = crate::FileId::new(221);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f<const N:>() {}\n");
        assert!(
            !parsed.errors.is_empty(),
            "the missing const type is diagnosed"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, generics, _) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("diagnosed syntax still lowers");
        assert_eq!(generics.params.len(), 1);
        let HirGenericParamKind::Const { ty } = generics.params[0].kind else {
            panic!("the malformed declaration remains a const parameter");
        };
        assert!(matches!(ty.kind, HirTyKind::Err));
    }

    #[test]
    fn function_clause_hir_ids_follow_written_order_after_parser_recovery() {
        let file = crate::FileId::new(225);
        let source = concat!(
            "fn f<T>(x: I32) -> Bool ",
            "ensures result > x borrows { x } requires x > 0 ",
            "effects { io } where T: Clone {}\n",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::ClauseOrder),
            "the parser diagnoses the deliberately reordered clauses"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, generics, body) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("recovered function still lowers");
        let body = body.expect("function body exists");
        assert_eq!(body.contracts.len(), 2);
        assert_eq!(body.contracts[0].kind, ContractKind::Ensures);
        assert_eq!(body.contracts[1].kind, ContractKind::Requires);
        let effects = signature.effects.expect("the effect clause is retained");
        let borrows = signature.borrows.expect("the borrow clause is retained");
        assert_eq!(borrows.len(), 1);
        assert_eq!(symbols.resolve(borrows[0].root.sym).unwrap().as_ref(), "x");
        let ensures_id = body.contracts[0].hir_id.local_id.raw();
        let requires_id = body.contracts[1].hir_id.local_id.raw();
        assert!(
            ensures_id < requires_id
                && requires_id < effects[0].hir_id.local_id.raw()
                && effects[0].hir_id.local_id.raw() < generics.where_preds[0].hir_id.local_id.raw(),
            "contract, effect, and where nodes follow their written order"
        );
    }

    #[test]
    fn duplicate_function_borrows_and_effects_keep_the_first_clause() {
        let file = crate::FileId::new(227);
        let source =
            "fn f(x: I32) borrows { x } borrows { x.field } effects { io } effects {} {}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::DuplicateClause),
            "duplicate clauses are diagnosed"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, _) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("diagnosed duplicate clauses still lower");
        let borrows = signature
            .borrows
            .expect("the first borrow clause is retained");
        assert_eq!(borrows.len(), 1);
        assert!(borrows[0].fields.is_empty());
        let effects = signature
            .effects
            .expect("the first effects clause is retained");
        assert_eq!(effects.len(), 1);
        assert_eq!(
            symbols
                .resolve(effects[0].segments[0].sym)
                .unwrap()
                .as_ref(),
            "io"
        );
    }

    #[test]
    fn malformed_fn_bound_return_type_recovers_as_error_type() {
        let file = crate::FileId::new(222);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f<T: Fn(, I32) ->>() {}\n");
        assert!(
            !parsed.errors.is_empty(),
            "the missing return type is diagnosed"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, generics, _) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("diagnosed function still lowers");
        let HirGenericParamKind::Type { bounds } = generics.params[0].kind else {
            panic!("the parameter remains a type parameter");
        };
        let HirBoundKind::FnSugar {
            params,
            ret: Some(ret),
            ..
        } = bounds[0].kind
        else {
            panic!("the malformed return remains an Fn-family bound");
        };
        assert_eq!(params.len(), 2, "parameter recovery preserves I32");
        assert!(matches!(params[0].kind, HirTyKind::Err));
        assert!(matches!(params[1].kind, HirTyKind::Path(_)));
        assert!(matches!(ret.kind, HirTyKind::Err));
        let bound_syntax = declaration
            .generic_params()
            .expect("generic parameters")
            .params()
            .next()
            .and_then(|param| match param {
                ast::GenericParam::Type(param) => param.bounds(),
                ast::GenericParam::Const(_) => None,
            })
            .and_then(|bounds| bounds.bounds().next())
            .and_then(|bound| match bound {
                ast::Bound::FnSugar(bound) => Some(bound.syntax().clone()),
                ast::Bound::Trait(_) => None,
            })
            .expect("function-sugar syntax");
        let return_error = direct_error_child_after_token(&bound_syntax, SyntaxKind::THIN_ARROW)
            .expect("missing return type error follows the arrow");
        let source_map = source_map.finish();
        assert_eq!(
            source_map
                .syntax_for_node(ret.hir_id)
                .expect("return error has a source map entry")
                .text_range(),
            return_error.text_range(),
            "return error maps to its own missing type, not the parameter slot"
        );
    }

    #[test]
    fn malformed_fn_bound_parameter_keeps_its_slot_and_following_type() {
        let file = crate::FileId::new(226);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f<T: Fn(, I32)>() {}\n");
        assert!(
            !parsed.errors.is_empty(),
            "the missing parameter is diagnosed"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, generics, _) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("diagnosed function still lowers");
        let HirGenericParamKind::Type { bounds } = generics.params[0].kind else {
            panic!("the parameter remains a type parameter");
        };
        let HirBoundKind::FnSugar { params, .. } = bounds[0].kind else {
            panic!("the bound remains an Fn-family bound");
        };
        assert_eq!(params.len(), 2, "both parameter entries survive recovery");
        assert!(
            matches!(params[0].kind, HirTyKind::Err),
            "the malformed slot recovers as an error type"
        );
        let HirTyKind::Path(path) = params[1].kind else {
            panic!("the valid type after the missing slot remains a path");
        };
        assert_eq!(
            symbols
                .resolve(path.segments[0].name.sym)
                .expect("the path segment symbol is valid")
                .as_ref(),
            "I32"
        );
    }

    #[test]
    fn malformed_trait_path_does_not_lower_as_its_valid_prefix() {
        let file = crate::FileId::new(223);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f<T: Clone.>() {}\n");
        assert!(!parsed.errors.is_empty(), "the malformed path is diagnosed");
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let ast::GenericParam::Type(type_param) = declaration
            .generic_params()
            .expect("generic parameter list")
            .params()
            .next()
            .expect("type parameter")
        else {
            panic!("parameter is a type parameter");
        };
        let ast::Bound::Trait(trait_bound) = type_param
            .bounds()
            .expect("type parameter bounds")
            .bounds()
            .next()
            .expect("trait bound")
        else {
            panic!("bound is a trait bound");
        };
        let syntax_path = trait_bound
            .path()
            .expect("malformed path node remains")
            .syntax()
            .clone();
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, generics, _) =
            lower_function_with_generics(&declaration, &arena, &symbols, &mut source_map)
                .expect("diagnosed function still lowers");
        let HirGenericParamKind::Type { bounds } = generics.params[0].kind else {
            panic!("the parameter remains a type parameter");
        };
        let HirBoundKind::Trait(path) = bounds[0].kind else {
            panic!("the declaration remains a trait bound");
        };
        assert!(
            path.segments.is_empty(),
            "an invalid path must not expose the valid Clone prefix"
        );
        let source_map = source_map.finish();
        assert_eq!(
            source_map
                .syntax_for_node(path.hir_id)
                .expect("malformed path has a source map entry")
                .text_range(),
            syntax_path.text_range(),
            "the empty fallback path maps to the malformed path syntax"
        );
    }

    #[test]
    fn failed_generic_path_batch_does_not_leave_partial_definitions() {
        let file = crate::FileId::new(224);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f<const : U32, T>() {}\n");
        assert!(!parsed.errors.is_empty(), "the missing name is diagnosed");
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let path_symbols = Interner::new();
        let lower_symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&path_symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        assert_eq!(
            lower_function_with_generics(&declaration, &arena, &lower_symbols, &mut source_map)
                .unwrap_err(),
            HirLoweringError::DefPath(DefPathInternerError::InternerSessionMismatch)
        );
        assert_eq!(
            paths.len().unwrap(),
            1,
            "failed generic interning keeps only the preexisting owner path"
        );
        assert_eq!(source_map.finish().node_to_syntax().len(), 1);
    }

    #[test]
    fn lowers_function_signature_parameters_and_body() {
        let file = crate::FileId::new(208);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "unsafe fn f(var x: I32, _: U8, var self: Self) -> (I32, U8) borrows {x.field, self.field} effects {io, net.http} { x }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("function lowers");
        assert_eq!(signature.safety, Safety::Unsafe);
        assert!(!signature.is_async);
        assert_eq!(signature.params.len(), 3);
        assert_eq!(
            signature.params[0].name.map(|name| {
                symbols
                    .resolve(name.sym)
                    .expect("first parameter name")
                    .to_string()
            }),
            Some("x".to_owned())
        );
        assert!(signature.params[0].mutable);
        assert_eq!(signature.params[1].name, None);
        assert!(signature.params[2].is_self);
        assert!(signature.params[2].mutable);
        assert_eq!(
            signature.params[2]
                .name
                .map(|name| symbols.resolve(name.sym).unwrap().to_string()),
            Some("self".to_owned())
        );
        assert!(matches!(signature.params[0].ty.kind, HirTyKind::Path(_)));
        assert!(matches!(
            signature.ret.map(|ty| ty.kind),
            Some(HirTyKind::Tuple(_))
        ));
        let borrows = signature.borrows.expect("borrows clause");
        assert_eq!(borrows.len(), 2);
        assert_eq!(borrows[0].fields.len(), 1);
        assert_eq!(borrows[1].fields.len(), 1);
        assert_eq!(symbols.resolve(borrows[0].root.sym).unwrap().as_ref(), "x");
        assert_eq!(
            symbols.resolve(borrows[1].root.sym).unwrap().as_ref(),
            "self"
        );
        let effects = signature.effects.expect("effects clause");
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[1].segments.len(), 2);
        assert_eq!(
            symbols
                .resolve(effects[1].segments[1].sym)
                .unwrap()
                .as_ref(),
            "http"
        );

        let body = body.expect("function has a body");
        assert_eq!(body.owner, owner);
        assert_eq!(body.params.len(), 3);
        assert_ne!(body.params[0].hir_id, signature.params[0].hir_id);
        assert!(body.params[0].mutable);
        assert_eq!(body.params[1].name, None);
        assert!(body.params[2].binding.raw() > body.params[1].binding.raw());
        let ExprKind::Block(block) = body.value.kind else {
            panic!("function body is represented by a block expression");
        };
        assert!(matches!(
            block.tail.map(|expr| expr.kind),
            Some(ExprKind::Path(_))
        ));
        assert!(body.contracts.is_empty());
        assert!(body.invariant_preds.is_empty());
        assert_eq!(source_map.finish().binding_to_syntax().len(), 3);
    }

    #[test]
    fn compatibility_function_lowering_does_not_require_generic_def_paths() {
        let file = crate::FileId::new(228);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f<T: Clone>(value: T) -> T where T: Copy { value }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("legacy lowering accepts a generic declaration");
        assert_eq!(signature.params.len(), 1);
        assert!(matches!(signature.params[0].ty.kind, HirTyKind::Path(_)));
        assert!(matches!(
            signature.ret.map(|ty| ty.kind),
            Some(HirTyKind::Path(_))
        ));
        assert!(body.is_some());
        assert_eq!(
            paths.len().unwrap(),
            1,
            "legacy lowering does not intern generic parameter definitions"
        );
    }

    #[test]
    fn lowers_item_tree_function_into_complete_hir_owner() {
        let file = crate::FileId::new(229);
        let source = "@inline\n@deprecated(\"reason\")\npub fn f<T: Clone>(value: T) -> T where T: Copy { value }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("function item entry");
        assert_eq!(entry.kind, ItemTreeKind::Function);
        let entry_id = entry.ast_id;
        let (_packages, paths, owner) = owner_context(&symbols);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_function_owner(owner, &expected_path, &item_tree, entry_id, &context)
                .expect("function item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.body.expect("function has a body").owner, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 2);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Inline
        );
        assert_eq!(
            hir_owner.raw_attrs[1].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level function is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(symbols.resolve(item.name.sym).unwrap().as_ref(), "f");
        let ItemKind::Fn {
            sig,
            generics,
            has_body,
        } = item.kind
        else {
            panic!("function item retains its function kind");
        };
        assert_eq!(sig.params.len(), 1);
        assert_eq!(generics.params.len(), 1);
        assert_eq!(generics.where_preds.len(), 1);
        assert!(has_body);
        let owner_node = crate::HirId {
            owner,
            local_id: crate::ItemLocalId::default(),
        };
        assert_eq!(
            source_map
                .syntax_for_node(owner_node)
                .expect("owner has a source-map entry")
                .text_range(),
            function_node(&parsed.root).text_range()
        );
        assert_eq!(
            item.span.range,
            function_node(&parsed.root).text_range(),
            "the HIR item span covers the function declaration"
        );
        assert_eq!(paths.len().unwrap(), 2);

        let mut stale_item_tree = item_tree.clone();
        stale_item_tree.items[0].vis = crate::Visibility::Private;
        assert!(matches!(
            lower_function_owner(owner, &expected_path, &stale_item_tree, entry_id, &context),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            2,
            "a stale visibility snapshot is rejected before lowering generic paths"
        );

        let mut stale_attributes = item_tree.clone();
        stale_attributes.items[0].raw_attrs.clear();
        assert!(matches!(
            lower_function_owner(owner, &expected_path, &stale_attributes, entry_id, &context),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            2,
            "stale attributes are rejected before lowering generic paths"
        );

        let mut changed_attributes = item_tree.clone();
        changed_attributes.items[0].raw_attrs[0].args = Some(Vec::new());
        assert!(matches!(
            lower_function_owner(
                owner,
                &expected_path,
                &changed_attributes,
                entry_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));

        let mut stale_argument_span = item_tree.clone();
        let attr = &mut stale_argument_span.items[0].raw_attrs[1];
        let argument = &mut attr.args.as_mut().expect("attribute has args")[0];
        let RawAttrArg::Str { span, .. } = argument else {
            panic!("deprecated attribute has a string argument");
        };
        *span = attr.span;
        assert!(matches!(
            lower_function_owner(
                owner,
                &expected_path,
                &stale_argument_span,
                entry_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));

        let wrong_name = symbols.intern("other").expect("symbol interns");
        let (_other_packages, other_paths, other_owner) =
            owner_context_with_symbol(&symbols, wrong_name);
        let other_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &other_paths,
        );
        assert!(matches!(
            lower_function_owner(
                other_owner,
                &expected_path,
                &item_tree,
                entry_id,
                &other_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
    }

    #[test]
    fn lowers_item_tree_alias_with_generic_parameters() {
        let file = crate::FileId::new(254);
        let source = "@deprecated(\"legacy alias\")\npub type Pair<T: Clone> = (T, T)\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("alias item entry");
        assert_eq!(entry.kind, ItemTreeKind::Alias);
        let name = symbols.intern("Pair").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_alias_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("type alias lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);
        assert!(hir_owner.body.is_none(), "type aliases do not own a body");

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level alias is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(item.name.sym, name);
        let ItemKind::Alias { generics, ty } = item.kind else {
            panic!("alias item retains its alias kind");
        };
        assert_eq!(generics.params.len(), 1);
        let generic_param = generics.params[0];
        assert_eq!(generic_param.name.sym, symbols.intern("T").unwrap());
        assert!(matches!(
            generic_param.kind,
            HirGenericParamKind::Type { bounds }
                if matches!(bounds, [HirBound { kind: HirBoundKind::Trait(_), .. }])
        ));
        let generic_path = paths
            .resolve(generic_param.def_id)
            .expect("generic parameter path resolves")
            .expect("generic parameter path exists");
        assert_eq!(
            generic_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::TypeParam(generic_param.name.sym),
                disambiguator: 0,
            })
        );
        let HirTyKind::Tuple(elements) = ty.kind else {
            panic!("alias retains its tuple type");
        };
        assert_eq!(elements.len(), 2);
        assert!(elements.iter().all(|element| matches!(
            element.kind,
            HirTyKind::Path(path)
                if path.segments.len() == 1
                    && path.segments[0].name.sym == generic_param.name.sym
        )));
        let declaration = parsed
            .root
            .descendants()
            .find_map(ast::TypeDecl::cast)
            .expect("type declaration");
        let source_ty = declaration
            .alias_def()
            .and_then(|definition| definition.ty())
            .expect("alias type");
        assert_eq!(item.span.range, declaration.syntax().text_range());
        assert_eq!(
            item.name.span.range,
            declaration
                .name()
                .and_then(|name| name.token())
                .expect("type name token")
                .text_range()
        );
        assert_eq!(
            source_map
                .syntax_for_node(ty.hir_id)
                .expect("alias type has a source-map entry")
                .text_range(),
            source_ty.syntax().text_range()
        );
        assert!(source_map.syntax_for_node(elements[0].hir_id).is_some());
        let path = paths
            .resolve(owner.def_id)
            .expect("type path resolves")
            .expect("type path exists");
        assert_eq!(
            path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::TypeNs(name),
                disambiguator: 0,
            })
        );
    }

    #[test]
    fn alias_owner_recovers_a_missing_type() {
        let file = crate::FileId::new(255);
        let source = "type Broken =\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(!parsed.errors.is_empty(), "the missing type is diagnosed");
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed alias");
        let entry = item_tree.items.first().expect("alias item entry");
        assert_eq!(entry.kind, ItemTreeKind::Alias);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_alias_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing alias type recovers to error HIR");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level alias is represented by an item owner");
        };
        let ItemKind::Alias { generics, ty } = item.kind else {
            panic!("alias item retains its alias kind");
        };
        assert!(generics.params.is_empty());
        assert!(matches!(ty.kind, HirTyKind::Err));
        assert!(hir_owner.body.is_none());
    }

    #[test]
    fn alias_owner_rejects_a_type_declaration_without_a_definition() {
        let file = crate::FileId::new(258);
        let source = "type Broken\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing definition is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed declaration");
        let entry = item_tree.items.first().expect("fallback alias item entry");
        assert_eq!(entry.kind, ItemTreeKind::Alias);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        assert!(matches!(
            lower_alias_owner(owner, &expected_path, &item_tree, entry.ast_id, &context),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "an unclassified type declaration must not allocate alias paths"
        );
    }

    #[test]
    fn alias_owner_rejects_non_type_paths_and_other_item_kinds() {
        let file = crate::FileId::new(256);
        let source = "type Alias = I32\nconst VALUE: I32 = 1\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let alias_entry = item_tree.items.first().expect("alias item entry");
        let const_entry = item_tree.items.get(1).expect("constant item entry");
        let wrong_name = symbols.intern("Alias").expect("symbol interns");
        let (_packages, wrong_paths, wrong_owner) =
            owner_context_with_data(&symbols, DefPathData::ValueNs(wrong_name));
        let wrong_expected_path = owner_def_path(&wrong_paths, wrong_owner);
        let arena = HirArena::new();
        let wrong_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &wrong_paths,
        );
        assert!(matches!(
            lower_alias_owner(
                wrong_owner,
                &wrong_expected_path,
                &item_tree,
                alias_entry.ast_id,
                &wrong_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            wrong_paths.len().unwrap(),
            1,
            "wrong namespace is rejected before generic paths are interned"
        );

        let name = symbols.intern("Alias").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);
        assert!(matches!(
            lower_alias_owner(
                owner,
                &expected_path,
                &item_tree,
                const_entry.ast_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "a constant entry cannot allocate alias generics"
        );
    }

    #[test]
    fn lowers_item_tree_refined_type_with_base_predicate_and_implicit_self() {
        let file = crate::FileId::new(259);
        let source = "@deprecated(\"bounded\")\npub type Positive<T> = I32 where self > 0\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("refined item entry");
        assert_eq!(entry.kind, ItemTreeKind::Refined);
        assert_eq!(entry.children.len(), 1);
        assert_eq!(
            entry.children[0].kind,
            ItemTreeKind::TypeParam,
            "the invalid generic declaration remains available for validation"
        );
        let name = symbols.intern("Positive").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_refined_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("refined type lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level refined type is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(item.name.sym, name);
        let ItemKind::Refined { base } = item.kind else {
            panic!("refined declaration retains its refined item kind");
        };
        let HirTyKind::Path(base_path) = base.kind else {
            panic!("refined type retains its base path");
        };
        assert_eq!(base_path.segments.len(), 1);
        assert_eq!(
            symbols
                .resolve(base_path.segments[0].name.sym)
                .expect("base path name resolves")
                .as_ref(),
            "I32"
        );
        assert!(matches!(
            paths.resolve(owner.def_id).unwrap().unwrap().data().last(),
            Some(DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
                disambiguator: 0,
            }) if *symbol == name
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "refined type HIR does not fabricate generic parameter paths"
        );

        let body = hir_owner
            .body
            .expect("refined type owns its predicate body");
        assert!(
            body.params.is_empty(),
            "self is implicit in the owner context"
        );
        assert!(body.contracts.is_empty());
        assert!(body.invariant_preds.is_empty());
        let ExprKind::Binary(BinOp::Gt, lhs, _) = body.value.kind else {
            panic!("predicate is retained as the owner's body value");
        };
        let ExprKind::Path(self_path) = lhs.kind else {
            panic!("self is retained as a value path");
        };
        assert_eq!(self_path.root, PathRoot::SelfValue);
        assert!(self_path.segments.is_empty());

        let declaration = parsed
            .root
            .descendants()
            .find_map(ast::TypeDecl::cast)
            .expect("type declaration");
        let definition = declaration.alias_def().expect("alias definition");
        let source_base = definition.ty().expect("base type");
        let source_predicate = definition
            .refinement()
            .and_then(|clause| clause.expression())
            .expect("refinement predicate");
        assert_eq!(item.span.range, declaration.syntax().text_range());
        assert_eq!(
            source_map
                .syntax_for_node(base.hir_id)
                .expect("base type has a source-map entry")
                .text_range(),
            source_base.syntax().text_range()
        );
        assert_eq!(
            source_map
                .syntax_for_node(body.value.hir_id)
                .expect("predicate has a source-map entry")
                .text_range(),
            source_predicate.syntax().text_range()
        );
    }

    #[test]
    fn refined_owner_recovers_a_missing_predicate() {
        let file = crate::FileId::new(260);
        let source = "type Broken = I32 where\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing predicate is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed refined type");
        let entry = item_tree.items.first().expect("refined item entry");
        assert_eq!(entry.kind, ItemTreeKind::Refined);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_refined_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing predicate recovers to error HIR");
        let body = hir_owner.body.expect("refined type retains its body");
        assert!(matches!(body.value.kind, ExprKind::Err));
    }

    #[test]
    fn refined_owner_recovers_a_missing_base_type() {
        let file = crate::FileId::new(262);
        let source = "type Broken = where self > 0\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing base type is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed refined type");
        let entry = item_tree.items.first().expect("refined item entry");
        assert_eq!(entry.kind, ItemTreeKind::Refined);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_refined_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing base type recovers to error HIR");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level refined type is represented by an item owner");
        };
        let ItemKind::Refined { base } = item.kind else {
            panic!("refined declaration retains its refined item kind");
        };
        assert!(matches!(base.kind, HirTyKind::Err));
        let body = hir_owner.body.expect("refined type retains its predicate");
        assert!(matches!(body.value.kind, ExprKind::Binary(BinOp::Gt, ..)));
    }

    #[test]
    fn refined_owner_rejects_wrong_paths_and_other_item_kinds() {
        let file = crate::FileId::new(261);
        let source = "type Refined = I32 where self > 0\ntype Alias = I32\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let refined_entry = item_tree.items.first().expect("refined item entry");
        let alias_entry = item_tree.items.get(1).expect("alias item entry");
        assert_eq!(refined_entry.kind, ItemTreeKind::Refined);
        assert_eq!(alias_entry.kind, ItemTreeKind::Alias);

        let wrong_name = symbols.intern("Refined").expect("symbol interns");
        let (_packages, wrong_paths, wrong_owner) =
            owner_context_with_data(&symbols, DefPathData::ValueNs(wrong_name));
        let wrong_expected_path = owner_def_path(&wrong_paths, wrong_owner);
        let arena = HirArena::new();
        let wrong_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &wrong_paths,
        );
        assert!(matches!(
            lower_refined_owner(
                wrong_owner,
                &wrong_expected_path,
                &item_tree,
                refined_entry.ast_id,
                &wrong_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(wrong_paths.len().unwrap(), 1);

        let name = symbols.intern("Refined").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);
        assert!(matches!(
            lower_refined_owner(
                owner,
                &expected_path,
                &item_tree,
                alias_entry.ast_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn lowers_item_tree_distinct_with_generics_and_public_constructor() {
        let file = crate::FileId::new(263);
        let source = "distinct Meters<T: Clone> = pub T\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("distinct item entry");
        assert_eq!(entry.kind, ItemTreeKind::Distinct);
        let name = symbols.intern("Meters").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_distinct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("distinct type lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(source_map.owner(), owner);
        assert!(hir_owner.body.is_none());
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        assert_eq!(item.vis, crate::Visibility::Private);
        let ItemKind::Distinct {
            generics,
            pub_ctor,
            base,
            has_predicate,
        } = item.kind
        else {
            panic!("distinct declaration retains its distinct item kind");
        };
        assert!(pub_ctor, "the inner `pub` exposes the constructor");
        assert!(!has_predicate);
        assert_eq!(generics.params.len(), 1);
        let generic = generics.params[0];
        assert_eq!(generic.name.sym, symbols.intern("T").unwrap());
        let generic_path = paths
            .resolve(generic.def_id)
            .expect("generic parameter path resolves")
            .expect("generic parameter path exists");
        assert_eq!(
            generic_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::TypeParam(generic.name.sym),
                disambiguator: 0,
            })
        );
        assert!(matches!(
            generic.kind,
            HirGenericParamKind::Type { bounds }
                if matches!(bounds, [HirBound { kind: HirBoundKind::Trait(_), .. }])
        ));
        assert!(matches!(
            base.kind,
            HirTyKind::Path(path)
                if path.segments.len() == 1 && path.segments[0].name.sym == generic.name.sym
        ));
    }

    #[test]
    fn lowers_item_tree_refined_distinct_with_implicit_self() {
        let file = crate::FileId::new(264);
        let source = "distinct Age = U8 where self <= 150\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("distinct item entry");
        assert_eq!(entry.kind, ItemTreeKind::Distinct);
        let name = symbols.intern("Age").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_distinct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("refined distinct type lowers to a complete owner");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        let ItemKind::Distinct {
            has_predicate,
            pub_ctor,
            ..
        } = item.kind
        else {
            panic!("distinct declaration retains its distinct item kind");
        };
        assert!(!pub_ctor);
        assert!(has_predicate);
        let body = hir_owner
            .body
            .expect("refined distinct owns a predicate body");
        assert!(body.params.is_empty());
        let ExprKind::Binary(BinOp::Le, lhs, _) = body.value.kind else {
            panic!("predicate is retained as the body value");
        };
        let ExprKind::Path(self_path) = lhs.kind else {
            panic!("self is retained as a value path");
        };
        assert_eq!(self_path.root, PathRoot::SelfValue);
        assert!(self_path.segments.is_empty());
    }

    #[test]
    fn distinct_owner_keeps_outer_visibility_separate_from_constructor_visibility() {
        let file = crate::FileId::new(268);
        let source = "pub distinct PublicType = I32\ndistinct PublicCtor = pub I32\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let public_type_entry = item_tree.items.first().expect("public distinct entry");
        let public_ctor_entry = item_tree.items.get(1).expect("public constructor entry");
        let arena = HirArena::new();

        let public_type_name = symbols.intern("PublicType").expect("symbol interns");
        let (_public_type_packages, public_type_paths, public_type_owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(public_type_name));
        let public_type_expected_path = owner_def_path(&public_type_paths, public_type_owner);
        let public_type_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &public_type_paths,
        );
        let (public_type_owner, _) = lower_distinct_owner(
            public_type_owner,
            &public_type_expected_path,
            &item_tree,
            public_type_entry.ast_id,
            &public_type_context,
        )
        .expect("outer visibility lowers");
        let OwnerNode::Item(public_type) = public_type_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        assert_eq!(public_type.vis, crate::Visibility::Pub);
        assert!(matches!(
            public_type.kind,
            ItemKind::Distinct {
                pub_ctor: false,
                ..
            }
        ));

        let public_ctor_name = symbols.intern("PublicCtor").expect("symbol interns");
        let (_public_ctor_packages, public_ctor_paths, public_ctor_owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(public_ctor_name));
        let public_ctor_expected_path = owner_def_path(&public_ctor_paths, public_ctor_owner);
        let public_ctor_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &public_ctor_paths,
        );
        let (public_ctor_owner, _) = lower_distinct_owner(
            public_ctor_owner,
            &public_ctor_expected_path,
            &item_tree,
            public_ctor_entry.ast_id,
            &public_ctor_context,
        )
        .expect("constructor visibility lowers");
        let OwnerNode::Item(public_ctor) = public_ctor_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        assert_eq!(public_ctor.vis, crate::Visibility::Private);
        assert!(matches!(
            public_ctor.kind,
            ItemKind::Distinct { pub_ctor: true, .. }
        ));
    }

    #[test]
    fn distinct_owner_recovers_a_missing_predicate() {
        let file = crate::FileId::new(265);
        let source = "distinct Broken = I32 where\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing predicate is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed distinct type");
        let entry = item_tree.items.first().expect("distinct item entry");
        assert_eq!(entry.kind, ItemTreeKind::Distinct);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_distinct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing predicate recovers to error HIR");
        let body = hir_owner
            .body
            .expect("the clause still defines a predicate body");
        assert!(matches!(body.value.kind, ExprKind::Err));
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        assert!(matches!(
            item.kind,
            ItemKind::Distinct {
                has_predicate: true,
                ..
            }
        ));
    }

    #[test]
    fn distinct_owner_recovers_a_missing_base_type() {
        let file = crate::FileId::new(267);
        let source = "distinct Broken =\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing base type is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed distinct type");
        let entry = item_tree.items.first().expect("distinct item entry");
        assert_eq!(entry.kind, ItemTreeKind::Distinct);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_distinct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing base type recovers to error HIR");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level distinct type is represented by an item owner");
        };
        assert!(matches!(
            item.kind,
            ItemKind::Distinct {
                base: HirTy {
                    kind: HirTyKind::Err,
                    ..
                },
                has_predicate: false,
                ..
            }
        ));
        assert!(hir_owner.body.is_none());
    }

    #[test]
    fn distinct_owner_rejects_wrong_paths_and_other_item_kinds() {
        let file = crate::FileId::new(266);
        let source = "distinct Distinct = I32\ntype Alias = I32\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let distinct_entry = item_tree.items.first().expect("distinct item entry");
        let alias_entry = item_tree.items.get(1).expect("alias item entry");

        let wrong_name = symbols.intern("Distinct").expect("symbol interns");
        let (_packages, wrong_paths, wrong_owner) =
            owner_context_with_data(&symbols, DefPathData::ValueNs(wrong_name));
        let wrong_expected_path = owner_def_path(&wrong_paths, wrong_owner);
        let arena = HirArena::new();
        let wrong_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &wrong_paths,
        );
        assert!(matches!(
            lower_distinct_owner(
                wrong_owner,
                &wrong_expected_path,
                &item_tree,
                distinct_entry.ast_id,
                &wrong_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(wrong_paths.len().unwrap(), 1);

        let name = symbols.intern("Distinct").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);
        assert!(matches!(
            lower_distinct_owner(
                owner,
                &expected_path,
                &item_tree,
                alias_entry.ast_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn lowers_item_tree_struct_with_generics_where_and_fields() {
        let file = crate::FileId::new(257);
        let source = concat!(
            "@deprecated(\"record\")\n",
            "pub type Pair<T>\n",
            "where T: Clone\n",
            "{\n",
            "    pub first: T\n",
            "    second: T\n",
            "}\n",
        );
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("struct item entry");
        assert_eq!(entry.kind, ItemTreeKind::Struct);
        let name = symbols.intern("Pair").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_struct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("record lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);
        assert!(hir_owner.body.is_none(), "records do not own a body");

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level record is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(item.name.sym, name);
        let ItemKind::Struct { generics, fields } = item.kind else {
            panic!("record item retains its struct kind");
        };
        assert_eq!(generics.params.len(), 1);
        assert_eq!(generics.where_preds.len(), 1);
        assert!(matches!(
            generics.params[0].kind,
            HirGenericParamKind::Type { bounds } if bounds.is_empty()
        ));
        assert!(matches!(
            generics.where_preds[0].bounds,
            [HirBound {
                kind: HirBoundKind::Trait(_),
                ..
            }]
        ));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].vis, crate::Visibility::Pub);
        assert_eq!(fields[1].vis, crate::Visibility::Private);
        assert_eq!(fields[0].name.sym, symbols.intern("first").unwrap());
        assert_eq!(fields[1].name.sym, symbols.intern("second").unwrap());
        assert!(fields.iter().all(|field| matches!(
            field.ty.kind,
            HirTyKind::Path(path)
                if path.segments.len() == 1
                    && path.segments[0].name.sym == generics.params[0].name.sym
        )));

        let declaration = parsed
            .root
            .descendants()
            .find_map(ast::TypeDecl::cast)
            .expect("type declaration");
        assert_eq!(item.span.range, declaration.syntax().text_range());
        let field_syntax = declaration
            .struct_def()
            .and_then(|definition| definition.fields())
            .expect("record field list")
            .fields()
            .collect::<Vec<_>>();
        for (field, syntax) in fields.iter().zip(&field_syntax) {
            assert_eq!(
                source_map
                    .syntax_for_node(field.hir_id)
                    .expect("field has a source-map entry")
                    .text_range(),
                syntax.syntax().text_range()
            );
            assert_eq!(
                field.name.span.range,
                syntax
                    .name()
                    .and_then(|name| name.token())
                    .expect("field name token")
                    .text_range()
            );
            assert_eq!(
                source_map
                    .syntax_for_node(field.ty.hir_id)
                    .expect("field type has a source-map entry")
                    .text_range(),
                syntax.ty().expect("field type").syntax().text_range()
            );
        }
        for (ordinal, field) in fields.iter().enumerate() {
            let field_path = paths
                .resolve(field.def_id)
                .expect("field definition path resolves")
                .expect("field definition path exists");
            assert_eq!(
                field_path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Field(field.name.sym),
                    disambiguator: 0,
                })
            );
            assert_eq!(field_path.data().len(), 2);
            assert_eq!(
                field_path.data()[0].data,
                DefPathData::TypeNs(name),
                "field {ordinal} is nested below its record"
            );
        }
        let record_path = paths
            .resolve(owner.def_id)
            .expect("record path resolves")
            .expect("record path exists");
        assert_eq!(
            record_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::TypeNs(name),
                disambiguator: 0,
            })
        );
        assert_eq!(paths.len().unwrap(), 4);
    }

    #[test]
    fn struct_owner_disambiguates_duplicate_field_names_in_source_order() {
        let file = crate::FileId::new(260);
        let source = "type Duplicate {\n    value: I32\n    value: U32\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("record item entry");
        let value = symbols.intern("Duplicate").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(value));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_struct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("duplicate field declarations remain available to semantic checking");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level record is represented by an item owner");
        };
        let ItemKind::Struct { fields, .. } = item.kind else {
            panic!("record item retains its struct kind");
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name.sym, fields[1].name.sym);
        for (ordinal, field) in fields.iter().enumerate() {
            let path = paths
                .resolve(field.def_id)
                .expect("field path resolves")
                .expect("field path exists");
            assert_eq!(
                path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Field(field.name.sym),
                    disambiguator: ordinal as u32,
                })
            );
        }
        assert_eq!(paths.len().unwrap(), 3);
    }

    #[test]
    fn struct_owner_recovers_a_missing_field_type() {
        let file = crate::FileId::new(258);
        let source = "type Broken {\n    value:\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the missing field type is diagnosed"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed record");
        let entry = item_tree.items.first().expect("record item entry");
        assert_eq!(entry.kind, ItemTreeKind::Struct);
        let name = symbols.intern("Broken").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_struct_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("diagnosed record remains available to later stages");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level record is represented by an item owner");
        };
        let ItemKind::Struct { fields, .. } = item.kind else {
            panic!("record item retains its struct kind");
        };
        assert_eq!(fields.len(), 1);
        assert!(matches!(fields[0].ty.kind, HirTyKind::Err));
    }

    #[test]
    fn struct_owner_rejects_wrong_paths_and_other_item_kinds() {
        let file = crate::FileId::new(259);
        let source = "type Record {\n}\ntype Alias = I32\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let record_entry = item_tree.items.first().expect("record item entry");
        let alias_entry = item_tree.items.get(1).expect("alias item entry");
        let name = symbols.intern("Record").expect("symbol interns");
        let (_packages, wrong_paths, wrong_owner) =
            owner_context_with_data(&symbols, DefPathData::ValueNs(name));
        let wrong_expected_path = owner_def_path(&wrong_paths, wrong_owner);
        let arena = HirArena::new();
        let wrong_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &wrong_paths,
        );
        assert!(matches!(
            lower_struct_owner(
                wrong_owner,
                &wrong_expected_path,
                &item_tree,
                record_entry.ast_id,
                &wrong_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(wrong_paths.len().unwrap(), 1);

        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::TypeNs(name));
        let expected_path = owner_def_path(&paths, owner);
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);
        assert!(matches!(
            lower_struct_owner(
                owner,
                &expected_path,
                &item_tree,
                alias_entry.ast_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn lowers_item_tree_const_into_complete_hir_owner() {
        let file = crate::FileId::new(234);
        let source = "@deprecated(\"keep\")\npub const ANSWER: I32 = 42\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("const item entry");
        assert_eq!(entry.kind, ItemTreeKind::Const);
        let entry_id = entry.ast_id;
        let name = symbols.intern("ANSWER").expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_symbol(&symbols, name);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_const_owner(owner, &expected_path, &item_tree, entry_id, &context)
                .expect("const item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(
            hir_owner.body.expect("const has an initializer").owner,
            owner
        );
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level const is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(item.name.sym, name);
        let ItemKind::Const { ty } = item.kind else {
            panic!("constant item retains its const kind");
        };
        assert!(matches!(ty.kind, HirTyKind::Path(_)));
        let body = hir_owner.body.expect("const initializer body");
        assert!(matches!(
            body.value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(42),
                negated: false
            })
        ));
        let const_node = parsed
            .root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::CONST_DECL)
            .expect("const declaration");
        assert_eq!(item.span.range, const_node.text_range());
        assert_eq!(
            source_map
                .syntax_for_node(crate::HirId {
                    owner,
                    local_id: crate::ItemLocalId::default(),
                })
                .expect("owner has a source-map entry")
                .text_range(),
            const_node.text_range()
        );
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn const_owner_recovers_a_missing_initializer() {
        let file = crate::FileId::new(235);
        let source = "const ANSWER: I32 =\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::ExpectedExpression)
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects a diagnosed const");
        let entry = item_tree.items.first().expect("const item entry");
        let name = symbols.intern("ANSWER").expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_symbol(&symbols, name);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_const_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing initializer recovers as an error body");
        let body = hir_owner.body.expect("const still has an owner body");
        assert!(matches!(body.value.kind, ExprKind::Err));
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("constant is represented by an item owner");
        };
        assert!(matches!(item.kind, ItemKind::Const { .. }));
    }

    #[test]
    fn const_owner_recovers_a_missing_type() {
        let file = crate::FileId::new(236);
        let source = "const ANSWER: = 42\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            !parsed.errors.is_empty(),
            "the parser diagnoses the missing type"
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects a diagnosed const");
        let entry = item_tree.items.first().expect("const item entry");
        let name = symbols.intern("ANSWER").expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_symbol(&symbols, name);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_const_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing type recovers as an error HIR type");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("constant is represented by an item owner");
        };
        let ItemKind::Const { ty } = item.kind else {
            panic!("constant item retains its const kind");
        };
        assert!(matches!(ty.kind, HirTyKind::Err));
        assert!(matches!(
            hir_owner.body.expect("initializer body").value.kind,
            ExprKind::Lit(Lit::Int {
                value: Some(42),
                negated: false
            })
        ));
    }

    #[test]
    fn lowers_item_tree_effect_into_complete_hir_owner() {
        let file = crate::FileId::new(237);
        let source =
            "@deprecated(\"old effect\")\npub effect write\n    covers { net.connect, alloc }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("effect item entry");
        assert_eq!(entry.kind, ItemTreeKind::Effect);
        let entry_id = entry.ast_id;
        let name = symbols.intern("write").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::Effect(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_effect_owner(owner, &expected_path, &item_tree, entry_id, &context)
                .expect("effect item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert!(hir_owner.body.is_none());
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level effect is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Pub);
        assert_eq!(item.name.sym, name);
        let ItemKind::Effect { covers } = item.kind else {
            panic!("effect item retains its effect kind");
        };
        assert_eq!(covers.len(), 2);
        assert_eq!(
            symbols.resolve(covers[0].segments[0].sym).unwrap().as_ref(),
            "net"
        );
        assert_eq!(
            symbols.resolve(covers[0].segments[1].sym).unwrap().as_ref(),
            "connect"
        );
        assert_eq!(
            symbols.resolve(covers[1].segments[0].sym).unwrap().as_ref(),
            "alloc"
        );
        let effect_ref = parsed
            .root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EFFECT_REF)
            .expect("covered effect reference");
        assert_eq!(
            source_map
                .syntax_for_node(covers[0].hir_id)
                .expect("effect reference has a source-map entry")
                .text_range(),
            effect_ref.text_range()
        );
        let effect_node = parsed
            .root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EFFECT_DECL)
            .expect("effect declaration");
        assert_eq!(item.span.range, effect_node.text_range());
        assert_eq!(paths.len().unwrap(), 1);

        let wrong_parent_path = DefPath::new(
            expected_path.package(),
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Module(
                        symbols.intern("other_module").expect("symbol interns"),
                    ),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::Effect(name),
                    disambiguator: 0,
                },
            ],
            &_packages,
            &symbols,
        )
        .expect("wrong-parent path is valid");
        let wrong_parent_owner = OwnerId {
            def_id: paths
                .intern(wrong_parent_path)
                .expect("wrong-parent definition interns"),
        };
        assert!(matches!(
            lower_effect_owner(
                wrong_parent_owner,
                &expected_path,
                &item_tree,
                entry_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));

        let (_wrong_packages, wrong_paths, wrong_owner) = owner_context_with_symbol(&symbols, name);
        let wrong_context = HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &wrong_paths,
        );
        assert!(matches!(
            lower_effect_owner(
                wrong_owner,
                &expected_path,
                &item_tree,
                entry_id,
                &wrong_context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
    }

    #[test]
    fn effect_owner_recovers_absent_and_malformed_covers_as_empty() {
        for (file_id, source, expected_parse_errors) in [
            (238, "effect audit\n", false),
            (239, "effect audit covers { net.connect\n", true),
            (240, "effect audit covers { alloc, null }\n", true),
            (241, "effect audit covers { alloc, r#self }\n", true),
        ] {
            let file = crate::FileId::new(file_id);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert_eq!(
                !parsed.errors.is_empty(),
                expected_parse_errors,
                "{source:?}: {:?}",
                parsed.errors
            );
            let symbols = Interner::new();
            let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
                .expect("item tree collects the effect");
            let entry = item_tree.items.first().expect("effect item entry");
            let name = symbols.intern("audit").expect("symbol interns");
            let (_packages, paths, owner) =
                owner_context_with_data(&symbols, DefPathData::Effect(name));
            let expected_path = owner_def_path(&paths, owner);
            let arena = HirArena::new();
            let context = HirLoweringContext::new(
                &ast_ids,
                &snapshot,
                &parsed.root,
                &arena,
                &symbols,
                &paths,
            );

            let (hir_owner, _) =
                lower_effect_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                    .expect("effect keeps an owner when covers is absent or malformed");
            let OwnerNode::Item(item) = hir_owner.node else {
                panic!("top-level effect is represented by an item owner");
            };
            let ItemKind::Effect { covers } = item.kind else {
                panic!("effect item retains its effect kind");
            };
            assert!(
                covers.is_empty(),
                "absent and malformed covers recover to the empty set: {source:?}"
            );
        }
    }

    #[test]
    fn lowers_item_tree_invariant_into_complete_hir_owner() {
        let file = crate::FileId::new(242);
        let source = "@deprecated(\"old invariant\")\ninvariant Account {\n self.balance >= 0,\n self.enabled\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("invariant item entry");
        assert_eq!(entry.kind, ItemTreeKind::Invariant);
        let entry_id = entry.ast_id;
        let name = symbols.intern("Account").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::Invariant(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_invariant_owner(owner, &expected_path, &item_tree, entry_id, &context)
                .expect("invariant item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level invariant is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Private);
        assert_eq!(item.name.sym, name);
        let ItemKind::Invariant { target } = item.kind else {
            panic!("invariant item retains its invariant kind");
        };
        assert_eq!(target.root, PathRoot::Plain);
        assert_eq!(target.segments.len(), 1);
        assert_eq!(target.segments[0].name.sym, name);
        assert!(target.segments[0].args.is_none());
        let invariant_node = parsed
            .root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::INVARIANT_DECL)
            .expect("invariant declaration");
        let declaration = ast::InvariantDecl::cast(invariant_node.clone())
            .expect("invariant declaration casts to its AST view");
        let target_name = declaration
            .name()
            .expect("invariant target name")
            .syntax()
            .clone();
        assert_eq!(
            source_map
                .syntax_for_node(target.hir_id)
                .expect("target path has a source-map entry")
                .text_range(),
            target_name.text_range()
        );
        assert_eq!(
            source_map
                .syntax_for_node(target.segments[0].hir_id)
                .expect("target segment has a source-map entry")
                .text_range(),
            target_name.text_range()
        );

        let body = hir_owner.body.expect("invariant owns a body");
        assert_eq!(body.owner, owner);
        assert!(body.params.is_empty());
        assert!(body.contracts.is_empty());
        assert!(matches!(body.value.kind, ExprKind::Tuple(elements) if elements.is_empty()));
        assert_eq!(body.invariant_preds.len(), 2);
        assert!(matches!(
            body.invariant_preds[0].kind,
            ExprKind::Binary(BinOp::Ge, _, _)
        ));
        assert!(matches!(
            body.invariant_preds[1].kind,
            ExprKind::Field { .. }
        ));
        let syntax_predicates = declaration
            .predicates()
            .expect("invariant expression list")
            .expressions()
            .collect::<Vec<_>>();
        assert_eq!(syntax_predicates.len(), body.invariant_preds.len());
        for (predicate, syntax) in body.invariant_preds.iter().zip(syntax_predicates) {
            assert_eq!(
                source_map
                    .syntax_for_node(predicate.hir_id)
                    .expect("predicate has a source-map entry")
                    .text_range(),
                syntax.syntax().text_range()
            );
        }
        assert_eq!(item.span.range, invariant_node.text_range());
        assert_eq!(paths.len().unwrap(), 1);
    }

    #[test]
    fn invariant_owner_recovers_empty_and_malformed_predicate_lists() {
        for (file_id, source, expected_predicates, expect_error_predicate) in [
            (243, "invariant Account {}\n", 0, false),
            (244, "invariant Account { self.balance >= 0\n", 1, false),
            (246, "invariant Account { self.balance >= }\n", 1, true),
        ] {
            let file = crate::FileId::new(file_id);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source:?}");
            let symbols = Interner::new();
            let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
                .expect("item tree collects the diagnosed invariant");
            let entry = item_tree.items.first().expect("invariant item entry");
            let name = symbols.intern("Account").expect("symbol interns");
            let (_packages, paths, owner) =
                owner_context_with_data(&symbols, DefPathData::Invariant(name));
            let expected_path = owner_def_path(&paths, owner);
            let arena = HirArena::new();
            let context = HirLoweringContext::new(
                &ast_ids,
                &snapshot,
                &parsed.root,
                &arena,
                &symbols,
                &paths,
            );

            let (hir_owner, _) =
                lower_invariant_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                    .expect("a diagnosed invariant remains available to later stages");
            let body = hir_owner.body.expect("invariant still owns a body");
            assert_eq!(
                body.invariant_preds.len(),
                expected_predicates,
                "{source:?}"
            );
            assert!(matches!(body.value.kind, ExprKind::Tuple(elements) if elements.is_empty()));
            if expect_error_predicate {
                assert!(matches!(body.invariant_preds[0].kind, ExprKind::Err));
            }
        }
    }

    #[test]
    fn invariant_owner_assigns_nested_definition_ordinals_across_predicates() {
        let file = crate::FileId::new(245);
        let source = "invariant Account { fn() { true }, fn() { false } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("invariant item entry");
        let name = symbols.intern("Account").expect("symbol interns");
        let (_packages, paths, owner) =
            owner_context_with_data(&symbols, DefPathData::Invariant(name));
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_invariant_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("invariant predicates share one owner path preorder");
        let body = hir_owner.body.expect("invariant owns a body");
        assert_eq!(body.invariant_preds.len(), 2);
        for (ordinal, predicate) in body.invariant_preds.iter().enumerate() {
            let ExprKind::Closure(closure) = predicate.kind else {
                panic!("each predicate retains its closure expression");
            };
            let closure_path = paths
                .resolve(closure.def_id)
                .expect("closure definition path resolves")
                .expect("closure definition path exists");
            assert_eq!(
                closure_path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Closure,
                    disambiguator: ordinal as u32,
                })
            );
        }
        assert_eq!(paths.len().unwrap(), 3);
    }

    #[test]
    fn lowers_item_tree_test_into_complete_hir_owner() {
        let file = crate::FileId::new(247);
        let source = "@deprecated(\"legacy test\")\ntest \"line\\nname\" { fn() { true } }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("test item entry");
        assert_eq!(entry.kind, ItemTreeKind::Test);
        let entry_id = entry.ast_id;
        let decoded_name = "line\nname";
        let name = symbols.intern(decoded_name).expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_data(
            &symbols,
            DefPathData::Test(std::sync::Arc::<str>::from(decoded_name)),
        );
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_test_owner(owner, &expected_path, &item_tree, entry_id, &context)
                .expect("test item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level test is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.vis, crate::Visibility::Private);
        assert_eq!(item.name.sym, name);
        assert_eq!(
            symbols.resolve(item.name.sym).unwrap().as_ref(),
            decoded_name
        );
        let ItemKind::Test { name: hir_name } = item.kind else {
            panic!("test item retains its test kind");
        };
        assert_eq!(hir_name, decoded_name);
        let declaration = parsed
            .root
            .descendants()
            .find_map(ast::TestDecl::cast)
            .expect("test declaration");
        let name_token = declaration.name().expect("test name literal");
        assert_eq!(item.name.span.range, name_token.syntax().text_range());

        let body = hir_owner.body.expect("test owns its body");
        assert_eq!(body.owner, owner);
        assert!(body.params.is_empty());
        assert!(body.contracts.is_empty());
        assert!(body.invariant_preds.is_empty());
        let ExprKind::Block(block) = body.value.kind else {
            panic!("test body is represented by a block expression");
        };
        let tail = block
            .tail
            .expect("test block preserves its tail expression");
        let ExprKind::Closure(closure) = tail.kind else {
            panic!("nested closure is retained in the test body");
        };
        let closure_path = paths
            .resolve(closure.def_id)
            .expect("closure path resolves")
            .expect("closure path exists");
        assert_eq!(
            closure_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 0,
            })
        );
        assert_eq!(paths.len().unwrap(), 2);
        assert_eq!(
            source_map
                .syntax_for_node(body.value.hir_id)
                .expect("test body has a source-map entry")
                .text_range(),
            declaration.syntax().text_range()
        );
        assert_eq!(item.span.range, declaration.syntax().text_range());
    }

    #[test]
    fn lowers_item_tree_property_into_typed_hir_owner() {
        let file = crate::FileId::new(249);
        let source = "@deprecated(\"old property\")\nproperty \"line\\npositive\" (var x: I32, _: Bool) { x >= 0 }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("property item entry");
        assert_eq!(entry.kind, ItemTreeKind::Property);
        let decoded_name = "line\npositive";
        let name = symbols.intern(decoded_name).expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_data(
            &symbols,
            DefPathData::Property(std::sync::Arc::<str>::from(decoded_name)),
        );
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, source_map) =
            lower_property_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("property item lowers to a complete owner");
        assert_eq!(hir_owner.id, owner);
        assert_eq!(hir_owner.raw_attrs.len(), 1);
        assert_eq!(
            hir_owner.raw_attrs[0].name,
            crate::item_tree::AttrName::Deprecated
        );
        assert!(hir_owner.diagnostics.is_empty());
        assert_eq!(source_map.owner(), owner);

        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level property is represented by an item owner");
        };
        assert_eq!(item.owner_id, owner);
        assert_eq!(item.name.sym, name);
        assert_eq!(
            symbols.resolve(item.name.sym).unwrap().as_ref(),
            decoded_name
        );
        let ItemKind::Property {
            name: hir_name,
            params,
        } = item.kind
        else {
            panic!("property item retains its property kind");
        };
        assert_eq!(hir_name, decoded_name);
        assert_eq!(params.len(), 2);
        assert!(params[0].mutable);
        assert!(!params[0].is_self);
        assert_eq!(
            params[0].name.expect("named property param").sym,
            symbols.intern("x").unwrap()
        );
        assert!(matches!(params[0].ty.kind, HirTyKind::Path(_)));
        assert!(!params[1].mutable);
        assert!(params[1].name.is_none(), "wildcard parameter has no name");
        assert!(matches!(params[1].ty.kind, HirTyKind::Path(_)));

        let declaration = parsed
            .root
            .descendants()
            .find_map(ast::PropertyDecl::cast)
            .expect("property declaration");
        let name_token = declaration.name().expect("property string name");
        assert_eq!(item.name.span.range, name_token.syntax().text_range());
        assert_eq!(item.span.range, declaration.syntax().text_range());
        let syntax_params = declaration
            .params()
            .expect("property has a parameter list")
            .params()
            .collect::<Vec<_>>();
        for (lowered, syntax) in params.iter().zip(&syntax_params) {
            assert_eq!(
                source_map
                    .syntax_for_node(lowered.hir_id)
                    .expect("declaration parameter has a source-map entry")
                    .text_range(),
                syntax.syntax().text_range()
            );
            let source_type = syntax.ty().expect("well-formed property parameter type");
            assert_eq!(
                source_map
                    .syntax_for_node(lowered.ty.hir_id)
                    .expect("property parameter type has a source-map entry")
                    .text_range(),
                source_type.syntax().text_range()
            );
        }
        let body = hir_owner.body.expect("property owns a body");
        assert_eq!(body.owner, owner);
        assert_eq!(body.params.len(), 2);
        assert_eq!(body.params[0].name, params[0].name);
        assert_eq!(body.params[0].mutable, params[0].mutable);
        assert_ne!(
            body.params[0].hir_id, params[0].hir_id,
            "declaration and body parameter records have distinct HIR IDs"
        );
        assert_eq!(body.params[1].name, params[1].name);
        assert_eq!(body.params[1].mutable, params[1].mutable);
        assert!(body.contracts.is_empty());
        assert!(body.invariant_preds.is_empty());
        assert!(matches!(body.value.kind, ExprKind::Block(_)));
        assert_eq!(
            source_map
                .syntax_for_binding(body.params[0].binding)
                .expect("named body binding has a source-map entry")
                .text_range(),
            syntax_params[0]
                .name()
                .expect("named source parameter")
                .syntax()
                .text_range()
        );
        let path = paths
            .resolve(owner.def_id)
            .expect("property path resolves")
            .expect("property path exists");
        assert!(matches!(
            path.data().last(),
            Some(DisambiguatedDefPathData {
                data: DefPathData::Property(path_name),
                disambiguator: 0
            }) if path_name.as_ref() == decoded_name
        ));
    }

    #[test]
    fn property_owner_supports_no_parameters_and_prepares_nested_paths() {
        let file = crate::FileId::new(250);
        let source = "property \"zero inputs\" () { [fn() { true }, fn() { false }] }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("property item entry");
        let (_packages, paths, owner) = owner_context_with_data(
            &symbols,
            DefPathData::Property(std::sync::Arc::<str>::from("zero inputs")),
        );
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_property_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("parameterless property lowers");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level property is represented by an item owner");
        };
        let ItemKind::Property { params, .. } = item.kind else {
            panic!("property item retains its property kind");
        };
        assert!(params.is_empty());
        let body = hir_owner.body.expect("property owns a body");
        assert!(body.params.is_empty());
        let ExprKind::Block(block) = body.value.kind else {
            panic!("property body is a block");
        };
        let Some(tail) = block.tail else {
            panic!("property array is the block tail");
        };
        let ExprKind::Array(elements) = tail.kind else {
            panic!("property tail retains its array");
        };
        assert_eq!(elements.len(), 2);
        for (ordinal, element) in elements.iter().enumerate() {
            let ExprKind::Closure(closure) = element.kind else {
                panic!("array element remains a closure");
            };
            let closure_path = paths
                .resolve(closure.def_id)
                .expect("closure path resolves")
                .expect("closure path exists");
            assert_eq!(
                closure_path.data().last(),
                Some(&DisambiguatedDefPathData {
                    data: DefPathData::Closure,
                    disambiguator: ordinal as u32,
                })
            );
        }
        assert_eq!(paths.len().unwrap(), 3);
    }

    #[test]
    fn property_owner_recovers_missing_parameter_type_and_body() {
        for (file_id, source, expect_missing_type, expect_missing_body) in [
            (251, "property \"without type\" (value:) {}\n", true, false),
            (252, "property \"without body\" (value: I32)\n", false, true),
        ] {
            let file = crate::FileId::new(file_id);
            let (snapshot, parsed, ast_ids) = parse_file(file, source);
            assert!(!parsed.errors.is_empty(), "{source:?}");
            let symbols = Interner::new();
            let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
                .expect("item tree collects the diagnosed property");
            let entry = item_tree.items.first().expect("property item entry");
            let decoded_name = if expect_missing_type {
                "without type"
            } else {
                "without body"
            };
            let (_packages, paths, owner) = owner_context_with_data(
                &symbols,
                DefPathData::Property(std::sync::Arc::<str>::from(decoded_name)),
            );
            let expected_path = owner_def_path(&paths, owner);
            let arena = HirArena::new();
            let context = HirLoweringContext::new(
                &ast_ids,
                &snapshot,
                &parsed.root,
                &arena,
                &symbols,
                &paths,
            );

            let (hir_owner, _) =
                lower_property_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                    .expect("diagnosed property remains available to later stages");
            let OwnerNode::Item(item) = hir_owner.node else {
                panic!("top-level property is represented by an item owner");
            };
            let ItemKind::Property { params, .. } = item.kind else {
                panic!("property item retains its property kind");
            };
            assert_eq!(params.len(), 1);
            if expect_missing_type {
                assert!(matches!(params[0].ty.kind, HirTyKind::Err));
            }
            let body = hir_owner.body.expect("property still owns a body");
            assert_eq!(body.params.len(), 1);
            assert_eq!(body.params[0].name, params[0].name);
            if expect_missing_body {
                assert!(matches!(body.value.kind, ExprKind::Err));
            } else {
                assert!(matches!(body.value.kind, ExprKind::Block(_)));
            }
        }
    }

    #[test]
    fn property_owner_rejects_a_non_property_definition_path() {
        let file = crate::FileId::new(253);
        let source = "property \"path mismatch\" () {}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry = item_tree.items.first().expect("property item entry");
        let (_packages, paths, owner) = owner_context_with_data(
            &symbols,
            DefPathData::Test(std::sync::Arc::<str>::from("path mismatch")),
        );
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        assert!(matches!(
            lower_property_owner(owner, &expected_path, &item_tree, entry.ast_id, &context),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "rejected owner path does not allocate nested definitions"
        );
    }

    #[test]
    fn test_owner_recovers_a_missing_body() {
        let file = crate::FileId::new(248);
        let source = "test \"missing body\"\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(!parsed.errors.is_empty(), "the missing body is diagnosed");
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects the diagnosed test");
        let entry = item_tree.items.first().expect("test item entry");
        let decoded_name = "missing body";
        let name = symbols.intern(decoded_name).expect("symbol interns");
        let (_packages, paths, owner) = owner_context_with_data(
            &symbols,
            DefPathData::Test(std::sync::Arc::<str>::from(decoded_name)),
        );
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_test_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("missing test body recovers as an error expression");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("top-level test is represented by an item owner");
        };
        assert_eq!(item.name.sym, name);
        let body = hir_owner.body.expect("test still owns a body");
        assert!(matches!(body.value.kind, ExprKind::Err));
    }

    #[test]
    fn function_owner_recovers_invalid_raw_function_names() {
        let file = crate::FileId::new(231);
        let source = "fn r#self() {}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::InvalidRawIdent)
        );
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects a diagnosed function");
        let entry = item_tree.items.first().expect("function item entry");
        assert!(entry.name.is_error());
        let (_packages, paths, owner) = owner_context_with_symbol(&symbols, crate::Symbol::ERROR);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_function_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("a parser-diagnosed name still lowers the function body");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("function is represented by an item owner");
        };
        assert!(item.name.sym.is_error());
        assert!(hir_owner.body.is_some());
    }

    #[test]
    fn function_owner_recovers_reserved_function_names() {
        let file = crate::FileId::new(232);
        let source = "fn null() {}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.iter().any(|error| error.code == Code::NoNull));
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects a diagnosed function");
        let entry = item_tree.items.first().expect("function item entry");
        assert!(entry.name.is_error());
        let (_packages, paths, owner) = owner_context_with_symbol(&symbols, crate::Symbol::ERROR);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        let (hir_owner, _) =
            lower_function_owner(owner, &expected_path, &item_tree, entry.ast_id, &context)
                .expect("a parser-diagnosed reserved name still lowers the function body");
        let OwnerNode::Item(item) = hir_owner.node else {
            panic!("function is represented by an item owner");
        };
        assert!(item.name.sym.is_error());
        assert!(hir_owner.body.is_some());
    }

    #[test]
    fn function_owner_lowering_rejects_non_function_item_entries() {
        let file = crate::FileId::new(230);
        let source = "const VALUE: I32 = 1\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let symbols = Interner::new();
        let item_tree = ItemTree::collect(file, source, &parsed.root, &ast_ids, &symbols)
            .expect("item tree collects");
        let entry_id = item_tree.items[0].ast_id;
        let (_packages, paths, owner) = owner_context(&symbols);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);

        assert!(matches!(
            lower_function_owner(owner, &expected_path, &item_tree, entry_id, &context),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "rejecting an unrelated owner does not intern extra definitions"
        );
    }

    #[test]
    fn function_owner_rejects_item_tree_from_an_older_snapshot() {
        let file = crate::FileId::new(233);
        let old_source = "fn f() {}\n";
        let (_old_snapshot, old_parsed, old_ast_ids) = parse_file(file, old_source);
        let symbols = Interner::new();
        let old_item_tree =
            ItemTree::collect(file, old_source, &old_parsed.root, &old_ast_ids, &symbols)
                .expect("old item tree collects");
        let stale_item_id = old_item_tree.items[0].ast_id;

        let current_source = "fn f() { 1 }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, current_source);
        let current_item_tree =
            ItemTree::collect(file, current_source, &parsed.root, &ast_ids, &symbols)
                .expect("current item tree collects");
        assert_eq!(
            old_item_tree, current_item_tree,
            "snapshot binding does not affect structural incremental equality"
        );
        assert!(
            !old_item_tree.matches_ast_ids(&ast_ids),
            "a same-file AST map from another snapshot does not match"
        );
        assert!(current_item_tree.matches_ast_ids(&ast_ids));

        let (_packages, paths, owner) = owner_context(&symbols);
        let expected_path = owner_def_path(&paths, owner);
        let arena = HirArena::new();
        let context =
            HirLoweringContext::new(&ast_ids, &snapshot, &parsed.root, &arena, &symbols, &paths);
        assert!(matches!(
            lower_function_owner(
                owner,
                &expected_path,
                &old_item_tree,
                stale_item_id,
                &context
            ),
            Err(HirLoweringError::ItemTreeEntryMismatch)
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "stale item trees are rejected before generic definition paths are interned"
        );
    }

    #[test]
    fn lowers_contracts_in_source_order_and_synthesizes_bodiless_body() {
        let file = crate::FileId::new(209);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f(x: I32) requires result > 0 ensures result > x\n",
        );
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::MissingFnBody),
            "the parser owns the missing-body diagnostic"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("contract-only declaration lowers");
        let body = body.expect("contracts need an owning body");
        assert_eq!(body.params.len(), 1);
        assert_eq!(body.contracts.len(), 2);
        assert_eq!(body.contracts[0].kind, ContractKind::Requires);
        assert_eq!(body.contracts[1].kind, ContractKind::Ensures);
        let ExprKind::Binary(_, requires_lhs, _) = body.contracts[0].expr.kind else {
            panic!("requires expression is binary");
        };
        assert!(matches!(requires_lhs.kind, ExprKind::Path(_)));
        let ExprKind::Binary(_, ensures_lhs, _) = body.contracts[1].expr.kind else {
            panic!("ensures expression is binary");
        };
        assert!(matches!(ensures_lhs.kind, ExprKind::ContractResult));
        assert_eq!(signature.requires, &[body.contracts[0].expr.hir_id]);
        assert_eq!(signature.ensures, &[body.contracts[1].expr.hir_id]);
        assert!(matches!(body.value.kind, ExprKind::Tuple(elements) if elements.is_empty()));
        assert_eq!(body.owner, owner);
    }

    #[test]
    fn lowers_result_and_old_only_inside_ensures_clauses() {
        let file = crate::FileId::new(213);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f(x: I32) ensures result > old(old(x)) { old(x) }\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("contract builtins lower");
        let body = body.expect("function body exists");
        let ExprKind::Binary(_, result, old) = body.contracts[0].expr.kind else {
            panic!("ensures expression is a binary expression");
        };
        assert!(matches!(result.kind, ExprKind::ContractResult));
        let ExprKind::ContractOld {
            args: old_args,
            base: None,
        } = old.kind
        else {
            panic!("old call in ensures lowers to ContractOld");
        };
        assert_eq!(old_args.len(), 1);
        assert_eq!(old_args[0].name, None);
        let ExprKind::ContractOld {
            args: inner_args, ..
        } = old_args[0].expr.kind
        else {
            panic!("nested old call also lowers to ContractOld");
        };
        assert_eq!(inner_args.len(), 1);
        let ExprKind::Block(block) = body.value.kind else {
            panic!("function body is a block");
        };
        assert!(matches!(
            block.tail.map(|expression| expression.kind),
            Some(ExprKind::Call { .. })
        ));
    }

    #[test]
    fn contract_old_preserves_malformed_argument_shapes_for_later_diagnostics() {
        let file = crate::FileId::new(214);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn f(x: I32) ensures (old(), old(x, x), old(value: x), old(..x)) {}\n",
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("contract old calls lower");
        let body = body.expect("function body exists");
        let ExprKind::Tuple(calls) = body.contracts[0].expr.kind else {
            panic!("ensures expression is a tuple");
        };
        assert_eq!(calls.len(), 4);
        let [
            Expr {
                kind:
                    ExprKind::ContractOld {
                        args: no_args,
                        base: None,
                    },
                ..
            },
            Expr {
                kind:
                    ExprKind::ContractOld {
                        args: two_args,
                        base: None,
                    },
                ..
            },
            Expr {
                kind:
                    ExprKind::ContractOld {
                        args: named_args,
                        base: None,
                    },
                ..
            },
            Expr {
                kind:
                    ExprKind::ContractOld {
                        args: base_args,
                        base: Some(_),
                    },
                ..
            },
        ] = calls
        else {
            panic!("every unqualified non-raw old call keeps its contract meaning");
        };
        assert!(no_args.is_empty());
        assert_eq!(two_args.len(), 2);
        assert_eq!(named_args.len(), 1);
        assert!(named_args[0].name.is_some());
        assert!(base_args.is_empty());
    }

    #[test]
    fn malformed_borrows_and_effects_sets_do_not_lower_as_empty_sets() {
        let file = crate::FileId::new(215);
        let (snapshot, parsed, ast_ids) = parse_file(
            file,
            "fn missing_borrows() borrows effects {}\n\
             fn missing_effects() effects borrows {}\n\
             fn reserved_effect() effects { alloc, null } {}\n\
             fn unclosed_effects() effects {\n",
        );
        assert!(
            !parsed.errors.is_empty(),
            "the parser owns the syntax errors"
        );
        let declarations = parsed
            .root
            .descendants()
            .filter_map(ast::FnDecl::cast)
            .collect::<Vec<_>>();
        assert_eq!(declarations.len(), 4);

        for (index, declaration) in declarations.iter().enumerate() {
            let function = declaration.syntax().clone();
            let arena = HirArena::new();
            let symbols = Interner::new();
            let (_packages, paths, owner) = owner_context(&symbols);
            let mut source_map =
                HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                    .expect("source map builder")
                    .with_def_path_interner(&paths);

            let (signature, _) = lower_function(declaration, &arena, &symbols, &mut source_map)
                .expect("malformed declarations still lower with recovery");
            match index {
                0 => {
                    assert!(signature.borrows.is_none());
                    assert_eq!(signature.effects.map(|effects| effects.len()), Some(0));
                }
                1 => {
                    assert_eq!(signature.borrows.map(|borrows| borrows.len()), Some(0));
                    assert!(signature.effects.is_none());
                }
                2 => {
                    assert!(
                        signature.effects.is_none(),
                        "a reserved identifier invalidates the complete effect set"
                    );
                }
                3 => {
                    assert!(signature.effects.is_none());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn bodiless_function_without_contracts_has_no_body_parameters() {
        let file = crate::FileId::new(210);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(x: I32)\n");
        assert!(!parsed.errors.is_empty(), "the missing body is diagnosed");
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("bodyless function lowers");
        assert_eq!(signature.params.len(), 1);
        assert!(body.is_none());
        let map = source_map.finish();
        assert!(map.binding_to_syntax().is_empty());
    }

    #[test]
    fn function_lowering_preserves_generic_field_values_in_the_body() {
        let file = crate::FileId::new(211);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f(x: I32) { (0, target.field<I32>) }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("generic field body lowers");
        assert_eq!(signature.params.len(), 1);
        let body = body.expect("function body is present");
        let ExprKind::Block(block) = body.value.kind else {
            panic!("function body is a block");
        };
        let tail = block.tail.expect("tuple is the function body tail");
        let ExprKind::Tuple(elements) = tail.kind else {
            panic!("tuple body remains a tuple");
        };
        assert!(matches!(
            elements[1].kind,
            ExprKind::Field { args: Some(args), .. } if args.args.len() == 1
        ));
        assert_eq!(
            paths.len().unwrap(),
            1,
            "generic field lowering does not intern nested definitions"
        );
        let map = source_map.finish();
        assert!(map.node_to_syntax().len() > 1);
        assert_eq!(map.binding_to_syntax().len(), 1);
    }

    #[test]
    fn lower_function_prepares_owner_wide_nested_definition_paths() {
        let file = crate::FileId::new(212);
        let (snapshot, parsed, ast_ids) =
            parse_file(file, "fn f() requires fn() { true } { fn() {} }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (_, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("function with nested closures lowers");
        let body = body.expect("body exists");
        let ExprKind::Block(block) = body.value.kind else {
            panic!("function body is a block");
        };
        let Some(closure_expr) = block.tail else {
            panic!("closure is the body tail");
        };
        let ExprKind::Closure(closure) = closure_expr.kind else {
            panic!("body tail is a closure");
        };
        assert_eq!(paths.len().unwrap(), 3, "owner plus two closures");
        assert!(
            source_map
                .nested_definition_for_syntax(
                    &function
                        .descendants()
                        .find(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                        .expect("first closure"),
                )
                .unwrap()
                .is_some()
        );
        let path = paths.resolve(closure.def_id).unwrap().unwrap();
        assert_eq!(
            path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 1,
            })
        );
    }

    #[test]
    fn lowers_async_function_with_move_wrapper_and_lexical_nested_paths() {
        let file = crate::FileId::new(216);
        let source = "async fn f(x: I32) -> U8 requires fn() { true } { fn() {} }\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let source_body = declaration.body().expect("async function body");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("async function lowers");
        assert!(signature.is_async);
        assert_eq!(signature.params.len(), 1);
        assert!(matches!(
            signature.ret.map(|ty| ty.kind),
            Some(HirTyKind::Path(_))
        ));
        let body = body.expect("async function has a body");
        assert_eq!(body.params.len(), 1);
        assert_eq!(paths.len().unwrap(), 4, "owner, wrapper, and two closures");

        let ExprKind::Block(outer_block) = body.value.kind else {
            panic!("async function returns a synthetic outer block");
        };
        assert!(outer_block.stmts.is_empty());
        let async_expr = outer_block.tail.expect("async block is the outer tail");
        let ExprKind::AsyncBlock {
            def_id,
            capture,
            block: source_block,
        } = async_expr.kind
        else {
            panic!("async function body is wrapped in an async block");
        };
        assert_eq!(capture, CaptureBy::Move);
        assert_eq!(
            body.value.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::AsyncFn)
        );
        assert_eq!(
            outer_block.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::AsyncFn)
        );
        assert_eq!(
            async_expr.span.ctxt,
            SpanCtxt::Desugared(DesugaringKind::AsyncFn)
        );
        assert_eq!(body.value.span.range, function.text_range());
        assert_eq!(outer_block.span.range, function.text_range());
        assert_eq!(async_expr.span.range, function.text_range());
        assert_eq!(source_block.span.ctxt, SpanCtxt::Root);
        assert_eq!(source_block.span.range, source_body.syntax().text_range());

        let wrapper_path = paths.resolve(def_id).unwrap().expect("wrapper path");
        assert_eq!(
            wrapper_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::AsyncBlock,
                disambiguator: 0,
            })
        );
        let owner_path = paths.resolve(owner.def_id).unwrap().expect("owner path");
        let ExprKind::Closure(contract_closure) = body.contracts[0].expr.kind else {
            panic!("closure in requires stays outside the generated async block");
        };
        let contract_path = paths
            .resolve(contract_closure.def_id)
            .unwrap()
            .expect("contract closure path");
        assert_eq!(
            contract_path.data().len(),
            owner_path.data().len() + 1,
            "contract closures remain direct children of their function"
        );
        assert_eq!(
            contract_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 1,
            })
        );
        assert_eq!(
            &contract_path.data()[..owner_path.data().len()],
            owner_path.data()
        );

        let Some(body_tail) = source_block.tail else {
            panic!("body closure is the async block tail");
        };
        let ExprKind::Closure(body_closure) = body_tail.kind else {
            panic!("body tail remains a closure expression");
        };
        let body_closure_path = paths
            .resolve(body_closure.def_id)
            .unwrap()
            .expect("body closure path");
        assert_eq!(
            body_closure_path.data().last(),
            Some(&DisambiguatedDefPathData {
                data: DefPathData::Closure,
                disambiguator: 2,
            })
        );
        assert_eq!(
            body_closure_path.data()[owner_path.data().len()],
            DisambiguatedDefPathData {
                data: DefPathData::AsyncBlock,
                disambiguator: 0,
            }
        );

        let function_pointer =
            SyntaxNodePtr::new(file, &function).expect("function syntax pointer");
        let body_pointer =
            SyntaxNodePtr::new(file, source_body.syntax()).expect("body syntax pointer");
        let map = source_map.finish();
        for hir_id in [body.value.hir_id, outer_block.hir_id, async_expr.hir_id] {
            assert_eq!(map.syntax_for_node(hir_id), Some(&function_pointer));
        }
        assert_eq!(
            map.syntax_for_node(source_block.hir_id),
            Some(&body_pointer)
        );
    }

    #[test]
    fn async_function_requires_nested_path_interner_before_allocating_hir() {
        let file = crate::FileId::new(217);
        let (snapshot, parsed, ast_ids) = parse_file(file, "async fn f(x: I32) { x }\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        assert_eq!(
            lower_function(&declaration, &arena, &symbols, &mut source_map).unwrap_err(),
            HirLoweringError::DefPathInternerRequired
        );
        assert_eq!(paths.len().unwrap(), 1);
        let map = source_map.finish();
        assert_eq!(map.node_to_syntax().len(), 1);
        assert!(map.binding_to_syntax().is_empty());
    }

    #[test]
    fn bodyless_async_function_has_no_synthetic_async_block() {
        let file = crate::FileId::new(218);
        let (snapshot, parsed, ast_ids) = parse_file(file, "async fn f(x: I32)\n");
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.code == Code::MissingFnBody),
            "the parser owns the missing-body diagnostic"
        );
        let function = function_node(&parsed.root);
        let declaration = ast::FnDecl::cast(function.clone()).expect("function declaration");
        let arena = HirArena::new();
        let symbols = Interner::new();
        let owner = owner_id(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder");

        let (signature, body) = lower_function(&declaration, &arena, &symbols, &mut source_map)
            .expect("bodyless async function lowers");
        assert!(signature.is_async);
        assert!(body.is_none());
        assert!(source_map.finish().binding_to_syntax().is_empty());
    }

    #[test]
    fn lowers_explicit_generic_arguments_on_paths_and_method_calls() {
        let file = crate::FileId::new(219);
        let source = "fn f() {\n    parse<I32, 8>(x)\n    receiver.map<U8, 5>(x)\n    (convert<U16>)(x)\n}\n";
        let (snapshot, parsed, ast_ids) = parse_file(file, source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let function = function_node(&parsed.root);
        let calls = function
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 3);

        let arena = HirArena::new();
        let symbols = Interner::new();
        let (_packages, paths, owner) = owner_context(&symbols);
        let mut source_map =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("source map builder")
                .with_def_path_interner(&paths);

        let path_callee = calls[0].callee().expect("generic path callee");
        let ast::Expr::Path(path_syntax) = &path_callee else {
            panic!("the first generic callee is a path");
        };
        let path_generic_args_syntax = path_syntax
            .generic_args()
            .expect("path has explicit generic arguments");
        let lowered_path = lower_expr(&path_callee, &arena, &symbols, &mut source_map)
            .expect("generic path lowers");
        let ExprKind::Path(path) = lowered_path.kind else {
            panic!("the generic path remains a path expression");
        };
        let path_args = path.segments[0].args.expect("path generic arguments");
        assert_eq!(path_args.args.len(), 2);
        assert!(matches!(
            path_args.args[0],
            HirGenericArg::Type(ty) if matches!(ty.kind, HirTyKind::Path(_))
        ));
        assert_eq!(
            path_args.args[1],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(8),
                negated: false,
            })
        );
        assert_eq!(
            path_args.span.range,
            path_generic_args_syntax.syntax().text_range()
        );

        let method_callee = calls[1].callee().expect("generic method callee");
        let ast::Expr::Field(field_syntax) = &method_callee else {
            panic!("the second generic callee is a field expression");
        };
        let method_generic_args_syntax = direct_generic_args(field_syntax.syntax())
            .expect("method has explicit generic arguments");
        let lowered_method = lower_expr(&method_callee, &arena, &symbols, &mut source_map)
            .expect("generic method lowers");
        let ExprKind::Field {
            args: Some(method_args),
            ..
        } = lowered_method.kind
        else {
            panic!("method field retains its generic arguments");
        };
        assert_eq!(method_args.args.len(), 2);
        assert!(matches!(
            method_args.args[0],
            HirGenericArg::Type(ty) if matches!(ty.kind, HirTyKind::Path(_))
        ));
        assert_eq!(
            method_args.args[1],
            HirGenericArg::Const(HirConstArg::Int {
                value: Some(5),
                negated: false,
            })
        );
        assert_eq!(
            method_args.span.range,
            method_generic_args_syntax.syntax().text_range()
        );

        let parenthesized_callee = calls[2].callee().expect("parenthesized generic callee");
        assert!(
            matches!(parenthesized_callee, ast::Expr::Paren(_)),
            "source retains the callee parentheses"
        );
        let lowered_parenthesized =
            lower_expr(&parenthesized_callee, &arena, &symbols, &mut source_map)
                .expect("a parenthesized generic callee lowers");
        let ExprKind::Path(parenthesized_path) = lowered_parenthesized.kind else {
            panic!("HIR erases callee parentheses");
        };
        assert!(
            parenthesized_path.segments[0].args.is_some(),
            "the generic arguments survive the parentheses"
        );

        let map = source_map.finish();
        let source_ty = path_generic_args_syntax
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::PATH_TYPE)
            .expect("generic type argument syntax");
        let source_ty_ptr =
            SyntaxNodePtr::new(file, &source_ty).expect("generic type syntax pointer");
        let HirGenericArg::Type(lowered_ty) = path_args.args[0] else {
            panic!("first path argument is a type");
        };
        assert_eq!(map.syntax_for_node(lowered_ty.hir_id), Some(&source_ty_ptr));
    }
}
