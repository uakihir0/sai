//! Resolution helpers for the package-level `export world` declaration.

use super::{Phase1PackageScopesCollection, RootDatabase, WitViewQueryError};
use sai_diagnostics::{Code, Diagnostic, Location, Severity, SourceRef};
use sai_hir::DefId;
use sai_intern::{InternError, Interner, PackageId, PackageIdError};
use sai_package::mvs::{MvsInputError, MvsPackageVersion};
use sai_query::QueryError;
use sai_resolve::module_target::{ModuleTargetError, resolve_module_target};
use sai_resolve::package_scopes::{DefKind, Res};
use sai_resolve::{ModuleId, sai_identifier_to_wit_name};
use sai_vfs::{
    FileId, FileInternerError, PackageKey, PackageRelativePath, SnapshotFile, TextRange, TextSize,
    WorkspaceSnapshot,
};
use sai_wit::{WitUnit, WitUnitIdentityError};
use std::ops::Range;
use std::sync::Arc;

/// The result of looking up a local `WORD` path in a source or hybrid package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalExportWorldLookup {
    Resolved(DefId),
    Missing,
}

/// Invalid compiler state encountered while resolving a local export-world path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalExportWorldLookupError {
    SessionMismatch,
    SymbolIntern(InternError),
    ModuleTarget(ModuleTargetError),
    MissingModuleScope(ModuleId),
}

/// Invalid state encountered while projecting a local world from a package
/// manifest and binding its diagnostics to the current source snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportWorldQueryError {
    Package(PackageIdError),
    PackageKey(sai_vfs::PackageKeyError),
    FileInterner(FileInternerError),
    Query(QueryError),
    Contract(super::WitContractQueryError),
    CollectionSessionMismatch,
    CollectionPackageMismatch {
        expected: String,
        found: String,
    },
    CollectionSnapshotMismatch,
    NonLocalWorldReference(String),
    UnsupportedPackageKind,
    InvalidArtifactWorldName(String),
    PackageVersion(MvsInputError),
    ArtifactWitUnit(WitUnitIdentityError),
    WitView(WitViewQueryError),
    MissingManifestFileKey(FileId),
    ManifestFileKeyMismatch {
        expected_package_key: String,
        found_package_key: String,
        found_path: String,
    },
    MissingManifestSnapshot(FileId),
    ManifestSnapshotFileMismatch(FileId),
    ManifestSnapshotPackageMismatch {
        expected: String,
        found: Option<String>,
    },
    ManifestSnapshotPathMismatch(String),
    ManifestSnapshotBytesMismatch,
    ManifestSpanOutOfBounds {
        span: Range<usize>,
        byte_len: usize,
    },
    ManifestSpanTooLarge(Range<usize>),
    InvalidManifestSpan(Range<usize>),
    LocalLookup(LocalExportWorldLookupError),
    DiagnosticSnapshotMismatch,
}

struct BoundExportWorldManifest<'a> {
    contract: Arc<sai_wit::WitContract>,
    file_id: FileId,
    file: &'a SnapshotFile,
    package_identity: String,
}

fn manifest_world_location(
    manifest_file: FileId,
    manifest: &SnapshotFile,
    span: Range<usize>,
) -> Result<Location, ExportWorldQueryError> {
    if span.start > span.end || span.end > manifest.source().raw_bytes().len() {
        return Err(ExportWorldQueryError::ManifestSpanOutOfBounds {
            span,
            byte_len: manifest.source().raw_bytes().len(),
        });
    }
    let start = u32::try_from(span.start)
        .map_err(|_| ExportWorldQueryError::ManifestSpanTooLarge(span.clone()))?;
    let end = u32::try_from(span.end)
        .map_err(|_| ExportWorldQueryError::ManifestSpanTooLarge(span.clone()))?;
    let range = TextRange::new(TextSize::new(start), TextSize::new(end))
        .ok_or(ExportWorldQueryError::InvalidManifestSpan(span))?;
    Ok(Location {
        file: SourceRef {
            path: manifest.path().to_owned(),
            package: manifest.package().map(str::to_owned),
            snapshot: manifest.document_version(),
        },
        span: sai_diagnostics::Span::new(manifest_file, range),
    })
}

