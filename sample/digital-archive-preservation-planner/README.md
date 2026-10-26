# Digital Archive Preservation Planner

This fixed-fixture planner builds a small preservation work queue for a
fictional community archive. It checks fixity and permission evidence, ranks
eligible migrations and replicas, then schedules them under separate task and
byte limits. The report returns in original catalog order so each scheduled,
held, deferred, or no-action item stays easy to match to its input.

The package reads no files and contacts no repository, storage service,
rights registry, or migration tool. A scheduled task is a plan only; it does
not copy or transform content.

## Hand-checked fixture result

The fixture contains eight objects and sets the minimum to two independent
copy domains. Migration has a 300-byte budget and a two-task limit;
replication has a 220-byte budget and a two-task limit. Each task is charged
one object's `size_bytes`. Candidates are ordered by format risk (`Obsolete`,
then `Watch`, then `Stable`), fewer existing copy domains, then record ID.
When an eligible non-obsolete item needs multiple copy domains, its replica
tasks form one all-or-nothing group: the planner schedules the group only when
both its full byte cost and task count fit.

| Input order | Record | Risk / fixity | Size | Existing domains | Outcome | Reason |
|---:|---|---|---:|---:|---|---|
| 1 | 4202 North inlet field recording | Obsolete / verified | 280 | 2 | Deferred migration | 4208 has higher queue priority and leaves only 110 bytes. |
| 2 | 4208 Harbor oral history master | Obsolete / verified | 190 | 1 | Scheduled migration | First eligible migration; 190 of 300 bytes. |
| 3 | 4205 Canal survey image sequence | Watch / verified | 240 | 1 | Deferred replication | After 4204, only 90 replication bytes remain. |
| 4 | 4201 Reading room policy scan | Stable / verified | 70 | 2 | Current | Already meets the two-domain minimum. |
| 5 | 4207 Tide gauge export | Obsolete / mismatch | 200 | 1 | Held | A fixity mismatch blocks planning any action. |
| 6 | 4206 Restricted harbor interview | Watch / verified | 150 | 1 | Held | Permission is still pending. |
| 7 | 4203 Pier inspection photographs | Stable / verified | 90 | 1 | Scheduled replication | Uses the 90 bytes left after the larger watch item is deferred. |
| 8 | 4204 Breakwater maintenance log | Watch / verified | 130 | 1 | Scheduled replication | Higher-ranked watch item; 130 of 220 bytes. |

Although 4204 appears last in the input, it is considered before 4205 because
both are `Watch` items and its lower record ID breaks the tie. It is scheduled
first. The 240-byte 4205 task does not fit the remaining 90 bytes, so it is
deferred and planning continues; the 90-byte stable item then fits exactly.
This is a deterministic greedy plan, not a global optimization.

A source-level threshold case raises the minimum to three domains. Record 4203
has one verified domain, so it requests two 90-byte replica tasks; both fit an
180-byte budget and a two-task limit. Record 4205 would need two 240-byte
tasks, so the whole group is deferred instead of scheduling only one copy.
Shortfalls above four replica tasks per item are held with
`ReplicationShortfallExceedsBound`.

Hand-calculated totals are **1 migration using 190 bytes**, **2 replica tasks
using 220 bytes**, **2 held items**, **2 deferred work items**, and **1 current
item**. Scheduled-action totals count operations; held, deferred, and current
totals count input items. No task is counted twice and the report still uses
the original eight-row input order.

## Planning rules

- Missing or mismatched fixity, zero verified copy domains, pending permission,
  or denied permission holds an item before it enters the queue. The first
  applicable finding is reported.
- An obsolete-format object needs a certified migration path. Without one, it
  is held instead of falling back to replication. Otherwise it receives one
  migration task. Other format classes request exactly as many replica tasks
  as the configured minimum exceeds their existing domain count, up to four.
- A non-obsolete object that meets the copy-domain minimum is reported as
  `Current`. Migration and replication are mutually exclusive for one item in
  one plan. Migration does not add a copy domain, so a migrated object can
  remain below the copy-domain minimum.
