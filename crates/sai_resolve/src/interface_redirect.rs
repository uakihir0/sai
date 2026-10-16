//! Lookup tables for the interface-item redirects of impl/16 §16.7.4.
//!
//! This module only builds the redirect candidates. Applying them to owner
//! path resolutions is a separate resolver step.

use crate::interface_impls::InterfaceImpls;
use sai_hir::DefId;
use std::collections::HashMap;

/// Accepted interface-item targets for one package.
///
/// These maps are lookup-only: their iteration order is not observable.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct InterfaceRedirects {
    functions: HashMap<DefId, DefId>,
    resources: HashMap<DefId, DefId>,
}

impl InterfaceRedirects {
    /// Builds redirects from the first impl selected for each interface.
    ///
    /// Matched functions remain available even when signature comparison is
    /// suppressed, so later analysis can continue with the name-matched
    /// implementation and report the original generic restriction separately.
    /// Resources come only from `resources`, which contains bindings accepted
    /// by §16.7.3; unresolved or rejected entries in `resource_bindings` never
    /// become redirects.
    pub fn from_impls(impls: &InterfaceImpls) -> Self {
        let mut redirects = Self::default();

        for implementation in &impls.impls {
            let Some(interface) = implementation.interface else {
                continue;
            };
            if impls.by_interface.get(&interface) != Some(&implementation.impl_def) {
                continue;
            }

            for function in &implementation.fns {
                // The collector omits unsupported functions from `fns`.
                // Keep this guard as a defensive invariant for constructed
                // InterfaceImpls values as well.
                if implementation.unsupported.contains(&function.interface_fn) {
                    continue;
                }
                redirects
                    .functions
                    .entry(function.interface_fn)
                    .or_insert_with(|| function.impl_fn.to_def_id());
            }

            for resource in &implementation.resources {
                redirects
                    .resources
                    .entry(resource.resource)
                    .or_insert(resource.implementation_type);
            }
        }

        redirects
    }

    /// Returns the implementation function for a matched interface function.
    pub fn function_target(&self, interface_fn: DefId) -> Option<DefId> {
        self.functions.get(&interface_fn).copied()
    }

