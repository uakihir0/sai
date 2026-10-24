//! Finds WIT data-type occurrences of a particular resource across a loaded
//! package closure.

use sai_wit::{
    LoadedWitPackage, Unsupported, WitIfaceIdx, WitInterface, WitItem, WitItemIdx, WitItemKind,
    WitPackage, WitSpan, WitType, WitTypeRef, WitUnit,
};
use std::collections::{HashMap, HashSet};

/// The unit, interface, and item identity of a WIT resource.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct WitResourceIdentity {
    pub(super) unit: WitUnit,
    pub(super) interface: WitIfaceIdx,
    pub(super) item: WitItemIdx,
}

impl WitResourceIdentity {
    pub(super) fn from_reference(reference: &WitTypeRef) -> Self {
        Self {
            unit: reference.interface.unit.clone(),
            interface: reference.interface.interface,
            item: reference.item,
        }
    }
}

/// The source construct containing the resource occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WitResourceOccurrenceSite {
    RecordField { index: usize, name: String },
    VariantCase { index: usize, name: String },
    AliasTarget,
    UsedAlias,
}

/// A WIT location that contains, directly or through aliases, the target
/// resource reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WitResourceOccurrence {
    pub(super) unit: WitUnit,
    pub(super) interface: WitIfaceIdx,
    pub(super) interface_name: String,
    pub(super) item: WitItemIdx,
    pub(super) item_name: String,
    pub(super) site: WitResourceOccurrenceSite,
    pub(super) item_span: Option<WitSpan>,
    /// False when an earlier unsupported WIT shape prevents proving that this
    /// is the first occurrence, even though this occurrence is known.
    pub(super) first_occurrence_known: bool,
}

/// Why the scan could not establish either the first occurrence or that no
/// occurrence exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WitResourceOccurrenceIncomplete {
    /// The converter discarded the map/list element references, so a hidden
    /// resource occurrence may precede any later known occurrence.
    UnsupportedType {
        unsupported: Unsupported,
        location: WitResourceOccurrence,
    },
    MissingPackage {
        unit: WitUnit,
    },
    MissingInterface {
        unit: WitUnit,
        interface: WitIfaceIdx,
    },
    MissingItem {
        unit: WitUnit,
        interface: WitIfaceIdx,
        item: WitItemIdx,
    },
}

/// The result of searching WIT declaration data for a resource reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WitResourceOccurrenceSearch {
    Found(WitResourceOccurrence),
    NotFound,
    Incomplete(WitResourceOccurrenceIncomplete),
}

/// Search a loaded package's dependency-first closure in closure and WIT
/// declaration order.
pub(super) fn first_wit_resource_occurrence(
    loaded: &LoadedWitPackage,
    target: &WitResourceIdentity,
) -> WitResourceOccurrenceSearch {
    let packages = loaded
        .dependency_models()
        .iter()
        .chain(std::iter::once(loaded.root()))
        .collect::<Vec<_>>();
    first_wit_resource_occurrence_in_closure(loaded.closure(), &packages, target)
}

/// Model-level entry point kept separate so focused tests can use
/// hand-built `WitPackage` values without parsing source text.
fn first_wit_resource_occurrence_in_closure(
    closure: &[WitUnit],
    packages: &[&WitPackage],
    target: &WitResourceIdentity,
) -> WitResourceOccurrenceSearch {
    let packages_by_unit = packages
        .iter()
        .map(|package| (package.unit.clone(), *package))
        .collect::<HashMap<_, _>>();

    let scanner = ResourceOccurrenceScanner {
        packages: &packages_by_unit,
        target,
    };

    let mut scan = ScanAccumulator::default();
    for unit in closure {
        let Some(package) = scanner.packages.get(unit).copied() else {
            scan.record_incomplete(WitResourceOccurrenceIncomplete::MissingPackage {
                unit: unit.clone(),
            });
            continue;
        };

        for (interface_index, interface) in package.interfaces.iter_enumerated() {
            for (item_index, item) in interface.items.iter_enumerated() {
                // Each top-level declaration gets its own visited set. This
                // avoids revisiting alias cycles without allowing an earlier
                // declaration to suppress a later declaration's location.
                let mut visited = HashSet::new();
                scanner.scan_item(
                    unit,
                    interface_index,
                    interface,
                    item_index,
                    item,
                    &mut visited,
                    &mut scan,
                );
                if let Some(occurrence) = scan.found.take() {
                    return WitResourceOccurrenceSearch::Found(occurrence);
                }
            }
        }
    }

    match scan.incomplete {
        Some(incomplete) => WitResourceOccurrenceSearch::Incomplete(incomplete),
        None => WitResourceOccurrenceSearch::NotFound,
    }
}

