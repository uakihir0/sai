//! End-to-end loading of embedded WASI WIT packages.
//!
//! The loader starts at one pinned WASI package, follows only its WIT
//! references to other embedded WASI packages, and returns parser-independent
//! models in dependency-first order. WASI references are resolved against
//! catalog nodes directly; this deliberately creates no package-graph edges.

use crate::{
    EMBEDDED_WASI_CATALOG, EMBEDDED_WASI_VERSION, EmbeddedWasiPackage, WitModelError, WitPackage,
    WitPackageGraph, WitPackageGraphError, WitPackageNode, WitParseError,
    WitReferenceResolutionError, WitResolveContext, WitUnit, WitUnitIdentityError, WitUnitKind,
    embedded_wasi_package, parse_source_files, resolve_package_references, wit_unit_closure,
};
use sai_intern::Interner;
use sai_package::version::Version;
use std::collections::HashMap;
use std::fmt;
use wit_parser::Resolve;

/// The loaded root package, its reachable dependencies, and their load order.
///
/// `dependency_models` and `closure` are both in dependency-first post-order.
/// The closure includes the root as its final unit; the dependency models do
/// not include the root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedWasiPackage {
    root: WitPackage,
    dependency_models: Vec<WitPackage>,
    closure: Vec<WitUnit>,
    direct_dependencies: HashMap<WitUnit, Vec<WitUnit>>,
}

impl LoadedWasiPackage {
    /// Returns the requested package model.
    pub fn root(&self) -> &WitPackage {
        &self.root
    }

    /// Returns reachable dependency models in dependency-first post-order.
    pub fn dependency_models(&self) -> &[WitPackage] {
        &self.dependency_models
    }

    /// Returns reachable units in dependency-first post-order, ending with the
    /// requested root unit.
    pub fn closure(&self) -> &[WitUnit] {
        &self.closure
    }

    /// Returns the first-reference ordered direct dependencies of a reachable
    /// unit. Unknown or unreachable units return `None`.
    pub fn direct_dependencies(&self, unit: &WitUnit) -> Option<&[WitUnit]> {
        self.direct_dependencies.get(unit).map(Vec::as_slice)
    }

    /// Consume the result into its root model, dependency models, and closure.
    pub fn into_parts(self) -> (WitPackage, Vec<WitPackage>, Vec<WitUnit>) {
        (self.root, self.dependency_models, self.closure)
    }
}

/// An invalid requested root or a compiler-internal embedded WIT load failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitWasiLoadError {
    InvalidRoot {
        unit: WitUnit,
        reason: WitWasiRootError,
    },
    CompilerInternal {
        unit: WitUnit,
        failure: WitWasiLoadFailure,
    },
}

impl WitWasiLoadError {
    /// Returns the root or package unit affected by this error.
    pub fn unit(&self) -> &WitUnit {
        match self {
            Self::InvalidRoot { unit, .. } | Self::CompilerInternal { unit, .. } => unit,
        }
    }

    /// Returns the compiler-internal failure when one occurred.
    pub fn failure(&self) -> Option<&WitWasiLoadFailure> {
        match self {
            Self::InvalidRoot { .. } => None,
            Self::CompilerInternal { failure, .. } => Some(failure),
        }
    }
}

impl fmt::Display for WitWasiLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot { unit, reason } => {
                write!(
                    formatter,
                    "invalid WASI WIT root `{}`: {reason}",
                    unit.canonical_name()
                )
            }
            Self::CompilerInternal { unit, failure } => write!(
                formatter,
                "compiler-internal failure loading embedded WIT unit `{}`: {failure}",
                unit.canonical_name()
            ),
        }
    }
}

impl std::error::Error for WitWasiLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRoot { .. } => None,
            Self::CompilerInternal { failure, .. } => std::error::Error::source(failure),
        }
    }
}

/// Why a requested unit cannot identify an embedded WASI root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WitWasiRootError {
    NotWasi,
    UnknownPackageSymbol,
    UnknownPackageName,
    UnitIdentityMismatch,
}

impl fmt::Display for WitWasiRootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWasi => formatter.write_str("the unit is not a WASI unit"),
            Self::UnknownPackageSymbol => {
                formatter.write_str("the package symbol does not belong to this interner")
            }
            Self::UnknownPackageName => {
                formatter.write_str("the package is not in the embedded WASI catalog")
            }
            Self::UnitIdentityMismatch => {
                formatter.write_str("the unit does not match the embedded package identity")
            }
        }
    }
}

