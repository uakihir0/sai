//! Ordered collection of resolved interface implementation items.
//!
//! This module accepts only resolved IDs, canonical SAI names, and source
//! locations. It does not inspect HIR or resolve the target/type paths; those
//! are supplied by the caller.

use sai_diagnostics::{Code, Diagnostic, Location, Related, Severity, Value};
use sai_hir::{DefId, LocalDefId};
use sai_vfs::Span;
use sai_wit::Unsupported;
use std::collections::{HashMap, HashSet};

/// Resolution state for one interface-impl target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterfaceImplTarget {
    /// The path resolved to an interface with its members in WIT order.
    Interface(InterfaceImplInterface),
    /// The path resolved successfully, but its definition is not an interface.
    ///
    /// The existing resolved-path diagnostic owner reports
    /// `IMPL_TARGET_NOT_INTERFACE`; this collector only preserves the state
    /// and skips the impl items.
    WrongKind,
    /// Name or member resolution already owns this target's error.
    Unresolved,
}

/// A resolved interface's functions and resources in declaration/WIT order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplInterface {
    pub def: DefId,
    pub items: Vec<InterfaceImplInterfaceItem>,
}

/// A function or resource declared by an interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplInterfaceItem {
    pub def: DefId,
    /// Canonical SAI spelling used for matching.
    pub name: String,
    pub kind: InterfaceImplInterfaceItemKind,
}

/// Supported interface item kinds that participate in §16.7.1 matching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceImplInterfaceItemKind {
    Function {
        unsupported: Option<InterfaceImplUnsupportedFunction>,
    },
    Resource,
}

/// Why an interface function is skipped by the matching step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceImplUnsupportedFunction {
    /// A function signature contains a WIT construct not yet supported.
    Wit(Unsupported),
    /// An async function is not implemented before Stage 5.
    AsyncBeforeStage5,
}

/// One resolved source item within an interface impl.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplItem {
    pub def: LocalDefId,
    /// The canonical SAI spelling of the item name.
    pub name: String,
    pub kind: InterfaceImplItemKind,
    /// The name token span, used for extra and repeated item diagnostics.
    pub name_location: Location,
    /// The complete `type R = C` span. Used for resource-binding records.
    pub item_span: Span,
    /// Snapshot-bound location of the complete `type R = C` binding.
    pub item_location: Location,
}

/// Source item kinds that can match an interface function or resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterfaceImplItemKind {
    Function,
    AssociatedType {
        /// Distinguishes an existing resolution error from a resolved
        /// non-definition such as a primitive. Both match by name, while
        /// §16.7.3 diagnoses resolved invalid targets.
        resolution: InterfaceImplTypeResolution,
        /// Snapshot-bound location of the RHS type `C`.
        type_location: Location,
    },
}

/// Resolution state for the RHS of an associated resource binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceImplTypeResolution {
    /// A complete path resolved to a definition. The driver still checks
    /// that it is a top-level resource in the current package.
    Definition(DefId),
    /// The type syntax resolved, but did not resolve to a definition.
    NonDefinition,
    /// Name resolution already owns this path or type error.
    Unresolved,
}

/// One interface impl in package generation order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplInput {
    pub impl_def: LocalDefId,
    pub target: InterfaceImplTarget,
    /// Complete target path location, used for duplicate, unsupported, and
    /// missing-item diagnostics.
    pub target_location: Location,
    /// Impl items in source order.
    pub items: Vec<InterfaceImplItem>,
}

/// Input to [`collect_interface_impls`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplsInput {
    pub package: String,
    /// Only resolved interface impls are represented; preserve generation
    /// order exactly.
    pub impls: Vec<InterfaceImplInput>,
}

/// A matched function from an interface to its implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceImplFunction {
    pub interface_fn: DefId,
    pub impl_fn: LocalDefId,
}