fn export_world_diagnostic_result(
    code: Code,
    message: String,
    primary_label: &str,
    location: Location,
    package_identity: &str,
    snapshot: &WorkspaceSnapshot,
) -> Result<sai_resolve::ExportWorld, ExportWorldQueryError> {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message, Some(location));
    diagnostic.primary_label = primary_label.to_owned();
    diagnostic.package = Some(package_identity.to_owned());
    Ok(sai_resolve::ExportWorld {
        world: None,
        diagnostics: vec![
            diagnostic
                .with_workspace_snapshot(snapshot)
                .map_err(|_| ExportWorldQueryError::DiagnosticSnapshotMismatch)?,
        ],
    })
}

/// Resolves a slash-separated local world name using the finalized package
/// scopes and their matching module tree.
///
/// An unresolved module or a final name that is not a world is a semantic
/// miss. Structural mismatches between the module tree and collected scopes
/// remain runtime errors for the caller.
pub(super) fn lookup_local_export_world(
    collection: &Phase1PackageScopesCollection,
    path: &str,
    symbols: &Interner,
) -> Result<LocalExportWorldLookup, LocalExportWorldLookupError> {
    let mut segments = path.split('/');
    let Some(first) = segments.next() else {
        return Ok(LocalExportWorldLookup::Missing);
    };
    let mut all_segments = vec![first];
    all_segments.extend(segments);
    if all_segments.iter().any(|segment| segment.is_empty()) {
        return Ok(LocalExportWorldLookup::Missing);
    }
    let Some((world_name, module_names)) = all_segments.split_last() else {
        return Ok(LocalExportWorldLookup::Missing);
    };

    let module_id = if module_names.is_empty() {
        ModuleId::ROOT
    } else {
        let module_symbols = module_names
            .iter()
            .map(|name| {
                symbols
                    .intern(name)
                    .map_err(LocalExportWorldLookupError::SymbolIntern)
            })
            .collect::<Result<Vec<_>, _>>()?;
        match resolve_module_target(
            &collection.module_tree,
            ModuleId::ROOT,
            &module_symbols,
            symbols,
        ) {
            Ok(module) => module,
            Err(ModuleTargetError::UnknownModule { .. }) => {
                return Ok(LocalExportWorldLookup::Missing);
            }
            Err(error) => return Err(LocalExportWorldLookupError::ModuleTarget(error)),
        }
    };

    if collection.module_tree.module(module_id).is_none() {
        return Err(LocalExportWorldLookupError::ModuleTarget(
            ModuleTargetError::MissingRootModule,
        ));
    }

    let module_scopes = collection
        .scopes
        .module(module_id)
        .ok_or(LocalExportWorldLookupError::MissingModuleScope(module_id))?;
    let world_symbol = symbols
        .intern(world_name)
        .map_err(LocalExportWorldLookupError::SymbolIntern)?;
    Ok(
        match module_scopes
            .members
            .get(&world_symbol)
            .map(|binding| binding.res)
        {
            Some(Res::Def(DefKind::World, world)) => LocalExportWorldLookup::Resolved(world),
            _ => LocalExportWorldLookup::Missing,
        },
    )
}

impl RootDatabase {
    /// Resolves a local `export world` path against one finalized collection.
    ///
    /// The collection must have been produced by this database session.
    /// Artifact and external package references are handled by the complete
    /// package-level export-world query.
    pub fn resolve_local_export_world(
        &self,
        collection: &Phase1PackageScopesCollection,
        path: &str,
    ) -> Result<LocalExportWorldLookup, LocalExportWorldLookupError> {
        if collection.session_token != self.session_token {
            return Err(LocalExportWorldLookupError::SessionMismatch);
        }
        lookup_local_export_world(collection, path, &self.symbols)
    }

