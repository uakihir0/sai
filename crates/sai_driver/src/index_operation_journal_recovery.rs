//! Pure policy for reconciling a decoded index-operation journal.
//!
//! This module classifies already-observed state and never reads or writes
//! files. The filesystem layer must produce each source observation in the
//! exact order of `journal.sources`, and must verify complete index
//! generations and affected package-file snapshots before classifying them.

use sai_semantic::{FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal};

/// Exact observation of one source path named by the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservedFileState {
    /// The path could not be inspected safely or completely.
    Unavailable,
    /// The exact current bytes or absence of the path.
    Available(FileState),
}

/// Classification of the current committed index generation.
///
/// `RecoverablePartial` means the header exactly matches the before or
/// planned-after header, every affected package file (including unlisted
/// orphans) exactly matches its recorded before or planned-after snapshot,
/// and every unaffected file required by that header verifies against its
/// digest. The complete index is neither generation. A filesystem observer
/// must not use this value if any bytes are unexpected or any required file
/// is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexGenerationObservation {
    /// The before header, every package file it lists, and every affected
    /// package-file before snapshot (including an unlisted orphan or absent
    /// file) verify exactly.
    Before,
    /// The planned-after header, every package file it lists, and every
    /// affected package-file planned-after snapshot verify exactly.
    PlannedAfter,
    /// All affected files match their before or planned-after snapshots, but
    /// neither complete generation is committed.
    RecoverablePartial,
    /// The header or a required package file is unexpected or contradictory.
    Contradictory,
    /// The complete generation cannot be inspected safely or completely.
    Unavailable,
}

/// Already-observed filesystem facts supplied to the pure recovery planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexJournalRecoveryObservation {
    /// Source states in exactly the order of `journal.sources`.
    pub sources: Vec<ObservedFileState>,
    pub index_generation: IndexGenerationObservation,
}

/// Action selected by the journal recovery policy.
///
/// The executor must perform the selected action under the exclusive
/// workspace lock, revalidate its observations before mutation, and verify
/// the result before clearing the journal. This enum is a plan only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalRecoveryDecision {
    /// The exact before state is already present; clear the journal after
    /// revalidation.
    ClearBefore,
    /// The client rejected an apply-edit request and the exact before state is
    /// present; clear the journal after revalidation.
    ClearRejected,
    /// The exact planned sources and complete planned index are committed;
    /// clear the journal without replaying symbol mappings.
    ClearCommitted,
    /// Restore every source and affected index file to its before snapshot,
    /// verify the before state, and then clear the journal.
    RestoreBefore,
    /// Sources are planned-after; commit and verify the planned index, then
    /// clear the journal.
    CommitPlannedIndex,
    /// The client may still apply the edit; preserve the journal and wait.
    PendingClient,
    /// Preserve the journal and block index-backed operations.
    Ambiguous(IndexJournalRecoveryAmbiguity),
}

/// Why the current observations cannot be reconciled automatically.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalRecoveryAmbiguity {
    InvalidJournal,
    InvalidOriginState,
    SourceObservationCountMismatch,
    SourceUnavailable,
    UnexpectedSourceState,
    IndexUnavailable,
    ContradictoryIndex,
    SourcesIndexConflict,
    StateObservationConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceGeneration {
    Before,
    PlannedAfter,
    MixedRecorded,
    Unexpected,
    Unavailable,
}