/// A matched interface resource and its resolved implementation type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceImplResource {
    pub resource: DefId,
    pub implementation_type: DefId,
    /// Complete `type R = C` source span.
    pub binding_span: Span,
}

/// A name-matched resource binding candidate before §16.7.3 validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImplResourceBinding {
    pub resource: DefId,
    pub implementation_type: InterfaceImplTypeResolution,
    /// The `C` type span used for `INVALID_RESOURCE_BINDING`.
    pub type_location: Location,
    /// The complete binding span used by later signature/member diagnostics.
    pub binding_span: Span,
    /// The complete source location, used for duplicate-binding notes.
    pub binding_location: Location,
}

/// One collected interface impl. Function/resource vectors preserve the
/// implementation's source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImpl {
    pub impl_def: LocalDefId,
    /// `None` for non-interface and unresolved targets.
    pub interface: Option<DefId>,
    pub fns: Vec<InterfaceImplFunction>,
    /// Resource items matched by name, before §16.7.3 validation.
    pub resource_bindings: Vec<InterfaceImplResourceBinding>,
    /// Resource bindings accepted by §16.7.3 in the driver adapter.
    pub resources: Vec<InterfaceImplResource>,
    /// Unsupported interface functions in interface declaration/WIT order.
    pub unsupported: Vec<DefId>,
}

/// Ordered interface impl records and their target/matching diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceImpls {
    /// One record for each input impl, in package generation order.
    pub impls: Vec<InterfaceImpl>,
    /// First impl for each interface. `HashMap` is used because `DefId` has
    /// session identity and intentionally does not implement `Ord`; callers
    /// must use this as a lookup table, not an iteration order.
    pub by_interface: HashMap<DefId, LocalDefId>,
    /// Diagnostics in impl generation order and spec step order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Collects interface impl records from resolved names and kinds.
