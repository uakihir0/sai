//! Resolution helpers for the package-level `export world` declaration.

use super::{Phase1PackageScopesCollection, RootDatabase};
use sai_hir::DefId;
use sai_intern::{InternError, Interner};
use sai_resolve::ModuleId;
use sai_resolve::module_target::{ModuleTargetError, resolve_module_target};
use sai_resolve::package_scopes::{DefKind, Res};

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
}
