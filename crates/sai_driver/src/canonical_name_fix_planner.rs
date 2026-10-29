//! A deliberately narrow planner for unused local binding fixes.
//!
//! This accepts only prefixing a direct ASCII binding in a let or var statement
//! with `_`, when both complete files are BOM-free, LF-only, and already
//! formatter-canonical. It refuses any case needing actual unit formatting or
//! layout widening. Whole-file equivalence relies on the formatter conforming
//! to the unit-formatting guarantee N-H-60; this planner does not verify that
//! conformance. Full format-unit support is future work.

use std::cell::RefCell;
use std::collections::BTreeMap;

use sai_diagnostics::{
    FixOperation, FixOperationName, FixOperationPlanner, Location, NodeGroup, OperationPlanError,
    TextEdit, Value,
};
use sai_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use sai_vfs::{FileId, Span, TextRange, WorkspaceSnapshot, WorkspaceSnapshotId};

/// Plans against exactly the workspace snapshot supplied at construction.
pub struct CanonicalNameFixPlanner {
    snapshot: WorkspaceSnapshotId,
    original: RefCell<Option<(FileId, Result<SyntaxNode, OperationPlanError>)>>,
    last_target: RefCell<Option<(FixOperation, Location, Span)>>,
    #[cfg(test)]
    format_calls: std::cell::Cell<usize>,
}

impl CanonicalNameFixPlanner {
    pub fn new(snapshot: &WorkspaceSnapshot) -> Self {
        Self {
            snapshot: snapshot.id(),
            original: RefCell::new(None),
            last_target: RefCell::new(None),
            #[cfg(test)]
            format_calls: std::cell::Cell::new(0),
        }
    }

