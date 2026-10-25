//! Conservative conversion of a small set of resolved HIR types and function
//! signatures.
//!
//! This module deliberately returns an unsupported result instead of
//! manufacturing semantic error types or guessing at unresolved names.

use crate::ty::{
    Const, FloatTy, FnPtrSig, FnSig, FnSigConstParam, FnSigGenericParam, FnSigTypeParam,
    GenericArg, IntTy, ParamConst, ParamTy, Region, SigRegion, Ty, TyInterner, TyKind, UintTy,
};
use sai_hir::types::{
    GenericArg as HirGenericArg, GenericParam, GenericParamKind, Generics, PathRoot, Ty as HirTy,
    TyKind as HirTyKind,
};
use sai_hir::{
    DefId, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, OwnerId, Safety, Symbol,
    body::FnSig as HirFnSig,
};
use sai_resolve::package_scopes::{DefKind, PathRes, Res};
use sai_resolve::prelude::PrimTy;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The reason an HIR type cannot be converted by the current subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedTy {
    OwnerMismatch,
    MissingPathResolution,
    IncompletePathResolution,
    QualifiedPath,
    Array,
    ArrayLength,
    Alias,
    GenericArguments,
    WrongNumberOfGenericArguments,
    GenericArgumentKind,
    GenericParameter,
    ConstParameter,
    SelfType,
    Projection,
    NominalType,
    Inference,
    Error,
    WrongNameKind,
    LocalValue,
    TestOnlyPath,
    ErrorPath,
    RegionIndexOverflow,
    FieldRegionWithoutParameter,
    FunctionPointerEffects,
    FunctionPointerRegion,
    FunctionPointerGenericParameter,
    ContextAfterFailure,
}

/// Why a function signature is outside the current partial projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedFnSig {
    ContextAfterFailure,
    NonFunctionItem,
    MissingTypePathResolution,
    GenericParameters,
    WherePredicates,
    Unsafe,
    Async,
    BorrowClause,
    Effects,
    SelfParameter,
    RecoveredGenericSyntax,
    RecoveredFunctionPointerSyntax,
    TooManyTypeParameters,
    ParameterType { index: usize, reason: UnsupportedTy },
    ReturnType(UnsupportedTy),
}

/// The type parameters of a direct local nominal declaration accepted by
/// partial signature lowering.
///
/// The driver constructs this metadata only from the exact current package
/// collection, snapshot, target-filtered HIR owner, and matching DefPath
/// interner. Every parameter must be type-only and unbounded; where clauses
/// and parser-recovered declaration syntax are rejected before construction.
/// Record fields and enum payloads must also be proven not to require an
/// implicit region parameter before the definition enters this allowlist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalNominalDef {
    pub type_params: Arc<[(DefId, Symbol)]>,
}

/// A validated direct local alias template and its ordered type binders.
///
/// The template's `ParamTy` indices belong to the alias binder. The driver
/// constructs this value only from the exact current package collection,
/// snapshot, target-filtered HIR owner, matching DefPath interner, and
/// owner-local path resolutions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTypeAlias {
    pub type_params: Arc<[(DefId, ParamTy)]>,
    pub template: Ty,
}

/// Why a local alias definition cannot be represented by this projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLocalTypeAlias {
    ContextAfterFailure,
    WherePredicates,
    GenericParameters,
    Type(UnsupportedTy),
    RegionBearingType,
    UnusedTypeParameter,
}

/// Region policy and type-path inputs for one HIR owner.
///
/// A context belongs to one owner and snapshot of the caller's choice. Callers
/// must not build it from a `TypeOwnerPathResolution` until that result has
/// been checked against the same owner and workspace snapshot as the HIR.
/// Discard a context after [`lower_ty`] returns an error because a failed
/// conversion may already have visited earlier borrow nodes.
pub struct LowerTyContext<'a> {
    interner: &'a TyInterner,
    owner: OwnerId,
    paths: &'a HashMap<HirId, PathRes>,
    unit_enums: Option<&'a HashSet<DefId>>,
    local_nominals: Option<&'a HashMap<DefId, LocalNominalDef>>,
    local_aliases: Option<&'a HashMap<DefId, LocalTypeAlias>>,
    generic_params: HashMap<DefId, ParamTy>,
    def_paths: Option<&'a DefPathInterner>,
    array_length_pointer_width: Option<u8>,
    array_literal_lengths_only: bool,
    region_mode: RegionMode,
    next_region_index: u32,
    within_fn_ptr: bool,
    const_params: HashMap<DefId, ParamConst>,
    failed: bool,
}

#[derive(Clone, Copy)]
enum RegionMode {
    Signature,
    Body,
    Field { has_implicit_region_parameter: bool },
}

impl<'a> LowerTyContext<'a> {
    /// Create a signature context. Borrow regions receive owner-local indices
    /// starting at zero and are numbered in pre-order across successful calls.
    /// Use [`for_field`](Self::for_field) for declaration field types.
    pub fn for_signature(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: None,
            local_nominals: None,
            local_aliases: None,
            generic_params: HashMap::new(),
            def_paths: None,
            array_length_pointer_width: None,
            array_literal_lengths_only: false,
            region_mode: RegionMode::Signature,
            next_region_index: 0,
            within_fn_ptr: false,
            const_params: HashMap::new(),
            failed: false,
        }
    }

    /// Create a signature context that can lower only the validated local
    /// unit enums explicitly supplied by the caller's matching collection.
    ///
    /// The set must be derived from the same source snapshot and package
    /// collection as `paths`; this crate cannot establish that provenance on
    /// its own. The driver checks and preserves that relationship before
    /// calling this constructor.
    pub fn for_signature_with_unit_enums(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
        unit_enums: &'a HashSet<DefId>,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: Some(unit_enums),
            local_nominals: None,
            local_aliases: None,
            generic_params: HashMap::new(),
            def_paths: None,
            array_length_pointer_width: None,
            array_literal_lengths_only: false,
            region_mode: RegionMode::Signature,
            next_region_index: 0,
            within_fn_ptr: false,
            const_params: HashMap::new(),
            failed: false,
        }
    }

    /// Create a signature context that can lower validated local unit enums
    /// and direct local records/enums with type-only generic arguments.
    ///
    /// The driver derives both sets from the exact current package collection,
    /// source snapshot, and target-filtered HIR. This crate cannot establish
    /// that provenance on its own.
    pub fn for_signature_with_local_nominals(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
        unit_enums: &'a HashSet<DefId>,
        local_nominals: &'a HashMap<DefId, LocalNominalDef>,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: Some(unit_enums),
            local_nominals: Some(local_nominals),
            local_aliases: None,
            generic_params: HashMap::new(),
            def_paths: None,
            array_length_pointer_width: None,
            array_literal_lengths_only: false,
            region_mode: RegionMode::Signature,
            next_region_index: 0,
            within_fn_ptr: false,
            const_params: HashMap::new(),
            failed: false,
        }
    }

    /// Create a declaration-field context.
    ///
    /// When `has_implicit_region_parameter` is true, every borrow in every
    /// field type of the declaration receives `Sig { item: owner, index: 0 }`,
    /// as required for the declaration's single implicit region parameter.
    /// Otherwise any borrow is unsupported; fields without borrows still
    /// lower normally.
    pub fn for_field(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
        has_implicit_region_parameter: bool,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: None,
            local_nominals: None,
            local_aliases: None,
            generic_params: HashMap::new(),
            def_paths: None,
            array_length_pointer_width: None,
            array_literal_lengths_only: false,
            region_mode: RegionMode::Field {
                has_implicit_region_parameter,
            },
            next_region_index: 0,
            within_fn_ptr: false,
            const_params: HashMap::new(),
            failed: false,
        }
    }

    /// Enable only validated local record and enum definitions in a field
    /// context.
    ///
    /// The caller must derive this exact-`DefId` allowlist from the same
    /// package collection and source snapshot as `paths`. This builder is
    /// deliberately inert for signature and body contexts, whose nominal
    /// allowlists are configured separately.
    pub fn with_field_local_nominals(
        mut self,
        local_nominals: &'a HashMap<DefId, LocalNominalDef>,
    ) -> Self {
        if matches!(self.region_mode, RegionMode::Field { .. }) {
            self.local_nominals = Some(local_nominals);
        }
        self
    }

    /// Enable the validated type parameters of one local record while
    /// lowering that record's field templates.
    ///
    /// The caller must supply the exact source-ordered binders validated for
    /// this field owner's current collection and snapshot. This builder is
    /// inert outside field contexts; function signatures and bodies keep
    /// their own binder rules.
    pub fn with_field_type_params(
        mut self,
        type_params: &[(DefId, ParamTy)],
    ) -> Result<Self, UnsupportedTy> {
        if !matches!(self.region_mode, RegionMode::Field { .. }) {
            return Ok(self);
        }

        let mut generic_params = HashMap::with_capacity(type_params.len());
        let mut names = HashSet::with_capacity(type_params.len());
        for (index, (def_id, parameter)) in type_params.iter().enumerate() {
            if u32::try_from(index).ok() != Some(parameter.index)
                || parameter.name.is_error()
                || !names.insert(parameter.name)
                || generic_params.insert(*def_id, *parameter).is_some()
            {
                return Err(UnsupportedTy::GenericParameter);
            }
        }
        self.generic_params = generic_params;
        Ok(self)
    }

    /// Create a body context. Every borrow region is erased.
    pub fn for_body(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: None,
            local_nominals: None,
            local_aliases: None,
            generic_params: HashMap::new(),
            def_paths: None,
            array_length_pointer_width: None,
            array_literal_lengths_only: false,
            region_mode: RegionMode::Body,
            next_region_index: 0,
            within_fn_ptr: false,
            const_params: HashMap::new(),
            failed: false,
        }
    }

    /// Attach the DefPath interner used to construct this context's owner.
    ///
    /// Generic function binders require this provenance so each local
    /// parameter path can be proven to be a direct child of `owner`.
    pub fn with_def_path_interner(mut self, def_paths: &'a DefPathInterner) -> Self {
        self.def_paths = Some(def_paths);
        self
    }

    /// Enable target-width-checked literal lengths for built-in arrays in a
    /// signature context.
    ///
    /// The driver supplies this only while projecting a top-level function
    /// signature from its target-bound collection. Body and field contexts
    /// deliberately remain unable to lower arrays through this adapter.
    pub fn with_array_length_pointer_width(mut self, pointer_width: u8) -> Self {
        if matches!(self.region_mode, RegionMode::Signature) {
            self.array_length_pointer_width = Some(pointer_width);
        }
        self
    }

    /// Enable only target-width-checked integer literal array lengths for a
    /// declaration-field context without an implicit region parameter.
    ///
    /// Unlike signature lowering, field projection does not admit symbolic
    /// const parameters or any other type-shaped length argument. This
    /// builder is deliberately inert for signature, body, and region-bearing
    /// field contexts so this field-specific support cannot widen those
    /// lowering paths.
    pub fn with_field_array_literal_pointer_width(mut self, pointer_width: u8) -> Self {
        if matches!(
            self.region_mode,
            RegionMode::Field {
                has_implicit_region_parameter: false,
            }
        ) {
            self.array_length_pointer_width = Some(pointer_width);
            self.array_literal_lengths_only = true;
        }
        self
    }

    /// Enable only the local, already-lowered type aliases validated by the
    /// caller for this signature projection.
    ///
    /// Alias expansion is intentionally not performed here. The driver
    /// supplies values only for direct same-package aliases whose right-hand
    /// sides were lowered independently and proven free of alias references
    /// and regions.
    pub fn with_local_type_aliases(mut self, aliases: &'a HashMap<DefId, LocalTypeAlias>) -> Self {
        if matches!(self.region_mode, RegionMode::Signature) {
            self.local_aliases = Some(aliases);
        }
        self
    }

    fn region(&mut self) -> Result<Region, UnsupportedTy> {
        match self.region_mode {
            RegionMode::Body => Ok(Region::Erased),
            RegionMode::Field {
                has_implicit_region_parameter: true,
            } => Ok(Region::Sig(SigRegion {
                item: self.owner.def_id.to_def_id(),
                index: 0,
            })),
            RegionMode::Field {
                has_implicit_region_parameter: false,
            } => Err(UnsupportedTy::FieldRegionWithoutParameter),
            RegionMode::Signature => {
                let index = self.next_region_index;
                self.next_region_index = index
                    .checked_add(1)
                    .ok_or(UnsupportedTy::RegionIndexOverflow)?;
                Ok(Region::Sig(SigRegion {
                    item: self.owner.def_id.to_def_id(),
                    index,
                }))
            }
        }
    }
}

/// Convert a resolved HIR type within the supported semantic type subset.
///
/// The caller owns path resolution and diagnostics. This function performs no
/// diagnostic emission and produces no error type; unsupported forms are
/// returned explicitly. A type parameter is accepted only while lowering a
/// function signature with its exact owner-local binder active.
pub fn lower_ty(context: &mut LowerTyContext<'_>, ty: &HirTy<'_>) -> Result<Ty, UnsupportedTy> {
    if context.failed {
        return Err(UnsupportedTy::ContextAfterFailure);
    }
    match lower_ty_inner(context, ty) {
        Ok(ty) => Ok(ty),
        Err(error) => {
            context.failed = true;
            Err(error)
        }
    }
}