struct ResourceOccurrenceScanner<'a> {
    packages: &'a HashMap<WitUnit, &'a WitPackage>,
    target: &'a WitResourceIdentity,
}

#[derive(Default)]
struct ScanAccumulator {
    found: Option<WitResourceOccurrence>,
    incomplete: Option<WitResourceOccurrenceIncomplete>,
}

impl ResourceOccurrenceScanner<'_> {
    fn scan_item(
        &self,
        unit: &WitUnit,
        interface_index: WitIfaceIdx,
        interface: &WitInterface,
        item_index: WitItemIdx,
        item: &WitItem,
        visited: &mut HashSet<WitResourceIdentity>,
        scan: &mut ScanAccumulator,
    ) {
        match &item.kind {
            WitItemKind::Record(fields) => {
                for (index, field) in fields.iter().enumerate() {
                    let location = self.location(
                        unit,
                        interface_index,
                        interface,
                        item_index,
                        item,
                        WitResourceOccurrenceSite::RecordField {
                            index,
                            name: field.name.clone(),
                        },
                    );
                    self.scan_type(&field.ty, &location, visited, scan);
                    if scan.found.is_some() {
                        return;
                    }
                }
            }
            WitItemKind::Variant(cases) => {
                for (index, case) in cases.iter().enumerate() {
                    let Some(payload) = &case.payload else {
                        continue;
                    };
                    let location = self.location(
                        unit,
                        interface_index,
                        interface,
                        item_index,
                        item,
                        WitResourceOccurrenceSite::VariantCase {
                            index,
                            name: case.name.clone(),
                        },
                    );
                    self.scan_type(payload, &location, visited, scan);
                    if scan.found.is_some() {
                        return;
                    }
                }
            }
            WitItemKind::Alias(ty) => {
                let location = self.location(
                    unit,
                    interface_index,
                    interface,
                    item_index,
                    item,
                    WitResourceOccurrenceSite::AliasTarget,
                );
                self.scan_type(ty, &location, visited, scan);
            }
            WitItemKind::Used(reference) => {
                let location = self.location(
                    unit,
                    interface_index,
                    interface,
                    item_index,
                    item,
                    WitResourceOccurrenceSite::UsedAlias,
                );
                self.scan_reference(reference, &location, visited, scan);
            }
            // Resource members and free functions do not count as data-shape
            // occurrences for this validation.
            WitItemKind::Resource(_)
            | WitItemKind::Func(_)
            | WitItemKind::Enum(_)
            | WitItemKind::Flags(_) => {}
        }
    }

    fn scan_type(
        &self,
        ty: &WitType,
        location: &WitResourceOccurrence,
        visited: &mut HashSet<WitResourceIdentity>,
        scan: &mut ScanAccumulator,
    ) {
        match ty {
            WitType::List(inner) | WitType::Option(inner) => {
                self.scan_type(inner, location, visited, scan)
            }
            WitType::Result { ok, err } => {
                if let Some(ok) = ok {
                    self.scan_type(ok, location, visited, scan);
                }
                if scan.found.is_none()
                    && let Some(err) = err
                {
                    self.scan_type(err, location, visited, scan);
                }
            }
            WitType::Tuple(types) => {
                for ty in types {
                    self.scan_type(ty, location, visited, scan);
                    if scan.found.is_some() {
                        return;
                    }
                }
            }
            WitType::Named(reference) | WitType::Own(reference) | WitType::Borrow(reference) => {
                self.scan_reference(reference, location, visited, scan)
            }
            WitType::Future(inner) | WitType::Stream(inner) => match inner {
                Some(inner) => self.scan_type(inner, location, visited, scan),
                None => {}
            },
            WitType::Unsupported(unsupported @ (Unsupported::Map | Unsupported::FixedList)) => {
                scan.record_incomplete(WitResourceOccurrenceIncomplete::UnsupportedType {
                    unsupported: *unsupported,
                    location: location.clone(),
                });
            }
            WitType::Unsupported(_)
            | WitType::Bool
            | WitType::S8
            | WitType::S16
            | WitType::S32
            | WitType::S64
            | WitType::U8
            | WitType::U16
            | WitType::U32
            | WitType::U64
            | WitType::F32
            | WitType::F64
            | WitType::Char
            | WitType::String => {}
        }
    }

    fn scan_reference(
        &self,
        reference: &WitTypeRef,
        referring_location: &WitResourceOccurrence,
        visited: &mut HashSet<WitResourceIdentity>,
        scan: &mut ScanAccumulator,
    ) {
        let identity = WitResourceIdentity::from_reference(reference);
        if &identity == self.target {
            if scan.found.is_none() {
                let mut occurrence = referring_location.clone();
                occurrence.first_occurrence_known = scan.incomplete.is_none();
                scan.found = Some(occurrence);
            }
            return;
        }
        if !visited.insert(identity) {
            return;
        }

        let Some(package) = self.packages.get(&reference.interface.unit).copied() else {
            scan.record_incomplete(WitResourceOccurrenceIncomplete::MissingPackage {
                unit: reference.interface.unit.clone(),
            });
            return;
        };
        let Some(interface) = package.interfaces.get(reference.interface.interface) else {
            scan.record_incomplete(WitResourceOccurrenceIncomplete::MissingInterface {
                unit: reference.interface.unit.clone(),
                interface: reference.interface.interface,
            });
            return;
        };
        let Some(item) = interface.items.get(reference.item) else {
            scan.record_incomplete(WitResourceOccurrenceIncomplete::MissingItem {
                unit: reference.interface.unit.clone(),
                interface: reference.interface.interface,
                item: reference.item,
            });
            return;
        };
        self.scan_item(
            &reference.interface.unit,
            reference.interface.interface,
            interface,
            reference.item,
            item,
            visited,
            scan,
        );
    }

    fn location(
        &self,
        unit: &WitUnit,
        interface_index: WitIfaceIdx,
        interface: &WitInterface,
        item_index: WitItemIdx,
        item: &WitItem,
        site: WitResourceOccurrenceSite,
    ) -> WitResourceOccurrence {
        WitResourceOccurrence {
            unit: unit.clone(),
            interface: interface_index,
            interface_name: interface.name.clone(),
            item: item_index,
            item_name: item.name.clone(),
            site,
            item_span: item.span.clone(),
            first_occurrence_known: true,
        }
    }
}

