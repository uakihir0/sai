//! Fail-closed CLI preflight for index-operation journal recovery.
//!
//! CLI callers must run this before ordinary index loading, discard recovery,
//! or symbol-ID allocation. The caller retains its exclusive workspace lock
//! for the whole call. LSP-origin journals are deliberately left unresolved:
//! this API has no access to the language server's current open-document
//! versions or client response.

use crate::index_operation_journal_executor::{
    IndexJournalClearExecution, IndexJournalCommitMarkerExecution, IndexJournalRestoreBeforeError,
    IndexJournalRestoreBeforeExecution, execute_index_operation_journal_clear,
    execute_index_operation_journal_mark_committed, execute_index_operation_journal_restore_before,
};
use crate::index_operation_journal_recovery::{
    IndexJournalRecoveryDecision, plan_index_operation_journal_recovery,
};
use crate::index_operation_journal_store::{
    IndexOperationJournalStore, IndexOperationJournalStoreError,
};
use crate::index_operation_journal_validation::{
    IndexJournalRecoveryContextError, capture_index_journal_recovery_context,
};
use sai_package::validation::PackageValidationContext;
use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
use sai_semantic::{IndexJournalOrigin, IndexJournalState, IndexOperationJournal};
use std::error::Error;
use std::fmt;

/// Result of checking and, when safe, settling a CLI-origin operation journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliIndexJournalPreflight {
    /// No operation journal exists; ordinary index processing may continue.
    Clean,
    /// The existing CLI journal was safely settled.
    Recovered(CliIndexJournalRecovery),
}

/// Recovery action completed by [`preflight_cli_index_operation_journal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliIndexJournalRecovery {
    /// The journal described an operation that had not changed its sources or
    /// committed index.
    ClearedBefore,
    /// The incomplete CLI operation was restored to its exact before state.
    RestoredBefore,
    /// The complete planned index was committed, marked, and then cleared.
    ClearedCommitted,
}

/// Failure that requires the caller to keep index access and ID allocation
/// blocked until a later recovery attempt succeeds.
#[derive(Debug)]
pub enum CliIndexJournalPreflightError {
    LockNotExclusive,
    RecoveryContext(IndexJournalRecoveryContextError),
    Store(IndexOperationJournalStoreError),
    RestoreBefore(IndexJournalRestoreBeforeError),
    /// LSP journals require client/document-version-aware recovery.
    LspOriginJournal,
    /// The journal state is ambiguous, pending, or requires recovery that this
    /// CLI-only preflight does not implement.
    RecoveryRequired {
        decision: IndexJournalRecoveryDecision,
    },
    /// A commit-marker transition failed to complete cleanly.
    CommitMarkerNotCompleted {
        outcome: IndexJournalCommitMarkerExecution,
    },
    /// The durable journal did not exactly match the expected marker after
    /// the marker executor returned success.
    CommittedJournalChanged,
    /// A fresh observation after the marker no longer classified as committed.
    CommittedDecisionChanged {
        decision: IndexJournalRecoveryDecision,
    },
    /// The rollback coordinator stopped before it verified and cleared the
    /// exact before state.
    RestoreNotCompleted {
        outcome: IndexJournalRestoreBeforeExecution,
    },
    /// The final journal clear did not report a successful clear.
    ClearNotCompleted {
        outcome: IndexJournalClearExecution,
    },
}

