use sai_hir::types::{GenericArg, GenericParamKind, LangItem, PathRoot, Ty, TyKind};
use sai_hir::{DefId, HirId, OwnerId};
use sai_resolve::package_scopes::{DefKind, PathRes, Res};
use sai_wit::{WitIfaceRef, WitType as Wit, WitTypeRef};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub(super) struct TypeRoots<'hir> {
    pub owner: OwnerId,
    pub types: Vec<&'hir Ty<'hir>>,
}

#[derive(Default)]
struct ContainmentSummary {
    contains_resource: bool,
    dependencies: HashSet<DefId>,
    unknown: bool,
}

#[derive(Clone, Copy)]
struct TypeSubstitution<'hir> {
    owner: OwnerId,
    ty: &'hir Ty<'hir>,
}

impl ContainmentSummary {
    fn merge(&mut self, other: Self) {
        self.contains_resource |= other.contains_resource;
        self.dependencies.extend(other.dependencies);
        self.unknown |= other.unknown;
    }
}

pub(super) fn collect_wit_view_containment(
    wit_view_scopes: &super::Phase1ActiveWitViewScopes,
) -> HashMap<DefId, sai_ty::boundary::ResourceContainment> {
    use sai_ty::boundary::ResourceContainment;
    use sai_wit::WitViewDefKind;

    let mut containment = HashMap::new();
    for foreign in wit_view_scopes.views() {
        for interface in foreign.view.interfaces.values() {
            let wit_interface = &foreign.view.model.interfaces[interface.wit];
            for member in interface.members.values() {
                if !matches!(
                    member.kind,
                    WitViewDefKind::Struct | WitViewDefKind::Enum | WitViewDefKind::Alias
                ) {
                    continue;
                }
                let item = &wit_interface.items[member.item];
                let reference = WitTypeRef {
                    interface: WitIfaceRef {
                        unit: foreign.view.unit.clone(),
                        interface: interface.wit,
                        name: wit_interface.name.clone(),
                    },
                    item: member.item,
                    item_name: item.name.clone(),
                };
                let contains = super::world_wit_type_contains_resource(
                    &Wit::Named(reference),
                    wit_view_scopes,
                    &mut HashSet::new(),
                );
                containment.insert(
                    member.def,
                    match contains {
                        Some(true) => ResourceContainment::Yes,
                        Some(false) => ResourceContainment::No,
                        None => ResourceContainment::Unknown,
                    },
                );
            }
        }
    }
    containment
}

pub(super) fn compute_source_containment<'hir>(
    roots: &HashMap<DefId, TypeRoots<'hir>>,
    aliases: &HashMap<DefId, sai_ty::boundary::BoundaryAlias<'hir>>,
    path_resolutions: &HashMap<HirId, PathRes>,
    canonical_types: &HashMap<DefId, sai_ty::boundary::CanonicalBoundaryType>,
    mut containment: HashMap<DefId, sai_ty::boundary::ResourceContainment>,
) -> HashMap<DefId, sai_ty::boundary::ResourceContainment> {
    use sai_ty::boundary::ResourceContainment;

    let mut summaries = HashMap::with_capacity(roots.len());
    for (definition, type_roots) in roots {
        let mut summary = ContainmentSummary::default();
        for ty in &type_roots.types {
            summary.merge(summarize_type(
                type_roots.owner,
                ty,
                roots,
                aliases,
                path_resolutions,
                canonical_types,
                &containment,
                &HashMap::new(),
                &mut HashSet::new(),
                &mut HashSet::new(),
            ));
            if summary.contains_resource {
                break;
            }
        }
        summaries.insert(*definition, summary);
    }

    // Resource containment is a reachability problem. A closed recursive
    // component with no resource and no incomplete edge is known resource
    // free; seeding every node as Unknown would leave such a component stuck.
    let mut contains_resource = summaries
        .iter()
        .filter_map(|(definition, summary)| summary.contains_resource.then_some(*definition))
        .collect::<HashSet<_>>();
    loop {
        let mut changed = false;
        for (definition, summary) in &summaries {
            if !contains_resource.contains(definition)
                && summary
                    .dependencies
                    .iter()
                    .any(|dependency| contains_resource.contains(dependency))
            {
                changed |= contains_resource.insert(*definition);
            }
        }
        if !changed {
            break;
        }
    }

    let mut unknown = summaries
        .iter()
        .filter_map(|(definition, summary)| {
            (!contains_resource.contains(definition) && summary.unknown).then_some(*definition)
        })
        .collect::<HashSet<_>>();
    loop {
        let mut changed = false;
        for (definition, summary) in &summaries {
            if contains_resource.contains(definition) || unknown.contains(definition) {
                continue;
            }
            if summary.dependencies.iter().any(|dependency| {
                unknown.contains(dependency)
                    || (!roots.contains_key(dependency)
                        && containment.get(dependency) != Some(&ResourceContainment::No))
            }) {
                changed |= unknown.insert(*definition);
            }
        }
        if !changed {
            break;
        }
    }

    for definition in roots.keys() {
        containment.insert(
            *definition,
            if contains_resource.contains(definition) {
                ResourceContainment::Yes
            } else if unknown.contains(definition) {
                ResourceContainment::Unknown
            } else {
                ResourceContainment::No
            },
        );
    }
    containment
}

