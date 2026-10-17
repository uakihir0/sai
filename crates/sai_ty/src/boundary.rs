//! Pure conversion of HIR boundary types into a symbolic WIT type tree.
//!
//! The result deliberately uses definition identities instead of
//! `WitIfaceIdx`/`WitItemIdx`. Interface checking can inspect the same
//! conversion result that WIT generation later materializes against its
//! generated-unit and view-item index maps.

use sai_hir::types::{
    GenericArg, GenericParam, GenericParamKind, LangItem, Path, PathRoot, Ty, TyKind,
};
use sai_hir::{DefId, HirId, OwnerId, Span};
use sai_resolve::package_scopes::{DefKind, PathRes, Res};
use sai_resolve::prelude::PrimTy;
use sai_wit::{WitType, WitTypeRef};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// The boundary position of one HIR type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Position {
    /// A top-level function parameter.
    Param,
    /// A top-level function result.
    Result,
    /// Any nested type component or non-signature type declaration.
    Nested,
}

/// A WIT type tree before named references are assigned WIT indices.
///
/// `DefId` values are session-local lookup identities. Consumers that persist
/// or fingerprint this tree must canonicalize them through definition paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundaryType {
    Unit,
    Bool,
    S8,
    S16,
    S32,
    S64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Char,
    String,
    List(Box<BoundaryType>),
    Option(Box<BoundaryType>),
    Result {
        ok: Option<Box<BoundaryType>>,
        err: Option<Box<BoundaryType>>,
    },
    Tuple(Vec<BoundaryType>),
    /// An interface record, alias, enum, or flags type.
    Named(DefId),
    /// An owned interface resource handle.
    Own(DefId),
    /// A borrowed interface resource handle.
    Borrow(DefId),
}

/// Canonical prelude type identities understood by this converter.
///
/// Callers must key these values by resolved `DefId`; the converter never
/// recognizes a standard type from its source spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CanonicalBoundaryType {
    String,
    Str,
    Slice,
    Vec,
    Bytes,
    Option,
    Result,
    Box,
    /// Stage 5 type; its feature gate and mapping are not implemented here.
    FutureReader,
    /// Stage 5 type; its feature gate and mapping are not implemented here.
    StreamReader,
}

/// Caller-supplied classification for a resolved non-prelude definition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BoundaryDefinition {
    /// A type declared directly in an interface, including an interface alias.
    InterfaceType,
    /// An interface resource.
    Resource,
    /// A nominal type declared outside an interface.
    OutsideInterfaceNominal,
    /// An alias outside an interface. The converter expands it only when its
    /// target is present in [`BoundaryTypeContext::aliases`].
    ModuleAlias,
}

/// Caller-supplied target for a module alias.
///
/// The target and generic parameters must be lowered HIR from `owner`;
/// callers are responsible for supplying their original path resolutions in
/// [`BoundaryTypeContext::path_resolutions`].
#[derive(Clone, Copy, Debug)]
pub struct BoundaryAlias<'hir> {
    pub owner: OwnerId,
    pub ty: &'hir Ty<'hir>,
    /// Parameters in declaration order, including both type and const
    /// parameters. Their HIR identities are used for type substitution.
    pub generic_params: &'hir [GenericParam<'hir>],
}

/// Whether a named interface type contains a resource handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResourceContainment {
    No,
    Yes,
    /// The caller did not have enough type information to decide.
    Unknown,
}

/// Why a resolved HIR type cannot cross the WIT boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IncompatibleReason {
    PointerSizedInteger,
    Never,
    FixedLengthArray,
    FunctionPointer,
    TypeParameter,
    Box,
    BorrowOfNonResource,
    BorrowOutsideParameter,
    StrOutsideParameter,
    SliceOutsideParameter,
    SliceContainsResource,
    UnitOutsideResult,
    OptionUnit,
    OutsideInterfaceNominal,
    DistinctType,
    RefinedType,
}

/// The smallest HIR type span that contains the incompatibility.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Incompatible {
    pub span: Span,
    pub reason: IncompatibleReason,
}

/// Result of converting one boundary type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundaryTypeOutcome {
    Mapped(BoundaryType),
    Incompatible(Incompatible),
    /// An existing HIR or resolution error owns the diagnostic. Do not add a
    /// cascading `WIT_INCOMPATIBLE_TYPE`.
    Suppressed,
    /// The caller has not supplied enough identity or alias information to
    /// convert this type safely.
    Deferred,
}

/// A local interface type reached while expanding a module alias.
///
/// The span is the alias use in the interface declaration, so consumers can
/// attach interface-use graph edges to source that actually emits the WIT
/// reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryTypeReference {
    pub definition: DefId,
    pub span: Span,
}

/// A boundary conversion together with references hidden by module aliases.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryTypeConversion {
    pub outcome: BoundaryTypeOutcome,
    pub references: Vec<BoundaryTypeReference>,
}

/// Immutable inputs used by [`boundary_type`].
///
/// `path_resolutions` must contain original owner path resolutions, not
/// type-checking redirects. Missing paths are deferred unless the HIR already
/// records an error name or error type.
#[derive(Clone, Copy)]
pub struct BoundaryTypeContext<'a> {
    pub path_resolutions: &'a HashMap<HirId, PathRes>,
    pub canonical_types: &'a HashMap<DefId, CanonicalBoundaryType>,
    pub definitions: &'a HashMap<DefId, BoundaryDefinition>,
    pub aliases: &'a HashMap<DefId, BoundaryAlias<'a>>,
    pub resource_containment: Option<&'a HashMap<DefId, ResourceContainment>>,
    /// The enclosing resource identity for `Self` in a resource member.
    pub self_resource: Option<DefId>,
}

/// Convert one HIR type into an index-free WIT type representation.
///
/// No database access or spelling-based prelude recognition occurs here.
pub fn boundary_type<'hir>(
    context: &BoundaryTypeContext<'hir>,
    owner: OwnerId,
    ty: &Ty<'hir>,
    position: Position,
) -> BoundaryTypeOutcome {
    boundary_type_with_references(context, owner, ty, position).outcome
}

/// Convert one HIR type and expose local interface references that become
/// visible only after expanding a module alias.
pub fn boundary_type_with_references<'hir>(
    context: &BoundaryTypeContext<'hir>,
    owner: OwnerId,
    ty: &Ty<'hir>,
    position: Position,
) -> BoundaryTypeConversion {
    if ty.hir_id.owner != owner {
        return BoundaryTypeConversion {
            outcome: BoundaryTypeOutcome::Deferred,
            references: Vec::new(),
        };
    }
    let mut references = Vec::new();
    let outcome = convert_type(
        context,
        owner,
        ty,
        position,
        position == Position::Result,
        &HashMap::new(),
        &mut HashSet::new(),
        None,
        &mut references,
    );
    BoundaryTypeConversion {
        outcome,
        references,
    }
}

#[derive(Clone, Copy)]
struct TypeSubstitution<'hir> {
    ty: &'hir Ty<'hir>,
    self_resource: Option<DefId>,
}

fn convert_type<'hir>(
    context: &BoundaryTypeContext<'hir>,
    owner: OwnerId,
    ty: &Ty<'hir>,
    position: Position,
    allow_unit: bool,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    alias_stack: &mut HashSet<(DefId, HirId)>,
    alias_use_span: Option<Span>,
    references: &mut Vec<BoundaryTypeReference>,
) -> BoundaryTypeOutcome {
    if ty.hir_id.owner != owner {
        return BoundaryTypeOutcome::Deferred;
    }
    match ty.kind {
        TyKind::Err | TyKind::Infer => BoundaryTypeOutcome::Suppressed,
        TyKind::Tuple(elements) if elements.is_empty() => {
            if allow_unit {
                BoundaryTypeOutcome::Mapped(BoundaryType::Unit)
            } else {
                incompatible(ty.span, IncompatibleReason::UnitOutsideResult)
            }
        }
        TyKind::Tuple(elements) => {
            let mut mapped = Vec::with_capacity(elements.len());
            for element in elements {
                match convert_type(
                    context,
                    owner,
                    element,
                    Position::Nested,
                    false,
                    substitutions,
                    alias_stack,
                    alias_use_span,
                    references,
                ) {
                    BoundaryTypeOutcome::Mapped(element) => mapped.push(element),
                    other => return other,
                }
            }
            BoundaryTypeOutcome::Mapped(BoundaryType::Tuple(mapped))
        }
        TyKind::FnPtr { .. } => incompatible(ty.span, IncompatibleReason::FunctionPointer),
        TyKind::Borrow(_, inner) => {
            let inner = match convert_type(
                context,
                owner,
                inner,
                Position::Nested,
                false,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            ) {
                BoundaryTypeOutcome::Mapped(inner) => inner,
                BoundaryTypeOutcome::Incompatible(_) => {
                    return incompatible(ty.span, IncompatibleReason::BorrowOfNonResource);
                }
                other => return other,
            };
            let BoundaryType::Own(resource) = inner else {
                return incompatible(ty.span, IncompatibleReason::BorrowOfNonResource);
            };
            if position != Position::Param {
                return incompatible(ty.span, IncompatibleReason::BorrowOutsideParameter);
            }
            BoundaryTypeOutcome::Mapped(BoundaryType::Borrow(resource))
        }
        TyKind::Path(path) => convert_path(
            context,
            owner,
            ty,
            path,
            position,
            allow_unit,
            substitutions,
            alias_stack,
            alias_use_span,
            references,
        ),
    }
}

