# Facility Maintenance Queue

Facility Maintenance Queue schedules a fixed morning backlog across three
technicians. It checks trade qualifications, sequential shift availability,
due times, and shared spare-part stock before assigning each job.

```text
facility-maintenance-queue plan
Community facilities maintenance queue
Priority order: emergency, scheduled, routine; fixture order breaks ties
#3101 [emergency] Civic Center generator: Replace failed transfer-switch breaker
  Ren Sato, 420–480 minutes after midnight
#3102 [emergency] Northside water pump: Repair leaking feed valve
  Sol Okafor, 420–510 minutes after midnight
#3103 [scheduled] Library access lift: Complete safety inspection
  Sol Okafor, 510–555 minutes after midnight
#3104 [scheduled] East Annex roof: Patch a leak above the records room
  Amara Chen, 480–570 minutes after midnight
#3105 [routine] Room 12: Replace a sticking door latch
  Amara Chen, 570–600 minutes after midnight
#3106 [routine] Civic Center chiller: held — spare parts unavailable
Scheduled: 5; held: 1
```

Minutes are measured from midnight, so 420 is 07:00. The fixture is ordered
by urgency, then due time and ID. A technician is assigned the earliest
finishing eligible job; ties retain technician fixture order. The planner
does not split work orders or double-book a technician. Stock is reserved
only when a job is scheduled. The chiller needs two filters while the fixed
cupboard contains one, so the order stays visible as held.

## Project structure

- `model.sai` defines work orders, technician skills, stock, and outcomes.
- `data.sai` contains the fixed morning backlog, shifts, and parts inventory.
- `scheduler.sai` checks eligibility and reserves technician time and stock.
- `report.sai` prints the plan and remaining inventory.
- `lib.sai` exposes `plan` and `summary` command-line modes and regression
  examples.

## Limits

The fixture has no calendars, travel time, task dependencies, part
substitutions, emergency callouts, or multi-day planning. It assumes the
work-order fixture already has the intended urgency and due-time order. The
greedy earliest-finish rule does not search for a globally optimal schedule.

## Review required

- The due times, shift boundaries, skill combinations, job durations, and
  urgency labels are illustrative choices. Facilities staff should replace
  them with locally approved safety and service policies.
- The planner processes the listed priority order and does not reorder jobs
  to improve total throughput. This makes emergency work easy to audit but
  may leave a feasible job held when another order consumed the technician
  or part first.
- A missing or insufficient part holds the entire job. The sample does not
  model approved substitutes, partial kits, or emergency purchasing.

## Package and compiler status

The package uses the `sai:process` CLI entry-point shape and `wasm32-wasi`.
The documented `sai:process` Git source is currently unavailable, so package
resolution needs a matching toolchain bundle index or a published source
repository. The sample passed `sai fmt --check` in this checkout. An offline
`sai check` reaches the current Stage 0 check, Stage 3 dependency, and
`@derive` feature gates, so semantic checking, building, and execution are
not verified. The output is hand-checked against the fixture.
