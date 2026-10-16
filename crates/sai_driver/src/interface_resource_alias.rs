//! Transparent SAI alias expansion for interface associated-type resource checks.
//!
//! The caller supplies the original path resolutions for the binding and all
//! top-level and interface-member aliases in the active source closure.
//! Keeping those resolutions keyed by the full `HirId` preserves the owner of
//! every path while following an alias chain.

use sai_hir::types::{GenericArg, GenericParam, GenericParamKind, Path, PathRoot, Ty, TyKind};
use sai_hir::{DefId, HirId, OwnerId};
use sai_resolve::package_scopes::{DefKind, PathRes, Res};
use std::collections::{HashMap, HashSet};

/// A source alias definition in the form needed by resource binding checks.
///
/// The root adapter can build this from top-level or interface-member alias
/// HIR.
#[derive(Clone, Copy, Debug)]
pub(super) struct LocalTypeAlias<'hir> {
    pub(super) owner: OwnerId,
    pub(super) params: &'hir [GenericParam<'hir>],
    pub(super) target: &'hir Ty<'hir>,
}

/// Inputs required to expand a binding type through local aliases.
pub(super) struct ResourceAliasContext<'a, 'hir> {
    /// Combined original path results for the binding and every alias owner.
    pub(super) path_resolutions: &'a HashMap<HirId, PathRes>,
    /// Successfully lowered local top-level and interface-member aliases,
    /// keyed by their `DefId`.
    pub(super) aliases: &'a HashMap<DefId, LocalTypeAlias<'hir>>,
    /// Resolved WIT aliases cannot expand to a local source resource.
    pub(super) wit_aliases: &'a HashSet<DefId>,
    /// Resource definitions declared at module scope in the owning package.
    pub(super) local_resources: &'a HashSet<DefId>,
}

/// Classification of an associated type RHS after transparent alias expansion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResourceAliasTarget {
    /// The type resolves to this local module-level resource.
    Resource(DefId),
    /// The type is known to have a non-resource head.
    NonResource,
    /// Resolution or expansion was incomplete, so the caller should defer.
    Suppressed,
}

/// Resolve an associated-type RHS through local transparent aliases.
///
/// Only fully resolved paths are classified. Alias cycles, unknown aliases,
/// malformed or unsupported generic arguments, and missing path resolutions
/// are suppressed so the caller can leave them to their existing diagnostics.
/// A fully resolved free type parameter is a known non-resource because
/// resource bindings require a concrete type.
pub(super) fn resolve_resource_alias_target<'hir>(
    context: &ResourceAliasContext<'_, 'hir>,
    owner: OwnerId,
    ty: &'hir Ty<'hir>,
) -> ResourceAliasTarget {
    if ty.hir_id.owner != owner {
        return ResourceAliasTarget::Suppressed;
    }

    expand_type(
        context,
        ty,
        &mut Vec::new(),
        0,
        &mut Vec::new(),
        &mut HashSet::new(),
        &mut 0,
    )
}

#[derive(Clone)]
struct BoundType<'hir> {
    ty: &'hir Ty<'hir>,
    /// Only the caller's environment is visible in a type argument. In
    /// particular, parameters declared by the alias being expanded cannot
    /// accidentally capture their own arguments.
    env_depth: usize,
    /// Substituted type arguments are expanded in the alias context where
    /// those arguments were written, not in the body that consumes them.
    active_aliases: Vec<DefId>,
}

#[derive(Clone)]
struct SubstitutionFrame<'hir> {
    id: usize,
    bindings: HashMap<DefId, BoundType<'hir>>,
}

