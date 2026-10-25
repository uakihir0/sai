use super::{
    LocalEnumVariantValueTypeProjection, OwnerControlFlowResolutionError,
    Phase1ActiveSourceMemberScopes, Phase1ActiveWitViewScopes, Phase1BuildContext,
    Phase1PackageScopesCollection, RootDatabase, TypeOwnerResolutions,
};
use sai_hir::{DefId, DefPathHash, OwnerId};
use sai_query::{DerivedQuery, Durability, InputQuery, QueryDatabase, QueryError};
use sai_stable_hash::{Fingerprint, StableHash, StableHasher};
use sai_vfs::{FileId, FileKey, PackageKey, WorkspaceSnapshot};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LocalEnumVariantValueTypeQueryKey {
    owner: DefPathHash,
    variant: DefPathHash,
}

impl StableHash for LocalEnumVariantValueTypeQueryKey {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE3);
        self.owner.raw().stable_hash(hasher);
        self.variant.raw().stable_hash(hasher);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalEnumVariantValueTypeQueryIdentity {
    source_fingerprint: Fingerprint,
}

impl StableHash for LocalEnumVariantValueTypeQueryIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE4);
        self.source_fingerprint.stable_hash(hasher);
    }
}

#[derive(Clone)]
struct StagedLocalEnumVariantValueTypeInput {
    identity: Arc<LocalEnumVariantValueTypeQueryIdentity>,
    projection: Option<LocalEnumVariantValueTypeProjection>,
}

thread_local! {
    static STAGED_LOCAL_ENUM_VARIANT_VALUE_TYPES:
        RefCell<HashMap<LocalEnumVariantValueTypeQueryKey, StagedLocalEnumVariantValueTypeInput>> =
        RefCell::new(HashMap::new());
}

struct StagedLocalEnumVariantValueTypeGuard {
    key: LocalEnumVariantValueTypeQueryKey,
    previous: Option<StagedLocalEnumVariantValueTypeInput>,
}

impl StagedLocalEnumVariantValueTypeGuard {
    fn install(
        key: LocalEnumVariantValueTypeQueryKey,
        identity: Arc<LocalEnumVariantValueTypeQueryIdentity>,
        projection: Option<LocalEnumVariantValueTypeProjection>,
    ) -> Self {
        let previous = STAGED_LOCAL_ENUM_VARIANT_VALUE_TYPES.with(|staged| {
            staged.borrow_mut().insert(
                key,
                StagedLocalEnumVariantValueTypeInput {
                    identity,
                    projection,
                },
            )
        });
        Self { key, previous }
    }
}

impl Drop for StagedLocalEnumVariantValueTypeGuard {
    fn drop(&mut self) {
        STAGED_LOCAL_ENUM_VARIANT_VALUE_TYPES.with(|staged| {
            let mut staged = staged.borrow_mut();
            staged.remove(&self.key);
            if let Some(previous) = self.previous.take() {
                staged.insert(self.key, previous);
            }
        });
    }
}

#[derive(Clone)]
pub(super) struct LocalEnumVariantValueTypeQueryOutput {
    identity: Arc<LocalEnumVariantValueTypeQueryIdentity>,
    projection: Option<LocalEnumVariantValueTypeProjection>,
}

impl LocalEnumVariantValueTypeQueryOutput {
    #[cfg(test)]
    pub(super) fn input_fingerprint(&self) -> Fingerprint {
        self.identity.source_fingerprint
    }
}

impl PartialEq for LocalEnumVariantValueTypeQueryOutput {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.projection == other.projection
    }
}

impl Eq for LocalEnumVariantValueTypeQueryOutput {}

impl StableHash for LocalEnumVariantValueTypeQueryOutput {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        // The projection contains session-local DefIds and Ty interner values.
        // Its persistent identity is the complete source-derived input digest.
        self.identity.stable_hash(hasher);
    }
}

pub(super) struct LocalEnumVariantValueTypeQueries {
    input: InputQuery<LocalEnumVariantValueTypeQueryKey, LocalEnumVariantValueTypeQueryIdentity>,
    query: DerivedQuery<LocalEnumVariantValueTypeQueryKey, LocalEnumVariantValueTypeQueryOutput>,
}

