//! Conversion from the resolved `wit-parser` graph into SAI's owned WIT model.

use crate::{
    ParsedWitPackageGroup, Unsupported, WitCase, WitField, WitFunc, WitIfaceIdx, WitIfaceRef,
    WitIndexVec, WitInterface, WitItem, WitItemIdx, WitItemKind, WitMember, WitMemberKind,
    WitPackage, WitPackageDeclaration, WitPackageName, WitSourceLocation, WitSpan,
    WitStabilityVersion, WitType, WitTypeRef, WitUnit, WitUnitKind, WitWorld, WitWorldIdx,
    WitWorldItem, WitWorldItemDirection, WitWorldItemOrder, WitWorldRef,
};
use sai_package::version::Version;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use wit_parser::{
    Function, FunctionKind, Handle, InterfaceId, PackageId as ParserPackageId, Resolve, Span,
    Stability, Type, TypeDefKind, TypeId, TypeOwner, WorldId, WorldItem, WorldKey,
};

/// A failure while converting a resolved parser package into the query model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitModelError {
    NestedPackages {
        count: usize,
        location: Option<WitSourceLocation>,
    },
    PackageMismatch {
        expected: String,
        found: String,
        location: Option<WitSourceLocation>,
    },
    Resolve(String),
    InvalidVersion(String),
    TooManyDefinitions,
    MissingInterface,
    MissingWorld,
    MissingItem,
    UnmappedSourceSpan,
    InvalidType(String),
    UnsupportedWorldItem,
    ReusedAfterFailure,
}

impl fmt::Display for WitModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NestedPackages { count, .. } => {
                write!(formatter, "WIT input declares {count} nested packages")
            }
            Self::PackageMismatch {
                expected, found, ..
            } => write!(
                formatter,
                "WIT package `{found}` does not match unit `{expected}`"
            ),
            Self::Resolve(message) => write!(formatter, "failed to resolve WIT package: {message}"),
            Self::InvalidVersion(version) => {
                write!(
                    formatter,
                    "WIT package version `{version}` is not a SAI version"
                )
            }
            Self::TooManyDefinitions => formatter.write_str("WIT package has too many definitions"),
            Self::MissingInterface => {
                formatter.write_str("WIT reference names an interface outside the loaded package")
            }
            Self::MissingWorld => {
                formatter.write_str("WIT reference names a world outside the loaded package")
            }
            Self::MissingItem => {
                formatter.write_str("WIT reference names a type outside the loaded package")
            }
            Self::UnmappedSourceSpan => {
                formatter.write_str("WIT source span is outside the loaded source files")
            }
            Self::InvalidType(message) => write!(formatter, "invalid resolved WIT type: {message}"),
            Self::UnsupportedWorldItem => {
                formatter.write_str("resolved WIT world contains an unsupported item")
            }
            Self::ReusedAfterFailure => {
                formatter.write_str("WIT resolve context cannot be reused after a failed load")
            }
        }
    }
}

impl std::error::Error for WitModelError {}

#[derive(Clone, Debug)]
struct SourceWorldInclude {
    source_world: String,
    target_package: String,
    target_world: String,
    names: Vec<(String, String)>,
    docs: Option<std::sync::Arc<str>>,
    since: Option<WitStabilityVersion>,
    deprecated: Option<WitStabilityVersion>,
    source_start: u32,
    span: Option<WitSpan>,
}

#[derive(Clone, Debug, Default)]
struct SourceWorldItems {
    imports: HashMap<SourceWorldItemKey, SourceWorldItem>,
    exports: HashMap<SourceWorldItemKey, SourceWorldItem>,
}

#[derive(Clone, Debug)]
struct SourceWorldItem {
    start: u32,
    span: Option<WitSpan>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SourceWorldItemKey {
    Function(String),
    Interface { unit: String, name: String },
}

/// A short-lived loader context for constructing one package model at a time.
///
/// This context owns only parser-local identity and source maps. It is not a
/// query value. A failed push may leave the parser's `Resolve` partially
/// updated, so the context is poisoned and cannot be reused.
#[derive(Default)]
pub struct WitResolveContext {
    package_units: HashMap<ParserPackageId, WitUnit>,
    source_mappings: Vec<(u32, crate::WitSourceMapping)>,
    named_result_function_spans: HashSet<(u32, u32)>,
    failed: bool,
}

impl WitResolveContext {
    /// Push one parsed package into a resolve that already contains its
    /// dependencies, then copy the package into the parser-independent model.
    pub fn push_package(
        &mut self,
        resolve: &mut Resolve,
        parsed: ParsedWitPackageGroup,
    ) -> Result<WitPackage, WitModelError> {
        if self.failed {
            return Err(WitModelError::ReusedAfterFailure);
        }
        let unit = parsed.unit.clone();
        let package_name = parsed.group.main.name.clone();
        let nested_count = parsed.group.nested.len();
        if nested_count > 0 {
            return Err(WitModelError::NestedPackages {
                count: nested_count,
                location: parsed.nested_package_locations.first().cloned(),
            });
        }
        let found_name = package_name.to_string();
        if found_name != unit.canonical_name() {
            return Err(WitModelError::PackageMismatch {
                expected: unit.canonical_name().to_owned(),
                found: found_name,
                location: parsed.package_name_location.clone(),
            });
        }

        let source_world_includes = capture_source_world_includes(&parsed, &self.package_units)?;
        let source_world_items = capture_source_world_items(&parsed, &self.package_units);
        let group = parsed.group;
        let source_map = group.source_map;
        let source_offset = resolve.push_source_map(source_map);
        let named_result_function_spans = parsed
            .named_result_function_spans
            .iter()
            .map(|span| {
                (
                    span.start
                        .checked_add(source_offset)
                        .expect("Resolve source spans are bounded"),
                    span.end
                        .checked_add(source_offset)
                        .expect("Resolve source spans are bounded"),
                )
            })
            .collect::<Vec<_>>();
        let parser_package = match resolve.push(group.main, source_offset) {
            Ok(package) => package,
            Err(error) => {
                self.failed = true;
                return Err(WitModelError::Resolve(error.to_string()));
            }
        };
        self.package_units.insert(parser_package, unit.clone());
        self.source_mappings.extend(
            parsed
                .source_mappings
                .into_iter()
                .map(|mapping| (source_offset, mapping)),
        );
        self.named_result_function_spans
            .extend(named_result_function_spans);
        let source_world_includes = source_world_includes
            .into_iter()
            .map(|mut include| {
                include.source_start = include
                    .source_start
                    .checked_add(source_offset)
                    .expect("Resolve source spans are bounded");
                include
            })
            .collect::<Vec<_>>();
        let source_world_items = source_world_items
            .into_iter()
            .map(|(name, mut items)| {
                (name, {
                    for item in items.imports.values_mut().chain(items.exports.values_mut()) {
                        item.start = item
                            .start
                            .checked_add(source_offset)
                            .expect("Resolve source spans are bounded");
                    }
                    items
                })
            })
            .collect::<HashMap<_, _>>();

        model_package(
            resolve,
            PackageModelInput {
                package_id: parser_package,
                unit,
                package_units: &self.package_units,
                source_mappings: &self.source_mappings,
                named_result_function_spans: &self.named_result_function_spans,
                package_name: &package_name,
                source_world_includes: &source_world_includes,
                source_world_items: &source_world_items,
            },
        )
    }
}

fn capture_source_world_items(
    parsed: &ParsedWitPackageGroup,
    package_units: &HashMap<ParserPackageId, WitUnit>,
) -> HashMap<String, SourceWorldItems> {
    parsed
        .group
        .main
        .worlds
        .iter()
        .map(|(_, world)| {
            let mut items = SourceWorldItems::default();
            for (key, item) in &world.imports {
                if let (Some(name), Some(start)) = (
                    source_world_item_key(parsed, package_units, key, item),
                    span_start(item.span()),
                ) {
                    items.imports.insert(
                        name,
                        SourceWorldItem {
                            start,
                            span: parsed
                                .location_for_parser_span(item.span())
                                .map(|location| WitSpan {
                                    path: location.path,
                                    range: location.range,
                                }),
                        },
                    );
                }
            }
            for (key, item) in &world.exports {
                if let (Some(name), Some(start)) = (
                    source_world_item_key(parsed, package_units, key, item),
                    span_start(item.span()),
                ) {
                    items.exports.insert(
                        name,
                        SourceWorldItem {
                            start,
                            span: parsed
                                .location_for_parser_span(item.span())
                                .map(|location| WitSpan {
                                    path: location.path,
                                    range: location.range,
                                }),
                        },
                    );
                }
            }
            (world.name.clone(), items)
        })
        .collect()
}

fn source_world_item_key(
    parsed: &ParsedWitPackageGroup,
    package_units: &HashMap<ParserPackageId, WitUnit>,
    key: &WorldKey,
    item: &WorldItem,
) -> Option<SourceWorldItemKey> {
    match item {
        WorldItem::Interface { id, span, .. } => {
            let package = &parsed.group.main;
            let name = package
                .interfaces
                .get(*id)
                .and_then(|interface| interface.name.clone());
            if let Some(name) = name {
                return Some(SourceWorldItemKey::Interface {
                    unit: parsed.unit().canonical_name().to_owned(),
                    name,
                });
            }
            if let Some(reference) = parsed.foreign_reference_for_parser_span(*span) {
                let unit = reference
                    .resolved_unit
                    .as_ref()
                    .or_else(|| package_unit_for_foreign_reference(reference, package_units))?;
                return Some(SourceWorldItemKey::Interface {
                    unit: unit.canonical_name().to_owned(),
                    name: reference.item.clone(),
                });
            }
            match key {
                WorldKey::Name(name) => Some(SourceWorldItemKey::Interface {
                    unit: parsed.unit().canonical_name().to_owned(),
                    name: name.clone(),
                }),
                WorldKey::Interface(_) => None,
            }
        }
        WorldItem::Function(function) => Some(SourceWorldItemKey::Function(function.name.clone())),
        WorldItem::Type { .. } => None,
    }
}

fn model_world_item_key(item: &WitWorldItem) -> SourceWorldItemKey {
    match item {
        WitWorldItem::Interface { reference, .. } => SourceWorldItemKey::Interface {
            unit: reference.unit.canonical_name().to_owned(),
            name: reference.name.clone(),
        },
        WitWorldItem::Func { name, .. } => SourceWorldItemKey::Function(name.clone()),
    }
}

fn source_world_item_span(
    source_items: Option<&SourceWorldItems>,
    direction: WitWorldItemDirection,
    key: &SourceWorldItemKey,
) -> Option<WitSpan> {
    let source_items = source_items?;
    let item = match direction {
        WitWorldItemDirection::Import => source_items.imports.get(key),
        WitWorldItemDirection::Export => source_items.exports.get(key),
        WitWorldItemDirection::Include => None,
    }?;
    item.span.clone()
}

fn capture_source_world_includes(
    parsed: &ParsedWitPackageGroup,
    package_units: &HashMap<ParserPackageId, WitUnit>,
) -> Result<Vec<SourceWorldInclude>, WitModelError> {
    let local_package = parsed.unit().canonical_name().to_owned();
    let mut result = Vec::new();
    let mut doc_scanners = HashMap::new();
    for (_, world) in parsed.group.main.worlds.iter() {
        for include in &world.includes {
            if matches!(include.stability, Stability::Unstable { .. }) {
                continue;
            }
            let (target_package, target_world) =
                if let Some(reference) = parsed.foreign_reference_for_parser_span(include.span) {
                    let target_unit = reference
                        .resolved_unit
                        .as_ref()
                        .or_else(|| package_unit_for_foreign_reference(reference, package_units))
                        .ok_or(WitModelError::MissingWorld)?;
                    (
                        target_unit.canonical_name().to_owned(),
                        reference.item.clone(),
                    )
                } else {
                    let target = parsed
                        .group
                        .main
                        .worlds
                        .get(include.id)
                        .ok_or(WitModelError::MissingWorld)?;
                    (local_package.clone(), target.name.clone())
                };
            let (since, deprecated) = stability(&include.stability)?;
            let location = parsed.location_for_parser_span(include.span);
            let docs = location
                .as_ref()
                .and_then(|location| {
                    let source = parsed.source_text(&location.path)?;
                    let scanner = doc_scanners
                        .entry(location.path.clone())
                        .or_insert_with(|| crate::source_scan::WorldIncludeDocScanner::new(source));
                    scanner.docs(location.range.start)
                })
                .map(std::sync::Arc::from);
            result.push(SourceWorldInclude {
                source_world: world.name.clone(),
                target_package,
                target_world,
                names: include
                    .names
                    .iter()
                    .map(|name| (name.name.clone(), name.as_.clone()))
                    .collect(),
                docs,
                since,
                deprecated,
                source_start: include.span.start(),
                span: location.map(|location| WitSpan {
                    path: location.path,
                    range: location.range,
                }),
            });
        }
    }
    Ok(result)
}

fn package_unit_for_foreign_reference<'a>(
    reference: &crate::WitForeignReference,
    package_units: &'a HashMap<ParserPackageId, WitUnit>,
) -> Option<&'a WitUnit> {
    let requested_version = reference
        .version
        .as_deref()
        .map(|version| {
            Version::parse(
                version
                    .split_once('+')
                    .map_or(version, |(without_build, _)| without_build),
            )
        })
        .transpose()
        .ok()?;
    let mut matches = package_units
        .values()
        .filter_map(|unit| {
            let (identity, version) = unit.canonical_name().rsplit_once('@')?;
            if identity != reference.identity {
                return None;
            }
            let selected_version = Version::parse(version).ok()?;
            if requested_version.as_ref().is_some_and(|requested| {
                requested.compatibility_line() != selected_version.compatibility_line()
                    || requested > &selected_version
            }) {
                return None;
            }
            Some(unit)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.canonical_name().cmp(right.canonical_name()));
    matches.dedup();
    (matches.len() == 1).then(|| matches[0])
}

struct PackageModelInput<'a> {
    package_id: ParserPackageId,
    unit: WitUnit,
    package_units: &'a HashMap<ParserPackageId, WitUnit>,
    source_mappings: &'a [(u32, crate::WitSourceMapping)],
    named_result_function_spans: &'a HashSet<(u32, u32)>,
    package_name: &'a wit_parser::PackageName,
    source_world_includes: &'a [SourceWorldInclude],
    source_world_items: &'a HashMap<String, SourceWorldItems>,
}

