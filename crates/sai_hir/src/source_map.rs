use crate::{
    BindingId, DefPathInterner, FileId, HirId, IndexVec, IndexVecError, ItemLocalId, LocalDefId,
    OwnerId, Span, SpanCtxt,
};
use sai_diagnostics::{
    Code, Diagnostic, DiagnosticTemplate, Location, Severity, SourceRef, Span as DiagnosticSpan,
};
use sai_syntax::{SyntaxNode, SyntaxNodePtr, SyntaxNodePtrError, ast::AstIdMap};
use sai_vfs::WorkspaceSnapshot;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_HIR_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

/// Runtime identity shared by one lowered owner's bodies and source map.
///
/// This identity is deliberately excluded from semantic equality and hashing:
/// it validates that borrowed HIR data is paired with the source map produced
/// by the same lowering pass, but does not affect incremental HIR semantics.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct HirSourceId(u64);

impl HirSourceId {
    pub(crate) fn fresh() -> Self {
        let id = NEXT_HIR_SOURCE_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("HIR source identity space exhausted");
        Self(id)
    }
}

/// Source relationships for one HIR owner.
#[derive(Clone, Debug)]
pub struct HirSourceMap {
    owner: OwnerId,
    source_id: HirSourceId,
    file: FileId,
    source_ref: SourceRef,
    source_bytes: Arc<[u8]>,
    node_to_syntax: IndexVec<ItemLocalId, SyntaxNodePtr>,
    syntax_to_nodes: HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
    binding_to_syntax: IndexVec<BindingId, SyntaxNodePtr>,
    diagnostics: Vec<SourceMapDiagnostic>,
}

impl HirSourceMap {
    pub const fn source_id(&self) -> HirSourceId {
        self.source_id
    }

