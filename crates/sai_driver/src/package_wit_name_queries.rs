use super::{
    DetachedWorkspaceDiagnostic, OwnedPhase1ModuleInput, Phase1PackageScopesQueryOutput,
    PreparedPhase1PackageScopes, STAGED_PHASE1_PACKAGE_SCOPES,
};
use sai_diagnostics::{Diagnostic, Location, Related, SourceRef};
use sai_hir::{DefPathError, DefPathHash, DefPathInternerError, LocalDefId};
use sai_intern::{Interner, PackageInterner};
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_resolve::package_scopes::{
    FilteredModule, PackageScopesError, Phase1PackageScopes, TopLevelOwnerKind,
};
use sai_resolve::{ModuleId, PackageWitNames, PackageWitNamesError, package_wit_names};
use sai_stable_hash::{Fingerprint, StableHash, StableHasher};
use sai_vfs::{PackageKey, WorkspaceSnapshot};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct PackageWitNamesMemo {
    names: PackageWitNames,
    diagnostics: Vec<DetachedWorkspaceDiagnostic>,
}

impl PartialEq for PackageWitNamesMemo {
    fn eq(&self, other: &Self) -> bool {
        self.names.names == other.names.names && self.diagnostics == other.diagnostics
    }
}

impl Eq for PackageWitNamesMemo {}

impl PackageWitNamesMemo {
    fn bind(&self, snapshot: &WorkspaceSnapshot) -> Result<PackageWitNames, PackageScopesError> {
        let diagnostics = self
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.bind_current_snapshot(snapshot))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PackageWitNames {
            names: self.names.names.clone(),
            diagnostics,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageWitNamesQueryError {
    Query(QueryError),
    Collection(PackageWitNamesError),
    PackageScopes(PackageScopesError),
    DefPathInterner(DefPathInternerError),
    DefPath(DefPathError),
    MissingCanonicalPath(LocalDefId),
    DiagnosticTemplate(String),
}

#[derive(Clone)]
pub(super) struct PackageWitNamesQueryOutput {
    pub(super) result: Result<PackageWitNamesMemo, PackageWitNamesQueryError>,
    pub(super) result_fingerprint: Fingerprint,
}

impl PartialEq for PackageWitNamesQueryOutput {
    fn eq(&self, other: &Self) -> bool {
        self.result == other.result
    }
}

impl Eq for PackageWitNamesQueryOutput {}

impl StableHash for PackageWitNamesQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.result_fingerprint.stable_hash(hasher);
    }
}

pub(super) struct PackageWitNamesQueries {
    pub(super) query: DerivedQuery<PackageKey, PackageWitNamesQueryOutput>,
}

impl PackageWitNamesQueries {
    pub(super) fn new(
        package_scopes: DerivedQuery<PackageKey, Phase1PackageScopesQueryOutput>,
        packages: Arc<PackageInterner>,
        symbols: Arc<Interner>,
    ) -> Result<Self, QueryError> {
        let query = DerivedQuery::new(
            "package_wit_names",
            move |database: &mut QueryDatabase, key: &PackageKey| {
                let scopes_output = package_scopes.get(database, key.clone())?;
                let staged = STAGED_PHASE1_PACKAGE_SCOPES.with(|staged| {
                    staged
                        .borrow()
                        .get(key)
                        .filter(|staged| staged.identity == scopes_output.identity)
                        .map(|staged| Arc::clone(&staged.prepared))
                });
                let Some(prepared) = staged else {
                    return Err(QueryError::MissingInput {
                        query: "package_wit_names_prepared_phase1_inputs".to_owned(),
                    });
                };

                let result =
                    collect_package_wit_names(&prepared, &scopes_output, &packages, &symbols);
                let result_fingerprint = match &result {
                    Ok(names) => {
                        let scopes = scopes_output.result.as_ref().map_err(|_| {
                            QueryError::MissingInput {
                                query: "package_wit_names_phase1_scopes".to_owned(),
                            }
                        })?;
                        stable_package_wit_names_fingerprint(
                            key,
                            &prepared,
                            &scopes.scopes,
                            names,
                            &prepared.def_paths,
                            &packages,
                            &symbols,
                        )
                        .map_err(|error| QueryError::Panicked {
                            query: "package_wit_names".to_owned(),
                            message: format!("stable result fingerprint failed: {error:?}"),
                        })?
                    }
                    Err(error) => package_wit_names_error_fingerprint(error),
                };

                Ok(PackageWitNamesQueryOutput {
                    result,
                    result_fingerprint,
                })
            },
        )?;
        Ok(Self { query })
    }