fn model_package(
    resolve: &Resolve,
    PackageModelInput {
        package_id,
        unit,
        package_units,
        source_mappings,
        named_result_function_spans,
        package_name,
        source_world_includes,
        source_world_items,
    }: PackageModelInput<'_>,
) -> Result<WitPackage, WitModelError> {
    let version = package_name
        .version
        .as_ref()
        .map(|version| {
            Version::parse(&version.to_string())
                .map_err(|_| WitModelError::InvalidVersion(version.to_string()))
        })
        .transpose()?;
    let name = WitPackageName {
        namespace: package_name.namespace.clone(),
        name: package_name.name.clone(),
        version,
    };
    let package = &resolve.packages[package_id];

    let interface_ids = package.interfaces.values().copied().collect::<Vec<_>>();
    let world_ids = package.worlds.values().copied().collect::<Vec<_>>();
    let mut interface_indexes = HashMap::with_capacity(interface_ids.len());
    for (index, id) in interface_ids.iter().copied().enumerate() {
        interface_indexes.insert(id, interface_index(index)?);
    }
    let mut world_indexes = HashMap::with_capacity(world_ids.len());
    for (index, id) in world_ids.iter().copied().enumerate() {
        world_indexes.insert(id, world_index(index)?);
    }

    let type_references = build_type_references(resolve, package_units)?;
    let generated_world_type_aliases = build_generated_world_type_aliases(
        resolve,
        package_id,
        &unit,
        package_units,
        &type_references,
    )?;
    let mut world_type_references = type_references.clone();
    world_type_references.extend(generated_world_type_aliases.clone());
    let unsupported_types = build_unsupported_types(resolve);

    let mut interfaces = Vec::with_capacity(interface_ids.len());
    for (wit_interface_name, interface_id) in package.interfaces.iter() {
        let parser_interface = &resolve.interfaces[*interface_id];
        let model_index = *interface_indexes
            .get(interface_id)
            .ok_or(WitModelError::MissingInterface)?;
        let interface_span = source_span(parser_interface.span, &unit, source_mappings);
        let mut items =
            Vec::with_capacity(parser_interface.types.len() + parser_interface.functions.len());

        let mut resource_items = HashMap::new();
        for (name, type_id) in &parser_interface.types {
            let definition = &resolve.types[*type_id];
            let owner_interface = match definition.owner {
                TypeOwner::Interface(owner) => owner,
                TypeOwner::World(_) => {
                    return Err(WitModelError::InvalidType(format!(
                        "interface item `{name}` is owned by a world"
                    )));
                }
                TypeOwner::None => {
                    return Err(WitModelError::InvalidType(format!(
                        "interface item `{name}` has no nominal owner"
                    )));
                }
            };
            let docs = docs(&definition.docs.contents);
            let (since, deprecated) = stability(&definition.stability)?;
            let span = source_span(definition.span, &unit, source_mappings);

            let (kind, unsupported) = if owner_interface != *interface_id {
                let reference = type_references
                    .get(type_id)
                    .ok_or(WitModelError::MissingItem)?
                    .clone();
                (
                    WitItemKind::Used(reference),
                    unsupported_types.get(type_id).copied().flatten(),
                )
            } else {
                item_kind(
                    resolve,
                    *type_id,
                    &unit,
                    &interface_indexes,
                    &type_references,
                    &unsupported_types,
                )?
            };
            let source_declaration = if owner_interface == *interface_id
                && matches!(definition.kind, TypeDefKind::Resource)
            {
                source_interface_resource_declaration(
                    definition.span,
                    wit_interface_name,
                    name,
                    &unit,
                    source_mappings,
                )
            } else {
                None
            };
            let item_index = item_index(items.len())?;
            if matches!(kind, WitItemKind::Resource(_)) {
                resource_items.insert(*type_id, item_index);
            }
            items.push(WitItem {
                name: name.clone(),
                docs,
                since,
                deprecated,
                kind,
                unsupported,
                declaration_span: source_declaration.as_ref().map(|(span, _)| span.clone()),
                declaration_token_pairs: source_declaration.map(|(_, token_pairs)| token_pairs),
                span,
            });
        }

        let mut free_functions = Vec::new();
        for function in parser_interface.functions.values() {
            if let Some(resource_id) = function.kind.resource() {
                let resource_item = *resource_items
                    .get(&resource_id)
                    .ok_or(WitModelError::MissingItem)?;
                let (member_kind, member_name) = resource_member_name(function)?;
                let (func, unsupported) = function_model(
                    function,
                    resolve,
                    &unit,
                    &interface_indexes,
                    &type_references,
                    &unsupported_types,
                    named_result_function_spans,
                )?;
                let item = items
                    .get_mut(resource_item.index())
                    .ok_or(WitModelError::MissingItem)?;
                let WitItemKind::Resource(members) = &mut item.kind else {
                    return Err(WitModelError::MissingItem);
                };
                members.push(WitMember {
                    name: member_name,
                    kind: member_kind,
                    func,
                    docs: docs(&function.docs.contents),
                    since: stability(&function.stability)?.0,
                    deprecated: stability(&function.stability)?.1,
                    unsupported,
                });
            } else {
                let (func, unsupported) = function_model(
                    function,
                    resolve,
                    &unit,
                    &interface_indexes,
                    &type_references,
                    &unsupported_types,
                    named_result_function_spans,
                )?;
                let source_declaration = source_function_declaration(
                    function.span,
                    wit_interface_name,
                    &function.name,
                    &unit,
                    source_mappings,
                );
                free_functions.push(WitItem {
                    name: function.name.clone(),
                    docs: docs(&function.docs.contents),
                    since: stability(&function.stability)?.0,
                    deprecated: stability(&function.stability)?.1,
                    kind: WitItemKind::Func(func),
                    unsupported,
                    span: source_span(function.span, &unit, source_mappings),
                    declaration_span: source_declaration.as_ref().map(|(span, _)| span.clone()),
                    declaration_token_pairs: source_declaration.map(|(_, token_pairs)| token_pairs),
                });
            }
        }
        items.extend(free_functions);

        interfaces.push(WitInterface {
            name: wit_interface_name.clone(),
            docs: docs(&parser_interface.docs.contents),
            since: stability(&parser_interface.stability)?.0,
            deprecated: stability(&parser_interface.stability)?.1,
            items: WitIndexVec::try_from_vec(items)
                .map_err(|_| WitModelError::TooManyDefinitions)?,
            span: interface_span,
        });
        debug_assert_eq!(model_index.index(), interfaces.len() - 1);
    }

    let mut worlds = Vec::with_capacity(world_ids.len());
    for (name, world_id) in package.worlds.iter() {
        let world = &resolve.worlds[*world_id];
        let (since, deprecated) = stability(&world.stability)?;
        let mut unsupported = None;
        let direct_items = source_world_items.get(name);
        let imports = world_items(
            &world.imports,
            resolve,
            &unit,
            package_units,
            &interface_indexes,
            &world_type_references,
            &generated_world_type_aliases,
            &unsupported_types,
            source_mappings,
            direct_items,
            WitWorldItemDirection::Import,
            named_result_function_spans,
            &mut unsupported,
        )?;
        let exports = world_items(
            &world.exports,
            resolve,
            &unit,
            package_units,
            &interface_indexes,
            &world_type_references,
            &generated_world_type_aliases,
            &unsupported_types,
            source_mappings,
            direct_items,
            WitWorldItemDirection::Export,
            named_result_function_spans,
            &mut unsupported,
        )?;
        let mut includes = Vec::new();
        let mut source_order = Vec::with_capacity(imports.len() + exports.len());
        for (index, (item, _)) in imports.iter().enumerate() {
            if let Some(start) = direct_items
                .and_then(|items| items.imports.get(&model_world_item_key(item)))
                .map(|item| item.start)
            {
                source_order.push((
                    Some(start),
                    WitWorldItemOrder::import(
                        u32::try_from(index).map_err(|_| WitModelError::TooManyDefinitions)?,
                    ),
                ));
            }
        }
        for (index, (item, _)) in exports.iter().enumerate() {
            if let Some(start) = direct_items
                .and_then(|items| items.exports.get(&model_world_item_key(item)))
                .map(|item| item.start)
            {
                source_order.push((
                    Some(start),
                    WitWorldItemOrder::export(
                        u32::try_from(index).map_err(|_| WitModelError::TooManyDefinitions)?,
                    ),
                ));
            }
        }
        for include in source_world_includes
            .iter()
            .filter(|include| include.source_world == *name)
        {
            let target = world_reference_named(
                resolve,
                package_units,
                &include.target_package,
                &include.target_world,
            )?;
            includes.push(WitWorldRef {
                names: include.names.clone(),
                docs: include.docs.clone(),
                since: include.since.clone(),
                deprecated: include.deprecated.clone(),
                span: include.span.clone(),
                ..target
            });
            source_order.push((
                Some(include.source_start),
                WitWorldItemOrder::include(
                    u32::try_from(includes.len() - 1)
                        .map_err(|_| WitModelError::TooManyDefinitions)?,
                ),
            ));
        }
        source_order.sort_by_key(|(start, order)| {
            (
                start.unwrap_or(u32::MAX),
                match order.direction {
                    crate::WitWorldItemDirection::Import => 0_u8,
                    crate::WitWorldItemDirection::Export => 1_u8,
                    crate::WitWorldItemDirection::Include => 2_u8,
                },
                order.index,
            )
        });
        let source_order = source_order.into_iter().map(|(_, order)| order).collect();
        worlds.push(WitWorld {
            name: name.clone(),
            docs: docs(&world.docs.contents),
            since,
            deprecated,
            imports: imports.into_iter().map(|(item, _)| item).collect(),
            exports: exports.into_iter().map(|(item, _)| item).collect(),
            includes,
            source_order,
            unsupported,
            span: source_span(world.span, &unit, source_mappings),
        });
    }

    let declaration_order = package_declaration_order(
        unit.kind(),
        resolve,
        &interface_ids,
        &world_ids,
        &interface_indexes,
        &world_indexes,
        source_mappings,
    )?;

    Ok(WitPackage {
        unit,
        name,
        interfaces: WitIndexVec::try_from_vec(interfaces)
            .map_err(|_| WitModelError::TooManyDefinitions)?,
        worlds: WitIndexVec::<WitWorldIdx, _>::try_from_vec(worlds)
            .map_err(|_| WitModelError::TooManyDefinitions)?,
        declaration_order,
    })
}

fn package_declaration_order(
    unit_kind: WitUnitKind,
    resolve: &Resolve,
    interface_ids: &[InterfaceId],
    world_ids: &[WorldId],
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    world_indexes: &HashMap<WorldId, WitWorldIdx>,
    source_mappings: &[(u32, crate::WitSourceMapping)],
) -> Result<Vec<WitPackageDeclaration>, WitModelError> {
    if matches!(unit_kind, WitUnitKind::Generated { .. }) {
        return interface_ids
            .iter()
            .map(|id| {
                interface_indexes
                    .get(id)
                    .copied()
                    .map(WitPackageDeclaration::Interface)
                    .ok_or(WitModelError::MissingInterface)
            })
            .chain(world_ids.iter().map(|id| {
                world_indexes
                    .get(id)
                    .copied()
                    .map(WitPackageDeclaration::World)
                    .ok_or(WitModelError::MissingWorld)
            }))
            .collect();
    }

    let mut declarations = Vec::with_capacity(interface_ids.len() + world_ids.len());
    for (index, id) in interface_ids.iter().copied().enumerate() {
        let interface = resolve
            .interfaces
            .get(id)
            .ok_or(WitModelError::MissingInterface)?;
        let (file_order, path, byte_start) = source_order_position(interface.span, source_mappings)
            .ok_or(WitModelError::UnmappedSourceSpan)?;
        let declaration = interface_indexes
            .get(&id)
            .copied()
            .map(WitPackageDeclaration::Interface)
            .ok_or(WitModelError::MissingInterface)?;
        declarations.push((file_order, path, byte_start, 0_u8, index, declaration));
    }
    for (index, id) in world_ids.iter().copied().enumerate() {
        let world = resolve.worlds.get(id).ok_or(WitModelError::MissingWorld)?;
        let (file_order, path, byte_start) = source_order_position(world.span, source_mappings)
            .ok_or(WitModelError::UnmappedSourceSpan)?;
        let declaration = world_indexes
            .get(&id)
            .copied()
            .map(WitPackageDeclaration::World)
            .ok_or(WitModelError::MissingWorld)?;
        declarations.push((file_order, path, byte_start, 1_u8, index, declaration));
    }

    declarations.sort_by(|left, right| {
        (&left.0, left.1.as_bytes(), &left.2, &left.3, &left.4).cmp(&(
            &right.0,
            right.1.as_bytes(),
            &right.2,
            &right.3,
            &right.4,
        ))
    });
    Ok(declarations
        .into_iter()
        .map(|(_, _, _, _, _, declaration)| declaration)
        .collect())
}

fn source_order_position(
    span: Span,
    source_mappings: &[(u32, crate::WitSourceMapping)],
) -> Option<(usize, String, usize)> {
    if !span.is_known() {
        return None;
    }
    let start = span.start();
    source_mappings
        .iter()
        .enumerate()
        .find_map(|(file_order, (base, source))| {
            let source_start = base.checked_add(source.offset)?;
            let source_end = source_start.checked_add(u32::try_from(source.parser_len).ok()?)?;
            if start < source_start || start > source_end {
                return None;
            }
            let parser_start = usize::try_from(start - source_start).ok()?;
            let byte_start = *source.parser_to_source.get(parser_start)?;
            Some((file_order, source.path.clone(), byte_start))
        })
}

