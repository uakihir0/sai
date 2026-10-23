//! Conservative execution of index-operation journal recovery decisions.
//!
//! Recovery planning is read-only. This module re-observes the workspace
//! immediately before acting. It can clear confirmed terminal decisions or
//! restore one source file from a `RestoreBefore` plan. Multi-file rollback,
//! index commit, journal transitions, and final clearing remain later slices.

use crate::index_operation_journal_recovery::{
    IndexJournalRecoveryDecision, ObservedFileState as PlannerObservedFileState,
    plan_index_operation_journal_recovery,
};
use crate::index_operation_journal_store::{
    IndexOperationJournalStore, IndexOperationJournalStoreError,
};
use crate::index_operation_journal_validation::IndexJournalWorkspaceInventory;
use sai_package::local_source::LocalPackageSourceFileUpdateError;
use sai_package::package_path::{ContentPackagePath, RepositoryPackageDirectory};
use sai_semantic::{FileState, IndexOperationJournal};
use sai_windows_fs::ConditionalFileUpdate;
use std::error::Error;
use std::fmt;

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

/// Result of restoring one source file from a journal's before snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalSourceRestoreExecution {
    /// The fresh observation produced a different planner decision.
    DecisionChanged {
        planned: IndexJournalRecoveryDecision,
        observed: IndexJournalRecoveryDecision,
    },
    /// The current planner decision requires another recovery action.
    NotRestorable {
        decision: IndexJournalRecoveryDecision,
    },
    /// The requested journal source index does not exist.
    InvalidSourceIndex { source_index: usize },
    /// The selected source already has its before state.
    AlreadyBefore { source_index: usize },
    /// The selected source was conditionally restored to its before state.
    Restored { source_index: usize },
}

/// Failure while resolving or conditionally restoring one journal source.
#[derive(Debug)]
pub enum IndexJournalSourceRestoreError {
    Store(IndexOperationJournalStoreError),
    SourceUpdate(LocalPackageSourceFileUpdateError),
    MissingPackageContext { package_node_key: String },
    MissingSourceDirectory { package_node_key: String },
    InvalidPackageRoot,
    InvalidSourcePath,
}

impl fmt::Display for IndexJournalSourceRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "index journal source restoration failed: {self:?}"
        )
    }
}

impl Error for IndexJournalSourceRestoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(source) => Some(source),
            Self::SourceUpdate(source) => Some(source),
            Self::MissingPackageContext { .. }
            | Self::MissingSourceDirectory { .. }
            | Self::InvalidPackageRoot
            | Self::InvalidSourcePath => None,
        }
    }
}