/// Plans recovery using only a decoded journal and exact filesystem
/// observations. The source observation vector must correspond one-for-one
/// with the journal's source entries.
pub fn plan_index_operation_journal_recovery(
    journal: &IndexOperationJournal,
    observation: &IndexJournalRecoveryObservation,
) -> IndexJournalRecoveryDecision {
    if journal.sources.is_empty()
        || journal
            .sources
            .iter()
            .any(|source| source.before == source.planned_after)
    {
        return ambiguous(IndexJournalRecoveryAmbiguity::InvalidJournal);
    }
    if !valid_origin_state(journal.origin, journal.state) {
        return ambiguous(IndexJournalRecoveryAmbiguity::InvalidOriginState);
    }
    if observation.sources.len() != journal.sources.len() {
        return ambiguous(IndexJournalRecoveryAmbiguity::SourceObservationCountMismatch);
    }

    let sources = classify_sources(journal, observation);
    match sources {
        SourceGeneration::Unavailable => {
            return ambiguous(IndexJournalRecoveryAmbiguity::SourceUnavailable);
        }
        SourceGeneration::Unexpected => {
            return ambiguous(IndexJournalRecoveryAmbiguity::UnexpectedSourceState);
        }
        SourceGeneration::Before
        | SourceGeneration::PlannedAfter
        | SourceGeneration::MixedRecorded => {}
    }

    let index = observation.index_generation;
    match index {
        IndexGenerationObservation::Unavailable => {
            return ambiguous(IndexJournalRecoveryAmbiguity::IndexUnavailable);
        }
        IndexGenerationObservation::Contradictory => {
            return ambiguous(IndexJournalRecoveryAmbiguity::ContradictoryIndex);
        }
        IndexGenerationObservation::Before
        | IndexGenerationObservation::PlannedAfter
        | IndexGenerationObservation::RecoverablePartial => {}
    }

    match (journal.origin, journal.state) {
        (IndexJournalOrigin::Cli, IndexJournalState::IndexCommitted) => {
            if sources == SourceGeneration::PlannedAfter
                && index == IndexGenerationObservation::PlannedAfter
            {
                IndexJournalRecoveryDecision::ClearCommitted
            } else {
                ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict)
            }
        }
        (
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared | IndexJournalState::SourcesApplied,
        ) => match (sources, index) {
            (SourceGeneration::PlannedAfter, IndexGenerationObservation::PlannedAfter) => {
                IndexJournalRecoveryDecision::ClearCommitted
            }
            (SourceGeneration::Before, IndexGenerationObservation::Before) => {
                IndexJournalRecoveryDecision::ClearBefore
            }
            (
                SourceGeneration::Before
                | SourceGeneration::PlannedAfter
                | SourceGeneration::MixedRecorded,
                IndexGenerationObservation::Before | IndexGenerationObservation::RecoverablePartial,
            ) => IndexJournalRecoveryDecision::RestoreBefore,
            (_, IndexGenerationObservation::PlannedAfter) => {
                ambiguous(IndexJournalRecoveryAmbiguity::SourcesIndexConflict)
            }
            _ => ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict),
        },
        (IndexJournalOrigin::LspApplyEdit, IndexJournalState::Prepared)
        | (IndexJournalOrigin::LspReturnedEdit, IndexJournalState::Prepared) => {
            // A prepared journal may already have exposed an edit whose client response is
            // delayed. Until the coordinator durably orders the state transition before
            // exposure, recover it with the same conservative policy as awaiting_client.
            plan_awaiting_client(sources, index)
        }
        (IndexJournalOrigin::LspApplyEdit, IndexJournalState::AwaitingClient)
        | (IndexJournalOrigin::LspReturnedEdit, IndexJournalState::AwaitingClient) => {
            plan_awaiting_client(sources, index)
        }
        (IndexJournalOrigin::LspApplyEdit, IndexJournalState::ClientApplied) => {
            plan_client_applied(sources, index)
        }
        (IndexJournalOrigin::LspApplyEdit, IndexJournalState::ClientRejected) => {
            if sources == SourceGeneration::Before && index == IndexGenerationObservation::Before {
                IndexJournalRecoveryDecision::ClearRejected
            } else {
                ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict)
            }
        }
        (
            IndexJournalOrigin::LspApplyEdit | IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::IndexCommitted,
        ) => {
            if sources == SourceGeneration::PlannedAfter
                && index == IndexGenerationObservation::PlannedAfter
            {
                IndexJournalRecoveryDecision::ClearCommitted
            } else {
                ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict)
            }
        }
        _ => ambiguous(IndexJournalRecoveryAmbiguity::InvalidOriginState),
    }
}

