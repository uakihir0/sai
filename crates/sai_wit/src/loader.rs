//! In-memory loading of a WIT package and its reachable dependency closure.
//!
//! Callers provide source files lazily. This lets tracked callers read only
//! inputs for packages actually reached from the requested unit.

use crate::{
    WitClosureError, WitModelError, WitPackage, WitPackageGraph, WitPackageGraphError,
    WitParseError, WitReferenceResolutionError, WitResolveContext, WitSourceFile, WitUnit,
    parse_source_files, resolve_package_references, wit_unit_closure,
};
use sai_stable_hash::{StableHash, StableHasher};
use std::collections::HashMap;
use std::fmt;
use wit_parser::Resolve;

/// A loaded package and the parser-independent models for its dependencies.
///
/// Dependency models and `closure` are in dependency-first post-order. The
/// closure includes the requested root as its final unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedWitPackage {
    root: WitPackage,
    dependency_models: Vec<WitPackage>,
    closure: Vec<WitUnit>,
    direct_dependencies: HashMap<WitUnit, Vec<WitUnit>>,
}

impl LoadedWitPackage {
    /// Returns the requested package model.
    pub fn root(&self) -> &WitPackage {
        &self.root
    }

    /// Returns reachable dependency models in dependency-first post-order.
    pub fn dependency_models(&self) -> &[WitPackage] {
        &self.dependency_models
    }

    /// Returns reachable units in dependency-first post-order, ending with
    /// the requested root unit.
    pub fn closure(&self) -> &[WitUnit] {
        &self.closure
    }

    /// Returns the first-reference ordered direct dependencies of a reachable
    /// unit. Unknown or unreachable units return `None`.
    pub fn direct_dependencies(&self, unit: &WitUnit) -> Option<&[WitUnit]> {
        self.direct_dependencies.get(unit).map(Vec::as_slice)
    }

    /// Consumes the result into its root, dependency models, and closure.
    pub fn into_parts(self) -> (WitPackage, Vec<WitPackage>, Vec<WitUnit>) {
        (self.root, self.dependency_models, self.closure)
    }
}

impl StableHash for LoadedWitPackage {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xF1);
        self.root.stable_hash(hasher);
        self.dependency_models.stable_hash(hasher);
        self.closure.stable_hash(hasher);

        // Hash-map iteration is intentionally avoided. Closure order is
        // deterministic, and each dependency list preserves first-reference
        // order.
        hasher.write_len(u64::try_from(self.closure.len()).unwrap_or(u64::MAX));
        for unit in &self.closure {
            unit.stable_hash(hasher);
            self.direct_dependencies
                .get(unit)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .stable_hash(hasher);
        }
    }
}

/// The cause of a failure while loading a WIT package closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageLoadFailure<E> {
    /// The caller could not supply source files for this unit.
    Source(E),
    /// The supplied source files did not parse.
    Parse(WitParseError),
    /// The active package visibility graph was invalid.
    Graph(WitPackageGraphError),
    /// A package reference did not resolve through the active graph.
    ReferenceResolution(WitReferenceResolutionError),
    /// The discovered dependency closure was cyclic or incomplete.
    Closure(WitClosureError<WitUnit>),
    /// A discovered package was unexpectedly absent from the parsed worklist.
    MissingResolvedPackage(WitUnit),
    /// The computed closure did not end with the requested root.
    ClosureDoesNotEndAtRoot,
    /// A parsed package could not be copied into the resolved model.
    Model(WitModelError),
}

/// A failure tied to the unit whose source or model failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageLoadError<E> {
    unit: WitUnit,
    failure: WitPackageLoadFailure<E>,
}

impl<E> WitPackageLoadError<E> {
    /// Returns the unit whose own loading failed.
    pub fn unit(&self) -> &WitUnit {
        &self.unit
    }

    /// Returns the typed loading failure.
    pub fn failure(&self) -> &WitPackageLoadFailure<E> {
        &self.failure
    }
}