impl From<IndexOperationJournalStoreError> for IndexJournalSourceRestoreError {
    fn from(error: IndexOperationJournalStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<LocalPackageSourceFileUpdateError> for IndexJournalSourceRestoreError {
    fn from(error: LocalPackageSourceFileUpdateError) -> Self {
        Self::SourceUpdate(error)
    }
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

/// Re-observes and restores at most one source file for a `RestoreBefore`
/// decision.
///
/// `planned_decision` must have been produced for `journal` under the same
/// exclusive workspace lock. The fresh observation and planner must still
/// authorize `RestoreBefore`. A selected source already at its before state is
/// an idempotent no-op; a source at its planned-after state is conditionally
/// changed to the before snapshot. Any unexpected or unavailable source makes
/// the fresh planner refuse this executor.
///
/// The current manifest context supplies the source root, while the package
/// root is reopened from the retained workspace-root handle. The journal is
/// verified again immediately before the conditional source update. This call
/// never changes the index generation, advances or clears the journal, or
/// restores another source; the caller must retain the journal and re-plan
/// before every subsequent recovery action.
pub fn execute_index_operation_journal_restore_source_before(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    inventory: &IndexJournalWorkspaceInventory,
    planned_decision: IndexJournalRecoveryDecision,
    source_index: usize,
) -> Result<IndexJournalSourceRestoreExecution, IndexJournalSourceRestoreError> {
    execute_index_operation_journal_restore_source_before_with_hook(
        store,
        journal,
        inventory,
        planned_decision,
        source_index,
        || {},
    )
}

fn execute_index_operation_journal_restore_source_before_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    inventory: &IndexJournalWorkspaceInventory,
    planned_decision: IndexJournalRecoveryDecision,
    source_index: usize,
    before_update: impl FnOnce(),
) -> Result<IndexJournalSourceRestoreExecution, IndexJournalSourceRestoreError> {
    let observation = store.observe_recovery(journal, inventory)?;
    let observed_decision = plan_index_operation_journal_recovery(journal, &observation);
    if observed_decision != planned_decision {
        return Ok(IndexJournalSourceRestoreExecution::DecisionChanged {
            planned: planned_decision,
            observed: observed_decision,
        });
    }
    if observed_decision != IndexJournalRecoveryDecision::RestoreBefore {
        return Ok(IndexJournalSourceRestoreExecution::NotRestorable {
            decision: observed_decision,
        });
    }

    let Some(source) = journal.sources.get(source_index) else {
        return Ok(IndexJournalSourceRestoreExecution::InvalidSourceIndex { source_index });
    };
    match observation.sources.get(source_index) {
        Some(PlannerObservedFileState::Available(state)) if state == &source.before => {
            return Ok(IndexJournalSourceRestoreExecution::AlreadyBefore { source_index });
        }
        Some(PlannerObservedFileState::Available(state)) if state == &source.planned_after => {}
        _ => unreachable!("RestoreBefore requires every source to match a recorded state"),
    }

    let context = inventory
        .contexts()
        .iter()
        .find(|context| context.package_node_key() == source.package_node_key)
        .ok_or_else(|| IndexJournalSourceRestoreError::MissingPackageContext {
            package_node_key: source.package_node_key.clone(),
        })?;
    let source_directory = context.source_directory().ok_or_else(|| {
        IndexJournalSourceRestoreError::MissingSourceDirectory {
            package_node_key: source.package_node_key.clone(),
        }
    })?;
    let source_directory = ContentPackagePath::parse(source_directory.as_str())
        .map_err(|_| IndexJournalSourceRestoreError::InvalidSourcePath)?;
    let package_root = RepositoryPackageDirectory::parse(&source.package_root)
        .map_err(|_| IndexJournalSourceRestoreError::InvalidPackageRoot)?;
    let package_directory = inventory
        .directory_for_package(&source.package_node_key)
        .ok_or_else(|| IndexJournalSourceRestoreError::MissingPackageContext {
            package_node_key: source.package_node_key.clone(),
        })?;
    let package_directory = package_directory
        .reopen_from_workspace_root(inventory.root_directory(), &package_root)
        .map_err(|error| {
            IndexJournalSourceRestoreError::SourceUpdate(
                LocalPackageSourceFileUpdateError::Directory(error),
            )
        })?;
    let source_path = ContentPackagePath::parse(&source.path)
        .map_err(|_| IndexJournalSourceRestoreError::InvalidSourcePath)?;

    before_update();
    store.verify_current(journal)?;
    let update = package_directory.update_source_file_if_matches(
        &source_directory,
        &source_path,
        file_state_bytes(&source.planned_after),
        file_state_bytes(&source.before),
    )?;
    Ok(match update {
        ConditionalFileUpdate::Updated => {
            IndexJournalSourceRestoreExecution::Restored { source_index }
        }
        ConditionalFileUpdate::AlreadyDesired => {
            IndexJournalSourceRestoreExecution::AlreadyBefore { source_index }
        }
    })
}

fn file_state_bytes(state: &FileState) -> Option<&[u8]> {
    match state {
        FileState::Absent => None,
        FileState::RegularFile(bytes) => Some(bytes),
    }
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
    use super::{
        IndexJournalClearExecution, IndexJournalSourceRestoreError,
        IndexJournalSourceRestoreExecution, execute_index_operation_journal_clear,
        execute_index_operation_journal_restore_source_before,
    };
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
        set_source_path_state(root, "src/lib.sai", state);
    }

    fn set_source_path_state(root: &PathBuf, source_path: &str, state: &FileState) {
        let path = root.join(source_path);
        match state {
            FileState::Absent => {
                let _ = fs::remove_file(path);
            }
            FileState::RegularFile(bytes) => {
                fs::write(path, bytes).expect("write source state");
            }
        }
    }

    fn two_source_journal() -> IndexOperationJournal {
        let mut journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let mut second = source_change();
        second.path = "src/other.sai".to_owned();
        second.before = FileState::RegularFile(b"before other".to_vec());
        second.planned_after = FileState::RegularFile(b"after other".to_vec());
        journal.sources.push(second);
        journal.encode().expect("encode two-source journal");
        journal
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

    fn setup_source_states(
        journal: &IndexOperationJournal,
        source_states: &[FileState],
        index_after: bool,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        crate::index_operation_journal_validation::IndexJournalWorkspaceInventory,
    ) {
        assert_eq!(source_states.len(), journal.sources.len());
        let (workspace, lock, inventory) = setup(journal, &source_states[0], index_after);
        for (source, state) in journal.sources.iter().zip(source_states) {
            set_source_path_state(&workspace.0, &source.path, state);
        }
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

    fn index_files(workspace: &TestWorkspace) -> (Vec<u8>, Vec<u8>) {
        let index_directory = workspace.0.join(".sai/index");
        (
            fs::read(index_directory.join("index.json")).expect("read index header"),
            fs::read(index_directory.join("acme.app.1.jsonl")).expect("read package index"),
        )
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
    fn restore_before_changes_only_the_selected_source_and_keeps_the_journal() {
        let journal = two_source_journal();
        let source_states = [
            journal.sources[0].before.clone(),
            journal.sources[1].planned_after.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let index_before = index_files(&workspace);

        let result = execute_index_operation_journal_restore_source_before(
            &store, &journal, &inventory, planned, 1,
        )
        .expect("restore selected source");

        assert_eq!(
            result,
            IndexJournalSourceRestoreExecution::Restored { source_index: 1 }
        );
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read first source"),
            b"before"
        );
        assert_eq!(
            fs::read(workspace.0.join("src/other.sai")).expect("read restored source"),
            b"before other"
        );
        assert_eq!(index_files(&workspace), index_before);
        assert!(journal_still_exists(&workspace));
        assert_eq!(
            store.read().expect("read unchanged journal"),
            Some(journal.clone())
        );
        assert_eq!(
            expected_decision(&store, &journal, &inventory),
            IndexJournalRecoveryDecision::ClearBefore,
            "one source step must leave the next decision to a later planner pass"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn already_before_source_is_a_noop_while_other_after_source_remains() {
        let journal = two_source_journal();
        let source_states = [
            journal.sources[0].before.clone(),
            journal.sources[1].planned_after.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);

        let result = execute_index_operation_journal_restore_source_before(
            &store, &journal, &inventory, planned, 0,
        )
        .expect("report already restored source");

        assert_eq!(
            result,
            IndexJournalSourceRestoreExecution::AlreadyBefore { source_index: 0 }
        );
        assert_eq!(
            fs::read(workspace.0.join("src/other.sai")).expect("read other source"),
            b"after other"
        );
        assert_eq!(index_files(&workspace), index_before);
        assert!(journal_still_exists(&workspace));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn changed_fresh_plan_and_invalid_source_index_do_not_mutate() {
        let journal = two_source_journal();
        let initial_states = [
            journal.sources[0].planned_after.clone(),
            journal.sources[1].before.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &initial_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let index_before = index_files(&workspace);

        set_source_path_state(
            &workspace.0,
            &journal.sources[0].path,
            &journal.sources[0].before,
        );
        let changed = execute_index_operation_journal_restore_source_before(
            &store, &journal, &inventory, planned, 0,
        )
        .expect("report changed fresh decision");
        assert_eq!(
            changed,
            IndexJournalSourceRestoreExecution::DecisionChanged {
                planned,
                observed: IndexJournalRecoveryDecision::ClearBefore,
            }
        );

        set_source_path_state(
            &workspace.0,
            &journal.sources[0].path,
            &journal.sources[0].planned_after,
        );
        let invalid_index = execute_index_operation_journal_restore_source_before(
            &store,
            &journal,
            &inventory,
            planned,
            journal.sources.len(),
        )
        .expect("report invalid source index");
        assert_eq!(
            invalid_index,
            IndexJournalSourceRestoreExecution::InvalidSourceIndex {
                source_index: journal.sources.len(),
            }
        );
        assert_eq!(index_files(&workspace), index_before);
        assert!(journal_still_exists(&workspace));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read source"),
            b"after"
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn source_state_changed_after_observation_is_preserved_as_a_conflict() {
        let journal = two_source_journal();
        let source_states = [
            journal.sources[0].planned_after.clone(),
            journal.sources[1].before.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);

        let error = super::execute_index_operation_journal_restore_source_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            0,
            || {
                fs::write(workspace.0.join("src/lib.sai"), b"third-party bytes")
                    .expect("replace source after observation");
            },
        )
        .expect_err("conditional update must reject third-party bytes");

        assert!(matches!(
            error,
            IndexJournalSourceRestoreError::SourceUpdate(
                sai_package::local_source::LocalPackageSourceFileUpdateError::Conflict
            )
        ));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read conflicting source"),
            b"third-party bytes"
        );
        assert_eq!(index_files(&workspace), index_before);
        assert!(journal_still_exists(&workspace));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn changed_journal_after_observation_prevents_source_update() {
        let journal = two_source_journal();
        let source_states = [
            journal.sources[0].planned_after.clone(),
            journal.sources[1].before.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let mut replacement = journal.clone();
        replacement.operation_id = "f8547899-0982-4d1f-85d6-9d3207deea14".to_owned();
        let index_before = index_files(&workspace);

        let error = super::execute_index_operation_journal_restore_source_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            0,
            || {
                fs::write(
                    workspace.0.join(".sai/index/operation.json"),
                    replacement.encode().expect("encode replacement"),
                )
                .expect("replace journal after observation");
            },
        )
        .expect_err("source update must reject a changed journal");

        assert!(matches!(
            error,
            IndexJournalSourceRestoreError::Store(IndexOperationJournalStoreError::JournalChanged)
        ));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read source"),
            b"after"
        );
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(
            store.read().expect("read replacement journal"),
            Some(replacement)
        );
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn restoring_to_absence_removes_only_the_selected_source() {
        let mut journal = two_source_journal();
        journal.sources[0].before = FileState::Absent;
        journal.encode().expect("encode absent before state");
        let source_states = [
            journal.sources[0].planned_after.clone(),
            journal.sources[1].planned_after.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);

        let result = execute_index_operation_journal_restore_source_before(
            &store, &journal, &inventory, planned, 0,
        )
        .expect("remove source with absent before state");

        assert_eq!(
            result,
            IndexJournalSourceRestoreExecution::Restored { source_index: 0 }
        );
        assert!(!workspace.0.join("src/lib.sai").exists());
        assert_eq!(
            fs::read(workspace.0.join("src/other.sai")).expect("read untouched source"),
            b"after other"
        );
        assert!(journal_still_exists(&workspace));
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