fn item_kind(
    resolve: &Resolve,
    type_id: TypeId,
    unit: &WitUnit,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
) -> Result<(WitItemKind, Option<Unsupported>), WitModelError> {
    let definition = &resolve.types[type_id];
    let result = match &definition.kind {
        TypeDefKind::Record(record) => {
            let mut unsupported = None;
            let fields = record
                .fields
                .iter()
                .map(|field| {
                    let (ty, found) = convert_type(
                        field.ty,
                        resolve,
                        unit,
                        interface_indexes,
                        type_references,
                        unsupported_types,
                    )?;
                    unsupported = first_unsupported(unsupported, found);
                    Ok(WitField {
                        name: field.name.clone(),
                        docs: docs(&field.docs.contents),
                        ty,
                    })
                })
                .collect::<Result<Vec<_>, WitModelError>>()?;
            (WitItemKind::Record(fields), unsupported)
        }
        TypeDefKind::Resource => (WitItemKind::Resource(Vec::new()), None),
        TypeDefKind::Flags(flags) => (
            WitItemKind::Flags(flags.flags.iter().map(|flag| flag.name.clone()).collect()),
            None,
        ),
        TypeDefKind::Variant(variant) => {
            let mut unsupported = None;
            let cases = variant
                .cases
                .iter()
                .map(|case| {
                    let payload = case
                        .ty
                        .map(|ty| {
                            convert_type(
                                ty,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )
                        })
                        .transpose()?
                        .map(|(ty, found)| {
                            unsupported = first_unsupported(unsupported, found);
                            ty
                        });
                    Ok(WitCase {
                        name: case.name.clone(),
                        docs: docs(&case.docs.contents),
                        payload,
                    })
                })
                .collect::<Result<Vec<_>, WitModelError>>()?;
            (WitItemKind::Variant(cases), unsupported)
        }
        TypeDefKind::Enum(enumeration) => (
            WitItemKind::Enum(
                enumeration
                    .cases
                    .iter()
                    .map(|case| case.name.clone())
                    .collect(),
            ),
            None,
        ),
        TypeDefKind::Type(ty) => {
            let (ty, unsupported) = convert_type(
                *ty,
                resolve,
                unit,
                interface_indexes,
                type_references,
                unsupported_types,
            )?;
            (WitItemKind::Alias(ty), unsupported)
        }
        TypeDefKind::Handle(handle) => {
            let (ty, unsupported) = convert_handle(
                *handle,
                resolve,
                unit,
                interface_indexes,
                type_references,
                unsupported_types,
            )?;
            (WitItemKind::Alias(ty), unsupported)
        }
        TypeDefKind::Option(_)
        | TypeDefKind::Result(_)
        | TypeDefKind::Tuple(_)
        | TypeDefKind::List(_)
        | TypeDefKind::Map(_, _)
        | TypeDefKind::FixedLengthList(_, _)
        | TypeDefKind::Future(_)
        | TypeDefKind::Stream(_) => {
            let (ty, unsupported) = convert_type(
                Type::Id(type_id),
                resolve,
                unit,
                interface_indexes,
                type_references,
                unsupported_types,
            )?;
            (WitItemKind::Alias(ty), unsupported)
        }
        TypeDefKind::Unknown => {
            return Err(WitModelError::InvalidType(
                "unknown type remains after resolution".to_owned(),
            ));
        }
    };

    Ok(result)
}

fn convert_type(
    ty: Type,
    resolve: &Resolve,
    unit: &WitUnit,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
) -> Result<(WitType, Option<Unsupported>), WitModelError> {
    let (ty, unsupported) = match ty {
        Type::Bool => (WitType::Bool, None),
        Type::S8 => (WitType::S8, None),
        Type::S16 => (WitType::S16, None),
        Type::S32 => (WitType::S32, None),
        Type::S64 => (WitType::S64, None),
        Type::U8 => (WitType::U8, None),
        Type::U16 => (WitType::U16, None),
        Type::U32 => (WitType::U32, None),
        Type::U64 => (WitType::U64, None),
        Type::F32 => (WitType::F32, None),
        Type::F64 => (WitType::F64, None),
        Type::Char => (WitType::Char, None),
        Type::String => (WitType::String, None),
        Type::ErrorContext => (
            WitType::Unsupported(Unsupported::ErrorContext),
            Some(Unsupported::ErrorContext),
        ),
        Type::Id(type_id) => {
            let definition = &resolve.types[type_id];
            if matches!(definition.owner, TypeOwner::World(_))
                && !type_references.contains_key(&type_id)
            {
                return Ok((
                    WitType::Unsupported(Unsupported::WorldType),
                    Some(Unsupported::WorldType),
                ));
            }
            match &definition.kind {
                TypeDefKind::Handle(handle) => {
                    return convert_handle(
                        *handle,
                        resolve,
                        unit,
                        interface_indexes,
                        type_references,
                        unsupported_types,
                    );
                }
                TypeDefKind::Resource => {
                    let reference =
                        type_reference(type_id, resolve, unit, interface_indexes, type_references)?;
                    (WitType::Own(reference), None)
                }
                TypeDefKind::Option(inner) => {
                    let (inner, unsupported) = convert_type(
                        *inner,
                        resolve,
                        unit,
                        interface_indexes,
                        type_references,
                        unsupported_types,
                    )?;
                    (WitType::Option(Box::new(inner)), unsupported)
                }
                TypeDefKind::Result(result) => {
                    let (ok, ok_unsupported) = result
                        .ok
                        .map(|inner| {
                            convert_type(
                                inner,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )
                        })
                        .transpose()?
                        .map_or((None, None), |(ty, unsupported)| {
                            (Some(Box::new(ty)), unsupported)
                        });
                    let (err, err_unsupported) = result
                        .err
                        .map(|inner| {
                            convert_type(
                                inner,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )
                        })
                        .transpose()?
                        .map_or((None, None), |(ty, unsupported)| {
                            (Some(Box::new(ty)), unsupported)
                        });
                    (
                        WitType::Result { ok, err },
                        first_unsupported(ok_unsupported, err_unsupported),
                    )
                }
                TypeDefKind::Tuple(tuple) => {
                    let mut unsupported = None;
                    let types = tuple
                        .types
                        .iter()
                        .map(|inner| {
                            let (inner, found) = convert_type(
                                *inner,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )?;
                            unsupported = first_unsupported(unsupported, found);
                            Ok(inner)
                        })
                        .collect::<Result<Vec<_>, WitModelError>>()?;
                    (WitType::Tuple(types), unsupported)
                }
                TypeDefKind::List(inner) => {
                    let (inner, unsupported) = convert_type(
                        *inner,
                        resolve,
                        unit,
                        interface_indexes,
                        type_references,
                        unsupported_types,
                    )?;
                    (WitType::List(Box::new(inner)), unsupported)
                }
                TypeDefKind::Map(_, _) => (
                    WitType::Unsupported(Unsupported::Map),
                    Some(Unsupported::Map),
                ),
                TypeDefKind::FixedLengthList(_, _) => (
                    WitType::Unsupported(Unsupported::FixedList),
                    Some(Unsupported::FixedList),
                ),
                TypeDefKind::Future(inner) => {
                    let (inner, inner_unsupported) = inner
                        .map(|inner| {
                            convert_type(
                                inner,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )
                        })
                        .transpose()?
                        .map_or((None, None), |(ty, unsupported)| {
                            (Some(Box::new(ty)), unsupported)
                        });
                    (
                        WitType::Future(inner),
                        first_unsupported(Some(Unsupported::Future), inner_unsupported),
                    )
                }
                TypeDefKind::Stream(inner) => {
                    let (inner, inner_unsupported) = inner
                        .map(|inner| {
                            convert_type(
                                inner,
                                resolve,
                                unit,
                                interface_indexes,
                                type_references,
                                unsupported_types,
                            )
                        })
                        .transpose()?
                        .map_or((None, None), |(ty, unsupported)| {
                            (Some(Box::new(ty)), unsupported)
                        });
                    (
                        WitType::Stream(inner),
                        first_unsupported(Some(Unsupported::Stream), inner_unsupported),
                    )
                }
                TypeDefKind::Unknown => {
                    return Err(WitModelError::InvalidType(
                        "unknown type remains after resolution".to_owned(),
                    ));
                }
                _ => (
                    WitType::Named(type_reference(
                        type_id,
                        resolve,
                        unit,
                        interface_indexes,
                        type_references,
                    )?),
                    unsupported_types.get(&type_id).copied().flatten(),
                ),
            }
        }
    };
    Ok((ty, unsupported))
}

fn convert_handle(
    handle: Handle,
    resolve: &Resolve,
    unit: &WitUnit,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
) -> Result<(WitType, Option<Unsupported>), WitModelError> {
    let (type_id, owned) = match handle {
        Handle::Own(type_id) => (type_id, true),
        Handle::Borrow(type_id) => (type_id, false),
    };
    if matches!(resolve.types[type_id].owner, TypeOwner::World(_))
        && !type_references.contains_key(&type_id)
    {
        return Ok((
            WitType::Unsupported(Unsupported::WorldType),
            Some(Unsupported::WorldType),
        ));
    }
    let reference = type_reference(type_id, resolve, unit, interface_indexes, type_references)?;
    Ok((
        if owned {
            WitType::Own(reference)
        } else {
            WitType::Borrow(reference)
        },
        unsupported_types.get(&type_id).copied().flatten(),
    ))
}

fn type_reference(
    type_id: TypeId,
    resolve: &Resolve,
    unit: &WitUnit,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
) -> Result<WitTypeRef, WitModelError> {
    if let Some(reference) = type_references.get(&type_id) {
        return Ok(reference.clone());
    }
    let definition = &resolve.types[type_id];
    let TypeOwner::Interface(owner) = definition.owner else {
        return Err(WitModelError::MissingItem);
    };
    let interface = *interface_indexes
        .get(&owner)
        .ok_or(WitModelError::MissingInterface)?;
    let parser_interface = &resolve.interfaces[owner];
    let name = definition
        .name
        .as_deref()
        .ok_or(WitModelError::MissingItem)?;
    let interface_name = parser_interface
        .name
        .as_deref()
        .ok_or(WitModelError::MissingInterface)?;
    let item = parser_interface
        .types
        .keys()
        .position(|candidate| candidate == name)
        .ok_or(WitModelError::MissingItem)?;
    Ok(WitTypeRef {
        interface: WitIfaceRef {
            unit: unit.clone(),
            interface,
            name: interface_name.to_owned(),
        },
        item: item_index(item)?,
        item_name: name.to_owned(),
    })
}

fn build_type_references(
    resolve: &Resolve,
    package_units: &HashMap<ParserPackageId, WitUnit>,
) -> Result<HashMap<TypeId, WitTypeRef>, WitModelError> {
    let mut references = HashMap::new();
    for (package_id, package) in resolve.packages.iter() {
        let Some(unit) = package_units.get(&package_id).cloned() else {
            continue;
        };
        for (interface_position, (interface_name, interface_id)) in
            package.interfaces.iter().enumerate()
        {
            let interface_index = interface_index(interface_position)?;
            let interface = &resolve.interfaces[*interface_id];
            for (item_position, (item_name, type_id)) in interface.types.iter().enumerate() {
                let definition = &resolve.types[*type_id];
                if definition.owner == TypeOwner::Interface(*interface_id) {
                    references.entry(*type_id).or_insert(WitTypeRef {
                        interface: WitIfaceRef {
                            unit: unit.clone(),
                            interface: interface_index,
                            name: interface_name.clone(),
                        },
                        item: item_index(item_position)?,
                        item_name: item_name.clone(),
                    });
                }
            }
        }
    }
    Ok(references)
}

fn build_generated_world_type_aliases(
    resolve: &Resolve,
    package_id: ParserPackageId,
    unit: &WitUnit,
    package_units: &HashMap<ParserPackageId, WitUnit>,
    type_references: &HashMap<TypeId, WitTypeRef>,
) -> Result<HashMap<TypeId, WitTypeRef>, WitModelError> {
    if !matches!(unit.kind(), WitUnitKind::Generated { .. }) {
        return Ok(HashMap::new());
    }

    let package = resolve
        .packages
        .get(package_id)
        .ok_or(WitModelError::MissingItem)?;
    let mut aliases = HashMap::new();
    for world_id in package.worlds.values() {
        let world = resolve
            .worlds
            .get(*world_id)
            .ok_or(WitModelError::MissingWorld)?;
        for item in world.imports.values().chain(world.exports.values()) {
            let WorldItem::Type { id, .. } = item else {
                continue;
            };
            let alias = resolve.types.get(*id).ok_or(WitModelError::MissingItem)?;
            if alias.owner != TypeOwner::World(*world_id) {
                continue;
            }
            let TypeDefKind::Type(Type::Id(target_id)) = &alias.kind else {
                continue;
            };
            let target = resolve
                .types
                .get(*target_id)
                .ok_or(WitModelError::MissingItem)?;
            let TypeOwner::Interface(target_interface) = target.owner else {
                continue;
            };
            let Some(target_package_id) = resolve.interfaces[target_interface].package else {
                continue;
            };
            let Some(reference) = type_references.get(target_id) else {
                continue;
            };
            if target_package_id == package_id {
                aliases.entry(*id).or_insert_with(|| reference.clone());
                continue;
            }
            let Some(selected_unit) = package_units.get(&target_package_id) else {
                continue;
            };
            if !matches!(
                &target.kind,
                TypeDefKind::Type(_)
                    | TypeDefKind::Record(_)
                    | TypeDefKind::Variant(_)
                    | TypeDefKind::Enum(_)
                    | TypeDefKind::Flags(_)
                    | TypeDefKind::Resource
            ) || !type_reference_matches_interface_item(
                resolve,
                target_package_id,
                target_interface,
                *target_id,
                selected_unit,
                reference,
            ) {
                continue;
            }
            aliases.entry(*id).or_insert_with(|| reference.clone());
        }
    }
    Ok(aliases)
}

fn type_reference_matches_interface_item(
    resolve: &Resolve,
    package_id: ParserPackageId,
    interface_id: InterfaceId,
    type_id: TypeId,
    unit: &WitUnit,
    reference: &WitTypeRef,
) -> bool {
    if &reference.interface.unit != unit {
        return false;
    }
    let Some(package) = resolve.packages.get(package_id) else {
        return false;
    };
    let Some((interface_position, interface_name)) = package
        .interfaces
        .iter()
        .enumerate()
        .find(|(_, (_, found_interface))| **found_interface == interface_id)
        .map(|(index, (name, _))| (index, name))
    else {
        return false;
    };
    let Some(expected_interface_index) = u32::try_from(interface_position)
        .ok()
        .map(WitIfaceIdx::from_raw)
    else {
        return false;
    };
    if reference.interface.interface != expected_interface_index
        || reference.interface.name != interface_name.as_str()
    {
        return false;
    }
    let Some(interface) = resolve.interfaces.get(interface_id) else {
        return false;
    };
    let Some((item_position, item_name)) = interface
        .types
        .iter()
        .enumerate()
        .find(|(_, (_, found_type))| **found_type == type_id)
        .map(|(index, (name, _))| (index, name))
    else {
        return false;
    };
    let Some(expected_item_index) = u32::try_from(item_position).ok().map(WitItemIdx::from_raw)
    else {
        return false;
    };
    reference.item == expected_item_index && reference.item_name == item_name.as_str()
}