    pub(super) fn get(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<(PackageWitNames, Fingerprint), PackageWitNamesQueryError> {
        let output = self
            .query
            .get(database, key)
            .map_err(PackageWitNamesQueryError::Query)?;
        let result = output.result.as_ref().map_err(Clone::clone)?;
        let names = result
            .bind(snapshot)
            .map_err(PackageWitNamesQueryError::PackageScopes)?;
        Ok((names, output.result_fingerprint))
    }
}

fn collect_package_wit_names(
    prepared: &PreparedPhase1PackageScopes,
    scopes_output: &Phase1PackageScopesQueryOutput,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<PackageWitNamesMemo, PackageWitNamesQueryError> {
    let scopes_memo = scopes_output.result.as_ref().map_err(|_| {
        PackageWitNamesQueryError::DiagnosticTemplate(
            "Phase 1 package scopes failed before WIT name collection".to_owned(),
        )
    })?;
    let module_inputs =
        borrowed_module_inputs(&prepared.modules).map_err(PackageWitNamesQueryError::Collection)?;
    let mut scopes = scopes_memo.scopes.clone();
    for diagnostic in &scopes_memo.detached_workspace_diagnostics {
        scopes.diagnostics.push(
            diagnostic
                .bind(&prepared.snapshot)
                .map_err(PackageWitNamesQueryError::PackageScopes)?,
        );
    }
    let names = package_wit_names(
        &prepared.module_tree,
        &scopes,
        &module_inputs,
        packages,
        symbols,
    )
    .map_err(PackageWitNamesQueryError::Collection)?;
    let diagnostics = names
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let mut detached =
                DetachedWorkspaceDiagnostic::from_diagnostic(diagnostic).map_err(|error| {
                    PackageWitNamesQueryError::DiagnosticTemplate(format!("{error:?}"))
                })?;
            clear_diagnostic_document_versions(&mut detached);
            Ok(detached)
        })
        .collect::<Result<Vec<_>, PackageWitNamesQueryError>>()?;
    Ok(PackageWitNamesMemo {
        names: PackageWitNames {
            names: names.names,
            diagnostics: Vec::new(),
        },
        diagnostics,
    })
}

fn borrowed_module_inputs<'a>(
    modules: &'a [OwnedPhase1ModuleInput],
) -> Result<BTreeMap<ModuleId, FilteredModule<'a>>, PackageWitNamesError> {
    modules
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let raw = u32::try_from(index).map_err(|_| PackageWitNamesError::TooManyModules)?;
            Ok((
                ModuleId::from_raw(raw),
                FilteredModule {
                    filtered: &input.filtered,
                    ast_ids: &input.ast_ids,
                    root: &input.root,
                    source: input.source.as_ref(),
                    source_ref: input.source_ref.clone(),
                },
            ))
        })
        .collect()
}

