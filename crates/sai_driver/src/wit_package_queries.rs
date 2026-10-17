use super::package_graph::PackageGraphNode;
use super::*;

/// Errors returned by the public tracked WIT-package query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageQueryError {
    InvalidUnit(String),
    Query(QueryError),
}

impl From<QueryError> for WitPackageQueryError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

/// A tracked input failure while reading one WIT unit's source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageSourceFailure {
    Contract(String),
    DirectoryNotDeclared,
    InventoryMissing,
    DirectoryNotApplicable,
    DirectoryMismatch {
        expected: String,
        found: String,
    },
    GeneratedTextMissing,
    FileInputMissing {
        package_relative_path: String,
    },
    FileReadError {
        package_relative_path: String,
        io_error_kind: String,
    },
    InvalidFileKey {
        package_relative_path: String,
    },
    Query(QueryError),
}

/// A failure while preparing or loading a package's WIT closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitPackageFailure {
    PackageGraphUnavailable,
    PackageNotActive {
        package_key: String,
    },
    Contract {
        package_key: String,
        reason: String,
    },
    UnitProjection {
        package_key: String,
        reason: String,
    },
    RequestedUnitMismatch {
        package_key: String,
        expected: String,
        found: String,
    },
    RootDirectoryNotDeclared {
        package_key: String,
    },
    Graph(WitPackageGraphError),
    Load(WitPackageLoadError<WitPackageSourceFailure>),
}

impl WitPackageFailure {
    /// Returns the WIT unit whose source or model caused this failure.
    ///
    /// Graph and manifest-projection failures belong to package validation
    /// and do not necessarily identify a complete WIT unit.
    pub fn failed_unit(&self) -> Option<&WitUnit> {
        match self {
            Self::Load(error) => Some(error.unit()),
            Self::PackageGraphUnavailable
            | Self::PackageNotActive { .. }
            | Self::Contract { .. }
            | Self::UnitProjection { .. }
            | Self::RequestedUnitMismatch { .. }
            | Self::RootDirectoryNotDeclared { .. }
            | Self::Graph(_) => None,
        }
    }

    /// Returns the generated unit whose source text has not been supplied.
    ///
    /// Source packages are viewed through generated WIT by their dependents.
    /// Until the `sai_ty` producer is available, this is an expected
    /// unavailable stage rather than a user-authored WIT load failure.
    pub fn generated_text_missing_unit(&self) -> Option<&WitUnit> {
        match self {
            Self::Load(error)
                if matches!(
                    error.failure(),
                    sai_wit::WitPackageLoadFailure::Source(
                        WitPackageSourceFailure::GeneratedTextMissing
                    )
                ) =>
            {
                Some(error.unit())
            }
            _ => None,
        }
    }
}

/// The result of loading one requested WIT unit and its reachable closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitPackageResult {
    pub package: Option<Arc<LoadedWitPackage>>,
    pub failure: Option<Arc<WitPackageFailure>>,
    /// Generated snapshots reached while loading this closure. These retain
    /// the origin mappings needed by the later WIT-view integration.
    pub generated_texts: Arc<[(WitUnit, Arc<GeneratedText>)]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WitPackageQueryValue {
    pub(super) package: Arc<LoadedWitPackage>,
    pub(super) generated_texts: Arc<[(WitUnit, Arc<GeneratedText>)]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WitPackageQueryOutput {
    pub(super) identity: Fingerprint,
    pub(super) selected_version: Option<String>,
    pub(super) result: Result<WitPackageQueryValue, WitPackageFailure>,
}

impl StableHash for WitPackageQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        // The query key omits the selected patch/prerelease version. Preserve
        // that exact value separately while using stable model fingerprints
        // for successful package closures.
        self.identity.stable_hash(hasher);
        self.selected_version.stable_hash(hasher);
        match &self.result {
            Ok(value) => {
                hasher.write_tag(0xF2);
                value.package.stable_hash(hasher);
                // `WitUnit` equality and stable hashing use a compatibility
                // line, while WIT references are normalized to the exact
                // active selection. Include every unit's canonical name so a
                // same-line dependency update invalidates the closure too.
                hasher.write_len(u64::try_from(value.package.closure().len()).unwrap_or(u64::MAX));
                for unit in value.package.closure() {
                    unit.canonical_name().stable_hash(hasher);
                }
                hasher.write_len(u64::try_from(value.generated_texts.len()).unwrap_or(u64::MAX));
                for (unit, generated) in value.generated_texts.iter() {
                    unit.canonical_name().stable_hash(hasher);
                    generated.stable_hash(hasher);
                }
            }
            Err(failure) => {
                hasher.write_tag(0xF3);
                stable_hash_wit_package_failure(failure, hasher);
            }
        }
    }
}

fn stable_hash_wit_package_failure(failure: &WitPackageFailure, hasher: &mut StableHasher) {
    match failure {
        WitPackageFailure::PackageGraphUnavailable => hasher.write_tag(0x01),
        WitPackageFailure::PackageNotActive { package_key } => {
            hasher.write_tag(0x02);
            package_key.stable_hash(hasher);
        }
        WitPackageFailure::Contract {
            package_key,
            reason,
        } => {
            hasher.write_tag(0x03);
            package_key.stable_hash(hasher);
            reason.stable_hash(hasher);
        }
        WitPackageFailure::UnitProjection {
            package_key,
            reason,
        } => {
            hasher.write_tag(0x04);
            package_key.stable_hash(hasher);
            reason.stable_hash(hasher);
        }
        WitPackageFailure::RequestedUnitMismatch {
            package_key,
            expected,
            found,
        } => {
            hasher.write_tag(0x05);
            package_key.stable_hash(hasher);
            expected.stable_hash(hasher);
            found.stable_hash(hasher);
        }
        WitPackageFailure::RootDirectoryNotDeclared { package_key } => {
            hasher.write_tag(0x06);
            package_key.stable_hash(hasher);
        }
        WitPackageFailure::Graph(_) => hasher.write_tag(0x07),
        WitPackageFailure::Load(error) => {
            hasher.write_tag(0x08);
            // The failed unit can be a dependency. Its exact version is
            // observable in the returned failure even though `WitUnit`
            // equality is scoped to a compatibility line.
            error.unit().canonical_name().stable_hash(hasher);
            match error.failure() {
                WitPackageLoadFailure::Source(source) => {
                    hasher.write_tag(match source {
                        WitPackageSourceFailure::Contract(_) => 0x01,
                        WitPackageSourceFailure::DirectoryNotDeclared => 0x02,
                        WitPackageSourceFailure::InventoryMissing => 0x03,
                        WitPackageSourceFailure::DirectoryNotApplicable => 0x04,
                        WitPackageSourceFailure::DirectoryMismatch { .. } => 0x05,
                        WitPackageSourceFailure::GeneratedTextMissing => 0x06,
                        WitPackageSourceFailure::FileInputMissing { .. } => 0x07,
                        WitPackageSourceFailure::FileReadError { .. } => 0x08,
                        WitPackageSourceFailure::InvalidFileKey { .. } => 0x09,
                        WitPackageSourceFailure::Query(_) => 0x0A,
                    });
                }
                WitPackageLoadFailure::Parse(_) => hasher.write_tag(0x10),
                WitPackageLoadFailure::Graph(_) => hasher.write_tag(0x11),
                WitPackageLoadFailure::ReferenceResolution(_) => hasher.write_tag(0x12),
                WitPackageLoadFailure::Closure(_) => hasher.write_tag(0x13),
                WitPackageLoadFailure::MissingResolvedPackage(_) => hasher.write_tag(0x14),
                WitPackageLoadFailure::ClosureDoesNotEndAtRoot => hasher.write_tag(0x15),
                WitPackageLoadFailure::Model(_) => hasher.write_tag(0x16),
            }
        }
    }
}

pub(super) struct WitPackageQueries {
    pub(super) query: DerivedQuery<WitUnit, WitPackageQueryOutput>,
}

impl WitPackageQueries {
    pub(super) fn new(
        vfs: VfsInputs,
        contracts: DerivedQuery<PackageKey, WitContractQueryOutput>,
        generated_texts: DerivedQuery<PackageKey, Option<Arc<GeneratedText>>>,
        wit_units: DerivedQuery<PackageKey, WitUnitOfQueryOutput>,
        package_graph: InputQuery<u8, Option<PackageGraph>>,
        packages: Arc<PackageInterner>,
        symbols: Arc<Interner>,
    ) -> Result<Self, QueryError> {
        let query = DerivedQuery::new(
            "wit_package",
            move |database: &mut QueryDatabase, requested: &WitUnit| {
                let identity = fingerprint(requested);
                match requested.kind() {
                    WitUnitKind::Wasi { .. } => {
                        load_wasi_wit_package(identity, requested, &symbols)
                    }
                    WitUnitKind::Dir { .. } | WitUnitKind::Generated { .. } => {
                        load_active_wit_package(
                            database,
                            identity,
                            requested,
                            &vfs,
                            &contracts,
                            &generated_texts,
                            &wit_units,
                            &package_graph,
                            &packages,
                            &symbols,
                        )
                    }
                }
            },
        )?;
        Ok(Self { query })
    }
}