#[derive(Clone, Copy)]
enum UnsupportedEdge<T> {
    Type(T),
    Feature(Unsupported),
}

fn build_unsupported_types(resolve: &Resolve) -> HashMap<TypeId, Option<Unsupported>> {
    fn add_type(ty: Type, references: &mut Vec<UnsupportedEdge<TypeId>>) {
        match ty {
            Type::ErrorContext => {
                references.push(UnsupportedEdge::Feature(Unsupported::ErrorContext))
            }
            Type::Id(type_id) => references.push(UnsupportedEdge::Type(type_id)),
            _ => {}
        }
    }

    let type_ids = resolve
        .types
        .iter()
        .map(|(type_id, _)| type_id)
        .collect::<Vec<_>>();
    let mut dependencies = HashMap::<TypeId, Vec<UnsupportedEdge<TypeId>>>::new();
    let mut local_results = HashMap::<TypeId, Option<Unsupported>>::new();
    for type_id in type_ids.iter().copied() {
        let definition = &resolve.types[type_id];
        let mut references = Vec::new();
        let unsupported = match &definition.kind {
            TypeDefKind::Map(key, value) => {
                add_type(*key, &mut references);
                add_type(*value, &mut references);
                Some(Unsupported::Map)
            }
            TypeDefKind::FixedLengthList(element, _) => {
                add_type(*element, &mut references);
                Some(Unsupported::FixedList)
            }
            TypeDefKind::Future(element) => {
                if let Some(element) = element {
                    add_type(*element, &mut references);
                }
                Some(Unsupported::Future)
            }
            TypeDefKind::Stream(element) => {
                if let Some(element) = element {
                    add_type(*element, &mut references);
                }
                Some(Unsupported::Stream)
            }
            TypeDefKind::Record(record) => {
                for field in &record.fields {
                    add_type(field.ty, &mut references);
                }
                None
            }
            TypeDefKind::Variant(variant) => {
                for case in &variant.cases {
                    if let Some(ty) = case.ty {
                        add_type(ty, &mut references);
                    }
                }
                None
            }
            TypeDefKind::Type(ty) | TypeDefKind::List(ty) | TypeDefKind::Option(ty) => {
                add_type(*ty, &mut references);
                None
            }
            TypeDefKind::Result(result) => {
                if let Some(ty) = result.ok {
                    add_type(ty, &mut references);
                }
                if let Some(ty) = result.err {
                    add_type(ty, &mut references);
                }
                None
            }
            TypeDefKind::Tuple(tuple) => {
                for ty in &tuple.types {
                    add_type(*ty, &mut references);
                }
                None
            }
            TypeDefKind::Handle(_)
            | TypeDefKind::Resource
            | TypeDefKind::Flags(_)
            | TypeDefKind::Enum(_)
            | TypeDefKind::Unknown => None,
        };
        local_results.insert(type_id, unsupported);
        dependencies.insert(type_id, references);
    }

    resolve_transitive_unsupported(&type_ids, &dependencies, &local_results)
}

fn resolve_transitive_unsupported<T>(
    ordered_types: &[T],
    dependencies: &HashMap<T, Vec<UnsupportedEdge<T>>>,
    local_results: &HashMap<T, Option<Unsupported>>,
) -> HashMap<T, Option<Unsupported>>
where
    T: Copy + Eq + std::hash::Hash,
{
    let known_types = ordered_types.iter().copied().collect::<HashSet<_>>();
    let mut remaining_dependencies = HashMap::with_capacity(ordered_types.len());
    let mut dependents = HashMap::<T, Vec<T>>::new();
    for type_id in ordered_types {
        let mut count = 0;
        if let Some(edges) = dependencies.get(type_id) {
            for edge in edges {
                match edge {
                    UnsupportedEdge::Type(dependency) if known_types.contains(dependency) => {
                        count += 1;
                        dependents.entry(*dependency).or_default().push(*type_id);
                    }
                    UnsupportedEdge::Type(_) | UnsupportedEdge::Feature(_) => {}
                }
            }
        }
        remaining_dependencies.insert(*type_id, count);
    }

    let mut ready = ordered_types
        .iter()
        .filter(|type_id| remaining_dependencies.get(type_id) == Some(&0))
        .copied()
        .collect::<VecDeque<_>>();
    let mut results = HashMap::with_capacity(ordered_types.len());
    while let Some(type_id) = ready.pop_front() {
        let result = local_results.get(&type_id).copied().flatten().or_else(|| {
            dependencies
                .get(&type_id)
                .into_iter()
                .flatten()
                .find_map(|edge| match edge {
                    UnsupportedEdge::Type(dependency) => results.get(dependency).copied().flatten(),
                    UnsupportedEdge::Feature(feature) => Some(*feature),
                })
        });
        results.insert(type_id, result);

        for dependent in dependents.get(&type_id).into_iter().flatten() {
            let remaining = remaining_dependencies
                .get_mut(dependent)
                .expect("dependent type is in the graph");
            *remaining -= 1;
            if *remaining == 0 {
                ready.push_back(*dependent);
            }
        }
    }

    // Parser-valid WIT graphs are acyclic. Retain deterministic behavior for
    // defensive callers and cyclic graph regressions by doing a per-root DFS
    // with a visited set when a cycle is present.
    if results.len() != ordered_types.len() {
        for root in ordered_types {
            let mut stack = vec![UnsupportedEdge::Type(*root)];
            let mut visited = HashSet::new();
            let mut result = None;
            while let Some(edge) = stack.pop() {
                match edge {
                    UnsupportedEdge::Feature(feature) => {
                        result = Some(feature);
                        break;
                    }
                    UnsupportedEdge::Type(type_id) => {
                        if !visited.insert(type_id) {
                            continue;
                        }
                        match local_results.get(&type_id).copied().flatten() {
                            Some(feature) => {
                                result = Some(feature);
                                break;
                            }
                            None => {}
                        }
                        if let Some(edges) = dependencies.get(&type_id) {
                            stack.extend(edges.iter().rev().copied());
                        }
                    }
                }
            }
            results.insert(*root, result);
        }
    }
    results
}

fn function_model(
    function: &Function,
    resolve: &Resolve,
    unit: &WitUnit,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
    named_result_function_spans: &HashSet<(u32, u32)>,
) -> Result<(WitFunc, Option<Unsupported>), WitModelError> {
    let named_results = function.span.is_known()
        && named_result_function_spans.contains(&(function.span.start(), function.span.end()));
    let is_async = function.kind.is_async();
    let mut unsupported = is_async.then_some(Unsupported::Async);
    let params = function
        .params
        .iter()
        .map(|param| {
            let (ty, found) = convert_type(
                param.ty,
                resolve,
                unit,
                interface_indexes,
                type_references,
                unsupported_types,
            )?;
            unsupported = first_unsupported(unsupported, found);
            Ok((param.name.clone(), ty))
        })
        .collect::<Result<Vec<_>, WitModelError>>()?;
    let result = if named_results {
        unsupported = first_unsupported(unsupported, Some(Unsupported::NamedResults));
        None
    } else {
        function
            .result
            .map(|ty| {
                let (ty, found) = convert_type(
                    ty,
                    resolve,
                    unit,
                    interface_indexes,
                    type_references,
                    unsupported_types,
                )?;
                unsupported = first_unsupported(unsupported, found);
                Ok(ty)
            })
            .transpose()?
    };
    Ok((
        WitFunc {
            params,
            result,
            is_async,
        },
        unsupported,
    ))
}

fn resource_member_name(function: &Function) -> Result<(WitMemberKind, String), WitModelError> {
    let kind = match function.kind {
        FunctionKind::Constructor(_) => {
            return Ok((WitMemberKind::Constructor, "constructor".to_owned()));
        }
        FunctionKind::Method(_) | FunctionKind::AsyncMethod(_) => WitMemberKind::Method,
        FunctionKind::Static(_) | FunctionKind::AsyncStatic(_) => WitMemberKind::Static,
        FunctionKind::Freestanding | FunctionKind::AsyncFreestanding => {
            return Err(WitModelError::InvalidType(
                "freestanding function was grouped as a resource member".to_owned(),
            ));
        }
    };
    let name = function
        .name
        .split_once(']')
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.rsplit_once('.').map(|(_, name)| name))
        .ok_or_else(|| WitModelError::InvalidType(function.name.clone()))?;
    Ok((kind, name.to_owned()))
}

#[allow(clippy::too_many_arguments)]
fn world_items(
    items: &wit_parser::IndexMap<WorldKey, WorldItem>,
    resolve: &Resolve,
    unit: &WitUnit,
    package_units: &HashMap<ParserPackageId, WitUnit>,
    interface_indexes: &HashMap<InterfaceId, WitIfaceIdx>,
    type_references: &HashMap<TypeId, WitTypeRef>,
    generated_world_type_aliases: &HashMap<TypeId, WitTypeRef>,
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
    source_mappings: &[(u32, crate::WitSourceMapping)],
    direct_items: Option<&SourceWorldItems>,
    direction: WitWorldItemDirection,
    named_result_function_spans: &HashSet<(u32, u32)>,
    unsupported: &mut Option<Unsupported>,
) -> Result<Vec<(WitWorldItem, Option<u32>)>, WitModelError> {
    let mut result = Vec::with_capacity(items.len());
    for (key, parser_item) in items {
        match parser_item {
            WorldItem::Interface {
                id,
                span,
                stability: item_stability,
                ..
            } => {
                let interface = &resolve.interfaces[*id];
                if interface.name.is_none() {
                    *unsupported =
                        first_unsupported(*unsupported, Some(Unsupported::InlineInterface));
                    continue;
                }
                let (since, deprecated) = stability(item_stability)?;
                let reference = interface_reference(resolve, *id, package_units)?;
                let source_key = SourceWorldItemKey::Interface {
                    unit: reference.unit.canonical_name().to_owned(),
                    name: reference.name.clone(),
                };
                let item_span = source_world_item_span(direct_items, direction, &source_key)
                    .or_else(|| source_span(*span, unit, source_mappings));
                result.push((
                    WitWorldItem::Interface {
                        reference,
                        docs: None,
                        since,
                        deprecated,
                        span: item_span,
                    },
                    span_start(*span),
                ));
            }
            WorldItem::Function(function) => {
                let name = match key {
                    WorldKey::Name(name) => name.clone(),
                    WorldKey::Interface(_) => {
                        *unsupported =
                            first_unsupported(*unsupported, Some(Unsupported::InlineInterface));
                        continue;
                    }
                };
                let source_key = SourceWorldItemKey::Function(name.clone());
                let source_start = span_start(function.span);
                let item_span = source_world_item_span(direct_items, direction, &source_key)
                    .or_else(|| source_span(function.span, unit, source_mappings));
                let item_docs = docs(&function.docs.contents);
                let (since, deprecated) = stability(&function.stability)?;
                let (function, found) = function_model(
                    function,
                    resolve,
                    unit,
                    interface_indexes,
                    type_references,
                    unsupported_types,
                    named_result_function_spans,
                )?;
                result.push((
                    WitWorldItem::Func {
                        name,
                        docs: item_docs,
                        func: function,
                        unsupported: found,
                        since,
                        deprecated,
                        span: item_span,
                    },
                    source_start,
                ));
            }
            WorldItem::Type { id, .. } => {
                if !generated_world_type_aliases.contains_key(id) {
                    *unsupported = first_unsupported(*unsupported, Some(Unsupported::WorldType));
                }
            }
        }
    }
    Ok(result)
}

fn docs(contents: &Option<String>) -> Option<std::sync::Arc<str>> {
    contents.as_deref().map(std::sync::Arc::from)
}

fn interface_reference(
    resolve: &Resolve,
    interface_id: InterfaceId,
    package_units: &HashMap<ParserPackageId, WitUnit>,
) -> Result<WitIfaceRef, WitModelError> {
    let parser_interface = &resolve.interfaces[interface_id];
    let package_id = parser_interface
        .package
        .ok_or(WitModelError::MissingInterface)?;
    let unit = package_units
        .get(&package_id)
        .ok_or(WitModelError::MissingInterface)?
        .clone();
    let raw_interface_index = resolve.packages[package_id]
        .interfaces
        .values()
        .position(|candidate| *candidate == interface_id)
        .ok_or(WitModelError::MissingInterface)?;
    Ok(WitIfaceRef {
        unit,
        interface: interface_index(raw_interface_index)?,
        name: parser_interface
            .name
            .clone()
            .ok_or(WitModelError::MissingInterface)?,
    })
}

fn world_reference(
    resolve: &Resolve,
    world_id: WorldId,
    package_units: &HashMap<ParserPackageId, WitUnit>,
) -> Result<WitWorldRef, WitModelError> {
    let parser_world = &resolve.worlds[world_id];
    let package_id = parser_world.package.ok_or(WitModelError::MissingWorld)?;
    let unit = package_units
        .get(&package_id)
        .ok_or(WitModelError::MissingWorld)?
        .clone();
    let raw_world_index = resolve.packages[package_id]
        .worlds
        .values()
        .position(|candidate| *candidate == world_id)
        .ok_or(WitModelError::MissingWorld)?;
    Ok(WitWorldRef {
        unit,
        world: world_index(raw_world_index)?,
        name: parser_world.name.clone(),
        names: Vec::new(),
        docs: None,
        since: None,
        deprecated: None,
        span: None,
    })
}

fn world_reference_named(
    resolve: &Resolve,
    package_units: &HashMap<ParserPackageId, WitUnit>,
    package_name: &str,
    world_name: &str,
) -> Result<WitWorldRef, WitModelError> {
    let (package_id, _) = package_units
        .iter()
        .find(|(_, unit)| unit.canonical_name() == package_name)
        .ok_or(WitModelError::MissingWorld)?;
    let world_id = resolve.packages[*package_id]
        .worlds
        .get(world_name)
        .copied()
        .ok_or(WitModelError::MissingWorld)?;
    world_reference(resolve, world_id, package_units)
}

fn span_start(span: Span) -> Option<u32> {
    span.is_known().then(|| span.start())
}

