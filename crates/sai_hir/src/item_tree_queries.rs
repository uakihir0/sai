use crate::item_tree::{ItemTree, ItemTreeError, StableItemTree, StableItemTreeError};
use sai_intern::Interner;
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_syntax::{ParseQueryError, SyntaxQueries};
use sai_vfs::{FileId, FileInterner, FileKey, WorkspaceSnapshot};
use std::sync::Arc;

/// Memoizes snapshot-independent item-tree shapes by stable file identity.
#[derive(Clone)]
pub struct ItemTreeQueries {
    syntax: SyntaxQueries,
    stable_item_tree: DerivedQuery<FileKey, StableItemTree>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemTreeQueryError {
    Syntax(ParseQueryError),
    Query(QueryError),
    FileNotInterned,
    ItemTree(ItemTreeError),
    StableShape(StableItemTreeError),
    ShapeMismatch,
}

impl From<ParseQueryError> for ItemTreeQueryError {
    fn from(error: ParseQueryError) -> Self {
        Self::Syntax(error)
    }
}

impl From<QueryError> for ItemTreeQueryError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl ItemTreeQueries {
    pub fn new(syntax: SyntaxQueries) -> Result<Self, QueryError> {
        let query_syntax = syntax.clone();
        let stable_item_tree = DerivedQuery::<FileKey, StableItemTree>::new(
            "def_collector",
            move |database: &mut QueryDatabase, key| {
                let symbols = Interner::new();
                query_syntax.with_query_ast(database, key.clone(), |source, root, ast_ids| {
                    let tree =
                        ItemTree::collect(FileId::new(0), source.text(), root, ast_ids, &symbols)
                            .unwrap_or_else(|error| {
                                panic!("query-local item-tree collection failed: {error:?}")
                            });
                    assert!(
                        !tree.matches_ast_ids(ast_ids),
                        "query-local trees must not claim snapshot validity"
                    );
                    tree.stable_shape(&symbols).unwrap_or_else(|error| {
                        panic!("query-local item-tree symbol resolution failed: {error:?}")
                    })
                })
            },
        )?;
        Ok(Self {
            syntax,
            stable_item_tree,
        })
    }

    /// Returns the memoized item-tree shape for a stable VFS file key.
    ///
    /// The returned value contains no snapshot pointers or source ranges. A
    /// caller that needs source navigation must bind those from its own AST.
    pub fn stable_item_tree(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
    ) -> Result<Arc<StableItemTree>, QueryError> {
        self.stable_item_tree.get(database, key)
    }

