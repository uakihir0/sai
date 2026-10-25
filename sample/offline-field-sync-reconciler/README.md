# Offline Field Sync Reconciler

This bounded library reconciles fictional work-order edits made by field
technicians while their devices were offline. It compares each edit's base
revision with the server journal, automatically merges edits to independent
fields, and leaves same-field conflicts visible for a person to resolve.

The fixture outcomes are hand-calculated examples, not program output:

```text
Work order  Outcome        Accepted edits       Review
4101        Merged         summary              none
4102        NeedsReview    none                 server changed status
4103        Merged         status, summary       none
4104        Merged         status               one exact retransmission
4105        PartialMerge   summary              due date delete/update race
4106        PartialMerge   status               collided IDs hold both copies; a distinct
                                                same-field edit conflicts, its replay counts once
4107        PartialMerge   summary              two offline due dates disagree
4108        PartialMerge   summary              one patch predates retained history

Summary: 3 merged, 4 partial merges, 1 needing review
Patches: 8 applied, 0 converged, 2 duplicate arrivals, 5 conflicts, 3 held
```

## Reconciliation rules

- A server journal is complete from `history_floor_revision` through
  `current_revision`; each field edit consumes exactly one revision. A local
  patch outside that retained window is held because the merge cannot prove
  that its field stayed untouched.
- Each patch carries its observed base revision and a typed field value.
  A patch for a field with no later server edit can merge even when another
  field changed on the server.
- If a field changed on the server after the patch's base revision, the
  server value is preserved and the patch is marked `RemoteFieldChanged`.
  Matching final values converge without another write.
- Distinct offline edits to the same field with different values are all held.
  In particular, clearing a due day does not silently erase a newer due day.
  Edits to other fields in that case may still merge.
- Event IDs are unique within a client and work order. An exact redelivery is
  counted once even if a different offline edit conflicts on the same field.
  Reusing an ID with a different payload holds every copy, and those copies
  remain conflict evidence for other edits to their field. Other independent
  patches remain eligible.
- A malformed or incomplete server journal prevents the whole batch from
  being reconciled. Duplicate work-order IDs also reject the batch before any
  patch is applied.
- Successful field changes advance the reported revision by one. The sample
  records decisions; it does not mutate a remote server or persist a journal.

## Modules and checks

- `src/model.sai` defines work orders, typed fields, revisions, changes, and
  bounded reports.
- `src/fixtures.sai` covers independent edits, remote and offline conflicts,
  exact redelivery, delete-versus-update, a reused event ID with a distinct
  conflicting edit and replay, and stale edits.
- `src/reconcile.sai` validates the retained journal and performs a
  field-level three-way merge.
- `src/audit.sai` preflights every case journal and work-order identity before
  aggregating results.
- `src/lib.sai` exposes the fixture audit and declares eleven regression tests.

The manifest has no external dependencies and targets `wasm32-core`. The
embedded regression declarations document expected Stage 3 behavior; the
current reference compiler has not built or run this sample.

## Decisions and review required

- **Decision:** Merge edits field by field. Preserve the current server value
  when the server and an offline client changed the same field to different
  values. **Reason:** A newer server edit must not be silently overwritten,
  while unrelated work can still make progress. **Review required:** Confirm
  that partial merge is appropriate for a work-order record.
- **Decision:** Hold an edit whose base revision is older than the complete
  retained journal, but continue to consider other valid edits in that upload.
  **Reason:** The stale edit cannot prove that its field stayed untouched, but
  that uncertainty does not invalidate other patches with their own valid
  bases. **Review required:** Confirm that the fixture's history-floor boundary
  and per-field hold policy are understandable.
- **Decision:** Scope event identity to `(client_id, event_id)`, count an exact
  payload replay once even if its first copy conflicts, and hold every copy
  when that key is reused with a different base, field, or value. Keep those
  colliding copies as same-field conflict evidence for other edits. **Reason:**
  A device-local counter may overlap another device's IDs, but a changed
  payload under one device's key is ambiguous and must not let a third edit
  bypass the conflict. **Review required:** Confirm that the replay-count and
  conflict-evidence rules match a useful offline-sync teaching model.
- **Decision:** Reject a batch before applying patches if it has duplicate
  work-order IDs or an incomplete server journal. **Reason:** Those conditions
  make the input identity or concurrency evidence unreliable. **Review
  required:** Confirm that this all-or-nothing preflight is clearer than
  processing unrelated work orders independently.
- **Decision:** Advance the result revision once per distinct accepted field
  change. **Reason:** The fixture models a simple append-only server journal
  where each accepted change has one auditable revision. **Review required:**
  Confirm that the revision policy is clear for readers.
- This example illustrates deterministic reconciliation only. Real offline
  sync also needs authenticated clients, durable idempotency storage,
  authorization checks, conflict resolution UI, transport retries,
  multi-record transactions, and privacy safeguards.

## Checks run

- The source formatter check and the `sai.package` manifest formatter check
  both passed.
- `sai check -C sample/offline-field-sync-reconciler` could not complete:
  current Stage 0 reports `FEATURE_NOT_IMPLEMENTED` for `@derive` and does not
  yet implement full `sai check`.
- `sai test --list -C sample/offline-field-sync-reconciler` could not run:
  the current compiler reports Stage 3 `sai test` as
  `FEATURE_NOT_IMPLEMENTED`.
- `git diff --check` passed.
