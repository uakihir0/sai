use crate::{BindingId, HirId, IndexVec, IndexVecError, ItemLocalId, OwnerId, Span, SpanCtxt};
use sai_diagnostics::Diagnostic;
use sai_syntax::{SyntaxNode, SyntaxNodePtr, SyntaxNodePtrError, ast::AstIdMap};
use std::collections::HashMap;

/// Source relationships for one HIR owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirSourceMap {
    owner: OwnerId,
    node_to_syntax: IndexVec<ItemLocalId, SyntaxNodePtr>,
    syntax_to_nodes: HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
    binding_to_syntax: IndexVec<BindingId, SyntaxNodePtr>,
    diagnostics: Vec<Diagnostic>,
}

impl HirSourceMap {
    pub const fn owner(&self) -> OwnerId {
        self.owner
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

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
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

/// Builds a source map while checking every syntax node against one parsed
/// file root and snapshot.
pub struct HirSourceMapBuilder<'root> {
    owner: OwnerId,
    snapshot: sai_vfs::WorkspaceSnapshot,
    file: crate::FileId,
    root: &'root SyntaxNode,
    node_to_syntax: IndexVec<ItemLocalId, SyntaxNodePtr>,
    syntax_to_nodes: HashMap<SyntaxNodePtr, Vec<ItemLocalId>>,
    binding_to_syntax: IndexVec<BindingId, SyntaxNodePtr>,
    diagnostics: Vec<Diagnostic>,
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

        let owner_pointer = syntax_pointer(file, root, owner_syntax)?;
        let mut builder = Self {
            owner,
            snapshot: snapshot.clone(),
            file,
            root,
            node_to_syntax: IndexVec::new(),
            syntax_to_nodes: HashMap::new(),
            binding_to_syntax: IndexVec::new(),
            diagnostics: Vec::new(),
        };
        let owner_local_id = builder.push_node_pointer(owner_pointer)?;
        debug_assert_eq!(owner_local_id, ItemLocalId::default());
        Ok(builder)
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

    pub fn add_binding(&mut self, syntax: &SyntaxNode) -> Result<BindingId, HirSourceMapError> {
        let pointer = syntax_pointer(self.file, self.root, syntax)?;
        self.binding_to_syntax
            .push(pointer)
            .map_err(|IndexVecError::TooManyElements| HirSourceMapError::TooManyBindings)
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
            .push(pointer)
            .map_err(|IndexVecError::TooManyElements| HirSourceMapError::TooManyBindings)?;
        Ok((binding, span))
    }

    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> Result<(), HirSourceMapError> {
        let diagnostic = diagnostic
            .with_workspace_snapshot(&self.snapshot)
            .map_err(|_| HirSourceMapError::DiagnosticNotValidForSnapshot)?;
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    pub fn finish(self) -> HirSourceMap {
        HirSourceMap {
            owner: self.owner,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HirSourceMapError {
    AstIdMapMismatch,
    WorkspaceSnapshotMismatch,
    DiagnosticNotValidForSnapshot,
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
        assert_eq!(map.diagnostics().len(), 1);
        assert!(
            map.diagnostics()[0]
                .clone()
                .with_workspace_snapshot(&snapshot)
                .is_ok()
        );

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