fn stability(
    stability: &Stability,
) -> Result<(Option<WitStabilityVersion>, Option<WitStabilityVersion>), WitModelError> {
    let (since, deprecated) = match stability {
        Stability::Unknown => (None, None),
        Stability::Unstable { deprecated, .. } => (None, deprecated.as_ref()),
        Stability::Stable { since, deprecated } => (Some(since), deprecated.as_ref()),
    };
    fn convert<T: ToString>(
        version: Option<&T>,
    ) -> Result<Option<WitStabilityVersion>, WitModelError> {
        version
            .map(|version| {
                WitStabilityVersion::parse(&version.to_string())
                    .map_err(|_| WitModelError::InvalidVersion(version.to_string()))
            })
            .transpose()
    }
    Ok((convert(since)?, convert(deprecated)?))
}

fn source_span(
    span: Span,
    unit: &WitUnit,
    source_mappings: &[(u32, crate::WitSourceMapping)],
) -> Option<WitSpan> {
    if !matches!(unit.kind(), WitUnitKind::Dir { .. }) || !span.is_known() {
        return None;
    }
    let start = span.start();
    let end = span.end();
    source_mappings.iter().find_map(|(base, source)| {
        let source_start = base.checked_add(source.offset)?;
        let source_end = source_start.checked_add(u32::try_from(source.parser_len).ok()?)?;
        if start < source_start || start > source_end || end < start || end > source_end {
            return None;
        }
        let parser_start = usize::try_from(start - source_start).ok()?;
        let parser_end = usize::try_from(end - source_start).ok()?;
        Some(WitSpan {
            path: source.path.clone(),
            range: source.parser_to_source[parser_start]..source.parser_to_source[parser_end],
        })
    })
}

fn source_function_declaration(
    span: Span,
    interface_name: &str,
    name: &str,
    unit: &WitUnit,
    source_mappings: &[(u32, crate::WitSourceMapping)],
) -> Option<(WitSpan, Vec<(String, String)>)> {
    let name_span = source_span(span, unit, source_mappings)?;
    source_mappings.iter().find_map(|(_, source)| {
        source
            .function_declarations
            .iter()
            .find(|declaration| {
                declaration.interface_name == interface_name
                    && declaration.name == name
                    && declaration.name_range == name_span.range
                    && source.path == name_span.path
            })
            .map(|declaration| {
                (
                    WitSpan {
                        path: source.path.clone(),
                        range: declaration.declaration_range.clone(),
                    },
                    declaration.token_pairs.clone(),
                )
            })
    })
}

fn source_interface_resource_declaration(
    span: Span,
    interface_name: &str,
    name: &str,
    unit: &WitUnit,
    source_mappings: &[(u32, crate::WitSourceMapping)],
) -> Option<(WitSpan, Vec<(String, String)>)> {
    let name_span = source_span(span, unit, source_mappings)?;
    let mut matches = source_mappings
        .iter()
        .filter(|(_, source)| source.path == name_span.path)
        .flat_map(|(_, source)| {
            source
                .resource_declarations
                .iter()
                .filter(|declaration| {
                    declaration.interface_name == interface_name
                        && declaration.name == name
                        && declaration.name_range == name_span.range
                })
                .map(|declaration| {
                    (
                        WitSpan {
                            path: source.path.clone(),
                            range: declaration.declaration_range.clone(),
                        },
                        declaration.token_pairs.clone(),
                    )
                })
        });
    let declaration = matches.next()?;
    matches.next().is_none().then_some(declaration)
}

fn first_unsupported(
    earlier: Option<Unsupported>,
    later: Option<Unsupported>,
) -> Option<Unsupported> {
    earlier.or(later)
}

fn interface_index(index: usize) -> Result<WitIfaceIdx, WitModelError> {
    u32::try_from(index)
        .map(WitIfaceIdx::from_raw)
        .map_err(|_| WitModelError::TooManyDefinitions)
}

fn item_index(index: usize) -> Result<WitItemIdx, WitModelError> {
    u32::try_from(index)
        .map(WitItemIdx::from_raw)
        .map_err(|_| WitModelError::TooManyDefinitions)
}

fn world_index(index: usize) -> Result<WitWorldIdx, WitModelError> {
    u32::try_from(index)
        .map(WitWorldIdx::from_raw)
        .map_err(|_| WitModelError::TooManyDefinitions)
}

#[cfg(test)]
mod tests {
    use super::{
        UnsupportedEdge, WitModelError, WitResolveContext, build_generated_world_type_aliases,
        build_type_references, build_unsupported_types, convert_type, interface_index,
        resolve_transitive_unsupported,
    };
    use crate::{
        Unsupported, WitIfaceIdx, WitItemIdx, WitItemKind, WitPackage, WitPackageDeclaration,
        WitSourceFile, WitSourceLocation, WitStabilityVersion, WitType, WitUnit, WitWorldIdx,
        WitWorldItem, WitWorldItemDirection, parse_source_files, print_wit,
    };
    use sai_intern::{Interner, PackageInterner};
    use sai_package::{mvs::MvsPackageVersion, version::Version};
    use sai_stable_hash::StableHash;
    use std::collections::HashMap;
    use wit_parser::{Resolve, Type};

    fn dir_unit() -> WitUnit {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:graphics#1")
            .expect("valid package key");
        let version = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid test package version");
        WitUnit::dir(package, &packages, &version).expect("package ID matches package version")
    }

    fn dir_unit_for(identity: &str, package_key: &str, version: &str) -> WitUnit {
        let packages = PackageInterner::new();
        let package = packages.intern(package_key).expect("valid package key");
        let version = MvsPackageVersion::new(
            identity,
            Version::parse(version).expect("valid test version"),
        )
        .expect("valid package version");
        WitUnit::dir(package, &packages, &version).expect("package ID matches package version")
    }

