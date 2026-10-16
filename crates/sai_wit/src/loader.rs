//! In-memory loading of a WIT package and its reachable dependency closure.
//!
//! Callers provide source files lazily. This lets tracked callers read only
//! inputs for packages actually reached from the requested unit.

use crate::{
    WitClosureError, WitModelError, WitPackage, WitPackageGraph, WitPackageGraphError,
    WitParseError, WitReferenceResolutionError, WitReferenceResolutionErrorKind, WitResolveContext,
    WitSourceFile, WitSourceLocation, WitUnit, parse_source_files, resolve_package_references,
    wit_unit_closure,
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

/// A registered WIT diagnostic code represented without depending on the
/// diagnostics crate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitPackageDiagnosticCode {
    /// A WIT source failed to parse or validate.
    ParseError,
    /// The declared WIT package identity or version differs from its unit.
    PackageMismatch,
}

impl WitPackageDiagnosticCode {
    /// Returns the canonical diagnostic code name for driver integration.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "WIT_PARSE_ERROR",
            Self::PackageMismatch => "WIT_PACKAGE_MISMATCH",
        }
    }
}

/// The diagnostic severity used by WIT package loading.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitPackageDiagnosticSeverity {
    /// WIT loading failures prevent the package from being used.
    Error,
}

impl WitPackageDiagnosticSeverity {
    /// Returns the canonical severity name for driver integration.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
        }
    }
}

/// A WIT diagnostic that can be bound to the failing unit's current source
/// snapshot by the driver.
///
/// `location.path` is the direct-child path supplied to `wit_parser`, and its
/// range is measured in original source bytes. The template deliberately
/// carries no session-local `FileId` or `SourceRef`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageDiagnosticTemplate {
    unit: WitUnit,
    code: WitPackageDiagnosticCode,
    severity: WitPackageDiagnosticSeverity,
    message: String,
    location: Option<WitSourceLocation>,
}

impl WitPackageDiagnosticTemplate {
    /// Returns the unit that owns this diagnostic.
    pub fn unit(&self) -> &WitUnit {
        &self.unit
    }

    /// Returns the registered diagnostic code.
    pub fn code(&self) -> WitPackageDiagnosticCode {
        self.code
    }

    /// Returns the hard diagnostic severity.
    pub fn severity(&self) -> WitPackageDiagnosticSeverity {
        self.severity
    }

    /// Returns the parser or validation message without rendering a location.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the source path and original-byte range when the failure can
    /// be attributed to one WIT file.
    pub fn location(&self) -> Option<&WitSourceLocation> {
        self.location.as_ref()
    }
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