/// Errors returned by the tracked WIT-view query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitViewQueryError {
    InvalidUnit(String),
    Query(QueryError),
    Package(WitPackageFailure),
    View(WitViewError),
    GeneratedSnapshotMissing { canonical_unit: String },
}

impl From<WitPackageQueryError> for WitViewQueryError {
    fn from(error: WitPackageQueryError) -> Self {
        match error {
            WitPackageQueryError::InvalidUnit(reason) => Self::InvalidUnit(reason),
            WitPackageQueryError::Query(error) => Self::Query(error),
        }
    }
}

impl std::fmt::Display for WitViewQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUnit(reason) => write!(formatter, "invalid WIT unit: {reason}"),
            Self::Query(error) => write!(formatter, "WIT view query failed: {error}"),
            Self::Package(error) => write!(formatter, "WIT package loading failed: {error:?}"),
            Self::View(error) => write!(formatter, "WIT view construction failed: {error}"),
            Self::GeneratedSnapshotMissing { canonical_unit } => write!(
                formatter,
                "loaded Generated unit `{canonical_unit}` has no tracked text snapshot"
            ),
        }
    }
}

impl std::error::Error for WitViewQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) => Some(error),
            Self::View(error) => Some(error),
            Self::InvalidUnit(_) | Self::Package(_) | Self::GeneratedSnapshotMissing { .. } => None,
        }
    }
}

pub(super) fn validate_generated_origin_definitions(
    origins: &[(String, DefId)],
    source_def_paths: &DefPathInterner,
) -> Result<(), WitViewError> {
    for (interface, def) in origins {
        match source_def_paths.resolve_def_id(*def).map_err(|error| {
            WitViewError::GeneratedOriginPath {
                interface: interface.clone(),
                error,
            }
        })? {
            Some(_) => {}
            None => {
                return Err(WitViewError::GeneratedOriginDefinitionNotFound {
                    interface: interface.clone(),
                    def: *def,
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WitViewQueryOutput {
    package_identity: Fingerprint,
    result: Result<Arc<WitView>, WitViewQueryError>,
}

impl StableHash for WitViewQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        // Fingerprinting the package-query output retains exact selected
        // versions throughout the closure, even though WitUnit's query key
        // uses compatibility-line identity.
        self.package_identity.stable_hash(hasher);
        match &self.result {
            Ok(view) => {
                hasher.write_tag(0xC1);
                view.stable_hash(hasher);
            }
            Err(error) => {
                hasher.write_tag(match error {
                    WitViewQueryError::InvalidUnit(_) => 0x01,
                    WitViewQueryError::Query(_) => 0x02,
                    WitViewQueryError::Package(_) => 0x03,
                    WitViewQueryError::View(_) => 0x04,
                    WitViewQueryError::GeneratedSnapshotMissing { .. } => 0x05,
                });
            }
        }
    }
}

pub(super) struct WitViewQueries {
    query: DerivedQuery<WitUnit, WitViewQueryOutput>,
}

impl WitViewQueries {
    pub(super) fn new(
        package_query: DerivedQuery<WitUnit, WitPackageQueryOutput>,
        packages: Arc<PackageInterner>,
        symbols: Arc<Interner>,
        source_def_paths: Arc<Mutex<HashMap<PackageId, Arc<DefPathInterner>>>>,
        source_def_path_generations: InputQuery<String, u64>,
        view_def_paths: Arc<Mutex<HashMap<String, Arc<DefPathInterner>>>>,
    ) -> Result<Self, QueryError> {
        let query = DerivedQuery::new(
            "wit_view",
            move |database: &mut QueryDatabase, unit: &WitUnit| {
                let package = package_query.get(database, unit.clone())?;
                if let Ok(value) = &package.result {
                    let package_unit = &value.package.root().unit;
                    let has_generated_origins = value
                        .generated_texts
                        .iter()
                        .find(|(generated_unit, _)| {
                            generated_unit.stable_key() == package_unit.stable_key()
                                && generated_unit.canonical_name() == package_unit.canonical_name()
                        })
                        .is_some_and(|(_, generated)| !generated.origins.is_empty());
                    if matches!(package_unit.kind(), WitUnitKind::Generated { .. })
                        && has_generated_origins
                    {
                        // The source path interner is populated by Phase 1
                        // collection rather than a query. Track its append-only
                        // definition count so a view built before that collection
                        // is retried when its generated origins become resolvable.
                        let _ = source_def_path_generations
                            .get_opt(database, value.package.root().unit.stable_key().to_owned())?;
                    }
                }
                let package_identity = fingerprint(&package);
                let result = package
                    .result
                    .as_ref()
                    .map_err(|failure| WitViewQueryError::Package(failure.clone()))
                    .and_then(|value| {
                        build_wit_view(
                            value,
                            &packages,
                            &symbols,
                            &source_def_paths,
                            &view_def_paths,
                        )
                    });
                Ok(WitViewQueryOutput {
                    package_identity,
                    result,
                })
            },
        )?;
        Ok(Self { query })
    }
}

fn build_wit_view(
    package: &WitPackageQueryValue,
    packages: &PackageInterner,
    symbols: &Interner,
    source_def_paths: &Mutex<HashMap<PackageId, Arc<DefPathInterner>>>,
    view_def_paths: &Mutex<HashMap<String, Arc<DefPathInterner>>>,
) -> Result<Arc<WitView>, WitViewQueryError> {
    let model = package.package.root();
    let unit = &model.unit;

    let view = match unit.kind() {
        WitUnitKind::Generated {
            package: source_package,
        } => {
            let generated = package
                .generated_texts
                .iter()
                .find(|(generated_unit, _)| {
                    generated_unit.stable_key() == unit.stable_key()
                        && generated_unit.canonical_name() == unit.canonical_name()
                })
                .map(|(_, generated)| generated)
                .ok_or_else(|| WitViewQueryError::GeneratedSnapshotMissing {
                    canonical_unit: unit.canonical_name().to_owned(),
                })?;
            let view_def_paths =
                super::persistent_wit_view_def_paths(unit, packages, symbols, view_def_paths)
                    .map_err(WitViewQueryError::View)?;
            if generated.origins.is_empty() {
                WitView::build(model.clone(), packages, symbols, view_def_paths)
            } else {
                let source_def_paths = source_def_paths
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&source_package)
                    .cloned()
                    .ok_or(WitViewQueryError::View(
                        WitViewError::GeneratedOriginInternerRequired,
                    ))?;
                WitView::build_with_origins(
                    model.clone(),
                    packages,
                    symbols,
                    view_def_paths,
                    &source_def_paths,
                    &generated.origins,
                )
            }
        }
        WitUnitKind::Dir { .. } | WitUnitKind::Wasi { .. } => {
            let view_def_paths =
                super::persistent_wit_view_def_paths(unit, packages, symbols, view_def_paths)
                    .map_err(WitViewQueryError::View)?;
            WitView::build(model.clone(), packages, symbols, view_def_paths)
        }
    }
    .map(Arc::new)
    .map_err(WitViewQueryError::View)?;
    Ok(view)
}

fn load_wasi_wit_package(
    identity: Fingerprint,
    requested: &WitUnit,
    symbols: &Interner,
) -> Result<WitPackageQueryOutput, QueryError> {
    let canonical = match canonicalize_wasi_wit_unit(requested, symbols) {
        Ok(unit) => unit,
        Err(reason) => {
            return Ok(failed_output(
                identity,
                None,
                WitPackageFailure::UnitProjection {
                    package_key: requested.canonical_name().to_owned(),
                    reason,
                },
            ));
        }
    };
    let version = Version::parse(EMBEDDED_WASI_VERSION).expect("embedded WASI version is valid");
    let mut nodes = Vec::with_capacity(EMBEDDED_WASI_CATALOG.len());
    for package in EMBEDDED_WASI_CATALOG {
        let symbol = symbols
            .intern(package.name)
            .map_err(|error| QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: format!("cannot intern embedded WASI unit: {error:?}"),
            })?;
        let unit =
            WitUnit::wasi(symbol, symbols, &version).map_err(|error| QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: format!("invalid embedded WASI unit: {error:?}"),
            })?;
        nodes.push(
            WitPackageNode::wasi(unit).map_err(|error| QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: error.to_string(),
            })?,
        );
    }
    let graph = match WitPackageGraph::new(canonical.clone(), nodes, []) {
        Ok(graph) => graph,
        Err(error) => {
            return Ok(failed_output(
                identity,
                Some(canonical.canonical_name().to_owned()),
                WitPackageFailure::Graph(error),
            ));
        }
    };
    let loaded = load_wit_package_closure(canonical.clone(), &graph, |unit| {
        let name = unit
            .canonical_name()
            .strip_prefix("wasi:")
            .and_then(|name| name.rsplit_once('@'))
            .map(|(name, _)| name)
            .ok_or(WitPackageSourceFailure::DirectoryNotDeclared)?;
        embedded_wasi_package(name)
            .map(|package| package.source_files())
            .ok_or(WitPackageSourceFailure::DirectoryNotDeclared)
    });
    finish_load(
        identity,
        Some(canonical.canonical_name().to_owned()),
        loaded,
        Vec::new(),
    )
}

