//! Conservative execution of clear-only index-operation journal decisions.
//!
//! Recovery planning is read-only. This module re-observes the workspace
//! immediately before acting and only clears a journal when the fresh planner
//! decision still matches the caller's earlier decision and authorizes a
//! clear. Rollback and index-commit decisions remain for later executor
//! slices.

use crate::index_operation_journal_recovery::{
    IndexJournalRecoveryDecision, plan_index_operation_journal_recovery,
};
use crate::index_operation_journal_store::{
    IndexOperationJournalStore, IndexOperationJournalStoreError,
};
use crate::index_operation_journal_validation::IndexJournalWorkspaceInventory;
use sai_semantic::IndexOperationJournal;

/// Result of executing a previously planned clear decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalClearExecution {
    /// The fresh observation produced a different planner decision.
    DecisionChanged {
        planned: IndexJournalRecoveryDecision,
        observed: IndexJournalRecoveryDecision,
    },
    /// The fresh planner decision requires an executor action not implemented
    /// by the clear-only recovery slice.
    NotClearable {
        decision: IndexJournalRecoveryDecision,
    },
    /// The exact journal was durably removed.
    Cleared {
        decision: IndexJournalRecoveryDecision,
    },
}

/// Re-observes and executes one clear-only recovery decision.
///
/// `planned_decision` must be the decision previously produced for `journal`
/// from an observation made under the same exclusive workspace lock. Before
/// clearing, this function obtains a fresh source and index observation,
/// recomputes the decision, and refuses to clear if it changed. Only
/// `ClearBefore`, `ClearRejected`, and `ClearCommitted` are executable here.
/// The other planner decisions are returned without mutation for later
/// executor slices.
///
/// The store's [`IndexOperationJournalStore::clear_if_current`] also verifies
/// that the durable record still equals `journal` while removing it and
/// flushes the containing directory. A failure or a journal race is returned
/// as a store error, preserving the record where possible. On any error, the
/// caller must read back the journal under the same exclusive lock and keep
/// recovery gated until a later observation resolves the state; an absent
/// journal after an error does not prove that recovery completed.
pub fn execute_index_operation_journal_clear(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    inventory: &IndexJournalWorkspaceInventory,
    planned_decision: IndexJournalRecoveryDecision,
) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
    execute_index_operation_journal_clear_with_hook(
        store,
        journal,
        inventory,
        planned_decision,
        || {},
    )
}

fn execute_index_operation_journal_clear_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    inventory: &IndexJournalWorkspaceInventory,
    planned_decision: IndexJournalRecoveryDecision,
    after_observation: impl FnOnce(),
) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
    let observation = store.observe_recovery(journal, inventory)?;
    let observed_decision = plan_index_operation_journal_recovery(journal, &observation);
    if observed_decision != planned_decision {
        return Ok(IndexJournalClearExecution::DecisionChanged {
            planned: planned_decision,
            observed: observed_decision,
        });
    }

    if !matches!(
        observed_decision,
        IndexJournalRecoveryDecision::ClearBefore
            | IndexJournalRecoveryDecision::ClearRejected
            | IndexJournalRecoveryDecision::ClearCommitted
    ) {
        return Ok(IndexJournalClearExecution::NotClearable {
            decision: observed_decision,
        });
    }

    after_observation();
    if store.clear_if_current(journal)? {
        Ok(IndexJournalClearExecution::Cleared {
            decision: observed_decision,
        })
    } else {
        // The journal disappeared after the fresh observation. Do not treat
        // that race as settled: the caller must read back current state and
        // keep recovery gated until it can classify the missing record.
        Err(IndexOperationJournalStoreError::JournalChanged)
    }
}