    /// Converts a WIT content failure to a source-independent diagnostic
    /// template, but only when this error belongs to `requested_unit`.
    ///
    /// Loading a parent can fail because a dependency failed. In that case
    /// this returns `None`; the dependency's own query emits the diagnostic,
    /// preventing the same failure from being reported once per parent.
    ///
    /// Non-content failures (for example, an unavailable input or an invalid
    /// package graph) do not become `WIT_PARSE_ERROR` diagnostics here.
    pub fn diagnostic_template_for(
        &self,
        requested_unit: &WitUnit,
    ) -> Option<WitPackageDiagnosticTemplate> {
        if self.unit != *requested_unit {
            return None;
        }

        let (code, message, location) = match &self.failure {
            WitPackageLoadFailure::Parse(error) => (
                WitPackageDiagnosticCode::ParseError,
                error.to_string(),
                error.location().cloned(),
            ),
            WitPackageLoadFailure::ReferenceResolution(error) => {
                let (code, location) = match error.kind() {
                    WitReferenceResolutionErrorKind::PackageMismatch { .. } => (
                        WitPackageDiagnosticCode::PackageMismatch,
                        error.location().cloned(),
                    ),
                    WitReferenceResolutionErrorKind::NestedPackage
                    | WitReferenceResolutionErrorKind::InvalidVersion(_)
                    | WitReferenceResolutionErrorKind::Reference(_) => (
                        WitPackageDiagnosticCode::ParseError,
                        error.location().cloned(),
                    ),
                    // These variants indicate a broken source-range mapping
                    // or an unexpected failure after rewriting valid source.
                    WitReferenceResolutionErrorKind::InvalidSourceRange
                    | WitReferenceResolutionErrorKind::Reparse(_) => return None,
                };
                (code, error.to_string(), location)
            }
            WitPackageLoadFailure::Model(error) => match error {
                WitModelError::NestedPackages { location, .. } => (
                    WitPackageDiagnosticCode::ParseError,
                    error.to_string(),
                    location.clone(),
                ),
                WitModelError::PackageMismatch { location, .. } => (
                    WitPackageDiagnosticCode::PackageMismatch,
                    error.to_string(),
                    location.clone(),
                ),
                // wit-parser 0.246.0 keeps the semantic error's span in
                // private anyhow context. Its rendered text is not a stable
                // source-location API, so preserve the message only.
                WitModelError::Resolve(_) => (
                    WitPackageDiagnosticCode::ParseError,
                    error.to_string(),
                    None,
                ),
                _ => return None,
            },
            _ => return None,
        };

        Some(WitPackageDiagnosticTemplate {
            unit: self.unit.clone(),
            code,
            severity: WitPackageDiagnosticSeverity::Error,
            message,
            location,
        })
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

    fn reference_resolution_failure(
        unit: WitUnit,
        kind: WitReferenceResolutionErrorKind,
        location: Option<WitSourceLocation>,
    ) -> WitPackageLoadError<()> {
        WitPackageLoadError {
            unit,
            failure: WitPackageLoadFailure::ReferenceResolution(WitReferenceResolutionError {
                kind,
                location,
            }),
        }
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
    fn parser_failure_becomes_a_path_and_byte_range_template() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.0.0", &packages);
        let graph = WitPackageGraph::new(
            root.clone(),
            [WitPackageNode::new(root.clone(), root_version).expect("root node")],
            [],
        )
        .expect("valid package graph");
        let source = "package acme:root@1.0.0;\ninterface {";
        let error = load_wit_package_closure(root.clone(), &graph, |_unit| {
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "broken.wit",
                source.as_bytes().to_vec(),
            )])
        })
        .expect_err("malformed WIT must fail");

        let template = error
            .diagnostic_template_for(&root)
            .expect("the failing root owns its diagnostic");
        assert_eq!(template.unit(), &root);
        assert_eq!(template.code(), WitPackageDiagnosticCode::ParseError);
        assert_eq!(template.severity(), WitPackageDiagnosticSeverity::Error);
        assert_eq!(template.code().as_str(), "WIT_PARSE_ERROR");
        assert_eq!(template.severity().as_str(), "error");
        let location = template.location().expect("parser span is retained");
        assert_eq!(location.path, "broken.wit");
        assert_eq!(&source[location.range.clone()], "{");
    }

    #[test]
    fn package_identity_mismatch_uses_its_registered_diagnostic_code() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.0.0", &packages);
        let graph = WitPackageGraph::new(
            root.clone(),
            [WitPackageNode::new(root.clone(), root_version).expect("root node")],
            [],
        )
        .expect("valid package graph");
        let source = "package acme:other@1.0.0;";
        let error = load_wit_package_closure(root.clone(), &graph, |_unit| {
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "mismatch.wit",
                source.as_bytes().to_vec(),
            )])
        })
        .expect_err("wrong WIT package identity must fail");

        let template = error
            .diagnostic_template_for(&root)
            .expect("the failing root owns its diagnostic");
        assert_eq!(template.code(), WitPackageDiagnosticCode::PackageMismatch);
        let location = template.location().expect("package declaration is located");
        assert_eq!(location.path, "mismatch.wit");
        assert_eq!(&source[location.range.clone()], "acme:other@1.0.0");
    }

    #[test]
    fn user_reference_failures_keep_diagnostic_codes_and_source_locations() {
        let packages = PackageInterner::new();
        let (root, _) = package_unit("acme:root", "1.0.0", &packages);
        let location = WitSourceLocation {
            path: "reference.wit".to_owned(),
            range: 12..19,
        };
        let cases = [
            (
                WitReferenceResolutionErrorKind::NestedPackage,
                WitPackageDiagnosticCode::ParseError,
            ),
            (
                WitReferenceResolutionErrorKind::InvalidVersion("1.2".to_owned()),
                WitPackageDiagnosticCode::ParseError,
            ),
            (
                WitReferenceResolutionErrorKind::Reference(
                    crate::WitPackageReferenceError::UnknownWasiPackage {
                        package_identity: "wasi:missing".into(),
                    },
                ),
                WitPackageDiagnosticCode::ParseError,
            ),
        ];

        for (kind, expected_code) in cases {
            let error = reference_resolution_failure(root.clone(), kind, Some(location.clone()));
            let template = error
                .diagnostic_template_for(&root)
                .expect("user WIT reference errors have diagnostics");
            assert_eq!(template.code(), expected_code);
            assert_eq!(template.location(), Some(&location));
        }
    }

    #[test]
    fn internal_reference_mapping_failures_do_not_become_user_diagnostics() {
        let packages = PackageInterner::new();
        let (root, _) = package_unit("acme:root", "1.0.0", &packages);
        let parse_error = parse_source_files(
            root.clone(),
            [WitSourceFile::new(
                "normalized.wit",
                "package acme:root@1.0.0;\ninterface {",
            )],
        )
        .expect_err("fixture must provide an underlying parser error");
        let location = Some(WitSourceLocation {
            path: "normalized.wit".to_owned(),
            range: 0..1,
        });

        for kind in [
            WitReferenceResolutionErrorKind::InvalidSourceRange,
            WitReferenceResolutionErrorKind::Reparse(parse_error),
        ] {
            let error = reference_resolution_failure(root.clone(), kind, location.clone());
            assert_eq!(
                error.diagnostic_template_for(&root),
                None,
                "internal source-mapping failure must be handled as an implementation error"
            );
        }
    }

    #[test]
    fn resolve_push_semantic_failure_remains_locationless() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.0.0", &packages);
        let graph = WitPackageGraph::new(
            root.clone(),
            [WitPackageNode::new(root.clone(), root_version).expect("root node")],
            [],
        )
        .expect("valid package graph");
        let error = load_wit_package_closure(root.clone(), &graph, |_unit| {
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "semantic.wit",
                b"package acme:root@1.0.0;\n@since(version = 2.0.0) interface api {}".to_vec(),
            )])
        })
        .expect_err("invalid @since semantic gate must fail");

        assert!(
            matches!(
                error.failure(),
                WitPackageLoadFailure::Model(WitModelError::Resolve(_))
            ),
            "failure should come from Resolve::push: {error}"
        );
        let template = error
            .diagnostic_template_for(&root)
            .expect("semantic WIT failure owns a diagnostic");
        assert_eq!(template.code(), WitPackageDiagnosticCode::ParseError);
        assert_eq!(template.location(), None);
    }

    #[test]
    fn dependency_failure_template_is_emitted_only_for_the_failing_unit() {
        let packages = PackageInterner::new();
        let (root, root_version) = package_unit("acme:root", "1.0.0", &packages);
        let (dependency, dependency_version) = package_unit("acme:dependency", "1.0.0", &packages);
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
        let dependency_source = "package acme:dependency@1.0.0;\ninterface {";
        let error = load_wit_package_closure(root.clone(), &graph, |unit| {
            let source = if unit == &root {
                "package acme:root@1.0.0;\nworld app { import acme:dependency/api; }"
            } else {
                dependency_source
            };
            Ok::<_, &'static str>(vec![WitSourceFile::new(
                "package.wit",
                source.as_bytes().to_vec(),
            )])
        })
        .expect_err("dependency syntax error must fail the closure");

        assert_eq!(error.unit(), &dependency);
        assert_eq!(error.diagnostic_template_for(&root), None);
        let template = error
            .diagnostic_template_for(&dependency)
            .expect("the failing dependency owns its diagnostic");
        assert_eq!(template.unit(), &dependency);
        let location = template.location().expect("dependency error is located");
        assert_eq!(location.path, "package.wit");
        assert_eq!(&dependency_source[location.range.clone()], "{");
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