fn stable_package_wit_names_fingerprint(
    package_key: &PackageKey,
    prepared: &PreparedPhase1PackageScopes,
    scopes: &Phase1PackageScopes,
    names: &PackageWitNamesMemo,
    def_paths: &sai_hir::DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<Fingerprint, PackageWitNamesQueryError> {
    let mut module_paths = HashMap::with_capacity(prepared.module_tree.modules.len());
    for module in &prepared.module_tree.modules {
        let path = module
            .path
            .iter()
            .map(|segment| {
                symbols
                    .resolve(*segment)
                    .map(|name| name.to_string())
                    .map_err(|error| {
                        PackageWitNamesQueryError::Collection(PackageWitNamesError::Symbol(error))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        module_paths.insert(module.file, path);
    }

    struct Entry {
        kind: u8,
        module_path: Vec<String>,
        source_order: usize,
        def_id: LocalDefId,
        canonical_path: DefPathHash,
    }
    let mut entries = Vec::new();
    for (source_order, owner) in scopes.top_level_owners.iter().enumerate() {
        let kind = match owner.kind {
            TopLevelOwnerKind::Interface => 0,
            TopLevelOwnerKind::World => 1,
            _ => continue,
        };
        let module_path = module_paths.get(&owner.file).cloned().ok_or_else(|| {
            PackageWitNamesQueryError::Collection(PackageWitNamesError::MissingOwnerModule(
                owner.file,
            ))
        })?;
        let def_id = owner.owner.def_id;
        let path = def_paths
            .resolve(def_id)
            .map_err(PackageWitNamesQueryError::DefPathInterner)?
            .ok_or(PackageWitNamesQueryError::MissingCanonicalPath(def_id))?;
        let canonical_path = DefPathHash::compute(&path, packages, symbols)
            .map_err(PackageWitNamesQueryError::DefPath)?;
        entries.push(Entry {
            kind,
            module_path,
            source_order,
            def_id,
            canonical_path,
        });
    }
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.module_path.cmp(&right.module_path))
            .then_with(|| left.source_order.cmp(&right.source_order))
    });

    let mut hasher = StableHasher::new();
    hasher.write_tag(0xE8);
    package_key.as_str().stable_hash(&mut hasher);
    hasher.write_len(u64::try_from(entries.len()).unwrap_or(u64::MAX));
    for (generation_order, entry) in entries.iter().enumerate() {
        hasher.write_len(u64::try_from(generation_order).unwrap_or(u64::MAX));
        entry.kind.stable_hash(&mut hasher);
        entry.module_path.stable_hash(&mut hasher);
        entry.canonical_path.raw().stable_hash(&mut hasher);
        match names.names.names.get(&entry.def_id) {
            Some(name) => {
                hasher.write_tag(0x01);
                name.as_str().stable_hash(&mut hasher);
            }
            None => hasher.write_tag(0x02),
        }
    }
    hasher.write_len(u64::try_from(names.diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in &names.diagnostics {
        diagnostic.code.as_str().stable_hash(&mut hasher);
        diagnostic.message.stable_hash(&mut hasher);
    }
    Ok(hasher.finish())
}

fn package_wit_names_error_fingerprint(error: &PackageWitNamesQueryError) -> Fingerprint {
    let mut hasher = StableHasher::new();
    hasher.write_tag(0xE9);
    match error {
        PackageWitNamesQueryError::Query(_) => hasher.write_tag(0x00),
        PackageWitNamesQueryError::Collection(_) => hasher.write_tag(0x01),
        PackageWitNamesQueryError::PackageScopes(_) => hasher.write_tag(0x02),
        PackageWitNamesQueryError::DefPathInterner(_) => hasher.write_tag(0x03),
        PackageWitNamesQueryError::DefPath(_) => hasher.write_tag(0x04),
        PackageWitNamesQueryError::MissingCanonicalPath(_) => hasher.write_tag(0x05),
        PackageWitNamesQueryError::DiagnosticTemplate(_) => hasher.write_tag(0x06),
    }
    hasher.finish()
}

fn clear_diagnostic_document_versions(diagnostic: &mut DetachedWorkspaceDiagnostic) {
    if let Some(location) = &mut diagnostic.primary {
        location.file.snapshot = None;
    }
    for label in &mut diagnostic.labels {
        label.location.file.snapshot = None;
    }
    for child in &mut diagnostic.children {
        if let Some(location) = &mut child.location {
            location.file.snapshot = None;
        }
    }
    for related in &mut diagnostic.related {
        if let Related::DeclaredHere { location, .. } = related {
            location.file.snapshot = None;
        }
    }
    if let Some(level) = &mut diagnostic.level {
        if let Some(location) = &mut level.location {
            location.file.snapshot = None;
        }
    }
}

fn refresh_location_reference(
    location: &mut Location,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), PackageScopesError> {
    let file = snapshot
        .file(location.span.file)
        .ok_or(PackageScopesError::InvalidSourceRange)?;
    location.file = SourceRef {
        path: file.path().to_owned(),
        package: file.package().map(str::to_owned),
        snapshot: file.document_version(),
    };
    Ok(())
}

trait BindCurrentSnapshot {
    fn bind_current_snapshot(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Diagnostic, PackageScopesError>;
}

impl BindCurrentSnapshot for DetachedWorkspaceDiagnostic {
    fn bind_current_snapshot(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Diagnostic, PackageScopesError> {
        let mut detached = self.clone();
        if let Some(location) = &mut detached.primary {
            refresh_location_reference(location, snapshot)?;
        }
        for label in &mut detached.labels {
            refresh_location_reference(&mut label.location, snapshot)?;
        }
        for child in &mut detached.children {
            if let Some(location) = &mut child.location {
                refresh_location_reference(location, snapshot)?;
            }
        }
        for related in &mut detached.related {
            if let Related::DeclaredHere { location, .. } = related {
                refresh_location_reference(location, snapshot)?;
            }
        }
        if let Some(level) = &mut detached.level {
            if let Some(location) = &mut level.location {
                refresh_location_reference(location, snapshot)?;
            }
        }
        detached.bind(snapshot)
    }
}