impl ScanAccumulator {
    fn record_incomplete(&mut self, incomplete: WitResourceOccurrenceIncomplete) {
        if self.incomplete.is_none() {
            self.incomplete = Some(incomplete);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WitResourceIdentity, WitResourceOccurrenceIncomplete, WitResourceOccurrenceSearch,
        WitResourceOccurrenceSite, first_wit_resource_occurrence_in_closure,
    };
    use sai_intern::Interner;
    use sai_package::version::Version;
    use sai_wit::{
        Unsupported, WitCase, WitField, WitIfaceIdx, WitIfaceRef, WitIndexVec, WitInterface,
        WitItem, WitItemIdx, WitItemKind, WitPackage, WitPackageName, WitSpan, WitType, WitTypeRef,
        WitUnit,
    };

    fn wasi_unit(name: &str) -> WitUnit {
        let symbols = Interner::new();
        let package = symbols.intern(name).expect("valid test package symbol");
        let version = Version::parse("0.2.3").expect("valid test version");
        WitUnit::wasi(package, &symbols, &version).expect("supported embedded WIT package")
    }

    fn item(name: &str, kind: WitItemKind) -> WitItem {
        WitItem {
            name: name.to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind,
            unsupported: None,
            declaration_span: None,
            declaration_token_pairs: None,
            span: Some(WitSpan {
                path: format!("{name}.wit"),
                range: 0..1,
            }),
        }
    }

    fn interface_index() -> WitIfaceIdx {
        WitIndexVec::<WitIfaceIdx, ()>::new()
            .push(())
            .expect("test interface index fits")
    }

    fn item_index(index: usize) -> WitItemIdx {
        let mut indexes = WitIndexVec::<WitItemIdx, ()>::new();
        let mut requested = None;
        for current in 0..=index {
            let item = indexes.push(()).expect("test item index fits");
            if current == index {
                requested = Some(item);
            }
        }
        requested.expect("the requested item index was pushed")
    }

    fn new_items() -> WitIndexVec<WitItemIdx, WitItem> {
        WitIndexVec::new()
    }

    fn interface(items: Vec<WitItem>) -> WitInterface {
        let mut indexed_items = WitIndexVec::new();
        for item in items {
            indexed_items.push(item).expect("test item index fits");
        }
        WitInterface {
            name: "streams".to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            items: indexed_items,
            span: None,
        }
    }

    fn package(unit: WitUnit, interfaces: Vec<WitInterface>) -> WitPackage {
        WitPackage {
            unit,
            name: WitPackageName {
                namespace: "test".to_owned(),
                name: "package".to_owned(),
                version: None,
            },
            interfaces: WitIndexVec::try_from_vec(interfaces).expect("test interface indices fit"),
            worlds: WitIndexVec::new(),
            declaration_order: Vec::new(),
        }
    }

    fn reference(
        unit: &WitUnit,
        interface: WitIfaceIdx,
        item: WitItemIdx,
        interface_name: &str,
        item_name: &str,
    ) -> WitTypeRef {
        WitTypeRef {
            interface: WitIfaceRef {
                unit: unit.clone(),
                interface,
                name: interface_name.to_owned(),
            },
            item,
            item_name: item_name.to_owned(),
        }
    }

    fn resource(name: &str) -> WitItem {
        item(name, WitItemKind::Resource(Vec::new()))
    }

    fn target(unit: &WitUnit, interface: WitIfaceIdx, item: WitItemIdx) -> WitResourceIdentity {
        WitResourceIdentity {
            unit: unit.clone(),
            interface,
            item,
        }
    }

    fn scan(
        packages: &[WitPackage],
        closure: &[WitUnit],
        target: &WitResourceIdentity,
    ) -> WitResourceOccurrenceSearch {
        let package_refs = packages.iter().collect::<Vec<_>>();
        first_wit_resource_occurrence_in_closure(closure, &package_refs, target)
    }

    #[test]
    fn follows_used_and_deep_aliases_through_wrappers() {
        let dependency = wasi_unit("io");
        let root = wasi_unit("clocks");

        let mut dependency_items = new_items();
        let dependency_resource = dependency_items
            .push(resource("stream"))
            .expect("test item index fits");
        let dependency_interface_index = interface_index();

        let mut root_items = new_items();
        let root_interface_index = interface_index();
        let alias_a = item_index(1);
        let alias_b = item_index(2);
        let used = root_items
            .push(item(
                "shared-stream",
                WitItemKind::Used(reference(
                    &root,
                    root_interface_index,
                    alias_a,
                    "streams",
                    "alias-a",
                )),
            ))
            .expect("test item index fits");
        assert_eq!(used.index(), 0);
        let alias_a_actual = root_items
            .push(item(
                "alias-a",
                WitItemKind::Alias(WitType::Option(Box::new(WitType::Tuple(vec![
                    WitType::Result {
                        ok: Some(Box::new(WitType::List(Box::new(WitType::Borrow(
                            reference(&root, root_interface_index, alias_b, "streams", "alias-b"),
                        ))))),
                        err: None,
                    },
                ])))),
            ))
            .expect("test item index fits");
        let alias_b_actual = root_items
            .push(item(
                "alias-b",
                WitItemKind::Alias(WitType::Future(Some(Box::new(WitType::Stream(Some(
                    Box::new(WitType::Own(reference(
                        &dependency,
                        dependency_interface_index,
                        dependency_resource,
                        "streams",
                        "stream",
                    ))),
                )))))),
            ))
            .expect("test item index fits");
        assert_eq!(alias_a_actual, alias_a);
        assert_eq!(alias_b_actual, alias_b);

        let packages = [
            package(
                dependency.clone(),
                vec![interface(dependency_items.iter().cloned().collect())],
            ),
            package(
                root.clone(),
                vec![interface(root_items.iter().cloned().collect())],
            ),
        ];
        let result = scan(
            &packages,
            &[dependency.clone(), root.clone()],
            &target(&dependency, dependency_interface_index, dependency_resource),
        );

        let WitResourceOccurrenceSearch::Found(occurrence) = result else {
            panic!("expected the target resource to be found through the alias chain");
        };
        assert_eq!(occurrence.unit, root);
        assert_eq!(occurrence.interface, root_interface_index);
        assert_eq!(occurrence.item, alias_b);
        assert_eq!(occurrence.item_name, "alias-b");
        assert_eq!(occurrence.site, WitResourceOccurrenceSite::AliasTarget);
        assert_eq!(
            occurrence.item_span.as_ref().map(|span| span.path.as_str()),
            Some("alias-b.wit")
        );
    }

    #[test]
    fn finds_the_first_matching_record_field_in_field_order() {
        let unit = wasi_unit("io");
        let interface_index = interface_index();
        let mut items = new_items();
        let resource_item = items.push(resource("stream")).expect("index fits");
        let record_item = items
            .push(item(
                "record",
                WitItemKind::Record(vec![
                    WitField {
                        name: "first".to_owned(),
                        docs: None,
                        ty: WitType::Own(reference(
                            &unit,
                            interface_index,
                            resource_item,
                            "streams",
                            "stream",
                        )),
                    },
                    WitField {
                        name: "second".to_owned(),
                        docs: None,
                        ty: WitType::Borrow(reference(
                            &unit,
                            interface_index,
                            resource_item,
                            "streams",
                            "stream",
                        )),
                    },
                ]),
            ))
            .expect("index fits");
        let package = package(
            unit.clone(),
            vec![interface(items.iter().cloned().collect())],
        );

        let result = scan(
            &[package],
            &[unit.clone()],
            &target(&unit, interface_index, resource_item),
        );
        let WitResourceOccurrenceSearch::Found(occurrence) = result else {
            panic!("expected the resource in the record field");
        };
        assert_eq!(occurrence.item, record_item);
        assert_eq!(
            occurrence.site,
            WitResourceOccurrenceSite::RecordField {
                index: 0,
                name: "first".to_owned()
            }
        );
    }

    #[test]
    fn finds_resource_references_in_variant_payloads_in_case_order() {
        let unit = wasi_unit("io");
        let interface_index = interface_index();
        let mut items = new_items();
        let resource_item = items.push(resource("stream")).expect("test index fits");
        let variant_item = items
            .push(item(
                "event",
                WitItemKind::Variant(vec![
                    WitCase {
                        name: "empty".to_owned(),
                        docs: None,
                        payload: None,
                    },
                    WitCase {
                        name: "data".to_owned(),
                        docs: None,
                        payload: Some(WitType::Result {
                            ok: Some(Box::new(WitType::Borrow(reference(
                                &unit,
                                interface_index,
                                resource_item,
                                "streams",
                                "stream",
                            )))),
                            err: None,
                        }),
                    },
                ]),
            ))
            .expect("test index fits");
        let package = package(
            unit.clone(),
            vec![interface(items.iter().cloned().collect())],
        );
        let result = scan(
            &[package],
            &[unit.clone()],
            &target(&unit, interface_index, resource_item),
        );

        let WitResourceOccurrenceSearch::Found(occurrence) = result else {
            panic!("expected the resource in the variant payload");
        };
        assert_eq!(occurrence.item, variant_item);
        assert_eq!(
            occurrence.site,
            WitResourceOccurrenceSite::VariantCase {
                index: 1,
                name: "data".to_owned()
            }
        );
    }

    #[test]
    fn does_not_match_equal_names_from_a_different_unit() {
        let target_unit = wasi_unit("io");
        let other_unit = wasi_unit("clocks");
        let target_interface = interface_index();
        let other_interface = interface_index();

        let mut target_items = new_items();
        let target_item = target_items.push(resource("stream")).expect("index fits");
        let target_package = package(
            target_unit.clone(),
            vec![interface(target_items.iter().cloned().collect())],
        );

        let mut other_items = new_items();
        let other_resource = other_items.push(resource("stream")).expect("index fits");
        other_items
            .push(item(
                "wrapper",
                WitItemKind::Record(vec![WitField {
                    name: "value".to_owned(),
                    docs: None,
                    ty: WitType::Own(reference(
                        &other_unit,
                        other_interface,
                        other_resource,
                        "streams",
                        "stream",
                    )),
                }]),
            ))
            .expect("index fits");
        let other_package = package(
            other_unit.clone(),
            vec![interface(other_items.iter().cloned().collect())],
        );

        assert_eq!(
            scan(
                &[other_package, target_package],
                &[other_unit, target_unit.clone()],
                &target(&target_unit, target_interface, target_item)
            ),
            WitResourceOccurrenceSearch::NotFound
        );
    }

    #[test]
    fn excludes_free_function_signatures() {
        let unit = wasi_unit("io");
        let interface_index = interface_index();
        let mut items = new_items();
        let resource_item = items.push(resource("stream")).expect("index fits");
        items
            .push(item(
                "consume",
                WitItemKind::Func(sai_wit::WitFunc {
                    params: vec![(
                        "stream".to_owned(),
                        WitType::Own(reference(
                            &unit,
                            interface_index,
                            resource_item,
                            "streams",
                            "stream",
                        )),
                    )],
                    result: None,
                    is_async: false,
                }),
            ))
            .expect("index fits");
        assert_eq!(
            scan(
                &[package(
                    unit.clone(),
                    vec![interface(items.iter().cloned().collect())]
                )],
                &[unit.clone()],
                &target(&unit, interface_index, resource_item)
            ),
            WitResourceOccurrenceSearch::NotFound
        );
    }

    #[test]
    fn terminates_on_cyclic_aliases_without_claiming_an_occurrence() {
        let unit = wasi_unit("io");
        let interface_index = interface_index();
        let alias_a = item_index(1);
        let alias_b = item_index(2);
        let mut items = new_items();
        items.push(resource("stream")).expect("index fits");
        assert_eq!(
            items
                .push(item(
                    "alias-a",
                    WitItemKind::Alias(WitType::Named(reference(
                        &unit,
                        interface_index,
                        alias_b,
                        "streams",
                        "alias-b",
                    ))),
                ))
                .expect("index fits"),
            alias_a
        );
        assert_eq!(
            items
                .push(item(
                    "alias-b",
                    WitItemKind::Alias(WitType::Borrow(reference(
                        &unit,
                        interface_index,
                        alias_a,
                        "streams",
                        "alias-a",
                    ))),
                ))
                .expect("index fits"),
            alias_b
        );
        items
            .push(item(
                "wrapper",
                WitItemKind::Record(vec![WitField {
                    name: "value".to_owned(),
                    docs: None,
                    ty: WitType::Named(reference(
                        &unit,
                        interface_index,
                        alias_a,
                        "streams",
                        "alias-a",
                    )),
                }]),
            ))
            .expect("index fits");
        assert_eq!(
            scan(
                &[package(
                    unit.clone(),
                    vec![interface(items.iter().cloned().collect())]
                )],
                &[unit.clone()],
                &target(&unit, interface_index, item_index(0))
            ),
            WitResourceOccurrenceSearch::NotFound
        );
    }

    #[test]
    fn preserves_known_occurrences_after_opaque_types_and_reports_unknown_only_scans() {
        let unit = wasi_unit("io");
        let interface_index = interface_index();
        let mut items = new_items();
        let resource_item = items.push(resource("stream")).expect("index fits");
        let wrapper = items
            .push(item(
                "wrapper",
                WitItemKind::Record(vec![WitField {
                    name: "opaque".to_owned(),
                    docs: None,
                    ty: WitType::Tuple(vec![
                        WitType::Unsupported(Unsupported::Map),
                        WitType::Own(reference(
                            &unit,
                            interface_index,
                            resource_item,
                            "streams",
                            "stream",
                        )),
                    ]),
                }]),
            ))
            .expect("index fits");
        let map_result = scan(
            &[package(
                unit.clone(),
                vec![interface(items.iter().cloned().collect())],
            )],
            &[unit.clone()],
            &target(&unit, interface_index, resource_item),
        );
        let WitResourceOccurrenceSearch::Found(map_occurrence) = map_result else {
            panic!("a later reference still proves an invalid binding");
        };
        assert_eq!(
            map_occurrence.site,
            WitResourceOccurrenceSite::RecordField {
                index: 0,
                name: "opaque".to_owned()
            }
        );
        assert!(
            !map_occurrence.first_occurrence_known,
            "the opaque map may contain an earlier occurrence"
        );

        let mut map_only_items = new_items();
        let map_only_resource = map_only_items.push(resource("stream")).expect("index fits");
        map_only_items
            .push(item(
                "wrapper",
                WitItemKind::Record(vec![WitField {
                    name: "opaque".to_owned(),
                    docs: None,
                    ty: WitType::Unsupported(Unsupported::Map),
                }]),
            ))
            .expect("index fits");
        let map_only_result = scan(
            &[package(
                unit.clone(),
                vec![interface(map_only_items.iter().cloned().collect())],
            )],
            &[unit.clone()],
            &target(&unit, interface_index, map_only_resource),
        );
        assert!(matches!(
            map_only_result,
            WitResourceOccurrenceSearch::Incomplete(
                WitResourceOccurrenceIncomplete::UnsupportedType {
                    unsupported: Unsupported::Map,
                    ..
                }
            )
        ));

        let mut fixed_items = new_items();
        let fixed_resource = fixed_items.push(resource("stream")).expect("index fits");
        fixed_items
            .push(item(
                "wrapper",
                WitItemKind::Record(vec![WitField {
                    name: "opaque".to_owned(),
                    docs: None,
                    ty: WitType::Unsupported(Unsupported::FixedList),
                }]),
            ))
            .expect("index fits");
        let fixed_result = scan(
            &[package(
                unit.clone(),
                vec![interface(fixed_items.iter().cloned().collect())],
            )],
            &[unit.clone()],
            &target(&unit, interface_index, fixed_resource),
        );
        assert!(matches!(
            fixed_result,
            WitResourceOccurrenceSearch::Incomplete(
                WitResourceOccurrenceIncomplete::UnsupportedType {
                    unsupported: Unsupported::FixedList,
                    ..
                }
            )
        ));
        assert_eq!(wrapper.index(), 1);
    }
}