fn expand_type<'hir>(
    context: &ResourceAliasContext<'_, 'hir>,
    ty: &'hir Ty<'hir>,
    substitutions: &mut Vec<SubstitutionFrame<'hir>>,
    env_depth: usize,
    active_aliases: &mut Vec<DefId>,
    active_params: &mut HashSet<(DefId, usize)>,
    next_frame_id: &mut usize,
) -> ResourceAliasTarget {
    match ty.kind {
        TyKind::Path(path) => {
            if path.hir_id.owner != ty.hir_id.owner {
                return ResourceAliasTarget::Suppressed;
            }

            // `Self` paths are context-dependent and cannot establish that
            // an associated binding names a module-level resource.
            if matches!(path.root, PathRoot::SelfValue | PathRoot::SelfType) {
                return ResourceAliasTarget::Suppressed;
            }

            let Some(resolution) = context.path_resolutions.get(&path.hir_id) else {
                return ResourceAliasTarget::Suppressed;
            };
            if resolution.resolved_segments != resolution.segment_count
                || u32::try_from(path.segments.len()).ok() != Some(resolution.segment_count)
            {
                return ResourceAliasTarget::Suppressed;
            }

            let Some((args, has_unsupported_prefix_args)) = path_type_args(path) else {
                return ResourceAliasTarget::Suppressed;
            };
            if has_unsupported_prefix_args {
                return ResourceAliasTarget::Suppressed;
            }

            match resolution.base {
                Res::Def(DefKind::TyParam, param) => {
                    if !args.is_empty() {
                        return ResourceAliasTarget::Suppressed;
                    }
                    let Some((frame_index, bound_ty)) =
                        substitutions[..env_depth].iter().rev().find_map(|frame| {
                            frame
                                .bindings
                                .get(&param)
                                .map(|bound| (frame.id, bound.clone()))
                        })
                    else {
                        // A generic type parameter has no concrete head
                        // until its caller supplies a type argument. Resource
                        // bindings must name a concrete, nongeneric module
                        // resource, so a fully resolved free parameter is
                        // known not to satisfy this contract.
                        return ResourceAliasTarget::NonResource;
                    };
                    let active_param = (param, frame_index);
                    if !active_params.insert(active_param) {
                        return ResourceAliasTarget::Suppressed;
                    }
                    let mut substituted_aliases = bound_ty.active_aliases;
                    let mut caller_substitutions = substitutions[..bound_ty.env_depth].to_vec();
                    let result = expand_type(
                        context,
                        bound_ty.ty,
                        &mut caller_substitutions,
                        bound_ty.env_depth,
                        &mut substituted_aliases,
                        active_params,
                        next_frame_id,
                    );
                    active_params.remove(&active_param);
                    return result;
                }
                Res::Def(DefKind::Alias, alias) => {
                    if active_aliases.contains(&alias) {
                        return ResourceAliasTarget::Suppressed;
                    }
                    let Some(alias_def) = context.aliases.get(&alias) else {
                        // A resolved WIT alias is known to be foreign, while
                        // any other unavailable alias may still be an
                        // incomplete source declaration and must defer.
                        return if context.wit_aliases.contains(&alias) {
                            ResourceAliasTarget::NonResource
                        } else {
                            ResourceAliasTarget::Suppressed
                        };
                    };
                    if alias_def.owner.def_id.to_def_id() != alias
                        || alias_def.target.hir_id.owner != alias_def.owner
                    {
                        return ResourceAliasTarget::Suppressed;
                    }

                    let Some(alias_args) = match_alias_args(alias_def.params, args, path) else {
                        return ResourceAliasTarget::Suppressed;
                    };
                    let frame_id = *next_frame_id;
                    let Some(next_id) = frame_id.checked_add(1) else {
                        return ResourceAliasTarget::Suppressed;
                    };
                    *next_frame_id = next_id;
                    let mut bindings = HashMap::new();
                    for (param, arg_ty) in alias_def.params.iter().zip(alias_args) {
                        match (param.kind, arg_ty) {
                            (GenericParamKind::Type { .. }, Some(arg_ty)) => {
                                if arg_ty.hir_id.owner != path.hir_id.owner {
                                    return ResourceAliasTarget::Suppressed;
                                }
                                bindings.insert(
                                    param.def_id.to_def_id(),
                                    BoundType {
                                        ty: arg_ty,
                                        env_depth,
                                        active_aliases: active_aliases.clone(),
                                    },
                                );
                            }
                            (GenericParamKind::Const { .. }, None) => {}
                            _ => return ResourceAliasTarget::Suppressed,
                        }
                    }
                    substitutions.push(SubstitutionFrame {
                        id: frame_id,
                        bindings,
                    });
                    active_aliases.push(alias);
                    let result = expand_type(
                        context,
                        alias_def.target,
                        substitutions,
                        env_depth + 1,
                        active_aliases,
                        active_params,
                        next_frame_id,
                    );
                    active_aliases.pop();
                    substitutions.pop();
                    return result;
                }
                Res::Def(DefKind::Resource, resource) => {
                    if !args.is_empty() {
                        return ResourceAliasTarget::NonResource;
                    }
                    return if context.local_resources.contains(&resource) {
                        ResourceAliasTarget::Resource(resource)
                    } else {
                        ResourceAliasTarget::NonResource
                    };
                }
                Res::Def(_, _) | Res::PrimTy(_) => {
                    return ResourceAliasTarget::NonResource;
                }
                Res::Err | Res::TestOnly(_) | Res::Local(_) => {
                    return ResourceAliasTarget::Suppressed;
                }
            }
        }
        TyKind::Infer | TyKind::Err => ResourceAliasTarget::Suppressed,
        TyKind::Borrow(_, _) | TyKind::Tuple(_) | TyKind::FnPtr { .. } => {
            ResourceAliasTarget::NonResource
        }
    }
}

