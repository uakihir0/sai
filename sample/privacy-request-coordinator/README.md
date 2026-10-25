# Privacy Request Coordinator

This fixed-fixture library coordinates a fictional subject's combined access and
erasure request across six data stores. It validates the request, discovers
matching records, preserves legal-hold and active-retention blockers, orders
eligible deletion work by declared dependencies, replays bounded action
receipts, and returns a typed report plus a readable summary.

The source contains no personal data, network calls, database adapters, or
deletion side effects. Its receipts describe simulated outcomes only.

## Fixture result

The request covers subject `501`; the fixture clock is minute `1500`. Discovery
finds six matching records and excludes one record belonging to another
subject.

| Record | Store | Result | Attempts |
|---:|---|---|---:|
| 8101 | Search index | Deleted before the profile record | 1 |
| 8102 | Marketing platform | Deleted after one retry; the operation key stayed stable | 2 |
| 8103 | Session cache | Retry budget exhausted | 2 |
| 8104 | Profile service | Blocked because a required derived-copy deletion failed | 0 |
| 8105 | Billing ledger | Retained through fixture minute 1600 | 0 |
| 8106 | Support archive | Blocked by a legal hold | 0 |

Expected summary: `NEEDS REVIEW`; 6 records discovered, 2 deleted, 1 retry
recovered, 1 retry budget exhausted, 1 active retention, 1 legal hold, and 1
dependency block. This is a hand-checked fixture expectation, not output
produced by running the SAI program.

The dependency graph requires search documents, marketing copies, and session
tokens to be removed before the profile record. The first two succeed, while
the session cache exhausts its two-attempt budget, so the profile deletion is
not attempted. The invoice and support conversation remain separately visible
as policy blockers; the coordinator never overrides either one.

## Modules

- `src/model.sai` defines the request, discovered records, dependency edges,
  deletion receipts, work states, and report.
- `src/fixtures.sai` supplies the synthetic request, seven catalog rows, three
  dependency edges, and five simulated receipts.
- `src/validation.sai` checks request identity, scope, IDs, and fixture time.
- `src/discovery.sai` selects matching subject records and flags duplicate
  catalog IDs before any plan can run.
- `src/planner.sai` blocks held or retained records and builds a stable
  topological order for eligible work. Duplicate or zero operation keys,
  missing dependency endpoints, and tasks with unresolved order fail closed.
- `src/execution.sai` validates per-record attempt numbers and operation keys,
  applies at most two simulated receipts per task, and blocks dependents whose
  prerequisites were not deleted. An extra receipt after a terminal result
  requires review without replacing that recorded result.
- The planner labels every task left without an order as
  `DependencyOrderUnresolved`; this can include a downstream task, so the label
  does not claim exact cycle membership. A held prerequisite keeps its original
  hold state, and its dependent action is blocked.
- `src/coordinator.sai` gates validation, discovery, planning, and receipt
  replay, then aggregates counts and a request status.
- `src/report.sai` renders a concise inventory, outcome, blocker, and retry
  summary.
- `src/lib.sai` exposes `fixture_report()` and `fixture_summary()` and declares
  source-level regression tests.

The project illustrates bounded records, fail-closed validation, catalog
integrity, topological ordering, stable idempotency keys, retries, legal holds,
retention deadlines, partial completion, and auditable status aggregation.

## Review required

- Confirm the identity-verification and scope-confirmation gates. Real intake
  needs a documented identity process, authorized representatives, and
  request-specific scope rules.
- Confirm that a legal hold always blocks the modeled erasure action and that
  the hold source is authoritative, current, and limited to the covered data.
- Confirm the retention deadline and boundary rule. In this fixture the
  restriction expires at minute `1600` (`now >= retained_until`); actual
  retention schedules, exceptions, and clock sources are policy decisions.
- Confirm whether an access response may list the existence and store location
  of held or retained records. The fixture includes those rows in its internal
  inventory, but does not create a user-facing disclosure package.
- Confirm the dependency edges against real data lineage. The example assumes
  the search index, marketing platform, and session cache must be cleared
  before deleting the profile record. Every edge must have both endpoints in
  the discovered records for this subject; a missing endpoint invalidates the
  plan before any action receipt can be applied.
- Confirm that the bounded planner's unresolved-order status is useful. It
  deliberately does not claim that every unranked task belongs to a cycle;
  downstream work can also remain unranked, and held prerequisites retain
  their hold status while dependent work is blocked.
- Confirm that a fixture-supplied operation key is stable and unique for each
  request/store record, and that two attempts are an acceptable teaching
  retry limit. A production adapter must use each service's documented
  idempotency and retry contract.
- Confirm which incomplete outcomes need legal or operational review. This
  sample reports `NEEDS REVIEW` after retry exhaustion and reports policy
  blockers separately instead of calling the request complete.
- Confirm the audit-retention policy for request evidence and action receipts.
  The sample does not model redaction, deletion of the coordinator's own logs,
  authorization, concurrency, deadlines, or durable recovery.

## Compiler status

This is a dependency-free `wasm32-core` library over in-memory fixture data.
The exact checks run in this worktree were:

- `target/debug/sai fmt --check sample/privacy-request-coordinator` passed.
- `target/debug/sai check -C sample/privacy-request-coordinator` parsed the
  source without syntax errors, then stopped at the unimplemented Stage 0
  `sai check` feature and unsupported `@derive` attributes.
- `target/debug/sai test --list -C sample/privacy-request-coordinator` stopped
  at the unimplemented Stage 3 `sai test` feature. The fourteen source-level test
  declarations therefore have not been executed.

The formatter validates syntax and canonical formatting; it does not typecheck
or execute the fixture.