fn convert_path<'hir>(
    context: &BoundaryTypeContext<'hir>,
    owner: OwnerId,
    ty: &Ty<'hir>,
    path: Path<'hir>,
    position: Position,
    allow_unit: bool,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    alias_stack: &mut HashSet<(DefId, HirId)>,
    alias_use_span: Option<Span>,
    references: &mut Vec<BoundaryTypeReference>,
) -> BoundaryTypeOutcome {
    if path.hir_id.owner != owner {
        return BoundaryTypeOutcome::Deferred;
    }
    if path.segments.iter().any(|segment| segment.name.is_error()) {
        return BoundaryTypeOutcome::Suppressed;
    }

    if path.root == PathRoot::SelfType {
        if !path.segments.is_empty() {
            return BoundaryTypeOutcome::Deferred;
        }
        return match context.self_resource {
            Some(resource) => BoundaryTypeOutcome::Mapped(BoundaryType::Own(resource)),
            None => BoundaryTypeOutcome::Deferred,
        };
    }

    let (resolution, lang_builtin) = match path.root {
        PathRoot::Plain => {
            let Some(path_res) = context.path_resolutions.get(&path.hir_id) else {
                return BoundaryTypeOutcome::Deferred;
            };
            let Ok(segment_count) = u32::try_from(path.segments.len()) else {
                return BoundaryTypeOutcome::Deferred;
            };
            if path_res.segment_count != segment_count
                || path_res.resolved_segments != segment_count
            {
                return BoundaryTypeOutcome::Deferred;
            }
            (Some(path_res.base), None)
        }
        PathRoot::Lang(LangItem::Option) if path.segments.len() == 1 => {
            (None, Some(CanonicalBoundaryType::Option))
        }
        PathRoot::Lang(LangItem::Result) if path.segments.len() == 1 => {
            (None, Some(CanonicalBoundaryType::Result))
        }
        PathRoot::Lang(LangItem::Option | LangItem::Result) => {
            return BoundaryTypeOutcome::Deferred;
        }
        PathRoot::SelfValue | PathRoot::Lang(_) | PathRoot::SelfType => {
            return BoundaryTypeOutcome::Deferred;
        }
    };

    if let Some(canonical) = lang_builtin.or_else(|| {
        let Some(Res::Def(_, def_id)) = resolution else {
            return None;
        };
        context.canonical_types.get(&def_id).copied()
    }) {
        return convert_canonical(
            context,
            owner,
            ty,
            path,
            canonical,
            position,
            substitutions,
            alias_stack,
            alias_use_span,
            references,
        );
    }

    match resolution.or_else(|| path_res_for(context, &path).map(|path_res| path_res.base)) {
        Some(Res::PrimTy(primitive)) => convert_primitive(primitive, ty.span),
        Some(Res::Def(DefKind::TyParam, def_id)) => {
            let Some(substitution) = substitutions.get(&def_id).copied() else {
                return incompatible(ty.span, IncompatibleReason::TypeParameter);
            };
            // A type argument belongs to the owner where the alias was used.
            // Preserve active alias applications while expanding it. Distinct
            // nested HIR applications such as `A<A<u32>>` remain finite.
            let substitution_context = BoundaryTypeContext {
                self_resource: substitution.self_resource,
                ..*context
            };
            convert_type(
                &substitution_context,
                substitution.ty.hir_id.owner,
                substitution.ty,
                position,
                allow_unit,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            )
        }
        Some(Res::Def(kind, def_id)) => convert_definition(
            context,
            kind,
            def_id,
            ty.span,
            path,
            position,
            allow_unit,
            substitutions,
            alias_stack,
            alias_use_span,
            references,
        ),
        Some(Res::Err | Res::Local(_) | Res::TestOnly(_)) => BoundaryTypeOutcome::Suppressed,
        None => BoundaryTypeOutcome::Deferred,
    }
}

fn path_res_for<'a>(context: &'a BoundaryTypeContext<'_>, path: &Path<'_>) -> Option<&'a PathRes> {
    (path.root == PathRoot::Plain)
        .then(|| context.path_resolutions.get(&path.hir_id))
        .flatten()
}

fn convert_primitive(primitive: PrimTy, span: Span) -> BoundaryTypeOutcome {
    let mapped = match primitive {
        PrimTy::Bool => BoundaryType::Bool,
        PrimTy::I8 => BoundaryType::S8,
        PrimTy::I16 => BoundaryType::S16,
        PrimTy::I32 => BoundaryType::S32,
        PrimTy::I64 => BoundaryType::S64,
        PrimTy::U8 | PrimTy::Byte => BoundaryType::U8,
        PrimTy::U16 => BoundaryType::U16,
        PrimTy::U32 => BoundaryType::U32,
        PrimTy::U64 => BoundaryType::U64,
        PrimTy::F32 => BoundaryType::F32,
        PrimTy::F64 => BoundaryType::F64,
        PrimTy::Char => BoundaryType::Char,
        PrimTy::ISize | PrimTy::USize => {
            return incompatible(span, IncompatibleReason::PointerSizedInteger);
        }
        PrimTy::Never => return incompatible(span, IncompatibleReason::Never),
        PrimTy::Array => return incompatible(span, IncompatibleReason::FixedLengthArray),
    };
    BoundaryTypeOutcome::Mapped(mapped)
}

fn convert_definition<'hir>(
    context: &BoundaryTypeContext<'hir>,
    kind: DefKind,
    def_id: DefId,
    span: Span,
    path: Path<'hir>,
    position: Position,
    allow_unit: bool,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    alias_stack: &mut HashSet<(DefId, HirId)>,
    alias_use_span: Option<Span>,
    references: &mut Vec<BoundaryTypeReference>,
) -> BoundaryTypeOutcome {
    match kind {
        DefKind::TyParam => incompatible(span, IncompatibleReason::TypeParameter),
        DefKind::Distinct => incompatible(span, IncompatibleReason::DistinctType),
        DefKind::Refined => incompatible(span, IncompatibleReason::RefinedType),
        _ => match context.definitions.get(&def_id) {
            Some(BoundaryDefinition::InterfaceType) => {
                if has_generic_arguments(path) {
                    BoundaryTypeOutcome::Deferred
                } else {
                    if let Some(span) = alias_use_span {
                        references.push(BoundaryTypeReference {
                            definition: def_id,
                            span,
                        });
                    }
                    BoundaryTypeOutcome::Mapped(BoundaryType::Named(def_id))
                }
            }
            Some(BoundaryDefinition::Resource) => {
                if has_generic_arguments(path) {
                    BoundaryTypeOutcome::Deferred
                } else {
                    if let Some(span) = alias_use_span {
                        references.push(BoundaryTypeReference {
                            definition: def_id,
                            span,
                        });
                    }
                    BoundaryTypeOutcome::Mapped(BoundaryType::Own(def_id))
                }
            }
            Some(BoundaryDefinition::OutsideInterfaceNominal) => {
                incompatible(span, IncompatibleReason::OutsideInterfaceNominal)
            }
            Some(BoundaryDefinition::ModuleAlias) => {
                let Some(alias) = context.aliases.get(&def_id) else {
                    return BoundaryTypeOutcome::Deferred;
                };
                let application = (def_id, path.hir_id);
                if alias.ty.hir_id.owner != alias.owner || !alias_stack.insert(application) {
                    return BoundaryTypeOutcome::Deferred;
                }
                let alias_substitutions =
                    match alias_type_substitutions(context, alias, path, substitutions) {
                        Ok(substitutions) => substitutions,
                        Err(outcome) => {
                            alias_stack.remove(&application);
                            return outcome;
                        }
                    };
                // An alias target is evaluated in its declaring context.
                // In particular, a `Self` from the using resource member
                // must not be captured while expanding a module alias.
                let alias_context = BoundaryTypeContext {
                    self_resource: None,
                    ..*context
                };
                let outcome = convert_type(
                    &alias_context,
                    alias.owner,
                    alias.ty,
                    position,
                    allow_unit,
                    &alias_substitutions,
                    alias_stack,
                    Some(alias_use_span.unwrap_or(span)),
                    references,
                );
                alias_stack.remove(&application);
                match outcome {
                    BoundaryTypeOutcome::Incompatible(finding) => {
                        incompatible(span, finding.reason)
                    }
                    other => other,
                }
            }
            None => BoundaryTypeOutcome::Deferred,
        },
    }
}

fn alias_type_substitutions<'hir>(
    context: &BoundaryTypeContext<'hir>,
    alias: &BoundaryAlias<'hir>,
    path: Path<'hir>,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
) -> Result<HashMap<DefId, TypeSubstitution<'hir>>, BoundaryTypeOutcome> {
    let arguments = alias_generic_arguments(path).ok_or(BoundaryTypeOutcome::Deferred)?;
    if alias.generic_params.is_empty() {
        if arguments.is_some() {
            return Err(BoundaryTypeOutcome::Deferred);
        }
        return Ok(substitutions.clone());
    }
    let Some(arguments) = arguments else {
        return Err(BoundaryTypeOutcome::Deferred);
    };
    if arguments.len() != alias.generic_params.len() {
        return Err(BoundaryTypeOutcome::Deferred);
    }

    let mut expanded = substitutions.clone();
    for (parameter, argument) in alias.generic_params.iter().zip(arguments) {
        match (parameter.kind, argument) {
            (GenericParamKind::Type { .. }, GenericArg::Type(ty)) => {
                validate_type_argument(context, ty, substitutions)?;
                let def_id = parameter.def_id.to_def_id();
                // A nested application of the same generic alias reuses
                // the declaration's parameter identities. The nested
                // environment must shadow the outer application; the caller
                // keeps its own map because this map is cloned per alias.
                expanded.insert(
                    def_id,
                    TypeSubstitution {
                        ty: *ty,
                        self_resource: context.self_resource,
                    },
                );
            }
            (GenericParamKind::Const { .. }, GenericArg::Const(_)) => {}
            (GenericParamKind::Type { .. }, _) | (GenericParamKind::Const { .. }, _) => {
                return Err(BoundaryTypeOutcome::Deferred);
            }
        }
    }
    Ok(expanded)
}

fn alias_generic_arguments<'hir>(path: Path<'hir>) -> Option<Option<&'hir [GenericArg<'hir>]>> {
    if path.segments.is_empty() {
        return None;
    }
    let mut arguments = None;
    for (index, segment) in path.segments.iter().enumerate() {
        let Some(generic_args) = segment.args else {
            continue;
        };
        if index + 1 != path.segments.len() || arguments.is_some() {
            return None;
        }
        arguments = Some(generic_args.args);
    }
    Some(arguments)
}

fn validate_type_argument<'hir>(
    context: &BoundaryTypeContext<'hir>,
    ty: &'hir Ty<'hir>,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
) -> Result<(), BoundaryTypeOutcome> {
    validate_type_paths(context, ty, substitutions, &mut HashSet::new())?;
    Ok(())
}

