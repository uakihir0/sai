# Spaceflight Mission Operations Planner

This fixed-fixture SAI library builds a deterministic eight-hour activity
timeline for a fictional spacecraft. It demonstrates how a bounded planner can
combine task prerequisites, priority ordering, crew work windows, a single
shared crew resource, and per-slot power and thermal budgets while retaining a
reason for every blocked or unplaced activity.

This is a software-planning example, not flight software or operational
guidance. Its thresholds, task data, and decisions have no safety or mission
approval. The plan must not be used to operate a real spacecraft.

## Fixture timeline

Each slot represents 15 minutes of mission elapsed time. Intervals use a
start-inclusive, end-exclusive convention. The fixture has an eight-hour
horizon, a 6 W aggregate power cap, a 5-unit aggregate thermal cap, and two
crew windows: slots 4–14 and 20–30. Crew-required work uses one shared crew
resource, so those tasks cannot overlap. The planner has a hard 32-slot
maximum horizon; larger configurations are rejected before graph search.

The expected fixture report is:

| Order | Activity | Planned interval | Result |
|---:|---|---|---|
| 1 | Boot attitude sensors | 00:00–00:30 | Scheduled |
| 2 | Map payload-bay temperatures | 01:00–02:00 | Scheduled |
| 3 | Calibrate star tracker | 02:00–02:30 | Scheduled |
| 4 | Balance battery strings | 02:30–03:15 | Scheduled after earlier power conflicts |
| 5 | Capture cold-trap specimen | 05:00–05:45 | Scheduled after both prerequisites |
| 6 | Check relay link | 05:45–06:15 | Scheduled |
| 7 | Downlink science package | 06:15–07:00 | Scheduled in a crew window |
| 8 | Inspect radiator panel | — | Unplaced: no crew window can contain its 11-slot duration |
| 9 | Run high-gain oven calibration | — | Unplaced: its 7 W draw exceeds the 6 W capacity |
| 10 | Run thermal-soak test | — | Unplaced: its 6 thermal units exceed the 5-unit capacity |
| 11 | Seal specimen archive | — | Blocked: its radiator-inspection prerequisite was not scheduled |
| 12 | Exercise cycle | — | Unplaced: candidate windows conflict with crew, power, and thermal limits |

The lower numeric priority value wins; equal priorities use task ID. The
planner first produces a stable topological order using that rule among tasks
whose prerequisites are already ordered. It then tries start slots in
ascending order, beginning no earlier than the latest prerequisite finish.
Power and thermal usage are accumulated against their caps for every
overlapping slot without overflowing U32 totals. A task is scheduled only if
it fits entirely in one crew window when crew is required, the crew is free
throughout, and both resource caps hold for each slot. Horizons above the
fixed planning bound produce a planning issue and no task search; they are not
reported as graph failures.

The exercise cycle's combined issue records that crew, power, and thermal
constraints each rejected at least one in-window candidate. It does not imply
that the three limits were violated simultaneously at one candidate start.

## Module map

- `model.sai` defines mission tasks, dependency edges, crew windows, resource
  limits, graph findings, task outcomes, and the plan report.
- `data.sai` supplies twelve fictional activities, eight prerequisite edges,
  two crew windows, and the fixed power, thermal, and horizon limits.
- `ordering.sai` validates task identifiers and dependency edges, then
  computes a deterministic priority-aware topological order. Duplicate IDs,
  unknown endpoints, self-links, duplicate links, and cycles are reported.
- `planner.sai` propagates prerequisite failures, searches the bounded slot
  horizon, reserves one crew resource, checks overlapping power and thermal
  loads with cap-aware arithmetic, and records the scheduled interval or an
  explicit issue.
- `lib.sai` exposes `fixture_plan()` and source-level tests for ordering,
  resource-driven start times, deterministic results, failure reasons,
  oversized-horizon rejection, and overlapping `0x8000_0000` loads under
  `U32::MAX` caps.

## Use and compiler status

Inspect `fixture_plan()` in `src/lib.sai` and follow the search rules in
`src/planner.sai`. Run `sai fmt --check sample/spaceflight-mission-operations`
from the repository root to check canonical formatting.

`sai fmt --check` succeeds and confirms the source parses in the formatter.
The repository compiler currently reports `FEATURE_NOT_IMPLEMENTED` for the
Stage 0 `sai check` gate (and for `@derive` at this stage), and for the Stage 3
`sai test` gate. The planner therefore has not yet been type-checked or
executed by SAI. The expected table above is hand-derived from the fixture and
planner rules, not compiler output. The embedded SAI tests are ready for
execution when those compiler stages are available.

The project is a pure `wasm32-core` library with fixed-size arrays. It reads no
telemetry, performs no network access, and does not command or control a
vehicle. The planner uses a greedy earliest-fit search without backtracking,
so it reports a deterministic feasible proposal for this heuristic rather
than an optimal schedule.

## Review required

- **Planning horizon:** The hard maximum of 32 slots matches the fixed
  eight-hour fixture at 15 minutes per slot and bounds the search work. Longer
  missions must be divided into reviewed segments or the fixed bound and
  planner capacity must be deliberately redesigned.
- **Resource limits:** The 6 W cap and 5 thermal units are invented example
  values. A mission authority would need to define valid limits, units,
  margins, and whether transient or averaged measurements should govern.
- **Time model:** Fifteen-minute slots can hide shorter conflicts, setup time,
  task interruption, and synchronization needs. Slot size and half-open
  interval semantics are modeling choices that require mission review.
- **Crew model:** The fixture treats both windows as valid work periods and
  assumes one indivisible crew resource. It does not represent named
  crewmembers, qualifications, fatigue, medical constraints, or different
  staffing requirements.
- **Greedy ordering:** Lower numeric priority followed by task ID defines the
  schedule's tie-break policy. The earliest-fit greedy pass has no backtracking;
  a task may remain unplaced even when a different earlier choice could make
  all tasks fit. Mission owners should review priority rules and decide whether
  optimization or alternate-plan generation is required.
- **Dependency policy:** Each prerequisite must finish before the dependent
  activity begins. The model does not support partial dependencies, conditional
  branches, or work that can start after only one of several prerequisites.
- **Blocked versus unplaced:** A task with an unscheduled prerequisite is
  `Blocked`; a task whose own timing, crew, power, or thermal requirements
  cannot be satisfied is `Unplaced`. The first failed prerequisite blocks
  downstream work without attempting a speculative schedule.
- **Scope of the result:** Graph validation covers task IDs and prerequisite
  edges. The fixture does not represent telemetry freshness, uncertainty,
  emergency overrides, communication delays, command execution, or
  verification that a scheduled task actually completed.