    fn generated_unit() -> WitUnit {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:generated#1")
            .expect("valid package key");
        let version = MvsPackageVersion::new(
            "acme:generated",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid test package version");
        WitUnit::generated(package, &packages, &version)
            .expect("package ID matches package version")
    }

    fn declaration_names(package: &WitPackage) -> Vec<(char, String)> {
        package
            .declarations_in_order()
            .into_iter()
            .map(|declaration| match declaration {
                WitPackageDeclaration::Interface(index) => {
                    ('i', package.interfaces[index].name.clone())
                }
                WitPackageDeclaration::World(index) => ('w', package.worlds[index].name.clone()),
            })
            .collect()
    }

    #[test]
    fn package_declaration_order_interleaves_directory_interfaces_and_worlds() {
        let parsed = parse_source_files(
            dir_unit(),
            [
                WitSourceFile::new(
                    "z-last.wit",
                    "package acme:graphics@1.2.3;\nworld late {} interface final {}",
                ),
                WitSourceFile::new(
                    "a-first.wit",
                    "package acme:graphics@1.2.3;\ninterface early {} world middle {}",
                ),
            ],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");

        assert_eq!(
            declaration_names(&package),
            [
                ('i', "early".to_owned()),
                ('w', "middle".to_owned()),
                ('w', "late".to_owned()),
                ('i', "final".to_owned()),
            ]
        );
        assert!(
            package
                .declaration_order
                .iter()
                .all(|declaration| match declaration {
                    WitPackageDeclaration::Interface(index) => {
                        package.interfaces[*index].span.is_some()
                    }
                    WitPackageDeclaration::World(index) => package.worlds[*index].span.is_some(),
                }),
            "directory package declarations retain their source spans"
        );
    }

    #[test]
    fn package_declaration_order_uses_wasi_source_file_order_without_public_spans() {
        let symbols = Interner::new();
        let package = symbols.intern("http").expect("WASI package symbol interns");
        let version =
            Version::parse(crate::EMBEDDED_WASI_VERSION).expect("embedded WASI version is valid");
        let unit = WitUnit::wasi(package, &symbols, &version)
            .expect("HTTP belongs to the embedded WASI catalog");
        let loaded = crate::load_embedded_wasi_package(&unit, &symbols)
            .expect("embedded HTTP package and its dependencies load");
        let package = loaded.root();

        assert_eq!(
            declaration_names(package),
            [
                ('i', "incoming-handler".to_owned()),
                ('i', "outgoing-handler".to_owned()),
                ('w', "imports".to_owned()),
                ('w', "proxy".to_owned()),
                ('i', "types".to_owned()),
            ]
        );
        assert!(
            package
                .interfaces
                .iter()
                .all(|interface| interface.span.is_none())
                && package.worlds.iter().all(|world| world.span.is_none()),
            "embedded WASI declaration ordering does not expose public source spans"
        );
    }

    #[test]
    fn generated_package_declaration_order_is_interfaces_then_worlds() {
        let parsed = parse_source_files(
            generated_unit(),
            [WitSourceFile::new(
                "generated.wit",
                "package acme:generated@1.2.3;\nworld zeta {} interface omega {} world alpha {} interface beta {}",
            )],
        )
        .expect("valid generated WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("generated package resolves");

        let expected = package
            .interfaces
            .iter_enumerated()
            .map(|(index, _)| WitPackageDeclaration::Interface(index))
            .chain(
                package
                    .worlds
                    .iter_enumerated()
                    .map(|(index, _)| WitPackageDeclaration::World(index)),
            )
            .collect::<Vec<_>>();
        assert_eq!(package.declaration_order, expected);
        assert!(
            package
                .interfaces
                .iter()
                .all(|interface| interface.span.is_none())
        );
        assert!(package.worlds.iter().all(|world| world.span.is_none()));
    }

    #[test]
    fn generated_world_use_aliases_resolve_to_package_interface_types() {
        let unit = generated_unit();
        let parsed = parse_source_files(
            unit.clone(),
            [WitSourceFile::new(
                "generated.wit",
                "package acme:generated@1.2.3;\ninterface api { type id = u32; }\nworld w { use api.{id}; import f: func(x: id) -> id; }",
            )],
        )
        .expect("generated WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("generated package resolves");

        let world = &package.worlds[WitWorldIdx::from_raw(0)];
        assert_eq!(world.unsupported, None);
        let WitWorldItem::Func {
            func, unsupported, ..
        } = world
            .imports
            .iter()
            .find(|item| matches!(item, WitWorldItem::Func { .. }))
            .expect("world function is retained after its generated use alias")
        else {
            unreachable!("the selected world item is a function");
        };
        assert_eq!(*unsupported, None);
        let expected = |item: &str| {
            WitType::Named(crate::WitTypeRef {
                interface: crate::WitIfaceRef {
                    unit: unit.clone(),
                    interface: WitIfaceIdx::from_raw(0),
                    name: "api".to_owned(),
                },
                item: WitItemIdx::from_raw(0),
                item_name: item.to_owned(),
            })
        };
        assert_eq!(func.params, [("x".to_owned(), expected("id"))]);
        assert_eq!(func.result, Some(expected("id")));
    }

    #[test]
    fn generated_world_use_aliases_resolve_to_selected_external_types() {
        let generated = generated_unit();
        let dependency = dir_unit_for("acme:dependency", "acme:dependency#1", "1.4.12");
        let dependency_source =
            "package acme:dependency@1.4.12;\ninterface api { type data = u32; }";
        let generated_source = concat!(
            "package acme:generated@1.2.3;\n",
            "world w {\n",
            "  use acme:dependency/api@1.4.12.{data};\n",
            "  import f: func(x: data) -> data;\n",
            "}\n",
        );

        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        context
            .push_package(
                &mut resolve,
                parse_source_files(
                    dependency.clone(),
                    [WitSourceFile::new("dependency.wit", dependency_source)],
                )
                .expect("selected dependency WIT parses"),
            )
            .expect("selected dependency package resolves");
        let package = context
            .push_package(
                &mut resolve,
                parse_source_files(
                    generated.clone(),
                    [WitSourceFile::new("generated.wit", generated_source)],
                )
                .expect("generated WIT parses"),
            )
            .expect("generated package resolves through its selected dependency");

        let generated_package_id = context
            .package_units
            .iter()
            .find_map(|(package_id, candidate)| (candidate == &generated).then_some(*package_id))
            .expect("generated parser package retains its exact unit");
        let dependency_package_id = context
            .package_units
            .iter()
            .find_map(|(package_id, candidate)| (candidate == &dependency).then_some(*package_id))
            .expect("dependency parser package retains its exact unit");
        let dependency_interface_id = *resolve.packages[dependency_package_id]
            .interfaces
            .get("api")
            .expect("dependency interface is resolved");
        let dependency_type_id = *resolve.interfaces[dependency_interface_id]
            .types
            .get("data")
            .expect("dependency named type is resolved");
        let mut mismatched_references =
            build_type_references(&resolve, &context.package_units).expect("references build");
        mismatched_references
            .get_mut(&dependency_type_id)
            .expect("selected dependency type has a reference")
            .interface
            .unit = generated.clone();
        assert!(
            build_generated_world_type_aliases(
                &resolve,
                generated_package_id,
                &generated,
                &context.package_units,
                &mismatched_references,
            )
            .expect("mismatched external target is rejected")
            .is_empty(),
            "the alias must not accept a reference from a different selected unit"
        );

        let world = &package.worlds[WitWorldIdx::from_raw(0)];
        assert_eq!(world.unsupported, None);
        let WitWorldItem::Func {
            func, unsupported, ..
        } = world
            .imports
            .iter()
            .find(|item| matches!(item, WitWorldItem::Func { .. }))
            .expect("the world function is retained")
        else {
            unreachable!("the selected world item is a function");
        };
        assert_eq!(*unsupported, None);
        let expected = crate::WitTypeRef {
            interface: crate::WitIfaceRef {
                unit: dependency.clone(),
                interface: WitIfaceIdx::from_raw(0),
                name: "api".to_owned(),
            },
            item: WitItemIdx::from_raw(0),
            item_name: "data".to_owned(),
        };
        assert_eq!(
            func.params,
            [("x".to_owned(), WitType::Named(expected.clone()))]
        );
        assert_eq!(func.result, Some(WitType::Named(expected)));

        let printed = print_wit(&package);
        assert!(printed.contains(
            "use acme:dependency/api@1.4.12.{data};\n\n  import f: func(x: data) -> data;"
        ));
        let mut roundtrip_context = WitResolveContext::default();
        let mut roundtrip_resolve = Resolve::default();
        roundtrip_context
            .push_package(
                &mut roundtrip_resolve,
                parse_source_files(
                    dependency,
                    [WitSourceFile::new("dependency.wit", dependency_source)],
                )
                .expect("selected dependency WIT reparses"),
            )
            .expect("selected dependency package resolves again");
        let roundtrip = roundtrip_context
            .push_package(
                &mut roundtrip_resolve,
                parse_source_files(
                    generated,
                    [WitSourceFile::new("generated.wit", printed.as_str())],
                )
                .expect("printed generated WIT reparses"),
            )
            .expect("round-trip generated package resolves");
        assert_eq!(print_wit(&roundtrip), printed);
    }

    #[test]
    fn generated_world_use_aliases_resolve_to_selected_external_resources() {
        let generated = generated_unit();
        let dependency = dir_unit_for("acme:dependency", "acme:dependency#1", "1.4.12");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        context
            .push_package(
                &mut resolve,
                parse_source_files(
                    dependency.clone(),
                    [WitSourceFile::new(
                        "dependency.wit",
                        "package acme:dependency@1.4.12;\ninterface api { resource widget; }",
                    )],
                )
                .expect("selected resource dependency WIT parses"),
            )
            .expect("selected resource dependency resolves");
        let package = context
            .push_package(
                &mut resolve,
                parse_source_files(
                    generated.clone(),
                    [WitSourceFile::new(
                        "generated.wit",
                        concat!(
                            "package acme:generated@1.2.3;\n",
                            "world w {\n",
                            "  use acme:dependency/api@1.4.12.{widget};\n",
                            "  import f: func(owned: own<widget>, borrowed: borrow<widget>) -> own<widget>;\n",
                            "}\n",
                        ),
                    )],
                )
                .expect("generated resource use parses"),
            )
            .expect("generated world with an external resource resolves");

        let world = &package.worlds[WitWorldIdx::from_raw(0)];
        assert_eq!(world.unsupported, None);
        let WitWorldItem::Func {
            func, unsupported, ..
        } = world
            .imports
            .iter()
            .find(|item| matches!(item, WitWorldItem::Func { .. }))
            .expect("the generated function resolves its external resource aliases")
        else {
            unreachable!("the selected world item is a function");
        };
        assert_eq!(*unsupported, None);
        let expected = crate::WitTypeRef {
            interface: crate::WitIfaceRef {
                unit: dependency.clone(),
                interface: WitIfaceIdx::from_raw(0),
                name: "api".to_owned(),
            },
            item: WitItemIdx::from_raw(0),
            item_name: "widget".to_owned(),
        };
        assert_eq!(
            func.params,
            [
                ("owned".to_owned(), WitType::Own(expected.clone())),
                ("borrowed".to_owned(), WitType::Borrow(expected.clone())),
            ]
        );
        assert_eq!(func.result, Some(WitType::Own(expected.clone())));
        let printed = print_wit(&package);
        assert!(
            printed.contains(
                "use acme:dependency/api@1.4.12.{widget};\n\n  import f: func(owned: widget, borrowed: borrow<widget>) -> widget;"
            ),
            "{printed}"
        );

        let mut roundtrip_context = WitResolveContext::default();
        let mut roundtrip_resolve = Resolve::default();
        roundtrip_context
            .push_package(
                &mut roundtrip_resolve,
                parse_source_files(
                    dependency,
                    [WitSourceFile::new(
                        "dependency.wit",
                        "package acme:dependency@1.4.12;\ninterface api { resource widget; }",
                    )],
                )
                .expect("selected resource dependency reparses"),
            )
            .expect("selected resource dependency resolves again");
        let roundtrip = roundtrip_context
            .push_package(
                &mut roundtrip_resolve,
                parse_source_files(
                    generated,
                    [WitSourceFile::new("generated.wit", printed.as_str())],
                )
                .expect("printed generated world reparses"),
            )
            .expect("round-trip generated world resolves");
        assert_eq!(print_wit(&roundtrip), printed);
        let roundtrip_world = &roundtrip.worlds[WitWorldIdx::from_raw(0)];
        let WitWorldItem::Func {
            func, unsupported, ..
        } = roundtrip_world
            .imports
            .iter()
            .find(|item| matches!(item, WitWorldItem::Func { .. }))
            .expect("the round-trip resource function remains present")
        else {
            unreachable!("the selected round-trip item is a function");
        };
        assert_eq!(*unsupported, None);
        assert_eq!(
            func.params,
            [
                ("owned".to_owned(), WitType::Own(expected.clone())),
                ("borrowed".to_owned(), WitType::Borrow(expected.clone())),
            ]
        );
        assert_eq!(func.result, Some(WitType::Own(expected)));
    }

    #[test]
    fn authored_world_use_aliases_remain_unsupported() {
        let unit = dir_unit_for("acme:generated", "acme:generated#1", "1.2.3");
        let parsed = parse_source_files(
            unit,
            [WitSourceFile::new(
                "authored.wit",
                "package acme:generated@1.2.3;\ninterface api { type id = u32; }\nworld w { use api.{id}; import f: func(x: id) -> id; }",
            )],
        )
        .expect("authored WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("authored package resolves");

        let world = &package.worlds[WitWorldIdx::from_raw(0)];
        assert_eq!(world.unsupported, Some(Unsupported::WorldType));
        let WitWorldItem::Func {
            func, unsupported, ..
        } = world
            .imports
            .iter()
            .find(|item| matches!(item, WitWorldItem::Func { .. }))
            .expect("world function is retained for unsupported-type reporting")
        else {
            unreachable!("the selected world item is a function");
        };
        assert_eq!(*unsupported, Some(Unsupported::WorldType));
        assert_eq!(
            func.params[0].1,
            WitType::Unsupported(Unsupported::WorldType)
        );
        assert_eq!(
            func.result,
            Some(WitType::Unsupported(Unsupported::WorldType))
        );
    }

    #[test]
    fn generated_world_alias_without_a_mapped_interface_target_stays_unsupported() {
        let unit = generated_unit();
        let parsed = parse_source_files(
            unit.clone(),
            [WitSourceFile::new(
                "generated.wit",
                "package acme:generated@1.2.3;\ninterface api { type id = u32; }\nworld w { use api.{id}; import f: func(x: id); }",
            )],
        )
        .expect("generated WIT parses");
        let mut resolve = Resolve::default();
        let package_id = resolve
            .push(parsed.group.main, 0)
            .expect("generated package resolves");
        let package_units = HashMap::from([(package_id, unit.clone())]);
        let package = &resolve.packages[package_id];
        let interface_id = *package
            .interfaces
            .values()
            .next()
            .expect("interface exists");
        let target_id = *resolve.interfaces[interface_id]
            .types
            .get("id")
            .expect("interface named type exists");
        let world_id = *package.worlds.values().next().expect("world exists");
        let alias_id = resolve.worlds[world_id]
            .imports
            .values()
            .find_map(|item| match item {
                wit_parser::WorldItem::Type { id, .. } => Some(*id),
                _ => None,
            })
            .expect("world use creates a parser alias");

        let mut type_references =
            build_type_references(&resolve, &package_units).expect("type references build");
        type_references.remove(&target_id);
        assert!(
            build_generated_world_type_aliases(
                &resolve,
                package_id,
                &unit,
                &package_units,
                &type_references,
            )
            .expect("alias mapping remains defensive")
            .is_empty(),
            "an alias whose target is unmapped must not be accepted"
        );
        let interface_indexes = package
            .interfaces
            .values()
            .enumerate()
            .map(|(index, id)| (*id, interface_index(index).expect("interface index fits")))
            .collect::<HashMap<_, _>>();
        let (ty, unsupported) = convert_type(
            Type::Id(alias_id),
            &resolve,
            &unit,
            &interface_indexes,
            &type_references,
            &build_unsupported_types(&resolve),
        )
        .expect("unsupported alias can be represented");
        assert_eq!(ty, WitType::Unsupported(Unsupported::WorldType));
        assert_eq!(unsupported, Some(Unsupported::WorldType));
    }

    #[test]
    fn converts_sorted_multifile_package_and_maps_declared_types_and_worlds() {
        let unit = dir_unit();
        let parsed = parse_source_files(
            unit,
            [
                WitSourceFile::new(
                    "z-world.wit",
                    "package acme:graphics@1.2.3;\nworld app { import types; export draw: func(); }",
                ),
                WitSourceFile::new(
                    "a-types.wit",
                    "package acme:graphics@1.2.3;\n/// A point.\ninterface types { record point { x: u32, y: u32 } draw: func(value: point) -> point; }",
                ),
            ],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");

        assert_eq!(package.name.namespace, "acme");
        assert_eq!(package.name.name, "graphics");
        assert_eq!(
            package.name.version.as_ref().map(ToString::to_string),
            Some("1.2.3".to_owned())
        );
        assert_eq!(package.interfaces.len(), 1);
        assert_eq!(package.interfaces[WitIfaceIdx::from_raw(0)].name, "types");
        assert_eq!(
            package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(0)]
                .span
                .as_ref()
                .map(|span| span.path.as_str()),
            Some("a-types.wit")
        );
        assert_eq!(
            package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(0)].name,
            "point"
        );
        let WitItemKind::Record(fields) =
            &package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(0)].kind
        else {
            panic!("record model expected");
        };
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["x", "y"]
        );
        let WitItemKind::Func(function) =
            &package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(1)].kind
        else {
            panic!("function model expected");
        };
        assert!(matches!(function.params[0].1, WitType::Named(_)));
        assert_eq!(
            function.result,
            Some(WitType::Named(match &function.params[0].1 {
                WitType::Named(reference) => reference.clone(),
                _ => unreachable!(),
            }))
        );
        assert_eq!(package.worlds.len(), 1);
        assert!(matches!(
            package.worlds[WitWorldIdx::from_raw(0)].imports[0],
            WitWorldItem::Interface { .. }
        ));
        assert_eq!(
            package.worlds[WitWorldIdx::from_raw(0)]
                .span
                .as_ref()
                .map(|span| span.path.as_str()),
            Some("z-world.wit")
        );
    }

    #[test]
    fn preserves_interleaved_world_import_export_order() {
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new(
                "world-order.wit",
                "package acme:graphics@1.2.3;\nworld app { import first: func(); export middle: func(); import last: func(); }",
            )],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");
        let world = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world is retained");

        assert_eq!(
            world
                .source_order
                .iter()
                .map(|entry| entry.direction)
                .collect::<Vec<_>>(),
            [
                WitWorldItemDirection::Import,
                WitWorldItemDirection::Export,
                WitWorldItemDirection::Import,
            ]
        );
    }

    #[test]
    fn captures_exact_source_spans_for_world_items_and_includes() {
        let source = "\
package acme:graphics@1.2.3;
interface api { call: func(); }
world base { import inherited: func(); }
world app {
    import api;
    export exposed: func();
    include base;
}
";
        let parsed =
            parse_source_files(dir_unit(), [WitSourceFile::new("world-spans.wit", source)])
                .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");
        let app = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world is retained");

        let api_span = match &app.imports[0] {
            WitWorldItem::Interface {
                reference, span, ..
            } if reference.name == "api" => {
                span.as_ref().expect("authored interface import has a span")
            }
            item => panic!("expected the api interface import, found {item:?}"),
        };
        let api_start = source.find("import api;").unwrap() + "import ".len();
        assert_eq!(api_span.path, "world-spans.wit");
        assert_eq!(api_span.range, api_start..api_start + "api".len());
        assert_eq!(&source[api_span.range.clone()], "api");

        let exposed_span = app
            .exports
            .iter()
            .find_map(|item| match item {
                WitWorldItem::Func {
                    name,
                    span: Some(span),
                    ..
                } if name == "exposed" => Some(span),
                _ => None,
            })
            .expect("authored world function has a span");
        let exposed_start = source.find("export exposed:").unwrap() + "export ".len();
        assert_eq!(exposed_span.path, "world-spans.wit");
        assert_eq!(
            exposed_span.range,
            exposed_start..exposed_start + "exposed".len()
        );
        assert_eq!(&source[exposed_span.range.clone()], "exposed");

        let include_span = app.includes[0]
            .span
            .as_ref()
            .expect("authored include has a span");
        let include_start = source.find("include base;").unwrap() + "include ".len();
        assert_eq!(include_span.path, "world-spans.wit");
        assert_eq!(
            include_span.range,
            include_start..include_start + "base".len()
        );
        assert_eq!(&source[include_span.range.clone()], "base");
    }

    #[test]
    fn preserves_interleaved_order_for_package_qualified_interface_items() {
        let dependency = dir_unit_for("acme:dep", "acme:dep#1", "1.0.0");
        let consumer = dir_unit_for("acme:consumer", "acme:consumer#1", "1.0.0");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let dependency = parse_source_files(
            dependency,
            [WitSourceFile::new(
                "dependency.wit",
                "package acme:dep@1.0.0;\ninterface api { call: func(); }",
            )],
        )
        .expect("dependency WIT parses");
        context
            .push_package(&mut resolve, dependency)
            .expect("dependency WIT resolves");

        let consumer_source = "package acme:consumer@1.0.0;\nworld app { export before: func(); import acme:dep/api@1.0.0; export after: func(); }";
        let consumer = parse_source_files(
            consumer,
            [WitSourceFile::new("consumer.wit", consumer_source)],
        )
        .expect("consumer WIT parses");
        let package = context
            .push_package(&mut resolve, consumer)
            .expect("consumer WIT resolves against its dependency");
        let world = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world is retained");

        assert_eq!(
            world
                .source_order
                .iter()
                .map(|entry| (entry.direction, entry.index))
                .collect::<Vec<_>>(),
            [
                (WitWorldItemDirection::Export, 0),
                (WitWorldItemDirection::Import, 0),
                (WitWorldItemDirection::Export, 1),
            ],
            "the resolved interface identity must not hide its direct source position"
        );
        assert!(matches!(
            &world.imports[0],
            WitWorldItem::Interface { reference, .. }
                if reference.name == "api"
                    && reference.unit.canonical_name() == "acme:dep@1.0.0"
        ));
        let WitWorldItem::Interface {
            span: Some(span), ..
        } = &world.imports[0]
        else {
            panic!("package-qualified interface import retains its source span");
        };
        let expected_start =
            consumer_source.find("acme:dep/api@1.0.0").unwrap() + "acme:dep/".len();
        assert_eq!(span.path, "consumer.wit");
        assert_eq!(span.range, expected_start..expected_start + "api".len());
        assert_eq!(&consumer_source[span.range.clone()], "api");
    }

