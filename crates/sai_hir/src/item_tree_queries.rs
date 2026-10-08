use crate::item_tree::{ItemTree, StableItemTree};
use sai_intern::Interner;
use sai_query::{DerivedQuery, QueryDatabase, QueryError};
use sai_syntax::SyntaxQueries;
use sai_vfs::{FileId, FileKey};
use std::sync::Arc;

/// Memoizes snapshot-independent item-tree shapes by stable file identity.
#[derive(Clone)]
pub struct ItemTreeQueries {
    stable_item_tree: DerivedQuery<FileKey, StableItemTree>,
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
        Ok(Self { stable_item_tree })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_query::{Durability, QueryDatabase};
    use sai_stable_hash::StableHash;
    use sai_syntax::SyntaxQueries;
    use sai_vfs::{FileKey, VfsInputs};

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
}