- Eligible tasks share one priority ordering: obsolete first, then watch, then
  stable; within a class, fewer copy domains precede more, and record ID breaks
  ties. A larger task that does not fit is deferred while the planner continues
  to later candidates.
- Migration and replication each have independent byte budgets and task
  limits. A deferred entry retains the full requested task count, but no part
  of its group is scheduled. The report totals scheduled operation counts and
  bytes separately from item outcome counts.
- Results are written back to the same positions as the input, even though the
  internal work queue is sorted.

## Modules and limits

- `src/model.sai` defines fixed-capacity input, action candidates, per-item
  outcomes and task counts, findings, and aggregate totals.
- `src/fixtures.sai` provides eight deliberately unsorted catalog records and
  the deterministic byte and task budgets.
- `src/preflight.sai` checks fixity, copy availability, permission, and the
  action required for each format risk.
- `src/queue.sai` applies the stable risk, copy-domain, and record-ID ordering.
- `src/planner.sai` schedules each eligible action and records deferred work
  without changing catalog order.
- `src/lib.sai` exposes `sample_plan()` and source-level assertions for the
  hand-calculated plan, holds, budgets, ordering, and task limits.

This is an illustrative `wasm32-core` package with fixed arrays and
fixture-backed scalar evidence. It assumes the input count fits the array,
record IDs are unique, byte sizes and budgets fit `U32`, and the fixture fields
are internally consistent. Replication supports at most four new domains for
one item in a plan; larger gaps are held. It does not read or verify object
bytes, compute checksums, identify storage failure domains, validate licenses,
test migration tools, estimate migration quality, model storage overhead,
detect duplicate IDs, reserve capacity in a real system, or execute scheduled
work. It also does not add overflow or malformed-input recovery for arbitrary
external values.

The assertions in `src/lib.sai` document expected behavior; they are not
claimed as executed tests. `sai fmt --check` checks source formatting, but the
current compiler's Stage 0 semantic `sai check` and `@derive` support are
incomplete, so type checking and assertion execution remain unverified.

## Review required

- The three format-risk classes and their strict order are fixture inputs. A
  real archive needs a documented format registry, designated risk owner, and
  review dates; a stable label must not be treated as a permanent guarantee.
- Only `Obsolete` creates a migration candidate. A `Watch` item that already
  meets the copy-domain minimum is reported `Current` by this planner, even
  though its format risk remains under observation. Confirm that a watch
  format should not trigger a migration task on its own.
- The queue uses greedy first-fit scheduling. It considers higher-risk work
  first, defers an item that does not fit, and continues to smaller items.
  This can schedule a lower-risk task while a larger higher-risk task remains
  deferred. Confirm whether that is preferable to strict risk blocking,
  deadline order, or an optimization objective for the institution.
- Obsolete items receive a migration task whenever permission, fixity, and a
  certified path pass, regardless of their current number of copies. They are
  not also replicated in the same plan. Confirm that migration should take
  precedence over another independent copy and that a missing path should
  block rather than fall back to replication. A successful migration does not
  itself increase independent-copy domains, so a separate planning pass may be
  needed to meet the replica threshold.
- Each requested replica is assumed to create one new independent copy
  domain. The planner schedules all missing replicas for an item together or
  defers the whole group; it never reports a partial group as complete.
  Shortfalls above four are held to keep this fixed planner bounded. Confirm
  the all-or-nothing grouping, capacity accounting, and four-task bound for a
  real repository.
- The planner treats the permission enum as authority for both migration and
  exact-copy replication. Actual rights, donor terms, privacy restrictions,
  and preservation exceptions can differ by action and repository.
- A count of two copy domains is used as a resilience threshold, but the
  fixture does not identify or validate domains. A real policy should define
  independence across administrators, credentials, regions, and failure
  modes, and distinguish verified replicas from merely recorded locations.
- The byte budget charges each task the object's declared size, with no
  transformation overhead, metadata, temporary space, or verification cost.
  Real capacity accounting may need separate read, write, workspace, and
  retention budgets.
