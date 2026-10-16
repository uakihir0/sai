//! Conversion from the resolved `wit-parser` graph into SAI's owned WIT model.

use crate::{
    ParsedWitPackageGroup, Unsupported, WitCase, WitField, WitFunc, WitIfaceIdx, WitIfaceRef,
    WitIndexVec, WitInterface, WitItem, WitItemIdx, WitItemKind, WitMember, WitMemberKind,
    WitPackage, WitPackageName, WitSourceLocation, WitSpan, WitType, WitTypeRef, WitUnit,
    WitUnitKind, WitWorld, WitWorldIdx, WitWorldItem,
};
use sai_package::version::Version;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use wit_parser::{
    Function, FunctionKind, Handle, InterfaceId, PackageId as ParserPackageId, Resolve, Span,
    Stability, Type, TypeDefKind, TypeId, TypeOwner, WorldItem, WorldKey,
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

        model_package(
            resolve,
            parser_package,
            unit,
            &self.package_units,
            &self.source_mappings,
            &self.named_result_function_spans,
            &package_name,
        )
    }
}

fn model_package(
    resolve: &Resolve,
    package_id: ParserPackageId,
    unit: WitUnit,
    package_units: &HashMap<ParserPackageId, WitUnit>,
    source_mappings: &[(u32, crate::WitSourceMapping)],
    named_result_function_spans: &HashSet<(u32, u32)>,
    package_name: &wit_parser::PackageName,
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

    let type_references = build_type_references(resolve, package_units)?;
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
            let deprecated = deprecated(&definition.stability)?;
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
            let item_index = item_index(items.len())?;
            if matches!(kind, WitItemKind::Resource(_)) {
                resource_items.insert(*type_id, item_index);
            }
            items.push(WitItem {
                name: name.clone(),
                docs,
                deprecated,
                kind,
                unsupported,
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
                    deprecated: deprecated(&function.stability)?,
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
                free_functions.push(WitItem {
                    name: function.name.clone(),
                    docs: docs(&function.docs.contents),
                    deprecated: deprecated(&function.stability)?,
                    kind: WitItemKind::Func(func),
                    unsupported,
                    span: source_span(function.span, &unit, source_mappings),
                });
            }
        }
        items.extend(free_functions);

        interfaces.push(WitInterface {
            name: wit_interface_name.clone(),
            docs: docs(&parser_interface.docs.contents),
            deprecated: deprecated(&parser_interface.stability)?,
            items: WitIndexVec::try_from_vec(items)
                .map_err(|_| WitModelError::TooManyDefinitions)?,
            span: interface_span,
        });
        debug_assert_eq!(model_index.index(), interfaces.len() - 1);
    }

    let mut worlds = Vec::with_capacity(world_ids.len());
    for (name, world_id) in package.worlds.iter() {
        let world = &resolve.worlds[*world_id];
        let mut unsupported = None;
        let imports = world_items(
            &world.imports,
            resolve,
            &unit,
            package_units,
            &interface_indexes,
            &type_references,
            &unsupported_types,
            named_result_function_spans,
            &mut unsupported,
        )?;
        let exports = world_items(
            &world.exports,
            resolve,
            &unit,
            package_units,
            &interface_indexes,
            &type_references,
            &unsupported_types,
            named_result_function_spans,
            &mut unsupported,
        )?;
        worlds.push(WitWorld {
            name: name.clone(),
            docs: docs(&world.docs.contents),
            imports,
            exports,
            unsupported,
            span: source_span(world.span, &unit, source_mappings),
        });
    }

    Ok(WitPackage {
        unit,
        name,
        interfaces: WitIndexVec::try_from_vec(interfaces)
            .map_err(|_| WitModelError::TooManyDefinitions)?,
        worlds: WitIndexVec::<WitWorldIdx, _>::try_from_vec(worlds)
            .map_err(|_| WitModelError::TooManyDefinitions)?,
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
            if matches!(definition.owner, TypeOwner::World(_)) {
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
    let item = parser_interface
        .types
        .keys()
        .position(|candidate| candidate == name)
        .ok_or(WitModelError::MissingItem)?;
    Ok(WitTypeRef {
        interface: WitIfaceRef {
            unit: unit.clone(),
            interface,
        },
        item: item_index(item)?,
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
        for (interface_position, interface_id) in package.interfaces.values().copied().enumerate() {
            let interface_index = interface_index(interface_position)?;
            let interface = &resolve.interfaces[interface_id];
            for (item_position, type_id) in interface.types.values().copied().enumerate() {
                let definition = &resolve.types[type_id];
                if definition.owner == TypeOwner::Interface(interface_id) {
                    references.entry(type_id).or_insert(WitTypeRef {
                        interface: WitIfaceRef {
                            unit: unit.clone(),
                            interface: interface_index,
                        },
                        item: item_index(item_position)?,
                    });
                }
            }
        }
    }
    Ok(references)
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
    unsupported_types: &HashMap<TypeId, Option<Unsupported>>,
    named_result_function_spans: &HashSet<(u32, u32)>,
    unsupported: &mut Option<Unsupported>,
) -> Result<Vec<WitWorldItem>, WitModelError> {
    let mut result = Vec::with_capacity(items.len());
    for (key, item) in items {
        match item {
            WorldItem::Interface { id, .. } => {
                let interface = &resolve.interfaces[*id];
                if interface.name.is_none() {
                    *unsupported =
                        first_unsupported(*unsupported, Some(Unsupported::InlineInterface));
                    continue;
                }
                result.push(WitWorldItem::Interface(interface_reference(
                    resolve,
                    *id,
                    package_units,
                )?));
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
                let (function, found) = function_model(
                    function,
                    resolve,
                    unit,
                    interface_indexes,
                    type_references,
                    unsupported_types,
                    named_result_function_spans,
                )?;
                result.push(WitWorldItem::Func {
                    name,
                    func: function,
                    unsupported: found,
                });
            }
            WorldItem::Type { .. } => {
                *unsupported = first_unsupported(*unsupported, Some(Unsupported::WorldType));
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
    })
}

fn deprecated(stability: &Stability) -> Result<Option<Version>, WitModelError> {
    let version = match stability {
        Stability::Unknown => None,
        Stability::Unstable { deprecated, .. } | Stability::Stable { deprecated, .. } => {
            deprecated.as_ref()
        }
    };
    version
        .map(|version| {
            Version::parse(&version.to_string())
                .map_err(|_| WitModelError::InvalidVersion(version.to_string()))
        })
        .transpose()
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

#[cfg(test)]
mod tests {
    use super::{
        UnsupportedEdge, WitModelError, WitResolveContext, resolve_transitive_unsupported,
    };
    use crate::{
        Unsupported, WitIfaceIdx, WitItemIdx, WitItemKind, WitSourceFile, WitSourceLocation,
        WitType, WitUnit, WitWorldIdx, WitWorldItem, parse_source_files,
    };
    use sai_intern::PackageInterner;
    use sai_package::{mvs::MvsPackageVersion, version::Version};
    use sai_stable_hash::StableHash;
    use std::collections::HashMap;
    use wit_parser::Resolve;

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
            WitWorldItem::Interface(_)
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
                },
                item: crate::WitItemIdx::from_raw(0),
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
