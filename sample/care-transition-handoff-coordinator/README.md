# Care Transition Handoff Coordinator

This fixed-fixture library identifies the next administrative handoff for six
fictional discharge cases. Each case has a clinician-supplied clearance flag
and a fixed sequence of pharmacy, transport, home-support, and follow-up
handoffs. The coordinator returns one next action and its owning team; it does
not send messages or release a person.

## Fixture behavior

The fixture clock is minute 600 (10:00). Team confirmations include an
expiration minute; evidence expiring before the fixture clock must be
refreshed. Cases are presented in a deliberately unsorted order and
coordinated by supplied priority, requested departure minute, then case ID.

| Case | Key input | Next action |
|---|---|---|
| 201 | Clinician clearance is still pending | Await the attending team |
| 202 | Pharmacy returned an incomplete handoff | Correct the pharmacy handoff |
| 204 | Pharmacy confirmation expired at minute 590 | Refresh pharmacy evidence |
| 203 | Pharmacy is current; transport response is pending | Await the transport team |
| 205 | Required confirmations are current; transport is explicitly not required | Ready for the discharge desk |
| 206 | Pharmacy handoff has not been requested | Create a pharmacy handoff |

Expected aggregate: one case ready for the discharge desk, two awaiting a
team, and three requiring a coordinator action. These are hand-checked
expectations, not output from an executed SAI program.

## Flow and modules

For each case, the workflow first checks the supplied clinician clearance.
Without it, no downstream handoff is opened. Otherwise it scans the four
component handoffs in order:

1. Pharmacy
2. Transport
3. Home support
4. Follow-up

An optional handoff is skipped even if its stored status is incomplete. The
first required handoff that is not currently confirmed stops the scan and
produces one of four actions: request it, wait for its response, correct a
returned packet, or refresh expired evidence. Only a case whose required
handoffs all pass is routed to the discharge desk.

- `model.sai` defines cases, teams, handoff states, actions, and aggregate
  results.
- `data.sai` supplies synthetic cases and the fixture clock.
- `policy.sai` orders cases deterministically.
- `workflow.sai` checks clinician clearance, walks the handoff sequence, and
  routes the first unresolved gate to its owning team.
- `coordinator.sai` processes cases in priority order and aggregates outcomes.
- `report.sai` renders a readable case-by-case report.
- `lib.sai` exposes the fixture plan and source-level regression cases.

The sample illustrates multiple modules, records and enums, fixed arrays,
stable ordering, `for` loops, early returns, `match`, optional component
handoffs, state transitions, and a typed handoff from one team to another.

## Limits

The coordinator only recommends the next owner and action from a supplied
snapshot. It does not contact teams, reserve a transport or home-care slot,
infer whether a discharge is clinically safe, or guarantee that evidence is
authentic. Its fixed stage order and single next action keep the example
deterministic, but may not reflect a facility's actual parallel workflow.

## Review required

- Clinical clearance is an input from the attending team. The coordinator
  never decides whether a person is medically ready to leave. Confirm that
  keeping this authority boundary explicit is appropriate for the teaching
  example.
- The required order—pharmacy, transport, home support, then follow-up—and
  the rule to stop at the first unresolved gate are illustrative. Local care
  pathways may allow parallel handoffs or different dependencies; review the
  order before adapting the example.
- A confirmation remains current through its recorded expiration minute;
  only a later fixture minute makes it stale. This inclusive boundary is a
  modeling assumption, not a facility evidence-retention policy.
- A handoff marked optional is skipped regardless of its stored status.
  Confirm that the caller, rather than this coordinator, is the authority
  that determines whether a component is required.
- Every case ID, label, priority, time, state, and confirmation is synthetic.
  The fixture contains no names, clinical details, or actual care guidance.

## Compiler status

This dependency-free project targets `wasm32-core`. `sai fmt --check` validates
source syntax and canonical formatting. `sai check --target wasm32-core --tests
--offline -C sample/care-transition-handoff-coordinator` currently stops at the
compiler's Stage 0 boundary: `sai check`, `@derive`, import resolution,
typechecking, and test analysis are not implemented. The source-level tests
are therefore not executed, and the fixture results remain hand-checked.