    #[test]
    fn preserves_world_includes_and_stability_annotations() {
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new(
                "world-includes.wit",
                "package acme:graphics@1.2.3;\n\
                 @since(version = 0.1.0+build.7)\n\
                 interface api {\n\
                   @since(version = 1.0.0-18446744073709551616+build.7)\n\
                   call: func();\n\
                 }\n\
                 world base { import inherited: func(); }\n\
                 @since(version = 0.1.0)\n\
                 @deprecated(version = 1.1.0)\n\
                 world app {\n\
                   @since(version = 0.2.0)\n\
                   import before: func();\n\
                   @since(version = 0.3.0)\n\
                   @deprecated(version = 1.0.0)\n\
                   include base with { inherited as renamed }\n\
                   @since(version = 0.4.0)\n\
                   export api;\n\
                   @since(version = 0.5.0)\n\
                   import after: func();\n\
                 }",
            )],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");

        let version = |value| WitStabilityVersion::parse(value).expect("valid test version");
        let api = package
            .interfaces
            .iter()
            .find(|interface| interface.name == "api")
            .expect("api interface is retained");
        assert_eq!(api.since, Some(version("0.1.0+build.7")));
        assert_eq!(
            api.items[WitItemIdx::from_raw(0)].since,
            Some(version("1.0.0-18446744073709551616+build.7"))
        );

        let world = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world is retained");
        assert_eq!(world.since, Some(version("0.1.0")));
        assert_eq!(world.deprecated, Some(version("1.1.0")));
        assert_eq!(world.includes.len(), 1);
        assert_eq!(world.includes[0].name, "base");
        assert_eq!(
            world.includes[0].names,
            [("inherited".to_owned(), "renamed".to_owned())]
        );
        assert_eq!(world.includes[0].since, Some(version("0.3.0")));
        assert_eq!(world.includes[0].deprecated, Some(version("1.0.0")));
        assert_eq!(
            world
                .source_order
                .iter()
                .map(|entry| entry.direction)
                .collect::<Vec<_>>(),
            [
                WitWorldItemDirection::Import,
                WitWorldItemDirection::Include,
                WitWorldItemDirection::Export,
                WitWorldItemDirection::Import,
            ]
        );

        let imported_functions = world
            .imports
            .iter()
            .filter_map(|item| match item {
                WitWorldItem::Func {
                    name,
                    since,
                    deprecated,
                    ..
                } => Some((name.as_str(), since.as_ref(), deprecated.as_ref())),
                WitWorldItem::Interface { .. } => None,
            })
            .collect::<Vec<_>>();
        assert!(
            imported_functions
                .iter()
                .any(|(name, since, _)| *name == "renamed" && since.is_none())
        );
        assert!(
            imported_functions
                .iter()
                .any(|(name, since, _)| { *name == "before" && *since == Some(&version("0.2.0")) })
        );
        assert!(
            imported_functions
                .iter()
                .any(|(name, since, _)| { *name == "after" && *since == Some(&version("0.5.0")) })
        );