fn load_active_wit_package(
    database: &mut QueryDatabase,
    identity: Fingerprint,
    requested: &WitUnit,
    vfs: &VfsInputs,
    contracts: &DerivedQuery<PackageKey, WitContractQueryOutput>,
    generated_texts: &DerivedQuery<PackageKey, Option<Arc<GeneratedText>>>,
    wit_units: &DerivedQuery<PackageKey, WitUnitOfQueryOutput>,
    package_graph: &InputQuery<u8, Option<PackageGraph>>,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<WitPackageQueryOutput, QueryError> {
    let Some(package_id) = package_id_for_wit_unit(requested.kind()) else {
        return Ok(failed_output(
            identity,
            None,
            WitPackageFailure::UnitProjection {
                package_key: requested.canonical_name().to_owned(),
                reason: "WIT unit has no package identity".to_owned(),
            },
        ));
    };
    let package_name = match packages.resolve(package_id) {
        Ok(name) => name,
        Err(error) => {
            return Ok(failed_output(
                identity,
                None,
                WitPackageFailure::UnitProjection {
                    package_key: requested.canonical_name().to_owned(),
                    reason: format!("unknown package identity: {error:?}"),
                },
            ));
        }
    };
    let package_key = match PackageKey::new(package_name.as_ref()) {
        Ok(key) => key,
        Err(error) => {
            return Ok(failed_output(
                identity,
                None,
                WitPackageFailure::UnitProjection {
                    package_key: package_name.to_string(),
                    reason: format!("invalid package key: {error:?}"),
                },
            ));
        }
    };
    let package_graph_output = package_graph.get(database, 0)?;
    let Some(package_graph) = package_graph_output.as_ref() else {
        return Ok(failed_output(
            identity,
            None,
            WitPackageFailure::PackageGraphUnavailable,
        ));
    };
    let Some(requested_node) = package_graph.node(&package_key) else {
        return Ok(failed_output(
            identity,
            None,
            WitPackageFailure::PackageNotActive {
                package_key: package_key.as_str().to_owned(),
            },
        ));
    };
    let requested_contract = match contracts
        .get(database, package_key.clone())?
        .result
        .as_ref()
    {
        Ok(contract) => Arc::clone(contract),
        Err(error) => {
            return Ok(failed_output(
                identity,
                Some(requested_node.version().to_string()),
                WitPackageFailure::Contract {
                    package_key: package_key.as_str().to_owned(),
                    reason: format!("{error:?}"),
                },
            ));
        }
    };
    if requested_contract.identity().to_string() != requested_node.identity()
        || requested_contract.version() != requested_node.version()
    {
        return Ok(failed_output(
            identity,
            Some(requested_node.version().to_string()),
            WitPackageFailure::Contract {
                package_key: package_key.as_str().to_owned(),
                reason: format!(
                    "manifest selects {}@{}, active graph selects {}@{}",
                    requested_contract.identity(),
                    requested_contract.version(),
                    requested_node.identity(),
                    requested_node.version()
                ),
            },
        ));
    }
    let selected = match selected_version(requested_node) {
        Ok(selected) => selected,
        Err(reason) => {
            return Ok(failed_output(
                identity,
                Some(requested_node.version().to_string()),
                WitPackageFailure::UnitProjection {
                    package_key: package_key.as_str().to_owned(),
                    reason,
                },
            ));
        }
    };
    let current_unit = if requested_node.role() == PackageGraphRole::Root {
        match requested.kind() {
            WitUnitKind::Dir { .. } if requested_contract.wit_directory().is_some() => {
                WitUnit::dir(package_id, packages, &selected)
            }
            WitUnitKind::Dir { .. } => {
                return Ok(failed_output(
                    identity,
                    Some(selected.version().to_string()),
                    WitPackageFailure::RootDirectoryNotDeclared {
                        package_key: package_key.as_str().to_owned(),
                    },
                ));
            }
            WitUnitKind::Generated { .. } => WitUnit::generated(package_id, packages, &selected),
            WitUnitKind::Wasi { .. } => unreachable!("WASI units use their own loader"),
        }
    } else {
        let projected = wit_units.get(database, package_key.clone())?;
        match &projected.result {
            Ok(Some(projected)) if same_unit_kind(projected, requested) => {
                unit_with_selected_version(projected, &package_key, &selected, packages)
            }
            Ok(Some(projected)) => {
                return Ok(failed_output(
                    identity,
                    Some(selected.version().to_string()),
                    WitPackageFailure::RequestedUnitMismatch {
                        package_key: package_key.as_str().to_owned(),
                        expected: projected.canonical_name().to_owned(),
                        found: requested.canonical_name().to_owned(),
                    },
                ));
            }
            Ok(None) => {
                return Ok(failed_output(
                    identity,
                    Some(selected.version().to_string()),
                    WitPackageFailure::UnitProjection {
                        package_key: package_key.as_str().to_owned(),
                        reason: "active package has no dependent-facing WIT unit".to_owned(),
                    },
                ));
            }
            Err(error) => {
                return Ok(failed_output(
                    identity,
                    Some(selected.version().to_string()),
                    WitPackageFailure::UnitProjection {
                        package_key: package_key.as_str().to_owned(),
                        reason: format!("{error:?}"),
                    },
                ));
            }
        }
    };
    let current_unit = match current_unit {
        Ok(unit) => unit,
        Err(error) => {
            return Ok(failed_output(
                identity,
                Some(selected.version().to_string()),
                WitPackageFailure::UnitProjection {
                    package_key: package_key.as_str().to_owned(),
                    reason: format!("{error:?}"),
                },
            ));
        }
    };
    if current_unit.stable_key() != requested.stable_key() {
        return Ok(failed_output(
            identity,
            Some(selected.version().to_string()),
            WitPackageFailure::RequestedUnitMismatch {
                package_key: package_key.as_str().to_owned(),
                expected: current_unit.canonical_name().to_owned(),
                found: requested.canonical_name().to_owned(),
            },
        ));
    }

    let mut units_by_package = HashMap::<PackageKey, WitUnit>::new();
    for node in package_graph.nodes() {
        let projected = if node.key() == &package_key {
            Some(current_unit.clone())
        } else {
            let projection = wit_units.get(database, node.key().clone())?;
            match &projection.result {
                Ok(Some(projected)) => {
                    let selected = match selected_version(node) {
                        Ok(selected) => selected,
                        Err(reason) => {
                            return Ok(failed_output(
                                identity,
                                Some(requested_node.version().to_string()),
                                WitPackageFailure::UnitProjection {
                                    package_key: node.key().as_str().to_owned(),
                                    reason,
                                },
                            ));
                        }
                    };
                    match unit_with_selected_version(projected, node.key(), &selected, packages) {
                        Ok(unit) => Some(unit),
                        Err(error) => {
                            return Ok(failed_output(
                                identity,
                                Some(requested_node.version().to_string()),
                                WitPackageFailure::UnitProjection {
                                    package_key: node.key().as_str().to_owned(),
                                    reason: format!("{error:?}"),
                                },
                            ));
                        }
                    }
                }
                // Projection failures for unrelated active packages do not
                // make this root's reachable WIT closure invalid. Represent
                // them as an active edge without a WIT unit; if a reachable
                // package actually references this node, the WIT resolver
                // reports that edge as unavailable.
                Ok(None) | Err(_) => None,
            }
        };
        if let Some(unit) = projected {
            units_by_package.insert(node.key().clone(), unit);
        }
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for node in package_graph.nodes() {
        let Some(source_unit) = units_by_package.get(node.key()).cloned() else {
            continue;
        };
        let selected = match selected_version(node) {
            Ok(selected) => selected,
            Err(reason) => {
                return Ok(failed_output(
                    identity,
                    Some(requested_node.version().to_string()),
                    WitPackageFailure::UnitProjection {
                        package_key: node.key().as_str().to_owned(),
                        reason,
                    },
                ));
            }
        };
        match WitPackageNode::new(source_unit.clone(), selected) {
            Ok(node) => nodes.push(node),
            Err(error) => {
                return Ok(failed_output(
                    identity,
                    Some(requested_node.version().to_string()),
                    WitPackageFailure::Graph(error),
                ));
            }
        }
        for edge in node.active_edges() {
            let Some(target) = package_graph.node(edge.target()) else {
                continue;
            };
            let selected = match selected_version(target) {
                Ok(selected) => selected,
                Err(reason) => {
                    return Ok(failed_output(
                        identity,
                        Some(requested_node.version().to_string()),
                        WitPackageFailure::UnitProjection {
                            package_key: target.key().as_str().to_owned(),
                            reason,
                        },
                    ));
                }
            };
            edges.push(WitPackageEdge::new(
                source_unit.clone(),
                edge.visible_identity().to_owned(),
                selected,
                units_by_package.get(edge.target()).cloned(),
            ));
        }
    }
    let wasi_version =
        Version::parse(EMBEDDED_WASI_VERSION).expect("embedded WASI version is valid");
    for package in EMBEDDED_WASI_CATALOG {
        let symbol = symbols
            .intern(package.name)
            .map_err(|error| QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: format!("cannot intern embedded WASI unit: {error:?}"),
            })?;
        let unit = WitUnit::wasi(symbol, symbols, &wasi_version).map_err(|error| {
            QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: format!("invalid embedded WASI unit: {error:?}"),
            }
        })?;
        nodes.push(
            WitPackageNode::wasi(unit).map_err(|error| QueryError::Panicked {
                query: "wit_package".to_owned(),
                message: error.to_string(),
            })?,
        );
    }
    let graph = match WitPackageGraph::new(current_unit.clone(), nodes, edges) {
        Ok(graph) => graph,
        Err(error) => {
            return Ok(failed_output(
                identity,
                Some(requested_node.version().to_string()),
                WitPackageFailure::Graph(error),
            ));
        }
    };

    let mut generated = Vec::new();
    let loaded = load_wit_package_closure(current_unit, &graph, |unit| {
        read_wit_package_source_files(
            database,
            unit,
            vfs,
            contracts,
            generated_texts,
            packages,
            &mut generated,
        )
    });
    finish_load(
        identity,
        Some(requested_node.version().to_string()),
        loaded,
        generated,
    )
}