impl LocalEnumVariantValueTypeQueries {
    pub(super) fn new() -> Result<Self, QueryError> {
        let input = InputQuery::<
            LocalEnumVariantValueTypeQueryKey,
            LocalEnumVariantValueTypeQueryIdentity,
        >::new("local_enum_variant_value_type_input", Durability::Low)?;
        let query_input = input.clone();
        let query = DerivedQuery::new(
            "local_enum_variant_value_type",
            move |database: &mut QueryDatabase, key: &LocalEnumVariantValueTypeQueryKey| {
                let identity = query_input.get(database, *key)?;
                let projection = STAGED_LOCAL_ENUM_VARIANT_VALUE_TYPES.with(|staged| {
                    staged
                        .borrow()
                        .get(key)
                        .filter(|staged| staged.identity.as_ref() == identity.as_ref())
                        .map(|staged| staged.projection.clone())
                });
                let Some(projection) = projection else {
                    return Err(QueryError::MissingInput {
                        query: "local_enum_variant_value_type_prepared_inputs".to_owned(),
                    });
                };
                Ok(LocalEnumVariantValueTypeQueryOutput {
                    identity,
                    projection,
                })
            },
        )?;
        Ok(Self { input, query })
    }

    fn get(
        &self,
        database: &mut QueryDatabase,
        key: LocalEnumVariantValueTypeQueryKey,
        identity: LocalEnumVariantValueTypeQueryIdentity,
        projection: Option<LocalEnumVariantValueTypeProjection>,
    ) -> Result<Arc<LocalEnumVariantValueTypeQueryOutput>, QueryError> {
        let identity = Arc::new(identity);
        let _staged =
            StagedLocalEnumVariantValueTypeGuard::install(key, Arc::clone(&identity), projection);
        self.input.set(database, key, identity.as_ref().clone())?;
        let output = self.query.get(database, key)?;
        if output.identity != identity {
            return Err(QueryError::MissingInput {
                query: "local_enum_variant_value_type_identity_mismatch".to_owned(),
            });
        }
        Ok(output)
    }
}

/// Errors from resolving stable identities or validating inputs for the
/// tracked local enum variant value-type query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalEnumVariantValueTypeQueryError {
    Resolution(OwnerControlFlowResolutionError),
    MissingDefinitionPath(DefId),
    DefinitionPathInterner(sai_hir::DefPathInternerError),
    DefinitionPath(sai_hir::DefPathError),
    MissingSourceFile { package: PackageKey, file: FileKey },
    SourceFileKey(FileId),
    SymbolLookup(sai_intern::LookupError),
    Query(QueryError),
}

impl From<OwnerControlFlowResolutionError> for LocalEnumVariantValueTypeQueryError {
    fn from(error: OwnerControlFlowResolutionError) -> Self {
        Self::Resolution(error)
    }
}

impl From<QueryError> for LocalEnumVariantValueTypeQueryError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl RootDatabase {
    /// Queries one exact local enum variant's validated value type through a
    /// tracked, source-derived input identity.
    ///
    /// Definition paths are resolved before entering the query so its key
    /// never depends on session-local `DefId` values. All local source files
    /// in the requesting package and its validated active source-member
    /// closure, target/build settings, package graph, declared requirements,
    /// primitive prelude, and WIT-view closure participate in invalidation.
    pub fn query_local_enum_variant_value_type(
        &mut self,
        resolutions: &TypeOwnerResolutions,
        collection: &Phase1PackageScopesCollection,
        snapshot: &WorkspaceSnapshot,
        member_scopes: Option<&Phase1ActiveSourceMemberScopes>,
        wit_view_scopes: Option<&Phase1ActiveWitViewScopes>,
        owner: OwnerId,
        variant: DefId,
    ) -> Result<Option<LocalEnumVariantValueTypeProjection>, LocalEnumVariantValueTypeQueryError>
    {
        validate_query_snapshot(resolutions, snapshot)?;
        let projection = resolutions.local_enum_variant_value_type_with_scopes(
            &self.semantic_types,
            collection,
            member_scopes,
            wit_view_scopes,
            owner,
            variant,
        )?;
        if projection.is_none() {
            return Ok(None);
        }

        let key = LocalEnumVariantValueTypeQueryKey {
            owner: self.definition_path_hash(owner.def_id.to_def_id())?,
            variant: self.definition_path_hash(variant)?,
        };
        let identity = LocalEnumVariantValueTypeQueryIdentity {
            source_fingerprint: self.local_enum_variant_value_source_fingerprint(
                collection,
                snapshot,
                member_scopes,
                wit_view_scopes,
            )?,
        };
        let output = self.local_enum_variant_value_types.get(
            &mut self.query_database,
            key,
            identity,
            projection,
        )?;
        Ok(output.projection.clone())
    }