///
/// The first resolved impl of an interface wins. A wrong-kind or unresolved
/// target is recorded without checking its items. Target-kind diagnostics
/// remain with the existing resolved-path owner. For a valid first impl,
/// unsupported functions are diagnosed and skipped, then source items are
/// matched by exact canonical SAI name and item kind.
pub fn collect_interface_impls(input: InterfaceImplsInput) -> InterfaceImpls {
    let mut impls = Vec::with_capacity(input.impls.len());
    let mut by_interface = HashMap::new();
    let mut first_target_by_interface = HashMap::new();
    let mut diagnostics = Vec::new();

    for implementation in input.impls {
        let mut record = InterfaceImpl {
            impl_def: implementation.impl_def,
            interface: None,
            fns: Vec::new(),
            resource_bindings: Vec::new(),
            resources: Vec::new(),
            unsupported: Vec::new(),
        };

        let interface = match implementation.target {
            InterfaceImplTarget::WrongKind => {
                impls.push(record);
                continue;
            }
            InterfaceImplTarget::Unresolved => {
                impls.push(record);
                continue;
            }
            InterfaceImplTarget::Interface(interface) => interface,
        };
        record.interface = Some(interface.def);

        if by_interface.contains_key(&interface.def) {
            let earlier_target = first_target_by_interface
                .get(&interface.def)
                .expect("by_interface has a corresponding first target");
            diagnostics.push(duplicate_interface_impl_diagnostic(
                &input.package,
                &implementation.target_location,
                earlier_target,
            ));
            impls.push(record);
            continue;
        }
        by_interface.insert(interface.def, implementation.impl_def);
        first_target_by_interface.insert(interface.def, implementation.target_location.clone());

        let mut interface_items_by_name = HashMap::with_capacity(interface.items.len());
        let mut effective_interface_item_indexes = Vec::with_capacity(interface.items.len());
        for (index, item) in interface.items.iter().enumerate() {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                interface_items_by_name.entry(item.name.as_str())
            {
                entry.insert(index);
                effective_interface_item_indexes.push(index);
            }
        }

        let mut matched_interface_items = HashSet::new();
        for &index in &effective_interface_item_indexes {
            let item = &interface.items[index];
            if let InterfaceImplInterfaceItemKind::Function {
                unsupported: Some(reason),
            } = item.kind
            {
                record.unsupported.push(item.def);
                diagnostics.push(unsupported_interface_function_diagnostic(
                    &input.package,
                    &implementation.target_location,
                    &item.name,
                    reason,
                ));
                continue;
            }
        }

        let mut first_item_by_name = HashMap::<&str, &Location>::new();
        for item in &implementation.items {
            if let Some(earlier_location) = first_item_by_name.get(item.name.as_str()) {
                diagnostics.push(repeated_interface_impl_item_diagnostic(
                    &input.package,
                    &item.name_location,
                    earlier_location,
                    &item.name,
                ));
                continue;
            }
            first_item_by_name.insert(&item.name, &item.name_location);

            let Some(&interface_index) = interface_items_by_name.get(item.name.as_str()) else {
                diagnostics.push(extra_interface_impl_item_diagnostic(
                    &input.package,
                    &item.name_location,
                    &item.name,
                ));
                continue;
            };
            let interface_item = &interface.items[interface_index];
            if matches!(
                interface_item.kind,
                InterfaceImplInterfaceItemKind::Function {
                    unsupported: Some(_)
                }
            ) {
                // The unsupported interface function is ignored by step 4.
                // A same-kind source function is therefore neither mapped
                // nor diagnosed; a different-kind item is still extra.
                if !matches!(&item.kind, InterfaceImplItemKind::Function) {
                    diagnostics.push(extra_interface_impl_item_diagnostic(
                        &input.package,
                        &item.name_location,
                        &item.name,
                    ));
                }
                continue;
            }

            match (interface_item.kind, &item.kind) {
                (
                    InterfaceImplInterfaceItemKind::Function { unsupported: None },
                    InterfaceImplItemKind::Function,
                ) => {
                    matched_interface_items.insert(interface_index);
                    record.fns.push(InterfaceImplFunction {
                        interface_fn: interface_item.def,
                        impl_fn: item.def,
                    });
                }
                (
                    InterfaceImplInterfaceItemKind::Resource,
                    InterfaceImplItemKind::AssociatedType {
                        resolution,
                        type_location,
                    },
                ) => {
                    matched_interface_items.insert(interface_index);
                    record.resource_bindings.push(InterfaceImplResourceBinding {
                        resource: interface_item.def,
                        implementation_type: *resolution,
                        type_location: type_location.clone(),
                        binding_span: item.item_span,
                        binding_location: item.item_location.clone(),
                    });
                }
                _ => {
                    diagnostics.push(extra_interface_impl_item_diagnostic(
                        &input.package,
                        &item.name_location,
                        &item.name,
                    ));
                }
            }
        }

        for &index in &effective_interface_item_indexes {
            let item = &interface.items[index];
            if matched_interface_items.contains(&index) {
                continue;
            }
            match item.kind {
                InterfaceImplInterfaceItemKind::Function {
                    unsupported: Some(_),
                } => {}
                InterfaceImplInterfaceItemKind::Function { unsupported: None } => {
                    diagnostics.push(missing_interface_impl_item_diagnostic(
                        &input.package,
                        &implementation.target_location,
                        &item.name,
                        "function",
                    ));
                }
                InterfaceImplInterfaceItemKind::Resource => {
                    diagnostics.push(missing_interface_impl_item_diagnostic(
                        &input.package,
                        &implementation.target_location,
                        &item.name,
                        "resource binding",
                    ));
                }
            }
        }

        impls.push(record);
    }

    InterfaceImpls {
        impls,
        by_interface,
        diagnostics,
    }
}

