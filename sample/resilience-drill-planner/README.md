# Resilience Drill Planner

This fixed-fixture example chooses a small set of fictional tabletop drills to
cover weighted failure scenarios. Before a drill can enter the plan, a
preparation confirmation and an approved runbook are required, and its
participant count must fit the drill's team cap. The planner recalculates
uncovered scenario weight after every selection, then greedily chooses the
largest marginal gain, breaking ties by shorter duration and then lower drill
ID.

This is only a tabletop exercise-planning example. It does not carry out
disaster response, test a live service, verify that a system can recover, or
guarantee availability or resilience.

## Hand-checked fixture result

The fixture has four scenarios with a total weight of 15, a nine-hour budget,
and a limit of three drills. The drill records are deliberately not in
selection order.

| Drill | Coverage | Hours | Team / cap | Preparation | Runbook | Result |
|---|---|---:|---:|---|---|---|
| D6 (106) | Queue Backpressure (3), Schema Rollback (2) | 4 | 4 / 4 | Confirmed | Approved | Deferred; final marginal weight 0 |
| D3 (103) | Regional Power (5), Queue Backpressure (3) | 3 | 5 / 5 | Confirmed | Approved | Scheduled first; marginal weight 8 |
| D4 (104) | Regional Power (5), Credential Revocation (5) | 1 | 7 / 6 | Confirmed | Approved | Held; team exceeds cap |
| D1 (101) | Credential Revocation (5) | 2 | 4 / 4 | Confirmed | Approved | Scheduled second; marginal weight 5 |
| D7 (107) | Regional Power (5) | 1 | 3 / 4 | Not confirmed | Approved | Held; preparation is unconfirmed |
| D2 (102) | Regional Power (5), Schema Rollback (2) | 2 | 4 / 4 | Confirmed | Approved | Scheduled third; marginal weight 2 |
| D5 (105) | Queue Backpressure (3), Schema Rollback (2) | 1 | 3 / 4 | Confirmed | Not approved | Held; runbook approval is missing |
| D8 (108) | Schema Rollback (2) | 2 | 3 / 4 | Confirmed | Approved | Deferred; final marginal weight 0 |

At the start, D3's marginal weight is **5 + 3 = 8**, ahead of D2's 7 and
D1's 5. D6 starts at **3 + 2 = 5** points, then falls to 2 when D3 covers
Queue Backpressure. Selecting D3 leaves Credential Revocation at 5 and Schema
Rollback at 2. D1 is then selected for 5 points. D2, D6, and D8 now each have
2 points of marginal gain. D2 beats D6 because it takes two hours instead of
four; D2 beats the equally short D8 because ID 102 is lower than 108. After
D2, all scenarios are covered, so D6 and D8 each have zero final marginal
weight.

The resulting plan schedules **D3, D1, and D2** in that order, uses
**3 + 2 + 2 = 7 of 9 available hours**, and covers
**8 + 5 + 2 = 15 of 15 weighted points**.
D4, D5, and D7 are held during preflight. D6 and D8 are left out after
recalculation because their scenarios have already been covered.

## Planning rules

- Preflight checks, in order, that preparation is confirmed, the runbook is
  approved, the required team fits the candidate's own cap, and at least one
  scenario is covered. A failed check holds the candidate and records the
  first finding.
- Every planning round recomputes each remaining eligible candidate's
  marginal weight from scenarios not yet covered. It chooses the greatest
  weight, then the shorter duration, then the lower drill ID.
- A drill is selectable only when it has positive marginal weight, fits the
  remaining hour budget, and a drill slot remains. A candidate that does not
  fit does not prevent a smaller eligible candidate from being considered.
- After selection stops, candidates with no remaining marginal coverage are
  deferred as `CoverageAlreadyPlanned`. Other candidates are deferred for the
  drill-count limit or hour budget. Results stay in fixture input order, with
  a separate selection order for scheduled drills.
- If both the drill-count limit and hour budget block a candidate that still
  has positive marginal weight, the report records
  `DrillCountLimitReached` as the primary finding.
- The objective is raw uncovered weight, not coverage per hour. Each scenario
  contributes its weight at most once, no matter how many selected drills
  mention it.

## Modules and limits

- `src/model.sai` defines scenarios, coverage, drill readiness, candidate
  limits, results, and findings.
- `src/fixtures.sai` provides the four weighted scenarios and eight unsorted
  drill candidates.
- `src/preflight.sai` applies the readiness, approval, team-cap, and
  non-empty-coverage gates.
- `src/planner.sai` recalculates marginal gains and builds a stable greedy
  plan under the hour and drill-count limits.
- `src/lib.sai` exposes `sample_plan()`.
- `src/assertions.sai` records the expected totals, selection order,
  tie-breaks, holds, and budget and count-limit variants as source-level tests.

## Language features and compiler stage

The source illustrates package modules, record and enum types, fixed-size
arrays, `Option`, loops, `match`, helper functions, and source-level tests.
`sai fmt --check sample/resilience-drill-planner` passes. The current Stage 0
does not implement `sai check`, `@derive`, import and use resolution, type
checking, or test-only analysis, so these source assertions have not been
executed and successful type checking is unverified.

The package uses fixed in-memory fixtures and does not read files, contact
services, or execute an exercise. It assumes at most four scenarios and eight
drills, unique drill IDs, positive scenario weights and drill durations, and
well-formed coverage flags. Its fixed-width totals are assumed to fit `U32`.
The fixture requires each of the four scenario kinds exactly once.
It does not model calendar overlap, on-call availability, dependency ordering,
facilitator skills, exercise safety controls, scenario probabilities, or
whether a runbook or preparation confirmation is still current.

## Review required

- **Scenario weights and scope:** The four names and weights (5, 5, 3, 2) are
  illustrative fixture judgments, not measured incident likelihood or impact.
  A real program needs an accountable owner to define, review, and periodically
  refresh its scenario set and weights.
- **Readiness gates:** The fixture trusts two booleans for preparation
  confirmation and runbook approval. A real planner should define who can
  attest, what evidence is accepted, when approvals expire, and how
  scenario-specific runbooks are linked.
- **Team cap:** The cap is an independent per-drill participant limit. It does
  not reserve people across drills or check skills, shifts, fatigue, safety,
  or simultaneous exercises. Confirm the cap's meaning and whether team
  membership must be modeled.
- **Greedy policy:** The planner maximizes absolute uncovered weight and uses
  duration and ID only as tie-breakers. It does not search for a globally
  optimal portfolio, account for hours as a value-per-cost ratio, or explain
  trade-offs between coverage and staff time. Confirm that this objective is
  suitable before adapting it to real planning.
- **Combined capacity findings:** When the drill-count limit and hour budget
  both block a positive-gain candidate, the single recorded finding gives
  precedence to `DrillCountLimitReached`. The assertions cover this case.
  Confirm whether a real planning report should keep this precedence or expose
  both blockers.
- **Operational boundary:** An approved runbook and a scheduled tabletop are
  not evidence that production services recover successfully. Keep exercise
  planning separate from incident command, reliability measurements, and
  availability claims.
