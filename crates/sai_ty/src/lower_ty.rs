//! Conservative conversion of a small set of resolved HIR types.
//!
//! This module deliberately returns an unsupported result instead of
//! manufacturing semantic error types or guessing at unresolved names.

use crate::ty::{FloatTy, FnSig, IntTy, Region, SigRegion, Ty, TyInterner, TyKind, UintTy};
use sai_hir::types::{Generics, PathRoot, Ty as HirTy, TyKind as HirTyKind};
use sai_hir::{DefId, HirId, OwnerId, Safety, body::FnSig as HirFnSig};
use sai_resolve::package_scopes::{DefKind, PathRes, Res};
use sai_resolve::prelude::PrimTy;
use std::collections::{HashMap, HashSet};

/// The reason an HIR type cannot be converted by the current subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedTy {
    OwnerMismatch,
    MissingPathResolution,
    IncompletePathResolution,
    QualifiedPath,
    Array,
    Alias,
    GenericArguments,
    GenericParameter,
    ConstParameter,
    SelfType,
    Projection,
    NominalType,
    FunctionPointer,
    Inference,
    Error,
    WrongNameKind,
    LocalValue,
    TestOnlyPath,
    ErrorPath,
    RegionIndexOverflow,
    FieldRegionWithoutParameter,
    ContextAfterFailure,
}

/// Why a function signature is outside the current non-generic subset.
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
    ParameterType { index: usize, reason: UnsupportedTy },
    ReturnType(UnsupportedTy),
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
    local_nongeneric_nominals: Option<&'a HashSet<DefId>>,
    region_mode: RegionMode,
    next_region_index: u32,
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
            local_nongeneric_nominals: None,
            region_mode: RegionMode::Signature,
            next_region_index: 0,
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
            local_nongeneric_nominals: None,
            region_mode: RegionMode::Signature,
            next_region_index: 0,
            failed: false,
        }
    }

    /// Create a signature context that can lower only validated local unit
    /// enums and direct, nongeneric local records/enums from the same
    /// collection as `paths`.
    ///
    /// The driver derives both sets from the exact current package collection,
    /// source snapshot, and target-filtered HIR. This crate cannot establish
    /// that provenance on its own.
    pub fn for_signature_with_local_nongeneric_nominals(
        interner: &'a TyInterner,
        owner: OwnerId,
        paths: &'a HashMap<HirId, PathRes>,
        unit_enums: &'a HashSet<DefId>,
        local_nongeneric_nominals: &'a HashSet<DefId>,
    ) -> Self {
        Self {
            interner,
            owner,
            paths,
            unit_enums: Some(unit_enums),
            local_nongeneric_nominals: Some(local_nongeneric_nominals),
            region_mode: RegionMode::Signature,
            next_region_index: 0,
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
            local_nongeneric_nominals: None,
            region_mode: RegionMode::Field {
                has_implicit_region_parameter,
            },
            next_region_index: 0,
            failed: false,
        }
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
            local_nongeneric_nominals: None,
            region_mode: RegionMode::Body,
            next_region_index: 0,
            failed: false,
        }
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

/// Convert a resolved HIR type within the primitive/tuple/reference subset.
///
/// The caller owns path resolution and diagnostics. This function performs no
/// diagnostic emission and produces no error type; unsupported forms are
/// returned explicitly.
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
/// Only non-generic, safe, synchronous signatures without effect or borrow
/// clauses are represented by the current `FnSig`. Every failure invalidates
/// the context so a partially consumed signature-region sequence cannot be
/// reused. An omitted return type lowers to unit.
pub fn lower_fn_sig(
    context: &mut LowerTyContext<'_>,
    generics: Generics<'_>,
    signature: HirFnSig<'_>,
) -> Result<FnSig, UnsupportedFnSig> {
    if context.failed {
        return Err(UnsupportedFnSig::ContextAfterFailure);
    }

    let unsupported = if !generics.params.is_empty() {
        Some(UnsupportedFnSig::GenericParameters)
    } else if !generics.where_preds.is_empty() {
        Some(UnsupportedFnSig::WherePredicates)
    } else if signature.safety == Safety::Unsafe {
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

    Ok(FnSig::new(inputs, output))
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
        HirTyKind::FnPtr { .. } => Err(UnsupportedTy::FunctionPointer),
        HirTyKind::Infer => Err(UnsupportedTy::Inference),
        HirTyKind::Err => Err(UnsupportedTy::Error),
    }
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

    if path.segments.iter().any(|segment| segment.args.is_some()) {
        return Err(UnsupportedTy::GenericArguments);
    }

    if let Res::Def(DefKind::Struct | DefKind::Enum, def_id) = resolution.base {
        let local_nongeneric_nominal = context
            .local_nongeneric_nominals
            .is_some_and(|definitions| definitions.contains(&def_id));
        let validated_unit_enum = matches!(resolution.base, Res::Def(DefKind::Enum, _))
            && context
                .unit_enums
                .is_some_and(|unit_enums| unit_enums.contains(&def_id));
        return if local_nongeneric_nominal || validated_unit_enum {
            Ok(context.interner.intern(TyKind::Adt(def_id)))
        } else {
            Err(UnsupportedTy::NominalType)
        };
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
        Res::Def(DefKind::TyParam, _) => Err(UnsupportedTy::GenericParameter),
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
    use sai_hir::types::{Name, Path, PathSegment};
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
        assert_eq!(direct_ty.kind(), &TyKind::Adt(enum_def));

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
        assert_eq!(lower_ty(&mut context, array), Err(UnsupportedTy::Array));
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
        assert_eq!(
            lower_ty(&mut context, &function),
            Err(UnsupportedTy::FunctionPointer)
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
            kind: sai_hir::types::GenericParamKind::Type { bounds: &[] },
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