fn selected_version(node: &PackageGraphNode) -> Result<MvsPackageVersion, String> {
    MvsPackageVersion::new(node.identity().to_owned(), node.version().clone())
        .map_err(|error| format!("invalid active package version: {error:?}"))
}

fn unit_with_selected_version(
    projected: &WitUnit,
    package_key: &PackageKey,
    selected: &MvsPackageVersion,
    packages: &PackageInterner,
) -> Result<WitUnit, WitUnitIdentityError> {
    let package = packages
        .intern(package_key.as_str())
        .map_err(WitUnitIdentityError::UnknownPackage)?;
    match projected.kind() {
        WitUnitKind::Dir { .. } => WitUnit::dir(package, packages, selected),
        WitUnitKind::Generated { .. } => WitUnit::generated(package, packages, selected),
        WitUnitKind::Wasi { .. } => Err(WitUnitIdentityError::UnknownWasiPackage),
    }
}

fn same_unit_kind(left: &WitUnit, right: &WitUnit) -> bool {
    matches!(
        (left.kind(), right.kind()),
        (WitUnitKind::Wasi { .. }, WitUnitKind::Wasi { .. })
            | (WitUnitKind::Dir { .. }, WitUnitKind::Dir { .. })
            | (WitUnitKind::Generated { .. }, WitUnitKind::Generated { .. })
    )
}

fn package_id_for_wit_unit(kind: WitUnitKind) -> Option<PackageId> {
    match kind {
        WitUnitKind::Wasi { .. } => None,
        WitUnitKind::Dir { package } | WitUnitKind::Generated { package } => Some(package),
    }
}

fn read_wit_package_source_files(
    database: &mut QueryDatabase,
    unit: &WitUnit,
    vfs: &VfsInputs,
    contracts: &DerivedQuery<PackageKey, WitContractQueryOutput>,
    generated_texts: &DerivedQuery<PackageKey, Option<Arc<GeneratedText>>>,
    packages: &PackageInterner,
    generated: &mut Vec<(WitUnit, Arc<GeneratedText>)>,
) -> Result<Vec<WitSourceFile>, WitPackageSourceFailure> {
    match unit.kind() {
        WitUnitKind::Wasi { .. } => {
            let name = unit
                .canonical_name()
                .strip_prefix("wasi:")
                .and_then(|name| name.rsplit_once('@'))
                .map(|(name, _)| name)
                .ok_or(WitPackageSourceFailure::DirectoryNotDeclared)?;
            embedded_wasi_package(name)
                .map(|package| package.source_files())
                .ok_or(WitPackageSourceFailure::DirectoryNotDeclared)
        }
        WitUnitKind::Generated { package } => {
            let package_name = packages
                .resolve(package)
                .map_err(|error| WitPackageSourceFailure::Contract(format!("{error:?}")))?;
            let key = PackageKey::new(package_name.as_ref())
                .map_err(|error| WitPackageSourceFailure::Contract(format!("{error:?}")))?;
            let snapshot = generated_texts
                .get(database, key)
                .map_err(WitPackageSourceFailure::Query)?
                .as_ref()
                .clone()
                .ok_or(WitPackageSourceFailure::GeneratedTextMissing)?;
            generated.push((unit.clone(), Arc::clone(&snapshot)));
            Ok(snapshot.source_input().files().to_vec())
        }
        WitUnitKind::Dir { package } => {
            let package_name = packages
                .resolve(package)
                .map_err(|error| WitPackageSourceFailure::Contract(format!("{error:?}")))?;
            let key = PackageKey::new(package_name.as_ref())
                .map_err(|error| WitPackageSourceFailure::Contract(format!("{error:?}")))?;
            let contract = contracts
                .get(database, key.clone())
                .map_err(WitPackageSourceFailure::Query)?;
            let contract = contract
                .result
                .as_ref()
                .map_err(|error| WitPackageSourceFailure::Contract(format!("{error:?}")))?;
            let expected_directory = contract
                .wit_directory()
                .ok_or(WitPackageSourceFailure::DirectoryNotDeclared)?;
            let snapshot = vfs
                .wit_files(database, key.clone())
                .map_err(WitPackageSourceFailure::Query)?
                .ok_or(WitPackageSourceFailure::InventoryMissing)?;
            let (directory, files) = match snapshot {
                WitFilesSnapshot::NotApplicable => {
                    return Err(WitPackageSourceFailure::DirectoryNotApplicable);
                }
                WitFilesSnapshot::Listed { directory, files } => (directory, files),
            };
            if directory != expected_directory {
                return Err(WitPackageSourceFailure::DirectoryMismatch {
                    expected: expected_directory.to_owned(),
                    found: directory,
                });
            }

            let prefix = (expected_directory != ".").then(|| format!("{expected_directory}/"));
            let mut sources = Vec::with_capacity(files.len());
            for path in files.iter() {
                let relative_path = match prefix.as_deref() {
                    Some(prefix) => path.as_str().strip_prefix(prefix).ok_or_else(|| {
                        WitPackageSourceFailure::DirectoryMismatch {
                            expected: expected_directory.to_owned(),
                            found: path.as_str().to_owned(),
                        }
                    })?,
                    None => path.as_str(),
                };
                let file_key = FileKey::from_package_path(key.as_str().to_owned(), path.clone())
                    .map_err(|_| WitPackageSourceFailure::InvalidFileKey {
                        package_relative_path: path.as_str().to_owned(),
                    })?;
                let contents = vfs
                    .file_contents(database, file_key)
                    .map_err(WitPackageSourceFailure::Query)?
                    .ok_or_else(|| WitPackageSourceFailure::FileInputMissing {
                        package_relative_path: path.as_str().to_owned(),
                    })?;
                match contents {
                    FileContents::Bytes(bytes) => sources.push(WitSourceFile::new(
                        relative_path.to_owned(),
                        bytes.as_ref().to_vec(),
                    )),
                    FileContents::ReadError { io_error_kind } => {
                        return Err(WitPackageSourceFailure::FileReadError {
                            package_relative_path: path.as_str().to_owned(),
                            io_error_kind,
                        });
                    }
                }
            }
            Ok(sources)
        }
    }
}

fn finish_load(
    identity: Fingerprint,
    selected_version: Option<String>,
    loaded: Result<LoadedWitPackage, WitPackageLoadError<WitPackageSourceFailure>>,
    generated_texts: Vec<(WitUnit, Arc<GeneratedText>)>,
) -> Result<WitPackageQueryOutput, QueryError> {
    if let Err(error) = &loaded
        && let WitPackageLoadFailure::Source(WitPackageSourceFailure::Query(query_error)) =
            error.failure()
    {
        return Err(query_error.clone());
    }
    let result = loaded
        .map(|package| WitPackageQueryValue {
            package: Arc::new(package),
            generated_texts: generated_texts.into(),
        })
        .map_err(WitPackageFailure::Load);
    Ok(WitPackageQueryOutput {
        identity,
        selected_version,
        result,
    })
}

fn failed_output(
    identity: Fingerprint,
    selected_version: Option<String>,
    failure: WitPackageFailure,
) -> WitPackageQueryOutput {
    WitPackageQueryOutput {
        identity,
        selected_version,
        result: Err(failure),
    }
}