/// A failure while loading catalog sources into a parser-independent model.
///
/// The affected unit is stored on [`WitWasiLoadError::CompilerInternal`],
/// keeping the internal failure category available to compiler diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitWasiLoadFailure {
    CatalogRootMissing,
    InvalidPinnedVersion(String),
    CatalogSymbolInterning {
        package_name: String,
        error: sai_intern::InternError,
    },
    InvalidCatalogUnit {
        package_name: String,
        error: WitUnitIdentityError,
    },
    Parse(WitParseError),
    Graph(WitPackageGraphError),
    ReferenceResolution(WitReferenceResolutionError),
    Closure(crate::WitClosureError<WitUnit>),
    MissingDiscoveredPackage(WitUnit),
    MissingResolvedPackage,
    ClosureDoesNotEndAtRoot,
    Model(WitModelError),
}

impl fmt::Display for WitWasiLoadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogRootMissing => {
                formatter.write_str("the requested root is missing from the embedded catalog")
            }
            Self::InvalidPinnedVersion(version) => {
                write!(formatter, "embedded WASI version `{version}` is invalid")
            }
            Self::CatalogSymbolInterning {
                package_name,
                error,
            } => write!(
                formatter,
                "cannot intern embedded package name `wasi:{package_name}`: {error:?}"
            ),
            Self::InvalidCatalogUnit {
                package_name,
                error,
            } => write!(
                formatter,
                "cannot create the embedded unit for `wasi:{package_name}`: {error:?}"
            ),
            Self::Parse(error) => write!(formatter, "embedded WIT source did not parse: {error}"),
            Self::Graph(error) => {
                write!(formatter, "embedded WIT package graph is invalid: {error}")
            }
            Self::ReferenceResolution(error) => {
                write!(
                    formatter,
                    "embedded WIT references did not resolve: {error}"
                )
            }
            Self::Closure(error) => {
                write!(formatter, "embedded WIT dependency closure failed: {error}")
            }
            Self::MissingDiscoveredPackage(unit) => write!(
                formatter,
                "discovered dependency `{}` is missing from the embedded catalog",
                unit.canonical_name()
            ),
            Self::MissingResolvedPackage => {
                formatter.write_str("resolved package is missing from the loader worklist")
            }
            Self::ClosureDoesNotEndAtRoot => {
                formatter.write_str("dependency closure did not end with its requested root")
            }
            Self::Model(error) => {
                write!(formatter, "embedded WIT model construction failed: {error}")
            }
        }
    }
}

impl std::error::Error for WitWasiLoadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::ReferenceResolution(error) => Some(error),
            Self::Closure(error) => Some(error),
            Self::Model(error) => Some(error),
            Self::CatalogRootMissing
            | Self::InvalidPinnedVersion(_)
            | Self::CatalogSymbolInterning { .. }
            | Self::InvalidCatalogUnit { .. }
            | Self::MissingDiscoveredPackage(_)
            | Self::MissingResolvedPackage
            | Self::ClosureDoesNotEndAtRoot => None,
        }
    }
}

/// Load one package from the pinned embedded WASI catalog.
///
/// `root` must be a WASI unit created with the same symbol interner supplied
/// here. The loader follows reachable WASI references, normalizes each package,
/// and pushes packages into one parser `Resolve` in dependency-first order.
pub fn load_embedded_wasi_package(
    root: &WitUnit,
    symbols: &Interner,
) -> Result<LoadedWasiPackage, WitWasiLoadError> {
    let package_name = match root.kind() {
        WitUnitKind::Wasi { package } => symbols
            .resolve(package)
            .map_err(|_| WitWasiLoadError::InvalidRoot {
                unit: root.clone(),
                reason: WitWasiRootError::UnknownPackageSymbol,
            })?
            .to_string(),
        WitUnitKind::Dir { .. } | WitUnitKind::Generated { .. } => {
            return Err(WitWasiLoadError::InvalidRoot {
                unit: root.clone(),
                reason: WitWasiRootError::NotWasi,
            });
        }
    };
    let Some(catalog_root) = embedded_wasi_package(&package_name) else {
        return Err(WitWasiLoadError::InvalidRoot {
            unit: root.clone(),
            reason: WitWasiRootError::UnknownPackageName,
        });
    };

    let version = Version::parse(EMBEDDED_WASI_VERSION).map_err(|_| {
        internal_error(
            root,
            WitWasiLoadFailure::InvalidPinnedVersion(EMBEDDED_WASI_VERSION.to_owned()),
        )
    })?;
    let package_symbol = symbols.intern(catalog_root.name).map_err(|error| {
        internal_error(
            root,
            WitWasiLoadFailure::CatalogSymbolInterning {
                package_name: catalog_root.name.to_owned(),
                error,
            },
        )
    })?;
    let canonical_root = WitUnit::wasi(package_symbol, symbols, &version).map_err(|error| {
        internal_error(
            root,
            WitWasiLoadFailure::InvalidCatalogUnit {
                package_name: catalog_root.name.to_owned(),
                error,
            },
        )
    })?;
    if root != &canonical_root || root.canonical_name() != canonical_root.canonical_name() {
        return Err(WitWasiLoadError::InvalidRoot {
            unit: root.clone(),
            reason: WitWasiRootError::UnitIdentityMismatch,
        });
    }

    load_embedded_wasi_package_from_catalog(root, symbols, &EMBEDDED_WASI_CATALOG)
}