impl fmt::Display for CliIndexJournalPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockNotExclusive => formatter
                .write_str("CLI index-journal preflight requires an exclusive workspace lock"),
            Self::RecoveryContext(source) => {
                write!(
                    formatter,
                    "could not authenticate the workspace for index-journal recovery: {source}"
                )
            }
            Self::Store(source) => source.fmt(formatter),
            Self::RestoreBefore(source) => source.fmt(formatter),
            Self::LspOriginJournal => formatter.write_str(
                "an LSP-origin index journal requires client-aware recovery and remains pending",
            ),
            Self::RecoveryRequired { decision } => {
                write!(
                    formatter,
                    "index-journal recovery is required: {decision:?}"
                )
            }
            Self::CommitMarkerNotCompleted { outcome } => {
                write!(
                    formatter,
                    "could not durably mark the committed index: {outcome:?}"
                )
            }
            Self::CommittedJournalChanged => formatter
                .write_str("the operation journal changed or disappeared after its commit marker"),
            Self::CommittedDecisionChanged { decision } => write!(
                formatter,
                "the committed index no longer has a clearable recovery state: {decision:?}"
            ),
            Self::RestoreNotCompleted { outcome } => {
                write!(
                    formatter,
                    "CLI index-journal rollback did not complete: {outcome:?}"
                )
            }
            Self::ClearNotCompleted { outcome } => {
                write!(
                    formatter,
                    "CLI index-journal clear did not complete: {outcome:?}"
                )
            }
        }
    }
}

impl Error for CliIndexJournalPreflightError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RecoveryContext(source) => Some(source),
            Self::Store(source) => Some(source),
            Self::RestoreBefore(source) => Some(source),
            Self::LockNotExclusive
            | Self::LspOriginJournal
            | Self::RecoveryRequired { .. }
            | Self::CommitMarkerNotCompleted { .. }
            | Self::CommittedJournalChanged
            | Self::CommittedDecisionChanged { .. }
            | Self::RestoreNotCompleted { .. }
            | Self::ClearNotCompleted { .. } => None,
        }
    }
}

impl From<IndexOperationJournalStoreError> for CliIndexJournalPreflightError {
    fn from(error: IndexOperationJournalStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<IndexJournalRecoveryContextError> for CliIndexJournalPreflightError {
    fn from(error: IndexJournalRecoveryContextError) -> Self {
        Self::RecoveryContext(error)
    }
}

impl From<IndexJournalRestoreBeforeError> for CliIndexJournalPreflightError {
    fn from(error: IndexJournalRestoreBeforeError) -> Self {
        Self::RestoreBefore(error)
    }
}

/// Checks for a pending operation journal before CLI index processing.
///
/// The caller must keep its existing exclusive workspace lock alive for this
/// entire call and must not read the ordinary index or allocate symbol IDs
/// unless this function returns `Ok`. `validation_context` is used only if a
/// journal exists and its package graph must be authenticated.
///
/// Missing journals return [`CliIndexJournalPreflight::Clean`]. Corrupt or
/// unreadable journals, LSP-origin journals, stale plans, ambiguous source or
/// index states, and uncertain durability outcomes return an error. Every
/// such error requires the caller to keep recovery gated and reread state
/// before retrying; an error does not imply that a conditional filesystem
/// operation had no visible effect.
///
/// LSP-origin journals are always blocked here because this API cannot
/// validate the server's active analysis snapshot, open-document versions, or
/// client response. Platforms without safe directory durability remain
/// fail-closed through the underlying store and filesystem operations.
pub fn preflight_cli_index_operation_journal(
    lock: &WorkspaceLockGuard,
    validation_context: PackageValidationContext<'_>,
) -> Result<CliIndexJournalPreflight, CliIndexJournalPreflightError> {
    if lock.mode() != WorkspaceLockMode::Exclusive {
        return Err(CliIndexJournalPreflightError::LockNotExclusive);
    }

    let store = IndexOperationJournalStore::new(lock);
    let Some(journal) = store.read()? else {
        return Ok(CliIndexJournalPreflight::Clean);
    };

    if journal.origin != IndexJournalOrigin::Cli {
        return Err(CliIndexJournalPreflightError::LspOriginJournal);
    }

    let context = capture_index_journal_recovery_context(lock, validation_context)?;
    let observation = store.observe_recovery(&journal, &context)?;
    let decision = plan_index_operation_journal_recovery(&journal, &observation);

    match decision {
        IndexJournalRecoveryDecision::ClearBefore => {
            clear_before(&store, &journal, &context, decision)?;
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::ClearedBefore,
            ))
        }
        IndexJournalRecoveryDecision::RestoreBefore => {
            let outcome = execute_index_operation_journal_restore_before(
                &store, &journal, &context, decision,
            )?;
            if outcome != IndexJournalRestoreBeforeExecution::Cleared {
                return Err(CliIndexJournalPreflightError::RestoreNotCompleted { outcome });
            }
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::RestoredBefore,
            ))
        }
        IndexJournalRecoveryDecision::ClearCommitted => {
            clear_committed(&store, &journal, &context, decision)?;
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::ClearedCommitted,
            ))
        }
        _ => Err(CliIndexJournalPreflightError::RecoveryRequired { decision }),
    }
}