    fn definition_path_hash(
        &self,
        def_id: DefId,
    ) -> Result<DefPathHash, LocalEnumVariantValueTypeQueryError> {
        let path = self
            .resolve_def_path(def_id)
            .map_err(LocalEnumVariantValueTypeQueryError::DefinitionPathInterner)?
            .ok_or(LocalEnumVariantValueTypeQueryError::MissingDefinitionPath(
                def_id,
            ))?;
        DefPathHash::compute(&path, &self.packages, &self.symbols)
            .map_err(LocalEnumVariantValueTypeQueryError::DefinitionPath)
    }

    fn local_enum_variant_value_source_fingerprint(
        &self,
        collection: &Phase1PackageScopesCollection,
        snapshot: &WorkspaceSnapshot,
        member_scopes: Option<&Phase1ActiveSourceMemberScopes>,
        wit_view_scopes: Option<&Phase1ActiveWitViewScopes>,
    ) -> Result<Fingerprint, LocalEnumVariantValueTypeQueryError> {
        let mut collections = BTreeMap::new();
        collections.insert(collection.package_key.clone(), collection);
        if let Some(member_scopes) = member_scopes {
            for package in member_scopes.source_packages() {
                collections.insert(package.package_key().clone(), &package.collection);
            }
        }

        let mut hasher = StableHasher::new();
        hasher.write_tag(0xE5);
        collection.package_key.stable_hash(&mut hasher);
        for (package_key, package_collection) in &collections {
            package_key.stable_hash(&mut hasher);
            hash_phase1_build_context(package_collection.build_context, &mut hasher);
            hash_target_cfg(&package_collection.target_cfg, &mut hasher);
            package_collection
                .package_graph_fingerprint
                .stable_hash(&mut hasher);
            package_collection
                .declared_requirements
                .as_deref()
                .stable_hash(&mut hasher);
            hash_module_tree_shape(self, &package_collection.module_tree, &mut hasher)?;

            let mut files = Vec::with_capacity(package_collection.module_tree.modules.len());
            for module in &package_collection.module_tree.modules {
                let file_key = self.file_key(module.file).cloned().ok_or(
                    LocalEnumVariantValueTypeQueryError::SourceFileKey(module.file),
                )?;
                let source = snapshot.file(module.file).ok_or_else(|| {
                    LocalEnumVariantValueTypeQueryError::MissingSourceFile {
                        package: package_key.clone(),
                        file: file_key.clone(),
                    }
                })?;
                files.push((file_key, Arc::clone(source.source())));
            }
            files.sort_by(|left, right| left.0.cmp(&right.0));
            hasher.write_len(u64::try_from(files.len()).unwrap_or(u64::MAX));
            for (file_key, source) in files {
                file_key.stable_hash(&mut hasher);
                source.raw_bytes().stable_hash(&mut hasher);
            }
        }
        sai_resolve::prelude::primitive_prelude_fingerprint().stable_hash(&mut hasher);
        wit_view_scopes
            .map(Phase1ActiveWitViewScopes::closure_fingerprint)
            .or(collection.wit_view_closure_fingerprint)
            .stable_hash(&mut hasher);
        Ok(hasher.finish())
    }