fn duplicate_interface_impl_diagnostic(
    package: &str,
    target: &Location,
    earlier_target: &Location,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::DuplicateInterfaceImpl,
        Severity::Error,
        "interface already has an implementation",
        Some(target.clone()),
    );
    diagnostic.primary_label = "duplicate interface implementation".to_owned();
    diagnostic.package = Some(package.to_owned());
    diagnostic
        .push_related(Related::DeclaredHere {
            location: earlier_target.clone(),
            what: "earlier interface implementation target".to_owned(),
        })
        .expect("unbound diagnostic accepts related locations");
    diagnostic
}

fn unsupported_interface_function_diagnostic(
    package: &str,
    target: &Location,
    name: &str,
    reason: InterfaceImplUnsupportedFunction,
) -> Diagnostic {
    let (code, description) = match reason {
        InterfaceImplUnsupportedFunction::Wit(feature) => {
            (Code::WitUnsupportedFeature, format!("{feature:?}"))
        }
        InterfaceImplUnsupportedFunction::AsyncBeforeStage5 => {
            (Code::FeatureNotImplemented, "async".to_owned())
        }
    };
    let mut diagnostic = Diagnostic::new(
        code,
        Severity::Error,
        format!("interface function `{name}` uses unsupported WIT feature `{description}`"),
        Some(target.clone()),
    );
    diagnostic.primary_label = "unsupported interface function".to_owned();
    diagnostic.package = Some(package.to_owned());
    diagnostic
        .data
        .insert("wit_feature".to_owned(), Value::String(description));
    diagnostic
}

fn extra_interface_impl_item_diagnostic(package: &str, item: &Location, name: &str) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::InterfaceImplExtraItem,
        Severity::Error,
        format!("interface implementation item `{name}` is not declared with this kind"),
        Some(item.clone()),
    );
    diagnostic.primary_label = "extra interface implementation item".to_owned();
    diagnostic.package = Some(package.to_owned());
    diagnostic
}

fn repeated_interface_impl_item_diagnostic(
    package: &str,
    item: &Location,
    earlier: &Location,
    name: &str,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::InterfaceImplExtraItem,
        Severity::Error,
        format!("interface implementation item `{name}` is repeated"),
        Some(item.clone()),
    );
    diagnostic.primary_label = "repeated interface implementation item".to_owned();
    diagnostic.package = Some(package.to_owned());
    diagnostic
        .push_related(Related::DeclaredHere {
            location: earlier.clone(),
            what: "earlier interface implementation item".to_owned(),
        })
        .expect("unbound diagnostic accepts related locations");
    diagnostic
}