fn validate_type_paths<'hir>(
    context: &BoundaryTypeContext<'hir>,
    ty: &'hir Ty<'hir>,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    substitution_stack: &mut HashSet<DefId>,
) -> Result<(), BoundaryTypeOutcome> {
    match ty.kind {
        TyKind::Err | TyKind::Infer => Err(BoundaryTypeOutcome::Suppressed),
        TyKind::Path(path) => {
            if ty.hir_id.owner != path.hir_id.owner
                || path.segments.iter().any(|segment| segment.name.is_error())
            {
                return Err(BoundaryTypeOutcome::Suppressed);
            }
            match path.root {
                PathRoot::Plain => {
                    let Some(path_res) = context.path_resolutions.get(&path.hir_id) else {
                        return Err(BoundaryTypeOutcome::Deferred);
                    };
                    let Ok(segment_count) = u32::try_from(path.segments.len()) else {
                        return Err(BoundaryTypeOutcome::Deferred);
                    };
                    if path_res.segment_count != segment_count
                        || path_res.resolved_segments != segment_count
                    {
                        return Err(BoundaryTypeOutcome::Deferred);
                    }
                    match path_res.base {
                        Res::Err | Res::Local(_) | Res::TestOnly(_) => {
                            return Err(BoundaryTypeOutcome::Suppressed);
                        }
                        Res::Def(DefKind::TyParam, def_id) => {
                            if let Some(substitution) = substitutions.get(&def_id) {
                                if !substitution_stack.insert(def_id) {
                                    return Err(BoundaryTypeOutcome::Deferred);
                                }
                                let result = validate_type_paths(
                                    context,
                                    substitution.ty,
                                    substitutions,
                                    substitution_stack,
                                );
                                substitution_stack.remove(&def_id);
                                result?;
                            }
                        }
                        Res::Def(DefKind::Alias, def_id)
                            if context.definitions.get(&def_id).is_some_and(|definition| {
                                *definition == BoundaryDefinition::ModuleAlias
                            }) =>
                        {
                            let Some(alias) = context.aliases.get(&def_id) else {
                                return Err(BoundaryTypeOutcome::Deferred);
                            };
                            // Even an unused type argument must not hide a
                            // malformed nested application.
                            alias_type_substitutions(context, alias, path, substitutions)?;
                        }
                        _ => {}
                    }
                }
                PathRoot::SelfType if path.segments.is_empty() => {
                    if context.self_resource.is_none() {
                        return Err(BoundaryTypeOutcome::Deferred);
                    }
                }
                PathRoot::Lang(LangItem::Option | LangItem::Result) if path.segments.len() == 1 => {
                }
                PathRoot::SelfType | PathRoot::SelfValue | PathRoot::Lang(_) => {
                    return Err(BoundaryTypeOutcome::Deferred);
                }
            }
            for segment in path.segments {
                let Some(arguments) = segment.args else {
                    continue;
                };
                for argument in arguments.args {
                    match argument {
                        GenericArg::Type(ty) | GenericArg::Binding { ty, .. } => {
                            validate_type_paths(context, ty, substitutions, substitution_stack)?;
                        }
                        GenericArg::Const(_) => {}
                    }
                }
            }
            Ok(())
        }
        TyKind::Borrow(_, inner) => {
            validate_type_paths(context, inner, substitutions, substitution_stack)
        }
        TyKind::Tuple(elements) => {
            for element in elements.iter() {
                validate_type_paths(context, element, substitutions, substitution_stack)?;
            }
            Ok(())
        }
        TyKind::FnPtr { params, ret, .. } => {
            for parameter in params.iter() {
                validate_type_paths(context, parameter, substitutions, substitution_stack)?;
            }
            if let Some(ret) = ret {
                validate_type_paths(context, ret, substitutions, substitution_stack)?;
            }
            Ok(())
        }
    }
}

fn convert_canonical<'hir>(
    context: &BoundaryTypeContext<'hir>,
    owner: OwnerId,
    ty: &Ty<'hir>,
    path: Path<'hir>,
    canonical: CanonicalBoundaryType,
    position: Position,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    alias_stack: &mut HashSet<(DefId, HirId)>,
    alias_use_span: Option<Span>,
    references: &mut Vec<BoundaryTypeReference>,
) -> BoundaryTypeOutcome {
    match canonical {
        CanonicalBoundaryType::String => {
            if has_generic_arguments(path) {
                BoundaryTypeOutcome::Deferred
            } else {
                BoundaryTypeOutcome::Mapped(BoundaryType::String)
            }
        }
        CanonicalBoundaryType::Str => {
            if has_generic_arguments(path) {
                BoundaryTypeOutcome::Deferred
            } else if position == Position::Param {
                BoundaryTypeOutcome::Mapped(BoundaryType::String)
            } else {
                incompatible(ty.span, IncompatibleReason::StrOutsideParameter)
            }
        }
        CanonicalBoundaryType::Bytes => {
            if has_generic_arguments(path) {
                BoundaryTypeOutcome::Deferred
            } else {
                BoundaryTypeOutcome::Mapped(BoundaryType::List(Box::new(BoundaryType::U8)))
            }
        }
        CanonicalBoundaryType::Box => incompatible(ty.span, IncompatibleReason::Box),
        CanonicalBoundaryType::FutureReader | CanonicalBoundaryType::StreamReader => {
            BoundaryTypeOutcome::Deferred
        }
        CanonicalBoundaryType::Vec | CanonicalBoundaryType::Option => {
            let Some(arguments) = type_arguments(path) else {
                return BoundaryTypeOutcome::Deferred;
            };
            if arguments.len() != 1 {
                return BoundaryTypeOutcome::Deferred;
            }
            let element = arguments[0];
            if canonical == CanonicalBoundaryType::Option && is_unit(element) {
                return incompatible(ty.span, IncompatibleReason::OptionUnit);
            }
            let element = match convert_type(
                context,
                owner,
                element,
                Position::Nested,
                false,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            ) {
                BoundaryTypeOutcome::Mapped(element) => element,
                other => return other,
            };
            let mapped = if canonical == CanonicalBoundaryType::Vec {
                BoundaryType::List(Box::new(element))
            } else {
                BoundaryType::Option(Box::new(element))
            };
            BoundaryTypeOutcome::Mapped(mapped)
        }
        CanonicalBoundaryType::Slice => {
            if position != Position::Param {
                return incompatible(ty.span, IncompatibleReason::SliceOutsideParameter);
            }
            let Some(arguments) = type_arguments(path) else {
                return BoundaryTypeOutcome::Deferred;
            };
            if arguments.len() != 1 {
                return BoundaryTypeOutcome::Deferred;
            }
            let element = arguments[0];
            let element = match convert_type(
                context,
                owner,
                element,
                Position::Nested,
                false,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            ) {
                BoundaryTypeOutcome::Mapped(element) => element,
                other => return other,
            };
            match contains_resource(context, &element) {
                Some(true) => incompatible(ty.span, IncompatibleReason::SliceContainsResource),
                Some(false) => BoundaryTypeOutcome::Mapped(BoundaryType::List(Box::new(element))),
                None => BoundaryTypeOutcome::Deferred,
            }
        }
        CanonicalBoundaryType::Result => {
            let Some(arguments) = type_arguments(path) else {
                return BoundaryTypeOutcome::Deferred;
            };
            if arguments.len() != 2 {
                return BoundaryTypeOutcome::Deferred;
            }
            let (ok, err) = (arguments[0], arguments[1]);
            let ok = match convert_type(
                context,
                owner,
                ok,
                Position::Nested,
                true,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            ) {
                BoundaryTypeOutcome::Mapped(BoundaryType::Unit) => None,
                BoundaryTypeOutcome::Mapped(mapped) => Some(Box::new(mapped)),
                other => return other,
            };
            let err = match convert_type(
                context,
                owner,
                err,
                Position::Nested,
                true,
                substitutions,
                alias_stack,
                alias_use_span,
                references,
            ) {
                BoundaryTypeOutcome::Mapped(BoundaryType::Unit) => None,
                BoundaryTypeOutcome::Mapped(mapped) => Some(Box::new(mapped)),
                other => return other,
            };
            BoundaryTypeOutcome::Mapped(BoundaryType::Result { ok, err })
        }
    }
}

fn type_arguments(path: Path<'_>) -> Option<Vec<&Ty<'_>>> {
    if path.segments.is_empty() {
        return None;
    }
    let mut arguments = None;
    for (index, segment) in path.segments.iter().enumerate() {
        let Some(generic_args) = segment.args else {
            continue;
        };
        if index + 1 != path.segments.len() || arguments.is_some() {
            return None;
        }
        let types = generic_args
            .args
            .iter()
            .map(|argument| match argument {
                GenericArg::Type(ty) => Some(*ty),
                GenericArg::Const(_) | GenericArg::Binding { .. } => None,
            })
            .collect::<Option<Vec<_>>>()?;
        arguments = Some(types);
    }
    arguments
}

fn has_generic_arguments(path: Path<'_>) -> bool {
    path.segments.iter().any(|segment| segment.args.is_some())
}

fn is_unit(ty: &Ty<'_>) -> bool {
    matches!(ty.kind, TyKind::Tuple(elements) if elements.is_empty())
}

fn contains_resource(context: &BoundaryTypeContext<'_>, ty: &BoundaryType) -> Option<bool> {
    match ty {
        BoundaryType::Own(_) | BoundaryType::Borrow(_) => Some(true),
        BoundaryType::Named(def_id) => context
            .resource_containment
            .and_then(|containment| containment.get(def_id))
            .map(|containment| match containment {
                ResourceContainment::No => Some(false),
                ResourceContainment::Yes => Some(true),
                ResourceContainment::Unknown => None,
            })
            .flatten(),
        BoundaryType::List(inner) | BoundaryType::Option(inner) => {
            contains_resource(context, inner)
        }
        BoundaryType::Result { ok, err } => combine_resource_facts(
            ok.as_deref()
                .map(|inner| contains_resource(context, inner))
                .unwrap_or(Some(false)),
            err.as_deref()
                .map(|inner| contains_resource(context, inner))
                .unwrap_or(Some(false)),
        ),
        BoundaryType::Tuple(elements) => {
            let mut contains = false;
            for element in elements {
                match contains_resource(context, element) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => contains = true,
                }
            }
            if contains { None } else { Some(false) }
        }
        BoundaryType::Unit
        | BoundaryType::Bool
        | BoundaryType::S8
        | BoundaryType::S16
        | BoundaryType::S32
        | BoundaryType::S64
        | BoundaryType::U8
        | BoundaryType::U16
        | BoundaryType::U32
        | BoundaryType::U64
        | BoundaryType::F32
        | BoundaryType::F64
        | BoundaryType::Char
        | BoundaryType::String => Some(false),
    }
}

fn combine_resource_facts(left: Option<bool>, right: Option<bool>) -> Option<bool> {
    match (left, right) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

fn incompatible(span: Span, reason: IncompatibleReason) -> BoundaryTypeOutcome {
    BoundaryTypeOutcome::Incompatible(Incompatible { span, reason })
}

/// Why a boundary type could not be materialized as a WIT model type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryWitTypeError {
    /// A named SAI boundary type has no generated or viewed WIT item.
    MissingNamedReference(DefId),
    /// A resource handle has no generated or viewed WIT resource item.
    MissingResourceReference(DefId),
    /// Unit is only representable as the absence of a function result.
    UnitOutsideFunctionResult,
}