    /// Projects a source or hybrid package's local `export world` from its
    /// tracked manifest, and binds an unknown-world diagnostic to the exact
    /// manifest file in the current workspace snapshot.
    ///
    /// External references and artifact-local names are handled by the full
    /// package export-world query.
    pub fn resolve_local_export_world_from_contract(
        &mut self,
        package: PackageId,
        collection: &Phase1PackageScopesCollection,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<sai_resolve::ExportWorld, ExportWorldQueryError> {
        if collection.session_token != self.session_token {
            return Err(ExportWorldQueryError::CollectionSessionMismatch);
        }
        let package_key_text = self
            .packages
            .resolve(package)
            .map_err(ExportWorldQueryError::Package)?;
        let package_key = PackageKey::new(package_key_text.as_ref())
            .map_err(ExportWorldQueryError::PackageKey)?;
        if collection.package_key() != &package_key {
            return Err(ExportWorldQueryError::CollectionPackageMismatch {
                expected: package_key.as_str().to_owned(),
                found: collection.package_key().as_str().to_owned(),
            });
        }
        if collection.snapshot_id != snapshot.id() {
            return Err(ExportWorldQueryError::CollectionSnapshotMismatch);
        }

        let bound = self.bind_export_world_manifest(package, snapshot)?;
        if !matches!(
            bound.contract.kind(),
            sai_wit::WitPackageKind::Source | sai_wit::WitPackageKind::Hybrid
        ) {
            return Err(ExportWorldQueryError::UnsupportedPackageKind);
        }
        let Some(export_world) = bound.contract.export_world() else {
            return Ok(sai_resolve::ExportWorld::default());
        };
        if export_world.value().contains(':') {
            return Err(ExportWorldQueryError::NonLocalWorldReference(
                export_world.value().to_owned(),
            ));
        }

        let location =
            manifest_world_location(bound.file_id, bound.file, export_world.span().clone())?;

        let result =
            match lookup_local_export_world(collection, export_world.value(), &self.symbols)
                .map_err(ExportWorldQueryError::LocalLookup)?
            {
                LocalExportWorldLookup::Resolved(world) => sai_resolve::ExportWorld {
                    world: Some(world),
                    diagnostics: Vec::new(),
                },
                LocalExportWorldLookup::Missing => {
                    return export_world_diagnostic_result(
                        Code::UnknownExportWorld,
                        format!("export world `{}` names no world", export_world.value()),
                        "unknown export world",
                        location,
                        &bound.package_identity,
                        snapshot,
                    );
                }
            };
        Ok(result)
    }

    /// Projects an artifact package's local `export world` through its own
    /// declared WIT directory. Root artifacts build their `Dir(P)` unit
    /// directly because root packages do not have a dependent-facing unit.
    pub fn resolve_artifact_export_world_from_contract(
        &mut self,
        package: PackageId,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<sai_resolve::ExportWorld, ExportWorldQueryError> {
        let bound = self.bind_export_world_manifest(package, snapshot)?;
        if bound.contract.kind() != sai_wit::WitPackageKind::Artifact {
            return Err(ExportWorldQueryError::UnsupportedPackageKind);
        }
        let Some(export_world) = bound.contract.export_world() else {
            return Ok(sai_resolve::ExportWorld::default());
        };
        if export_world.value().contains(':') {
            return Err(ExportWorldQueryError::NonLocalWorldReference(
                export_world.value().to_owned(),
            ));
        }
        let location =
            manifest_world_location(bound.file_id, bound.file, export_world.span().clone())?;
        if export_world.value().contains('/') {
            return Err(ExportWorldQueryError::InvalidArtifactWorldName(
                export_world.value().to_owned(),
            ));
        }
        let Some(wit_name) = sai_identifier_to_wit_name(export_world.value()) else {
            return Err(ExportWorldQueryError::InvalidArtifactWorldName(
                export_world.value().to_owned(),
            ));
        };

        let selected = MvsPackageVersion::new(
            bound.contract.identity().to_string(),
            bound.contract.version().clone(),
        )
        .map_err(ExportWorldQueryError::PackageVersion)?;
        let unit = WitUnit::dir(package, &self.packages, &selected)
            .map_err(ExportWorldQueryError::ArtifactWitUnit)?;
        let view = self
            .wit_view(&unit)
            .map_err(ExportWorldQueryError::WitView)?;
        let Some(world) = view.worlds.get(&wit_name) else {
            return export_world_diagnostic_result(
                Code::UnknownExportWorld,
                format!("export world `{}` names no WIT world", export_world.value()),
                "unknown export world",
                location,
                &bound.package_identity,
                snapshot,
            );
        };
        if let Some(unsupported) = view.model.worlds[world.wit].unsupported {
            return export_world_diagnostic_result(
                Code::WitUnsupportedFeature,
                format!(
                    "export world `{}` uses unsupported feature `{unsupported:?}`",
                    export_world.value()
                ),
                "unsupported WIT world",
                location,
                &bound.package_identity,
                snapshot,
            );
        }
        Ok(sai_resolve::ExportWorld {
            world: Some(world.def),
            diagnostics: Vec::new(),
        })
    }

    fn bind_export_world_manifest<'a>(
        &mut self,
        package: PackageId,
        snapshot: &'a WorkspaceSnapshot,
    ) -> Result<BoundExportWorldManifest<'a>, ExportWorldQueryError> {
        let package_key_text = self
            .packages
            .resolve(package)
            .map_err(ExportWorldQueryError::Package)?;
        let package_key = PackageKey::new(package_key_text.as_ref())
            .map_err(ExportWorldQueryError::PackageKey)?;
        let contract = self
            .wit_contract(package)
            .map_err(ExportWorldQueryError::Contract)?;
        let manifest_file = self
            .intern_package_file(
                package,
                PackageRelativePath::new("sai.package")
                    .expect("the package manifest path is canonical"),
            )
            .map_err(ExportWorldQueryError::FileInterner)?;
        let file_key = self
            .file_key(manifest_file)
            .ok_or(ExportWorldQueryError::MissingManifestFileKey(manifest_file))?;
        if file_key.package_key() != package_key.as_str() || file_key.path() != "sai.package" {
            return Err(ExportWorldQueryError::ManifestFileKeyMismatch {
                expected_package_key: package_key.as_str().to_owned(),
                found_package_key: file_key.package_key().to_owned(),
                found_path: file_key.path().to_owned(),
            });
        }
        let manifest =
            snapshot
                .file(manifest_file)
                .ok_or(ExportWorldQueryError::MissingManifestSnapshot(
                    manifest_file,
                ))?;
        if manifest.source().id() != manifest_file {
            return Err(ExportWorldQueryError::ManifestSnapshotFileMismatch(
                manifest_file,
            ));
        }
        let package_identity = format!("{}@{}", contract.identity(), contract.version());
        if manifest.package() != Some(package_identity.as_str()) {
            return Err(ExportWorldQueryError::ManifestSnapshotPackageMismatch {
                expected: package_identity,
                found: manifest.package().map(str::to_owned),
            });
        }
        let manifest_input = self
            .declared_requirements
            .input
            .get(&mut self.query_database, package_key)
            .map_err(ExportWorldQueryError::Query)?;
        let valid_manifest_path = if manifest_input.dependency {
            manifest.path() == format!("dep:{package_identity}/sai.package")
        } else {
            !manifest.path().starts_with("dep:")
                && (manifest.path() == "sai.package" || manifest.path().ends_with("/sai.package"))
        };
        if !valid_manifest_path {
            return Err(ExportWorldQueryError::ManifestSnapshotPathMismatch(
                manifest.path().to_owned(),
            ));
        }
        if manifest.source().raw_bytes() != manifest_input.source_bytes.as_ref() {
            return Err(ExportWorldQueryError::ManifestSnapshotBytesMismatch);
        }
        Ok(BoundExportWorldManifest {
            contract,
            file_id: manifest_file,
            file: manifest,
            package_identity,
        })
    }
}
