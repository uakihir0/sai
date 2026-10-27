# Carbon-Aware Compute Window Planner

Carbon-Aware Compute Window Planner assigns a small, fixed batch of compute jobs
across three fictional regions and four one-hour windows. A placement must meet
the job's deadline, use a region permitted by its data policy, have a ready copy
of the required dataset in that region, and fit every hour's verified compute
capacity. Among contiguous windows with verified grid data, it selects the
one with the lowest fixture-estimated emissions.

The fixture horizon is 09:00–13:00. Its hand-checked report is:

| Job | Region | Window | Emissions | Unverified windows | Result |
|---|---|---|---:|---:|---|
| Secure customer export | north-hub | 10:00–11:00 | 240 gCO₂e | 0 | Scheduled |
| Release test suite | south-hub | 09:00–10:00 | 100 gCO₂e | 0 | Scheduled |
| Model retraining | south-hub | 09:00–11:00 | 360 gCO₂e | 0 | Scheduled |
| Catalog snapshot build | east-hub | 12:00–13:00 | 240 gCO₂e | 1 | Scheduled |
| Embedding backfill | north-hub | 10:00–12:00 | 360 gCO₂e | 0 | Scheduled |
| Audit dataset scan | east-hub | 11:00–12:00 | 750 gCO₂e | 1 | Scheduled |
| Regional telemetry repair | — | — | — | 0 | Held: the only ready dataset copy is outside allowed regions |
| Market stress simulation | — | — | — | 0 | Held: demand is above every south-hub slot capacity |
| Weekend analytics refresh | — | — | — | 0 | Held: three slots cannot finish by slot 2 |
| Sensor validation replay | — | — | — | 1 | Held: the only deadline window has unknown carbon intensity |

The six scheduled jobs reserve 5/8 and 2/8 kW in north-hub at 10:00 and
11:00; 5/5 and 3/5 kW in south-hub at 09:00 and 10:00; and 5/8 and 2/8 kW in
east-hub at 11:00 and 12:00. The summed fixture estimate is 2,050 gCO₂e.
For example, model retraining uses south-hub's 09:00 and 10:00 intensities
(50 + 70 gCO₂e/kWh) at 3 kW for two one-hour slots: 120 × 3 = 360 gCO₂e.
Embedding backfill uses north-hub at 10:00–12:00: (80 + 100) × 2 = 360 gCO₂e.
Three candidate windows have missing or unknown grid values. Two belong to jobs
that still have verified scheduled windows; the report counts these alternatives
so their selected windows are not presented as proven globally lowest-carbon
choices. The third is sensor validation's only window, so that job is held.

## Planning flow

1. `data.sai` provides ten jobs, the three-region catalog, fourteen ready
   dataset copies, and twelve region/slot grid cells. The east-hub 09:00 cell
   intentionally has no verified carbon intensity.
2. `planner.sai` rejects invalid or duplicate identities, unknown regions,
   invalid time windows, and duplicate grid cells before creating any decision.
   An absent grid cell and present-but-unknown capacity or intensity remain
   explicit uncertainty holds instead of being inferred from nearby hours.
3. Jobs are considered by earliest deadline, descending priority, then
   ascending ID. For each job, the planner scans allowed regions and every
   contiguous start time that can finish by its deadline. A region is eligible
   only if a ready copy of that job's dataset is already present there.
4. Every slot in a window must have known carbon intensity and capacity. Windows
   with missing or unknown grid values are omitted and counted on that job's
   decision, even when another verified window can be scheduled. The planner
   checks each slot's remaining capacity, calculates
   `intensity × demand × one hour` in grams, and chooses the lowest-emission
   feasible window. Ties go to the earlier start slot and then lower region ID.
5. Capacity is reserved immediately in dispatch order. If no candidate is
   feasible, the decision records a stable reason: region policy, missing or
   misplaced data, short deadline, missing grid cell, unknown intensity,
   unknown capacity, capacity, or arithmetic overflow.
6. `report.sai` summarizes scheduled and held jobs, total fixture emissions,
   unverified alternatives, uncertain-input holds, and capacity holds.
   `lib.sai` pins placements, emissions, slot reservations, invalid input
   rejection, unverified alternatives, and explicit uncertain outcomes.

This is a deterministic greedy scheduler for a small teaching fixture, not a
job-dependency planner or global optimizer. It plans independent jobs against
their existing data locations. Earlier deadline and priority decisions can
consume capacity that a lower-ranked job could otherwise use. Job decisions
remain in input order; `scheduled_order` exposes the separate dispatch order.

## Project structure

- `model.sai` defines fixed bounds, jobs, region and grid records, results, and
  stable hold reasons.
- `data.sai` contains the fictional jobs and their regional dataset copies.
- `planner.sai` validates the fixture and selects contiguous, capacity-bounded
  windows in deterministic dispatch order.
- `report.sai` derives a compact summary from the structured plan.
- `lib.sai` exposes `fixture_report()` and `fixture_summary()` with source-level
  contract tests.

## Review required

- Region permissions are a fixed three-boolean array indexed by catalog order;
  operators must verify that the policy actually represents the data's legal
  residency and processing constraints. A dataset replica marked ready is
  trusted without checking freshness, encryption, access controls, or integrity.
- Each compute unit is modeled as 1 kW and each slot is exactly one hour.
  Carbon intensity is an integer in gCO₂e/kWh, and estimated emissions are
  intensity multiplied by requested kW and one hour. Validate units, meter
  boundaries, embodied emissions, cooling overhead, and the grid factor's
  location and publication time before using real operational records.
- A job needs the same region and the same reserved demand for all consecutive
  slots. The example does not model preemption, partial progress, transfer time,
  startup energy, variable power, or queueing beyond this horizon.
- Jobs run in earliest-deadline, then priority, then ID order. Each job takes
  its locally lowest-carbon feasible window and reserves capacity immediately.
  This can hold later work even if a different batch-wide assignment would
  schedule more jobs or reduce total emissions. Confirm that this transparent
  greedy teaching rule is appropriate before using it to guide dispatch.
- Unknown grid cells, carbon intensities, and capacities cause holds if no
  other verified window works. A verified placement may therefore be selected
  while one or more unknown alternatives were excluded; the structured report
  counts those windows but does not estimate their possible emissions. Confirm
  that this is a useful balance between scheduling verified work and waiting
  for complete grid data. Hold-reason precedence is deterministic and may
  select one uncertainty reason when several missing inputs affect the same
  job. Confirm this reason ordering is useful for an operator's review queue.
- All values are fictional, and `Scheduled` means only that the in-memory
  fixture passes these simplified checks. The planner does not query a grid,
  launch workloads, move datasets, verify regional compliance, or guarantee
  real emissions or runtime.

## Package and compiler status

This is a pure `wasm32-core` library with bounded in-memory input and no external
services or persistent state. `sai fmt --check` passes. The attempted
`sai check --target wasm32-core --tests --offline` stops because the compiler
does not implement `sai check` past stage 0 and does not support `@derive` at
that stage. `sai test --target wasm32-core --offline` reports that stage 3
`sai test` is not implemented, so the source-level regression tests have not
run. The report above is hand-calculated from the fixtures, not
compiler-generated output.