fn plan_awaiting_client(
    sources: SourceGeneration,
    index: IndexGenerationObservation,
) -> IndexJournalRecoveryDecision {
    match (sources, index) {
        (SourceGeneration::Before, IndexGenerationObservation::Before) => {
            IndexJournalRecoveryDecision::PendingClient
        }
        (SourceGeneration::PlannedAfter, IndexGenerationObservation::PlannedAfter) => {
            IndexJournalRecoveryDecision::ClearCommitted
        }
        (
            SourceGeneration::PlannedAfter,
            IndexGenerationObservation::Before | IndexGenerationObservation::RecoverablePartial,
        ) => IndexJournalRecoveryDecision::CommitPlannedIndex,
        (_, IndexGenerationObservation::PlannedAfter) => {
            ambiguous(IndexJournalRecoveryAmbiguity::SourcesIndexConflict)
        }
        _ => ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict),
    }
}

fn plan_client_applied(
    sources: SourceGeneration,
    index: IndexGenerationObservation,
) -> IndexJournalRecoveryDecision {
    match (sources, index) {
        (SourceGeneration::PlannedAfter, IndexGenerationObservation::PlannedAfter) => {
            IndexJournalRecoveryDecision::ClearCommitted
        }
        (
            SourceGeneration::PlannedAfter,
            IndexGenerationObservation::Before | IndexGenerationObservation::RecoverablePartial,
        ) => IndexJournalRecoveryDecision::CommitPlannedIndex,
        _ => ambiguous(IndexJournalRecoveryAmbiguity::StateObservationConflict),
    }
}

fn classify_sources(
    journal: &IndexOperationJournal,
    observation: &IndexJournalRecoveryObservation,
) -> SourceGeneration {
    let mut saw_before = false;
    let mut saw_planned_after = false;
    for (source, observed) in journal.sources.iter().zip(&observation.sources) {
        let state = match observed {
            ObservedFileState::Unavailable => return SourceGeneration::Unavailable,
            ObservedFileState::Available(state) => state,
        };
        if state == &source.before {
            saw_before = true;
        } else if state == &source.planned_after {
            saw_planned_after = true;
        } else {
            return SourceGeneration::Unexpected;
        }
    }
    match (saw_before, saw_planned_after) {
        (true, false) => SourceGeneration::Before,
        (false, true) => SourceGeneration::PlannedAfter,
        (true, true) => SourceGeneration::MixedRecorded,
        (false, false) => SourceGeneration::Unexpected,
    }
}

fn valid_origin_state(origin: IndexJournalOrigin, state: IndexJournalState) -> bool {
    matches!(
        (origin, state),
        (
            IndexJournalOrigin::Cli,
            IndexJournalState::Prepared
                | IndexJournalState::SourcesApplied
                | IndexJournalState::IndexCommitted
        ) | (
            IndexJournalOrigin::LspApplyEdit,
            IndexJournalState::Prepared
                | IndexJournalState::AwaitingClient
                | IndexJournalState::ClientApplied
                | IndexJournalState::ClientRejected
                | IndexJournalState::IndexCommitted
        ) | (
            IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::Prepared
                | IndexJournalState::AwaitingClient
                | IndexJournalState::IndexCommitted
        )
    )
}

fn ambiguous(reason: IndexJournalRecoveryAmbiguity) -> IndexJournalRecoveryDecision {
    IndexJournalRecoveryDecision::Ambiguous(reason)
}

#[cfg(test)]
mod tests {
    use super::{
        IndexGenerationObservation as IndexGeneration, IndexJournalRecoveryAmbiguity as Ambiguity,
        IndexJournalRecoveryDecision as Decision, IndexJournalRecoveryObservation as Observation,
        ObservedFileState, plan_index_operation_journal_recovery,
    };
    use sai_semantic::{
        FileState, IndexJournalOrigin, IndexJournalState, IndexOperationJournal, IndexSnapshot,
        PackageFileState, PackageIndexFileName, SourceFileChange, SymbolHex, SymbolIdMapping,
    };
    use sai_stable_hash::hash_bytes;

    #[derive(Clone, Copy, Debug)]
    enum SourceCase {
        Before,
        PlannedAfter,
        Mixed,
        Unexpected,
        Unavailable,
        Missing,
    }