impl<E: fmt::Display> fmt::Display for WitPackageLoadError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to load WIT unit `{}`: ",
            self.unit.canonical_name()
        )?;
        match &self.failure {
            WitPackageLoadFailure::Source(error) => error.fmt(formatter),
            WitPackageLoadFailure::Parse(error) => error.fmt(formatter),
            WitPackageLoadFailure::Graph(error) => error.fmt(formatter),
            WitPackageLoadFailure::ReferenceResolution(error) => error.fmt(formatter),
            WitPackageLoadFailure::Closure(error) => error.fmt(formatter),
            WitPackageLoadFailure::MissingResolvedPackage(unit) => write!(
                formatter,
                "resolved unit `{}` is missing from the closure worklist",
                unit.canonical_name()
            ),
            WitPackageLoadFailure::ClosureDoesNotEndAtRoot => {
                formatter.write_str("dependency closure does not end with its requested root")
            }
            WitPackageLoadFailure::Model(error) => error.fmt(formatter),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for WitPackageLoadError<E> {}

/// Loads one package plus the dependency closure found in its WIT references.
///
/// `source_files` is called once per reachable unit. References are resolved
/// against `graph` before dependencies are visited, then all package models
/// are pushed into one short-lived `wit_parser::Resolve` in dependency-first
/// order. The first failing unit is retained so callers can report its error
/// only from that unit's own check query.
pub fn load_wit_package_closure<E>(
    root: WitUnit,
    graph: &WitPackageGraph,
    mut source_files: impl FnMut(&WitUnit) -> Result<Vec<WitSourceFile>, E>,
) -> Result<LoadedWitPackage, WitPackageLoadError<E>> {
    if root.stable_key() != graph.root().stable_key() {
        return Err(WitPackageLoadError {
            unit: root.clone(),
            failure: WitPackageLoadFailure::Graph(WitPackageGraphError::MissingRootUnit { root }),
        });
    }
    if root.canonical_name() != graph.root().canonical_name() {
        let selected = graph
            .nodes()
            .iter()
            .find(|node| node.unit() == graph.root())
            .expect("a validated package graph contains its root")
            .clone();
        return Err(WitPackageLoadError {
            unit: root.clone(),
            failure: WitPackageLoadFailure::Graph(WitPackageGraphError::RootUnitVersionMismatch {
                root,
                selected,
            }),
        });
    }

    let mut parsed_packages = HashMap::new();
    let mut direct_dependencies = HashMap::<WitUnit, Vec<WitUnit>>::new();
    let mut pending = vec![root.clone()];

    while let Some(unit) = pending.pop() {
        if direct_dependencies.contains_key(&unit) {
            continue;
        }
        let files = source_files(&unit).map_err(|failure| WitPackageLoadError {
            unit: unit.clone(),
            failure: WitPackageLoadFailure::Source(failure),
        })?;
        let parsed =
            parse_source_files(unit.clone(), files).map_err(|error| WitPackageLoadError {
                unit: unit.clone(),
                failure: WitPackageLoadFailure::Parse(error),
            })?;
        let resolved =
            resolve_package_references(parsed, graph).map_err(|error| WitPackageLoadError {
                unit: unit.clone(),
                failure: WitPackageLoadFailure::ReferenceResolution(error),
            })?;
        let (parsed, dependencies) = resolved.into_parts();
        for dependency in dependencies.iter().rev() {
            pending.push(dependency.clone());
        }
        direct_dependencies.insert(unit.clone(), dependencies);
        parsed_packages.insert(unit, parsed);
    }

    let closure = wit_unit_closure(root.clone(), &direct_dependencies).map_err(|error| {
        WitPackageLoadError {
            unit: root.clone(),
            failure: WitPackageLoadFailure::Closure(error),
        }
    })?;
    if closure.last() != Some(&root) {
        return Err(WitPackageLoadError {
            unit: root.clone(),
            failure: WitPackageLoadFailure::ClosureDoesNotEndAtRoot,
        });
    }

    let mut resolve = Resolve::default();
    let mut context = WitResolveContext::default();
    let mut root_model = None;
    let mut dependency_models = Vec::with_capacity(closure.len().saturating_sub(1));
    for unit in &closure {
        let parsed = parsed_packages
            .remove(unit)
            .ok_or_else(|| WitPackageLoadError {
                unit: unit.clone(),
                failure: WitPackageLoadFailure::MissingResolvedPackage(unit.clone()),
            })?;
        let model = context
            .push_package(&mut resolve, parsed)
            .map_err(|error| WitPackageLoadError {
                unit: unit.clone(),
                failure: WitPackageLoadFailure::Model(error),
            })?;
        if unit == &root {
            root_model = Some(model);
        } else {
            dependency_models.push(model);
        }
    }
    let root_model = root_model.ok_or_else(|| WitPackageLoadError {
        unit: root.clone(),
        failure: WitPackageLoadFailure::ClosureDoesNotEndAtRoot,
    })?;

    Ok(LoadedWitPackage {
        root: root_model,
        dependency_models,
        closure,
        direct_dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WitPackageEdge, WitPackageNode, WitUnitKind};
    use sai_intern::{Interner, PackageInterner};
    use sai_package::{mvs::MvsPackageVersion, version::Version};

    fn package_unit(
        identity: &str,
        version: &str,
        packages: &PackageInterner,
    ) -> (WitUnit, MvsPackageVersion) {
        let selected =
            MvsPackageVersion::new(identity, Version::parse(version).expect("valid version"))
                .expect("valid selected package");
        let package_key = format!("{}#{}", identity, selected.version().compatibility_line());
        let package = packages.intern(&package_key).expect("valid package key");
        (
            WitUnit::dir(package, packages, &selected).expect("valid directory unit"),
            selected,
        )
    }

    fn wasi_unit(name: &str, symbols: &Interner) -> WitUnit {
        let symbol = symbols.intern(name).expect("valid symbol");
        WitUnit::wasi(
            symbol,
            symbols,
            &Version::parse(crate::EMBEDDED_WASI_VERSION).expect("pinned version"),
        )
        .expect("known embedded unit")
    }

    #[test]
    fn loads_reachable_models_in_first_reference_post_order() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let (root, root_version) = package_unit("acme:root", "1.2.3", &packages);
        let (dependency, dependency_version) = package_unit("acme:dependency", "2.0.0", &packages);
        let wasi_io = wasi_unit("io", &symbols);
        let nodes = [
            WitPackageNode::new(root.clone(), root_version).expect("root node"),
            WitPackageNode::new(dependency.clone(), dependency_version.clone())
                .expect("dependency node"),
            WitPackageNode::wasi(wasi_io.clone()).expect("WASI node"),
        ];
        let edges = [WitPackageEdge::new(
            root.clone(),
            "acme:dependency",
            dependency_version,
            Some(dependency.clone()),
        )];
        let graph = WitPackageGraph::new(root.clone(), nodes, edges).expect("valid package graph");
        let loaded = load_wit_package_closure(root.clone(), &graph, |unit| {
            let source = if unit == &root {
                "package acme:root@1.2.3;\ninterface client { use acme:dependency/types.{value}; }\nworld root {}"
            } else if unit == &dependency {
                "package acme:dependency@2.0.0;\ninterface types { record value { n: u32 } }\nworld dependency {}"
            } else if unit == &wasi_io {
                "package wasi:io@0.2.3;\ninterface streams {}\nworld io {}"
            } else {
                panic!("unexpected reachable package {}", unit.canonical_name());
            };
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "package.wit",
                source.as_bytes().to_vec(),
            )])
        })
        .expect("closure loads");

        assert_eq!(loaded.root().unit, root);
        assert_eq!(loaded.closure().last(), Some(&root));
        assert_eq!(loaded.dependency_models().len(), 1);
        assert_eq!(loaded.dependency_models()[0].unit, dependency);
        assert_eq!(
            loaded.direct_dependencies(&root),
            Some([dependency].as_slice())
        );
        assert!(matches!(loaded.root().unit.kind(), WitUnitKind::Dir { .. }));
    }

    #[test]
    fn attributes_source_failures_to_the_unit_whose_inputs_failed() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.2.3", &packages);
        let (dependency, dependency_version) = package_unit("acme:dependency", "2.0.0", &packages);
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                WitPackageNode::new(root.clone(), root_version).expect("root node"),
                WitPackageNode::new(dependency.clone(), dependency_version.clone())
                    .expect("dependency node"),
            ],
            [WitPackageEdge::new(
                root.clone(),
                "acme:dependency",
                dependency_version,
                Some(dependency.clone()),
            )],
        )
        .expect("valid package graph");
        let error = load_wit_package_closure(root.clone(), &graph, |unit| {
            if unit == &root {
                Ok::<_, &'static str>(vec![WitSourceFile::new(
                    "root.wit",
                    b"package acme:root@1.2.3;\ninterface client { use acme:dependency/types.{value}; }\nworld root {}"
                        .to_vec(),
                )])
            } else {
                Err("dependency input unavailable")
            }
        })
        .expect_err("dependency input fails");

        assert_eq!(error.unit(), &dependency);
        assert_eq!(
            error.failure(),
            &WitPackageLoadFailure::Source("dependency input unavailable")
        );
    }

    #[test]
    fn rejects_absent_and_stale_roots_before_reading_sources() {
        let packages = PackageInterner::new();
        let (selected_root, selected_version) = package_unit("acme:root", "1.2.4", &packages);
        let graph = WitPackageGraph::new(
            selected_root.clone(),
            [WitPackageNode::new(selected_root.clone(), selected_version)
                .expect("selected root node")],
            [],
        )
        .expect("valid graph");
        let (stale_root, _) = package_unit("acme:root", "1.2.3", &packages);
        let (absent_root, _) = package_unit("acme:absent", "1.0.0", &packages);

        for root in [stale_root, absent_root] {
            let mut source_reads = 0;
            let error = load_wit_package_closure(root.clone(), &graph, |_unit| {
                source_reads += 1;
                Ok::<_, &'static str>(vec![WitSourceFile::new(
                    "root.wit",
                    b"package acme:root@1.2.4;\nworld root {}".to_vec(),
                )])
            })
            .expect_err("the requested root is not the graph's exact root");
            assert_eq!(error.unit(), &root);
            assert_eq!(source_reads, 0, "invalid roots do not read source inputs");
            assert!(matches!(error.failure(), WitPackageLoadFailure::Graph(_)));
        }
    }

    #[test]
    fn visits_sibling_and_diamond_dependencies_once_in_reference_order() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.0.0", &packages);
        let (first, first_version) = package_unit("acme:first", "1.0.0", &packages);
        let (second, second_version) = package_unit("acme:second", "1.0.0", &packages);
        let (shared, shared_version) = package_unit("acme:shared", "1.0.0", &packages);
        let graph = WitPackageGraph::new(
            root.clone(),
            [
                WitPackageNode::new(root.clone(), root_version).expect("root node"),
                WitPackageNode::new(first.clone(), first_version.clone()).expect("first node"),
                WitPackageNode::new(second.clone(), second_version.clone()).expect("second node"),
                WitPackageNode::new(shared.clone(), shared_version.clone()).expect("shared node"),
            ],
            [
                WitPackageEdge::new(
                    root.clone(),
                    "acme:first",
                    first_version,
                    Some(first.clone()),
                ),
                WitPackageEdge::new(
                    root.clone(),
                    "acme:second",
                    second_version,
                    Some(second.clone()),
                ),
                WitPackageEdge::new(
                    first.clone(),
                    "acme:shared",
                    shared_version.clone(),
                    Some(shared.clone()),
                ),
                WitPackageEdge::new(
                    second.clone(),
                    "acme:shared",
                    shared_version,
                    Some(shared.clone()),
                ),
            ],
        )
        .expect("valid diamond graph");
        let mut reads = Vec::new();
        let loaded = load_wit_package_closure(root.clone(), &graph, |unit| {
            reads.push(unit.clone());
            let source = if unit == &root {
                "package acme:root@1.0.0;\nworld app { import acme:first/api; import acme:second/api; }"
            } else if unit == &first {
                "package acme:first@1.0.0;\ninterface api {}\nworld first { import acme:shared/api; }"
            } else if unit == &second {
                "package acme:second@1.0.0;\ninterface api {}\nworld second { import acme:shared/api; }"
            } else if unit == &shared {
                "package acme:shared@1.0.0;\ninterface api {}\nworld shared {}"
            } else {
                panic!("unexpected unit {}", unit.canonical_name());
            };
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "package.wit",
                source.as_bytes().to_vec(),
            )])
        })
        .expect("diamond closure loads");

        assert_eq!(
            reads,
            [root.clone(), first.clone(), shared.clone(), second.clone()]
        );
        assert_eq!(
            loaded.closure(),
            [shared, first, second, root].as_slice(),
            "dependency closure follows first-reference DFS post-order"
        );
    }
}
