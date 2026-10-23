//! Conservative execution of index-operation journal recovery decisions.
//!
//! Recovery planning is read-only. This module re-observes the workspace
//! immediately before acting. It can clear confirmed terminal decisions,
//! restore one source file or index file, and coordinate a complete CLI
//! rollback through before-state verification and journal clearing. Index
//! commit, journal transitions, and forward recovery remain later slices.

use crate::index_operation_journal_recovery::{
    IndexJournalRecoveryDecision, ObservedFileState as PlannerObservedFileState,
    plan_index_operation_journal_recovery,
};
use crate::index_operation_journal_store::{
    IndexOperationJournalIndexTarget, IndexOperationJournalStore, IndexOperationJournalStoreError,
};
use crate::index_operation_journal_validation::IndexJournalRecoveryContext;
use sai_package::local_source::LocalPackageSourceFileUpdateError;
use sai_package::package_path::{ContentPackagePath, RepositoryPackageDirectory};
use sai_semantic::{FileState, IndexOperationJournal, PackageIndexFileName};
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

/// Result of restoring the committed index header to its before snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalHeaderRestoreExecution {
    /// The fresh observation produced a different planner decision.
    DecisionChanged {
        planned: IndexJournalRecoveryDecision,
        observed: IndexJournalRecoveryDecision,
    },
    /// The current planner decision requires another recovery action.
    NotRestorable {
        decision: IndexJournalRecoveryDecision,
    },
    /// The header already has the recorded before state.
    AlreadyBefore,
    /// The header was conditionally restored to its before snapshot.
    Restored,
}

/// Result of restoring one journaled package-index file from a `RestoreBefore` plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalPackageRestoreExecution {
    /// The fresh observation produced a different planner decision.
    DecisionChanged {
        planned: IndexJournalRecoveryDecision,
        observed: IndexJournalRecoveryDecision,
    },
    /// The current planner decision requires another recovery action.
    NotRestorable {
        decision: IndexJournalRecoveryDecision,
    },
    /// The header is not yet at its before snapshot; restore it before package files.
    HeaderNotBefore,
    /// The selected package-index file already has its before state.
    AlreadyBefore,
    /// The selected package-index file was conditionally restored to its before state.
    Restored,
}

/// Result of completing a `RestoreBefore` decision and clearing its journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalRestoreBeforeExecution {
    /// A fresh observation no longer matches the decision used for an action.
    DecisionChanged {
        planned: IndexJournalRecoveryDecision,
        observed: IndexJournalRecoveryDecision,
    },
    /// The current decision is not eligible for CLI before-state restoration.
    NotRestorable {
        decision: IndexJournalRecoveryDecision,
    },
    /// The package rollback precondition no longer has the before header.
    HeaderNotBefore,
    /// All restore steps returned, but a fresh observation did not verify the
    /// complete before state, so the journal was retained.
    NotVerified {
        decision: IndexJournalRecoveryDecision,
    },
    /// The complete before state was freshly verified and the journal cleared.
    Cleared,
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

/// Failure while coordinating a complete `RestoreBefore` operation.
#[derive(Debug)]
pub enum IndexJournalRestoreBeforeError {
    Store(IndexOperationJournalStoreError),
    SourceRestore(IndexJournalSourceRestoreError),
}

impl fmt::Display for IndexJournalRestoreBeforeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "index journal rollback did not complete: {self:?}"
        )
    }
}

impl Error for IndexJournalRestoreBeforeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(source) => Some(source),
            Self::SourceRestore(source) => Some(source),
        }
    }
}

