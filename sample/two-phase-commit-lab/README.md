# Two-Phase Commit Lab

This bounded simulation follows a coordinator and three prepared participants
through a commit, a veto, and two coordinator crash points. It makes the
transaction's durable decision visible separately from how far that decision
has reached each participant.

## Demonstrations

When every participant votes yes, the coordinator writes one commit decision
before delivering it:

```text
account-a: Yes -> Committed
account-b: Yes -> Committed
inventory: Yes -> Committed
Coordinator decision: Commit (recorded)
Status: Complete
```

A single no vote forces an abort for every participant that prepared:

```text
account-a: Yes -> Aborted
account-b: No  -> Aborted
inventory: Yes -> Aborted
Coordinator decision: Abort (recorded)
Status: Complete
```

If the coordinator stops after collecting yes votes but before recording a
decision, it does not guess. Prepared participants remain in doubt:

```text
account-a: Yes -> InDoubt
account-b: Yes -> InDoubt
inventory: Yes -> InDoubt
Coordinator decision: Unrecorded
Status: InDoubt
```

If the coordinator stops after recording Commit but before sending any
decision messages, restart replays that same decision. The first retry reaches
two participants; the second retry finishes the last one:

```text
After decision log:      3 DecisionPending, Commit recorded
After first replay:      2 Committed, 1 DecisionPending, Commit recorded
After second replay:     3 Committed, Commit recorded
```

A restart with no durable decision leaves the original in-doubt report
unchanged. The replay API cannot manufacture a commit or abort from the votes.

## Model and invariants

- `src/model.sai` defines the four-slot bound, votes, crash points, decisions,
  participant states, and input scenarios.
- `src/coordinator.sai` validates participant identities and array shape,
  chooses Abort when any participant votes No, records the global decision,
  tracks decision delivery, and replays only a recorded decision. Its public
  `Report` type has private fields; read-only accessors expose the result while
  keeping callers outside this package from fabricating a decision for
  `resume`.
- `src/fixtures.sai` defines the three fictional participants and the commit,
  veto, pre-log crash, post-log crash, partial-replay, and invalid-input cases.
- `src/lib.sai` exposes reports and embeds assertions for each case.

The fixture assumes the coordinator has received one vote from each participant
before the modeled crash point. `decision_channel_open` applies to ordinary
decision delivery. A post-log crash delays all delivery; the replay fixture
then supplies which participant channels are reachable on each recovery pass.

## Review required

- The decision log is represented by one enum value. The example does not
  implement disk writes, flush ordering, torn-write recovery, replica quorum,
  coordinator election, or durable participant logs.
- Votes and prepare exchanges are already complete when `run` begins. Message
  duplication, reordering, lost prepare requests, lost votes, participant
  timeouts, and retry identifiers are outside the model.
- A No voter is locally Aborted even if the coordinator crashes before its
  global decision is recorded. Yes voters are InDoubt until a decision exists.
  Review this simplification before using the states to teach a particular
  database's prepared-transaction behavior.
- `DecisionPending` means a durable global decision exists but has not reached
  that participant. `InDoubt` means no global decision is recorded. Neither
  state is permission to make a unilateral commit decision.
- The recovery pass accepts a caller-provided availability array and reuses the
  recorded decision held by the coordinator-owned `Report` value. Private report
  fields prevent external packages from fabricating or editing that decision,
  but the sample still models redelivery rather than an independent storage
  reread.
- This is a finite state-transition teaching fixture, not an implementation of
  a transaction coordinator or a claim of atomicity under real failures.

## Package and compiler status

This is a dependency-free `wasm32-core` library. Run `sai fmt --check
sample/two-phase-commit-lab` from the repository root to check parsing and
formatting. The embedded assertions describe the intended model behavior; the
current compiler's Stage 0 feature gate and Stage 3 test-runner gate prevent
semantic checking and execution of these tests. The traces above are hand
calculated from the fixtures, not compiler output.