fn summarize_type<'hir>(
    owner: OwnerId,
    ty: &'hir Ty<'hir>,
    roots: &HashMap<DefId, TypeRoots<'hir>>,
    aliases: &HashMap<DefId, sai_ty::boundary::BoundaryAlias<'hir>>,
    path_resolutions: &HashMap<HirId, PathRes>,
    canonical_types: &HashMap<DefId, sai_ty::boundary::CanonicalBoundaryType>,
    known_containment: &HashMap<DefId, sai_ty::boundary::ResourceContainment>,
    substitutions: &HashMap<DefId, TypeSubstitution<'hir>>,
    alias_stack: &mut HashSet<(DefId, HirId)>,
    type_parameter_stack: &mut HashSet<(DefId, HirId)>,
) -> ContainmentSummary {
    let mut summary = ContainmentSummary::default();
    if ty.hir_id.owner != owner {
        summary.unknown = true;
        return summary;
    }
    match ty.kind {
        TyKind::Borrow(_, inner) => summary.merge(summarize_type(
            owner,
            inner,
            roots,
            aliases,
            path_resolutions,
            canonical_types,
            known_containment,
            substitutions,
            alias_stack,
            type_parameter_stack,
        )),
        TyKind::Tuple(elements) => {
            for element in elements {
                summary.merge(summarize_type(
                    owner,
                    element,
                    roots,
                    aliases,
                    path_resolutions,
                    canonical_types,
                    known_containment,
                    substitutions,
                    alias_stack,
                    type_parameter_stack,
                ));
                if summary.contains_resource {
                    break;
                }
            }
        }
        TyKind::Path(path) => {
            if matches!(
                path.root,
                PathRoot::Lang(LangItem::Option | LangItem::Result)
            ) {
                let Some(arguments) = type_arguments(path) else {
                    summary.unknown = true;
                    return summary;
                };
                for argument in arguments {
                    summary.merge(summarize_type(
                        owner,
                        argument,
                        roots,
                        aliases,
                        path_resolutions,
                        canonical_types,
                        known_containment,
                        substitutions,
                        alias_stack,
                        type_parameter_stack,
                    ));
                    if summary.contains_resource {
                        break;
                    }
                }
                return summary;
            }
            if path.root != PathRoot::Plain {
                summary.unknown = true;
                return summary;
            }
            let Some(path_resolution) = path_resolutions.get(&path.hir_id) else {
                summary.unknown = true;
                return summary;
            };
            match path_resolution.base {
                Res::Def(DefKind::TyParam, parameter) => {
                    let Some(substitution) = substitutions.get(&parameter) else {
                        summary.unknown = true;
                        return summary;
                    };
                    let active_substitution = (parameter, substitution.ty.hir_id);
                    if !type_parameter_stack.insert(active_substitution) {
                        summary.unknown = true;
                        return summary;
                    }
                    summary.merge(summarize_type(
                        substitution.owner,
                        substitution.ty,
                        roots,
                        aliases,
                        path_resolutions,
                        canonical_types,
                        known_containment,
                        substitutions,
                        alias_stack,
                        type_parameter_stack,
                    ));
                    type_parameter_stack.remove(&active_substitution);
                    return summary;
                }
                Res::Def(DefKind::Resource, _) => {
                    summary.contains_resource = true;
                    return summary;
                }
                Res::Def(DefKind::Alias, definition) if aliases.contains_key(&definition) => {
                    let alias = &aliases[&definition];
                    let application = (definition, path.hir_id);
                    if !alias_stack.insert(application) {
                        summary.unknown = true;
                        return summary;
                    }
                    let Some(arguments) = path_generic_arguments(path) else {
                        alias_stack.remove(&application);
                        summary.unknown = true;
                        return summary;
                    };
                    if arguments.len() != alias.generic_params.len() {
                        alias_stack.remove(&application);
                        summary.unknown = true;
                        return summary;
                    }
                    let mut alias_substitutions = substitutions.clone();
                    for (parameter, argument) in alias.generic_params.iter().zip(arguments) {
                        match (parameter.kind, argument) {
                            (GenericParamKind::Type { .. }, GenericArg::Type(ty)) => {
                                alias_substitutions.insert(
                                    parameter.def_id.to_def_id(),
                                    TypeSubstitution { owner, ty },
                                );
                            }
                            (GenericParamKind::Const { .. }, GenericArg::Const(_)) => {}
                            _ => {
                                alias_stack.remove(&application);
                                summary.unknown = true;
                                return summary;
                            }
                        }
                    }
                    summary.merge(summarize_type(
                        alias.owner,
                        alias.ty,
                        roots,
                        aliases,
                        path_resolutions,
                        canonical_types,
                        known_containment,
                        &alias_substitutions,
                        alias_stack,
                        type_parameter_stack,
                    ));
                    alias_stack.remove(&application);
                    return summary;
                }
                Res::Def(_, definition) if roots.contains_key(&definition) => {
                    summary.dependencies.insert(definition);
                    if has_type_arguments(path) {
                        // Interface types with generic arguments need their
                        // own parameter substitution before containment can be
                        // decided. Keep the fact unknown rather than infer it
                        // from an argument that the type might not use.
                        summary.unknown = true;
                    }
                }
                Res::Def(_, definition)
                    if known_containment.contains_key(&definition)
                        && !roots.contains_key(&definition) =>
                {
                    if known_containment.get(&definition)
                        == Some(&sai_ty::boundary::ResourceContainment::Yes)
                    {
                        summary.contains_resource = true;
                    } else if known_containment.get(&definition)
                        == Some(&sai_ty::boundary::ResourceContainment::Unknown)
                    {
                        summary.unknown = true;
                    }
                }
                Res::PrimTy(sai_resolve::prelude::PrimTy::Array) => {
                    let Some(arguments) = path_generic_arguments(path) else {
                        summary.unknown = true;
                        return summary;
                    };
                    let [GenericArg::Type(element), GenericArg::Const(_)] = arguments.as_slice()
                    else {
                        summary.unknown = true;
                        return summary;
                    };
                    summary.merge(summarize_type(
                        owner,
                        element,
                        roots,
                        aliases,
                        path_resolutions,
                        canonical_types,
                        known_containment,
                        substitutions,
                        alias_stack,
                        type_parameter_stack,
                    ));
                }
                Res::PrimTy(_) => {}
                Res::Def(_, definition) if canonical_types.contains_key(&definition) => {
                    let canonical = canonical_types[&definition];
                    if canonical_container_has_type_arguments(canonical) {
                        let Some(arguments) = type_arguments(path) else {
                            summary.unknown = true;
                            return summary;
                        };
                        for argument in arguments {
                            summary.merge(summarize_type(
                                owner,
                                argument,
                                roots,
                                aliases,
                                path_resolutions,
                                canonical_types,
                                known_containment,
                                substitutions,
                                alias_stack,
                                type_parameter_stack,
                            ));
                            if summary.contains_resource {
                                break;
                            }
                        }
                    } else if matches!(
                        canonical,
                        sai_ty::boundary::CanonicalBoundaryType::Box
                            | sai_ty::boundary::CanonicalBoundaryType::FutureReader
                            | sai_ty::boundary::CanonicalBoundaryType::StreamReader
                    ) && has_type_arguments(path)
                    {
                        summary.unknown = true;
                    }
                }
                Res::Err | Res::Local(_) | Res::TestOnly(_) | Res::Def(_, _) => {
                    summary.unknown = true;
                }
            }
        }
        TyKind::FnPtr { .. } | TyKind::Infer | TyKind::Err => summary.unknown = true,
    }
    summary
}