    #[cfg(test)]
    pub(super) fn local_enum_variant_value_type_query_output_for_tests(
        &mut self,
        resolutions: &TypeOwnerResolutions,
        collection: &Phase1PackageScopesCollection,
        snapshot: &WorkspaceSnapshot,
        member_scopes: Option<&Phase1ActiveSourceMemberScopes>,
        wit_view_scopes: Option<&Phase1ActiveWitViewScopes>,
        owner: OwnerId,
        variant: DefId,
    ) -> Result<Arc<LocalEnumVariantValueTypeQueryOutput>, LocalEnumVariantValueTypeQueryError>
    {
        validate_query_snapshot(resolutions, snapshot)?;
        let projection = resolutions.local_enum_variant_value_type_with_scopes(
            &self.semantic_types,
            collection,
            member_scopes,
            wit_view_scopes,
            owner,
            variant,
        )?;
        let key = LocalEnumVariantValueTypeQueryKey {
            owner: self.definition_path_hash(owner.def_id.to_def_id())?,
            variant: self.definition_path_hash(variant)?,
        };
        let identity = LocalEnumVariantValueTypeQueryIdentity {
            source_fingerprint: self.local_enum_variant_value_source_fingerprint(
                collection,
                snapshot,
                member_scopes,
                wit_view_scopes,
            )?,
        };
        Ok(self.local_enum_variant_value_types.get(
            &mut self.query_database,
            key,
            identity,
            projection,
        )?)
    }
}

fn validate_query_snapshot(
    resolutions: &TypeOwnerResolutions,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), LocalEnumVariantValueTypeQueryError> {
    if !resolutions.matches_snapshot(snapshot) {
        return Err(OwnerControlFlowResolutionError::SnapshotMismatch.into());
    }
    Ok(())
}

fn hash_phase1_build_context(context: Phase1BuildContext, hasher: &mut StableHasher) {
    hasher.write_tag(0xE6);
    context.is_root_package.stable_hash(hasher);
    context.is_root_core_module_build.stable_hash(hasher);
    context.is_test_build.stable_hash(hasher);
    context.analyzes_tests.stable_hash(hasher);
}

fn hash_target_cfg(target: &sai_hir::attrs::TargetCfg, hasher: &mut StableHasher) {
    hasher.write_tag(0xE7);
    target.target_name().stable_hash(hasher);
    target.pointer_width().stable_hash(hasher);
    target.capabilities().stable_hash(hasher);
}

fn hash_module_tree_shape(
    database: &RootDatabase,
    module_tree: &super::ModuleTree,
    hasher: &mut StableHasher,
) -> Result<(), LocalEnumVariantValueTypeQueryError> {
    let path_strings = |path: &[sai_intern::Symbol]| {
        path.iter()
            .map(|symbol| {
                database
                    .resolve_symbol(*symbol)
                    .map(|name| name.to_string())
                    .map_err(LocalEnumVariantValueTypeQueryError::SymbolLookup)
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let mut modules = module_tree
        .modules
        .iter()
        .map(|module| {
            Ok((
                path_strings(&module.path)?,
                module
                    .children
                    .iter()
                    .map(|(name, entry)| {
                        let kind = match entry {
                            sai_resolve::ChildEntry::Module(_) => 0u8,
                            sai_resolve::ChildEntry::Namespace(_) => 1u8,
                            sai_resolve::ChildEntry::Both(_, _) => 2u8,
                        };
                        (name.clone(), kind)
                    })
                    .collect::<Vec<_>>(),
            ))
        })
        .collect::<Result<Vec<_>, LocalEnumVariantValueTypeQueryError>>()?;
    modules.sort_by(|left, right| left.0.cmp(&right.0));
    hasher.write_tag(0xE8);
    modules.stable_hash(hasher);

    let mut namespaces = module_tree
        .namespaces
        .iter()
        .map(|namespace| {
            Ok((
                path_strings(&namespace.path)?,
                namespace
                    .children
                    .iter()
                    .map(|(name, entry)| {
                        let kind = match entry {
                            sai_resolve::ChildEntry::Module(_) => 0u8,
                            sai_resolve::ChildEntry::Namespace(_) => 1u8,
                            sai_resolve::ChildEntry::Both(_, _) => 2u8,
                        };
                        (name.clone(), kind)
                    })
                    .collect::<Vec<_>>(),
            ))
        })
        .collect::<Result<Vec<_>, LocalEnumVariantValueTypeQueryError>>()?;
    namespaces.sort_by(|left, right| left.0.cmp(&right.0));
    hasher.write_tag(0xE9);
    namespaces.stable_hash(hasher);
    Ok(())
}