    /// Returns the accepted local implementation resource for an interface resource.
    pub fn resource_target(&self, resource: DefId) -> Option<DefId> {
        self.resources.get(&resource).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_impls::{
        InterfaceImpl, InterfaceImplFunction, InterfaceImplResource, InterfaceImplResourceBinding,
        InterfaceImplTypeResolution,
    };
    use sai_diagnostics::Location;
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, LocalDefId};
    use sai_intern::{Interner, PackageId, PackageInterner};
    use sai_vfs::{FileId, Span, TextRange, TextSize};
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
                .intern("acme:redirects#1")
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
                .expect("path count fits");
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
            self.paths.intern(path).expect("local definition interns")
        }
    }

    fn span(start: u32, end: u32) -> Span {
        Span::new(
            FileId::new(1),
            TextRange::new(TextSize::new(start), TextSize::new(end)).expect("test span is ordered"),
        )
    }

    fn resource_binding_candidate(
        resource: DefId,
        resolution: InterfaceImplTypeResolution,
    ) -> InterfaceImplResourceBinding {
        InterfaceImplResourceBinding {
            resource,
            implementation_type: resolution,
            type_location: Location::new("src/lib.sai", span(10, 20)),
            binding_span: span(5, 21),
            binding_location: Location::new("src/lib.sai", span(5, 21)),
        }
    }

    fn implementation(
        impl_def: LocalDefId,
        interface: Option<DefId>,
        fns: Vec<InterfaceImplFunction>,
        resource_bindings: Vec<InterfaceImplResourceBinding>,
        resources: Vec<InterfaceImplResource>,
        unsupported: Vec<DefId>,
    ) -> InterfaceImpl {
        InterfaceImpl {
            impl_def,
            interface,
            fns,
            resource_bindings,
            resources,
            unsupported,
        }
    }

    fn impls(records: Vec<InterfaceImpl>, selected: &[(DefId, LocalDefId)]) -> InterfaceImpls {
        InterfaceImpls {
            impls: records,
            by_interface: selected.iter().copied().collect(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn builds_function_and_accepted_resource_lookups() {
        let mut fixture = Fixture::new();
        let interface = fixture.local("interface").to_def_id();
        let interface_fn = fixture.local("interface-fn").to_def_id();
        let impl_fn = fixture.local("impl-fn");
        let interface_resource = fixture.local("interface-resource").to_def_id();
        let implementation_resource = fixture.local("implementation-resource").to_def_id();
        let impl_def = fixture.local("impl");

        let result = impls(
            vec![implementation(
                impl_def,
                Some(interface),
                vec![InterfaceImplFunction {
                    interface_fn,
                    impl_fn,
                    compare_signature: true,
                }],
                Vec::new(),
                vec![InterfaceImplResource {
                    resource: interface_resource,
                    implementation_type: implementation_resource,
                    binding_span: span(5, 21),
                }],
                Vec::new(),
            )],
            &[(interface, impl_def)],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(
            redirects.function_target(interface_fn),
            Some(impl_fn.to_def_id())
        );
        assert_eq!(
            redirects.resource_target(interface_resource),
            Some(implementation_resource)
        );
        assert_eq!(
            redirects.function_target(fixture.local("unmatched-function").to_def_id()),
            None
        );
        assert_eq!(
            redirects.resource_target(fixture.local("unmatched-resource").to_def_id()),
            None
        );
    }

    #[test]
    fn later_duplicate_impl_contributes_no_redirects() {
        let mut fixture = Fixture::new();
        let interface = fixture.local("interface").to_def_id();
        let interface_fn = fixture.local("interface-fn").to_def_id();
        let first_fn = fixture.local("first-fn");
        let later_fn = fixture.local("later-fn");
        let interface_resource = fixture.local("interface-resource").to_def_id();
        let first_resource = fixture.local("first-resource");
        let later_resource = fixture.local("later-resource");
        let first_impl = fixture.local("first-impl");
        let later_impl = fixture.local("later-impl");

        let result = impls(
            vec![
                implementation(
                    first_impl,
                    Some(interface),
                    vec![InterfaceImplFunction {
                        interface_fn,
                        impl_fn: first_fn,
                        compare_signature: true,
                    }],
                    Vec::new(),
                    vec![InterfaceImplResource {
                        resource: interface_resource,
                        implementation_type: first_resource.to_def_id(),
                        binding_span: span(5, 21),
                    }],
                    Vec::new(),
                ),
                implementation(
                    later_impl,
                    Some(interface),
                    vec![InterfaceImplFunction {
                        interface_fn,
                        impl_fn: later_fn,
                        compare_signature: true,
                    }],
                    Vec::new(),
                    vec![InterfaceImplResource {
                        resource: interface_resource,
                        implementation_type: later_resource.to_def_id(),
                        binding_span: span(25, 41),
                    }],
                    Vec::new(),
                ),
            ],
            &[(interface, first_impl)],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(
            redirects.function_target(interface_fn),
            Some(first_fn.to_def_id())
        );
        assert_eq!(
            redirects.resource_target(interface_resource),
            Some(first_resource.to_def_id())
        );
        assert_ne!(
            redirects.function_target(interface_fn),
            Some(later_fn.to_def_id())
        );
        assert_ne!(
            redirects.resource_target(interface_resource),
            Some(later_resource.to_def_id())
        );
    }

    #[test]
    fn rejected_and_unresolved_resource_candidates_do_not_redirect() {
        let mut fixture = Fixture::new();
        let interface = fixture.local("interface").to_def_id();
        let rejected_resource = fixture.local("rejected-resource").to_def_id();
        let unresolved_resource = fixture.local("unresolved-resource").to_def_id();
        let plausible_target = fixture.local("plausible-target").to_def_id();
        let accepted_resource = fixture.local("accepted-resource").to_def_id();
        let accepted_target = fixture.local("accepted-target").to_def_id();
        let impl_def = fixture.local("impl");

        let result = impls(
            vec![implementation(
                impl_def,
                Some(interface),
                Vec::new(),
                vec![
                    resource_binding_candidate(
                        rejected_resource,
                        InterfaceImplTypeResolution::Definition(plausible_target),
                    ),
                    resource_binding_candidate(
                        unresolved_resource,
                        InterfaceImplTypeResolution::Unresolved,
                    ),
                ],
                vec![InterfaceImplResource {
                    resource: accepted_resource,
                    implementation_type: accepted_target,
                    binding_span: span(25, 41),
                }],
                Vec::new(),
            )],
            &[(interface, impl_def)],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(redirects.resource_target(rejected_resource), None);
        assert_eq!(redirects.resource_target(unresolved_resource), None);
        assert_eq!(
            redirects.resource_target(accepted_resource),
            Some(accepted_target)
        );
    }

    #[test]
    fn generic_restriction_suppresses_comparison_but_keeps_function_redirect() {
        let mut fixture = Fixture::new();
        let interface = fixture.local("interface").to_def_id();
        let interface_fn = fixture.local("interface-fn").to_def_id();
        let impl_fn = fixture.local("impl-fn");
        let impl_def = fixture.local("impl");
        let result = impls(
            vec![implementation(
                impl_def,
                Some(interface),
                vec![InterfaceImplFunction {
                    interface_fn,
                    impl_fn,
                    compare_signature: false,
                }],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )],
            &[(interface, impl_def)],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(
            redirects.function_target(interface_fn),
            Some(impl_fn.to_def_id())
        );
    }

    #[test]
    fn explicitly_unsupported_function_is_not_redirected() {
        let mut fixture = Fixture::new();
        let interface = fixture.local("interface").to_def_id();
        let interface_fn = fixture.local("unsupported-interface-fn").to_def_id();
        let impl_fn = fixture.local("impl-fn");
        let impl_def = fixture.local("impl");
        let result = impls(
            vec![implementation(
                impl_def,
                Some(interface),
                vec![InterfaceImplFunction {
                    interface_fn,
                    impl_fn,
                    compare_signature: true,
                }],
                Vec::new(),
                Vec::new(),
                vec![interface_fn],
            )],
            &[(interface, impl_def)],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(redirects.function_target(interface_fn), None);
    }

    #[test]
    fn impl_record_without_interface_is_not_redirected() {
        let mut fixture = Fixture::new();
        let interface_fn = fixture.local("interface-fn").to_def_id();
        let impl_fn = fixture.local("impl-fn");
        let impl_def = fixture.local("impl");
        let result = impls(
            vec![implementation(
                impl_def,
                None,
                vec![InterfaceImplFunction {
                    interface_fn,
                    impl_fn,
                    compare_signature: true,
                }],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )],
            &[],
        );

        let redirects = InterfaceRedirects::from_impls(&result);
        assert_eq!(redirects.function_target(interface_fn), None);
    }
}