fn canonical_container_has_type_arguments(
    canonical: sai_ty::boundary::CanonicalBoundaryType,
) -> bool {
    matches!(
        canonical,
        sai_ty::boundary::CanonicalBoundaryType::Option
            | sai_ty::boundary::CanonicalBoundaryType::Result
            | sai_ty::boundary::CanonicalBoundaryType::Vec
            | sai_ty::boundary::CanonicalBoundaryType::Slice
    )
}

fn has_type_arguments(path: sai_hir::types::Path<'_>) -> bool {
    path.segments.iter().any(|segment| {
        segment
            .args
            .is_some_and(|arguments| !arguments.args.is_empty())
    })
}

fn type_arguments(path: sai_hir::types::Path<'_>) -> Option<Vec<&Ty<'_>>> {
    if !has_type_arguments(path) {
        return None;
    }
    path_generic_arguments(path)?
        .into_iter()
        .map(|argument| match argument {
            GenericArg::Type(ty) => Some(ty),
            GenericArg::Const(_) | GenericArg::Binding { .. } => None,
        })
        .collect()
}

fn path_generic_arguments(path: sai_hir::types::Path<'_>) -> Option<Vec<GenericArg<'_>>> {
    let mut arguments = None;
    for (index, segment) in path.segments.iter().enumerate() {
        let Some(generic_args) = segment.args else {
            continue;
        };
        if index + 1 != path.segments.len() || arguments.is_some() {
            return None;
        }
        arguments = Some(generic_args.args.to_vec());
    }
    Some(arguments.unwrap_or_default())
}