    fn validated_target(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Span, OperationPlanError> {
        if operation.name() != FixOperationName::ReplaceNode {
            return Err(OperationPlanError::UnsupportedOperation);
        }
        if snapshot.id() != self.snapshot {
            return Err(OperationPlanError::NotApplicable);
        }
        if let Some((cached_operation, cached_primary, target)) = self.last_target.borrow().as_ref()
            && cached_operation == operation
            && cached_primary == primary
        {
            return Ok(*target);
        }
        let target = self.compute_target(operation, primary, snapshot)?;
        *self.last_target.borrow_mut() = Some((operation.clone(), primary.clone(), target));
        Ok(target)
    }

    fn format_source(&self, source: &[u8]) -> Result<String, OperationPlanError> {
        #[cfg(test)]
        self.format_calls.set(self.format_calls.get() + 1);
        sai_fmt::format_source(source).map_err(|_| OperationPlanError::FormattingFailed)
    }

    fn original_root(
        &self,
        snapshot: &WorkspaceSnapshot,
        file_id: FileId,
    ) -> Result<SyntaxNode, OperationPlanError> {
        if let Some((cached_id, root)) = self.original.borrow().as_ref()
            && *cached_id == file_id
        {
            return root.clone();
        }
        let result = (|| {
            let file = snapshot
                .file(file_id)
                .ok_or(OperationPlanError::TargetNotFound)?;
            let source = file.source();
            if source.had_bom()
                || source.text().contains('\r')
                || !source.invalid_utf8_sequences().is_empty()
            {
                return Err(OperationPlanError::NotApplicable);
            }
            if self.format_source(source.raw_bytes())? != source.text() {
                return Err(OperationPlanError::NotApplicable);
            }
            Ok(sai_syntax::parse(snapshot, file_id)
                .map_err(|_| OperationPlanError::NotApplicable)?
                .root)
        })();
        *self.original.borrow_mut() = Some((file_id, result.clone()));
        result
    }

    fn compute_target(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Span, OperationPlanError> {
        let file = snapshot
            .file(primary.span.file)
            .ok_or(OperationPlanError::TargetNotFound)?;
        if !file.matches_reference(
            &primary.file.path,
            primary.file.package.as_deref(),
            primary.file.snapshot,
        ) {
            return Err(OperationPlanError::NotApplicable);
        }
        let source = file.source();
        let raw = source.raw_bytes();
        if operation.node().group() != NodeGroup::Name {
            return Err(OperationPlanError::TargetNotFound);
        }
        let root = self.original_root(snapshot, primary.span.file)?;
        let node = name_at_index(&root, operation.node().index())
            .ok_or(OperationPlanError::TargetNotFound)?;
        if !is_direct_binding(&node) {
            return Err(OperationPlanError::NotApplicable);
        }
        let mut children = node.children_with_tokens();
        let Some(SyntaxElement::Token(token)) = children.next() else {
            return Err(OperationPlanError::NotApplicable);
        };
        if children.next().is_some()
            || token.kind() != SyntaxKind::LIDENT
            || token.text_range() != node.text_range()
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let name = token.text();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !name.as_bytes()[0].is_ascii_lowercase()
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let Some(Value::String(replacement)) = operation.args().get("text") else {
            return Err(OperationPlanError::NotApplicable);
        };
        if replacement != &format!("_{name}") {
            return Err(OperationPlanError::NotApplicable);
        }
        let range = source
            .to_original_range(node.text_range())
            .ok_or(OperationPlanError::NotApplicable)?;
        if primary.span != Span::new(primary.span.file, range)
            || raw.get(range.start().to_usize()..range.end().to_usize()) != Some(name.as_bytes())
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let mut changed = raw.to_vec();
        changed.splice(
            range.start().to_usize()..range.end().to_usize(),
            replacement.bytes(),
        );
        let changed_text =
            std::str::from_utf8(&changed).map_err(|_| OperationPlanError::NotApplicable)?;
        if self.format_source(&changed)? != changed_text {
            return Err(OperationPlanError::NotApplicable);
        }
        let reparsed =
            sai_fmt::parse_source(&changed).map_err(|_| OperationPlanError::NotApplicable)?;
        let new_node = name_at_index(reparsed.root(), operation.node().index())
            .ok_or(OperationPlanError::NotApplicable)?;
        let mut new_children = new_node.children_with_tokens();
        match (new_children.next(), new_children.next()) {
            (Some(SyntaxElement::Token(new_token)), None)
                if new_token.kind() == token.kind() && new_token.text() == replacement => {}
            _ => return Err(OperationPlanError::NotApplicable),
        }
        if !same_syntax_except_name(&root, reparsed.root(), operation.node().index()) {
            return Err(OperationPlanError::NotApplicable);
        }
        Ok(Span::new(primary.span.file, range))
    }
}

fn is_direct_binding(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SyntaxKind::VAR_STMT => true,
        SyntaxKind::BINDING_PAT => {
            parent
                .parent()
                .is_some_and(|statement| statement.kind() == SyntaxKind::LET_STMT)
                && parent.children().count() == 1
        }
        _ => false,
    }
}

fn name_at_index(root: &SyntaxNode, target: u64) -> Option<SyntaxNode> {
    let mut stack = vec![SyntaxElement::Node(root.clone())];
    let mut index = 0_u64;
    while let Some(element) = stack.pop() {
        if index == target {
            return match element {
                SyntaxElement::Node(node) if node.kind() == SyntaxKind::NAME => Some(node),
                _ => None,
            };
        }
        index = index.checked_add(1)?;
        if let SyntaxElement::Node(node) = element {
            stack.extend(
                node.children_with_tokens()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
    }
    None
}

fn same_syntax_except_name(before: &SyntaxNode, after: &SyntaxNode, target: u64) -> bool {
    let mut stack = vec![(
        SyntaxElement::Node(before.clone()),
        SyntaxElement::Node(after.clone()),
    )];
    let mut index = 0_u64;
    while let Some((left, right)) = stack.pop() {
        match (left, right) {
            (SyntaxElement::Node(left), SyntaxElement::Node(right))
                if left.kind() == right.kind() =>
            {
                let left_children = left.children_with_tokens().collect::<Vec<_>>();
                let right_children = right.children_with_tokens().collect::<Vec<_>>();
                if left_children.len() != right_children.len() {
                    return false;
                }
                stack.extend(left_children.into_iter().zip(right_children).rev());
            }
            (SyntaxElement::Token(left), SyntaxElement::Token(right))
                if left.kind() == right.kind()
                    && (target.checked_add(1) == Some(index) || left.text() == right.text()) => {}
            _ => return false,
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    true
}

impl FixOperationPlanner for CanonicalNameFixPlanner {
    fn plan_edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError> {
        let target = self.validated_target(operation, primary, snapshot)?;
        Ok(vec![TextEdit {
            file: primary.file.clone(),
            range: Span::new(target.file, TextRange::empty(target.range.start())),
            new_text: "_".to_owned(),
        }])
    }

    fn apply_operation(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
        let target = self.validated_target(operation, primary, snapshot)?;
        let Some(Value::String(replacement)) = operation.args().get("text") else {
            return Err(OperationPlanError::NotApplicable);
        };
        let mut result = snapshot
            .file(target.file)
            .ok_or(OperationPlanError::TargetNotFound)?
            .source()
            .raw_bytes()
            .to_vec();
        result.splice(
            target.range.start().to_usize()..target.range.end().to_usize(),
            replacement.bytes(),
        );
        Ok(BTreeMap::from([(target.file, result)]))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sai_diagnostics::{Applicability, Fix, NodeId, SourceRef, ValueObject};
    use sai_vfs::{SnapshotFile, SourceFile};

    use super::*;

    fn snapshot(bytes: &[u8]) -> WorkspaceSnapshot {
        let id = FileId::new(1);
        let source = Arc::new(SourceFile::decode(id, bytes).unwrap());
        WorkspaceSnapshot::new([SnapshotFile::new(source, "src/main.sai", None, Some(7))]).unwrap()
    }

    fn target(snapshot: &WorkspaceSnapshot, name: &str) -> (Location, FixOperation) {
        let file_id = FileId::new(1);
        let file = snapshot.file(file_id).unwrap();
        let parsed = sai_syntax::parse(snapshot, file_id).unwrap();
        let mut stack = vec![SyntaxElement::Node(parsed.root.clone())];
        let mut index = 0_u64;
        while let Some(element) = stack.pop() {
            if let SyntaxElement::Node(node) = element {
                if node.kind() == SyntaxKind::NAME && node.children_with_tokens().any(|child| matches!(child, SyntaxElement::Token(token) if token.text() == name)) {
                    let location = Location::from_decoded_source(
                        SourceRef { path: file.path().to_owned(), package: file.package().map(str::to_owned), snapshot: file.document_version() },
                        Span::new(file_id, node.text_range()), file.source(),
                    ).unwrap();
                    let operation = FixOperation::new(FixOperationName::ReplaceNode, NodeId::new(NodeGroup::Name, index), ValueObject::from([("text".to_owned(), Value::String(format!("_{name}")))] )).unwrap();
                    return (location, operation);
                }
                stack.extend(
                    node.children_with_tokens()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                );
            }
            index += 1;
        }
        panic!("name not found: {name}");
    }

    #[test]
    fn plans_minimal_fix_and_fix_new_agrees() {
        let snapshot = snapshot("// 日本語\nfn f() {\n    let value = 1\n}\n".as_bytes());
        let (primary, operation) = target(&snapshot, "value");
        let planner = CanonicalNameFixPlanner::new(&snapshot);
        let fix = Fix::new(
            "unused",
            Applicability::MachineApplicable,
            operation,
            vec![],
            &primary,
            &snapshot,
            &planner,
        )
        .unwrap();
        assert_eq!(fix.edits().len(), 1);
        assert_eq!(
            fix.edits()[0].range.range,
            TextRange::empty(primary.span.range.start())
        );
        assert_eq!(fix.edits()[0].new_text, "_");
        let result = planner
            .apply_operation(fix.operation(), &primary, &snapshot)
            .unwrap();
        assert_eq!(
            result[&primary.span.file],
            "// 日本語\nfn f() {\n    let _value = 1\n}\n".as_bytes()
        );
    }

    #[test]
    fn rejects_bom_and_non_lf_line_endings() {
        for input in [
            "\u{feff}fn f() {\n    var value = 1\n}\n",
            "fn f() {\r\n    var value = 1\r\n}\r\n",
            "\u{feff}fn f() {\r\n    var value = 1\r\n}\r\n",
            "fn f() {\r\n    var value = 1\n}\n",
            "fn f() {\r    var value = 1\r}\r",
        ] {
            let snapshot = snapshot(input.as_bytes());
            let (primary, operation) = target(&snapshot, "value");
            let planner = CanonicalNameFixPlanner::new(&snapshot);
            assert_eq!(
                planner.plan_edits(&operation, &primary, &snapshot),
                Err(OperationPlanError::NotApplicable)
            );
            assert_eq!(
                planner.apply_operation(&operation, &primary, &snapshot),
                Err(OperationPlanError::NotApplicable)
            );
            assert_eq!(planner.format_calls.get(), 0);
        }
    }

    #[test]
    fn rejects_stale_snapshot_bad_reference_and_wrong_id() {
        let input = b"fn f() {\n    let value = 1\n}\n";
        let original = snapshot(input);
        let (primary, operation) = target(&original, "value");
        let planner = CanonicalNameFixPlanner::new(&original);
        assert!(planner.plan_edits(&operation, &primary, &original).is_ok());
        let other = snapshot(input);
        assert_eq!(
            planner.plan_edits(&operation, &primary, &other),
            Err(OperationPlanError::NotApplicable)
        );
        let mut bad_location = primary.clone();
        bad_location.file.snapshot = Some(8);
        assert_eq!(
            planner.plan_edits(&operation, &bad_location, &original),
            Err(OperationPlanError::NotApplicable)
        );
        let wrong = FixOperation::new(
            FixOperationName::ReplaceNode,
            NodeId::new(NodeGroup::Name, 0),
            operation.args().clone(),
        )
        .unwrap();
        assert_eq!(
            planner.plan_edits(&wrong, &primary, &original),
            Err(OperationPlanError::TargetNotFound)
        );
        let mut bad_range = primary.clone();
        bad_range.span.range = TextRange::empty(primary.span.range.start());
        assert_eq!(
            planner.plan_edits(&operation, &bad_range, &original),
            Err(OperationPlanError::NotApplicable)
        );
    }

    #[test]
    fn rejects_noncanonical_and_invalid_inputs() {
        let original = snapshot(b"fn f(){let value=1\n}\n");
        let (primary, operation) = target(&original, "value");
        assert_eq!(
            CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
            Err(OperationPlanError::NotApplicable)
        );
        let original = snapshot(b"fn f() {\n    let value = 1\n}\n// \xff\n");
        let (primary, operation) = target(&original, "value");
        assert_eq!(
            CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
            Err(OperationPlanError::NotApplicable)
        );
    }

    #[test]
    fn rejects_other_replacements_and_operations() {
        let original = snapshot(b"fn f() {\n    let value = 1\n}\n");
        let (primary, operation) = target(&original, "value");
        let planner = CanonicalNameFixPlanner::new(&original);
        let other = FixOperation::new(
            FixOperationName::ReplaceNode,
            operation.node(),
            ValueObject::from([("text".to_owned(), Value::String("other".to_owned()))]),
        )
        .unwrap();
        assert_eq!(
            planner.plan_edits(&other, &primary, &original),
            Err(OperationPlanError::NotApplicable)
        );
        let other = FixOperation::new(
            FixOperationName::RemoveNode,
            operation.node(),
            ValueObject::new(),
        )
        .unwrap();
        assert_eq!(
            planner.plan_edits(&other, &primary, &original),
            Err(OperationPlanError::UnsupportedOperation)
        );
    }

    #[test]
    fn rejects_parameters_raw_identifiers_and_formatter_refusal() {
        let parameter = sai_fmt::format_source(b"fn f(value: I32) {}").unwrap();
        let original = snapshot(parameter.as_bytes());
        let (primary, operation) = target(&original, "value");
        assert_eq!(
            CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
            Err(OperationPlanError::NotApplicable)
        );

        let raw = sai_fmt::format_source(b"fn f() { let r#type = 1\n }").unwrap();
        let original = snapshot(raw.as_bytes());
        let (primary, operation) = target(&original, "r#type");
        assert_eq!(
            CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
            Err(OperationPlanError::NotApplicable)
        );

        let original = snapshot(b"fn f() {\n    let value /* comment */ = 1\n}\n");
        let (primary, operation) = target(&original, "value");
        assert_eq!(
            CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
            Err(OperationPlanError::FormattingFailed)
        );
    }

    #[test]
    fn rejects_nested_patterns_and_constructor_puns() {
        for source in [
            "fn f() {\n    let User(name) = input\n}\n",
            "fn f() {\n    let User(name: value) = input\n}\n",
            "fn f() {\n    let (value, other) = input\n}\n",
            "fn f() {\n    let (value) = input\n}\n",
            "fn f() {\n    let value @ User(other) = input\n}\n",
            "fn f() {\n    let var value = input\n}\n",
            "fn f() {\n    let closure = fn(value: I32) { value }\n}\n",
        ] {
            let canonical = sai_fmt::format_source(source.as_bytes()).unwrap();
            let original = snapshot(canonical.as_bytes());
            let name = if source.contains("User(name) =") {
                "name"
            } else {
                "value"
            };
            let (primary, operation) = target(&original, name);
            assert_eq!(
                CanonicalNameFixPlanner::new(&original).plan_edits(&operation, &primary, &original),
                Err(OperationPlanError::NotApplicable),
                "source: {canonical}"
            );
        }
    }

    #[test]
    fn reuses_snapshot_and_fix_plan_without_changing_results() {
        use std::time::Instant;

        let mut source = String::from("fn f() {\n");
        for index in 0..8 {
            source.push_str(&format!("    let value{index} = {index}\n"));
        }
        source.push_str("}\n");
        assert_eq!(sai_fmt::format_source(source.as_bytes()).unwrap(), source);
        let snapshot = snapshot(source.as_bytes());
        let targets = (0..8)
            .map(|index| target(&snapshot, &format!("value{index}")))
            .collect::<Vec<_>>();

        let baseline_start = Instant::now();
        let mut baseline_calls = 0;
        let mut expected = Vec::new();
        for (primary, operation) in &targets {
            let first = CanonicalNameFixPlanner::new(&snapshot);
            let edits = first.plan_edits(operation, primary, &snapshot).unwrap();
            let second = CanonicalNameFixPlanner::new(&snapshot);
            let applied = second
                .apply_operation(operation, primary, &snapshot)
                .unwrap();
            baseline_calls += first.format_calls.get() + second.format_calls.get();
            expected.push((edits, applied));
        }
        let baseline_elapsed = baseline_start.elapsed();

        let cached_start = Instant::now();
        let planner = CanonicalNameFixPlanner::new(&snapshot);
        for ((primary, operation), (edits, applied)) in targets.iter().zip(&expected) {
            let fix = Fix::new(
                "unused",
                Applicability::MachineApplicable,
                operation.clone(),
                vec![],
                primary,
                &snapshot,
                &planner,
            )
            .unwrap();
            assert_eq!(fix.edits(), edits);
            assert_eq!(
                &planner
                    .apply_operation(operation, primary, &snapshot)
                    .unwrap(),
                applied
            );
        }
        let cached_elapsed = cached_start.elapsed();
        assert_eq!(baseline_calls, 32);
        assert_eq!(planner.format_calls.get(), 9);
        eprintln!(
            "8 candidates: uncached {baseline_calls} formatter calls in {baseline_elapsed:?}; cached {} calls in {cached_elapsed:?}",
            planner.format_calls.get()
        );
    }

    #[test]
    fn switches_bounded_cache_between_snapshot_files() {
        let input = b"fn f() {\n    let value = 1\n}\n";
        let other_input = b"fn f() {\n    let other = 1\n}\n";
        let first_id = FileId::new(1);
        let second_id = FileId::new(2);
        let first = Arc::new(SourceFile::decode(first_id, input).unwrap());
        let second = Arc::new(SourceFile::decode(second_id, other_input).unwrap());
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(first, "src/main.sai", None, Some(7)),
            SnapshotFile::new(second, "src/other.sai", None, Some(7)),
        ])
        .unwrap();
        let (primary, operation) = target(&snapshot, "value");
        let mut second_primary = primary.clone();
        second_primary.file.path = "src/other.sai".to_owned();
        second_primary.span = Span::new(second_id, primary.span.range);
        let second_operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            operation.node(),
            ValueObject::from([("text".to_owned(), Value::String("_other".to_owned()))]),
        )
        .unwrap();
        let planner = CanonicalNameFixPlanner::new(&snapshot);
        assert_eq!(
            planner.plan_edits(&operation, &primary, &snapshot).unwrap()[0]
                .range
                .file,
            first_id
        );
        assert_eq!(
            planner.apply_operation(&operation, &second_primary, &snapshot),
            Err(OperationPlanError::NotApplicable)
        );
        assert_eq!(
            planner
                .plan_edits(&second_operation, &second_primary, &snapshot)
                .unwrap()[0]
                .range
                .file,
            second_id
        );
        assert_eq!(
            planner
                .apply_operation(&second_operation, &second_primary, &snapshot)
                .unwrap()[&second_id],
            b"fn f() {\n    let _other = 1\n}\n"
        );
        assert_eq!(
            planner.plan_edits(&operation, &primary, &snapshot).unwrap()[0]
                .range
                .file,
            first_id
        );
        assert_eq!(planner.format_calls.get(), 6);
    }

    #[test]
    fn changed_arguments_cannot_reuse_cached_validation() {
        let snapshot = snapshot(b"fn f() {\n    let value = 1\n}\n");
        let (primary, operation) = target(&snapshot, "value");
        let planner = CanonicalNameFixPlanner::new(&snapshot);
        let expected = b"fn f() {\n    let _value = 1\n}\n";
        assert_eq!(
            planner
                .apply_operation(&operation, &primary, &snapshot)
                .unwrap()[&primary.span.file],
            expected
        );
        let changed = FixOperation::new(
            FixOperationName::ReplaceNode,
            operation.node(),
            ValueObject::from([("text".to_owned(), Value::String("_other".to_owned()))]),
        )
        .unwrap();
        assert_eq!(
            planner.plan_edits(&changed, &primary, &snapshot),
            Err(OperationPlanError::NotApplicable)
        );
        assert_eq!(
            planner.apply_operation(&changed, &primary, &snapshot),
            Err(OperationPlanError::NotApplicable)
        );
        assert_eq!(
            planner
                .apply_operation(&operation, &primary, &snapshot)
                .unwrap()[&primary.span.file],
            expected
        );
        assert_eq!(planner.format_calls.get(), 2);
    }

    #[test]
    fn fix_rejects_planned_edit_that_disagrees_with_operation() {
        use sai_diagnostics::FixValidationError;

        struct WrongEdit(CanonicalNameFixPlanner);
        impl FixOperationPlanner for WrongEdit {
            fn plan_edits(
                &self,
                operation: &FixOperation,
                primary: &Location,
                snapshot: &WorkspaceSnapshot,
            ) -> Result<Vec<TextEdit>, OperationPlanError> {
                let mut edits = self.0.plan_edits(operation, primary, snapshot)?;
                edits[0].new_text = "!".to_owned();
                Ok(edits)
            }
            fn apply_operation(
                &self,
                operation: &FixOperation,
                primary: &Location,
                snapshot: &WorkspaceSnapshot,
            ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
                self.0.apply_operation(operation, primary, snapshot)
            }
        }
        let snapshot = snapshot(b"fn f() {\n    let value = 1\n}\n");
        let (primary, operation) = target(&snapshot, "value");
        let planner = WrongEdit(CanonicalNameFixPlanner::new(&snapshot));
        assert_eq!(
            Fix::new(
                "unused",
                Applicability::MachineApplicable,
                operation,
                vec![],
                &primary,
                &snapshot,
                &planner
            ),
            Err(FixValidationError::OperationAndEditsDisagree)
        );
    }

    #[test]
    fn rejects_layout_change_in_candidate() {
        let mut found = false;
        for length in 1..120 {
            let name = format!("x{}", "x".repeat(length));
            let input = format!("fn f() {{\n    let {name} = 1\n}}\n");
            if sai_fmt::format_source(input.as_bytes()).ok().as_deref() != Some(&input) {
                continue;
            }
            let changed = input.replace(&name, &format!("_{name}"));
            if sai_fmt::format_source(changed.as_bytes()).ok().as_deref() == Some(&changed) {
                continue;
            }
            let original = snapshot(input.as_bytes());
            let (primary, operation) = target(&original, &name);
            assert!(
                CanonicalNameFixPlanner::new(&original)
                    .plan_edits(&operation, &primary, &original)
                    .is_err()
            );
            found = true;
            break;
        }
        assert!(found, "expected a formatter width boundary");
    }

    #[test]
    fn integrates_with_unused_binding_conversion() {
        use std::collections::HashSet;

        use crate::unused_binding_diagnostics::{
            LintContext, OwnerScope, UnusedBindingDiagnostics, UnusedBindingInputs,
            unused_binding_diagnostics,
        };
        use sai_diagnostics::{LintPolicies, LintResolutionContext};
        use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId};
        use sai_intern::{Interner, PackageInterner};
        use sai_resolve::unused_binding::ModuleAliases;
        use sai_syntax::ast::AstIdMap;

        let text = "fn sample() {\n    let value = 1\n}\n";
        let snapshot = snapshot(text.as_bytes());
        let file_id = FileId::new(1);
        let parsed = sai_syntax::parse(&snapshot, file_id).unwrap();
        assert!(parsed.errors.is_empty());
        let ast_ids = AstIdMap::new(file_id, &parsed.root).unwrap();
        let symbols = Interner::new();
        let tree =
            sai_hir::item_tree::ItemTree::collect(file_id, text, &parsed.root, &ast_ids, &symbols)
                .unwrap();
        let entry = &tree.items[0];
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(entry.name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let owner = OwnerId {
            def_id: paths.intern(path.clone()).unwrap(),
        };
        let arena = sai_hir::arena::HirArena::new();
        let lowering = sai_hir::lower::HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &paths,
        );
        let (lowered, source_map) =
            sai_hir::lower::lower_function_owner(owner, &path, &tree, entry.ast_id, &lowering)
                .unwrap();
        assert!(lowered.diagnostics.is_empty());
        let policies = LintPolicies::default();
        let lint = LintResolutionContext {
            primary: None,
            attributes: &[],
            cli: None,
            manifest: None,
            package_policy: &policies,
            root_policy: &policies,
            is_root_or_workspace_member: true,
            deny_warnings: false,
        };
        let aliases = HashSet::new();
        let planner = CanonicalNameFixPlanner::new(&snapshot);
        let result = unused_binding_diagnostics(UnusedBindingInputs {
            body: lowered.body.unwrap(),
            source_map: &source_map,
            snapshot: &snapshot,
            symbols: &symbols,
            module_aliases: ModuleAliases::Complete(&aliases),
            owner_scope: OwnerScope::Complete {
                lowering_diagnostics: &lowered.diagnostics,
            },
            lint: LintContext::Complete(&lint),
            planner: &planner,
        });
        let UnusedBindingDiagnostics::Complete(diagnostics) = result else {
            panic!("expected complete unused binding diagnostics");
        };
        assert_eq!(diagnostics.len(), 1);
        let fix = &diagnostics[0].fixes()[0];
        assert_eq!(fix.edits()[0].new_text, "_");
        let primary = diagnostics[0].primary().unwrap();
        assert_eq!(
            planner
                .apply_operation(fix.operation(), primary, &snapshot)
                .unwrap()[&file_id],
            text.replace("value", "_value").as_bytes()
        );
    }
}
