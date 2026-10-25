# Community Garden Harvest Planner

This fixed-fixture library plans three days of harvest pickups from a shared
community garden. It matches ripe bed yields to pantry requests while
respecting request priority, pickup deadlines, and the daily capacities for
harvesting, wash-and-pack work, and same-day cold-chain handling.

`example_plan()` returns the individual pickup allocations and one outcome
for every pantry request. `example_summary()` returns totals that a caller can
render as a report. The illustrative request section below is sorted by ID for
readability; the plan itself retains outcomes in fixture order. The intended
report for the fixture is:

```text
Day 1
  North Pantry      tomatoes       10 kg
  South Kitchen     leafy greens    6 kg
Day 2
  School Pantry     tomatoes       16 kg
  Community Kitchen zucchini        2 kg
Day 3
  Senior Kitchen    beans          10 kg

Request #601 North Pantry: fulfilled 10 / 10 kg
Request #602 South Kitchen: partially planned 6 / 8 kg
Request #603 School Pantry: fulfilled 16 / 16 kg
Request #604 Community Kitchen: partially planned 2 / 10 kg
Request #605 Senior Kitchen: partially planned 10 / 12 kg
Request #606 East Pantry: 0 / 6 kg — daily handling capacity already reserved

Planned: 44 kg; unfilled: 18 kg
Requests: 2 fulfilled, 3 partial, 1 unplanned
Daily throughput: day 1: 16 kg, day 2: 18 kg, day 3: 10 kg
```

## Fixed scenario

Each bed has one crop, an available weight, a first-ready day, and a final
pick day. The final pick day is inclusive.

| Bed | Crop | Available | Ready | Final pick |
|---:|---|---:|---:|---:|
| 101 | Tomatoes | 18 kg | 1 | 1 |
| 102 | Tomatoes | 24 kg | 2 | 3 |
| 201 | Leafy greens | 12 kg | 1 | 1 |
| 301 | Zucchini | 14 kg | 1 | 2 |
| 401 | Beans | 10 kg | 2 | 3 |

The six pantry needs ask for 10 kg tomatoes by day 1, 8 kg leafy greens by
day 1, 16 kg tomatoes by day 2, 10 kg zucchini by day 2, 12 kg beans by day
3, and 6 kg leafy greens by day 1. Their priorities are Immediate, Standard,
or Flexible. The fixture stores the needs out of service order to exercise
the stable selection rule.

| Day | Harvest capacity | Wash-and-pack capacity | Cold-chain throughput |
|---:|---:|---:|---:|
| 1 | 22 kg | 20 kg | 16 kg |
| 2 | 24 kg | 22 kg | 18 kg |
| 3 | 20 kg | 18 kg | 12 kg |

## Planning rules and modules

The planner serves lower-numbered priority classes first (`Immediate`,
`Standard`, then `Flexible`), then earlier due day, then lower request ID. For
each request it checks days from 1 through its inclusive deadline and beds in
fixture order. A pickup amount is bounded by the request remainder, the
remaining yield in one ready bed, and the remaining capacity of all three
handling stages. Partial pickups are allowed, and an allocation for a request
and day combines the weight taken from any matching beds that day. Produce
and daily handling capacity are reserved as requests are planned; the planner
does not backtrack or rearrange an earlier allocation.

- `model.sai` defines crops, beds, pantry needs, capacities, allocations, and
  outcomes.
- `data.sai` provides the fictional garden, pantry, and volunteer-capacity
  fixture.
- `policy.sai` defines stable request ordering, harvest-window eligibility,
  and the minimum of the three daily handling limits.
- `planner.sai` selects needs, reserves bed yield and daily capacity, and
  records partial or unplanned outcomes.
- `report.sai` aggregates planned and unfilled kilograms and per-day totals.
- `lib.sai` exposes the fixture plan and summary API.

The sample illustrates records and enums, fixed arrays, options and matches,
source-ordered module imports, loops with mutable state, stable priority
selection, bounded resource reservation, and a separate reporting pass.

## Scope and compiler status

This is a deterministic, dependency-free `wasm32-core` illustration. It
assumes request IDs are unique, all due days are between 1 and 3, bed
availability is internally consistent, and each kg passes through harvest,
wash-and-pack, and cold-chain handling on the same day. Cold-chain capacity is
modeled as daily throughput, not persistent refrigerator occupancy. The
planner does not water crops, forecast yields, grade produce, model
volunteer skills or hours, store a pickup across days, allocate pantry stock
after delivery, or globally optimize all requests. The counts and output above
are hand-checked scenario expectations, not a claim that the sample has been
built or run.

The current compiler does not yet implement the complete semantic pipeline
needed for this multi-module project. `sai fmt --check` can check the source
format and parse the files; `sai check` may stop at the current compiler-stage
gate before it can validate the project semantically.

## Review required

- Confirm that priority, due-day, then request-ID ordering is an appropriate
  teaching policy, and that processing needs in that order without backtracking
  is clear to readers.
- Confirm that readiness and final-pick days are inclusive, pickups are due
  by the same day they are harvested, and partial allocations are acceptable.
- Confirm that measuring harvest, wash-and-pack, and cold-chain work as
  independent daily kilogram-throughput caps is a useful simplification; it
  does not model volunteer identities, skills, shift hours, or simultaneous
  cold-room occupancy.
- Confirm that out-of-order static fixture rows, stable bed order, and the
  three-day horizon make the example auditable. No real volunteer, pantry, or
  garden information is included.