        assert!(world.exports.iter().any(|item| matches!(
            item,
            WitWorldItem::Interface {
                reference,
                since: Some(since),
                ..
            } if reference.unit == package.unit
                && reference.name == "api"
                && since == &version("0.4.0")
        )));
    }

    #[test]
    fn omits_unstable_includes_before_printing_the_resolved_model() {
        let source = "\
package acme:graphics@1.2.3;
world base { import hidden: func(); }
world app {
    @unstable(feature = hidden-api)
    @deprecated(version = 1.0.0)
    include base;
}
";
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new("unstable-include.wit", source)],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("unstable include is filtered by the resolver");
        let app = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("stable app world is retained");

        assert!(app.includes.is_empty());
        assert!(app.imports.is_empty());
        assert!(app.source_order.is_empty());
        assert!(!print_wit(&package).contains("include base"));
    }

    #[test]
    fn aggregates_resource_members_and_retain_map_as_an_unsupported_use() {
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new(
                "resource.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { resource canvas { constructor(); paint: func(color: u32); make: static func() -> canvas; } map-values: func(values: map<string, u32>); }",
            )],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");

        let WitItemKind::Resource(members) =
            &package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(0)].kind
        else {
            panic!("resource model expected");
        };
        assert_eq!(
            members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            ["constructor", "paint", "make"]
        );
        assert_eq!(
            members[2].func.result,
            Some(WitType::Own(crate::WitTypeRef {
                interface: crate::WitIfaceRef {
                    unit: package.unit.clone(),
                    interface: crate::WitIfaceIdx::from_raw(0),
                    name: "drawing".to_owned(),
                },
                item: crate::WitItemIdx::from_raw(0),
                item_name: "canvas".to_owned(),
            }))
        );
        assert_eq!(
            package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(1)].unsupported,
            Some(Unsupported::Map)
        );
        assert_eq!(
            package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(1)].kind,
            WitItemKind::Func(crate::WitFunc {
                params: vec![("values".to_owned(), WitType::Unsupported(Unsupported::Map))],
                result: None,
                is_async: false
            })
        );
    }

    #[test]
    fn authored_resource_and_member_provenance_maps_exact_direct_source_declarations() {
        let source = concat!(
            "package acme:graphics@1.2.3;\n",
            "interface drawing {\n",
            "  @since(version = 1.0.0)\n",
            "  resource canvas {\n",
            "    @since(version = 1.0.0)\n",
            "    @deprecated(version = 2.0.0)\n",
            "    constructor(seed: tuple<u32, string>);\n",
            "    paint: func(color: result<tuple<u32, string>, option<list<u8>>>);\n",
            "    make: static func() -> canvas;\n",
            "  }\n",
            "}\n",
        );
        let parsed = parse_source_files(dir_unit(), [WitSourceFile::new("resources.wit", source)])
            .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");
        let item = &package.interfaces[WitIfaceIdx::from_raw(0)].items[WitItemIdx::from_raw(0)];
        assert_eq!(item.name, "canvas");
        let item_span = item.span.as_ref().expect("resource name span is mapped");
        assert_eq!(item_span.path, "resources.wit");
        assert_eq!(&source[item_span.range.clone()], "canvas");
        let declaration_span = item
            .declaration_span
            .as_ref()
            .expect("resource declaration span is scanner-proven");
        assert_eq!(
            &source[declaration_span.range.clone()],
            "@since(version = 1.0.0)\n  resource canvas {\n    @since(version = 1.0.0)\n    @deprecated(version = 2.0.0)\n    constructor(seed: tuple<u32, string>);\n    paint: func(color: result<tuple<u32, string>, option<list<u8>>>);\n    make: static func() -> canvas;\n  }"
        );
        let WitItemKind::Resource(members) = &item.kind else {
            panic!("resource members expected");
        };
        let source_members = crate::authored_interface_resource_tokens(
            source,
            "drawing",
            "canvas",
            &item_span.range,
        )
        .expect("resource scanner matches the parsed resource name span");
        assert_eq!(
            members
                .iter()
                .map(|member| (member.name.as_str(), member.kind))
                .collect::<Vec<_>>(),
            source_members
                .members
                .iter()
                .map(|member| (member.name.as_str(), member.kind))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            ["constructor", "paint", "make"]
        );
        for source_member in &source_members.members {
            assert_eq!(
                &source[source_member.name_range.clone()],
                source_member.name
            );
            assert!(source[source_member.declaration_range.clone()].ends_with(';'));
            assert_eq!(
                source_member
                    .token_pairs
                    .iter()
                    .filter(|(_, text)| text.is_empty())
                    .count(),
                1
            );
        }
        assert_eq!(
            &source[source_members.members[0].declaration_range.clone()],
            "@since(version = 1.0.0)\n    @deprecated(version = 2.0.0)\n    constructor(seed: tuple<u32, string>);"
        );
    }

    #[test]
    fn authored_type_and_enum_provenance_binds_multifile_escaped_names_to_root_sources() {
        let alpha = concat!(
            "package acme:graphics@1.2.3;\n",
            "interface alpha { type %type = u32; enum %enum { %type, ready } }\n",
        );
        let bravo = concat!(
            "package acme:graphics@1.2.3;\n",
            "interface bravo { type %type = string; enum %enum { %type, done } }\n",
        );
        let parsed = parse_source_files(
            dir_unit(),
            [
                WitSourceFile::new("z-bravo.wit", bravo),
                WitSourceFile::new("a-alpha.wit", alpha),
            ],
        )
        .expect("both authored root files parse");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("the multi-file root package resolves");
        let sources = HashMap::from([("a-alpha.wit", alpha), ("z-bravo.wit", bravo)]);
        let mut local_name_ranges = Vec::new();

        for (interface_name, expected_path) in [("alpha", "a-alpha.wit"), ("bravo", "z-bravo.wit")]
        {
            let interface = package
                .interfaces
                .iter()
                .find(|interface| interface.name == interface_name)
                .expect("root interface is retained");
            let item = interface
                .items
                .iter()
                .find(|item| item.name == "type")
                .expect("escaped alias name is normalized in the model");
            assert!(matches!(&item.kind, WitItemKind::Alias(_)));
            let name_span = item.span.as_ref().expect("alias name span is mapped");
            assert_eq!(name_span.path, expected_path);
            let source = sources[name_span.path.as_str()];
            assert_eq!(&source[name_span.range.clone()], "%type");
            local_name_ranges.push(name_span.range.clone());

            let tokens = crate::authored_interface_alias_tokens(
                source,
                interface_name,
                &item.name,
                &name_span.range,
            )
            .expect("the source selected by the parser span proves the direct alias");
            assert_eq!(
                &source[tokens.declaration_range],
                if interface_name == "alpha" {
                    "type %type = u32;"
                } else {
                    "type %type = string;"
                }
            );

            let enum_item = interface
                .items
                .iter()
                .find(|item| item.name == "enum")
                .expect("escaped enum name is normalized in the model");
            assert!(matches!(&enum_item.kind, WitItemKind::Enum(_)));
            let enum_span = enum_item.span.as_ref().expect("enum name span is mapped");
            assert_eq!(enum_span.path, expected_path);
            assert_eq!(&source[enum_span.range.clone()], "%enum");
            let enum_tokens = crate::authored_interface_enum_tokens(
                source,
                interface_name,
                &enum_item.name,
                &enum_span.range,
            )
            .expect("the selected source proves the direct escaped enum");
            assert_eq!(
                enum_tokens
                    .cases
                    .iter()
                    .map(|case| case.name.as_str())
                    .collect::<Vec<_>>(),
                if interface_name == "alpha" {
                    ["type", "ready"]
                } else {
                    ["type", "done"]
                }
            );
            assert_eq!(
                &source[enum_tokens.cases[0].name_range.clone()],
                "%type",
                "the scanner preserves the escaped case's exact source range"
            );
        }
        assert_eq!(
            local_name_ranges[0], local_name_ranges[1],
            "both escaped aliases use the same local byte range, so file identity is decisive"
        );
    }

    #[test]
    fn preserves_authored_world_include_docs_in_printing_round_trip_and_hash() {
        let unit = dir_unit();
        let source = "\
package acme:graphics@1.2.3;
world base {}
world app {
  /// Included worker contract.
  @since(version = 0.3.0)
  include base;
}
";
        let parsed = parse_source_files(
            unit.clone(),
            [WitSourceFile::new("include-docs.wit", source)],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("authored package resolves");
        let app_index = package
            .worlds
            .iter_enumerated()
            .find_map(|(index, world)| (world.name == "app").then_some(index))
            .expect("app world is retained");
        let include = &package.worlds[app_index].includes[0];
        assert_eq!(include.docs.as_deref(), Some("Included worker contract."));

        let printed = print_wit(&package);
        assert!(printed.contains(
            "  /// Included worker contract.\n  @since(version = 0.3.0)\n  include base;\n"
        ));
        let reparsed = parse_source_files(unit, [WitSourceFile::new("round-trip.wit", printed)])
            .expect("printed WIT parses");
        let mut round_trip_context = WitResolveContext::default();
        let mut round_trip_resolve = Resolve::default();
        let round_trip = round_trip_context
            .push_package(&mut round_trip_resolve, reparsed)
            .expect("printed package resolves");
        let round_trip_app = round_trip
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world survives round trip");
        assert_eq!(
            round_trip_app.includes[0].docs.as_deref(),
            Some("Included worker contract.")
        );

        let mut without_docs = package.clone();
        without_docs.worlds[app_index].includes[0].docs = None;
        assert_ne!(
            sai_stable_hash::fingerprint(&package),
            sai_stable_hash::fingerprint(&without_docs),
            "include documentation participates in the stable model hash"
        );
    }

    #[test]
    fn propagates_unsupported_features_through_named_types_and_keeps_scope_local() {
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new(
                "unsupported.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { type map-type = map<string, u32>; record container { values: map-type } type container-alias = container; resource store { lookup: func() -> map-type; } use-map: func(value: container-alias); } world app { import use-map: func(value: map<string, u32>); }",
            )],
        )
        .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("self-contained package resolves");

        let items = &package.interfaces[WitIfaceIdx::from_raw(0)].items;
        assert_eq!(items[WitItemIdx::from_raw(0)].name, "map-type");
        assert_eq!(
            items[WitItemIdx::from_raw(0)].unsupported,
            Some(Unsupported::Map)
        );
        assert_eq!(items[WitItemIdx::from_raw(1)].name, "container");
        assert_eq!(
            items[WitItemIdx::from_raw(1)].unsupported,
            Some(Unsupported::Map)
        );
        assert_eq!(items[WitItemIdx::from_raw(2)].name, "container-alias");
        assert_eq!(
            items[WitItemIdx::from_raw(2)].unsupported,
            Some(Unsupported::Map)
        );
        assert_eq!(items[WitItemIdx::from_raw(3)].name, "store");
        assert_eq!(items[WitItemIdx::from_raw(3)].unsupported, None);
        let WitItemKind::Resource(members) = &items[WitItemIdx::from_raw(3)].kind else {
            panic!("resource model expected");
        };
        assert_eq!(members[0].unsupported, Some(Unsupported::Map));
        assert_eq!(items[WitItemIdx::from_raw(4)].name, "use-map");
        assert_eq!(
            items[WitItemIdx::from_raw(4)].unsupported,
            Some(Unsupported::Map)
        );

        assert_eq!(package.worlds[WitWorldIdx::from_raw(0)].unsupported, None);
        assert!(matches!(
            &package.worlds[WitWorldIdx::from_raw(0)].imports[0],
            WitWorldItem::Func {
                name,
                unsupported: Some(Unsupported::Map),
                ..
            } if name == "use-map"
        ));
    }

    #[test]
    fn keeps_first_unsupported_type_in_wit_traversal_order() {
        let ordered_types = [0_u8, 1, 2, 3, 4];
        let dependencies = HashMap::from([
            (0, vec![UnsupportedEdge::Type(2)]),
            (1, vec![]),
            (2, vec![UnsupportedEdge::Type(3)]),
            (3, vec![]),
            (4, vec![UnsupportedEdge::Type(0), UnsupportedEdge::Type(1)]),
        ]);
        let local_results = HashMap::from([
            (0, None),
            (1, Some(Unsupported::Future)),
            (2, None),
            (3, Some(Unsupported::Map)),
            (4, None),
        ]);

        let results = resolve_transitive_unsupported(&ordered_types, &dependencies, &local_results);

        assert_eq!(results.get(&4), Some(&Some(Unsupported::Map)));
    }

    #[test]
    fn propagates_unsupported_features_through_recursive_dependency_cycles() {
        let ordered_types = [0_u8, 1, 2];
        let dependencies = HashMap::from([
            (0, vec![UnsupportedEdge::Type(1)]),
            (1, vec![UnsupportedEdge::Type(0), UnsupportedEdge::Type(2)]),
            (2, vec![]),
        ]);
        let local_results = HashMap::from([(0, None), (1, None), (2, Some(Unsupported::Map))]);

        let results = resolve_transitive_unsupported(&ordered_types, &dependencies, &local_results);

        assert_eq!(results.get(&0), Some(&Some(Unsupported::Map)));
        assert_eq!(results.get(&1), Some(&Some(Unsupported::Map)));
        assert_eq!(results.get(&2), Some(&Some(Unsupported::Map)));
    }

    #[test]
    fn nested_packages_are_rejected_before_mutating_the_resolve() {
        let source =
            "package acme:graphics@1.2.3;\npackage acme:child@1.0.0 { interface child {} }";
        let parsed = parse_source_files(dir_unit(), [WitSourceFile::new("nested.wit", source)])
            .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let error = context
            .push_package(&mut resolve, parsed)
            .expect_err("nested packages are not one WIT unit");
        let WitModelError::NestedPackages { count, location } = error else {
            panic!("nested package error expected");
        };
        assert_eq!(count, 1);
        let location = location.expect("nested package declaration has a source location");
        assert_eq!(location.path, "nested.wit");
        assert_eq!(&source[location.range], "acme:child@1.0.0");
        assert_eq!(resolve.packages.len(), 0);
    }

    #[test]
    fn package_identity_mismatch_is_rejected_before_mutating_the_resolve() {
        let packages = PackageInterner::new();
        let unit = WitUnit::dir(
            packages
                .intern("acme:graphics#1")
                .expect("valid package key"),
            &packages,
            &MvsPackageVersion::new(
                "acme:graphics",
                Version::parse("1.2.3").expect("valid test version"),
            )
            .expect("valid test package version"),
        )
        .expect("package ID matches package version");
        let source = "package other:graphics@1.2.3;\ninterface drawing {}";
        let parsed = parse_source_files(unit, [WitSourceFile::new("mismatch.wit", source)])
            .expect("valid WIT parses");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();

        let error = context
            .push_package(&mut resolve, parsed)
            .expect_err("WIT package must match the unit identity");
        let WitModelError::PackageMismatch {
            expected,
            found,
            location,
        } = error
        else {
            panic!("package mismatch error expected");
        };
        assert_eq!(expected, "acme:graphics@1.2.3");
        assert_eq!(found, "other:graphics@1.2.3");
        assert_eq!(
            location,
            Some(WitSourceLocation {
                path: "mismatch.wit".to_owned(),
                range: 8..28,
            })
        );
        assert_eq!(
            &source[location.expect("package declaration location").range],
            "other:graphics@1.2.3"
        );
        assert_eq!(resolve.packages.len(), 0);
        assert!(!context.failed);
    }

    #[test]
    fn direct_push_resolves_external_world_includes_to_an_existing_dependency() {
        let dependency_unit = dir_unit_for("acme:dependency", "acme:dependency#1", "1.4.12");
        let dependency_source = "\
package acme:dependency@1.4.12;
world %interface { import dependency-fn: func(); }
";
        let root_unit = dir_unit_for("acme:root", "acme:root#1", "1.0.0");
        let root_source = "\
package acme:root@1.0.0;
world app { include acme:dependency/%interface@1.4.12; }
";
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();

        let dependency = parse_source_files(
            dependency_unit.clone(),
            [WitSourceFile::new("dependency.wit", dependency_source)],
        )
        .expect("dependency WIT parses");
        context
            .push_package(&mut resolve, dependency)
            .expect("dependency is pushed first");

        let root = parse_source_files(root_unit, [WitSourceFile::new("root.wit", root_source)])
            .expect("root WIT parses");
        let package = context
            .push_package(&mut resolve, root)
            .expect("an existing Resolve dependency resolves the external include");
        let world = package
            .worlds
            .iter()
            .find(|world| world.name == "app")
            .expect("app world is retained");

        assert_eq!(world.includes.len(), 1);
        assert_eq!(world.includes[0].unit, dependency_unit);
        assert_eq!(world.includes[0].name, "interface");
        assert_eq!(world.includes[0].world.raw(), 0);
    }

    #[test]
    fn retains_named_results_as_unsupported_on_functions_members_and_world_items() {
        let source = "package acme:graphics@1.2.3;\ninterface drawing { result-values: func() -> (left: u32, right: string); map-before-results: func(value: map<string, u32>) -> (left: u32, right: string); async-before-map: async func(value: map<string, u32>); single: func() -> (answer: bool); ordinary: func() -> tuple<u32, string>; resource canvas { convert: func() -> (left: u32, right: string); make: static func() -> (%func: u32, handle: string); } } world app { import required: func() -> (value: u32, error: string); }";
        let parsed = parse_source_files(
            dir_unit(),
            [WitSourceFile::new("named-results.wit", source)],
        )
        .expect("named results are normalized for wit-parser");
        let mut context = WitResolveContext::default();
        let mut resolve = Resolve::default();
        let package = context
            .push_package(&mut resolve, parsed)
            .expect("named results remain represented as unsupported");
        let items = &package.interfaces[WitIfaceIdx::from_raw(0)].items;

        let result_values = items
            .iter()
            .find(|item| item.name == "result-values")
            .expect("free function is retained");
        assert_eq!(result_values.unsupported, Some(Unsupported::NamedResults));
        assert!(matches!(
            &result_values.kind,
            WitItemKind::Func(function) if function.result.is_none()
        ));

        let map_before_results = items
            .iter()
            .find(|item| item.name == "map-before-results")
            .expect("mixed function is retained");
        assert_eq!(map_before_results.unsupported, Some(Unsupported::Map));
        let async_before_map = items
            .iter()
            .find(|item| item.name == "async-before-map")
            .expect("async function is retained");
        assert_eq!(async_before_map.unsupported, Some(Unsupported::Async));

        let single = items
            .iter()
            .find(|item| item.name == "single")
            .expect("single named result is retained as its type");
        assert_eq!(single.unsupported, None);
        assert!(matches!(
            &single.kind,
            WitItemKind::Func(function) if function.result == Some(WitType::Bool)
        ));

        let ordinary = items
            .iter()
            .find(|item| item.name == "ordinary")
            .expect("ordinary tuple function is retained");
        assert_eq!(ordinary.unsupported, None);
        assert!(matches!(
            &ordinary.kind,
            WitItemKind::Func(function) if matches!(function.result, Some(WitType::Tuple(_)))
        ));
        let ordinary_span = ordinary.span.as_ref().expect("function has a source span");
        assert_eq!(
            &source[ordinary_span.range.clone()],
            "ordinary",
            "source spans after rewritten results map back to the original bytes"
        );

        let canvas = items
            .iter()
            .find(|item| item.name == "canvas")
            .expect("resource is retained");
        let WitItemKind::Resource(members) = &canvas.kind else {
            panic!("resource members expected");
        };
        assert_eq!(members[0].name, "convert");
        assert_eq!(members[0].unsupported, Some(Unsupported::NamedResults));
        assert_eq!(members[0].func.result, None);
        assert_eq!(members[1].name, "make");
        assert_eq!(members[1].unsupported, Some(Unsupported::NamedResults));
        assert_eq!(members[1].func.result, None);

        let world = &package.worlds[WitWorldIdx::from_raw(0)];
        assert!(matches!(
            &world.imports[0],
            WitWorldItem::Func {
                name,
                func,
                unsupported: Some(Unsupported::NamedResults),
                ..
            } if name == "required" && func.result.is_none()
        ));
    }

    #[test]
    fn invalid_named_result_fields_are_left_for_wit_parser_to_reject() {
        for (path, source) in [
            (
                "invalid-number.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (1: u32, valid: string); }",
            ),
            (
                "invalid-keyword.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (func: u32, valid: string); }",
            ),
            (
                "duplicate-result.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (value: u32, value: string); }",
            ),
            (
                "invalid-control.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (value\u{202e}: u32, valid: string); }",
            ),
            (
                "comment-control.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (value /*\u{202e}*/: u32, valid: string); }",
            ),
            (
                "invalid-string.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { bad: func() -> (value \"not-a-label\": u32, valid: string); }",
            ),
        ] {
            let error = parse_source_files(dir_unit(), [WitSourceFile::new(path, source)])
                .expect_err("invalid result field names must not be normalized away");
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn propagates_unsupported_types_across_package_boundaries() {
        let dependency = dir_unit_for("acme:dep", "acme:dep#1", "1.0.0");
        let consumer = dir_unit_for("acme:consumer", "acme:consumer#1", "1.0.0");
        let mut resolve = Resolve::default();
        let mut context = WitResolveContext::default();
        let dependency = parse_source_files(
            dependency,
            [WitSourceFile::new(
                "dependency.wit",
                "package acme:dep@1.0.0;\ninterface types { type map-type = map<string, u32>; }",
            )],
        )
        .expect("dependency WIT parses");
        context
            .push_package(&mut resolve, dependency)
            .expect("dependency WIT resolves");

        let consumer = parse_source_files(
            consumer,
            [WitSourceFile::new(
                "consumer.wit",
                "package acme:consumer@1.0.0;\ninterface api { use acme:dep/types@1.0.0.{map-type}; consume: func(value: map-type); }",
            )],
        )
        .expect("consumer WIT parses");
        let package = context
            .push_package(&mut resolve, consumer)
            .expect("consumer WIT resolves against its dependency");

        let items = &package.interfaces[WitIfaceIdx::from_raw(0)].items;
        let imported_type = items
            .iter()
            .find(|item| item.name == "map-type")
            .expect("imported type remains in the model");
        assert_eq!(imported_type.unsupported, Some(Unsupported::Map));
        let consumer_function = items
            .iter()
            .find(|item| item.name == "consume")
            .expect("consumer function remains in the model");
        assert_eq!(consumer_function.unsupported, Some(Unsupported::Map));
    }

    #[test]
    fn propagates_unsupported_types_through_recursive_resolve_graphs() {
        use wit_parser::{
            Docs, Field, Record, Resolve, Span, Stability, Type, TypeDef, TypeDefKind, TypeOwner,
        };

        let recursive_source = parse_source_files(
            dir_unit(),
            [WitSourceFile::new(
                "recursive.wit",
                "package acme:graphics@1.2.3;\ninterface drawing { record root { child: cycle } type cycle = root; type unsupported = map<string, u32>; }",
            )],
        )
        .expect_err("wit-parser rejects recursive WIT type definitions");
        assert!(recursive_source.to_string().contains("depends on itself"));

        let mut resolve = Resolve::default();
        let root = resolve.types.alloc(TypeDef {
            name: Some("root".to_owned()),
            kind: TypeDefKind::Type(Type::Bool),
            owner: TypeOwner::None,
            docs: Docs::default(),
            stability: Stability::Unknown,
            span: Span::default(),
        });
        let map = resolve.types.alloc(TypeDef {
            name: Some("map".to_owned()),
            kind: TypeDefKind::Map(Type::String, Type::U32),
            owner: TypeOwner::None,
            docs: Docs::default(),
            stability: Stability::Unknown,
            span: Span::default(),
        });
        let cycle = resolve.types.alloc(TypeDef {
            name: Some("cycle".to_owned()),
            kind: TypeDefKind::Type(Type::Id(root)),
            owner: TypeOwner::None,
            docs: Docs::default(),
            stability: Stability::Unknown,
            span: Span::default(),
        });
        resolve.types[root].kind = TypeDefKind::Record(Record {
            fields: vec![Field {
                name: "cycle".to_owned(),
                ty: Type::Id(cycle),
                docs: Docs::default(),
                span: Span::default(),
            }],
        });
        resolve.types[cycle].kind = TypeDefKind::Record(Record {
            fields: vec![
                Field {
                    name: "root".to_owned(),
                    ty: Type::Id(root),
                    docs: Docs::default(),
                    span: Span::default(),
                },
                Field {
                    name: "map".to_owned(),
                    ty: Type::Id(map),
                    docs: Docs::default(),
                    span: Span::default(),
                },
            ],
        });

        let unsupported = super::build_unsupported_types(&resolve);
        assert_eq!(unsupported.get(&root), Some(&Some(Unsupported::Map)));
        assert_eq!(unsupported.get(&cycle), Some(&Some(Unsupported::Map)));
        assert_eq!(unsupported.get(&map), Some(&Some(Unsupported::Map)));
    }

    #[test]
    fn model_hashes_ignore_session_local_package_ids_in_nested_type_refs() {
        let first_unit = dir_unit_for("acme:graphics", "acme:graphics#1", "1.2.3");
        let packages = PackageInterner::new();
        packages
            .intern("noise:package#1")
            .expect("valid unrelated package key");
        let package = packages
            .intern("acme:graphics#1")
            .expect("valid package key");
        let version = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid package version");
        let second_unit =
            WitUnit::dir(package, &packages, &version).expect("package ID matches package version");
        assert_ne!(first_unit.kind(), second_unit.kind());

        let source = "package acme:graphics@1.2.3;\ninterface shapes { record point { x: u32 } type point-alias = point; consume: func(value: point) -> point; }";
        let load = |unit| {
            let parsed = parse_source_files(unit, [WitSourceFile::new("stable.wit", source)])
                .expect("valid WIT parses");
            let mut context = WitResolveContext::default();
            let mut resolve = Resolve::default();
            context
                .push_package(&mut resolve, parsed)
                .expect("self-contained package resolves")
        };
        let first = load(first_unit);
        let second = load(second_unit);

        assert_eq!(first, second);
        assert_eq!(first.fingerprint(), second.fingerprint());
    }
}