    /// Returns the item tree rebound to the supplied workspace snapshot.
    ///
    /// The memoized query stores only its stable shape. This adapter resolves
    /// current symbols, AST pointers, and source ranges from the requesting
    /// snapshot, then verifies that the bound tree matches the cached shape.
    pub fn def_collector(
        &self,
        database: &mut QueryDatabase,
        files: &FileInterner,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
        symbols: &Interner,
    ) -> Result<Arc<ItemTree>, ItemTreeQueryError> {
        let key = files
            .file_key(file_id)
            .cloned()
            .ok_or(ItemTreeQueryError::FileNotInterned)?;
        let parsed = self
            .syntax
            .parse(database, files, snapshot, file_id)
            .map_err(ItemTreeQueryError::Syntax)?;
        let ast_ids = self
            .syntax
            .ast_id_map(database, files, snapshot, file_id)
            .map_err(ItemTreeQueryError::Syntax)?;
        let source = snapshot.file(file_id).ok_or(ItemTreeQueryError::Syntax(
            ParseQueryError::MissingSnapshotFile,
        ))?;
        let tree = ItemTree::collect(
            file_id,
            source.source().text(),
            &parsed.root,
            &ast_ids,
            symbols,
        )
        .map_err(ItemTreeQueryError::ItemTree)?;
        let stable_shape = self
            .stable_item_tree
            .get(database, key)
            .map_err(ItemTreeQueryError::Query)?;
        let bound_shape = tree
            .stable_shape(symbols)
            .map_err(ItemTreeQueryError::StableShape)?;
        if stable_shape.as_ref() != &bound_shape {
            return Err(ItemTreeQueryError::ShapeMismatch);
        }
        Ok(Arc::new(tree))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_intern::Interner;
    use sai_query::{Durability, QueryDatabase};
    use sai_stable_hash::StableHash;
    use sai_syntax::SyntaxQueries;
    use sai_vfs::{FileInterner, FileKey, SnapshotFile, SourceFile, VfsInputs, WorkspaceSnapshot};
    use std::sync::Arc;

    fn snapshot(file_id: FileId, bytes: &[u8], version: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(file_id, bytes).expect("source decodes")),
            "src/lib.sai",
            Some("sai:core@1.0.0".to_owned()),
            Some(version),
        )])
        .expect("workspace snapshot is valid")
    }

    #[test]
    fn stable_item_tree_query_tracks_edits_and_backdates_equal_shapes() {
        let key = FileKey::new("sai:core", "src/lib.sai").expect("stable file key");
        let original =
            b"//! module docs\nfn before() { 1 }\n@target(any(\"wasm32-core\"))\nfn run() {}\n";
        let vfs = VfsInputs::new().expect("VFS query IDs are available");
        let syntax = SyntaxQueries::new(vfs.clone()).expect("syntax query is available");
        let queries = ItemTreeQueries::new(syntax).expect("item-tree query is available");
        let mut database = QueryDatabase::new();
        vfs.set_file_contents(
            &mut database,
            key.clone(),
            Arc::from(original.as_slice()),
            Durability::Low,
        )
        .expect("original source is set");

        let first = queries
            .stable_item_tree(&mut database, key.clone())
            .expect("item-tree shape collects");
        let repeated = queries
            .stable_item_tree(&mut database, key.clone())
            .expect("item-tree shape is memoized");
        assert!(Arc::ptr_eq(&first, &repeated));

        let body_edit = b"//! module docs\nfn before() { 123456 }\n@target(any(\"wasm32-core\"))\nfn run() {}\n";
        vfs.set_file_contents(
            &mut database,
            key.clone(),
            Arc::from(body_edit.as_slice()),
            Durability::Low,
        )
        .expect("body-only edit is set");
        let body_only = queries
            .stable_item_tree(&mut database, key.clone())
            .expect("item-tree shape refreshes after the edit");
        assert!(Arc::ptr_eq(&first, &body_only));

        let renamed = b"//! module docs\nfn before() { 123456 }\n@target(any(\"wasm32-core\"))\nfn renamed() {}\n";
        vfs.set_file_contents(
            &mut database,
            key.clone(),
            Arc::from(renamed.as_slice()),
            Durability::Low,
        )
        .expect("declaration rename is set");
        let changed = queries
            .stable_item_tree(&mut database, key.clone())
            .expect("renamed item-tree shape collects");
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_ne!(first.fingerprint(), changed.fingerprint());

        let other_key = FileKey::new("sai:core", "src/other.sai").expect("other file key");
        vfs.set_file_contents(
            &mut database,
            other_key.clone(),
            Arc::from(renamed.as_slice()),
            Durability::Low,
        )
        .expect("same source is set under another file key");
        let other_file = queries
            .stable_item_tree(&mut database, other_key)
            .expect("other file shape collects");
        assert_eq!(*changed, *other_file);
        assert_eq!(changed.fingerprint(), other_file.fingerprint());
    }

    #[test]
    fn def_collector_rebinds_cached_shape_to_each_snapshot() {
        let key = FileKey::new("sai:core", "src/lib.sai").expect("stable file key");
        let mut files = FileInterner::new();
        let file_id = files.intern(key.clone()).expect("file ID is interned");
        let original =
            b"//! module docs\nfn before() { 1 }\n@target(any(\"wasm32-core\"))\nfn run() {}\n";
        let first_snapshot = snapshot(file_id, original, 1);
        let vfs = VfsInputs::new().expect("VFS query IDs are available");
        let syntax = SyntaxQueries::new(vfs.clone()).expect("syntax query is available");
        let queries = ItemTreeQueries::new(syntax.clone()).expect("item-tree query is available");
        let mut database = QueryDatabase::new();
        vfs.set_file_contents(
            &mut database,
            key.clone(),
            Arc::from(original.as_slice()),
            Durability::Low,
        )
        .expect("original source is set");
        let symbols = Interner::new();
        assert!(matches!(
            queries.def_collector(
                &mut database,
                &files,
                &first_snapshot,
                FileId::new(999),
                &symbols,
            ),
            Err(ItemTreeQueryError::FileNotInterned)
        ));

        let first_tree = queries
            .def_collector(&mut database, &files, &first_snapshot, file_id, &symbols)
            .expect("first snapshot item tree collects");
        let first_ast_ids = syntax
            .ast_id_map(&mut database, &files, &first_snapshot, file_id)
            .expect("first snapshot AST ID map is available");
        let first_parse = syntax
            .parse(&mut database, &files, &first_snapshot, file_id)
            .expect("first snapshot parse is available");
        assert!(first_tree.matches_ast_ids(&first_ast_ids));
        assert!(
            first_tree
                .module_docs
                .as_ref()
                .expect("module docs pointer exists")
                .resolve(file_id, &first_parse.root)
                .is_some()
        );

        let edited = b"//! module docs\nfn before() { 123456 }\n@target(any(\"wasm32-core\"))\nfn run() {}\n";
        let second_snapshot = snapshot(file_id, edited, 2);
        vfs.set_file_contents(
            &mut database,
            key,
            Arc::from(edited.as_slice()),
            Durability::Low,
        )
        .expect("body-only edit is set");
        let second_tree = queries
            .def_collector(&mut database, &files, &second_snapshot, file_id, &symbols)
            .expect("second snapshot item tree collects");
        let second_ast_ids = syntax
            .ast_id_map(&mut database, &files, &second_snapshot, file_id)
            .expect("second snapshot AST ID map is available");
        let second_parse = syntax
            .parse(&mut database, &files, &second_snapshot, file_id)
            .expect("second snapshot parse is available");

        assert_eq!(*first_tree, *second_tree);
        assert!(second_tree.matches_ast_ids(&second_ast_ids));
        assert!(!first_tree.matches_ast_ids(&second_ast_ids));
        assert_ne!(
            first_tree.items[1].raw_attrs[0].span,
            second_tree.items[1].raw_attrs[0].span
        );
        assert_ne!(
            first_tree.items[1].raw_attrs[0].ptr,
            second_tree.items[1].raw_attrs[0].ptr
        );
        assert!(
            second_tree.items[1].raw_attrs[0]
                .ptr
                .resolve(file_id, &second_parse.root)
                .is_some()
        );
    }
}
