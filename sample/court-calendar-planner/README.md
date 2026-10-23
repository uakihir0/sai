# Court Calendar Planner

Court Calendar Planner audits a fixed proposed docket for a small courthouse.
It checks each hearing against a filing-based hearing deadline, judge and
courtroom availability, courtroom capacity, and overlapping assignments for
judges, rooms, parties, witnesses, and counsel. It reports blocked proposals
as unresolved and leaves them in place for a clerk to review.

The fixture has seven hearing proposals across three ordinal court days. One
proposal passes every check. Six remain unresolved:

- Park v. Harbor Works and Northline v. Market Square both fail because
  witness Morgan Reed is scheduled in overlapping time slots. Park also
  overlaps Blue v. Northbank in judge and courtroom.
- Fox v. Skyway Storage names a judge who is unavailable for its slot.
- Patel v. Granite Transit uses a courtroom marked unavailable, and its six
  listed attendees exceed that room's capacity of five.
- Rowan v. Metro Authority is set after its hearing deadline.
- Blue v. Northbank overlaps Park v. Harbor Works in both judge and courtroom.

```text
court-calendar-planner summary
Harbor County docket review
Proposed hearings: 7
Ready: 1
Unresolved: 6
Failed per-hearing checks: 10
```

```text
court-calendar-planner unresolved
Unresolved hearing proposals
Mon 09:00-10:00 | Park v. Harbor Works | Judge Avery | Courtroom Cedar | UNRESOLVED
  shared participant Morgan Reed also appears in hearing 302
  judge is also scheduled for hearing 307
  courtroom is also booked for hearing 307
Mon 09:30-10:30 | Northline v. Market Square | Judge Brooks | Courtroom Birch | UNRESOLVED
  shared participant Morgan Reed also appears in hearing 301
Tue 09:00-10:00 | Fox v. Skyway Storage | Judge Chen | Courtroom Cedar | UNRESOLVED
  judge unavailable for the proposed slot
Tue 11:00-12:00 | Patel v. Granite Transit | Judge Brooks | Courtroom Birch | UNRESOLVED
  courtroom unavailable for the proposed slot
  6 listed attendees exceed courtroom capacity 5
Wed 09:00-10:00 | Rowan v. Metro Authority | Judge Avery | Courtroom Cedar | UNRESOLVED
  hearing deadline is court day 2; proposal is court day 3
Mon 09:30-10:30 | Blue v. Northbank | Judge Avery | Courtroom Cedar | UNRESOLVED
  judge is also scheduled for hearing 301
  courtroom is also booked for hearing 301
```

## Commands

- `calendar` prints every proposed hearing and its ready or unresolved status.
- `audit` prints every proposal with each failed check.
- `unresolved` prints only proposals that need clerk review.
- `summary` totals proposals, ready outcomes, unresolved outcomes, and failed
  per-hearing checks.

The input is deliberately fixed. The planner does not move a hearing, search
for an alternate slot, choose a judge or room, or declare a blocked proposal
resolved. Each overlap is reported against both affected proposals, so the
summary counts a shared conflict once for each docket row it invalidates.

## Scheduling rules

- Court days are integer ordinals. A case's hearing deadline is its filing day
  plus its configured hearing window. A proposal on a later court day fails.
- A judge and a courtroom must each have an explicit availability row for the
  proposed slot, and that row must be marked available. Missing availability
  is treated as unavailable.
- Each `CaseParticipant` row represents one expected person in the courtroom,
  including parties, witnesses, and counsel. The number of rows must fit the
  assigned room's capacity.
- Time intervals are half-open: a hearing ending at 10:30 does not overlap one
  beginning at 10:30. Any other interval intersection is an overlap.
- Two overlapping hearings cannot share a judge, room, or participant person
  ID. Person IDs, rather than names, identify shared people.
- A proposal is ready only when every reference exists and every rule passes.
  Otherwise its outcome remains unresolved, with each failed check shown.

These are small, synthetic teaching rules. Court-day ordinals do not account
for weekends, holidays, jurisdiction-specific filing rules, continuances,
recusal, travel, or legal deadline computation. The fixture is an audit
example, not legal guidance or an operational court calendar.

## Project structure

- `model.sai` defines cases, court days, judges, rooms, participants, proposals,
  and the review result.
- `data.sai` provides the docket and explicit availability fixtures.
- `planner.sai` performs lookups, deadline and capacity checks, and pairwise
  overlap analysis.
- `report.sai` prints calendars and explains unresolved outcomes.
- `lib.sai` dispatches the command-line views and contains assertions for
  docket counts, participant/resource conflicts, and half-open intervals.

The example demonstrates cross-module data flow, fixed arrays, nested lookups,
option matching, interval arithmetic, participant-set intersection, resource
availability, capacity constraints, and per-proposal audit results.

## Package and compiler status

The manifest targets `wasm32-wasi` and uses the `sai:process` command-line
entry-point shape. The `sai:process` Git source has been unavailable in other
sample environments, so package resolution may require a matching toolchain
bundle index or a published source repository. This package is illustrative:
the current compiler may not support its complete package, process,
command-line, loop, formatting, derive, and multi-module pipeline. No
successful check, build, or run is claimed, and the embedded SAI assertions
have not been executed. The output above is hand-checked against the fixed
fixture.