impl fmt::Display for BoundaryWitTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNamedReference(definition) => {
                write!(
                    formatter,
                    "missing WIT reference for named type {definition:?}"
                )
            }
            Self::MissingResourceReference(definition) => write!(
                formatter,
                "missing WIT reference for resource type {definition:?}"
            ),
            Self::UnitOutsideFunctionResult => {
                formatter.write_str("unit is only valid as a function result")
            }
        }
    }
}

impl std::error::Error for BoundaryWitTypeError {}

/// Materialize an index-free boundary type into the WIT model.
///
/// `references` must contain each named interface type and resource reachable
/// from `ty`, keyed by its resolved definition identity. The caller supplies
/// `Position::Result` only for a function's outermost result; unit there is
/// represented by `Ok(None)`, while unit anywhere nested is an error.
pub fn materialize_boundary_type(
    ty: &BoundaryType,
    references: &HashMap<DefId, WitTypeRef>,
    position: Position,
) -> Result<Option<WitType>, BoundaryWitTypeError> {
    match ty {
        BoundaryType::Unit if position == Position::Result => Ok(None),
        BoundaryType::Unit => Err(BoundaryWitTypeError::UnitOutsideFunctionResult),
        BoundaryType::Bool => Ok(Some(WitType::Bool)),
        BoundaryType::S8 => Ok(Some(WitType::S8)),
        BoundaryType::S16 => Ok(Some(WitType::S16)),
        BoundaryType::S32 => Ok(Some(WitType::S32)),
        BoundaryType::S64 => Ok(Some(WitType::S64)),
        BoundaryType::U8 => Ok(Some(WitType::U8)),
        BoundaryType::U16 => Ok(Some(WitType::U16)),
        BoundaryType::U32 => Ok(Some(WitType::U32)),
        BoundaryType::U64 => Ok(Some(WitType::U64)),
        BoundaryType::F32 => Ok(Some(WitType::F32)),
        BoundaryType::F64 => Ok(Some(WitType::F64)),
        BoundaryType::Char => Ok(Some(WitType::Char)),
        BoundaryType::String => Ok(Some(WitType::String)),
        BoundaryType::List(inner) => Ok(Some(WitType::List(Box::new(
            materialize_nested_boundary_type(inner, references)?,
        )))),
        BoundaryType::Option(inner) => Ok(Some(WitType::Option(Box::new(
            materialize_nested_boundary_type(inner, references)?,
        )))),
        BoundaryType::Result { ok, err } => {
            let ok = ok
                .as_deref()
                .map(|ty| materialize_nested_boundary_type(ty, references).map(Box::new))
                .transpose()?;
            let err = err
                .as_deref()
                .map(|ty| materialize_nested_boundary_type(ty, references).map(Box::new))
                .transpose()?;
            Ok(Some(WitType::Result { ok, err }))
        }
        BoundaryType::Tuple(elements) => Ok(Some(WitType::Tuple(
            elements
                .iter()
                .map(|ty| materialize_nested_boundary_type(ty, references))
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        BoundaryType::Named(definition) => references
            .get(definition)
            .cloned()
            .map(WitType::Named)
            .map(Some)
            .ok_or(BoundaryWitTypeError::MissingNamedReference(*definition)),
        BoundaryType::Own(definition) => references
            .get(definition)
            .cloned()
            .map(WitType::Own)
            .map(Some)
            .ok_or(BoundaryWitTypeError::MissingResourceReference(*definition)),
        BoundaryType::Borrow(definition) => references
            .get(definition)
            .cloned()
            .map(WitType::Borrow)
            .map(Some)
            .ok_or(BoundaryWitTypeError::MissingResourceReference(*definition)),
    }
}

fn materialize_nested_boundary_type(
    ty: &BoundaryType,
    references: &HashMap<DefId, WitTypeRef>,
) -> Result<WitType, BoundaryWitTypeError> {
    // Nested values can never denote an absent function result. Keeping this
    // assertion local makes the `Option<WitType>` at the public boundary
    // unambiguously mean "no function result".
    materialize_boundary_type(ty, references, Position::Nested)?
        .ok_or(BoundaryWitTypeError::UnitOutsideFunctionResult)
}

#[cfg(test)]
mod tests {
    use super::{
        BoundaryAlias, BoundaryDefinition, BoundaryType, BoundaryTypeContext, BoundaryTypeOutcome,
        BoundaryTypeReference, BoundaryWitTypeError, CanonicalBoundaryType, Incompatible,
        IncompatibleReason, Position, ResourceContainment, boundary_type,
        boundary_type_with_references, materialize_boundary_type,
    };
    use sai_hir::types::{
        ConstArg, GenericArg, GenericArgs, GenericParam, GenericParamKind, LangItem, Name, Path,
        PathRoot, PathSegment, Ty, TyKind,
    };
    use sai_hir::{
        DefId, DefIndex, DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId,
        ItemLocalId, LocalDefId, Mutability, OwnerId, Span, SpanCtxt,
    };
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_resolve::package_scopes::{DefKind, PathRes, Res};
    use sai_resolve::prelude::PrimTy;
    use sai_vfs::{FileId, TextRange, TextSize};
    use sai_wit::WitType;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    struct TestIds {
        owner: OwnerId,
        alias_owner: OwnerId,
        second_alias_owner: OwnerId,
        alias_type_param: LocalDefId,
        second_alias_type_param: LocalDefId,
        package: sai_hir::PackageId,
        symbol: sai_hir::Symbol,
    }

    fn test_ids() -> TestIds {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:boundary#1.0")
            .expect("package interns");
        let symbols = Interner::new();
        let symbol = symbols.intern("T").expect("symbol interns");
        let paths =
            DefPathInterner::new(package, &packages, &symbols).expect("path interner builds");
        let intern_owner = |name, disambiguator| {
            let path = DefPath::new(
                package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(name),
                    disambiguator,
                }],
                &packages,
                &symbols,
            )
            .expect("owner path builds");
            let def_id = paths.intern(path).expect("owner definition interns");
            (OwnerId { def_id }, def_id)
        };
        let (owner, _) = intern_owner(symbol, 0);
        let alias_name = symbols.intern("Alias").expect("alias name interns");
        let second_alias_name = symbols.intern("NextAlias").expect("alias name interns");
        let (alias_owner, alias_local_def) = intern_owner(alias_name, 0);
        let (second_alias_owner, second_alias_local_def) = intern_owner(second_alias_name, 0);
        let alias_type_param = paths
            .intern_generic_param(alias_local_def, symbol, false, 0)
            .expect("alias type parameter interns");
        let second_alias_type_param = paths
            .intern_generic_param(second_alias_local_def, symbol, false, 0)
            .expect("second alias type parameter interns");
        TestIds {
            owner,
            alias_owner,
            second_alias_owner,
            alias_type_param,
            second_alias_type_param,
            package,
            symbol,
        }
    }

    fn def_id(ids: &TestIds, raw: u32) -> DefId {
        DefId::from_foreign(ids.package, DefIndex::from_raw(raw))
    }

    fn hir_id(owner: OwnerId, raw: u32) -> HirId {
        HirId {
            owner,
            local_id: ItemLocalId::from_raw(raw),
        }
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("valid test span"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn path_type<'hir>(
        owner: OwnerId,
        symbol: sai_hir::Symbol,
        id: u32,
        start: u32,
        end: u32,
    ) -> Ty<'hir> {
        let hir_id = hir_id(owner, id);
        let name = Name {
            sym: symbol,
            span: span(start, end),
        };
        let segment = Box::leak(Box::new(PathSegment {
            hir_id,
            name,
            args: None,
        }));
        Ty {
            hir_id,
            kind: TyKind::Path(Path {
                hir_id,
                root: PathRoot::Plain,
                segments: std::slice::from_ref(segment),
                span: span(start, end),
            }),
            span: span(start, end),
        }
    }

    fn path_type_with_args<'hir>(
        owner: OwnerId,
        symbol: sai_hir::Symbol,
        id: u32,
        start: u32,
        end: u32,
        args: &'hir [GenericArg<'hir>],
    ) -> Ty<'hir> {
        let hir_id = hir_id(owner, id);
        let name = Name {
            sym: symbol,
            span: span(start, end),
        };
        let generic_args = Box::leak(Box::new(GenericArgs {
            args,
            span: span(start, end),
        }));
        let segment = Box::leak(Box::new(PathSegment {
            hir_id,
            name,
            args: Some(generic_args),
        }));
        Ty {
            hir_id,
            kind: TyKind::Path(Path {
                hir_id,
                root: PathRoot::Plain,
                segments: std::slice::from_ref(segment),
                span: span(start, end),
            }),
            span: span(start, end),
        }
    }

    fn type_generic_param<'hir>(def_id: LocalDefId, symbol: sai_hir::Symbol) -> GenericParam<'hir> {
        GenericParam {
            def_id,
            name: Name {
                sym: symbol,
                span: span(0, 1),
            },
            kind: GenericParamKind::Type { bounds: &[] },
        }
    }

    fn insert_path(paths: &mut HashMap<HirId, PathRes>, ty: &Ty<'_>, base: Res) {
        let TyKind::Path(path) = ty.kind else {
            panic!("expected path type");
        };
        let segment_count = u32::try_from(path.segments.len()).expect("test path is short");
        paths.insert(
            path.hir_id,
            PathRes {
                base,
                resolved_segments: segment_count,
                segment_count,
            },
        );
    }

    fn make_context<'a>(
        paths: &'a HashMap<HirId, PathRes>,
        canonical: &'a HashMap<DefId, CanonicalBoundaryType>,
        definitions: &'a HashMap<DefId, BoundaryDefinition>,
        resource_containment: Option<&'a HashMap<DefId, ResourceContainment>>,
        self_resource: Option<DefId>,
    ) -> BoundaryTypeContext<'a> {
        make_context_with_aliases(
            paths,
            canonical,
            definitions,
            empty_aliases(),
            resource_containment,
            self_resource,
        )
    }

    fn empty_aliases() -> &'static HashMap<DefId, BoundaryAlias<'static>> {
        static ALIASES: OnceLock<HashMap<DefId, BoundaryAlias<'static>>> = OnceLock::new();
        ALIASES.get_or_init(HashMap::new)
    }

    fn make_context_with_aliases<'a>(
        paths: &'a HashMap<HirId, PathRes>,
        canonical: &'a HashMap<DefId, CanonicalBoundaryType>,
        definitions: &'a HashMap<DefId, BoundaryDefinition>,
        aliases: &'a HashMap<DefId, BoundaryAlias<'a>>,
        resource_containment: Option<&'a HashMap<DefId, ResourceContainment>>,
        self_resource: Option<DefId>,
    ) -> BoundaryTypeContext<'a> {
        BoundaryTypeContext {
            path_resolutions: paths,
            canonical_types: canonical,
            definitions,
            aliases,
            resource_containment,
            self_resource,
        }
    }

    #[test]
    fn materializes_primitives_and_structural_types_recursively() {
        let references = HashMap::new();
        let primitive_cases = [
            (BoundaryType::Bool, WitType::Bool),
            (BoundaryType::S8, WitType::S8),
            (BoundaryType::S16, WitType::S16),
            (BoundaryType::S32, WitType::S32),
            (BoundaryType::S64, WitType::S64),
            (BoundaryType::U8, WitType::U8),
            (BoundaryType::U16, WitType::U16),
            (BoundaryType::U32, WitType::U32),
            (BoundaryType::U64, WitType::U64),
            (BoundaryType::F32, WitType::F32),
            (BoundaryType::F64, WitType::F64),
            (BoundaryType::Char, WitType::Char),
            (BoundaryType::String, WitType::String),
        ];
        for (boundary, wit) in primitive_cases {
            assert_eq!(
                materialize_boundary_type(&boundary, &references, Position::Nested),
                Ok(Some(wit))
            );
        }

        let boundary = BoundaryType::Tuple(vec![
            BoundaryType::List(Box::new(BoundaryType::U8)),
            BoundaryType::Option(Box::new(BoundaryType::String)),
            BoundaryType::Result {
                ok: Some(Box::new(BoundaryType::Tuple(vec![
                    BoundaryType::Bool,
                    BoundaryType::F64,
                ]))),
                err: None,
            },
            BoundaryType::Result {
                ok: None,
                err: Some(Box::new(BoundaryType::Char)),
            },
        ]);
        let expected = WitType::Tuple(vec![
            WitType::List(Box::new(WitType::U8)),
            WitType::Option(Box::new(WitType::String)),
            WitType::Result {
                ok: Some(Box::new(WitType::Tuple(vec![WitType::Bool, WitType::F64]))),
                err: None,
            },
            WitType::Result {
                ok: None,
                err: Some(Box::new(WitType::Char)),
            },
        ]);
        assert_eq!(
            materialize_boundary_type(&boundary, &references, Position::Nested),
            Ok(Some(expected))
        );
        for (boundary, expected) in [
            (
                BoundaryType::Result {
                    ok: None,
                    err: Some(Box::new(BoundaryType::U32)),
                },
                WitType::Result {
                    ok: None,
                    err: Some(Box::new(WitType::U32)),
                },
            ),
            (
                BoundaryType::Result {
                    ok: Some(Box::new(BoundaryType::U32)),
                    err: None,
                },
                WitType::Result {
                    ok: Some(Box::new(WitType::U32)),
                    err: None,
                },
            ),
        ] {
            assert_eq!(
                materialize_boundary_type(&boundary, &references, Position::Result),
                Ok(Some(expected)),
                "a unit Result arm is encoded by the missing WIT payload"
            );
        }
    }

    #[test]
    fn materializes_unit_only_as_the_outer_function_result() {
        let references = HashMap::new();
        assert_eq!(
            materialize_boundary_type(&BoundaryType::Unit, &references, Position::Result),
            Ok(None)
        );
        assert_eq!(
            materialize_boundary_type(&BoundaryType::Unit, &references, Position::Nested),
            Err(BoundaryWitTypeError::UnitOutsideFunctionResult)
        );
        assert_eq!(
            materialize_boundary_type(&BoundaryType::Unit, &references, Position::Param),
            Err(BoundaryWitTypeError::UnitOutsideFunctionResult)
        );
        assert_eq!(
            materialize_boundary_type(
                &BoundaryType::List(Box::new(BoundaryType::Unit)),
                &references,
                Position::Nested
            ),
            Err(BoundaryWitTypeError::UnitOutsideFunctionResult)
        );
    }

    #[test]
    fn reports_missing_named_and_resource_references_by_kind() {
        let ids = test_ids();
        let references = HashMap::new();
        assert_eq!(
            materialize_boundary_type(
                &BoundaryType::Named(def_id(&ids, 90)),
                &references,
                Position::Nested
            ),
            Err(BoundaryWitTypeError::MissingNamedReference(def_id(
                &ids, 90
            )))
        );
        assert_eq!(
            materialize_boundary_type(
                &BoundaryType::Own(def_id(&ids, 91)),
                &references,
                Position::Nested
            ),
            Err(BoundaryWitTypeError::MissingResourceReference(def_id(
                &ids, 91
            )))
        );
        assert_eq!(
            materialize_boundary_type(
                &BoundaryType::Borrow(def_id(&ids, 92)),
                &references,
                Position::Nested
            ),
            Err(BoundaryWitTypeError::MissingResourceReference(def_id(
                &ids, 92
            )))
        );
    }

    #[test]
    fn primitive_mappings_and_unmapped_primitives_are_identity_based() {
        let ids = test_ids();
        let primitives = [
            (PrimTy::Bool, BoundaryType::Bool),
            (PrimTy::I8, BoundaryType::S8),
            (PrimTy::I16, BoundaryType::S16),
            (PrimTy::I32, BoundaryType::S32),
            (PrimTy::I64, BoundaryType::S64),
            (PrimTy::U8, BoundaryType::U8),
            (PrimTy::Byte, BoundaryType::U8),
            (PrimTy::U16, BoundaryType::U16),
            (PrimTy::U32, BoundaryType::U32),
            (PrimTy::U64, BoundaryType::U64),
            (PrimTy::F32, BoundaryType::F32),
            (PrimTy::F64, BoundaryType::F64),
            (PrimTy::Char, BoundaryType::Char),
        ];
        let mut paths = HashMap::new();
        let types = primitives
            .iter()
            .enumerate()
            .map(|(index, (primitive, _))| {
                let ty = path_type(ids.owner, ids.symbol, index as u32 + 1, 10, 14);
                insert_path(&mut paths, &ty, Res::PrimTy(*primitive));
                ty
            })
            .collect::<Vec<_>>();
        let canonical = HashMap::new();
        let definitions = HashMap::new();
        let context = make_context(&paths, &canonical, &definitions, None, None);

        for ((_, expected), ty) in primitives.into_iter().zip(&types) {
            assert_eq!(
                boundary_type(&context, ids.owner, ty, Position::Nested),
                BoundaryTypeOutcome::Mapped(expected)
            );
        }

        for (index, primitive) in [PrimTy::ISize, PrimTy::USize, PrimTy::Never, PrimTy::Array]
            .into_iter()
            .enumerate()
        {
            let ty = path_type(ids.owner, ids.symbol, 100 + index as u32, 20, 28);
            let mut paths = paths.clone();
            insert_path(&mut paths, &ty, Res::PrimTy(primitive));
            let context = make_context(&paths, &canonical, &definitions, None, None);
            let BoundaryTypeOutcome::Incompatible(issue) =
                boundary_type(&context, ids.owner, &ty, Position::Param)
            else {
                panic!("unsupported primitive should be incompatible");
            };
            assert_eq!(
                issue.reason,
                match primitive {
                    PrimTy::ISize | PrimTy::USize => IncompatibleReason::PointerSizedInteger,
                    PrimTy::Never => IncompatibleReason::Never,
                    PrimTy::Array => IncompatibleReason::FixedLengthArray,
                    _ => unreachable!(),
                }
            );
            assert_eq!(issue.span, ty.span);
        }
    }

    #[test]
    fn structural_types_preserve_positions_and_first_incompatible_span() {
        let ids = test_ids();
        let unit = Ty {
            hir_id: hir_id(ids.owner, 1),
            kind: TyKind::Tuple(&[]),
            span: span(5, 7),
        };
        let empty_tuple = boundary_type(
            &make_context(
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                None,
            ),
            ids.owner,
            &unit,
            Position::Result,
        );
        assert_eq!(empty_tuple, BoundaryTypeOutcome::Mapped(BoundaryType::Unit));
        let empty_tuple = boundary_type(
            &make_context(
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                None,
            ),
            ids.owner,
            &unit,
            Position::Nested,
        );
        assert!(matches!(
            empty_tuple,
            BoundaryTypeOutcome::Incompatible(Incompatible {
                reason: IncompatibleReason::UnitOutsideResult,
                ..
            })
        ));

        let fn_ptr = Ty {
            hir_id: hir_id(ids.owner, 2),
            kind: TyKind::FnPtr {
                safety: sai_hir::Safety::Safe,
                params: &[],
                ret: None,
                effects: None,
            },
            span: span(10, 20),
        };
        let result = boundary_type(
            &make_context(
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                None,
            ),
            ids.owner,
            &fn_ptr,
            Position::Param,
        );
        assert_eq!(
            result,
            BoundaryTypeOutcome::Incompatible(super::Incompatible {
                span: fn_ptr.span,
                reason: IncompatibleReason::FunctionPointer,
            })
        );

        let bad = Ty {
            hir_id: hir_id(ids.owner, 3),
            kind: TyKind::Path(Path {
                hir_id: hir_id(ids.owner, 3),
                root: PathRoot::Plain,
                segments: &[],
                span: span(31, 35),
            }),
            span: span(31, 35),
        };
        let tuple = Ty {
            hir_id: hir_id(ids.owner, 4),
            kind: TyKind::Tuple(std::slice::from_ref(&bad)),
            span: span(30, 36),
        };
        let result = boundary_type(
            &make_context(
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                None,
            ),
            ids.owner,
            &tuple,
            Position::Nested,
        );
        assert_eq!(result, BoundaryTypeOutcome::Deferred);
    }

    #[test]
    fn expands_known_nongeneric_alias_and_reports_the_interface_use_span() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let target_ty = path_type(ids.alias_owner, ids.symbol, 2, 40, 45);
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(&mut paths, &target_ty, Res::PrimTy(PrimTy::USize));
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &[],
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Incompatible(Incompatible {
                span: use_ty.span,
                reason: IncompatibleReason::PointerSizedInteger,
            })
        );
    }

    #[test]
    fn alias_expansion_exposes_hidden_interface_references_at_the_use_span() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let record_def = ids.second_alias_owner.def_id.to_def_id();
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let target_ty = path_type(ids.alias_owner, ids.symbol, 2, 40, 49);
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(
            &mut paths,
            &target_ty,
            Res::Def(DefKind::Struct, record_def),
        );
        let canonical = HashMap::new();
        let definitions = HashMap::from([
            (alias_def, BoundaryDefinition::ModuleAlias),
            (record_def, BoundaryDefinition::InterfaceType),
        ]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &[],
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type_with_references(&context, ids.owner, &use_ty, Position::Nested),
            super::BoundaryTypeConversion {
                outcome: BoundaryTypeOutcome::Mapped(BoundaryType::Named(record_def)),
                references: vec![BoundaryTypeReference {
                    definition: record_def,
                    span: use_ty.span,
                }],
            }
        );
    }

    #[test]
    fn recursively_expands_alias_chains() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let next_alias_def = ids.second_alias_owner.def_id.to_def_id();
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let next_alias_ty = path_type(ids.alias_owner, ids.symbol, 2, 20, 29);
        let target_ty = path_type(ids.second_alias_owner, ids.symbol, 3, 40, 43);
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(
            &mut paths,
            &next_alias_ty,
            Res::Def(DefKind::Alias, next_alias_def),
        );
        insert_path(&mut paths, &target_ty, Res::PrimTy(PrimTy::U32));
        let canonical = HashMap::new();
        let definitions = HashMap::from([
            (alias_def, BoundaryDefinition::ModuleAlias),
            (next_alias_def, BoundaryDefinition::ModuleAlias),
        ]);
        let aliases = HashMap::from([
            (
                alias_def,
                BoundaryAlias {
                    owner: ids.alias_owner,
                    ty: &next_alias_ty,
                    generic_params: &[],
                },
            ),
            (
                next_alias_def,
                BoundaryAlias {
                    owner: ids.second_alias_owner,
                    ty: &target_ty,
                    generic_params: &[],
                },
            ),
        ]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Nested),
            BoundaryTypeOutcome::Mapped(BoundaryType::U32)
        );
    }

    #[test]
    fn substitutes_type_parameters_in_generic_aliases() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let param_def = ids.alias_type_param.to_def_id();
        let u64_argument = path_type(ids.owner, ids.symbol, 2, 10, 13);
        let direct_arguments = [GenericArg::Type(&u64_argument)];
        let direct_use = path_type_with_args(ids.owner, ids.symbol, 1, 5, 15, &direct_arguments);
        let nested_arguments = [GenericArg::Type(&u64_argument)];
        let nested_application =
            path_type_with_args(ids.owner, ids.symbol, 3, 20, 30, &nested_arguments);
        let nested_use_arguments = [GenericArg::Type(&nested_application)];
        let nested_use =
            path_type_with_args(ids.owner, ids.symbol, 4, 18, 32, &nested_use_arguments);
        let target_ty = path_type(ids.alias_owner, ids.symbol, 5, 40, 41);
        let generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let generic_params = [generic_param];

        let mut paths = HashMap::new();
        insert_path(&mut paths, &direct_use, Res::Def(DefKind::Alias, alias_def));
        insert_path(
            &mut paths,
            &nested_application,
            Res::Def(DefKind::Alias, alias_def),
        );
        insert_path(&mut paths, &nested_use, Res::Def(DefKind::Alias, alias_def));
        insert_path(&mut paths, &u64_argument, Res::PrimTy(PrimTy::U64));
        insert_path(
            &mut paths,
            &target_ty,
            Res::Def(DefKind::TyParam, param_def),
        );
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &generic_params,
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &direct_use, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::U64)
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &nested_use, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::U64),
            "a nested use of the same generic alias is finite, not a definition cycle"
        );
    }

    #[test]
    fn substitutes_outer_generic_argument_through_inner_generic_alias() {
        let ids = test_ids();
        let wrap_def = ids.alias_owner.def_id.to_def_id();
        let outer_def = ids.second_alias_owner.def_id.to_def_id();
        let wrap_param_def = ids.alias_type_param.to_def_id();
        let outer_param_def = ids.second_alias_type_param.to_def_id();

        let u32_argument = path_type(ids.owner, ids.symbol, 2, 10, 13);
        let use_arguments = [GenericArg::Type(&u32_argument)];
        let use_ty = path_type_with_args(ids.owner, ids.symbol, 1, 5, 15, &use_arguments);
        let outer_target_argument = path_type(ids.second_alias_owner, ids.symbol, 4, 25, 26);
        let wrap_arguments = [GenericArg::Type(&outer_target_argument)];
        let outer_target = path_type_with_args(
            ids.second_alias_owner,
            ids.symbol,
            3,
            20,
            30,
            &wrap_arguments,
        );
        let wrap_target = path_type(ids.alias_owner, ids.symbol, 5, 40, 41);
        let wrap_generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let wrap_generic_params = [wrap_generic_param];
        let outer_generic_param = type_generic_param(ids.second_alias_type_param, ids.symbol);
        let outer_generic_params = [outer_generic_param];

        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, outer_def));
        insert_path(&mut paths, &u32_argument, Res::PrimTy(PrimTy::U32));
        insert_path(
            &mut paths,
            &outer_target,
            Res::Def(DefKind::Alias, wrap_def),
        );
        insert_path(
            &mut paths,
            &outer_target_argument,
            Res::Def(DefKind::TyParam, outer_param_def),
        );
        insert_path(
            &mut paths,
            &wrap_target,
            Res::Def(DefKind::TyParam, wrap_param_def),
        );
        let canonical = HashMap::new();
        let definitions = HashMap::from([
            (wrap_def, BoundaryDefinition::ModuleAlias),
            (outer_def, BoundaryDefinition::ModuleAlias),
        ]);
        let aliases = HashMap::from([
            (
                wrap_def,
                BoundaryAlias {
                    owner: ids.alias_owner,
                    ty: &wrap_target,
                    generic_params: &wrap_generic_params,
                },
            ),
            (
                outer_def,
                BoundaryAlias {
                    owner: ids.second_alias_owner,
                    ty: &outer_target,
                    generic_params: &outer_generic_params,
                },
            ),
        ]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::U32)
        );
    }

    #[test]
    fn substitutes_nested_type_structure_through_generic_alias_chain() {
        let ids = test_ids();
        let inner_def = ids.alias_owner.def_id.to_def_id();
        let outer_def = ids.second_alias_owner.def_id.to_def_id();
        let inner_param_def = ids.alias_type_param.to_def_id();
        let outer_param_def = ids.second_alias_type_param.to_def_id();

        let u32_argument = path_type(ids.owner, ids.symbol, 2, 10, 13);
        let use_arguments = [GenericArg::Type(&u32_argument)];
        let use_ty = path_type_with_args(ids.owner, ids.symbol, 1, 5, 15, &use_arguments);

        let outer_parameter_ty = path_type(ids.second_alias_owner, ids.symbol, 4, 25, 26);
        let bool_ty = path_type(ids.second_alias_owner, ids.symbol, 5, 27, 31);
        let tuple_elements = [outer_parameter_ty, bool_ty];
        let tuple_argument = Ty {
            hir_id: hir_id(ids.second_alias_owner, 6),
            kind: TyKind::Tuple(&tuple_elements),
            span: span(24, 32),
        };
        let inner_arguments = [GenericArg::Type(&tuple_argument)];
        let outer_target = path_type_with_args(
            ids.second_alias_owner,
            ids.symbol,
            3,
            20,
            34,
            &inner_arguments,
        );
        let inner_target = path_type(ids.alias_owner, ids.symbol, 7, 40, 41);

        let inner_generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let inner_generic_params = [inner_generic_param];
        let outer_generic_param = type_generic_param(ids.second_alias_type_param, ids.symbol);
        let outer_generic_params = [outer_generic_param];

        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, outer_def));
        insert_path(&mut paths, &u32_argument, Res::PrimTy(PrimTy::U32));
        insert_path(
            &mut paths,
            &outer_target,
            Res::Def(DefKind::Alias, inner_def),
        );
        insert_path(
            &mut paths,
            &outer_parameter_ty,
            Res::Def(DefKind::TyParam, outer_param_def),
        );
        insert_path(&mut paths, &bool_ty, Res::PrimTy(PrimTy::Bool));
        insert_path(
            &mut paths,
            &inner_target,
            Res::Def(DefKind::TyParam, inner_param_def),
        );

        let canonical = HashMap::new();
        let definitions = HashMap::from([
            (inner_def, BoundaryDefinition::ModuleAlias),
            (outer_def, BoundaryDefinition::ModuleAlias),
        ]);
        let aliases = HashMap::from([
            (
                inner_def,
                BoundaryAlias {
                    owner: ids.alias_owner,
                    ty: &inner_target,
                    generic_params: &inner_generic_params,
                },
            ),
            (
                outer_def,
                BoundaryAlias {
                    owner: ids.second_alias_owner,
                    ty: &outer_target,
                    generic_params: &outer_generic_params,
                },
            ),
        ]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::Tuple(vec![
                BoundaryType::U32,
                BoundaryType::Bool,
            ]))
        );
    }

    #[test]
    fn malformed_and_unresolved_generic_alias_arguments_defer() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let param_def = ids.alias_type_param.to_def_id();
        let target_ty = path_type(ids.alias_owner, ids.symbol, 20, 40, 41);
        let generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let generic_params = [generic_param];

        let no_arguments = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let u64_argument = path_type(ids.owner, ids.symbol, 3, 20, 23);
        let unresolved_argument = path_type(ids.owner, ids.symbol, 4, 30, 37);
        let one_type_argument = [GenericArg::Type(&u64_argument)];
        let wrong_kind_argument = [GenericArg::Const(ConstArg::Bool(true))];
        let too_many_arguments = [
            GenericArg::Type(&u64_argument),
            GenericArg::Type(&u64_argument),
        ];
        let wrong_kind =
            path_type_with_args(ids.owner, ids.symbol, 2, 12, 18, &wrong_kind_argument);
        let too_many = path_type_with_args(ids.owner, ids.symbol, 5, 25, 39, &too_many_arguments);
        let unresolved_argument = [GenericArg::Type(&unresolved_argument)];
        let unresolved =
            path_type_with_args(ids.owner, ids.symbol, 6, 42, 51, &unresolved_argument);
        let valid = path_type_with_args(ids.owner, ids.symbol, 7, 52, 60, &one_type_argument);

        let mut paths = HashMap::new();
        for ty in [&no_arguments, &wrong_kind, &too_many, &unresolved, &valid] {
            insert_path(&mut paths, ty, Res::Def(DefKind::Alias, alias_def));
        }
        insert_path(&mut paths, &u64_argument, Res::PrimTy(PrimTy::U64));
        insert_path(
            &mut paths,
            &target_ty,
            Res::Def(DefKind::TyParam, param_def),
        );
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &generic_params,
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        for ty in [&no_arguments, &wrong_kind, &too_many, &unresolved] {
            assert_eq!(
                boundary_type(&context, ids.owner, ty, Position::Param),
                BoundaryTypeOutcome::Deferred
            );
        }
    }

    #[test]
    fn recursive_generic_alias_application_defers() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let param_def = ids.alias_type_param.to_def_id();
        let u64_argument = path_type(ids.owner, ids.symbol, 2, 10, 13);
        let use_arguments = [GenericArg::Type(&u64_argument)];
        let use_ty = path_type_with_args(ids.owner, ids.symbol, 1, 5, 15, &use_arguments);
        let target_argument = path_type(ids.alias_owner, ids.symbol, 4, 25, 26);
        let target_arguments = [GenericArg::Type(&target_argument)];
        let target_ty =
            path_type_with_args(ids.alias_owner, ids.symbol, 3, 20, 30, &target_arguments);
        let generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let generic_params = [generic_param];

        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(&mut paths, &u64_argument, Res::PrimTy(PrimTy::U64));
        insert_path(&mut paths, &target_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(
            &mut paths,
            &target_argument,
            Res::Def(DefKind::TyParam, param_def),
        );
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &generic_params,
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );
    }

    #[test]
    fn alias_cycles_generic_aliases_and_unknown_aliases_defer() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let next_alias_def = ids.second_alias_owner.def_id.to_def_id();
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let cycle_to_next = path_type(ids.alias_owner, ids.symbol, 2, 20, 29);
        let cycle_to_first = path_type(ids.second_alias_owner, ids.symbol, 3, 30, 39);
        let generic_target = path_type(ids.alias_owner, ids.symbol, 4, 40, 45);
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        insert_path(
            &mut paths,
            &cycle_to_next,
            Res::Def(DefKind::Alias, next_alias_def),
        );
        insert_path(
            &mut paths,
            &cycle_to_first,
            Res::Def(DefKind::Alias, alias_def),
        );
        insert_path(&mut paths, &generic_target, Res::PrimTy(PrimTy::USize));
        let canonical = HashMap::new();
        let definitions = HashMap::from([
            (alias_def, BoundaryDefinition::ModuleAlias),
            (next_alias_def, BoundaryDefinition::ModuleAlias),
        ]);
        let cyclic_aliases = HashMap::from([
            (
                alias_def,
                BoundaryAlias {
                    owner: ids.alias_owner,
                    ty: &cycle_to_next,
                    generic_params: &[],
                },
            ),
            (
                next_alias_def,
                BoundaryAlias {
                    owner: ids.second_alias_owner,
                    ty: &cycle_to_first,
                    generic_params: &[],
                },
            ),
        ]);
        let context = make_context_with_aliases(
            &paths,
            &canonical,
            &definitions,
            &cyclic_aliases,
            None,
            None,
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );

        let generic_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let generic_params = [generic_param];
        let generic_aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &generic_target,
                generic_params: &generic_params,
            },
        )]);
        let generic_context = make_context_with_aliases(
            &paths,
            &canonical,
            &definitions,
            &generic_aliases,
            None,
            None,
        );
        assert_eq!(
            boundary_type(&generic_context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );

        let loop_def = next_alias_def;
        let loop_param_def = ids.second_alias_type_param.to_def_id();
        let u32_ty = path_type(ids.owner, ids.symbol, 100, 100, 103);
        let loop_param_ty = path_type(ids.second_alias_owner, ids.symbol, 101, 104, 105);
        let recursive_loop_args = [GenericArg::Type(&loop_param_ty)];
        let recursive_loop_ty = path_type_with_args(
            ids.second_alias_owner,
            ids.symbol,
            102,
            106,
            113,
            &recursive_loop_args,
        );
        let identity_target_ty = path_type(ids.alias_owner, ids.symbol, 103, 114, 115);
        let identity_args = [GenericArg::Type(&recursive_loop_ty)];
        let identity_application_ty = path_type_with_args(
            ids.second_alias_owner,
            ids.symbol,
            104,
            116,
            126,
            &identity_args,
        );
        let recursive_loop_use_args = [GenericArg::Type(&u32_ty)];
        let recursive_loop_use = path_type_with_args(
            ids.owner,
            ids.symbol,
            105,
            127,
            137,
            &recursive_loop_use_args,
        );
        let identity_param = type_generic_param(ids.alias_type_param, ids.symbol);
        let loop_param = type_generic_param(ids.second_alias_type_param, ids.symbol);
        let identity_params = [identity_param];
        let loop_params = [loop_param];
        let mut recursive_paths = HashMap::new();
        insert_path(&mut recursive_paths, &u32_ty, Res::PrimTy(PrimTy::U32));
        insert_path(
            &mut recursive_paths,
            &loop_param_ty,
            Res::Def(DefKind::TyParam, loop_param_def),
        );
        insert_path(
            &mut recursive_paths,
            &recursive_loop_ty,
            Res::Def(DefKind::Alias, loop_def),
        );
        insert_path(
            &mut recursive_paths,
            &identity_target_ty,
            Res::Def(DefKind::TyParam, ids.alias_type_param.to_def_id()),
        );
        insert_path(
            &mut recursive_paths,
            &identity_application_ty,
            Res::Def(DefKind::Alias, alias_def),
        );
        insert_path(
            &mut recursive_paths,
            &recursive_loop_use,
            Res::Def(DefKind::Alias, loop_def),
        );
        let recursive_aliases = HashMap::from([
            (
                alias_def,
                BoundaryAlias {
                    owner: ids.alias_owner,
                    ty: &identity_target_ty,
                    generic_params: &identity_params,
                },
            ),
            (
                loop_def,
                BoundaryAlias {
                    owner: ids.second_alias_owner,
                    ty: &identity_application_ty,
                    generic_params: &loop_params,
                },
            ),
        ]);
        let recursive_context = make_context_with_aliases(
            &recursive_paths,
            &canonical,
            &definitions,
            &recursive_aliases,
            None,
            None,
        );
        assert_eq!(
            boundary_type(
                &recursive_context,
                ids.owner,
                &recursive_loop_use,
                Position::Param,
            ),
            BoundaryTypeOutcome::Deferred,
            "a generic alias cycle mediated through a substituted type parameter defers"
        );

        let unknown_def = def_id(&ids, 500);
        let unknown_ty = path_type(ids.owner, ids.symbol, 5, 50, 56);
        let mut unknown_paths = paths.clone();
        insert_path(
            &mut unknown_paths,
            &unknown_ty,
            Res::Def(DefKind::Alias, unknown_def),
        );
        let unknown_definitions = HashMap::from([(unknown_def, BoundaryDefinition::ModuleAlias)]);
        let unknown_context = make_context_with_aliases(
            &unknown_paths,
            &canonical,
            &unknown_definitions,
            empty_aliases(),
            None,
            None,
        );
        assert_eq!(
            boundary_type(&unknown_context, ids.owner, &unknown_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );
    }

    #[test]
    fn alias_expansion_preserves_unit_position_rules() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let target_ty = Ty {
            hir_id: hir_id(ids.alias_owner, 2),
            kind: TyKind::Tuple(&[]),
            span: span(40, 42),
        };
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &[],
            },
        )]);
        let context =
            make_context_with_aliases(&paths, &canonical, &definitions, &aliases, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Result),
            BoundaryTypeOutcome::Mapped(BoundaryType::Unit)
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Incompatible(Incompatible {
                span: use_ty.span,
                reason: IncompatibleReason::UnitOutsideResult,
            })
        );
    }

    #[test]
    fn module_alias_does_not_capture_the_using_resources_self_type() {
        let ids = test_ids();
        let alias_def = ids.alias_owner.def_id.to_def_id();
        let resource_id = def_id(&ids, 501);
        let use_ty = path_type(ids.owner, ids.symbol, 1, 5, 10);
        let target_hir_id = hir_id(ids.alias_owner, 2);
        let target_ty = Ty {
            hir_id: target_hir_id,
            kind: TyKind::Path(Path {
                hir_id: target_hir_id,
                root: PathRoot::SelfType,
                segments: &[],
                span: span(40, 44),
            }),
            span: span(40, 44),
        };
        let mut paths = HashMap::new();
        insert_path(&mut paths, &use_ty, Res::Def(DefKind::Alias, alias_def));
        let canonical = HashMap::new();
        let definitions = HashMap::from([(alias_def, BoundaryDefinition::ModuleAlias)]);
        let aliases = HashMap::from([(
            alias_def,
            BoundaryAlias {
                owner: ids.alias_owner,
                ty: &target_ty,
                generic_params: &[],
            },
        )]);
        let context = make_context_with_aliases(
            &paths,
            &canonical,
            &definitions,
            &aliases,
            None,
            Some(resource_id),
        );

        assert_eq!(
            boundary_type(&context, ids.owner, &use_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );
    }

    #[test]
    fn canonical_builtin_identities_map_structural_types_and_positions() {
        let ids = test_ids();
        let option_id = def_id(&ids, 100);
        let result_id = def_id(&ids, 101);
        let slice_id = def_id(&ids, 102);
        let str_id = def_id(&ids, 103);
        let resource_id = def_id(&ids, 104);

        let mut canonical = HashMap::from([
            (option_id, CanonicalBoundaryType::Option),
            (result_id, CanonicalBoundaryType::Result),
            (slice_id, CanonicalBoundaryType::Slice),
            (str_id, CanonicalBoundaryType::Str),
        ]);
        let mut definitions = HashMap::from([(resource_id, BoundaryDefinition::Resource)]);
        let mut paths = HashMap::new();
        let u32_ty = path_type(ids.owner, ids.symbol, 1, 20, 23);
        insert_path(&mut paths, &u32_ty, Res::PrimTy(PrimTy::U32));
        let unit_ty = Ty {
            hir_id: hir_id(ids.owner, 2),
            kind: TyKind::Tuple(&[]),
            span: span(24, 26),
        };
        let option_args = [GenericArg::Type(&unit_ty)];
        let option_ty = builtin_path_type(ids.owner, ids.symbol, 3, 10, 18, &option_args);
        insert_path(&mut paths, &option_ty, Res::Def(DefKind::Struct, option_id));

        let result_args = [GenericArg::Type(&unit_ty), GenericArg::Type(&u32_ty)];
        let result_ty = builtin_path_type(ids.owner, ids.symbol, 4, 30, 45, &result_args);
        insert_path(&mut paths, &result_ty, Res::Def(DefKind::Enum, result_id));

        let slice_args = [GenericArg::Type(&u32_ty)];
        let slice_ty = builtin_path_type(ids.owner, ids.symbol, 5, 50, 62, &slice_args);
        insert_path(&mut paths, &slice_ty, Res::Def(DefKind::Struct, slice_id));

        let str_ty = path_type(ids.owner, ids.symbol, 6, 70, 73);
        insert_path(&mut paths, &str_ty, Res::Def(DefKind::Struct, str_id));

        let context = make_context(&paths, &canonical, &definitions, None, None);
        let option = boundary_type(&context, ids.owner, &option_ty, Position::Param);
        assert_eq!(
            option,
            BoundaryTypeOutcome::Incompatible(super::Incompatible {
                span: option_ty.span,
                reason: IncompatibleReason::OptionUnit,
            })
        );
        let result = boundary_type(&context, ids.owner, &result_ty, Position::Result);
        assert_eq!(
            result,
            BoundaryTypeOutcome::Mapped(BoundaryType::Result {
                ok: None,
                err: Some(Box::new(BoundaryType::U32)),
            })
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &slice_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::List(Box::new(BoundaryType::U32)))
        );
        assert!(matches!(
            boundary_type(&context, ids.owner, &slice_ty, Position::Nested),
            BoundaryTypeOutcome::Incompatible(super::Incompatible {
                reason: IncompatibleReason::SliceOutsideParameter,
                ..
            })
        ));
        assert_eq!(
            boundary_type(&context, ids.owner, &str_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::String)
        );
        assert!(matches!(
            boundary_type(&context, ids.owner, &str_ty, Position::Result),
            BoundaryTypeOutcome::Incompatible(super::Incompatible {
                reason: IncompatibleReason::StrOutsideParameter,
                ..
            })
        ));

        let resource_ty = path_type(ids.owner, ids.symbol, 7, 80, 81);
        insert_path(
            &mut paths,
            &resource_ty,
            Res::Def(DefKind::Resource, resource_id),
        );
        let mut resource_context =
            make_context(&paths, &canonical, &definitions, None, Some(resource_id));
        assert_eq!(
            boundary_type(&resource_context, ids.owner, &resource_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::Own(resource_id))
        );

        let self_hir_id = hir_id(ids.owner, 11);
        let self_ty = Ty {
            hir_id: self_hir_id,
            kind: TyKind::Path(Path {
                hir_id: self_hir_id,
                root: PathRoot::SelfType,
                segments: &[],
                span: span(77, 81),
            }),
            span: span(77, 81),
        };
        assert_eq!(
            boundary_type(&resource_context, ids.owner, &self_ty, Position::Result),
            BoundaryTypeOutcome::Mapped(BoundaryType::Own(resource_id))
        );
        let borrowed_self = Ty {
            hir_id: hir_id(ids.owner, 12),
            kind: TyKind::Borrow(Mutability::Mut, &self_ty),
            span: span(76, 81),
        };
        assert_eq!(
            boundary_type(
                &resource_context,
                ids.owner,
                &borrowed_self,
                Position::Param
            ),
            BoundaryTypeOutcome::Mapped(BoundaryType::Borrow(resource_id))
        );
        let no_resource_context = make_context(&paths, &canonical, &definitions, None, None);
        assert_eq!(
            boundary_type(&no_resource_context, ids.owner, &self_ty, Position::Result),
            BoundaryTypeOutcome::Deferred
        );
        let self_qualified =
            builtin_path_type_with_root(ids.owner, ids.symbol, 13, 77, 85, &[], PathRoot::SelfType);
        assert_eq!(
            boundary_type(
                &resource_context,
                ids.owner,
                &self_qualified,
                Position::Result
            ),
            BoundaryTypeOutcome::Deferred,
            "associated paths rooted at `Self` must not map to the resource itself"
        );

        let borrowed = Ty {
            hir_id: hir_id(ids.owner, 8),
            kind: TyKind::Borrow(Mutability::Not, &resource_ty),
            span: span(79, 83),
        };
        assert_eq!(
            boundary_type(&resource_context, ids.owner, &borrowed, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::Borrow(resource_id))
        );
        assert!(matches!(
            boundary_type(&resource_context, ids.owner, &borrowed, Position::Nested),
            BoundaryTypeOutcome::Incompatible(super::Incompatible {
                reason: IncompatibleReason::BorrowOutsideParameter,
                ..
            })
        ));

        let (next_canonical, next_definitions, next_paths) =
            (&mut canonical, &mut definitions, &mut paths);
        next_canonical.insert(def_id(&ids, 105), CanonicalBoundaryType::Vec);
        next_definitions.insert(def_id(&ids, 106), BoundaryDefinition::InterfaceType);
        let vec_id = def_id(&ids, 105);
        let record_id = def_id(&ids, 106);
        let record_ty = path_type(ids.owner, ids.symbol, 9, 90, 96);
        insert_path(next_paths, &record_ty, Res::Def(DefKind::Struct, record_id));
        let vec_args = [GenericArg::Type(&record_ty)];
        let vec_ty = builtin_path_type(ids.owner, ids.symbol, 10, 88, 98, &vec_args);
        insert_path(next_paths, &vec_ty, Res::Def(DefKind::Struct, vec_id));
        let mut containment = HashMap::from([(record_id, ResourceContainment::Yes)]);
        resource_context = make_context(
            next_paths,
            next_canonical,
            next_definitions,
            Some(&containment),
            Some(resource_id),
        );
        assert_eq!(
            boundary_type(&resource_context, ids.owner, &vec_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::List(Box::new(BoundaryType::Named(
                record_id
            ))))
        );
        containment.insert(record_id, ResourceContainment::No);
    }

    #[test]
    fn lang_item_option_and_result_use_semantic_identity() {
        let ids = test_ids();
        let u32_ty = path_type(ids.owner, ids.symbol, 1, 10, 13);
        let unit_ty = Ty {
            hir_id: hir_id(ids.owner, 2),
            kind: TyKind::Tuple(&[]),
            span: span(20, 22),
        };
        let option_args = [GenericArg::Type(&u32_ty)];
        let option_ty = builtin_path_type_with_root(
            ids.owner,
            ids.symbol,
            3,
            5,
            15,
            &option_args,
            PathRoot::Lang(LangItem::Option),
        );
        let result_args = [GenericArg::Type(&unit_ty), GenericArg::Type(&u32_ty)];
        let result_ty = builtin_path_type_with_root(
            ids.owner,
            ids.symbol,
            4,
            30,
            45,
            &result_args,
            PathRoot::Lang(LangItem::Result),
        );
        let malformed_args = [GenericArg::Const(ConstArg::Bool(true))];
        let malformed_option_ty = builtin_path_type_with_root(
            ids.owner,
            ids.symbol,
            5,
            50,
            62,
            &malformed_args,
            PathRoot::Lang(LangItem::Option),
        );
        let mut paths = HashMap::new();
        insert_path(&mut paths, &u32_ty, Res::PrimTy(PrimTy::U32));
        let empty_canonical = HashMap::new();
        let empty_definitions = HashMap::new();
        let context = make_context(&paths, &empty_canonical, &empty_definitions, None, None);

        assert_eq!(
            boundary_type(&context, ids.owner, &option_ty, Position::Param),
            BoundaryTypeOutcome::Mapped(BoundaryType::Option(Box::new(BoundaryType::U32)))
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &result_ty, Position::Result),
            BoundaryTypeOutcome::Mapped(BoundaryType::Result {
                ok: None,
                err: Some(Box::new(BoundaryType::U32)),
            })
        );
        assert_eq!(
            boundary_type(&context, ids.owner, &malformed_option_ty, Position::Param),
            BoundaryTypeOutcome::Deferred
        );
    }

    #[test]
    fn unresolved_and_error_types_do_not_cascade() {
        let ids = test_ids();
        let unresolved = path_type(ids.owner, ids.symbol, 1, 10, 15);
        let paths = HashMap::new();
        let canonical = HashMap::new();
        let definitions = HashMap::new();
        let context = make_context(&paths, &canonical, &definitions, None, None);
        assert_eq!(
            boundary_type(&context, ids.owner, &unresolved, Position::Param),
            BoundaryTypeOutcome::Deferred
        );

        let error_ty = Ty {
            hir_id: hir_id(ids.owner, 2),
            kind: TyKind::Err,
            span: span(20, 21),
        };
        assert_eq!(
            boundary_type(&context, ids.owner, &error_ty, Position::Param),
            BoundaryTypeOutcome::Suppressed
        );

        let error_path = Ty {
            hir_id: hir_id(ids.owner, 3),
            kind: TyKind::Path(Path {
                hir_id: hir_id(ids.owner, 3),
                root: PathRoot::Plain,
                segments: &[],
                span: span(30, 31),
            }),
            span: span(30, 31),
        };
        let mut paths = HashMap::new();
        insert_path(&mut paths, &error_path, Res::Err);
        let context = make_context(&paths, &canonical, &definitions, None, None);
        assert_eq!(
            boundary_type(&context, ids.owner, &error_path, Position::Nested),
            BoundaryTypeOutcome::Suppressed
        );

        let module_alias_id = def_id(&ids, 120);
        let mut paths = HashMap::new();
        let alias_ty = path_type(ids.owner, ids.symbol, 4, 40, 44);
        insert_path(
            &mut paths,
            &alias_ty,
            Res::Def(DefKind::Alias, module_alias_id),
        );
        let definitions = HashMap::from([(module_alias_id, BoundaryDefinition::ModuleAlias)]);
        let context = make_context(&paths, &canonical, &definitions, None, None);
        assert_eq!(
            boundary_type(&context, ids.owner, &alias_ty, Position::Nested),
            BoundaryTypeOutcome::Deferred
        );
    }

    fn builtin_path_type<'hir>(
        owner: OwnerId,
        symbol: sai_hir::Symbol,
        id: u32,
        start: u32,
        end: u32,
        arguments: &'hir [GenericArg<'hir>],
    ) -> Ty<'hir> {
        builtin_path_type_with_root(owner, symbol, id, start, end, arguments, PathRoot::Plain)
    }

    fn builtin_path_type_with_root<'hir>(
        owner: OwnerId,
        symbol: sai_hir::Symbol,
        id: u32,
        start: u32,
        end: u32,
        arguments: &'hir [GenericArg<'hir>],
        root: PathRoot,
    ) -> Ty<'hir> {
        let hir_id = hir_id(owner, id);
        let name = Name {
            sym: symbol,
            span: span(start, end),
        };
        let generic_args = Box::leak(Box::new(GenericArgs {
            args: arguments,
            span: span(start + 1, end - 1),
        }));
        let segment = Box::leak(Box::new(PathSegment {
            hir_id,
            name,
            args: Some(generic_args),
        }));
        Ty {
            hir_id,
            kind: TyKind::Path(Path {
                hir_id,
                root,
                segments: std::slice::from_ref(segment),
                span: span(start, end),
            }),
            span: span(start, end),
        }
    }
}