fn missing_interface_impl_item_diagnostic(
    package: &str,
    target: &Location,
    name: &str,
    kind: &str,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::InterfaceImplMissingItem,
        Severity::Error,
        format!("interface implementation is missing {kind} `{name}`"),
        Some(target.clone()),
    );
    diagnostic.primary_label = "missing interface implementation item".to_owned();
    diagnostic.package = Some(package.to_owned());
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_diagnostics::Related;
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, PackageId};
    use sai_intern::{Interner, PackageInterner};
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::sync::Arc;

    struct Fixture {
        packages: PackageInterner,
        symbols: Interner,
        paths: DefPathInterner,
        package: PackageId,
    }

    impl Fixture {
        fn new() -> Self {
            let packages = PackageInterner::new();
            let package = packages
                .intern("acme:impls#1")
                .expect("package key interns");
            let symbols = Interner::new();
            let paths = DefPathInterner::new(package, &packages, &symbols)
                .expect("definition path interner creates");
            Self {
                packages,
                symbols,
                paths,
                package,
            }
        }

        fn local(&mut self, name: &str) -> LocalDefId {
            let disambiguator = u32::try_from(self.paths.len().expect("path count reads"))
                .expect("test path count fits");
            let path = DefPath::new(
                self.package,
                vec![DisambiguatedDefPathData {
                    data: DefPathData::Test(Arc::from(name)),
                    disambiguator,
                }],
                &self.packages,
                &self.symbols,
            )
            .expect("definition path is valid");
            self.paths
                .intern(path)
                .expect("local definition ID interns")
        }

        fn interface(
            &mut self,
            name: &str,
            items: Vec<InterfaceImplInterfaceItem>,
        ) -> InterfaceImplInterface {
            InterfaceImplInterface {
                def: self.local(name).to_def_id(),
                items,
            }
        }

        fn function(
            &mut self,
            name: &str,
            unsupported: Option<InterfaceImplUnsupportedFunction>,
        ) -> InterfaceImplInterfaceItem {
            InterfaceImplInterfaceItem {
                def: self.local(&format!("interface-fn-{name}")).to_def_id(),
                name: name.to_owned(),
                kind: InterfaceImplInterfaceItemKind::Function { unsupported },
            }
        }

        fn resource(&mut self, name: &str) -> InterfaceImplInterfaceItem {
            InterfaceImplInterfaceItem {
                def: self
                    .local(&format!("interface-resource-{name}"))
                    .to_def_id(),
                name: name.to_owned(),
                kind: InterfaceImplInterfaceItemKind::Resource,
            }
        }

        fn implementation_item(
            &mut self,
            name: &str,
            kind: InterfaceImplItemKind,
            name_start: u32,
            item_start: u32,
            item_end: u32,
        ) -> InterfaceImplItem {
            InterfaceImplItem {
                def: self.local(&format!("impl-item-{name}-{name_start}")),
                name: name.to_owned(),
                kind,
                name_location: location("src/lib.sai", name_start, name_start + name.len() as u32),
                item_span: span(item_start, item_end),
                item_location: location("src/lib.sai", item_start, item_end),
            }
        }

        fn implementation(
            &mut self,
            target: InterfaceImplTarget,
            impl_name: &str,
            target_start: u32,
            items: Vec<InterfaceImplItem>,
        ) -> InterfaceImplInput {
            InterfaceImplInput {
                impl_def: self.local(impl_name),
                target,
                target_location: location("src/lib.sai", target_start, target_start + 10),
                items,
            }
        }
    }

    fn span(start: u32, end: u32) -> Span {
        Span::new(
            FileId::new(1),
            TextRange::new(TextSize::new(start), TextSize::new(end)).expect("test span is ordered"),
        )
    }

    fn location(path: &str, start: u32, end: u32) -> Location {
        Location::new(path, span(start, end))
    }

    fn input(impls: Vec<InterfaceImplInput>) -> InterfaceImplsInput {
        InterfaceImplsInput {
            package: "acme:impls@1.0.0".to_owned(),
            impls,
        }
    }

    #[test]
    fn matches_functions_and_resource_bindings_and_preserves_item_spans() {
        let mut fixture = Fixture::new();
        let run = fixture.function("run", None);
        let buffer = fixture.resource("Buffer");
        let stop = fixture.function("stop", None);
        let run_def = run.def;
        let buffer_def = buffer.def;
        let stop_def = stop.def;
        let interface = fixture.interface("api", vec![run, buffer, stop]);
        let interface_def = interface.def;
        let run_impl =
            fixture.implementation_item("run", InterfaceImplItemKind::Function, 20, 16, 30);
        let run_impl_def = run_impl.def;
        let buffer_impl_target = fixture.local("BufferImpl").to_def_id();
        let buffer_impl = fixture.implementation_item(
            "Buffer",
            InterfaceImplItemKind::AssociatedType {
                resolution: InterfaceImplTypeResolution::Definition(buffer_impl_target),
                type_location: location("src/lib.sai", 53, 65),
            },
            40,
            34,
            55,
        );
        let stop_impl =
            fixture.implementation_item("stop", InterfaceImplItemKind::Function, 65, 61, 75);
        let stop_impl_def = stop_impl.def;
        let implementation = fixture.implementation(
            InterfaceImplTarget::Interface(interface),
            "impl-api",
            100,
            vec![run_impl, buffer_impl, stop_impl],
        );

        let result = collect_interface_impls(input(vec![implementation]));

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.by_interface.get(&interface_def),
            Some(&result.impls[0].impl_def)
        );
        assert_eq!(
            result.impls[0].fns,
            [
                InterfaceImplFunction {
                    interface_fn: run_def,
                    impl_fn: run_impl_def,
                },
                InterfaceImplFunction {
                    interface_fn: stop_def,
                    impl_fn: stop_impl_def,
                },
            ]
        );
        assert_eq!(
            result.impls[0].resource_bindings,
            [InterfaceImplResourceBinding {
                resource: buffer_def,
                implementation_type: InterfaceImplTypeResolution::Definition(buffer_impl_target),
                type_location: location("src/lib.sai", 53, 65),
                binding_span: span(34, 55),
                binding_location: location("src/lib.sai", 34, 55),
            }]
        );
        assert!(
            result.impls[0].resources.is_empty(),
            "the data-only collector leaves resource validity to §16.7.3"
        );
    }

    #[test]
    fn first_interface_impl_wins_and_later_target_items_are_skipped() {
        let mut fixture = Fixture::new();
        let interface_run = fixture.function("run", None);
        let interface = fixture.interface("api", vec![interface_run]);
        let first_target = location("src/lib.sai", 10, 18);
        let first_item =
            fixture.implementation_item("run", InterfaceImplItemKind::Function, 25, 20, 31);
        let first = InterfaceImplInput {
            impl_def: fixture.local("first-impl"),
            target: InterfaceImplTarget::Interface(interface.clone()),
            target_location: first_target.clone(),
            items: vec![first_item],
        };
        let second_target = location("src/lib.sai", 50, 58);
        let second_item =
            fixture.implementation_item("extra", InterfaceImplItemKind::Function, 65, 60, 72);
        let second = InterfaceImplInput {
            impl_def: fixture.local("second-impl"),
            target: InterfaceImplTarget::Interface(interface.clone()),
            target_location: second_target.clone(),
            items: vec![second_item],
        };

        let result = collect_interface_impls(input(vec![first, second]));

        assert_eq!(result.impls.len(), 2);
        assert_eq!(
            result.by_interface.get(&interface.def),
            Some(&result.impls[0].impl_def)
        );
        assert_eq!(result.impls[1].interface, Some(interface.def));
        assert!(result.impls[1].fns.is_empty());
        assert!(result.impls[1].resources.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::DuplicateInterfaceImpl);
        assert_eq!(diagnostic.primary(), Some(&second_target));
        assert!(matches!(
            diagnostic.related(),
            [Related::DeclaredHere { location, .. }] if location == &first_target
        ));
    }

    #[test]
    fn repeated_extra_and_wrong_kind_items_keep_source_order_and_spans() {
        let mut fixture = Fixture::new();
        let interface_run = fixture.function("run", None);
        let interface_buffer = fixture.resource("Buffer");
        let interface_stop = fixture.function("stop", None);
        let interface =
            fixture.interface("api", vec![interface_run, interface_buffer, interface_stop]);
        let target = location("src/lib.sai", 100, 108);
        let extra_first =
            fixture.implementation_item("extra", InterfaceImplItemKind::Function, 10, 6, 19);
        let extra_first_location = extra_first.name_location.clone();
        let extra_repeat =
            fixture.implementation_item("extra", InterfaceImplItemKind::Function, 25, 21, 34);
        let extra_repeat_location = extra_repeat.name_location.clone();
        let run_type = fixture.local("RunType").to_def_id();
        let wrong_kind = fixture.implementation_item(
            "run",
            InterfaceImplItemKind::AssociatedType {
                resolution: InterfaceImplTypeResolution::Definition(run_type),
                type_location: location("src/lib.sai", 28, 36),
            },
            40,
            35,
            50,
        );
        let wrong_kind_location = wrong_kind.name_location.clone();
        let buffer_impl_type = fixture.local("BufferImpl").to_def_id();
        let matched_resource = fixture.implementation_item(
            "Buffer",
            InterfaceImplItemKind::AssociatedType {
                resolution: InterfaceImplTypeResolution::Definition(buffer_impl_type),
                type_location: location("src/lib.sai", 47, 58),
            },
            60,
            55,
            74,
        );
        let implementation = InterfaceImplInput {
            impl_def: fixture.local("impl-api"),
            target: InterfaceImplTarget::Interface(interface),
            target_location: target.clone(),
            items: vec![extra_first, extra_repeat, wrong_kind, matched_resource],
        };

        let result = collect_interface_impls(input(vec![implementation]));

        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                Code::InterfaceImplExtraItem,
                Code::InterfaceImplExtraItem,
                Code::InterfaceImplExtraItem,
                Code::InterfaceImplMissingItem,
                Code::InterfaceImplMissingItem,
            ]
        );
        assert_eq!(result.diagnostics[0].primary(), Some(&extra_first_location));
        assert_eq!(
            result.diagnostics[1].primary(),
            Some(&extra_repeat_location)
        );
        assert!(matches!(
            result.diagnostics[1].related(),
            [Related::DeclaredHere { location, .. }] if location == &extra_first_location
        ));
        assert_eq!(result.diagnostics[2].primary(), Some(&wrong_kind_location));
        assert_eq!(result.diagnostics[3].primary(), Some(&target));
        assert!(result.diagnostics[3].message.contains("`run`"));
        assert_eq!(result.diagnostics[4].primary(), Some(&target));
        assert!(result.diagnostics[4].message.contains("`stop`"));
        assert_eq!(result.impls[0].resource_bindings.len(), 1);
        assert!(result.impls[0].resources.is_empty());
        assert_eq!(result.impls[0].fns.len(), 0);
    }

    #[test]
    fn unsupported_interface_functions_are_reported_and_skipped_from_matching() {
        let mut fixture = Fixture::new();
        let unsupported_map = fixture.function(
            "map_call",
            Some(InterfaceImplUnsupportedFunction::Wit(Unsupported::Map)),
        );
        let unsupported_map_def = unsupported_map.def;
        let unsupported_async = fixture.function(
            "async_read",
            Some(InterfaceImplUnsupportedFunction::AsyncBeforeStage5),
        );
        let unsupported_async_def = unsupported_async.def;
        let supported = fixture.function("ping", None);
        let supported_def = supported.def;
        let interface =
            fixture.interface("api", vec![unsupported_map, unsupported_async, supported]);
        let target = location("src/lib.sai", 100, 108);
        let unsupported_map_impl =
            fixture.implementation_item("map_call", InterfaceImplItemKind::Function, 10, 6, 23);
        let unsupported_async_impl =
            fixture.implementation_item("async_read", InterfaceImplItemKind::Function, 30, 26, 44);
        let unsupported_impl =
            fixture.implementation_item("ping", InterfaceImplItemKind::Function, 50, 46, 58);
        let implementation = InterfaceImplInput {
            impl_def: fixture.local("impl-api"),
            target: InterfaceImplTarget::Interface(interface),
            target_location: target.clone(),
            items: vec![
                unsupported_map_impl,
                unsupported_async_impl,
                unsupported_impl.clone(),
            ],
        };

        let result = collect_interface_impls(input(vec![implementation]));

        assert_eq!(
            result.impls[0].unsupported,
            [unsupported_map_def, unsupported_async_def]
        );
        assert_eq!(
            result.impls[0].fns,
            [InterfaceImplFunction {
                interface_fn: supported_def,
                impl_fn: unsupported_impl.def,
            }]
        );
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::WitUnsupportedFeature, Code::FeatureNotImplemented]
        );
        assert_eq!(result.diagnostics[0].primary(), Some(&target));
        assert!(result.diagnostics[0].message.contains("`map_call`"));
        assert_eq!(result.diagnostics[1].primary(), Some(&target));
        assert!(result.diagnostics[1].message.contains("`async_read`"));
        assert!(
            result
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != Code::InterfaceImplMissingItem),
            "the unsupported function does not create a missing-item cascade"
        );
    }

    #[test]
    fn duplicate_interface_member_names_only_use_the_first_member() {
        let mut fixture = Fixture::new();
        let effective_run = fixture.function("run", None);
        let effective_run_def = effective_run.def;
        let duplicate_unsupported = fixture.function(
            "run",
            Some(InterfaceImplUnsupportedFunction::Wit(Unsupported::Map)),
        );
        let duplicate_resource = fixture.resource("run");
        let interface = fixture.interface(
            "api",
            vec![effective_run, duplicate_unsupported, duplicate_resource],
        );
        let implementation_run =
            fixture.implementation_item("run", InterfaceImplItemKind::Function, 20, 16, 28);
        let implementation_run_def = implementation_run.def;
        let implementation = fixture.implementation(
            InterfaceImplTarget::Interface(interface),
            "impl-api",
            100,
            vec![implementation_run],
        );

        let result = collect_interface_impls(input(vec![implementation]));

        assert_eq!(
            result.impls[0].fns,
            [InterfaceImplFunction {
                interface_fn: effective_run_def,
                impl_fn: implementation_run_def,
            }]
        );
        assert!(result.impls[0].resources.is_empty());
        assert!(result.impls[0].unsupported.is_empty());
        assert!(
            result.diagnostics.is_empty(),
            "later duplicate names must not add unsupported or missing-item diagnostics"
        );
    }

    #[test]
    fn wrong_kind_and_unresolved_targets_are_preserved_without_item_diagnostics() {
        let mut fixture = Fixture::new();
        let item =
            fixture.implementation_item("extra", InterfaceImplItemKind::Function, 20, 16, 28);
        let wrong_target = location("src/lib.sai", 50, 58);
        let wrong_kind = InterfaceImplInput {
            impl_def: fixture.local("wrong-kind-impl"),
            target: InterfaceImplTarget::WrongKind,
            target_location: wrong_target.clone(),
            items: vec![item.clone()],
        };
        let unresolved_target = location("src/lib.sai", 70, 79);
        let unresolved = InterfaceImplInput {
            impl_def: fixture.local("unresolved-impl"),
            target: InterfaceImplTarget::Unresolved,
            target_location: unresolved_target,
            items: vec![item],
        };

        let result = collect_interface_impls(input(vec![wrong_kind, unresolved]));

        assert_eq!(result.impls.len(), 2);
        assert!(result.impls.iter().all(|implementation| {
            implementation.interface.is_none()
                && implementation.fns.is_empty()
                && implementation.resources.is_empty()
                && implementation.unsupported.is_empty()
        }));
        assert!(
            result.diagnostics.is_empty(),
            "IR-619 owns target-kind diagnostics and unresolved paths own their resolution errors"
        );
        assert_eq!(wrong_target.span, span(50, 58));
    }

    #[test]
    fn unresolved_resource_rhs_matches_without_a_missing_item_cascade() {
        let mut fixture = Fixture::new();
        let resource = fixture.resource("Stream");
        let interface = fixture.interface("api", vec![resource]);
        let item = fixture.implementation_item(
            "Stream",
            InterfaceImplItemKind::AssociatedType {
                resolution: InterfaceImplTypeResolution::Unresolved,
                type_location: location("src/lib.sai", 27, 34),
            },
            20,
            15,
            35,
        );
        let implementation = fixture.implementation(
            InterfaceImplTarget::Interface(interface),
            "impl-api",
            100,
            vec![item],
        );

        let result = collect_interface_impls(input(vec![implementation]));

        assert!(result.diagnostics.is_empty());
        assert_eq!(result.impls[0].resource_bindings.len(), 1);
        assert_eq!(
            result.impls[0].resource_bindings[0].implementation_type,
            InterfaceImplTypeResolution::Unresolved
        );
        assert!(result.impls[0].resources.is_empty());
    }
}