fn clear_before(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &crate::index_operation_journal_validation::IndexJournalRecoveryContext,
    decision: IndexJournalRecoveryDecision,
) -> Result<(), CliIndexJournalPreflightError> {
    match execute_index_operation_journal_clear(store, journal, context, decision)? {
        IndexJournalClearExecution::Cleared {
            decision: IndexJournalRecoveryDecision::ClearBefore,
        } => Ok(()),
        outcome => Err(CliIndexJournalPreflightError::ClearNotCompleted { outcome }),
    }
}

fn clear_committed(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &crate::index_operation_journal_validation::IndexJournalRecoveryContext,
    decision: IndexJournalRecoveryDecision,
) -> Result<(), CliIndexJournalPreflightError> {
    // Always run the marker executor, including for an already marked
    // journal: its idempotent path synchronizes the index directory after a
    // previous uncertain marker flush.
    let marker = execute_index_operation_journal_mark_committed(store, journal, context, decision)?;
    if !matches!(
        marker,
        IndexJournalCommitMarkerExecution::MarkedCommitted
            | IndexJournalCommitMarkerExecution::AlreadyCommitted
    ) {
        return Err(CliIndexJournalPreflightError::CommitMarkerNotCompleted { outcome: marker });
    }

    let mut expected_committed = journal.clone();
    expected_committed.state = IndexJournalState::IndexCommitted;
    let Some(committed) = store.read()? else {
        return Err(CliIndexJournalPreflightError::CommittedJournalChanged);
    };
    if committed != expected_committed {
        return Err(CliIndexJournalPreflightError::CommittedJournalChanged);
    }

    // The marker and clear are separate durable steps. Read back the exact
    // marked record, then make a new complete observation and plan so the
    // first plan cannot authorize both mutations.
    let observation = store.observe_recovery(&committed, context)?;
    let fresh_decision = plan_index_operation_journal_recovery(&committed, &observation);
    if fresh_decision != IndexJournalRecoveryDecision::ClearCommitted {
        return Err(CliIndexJournalPreflightError::CommittedDecisionChanged {
            decision: fresh_decision,
        });
    }

    match execute_index_operation_journal_clear(store, &committed, context, fresh_decision)? {
        IndexJournalClearExecution::Cleared {
            decision: IndexJournalRecoveryDecision::ClearCommitted,
        } => Ok(()),
        outcome => Err(CliIndexJournalPreflightError::ClearNotCompleted { outcome }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliIndexJournalPreflight, CliIndexJournalPreflightError, CliIndexJournalRecovery,
        preflight_cli_index_operation_journal,
    };
    use crate::index_operation_journal_recovery::{
        IndexJournalRecoveryAmbiguity, IndexJournalRecoveryDecision,
    };
    use crate::index_operation_journal_validation::IndexJournalRecoveryContextError;
    use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange,
    };
    use sai_stable_hash::hash_bytes;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    const ROOT_LOCK_BYTES: &[u8] = b"lock 2\n\
        root acme:app @ 1.0.0\n\
            targets { wasm32-core, wasm32-wasi }\n";

    struct TestWorkspace(PathBuf);

    impl TestWorkspace {
        fn new() -> Self {
            let id = NEXT_TEST_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sai-journal-cli-preflight-{}-{id}",
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
            "0".repeat(32)
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

    fn create_workspace_with_lock_mode(
        mode: WorkspaceLockMode,
    ) -> (TestWorkspace, WorkspaceLockGuard) {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.0.join("src")).expect("create source directory");
        fs::write(
            workspace.0.join("sai.package"),
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        )
        .expect("write package manifest");
        fs::write(workspace.0.join("sai.lock"), ROOT_LOCK_BYTES).expect("write workspace lock");
        let lock = WorkspaceLockGuard::acquire(&workspace.0, mode).expect("acquire workspace lock");
        (workspace, lock)
    }

    fn create_workspace() -> (TestWorkspace, WorkspaceLockGuard) {
        create_workspace_with_lock_mode(WorkspaceLockMode::Exclusive)
    }

    fn write_source(root: &Path, state: &FileState) {
        match state {
            FileState::Absent => {
                let _ = fs::remove_file(root.join("src/lib.sai"));
            }
            FileState::RegularFile(bytes) => {
                fs::write(root.join("src/lib.sai"), bytes).expect("write source state");
            }
        }
    }

    fn write_index(
        root: &Path,
        journal: &IndexOperationJournal,
        index_after: bool,
        source_state: &FileState,
    ) {
        write_source(root, source_state);
        let index_directory = root.join(".sai/index");
        fs::create_dir_all(&index_directory).expect("create index directory");
        let snapshot = if index_after {
            &journal.index_after
        } else {
            &journal.index_before
        };
        let FileState::RegularFile(header) = &snapshot.header else {
            panic!("fixture has a regular index header");
        };
        fs::write(index_directory.join("index.json"), header).expect("write index header");
        for package_file in &snapshot.package_files {
            let FileState::RegularFile(bytes) = &package_file.state else {
                panic!("fixture has a regular package index");
            };
            fs::write(index_directory.join(package_file.file.as_str()), bytes)
                .expect("write package index");
        }
        fs::write(
            index_directory.join("operation.json"),
            journal.encode().expect("encode operation journal"),
        )
        .expect("write operation journal");
    }

    fn preflight(
        lock: &WorkspaceLockGuard,
    ) -> Result<CliIndexJournalPreflight, CliIndexJournalPreflightError> {
        preflight_cli_index_operation_journal(
            lock,
            sai_package::validation::PackageValidationContext::default(),
        )
    }

    #[test]
    fn absent_journal_allows_index_processing() {
        let (_workspace, lock) = create_workspace();

        assert!(matches!(
            preflight(&lock),
            Ok(CliIndexJournalPreflight::Clean)
        ));
    }

    #[test]
    fn shared_lock_is_rejected_even_when_no_journal_exists() {
        let (_workspace, lock) = create_workspace_with_lock_mode(WorkspaceLockMode::Shared);

        assert!(matches!(
            preflight(&lock),
            Err(CliIndexJournalPreflightError::LockNotExclusive)
        ));
    }

    #[test]
    fn corrupt_journal_blocks_and_is_preserved() {
        let (workspace, lock) = create_workspace();
        let journal_path = workspace.0.join(".sai/index/operation.json");
        fs::create_dir_all(journal_path.parent().unwrap()).expect("create index directory");
        let corrupt = b"{not canonical journal}\n";
        fs::write(&journal_path, corrupt).expect("write corrupt journal");

        assert!(matches!(
            preflight(&lock),
            Err(CliIndexJournalPreflightError::Store(_))
        ));
        assert_eq!(fs::read(journal_path).unwrap(), corrupt);
    }

    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn unchanged_cli_operation_is_cleared_without_modifying_the_before_state() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock) = create_workspace();
        write_index(&workspace.0, &journal, false, &journal.sources[0].before);

        assert!(matches!(
            preflight(&lock),
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::ClearedBefore
            ))
        ));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).unwrap(),
            b"before"
        );
        assert!(!workspace.0.join(".sai/index/operation.json").exists());
    }

    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn partial_cli_operation_is_restored_before_index_processing() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock) = create_workspace();
        write_index(
            &workspace.0,
            &journal,
            true,
            &journal.sources[0].planned_after,
        );
        // Keep the after header but restore the package file to the before
        // bytes, representing a recoverable partial index publication.
        let FileState::RegularFile(before_package) = &journal.index_before.package_files[0].state
        else {
            panic!("fixture has a before package index");
        };
        fs::write(
            workspace.0.join(".sai/index/acme.app.1.jsonl"),
            before_package,
        )
        .expect("write partial package index");

        assert!(matches!(
            preflight(&lock),
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::RestoredBefore
            ))
        ));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).unwrap(),
            b"before"
        );
        let FileState::RegularFile(before_header) = &journal.index_before.header else {
            panic!("fixture has a before header");
        };
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).unwrap(),
            *before_header
        );
        assert!(!workspace.0.join(".sai/index/operation.json").exists());
    }

    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn committed_cli_operation_marks_then_freshly_plans_before_clear() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock) = create_workspace();
        write_index(
            &workspace.0,
            &journal,
            true,
            &journal.sources[0].planned_after,
        );

        assert!(matches!(
            preflight(&lock),
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::ClearedCommitted
            ))
        ));
        assert_eq!(fs::read(workspace.0.join("src/lib.sai")).unwrap(), b"after");
        assert!(!workspace.0.join(".sai/index/operation.json").exists());
    }

    #[cfg(all(
        unix,
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn already_marked_cli_operation_is_resettled_and_cleared() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::IndexCommitted);
        let (workspace, lock) = create_workspace();
        write_index(
            &workspace.0,
            &journal,
            true,
            &journal.sources[0].planned_after,
        );

        assert!(matches!(
            preflight(&lock),
            Ok(CliIndexJournalPreflight::Recovered(
                CliIndexJournalRecovery::ClearedCommitted
            ))
        ));
        assert!(!workspace.0.join(".sai/index/operation.json").exists());
    }

    #[test]
    fn lsp_origin_journal_blocks_and_is_preserved() {
        let journal = journal(
            IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::AwaitingClient,
        );
        let (workspace, lock) = create_workspace();
        write_index(&workspace.0, &journal, false, &journal.sources[0].before);
        let journal_path = workspace.0.join(".sai/index/operation.json");
        let original_journal = fs::read(&journal_path).expect("read original journal");

        assert!(matches!(
            preflight(&lock),
            Err(CliIndexJournalPreflightError::LspOriginJournal)
        ));
        assert_eq!(fs::read(journal_path).unwrap(), original_journal);
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).unwrap(),
            b"before"
        );
    }

    #[test]
    fn unexpected_cli_source_blocks_and_preserves_the_journal() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock) = create_workspace();
        write_index(&workspace.0, &journal, false, &journal.sources[0].before);
        fs::write(workspace.0.join("src/lib.sai"), b"third-party edit")
            .expect("write unrelated source state");
        let journal_path = workspace.0.join(".sai/index/operation.json");
        let original_journal = fs::read(&journal_path).expect("read original journal");

        // Platforms without open root-directory identity support cannot
        // authenticate this workspace yet; supported platforms must classify
        // the third-party bytes as ambiguous. Both outcomes preserve state.
        assert!(matches!(
            preflight(&lock),
            Err(CliIndexJournalPreflightError::RecoveryContext(
                IndexJournalRecoveryContextError::LockedWorkspaceRootIdentityUnsupported
            )) | Err(CliIndexJournalPreflightError::RecoveryRequired {
                decision: IndexJournalRecoveryDecision::Ambiguous(
                    IndexJournalRecoveryAmbiguity::UnexpectedSourceState
                )
            })
        ));
        assert_eq!(fs::read(journal_path).unwrap(), original_journal);
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).unwrap(),
            b"third-party edit"
        );
    }
}