    fn journal(origin: IndexJournalOrigin, state: IndexJournalState) -> IndexOperationJournal {
        let file = PackageIndexFileName::parse("acme.user.1.jsonl").expect("valid index filename");
        let before_entry = index_entry("acme:user.greet");
        let after_entry = index_entry("acme:user.say");
        let journal = IndexOperationJournal {
            operation_id: "1c71050c-1e25-4acd-8a56-03af72c9ce7a".into(),
            origin,
            state,
            sources: (0..2)
                .map(|index| SourceFileChange {
                    package_node_key: "package:acme:user@1.0.0".into(),
                    package_root: ".".into(),
                    path: format!("src/lib{index}.sai"),
                    before: FileState::RegularFile(format!("before {index}\n").into_bytes()),
                    planned_after: FileState::RegularFile(format!("after {index}\n").into_bytes()),
                })
                .collect(),
            index_before: index_snapshot(file.clone(), before_entry),
            index_after: index_snapshot(file.clone(), after_entry),
            symbol_mappings: vec![SymbolIdMapping {
                package_node_key: "package:acme:user@1.0.0".into(),
                hex: SymbolHex::parse("000001").expect("valid symbol hex"),
                before_path: Some("acme:user.greet".into()),
                after_path: Some("acme:user.say".into()),
            }],
        };
        journal
            .encode()
            .expect("fixture is a valid decoded journal");
        journal
    }

    fn index_entry(path: &str) -> Vec<u8> {
        format!(
            "{{\"hex\":\"000001\",\"def_path_hash\":\"00000000000000000000000000000001\",\"path\":\"{path}\",\"kind\":\"fn\",\"fingerprint\":\"00000000000000000000000000000002\"}}\n"
        )
        .into_bytes()
    }

    fn index_snapshot(file: PackageIndexFileName, entries: Vec<u8>) -> IndexSnapshot {
        let digest = hash_bytes(&entries).to_hex();
        let header = format!(
            "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"{file}\",\"blake3\":\"{digest}\"}}]}}\n"
        )
        .into_bytes();
        IndexSnapshot {
            header: FileState::RegularFile(header),
            package_files: vec![PackageFileState {
                file,
                state: FileState::RegularFile(entries),
            }],
        }
    }