    pub const fn owner(&self) -> OwnerId {
        self.owner
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub fn source_ref(&self) -> &SourceRef {
        &self.source_ref
    }

    pub fn matches_snapshot_file(&self, snapshot: &WorkspaceSnapshot) -> bool {
        snapshot.file(self.file).is_some_and(|file| {
            file.matches_reference(
                &self.source_ref.path,
                self.source_ref.package.as_deref(),
                self.source_ref.snapshot,
            ) && file.source().id() == self.file
                && file.source().raw_bytes() == self.source_bytes.as_ref()
        })
    }

    pub fn matches_body(&self, body: &crate::body::Body<'_>) -> bool {
        self.owner == body.owner && self.source_id == body.source_id
    }

    /// Rebuilds a stored syntax pointer against the current red root when the
    /// source file is unchanged.
    pub fn rebind_syntax_pointer(
        &self,
        pointer: &SyntaxNodePtr,
        ast_ids: &AstIdMap,
        snapshot: &WorkspaceSnapshot,
        root: &SyntaxNode,
    ) -> Result<Option<SyntaxNodePtr>, HirSourceMapError> {
        if !self.matches_snapshot_file(snapshot) {
            return Err(HirSourceMapError::SourceTextNotValidForSnapshot);
        }
        if !ast_ids.matches_snapshot(snapshot) {
            return Err(HirSourceMapError::WorkspaceSnapshotMismatch);
        }
        if !ast_ids.matches_root(self.file, root) {
            return Err(HirSourceMapError::AstIdMapMismatch);
        }
        if pointer.file() != self.file {
            return Err(HirSourceMapError::SyntaxFileMismatch);
        }
        if !self
            .node_to_syntax
            .iter_enumerated()
            .map(|(_, stored)| stored)
            .chain(
                self.binding_to_syntax
                    .iter_enumerated()
                    .map(|(_, stored)| stored),
            )
            .chain(self.syntax_to_nodes.keys())
            .any(|stored| stored == pointer)
        {
            return Err(HirSourceMapError::SyntaxPointerNotInSourceMap);
        }
        Ok(pointer.rebind(self.file, root))
    }

    pub fn node_to_syntax(&self) -> &IndexVec<ItemLocalId, SyntaxNodePtr> {
        &self.node_to_syntax
    }

    pub fn syntax_to_nodes(&self) -> &HashMap<SyntaxNodePtr, Vec<ItemLocalId>> {
        &self.syntax_to_nodes
    }

    pub fn binding_to_syntax(&self) -> &IndexVec<BindingId, SyntaxNodePtr> {
        &self.binding_to_syntax
    }

    /// Binds source-local diagnostics to the snapshot that consumes this map.
    ///
    /// Snapshot-independent diagnostics can be rebound when the owner's file
    /// is unchanged, even if another workspace file changed. Diagnostics that
    /// carry broader contextual state retain their original snapshot binding
    /// and cannot be reused across snapshots.
    pub fn diagnostics(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<Diagnostic>, HirSourceMapError> {
        if !self.matches_snapshot_file(snapshot) {
            return Err(HirSourceMapError::SourceTextNotValidForSnapshot);
        }
        self.diagnostics
            .iter()
            .map(|diagnostic| {
                let diagnostic = match diagnostic {
                    SourceMapDiagnostic::Template(template) => template
                        .bind(self.file, self.source_ref.clone())
                        .with_workspace_snapshot(snapshot),
                    SourceMapDiagnostic::SnapshotBound(diagnostic) => {
                        diagnostic.clone().with_workspace_snapshot(snapshot)
                    }
                };
                diagnostic.map_err(|_| HirSourceMapError::DiagnosticNotValidForSnapshot)
            })
            .collect()
    }

    pub fn syntax_for_node(&self, id: HirId) -> Option<&SyntaxNodePtr> {
        (id.owner == self.owner)
            .then(|| self.node_to_syntax.get(id.local_id))
            .flatten()
    }

    pub fn nodes_for_syntax(&self, pointer: &SyntaxNodePtr) -> &[ItemLocalId] {
        self.syntax_to_nodes
            .get(pointer)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn syntax_for_binding(&self, id: BindingId) -> Option<&SyntaxNodePtr> {
        self.binding_to_syntax.get(id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceMapDiagnostic {
    Template(DiagnosticTemplate),
    SnapshotBound(Diagnostic),
}

fn same_pointer_index_vec<I: sai_intern::Idx>(
    left: &IndexVec<I, SyntaxNodePtr>,
    right: &IndexVec<I, SyntaxNodePtr>,
) -> bool {
    left.len() == right.len()
        && left.iter_enumerated().zip(right.iter_enumerated()).all(
            |((left_id, left), (right_id, right))| {
                left_id == right_id && left.is_same_source_node(right)
            },
        )
}

fn same_syntax_to_nodes(
    left: &HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
    right: &HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(left_pointer, left_nodes)| {
            right.iter().any(|(right_pointer, right_nodes)| {
                left_pointer.is_same_source_node(right_pointer) && left_nodes == right_nodes
            })
        })
}

// Source maps represent file-local syntax relationships, so equality ignores
// red-root identities only after owner-file provenance and bytes match. A
// caller that needs current syntax nodes must use `rebind_syntax_pointer`.
impl PartialEq for HirSourceMap {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.file == other.file
            && self.source_ref == other.source_ref
            && self.source_bytes == other.source_bytes
            && same_pointer_index_vec(&self.node_to_syntax, &other.node_to_syntax)
            && same_syntax_to_nodes(&self.syntax_to_nodes, &other.syntax_to_nodes)
            && same_pointer_index_vec(&self.binding_to_syntax, &other.binding_to_syntax)
            && self.diagnostics == other.diagnostics
    }
}

impl Eq for HirSourceMap {}

/// Builds a source map while checking every syntax node against one parsed
/// file root and snapshot.
pub struct HirSourceMapBuilder<'root> {
    owner: OwnerId,
    source_id: HirSourceId,
    snapshot: sai_vfs::WorkspaceSnapshot,
    file: crate::FileId,
    source_ref: SourceRef,
    source_bytes: Arc<[u8]>,
    root: &'root SyntaxNode,
    owner_syntax: SyntaxNode,
    def_path_interner: Option<&'root DefPathInterner>,
    nested_definition_ids: HashMap<SyntaxNodePtr, LocalDefId>,
    node_to_syntax: IndexVec<ItemLocalId, SyntaxNodePtr>,
    syntax_to_nodes: HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
    binding_to_syntax: IndexVec<BindingId, SyntaxNodePtr>,
    binding_ids_by_syntax: HashMap<SyntaxNodePtr, BindingId>,
    diagnostics: Vec<SourceMapDiagnostic>,
}

impl<'root> HirSourceMapBuilder<'root> {
    pub fn new(
        owner: OwnerId,
        ast_ids: &AstIdMap,
        snapshot: &sai_vfs::WorkspaceSnapshot,
        root: &'root SyntaxNode,
        owner_syntax: &SyntaxNode,
    ) -> Result<Self, HirSourceMapError> {
        let file = ast_ids.file();
        if !ast_ids.matches_snapshot(snapshot) {
            return Err(HirSourceMapError::WorkspaceSnapshotMismatch);
        }
        if !ast_ids.matches_root(file, root) {
            return Err(HirSourceMapError::AstIdMapMismatch);
        }
        let snapshot_file = snapshot
            .file(file)
            .ok_or(HirSourceMapError::FileMissingFromSnapshot)?;
        let source_ref = SourceRef {
            path: snapshot_file.path().to_owned(),
            package: snapshot_file.package().map(str::to_owned),
            snapshot: snapshot_file.document_version(),
        };
        let source_bytes = snapshot_file.source().raw_bytes_arc();

        let owner_pointer = syntax_pointer(file, root, owner_syntax)?;
        let mut builder = Self {
            owner,
            source_id: HirSourceId::fresh(),
            snapshot: snapshot.clone(),
            file,
            source_ref,
            source_bytes,
            root,
            owner_syntax: owner_syntax.clone(),
            def_path_interner: None,
            nested_definition_ids: HashMap::new(),
            node_to_syntax: IndexVec::new(),
            syntax_to_nodes: HashMap::new(),
            binding_to_syntax: IndexVec::new(),
            binding_ids_by_syntax: HashMap::new(),
            diagnostics: Vec::new(),
        };
        let owner_local_id = builder.push_node_pointer(owner_pointer)?;
        debug_assert_eq!(owner_local_id, ItemLocalId::default());
        Ok(builder)
    }

    /// Enable lowering of lexical closure and async-block definitions.
    pub fn with_def_path_interner(mut self, interner: &'root DefPathInterner) -> Self {
        self.def_path_interner = Some(interner);
        self
    }

    pub(crate) const fn owner(&self) -> OwnerId {
        self.owner
    }

    pub const fn source_id(&self) -> HirSourceId {
        self.source_id
    }

    pub(crate) fn owner_syntax(&self) -> &SyntaxNode {
        &self.owner_syntax
    }

    pub(crate) fn def_path_interner(&self) -> Option<&'root DefPathInterner> {
        self.def_path_interner
    }

    pub(crate) fn pointer_for(
        &self,
        syntax: &SyntaxNode,
    ) -> Result<SyntaxNodePtr, HirSourceMapError> {
        syntax_pointer(self.file, self.root, syntax)
    }

    pub(crate) fn record_nested_definition(
        &mut self,
        syntax: &SyntaxNode,
        id: LocalDefId,
    ) -> Result<(), HirSourceMapError> {
        let pointer = self.pointer_for(syntax)?;
        match self.nested_definition_ids.entry(pointer) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(id);
            }
            std::collections::hash_map::Entry::Occupied(entry) if *entry.get() == id => {}
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(HirSourceMapError::NestedDefinitionIdMismatch);
            }
        }
        Ok(())
    }

    pub(crate) fn nested_definition_for_syntax(
        &self,
        syntax: &SyntaxNode,
    ) -> Result<Option<LocalDefId>, HirSourceMapError> {
        let pointer = self.pointer_for(syntax)?;
        Ok(self.nested_definition_ids.get(&pointer).copied())
    }

    /// The HIR node for the owner itself always has local ID zero.
    pub fn owner_hir_id(&self) -> HirId {
        HirId {
            owner: self.owner,
            local_id: ItemLocalId::default(),
        }
    }

    pub fn add_node(&mut self, syntax: &SyntaxNode) -> Result<HirId, HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        let local_id = self.push_node_pointer(pointer)?;
        Ok(HirId {
            owner: self.owner,
            local_id,
        })
    }

    /// Record a HIR node and return its span after validating the syntax node
    /// against this builder's file root.
    pub fn add_node_with_span(
        &mut self,
        syntax: &SyntaxNode,
        ctxt: SpanCtxt,
    ) -> Result<(HirId, Span), HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        let span = Span {
            file: self.file,
            range: syntax.text_range(),
            ctxt,
        };
        let local_id = self.push_node_pointer(pointer)?;
        Ok((
            HirId {
                owner: self.owner,
                local_id,
            },
            span,
        ))
    }

    /// Return a span for syntax proven to belong to this builder's file root.
    pub fn span_for(&self, syntax: &SyntaxNode, ctxt: SpanCtxt) -> Result<Span, HirSourceMapError> {
        syntax_pointer(self.file, self.root, syntax)?;
        Ok(Span {
            file: self.file,
            range: syntax.text_range(),
            ctxt,
        })
    }

    /// Return exact decoded source text for syntax validated against this map's snapshot.
    pub fn text_for_syntax(&self, syntax: &SyntaxNode) -> Result<&str, HirSourceMapError> {
        syntax_pointer(self.file, self.root, syntax)?;
        let file = self
            .snapshot
            .file(self.file)
            .ok_or(HirSourceMapError::SourceTextNotValidForSnapshot)?;
        let range = syntax.text_range();
        file.source()
            .text()
            .get(range.start().to_usize()..range.end().to_usize())
            .ok_or(HirSourceMapError::SourceTextNotValidForSnapshot)
    }

    pub fn add_binding(&mut self, syntax: &SyntaxNode) -> Result<BindingId, HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        let binding = self
            .binding_to_syntax
            .push(pointer.clone())
            .map_err(|IndexVecError::TooManyElements| HirSourceMapError::TooManyBindings)?;
        self.binding_ids_by_syntax.insert(pointer, binding);
        Ok(binding)
    }

    /// Record a binding and return its validated source span.
    pub fn add_binding_with_span(
        &mut self,
        syntax: &SyntaxNode,
        ctxt: SpanCtxt,
    ) -> Result<(BindingId, Span), HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        let span = Span {
            file: self.file,
            range: syntax.text_range(),
            ctxt,
        };
        let binding = self
            .binding_to_syntax
            .push(pointer.clone())
            .map_err(|IndexVecError::TooManyElements| HirSourceMapError::TooManyBindings)?;
        self.binding_ids_by_syntax.insert(pointer, binding);
        Ok((binding, span))
    }

    /// Associate another syntax binding occurrence with an existing logical binding.
    ///
    /// Or-pattern alternatives use this to share one `BindingId` while keeping
    /// `binding_to_syntax` pointed at the first occurrence in source order.
    pub fn add_binding_alias(
        &mut self,
        syntax: &SyntaxNode,
        binding: BindingId,
    ) -> Result<(), HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        if self.binding_to_syntax.get(binding).is_none() {
            return Err(HirSourceMapError::UnknownBindingAlias);
        }
        self.binding_ids_by_syntax.insert(pointer, binding);
        Ok(())
    }

    /// Find a planned logical binding for one syntax occurrence, if present.
    pub fn binding_for_syntax(
        &self,
        syntax: &SyntaxNode,
    ) -> Result<Option<BindingId>, HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        Ok(self.binding_ids_by_syntax.get(&pointer).copied())
    }

    /// Create and record an error diagnostic at a validated syntax node.
    pub fn push_error_diagnostic_for_node(
        &mut self,
        code: Code,
        message: impl Into<String>,
        syntax: &SyntaxNode,
    ) -> Result<(), HirSourceMapError> {
        syntax_pointer(self.file, self.root, syntax)?;
        let file = self
            .snapshot
            .file(self.file)
            .ok_or(HirSourceMapError::DiagnosticNotValidForSnapshot)?;
        let location = Location::from_decoded_source(
            SourceRef {
                path: file.path().to_owned(),
                package: file.package().map(str::to_owned),
                snapshot: file.document_version(),
            },
            DiagnosticSpan::new(self.file, syntax.text_range()),
            file.source(),
        )
        .ok_or(HirSourceMapError::DiagnosticNotValidForSnapshot)?;
        let message = message.into();
        let mut diagnostic =
            Diagnostic::new(code, Severity::Error, message.clone(), Some(location));
        diagnostic.primary_label = message;
        self.push_diagnostic(diagnostic)
    }

    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> Result<(), HirSourceMapError> {
        let diagnostic = diagnostic
            .with_workspace_snapshot(&self.snapshot)
            .map_err(|_| HirSourceMapError::DiagnosticNotValidForSnapshot)?;
        let stored =
            match diagnostic.to_template_for_file(&self.snapshot, self.file, &self.source_ref) {
                Ok(template) => SourceMapDiagnostic::Template(template),
                Err(_) => SourceMapDiagnostic::SnapshotBound(diagnostic),
            };
        self.diagnostics.push(stored);
        Ok(())
    }

    pub fn finish(self) -> HirSourceMap {
        HirSourceMap {
            owner: self.owner,
            source_id: self.source_id,
            file: self.file,
            source_ref: self.source_ref,
            source_bytes: self.source_bytes,
            node_to_syntax: self.node_to_syntax,
            syntax_to_nodes: self.syntax_to_nodes,
            binding_to_syntax: self.binding_to_syntax,
            diagnostics: self.diagnostics,
        }
    }

    fn push_node_pointer(
        &mut self,
        pointer: SyntaxNodePtr,
    ) -> Result<ItemLocalId, HirSourceMapError> {
        let local_id = self
            .node_to_syntax
            .push(pointer.clone())
            .map_err(|IndexVecError::TooManyElements| HirSourceMapError::TooManyNodes)?;
        self.syntax_to_nodes
            .entry(pointer)
            .or_default()
            .push(local_id);
        Ok(local_id)
    }
}