/// Lower a deliberately narrow function signature while retaining its
/// parameter and return type information.
///
/// Only unbounded type parameters, `USize` const parameters, safe synchronous
/// signatures, and types supported by this module are represented by the
/// current partial `FnSig`. Type and const binders share source-order indices.
/// Every failure invalidates the context so a partially consumed
/// signature-region sequence cannot be reused. An omitted return type lowers
/// to unit.
pub fn lower_fn_sig(
    context: &mut LowerTyContext<'_>,
    generics: Generics<'_>,
    signature: HirFnSig<'_>,
) -> Result<FnSig, UnsupportedFnSig> {
    if context.failed {
        return Err(UnsupportedFnSig::ContextAfterFailure);
    }

    if !generics.where_preds.is_empty() {
        context.failed = true;
        return Err(UnsupportedFnSig::WherePredicates);
    }

    let mut generic_params = Vec::with_capacity(generics.params.len());
    let mut generic_param_map = HashMap::with_capacity(generics.params.len());
    let mut const_param_map = HashMap::with_capacity(generics.params.len());
    let mut generic_names = HashSet::with_capacity(generics.params.len());
    let generic_owner_path = if generics.params.is_empty() {
        None
    } else {
        let Some(def_paths) = context.def_paths else {
            context.failed = true;
            return Err(UnsupportedFnSig::GenericParameters);
        };
        match def_paths.resolve(context.owner.def_id) {
            Ok(Some(path)) => Some(path),
            Ok(None) | Err(_) => {
                context.failed = true;
                return Err(UnsupportedFnSig::GenericParameters);
            }
        }
    };
    for (index, parameter) in generics.params.iter().enumerate() {
        let parameter_has_exact_owner = generic_owner_path
            .as_ref()
            .zip(context.def_paths)
            .is_some_and(|(owner_path, def_paths)| match parameter.kind {
                GenericParamKind::Type { .. } => {
                    is_direct_owner_type_parameter(def_paths, owner_path, parameter)
                }
                GenericParamKind::Const { .. } => {
                    is_direct_function_const_parameter(def_paths, owner_path, parameter)
                }
            });
        if parameter.name.sym.is_error()
            || !generic_names.insert(parameter.name.sym)
            || !parameter_has_exact_owner
        {
            context.failed = true;
            return Err(UnsupportedFnSig::GenericParameters);
        }
        let index = u32::try_from(index).map_err(|_| {
            context.failed = true;
            UnsupportedFnSig::TooManyTypeParameters
        })?;
        let def_id = parameter.def_id.to_def_id();
        match parameter.kind {
            GenericParamKind::Type { bounds } => {
                if !bounds.is_empty() {
                    context.failed = true;
                    return Err(UnsupportedFnSig::GenericParameters);
                }
                let param = ParamTy {
                    index,
                    name: parameter.name.sym,
                };
                if generic_param_map.insert(def_id, param).is_some() {
                    context.failed = true;
                    return Err(UnsupportedFnSig::GenericParameters);
                }
                let binder = FnSigTypeParam { def_id, param };
                generic_params.push(FnSigGenericParam::Type(binder));
            }
            GenericParamKind::Const { ty } => {
                if !is_builtin_usize_const_type(context, ty) {
                    context.failed = true;
                    return Err(UnsupportedFnSig::GenericParameters);
                }
                let param = ParamConst {
                    index,
                    name: parameter.name.sym,
                };
                if const_param_map.insert(def_id, param).is_some() {
                    context.failed = true;
                    return Err(UnsupportedFnSig::GenericParameters);
                }
                generic_params.push(FnSigGenericParam::Const(FnSigConstParam { def_id, param }));
            }
        }
    }

    let unsupported = if signature.safety == Safety::Unsafe {
        Some(UnsupportedFnSig::Unsafe)
    } else if signature.is_async {
        Some(UnsupportedFnSig::Async)
    } else if signature.borrows.is_some() {
        Some(UnsupportedFnSig::BorrowClause)
    } else if signature.effects.is_some() {
        Some(UnsupportedFnSig::Effects)
    } else if signature.params.iter().any(|param| param.is_self) {
        Some(UnsupportedFnSig::SelfParameter)
    } else {
        None
    };
    if let Some(reason) = unsupported {
        context.failed = true;
        return Err(reason);
    }
    context.generic_params = generic_param_map;
    context.const_params = const_param_map;

    let mut inputs = Vec::with_capacity(signature.params.len());
    for (index, parameter) in signature.params.iter().enumerate() {
        match lower_ty(context, parameter.ty) {
            Ok(ty) => inputs.push(ty),
            Err(reason) => {
                return Err(UnsupportedFnSig::ParameterType { index, reason });
            }
        }
    }
    let output = match signature.ret {
        Some(return_type) => match lower_ty(context, return_type) {
            Ok(ty) => ty,
            Err(reason) => return Err(UnsupportedFnSig::ReturnType(reason)),
        },
        None => context.interner.tuple(Vec::new()),
    };

    Ok(FnSig::new_with_generic_params(
        generic_params,
        inputs,
        output,
    ))
}

/// Lower a direct local alias RHS into a template whose type parameters are
/// indexed in the alias's own source order.
///
/// The caller must first validate the alias declaration's current source
/// owner and syntax recovery. This routine verifies the HIR binder kinds,
/// bounds, owner-local DefPaths, and path-map owner before lowering the RHS.
/// The resulting template is deliberately not expanded here; function
/// signature uses substitute their explicitly lowered arguments into it.
pub fn lower_local_type_alias(
    context: &mut LowerTyContext<'_>,
    generics: Generics<'_>,
    ty: &HirTy<'_>,
) -> Result<LocalTypeAlias, UnsupportedLocalTypeAlias> {
    if context.failed {
        return Err(UnsupportedLocalTypeAlias::ContextAfterFailure);
    }
    if !generics.where_preds.is_empty() {
        context.failed = true;
        return Err(UnsupportedLocalTypeAlias::WherePredicates);
    }

    let owner_path = if generics.params.is_empty() {
        None
    } else {
        let Some(def_paths) = context.def_paths else {
            context.failed = true;
            return Err(UnsupportedLocalTypeAlias::GenericParameters);
        };
        match def_paths.resolve(context.owner.def_id) {
            Ok(Some(path)) if path.package() == context.owner.def_id.package() => Some(path),
            Ok(Some(_)) | Ok(None) | Err(_) => {
                context.failed = true;
                return Err(UnsupportedLocalTypeAlias::GenericParameters);
            }
        }
    };

    let mut type_params = Vec::with_capacity(generics.params.len());
    let mut generic_param_map = HashMap::with_capacity(generics.params.len());
    let mut generic_names = HashSet::with_capacity(generics.params.len());
    for (index, parameter) in generics.params.iter().enumerate() {
        let GenericParamKind::Type { bounds } = parameter.kind else {
            context.failed = true;
            return Err(UnsupportedLocalTypeAlias::GenericParameters);
        };
        let name = parameter.name.sym;
        let parameter_has_exact_owner =
            owner_path
                .as_ref()
                .zip(context.def_paths)
                .is_some_and(|(owner_path, def_paths)| {
                    is_direct_owner_type_parameter(def_paths, owner_path, parameter)
                });
        if !bounds.is_empty()
            || name.is_error()
            || !generic_names.insert(name)
            || !parameter_has_exact_owner
        {
            context.failed = true;
            return Err(UnsupportedLocalTypeAlias::GenericParameters);
        }
        let index = u32::try_from(index).map_err(|_| {
            context.failed = true;
            UnsupportedLocalTypeAlias::GenericParameters
        })?;
        let def_id = parameter.def_id.to_def_id();
        let param = ParamTy { index, name };
        if generic_param_map.insert(def_id, param).is_some() {
            context.failed = true;
            return Err(UnsupportedLocalTypeAlias::GenericParameters);
        }
        type_params.push((def_id, param));
    }

    context.generic_params = generic_param_map;
    context.const_params.clear();
    let template = lower_ty(context, ty).map_err(UnsupportedLocalTypeAlias::Type)?;
    if semantic_type_contains_reference(&template) {
        context.failed = true;
        return Err(UnsupportedLocalTypeAlias::RegionBearingType);
    }
    let Some(used_params) = semantic_type_parameter_usage(&template, type_params.len()) else {
        context.failed = true;
        return Err(UnsupportedLocalTypeAlias::GenericParameters);
    };
    if used_params.iter().any(|used| !used) {
        context.failed = true;
        return Err(UnsupportedLocalTypeAlias::UnusedTypeParameter);
    }

    Ok(LocalTypeAlias {
        type_params: type_params.into(),
        template,
    })
}

fn is_direct_owner_type_parameter(
    def_paths: &DefPathInterner,
    owner_path: &sai_hir::DefPath,
    parameter: &GenericParam<'_>,
) -> bool {
    let Ok(Some(parameter_path)) = def_paths.resolve(parameter.def_id) else {
        return false;
    };
    let owner_components = owner_path.data();
    let parameter_components = parameter_path.data();
    if parameter_path.package() != owner_path.package()
        || parameter_components.len() != owner_components.len() + 1
        || parameter_components[..owner_components.len()] != *owner_components
    {
        return false;
    }

    matches!(
        parameter_components.last(),
        Some(DisambiguatedDefPathData {
            data: DefPathData::TypeParam(name),
            disambiguator: 0,
        }) if *name == parameter.name.sym
    )
}

fn is_direct_function_const_parameter(
    def_paths: &DefPathInterner,
    owner_path: &sai_hir::DefPath,
    parameter: &GenericParam<'_>,
) -> bool {
    let Ok(Some(parameter_path)) = def_paths.resolve(parameter.def_id) else {
        return false;
    };
    let owner_components = owner_path.data();
    let parameter_components = parameter_path.data();
    if parameter_path.package() != owner_path.package()
        || parameter_components.len() != owner_components.len() + 1
        || parameter_components[..owner_components.len()] != *owner_components
    {
        return false;
    }

    matches!(
        parameter_components.last(),
        Some(DisambiguatedDefPathData {
            data: DefPathData::ConstParam(name),
            disambiguator: 0,
        }) if *name == parameter.name.sym
    )
}