fn load_embedded_wasi_package_from_catalog(
    root: &WitUnit,
    symbols: &Interner,
    catalog: &[EmbeddedWasiPackage],
) -> Result<LoadedWasiPackage, WitWasiLoadError> {
    let mut units = HashMap::<WitUnit, &EmbeddedWasiPackage>::new();
    let version = Version::parse(EMBEDDED_WASI_VERSION).map_err(|_| {
        internal_error(
            root,
            WitWasiLoadFailure::InvalidPinnedVersion(EMBEDDED_WASI_VERSION.to_owned()),
        )
    })?;

    let mut nodes = Vec::with_capacity(catalog.len());
    for package in catalog {
        let symbol = symbols.intern(package.name).map_err(|error| {
            internal_error(
                root,
                WitWasiLoadFailure::CatalogSymbolInterning {
                    package_name: package.name.to_owned(),
                    error,
                },
            )
        })?;
        let unit = WitUnit::wasi(symbol, symbols, &version).map_err(|error| {
            internal_error(
                root,
                WitWasiLoadFailure::InvalidCatalogUnit {
                    package_name: package.name.to_owned(),
                    error,
                },
            )
        })?;
        // The graph constructor diagnoses duplicate package names as a
        // `WitPackageGraphError`; retain every node until then.
        units.insert(unit.clone(), package);
        nodes.push(
            WitPackageNode::wasi(unit)
                .map_err(|error| internal_error(root, WitWasiLoadFailure::Graph(error)))?,
        );
    }

    if !units.contains_key(root) {
        return Err(internal_error(root, WitWasiLoadFailure::CatalogRootMissing));
    }

    // WASI 0.2.3 package references are visible through the pinned catalog.
    // They are not active MVS package dependencies, so this graph has nodes but
    // intentionally contains no WitPackageEdge values.
    let graph = WitPackageGraph::new(root.clone(), nodes, [])
        .map_err(|error| internal_error(root, WitWasiLoadFailure::Graph(error)))?;

    let mut resolved = HashMap::new();
    let mut direct_dependencies = HashMap::<WitUnit, Vec<WitUnit>>::new();
    let mut pending = vec![root.clone()];
    while let Some(unit) = pending.pop() {
        if direct_dependencies.contains_key(&unit) {
            continue;
        }
        let Some(package) = units.get(&unit).copied() else {
            return Err(internal_error(
                &unit,
                WitWasiLoadFailure::MissingDiscoveredPackage(unit.clone()),
            ));
        };
        let parsed = parse_source_files(unit.clone(), package.source_files())
            .map_err(|error| internal_error(&unit, WitWasiLoadFailure::Parse(error)))?;
        let resolved_group = resolve_package_references(parsed, &graph).map_err(|error| {
            internal_error(&unit, WitWasiLoadFailure::ReferenceResolution(error))
        })?;
        let (parsed, dependencies) = resolved_group.into_parts();
        for dependency in dependencies.iter().rev() {
            pending.push(dependency.clone());
        }
        direct_dependencies.insert(unit.clone(), dependencies);
        resolved.insert(unit, parsed);
    }

    let closure = wit_unit_closure(root.clone(), &direct_dependencies)
        .map_err(|error| internal_error(root, WitWasiLoadFailure::Closure(error)))?;
    if closure.last() != Some(root) {
        return Err(internal_error(
            root,
            WitWasiLoadFailure::ClosureDoesNotEndAtRoot,
        ));
    }

    let mut resolve = Resolve::default();
    let mut context = WitResolveContext::default();
    let mut root_model = None;
    let mut dependency_models = Vec::with_capacity(closure.len().saturating_sub(1));
    for unit in &closure {
        let parsed = resolved
            .remove(unit)
            .ok_or_else(|| internal_error(unit, WitWasiLoadFailure::MissingResolvedPackage))?;
        let model = context
            .push_package(&mut resolve, parsed)
            .map_err(|error| internal_error(unit, WitWasiLoadFailure::Model(error)))?;
        if unit == root {
            root_model = Some(model);
        } else {
            dependency_models.push(model);
        }
    }
    let root_model = root_model
        .ok_or_else(|| internal_error(root, WitWasiLoadFailure::ClosureDoesNotEndAtRoot))?;

    Ok(LoadedWasiPackage {
        root: root_model,
        dependency_models,
        closure,
        direct_dependencies,
    })
}