#[cfg(test)]
mod tests {
    use super::{IndexJournalClearExecution, execute_index_operation_journal_clear};
    use crate::index_operation_journal_recovery::{
        IndexJournalRecoveryAmbiguity, IndexJournalRecoveryDecision,
        plan_index_operation_journal_recovery,
    };
    use crate::index_operation_journal_store::{
        IndexOperationJournalStore, IndexOperationJournalStoreError,
    };
    use sai_package::local_source::LocalPackageDirectory;
    use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange,
    };
    use sai_stable_hash::hash_bytes;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct TestWorkspace(PathBuf);

    impl TestWorkspace {
        fn new() -> Self {
            let id = NEXT_TEST_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-journal-clear-executor-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary workspace");
            Self(path)
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn source_change() -> SourceFileChange {
        SourceFileChange {
            package_node_key: "package:acme:app@1.0.0".to_owned(),
            package_root: ".".to_owned(),
            path: "src/lib.sai".to_owned(),
            before: FileState::RegularFile(b"before".to_vec()),
            planned_after: FileState::RegularFile(b"after".to_vec()),
        }
    }

    fn journal(origin: IndexJournalOrigin, state: IndexJournalState) -> IndexOperationJournal {
        let file = PackageIndexFileName::parse("acme.app.1.jsonl").unwrap();
        let before_package = package_bytes(false);
        let after_package = package_bytes(true);
        let before_header = header(&file, &before_package);
        let after_header = header(&file, &after_package);
        IndexOperationJournal {
            operation_id: "5197cf1c-dbec-4b13-9077-1c2c1ce2d664".to_owned(),
            origin,
            state,
            sources: vec![source_change()],
            index_before: IndexSnapshot {
                header: FileState::RegularFile(before_header),
                package_files: vec![PackageFileState {
                    file: file.clone(),
                    state: FileState::RegularFile(before_package),
                }],
            },
            index_after: IndexSnapshot {
                header: FileState::RegularFile(after_header),
                package_files: vec![PackageFileState {
                    file,
                    state: FileState::RegularFile(after_package),
                }],
            },
            symbol_mappings: Vec::new(),
        }
    }

    fn package_bytes(after: bool) -> Vec<u8> {
        let fingerprint = if after { "b" } else { "a" }.repeat(32);
        format!(
            "{{\"hex\":\"000001\",\"def_path_hash\":\"{}\",\"path\":\"acme:app.sample\",\"kind\":\"fn\",\"fingerprint\":\"{fingerprint}\"}}\n",
            "0".repeat(32),
        )
        .into_bytes()
    }

    fn header(file: &PackageIndexFileName, package: &[u8]) -> Vec<u8> {
        format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{}\",\"blake3\":\"{}\"}}]}}\n",
            file.as_str(),
            hash_bytes(package).to_hex()
        )
        .into_bytes()
    }

    fn set_source_state(root: &PathBuf, state: &FileState) {
        let path = root.join("src/lib.sai");
        match state {
            FileState::Absent => {
                let _ = fs::remove_file(path);
            }
            FileState::RegularFile(bytes) => {
                fs::write(path, bytes).expect("write source state");
            }
        }
    }

    fn write_index_state(root: &PathBuf, journal: &IndexOperationJournal, after: bool) {
        let index_directory = root.join(".sai/index");
        fs::create_dir_all(&index_directory).expect("create index directory");
        let snapshot = if after {
            &journal.index_after
        } else {
            &journal.index_before
        };
        let FileState::RegularFile(header) = &snapshot.header else {
            panic!("test snapshot has a regular header");
        };
        fs::write(index_directory.join("index.json"), header).expect("write index header");
        for package_file in &snapshot.package_files {
            let FileState::RegularFile(bytes) = &package_file.state else {
                panic!("test package file has a regular state");
            };
            fs::write(index_directory.join(package_file.file.as_str()), bytes)
                .expect("write package index file");
        }
        fs::write(
            index_directory.join("operation.json"),
            journal.encode().expect("encode test journal"),
        )
        .expect("write operation journal");
    }

    fn setup(
        journal: &IndexOperationJournal,
        source_state: &FileState,
        index_after: bool,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        crate::index_operation_journal_validation::IndexJournalWorkspaceInventory,
    ) {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.0.join("src")).expect("create source directory");
        fs::write(
            workspace.0.join("sai.package"),
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        )
        .expect("write package manifest");
        let lock = WorkspaceLockGuard::acquire(&workspace.0, WorkspaceLockMode::Exclusive)
            .expect("acquire workspace lock");
        let root = LocalPackageDirectory::open(&workspace.0).expect("open root package");
        let parsed_lock = sai_package::lock::parse(
            b"lock 2\n\
              root acme:app @ 1.0.0\n\
                  targets { wasm32-core, wasm32-wasi }\n",
        );
        let inventory =
            crate::index_operation_journal_validation::build_index_journal_workspace_inventory(
                &root,
                &parsed_lock,
                sai_package::validation::PackageValidationContext::default(),
            )
            .expect("build workspace inventory");
        set_source_state(&workspace.0, source_state);
        write_index_state(&workspace.0, journal, index_after);
        (workspace, lock, inventory)
    }

    fn expected_decision(
        store: &IndexOperationJournalStore<'_>,
        journal: &IndexOperationJournal,
        inventory: &crate::index_operation_journal_validation::IndexJournalWorkspaceInventory,
    ) -> IndexJournalRecoveryDecision {
        let observation = store
            .observe_recovery(journal, inventory)
            .expect("observe recovery state");
        plan_index_operation_journal_recovery(journal, &observation)
    }

    fn journal_still_exists(workspace: &TestWorkspace) -> bool {
        workspace.0.join(".sai/index/operation.json").exists()
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn clear_before_rejected_and_committed_decisions_are_executed() {
        let cases = [
            (
                IndexJournalOrigin::Cli,
                IndexJournalState::Prepared,
                false,
                IndexJournalRecoveryDecision::ClearBefore,
            ),
            (
                IndexJournalOrigin::LspApplyEdit,
                IndexJournalState::ClientRejected,
                false,
                IndexJournalRecoveryDecision::ClearRejected,
            ),
            (
                IndexJournalOrigin::Cli,
                IndexJournalState::IndexCommitted,
                true,
                IndexJournalRecoveryDecision::ClearCommitted,
            ),
        ];
        for (origin, state, planned_index, expected) in cases {
            let journal = journal(origin, state);
            let source_state = if planned_index {
                &journal.sources[0].planned_after
            } else {
                &journal.sources[0].before
            };
            let (workspace, lock, inventory) = setup(&journal, source_state, planned_index);
            let store = IndexOperationJournalStore::new(&lock);
            assert_eq!(
                expected_decision(&store, &journal, &inventory),
                expected,
                "{origin:?} {state:?}"
            );

            let result =
                execute_index_operation_journal_clear(&store, &journal, &inventory, expected)
                    .expect("execute clear");
            assert_eq!(
                result,
                IndexJournalClearExecution::Cleared { decision: expected }
            );
            assert!(!journal_still_exists(&workspace));
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn recovery_decisions_outside_clear_only_slice_are_reported_without_mutation() {
        let cases = [
            (
                IndexJournalOrigin::Cli,
                IndexJournalState::Prepared,
                FileState::RegularFile(b"after".to_vec()),
                false,
                IndexJournalRecoveryDecision::RestoreBefore,
            ),
            (
                IndexJournalOrigin::LspApplyEdit,
                IndexJournalState::AwaitingClient,
                FileState::RegularFile(b"after".to_vec()),
                false,
                IndexJournalRecoveryDecision::CommitPlannedIndex,
            ),
            (
                IndexJournalOrigin::LspReturnedEdit,
                IndexJournalState::AwaitingClient,
                FileState::RegularFile(b"before".to_vec()),
                false,
                IndexJournalRecoveryDecision::PendingClient,
            ),
            (
                IndexJournalOrigin::Cli,
                IndexJournalState::Prepared,
                FileState::RegularFile(b"unexpected".to_vec()),
                false,
                IndexJournalRecoveryDecision::Ambiguous(
                    IndexJournalRecoveryAmbiguity::UnexpectedSourceState,
                ),
            ),
        ];
        for (origin, state, source_state, planned_index, expected) in cases {
            let journal = journal(origin, state);
            let (workspace, lock, inventory) = setup(&journal, &source_state, planned_index);
            let store = IndexOperationJournalStore::new(&lock);

            assert_eq!(
                expected_decision(&store, &journal, &inventory),
                expected,
                "{origin:?} {state:?}"
            );
            assert_eq!(
                execute_index_operation_journal_clear(&store, &journal, &inventory, expected,)
                    .expect("report non-clearable decision"),
                IndexJournalClearExecution::NotClearable { decision: expected }
            );
            assert!(journal_still_exists(&workspace));
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn second_observation_that_changes_the_decision_prevents_clear() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let initial = expected_decision(&store, &journal, &inventory);
        assert_eq!(initial, IndexJournalRecoveryDecision::ClearBefore);

        set_source_state(
            &workspace.0,
            &FileState::RegularFile(b"unexpected".to_vec()),
        );
        let result = execute_index_operation_journal_clear(&store, &journal, &inventory, initial)
            .expect("fresh decision is reported");

        assert_eq!(
            result,
            IndexJournalClearExecution::DecisionChanged {
                planned: IndexJournalRecoveryDecision::ClearBefore,
                observed: IndexJournalRecoveryDecision::Ambiguous(
                    IndexJournalRecoveryAmbiguity::UnexpectedSourceState
                ),
            }
        );
        assert!(journal_still_exists(&workspace));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn current_journal_race_is_rejected_after_fresh_observation() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let decision = expected_decision(&store, &journal, &inventory);
        assert_eq!(decision, IndexJournalRecoveryDecision::ClearBefore);
        let mut replacement = journal.clone();
        replacement.operation_id = "f8547899-0982-4d1f-85d6-9d3207deea14".to_owned();

        let error =
            execute_clear_only_with_hook_for_test(&store, &journal, &inventory, decision, || {
                fs::write(
                    workspace.0.join(".sai/index/operation.json"),
                    replacement.encode().expect("encode replacement"),
                )
                .expect("replace journal after observation");
            })
            .expect_err("clear must reject a changed current journal");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::JournalChanged
        ));
        assert!(journal_still_exists(&workspace));
        let current = store.read().expect("read current replacement journal");
        assert_eq!(current, Some(replacement));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn journal_removed_after_observation_is_unconfirmed_and_never_successful() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let decision = expected_decision(&store, &journal, &inventory);
        assert_eq!(decision, IndexJournalRecoveryDecision::ClearBefore);

        let error =
            execute_clear_only_with_hook_for_test(&store, &journal, &inventory, decision, || {
                fs::remove_file(workspace.0.join(".sai/index/operation.json"))
                    .expect("remove journal after observation");
            })
            .expect_err("a concurrent journal removal is not a successful recovery");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::JournalChanged
        ));
        assert!(!journal_still_exists(&workspace));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("source remains readable"),
            b"before"
        );
        let FileState::RegularFile(expected_header) = &journal.index_before.header else {
            panic!("before header is regular");
        };
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json"))
                .expect("index header remains readable"),
            *expected_header
        );
        let package_file = &journal.index_before.package_files[0];
        let FileState::RegularFile(expected_package) = &package_file.state else {
            panic!("before package file is regular");
        };
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(package_file.file.as_str())
            )
            .expect("package file remains readable"),
            *expected_package
        );
        assert_eq!(store.read().expect("read back missing journal"), None);
    }

    fn execute_clear_only_with_hook_for_test(
        store: &IndexOperationJournalStore<'_>,
        journal: &IndexOperationJournal,
        inventory: &crate::index_operation_journal_validation::IndexJournalWorkspaceInventory,
        planned_decision: IndexJournalRecoveryDecision,
        after_observation: impl FnOnce(),
    ) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
        super::execute_index_operation_journal_clear_with_hook(
            store,
            journal,
            inventory,
            planned_decision,
            after_observation,
        )
    }
}