fn semantic_type_contains_reference(root: &Ty) -> bool {
    let mut pending = vec![root];
    while let Some(ty) = pending.pop() {
        match ty.kind() {
            TyKind::Ref(..) => return true,
            TyKind::Tuple(elements) => pending.extend(elements.iter()),
            TyKind::Array(element, _) => pending.push(element),
            TyKind::FnPtr(signature) => {
                pending.extend(signature.inputs());
                pending.push(signature.output());
            }
            TyKind::Adt(_, arguments) => pending.extend(arguments.iter()),
            TyKind::Int(_)
            | TyKind::Uint(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Byte
            | TyKind::Char
            | TyKind::Never
            | TyKind::Param(_) => {}
        }
    }
    false
}

fn semantic_type_parameter_usage(root: &Ty, count: usize) -> Option<Vec<bool>> {
    let mut used = vec![false; count];
    let mut pending = vec![root];
    while let Some(ty) = pending.pop() {
        match ty.kind() {
            TyKind::Param(parameter) => {
                *used.get_mut(usize::try_from(parameter.index).ok()?)? = true;
            }
            TyKind::Tuple(elements) => pending.extend(elements.iter()),
            TyKind::Ref(_, inner, _) | TyKind::Array(inner, _) => pending.push(inner),
            TyKind::FnPtr(signature) => {
                pending.extend(signature.inputs());
                pending.push(signature.output());
            }
            TyKind::Adt(_, arguments) => pending.extend(arguments.iter()),
            TyKind::Int(_)
            | TyKind::Uint(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Byte
            | TyKind::Char
            | TyKind::Never => {}
        }
    }
    Some(used)
}

fn is_builtin_usize_const_type(context: &LowerTyContext<'_>, ty: &HirTy<'_>) -> bool {
    if ty.hir_id.owner != context.owner {
        return false;
    }
    let HirTyKind::Path(path) = ty.kind else {
        return false;
    };
    if path.hir_id.owner != context.owner
        || path.root != PathRoot::Plain
        || path.segments.len() != 1
    {
        return false;
    }
    let segment = path.segments[0];
    if segment.hir_id.owner != context.owner || segment.args.is_some() {
        return false;
    }
    let Some(resolution) = context.paths.get(&path.hir_id) else {
        return false;
    };
    resolution.base == Res::PrimTy(PrimTy::USize)
        && resolution.resolved_segments == 1
        && resolution.segment_count == 1
}

fn lower_ty_inner(context: &mut LowerTyContext<'_>, ty: &HirTy<'_>) -> Result<Ty, UnsupportedTy> {
    if ty.hir_id.owner != context.owner {
        return Err(UnsupportedTy::OwnerMismatch);
    }
    match ty.kind {
        HirTyKind::Path(path) => lower_path_ty(context, path),
        HirTyKind::Borrow(mutability, inner) => {
            if inner.hir_id.owner != context.owner {
                return Err(UnsupportedTy::OwnerMismatch);
            }
            if context.within_fn_ptr {
                return Err(UnsupportedTy::FunctionPointerRegion);
            }
            // Borrow positions precede their children in the specified
            // signature-region traversal.
            let region = context.region()?;
            let inner = lower_ty_inner(context, inner)?;
            Ok(context
                .interner
                .intern(TyKind::Ref(region, inner, mutability)))
        }
        HirTyKind::Tuple(elements) => {
            let elements = elements
                .iter()
                .map(|element| lower_ty_inner(context, element))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(context.interner.tuple(elements))
        }
        HirTyKind::FnPtr {
            safety,
            params,
            ret,
            effects,
        } => lower_fn_ptr(context, safety, params, ret, effects),
        HirTyKind::Infer => Err(UnsupportedTy::Inference),
        HirTyKind::Err => Err(UnsupportedTy::Error),
    }
}

fn lower_fn_ptr(
    context: &mut LowerTyContext<'_>,
    safety: Safety,
    params: &[HirTy<'_>],
    ret: Option<&HirTy<'_>>,
    effects: Option<&[sai_hir::types::EffectRef<'_>]>,
) -> Result<Ty, UnsupportedTy> {
    if effects.is_some() {
        return Err(UnsupportedTy::FunctionPointerEffects);
    }

    let previous_within_fn_ptr = context.within_fn_ptr;
    context.within_fn_ptr = true;
    let lowered = (|| {
        let inputs = params
            .iter()
            .map(|parameter| lower_ty_inner(context, parameter))
            .collect::<Result<Vec<_>, _>>()?;
        let output = match ret {
            Some(return_type) => lower_ty_inner(context, return_type)?,
            None => context.interner.tuple(Vec::new()),
        };
        Ok::<_, UnsupportedTy>((inputs, output))
    })();
    context.within_fn_ptr = previous_within_fn_ptr;

    let (inputs, output) = lowered?;
    Ok(context
        .interner
        .intern(TyKind::FnPtr(FnPtrSig::new(safety, inputs, output))))
}

fn lower_path_ty(
    context: &mut LowerTyContext<'_>,
    path: sai_hir::types::Path<'_>,
) -> Result<Ty, UnsupportedTy> {
    if path.hir_id.owner != context.owner
        || path
            .segments
            .iter()
            .any(|segment| segment.hir_id.owner != context.owner)
    {
        return Err(UnsupportedTy::OwnerMismatch);
    }

    match path.root {
        PathRoot::SelfType => {
            return Err(if path.segments.is_empty() {
                UnsupportedTy::SelfType
            } else {
                UnsupportedTy::Projection
            });
        }
        PathRoot::Plain => {}
        PathRoot::SelfValue | PathRoot::Lang(_) => return Err(UnsupportedTy::WrongNameKind),
    }

    let resolution = context
        .paths
        .get(&path.hir_id)
        .copied()
        .ok_or(UnsupportedTy::MissingPathResolution)?;
    let segment_count =
        u32::try_from(path.segments.len()).map_err(|_| UnsupportedTy::IncompletePathResolution)?;
    if resolution.resolved_segments != segment_count || resolution.segment_count != segment_count {
        return Err(UnsupportedTy::IncompletePathResolution);
    }
    if path.segments.is_empty() {
        return Err(UnsupportedTy::QualifiedPath);
    }

    if path.segments[..path.segments.len() - 1]
        .iter()
        .any(|segment| segment.args.is_some())
    {
        return Err(UnsupportedTy::GenericArguments);
    }
    let generic_args = path.segments.last().and_then(|segment| segment.args);

    if resolution.base == Res::PrimTy(PrimTy::Array) {
        return lower_builtin_array(context, &path, generic_args);
    }

    if let Res::Def(DefKind::Alias, def_id) = resolution.base {
        let alias = context
            .local_aliases
            .and_then(|aliases| aliases.get(&def_id))
            .ok_or(UnsupportedTy::Alias)?;
        if alias.type_params.is_empty() {
            if generic_args.is_some() {
                return Err(UnsupportedTy::GenericArguments);
            }
            return Ok(alias.template.clone());
        }

        let Some(generic_args) = generic_args else {
            return Err(UnsupportedTy::WrongNumberOfGenericArguments);
        };
        let written_args = generic_args.args;
        let type_arg_count = written_args
            .iter()
            .filter(|arg| !matches!(arg, HirGenericArg::Binding { .. }))
            .count();
        if type_arg_count != alias.type_params.len() {
            return Err(UnsupportedTy::WrongNumberOfGenericArguments);
        }
        if written_args
            .iter()
            .any(|arg| !matches!(arg, HirGenericArg::Type(_)))
        {
            return Err(UnsupportedTy::GenericArgumentKind);
        }
        let arguments = written_args
            .iter()
            .map(|arg| match arg {
                HirGenericArg::Type(ty) => lower_ty_inner(context, ty).map(GenericArg::Type),
                HirGenericArg::Const(_) | HirGenericArg::Binding { .. } => {
                    Err(UnsupportedTy::GenericArgumentKind)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        return context
            .interner
            .substitute(&alias.template, &arguments)
            .map_err(|_| UnsupportedTy::Alias);
    }

    if let Res::Def(DefKind::Struct | DefKind::Enum, def_id) = resolution.base {
        if let Some(definition) = context
            .local_nominals
            .and_then(|definitions| definitions.get(&def_id))
        {
            let written_args = generic_args.map_or(&[][..], |args| args.args);
            let type_arg_count = written_args
                .iter()
                .filter(|arg| !matches!(arg, HirGenericArg::Binding { .. }))
                .count();
            if type_arg_count != definition.type_params.len() {
                return Err(UnsupportedTy::WrongNumberOfGenericArguments);
            }
            if written_args
                .iter()
                .any(|arg| !matches!(arg, HirGenericArg::Type(_)))
            {
                return Err(UnsupportedTy::GenericArgumentKind);
            }
            let type_args = written_args
                .iter()
                .map(|arg| match arg {
                    HirGenericArg::Type(ty) => lower_ty_inner(context, ty),
                    HirGenericArg::Const(_) | HirGenericArg::Binding { .. } => {
                        Err(UnsupportedTy::GenericArgumentKind)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(context
                .interner
                .intern(TyKind::Adt(def_id, type_args.into())));
        }

        let validated_unit_enum = matches!(resolution.base, Res::Def(DefKind::Enum, _))
            && context
                .unit_enums
                .is_some_and(|unit_enums| unit_enums.contains(&def_id));
        if generic_args.is_some() {
            return Err(UnsupportedTy::GenericArguments);
        }
        return if validated_unit_enum {
            Ok(context.interner.intern(TyKind::Adt(def_id, Arc::from([]))))
        } else {
            Err(UnsupportedTy::NominalType)
        };
    }

    if generic_args.is_some() {
        return Err(UnsupportedTy::GenericArguments);
    }

    if path.segments.len() != 1 {
        return Err(UnsupportedTy::QualifiedPath);
    }

    match resolution.base {
        Res::PrimTy(PrimTy::Array) => Err(UnsupportedTy::Array),
        Res::PrimTy(primitive) => {
            let kind = primitive_ty_kind(primitive)?;
            Ok(context.interner.intern(kind))
        }
        Res::Def(DefKind::Alias, _) => Err(UnsupportedTy::Alias),
        Res::Def(DefKind::TyParam, def_id) => context
            .generic_params
            .get(&def_id)
            .copied()
            .map(|param| context.interner.intern(TyKind::Param(param)))
            .ok_or(if context.within_fn_ptr {
                UnsupportedTy::FunctionPointerGenericParameter
            } else {
                UnsupportedTy::GenericParameter
            }),
        Res::Def(DefKind::ConstParam, _) => Err(UnsupportedTy::ConstParameter),
        Res::Def(DefKind::AssocTy, _) => Err(UnsupportedTy::Projection),
        Res::Def(
            DefKind::Struct
            | DefKind::Enum
            | DefKind::Distinct
            | DefKind::Refined
            | DefKind::Resource,
            _,
        ) => Err(UnsupportedTy::NominalType),
        Res::Def(_, _) => Err(UnsupportedTy::WrongNameKind),
        Res::Local(_) => Err(UnsupportedTy::LocalValue),
        Res::TestOnly(_) => Err(UnsupportedTy::TestOnlyPath),
        Res::Err => Err(UnsupportedTy::ErrorPath),
    }
}

fn lower_builtin_array(
    context: &mut LowerTyContext<'_>,
    path: &sai_hir::types::Path<'_>,
    generic_args: Option<&sai_hir::types::GenericArgs<'_>>,
) -> Result<Ty, UnsupportedTy> {
    if path.root != PathRoot::Plain || path.segments.len() != 1 {
        return Err(UnsupportedTy::QualifiedPath);
    }
    let Some(generic_args) = generic_args else {
        return Err(UnsupportedTy::WrongNumberOfGenericArguments);
    };
    let argument_count = generic_args
        .args
        .iter()
        .filter(|arg| !matches!(arg, HirGenericArg::Binding { .. }))
        .count();
    if argument_count != 2 {
        return Err(UnsupportedTy::WrongNumberOfGenericArguments);
    }
    if generic_args
        .args
        .iter()
        .any(|arg| matches!(arg, HirGenericArg::Binding { .. }))
    {
        return Err(UnsupportedTy::GenericArgumentKind);
    }
    let HirGenericArg::Type(element) = generic_args.args[0] else {
        return Err(UnsupportedTy::GenericArgumentKind);
    };
    let Some(pointer_width) = context.array_length_pointer_width else {
        return Err(UnsupportedTy::Array);
    };
    let length = match generic_args.args[1] {
        HirGenericArg::Const(sai_hir::types::ConstArg::Int {
            value: Some(length),
            negated: false,
        }) if unsigned_value_fits_width(length, pointer_width) => Const::Value(length),
        HirGenericArg::Const(sai_hir::types::ConstArg::Int {
            value: Some(_),
            negated: false,
        }) => return Err(UnsupportedTy::ArrayLength),
        HirGenericArg::Const(_) => return Err(UnsupportedTy::ArrayLength),
        HirGenericArg::Type(_) if context.array_literal_lengths_only => {
            return Err(UnsupportedTy::ArrayLength);
        }
        HirGenericArg::Type(ty) => Const::Param(array_length_const_parameter(context, ty)?),
        HirGenericArg::Binding { .. } => return Err(UnsupportedTy::GenericArgumentKind),
    };
    let element = lower_ty_inner(context, element)?;
    Ok(context.interner.intern(TyKind::Array(element, length)))
}

fn array_length_const_parameter(
    context: &LowerTyContext<'_>,
    ty: &HirTy<'_>,
) -> Result<ParamConst, UnsupportedTy> {
    if ty.hir_id.owner != context.owner {
        return Err(UnsupportedTy::OwnerMismatch);
    }
    let HirTyKind::Path(path) = ty.kind else {
        return Err(UnsupportedTy::GenericArgumentKind);
    };
    if path.hir_id.owner != context.owner {
        return Err(UnsupportedTy::OwnerMismatch);
    }
    if path.root != PathRoot::Plain || path.segments.len() != 1 {
        return Err(UnsupportedTy::ArrayLength);
    }
    let segment = path.segments[0];
    if segment.hir_id.owner != context.owner {
        return Err(UnsupportedTy::OwnerMismatch);
    }
    if segment.args.is_some() {
        return Err(UnsupportedTy::ArrayLength);
    }
    let resolution = context
        .paths
        .get(&path.hir_id)
        .ok_or(UnsupportedTy::ArrayLength)?;
    if resolution.resolved_segments != 1 || resolution.segment_count != 1 {
        return Err(UnsupportedTy::ArrayLength);
    }
    match resolution.base {
        Res::Def(DefKind::ConstParam, def_id) => context
            .const_params
            .get(&def_id)
            .copied()
            .ok_or(UnsupportedTy::ArrayLength),
        Res::Def(DefKind::TyParam, _) | Res::PrimTy(_) => Err(UnsupportedTy::GenericArgumentKind),
        Res::Def(
            DefKind::Struct
            | DefKind::Enum
            | DefKind::Distinct
            | DefKind::Refined
            | DefKind::Resource
            | DefKind::Alias,
            _,
        ) => Err(UnsupportedTy::GenericArgumentKind),
        Res::Def(_, _) | Res::Local(_) | Res::TestOnly(_) | Res::Err => {
            Err(UnsupportedTy::ArrayLength)
        }
    }
}

fn unsigned_value_fits_width(value: u128, width: u8) -> bool {
    match width {
        1..=127 => value < (1u128 << u32::from(width)),
        128 => true,
        _ => false,
    }
}

fn primitive_ty_kind(primitive: PrimTy) -> Result<TyKind, UnsupportedTy> {
    Ok(match primitive {
        PrimTy::I8 => TyKind::Int(IntTy::I8),
        PrimTy::I16 => TyKind::Int(IntTy::I16),
        PrimTy::I32 => TyKind::Int(IntTy::I32),
        PrimTy::I64 => TyKind::Int(IntTy::I64),
        PrimTy::ISize => TyKind::Int(IntTy::ISize),
        PrimTy::U8 => TyKind::Uint(UintTy::U8),
        PrimTy::U16 => TyKind::Uint(UintTy::U16),
        PrimTy::U32 => TyKind::Uint(UintTy::U32),
        PrimTy::U64 => TyKind::Uint(UintTy::U64),
        PrimTy::USize => TyKind::Uint(UintTy::USize),
        PrimTy::F32 => TyKind::Float(FloatTy::F32),
        PrimTy::F64 => TyKind::Float(FloatTy::F64),
        PrimTy::Bool => TyKind::Bool,
        PrimTy::Byte => TyKind::Byte,
        PrimTy::Char => TyKind::Char,
        PrimTy::Never => TyKind::Never,
        PrimTy::Array => return Err(UnsupportedTy::Array),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::arena::HirArena;
    use sai_hir::types::{ConstArg, GenericArgs, Name, Path, PathSegment};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, ItemLocalId,
        Mutability,
    };
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_resolve::package_scopes::ImportIndex;
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::sync::Arc;

    fn owner() -> (sai_hir::OwnerId, Interner) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:types#1.0").expect("package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("path interner");
        let symbol = symbols.intern("item").expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("definition path");
        let owner = sai_hir::OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        };
        (owner, symbols)
    }

    fn owner_with_generic_params(
        names: &[&str],
    ) -> (
        sai_hir::OwnerId,
        Interner,
        Vec<sai_hir::LocalDefId>,
        DefPathInterner,
    ) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:generic-types#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let item_name = symbols.intern("item").unwrap();
        let item_path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(item_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let owner = sai_hir::OwnerId {
            def_id: paths.intern(item_path).unwrap(),
        };
        let parameter_symbols = names
            .iter()
            .map(|name| symbols.intern(name).unwrap())
            .collect::<Vec<_>>();
        let parameters = parameter_symbols
            .iter()
            .copied()
            .map(|name| (name, false, 0))
            .collect::<Vec<_>>();
        let def_ids = paths
            .intern_generic_params(owner.def_id, &parameters)
            .unwrap();
        (owner, symbols, def_ids, paths)
    }

    fn owner_with_same_package_foreign_param() -> (
        sai_hir::OwnerId,
        Interner,
        sai_hir::LocalDefId,
        sai_hir::LocalDefId,
        DefPathInterner,
    ) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:generic-owner#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner_for = |name: &str| {
            let item_name = symbols.intern(name).unwrap();
            let item_path = DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::ValueNs(item_name),
                    disambiguator: 0,
                }],
                &packages,
                &symbols,
            )
            .unwrap();
            sai_hir::OwnerId {
                def_id: paths.intern(item_path).unwrap(),
            }
        };
        let owner = owner_for("first");
        let foreign_owner = owner_for("second");
        let type_name = symbols.intern("T").unwrap();
        let local_param = paths
            .intern_generic_param(owner.def_id, type_name, false, 0)
            .unwrap();
        let foreign_param = paths
            .intern_generic_param(foreign_owner.def_id, type_name, false, 0)
            .unwrap();
        (owner, symbols, local_param, foreign_param, paths)
    }

    fn owner_with_same_package_foreign_const_param() -> (
        sai_hir::OwnerId,
        Interner,
        sai_hir::LocalDefId,
        sai_hir::LocalDefId,
        DefPathInterner,
    ) {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:const-generic-owner#1.0").unwrap();
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let owner_for = |name: &str| {
            let item_name = symbols.intern(name).unwrap();
            let item_path = DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::ValueNs(item_name),
                    disambiguator: 0,
                }],
                &packages,
                &symbols,
            )
            .unwrap();
            sai_hir::OwnerId {
                def_id: paths.intern(item_path).unwrap(),
            }
        };
        let owner = owner_for("first");
        let foreign_owner = owner_for("second");
        let const_name = symbols.intern("N").unwrap();
        let local_param = paths
            .intern_generic_param(owner.def_id, const_name, true, 0)
            .unwrap();
        let foreign_param = paths
            .intern_generic_param(foreign_owner.def_id, const_name, true, 0)
            .unwrap();
        (owner, symbols, local_param, foreign_param, paths)
    }

    fn hir_id(owner: sai_hir::OwnerId, local_id: u32) -> HirId {
        HirId {
            owner,
            local_id: ItemLocalId::from_raw(local_id),
        }
    }

    fn span() -> sai_hir::Span {
        sai_hir::Span {
            file: FileId::new(1),
            range: TextRange::empty(TextSize::new(0)),
            ctxt: sai_hir::SpanCtxt::Root,
        }
    }

    fn resolved_path<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: sai_hir::OwnerId,
        symbols: &Interner,
        local_id: u32,
        spelling: &str,
        args: Option<&'hir sai_hir::types::GenericArgs<'hir>>,
        resolved: Res,
    ) -> (&'hir HirTy<'hir>, HashMap<HirId, PathRes>) {
        let path_id = hir_id(owner, local_id);
        let segment = arena.alloc_path_segment(PathSegment {
            hir_id: hir_id(owner, local_id + 1),
            name: Name {
                sym: symbols.intern(spelling).expect("symbol interns"),
                span: span(),
            },
            args,
        });
        let segments = arena.alloc_path_segments(&[*segment]);
        let path = arena.alloc_path(Path {
            hir_id: path_id,
            root: PathRoot::Plain,
            segments,
            span: span(),
        });
        let ty = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, local_id + 2),
            kind: HirTyKind::Path(*path),
            span: span(),
        });
        let paths = HashMap::from([(
            path_id,
            PathRes {
                base: resolved,
                resolved_segments: 1,
                segment_count: 1,
            },
        )]);
        (ty, paths)
    }

    fn resolved_array_path<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: sai_hir::OwnerId,
        symbols: &Interner,
        local_id: u32,
        args: &[HirGenericArg<'hir>],
    ) -> (&'hir HirTy<'hir>, HashMap<HirId, PathRes>) {
        let args = arena.alloc_generic_args_list(args);
        let generic_args = arena.alloc_generic_args(GenericArgs { args, span: span() });
        resolved_path(
            arena,
            owner,
            symbols,
            local_id,
            "Array",
            Some(generic_args),
            Res::PrimTy(PrimTy::Array),
        )
    }

    fn resolved_qualified_path<'hir>(
        arena: &'hir HirArena<'hir>,
        owner: sai_hir::OwnerId,
        symbols: &Interner,
        local_id: u32,
        prefix: &str,
        name: &str,
        resolved_segments: u32,
        resolved: Res,
    ) -> (&'hir HirTy<'hir>, HashMap<HirId, PathRes>) {
        let path_id = hir_id(owner, local_id);
        let prefix = arena.alloc_path_segment(PathSegment {
            hir_id: hir_id(owner, local_id + 1),
            name: Name {
                sym: symbols.intern(prefix).expect("symbol interns"),
                span: span(),
            },
            args: None,
        });
        let segment = arena.alloc_path_segment(PathSegment {
            hir_id: hir_id(owner, local_id + 2),
            name: Name {
                sym: symbols.intern(name).expect("symbol interns"),
                span: span(),
            },
            args: None,
        });
        let segments = arena.alloc_path_segments(&[*prefix, *segment]);
        let path = arena.alloc_path(Path {
            hir_id: path_id,
            root: PathRoot::Plain,
            segments,
            span: span(),
        });
        let ty = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, local_id + 3),
            kind: HirTyKind::Path(*path),
            span: span(),
        });
        let paths = HashMap::from([(
            path_id,
            PathRes {
                base: resolved,
                resolved_segments,
                segment_count: 2,
            },
        )]);
        (ty, paths)
    }

    #[test]
    fn validated_unit_enum_paths_lower_only_with_the_supplied_definition_set() {
        let (owner, symbols) = owner();
        let enum_def = owner.def_id.to_def_id();
        let unit_enums = HashSet::from([enum_def]);
        let interner = TyInterner::new();
        let arena = HirArena::new();

        let (direct, direct_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            60,
            "State",
            None,
            Res::Def(DefKind::Enum, enum_def),
        );
        let (qualified, qualified_paths) = resolved_qualified_path(
            &arena,
            owner,
            &symbols,
            70,
            "api",
            "State",
            2,
            Res::Def(DefKind::Enum, enum_def),
        );
        let mut direct_context = LowerTyContext::for_signature_with_unit_enums(
            &interner,
            owner,
            &direct_paths,
            &unit_enums,
        );
        let direct_ty = lower_ty(&mut direct_context, direct).expect("validated unit enum lowers");
        assert_eq!(direct_ty.kind(), &TyKind::Adt(enum_def, Arc::from([])));

        let mut qualified_context = LowerTyContext::for_signature_with_unit_enums(
            &interner,
            owner,
            &qualified_paths,
            &unit_enums,
        );
        let qualified_ty =
            lower_ty(&mut qualified_context, qualified).expect("qualified unit enum lowers");
        assert!(direct_ty.ptr_eq(&qualified_ty));

        let mut unsupported_context =
            LowerTyContext::for_signature(&interner, owner, &direct_paths);
        assert_eq!(
            lower_ty(&mut unsupported_context, direct),
            Err(UnsupportedTy::NominalType),
            "an enum does not become supported without validated provenance"
        );
    }

    #[test]
    fn unit_enum_lowering_rejects_generic_and_incompletely_resolved_paths() {
        let (owner, symbols) = owner();
        let enum_def = owner.def_id.to_def_id();
        let unit_enums = HashSet::from([enum_def]);
        let interner = TyInterner::new();
        let arena = HirArena::new();

        let empty_args = arena.alloc_generic_args(sai_hir::types::GenericArgs {
            args: &[],
            span: span(),
        });
        let (generic, generic_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            80,
            "State",
            Some(empty_args),
            Res::Def(DefKind::Enum, enum_def),
        );
        let mut generic_context = LowerTyContext::for_signature_with_unit_enums(
            &interner,
            owner,
            &generic_paths,
            &unit_enums,
        );
        assert_eq!(
            lower_ty(&mut generic_context, generic),
            Err(UnsupportedTy::GenericArguments),
            "even empty explicit generic arguments are not erased"
        );

        let (partial, partial_paths) = resolved_qualified_path(
            &arena,
            owner,
            &symbols,
            90,
            "api",
            "State",
            1,
            Res::Def(DefKind::Enum, enum_def),
        );
        let mut partial_context = LowerTyContext::for_signature_with_unit_enums(
            &interner,
            owner,
            &partial_paths,
            &unit_enums,
        );
        assert_eq!(
            lower_ty(&mut partial_context, partial),
            Err(UnsupportedTy::IncompletePathResolution)
        );
    }

    #[test]
    fn primitive_paths_lower_to_the_matching_semantic_types() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let cases = [
            ("I8", PrimTy::I8, TyKind::Int(IntTy::I8)),
            ("I16", PrimTy::I16, TyKind::Int(IntTy::I16)),
            ("I32", PrimTy::I32, TyKind::Int(IntTy::I32)),
            ("I64", PrimTy::I64, TyKind::Int(IntTy::I64)),
            ("ISize", PrimTy::ISize, TyKind::Int(IntTy::ISize)),
            ("U8", PrimTy::U8, TyKind::Uint(UintTy::U8)),
            ("U16", PrimTy::U16, TyKind::Uint(UintTy::U16)),
            ("U32", PrimTy::U32, TyKind::Uint(UintTy::U32)),
            ("U64", PrimTy::U64, TyKind::Uint(UintTy::U64)),
            ("USize", PrimTy::USize, TyKind::Uint(UintTy::USize)),
            ("F32", PrimTy::F32, TyKind::Float(FloatTy::F32)),
            ("F64", PrimTy::F64, TyKind::Float(FloatTy::F64)),
            ("Bool", PrimTy::Bool, TyKind::Bool),
            ("Byte", PrimTy::Byte, TyKind::Byte),
            ("Char", PrimTy::Char, TyKind::Char),
            ("Never", PrimTy::Never, TyKind::Never),
        ];

        for (index, (spelling, primitive, expected)) in cases.into_iter().enumerate() {
            let local_id = (index as u32) * 3;
            let (hir_ty, paths) = resolved_path(
                &arena,
                owner,
                &symbols,
                local_id,
                spelling,
                None,
                Res::PrimTy(primitive),
            );
            let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
            assert_eq!(lower_ty(&mut context, &hir_ty).unwrap().kind(), &expected);
        }
    }

    #[test]
    fn unit_tuple_and_nested_references_lower_with_regions() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let unit_children: [HirTy<'_>; 0] = [];
        let unit = HirTy {
            hir_id: hir_id(owner, 30),
            kind: HirTyKind::Tuple(&unit_children),
            span: span(),
        };
        let (boolean, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            40,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let inner_reference = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 51),
            kind: HirTyKind::Borrow(Mutability::Not, boolean),
            span: span(),
        });
        let reference = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 50),
            kind: HirTyKind::Borrow(Mutability::Mut, inner_reference),
            span: span(),
        });

        let unit = arena.alloc_ty(unit);
        let empty_paths = HashMap::new();
        let mut unit_context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        let unit_semantic = lower_ty(&mut unit_context, unit).unwrap();
        assert_eq!(unit_semantic.kind(), &TyKind::Tuple(Arc::from([])));

        let mut signature_context = LowerTyContext::for_signature(&interner, owner, &paths);
        let reference_semantic = lower_ty(&mut signature_context, reference).unwrap();
        let TyKind::Ref(Region::Sig(outer), inner, Mutability::Mut) = reference_semantic.kind()
        else {
            panic!("outer reference lowered");
        };
        assert_eq!(outer.item, owner.def_id.to_def_id());
        assert_eq!(outer.index, 0);
        assert!(matches!(
            inner.kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 1, .. }), _, Mutability::Not)
        ));
        let following_reference = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 55),
            kind: HirTyKind::Borrow(Mutability::Not, boolean),
            span: span(),
        });
        let following_semantic = lower_ty(&mut signature_context, following_reference).unwrap();
        assert!(matches!(
            following_semantic.kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 2, .. }), _, Mutability::Not)
        ));

        let mut body_context = LowerTyContext::for_body(&interner, owner, &paths);
        let erased = lower_ty(&mut body_context, reference).unwrap();
        assert!(matches!(
            erased.kind(),
            TyKind::Ref(Region::Erased, inner, Mutability::Mut)
                if matches!(inner.kind(), TyKind::Ref(Region::Erased, _, Mutability::Not))
        ));

        let tuple_children = arena.alloc_tys(&[*boolean, *boolean]);
        let tuple = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 60),
            kind: HirTyKind::Tuple(tuple_children),
            span: span(),
        });
        let mut tuple_context = LowerTyContext::for_signature(&interner, owner, &paths);
        let tuple_semantic = lower_ty(&mut tuple_context, tuple).unwrap();
        let boolean_semantic = interner.intern(TyKind::Bool);
        assert_eq!(
            tuple_semantic.kind(),
            &TyKind::Tuple(Arc::from([boolean_semantic.clone(), boolean_semantic]))
        );
    }

    #[test]
    fn field_borrows_share_the_declaration_implicit_region() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (boolean, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            180,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let nested_borrow = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 190),
            kind: HirTyKind::Borrow(
                Mutability::Mut,
                arena.alloc_ty(HirTy {
                    hir_id: hir_id(owner, 191),
                    kind: HirTyKind::Borrow(Mutability::Not, boolean),
                    span: span(),
                }),
            ),
            span: span(),
        });
        let second_borrow = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 192),
            kind: HirTyKind::Borrow(Mutability::Not, boolean),
            span: span(),
        });
        let children = arena.alloc_tys(&[*nested_borrow, *second_borrow]);
        let field_tuple = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 193),
            kind: HirTyKind::Tuple(children),
            span: span(),
        });

        let mut context = LowerTyContext::for_field(&interner, owner, &paths, true);
        let semantic = lower_ty(&mut context, field_tuple).unwrap();
        let TyKind::Tuple(elements) = semantic.kind() else {
            panic!("field tuple lowers as a tuple");
        };
        assert!(matches!(
            elements[0].kind(),
            TyKind::Ref(
                Region::Sig(SigRegion { item, index: 0 }),
                inner,
                Mutability::Mut
            ) if *item == owner.def_id.to_def_id()
                && matches!(
                    inner.kind(),
                    TyKind::Ref(
                        Region::Sig(SigRegion { item, index: 0 }),
                        _,
                        Mutability::Not
                    ) if *item == owner.def_id.to_def_id()
                )
        ));
        assert!(matches!(
            elements[1].kind(),
            TyKind::Ref(
                Region::Sig(SigRegion { item, index: 0 }),
                _,
                Mutability::Not
            ) if *item == owner.def_id.to_def_id()
        ));

        let mut context = LowerTyContext::for_field(&interner, owner, &paths, false);
        assert_eq!(
            lower_ty(&mut context, second_borrow),
            Err(UnsupportedTy::FieldRegionWithoutParameter)
        );
    }

    #[test]
    fn field_array_context_accepts_only_target_fit_literal_lengths() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (byte, byte_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            10,
            "U8",
            None,
            Res::PrimTy(PrimTy::U8),
        );

        for (pointer_width, largest, first_overflow) in [
            (32, (1u128 << 32) - 1, 1u128 << 32),
            (64, (1u128 << 64) - 1, 1u128 << 64),
        ] {
            for length in [0, largest] {
                let args = [
                    HirGenericArg::Type(byte),
                    HirGenericArg::Const(ConstArg::Int {
                        value: Some(length),
                        negated: false,
                    }),
                ];
                let (array, mut paths) = resolved_array_path(
                    &arena,
                    owner,
                    &symbols,
                    100 + u32::from(pointer_width),
                    &args,
                );
                paths.extend(byte_paths.clone());
                let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
                    .with_field_array_literal_pointer_width(pointer_width);
                assert!(matches!(
                    lower_ty(&mut context, array).map(|ty| ty.kind().clone()),
                    Ok(TyKind::Array(_, Const::Value(actual))) if actual == length
                ));
            }

            let overflow_args = [
                HirGenericArg::Type(byte),
                HirGenericArg::Const(ConstArg::Int {
                    value: Some(first_overflow),
                    negated: false,
                }),
            ];
            let (overflow, mut paths) = resolved_array_path(
                &arena,
                owner,
                &symbols,
                200 + u32::from(pointer_width),
                &overflow_args,
            );
            paths.extend(byte_paths.clone());
            let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
                .with_field_array_literal_pointer_width(pointer_width);
            assert_eq!(
                lower_ty(&mut context, overflow),
                Err(UnsupportedTy::ArrayLength),
                "the first non-representable USize value fails closed at {pointer_width} bits"
            );
        }

        let (inner, mut inner_paths) = resolved_array_path(
            &arena,
            owner,
            &symbols,
            300,
            &[
                HirGenericArg::Type(byte),
                HirGenericArg::Const(ConstArg::Int {
                    value: Some(2),
                    negated: false,
                }),
            ],
        );
        let (outer, mut outer_paths) = resolved_array_path(
            &arena,
            owner,
            &symbols,
            320,
            &[
                HirGenericArg::Type(inner),
                HirGenericArg::Const(ConstArg::Int {
                    value: Some(3),
                    negated: false,
                }),
            ],
        );
        inner_paths.extend(byte_paths.clone());
        outer_paths.extend(inner_paths);
        let mut context = LowerTyContext::for_field(&interner, owner, &outer_paths, false)
            .with_field_array_literal_pointer_width(32);
        let lowered = lower_ty(&mut context, outer).expect("nested literal arrays lower");
        let TyKind::Array(inner, Const::Value(3)) = lowered.kind() else {
            panic!("the outer literal array shape is retained");
        };
        assert!(matches!(inner.kind(), TyKind::Array(_, Const::Value(2))));
        let mut region_bearing_context =
            LowerTyContext::for_field(&interner, owner, &outer_paths, true)
                .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut region_bearing_context, outer),
            Err(UnsupportedTy::Array),
            "region-bearing field contexts remain outside this literal-array adapter"
        );

        let invalid_lengths = [
            ConstArg::Int {
                value: Some(1),
                negated: true,
            },
            ConstArg::Int {
                value: None,
                negated: false,
            },
            ConstArg::Bool(true),
            ConstArg::Char('x'),
        ];
        for (index, length) in invalid_lengths.into_iter().enumerate() {
            let args = [HirGenericArg::Type(byte), HirGenericArg::Const(length)];
            let (array, mut paths) =
                resolved_array_path(&arena, owner, &symbols, 400 + index as u32 * 10, &args);
            paths.extend(byte_paths.clone());
            let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
                .with_field_array_literal_pointer_width(32);
            assert_eq!(
                lower_ty(&mut context, array),
                Err(UnsupportedTy::ArrayLength)
            );
        }

        let type_length_args = [HirGenericArg::Type(byte), HirGenericArg::Type(byte)];
        let (type_length, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 460, &type_length_args);
        paths.extend(byte_paths.clone());
        let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
            .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, type_length),
            Err(UnsupportedTy::ArrayLength),
            "type-shaped lengths and symbolic const substitution remain unsupported"
        );

        let mut signature_context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut signature_context, type_length),
            Err(UnsupportedTy::Array),
            "the field-only builder cannot enable signature array lowering"
        );
        let mut body_context = LowerTyContext::for_body(&interner, owner, &paths)
            .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut body_context, type_length),
            Err(UnsupportedTy::Array),
            "the field-only builder cannot enable body array lowering"
        );

        let wrong_element_kind_args = [
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1),
                negated: false,
            }),
            HirGenericArg::Type(byte),
        ];
        let (wrong_element_kind, paths) =
            resolved_array_path(&arena, owner, &symbols, 480, &wrong_element_kind_args);
        let mut paths = paths;
        paths.extend(byte_paths.clone());
        let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
            .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, wrong_element_kind),
            Err(UnsupportedTy::GenericArgumentKind)
        );

        let binding_args = [
            HirGenericArg::Type(byte),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1),
                negated: false,
            }),
            HirGenericArg::Binding {
                name: sai_hir::types::Name {
                    sym: symbols.intern("Item").unwrap(),
                    span: span(),
                },
                ty: byte,
            },
        ];
        let (with_binding, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 500, &binding_args);
        paths.extend(byte_paths);
        let mut context = LowerTyContext::for_field(&interner, owner, &paths, false)
            .with_field_array_literal_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, with_binding),
            Err(UnsupportedTy::GenericArgumentKind),
            "associated bindings remain unsupported"
        );
    }

    #[test]
    fn built_in_array_types_require_target_fit_nonnegative_integer_lengths() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (boolean, boolean_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            10,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let borrowed_element = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 20),
            kind: HirTyKind::Borrow(Mutability::Not, boolean),
            span: span(),
        });
        let valid_args = [
            HirGenericArg::Type(borrowed_element),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(4),
                negated: false,
            }),
        ];
        let (array, mut paths) = resolved_array_path(&arena, owner, &symbols, 30, &valid_args);
        paths.extend(boolean_paths.clone());
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_array_length_pointer_width(32);
        let semantic = lower_ty(&mut context, array).expect("the resolved built-in array lowers");
        let TyKind::Array(element, Const::Value(4)) = semantic.kind() else {
            panic!("the array retains its element type and literal length");
        };
        assert!(matches!(
            element.kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 0, .. }), _, Mutability::Not)
        ));

        let following_borrow = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 40),
            kind: HirTyKind::Borrow(Mutability::Mut, boolean),
            span: span(),
        });
        let following = lower_ty(&mut context, following_borrow)
            .expect("array element lowering preserves later signature region order");
        assert!(matches!(
            following.kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 1, .. }), _, Mutability::Mut)
        ));

        let overflow_args = [
            HirGenericArg::Type(boolean),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1u128 << 32),
                negated: false,
            }),
        ];
        let (overflow, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 50, &overflow_args);
        paths.extend(boolean_paths.clone());
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_array_length_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, overflow),
            Err(UnsupportedTy::ArrayLength),
            "the first value outside USize fails closed"
        );

        for (local_id, argument) in [
            (
                60,
                ConstArg::Int {
                    value: Some(1),
                    negated: true,
                },
            ),
            (70, ConstArg::Bool(true)),
            (80, ConstArg::Char('x')),
        ] {
            let args = [HirGenericArg::Type(boolean), HirGenericArg::Const(argument)];
            let (invalid, mut paths) =
                resolved_array_path(&arena, owner, &symbols, local_id, &args);
            paths.extend(boolean_paths.clone());
            let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
                .with_array_length_pointer_width(32);
            assert_eq!(
                lower_ty(&mut context, invalid),
                Err(UnsupportedTy::ArrayLength)
            );
        }

        let type_length_args = [HirGenericArg::Type(boolean), HirGenericArg::Type(boolean)];
        let (wrong_kind, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 90, &type_length_args);
        paths.extend(boolean_paths.clone());
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_array_length_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, wrong_kind),
            Err(UnsupportedTy::GenericArgumentKind)
        );

        let binding_args = [
            HirGenericArg::Type(boolean),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(4),
                negated: false,
            }),
            HirGenericArg::Binding {
                name: Name {
                    sym: symbols.intern("Item").expect("binding name interns"),
                    span: span(),
                },
                ty: boolean,
            },
        ];
        let (with_binding, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 95, &binding_args);
        paths.extend(boolean_paths.clone());
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_array_length_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, with_binding),
            Err(UnsupportedTy::GenericArgumentKind),
            "associated bindings do not count toward array arity and remain unsupported"
        );

        let no_target_width_args = [
            HirGenericArg::Type(boolean),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1),
                negated: false,
            }),
        ];
        let (without_width, mut paths) =
            resolved_array_path(&arena, owner, &symbols, 100, &no_target_width_args);
        paths.extend(boolean_paths);
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, without_width),
            Err(UnsupportedTy::Array)
        );
    }

    #[test]
    fn unsupported_hir_and_path_forms_fail_closed() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (array, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            60,
            "Array",
            None,
            Res::PrimTy(PrimTy::Array),
        );
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, array),
            Err(UnsupportedTy::WrongNumberOfGenericArguments)
        );
        assert_eq!(
            lower_ty(&mut context, array),
            Err(UnsupportedTy::ContextAfterFailure)
        );

        let function = HirTy {
            hir_id: hir_id(owner, 70),
            kind: HirTyKind::FnPtr {
                safety: sai_hir::Safety::Safe,
                params: &[],
                ret: None,
                effects: None,
            },
            span: span(),
        };
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        let lowered = lower_ty(&mut context, &function)
            .expect("a pure function pointer with no arguments lowers");
        let TyKind::FnPtr(signature) = lowered.kind() else {
            panic!("the result is a function pointer");
        };
        assert_eq!(signature.safety(), sai_hir::Safety::Safe);
        assert!(signature.inputs().is_empty());
        assert!(
            matches!(signature.output().kind(), TyKind::Tuple(elements) if elements.is_empty())
        );

        let inference = HirTy {
            hir_id: hir_id(owner, 71),
            kind: HirTyKind::Infer,
            span: span(),
        };
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        assert_eq!(
            lower_ty(&mut context, &inference),
            Err(UnsupportedTy::Inference)
        );
        let error = HirTy {
            hir_id: hir_id(owner, 72),
            kind: HirTyKind::Err,
            span: span(),
        };
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        assert_eq!(lower_ty(&mut context, &error), Err(UnsupportedTy::Error));
    }

    #[test]
    fn function_pointer_types_preserve_safety_order_and_supported_children() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (i32_ty, mut paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            10,
            "I32",
            None,
            Res::PrimTy(PrimTy::I32),
        );
        let (bool_ty, bool_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            20,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let (u8_ty, u8_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            30,
            "U8",
            None,
            Res::PrimTy(PrimTy::U8),
        );
        let (char_ty, char_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            40,
            "Char",
            None,
            Res::PrimTy(PrimTy::Char),
        );
        paths.extend(bool_paths);
        paths.extend(u8_paths);
        paths.extend(char_paths);

        let tuple_elements = arena.alloc_tys(&[*i32_ty, *bool_ty]);
        let tuple = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 50),
            kind: HirTyKind::Tuple(tuple_elements),
            span: span(),
        });
        let array_args = [
            HirGenericArg::Type(u8_ty),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(2),
                negated: false,
            }),
        ];
        let (array, array_paths) = resolved_array_path(&arena, owner, &symbols, 60, &array_args);
        paths.extend(array_paths);
        let params = arena.alloc_tys(&[*tuple, *array]);
        let function = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 70),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params,
                ret: Some(char_ty),
                effects: None,
            },
            span: span(),
        });

        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_array_length_pointer_width(32);
        let lowered = lower_ty(&mut context, function).expect("the pure function pointer lowers");
        let TyKind::FnPtr(signature) = lowered.kind() else {
            panic!("the result retains a function pointer signature");
        };
        assert_eq!(signature.safety(), Safety::Safe);
        assert_eq!(signature.inputs().len(), 2);
        assert!(matches!(
            signature.inputs()[0].kind(),
            TyKind::Tuple(elements)
                if matches!(elements[0].kind(), TyKind::Int(IntTy::I32))
                    && matches!(elements[1].kind(), TyKind::Bool)
        ));
        assert!(matches!(
            signature.inputs()[1].kind(),
            TyKind::Array(element, Const::Value(2))
                if matches!(element.kind(), TyKind::Uint(UintTy::U8))
        ));
        assert!(matches!(signature.output().kind(), TyKind::Char));

        let no_params_no_return = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 80),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: &[],
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let unit = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 81),
            kind: HirTyKind::Tuple(&[]),
            span: span(),
        });
        let explicit_unit = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 82),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: &[],
                ret: Some(unit),
                effects: None,
            },
            span: span(),
        });
        let unsafe_no_params = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 83),
            kind: HirTyKind::FnPtr {
                safety: Safety::Unsafe,
                params: &[],
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        let omitted = lower_ty(&mut context, no_params_no_return).unwrap();
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        let explicit = lower_ty(&mut context, explicit_unit).unwrap();
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        let unsafe_pointer = lower_ty(&mut context, unsafe_no_params).unwrap();
        assert!(omitted.ptr_eq(&explicit), "omitted output is unit");
        assert!(
            !omitted.ptr_eq(&unsafe_pointer),
            "safety participates in identity"
        );
        assert!(matches!(
            unsafe_pointer.kind(),
            TyKind::FnPtr(signature) if signature.safety() == Safety::Unsafe
        ));
    }

    #[test]
    fn function_pointer_effects_regions_parameters_and_invalid_paths_fail_closed() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (boolean, bool_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            10,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let pure_effects = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 20),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: &[],
                ret: None,
                effects: Some(&[]),
            },
            span: span(),
        });
        let effect_refs = arena.alloc_effect_refs(&[sai_hir::types::EffectRef {
            hir_id: hir_id(owner, 22),
            segments: &[],
        }]);
        let nonempty_effects = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 21),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: &[],
                ret: None,
                effects: Some(effect_refs),
            },
            span: span(),
        });
        for (hir_ty, expected) in [
            (pure_effects, UnsupportedTy::FunctionPointerEffects),
            (nonempty_effects, UnsupportedTy::FunctionPointerEffects),
        ] {
            let mut context = LowerTyContext::for_signature(&interner, owner, &bool_paths);
            assert_eq!(lower_ty(&mut context, hir_ty), Err(expected));
        }

        let borrowed = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 30),
            kind: HirTyKind::Borrow(Mutability::Not, boolean),
            span: span(),
        });
        let borrowed_array_args = [
            HirGenericArg::Type(borrowed),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1),
                negated: false,
            }),
        ];
        let (borrowed_array, mut borrowed_paths) =
            resolved_array_path(&arena, owner, &symbols, 33, &borrowed_array_args);
        borrowed_paths.extend(bool_paths.clone());
        let borrowed_tuple_elements = arena.alloc_tys(&[*borrowed_array]);
        let borrowed_tuple = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 32),
            kind: HirTyKind::Tuple(borrowed_tuple_elements),
            span: span(),
        });
        let borrowed_params = arena.alloc_tys(&[*borrowed_tuple]);
        let borrowed_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 31),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: borrowed_params,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &borrowed_paths)
            .with_array_length_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, borrowed_fn),
            Err(UnsupportedTy::FunctionPointerRegion)
        );
        assert_eq!(
            context.next_region_index, 0,
            "a rejected nested function-pointer borrow does not consume an outer signature region"
        );

        let param_def = owner.def_id.to_def_id();
        let (param, param_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            40,
            "T",
            None,
            Res::Def(DefKind::TyParam, param_def),
        );
        let param_tuple_elements = arena.alloc_tys(&[*param]);
        let param_tuple = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 41),
            kind: HirTyKind::Tuple(param_tuple_elements),
            span: span(),
        });
        let param_types = arena.alloc_tys(&[*param_tuple]);
        let generic_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 50),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: param_types,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &param_paths);
        assert_eq!(
            lower_ty(&mut context, generic_fn),
            Err(UnsupportedTy::FunctionPointerGenericParameter)
        );

        let (array_param, mut array_param_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            110,
            "T",
            None,
            Res::Def(DefKind::TyParam, param_def),
        );
        let array_param_args = [
            HirGenericArg::Type(array_param),
            HirGenericArg::Const(ConstArg::Int {
                value: Some(1),
                negated: false,
            }),
        ];
        let (param_array, param_array_paths) =
            resolved_array_path(&arena, owner, &symbols, 120, &array_param_args);
        array_param_paths.extend(param_array_paths);
        let param_array_inputs = arena.alloc_tys(&[*param_array]);
        let param_array_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 130),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: param_array_inputs,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &array_param_paths)
            .with_array_length_pointer_width(32);
        assert_eq!(
            lower_ty(&mut context, param_array_fn),
            Err(UnsupportedTy::FunctionPointerGenericParameter)
        );

        let parameter_name = symbols.intern("T").expect("type parameter name interns");
        let (adt_parameter, adt_parameter_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            140,
            "T",
            None,
            Res::Def(DefKind::TyParam, param_def),
        );
        let adt_args = arena.alloc_generic_args_list(&[HirGenericArg::Type(adt_parameter)]);
        let adt_generic_args = arena.alloc_generic_args(GenericArgs {
            args: adt_args,
            span: span(),
        });
        let adt_def = owner.def_id.to_def_id();
        let (adt_application, mut adt_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            150,
            "Wrapper",
            Some(adt_generic_args),
            Res::Def(DefKind::Struct, adt_def),
        );
        adt_paths.extend(adt_parameter_paths);
        let adt_inputs = arena.alloc_tys(&[*adt_application]);
        let adt_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 160),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: adt_inputs,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let unit_enums = HashSet::new();
        let local_nominals = HashMap::from([(
            adt_def,
            LocalNominalDef {
                type_params: Arc::from([(param_def, parameter_name)]),
            },
        )]);
        let mut context = LowerTyContext::for_signature_with_local_nominals(
            &interner,
            owner,
            &adt_paths,
            &unit_enums,
            &local_nominals,
        );
        assert_eq!(
            lower_ty(&mut context, adt_fn),
            Err(UnsupportedTy::FunctionPointerGenericParameter)
        );

        for (local_id, kind, expected) in [
            (52, HirTyKind::Infer, UnsupportedTy::Inference),
            (54, HirTyKind::Err, UnsupportedTy::Error),
        ] {
            let child = arena.alloc_ty(HirTy {
                hir_id: hir_id(owner, local_id),
                kind,
                span: span(),
            });
            let children = arena.alloc_tys(&[*child]);
            let function = arena.alloc_ty(HirTy {
                hir_id: hir_id(owner, local_id + 1),
                kind: HirTyKind::FnPtr {
                    safety: Safety::Safe,
                    params: children,
                    ret: None,
                    effects: None,
                },
                span: span(),
            });
            let mut context = LowerTyContext::for_signature(&interner, owner, &bool_paths);
            assert_eq!(lower_ty(&mut context, function), Err(expected));
        }

        let (foreign_owner, _, _, _) = owner_with_generic_params(&[]);
        let wrong_owner_child = arena.alloc_ty(HirTy {
            hir_id: hir_id(foreign_owner, 60),
            kind: HirTyKind::Tuple(&[]),
            span: span(),
        });
        let wrong_owner_params = arena.alloc_tys(&[*wrong_owner_child]);
        let wrong_owner_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 61),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: wrong_owner_params,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &bool_paths);
        assert_eq!(
            lower_ty(&mut context, wrong_owner_fn),
            Err(UnsupportedTy::OwnerMismatch)
        );

        let (unresolved, _) = resolved_path(
            &arena,
            owner,
            &symbols,
            70,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let unresolved_params = arena.alloc_tys(&[*unresolved]);
        let unresolved_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 80),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: unresolved_params,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let no_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &no_paths);
        assert_eq!(
            lower_ty(&mut context, unresolved_fn),
            Err(UnsupportedTy::MissingPathResolution)
        );

        let (partial, partial_paths) = resolved_qualified_path(
            &arena,
            owner,
            &symbols,
            90,
            "module",
            "Bool",
            1,
            Res::PrimTy(PrimTy::Bool),
        );
        let partial_params = arena.alloc_tys(&[*partial]);
        let partial_fn = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 100),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: partial_params,
                ret: None,
                effects: None,
            },
            span: span(),
        });
        let mut context = LowerTyContext::for_signature(&interner, owner, &partial_paths);
        assert_eq!(
            lower_ty(&mut context, partial_fn),
            Err(UnsupportedTy::IncompletePathResolution)
        );
    }

    #[test]
    fn aliases_generics_self_and_projections_are_not_guessed() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let cases = [
            ("Alias", DefKind::Alias, UnsupportedTy::Alias),
            ("T", DefKind::TyParam, UnsupportedTy::GenericParameter),
            ("Count", DefKind::ConstParam, UnsupportedTy::ConstParameter),
            ("Assoc", DefKind::AssocTy, UnsupportedTy::Projection),
            ("Record", DefKind::Struct, UnsupportedTy::NominalType),
        ];
        for (index, (spelling, def_kind, expected)) in cases.into_iter().enumerate() {
            let (hir_ty, paths) = resolved_path(
                &arena,
                owner,
                &symbols,
                80 + (index as u32) * 3,
                spelling,
                None,
                Res::Def(def_kind, owner.def_id.to_def_id()),
            );
            let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
            assert_eq!(lower_ty(&mut context, hir_ty), Err(expected));
        }

        let self_type = HirTy {
            hir_id: hir_id(owner, 110),
            kind: HirTyKind::Path(Path {
                hir_id: hir_id(owner, 111),
                root: PathRoot::SelfType,
                segments: &[],
                span: span(),
            }),
            span: span(),
        };
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        assert_eq!(
            lower_ty(&mut context, &self_type),
            Err(UnsupportedTy::SelfType)
        );

        let self_projection_segment = arena.alloc_path_segment(PathSegment {
            hir_id: hir_id(owner, 112),
            name: Name {
                sym: symbols.intern("Associated").unwrap(),
                span: span(),
            },
            args: None,
        });
        let self_projection_segments = arena.alloc_path_segments(&[*self_projection_segment]);
        let self_projection = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 113),
            kind: HirTyKind::Path(Path {
                hir_id: hir_id(owner, 114),
                root: PathRoot::SelfType,
                segments: self_projection_segments,
                span: span(),
            }),
            span: span(),
        });
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        assert_eq!(
            lower_ty(&mut context, self_projection),
            Err(UnsupportedTy::Projection)
        );
    }

    #[test]
    fn validated_local_nominals_are_enabled_only_for_field_contexts() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let nominal_def = owner.def_id.to_def_id();
        let (nominal, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            80,
            "Record",
            None,
            Res::Def(DefKind::Struct, nominal_def),
        );
        let local_nominals = HashMap::from([(
            nominal_def,
            LocalNominalDef {
                type_params: Arc::from([]),
            },
        )]);

        let mut field_context = LowerTyContext::for_field(&interner, owner, &paths, false)
            .with_field_local_nominals(&local_nominals);
        assert!(matches!(
            lower_ty(&mut field_context, nominal),
            Ok(ty) if matches!(ty.kind(), TyKind::Adt(def_id, args)
                if *def_id == nominal_def && args.is_empty())
        ));

        let mut signature_context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_field_local_nominals(&local_nominals);
        assert_eq!(
            lower_ty(&mut signature_context, nominal),
            Err(UnsupportedTy::NominalType),
            "the field-only builder cannot widen signature lowering"
        );

        let mut body_context = LowerTyContext::for_body(&interner, owner, &paths)
            .with_field_local_nominals(&local_nominals);
        assert_eq!(
            lower_ty(&mut body_context, nominal),
            Err(UnsupportedTy::NominalType),
            "the field-only builder cannot widen body lowering"
        );
    }

    #[test]
    fn validated_type_parameters_are_enabled_only_for_field_contexts() {
        let (owner, symbols, parameter_ids, _) = owner_with_generic_params(&["T"]);
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let parameter_def = parameter_ids[0].to_def_id();
        let (parameter_ty, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            590,
            "T",
            None,
            Res::Def(DefKind::TyParam, parameter_def),
        );
        let parameter_name = symbols.intern("T").expect("parameter name interns");
        let type_params = [(
            parameter_def,
            ParamTy {
                index: 0,
                name: parameter_name,
            },
        )];

        let mut field_context = LowerTyContext::for_field(&interner, owner, &paths, false)
            .with_field_type_params(&type_params)
            .expect("validated field binders form a source-ordered map");
        assert!(matches!(
            lower_ty(&mut field_context, parameter_ty),
            Ok(ty) if matches!(ty.kind(), TyKind::Param(param)
                if *param == type_params[0].1)
        ));

        let mut signature_context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_field_type_params(&type_params)
            .expect("field-only binder setup is inert for signatures");
        assert_eq!(
            lower_ty(&mut signature_context, parameter_ty),
            Err(UnsupportedTy::GenericParameter),
            "field parameters cannot widen signature lowering"
        );

        let mut body_context = LowerTyContext::for_body(&interner, owner, &paths)
            .with_field_type_params(&type_params)
            .expect("field-only binder setup is inert for bodies");
        assert_eq!(
            lower_ty(&mut body_context, parameter_ty),
            Err(UnsupportedTy::GenericParameter),
            "field parameters cannot widen body lowering"
        );

        let invalid_params = [(
            parameter_def,
            ParamTy {
                index: 1,
                name: parameter_name,
            },
        )];
        assert_eq!(
            LowerTyContext::for_field(&interner, owner, &paths, false)
                .with_field_type_params(&invalid_params)
                .err(),
            Some(UnsupportedTy::GenericParameter),
            "a mismatched source-order index is rejected before any binder is installed"
        );
    }

    #[test]
    fn local_alias_lowering_requires_an_exact_signature_allowlist_entry() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let alias_def = owner.def_id.to_def_id();
        let alias_ty = interner.intern(TyKind::Uint(UintTy::U32));
        let aliases = HashMap::from([(
            alias_def,
            LocalTypeAlias {
                type_params: Arc::from([]),
                template: alias_ty.clone(),
            },
        )]);
        let (alias, alias_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            520,
            "Count",
            None,
            Res::Def(DefKind::Alias, alias_def),
        );

        let mut context = LowerTyContext::for_signature(&interner, owner, &alias_paths)
            .with_local_type_aliases(&aliases);
        let lowered = lower_ty(&mut context, alias).expect("validated alias lowers");
        assert!(lowered.ptr_eq(&alias_ty));

        let mut unvalidated_context = LowerTyContext::for_signature(&interner, owner, &alias_paths);
        assert_eq!(
            lower_ty(&mut unvalidated_context, alias),
            Err(UnsupportedTy::Alias),
            "an alias resolution remains unsupported without its exact validated value"
        );

        let empty_args = arena.alloc_generic_args(GenericArgs {
            args: &[],
            span: span(),
        });
        let (generic_alias, generic_alias_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            530,
            "Count",
            Some(empty_args),
            Res::Def(DefKind::Alias, alias_def),
        );
        let mut generic_context =
            LowerTyContext::for_signature(&interner, owner, &generic_alias_paths)
                .with_local_type_aliases(&aliases);
        assert_eq!(
            lower_ty(&mut generic_context, generic_alias),
            Err(UnsupportedTy::GenericArguments),
            "an explicit empty argument list cannot bypass alias arity checks"
        );

        let mut body_context = LowerTyContext::for_body(&interner, owner, &alias_paths)
            .with_local_type_aliases(&aliases);
        assert_eq!(
            lower_ty(&mut body_context, alias),
            Err(UnsupportedTy::Alias),
            "the signature-only allowlist does not enable body lowering"
        );
    }

    #[test]
    fn generic_local_alias_substitutes_arguments_in_the_use_context_once() {
        let (owner, symbols, generic_defs, _) = owner_with_generic_params(&["T", "U"]);
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let alias_def = owner.def_id.to_def_id();
        let alias_parameter = ParamTy {
            index: 0,
            name: symbols.intern("T").unwrap(),
        };
        let caller_parameter = ParamTy {
            index: 1,
            name: symbols.intern("U").unwrap(),
        };
        let caller_parameter_def = generic_defs[1].to_def_id();
        let aliases = HashMap::from([(
            alias_def,
            LocalTypeAlias {
                type_params: Arc::from([(generic_defs[0].to_def_id(), alias_parameter)]),
                template: interner.intern(TyKind::Param(alias_parameter)),
            },
        )]);

        let (caller_type, caller_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            540,
            "U",
            None,
            Res::Def(DefKind::TyParam, caller_parameter_def),
        );
        let arguments = arena.alloc_generic_args_list(&[HirGenericArg::Type(caller_type)]);
        let generic_args = arena.alloc_generic_args(GenericArgs {
            args: arguments,
            span: span(),
        });
        let (alias_type, mut paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            550,
            "Generic",
            Some(generic_args),
            Res::Def(DefKind::Alias, alias_def),
        );
        paths.extend(caller_paths);
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_local_type_aliases(&aliases);
        context
            .generic_params
            .insert(caller_parameter_def, caller_parameter);
        let lowered =
            lower_ty(&mut context, alias_type).expect("the use-site type argument lowers");
        assert_eq!(
            lowered.kind(),
            &TyKind::Param(caller_parameter),
            "the caller's index-one parameter replaces the alias's index-zero parameter"
        );

        let (missing, missing_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            560,
            "Generic",
            None,
            Res::Def(DefKind::Alias, alias_def),
        );
        let mut missing_context = LowerTyContext::for_signature(&interner, owner, &missing_paths)
            .with_local_type_aliases(&aliases);
        assert_eq!(
            lower_ty(&mut missing_context, missing),
            Err(UnsupportedTy::WrongNumberOfGenericArguments)
        );

        let const_args = arena.alloc_generic_args_list(&[HirGenericArg::Const(ConstArg::Int {
            value: Some(4),
            negated: false,
        })]);
        let const_args = arena.alloc_generic_args(GenericArgs {
            args: const_args,
            span: span(),
        });
        let (wrong_kind, wrong_kind_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            570,
            "Generic",
            Some(const_args),
            Res::Def(DefKind::Alias, alias_def),
        );
        let mut wrong_kind_context =
            LowerTyContext::for_signature(&interner, owner, &wrong_kind_paths)
                .with_local_type_aliases(&aliases);
        assert_eq!(
            lower_ty(&mut wrong_kind_context, wrong_kind),
            Err(UnsupportedTy::GenericArgumentKind)
        );
    }

    #[test]
    fn local_alias_allowlist_rejects_unregistered_and_external_definitions() {
        let packages = PackageInterner::new();
        let local_package = packages.intern("acme:aliases#1.0").unwrap();
        let external_package = packages.intern("acme:external#1.0").unwrap();
        let symbols = Interner::new();
        let local_paths = DefPathInterner::new(local_package, &packages, &symbols).unwrap();
        let use_name = symbols.intern("use_count").unwrap();
        let owner_path = DefPath::new(
            local_package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(use_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let owner = sai_hir::OwnerId {
            def_id: local_paths.intern(owner_path).unwrap(),
        };
        let count_name = symbols.intern("Count").unwrap();
        let local_alias_path = DefPath::new(
            local_package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(count_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let local_alias = local_paths.intern(local_alias_path).unwrap().to_def_id();
        let unregistered_name = symbols.intern("OtherCount").unwrap();
        let unregistered_path = DefPath::new(
            local_package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(unregistered_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let unregistered_alias = local_paths.intern(unregistered_path).unwrap().to_def_id();
        let external_paths = DefPathInterner::new(external_package, &packages, &symbols).unwrap();
        let external_alias_path = DefPath::new(
            external_package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(count_name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let external_alias = external_paths
            .intern(external_alias_path)
            .unwrap()
            .to_def_id();

        let interner = TyInterner::new();
        let aliases = HashMap::from([(
            local_alias,
            LocalTypeAlias {
                type_params: Arc::from([]),
                template: interner.intern(TyKind::Uint(UintTy::U32)),
            },
        )]);
        let arena = HirArena::new();
        for (local_id, def_id) in [(550, unregistered_alias), (560, external_alias)] {
            let (hir_ty, paths) = resolved_path(
                &arena,
                owner,
                &symbols,
                local_id,
                "Count",
                None,
                Res::Def(DefKind::Alias, def_id),
            );
            let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
                .with_local_type_aliases(&aliases);
            assert_eq!(
                lower_ty(&mut context, hir_ty),
                Err(UnsupportedTy::Alias),
                "an alias's spelling or external package identity cannot borrow a local allowlist entry"
            );
        }
    }

    #[test]
    fn incomplete_and_cross_owner_paths_are_rejected() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (hir_ty, mut paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            120,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let path_id = match hir_ty.kind {
            HirTyKind::Path(path) => path.hir_id,
            _ => unreachable!(),
        };
        paths.insert(
            path_id,
            PathRes {
                base: Res::PrimTy(PrimTy::Bool),
                resolved_segments: 0,
                segment_count: 1,
            },
        );
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, hir_ty),
            Err(UnsupportedTy::IncompletePathResolution)
        );

        let foreign_owner = {
            let packages = PackageInterner::new();
            let package = packages.intern("acme:foreign#1.0").unwrap();
            let foreign_symbols = Interner::new();
            let paths =
                DefPathInterner::new(package, &packages, &foreign_symbols).expect("path interner");
            let symbol = foreign_symbols.intern("foreign").unwrap();
            let path = DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(symbol),
                    disambiguator: 0,
                }],
                &packages,
                &foreign_symbols,
            )
            .unwrap();
            sai_hir::OwnerId {
                def_id: paths.intern(path).unwrap(),
            }
        };
        let foreign = HirTy {
            hir_id: hir_id(foreign_owner, 130),
            kind: HirTyKind::Tuple(&[]),
            span: span(),
        };
        let empty_paths = HashMap::new();
        let mut context = LowerTyContext::for_signature(&interner, owner, &empty_paths);
        assert_eq!(
            lower_ty(&mut context, &foreign),
            Err(UnsupportedTy::OwnerMismatch)
        );

        let (missing, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            140,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        let mut wrong_map = paths.clone();
        let key = wrong_map.keys().next().copied().unwrap();
        wrong_map.remove(&key);
        let mut context = LowerTyContext::for_signature(&interner, owner, &wrong_map);
        assert_eq!(
            lower_ty(&mut context, missing),
            Err(UnsupportedTy::MissingPathResolution)
        );
    }

    #[test]
    fn test_only_resolution_is_not_semantically_lowered() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (hir_ty, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            150,
            "Hidden",
            None,
            Res::TestOnly(ImportIndex(0)),
        );
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, hir_ty),
            Err(UnsupportedTy::TestOnlyPath)
        );
    }

    #[test]
    fn generic_arguments_are_not_dropped_during_path_lowering() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let empty_args = sai_hir::types::GenericArgs {
            args: &[],
            span: span(),
        };
        let (hir_ty, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            160,
            "Bool",
            Some(&empty_args),
            Res::PrimTy(PrimTy::Bool),
        );
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, hir_ty),
            Err(UnsupportedTy::GenericArguments)
        );

        let (hir_ty, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            170,
            "Record",
            Some(&empty_args),
            Res::Def(DefKind::Struct, owner.def_id.to_def_id()),
        );
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        assert_eq!(
            lower_ty(&mut context, hir_ty),
            Err(UnsupportedTy::GenericArguments)
        );
    }

    #[test]
    fn function_signature_regions_follow_parameter_then_result_order() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (i32_ty, mut paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            200,
            "I32",
            None,
            Res::PrimTy(PrimTy::I32),
        );
        let (bool_ty, bool_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            210,
            "Bool",
            None,
            Res::PrimTy(PrimTy::Bool),
        );
        paths.extend(bool_paths);
        let (i64_ty, i64_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            220,
            "I64",
            None,
            Res::PrimTy(PrimTy::I64),
        );
        paths.extend(i64_paths);
        let (u8_ty, u8_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            230,
            "U8",
            None,
            Res::PrimTy(PrimTy::U8),
        );
        paths.extend(u8_paths);

        let first_input = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 250),
            kind: HirTyKind::Borrow(Mutability::Not, i32_ty),
            span: span(),
        });
        let second_input_borrow = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 260),
            kind: HirTyKind::Borrow(Mutability::Mut, bool_ty),
            span: span(),
        });
        let second_input_elements = arena.alloc_tys(&[*second_input_borrow, *i64_ty]);
        let second_input = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 270),
            kind: HirTyKind::Tuple(second_input_elements),
            span: span(),
        });
        let output = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 280),
            kind: HirTyKind::Borrow(Mutability::Not, u8_ty),
            span: span(),
        });
        let params = arena.alloc_fn_params(&[
            sai_hir::body::FnParam {
                hir_id: hir_id(owner, 300),
                name: None,
                is_self: false,
                mutable: false,
                ty: first_input,
                span: span(),
            },
            sai_hir::body::FnParam {
                hir_id: hir_id(owner, 310),
                name: None,
                is_self: false,
                mutable: false,
                ty: second_input,
                span: span(),
            },
        ]);
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params,
            ret: Some(output),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let generics = Generics {
            params: &[],
            where_preds: &[],
        };

        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
        let lowered = lower_fn_sig(&mut context, generics, signature).unwrap();
        assert!(matches!(
            lowered.inputs()[0].kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 0, .. }), _, Mutability::Not)
        ));
        let TyKind::Tuple(second_elements) = lowered.inputs()[1].kind() else {
            panic!("second input lowers as a tuple");
        };
        assert!(matches!(
            second_elements[0].kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 1, .. }), _, Mutability::Mut)
        ));
        assert!(matches!(
            lowered.output().kind(),
            TyKind::Ref(Region::Sig(SigRegion { index: 2, .. }), _, Mutability::Not)
        ));
    }

    #[test]
    fn function_without_explicit_return_uses_unit_output() {
        let (owner, _) = owner();
        let interner = TyInterner::new();
        let paths = HashMap::new();
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: &[],
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let generics = Generics {
            params: &[],
            where_preds: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths);

        let lowered = lower_fn_sig(&mut context, generics, signature).unwrap();
        assert!(lowered.inputs().is_empty());
        assert_eq!(lowered.output().kind(), &TyKind::Tuple(Arc::from([])));
    }

    #[test]
    fn function_type_parameters_lower_in_owner_local_source_order() {
        let (owner, symbols, param_def_ids, def_paths) = owner_with_generic_params(&["B", "A"]);
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let generic_nodes = param_def_ids
            .iter()
            .zip(["B", "A"])
            .map(|(def_id, name)| {
                arena.alloc_generic_param(sai_hir::types::GenericParam {
                    def_id: *def_id,
                    name: Name {
                        sym: symbols.intern(name).unwrap(),
                        span: span(),
                    },
                    kind: GenericParamKind::Type { bounds: &[] },
                })
            })
            .collect::<Vec<_>>();
        let generics = Generics {
            params: arena
                .alloc_generic_params(&generic_nodes.iter().map(|node| **node).collect::<Vec<_>>()),
            where_preds: &[],
        };
        let (input, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            350,
            "A",
            None,
            Res::Def(DefKind::TyParam, param_def_ids[1].to_def_id()),
        );
        let (output, output_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            360,
            "B",
            None,
            Res::Def(DefKind::TyParam, param_def_ids[0].to_def_id()),
        );
        let mut paths = paths;
        paths.extend(output_paths);
        let parameter = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 370),
            name: None,
            is_self: false,
            mutable: false,
            ty: input,
            span: span(),
        });
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*parameter]),
            ret: Some(output),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_def_path_interner(&def_paths);

        let lowered = lower_fn_sig(&mut context, generics, signature).unwrap();
        assert_eq!(
            lowered
                .type_params()
                .iter()
                .map(|parameter| parameter.param.index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            lowered
                .type_params()
                .iter()
                .map(|parameter| parameter.param.name)
                .collect::<Vec<_>>(),
            [symbols.intern("B").unwrap(), symbols.intern("A").unwrap()]
        );
        assert_eq!(
            lowered
                .type_params()
                .iter()
                .map(|parameter| parameter.def_id)
                .collect::<Vec<_>>(),
            [param_def_ids[0].to_def_id(), param_def_ids[1].to_def_id()]
        );
        assert_eq!(
            lowered.inputs()[0].kind(),
            &TyKind::Param(ParamTy {
                index: 1,
                name: symbols.intern("A").unwrap(),
            })
        );
        assert_eq!(
            lowered.output().kind(),
            &TyKind::Param(ParamTy {
                index: 0,
                name: symbols.intern("B").unwrap(),
            })
        );
    }

    #[test]
    fn array_length_uses_only_the_exact_enclosing_usize_const_binder() {
        let (owner, symbols, local_param, foreign_param, def_paths) =
            owner_with_same_package_foreign_const_param();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (usize_ty, mut paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            500,
            "USize",
            None,
            Res::PrimTy(PrimTy::USize),
        );
        let (element_ty, element_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            510,
            "U8",
            None,
            Res::PrimTy(PrimTy::U8),
        );
        paths.extend(element_paths);
        let generic_param = arena.alloc_generic_param(sai_hir::types::GenericParam {
            def_id: local_param,
            name: Name {
                sym: symbols.intern("N").unwrap(),
                span: span(),
            },
            kind: GenericParamKind::Const { ty: usize_ty },
        });
        let generics = Generics {
            params: arena.alloc_generic_params(&[*generic_param]),
            where_preds: &[],
        };
        let (local_const, local_const_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            520,
            "N",
            None,
            Res::Def(DefKind::ConstParam, local_param.to_def_id()),
        );
        paths.extend(local_const_paths);
        let local_args = [
            HirGenericArg::Type(element_ty),
            HirGenericArg::Type(local_const),
        ];
        let (local_array, local_array_paths) =
            resolved_array_path(&arena, owner, &symbols, 530, &local_args);
        paths.extend(local_array_paths);
        let local_parameter = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 540),
            name: None,
            is_self: false,
            mutable: false,
            ty: local_array,
            span: span(),
        });
        let local_signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*local_parameter]),
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_def_path_interner(&def_paths)
            .with_array_length_pointer_width(64);
        let lowered = lower_fn_sig(&mut context, generics, local_signature)
            .expect("the exact USize const binder is valid as an array length");
        let binder = lowered.const_params()[0].param;
        assert_eq!(binder.index, 0);
        assert!(matches!(
            lowered.inputs()[0].kind(),
            TyKind::Array(_, Const::Param(parameter)) if *parameter == binder
        ));

        let (foreign_const, foreign_const_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            550,
            "N",
            None,
            Res::Def(DefKind::ConstParam, foreign_param.to_def_id()),
        );
        let foreign_args = [
            HirGenericArg::Type(element_ty),
            HirGenericArg::Type(foreign_const),
        ];
        let (foreign_array, foreign_array_paths) =
            resolved_array_path(&arena, owner, &symbols, 560, &foreign_args);
        paths.extend(foreign_const_paths);
        paths.extend(foreign_array_paths);
        let foreign_parameter = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 570),
            name: None,
            is_self: false,
            mutable: false,
            ty: foreign_array,
            span: span(),
        });
        let foreign_signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*foreign_parameter]),
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_def_path_interner(&def_paths)
            .with_array_length_pointer_width(64);
        assert_eq!(
            lower_fn_sig(&mut context, generics, foreign_signature),
            Err(UnsupportedFnSig::ParameterType {
                index: 0,
                reason: UnsupportedTy::ArrayLength,
            }),
            "a same-spelled const binder from another owner is not captured"
        );
    }

    #[test]
    fn function_pointer_reuses_only_the_enclosing_function_type_parameter() {
        let (owner, symbols, local_param, foreign_param, def_paths) =
            owner_with_same_package_foreign_param();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let generic_param = arena.alloc_generic_param(sai_hir::types::GenericParam {
            def_id: local_param,
            name: Name {
                sym: symbols.intern("T").unwrap(),
                span: span(),
            },
            kind: GenericParamKind::Type { bounds: &[] },
        });
        let generics = Generics {
            params: arena.alloc_generic_params(&[*generic_param]),
            where_preds: &[],
        };
        let (local_ty, mut local_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            420,
            "T",
            None,
            Res::Def(DefKind::TyParam, local_param.to_def_id()),
        );
        let callback_params = arena.alloc_tys(&[*local_ty]);
        let callback = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 440),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: callback_params,
                ret: Some(local_ty),
                effects: None,
            },
            span: span(),
        });
        let callback_param = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 450),
            name: None,
            is_self: false,
            mutable: false,
            ty: callback,
            span: span(),
        });
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*callback_param]),
            ret: Some(local_ty),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &local_paths)
            .with_def_path_interner(&def_paths);
        let lowered = lower_fn_sig(&mut context, generics, signature)
            .expect("a callback can reuse its enclosing function's binder");
        let binder = lowered.type_params()[0].param;
        assert_eq!(binder.index, 0);
        let TyKind::FnPtr(callback) = lowered.inputs()[0].kind() else {
            panic!("the first outer argument remains a function pointer");
        };
        assert_eq!(callback.inputs()[0].kind(), &TyKind::Param(binder));
        assert_eq!(callback.output().kind(), &TyKind::Param(binder));
        assert_eq!(lowered.output().kind(), &TyKind::Param(binder));

        let (foreign_ty, foreign_paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            460,
            "T",
            None,
            Res::Def(DefKind::TyParam, foreign_param.to_def_id()),
        );
        local_paths.extend(foreign_paths);
        let foreign_callback_params = arena.alloc_tys(&[*foreign_ty]);
        let foreign_callback = arena.alloc_ty(HirTy {
            hir_id: hir_id(owner, 480),
            kind: HirTyKind::FnPtr {
                safety: Safety::Safe,
                params: foreign_callback_params,
                ret: Some(foreign_ty),
                effects: None,
            },
            span: span(),
        });
        let foreign_callback_param = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 490),
            name: None,
            is_self: false,
            mutable: false,
            ty: foreign_callback,
            span: span(),
        });
        let foreign_signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*foreign_callback_param]),
            ret: Some(local_ty),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &local_paths)
            .with_def_path_interner(&def_paths);
        assert_eq!(
            lower_fn_sig(&mut context, generics, foreign_signature),
            Err(UnsupportedFnSig::ParameterType {
                index: 0,
                reason: UnsupportedTy::FunctionPointerGenericParameter,
            }),
            "a same-spelled parameter from another owner is not captured by the enclosing binder"
        );
    }

    #[test]
    fn foreign_owner_type_parameter_resolution_is_not_captured_by_name() {
        let (owner, symbols, local_param, foreign_param, def_paths) =
            owner_with_same_package_foreign_param();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let generic_param = arena.alloc_generic_param(sai_hir::types::GenericParam {
            def_id: local_param,
            name: Name {
                sym: symbols.intern("T").unwrap(),
                span: span(),
            },
            kind: GenericParamKind::Type { bounds: &[] },
        });
        let generics = Generics {
            params: arena.alloc_generic_params(&[*generic_param]),
            where_preds: &[],
        };
        let (input, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            380,
            "T",
            None,
            Res::Def(DefKind::TyParam, foreign_param.to_def_id()),
        );
        assert_eq!(
            local_param.package(),
            foreign_param.package(),
            "both binders belong to the same package"
        );
        assert_ne!(
            local_param.to_def_id(),
            foreign_param.to_def_id(),
            "the local and foreign function binders have distinct DefIds"
        );
        let parameter = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 390),
            name: None,
            is_self: false,
            mutable: false,
            ty: input,
            span: span(),
        });
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*parameter]),
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_def_path_interner(&def_paths);

        assert_eq!(
            lower_fn_sig(&mut context, generics, signature),
            Err(UnsupportedFnSig::ParameterType {
                index: 0,
                reason: UnsupportedTy::GenericParameter,
            })
        );
        assert_eq!(
            lower_ty(&mut context, input),
            Err(UnsupportedTy::ContextAfterFailure),
            "a failed foreign parameter conversion poisons its owner context"
        );
    }

    #[test]
    fn foreign_owner_binders_are_rejected_even_with_matching_resolution() {
        let (owner, symbols, _local_param, foreign_param, def_paths) =
            owner_with_same_package_foreign_param();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let generic_param = arena.alloc_generic_param(sai_hir::types::GenericParam {
            def_id: foreign_param,
            name: Name {
                sym: symbols.intern("T").unwrap(),
                span: span(),
            },
            kind: GenericParamKind::Type { bounds: &[] },
        });
        let generics = Generics {
            params: arena.alloc_generic_params(&[*generic_param]),
            where_preds: &[],
        };
        let (input, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            400,
            "T",
            None,
            Res::Def(DefKind::TyParam, foreign_param.to_def_id()),
        );
        let parameter = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 410),
            name: None,
            is_self: false,
            mutable: false,
            ty: input,
            span: span(),
        });
        let signature = HirFnSig {
            safety: Safety::Safe,
            is_async: false,
            params: arena.alloc_fn_params(&[*parameter]),
            ret: None,
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let mut context = LowerTyContext::for_signature(&interner, owner, &paths)
            .with_def_path_interner(&def_paths);

        assert_eq!(
            lower_fn_sig(&mut context, generics, signature),
            Err(UnsupportedFnSig::GenericParameters),
            "a matching resolution is not enough when its binder belongs to another function"
        );
    }

    #[test]
    fn unsupported_function_signature_metadata_fails_closed() {
        let (owner, symbols) = owner();
        let interner = TyInterner::new();
        let arena = HirArena::new();
        let (input, paths) = resolved_path(
            &arena,
            owner,
            &symbols,
            320,
            "I32",
            None,
            Res::PrimTy(PrimTy::I32),
        );
        let param = arena.alloc_fn_param(sai_hir::body::FnParam {
            hir_id: hir_id(owner, 330),
            name: None,
            is_self: false,
            mutable: false,
            ty: input,
            span: span(),
        });
        let params = arena.alloc_fn_params(&[*param]);
        let generic_param = arena.alloc_generic_param(sai_hir::types::GenericParam {
            def_id: owner.def_id,
            name: Name {
                sym: symbols.intern("T").unwrap(),
                span: span(),
            },
            kind: sai_hir::types::GenericParamKind::Const { ty: input },
        });
        let generic_params = arena.alloc_generic_params(&[*generic_param]);
        let cases = [
            (
                Generics {
                    params: generic_params,
                    where_preds: &[],
                },
                HirFnSig {
                    safety: Safety::Safe,
                    is_async: false,
                    params,
                    ret: None,
                    borrows: None,
                    effects: None,
                    requires: &[],
                    ensures: &[],
                },
                UnsupportedFnSig::GenericParameters,
            ),
            (
                Generics {
                    params: &[],
                    where_preds: &[],
                },
                HirFnSig {
                    safety: Safety::Unsafe,
                    is_async: false,
                    params,
                    ret: None,
                    borrows: None,
                    effects: None,
                    requires: &[],
                    ensures: &[],
                },
                UnsupportedFnSig::Unsafe,
            ),
            (
                Generics {
                    params: &[],
                    where_preds: &[],
                },
                HirFnSig {
                    safety: Safety::Safe,
                    is_async: true,
                    params,
                    ret: None,
                    borrows: None,
                    effects: None,
                    requires: &[],
                    ensures: &[],
                },
                UnsupportedFnSig::Async,
            ),
            (
                Generics {
                    params: &[],
                    where_preds: &[],
                },
                HirFnSig {
                    safety: Safety::Safe,
                    is_async: false,
                    params,
                    ret: None,
                    borrows: Some(&[]),
                    effects: None,
                    requires: &[],
                    ensures: &[],
                },
                UnsupportedFnSig::BorrowClause,
            ),
            (
                Generics {
                    params: &[],
                    where_preds: &[],
                },
                HirFnSig {
                    safety: Safety::Safe,
                    is_async: false,
                    params,
                    ret: None,
                    borrows: None,
                    effects: Some(&[]),
                    requires: &[],
                    ensures: &[],
                },
                UnsupportedFnSig::Effects,
            ),
        ];

        for (generics, signature, expected) in cases {
            let mut context = LowerTyContext::for_signature(&interner, owner, &paths);
            assert_eq!(
                lower_fn_sig(&mut context, generics, signature),
                Err(expected)
            );
            assert_eq!(
                lower_ty(&mut context, input),
                Err(UnsupportedTy::ContextAfterFailure)
            );
        }
    }
}