fn internal_error(unit: &WitUnit, failure: WitWasiLoadFailure) -> WitWasiLoadError {
    WitWasiLoadError::CompilerInternal {
        unit: unit.clone(),
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmbeddedWasiFile, WitFunc, WitItemKind, WitType, WitTypeRef};
    use sai_intern::Interner;
    use std::collections::HashSet;

    fn root_unit(name: &str, symbols: &Interner) -> WitUnit {
        let symbol = symbols.intern(name).expect("valid package symbol");
        let version = Version::parse(EMBEDDED_WASI_VERSION).expect("valid embedded version");
        WitUnit::wasi(symbol, symbols, &version).expect("known embedded WASI unit")
    }

    fn find_type_reference<'a>(ty: &'a WitType, unit: &WitUnit) -> Option<&'a WitTypeRef> {
        match ty {
            WitType::Named(reference) | WitType::Own(reference) | WitType::Borrow(reference)
                if &reference.interface.unit == unit =>
            {
                Some(reference)
            }
            WitType::List(inner)
            | WitType::Option(inner)
            | WitType::Future(Some(inner))
            | WitType::Stream(Some(inner)) => find_type_reference(inner, unit),
            WitType::Result { ok, err } => ok
                .iter()
                .chain(err.iter())
                .find_map(|inner| find_type_reference(inner, unit)),
            WitType::Tuple(items) => items
                .iter()
                .find_map(|inner| find_type_reference(inner, unit)),
            _ => None,
        }
    }

    fn find_function_reference<'a>(
        function: &'a WitFunc,
        unit: &WitUnit,
    ) -> Option<&'a WitTypeRef> {
        function
            .params
            .iter()
            .find_map(|(_, ty)| find_type_reference(ty, unit))
            .or_else(|| {
                function
                    .result
                    .as_ref()
                    .and_then(|ty| find_type_reference(ty, unit))
            })
    }

    fn find_item_reference<'a>(item: &'a crate::WitItem, unit: &WitUnit) -> Option<&'a WitTypeRef> {
        match &item.kind {
            WitItemKind::Used(reference) if &reference.interface.unit == unit => Some(reference),
            WitItemKind::Alias(ty) => find_type_reference(ty, unit),
            WitItemKind::Record(fields) => fields
                .iter()
                .find_map(|field| find_type_reference(&field.ty, unit)),
            WitItemKind::Variant(cases) => cases.iter().find_map(|case| {
                case.payload
                    .as_ref()
                    .and_then(|ty| find_type_reference(ty, unit))
            }),
            WitItemKind::Resource(members) => members
                .iter()
                .find_map(|member| find_function_reference(&member.func, unit)),
            WitItemKind::Func(function) => find_function_reference(function, unit),
            WitItemKind::Enum(_) | WitItemKind::Flags(_) => None,
            WitItemKind::Used(_) => None,
        }
    }

    #[test]
    fn loads_every_catalog_package_and_orders_all_dependencies_first() {
        let symbols = Interner::new();
        for package in EMBEDDED_WASI_CATALOG {
            let root = root_unit(package.name, &symbols);
            let loaded = load_embedded_wasi_package(&root, &symbols)
                .unwrap_or_else(|error| panic!("failed to load {}: {error}", package.name));
            assert_eq!(loaded.root().unit, root);
            assert_eq!(loaded.root().name.namespace, "wasi");
            assert_eq!(loaded.root().name.name, package.name);
            assert_eq!(
                loaded.root().name.version.as_ref().map(ToString::to_string),
                Some(EMBEDDED_WASI_VERSION.to_owned())
            );
            assert_eq!(loaded.closure().last(), Some(&root));
            assert_eq!(
                loaded
                    .dependency_models()
                    .iter()
                    .map(|model| model.unit.clone())
                    .collect::<Vec<_>>(),
                loaded.closure()[..loaded.closure().len() - 1]
            );
            assert_eq!(
                loaded
                    .closure()
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
                    .len(),
                loaded.closure().len(),
                "closure for {} contains each unit only once",
                package.name
            );

            let positions = loaded
                .closure()
                .iter()
                .enumerate()
                .map(|(index, unit)| (unit.clone(), index))
                .collect::<HashMap<_, _>>();
            for (consumer, dependencies) in &loaded.direct_dependencies {
                for dependency in dependencies {
                    assert!(
                        positions[dependency] < positions[consumer],
                        "{} must be emitted after {}",
                        consumer.canonical_name(),
                        dependency.canonical_name()
                    );
                }
            }
        }
    }

    #[test]
    fn resolves_external_type_references_to_loaded_dependency_units() {
        let symbols = Interner::new();
        let loaded = load_embedded_wasi_package(&root_unit("cli", &symbols), &symbols)
            .expect("the CLI package and its WASI dependencies load");
        let io_unit = loaded
            .dependency_models()
            .iter()
            .find(|package| package.name.name == "io")
            .map(|package| &package.unit)
            .expect("CLI has the I/O dependency");

        // Ensure type references in the consumer model point at declarations
        // actually present in the dependency model.
        let io_package = loaded
            .dependency_models()
            .iter()
            .find(|package| &package.unit == io_unit)
            .expect("I/O dependency model is present");
        let reference = loaded
            .root()
            .interfaces
            .iter()
            .flat_map(|interface| interface.items.iter())
            .find_map(|item| find_item_reference(item, io_unit))
            .expect("CLI has a reference into the I/O package");
        let target_interface = io_package
            .interfaces
            .get(reference.interface.interface)
            .expect("external type points to a loaded I/O interface");
        assert!(
            target_interface.items.get(reference.item).is_some(),
            "external type points to an existing I/O item"
        );
    }

    #[test]
    fn missing_catalog_root_fails_as_a_compiler_internal_load_error() {
        static OTHER_FILES: [EmbeddedWasiFile; 1] = [EmbeddedWasiFile {
            relative_path: "world.wit",
            contents: b"package wasi:io@0.2.3;",
        }];
        static OTHER_PACKAGE: EmbeddedWasiPackage = EmbeddedWasiPackage {
            name: "io",
            upstream_repository: "test",
            upstream_commit: "test",
            files: &OTHER_FILES,
        };

        let symbols = Interner::new();
        let root = root_unit("cli", &symbols);
        let error = load_embedded_wasi_package_from_catalog(&root, &symbols, &[OTHER_PACKAGE])
            .expect_err("root absent from injected catalog");
        assert_eq!(error.unit(), &root);
        assert!(matches!(
            error,
            WitWasiLoadError::CompilerInternal {
                failure: WitWasiLoadFailure::CatalogRootMissing,
                ..
            }
        ));
    }

    #[test]
    fn malformed_catalog_root_fails_with_its_unit_and_parse_failure() {
        static MALFORMED_FILES: [EmbeddedWasiFile; 1] = [EmbeddedWasiFile {
            relative_path: "malformed.wit",
            contents: b"package wasi:cli@0.2.3;\ninterface {",
        }];
        static MALFORMED_PACKAGE: EmbeddedWasiPackage = EmbeddedWasiPackage {
            name: "cli",
            upstream_repository: "test",
            upstream_commit: "test",
            files: &MALFORMED_FILES,
        };

        let symbols = Interner::new();
        let root = root_unit("cli", &symbols);
        let error = load_embedded_wasi_package_from_catalog(&root, &symbols, &[MALFORMED_PACKAGE])
            .expect_err("malformed embedded root must fail");
        assert_eq!(error.unit(), &root);
        assert!(matches!(
            error,
            WitWasiLoadError::CompilerInternal {
                failure: WitWasiLoadFailure::Parse(_),
                ..
            }
        ));
    }
}