impl From<IndexOperationJournalStoreError> for IndexJournalRestoreBeforeError {
    fn from(error: IndexOperationJournalStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<IndexJournalSourceRestoreError> for IndexJournalRestoreBeforeError {
    fn from(error: IndexJournalSourceRestoreError) -> Self {
        Self::SourceRestore(error)
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
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
    execute_index_operation_journal_clear_with_hook(
        store,
        journal,
        context,
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
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    source_index: usize,
) -> Result<IndexJournalSourceRestoreExecution, IndexJournalSourceRestoreError> {
    execute_index_operation_journal_restore_source_before_with_hook(
        store,
        journal,
        context,
        planned_decision,
        source_index,
        || {},
    )
}

/// Re-observes and restores the index header for a `RestoreBefore` decision.
///
/// `planned_decision` must have been produced for `journal` under the same
/// exclusive workspace lock. The fresh observation and planner must still
/// authorize `RestoreBefore`. The journal store accepts only the exact
/// recorded before/after header pair, treats an already-before header as an
/// idempotent no-op, verifies the current journal, and conditionally updates
/// only `index.json`.
///
/// This slice never updates package-index files or sources, changes journal
/// state, or clears the journal. The caller must retain the journal and
/// re-plan before each subsequent recovery action.
pub fn execute_index_operation_journal_restore_header_before(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
) -> Result<IndexJournalHeaderRestoreExecution, IndexOperationJournalStoreError> {
    execute_index_operation_journal_restore_header_before_with_hook(
        store,
        journal,
        context,
        planned_decision,
        || {},
    )
}

/// Re-observes and restores one recorded package-index file from a
/// `RestoreBefore` decision.
///
/// `planned_decision` must have been produced for `journal` under the same
/// exclusive workspace lock. The fresh observation and planner must still
/// authorize `RestoreBefore`. The store then requires the exact recorded
/// before header before conditionally updating the selected package file from
/// its planned-after state to before. An already-before file is an idempotent
/// no-op. This slice never changes a source, another package file, the header,
/// journal state, or journal presence.
pub fn execute_index_operation_journal_restore_package_before(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    file: &PackageIndexFileName,
    planned_decision: IndexJournalRecoveryDecision,
) -> Result<IndexJournalPackageRestoreExecution, IndexOperationJournalStoreError> {
    execute_index_operation_journal_restore_package_before_with_hook(
        store,
        journal,
        context,
        file,
        planned_decision,
        || {},
    )
}

fn execute_index_operation_journal_restore_package_before_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    file: &PackageIndexFileName,
    planned_decision: IndexJournalRecoveryDecision,
    before_update: impl FnOnce(),
) -> Result<IndexJournalPackageRestoreExecution, IndexOperationJournalStoreError> {
    let observation = store.observe_recovery(journal, context)?;
    let observed_decision = plan_index_operation_journal_recovery(journal, &observation);
    if observed_decision != planned_decision {
        return Ok(IndexJournalPackageRestoreExecution::DecisionChanged {
            planned: planned_decision,
            observed: observed_decision,
        });
    }
    if observed_decision != IndexJournalRecoveryDecision::RestoreBefore {
        return Ok(IndexJournalPackageRestoreExecution::NotRestorable {
            decision: observed_decision,
        });
    }

    before_update();
    store.verify_recovery_context(context)?;
    match store.update_package_index_file_if_before_header(
        journal,
        file,
        &package_file_state(journal, file, true)?,
        &package_file_state(journal, file, false)?,
    ) {
        Ok(ConditionalFileUpdate::Updated) => Ok(IndexJournalPackageRestoreExecution::Restored),
        Ok(ConditionalFileUpdate::AlreadyDesired) => {
            Ok(IndexJournalPackageRestoreExecution::AlreadyBefore)
        }
        Err(IndexOperationJournalStoreError::IndexHeaderNotBefore) => {
            Ok(IndexJournalPackageRestoreExecution::HeaderNotBefore)
        }
        Err(error) => Err(error),
    }
}

fn package_file_state(
    journal: &IndexOperationJournal,
    file: &PackageIndexFileName,
    after: bool,
) -> Result<FileState, IndexOperationJournalStoreError> {
    let snapshot = if after {
        &journal.index_after
    } else {
        &journal.index_before
    };
    snapshot
        .package_files
        .binary_search_by(|package_file| package_file.file.cmp(file))
        .ok()
        .map(|index| snapshot.package_files[index].state.clone())
        .ok_or(IndexOperationJournalStoreError::IndexFileNotRecorded)
}

/// Completes a CLI rollback in deterministic reverse-publication order.
///
/// `planned_decision` must be the fresh planner result obtained under the same
/// exclusive workspace lock and lock-bound recovery context used by `store`. Only
/// `RestoreBefore` and an already-completed `ClearBefore` may proceed. For a
/// rollback this restores the index header first, affected package-index files
/// in their recorded order, then source files in journal order. Each
/// one-file executor obtains its own observation and planner result before
/// mutating. If a step reports that the state has already reached
/// `ClearBefore`, the coordinator re-observes that state before attempting the
/// clear.
///
/// The journal remains unchanged throughout restoration. A fresh complete
/// observation must classify as `ClearBefore` before the journal is removed.
/// Any changed plan, unexpected or unavailable state, I/O error, or uncertain
/// durability outcome stops the coordinator; the caller must retain recovery
/// gating and read back the journal and workspace before retrying. An already
/// cleared `ClearBefore` operation can be resumed by calling this function
/// with a fresh `ClearBefore` plan while the journal is still present.
pub fn execute_index_operation_journal_restore_before(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
) -> Result<IndexJournalRestoreBeforeExecution, IndexJournalRestoreBeforeError> {
    execute_index_operation_journal_restore_before_with_hook(
        store,
        journal,
        context,
        planned_decision,
        || {},
    )
}

fn execute_index_operation_journal_restore_before_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    after_before_verification: impl FnOnce(),
) -> Result<IndexJournalRestoreBeforeExecution, IndexJournalRestoreBeforeError> {
    if !matches!(
        planned_decision,
        IndexJournalRecoveryDecision::RestoreBefore | IndexJournalRecoveryDecision::ClearBefore
    ) {
        return Ok(IndexJournalRestoreBeforeExecution::NotRestorable {
            decision: planned_decision,
        });
    }

    if planned_decision == IndexJournalRecoveryDecision::ClearBefore {
        return clear_verified_before_state(
            store,
            journal,
            context,
            planned_decision,
            after_before_verification,
        );
    }

    match execute_index_operation_journal_restore_header_before(
        store,
        journal,
        context,
        IndexJournalRecoveryDecision::RestoreBefore,
    )? {
        IndexJournalHeaderRestoreExecution::Restored
        | IndexJournalHeaderRestoreExecution::AlreadyBefore => {}
        IndexJournalHeaderRestoreExecution::DecisionChanged { observed, .. }
            if observed == IndexJournalRecoveryDecision::ClearBefore =>
        {
            return clear_verified_before_state(
                store,
                journal,
                context,
                observed,
                after_before_verification,
            );
        }
        IndexJournalHeaderRestoreExecution::DecisionChanged { planned, observed } => {
            return Ok(IndexJournalRestoreBeforeExecution::DecisionChanged { planned, observed });
        }
        IndexJournalHeaderRestoreExecution::NotRestorable { decision } => {
            return Ok(IndexJournalRestoreBeforeExecution::NotRestorable { decision });
        }
    }

    for package_file in &journal.index_before.package_files {
        match execute_index_operation_journal_restore_package_before(
            store,
            journal,
            context,
            &package_file.file,
            IndexJournalRecoveryDecision::RestoreBefore,
        )? {
            IndexJournalPackageRestoreExecution::Restored
            | IndexJournalPackageRestoreExecution::AlreadyBefore => {}
            IndexJournalPackageRestoreExecution::DecisionChanged { observed, .. }
                if observed == IndexJournalRecoveryDecision::ClearBefore =>
            {
                return clear_verified_before_state(
                    store,
                    journal,
                    context,
                    observed,
                    after_before_verification,
                );
            }
            IndexJournalPackageRestoreExecution::DecisionChanged { planned, observed } => {
                return Ok(IndexJournalRestoreBeforeExecution::DecisionChanged {
                    planned,
                    observed,
                });
            }
            IndexJournalPackageRestoreExecution::NotRestorable { decision } => {
                return Ok(IndexJournalRestoreBeforeExecution::NotRestorable { decision });
            }
            IndexJournalPackageRestoreExecution::HeaderNotBefore => {
                return Ok(IndexJournalRestoreBeforeExecution::HeaderNotBefore);
            }
        }
    }

    for source_index in 0..journal.sources.len() {
        match execute_index_operation_journal_restore_source_before(
            store,
            journal,
            context,
            IndexJournalRecoveryDecision::RestoreBefore,
            source_index,
        )? {
            IndexJournalSourceRestoreExecution::Restored { .. }
            | IndexJournalSourceRestoreExecution::AlreadyBefore { .. } => {}
            IndexJournalSourceRestoreExecution::DecisionChanged { observed, .. }
                if observed == IndexJournalRecoveryDecision::ClearBefore =>
            {
                return clear_verified_before_state(
                    store,
                    journal,
                    context,
                    observed,
                    after_before_verification,
                );
            }
            IndexJournalSourceRestoreExecution::DecisionChanged { planned, observed } => {
                return Ok(IndexJournalRestoreBeforeExecution::DecisionChanged {
                    planned,
                    observed,
                });
            }
            IndexJournalSourceRestoreExecution::NotRestorable { decision } => {
                return Ok(IndexJournalRestoreBeforeExecution::NotRestorable { decision });
            }
            IndexJournalSourceRestoreExecution::InvalidSourceIndex { .. } => {
                unreachable!("coordinator iterates over the recorded source entries");
            }
        }
    }

    clear_verified_before_state(
        store,
        journal,
        context,
        IndexJournalRecoveryDecision::ClearBefore,
        after_before_verification,
    )
}

fn clear_verified_before_state(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    after_before_verification: impl FnOnce(),
) -> Result<IndexJournalRestoreBeforeExecution, IndexJournalRestoreBeforeError> {
    let observation = store.observe_recovery(journal, context)?;
    let observed_decision = plan_index_operation_journal_recovery(journal, &observation);
    if observed_decision != planned_decision {
        return Ok(IndexJournalRestoreBeforeExecution::DecisionChanged {
            planned: planned_decision,
            observed: observed_decision,
        });
    }
    if observed_decision != IndexJournalRecoveryDecision::ClearBefore {
        return Ok(IndexJournalRestoreBeforeExecution::NotVerified {
            decision: observed_decision,
        });
    }

    after_before_verification();
    Ok(
        match execute_index_operation_journal_clear(
            store,
            journal,
            context,
            IndexJournalRecoveryDecision::ClearBefore,
        )? {
            IndexJournalClearExecution::Cleared {
                decision: IndexJournalRecoveryDecision::ClearBefore,
            } => IndexJournalRestoreBeforeExecution::Cleared,
            IndexJournalClearExecution::DecisionChanged { planned, observed } => {
                IndexJournalRestoreBeforeExecution::DecisionChanged { planned, observed }
            }
            IndexJournalClearExecution::NotClearable { decision } => {
                IndexJournalRestoreBeforeExecution::NotVerified { decision }
            }
            IndexJournalClearExecution::Cleared { .. } => {
                unreachable!("clear was requested only after a ClearBefore decision");
            }
        },
    )
}

fn execute_index_operation_journal_restore_header_before_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    before_update: impl FnOnce(),
) -> Result<IndexJournalHeaderRestoreExecution, IndexOperationJournalStoreError> {
    let observation = store.observe_recovery(journal, context)?;
    let observed_decision = plan_index_operation_journal_recovery(journal, &observation);
    if observed_decision != planned_decision {
        return Ok(IndexJournalHeaderRestoreExecution::DecisionChanged {
            planned: planned_decision,
            observed: observed_decision,
        });
    }
    if observed_decision != IndexJournalRecoveryDecision::RestoreBefore {
        return Ok(IndexJournalHeaderRestoreExecution::NotRestorable {
            decision: observed_decision,
        });
    }

    before_update();
    store.verify_recovery_context(context)?;
    let update = store.update_index_file_if_matches(
        journal,
        IndexOperationJournalIndexTarget::Header,
        &journal.index_after.header,
        &journal.index_before.header,
    )?;
    Ok(match update {
        ConditionalFileUpdate::Updated => IndexJournalHeaderRestoreExecution::Restored,
        ConditionalFileUpdate::AlreadyDesired => IndexJournalHeaderRestoreExecution::AlreadyBefore,
    })
}

fn execute_index_operation_journal_restore_source_before_with_hook(
    store: &IndexOperationJournalStore<'_>,
    journal: &IndexOperationJournal,
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    source_index: usize,
    before_update: impl FnOnce(),
) -> Result<IndexJournalSourceRestoreExecution, IndexJournalSourceRestoreError> {
    let observation = store.observe_recovery(journal, context)?;
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

    let package_context = context
        .inventory()
        .contexts()
        .iter()
        .find(|context| context.package_node_key() == source.package_node_key)
        .ok_or_else(|| IndexJournalSourceRestoreError::MissingPackageContext {
            package_node_key: source.package_node_key.clone(),
        })?;
    let source_directory = package_context.source_directory().ok_or_else(|| {
        IndexJournalSourceRestoreError::MissingSourceDirectory {
            package_node_key: source.package_node_key.clone(),
        }
    })?;
    let source_directory = ContentPackagePath::parse(source_directory.as_str())
        .map_err(|_| IndexJournalSourceRestoreError::InvalidSourcePath)?;
    let package_root = RepositoryPackageDirectory::parse(&source.package_root)
        .map_err(|_| IndexJournalSourceRestoreError::InvalidPackageRoot)?;
    let package_directory = context
        .inventory()
        .directory_for_package(&source.package_node_key)
        .ok_or_else(|| IndexJournalSourceRestoreError::MissingPackageContext {
            package_node_key: source.package_node_key.clone(),
        })?;
    let package_directory = package_directory
        .reopen_from_workspace_root(context.inventory().root_directory(), &package_root)
        .map_err(|error| {
            IndexJournalSourceRestoreError::SourceUpdate(
                LocalPackageSourceFileUpdateError::Directory(error),
            )
        })?;
    let source_path = ContentPackagePath::parse(&source.path)
        .map_err(|_| IndexJournalSourceRestoreError::InvalidSourcePath)?;

    before_update();
    store.verify_current(journal, context)?;
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
    context: &IndexJournalRecoveryContext,
    planned_decision: IndexJournalRecoveryDecision,
    after_observation: impl FnOnce(),
) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
    let observation = store.observe_recovery(journal, context)?;
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
    store.verify_recovery_context(context)?;
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
        IndexJournalClearExecution, IndexJournalHeaderRestoreExecution,
        IndexJournalPackageRestoreExecution, IndexJournalRestoreBeforeExecution,
        IndexJournalSourceRestoreError, IndexJournalSourceRestoreExecution,
        execute_index_operation_journal_clear, execute_index_operation_journal_restore_before,
        execute_index_operation_journal_restore_header_before,
        execute_index_operation_journal_restore_package_before,
        execute_index_operation_journal_restore_source_before,
    };
    use crate::index_operation_journal_recovery::{
        IndexGenerationObservation, IndexJournalRecoveryAmbiguity, IndexJournalRecoveryDecision,
        plan_index_operation_journal_recovery,
    };
    use crate::index_operation_journal_store::{
        IndexOperationJournalStore, IndexOperationJournalStoreError,
    };
    use crate::index_operation_journal_validation::{
        IndexJournalRecoveryContext, capture_index_journal_recovery_context,
    };
    use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange, SymbolHex, SymbolIdMapping,
    };
    use sai_stable_hash::hash_bytes;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    const ROOT_LOCK_BYTES: &[u8] = b"lock 2\n\
        root acme:app @ 1.0.0\n\
            targets { wasm32-core, wasm32-wasi }\n";
    const TWO_PACKAGE_LOCK_BYTES: &[u8] = b"lock 2\n\
        root acme:app @ 1.0.0\n\
            targets { wasm32-core, wasm32-wasi }\n\
            requires acme:extra @ ^1 -> 1.0.0 from path \"packages/extra\"\n\
        \n\
        package acme:extra @ 1.0.0\n\
            source path \"packages/extra\"\n";

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

    fn journal_with_two_affected_package_files(
        origin: IndexJournalOrigin,
        state: IndexJournalState,
    ) -> IndexOperationJournal {
        let mut journal = journal(origin, state);
        let first_file = journal.index_before.package_files[0].file.clone();
        let first_before = match &journal.index_before.package_files[0].state {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("fixture before package file is regular"),
        };
        let first_after = match &journal.index_after.package_files[0].state {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("fixture after package file is regular"),
        };
        let second_file = PackageIndexFileName::parse("acme.extra.1.jsonl").unwrap();
        let second_before = package_bytes_for("acme:extra.sample", false);
        let second_after = package_bytes_for("acme:extra.sample", true);
        let mut second_source = source_change();
        second_source.package_node_key = "package:acme:extra@1.0.0".to_owned();
        second_source.package_root = "packages/extra".to_owned();
        second_source.path = "src/lib.sai".to_owned();
        second_source.before = FileState::RegularFile(b"before extra source".to_vec());
        second_source.planned_after = FileState::RegularFile(b"after extra source".to_vec());
        journal.sources.push(second_source);
        journal.index_before.package_files.push(PackageFileState {
            file: second_file.clone(),
            state: FileState::RegularFile(second_before.clone()),
        });
        journal.index_after.package_files.push(PackageFileState {
            file: second_file.clone(),
            state: FileState::RegularFile(second_after.clone()),
        });
        journal.index_before.header = FileState::RegularFile(header_with_package_files(&[
            (&first_file, &first_before),
            (&second_file, &second_before),
        ]));
        journal.index_after.header = FileState::RegularFile(header_with_package_files(&[
            (&first_file, &first_after),
            (&second_file, &second_after),
        ]));
        journal.encode().expect("fixture remains a valid journal");
        journal
    }

    fn journal_with_package_file_absent_before() -> IndexOperationJournal {
        let mut journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        journal.index_before.header = FileState::RegularFile(
            b"{\"format\":\"sai-index\",\"version\":1,\"packages\":[]}\n".to_vec(),
        );
        journal.index_before.package_files[0].state = FileState::Absent;
        journal.symbol_mappings = vec![SymbolIdMapping {
            package_node_key: "package:acme:app@1.0.0".to_owned(),
            hex: SymbolHex::parse("000001").expect("valid symbol hex"),
            before_path: None,
            after_path: Some("acme:app.sample".to_owned()),
        }];
        journal
            .encode()
            .expect("empty-index before snapshot is a valid journal");
        journal
    }

    fn package_bytes(after: bool) -> Vec<u8> {
        package_bytes_for("acme:app.sample", after)
    }

    fn package_bytes_for(path: &str, after: bool) -> Vec<u8> {
        let fingerprint = if after { "b" } else { "a" }.repeat(32);
        format!(
            "{{\"hex\":\"000001\",\"def_path_hash\":\"{}\",\"path\":\"{path}\",\"kind\":\"fn\",\"fingerprint\":\"{fingerprint}\"}}\n",
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

    fn header_with_package_files(package_files: &[(&PackageIndexFileName, &[u8])]) -> Vec<u8> {
        let records = package_files
            .iter()
            .map(|(file, bytes)| {
                format!(
                    "{{\"file\":\"{}\",\"blake3\":\"{}\"}}",
                    file.as_str(),
                    hash_bytes(bytes).to_hex()
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{records}]}}\n")
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
        match &snapshot.header {
            FileState::RegularFile(header) => {
                fs::write(index_directory.join("index.json"), header).expect("write index header");
            }
            FileState::Absent => {
                let _ = fs::remove_file(index_directory.join("index.json"));
            }
        }
        for package_file in &snapshot.package_files {
            let path = index_directory.join(package_file.file.as_str());
            match &package_file.state {
                FileState::RegularFile(bytes) => {
                    fs::write(path, bytes).expect("write package index file");
                }
                FileState::Absent => {
                    let _ = fs::remove_file(path);
                }
            }
        }
        fs::write(
            index_directory.join("operation.json"),
            journal.encode().expect("encode test journal"),
        )
        .expect("write operation journal");
    }

    fn capture_recovery_context(lock: &WorkspaceLockGuard) -> IndexJournalRecoveryContext {
        capture_index_journal_recovery_context(
            lock,
            sai_package::validation::PackageValidationContext::default(),
        )
        .expect("capture lock-bound recovery context")
    }

    fn setup(
        journal: &IndexOperationJournal,
        source_state: &FileState,
        index_after: bool,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        IndexJournalRecoveryContext,
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
        fs::write(workspace.0.join("sai.lock"), ROOT_LOCK_BYTES).expect("write workspace lock");
        let lock = WorkspaceLockGuard::acquire(&workspace.0, WorkspaceLockMode::Exclusive)
            .expect("acquire workspace lock");
        let context = capture_recovery_context(&lock);
        set_source_state(&workspace.0, source_state);
        write_index_state(&workspace.0, journal, index_after);
        (workspace, lock, context)
    }

    fn setup_source_states(
        journal: &IndexOperationJournal,
        source_states: &[FileState],
        index_after: bool,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        IndexJournalRecoveryContext,
    ) {
        assert_eq!(source_states.len(), journal.sources.len());
        let (workspace, lock, inventory) = setup(journal, &source_states[0], index_after);
        for (source, state) in journal.sources.iter().zip(source_states) {
            set_source_path_state(&workspace.0, &source.path, state);
        }
        (workspace, lock, inventory)
    }

    fn setup_recoverable_partial_index(
        journal: &IndexOperationJournal,
        source_state: &FileState,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        IndexJournalRecoveryContext,
    ) {
        let (workspace, lock, inventory) = setup(journal, source_state, true);
        for package_file in &journal.index_before.package_files {
            let path = workspace
                .0
                .join(".sai/index")
                .join(package_file.file.as_str());
            match &package_file.state {
                FileState::Absent => {
                    let _ = fs::remove_file(path);
                }
                FileState::RegularFile(bytes) => {
                    fs::write(path, bytes).expect("write before package index file");
                }
            }
        }
        (workspace, lock, inventory)
    }

    fn setup_two_package_recoverable_index(
        journal: &IndexOperationJournal,
    ) -> (
        TestWorkspace,
        WorkspaceLockGuard,
        IndexJournalRecoveryContext,
    ) {
        let workspace = TestWorkspace::new();
        fs::create_dir_all(workspace.0.join("src")).expect("create root source directory");
        fs::create_dir_all(workspace.0.join("packages/extra/src"))
            .expect("create member source directory");
        fs::write(
            workspace.0.join("sai.package"),
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n\
             requires { acme:extra @ ^1 from path \"packages/extra\" }\n",
        )
        .expect("write root package manifest");
        fs::write(
            workspace.0.join("packages/extra/sai.package"),
            "package acme:extra @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core, wasm32-wasi }\n",
        )
        .expect("write member package manifest");
        fs::write(workspace.0.join("sai.lock"), TWO_PACKAGE_LOCK_BYTES)
            .expect("write workspace lock");
        let lock = WorkspaceLockGuard::acquire(&workspace.0, WorkspaceLockMode::Exclusive)
            .expect("acquire workspace lock");
        let inventory = capture_recovery_context(&lock);
        for source in &journal.sources {
            let source_root = if source.package_root == "." {
                workspace.0.clone()
            } else {
                workspace.0.join(&source.package_root)
            };
            set_source_path_state(&source_root, &source.path, &source.planned_after);
        }
        write_index_state(&workspace.0, journal, true);
        for package_file in &journal.index_before.package_files {
            let path = workspace
                .0
                .join(".sai/index")
                .join(package_file.file.as_str());
            match &package_file.state {
                FileState::Absent => {
                    let _ = fs::remove_file(path);
                }
                FileState::RegularFile(bytes) => {
                    fs::write(path, bytes).expect("write before package index file");
                }
            }
        }
        (workspace, lock, inventory)
    }

    fn expected_decision(
        store: &IndexOperationJournalStore<'_>,
        journal: &IndexOperationJournal,
        inventory: &IndexJournalRecoveryContext,
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

    fn journal_bytes(workspace: &TestWorkspace) -> Vec<u8> {
        fs::read(workspace.0.join(".sai/index/operation.json")).expect("read operation journal")
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
    fn restore_before_restores_only_the_header_and_keeps_the_journal() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let second_package_file = &journal.index_before.package_files[1];
        let FileState::RegularFile(second_package_after) =
            &journal.index_after.package_files[1].state
        else {
            panic!("after package file is regular");
        };
        let second_package_after = second_package_after.clone();
        fs::write(
            workspace
                .0
                .join(".sai/index")
                .join(second_package_file.file.as_str()),
            &second_package_after,
        )
        .expect("restore second package file to the after state");
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let source_before = fs::read(workspace.0.join("src/lib.sai")).expect("read source");
        let member_source_before =
            fs::read(workspace.0.join("packages/extra/src/lib.sai")).expect("read member source");
        let package_file = &journal.index_before.package_files[0];
        let FileState::RegularFile(package_before) = &package_file.state else {
            panic!("before package file is regular");
        };
        let package_before = package_before.clone();
        let header_before = match &journal.index_before.header {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("before header is regular"),
        };
        let journal_before = journal_bytes(&workspace);

        let result = execute_index_operation_journal_restore_header_before(
            &store, &journal, &inventory, planned,
        )
        .expect("restore before header");

        assert_eq!(result, IndexJournalHeaderRestoreExecution::Restored);
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read restored header"),
            header_before
        );
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(package_file.file.as_str())
            )
            .expect("read package file"),
            package_before
        );
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(second_package_file.file.as_str())
            )
            .expect("read untouched second package file"),
            second_package_after
        );
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read unchanged source"),
            source_before
        );
        assert_eq!(
            fs::read(workspace.0.join("packages/extra/src/lib.sai"))
                .expect("read unchanged member source"),
            member_source_before
        );
        assert_eq!(journal_bytes(&workspace), journal_before);
        let remaining_observation = store
            .observe_recovery(&journal, &inventory)
            .expect("observe remaining mixed package files");
        assert_eq!(
            remaining_observation.index_generation,
            IndexGenerationObservation::RecoverablePartial
        );
        assert_eq!(
            plan_index_operation_journal_recovery(&journal, &remaining_observation),
            IndexJournalRecoveryDecision::RestoreBefore,
            "the remaining mixed package files require a new planner pass"
        );
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
    fn restore_before_removes_a_header_whose_before_state_was_absent() {
        let mut journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        journal.index_before.header = FileState::Absent;
        journal.index_before.package_files[0].state = FileState::Absent;
        journal.symbol_mappings = vec![SymbolIdMapping {
            package_node_key: "package:acme:app@1.0.0".to_owned(),
            hex: SymbolHex::parse("000001").expect("valid symbol hex"),
            before_path: None,
            after_path: Some("acme:app.sample".to_owned()),
        }];
        journal
            .encode()
            .expect("empty-index before snapshot is a valid journal");
        let (workspace, lock, inventory) =
            setup_recoverable_partial_index(&journal, &journal.sources[0].planned_after);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let journal_before = journal_bytes(&workspace);

        let result = execute_index_operation_journal_restore_header_before(
            &store, &journal, &inventory, planned,
        )
        .expect("remove the planned-after header");

        assert_eq!(result, IndexJournalHeaderRestoreExecution::Restored);
        assert!(!workspace.0.join(".sai/index/index.json").exists());
        assert!(
            !workspace
                .0
                .join(".sai/index")
                .join(journal.index_before.package_files[0].file.as_str())
                .exists()
        );
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
        assert_eq!(
            expected_decision(&store, &journal, &inventory),
            IndexJournalRecoveryDecision::RestoreBefore
        );
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
    fn before_header_is_an_idempotent_noop_for_restore_before() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) =
            setup(&journal, &journal.sources[0].planned_after, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let index_before = index_files(&workspace);
        let journal_before = journal_bytes(&workspace);

        let result = execute_index_operation_journal_restore_header_before(
            &store, &journal, &inventory, planned,
        )
        .expect("report already-before header");

        assert_eq!(result, IndexJournalHeaderRestoreExecution::AlreadyBefore);
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
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
    fn restore_package_file_changes_only_the_selected_file_after_header_restore() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let initial_plan = expected_decision(&store, &journal, &inventory);
        assert_eq!(initial_plan, IndexJournalRecoveryDecision::RestoreBefore);
        assert_eq!(
            execute_index_operation_journal_restore_header_before(
                &store,
                &journal,
                &inventory,
                initial_plan,
            )
            .expect("restore before header"),
            IndexJournalHeaderRestoreExecution::Restored
        );

        let second_file = &journal.index_before.package_files[1].file;
        let FileState::RegularFile(second_after) = &journal.index_after.package_files[1].state
        else {
            panic!("second after package file is regular");
        };
        fs::write(
            workspace.0.join(".sai/index").join(second_file.as_str()),
            second_after,
        )
        .expect("restore second package file to the after state");
        let root_source_before =
            fs::read(workspace.0.join("src/lib.sai")).expect("read root source");
        let member_source_before =
            fs::read(workspace.0.join("packages/extra/src/lib.sai")).expect("read member source");
        let header_before =
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read before header");
        let first_file = &journal.index_before.package_files[0].file;
        let first_before = match &journal.index_before.package_files[0].state {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("first before package file is regular"),
        };
        let second_file = &journal.index_before.package_files[1].file;
        let second_before = match &journal.index_before.package_files[1].state {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("second before package file is regular"),
        };
        let journal_before = journal_bytes(&workspace);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);

        assert_eq!(
            execute_index_operation_journal_restore_package_before(
                &store, &journal, &inventory, first_file, planned,
            )
            .expect("already-before package file is idempotent"),
            IndexJournalPackageRestoreExecution::AlreadyBefore
        );
        let result = execute_index_operation_journal_restore_package_before(
            &store,
            &journal,
            &inventory,
            second_file,
            planned,
        )
        .expect("restore selected package file");

        assert_eq!(result, IndexJournalPackageRestoreExecution::Restored);
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read unchanged header"),
            header_before
        );
        assert_eq!(
            fs::read(workspace.0.join(".sai/index").join(first_file.as_str()))
                .expect("read unchanged first package file"),
            first_before
        );
        assert_eq!(
            fs::read(workspace.0.join(".sai/index").join(second_file.as_str()))
                .expect("read restored second package file"),
            second_before
        );
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read unchanged root source"),
            root_source_before
        );
        assert_eq!(
            fs::read(workspace.0.join("packages/extra/src/lib.sai"))
                .expect("read unchanged member source"),
            member_source_before
        );
        assert_eq!(journal_bytes(&workspace), journal_before);
        let remaining_observation = store
            .observe_recovery(&journal, &inventory)
            .expect("observe before generation");
        assert_eq!(
            remaining_observation.index_generation,
            IndexGenerationObservation::Before
        );
        assert_eq!(
            plan_index_operation_journal_recovery(&journal, &remaining_observation),
            IndexJournalRecoveryDecision::RestoreBefore
        );
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
    fn package_restore_waits_for_the_before_header() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let index_before = (
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read header"),
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(journal.index_after.package_files[1].file.as_str()),
            )
            .expect("read selected package file"),
        );
        let journal_before = journal_bytes(&workspace);

        let result = execute_index_operation_journal_restore_package_before(
            &store,
            &journal,
            &inventory,
            &journal.index_after.package_files[1].file,
            planned,
        )
        .expect("report that header rollback must happen first");

        assert_eq!(result, IndexJournalPackageRestoreExecution::HeaderNotBefore);
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read unchanged header"),
            index_before.0
        );
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(journal.index_after.package_files[1].file.as_str()),
            )
            .expect("read unchanged package file"),
            index_before.1
        );
        assert_eq!(journal_bytes(&workspace), journal_before);
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
    fn changed_header_after_observation_prevents_package_restore() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let initial_plan = expected_decision(&store, &journal, &inventory);
        assert_eq!(
            execute_index_operation_journal_restore_header_before(
                &store,
                &journal,
                &inventory,
                initial_plan,
            )
            .expect("restore before header"),
            IndexJournalHeaderRestoreExecution::Restored
        );
        let planned = expected_decision(&store, &journal, &inventory);
        let before_package = fs::read(
            workspace
                .0
                .join(".sai/index")
                .join(journal.index_after.package_files[1].file.as_str()),
        )
        .expect("read selected package file");
        let header_after = match &journal.index_after.header {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("after header is regular"),
        };

        let result = super::execute_index_operation_journal_restore_package_before_with_hook(
            &store,
            &journal,
            &inventory,
            &journal.index_after.package_files[1].file,
            planned,
            || {
                fs::write(workspace.0.join(".sai/index/index.json"), &header_after)
                    .expect("replace the before header after observation");
            },
        )
        .expect("header precondition prevents package mutation");

        assert_eq!(result, IndexJournalPackageRestoreExecution::HeaderNotBefore);
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(journal.index_after.package_files[1].file.as_str()),
            )
            .expect("read unchanged package file"),
            before_package
        );
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read raced header"),
            header_after
        );
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
    fn changed_lock_after_observation_prevents_header_restore() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, context) =
            setup_recoverable_partial_index(&journal, &journal.sources[0].planned_after);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &context);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let index_before = index_files(&workspace);
        let journal_before = journal_bytes(&workspace);

        let error = super::execute_index_operation_journal_restore_header_before_with_hook(
            &store,
            &journal,
            &context,
            planned,
            || {
                fs::write(workspace.0.join("sai.lock"), b"changed workspace lock\n")
                    .expect("change lock after observation");
            },
        )
        .expect_err("a changed lock must stop the index mutation");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::RecoveryContext(
                crate::index_operation_journal_validation::IndexJournalRecoveryContextError::LockSnapshotChanged
            )
        ));
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
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
    fn third_party_package_file_after_observation_is_preserved() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let initial_plan = expected_decision(&store, &journal, &inventory);
        execute_index_operation_journal_restore_header_before(
            &store,
            &journal,
            &inventory,
            initial_plan,
        )
        .expect("restore before header");
        let planned = expected_decision(&store, &journal, &inventory);
        let third_party = b"third-party package index\n";

        let error = super::execute_index_operation_journal_restore_package_before_with_hook(
            &store,
            &journal,
            &inventory,
            &journal.index_after.package_files[1].file,
            planned,
            || {
                fs::write(
                    workspace
                        .0
                        .join(".sai/index")
                        .join(journal.index_after.package_files[1].file.as_str()),
                    third_party,
                )
                .expect("replace package file after observation");
            },
        )
        .expect_err("conditional package update rejects third-party bytes");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::IndexFileChanged
        ));
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join(".sai/index")
                    .join(journal.index_after.package_files[1].file.as_str()),
            )
            .expect("read third-party package file"),
            third_party
        );
        assert!(journal_still_exists(&workspace));
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
    fn replaced_journal_after_observation_prevents_package_restore() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let initial_plan = expected_decision(&store, &journal, &inventory);
        execute_index_operation_journal_restore_header_before(
            &store,
            &journal,
            &inventory,
            initial_plan,
        )
        .expect("restore before header");
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);
        let selected_file = &journal.index_after.package_files[1].file;
        let selected_file_before =
            fs::read(workspace.0.join(".sai/index").join(selected_file.as_str()))
                .expect("read selected package file before journal replacement");
        let mut replacement = journal.clone();
        replacement.operation_id = "31d23303-74c7-4053-97e0-a8bc4bde874b".to_owned();
        let replacement_bytes = replacement.encode().expect("encode replacement journal");

        let error = super::execute_index_operation_journal_restore_package_before_with_hook(
            &store,
            &journal,
            &inventory,
            &journal.index_after.package_files[1].file,
            planned,
            || {
                fs::write(
                    workspace.0.join(".sai/index/operation.json"),
                    &replacement_bytes,
                )
                .expect("replace journal after observation");
            },
        )
        .expect_err("package update rejects changed journal");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::JournalChanged
        ));
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(
            fs::read(workspace.0.join(".sai/index").join(selected_file.as_str()))
                .expect("read selected package file after journal replacement"),
            selected_file_before
        );
        assert_eq!(journal_bytes(&workspace), replacement_bytes);
        assert!(journal_still_exists(&workspace));
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
    fn package_restore_removes_a_file_whose_before_state_was_absent() {
        let journal = journal_with_package_file_absent_before();
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].planned_after, true);
        let header_before = match &journal.index_before.header {
            FileState::RegularFile(bytes) => bytes.clone(),
            FileState::Absent => panic!("before header is regular"),
        };
        fs::write(workspace.0.join(".sai/index/index.json"), &header_before)
            .expect("publish before header");
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let file = &journal.index_before.package_files[0].file;
        assert!(workspace.0.join(".sai/index").join(file.as_str()).exists());

        let result = execute_index_operation_journal_restore_package_before(
            &store, &journal, &inventory, file, planned,
        )
        .expect("remove package file to recorded absence");

        assert_eq!(result, IndexJournalPackageRestoreExecution::Restored);
        assert!(!workspace.0.join(".sai/index").join(file.as_str()).exists());
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read before header"),
            header_before
        );
        assert!(journal_still_exists(&workspace));
        assert_eq!(
            expected_decision(&store, &journal, &inventory),
            IndexJournalRecoveryDecision::RestoreBefore
        );
        assert_eq!(
            execute_index_operation_journal_restore_package_before(
                &store,
                &journal,
                &inventory,
                file,
                IndexJournalRecoveryDecision::RestoreBefore,
            )
            .expect("already-absent package file is an idempotent no-op"),
            IndexJournalPackageRestoreExecution::AlreadyBefore
        );
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
    fn stale_or_non_restore_decisions_do_not_change_the_header() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) =
            setup_recoverable_partial_index(&journal, &journal.sources[0].planned_after);
        let store = IndexOperationJournalStore::new(&lock);
        let observed = expected_decision(&store, &journal, &inventory);
        assert_eq!(observed, IndexJournalRecoveryDecision::RestoreBefore);
        let index_after = index_files(&workspace);

        let stale = execute_index_operation_journal_restore_header_before(
            &store,
            &journal,
            &inventory,
            IndexJournalRecoveryDecision::ClearBefore,
        )
        .expect("report stale decision");
        assert_eq!(
            stale,
            IndexJournalHeaderRestoreExecution::DecisionChanged {
                planned: IndexJournalRecoveryDecision::ClearBefore,
                observed,
            }
        );
        assert_eq!(index_files(&workspace), index_after);

        let (before_workspace, before_lock, before_inventory) =
            setup(&journal, &journal.sources[0].before, false);
        let before_store = IndexOperationJournalStore::new(&before_lock);
        let clear_before = expected_decision(&before_store, &journal, &before_inventory);
        assert_eq!(clear_before, IndexJournalRecoveryDecision::ClearBefore);
        assert_eq!(
            execute_index_operation_journal_restore_header_before(
                &before_store,
                &journal,
                &before_inventory,
                clear_before,
            )
            .expect("report non-restore decision"),
            IndexJournalHeaderRestoreExecution::NotRestorable {
                decision: IndexJournalRecoveryDecision::ClearBefore,
            }
        );
        assert_eq!(
            index_files(&before_workspace),
            (
                match &journal.index_before.header {
                    FileState::RegularFile(bytes) => bytes.clone(),
                    FileState::Absent => panic!("before header is regular"),
                },
                match &journal.index_before.package_files[0].state {
                    FileState::RegularFile(bytes) => bytes.clone(),
                    FileState::Absent => panic!("before package file is regular"),
                },
            )
        );
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
    fn third_party_header_after_observation_is_preserved_as_a_conflict() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) =
            setup_recoverable_partial_index(&journal, &journal.sources[0].planned_after);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);
        let journal_before = journal_bytes(&workspace);
        let third_party = b"{\"third-party\":\"header\"}\n";

        let error = super::execute_index_operation_journal_restore_header_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            || {
                fs::write(workspace.0.join(".sai/index/index.json"), third_party)
                    .expect("replace header after observation");
            },
        )
        .expect_err("conditional header update must reject third-party bytes");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::IndexFileChanged
        ));
        assert_eq!(
            fs::read(workspace.0.join(".sai/index/index.json")).expect("read conflicting header"),
            third_party
        );
        let (_, package_after) = index_files(&workspace);
        assert_eq!(package_after, index_before.1);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
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
    fn changed_journal_after_observation_prevents_header_update() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) =
            setup_recoverable_partial_index(&journal, &journal.sources[0].planned_after);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);
        let mut replacement = journal.clone();
        replacement.operation_id = "f8547899-0982-4d1f-85d6-9d3207deea14".to_owned();
        let replacement_bytes = replacement.encode().expect("encode replacement journal");

        let error = super::execute_index_operation_journal_restore_header_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            || {
                fs::write(
                    workspace.0.join(".sai/index/operation.json"),
                    &replacement_bytes,
                )
                .expect("replace journal after observation");
            },
        )
        .expect_err("header update must reject a changed journal");

        assert!(matches!(
            error,
            IndexOperationJournalStoreError::JournalChanged
        ));
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), replacement_bytes);
        assert_eq!(
            store.read().expect("read replacement journal"),
            Some(replacement)
        );
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn source_parent_replaced_by_symlink_after_observation_cannot_redirect_restore() {
        let journal = two_source_journal();
        let source_states = [
            journal.sources[0].planned_after.clone(),
            journal.sources[1].before.clone(),
        ];
        let (workspace, lock, inventory) = setup_source_states(&journal, &source_states, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        let index_before = index_files(&workspace);
        let outside = workspace.0.join("outside");
        fs::create_dir_all(&outside).expect("create outside directory");
        fs::write(outside.join("lib.sai"), b"after").expect("write outside sentinel");

        let error = super::execute_index_operation_journal_restore_source_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            0,
            || {
                fs::rename(workspace.0.join("src"), workspace.0.join("src-captured"))
                    .expect("move original source directory");
                std::os::unix::fs::symlink(&outside, workspace.0.join("src"))
                    .expect("replace source directory with symlink");
            },
        )
        .expect_err("source update must reject a replaced symlink parent");

        assert!(matches!(
            error,
            IndexJournalSourceRestoreError::SourceUpdate(
                sai_package::local_source::LocalPackageSourceFileUpdateError::UnsafePathComponent
            )
        ));
        assert_eq!(
            fs::read(outside.join("lib.sai")).expect("read outside sentinel"),
            b"after"
        );
        assert_eq!(index_files(&workspace), index_before);
        assert!(journal_still_exists(&workspace));
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
    ))]
    #[test]
    fn changed_lock_after_observation_prevents_source_restore() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, context) = setup(&journal, &journal.sources[0].planned_after, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &context);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let source_before = fs::read(workspace.0.join("src/lib.sai"))
            .expect("read source before attempted restore");
        let index_before = index_files(&workspace);
        let journal_before = journal_bytes(&workspace);

        let error = super::execute_index_operation_journal_restore_source_before_with_hook(
            &store,
            &journal,
            &context,
            planned,
            0,
            || {
                fs::write(workspace.0.join("sai.lock"), b"changed workspace lock\n")
                    .expect("change lock after observation");
            },
        )
        .expect_err("a changed lock must stop the source mutation");

        assert!(matches!(
            error,
            super::IndexJournalSourceRestoreError::Store(
                IndexOperationJournalStoreError::RecoveryContext(
                    crate::index_operation_journal_validation::IndexJournalRecoveryContextError::LockSnapshotChanged
                )
            )
        ));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read source after rejected restore"),
            source_before
        );
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
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
        not(any(
            target_os = "espidf",
            target_os = "horizon",
            target_os = "redox",
            target_os = "solaris",
            target_os = "vita"
        ))
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
    fn coordinator_restores_mixed_sources_and_packages_before_clearing() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let index_directory = workspace.0.join(".sai/index");
        let first_file = &journal.index_after.package_files[0];
        let FileState::RegularFile(first_after) = &first_file.state else {
            panic!("first after package file is regular");
        };
        fs::write(index_directory.join(first_file.file.as_str()), first_after)
            .expect("leave one package file at its planned-after state");
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        assert_eq!(
            store.read().expect("read original journal"),
            Some(journal.clone())
        );

        let result =
            execute_index_operation_journal_restore_before(&store, &journal, &inventory, planned)
                .expect("complete the full CLI rollback");

        assert_eq!(result, IndexJournalRestoreBeforeExecution::Cleared);
        assert!(!journal_still_exists(&workspace));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read restored root source"),
            b"before"
        );
        assert_eq!(
            fs::read(workspace.0.join("packages/extra/src/lib.sai"))
                .expect("read restored member source"),
            b"before extra source"
        );
        assert_eq!(
            fs::read(index_directory.join("index.json")).expect("read restored header"),
            match &journal.index_before.header {
                FileState::RegularFile(bytes) => bytes.clone(),
                FileState::Absent => panic!("before header is present"),
            }
        );
        for package_file in &journal.index_before.package_files {
            assert_eq!(
                fs::read(index_directory.join(package_file.file.as_str()))
                    .expect("read restored package file"),
                match &package_file.state {
                    FileState::RegularFile(bytes) => bytes.clone(),
                    FileState::Absent => panic!("fixture package files are present"),
                }
            );
        }
        assert_eq!(
            store.read().expect("read cleared journal"),
            None,
            "the journal has been removed only after the complete before state"
        );
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
    fn coordinator_resumes_after_header_and_one_package_file_were_restored() {
        let journal = journal_with_two_affected_package_files(
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared,
        );
        let (workspace, lock, inventory) = setup_two_package_recoverable_index(&journal);
        let store = IndexOperationJournalStore::new(&lock);
        let initial_plan = expected_decision(&store, &journal, &inventory);
        assert_eq!(initial_plan, IndexJournalRecoveryDecision::RestoreBefore);
        assert_eq!(
            execute_index_operation_journal_restore_header_before(
                &store,
                &journal,
                &inventory,
                initial_plan,
            )
            .expect("restore header before crash"),
            IndexJournalHeaderRestoreExecution::Restored
        );
        let second = &journal.index_after.package_files[1];
        let FileState::RegularFile(second_after) = &second.state else {
            panic!("second after package file is regular");
        };
        fs::write(
            workspace.0.join(".sai/index").join(second.file.as_str()),
            second_after,
        )
        .expect("leave second package file partially restored");
        let resumed_plan = expected_decision(&store, &journal, &inventory);
        assert_eq!(resumed_plan, IndexJournalRecoveryDecision::RestoreBefore);

        let result = execute_index_operation_journal_restore_before(
            &store,
            &journal,
            &inventory,
            resumed_plan,
        )
        .expect("resume rollback from the mixed state");

        assert_eq!(result, IndexJournalRestoreBeforeExecution::Cleared);
        assert!(!journal_still_exists(&workspace));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read restored source"),
            b"before"
        );
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
    fn coordinator_removes_package_file_with_absent_before_state() {
        let journal = journal_with_package_file_absent_before();
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].planned_after, true);
        let store = IndexOperationJournalStore::new(&lock);
        let index_directory = workspace.0.join(".sai/index");
        let FileState::RegularFile(before_header) = &journal.index_before.header else {
            panic!("before header is regular");
        };
        fs::write(index_directory.join("index.json"), before_header)
            .expect("restore header while package file remains");
        let target = &journal.index_after.package_files[0].file;
        assert!(index_directory.join(target.as_str()).exists());
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);

        let result =
            execute_index_operation_journal_restore_before(&store, &journal, &inventory, planned)
                .expect("remove the package file created by the planned operation");

        assert_eq!(result, IndexJournalRestoreBeforeExecution::Cleared);
        assert!(!index_directory.join(target.as_str()).exists());
        assert_eq!(
            fs::read(index_directory.join("index.json")).expect("read before header"),
            *before_header
        );
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read before source"),
            b"before"
        );
        assert!(!journal_still_exists(&workspace));
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
    fn coordinator_clears_a_completed_before_state_after_restart() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::ClearBefore);

        let result =
            execute_index_operation_journal_restore_before(&store, &journal, &inventory, planned)
                .expect("finish clearing after a crash between restore and clear");

        assert_eq!(result, IndexJournalRestoreBeforeExecution::Cleared);
        assert!(!journal_still_exists(&workspace));
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read unchanged source"),
            b"before"
        );
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
    fn coordinator_preserves_journal_for_a_client_pending_decision() {
        let journal = journal(
            IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::AwaitingClient,
        );
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::PendingClient);
        let index_before = index_files(&workspace);
        let journal_before = journal_bytes(&workspace);

        let result =
            execute_index_operation_journal_restore_before(&store, &journal, &inventory, planned)
                .expect("report that an awaiting client is not rollbackable");

        assert_eq!(
            result,
            IndexJournalRestoreBeforeExecution::NotRestorable {
                decision: IndexJournalRecoveryDecision::PendingClient,
            }
        );
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert!(journal_still_exists(&workspace));
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
    fn coordinator_stops_when_a_source_changes_outside_the_recorded_states() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) =
            setup(&journal, &journal.sources[0].planned_after, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::RestoreBefore);
        let journal_before = journal_bytes(&workspace);
        let index_before = index_files(&workspace);
        fs::write(workspace.0.join("src/lib.sai"), b"third-party bytes")
            .expect("introduce an unexpected source state");

        let result =
            execute_index_operation_journal_restore_before(&store, &journal, &inventory, planned)
                .expect("stop cleanly on the changed planner decision");

        assert_eq!(
            result,
            IndexJournalRestoreBeforeExecution::DecisionChanged {
                planned: IndexJournalRecoveryDecision::RestoreBefore,
                observed: IndexJournalRecoveryDecision::Ambiguous(
                    IndexJournalRecoveryAmbiguity::UnexpectedSourceState
                ),
            }
        );
        assert_eq!(index_files(&workspace), index_before);
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read third-party source"),
            b"third-party bytes"
        );
        assert!(journal_still_exists(&workspace));
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
    fn coordinator_rechecks_before_clear_and_retains_journal_if_state_changes() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        let (workspace, lock, inventory) = setup(&journal, &journal.sources[0].before, false);
        let store = IndexOperationJournalStore::new(&lock);
        let planned = expected_decision(&store, &journal, &inventory);
        assert_eq!(planned, IndexJournalRecoveryDecision::ClearBefore);
        let journal_before = journal_bytes(&workspace);

        let result = super::execute_index_operation_journal_restore_before_with_hook(
            &store,
            &journal,
            &inventory,
            planned,
            || {
                fs::write(workspace.0.join("src/lib.sai"), b"third-party bytes")
                    .expect("change source after full before verification");
            },
        )
        .expect("report the fresh clear observation");

        assert_eq!(
            result,
            IndexJournalRestoreBeforeExecution::DecisionChanged {
                planned: IndexJournalRecoveryDecision::ClearBefore,
                observed: IndexJournalRecoveryDecision::Ambiguous(
                    IndexJournalRecoveryAmbiguity::UnexpectedSourceState
                ),
            }
        );
        assert_eq!(journal_bytes(&workspace), journal_before);
        assert_eq!(
            fs::read(workspace.0.join("src/lib.sai")).expect("read changed source"),
            b"third-party bytes"
        );
        assert!(journal_still_exists(&workspace));
    }

    fn execute_clear_only_with_hook_for_test(
        store: &IndexOperationJournalStore<'_>,
        journal: &IndexOperationJournal,
        context: &IndexJournalRecoveryContext,
        planned_decision: IndexJournalRecoveryDecision,
        after_observation: impl FnOnce(),
    ) -> Result<IndexJournalClearExecution, IndexOperationJournalStoreError> {
        super::execute_index_operation_journal_clear_with_hook(
            store,
            journal,
            context,
            planned_decision,
            after_observation,
        )
    }
}