pub(super) fn canonicalize_wit_unit(
    unit: &WitUnit,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<WitUnit, WitPackageQueryError> {
    match unit.kind() {
        WitUnitKind::Wasi { .. } => {
            canonicalize_wasi_wit_unit(unit, symbols).map_err(WitPackageQueryError::InvalidUnit)
        }
        WitUnitKind::Dir { .. } | WitUnitKind::Generated { .. } => {
            canonicalize_package_wit_unit(unit, packages).map_err(WitPackageQueryError::InvalidUnit)
        }
    }
}

fn canonicalize_wasi_wit_unit(unit: &WitUnit, symbols: &Interner) -> Result<WitUnit, String> {
    let Some(name_and_version) = unit.canonical_name().strip_prefix("wasi:") else {
        return Err("WASI unit has a non-canonical package name".to_owned());
    };
    let Some((name, version)) = name_and_version.rsplit_once('@') else {
        return Err("WASI unit has no selected version".to_owned());
    };
    let version =
        Version::parse(version).map_err(|error| format!("invalid WASI version: {error}"))?;
    let package = symbols
        .intern(name)
        .map_err(|error| format!("cannot intern WASI package: {error:?}"))?;
    let canonical = WitUnit::wasi(package, symbols, &version)
        .map_err(|error| format!("invalid WASI unit: {error:?}"))?;
    if canonical.stable_key() != unit.stable_key()
        || canonical.canonical_name() != unit.canonical_name()
    {
        return Err("WASI unit stable identity does not match its canonical name".to_owned());
    }
    Ok(canonical)
}

fn canonicalize_package_wit_unit(
    unit: &WitUnit,
    packages: &PackageInterner,
) -> Result<WitUnit, String> {
    let (prefix, is_dir) = match unit.kind() {
        WitUnitKind::Dir { .. } => ("wit:dir:", true),
        WitUnitKind::Generated { .. } => ("wit:gen:", false),
        WitUnitKind::Wasi { .. } => return Err("expected a package WIT unit".to_owned()),
    };
    let Some(package_key_text) = unit.stable_key().strip_prefix(prefix) else {
        return Err("WIT unit has an invalid stable-key prefix".to_owned());
    };
    let package_key = PackageKey::new(package_key_text)
        .map_err(|error| format!("invalid WIT package key: {error:?}"))?;
    let Some((identity, version_text)) = unit.canonical_name().rsplit_once('@') else {
        return Err("WIT unit has no selected package version".to_owned());
    };
    let version = Version::parse(version_text)
        .map_err(|error| format!("invalid WIT package version: {error}"))?;
    let Some((key_identity, key_line)) = package_key.as_str().split_once('#') else {
        return Err("WIT unit package key has no compatibility line".to_owned());
    };
    if identity != key_identity || version.compatibility_line().to_string() != key_line {
        return Err("WIT unit name and stable package key disagree".to_owned());
    }
    let selected = MvsPackageVersion::new(identity.to_owned(), version)
        .map_err(|error| format!("invalid selected package version: {error:?}"))?;
    let package = packages
        .intern(package_key.as_str())
        .map_err(|error| format!("cannot intern WIT package key: {error:?}"))?;
    let canonical = if is_dir {
        WitUnit::dir(package, packages, &selected)
    } else {
        WitUnit::generated(package, packages, &selected)
    }
    .map_err(|error| format!("invalid WIT unit identity: {error:?}"))?;
    if canonical.stable_key() != unit.stable_key()
        || canonical.canonical_name() != unit.canonical_name()
    {
        return Err("WIT unit stable identity does not match its canonical name".to_owned());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_graph::PackageGraphEdge;

    fn package_version(identity: &str, version: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(
            identity.to_owned(),
            Version::parse(version).expect("test version is valid"),
        )
        .expect("test package version is valid")
    }

    fn root_node(
        key: &str,
        identity: &str,
        version: &str,
        edges: impl IntoIterator<Item = PackageGraphEdge>,
    ) -> PackageGraphNode {
        PackageGraphNode::new(
            PackageKey::new(key).expect("test package key is valid"),
            identity,
            Version::parse(version).expect("test version is valid"),
            PackageForm::Source,
            PackageGraphRole::Root,
            edges,
        )
        .expect("test root node is valid")
    }

    fn source_node(
        key: &str,
        identity: &str,
        version: &str,
        form: PackageForm,
    ) -> PackageGraphNode {
        PackageGraphNode::new(
            PackageKey::new(key).expect("test package key is valid"),
            identity,
            Version::parse(version).expect("test version is valid"),
            form,
            PackageGraphRole::Dependency,
            [],
        )
        .expect("test dependency node is valid")
    }

    fn graph(nodes: impl IntoIterator<Item = PackageGraphNode>) -> PackageGraph {
        PackageGraph::new(
            Version::parse("1.8.2").expect("toolchain version is valid"),
            nodes,
        )
        .expect("test package graph is valid")
    }

    fn root_dir_database() -> (RootDatabase, PackageId, MvsPackageVersion, WitUnit) {
        let mut database = RootDatabase::new().expect("driver initializes");
        let package = database
            .intern_package("acme:root#1")
            .expect("root key interns");
        let selected = package_version("acme:root", "1.2.3");
        let node = root_node("acme:root#1", "acme:root", "1.2.3", []);
        database
            .set_package_graph(Some(graph([node])))
            .expect("root graph is set");
        database
            .set_package_manifest(
                package,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("root contract is tracked");
        let unit = WitUnit::dir(package, &database.packages, &selected)
            .expect("root directory unit is valid");
        (database, package, selected, unit)
    }

    fn set_root_wit_source(database: &mut RootDatabase, source: &[u8]) {
        let path = PackageRelativePath::new("wit/root.wit").expect("WIT path is valid");
        let key = PackageKey::new("acme:root#1").expect("root key is valid");
        database
            .set_wit_files(
                key,
                WitFilesSnapshot::listed("wit", [path.clone()]).expect("WIT inventory is valid"),
                Durability::Low,
            )
            .expect("WIT inventory is tracked");
        database
            .set_file_contents(
                FileKey::from_package_path("acme:root#1", path).expect("WIT file key is valid"),
                Arc::from(source),
                Durability::Low,
            )
            .expect("WIT bytes are tracked");
    }

    #[test]
    fn dir_query_preserves_inventory_file_absence_and_read_failures() {
        let (mut database, _, _, unit) = root_dir_database();
        let path = PackageRelativePath::new("wit/root.wit").expect("WIT path is valid");
        let package_key = PackageKey::new("acme:root#1").expect("root key is valid");

        database
            .set_wit_files(
                package_key.clone(),
                WitFilesSnapshot::not_applicable(),
                Durability::Low,
            )
            .expect("not-applicable inventory is tracked");
        let not_applicable = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("not-applicable inventory fails");
        assert!(matches!(
            not_applicable.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(
                    error.failure(),
                    WitPackageLoadFailure::Source(
                        WitPackageSourceFailure::DirectoryNotApplicable
                    )
                )
        ));

        database
            .remove_wit_files(package_key.clone())
            .expect("inventory input is removed");
        let missing_inventory = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("missing inventory fails");
        assert!(matches!(
            missing_inventory.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(
                    error.failure(),
                    WitPackageLoadFailure::Source(WitPackageSourceFailure::InventoryMissing)
                )
        ));

        database
            .set_wit_files(
                package_key,
                WitFilesSnapshot::listed("wit", [path.clone()]).expect("WIT inventory is valid"),
                Durability::Low,
            )
            .expect("WIT inventory is tracked");
        let file_key =
            FileKey::from_package_path("acme:root#1", path).expect("WIT file key is valid");
        database
            .set_file_read_error(file_key.clone(), "PermissionDenied", Durability::Low)
            .expect("read failure is tracked");
        let read_error = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("read error fails");
        assert!(matches!(
            read_error.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(
                    error.failure(),
                    WitPackageLoadFailure::Source(
                        WitPackageSourceFailure::FileReadError {
                            io_error_kind,
                            ..
                        }
                    ) if io_error_kind == "PermissionDenied"
                )
        ));

        database
            .remove_file(file_key)
            .expect("file input is removed");
        let missing_file = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("missing file input fails");
        assert!(matches!(
            missing_file.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(
                    error.failure(),
                    WitPackageLoadFailure::Source(
                        WitPackageSourceFailure::FileInputMissing { .. }
                    )
                )
        ));
    }

    #[test]
    fn dir_query_checks_directory_and_empty_inventory_is_a_parse_error() {
        let (mut database, _, _, unit) = root_dir_database();
        let wrong_path = PackageRelativePath::new("other/root.wit").expect("WIT path is valid");
        database
            .set_wit_files(
                PackageKey::new("acme:root#1").expect("root key is valid"),
                WitFilesSnapshot::listed("other", [wrong_path])
                    .expect("other-directory inventory is valid"),
                Durability::Low,
            )
            .expect("WIT inventory is tracked");
        let wrong_directory = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("directory mismatch fails");
        assert!(matches!(
            wrong_directory.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(
                    error.failure(),
                    WitPackageLoadFailure::Source(
                        WitPackageSourceFailure::DirectoryMismatch { .. }
                    )
                )
        ));

        database
            .set_wit_files(
                PackageKey::new("acme:root#1").expect("root key is valid"),
                WitFilesSnapshot::listed("wit", []).expect("empty WIT inventory is valid"),
                Durability::Low,
            )
            .expect("empty WIT inventory is tracked");
        let empty = database
            .wit_package(&unit)
            .expect("WIT query succeeds")
            .failure
            .expect("empty inventory reaches parsing");
        assert!(matches!(
            empty.as_ref(),
            WitPackageFailure::Load(error)
                if matches!(error.failure(), WitPackageLoadFailure::Parse(_))
        ));
    }

    #[test]
    fn generated_dependency_loads_only_through_an_active_edge_and_tracks_text() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let root = database
            .intern_package("acme:root#1")
            .expect("root key interns");
        let dependency = database
            .intern_package("acme:dep#2")
            .expect("dependency key interns");
        let root_edge = PackageGraphEdge::new(
            PackageKey::new("acme:dep#2").expect("dependency key is valid"),
            "acme:dep",
        )
        .expect("root edge is valid");
        database
            .set_package_graph(Some(graph([
                root_node("acme:root#1", "acme:root", "1.2.3", [root_edge]),
                source_node("acme:dep#2", "acme:dep", "2.0.0", PackageForm::Source),
            ])))
            .expect("active graph is set");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("root contract is tracked");
        database
            .set_package_manifest(
                dependency,
                Arc::from(&b"package acme:dep @ 2.0.0\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("dependency contract is tracked");
        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\nworld app { import acme:dep/api; }",
        );
        let generated = |text: &str| GeneratedText {
            text: Arc::from(text),
            complete: true,
            origins: Arc::from([]),
        };
        database
            .set_generated_wit_text(
                dependency,
                generated("package acme:dep@2.0.0;\ninterface api {}"),
                Durability::Low,
            )
            .expect("generated WIT is tracked");
        let selected = package_version("acme:root", "1.2.3");
        let root_unit =
            WitUnit::dir(root, &database.packages, &selected).expect("root unit is valid");

        let first = database
            .wit_package(&root_unit)
            .expect("WIT query succeeds");
        let first_package = first
            .package
            .unwrap_or_else(|| panic!("root closure fails: {:?}", first.failure.as_deref()));
        assert_eq!(first_package.closure().len(), 2);
        assert_eq!(
            first_package.dependency_models()[0].unit.canonical_name(),
            "acme:dep@2.0.0"
        );
        assert_eq!(first.generated_texts.len(), 1);

        database
            .set_generated_wit_text(
                dependency,
                generated("package acme:dep@2.0.0;\ninterface api { type replacement = u32; }"),
                Durability::Low,
            )
            .expect("updated generated WIT is tracked");
        let second = database
            .wit_package(&root_unit)
            .expect("WIT query recomputes");
        let second_package = second.package.expect("updated root closure loads");
        assert_ne!(
            sai_stable_hash::fingerprint(first_package.as_ref()),
            sai_stable_hash::fingerprint(second_package.as_ref()),
            "reachable generated text changes the tracked package model"
        );
    }

    #[test]
    fn unrelated_unit_projection_failure_does_not_block_a_reachable_closure() {
        let (mut database, root, _, root_unit) = root_dir_database();
        let reachable = database
            .intern_package("acme:reachable#2")
            .expect("reachable package key interns");
        let unrelated = database
            .intern_package("acme:unrelated#3")
            .expect("unrelated package key interns");
        let root_edges = [
            PackageGraphEdge::new(
                PackageKey::new("acme:reachable#2").expect("reachable key is valid"),
                "acme:reachable",
            )
            .expect("reachable edge is valid"),
            PackageGraphEdge::new(
                PackageKey::new("acme:unrelated#3").expect("unrelated key is valid"),
                "acme:unrelated",
            )
            .expect("unrelated edge is valid"),
        ];
        database
            .set_package_graph(Some(graph([
                root_node("acme:root#1", "acme:root", "1.2.3", root_edges),
                source_node(
                    "acme:reachable#2",
                    "acme:reachable",
                    "2.0.0",
                    PackageForm::Source,
                ),
                source_node(
                    "acme:unrelated#3",
                    "acme:unrelated",
                    "3.0.0",
                    PackageForm::Artifact,
                ),
            ])))
            .expect("active graph is set");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("root contract is tracked");
        database
            .set_package_manifest(
                reachable,
                Arc::from(&b"package acme:reachable @ 2.0.0\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("reachable source contract is tracked");
        database
            .set_package_manifest(
                unrelated,
                Arc::from(&b"package acme:unrelated @ 3.0.0\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("unrelated mismatched contract is tracked");
        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\nworld app { import acme:reachable/api; }",
        );
        database
            .set_generated_wit_text(
                reachable,
                GeneratedText {
                    text: Arc::from("package acme:reachable@2.0.0;\ninterface api {}"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("reachable generated WIT is tracked");

        assert!(matches!(
            database.wit_unit_of(unrelated),
            Err(WitUnitOfError::PackageFormMismatch { .. })
        ));
        let result = database
            .wit_package(&root_unit)
            .expect("unrelated unit failure does not panic query");
        let loaded = result.package.unwrap_or_else(|| {
            panic!(
                "unrelated active package must not fail the root closure: {:?}",
                result.failure.as_deref()
            )
        });
        assert_eq!(loaded.closure().len(), 2);
        assert_eq!(
            loaded.dependency_models()[0].unit.canonical_name(),
            "acme:reachable@2.0.0"
        );
    }

    #[test]
    fn dependency_parse_failure_tracks_its_exact_same_line_version() {
        let mut database = RootDatabase::new().expect("driver initializes");
        let root = database
            .intern_package("acme:root#1")
            .expect("root key interns");
        let dependency = database
            .intern_package("acme:dep#1")
            .expect("dependency key interns");
        let edge = PackageGraphEdge::new(
            PackageKey::new("acme:dep#1").expect("dependency key is valid"),
            "acme:dep",
        )
        .expect("dependency edge is valid");
        let set_graph = |database: &mut RootDatabase, version: &str| {
            database
                .set_package_graph(Some(graph([
                    root_node("acme:root#1", "acme:root", "1.2.3", [edge.clone()]),
                    source_node("acme:dep#1", "acme:dep", version, PackageForm::Source),
                ])))
                .expect("active graph is set");
        };
        set_graph(&mut database, "1.2.3");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("root contract is tracked");
        database
            .set_package_manifest(
                dependency,
                Arc::from(&b"package acme:dep @ 1.2.3\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("dependency contract is tracked");
        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\nworld app { import acme:dep/api; }",
        );
        database
            .set_generated_wit_text(
                dependency,
                GeneratedText {
                    text: Arc::from("package acme:dep@1.2.3;\ninterface api {"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("invalid generated WIT is tracked");
        let root_selected = package_version("acme:root", "1.2.3");
        let root_unit =
            WitUnit::dir(root, &database.packages, &root_selected).expect("root unit is valid");

        let first = database
            .wit_package(&root_unit)
            .expect("first package query returns a failure value")
            .failure
            .expect("dependency parse failure is returned");
        let WitPackageFailure::Load(first) = first.as_ref() else {
            panic!("dependency parse failure is a loading failure");
        };
        assert_eq!(first.unit().canonical_name(), "acme:dep@1.2.3");

        set_graph(&mut database, "1.2.4");
        database
            .set_package_manifest(
                dependency,
                Arc::from(&b"package acme:dep @ 1.2.4\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("updated dependency contract is tracked");
        database
            .set_generated_wit_text(
                dependency,
                GeneratedText {
                    text: Arc::from("package acme:dep@1.2.4;\ninterface api {"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("updated invalid generated WIT is tracked");
        let second = database
            .wit_package(&root_unit)
            .expect("updated package query returns a failure value")
            .failure
            .expect("updated dependency parse failure is returned");
        let WitPackageFailure::Load(second) = second.as_ref() else {
            panic!("updated dependency parse failure is a loading failure");
        };
        assert_eq!(second.unit().canonical_name(), "acme:dep@1.2.4");
    }

    #[test]
    fn explicit_generated_root_and_embedded_wasi_use_the_generic_query() {
        let (mut database, root, selected, _) = root_dir_database();
        let generated_unit =
            WitUnit::generated(root, &database.packages, &selected).expect("generated root unit");
        database
            .set_generated_wit_text(
                root,
                GeneratedText {
                    text: Arc::from("package acme:root@1.2.3;\nworld app {}"),
                    complete: false,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("root generated text is tracked");
        let generated = database
            .wit_package(&generated_unit)
            .expect("generated root query succeeds");
        assert_eq!(
            generated
                .package
                .expect("explicit generated root loads")
                .root()
                .unit
                .canonical_name(),
            "acme:root@1.2.3"
        );
        assert_eq!(generated.generated_texts.len(), 1);

        let wasi = database.wasi_unit("io").expect("known WASI unit");
        let loaded = database
            .wit_package(&wasi)
            .expect("generic WASI query succeeds")
            .package
            .expect("embedded WASI closure loads");
        assert_eq!(loaded.root().unit.canonical_name(), "wasi:io@0.2.3");
        assert_eq!(loaded.closure().last(), Some(&wasi));
    }

    #[test]
    fn active_graph_version_refresh_rebuilds_a_stale_same_line_unit() {
        let (mut database, root, _, old_unit) = root_dir_database();
        let current_node = root_node("acme:root#1", "acme:root", "1.2.4", []);
        database
            .set_package_graph(Some(graph([current_node])))
            .expect("updated graph is set");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.4\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("updated contract is tracked");
        set_root_wit_source(&mut database, b"package acme:root@1.2.4;\nworld app {}");

        let current = database
            .wit_package(&old_unit)
            .expect("stale compatibility-key query rebinds to current version")
            .package
            .expect("current root loads");
        assert_eq!(current.root().unit.canonical_name(), "acme:root@1.2.4");
    }

    #[test]
    fn package_units_from_another_database_are_canonicalized_locally() {
        let (mut local, local_package, _, _) = root_dir_database();
        set_root_wit_source(&mut local, b"package acme:root@1.2.3;\nworld app {}");

        let mut foreign = RootDatabase::new().expect("foreign driver initializes");
        foreign
            .intern_package("acme:filler#1")
            .expect("foreign filler package interns");
        let foreign_package = foreign
            .intern_package("acme:root#1")
            .expect("foreign root key interns");
        foreign
            .set_package_graph(Some(graph([root_node(
                "acme:root#1",
                "acme:root",
                "1.2.3",
                [],
            )])))
            .expect("foreign active graph is set");
        foreign
            .set_package_manifest(
                foreign_package,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("foreign root contract is tracked");
        set_root_wit_source(&mut foreign, b"package acme:root@1.2.3;\nworld app {}");
        let selected = package_version("acme:root", "1.2.3");
        let foreign_unit = WitUnit::dir(foreign_package, &foreign.packages, &selected)
            .expect("foreign root unit is valid");
        assert_ne!(local_package, foreign_package);

        let result = local
            .wit_package(&foreign_unit)
            .expect("foreign package unit is reconstructed locally");
        assert_eq!(
            result
                .package
                .expect("canonicalized package loads")
                .root()
                .unit
                .canonical_name(),
            "acme:root@1.2.3"
        );
    }

    fn seed_source_interface(database: &mut RootDatabase, package: PackageId, name: &str) -> DefId {
        let def_paths = if let Some(def_paths) = database.def_paths.get(&package) {
            Arc::clone(def_paths)
        } else {
            let def_paths = Arc::new(
                DefPathInterner::new(package, &database.packages, &database.symbols)
                    .expect("source package path interner is valid"),
            );
            database.def_paths.insert(package, Arc::clone(&def_paths));
            def_paths
        };
        database
            .wit_source_def_paths
            .lock()
            .expect("shared source path map is not poisoned")
            .insert(package, Arc::clone(&def_paths));
        let symbol = database
            .symbols
            .intern(name)
            .expect("source interface name interns");
        let path = DefPath::new(
            package,
            vec![sai_hir::DisambiguatedDefPathData {
                data: DefPathData::Interface(symbol),
                disambiguator: 0,
            }],
            &database.packages,
            &database.symbols,
        )
        .expect("source interface path is valid");
        def_paths
            .intern(path)
            .expect("source interface path interns")
            .to_def_id()
    }

    #[test]
    fn dir_view_query_caches_and_reuses_definition_ids_after_input_changes() {
        let (mut database, _, _, unit) = root_dir_database();
        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\ninterface api { ping: func(); }",
        );

        let first = database.wit_view(&unit).expect("root WIT view builds");
        let cached = database
            .wit_view(&unit)
            .expect("cached root WIT view is available");
        assert!(Arc::ptr_eq(&first, &cached));
        let api_def = first.interfaces["api"].def;
        let ping_def = first.interfaces["api"].members["ping"].def;
        let first_fingerprint = sai_stable_hash::fingerprint(first.as_ref());

        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\ninterface api { ping: func() -> u32; }\ninterface extra {}",
        );
        let refreshed = database
            .wit_view(&unit)
            .expect("tracked WIT file changes rebuild the view");
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(refreshed.interfaces["api"].def, api_def);
        assert_eq!(refreshed.interfaces["api"].members["ping"].def, ping_def);
        assert!(refreshed.interfaces.contains_key("extra"));
        assert_ne!(
            first_fingerprint,
            sai_stable_hash::fingerprint(refreshed.as_ref())
        );
    }

    #[test]
    fn wit_view_returns_loader_failures_as_values_and_recovers_after_fixing_input() {
        let (mut database, _, _, unit) = root_dir_database();
        set_root_wit_source(&mut database, b"this is not a WIT package");

        assert!(matches!(
            database.wit_view(&unit),
            Err(WitViewQueryError::Package(WitPackageFailure::Load(error)))
                if matches!(error.failure(), WitPackageLoadFailure::Parse(_))
        ));

        set_root_wit_source(&mut database, b"package acme:root@1.2.3;\ninterface api {}");
        assert!(
            database.wit_view(&unit).is_ok(),
            "a tracked load failure must not poison a later valid view"
        );
    }

    #[test]
    fn generated_view_tracks_origin_only_changes_and_rejects_invalid_origins() {
        let (mut database, package, selected, _) = root_dir_database();
        let unit = WitUnit::generated(package, &database.packages, &selected)
            .expect("explicit generated root is valid");
        let first_origin = seed_source_interface(&mut database, package, "Canvas");
        let second_origin = seed_source_interface(&mut database, package, "DrawingSurface");
        let generated_text = "package acme:root@1.2.3;\ninterface canvas { ping: func(); }";
        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from(generated_text),
                    complete: true,
                    origins: Arc::from([("canvas".to_owned(), first_origin)]),
                },
                Durability::Low,
            )
            .expect("initial generated snapshot is accepted");

        let first = database.wit_view(&unit).expect("generated view builds");
        assert_eq!(first.interfaces["canvas"].origin, Some(first_origin));
        let first_identity = first.interfaces["canvas"]
            .origin_identity
            .clone()
            .expect("generated origin has stable identity");
        let interface_def = first.interfaces["canvas"].def;

        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from(generated_text),
                    complete: true,
                    origins: Arc::from([("canvas".to_owned(), second_origin)]),
                },
                Durability::Low,
            )
            .expect("origin-only generated snapshot change is accepted");
        let changed = database
            .wit_view(&unit)
            .expect("origin-only change invalidates the generated view");
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.interfaces["canvas"].origin, Some(second_origin));
        assert_eq!(changed.interfaces["canvas"].def, interface_def);
        assert_ne!(
            changed.interfaces["canvas"].origin_identity.as_ref(),
            Some(&first_identity)
        );

        let view_interner = database
            .wit_view_def_paths
            .lock()
            .expect("view path map is not poisoned")
            .get(unit.stable_key())
            .cloned()
            .expect("generated view retains its persistent path interner");
        let definitions_before_invalid_input =
            view_interner.len().expect("view path interner is readable");
        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from(generated_text),
                    complete: true,
                    origins: Arc::from([("unknown".to_owned(), second_origin)]),
                },
                Durability::Low,
            )
            .expect("unknown origin name is retained for view validation");
        assert!(matches!(
            database.wit_view(&unit),
            Err(WitViewQueryError::View(WitViewError::UnknownGeneratedOrigin(name)))
                if name == "unknown"
        ));
        assert_eq!(
            view_interner
                .len()
                .expect("view path interner remains readable"),
            definitions_before_invalid_input,
            "invalid origins must fail before allocating view definition paths"
        );
    }

    #[test]
    fn generated_view_requires_the_retained_source_package_interner() {
        let (mut database, package, selected, _) = root_dir_database();
        let unit = WitUnit::generated(package, &database.packages, &selected)
            .expect("explicit generated root is valid");
        let origin = DefId::from_foreign(
            package,
            sai_hir::IndexVec::<sai_hir::DefIndex, ()>::new()
                .push(())
                .expect("foreign definition index"),
        );
        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from("package acme:root@1.2.3;\ninterface canvas {}"),
                    complete: true,
                    origins: Arc::from([("canvas".to_owned(), origin)]),
                },
                Durability::Low,
            )
            .expect("package-local origin passes the input boundary");

        assert!(matches!(
            database.wit_view(&unit),
            Err(WitViewQueryError::View(
                WitViewError::GeneratedOriginInternerRequired
            ))
        ));
        assert!(
            !database
                .wit_view_def_paths
                .lock()
                .expect("view path map is not poisoned")
                .contains_key(unit.stable_key()),
            "missing source path provenance must not allocate view session state"
        );
    }

    #[test]
    fn generated_view_without_origins_does_not_require_a_source_interner() {
        let (mut database, package, selected, _) = root_dir_database();
        let unit = WitUnit::generated(package, &database.packages, &selected)
            .expect("explicit generated root is valid");
        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from("package acme:root@1.2.3;\nworld app {}"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("generated snapshot without origins is accepted");

        let view = database
            .wit_view(&unit)
            .expect("originless generated view needs no source interner");
        assert!(view.interfaces.is_empty());
        assert!(view.worlds.contains_key("app"));
        assert!(
            !database
                .wit_source_def_paths
                .lock()
                .expect("source path map is not poisoned")
                .contains_key(&package)
        );
    }

    #[test]
    fn generated_origin_lookup_failure_is_not_cached_before_interner_repair() {
        let (mut database, package, selected, _) = root_dir_database();
        let unit = WitUnit::generated(package, &database.packages, &selected)
            .expect("explicit generated root is valid");
        let origin = seed_source_interface(&mut database, package, "Canvas");
        let retained_source_paths = database
            .wit_source_def_paths
            .lock()
            .expect("source path map is not poisoned")
            .get(&package)
            .cloned()
            .expect("source path interner is retained");
        let unrelated_source_paths = Arc::new(
            DefPathInterner::new(package, &database.packages, &database.symbols)
                .expect("replacement source path interner is valid"),
        );
        database
            .wit_source_def_paths
            .lock()
            .expect("source path map is not poisoned")
            .insert(package, unrelated_source_paths);
        database
            .set_generated_wit_text(
                package,
                GeneratedText {
                    text: Arc::from("package acme:root@1.2.3;\ninterface canvas {}"),
                    complete: true,
                    origins: Arc::from([("canvas".to_owned(), origin)]),
                },
                Durability::Low,
            )
            .expect("generated origin snapshot is accepted");

        assert!(matches!(
            database.wit_view(&unit),
            Err(WitViewQueryError::View(WitViewError::GeneratedOriginPath {
                error: DefPathInternerError::ForeignDefinitionId,
                ..
            }))
        ));
        assert!(
            !database
                .wit_view_def_paths
                .lock()
                .expect("view path map is not poisoned")
                .contains_key(unit.stable_key()),
            "an unresolved origin must fail before the derived view query"
        );

        database
            .wit_source_def_paths
            .lock()
            .expect("source path map is not poisoned")
            .insert(package, retained_source_paths);
        let recovered = database
            .wit_view(&unit)
            .expect("view succeeds when the retained interner owns the origin");
        assert_eq!(recovered.interfaces["canvas"].origin, Some(origin));
    }

    #[test]
    fn wit_view_refreshes_the_exact_active_version_under_a_compatible_key() {
        let (mut database, root, _, old_unit) = root_dir_database();
        set_root_wit_source(&mut database, b"package acme:root@1.2.3;\ninterface api {}");
        let first = database
            .wit_view(&old_unit)
            .expect("initial exact-version view builds");
        let api_def = first.interfaces["api"].def;
        let first_fingerprint = sai_stable_hash::fingerprint(first.as_ref());

        database
            .set_package_graph(Some(graph([root_node(
                "acme:root#1",
                "acme:root",
                "1.2.4",
                [],
            )])))
            .expect("updated active version is tracked");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.4\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("updated package contract is tracked");
        set_root_wit_source(&mut database, b"package acme:root@1.2.4;\ninterface api {}");

        let refreshed = database
            .wit_view(&old_unit)
            .expect("stale compatibility-key view rebinds to current selection");
        assert_eq!(refreshed.unit.canonical_name(), "acme:root@1.2.4");
        assert_eq!(refreshed.interfaces["api"].def, api_def);
        assert_ne!(
            first_fingerprint,
            sai_stable_hash::fingerprint(refreshed.as_ref()),
            "exact selected versions participate in the view fingerprint"
        );
    }

    #[test]
    fn wit_view_tracks_exact_versions_of_reachable_dependencies() {
        let (mut database, root, _, root_unit) = root_dir_database();
        let dependency = database
            .intern_package("acme:dep#2")
            .expect("dependency package key interns");
        let edge = PackageGraphEdge::new(
            PackageKey::new("acme:dep#2").expect("dependency key is valid"),
            "acme:dep",
        )
        .expect("active dependency edge is valid");
        let set_graph = |database: &mut RootDatabase, dependency_version: &str| {
            database
                .set_package_graph(Some(graph([
                    root_node("acme:root#1", "acme:root", "1.2.3", [edge.clone()]),
                    source_node(
                        "acme:dep#2",
                        "acme:dep",
                        dependency_version,
                        PackageForm::Source,
                    ),
                ])))
                .expect("updated active package graph is tracked");
        };
        set_graph(&mut database, "2.4.1");
        database
            .set_package_manifest(
                root,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("root contract is tracked");
        database
            .set_package_manifest(
                dependency,
                Arc::from(&b"package acme:dep @ 2.4.1\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("dependency contract is tracked");
        set_root_wit_source(
            &mut database,
            b"package acme:root@1.2.3;\nworld app { import acme:dep/api; }",
        );
        database
            .set_generated_wit_text(
                dependency,
                GeneratedText {
                    text: Arc::from("package acme:dep@2.4.1;\ninterface api {}"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("first dependency WIT snapshot is tracked");
        let first = database
            .wit_view(&root_unit)
            .expect("root view loads its active generated dependency");

        set_graph(&mut database, "2.4.2");
        database
            .set_package_manifest(
                dependency,
                Arc::from(&b"package acme:dep @ 2.4.2\nsource \"src\"\n"[..]),
                PackageRole::Dependency,
                Durability::Low,
            )
            .expect("updated dependency contract is tracked");
        database
            .set_generated_wit_text(
                dependency,
                GeneratedText {
                    text: Arc::from("package acme:dep@2.4.2;\ninterface api {}"),
                    complete: true,
                    origins: Arc::from([]),
                },
                Durability::Low,
            )
            .expect("updated exact dependency snapshot is tracked");
        let refreshed = database
            .wit_view(&root_unit)
            .expect("reachable same-line version change refreshes the view query");
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(refreshed.unit.canonical_name(), "acme:root@1.2.3");
    }

    #[test]
    fn wit_view_canonicalizes_foreign_session_units_and_shares_wasi_interner() {
        let (mut local, local_package, selected, local_unit) = root_dir_database();
        set_root_wit_source(&mut local, b"package acme:root@1.2.3;\ninterface api {}");

        let mut foreign = RootDatabase::new().expect("foreign driver initializes");
        foreign
            .intern_package("acme:filler#1")
            .expect("foreign filler package interns first");
        let foreign_package = foreign
            .intern_package("acme:root#1")
            .expect("foreign root package interns");
        foreign
            .set_package_graph(Some(graph([root_node(
                "acme:root#1",
                "acme:root",
                "1.2.3",
                [],
            )])))
            .expect("foreign active graph is set");
        foreign
            .set_package_manifest(
                foreign_package,
                Arc::from(&b"package acme:root @ 1.2.3\nsource \"src\"\nwit \"wit\"\n"[..]),
                PackageRole::Root,
                Durability::Low,
            )
            .expect("foreign contract is tracked");
        set_root_wit_source(&mut foreign, b"package acme:root@1.2.3;\ninterface api {}");
        let foreign_unit = WitUnit::dir(foreign_package, &foreign.packages, &selected)
            .expect("foreign WIT unit is valid");
        assert_ne!(local_package, foreign_package);

        let foreign_first = local
            .wit_view(&foreign_unit)
            .expect("foreign WIT unit is reconstructed locally");
        let local_view = local
            .wit_view(&local_unit)
            .expect("local unit reaches the same canonical query key");
        assert!(Arc::ptr_eq(&foreign_first, &local_view));
        assert_eq!(foreign_first.unit.canonical_name(), "acme:root@1.2.3");
        assert_eq!(
            foreign_first
                .package_key(&local.packages)
                .expect("local view package key"),
            local_unit.stable_key()
        );

        let wasi = local.wasi_unit("io").expect("known embedded WASI unit");
        let generic_wasi_view = local
            .wit_view(&wasi)
            .expect("generic WIT view query can build an embedded root");
        let compatibility_wasi_view = local
            .wasi_view(wasi.clone())
            .expect("existing WASI view API remains available");
        assert!(Arc::ptr_eq(&generic_wasi_view, &compatibility_wasi_view));

        let foreign_symbols = RootDatabase::new().expect("foreign symbol session initializes");
        for index in 0..16 {
            foreign_symbols
                .symbols
                .intern(&format!("foreign-wasi-prefix-{index}"))
                .expect("foreign prefix symbol interns");
        }
        let foreign_wasi = foreign_symbols
            .wasi_unit("io")
            .expect("foreign-session WASI unit is valid");
        let canonical_wasi_view = local
            .wit_view(&foreign_wasi)
            .expect("foreign WASI symbols are reconstructed from canonical identity");
        assert!(Arc::ptr_eq(&generic_wasi_view, &canonical_wasi_view));
    }
}

impl RootDatabase {
    /// Loads one WIT package and the tracked closure of its active foreign
    /// references. Only raw VFS inputs and generated-WIT snapshots are read.
    pub fn wit_package(
        &mut self,
        unit: &WitUnit,
    ) -> Result<WitPackageResult, WitPackageQueryError> {
        let unit = canonicalize_wit_unit(unit, &self.packages, &self.symbols)?;
        let output = self
            .wit_packages
            .query
            .get(&mut self.query_database, unit)?;
        match &output.result {
            Ok(value) => Ok(WitPackageResult {
                package: Some(Arc::clone(&value.package)),
                failure: None,
                generated_texts: Arc::clone(&value.generated_texts),
            }),
            Err(failure) => Ok(WitPackageResult {
                package: None,
                failure: Some(Arc::new(failure.clone())),
                generated_texts: Arc::from([]),
            }),
        }
    }

    /// Builds the tracked SAI-facing view of a WIT unit and its loaded root
    /// package. Incoming units are reconstructed from their canonical identity
    /// so session-local package and symbol IDs cannot alias another database.
    pub fn wit_view(&mut self, unit: &WitUnit) -> Result<Arc<WitView>, WitViewQueryError> {
        let unit = canonicalize_wit_unit(unit, &self.packages, &self.symbols)
            .map_err(WitViewQueryError::from)?;
        if let WitUnitKind::Generated { package } = unit.kind() {
            let package_name = self
                .packages
                .resolve(package)
                .map_err(|error| WitViewQueryError::InvalidUnit(format!("{error:?}")))?;
            let package_key = PackageKey::new(package_name.as_ref())
                .map_err(|error| WitViewQueryError::InvalidUnit(format!("{error:?}")))?;
            let generated = self
                .generated_wit_texts
                .query
                .get(&mut self.query_database, package_key)
                .map_err(WitViewQueryError::Query)?;
            if let Some(generated) = generated
                .as_ref()
                .as_ref()
                .filter(|generated| !generated.origins.is_empty())
            {
                // Source DefPathInterner contents are session state rather
                // than a query input. Validate origin IDs before querying so
                // a temporarily missing path cannot become a cached failure.
                let source_def_paths = self
                    .wit_source_def_paths
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&package)
                    .cloned()
                    .ok_or(WitViewQueryError::View(
                        WitViewError::GeneratedOriginInternerRequired,
                    ))?;
                validate_generated_origin_definitions(&generated.origins, &source_def_paths)
                    .map_err(WitViewQueryError::View)?;
            }
        }
        let output = self
            .wit_views
            .query
            .get(&mut self.query_database, unit)
            .map_err(WitViewQueryError::Query)?;
        output.result.clone()
    }
}