/// Returns final-segment type arguments and whether a prefix segment carries
/// arguments. Prefix arguments do not participate in resolving the final
/// type head, so they must not be silently ignored.
fn path_type_args<'hir>(path: Path<'hir>) -> Option<(&'hir [GenericArg<'hir>], bool)> {
    let (last, prefix) = path.segments.split_last()?;
    let args = last.args.map_or(&[][..], |args| args.args);
    let has_unsupported_prefix_args = prefix
        .iter()
        .any(|segment| segment.args.is_some_and(|args| !args.args.is_empty()));
    Some((args, has_unsupported_prefix_args))
}

/// Validate an alias application and retain type arguments in declaration
/// order. Const arguments are shape-checked but need no substitution because
/// SAI type aliases select their nominal type head through resolved paths.
fn match_alias_args<'hir>(
    params: &'hir [GenericParam<'hir>],
    args: &'hir [GenericArg<'hir>],
    path: Path<'hir>,
) -> Option<Vec<Option<&'hir Ty<'hir>>>> {
    if params.len() != args.len() {
        return if params.is_empty() && args.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    }

    // A path without explicit arguments is valid only for a nongeneric alias.
    let final_segment_has_args = path
        .segments
        .last()
        .is_some_and(|segment| segment.args.is_some());
    if !params.is_empty() && !final_segment_has_args {
        return None;
    }

    params
        .iter()
        .zip(args)
        .map(|(param, arg)| match (param.kind, arg) {
            (GenericParamKind::Type { .. }, GenericArg::Type(ty)) => Some(Some(*ty)),
            (GenericParamKind::Const { .. }, GenericArg::Const(_)) => Some(None),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::types::{GenericArgs, Name, PathSegment};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, LocalDefId,
        OwnerId, Span, SpanCtxt,
    };
    use sai_intern::{Idx, Interner, ItemLocalId, PackageInterner};
    use sai_resolve::prelude::PrimTy;
    use sai_vfs::{FileId, TextRange, TextSize};

    struct TestIds {
        packages: PackageInterner,
        symbols: Interner,
        paths: DefPathInterner,
        package: sai_hir::PackageId,
    }

    impl TestIds {
        fn new() -> Self {
            let packages = PackageInterner::new();
            let package = packages.intern("example:alias-test#1.0.0").unwrap();
            let symbols = Interner::new();
            let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
            Self {
                packages,
                symbols,
                paths,
                package,
            }
        }

        fn definition(&self, name: &str) -> LocalDefId {
            self.intern_path(vec![DefPathData::TypeNs(
                self.symbols.intern(name).unwrap(),
            )])
        }

        fn type_param(&self, alias: LocalDefId, name: &str) -> LocalDefId {
            let parent = self.paths.resolve(alias).unwrap().unwrap();
            let mut data = parent.data().to_vec();
            data.push(DisambiguatedDefPathData {
                data: DefPathData::TypeParam(self.symbols.intern(name).unwrap()),
                disambiguator: 0,
            });
            self.paths
                .intern(DefPath::new(self.package, data, &self.packages, &self.symbols).unwrap())
                .unwrap()
        }

        fn intern_path(&self, data: Vec<DefPathData>) -> LocalDefId {
            let data = data
                .into_iter()
                .map(|data| DisambiguatedDefPathData {
                    data,
                    disambiguator: 0,
                })
                .collect();
            self.paths
                .intern(DefPath::new(self.package, data, &self.packages, &self.symbols).unwrap())
                .unwrap()
        }

        fn owner(local: LocalDefId) -> OwnerId {
            OwnerId { def_id: local }
        }

        fn hir_id(owner: OwnerId, local_id: u32) -> HirId {
            HirId {
                owner,
                local_id: ItemLocalId::from_raw(local_id),
            }
        }

        fn span(&self) -> Span {
            Span {
                file: FileId::new(1),
                range: TextRange::empty(TextSize::new(0)),
                ctxt: SpanCtxt::Root,
            }
        }

        fn name(&self) -> Name {
            Name {
                sym: self.symbols.intern("T").unwrap(),
                span: self.span(),
            }
        }

        fn type_param_node(&self, alias: LocalDefId, name: &str) -> GenericParam<'static> {
            let def_id = self.type_param(alias, name);
            GenericParam {
                def_id,
                name: self.name(),
                kind: GenericParamKind::Type { bounds: &[] },
            }
        }

        fn path_ty(
            &self,
            owner: OwnerId,
            local_id: u32,
            resolution: Res,
            args: Option<Vec<GenericArg<'static>>>,
            resolutions: &mut HashMap<HirId, PathRes>,
        ) -> &'static Ty<'static> {
            let path_hir_id = Self::hir_id(owner, local_id);
            let segment_hir_id = Self::hir_id(owner, local_id + 1);
            let args = args.map(|args| {
                let args: &'static [GenericArg<'static>] = Box::leak(args.into_boxed_slice());
                let args: &'static GenericArgs<'static> = Box::leak(Box::new(GenericArgs {
                    args,
                    span: self.span(),
                }));
                args
            });
            let segment = Box::leak(Box::new(PathSegment {
                hir_id: segment_hir_id,
                name: self.name(),
                args,
            }));
            let segments: &'static [PathSegment<'static>] =
                Box::leak(vec![*segment].into_boxed_slice());
            let span = self.span();
            let path = Path {
                hir_id: path_hir_id,
                root: PathRoot::Plain,
                segments,
                span,
            };
            let ty = Box::leak(Box::new(Ty {
                hir_id: path_hir_id,
                kind: TyKind::Path(path),
                span,
            }));
            resolutions.insert(
                path_hir_id,
                PathRes {
                    base: resolution,
                    resolved_segments: 1,
                    segment_count: 1,
                },
            );
            ty
        }
    }

    #[test]
    fn expands_a_generic_alias_chain_to_a_local_resource() {
        let ids = TestIds::new();
        let binding_owner = TestIds::owner(ids.definition("Binding"));
        let alias_a = ids.definition("AliasA");
        let alias_b = ids.definition("AliasB");
        let resource = ids.definition("LocalResource");
        let alias_a_owner = TestIds::owner(alias_a);
        let alias_b_owner = TestIds::owner(alias_b);
        let param_a = ids.type_param_node(alias_a, "T");
        let param_b = ids.type_param_node(alias_b, "U");

        let mut path_resolutions = HashMap::new();
        let resource_ty = ids.path_ty(
            binding_owner,
            10,
            Res::Def(DefKind::Resource, resource.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let binding_ty = ids.path_ty(
            binding_owner,
            20,
            Res::Def(DefKind::Alias, alias_a.to_def_id()),
            Some(vec![GenericArg::Type(resource_ty)]),
            &mut path_resolutions,
        );
        let alias_a_arg = ids.path_ty(
            alias_a_owner,
            10,
            Res::Def(DefKind::TyParam, param_a.def_id.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let alias_a_target = ids.path_ty(
            alias_a_owner,
            20,
            Res::Def(DefKind::Alias, alias_b.to_def_id()),
            Some(vec![GenericArg::Type(alias_a_arg)]),
            &mut path_resolutions,
        );
        let alias_b_target = ids.path_ty(
            alias_b_owner,
            10,
            Res::Def(DefKind::TyParam, param_b.def_id.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let aliases = HashMap::from([
            (
                alias_a.to_def_id(),
                LocalTypeAlias {
                    owner: alias_a_owner,
                    params: Box::leak(vec![param_a].into_boxed_slice()),
                    target: alias_a_target,
                },
            ),
            (
                alias_b.to_def_id(),
                LocalTypeAlias {
                    owner: alias_b_owner,
                    params: Box::leak(vec![param_b].into_boxed_slice()),
                    target: alias_b_target,
                },
            ),
        ]);
        let local_resources = HashSet::from([resource.to_def_id()]);
        let wit_aliases = HashSet::new();
        let context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };

        assert_eq!(
            resolve_resource_alias_target(&context, binding_owner, binding_ty),
            ResourceAliasTarget::Resource(resource.to_def_id())
        );
    }

    #[test]
    fn classifies_a_primitive_alias_target_as_non_resource() {
        let ids = TestIds::new();
        let binding_owner = TestIds::owner(ids.definition("Binding"));
        let alias = ids.definition("PrimitiveAlias");
        let alias_owner = TestIds::owner(alias);
        let mut path_resolutions = HashMap::new();
        let binding_ty = ids.path_ty(
            binding_owner,
            10,
            Res::Def(DefKind::Alias, alias.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let primitive_ty = ids.path_ty(
            alias_owner,
            10,
            Res::PrimTy(PrimTy::U32),
            None,
            &mut path_resolutions,
        );
        let aliases = HashMap::from([(
            alias.to_def_id(),
            LocalTypeAlias {
                owner: alias_owner,
                params: &[],
                target: primitive_ty,
            },
        )]);
        let local_resources = HashSet::new();
        let wit_aliases = HashSet::new();
        let context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };

        assert_eq!(
            resolve_resource_alias_target(&context, binding_owner, binding_ty),
            ResourceAliasTarget::NonResource
        );
    }

    #[test]
    fn expands_a_nested_application_of_the_same_alias() {
        let ids = TestIds::new();
        let binding_owner = TestIds::owner(ids.definition("Binding"));
        let alias = ids.definition("Identity");
        let alias_owner = TestIds::owner(alias);
        let resource = ids.definition("LocalResource");
        let param = ids.type_param_node(alias, "T");

        let mut path_resolutions = HashMap::new();
        let resource_ty = ids.path_ty(
            binding_owner,
            10,
            Res::Def(DefKind::Resource, resource.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let inner_application = ids.path_ty(
            binding_owner,
            20,
            Res::Def(DefKind::Alias, alias.to_def_id()),
            Some(vec![GenericArg::Type(resource_ty)]),
            &mut path_resolutions,
        );
        let binding_ty = ids.path_ty(
            binding_owner,
            30,
            Res::Def(DefKind::Alias, alias.to_def_id()),
            Some(vec![GenericArg::Type(inner_application)]),
            &mut path_resolutions,
        );
        let alias_target = ids.path_ty(
            alias_owner,
            10,
            Res::Def(DefKind::TyParam, param.def_id.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let aliases = HashMap::from([(
            alias.to_def_id(),
            LocalTypeAlias {
                owner: alias_owner,
                params: Box::leak(vec![param].into_boxed_slice()),
                target: alias_target,
            },
        )]);
        let local_resources = HashSet::from([resource.to_def_id()]);
        let wit_aliases = HashSet::new();
        let context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };

        assert_eq!(
            resolve_resource_alias_target(&context, binding_owner, binding_ty),
            ResourceAliasTarget::Resource(resource.to_def_id())
        );
    }

    #[test]
    fn suppresses_missing_path_results_and_unavailable_aliases() {
        let ids = TestIds::new();
        let binding_owner = TestIds::owner(ids.definition("Binding"));
        let alias = ids.definition("UnavailableAlias");
        let mut path_resolutions = HashMap::new();
        let binding_ty = ids.path_ty(
            binding_owner,
            10,
            Res::Def(DefKind::Alias, alias.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let local_resources = HashSet::new();
        let aliases = HashMap::new();
        let wit_aliases = HashSet::new();
        let context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };
        assert_eq!(
            resolve_resource_alias_target(&context, binding_owner, binding_ty),
            ResourceAliasTarget::Suppressed
        );
        let wit_aliases = HashSet::from([alias.to_def_id()]);
        let wit_alias_context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };
        assert_eq!(
            resolve_resource_alias_target(&wit_alias_context, binding_owner, binding_ty),
            ResourceAliasTarget::NonResource
        );

        let missing_path_resolutions = HashMap::new();
        let missing_path_context = ResourceAliasContext {
            path_resolutions: &missing_path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };
        assert_eq!(
            resolve_resource_alias_target(&missing_path_context, binding_owner, binding_ty),
            ResourceAliasTarget::Suppressed
        );
    }

    #[test]
    fn suppresses_alias_cycles_and_unsupplied_generic_parameters() {
        let ids = TestIds::new();
        let binding_owner = TestIds::owner(ids.definition("Binding"));
        let alias_a = ids.definition("AliasA");
        let alias_b = ids.definition("AliasB");
        let alias_a_owner = TestIds::owner(alias_a);
        let alias_b_owner = TestIds::owner(alias_b);
        let mut path_resolutions = HashMap::new();
        let binding_ty = ids.path_ty(
            binding_owner,
            10,
            Res::Def(DefKind::Alias, alias_a.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let alias_a_target = ids.path_ty(
            alias_a_owner,
            10,
            Res::Def(DefKind::Alias, alias_b.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let alias_b_target = ids.path_ty(
            alias_b_owner,
            10,
            Res::Def(DefKind::Alias, alias_a.to_def_id()),
            None,
            &mut path_resolutions,
        );
        let aliases = HashMap::from([
            (
                alias_a.to_def_id(),
                LocalTypeAlias {
                    owner: alias_a_owner,
                    params: &[],
                    target: alias_a_target,
                },
            ),
            (
                alias_b.to_def_id(),
                LocalTypeAlias {
                    owner: alias_b_owner,
                    params: &[],
                    target: alias_b_target,
                },
            ),
        ]);
        let local_resources = HashSet::new();
        let wit_aliases = HashSet::new();
        let context = ResourceAliasContext {
            path_resolutions: &path_resolutions,
            aliases: &aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };
        assert_eq!(
            resolve_resource_alias_target(&context, binding_owner, binding_ty),
            ResourceAliasTarget::Suppressed
        );

        let generic_alias = ids.definition("GenericAlias");
        let generic_alias_owner = TestIds::owner(generic_alias);
        let generic_param = ids.type_param_node(generic_alias, "T");
        let mut generic_resolutions = HashMap::new();
        let generic_binding_ty = ids.path_ty(
            binding_owner,
            30,
            Res::Def(DefKind::Alias, generic_alias.to_def_id()),
            None,
            &mut generic_resolutions,
        );
        let generic_target = ids.path_ty(
            generic_alias_owner,
            10,
            Res::Def(DefKind::TyParam, generic_param.def_id.to_def_id()),
            None,
            &mut generic_resolutions,
        );
        let generic_aliases = HashMap::from([(
            generic_alias.to_def_id(),
            LocalTypeAlias {
                owner: generic_alias_owner,
                params: Box::leak(vec![generic_param].into_boxed_slice()),
                target: generic_target,
            },
        )]);
        let generic_context = ResourceAliasContext {
            path_resolutions: &generic_resolutions,
            aliases: &generic_aliases,
            wit_aliases: &wit_aliases,
            local_resources: &local_resources,
        };
        assert_eq!(
            resolve_resource_alias_target(&generic_context, binding_owner, generic_binding_ty),
            ResourceAliasTarget::Suppressed
        );
    }
}