fn syntax_pointer(
    file: crate::FileId,
    root: &SyntaxNode,
    node: &SyntaxNode,
) -> Result<SyntaxNodePtr, HirSourceMapError> {
    let pointer = SyntaxNodePtr::new(file, node).map_err(|error| match error {
        SyntaxNodePtrError::FileMismatch => HirSourceMapError::SyntaxFileMismatch,
    })?;
    if pointer.resolve(file, root).is_none() {
        return Err(HirSourceMapError::SyntaxTreeMismatch);
    }
    Ok(pointer)
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirSourceMapError {
    AstIdMapMismatch,
    WorkspaceSnapshotMismatch,
    FileMissingFromSnapshot,
    DiagnosticNotValidForSnapshot,
    SourceTextNotValidForSnapshot,
    UnknownBindingAlias,
    NestedDefinitionIdMismatch,
    SyntaxPointerNotInSourceMap,
    SyntaxFileMismatch,
    SyntaxTreeMismatch,
    TooManyNodes,
    TooManyBindings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, PackageInterner};
    use sai_diagnostics::{Code, Severity};
    use sai_syntax::{
        SyntaxKind,
        ast::{AstIdMap, AstNode},
    };
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};
    use std::sync::Arc;

    fn parse_file(
        file: crate::FileId,
        source: &str,
    ) -> (WorkspaceSnapshot, sai_syntax::Parse, AstIdMap) {
        let source = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, Some(1))])
                .expect("valid snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("parse succeeds");
        let ast_ids = AstIdMap::new(file, &parsed.root).expect("AST IDs");
        (snapshot, parsed, ast_ids)
    }

    fn function_node(root: &SyntaxNode) -> SyntaxNode {
        root.descendants()
            .find(|node| node.kind() == SyntaxKind::FN_DECL)
            .expect("function declaration")
    }

    fn owner_id(name: &str) -> OwnerId {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").expect("package interns");
        let symbols = crate::Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols).expect("path interner");
        let symbol = symbols.intern(name).expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .expect("definition path");
        OwnerId {
            def_id: paths.intern(path).expect("definition interns"),
        }
    }

    #[test]
    fn source_map_records_owner_nodes_shared_syntax_and_bindings() {
        let file = crate::FileId::new(51);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() {}\n");
        let function = function_node(&parsed.root);
        let name = sai_syntax::ast::FnDecl::cast(function.clone())
            .and_then(|function| function.name())
            .expect("function name");
        let owner = owner_id("f");
        let mut builder =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("builder");
        assert_eq!(builder.owner_hir_id().local_id, ItemLocalId::default());

        let first = builder.add_node(name.syntax()).expect("HIR node");
        let second = builder.add_node(name.syntax()).expect("HIR node");
        let binding = builder.add_binding(name.syntax()).expect("binding");
        builder
            .push_diagnostic(Diagnostic::new(
                Code::ExpectedToken,
                Severity::Error,
                "test diagnostic",
                None,
            ))
            .expect("diagnostic belongs to the map snapshot");
        let map = builder.finish();

        assert_eq!(map.file(), file);
        assert_eq!(map.source_ref().path, "src/lib.sai");
        assert_eq!(map.source_ref().snapshot, Some(1));
        assert!(map.matches_snapshot_file(&snapshot));
        assert_eq!(map.node_to_syntax().len(), 3);
        assert_eq!(map.binding_to_syntax().len(), 1);
        assert_eq!(
            map.nodes_for_syntax(map.node_to_syntax().get(ItemLocalId::default()).unwrap()),
            &[ItemLocalId::default()]
        );
        assert_eq!(
            map.nodes_for_syntax(map.syntax_for_node(second).unwrap()),
            &[first.local_id, second.local_id]
        );
        assert_eq!(
            map.syntax_for_binding(binding).unwrap().kind(),
            SyntaxKind::NAME
        );
        let diagnostics = map
            .diagnostics(&snapshot)
            .expect("diagnostics bind to the current snapshot");
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .clone()
                .with_workspace_snapshot(&snapshot)
                .is_ok()
        );

        let mut equivalent_builder =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("equivalent builder");
        equivalent_builder
            .add_node(name.syntax())
            .expect("first equivalent HIR node");
        equivalent_builder
            .add_node(name.syntax())
            .expect("second equivalent HIR node");
        equivalent_builder
            .add_binding(name.syntax())
            .expect("equivalent binding");
        equivalent_builder
            .push_diagnostic(Diagnostic::new(
                Code::ExpectedToken,
                Severity::Error,
                "test diagnostic",
                None,
            ))
            .expect("equivalent diagnostic");
        let equivalent = equivalent_builder.finish();
        assert_ne!(map.source_id(), equivalent.source_id());
        assert_eq!(map, equivalent);

        let other_owner = owner_id("g");
        assert!(
            map.syntax_for_node(HirId {
                owner: other_owner,
                local_id: first.local_id,
            })
            .is_none()
        );
    }

    #[test]
    fn source_map_rebinds_local_diagnostics_after_unrelated_file_changes() {
        let file = crate::FileId::new(54);
        let unrelated_file = crate::FileId::new(55);
        let owner_source =
            Arc::new(SourceFile::new(file, "fn f() {}\n", false).expect("valid owner source"));
        let make_snapshot = |unrelated_text: &str, document_version| {
            let unrelated_source = Arc::new(
                SourceFile::new(unrelated_file, unrelated_text, false)
                    .expect("valid unrelated source"),
            );
            WorkspaceSnapshot::new([
                SnapshotFile::new(Arc::clone(&owner_source), "src/lib.sai", None, Some(1)),
                SnapshotFile::new(
                    unrelated_source,
                    "src/other.sai",
                    None,
                    Some(document_version),
                ),
            ])
            .expect("valid workspace snapshot")
        };
        let owner = owner_id("f");
        let build_source_map = |snapshot: &WorkspaceSnapshot| {
            let parsed = sai_syntax::parse(snapshot, file).expect("parse succeeds");
            let ast_ids = AstIdMap::new(file, &parsed.root).expect("AST IDs");
            let function = function_node(&parsed.root);
            let mut builder =
                HirSourceMapBuilder::new(owner, &ast_ids, snapshot, &parsed.root, &function)
                    .expect("builder");
            builder.add_node(&function).expect("owner node is recorded");
            builder
                .push_error_diagnostic_for_node(Code::ExpectedToken, "local diagnostic", &function)
                .expect("diagnostic belongs to the owner file");
            (builder.finish(), ast_ids, parsed)
        };

        let first_snapshot = make_snapshot("fn first() {}\n", 1);
        let second_snapshot = make_snapshot("fn second() {}\n", 2);
        let (first_map, _, _) = build_source_map(&first_snapshot);
        let (second_map, second_ast_ids, second_parse) = build_source_map(&second_snapshot);
        assert!(first_map.matches_snapshot_file(&second_snapshot));
        assert!(same_pointer_index_vec(
            &first_map.node_to_syntax,
            &second_map.node_to_syntax
        ));
        assert!(same_syntax_to_nodes(
            &first_map.syntax_to_nodes,
            &second_map.syntax_to_nodes
        ));
        assert!(first_map.diagnostics == second_map.diagnostics);
        assert!(same_pointer_index_vec(
            &first_map.binding_to_syntax,
            &second_map.binding_to_syntax
        ));
        assert_eq!(first_map.owner, second_map.owner);
        assert_eq!(first_map.file, second_map.file);
        assert_eq!(first_map.source_ref, second_map.source_ref);
        assert_eq!(first_map.source_bytes, second_map.source_bytes);
        assert_eq!(first_map, second_map);
        let original_pointer = first_map
            .node_to_syntax()
            .get(ItemLocalId::default())
            .expect("source map has its owner pointer");
        let rebound_pointer = first_map
            .rebind_syntax_pointer(
                original_pointer,
                &second_ast_ids,
                &second_snapshot,
                &second_parse.root,
            )
            .expect("source pointer can be rebound")
            .expect("source pointer identifies a node in the current root");
        assert!(rebound_pointer.resolve(file, &second_parse.root).is_some());

        let rebound = first_map
            .diagnostics(&second_snapshot)
            .expect("local diagnostics rebind to the current snapshot");
        assert_eq!(rebound.len(), 1);
        assert!(
            rebound[0]
                .clone()
                .with_workspace_snapshot(&second_snapshot)
                .is_ok()
        );

        let changed_owner = Arc::new(
            SourceFile::new(file, "fn f() { true }\n", false).expect("valid owner source"),
        );
        let changed_snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(changed_owner, "src/lib.sai", None, Some(2)),
            SnapshotFile::new(
                Arc::new(
                    SourceFile::new(unrelated_file, "fn second() {}\n", false)
                        .expect("valid unrelated source"),
                ),
                "src/other.sai",
                None,
                Some(2),
            ),
        ])
        .expect("valid changed workspace snapshot");
        assert_eq!(
            first_map.diagnostics(&changed_snapshot),
            Err(HirSourceMapError::SourceTextNotValidForSnapshot)
        );
    }

    #[test]
    fn source_map_rejects_foreign_pointer_before_rebinding() {
        let file = crate::FileId::new(58);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() {}\n");
        let function = function_node(&parsed.root);
        let owner = owner_id("f");
        let mut builder =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("builder");
        builder.add_node(&function).expect("owner node is recorded");
        let map = builder.finish();

        let (_, foreign_parse, _) = parse_file(file, "fn g() {}\n");
        let foreign_function = function_node(&foreign_parse.root);
        let foreign_pointer =
            SyntaxNodePtr::new(file, &foreign_function).expect("foreign pointer is valid");
        let stored_pointer = map
            .node_to_syntax()
            .get(ItemLocalId::default())
            .expect("source map has its owner pointer");
        assert!(
            foreign_pointer.is_same_source_node(stored_pointer),
            "the collision must exercise equal node shape across unrelated roots"
        );

        assert_eq!(
            map.rebind_syntax_pointer(&foreign_pointer, &ast_ids, &snapshot, &parsed.root),
            Err(HirSourceMapError::SyntaxPointerNotInSourceMap)
        );
    }

    #[test]
    fn source_map_keeps_contextual_diagnostics_bound_to_their_snapshot() {
        let file = crate::FileId::new(56);
        let unrelated_file = crate::FileId::new(57);
        let owner_source =
            Arc::new(SourceFile::new(file, "fn f() {}\n", false).expect("valid owner source"));
        let make_snapshot = |unrelated_text: &str, document_version| {
            WorkspaceSnapshot::new([
                SnapshotFile::new(Arc::clone(&owner_source), "src/lib.sai", None, Some(1)),
                SnapshotFile::new(
                    Arc::new(
                        SourceFile::new(unrelated_file, unrelated_text, false)
                            .expect("valid unrelated source"),
                    ),
                    "src/other.sai",
                    None,
                    Some(document_version),
                ),
            ])
            .expect("valid workspace snapshot")
        };
        let owner = owner_id("f");
        let build_source_map = |snapshot: &WorkspaceSnapshot| {
            let parsed = sai_syntax::parse(snapshot, file).expect("parse succeeds");
            let ast_ids = AstIdMap::new(file, &parsed.root).expect("AST IDs");
            let function = function_node(&parsed.root);
            let mut builder =
                HirSourceMapBuilder::new(owner, &ast_ids, snapshot, &parsed.root, &function)
                    .expect("builder");
            let diagnostic = Diagnostic::new(
                Code::ExpectedToken,
                Severity::Error,
                "contextual diagnostic",
                None,
            )
            .with_cause(sai_diagnostics::CauseKey::new([
                sai_diagnostics::PoisonFact::ErrorType("cause".to_owned()),
            ]))
            .expect("diagnostic cause is valid");
            builder
                .push_diagnostic(diagnostic)
                .expect("diagnostic belongs to its source map snapshot");
            builder.finish()
        };

        let first_snapshot = make_snapshot("fn first() {}\n", 1);
        let second_snapshot = make_snapshot("fn second() {}\n", 2);
        let first_map = build_source_map(&first_snapshot);
        let second_map = build_source_map(&second_snapshot);
        assert_ne!(first_map, second_map);
        assert_eq!(
            first_map
                .diagnostics(&first_snapshot)
                .expect("diagnostics remain valid for their own snapshot")
                .len(),
            1
        );
        assert_eq!(
            first_map.diagnostics(&second_snapshot),
            Err(HirSourceMapError::DiagnosticNotValidForSnapshot)
        );
    }

    #[test]
    fn lowering_span_helpers_bind_ranges_to_validated_hir_nodes_and_bindings() {
        let file = crate::FileId::new(53);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f(x: I32) { for i in x {} }\n");
        let function = function_node(&parsed.root);
        let function_ast =
            sai_syntax::ast::FnDecl::cast(function.clone()).expect("function declaration");
        let name = function_ast.name().expect("function name");
        let parameter_name = function_ast
            .params()
            .and_then(|params| params.params().next())
            .and_then(|param| param.name())
            .expect("parameter name");
        let for_loop = function
            .descendants()
            .find(|node| node.kind() == SyntaxKind::FOR_EXPR)
            .expect("for expression");
        let owner = owner_id("f");
        let mut builder =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("builder");

        let (node_id, node_span) = builder
            .add_node_with_span(name.syntax(), SpanCtxt::Root)
            .expect("node span");
        assert_eq!(node_id.local_id.raw(), 1);
        assert_eq!(node_span.file, file);
        assert_eq!(node_span.range, name.syntax().text_range());
        assert_eq!(node_span.ctxt, SpanCtxt::Root);

        let desugared_ctxt = SpanCtxt::Desugared(crate::DesugaringKind::ForLoop);
        let desugared_span = builder
            .span_for(&for_loop, desugared_ctxt)
            .expect("desugared span");
        assert_eq!(desugared_span.range, for_loop.text_range());
        assert_eq!(desugared_span.ctxt, desugared_ctxt);
        let (binding_id, binding_span) = builder
            .add_binding_with_span(parameter_name.syntax(), SpanCtxt::Root)
            .expect("binding span");
        assert_eq!(binding_id.raw(), 0);
        assert_eq!(binding_span.file, file);
        assert_eq!(binding_span.range, parameter_name.syntax().text_range());
        assert_eq!(binding_span.ctxt, SpanCtxt::Root);

        let unrecorded_span = builder
            .span_for(&function, SpanCtxt::Root)
            .expect("owner span");
        assert_eq!(unrecorded_span.range, function.text_range());
        let map = builder.finish();
        assert_eq!(
            map.syntax_for_node(node_id).unwrap().kind(),
            SyntaxKind::NAME
        );
        assert_eq!(
            map.syntax_for_binding(binding_id).unwrap().kind(),
            SyntaxKind::NAME
        );
        assert_eq!(map.node_to_syntax().len(), 2);
        assert_eq!(map.binding_to_syntax().len(), 1);
    }

    #[test]
    fn source_map_rejects_foreign_files_and_snapshots() {
        let file = crate::FileId::new(52);
        let (snapshot, parsed, ast_ids) = parse_file(file, "fn f() {}\n");
        let function = function_node(&parsed.root);
        let owner = owner_id("f");
        let mut builder =
            HirSourceMapBuilder::new(owner, &ast_ids, &snapshot, &parsed.root, &function)
                .expect("builder");

        let (other_snapshot, other_parsed, _) = parse_file(file, "fn f() {}\n");
        let other_function = function_node(&other_parsed.root);
        assert_eq!(
            builder.add_node(&other_function),
            Err(HirSourceMapError::SyntaxTreeMismatch)
        );
        let diagnostic_from_other_snapshot = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "foreign snapshot diagnostic",
            None,
        )
        .with_workspace_snapshot(&other_snapshot)
        .expect("empty diagnostic binds to its snapshot");
        assert_eq!(
            builder.push_diagnostic(diagnostic_from_other_snapshot),
            Err(HirSourceMapError::DiagnosticNotValidForSnapshot)
        );

        let other_file = crate::FileId::new(53);
        let (_other_file_snapshot, other_file_parsed, _) = parse_file(other_file, "fn g() {}\n");
        let other_file_function = function_node(&other_file_parsed.root);
        assert_eq!(
            builder.add_node(&other_file_function),
            Err(HirSourceMapError::SyntaxFileMismatch)
        );

        let other_root_ids = AstIdMap::new(file, &other_parsed.root).expect("other AST IDs");
        assert_eq!(
            HirSourceMapBuilder::new(
                owner,
                &other_root_ids,
                &other_snapshot,
                &parsed.root,
                &function
            )
            .err(),
            Some(HirSourceMapError::AstIdMapMismatch)
        );
        assert_eq!(
            HirSourceMapBuilder::new(owner, &ast_ids, &other_snapshot, &parsed.root, &function)
                .err(),
            Some(HirSourceMapError::WorkspaceSnapshotMismatch)
        );
    }
}