    fn observation(
        journal: &IndexOperationJournal,
        source_case: SourceCase,
        index_generation: IndexGeneration,
    ) -> Observation {
        let sources = match source_case {
            SourceCase::Before => journal
                .sources
                .iter()
                .map(|source| ObservedFileState::Available(source.before.clone()))
                .collect(),
            SourceCase::PlannedAfter => journal
                .sources
                .iter()
                .map(|source| ObservedFileState::Available(source.planned_after.clone()))
                .collect(),
            SourceCase::Mixed => journal
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    ObservedFileState::Available(if index == 0 {
                        source.before.clone()
                    } else {
                        source.planned_after.clone()
                    })
                })
                .collect(),
            SourceCase::Unexpected => journal
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    ObservedFileState::Available(if index == 0 {
                        FileState::RegularFile(b"third-party bytes".to_vec())
                    } else {
                        source.before.clone()
                    })
                })
                .collect(),
            SourceCase::Unavailable => journal
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    if index == 0 {
                        ObservedFileState::Unavailable
                    } else {
                        ObservedFileState::Available(source.before.clone())
                    }
                })
                .collect(),
            SourceCase::Missing => journal
                .sources
                .first()
                .map(|source| vec![ObservedFileState::Available(source.before.clone())])
                .unwrap_or_default(),
        };
        Observation {
            sources,
            index_generation,
        }
    }

    #[test]
    fn cli_recovery_rolls_back_uncommitted_generations_and_clears_exact_commits() {
        let cases = [
            (
                IndexJournalState::Prepared,
                SourceCase::Before,
                IndexGeneration::Before,
                Decision::ClearBefore,
            ),
            (
                IndexJournalState::SourcesApplied,
                SourceCase::Mixed,
                IndexGeneration::RecoverablePartial,
                Decision::RestoreBefore,
            ),
            (
                IndexJournalState::Prepared,
                SourceCase::PlannedAfter,
                IndexGeneration::Before,
                Decision::RestoreBefore,
            ),
            (
                IndexJournalState::SourcesApplied,
                SourceCase::PlannedAfter,
                IndexGeneration::PlannedAfter,
                Decision::ClearCommitted,
            ),
            (
                IndexJournalState::IndexCommitted,
                SourceCase::PlannedAfter,
                IndexGeneration::PlannedAfter,
                Decision::ClearCommitted,
            ),
        ];
        for (state, sources, index, expected) in cases {
            let journal = journal(IndexJournalOrigin::Cli, state);
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &journal,
                    &observation(&journal, sources, index)
                ),
                expected,
                "{state:?} {sources:?} {index:?}"
            );
        }
    }

    #[test]
    fn cli_does_not_roll_back_a_committed_index_or_clear_a_conflict() {
        let journal = journal(IndexJournalOrigin::Cli, IndexJournalState::IndexCommitted);
        assert_eq!(
            plan_index_operation_journal_recovery(
                &journal,
                &observation(&journal, SourceCase::PlannedAfter, IndexGeneration::Before,)
            ),
            Decision::Ambiguous(Ambiguity::StateObservationConflict)
        );
        assert_eq!(
            plan_index_operation_journal_recovery(
                &journal,
                &observation(&journal, SourceCase::Before, IndexGeneration::PlannedAfter,)
            ),
            Decision::Ambiguous(Ambiguity::StateObservationConflict)
        );
    }

    #[test]
    fn apply_edit_recovery_distinguishes_pending_applied_and_rejected_states() {
        let cases = [
            (
                IndexJournalState::AwaitingClient,
                SourceCase::Before,
                IndexGeneration::Before,
                Decision::PendingClient,
            ),
            (
                IndexJournalState::AwaitingClient,
                SourceCase::PlannedAfter,
                IndexGeneration::Before,
                Decision::CommitPlannedIndex,
            ),
            (
                IndexJournalState::AwaitingClient,
                SourceCase::PlannedAfter,
                IndexGeneration::PlannedAfter,
                Decision::ClearCommitted,
            ),
            (
                IndexJournalState::ClientApplied,
                SourceCase::PlannedAfter,
                IndexGeneration::RecoverablePartial,
                Decision::CommitPlannedIndex,
            ),
            (
                IndexJournalState::ClientRejected,
                SourceCase::Before,
                IndexGeneration::Before,
                Decision::ClearRejected,
            ),
        ];
        for (state, sources, index, expected) in cases {
            let journal = journal(IndexJournalOrigin::LspApplyEdit, state);
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &journal,
                    &observation(&journal, sources, index)
                ),
                expected,
                "{state:?} {sources:?} {index:?}"
            );
        }

        let rejected = journal(
            IndexJournalOrigin::LspApplyEdit,
            IndexJournalState::ClientRejected,
        );
        assert_eq!(
            plan_index_operation_journal_recovery(
                &rejected,
                &observation(&rejected, SourceCase::Before, IndexGeneration::PlannedAfter,)
            ),
            Decision::Ambiguous(Ambiguity::StateObservationConflict)
        );
    }

    #[test]
    fn returned_edit_recovery_keeps_before_snapshots_pending() {
        let awaiting = journal(
            IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::AwaitingClient,
        );
        assert_eq!(
            plan_index_operation_journal_recovery(
                &awaiting,
                &observation(&awaiting, SourceCase::Before, IndexGeneration::Before,)
            ),
            Decision::PendingClient
        );
        assert_eq!(
            plan_index_operation_journal_recovery(
                &awaiting,
                &observation(&awaiting, SourceCase::PlannedAfter, IndexGeneration::Before,)
            ),
            Decision::CommitPlannedIndex
        );
        let committed = journal(
            IndexJournalOrigin::LspReturnedEdit,
            IndexJournalState::IndexCommitted,
        );
        assert_eq!(
            plan_index_operation_journal_recovery(
                &committed,
                &observation(
                    &committed,
                    SourceCase::PlannedAfter,
                    IndexGeneration::PlannedAfter,
                )
            ),
            Decision::ClearCommitted
        );
    }

    #[test]
    fn prepared_lsp_journals_preserve_unresolved_client_edits() {
        for origin in [
            IndexJournalOrigin::LspApplyEdit,
            IndexJournalOrigin::LspReturnedEdit,
        ] {
            let prepared = journal(origin, IndexJournalState::Prepared);
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &prepared,
                    &observation(&prepared, SourceCase::Before, IndexGeneration::Before,)
                ),
                Decision::PendingClient
            );
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &prepared,
                    &observation(&prepared, SourceCase::PlannedAfter, IndexGeneration::Before,)
                ),
                Decision::CommitPlannedIndex
            );
        }
    }

    #[test]
    fn lsp_index_committed_state_requires_the_complete_planned_generation() {
        for origin in [
            IndexJournalOrigin::LspApplyEdit,
            IndexJournalOrigin::LspReturnedEdit,
        ] {
            let committed = journal(origin, IndexJournalState::IndexCommitted);
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &committed,
                    &observation(
                        &committed,
                        SourceCase::PlannedAfter,
                        IndexGeneration::PlannedAfter,
                    )
                ),
                Decision::ClearCommitted
            );
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &committed,
                    &observation(
                        &committed,
                        SourceCase::PlannedAfter,
                        IndexGeneration::Before,
                    )
                ),
                Decision::Ambiguous(Ambiguity::StateObservationConflict)
            );
        }
    }

    #[test]
    fn invalid_pairs_and_unverifiable_observations_are_ambiguous() {
        let invalid_pairs = [
            (IndexJournalOrigin::Cli, IndexJournalState::AwaitingClient),
            (IndexJournalOrigin::Cli, IndexJournalState::ClientApplied),
            (IndexJournalOrigin::Cli, IndexJournalState::ClientRejected),
            (
                IndexJournalOrigin::LspApplyEdit,
                IndexJournalState::SourcesApplied,
            ),
            (
                IndexJournalOrigin::LspReturnedEdit,
                IndexJournalState::SourcesApplied,
            ),
            (
                IndexJournalOrigin::LspReturnedEdit,
                IndexJournalState::ClientApplied,
            ),
            (
                IndexJournalOrigin::LspReturnedEdit,
                IndexJournalState::ClientRejected,
            ),
        ];
        for (origin, state) in invalid_pairs {
            let invalid_pair = journal(origin, state);
            assert_eq!(
                plan_index_operation_journal_recovery(
                    &invalid_pair,
                    &observation(&invalid_pair, SourceCase::Before, IndexGeneration::Before,)
                ),
                Decision::Ambiguous(Ambiguity::InvalidOriginState),
                "{origin:?} {state:?}"
            );
        }

        let lsp = journal(
            IndexJournalOrigin::LspApplyEdit,
            IndexJournalState::AwaitingClient,
        );
        for (sources, index, expected) in [
            (
                SourceCase::Mixed,
                IndexGeneration::Before,
                Ambiguity::StateObservationConflict,
            ),
            (
                SourceCase::Unexpected,
                IndexGeneration::Before,
                Ambiguity::UnexpectedSourceState,
            ),
            (
                SourceCase::Unavailable,
                IndexGeneration::Before,
                Ambiguity::SourceUnavailable,
            ),
            (
                SourceCase::PlannedAfter,
                IndexGeneration::Contradictory,
                Ambiguity::ContradictoryIndex,
            ),
            (
                SourceCase::Before,
                IndexGeneration::Unavailable,
                Ambiguity::IndexUnavailable,
            ),
        ] {
            assert_eq!(
                plan_index_operation_journal_recovery(&lsp, &observation(&lsp, sources, index)),
                Decision::Ambiguous(expected),
                "{sources:?} {index:?}"
            );
        }
        assert_eq!(
            plan_index_operation_journal_recovery(
                &lsp,
                &observation(&lsp, SourceCase::Missing, IndexGeneration::Before,)
            ),
            Decision::Ambiguous(Ambiguity::SourceObservationCountMismatch)
        );
        let mut empty_journal = journal(IndexJournalOrigin::Cli, IndexJournalState::Prepared);
        empty_journal.sources.clear();
        assert_eq!(
            plan_index_operation_journal_recovery(
                &empty_journal,
                &Observation {
                    sources: Vec::new(),
                    index_generation: IndexGeneration::Before,
                }
            ),
            Decision::Ambiguous(Ambiguity::InvalidJournal)
        );
    }
}
