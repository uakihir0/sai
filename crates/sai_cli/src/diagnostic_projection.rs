//! Bounded snapshot-validated diagnostic views, independent of `sai check`.
//!
//! A view borrows and preserves the original diagnostic. It validates source
//! references without cloning, serializing JSON, or rendering human output.

use sai_diagnostics::{Diagnostic, FixValidationError, Location, Related, SourceRef};
use sai_vfs::{Position, WorkspaceSnapshot};

const MAX_ENTRIES: usize = 256;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_REFERENCE_BYTES: usize = 1024 * 1024;

pub enum SnapshotDiagnosticOutcome<'a> {
    Validated(SnapshotDiagnosticView<'a>),
    Incomplete(IncompleteReason),
    Rejected(FixValidationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompleteReason {
    UnboundDiagnostic,
    EntryLimit,
    ReferenceLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocationError {
    SourceMismatch,
    InvalidRange,
    /// The source is oversized, has a leading BOM, or contains malformed UTF-8.
    UnsupportedSource,
    /// The path and package references exceed the bounded comparison size.
    UnsupportedReference,
}

pub struct SnapshotDiagnosticView<'a> {
    diagnostic: &'a Diagnostic,
    snapshot: &'a WorkspaceSnapshot,
    primary_position: Option<Result<(Position, Position), LocationError>>,
}

impl SnapshotDiagnosticView<'_> {
    /// Returns the original diagnostic, including fields this view does not
    /// transform such as `data`.
    pub fn diagnostic(&self) -> &Diagnostic {
        self.diagnostic
    }

    /// Returns the cached primary position when its source is supported.
    ///
    /// A `Some(Err(..))` means the diagnostic was validated against the
    /// snapshot, but its primary source could not be mapped by this bounded
    /// view.
    pub fn primary_position(&self) -> Option<Result<(Position, Position), LocationError>> {
        self.primary_position
    }

    /// Maps a location when its file reference and range belong to the
    /// validated snapshot. Position mapping supports valid UTF-8 sources
    /// without a BOM that are at most 1 MiB. Path and package references are
    /// limited to 1 MiB for this lookup.
    pub fn location(&self, location: &Location) -> Result<(Position, Position), LocationError> {
        primary_position(self.snapshot, location)
    }
}

fn primary_position(
    snapshot: &WorkspaceSnapshot,
    location: &Location,
) -> Result<(Position, Position), LocationError> {
    if location_reference_bytes(location) > MAX_REFERENCE_BYTES {
        return Err(LocationError::UnsupportedReference);
    }
    let file = snapshot
        .file(location.span.file)
        .ok_or(LocationError::SourceMismatch)?;
    if !file.matches_reference(
        &location.file.path,
        location.file.package.as_deref(),
        location.file.snapshot,
    ) {
        return Err(LocationError::SourceMismatch);
    }
    let source = file.source();
    let bytes = source.raw_bytes();
    if bytes.len() > MAX_SOURCE_BYTES || source.had_bom() || std::str::from_utf8(bytes).is_err() {
        return Err(LocationError::UnsupportedSource);
    }
    let range = location.span.range;
    if range.end().to_usize() > bytes.len()
        || !source.is_original_char_boundary(range.start())
        || !source.is_original_char_boundary(range.end())
        || !source.is_original_crlf_boundary(range.start())
        || !source.is_original_crlf_boundary(range.end())
    {
        return Err(LocationError::InvalidRange);
    }
    Ok((
        source.line_index().position(range.start()),
        source.line_index().position(range.end()),
    ))
}

fn add_reference_bytes(total: &mut usize, location: &Location) -> bool {
    *total = total.saturating_add(location_reference_bytes(location));
    *total <= MAX_REFERENCE_BYTES
}

fn location_reference_bytes(location: &Location) -> usize {
    source_reference_bytes(&location.file)
}

fn source_reference_bytes(reference: &SourceRef) -> usize {
    reference
        .path
        .len()
        .saturating_add(reference.package.as_ref().map_or(0, String::len))
}

fn within_reference_limit(diagnostic: &Diagnostic) -> bool {
    let mut total = 0;
    macro_rules! include_location {
        ($location:expr) => {
            if let Some(location) = $location {
                if !add_reference_bytes(&mut total, location) {
                    return false;
                }
            }
        };
    }

    include_location!(diagnostic.primary());
    if let Some(level) = diagnostic.level() {
        include_location!(level.location.as_ref());
    }
    for label in diagnostic.labels() {
        include_location!(Some(&label.location));
    }
    for child in diagnostic.children() {
        include_location!(child.location.as_ref());
    }
    for related in diagnostic.related() {
        match related {
            Related::EffectChain { steps, .. } | Related::RequirementChain { steps } => {
                for step in steps {
                    include_location!(step.location.as_ref());
                }
            }
            Related::CapabilityChain { steps, policy, .. } => {
                for step in steps {
                    include_location!(step.location.as_ref());
                }
                include_location!(policy.as_ref());
            }
            Related::BorrowConflict {
                loan,
                access,
                later_use,
                ..
            } => {
                include_location!(Some(loan));
                include_location!(Some(access));
                include_location!(later_use.as_ref());
            }
            Related::UseAfterMove {
                moved_at, used_at, ..
            } => {
                include_location!(Some(moved_at));
                include_location!(Some(used_at));
            }
            Related::DeclaredHere { location, .. } => {
                include_location!(Some(location));
            }
        }
    }
    for fix in diagnostic.fixes() {
        for edit in fix.edits() {
            total = total.saturating_add(source_reference_bytes(&edit.file));
            if total > MAX_REFERENCE_BYTES {
                return false;
            }
        }
    }
    true
}

/// Validates a snapshot-bound diagnostic and returns a borrowed location view.
///
/// Entry count and aggregate source-reference byte limits bound the diagnostic
/// data examined during snapshot validation. This function never clones the
/// diagnostic, so it preserves arbitrary diagnostic data without claiming to
/// serialize or render it.
pub fn validate_snapshot<'a>(
    diagnostic: &'a Diagnostic,
    snapshot: &'a WorkspaceSnapshot,
) -> SnapshotDiagnosticOutcome<'a> {
    if !diagnostic.has_workspace_snapshot() {
        return SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::UnboundDiagnostic);
    }
    let mut entries = diagnostic
        .labels()
        .len()
        .saturating_add(diagnostic.children().len())
        .saturating_add(diagnostic.related().len())
        .saturating_add(diagnostic.fixes().len())
        .saturating_add(diagnostic.cause().facts().len());
    if entries > MAX_ENTRIES {
        return SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit);
    }
    for related in diagnostic.related() {
        let steps = match related {
            Related::EffectChain { steps, .. }
            | Related::CapabilityChain { steps, .. }
            | Related::RequirementChain { steps } => steps.len(),
            _ => 0,
        };
        entries = entries.saturating_add(steps);
    }
    for fix in diagnostic.fixes() {
        entries = entries
            .saturating_add(fix.edits().len())
            .saturating_add(fix.placeholders().len())
            .saturating_add(fix.operation().args().len());
    }
    if entries > MAX_ENTRIES {
        return SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit);
    }
    if !within_reference_limit(diagnostic) {
        return SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::ReferenceLimit);
    }
    match diagnostic.validate_workspace_snapshot(snapshot) {
        Ok(()) => SnapshotDiagnosticOutcome::Validated(SnapshotDiagnosticView {
            diagnostic,
            snapshot,
            primary_position: diagnostic
                .primary()
                .map(|location| primary_position(snapshot, location)),
        }),
        Err(error) => SnapshotDiagnosticOutcome::Rejected(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_diagnostics::{
        Applicability, CauseKey, ChainStep, Child, ChildKind, Code, Fix, FixOperation,
        FixOperationName, FixOperationPlanner, Label, LevelInfo, LevelSource, LintLevel, NodeGroup,
        NodeId, OperationPlanError, Placeholder, PoisonFact, Relation, Severity, SourceRef,
        TextEdit, Value,
    };
    use sai_vfs::{FileId, SnapshotFile, SourceFile, Span, TextRange, TextSize};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn snapshot_with_bytes(bytes: &[u8]) -> WorkspaceSnapshot {
        WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(FileId::new(1), bytes).unwrap()),
            "src/lib.sai",
            None,
            Some(7),
        )])
        .unwrap()
    }

    fn snapshot(bytes: &[u8]) -> WorkspaceSnapshot {
        snapshot_with_bytes(bytes)
    }

    fn location_for(
        file_id: FileId,
        path: &str,
        package: Option<&str>,
        version: Option<u64>,
        start: u32,
        end: u32,
    ) -> Location {
        Location {
            file: SourceRef {
                path: path.into(),
                package: package.map(str::to_owned),
                snapshot: version,
            },
            span: Span::new(
                file_id,
                TextRange::new(TextSize::new(start), TextSize::new(end)).unwrap(),
            ),
        }
    }

    fn location(start: u32, end: u32) -> Location {
        location_for(FileId::new(1), "src/lib.sai", None, Some(7), start, end)
    }

    struct Planner;

    impl FixOperationPlanner for Planner {
        fn plan_edits(
            &self,
            _: &FixOperation,
            primary: &Location,
            _: &WorkspaceSnapshot,
        ) -> Result<Vec<TextEdit>, OperationPlanError> {
            Ok(vec![TextEdit {
                file: primary.file.clone(),
                range: primary.span,
                new_text: "z".into(),
            }])
        }

        fn apply_operation(
            &self,
            _: &FixOperation,
            _: &Location,
            _: &WorkspaceSnapshot,
        ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
            Ok(BTreeMap::from([(FileId::new(1), b"z\r\nb".to_vec())]))
        }
    }

    #[test]
    fn retains_structured_values_and_projects_ascii_positions() {
        let snapshot = snapshot(b"a\r\nb");
        let mut diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Warning,
            "message",
            Some(location(0, 1)),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        diagnostic
            .set_level(Some(LevelInfo {
                value: LintLevel::Warn,
                source: LevelSource::Manifest,
                location: Some(location(3, 4)),
            }))
            .unwrap();
        diagnostic
            .push_label(Label {
                location: location(3, 4),
                message: "label".into(),
            })
            .unwrap();
        diagnostic
            .push_related(Related::DeclaredHere {
                location: location(3, 4),
                what: "declaration".into(),
            })
            .unwrap();
        let primary = location(0, 1);
        let operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            NodeId::new(NodeGroup::Name, 1),
            BTreeMap::from([("text".into(), Value::String("z".into()))]),
        )
        .unwrap();
        diagnostic
            .push_fix(
                Fix::new(
                    "replace",
                    Applicability::MachineApplicable,
                    operation,
                    vec![],
                    &primary,
                    &snapshot,
                    &Planner,
                )
                .unwrap(),
            )
            .unwrap();
        let expected = diagnostic.clone();
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &snapshot)
        else {
            panic!("expected partial view")
        };
        assert_eq!(view.diagnostic(), &expected);
        let (start, end) = view.location(&location(3, 4)).unwrap();
        assert_eq!(
            (start.byte, start.line, start.col, start.utf16_col),
            (3, 2, 1, 1)
        );
        assert_eq!(end.byte, 4);
        assert_eq!(
            view.location(&location(2, 2)),
            Err(LocationError::InvalidRange)
        );
        let mut wrong = location(0, 1);
        wrong.file.snapshot = Some(8);
        assert_eq!(view.location(&wrong), Err(LocationError::SourceMismatch));
        wrong = location(0, 1);
        wrong.span.file = FileId::new(2);
        assert_eq!(view.location(&wrong), Err(LocationError::SourceMismatch));
        assert_eq!(
            view.location(&location(0, 5)),
            Err(LocationError::InvalidRange)
        );
        let mut wrong = location(0, 1);
        wrong.file.path = "src/other.sai".into();
        assert_eq!(view.location(&wrong), Err(LocationError::SourceMismatch));
    }

    #[test]
    fn rejects_stale_and_unbound_and_maps_valid_unicode_positions() {
        let first = snapshot("é".as_bytes());
        let second = snapshot("é".as_bytes());
        let diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(0, 2)),
        )
        .with_workspace_snapshot(&first)
        .unwrap();
        assert!(matches!(
            validate_snapshot(&diagnostic, &second),
            SnapshotDiagnosticOutcome::Rejected(FixValidationError::SnapshotMismatch)
        ));
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &first)
        else {
            panic!("expected partial view")
        };
        assert_eq!(
            view.location(&location(0, 2)),
            Ok((
                Position {
                    byte: 0,
                    line: 1,
                    col: 1,
                    utf16_col: 1,
                },
                Position {
                    byte: 2,
                    line: 1,
                    col: 3,
                    utf16_col: 2,
                },
            ))
        );
        for bytes in [&b"\xef\xbb\xbfa"[..], &b"\xff"[..]] {
            let source = snapshot(bytes);
            let diagnostic = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
                .with_workspace_snapshot(&source)
                .unwrap();
            let SnapshotDiagnosticOutcome::Validated(view) =
                validate_snapshot(&diagnostic, &source)
            else {
                panic!("expected partial view")
            };
            assert_eq!(
                view.location(&location(0, 0)),
                Err(LocationError::UnsupportedSource)
            );
        }

        let unicode = snapshot("é🙂\r\nz".as_bytes());
        let diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(6, 6)),
        )
        .with_workspace_snapshot(&unicode)
        .unwrap();
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &unicode)
        else {
            panic!("valid UTF-8 should be supported")
        };
        assert_eq!(
            view.primary_position(),
            Some(Ok((
                Position {
                    byte: 6,
                    line: 1,
                    col: 7,
                    utf16_col: 4,
                },
                Position {
                    byte: 6,
                    line: 1,
                    col: 7,
                    utf16_col: 4,
                },
            )))
        );
        assert_eq!(
            view.location(&location(8, 9)),
            Ok((
                Position {
                    byte: 8,
                    line: 2,
                    col: 1,
                    utf16_col: 1,
                },
                Position {
                    byte: 9,
                    line: 2,
                    col: 2,
                    utf16_col: 2,
                },
            ))
        );
        assert_eq!(
            view.location(&location(1, 1)),
            Err(LocationError::InvalidRange),
            "a location must not split a UTF-8 scalar"
        );

        let unbound = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None);
        assert!(matches!(
            validate_snapshot(&unbound, &first),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::UnboundDiagnostic)
        ));
    }

    #[test]
    fn preserves_arbitrary_data_and_enforces_entry_limit() {
        let snapshot = snapshot(b"a");
        let mut diagnostic = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
            .with_workspace_snapshot(&snapshot)
            .unwrap();
        let data = Value::Object(BTreeMap::from([(
            "nested".into(),
            Value::Array(vec![Value::String("retained".into()), Value::Null]),
        )]));
        diagnostic.data.insert("key".into(), data.clone());
        let mut deep = Value::Null;
        for _ in 0..129 {
            deep = Value::Array(vec![deep]);
        }
        diagnostic.data.insert("deep".into(), deep);
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &snapshot)
        else {
            panic!("arbitrary diagnostic data should remain available")
        };
        assert_eq!(view.diagnostic().data.get("key"), Some(&data));
        assert!(
            view.diagnostic().data.contains_key("deep"),
            "data deeper than the diagnostic-template limit must remain unvisited"
        );

        let mut diagnostic = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
            .with_workspace_snapshot(&snapshot)
            .unwrap();
        for _ in 0..MAX_ENTRIES {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "note".into(),
                    location: None,
                })
                .unwrap();
        }
        assert!(matches!(
            validate_snapshot(&diagnostic, &snapshot),
            SnapshotDiagnosticOutcome::Validated(_)
        ));
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "note".into(),
                location: None,
            })
            .unwrap();
        assert!(matches!(
            validate_snapshot(&diagnostic, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit)
        ));

        let exact_size_snapshot = snapshot_with_bytes(&vec![b'a'; MAX_SOURCE_BYTES]);
        let exact_size_diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(0, 0)),
        )
        .with_workspace_snapshot(&exact_size_snapshot)
        .unwrap();
        let SnapshotDiagnosticOutcome::Validated(exact_size_view) =
            validate_snapshot(&exact_size_diagnostic, &exact_size_snapshot)
        else {
            panic!("exactly 1 MiB source should be supported")
        };
        assert!(exact_size_view.primary_position().unwrap().is_ok());

        let oversized_snapshot = snapshot_with_bytes(&vec![b'a'; MAX_SOURCE_BYTES + 1]);
        let oversized = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(0, 0)),
        )
        .with_workspace_snapshot(&oversized_snapshot)
        .unwrap();
        let SnapshotDiagnosticOutcome::Validated(view) =
            validate_snapshot(&oversized, &oversized_snapshot)
        else {
            panic!("oversized source still allows snapshot validation")
        };
        assert_eq!(
            view.primary_position(),
            Some(Err(LocationError::UnsupportedSource))
        );
    }

    #[test]
    fn enforces_aggregate_source_reference_byte_limit() {
        let path = format!("src/{}", "a".repeat(MAX_REFERENCE_BYTES / 2 - "src/".len()));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::decode(FileId::new(1), b"a").unwrap()),
            path.clone(),
            None,
            Some(7),
        )])
        .unwrap();
        let primary = location_for(FileId::new(1), &path, None, Some(7), 0, 1);
        let mut diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(primary.clone()),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        diagnostic
            .push_label(Label {
                location: primary.clone(),
                message: "at aggregate boundary".into(),
            })
            .unwrap();
        assert!(matches!(
            validate_snapshot(&diagnostic, &snapshot),
            SnapshotDiagnosticOutcome::Validated(_)
        ));
        diagnostic
            .push_label(Label {
                location: primary,
                message: "over aggregate boundary".into(),
            })
            .unwrap();
        assert!(matches!(
            validate_snapshot(&diagnostic, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::ReferenceLimit)
        ));
    }

    #[test]
    fn bounds_caller_supplied_location_reference_comparisons() {
        let snapshot = snapshot(b"a");
        let diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(0, 1)),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &snapshot)
        else {
            panic!("expected a validated view")
        };
        let oversized = location_for(
            FileId::new(1),
            &format!("src/{}", "a".repeat(MAX_REFERENCE_BYTES)),
            None,
            Some(7),
            0,
            1,
        );
        assert_eq!(
            view.location(&oversized),
            Err(LocationError::UnsupportedReference)
        );
    }

    #[test]
    fn includes_fix_edit_references_in_aggregate_limit() {
        struct CrossFilePlanner {
            file: SourceRef,
        }

        impl FixOperationPlanner for CrossFilePlanner {
            fn plan_edits(
                &self,
                _: &FixOperation,
                _: &Location,
                _: &WorkspaceSnapshot,
            ) -> Result<Vec<TextEdit>, OperationPlanError> {
                Ok(vec![TextEdit {
                    file: self.file.clone(),
                    range: Span::new(
                        FileId::new(2),
                        TextRange::new(TextSize::new(0), TextSize::new(1)).unwrap(),
                    ),
                    new_text: "z".into(),
                }])
            }

            fn apply_operation(
                &self,
                _: &FixOperation,
                _: &Location,
                _: &WorkspaceSnapshot,
            ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
                Ok(BTreeMap::from([(FileId::new(2), b"z".to_vec())]))
            }
        }

        let path = format!("src/{}", "a".repeat(MAX_REFERENCE_BYTES));
        let edit_file = SourceRef {
            path,
            package: None,
            snapshot: Some(7),
        };
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(FileId::new(1), b"a").unwrap()),
                "src/lib.sai",
                None,
                Some(7),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(FileId::new(2), b"a").unwrap()),
                edit_file.path.clone(),
                None,
                Some(7),
            ),
        ])
        .unwrap();
        let primary = location(0, 1);
        let operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            NodeId::new(NodeGroup::Name, 1),
            BTreeMap::from([("text".into(), Value::String("z".into()))]),
        )
        .unwrap();
        let fix = Fix::new(
            "replace",
            Applicability::MachineApplicable,
            operation,
            vec![],
            &primary,
            &snapshot,
            &CrossFilePlanner { file: edit_file },
        )
        .unwrap();
        let diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(primary),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap()
        .with_fix(fix)
        .unwrap();
        assert!(matches!(
            validate_snapshot(&diagnostic, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::ReferenceLimit)
        ));
    }

    #[test]
    fn validates_locations_in_every_snapshot_file() {
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::decode(FileId::new(1), b"a").unwrap()),
                "src/lib.sai",
                None,
                Some(7),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::decode(FileId::new(2), "é🙂".as_bytes()).unwrap()),
                "src/dep.sai",
                Some("sai:dep@1.0.0".into()),
                Some(9),
            ),
        ])
        .unwrap();
        let diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(location(0, 1)),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        let SnapshotDiagnosticOutcome::Validated(view) = validate_snapshot(&diagnostic, &snapshot)
        else {
            panic!("expected a validated view")
        };
        let dependency_location = location_for(
            FileId::new(2),
            "src/dep.sai",
            Some("sai:dep@1.0.0"),
            Some(9),
            0,
            6,
        );
        assert_eq!(
            view.location(&dependency_location),
            Ok((
                Position {
                    byte: 0,
                    line: 1,
                    col: 1,
                    utf16_col: 1,
                },
                Position {
                    byte: 6,
                    line: 1,
                    col: 7,
                    utf16_col: 4,
                },
            ))
        );
        for invalid in [
            location_for(
                FileId::new(2),
                "src/other.sai",
                Some("sai:dep@1.0.0"),
                Some(9),
                0,
                1,
            ),
            location_for(
                FileId::new(2),
                "src/dep.sai",
                Some("sai:other@1.0.0"),
                Some(9),
                0,
                1,
            ),
            location_for(
                FileId::new(2),
                "src/dep.sai",
                Some("sai:dep@1.0.0"),
                Some(8),
                0,
                1,
            ),
        ] {
            assert_eq!(view.location(&invalid), Err(LocationError::SourceMismatch));
        }
    }

    #[test]
    fn bounds_related_chain_and_fix_validation_work() {
        let snapshot = snapshot(b"a\r\nb");
        let mut exact_related =
            Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
                .with_workspace_snapshot(&snapshot)
                .unwrap();
        exact_related
            .push_related(Related::RequirementChain {
                steps: (0..MAX_ENTRIES - 1)
                    .map(|index| ChainStep {
                        name: format!("step-{index}"),
                        symbol: None,
                        location: None,
                        relation: Relation::Requires,
                    })
                    .collect(),
            })
            .unwrap();
        assert!(matches!(
            validate_snapshot(&exact_related, &snapshot),
            SnapshotDiagnosticOutcome::Validated(_)
        ));
        let mut over_related =
            Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
                .with_workspace_snapshot(&snapshot)
                .unwrap();
        over_related
            .push_related(Related::RequirementChain {
                steps: (0..MAX_ENTRIES)
                    .map(|index| ChainStep {
                        name: format!("step-{index}"),
                        symbol: None,
                        location: None,
                        relation: Relation::Requires,
                    })
                    .collect(),
            })
            .unwrap();
        assert!(matches!(
            validate_snapshot(&over_related, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit)
        ));

        let cause = |count| {
            CauseKey::new((0..count).map(|index| PoisonFact::ErrorType(format!("cause-{index}"))))
        };
        let exact_cause = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
            .with_workspace_snapshot(&snapshot)
            .unwrap()
            .with_cause(cause(MAX_ENTRIES))
            .unwrap();
        assert!(matches!(
            validate_snapshot(&exact_cause, &snapshot),
            SnapshotDiagnosticOutcome::Validated(_)
        ));
        let over_cause = Diagnostic::new(Code::ExpectedToken, Severity::Error, "message", None)
            .with_workspace_snapshot(&snapshot)
            .unwrap()
            .with_cause(cause(MAX_ENTRIES + 1))
            .unwrap();
        assert!(matches!(
            validate_snapshot(&over_cause, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit)
        ));

        let primary = location(0, 1);
        let operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            NodeId::new(NodeGroup::Name, 1),
            BTreeMap::from([("text".into(), Value::String("z".into()))]),
        )
        .unwrap();
        let placeholders = |count| {
            (0..count)
                .map(|_| Placeholder {
                    edit: 0,
                    start: 0,
                    end: 1,
                    label: "name".into(),
                })
                .collect()
        };
        let mut exact_fix = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(primary.clone()),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        exact_fix
            .push_fix(
                Fix::new(
                    "replace",
                    Applicability::HasPlaceholders,
                    operation.clone(),
                    placeholders(MAX_ENTRIES - 3),
                    &primary,
                    &snapshot,
                    &Planner,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            validate_snapshot(&exact_fix, &snapshot),
            SnapshotDiagnosticOutcome::Validated(_)
        ));
        let mut over_fix = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "message",
            Some(primary.clone()),
        )
        .with_workspace_snapshot(&snapshot)
        .unwrap();
        over_fix
            .push_fix(
                Fix::new(
                    "replace",
                    Applicability::HasPlaceholders,
                    operation,
                    placeholders(MAX_ENTRIES - 2),
                    &primary,
                    &snapshot,
                    &Planner,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            validate_snapshot(&over_fix, &snapshot),
            SnapshotDiagnosticOutcome::Incomplete(IncompleteReason::EntryLimit)
        ));
    }
}
