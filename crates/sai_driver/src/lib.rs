#![forbid(unsafe_code)]

use sai_hir::item_tree::ItemTree;
use sai_hir::{ItemTreeQueries, ItemTreeQueryError};
use sai_intern::{Interner, LookupError, PackageId, PackageIdError, PackageInterner, Symbol};
use sai_query::{Durability, QueryDatabase, QueryError};
use sai_syntax::{Parse, ParseQueryError, SyntaxQueries, ast};
use sai_vfs::{
    FileContents, FileId, FileInterner, FileInternerError, FileKey, LineIndex, PackageFileKind,
    PackageFilesSnapshot, PackageKey, PackageKeyError, PackageRelativePath, PackageSourceDirectory,
    PackageSourcePath, SourceText, VfsInputSnapshot, VfsInputUpdate, VfsInputs, WitFilesSnapshot,
    WorkspaceSnapshot,
};
use std::sync::Arc;

/// Owns the query runtime and the session state for the implemented frontend.
///
/// The driver applies VFS inputs before requesting tracked syntax and
/// item-tree queries. Snapshot-bound results are always rebound to the
/// [`WorkspaceSnapshot`] passed by the caller.
pub struct RootDatabase {
    query_database: QueryDatabase,
    vfs: VfsInputs,
    syntax: SyntaxQueries,
    hir: ItemTreeQueries,
    packages: PackageInterner,
    files: FileInterner,
    symbols: Interner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootDatabaseError {
    Query(QueryError),
    Package(PackageIdError),
    PackageKey(PackageKeyError),
    FileInterner(FileInternerError),
    FileNotInterned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InternedWitFiles {
    NotApplicable,
    Listed(Vec<FileId>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternedPackageFiles {
    source_directory_exists: bool,
    entries: Vec<InternedPackageFileEntry>,
}

impl InternedPackageFiles {
    pub const fn source_directory_exists(&self) -> bool {
        self.source_directory_exists
    }

    pub fn entries(&self) -> &[InternedPackageFileEntry] {
        &self.entries
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InternedPackageFileEntry {
    File {
        path: PackageSourcePath,
        file_id: FileId,
    },
    Directory {
        path: PackageSourcePath,
    },
    Symlink {
        path: PackageSourcePath,
    },
}

impl InternedPackageFileEntry {
    pub fn path(&self) -> &PackageSourcePath {
        match self {
            Self::File { path, .. } | Self::Directory { path } | Self::Symlink { path } => path,
        }
    }

    pub const fn kind(&self) -> PackageFileKind {
        match self {
            Self::File { .. } => PackageFileKind::File,
            Self::Directory { .. } => PackageFileKind::Directory,
            Self::Symlink { .. } => PackageFileKind::Symlink,
        }
    }

    pub const fn file_id(&self) -> Option<FileId> {
        match self {
            Self::File { file_id, .. } => Some(*file_id),
            Self::Directory { .. } | Self::Symlink { .. } => None,
        }
    }
}

impl From<QueryError> for RootDatabaseError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<PackageIdError> for RootDatabaseError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<PackageKeyError> for RootDatabaseError {
    fn from(error: PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<FileInternerError> for RootDatabaseError {
    fn from(error: FileInternerError) -> Self {
        Self::FileInterner(error)
    }
}

impl RootDatabase {
    /// Creates a fresh compiler session with empty VFS inputs.
    pub fn new() -> Result<Self, QueryError> {
        let vfs = VfsInputs::new()?;
        let syntax = SyntaxQueries::new(vfs.clone())?;
        let hir = ItemTreeQueries::new(syntax.clone())?;
        Ok(Self {
            query_database: QueryDatabase::new(),
            vfs,
            syntax,
            hir,
            packages: PackageInterner::new(),
            files: FileInterner::new(),
            symbols: Interner::new(),
        })
    }

    /// Interns a stable package key for this session.
    pub fn intern_package(&self, package_key: &str) -> Result<PackageId, PackageIdError> {
        self.packages.intern(package_key)
    }

    /// Interns a stable file key and returns its session-local file ID.
    pub fn intern_file_key(&mut self, key: FileKey) -> Result<FileId, FileInternerError> {
        self.files.intern(key)
    }

    /// Interns a source file beneath the package's configured source directory.
    pub fn intern_source_file(
        &mut self,
        package_id: PackageId,
        source_directory: PackageSourceDirectory,
        source_relative_path: PackageSourcePath,
    ) -> Result<FileId, FileInternerError> {
        self.files.intern_source_file(
            &self.packages,
            package_id,
            source_directory,
            source_relative_path,
        )
    }

    /// Interns a package-relative metadata or WIT file.
    pub fn intern_package_file(
        &mut self,
        package_id: PackageId,
        package_relative_path: PackageRelativePath,
    ) -> Result<FileId, FileInternerError> {
        self.files
            .intern_package_file(&self.packages, package_id, package_relative_path)
    }

    /// Returns the stable file identity assigned to a session-local ID.
    pub fn file_key(&self, file_id: FileId) -> Option<&FileKey> {
        self.files.file_key(file_id)
    }

    /// Resolves a session-local symbol returned by frontend query results.
    pub fn resolve_symbol(&self, symbol: Symbol) -> Result<Arc<str>, LookupError> {
        self.symbols.resolve(symbol)
    }

    /// Sets raw file bytes as a tracked VFS input.
    pub fn set_file_contents(
        &mut self,
        key: FileKey,
        contents: Arc<[u8]>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_file_contents(&mut self.query_database, key, contents, durability)
    }

    /// Records a file read failure as a tracked VFS input.
    pub fn set_file_read_error(
        &mut self,
        key: FileKey,
        io_error_kind: impl Into<String>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_file_read_error(&mut self.query_database, key, io_error_kind, durability)
    }

    /// Removes a file input when the file leaves the source snapshot.
    pub fn remove_file(&mut self, key: FileKey) -> Result<bool, QueryError> {
        self.vfs.remove_file(&mut self.query_database, key)
    }

    /// Sets the tracked source-file inventory for one package.
    pub fn set_package_files(
        &mut self,
        key: PackageKey,
        files: PackageFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_package_files(&mut self.query_database, key, files, durability)
    }

    /// Removes the tracked source-file inventory for one package.
    pub fn remove_package_files(&mut self, key: PackageKey) -> Result<bool, QueryError> {
        self.vfs.remove_package_files(&mut self.query_database, key)
    }

    /// Sets the tracked WIT-file inventory for one package.
    pub fn set_wit_files(
        &mut self,
        key: PackageKey,
        files: WitFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.vfs
            .set_wit_files(&mut self.query_database, key, files, durability)
    }

    /// Removes the tracked WIT-file inventory for one package.
    pub fn remove_wit_files(&mut self, key: PackageKey) -> Result<bool, QueryError> {
        self.vfs.remove_wit_files(&mut self.query_database, key)
    }

    /// Applies file and package-inventory updates in a single query revision.
    pub fn apply_vfs_updates(
        &mut self,
        updates: impl IntoIterator<Item = VfsInputUpdate>,
    ) -> Result<usize, QueryError> {
        self.vfs.apply_updates(&mut self.query_database, updates)
    }

    /// Reconciles a complete VFS snapshot, removing values omitted from it.
    pub fn replace_vfs_snapshot(
        &mut self,
        previous: Option<&VfsInputSnapshot>,
        current: &VfsInputSnapshot,
    ) -> Result<usize, QueryError> {
        self.vfs
            .replace_snapshot(&mut self.query_database, previous, current)
    }

    /// Reads tracked file contents, including tracked absence.
    pub fn file_contents(&mut self, key: FileKey) -> Result<Option<FileContents>, QueryError> {
        self.vfs.file_contents(&mut self.query_database, key)
    }

    /// Reads decoded source and binds it to an ID interned by this database.
    pub fn source_text(&mut self, file_id: FileId) -> Result<SourceText, RootDatabaseError> {
        let key = self
            .files
            .file_key(file_id)
            .cloned()
            .ok_or(RootDatabaseError::FileNotInterned)?;
        self.vfs
            .source_text(&mut self.query_database, key, file_id)
            .map_err(RootDatabaseError::from)
    }

    /// Reads the memoized line index for a stable file identity.
    pub fn line_index(&mut self, key: FileKey) -> Result<Arc<LineIndex>, QueryError> {
        self.vfs.line_index(&mut self.query_database, key)
    }

    /// Reads a package's source inventory and interns file entries.
    ///
    /// `None` means no inventory input exists. A returned value preserves
    /// whether the configured source directory exists; directories and
    /// symlinks remain listed but receive no `FileId`.
    pub fn intern_package_files(
        &mut self,
        package_id: PackageId,
        source_directory: PackageSourceDirectory,
    ) -> Result<Option<InternedPackageFiles>, RootDatabaseError> {
        let package_name = self.packages.resolve(package_id)?;
        let package_key = PackageKey::new(package_name.as_ref())?;
        let Some(snapshot) = self
            .vfs
            .package_files(&mut self.query_database, package_key)?
        else {
            return Ok(None);
        };
        let source_paths = snapshot
            .entries()
            .iter()
            .filter(|entry| entry.kind() == PackageFileKind::File)
            .map(|entry| entry.path().clone())
            .collect::<Vec<_>>();
        let file_ids = self.files.intern_source_files(
            &self.packages,
            package_id,
            source_directory,
            source_paths,
        )?;
        let mut file_ids = file_ids.into_iter();
        let entries = snapshot
            .entries()
            .iter()
            .map(|entry| match entry.kind() {
                PackageFileKind::File => InternedPackageFileEntry::File {
                    path: entry.path().clone(),
                    file_id: file_ids
                        .next()
                        .expect("every inventoried source file has an interned file ID"),
                },
                PackageFileKind::Directory => InternedPackageFileEntry::Directory {
                    path: entry.path().clone(),
                },
                PackageFileKind::Symlink => InternedPackageFileEntry::Symlink {
                    path: entry.path().clone(),
                },
            })
            .collect();
        debug_assert!(file_ids.next().is_none());
        Ok(Some(InternedPackageFiles {
            source_directory_exists: snapshot.source_directory_exists(),
            entries,
        }))
    }

    /// Reads WIT paths and interns them as session-local file IDs.
    ///
    /// `None` means no WIT inventory input exists. `NotApplicable` preserves
    /// an explicitly recorded package without a usable WIT directory, while
    /// `Listed([])` preserves an existing directory with no direct WIT files.
    pub fn intern_wit_files(
        &mut self,
        package_id: PackageId,
    ) -> Result<Option<InternedWitFiles>, RootDatabaseError> {
        let package_name = self.packages.resolve(package_id)?;
        let package_key = PackageKey::new(package_name.as_ref())?;
        let Some(snapshot) = self.vfs.wit_files(&mut self.query_database, package_key)? else {
            return Ok(None);
        };
        match snapshot {
            WitFilesSnapshot::NotApplicable => Ok(Some(InternedWitFiles::NotApplicable)),
            WitFilesSnapshot::Listed { files, .. } => {
                let file_ids = self.files.intern_package_files(
                    &self.packages,
                    package_id,
                    files.iter().cloned(),
                )?;
                Ok(Some(InternedWitFiles::Listed(file_ids)))
            }
        }
    }

    /// Parses a file and binds syntax diagnostics to the supplied snapshot.
    pub fn parse_file(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<Parse, ParseQueryError> {
        self.syntax
            .parse(&mut self.query_database, &self.files, snapshot, file_id)
    }

    /// Binds the memoized AST-ID shape to the supplied snapshot.
    pub fn ast_id_map(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<ast::AstIdMap, ParseQueryError> {
        self.syntax
            .ast_id_map(&mut self.query_database, &self.files, snapshot, file_id)
    }

    /// Collects and snapshot-binds the item's declarations for one file.
    pub fn def_collector(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<Arc<ItemTree>, ItemTreeQueryError> {
        self.hir.def_collector(
            &mut self.query_database,
            &self.files,
            snapshot,
            file_id,
            &self.symbols,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};

    fn snapshot(file_id: FileId, contents: &[u8], version: u64) -> WorkspaceSnapshot {
        WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(file_id, contents).expect("source decodes")),
            "src/lib.sai",
            Some("acme:app@0.1.0".to_owned()),
            Some(version),
        )])
        .expect("workspace snapshot is valid")
    }

    #[test]
    fn root_database_updates_inputs_and_rebinds_syntax_and_item_tree() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let key = FileKey::new("acme:app#0.1", "src/lib.sai").expect("file key is valid");
        let file_id = database
            .intern_file_key(key.clone())
            .expect("file key is interned");
        let first_contents: Arc<[u8]> = Arc::from(&b"fn alpha() {}\n"[..]);
        assert!(
            database
                .set_file_contents(key.clone(), Arc::clone(&first_contents), Durability::Low)
                .expect("initial source is set")
        );
        let first_snapshot = snapshot(file_id, &first_contents, 1);

        let first_parse = database
            .parse_file(&first_snapshot, file_id)
            .expect("first parse binds to the snapshot");
        let first_tree = database
            .def_collector(&first_snapshot, file_id)
            .expect("first item tree binds to the snapshot");
        assert_eq!(first_tree.items.len(), 1);
        assert_eq!(
            database
                .resolve_symbol(first_tree.items[0].name)
                .expect("item name belongs to the database")
                .as_ref(),
            "alpha"
        );
        let other_database = RootDatabase::new().expect("another session initializes");
        assert_eq!(
            other_database.resolve_symbol(first_tree.items[0].name),
            Err(LookupError::UnknownSymbol)
        );
        let first_ast_ids = database
            .ast_id_map(&first_snapshot, file_id)
            .expect("first AST IDs bind to the snapshot");
        assert!(first_tree.matches_ast_ids(&first_ast_ids));
        assert_eq!(
            database
                .source_text(file_id)
                .expect("interned file has decoded source")
                .source()
                .text(),
            "fn alpha() {}\n"
        );
        assert_eq!(
            database.source_text(FileId::new(99)),
            Err(RootDatabaseError::FileNotInterned)
        );

        let second_contents: Arc<[u8]> = Arc::from(&b"fn beta() {}\n"[..]);
        assert!(
            database
                .set_file_contents(key, Arc::clone(&second_contents), Durability::Low)
                .expect("updated source is set")
        );
        let second_snapshot = snapshot(file_id, &second_contents, 2);
        let second_parse = database
            .parse_file(&second_snapshot, file_id)
            .expect("updated parse binds to the snapshot");
        let second_tree = database
            .def_collector(&second_snapshot, file_id)
            .expect("updated item tree binds to the snapshot");
        let second_ast_ids = database
            .ast_id_map(&second_snapshot, file_id)
            .expect("updated AST IDs bind to the snapshot");

        assert_ne!(first_parse.root.green(), second_parse.root.green());
        assert_eq!(second_tree.items.len(), 1);
        assert_ne!(first_tree.items[0].name, second_tree.items[0].name);
        assert_eq!(
            database
                .resolve_symbol(second_tree.items[0].name)
                .expect("updated item name belongs to the database")
                .as_ref(),
            "beta"
        );
        assert!(second_tree.matches_ast_ids(&second_ast_ids));
    }

    #[test]
    fn root_database_interns_wit_paths_and_preserves_listing_states() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let package_id = database
            .intern_package("acme:app#0.1")
            .expect("package is interned");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");

        assert_eq!(database.intern_wit_files(package_id), Ok(None));

        let foreign_packages = PackageInterner::new();
        let foreign_package_id = foreign_packages
            .intern("acme:app#0.1")
            .expect("foreign package is interned");
        assert_eq!(
            database.intern_wit_files(foreign_package_id),
            Err(RootDatabaseError::Package(PackageIdError::UnknownPackage))
        );

        assert!(
            database
                .set_wit_files(
                    package_key.clone(),
                    WitFilesSnapshot::not_applicable(),
                    Durability::Low,
                )
                .expect("not-applicable inventory is set")
        );
        assert_eq!(
            database.intern_wit_files(package_id),
            Ok(Some(InternedWitFiles::NotApplicable))
        );

        assert!(
            database
                .set_wit_files(
                    package_key.clone(),
                    WitFilesSnapshot::listed("wit", []).expect("empty WIT inventory is valid"),
                    Durability::Low,
                )
                .expect("empty inventory is set")
        );
        assert_eq!(
            database.intern_wit_files(package_id),
            Ok(Some(InternedWitFiles::Listed(Vec::new())))
        );

        assert!(
            database
                .set_wit_files(
                    package_key,
                    WitFilesSnapshot::listed(
                        "wit",
                        [PackageRelativePath::new("wit/app.wit").expect("WIT path is valid")],
                    )
                    .expect("WIT inventory is valid"),
                    Durability::Low,
                )
                .expect("WIT file inventory is set")
        );
        let Some(InternedWitFiles::Listed(file_ids)) = database
            .intern_wit_files(package_id)
            .expect("WIT paths intern")
        else {
            panic!("one WIT file should be listed");
        };
        assert_eq!(file_ids.len(), 1);
        assert_eq!(
            database
                .file_key(file_ids[0])
                .expect("WIT file ID resolves")
                .path(),
            "wit/app.wit"
        );
        assert_eq!(
            database
                .file_key(file_ids[0])
                .expect("WIT file ID resolves")
                .package_key(),
            "acme:app#0.1"
        );
    }

    #[test]
    fn root_database_interns_package_source_files_and_preserves_inventory_entries() {
        let mut database = RootDatabase::new().expect("root database initializes");
        let package_id = database
            .intern_package("acme:app#0.1")
            .expect("package is interned");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");
        let source_directory =
            PackageSourceDirectory::new("src").expect("source directory is valid");

        assert_eq!(
            database.intern_package_files(package_id, source_directory.clone()),
            Ok(None)
        );

        let foreign_packages = PackageInterner::new();
        let foreign_package_id = foreign_packages
            .intern("acme:app#0.1")
            .expect("foreign package is interned");
        assert_eq!(
            database.intern_package_files(foreign_package_id, source_directory.clone()),
            Err(RootDatabaseError::Package(PackageIdError::UnknownPackage))
        );

        assert!(
            database
                .set_package_files(
                    package_key.clone(),
                    PackageFilesSnapshot::new(false, []).expect("missing source dir is valid"),
                    Durability::Low,
                )
                .expect("missing source inventory is set")
        );
        let absent = database
            .intern_package_files(package_id, source_directory.clone())
            .expect("missing source inventory is read")
            .expect("inventory input exists");
        assert!(!absent.source_directory_exists());
        assert!(absent.entries().is_empty());

        assert!(
            database
                .set_package_files(
                    package_key.clone(),
                    PackageFilesSnapshot::new(true, []).expect("empty source dir is valid"),
                    Durability::Low,
                )
                .expect("empty source inventory is set")
        );
        let empty = database
            .intern_package_files(package_id, source_directory.clone())
            .expect("empty source inventory is read")
            .expect("inventory input exists");
        assert!(empty.source_directory_exists());
        assert!(empty.entries().is_empty());

        assert!(
            database
                .set_package_files(
                    package_key,
                    PackageFilesSnapshot::new(
                        true,
                        [
                            sai_vfs::PackageFileEntry::new(
                                "nested",
                                PackageFileKind::Directory,
                            )
                            .expect("directory entry is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "nested/lib.sai",
                                PackageFileKind::File,
                            )
                            .expect("source file entry is valid"),
                            sai_vfs::PackageFileEntry::new(
                                "link",
                                PackageFileKind::Symlink,
                            )
                            .expect("symlink entry is valid"),
                        ],
                    )
                    .expect("source inventory is valid"),
                    Durability::Low,
                )
                .expect("source inventory is set")
        );
        let listed = database
            .intern_package_files(package_id, source_directory)
            .expect("source inventory is read")
            .expect("inventory input exists");
        assert!(listed.source_directory_exists());
        assert_eq!(listed.entries().len(), 3);
        assert_eq!(listed.entries()[0].path().as_str(), "link");
        assert_eq!(listed.entries()[0].kind(), PackageFileKind::Symlink);
        assert_eq!(listed.entries()[0].file_id(), None);
        assert_eq!(listed.entries()[1].path().as_str(), "nested");
        assert_eq!(listed.entries()[1].kind(), PackageFileKind::Directory);
        assert_eq!(listed.entries()[1].file_id(), None);
        let source_file = &listed.entries()[2];
        assert_eq!(source_file.path().as_str(), "nested/lib.sai");
        assert_eq!(source_file.kind(), PackageFileKind::File);
        let file_id = source_file.file_id().expect("source files have IDs");
        let stable_key = database
            .file_key(file_id)
            .expect("source file ID resolves in this session");
        assert_eq!(stable_key.package_key(), "acme:app#0.1");
        assert_eq!(stable_key.path(), "src/nested/lib.sai");
    }
}
