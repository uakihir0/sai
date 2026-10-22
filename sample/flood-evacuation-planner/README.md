# Flood Evacuation Planner

Flood Evacuation Planner turns a small county's fixed incident snapshot into
an evacuation plan and shelter-capacity report:

```text
River County evacuation plan
Fixture order: critical, high, standard; household IDs ascend within each group
Hazard limit: 10 points

Linh Nguyen (4 people, critical): assigned to Clinic Annex; departs 06:00, arrives 06:25 (25 min)
Omar Patel (3 people, high): assigned to Central School; departs 06:10, arrives 06:40 (30 min)
Rosa Alvarez (4 people, high): assigned to West Stadium; departs 06:30, arrives 07:15 (45 min)
Kenji Mori (2 people, high): assigned to West Stadium; departs 06:45, arrives 07:27 (42 min)
Mae Carter (3 people, standard): assigned to Central School; departs 07:00, arrives 07:35 (35 min)
June Okafor (8 people, standard): on hold — no shelter can fit the whole household
Sam Rivera (2 people, standard): on hold — accessible places are unavailable
Tara Brooks (2 people, standard): on hold — every safe route misses the arrival deadline

Assigned: 5 households / 16 people
On hold: 3 households / 12 people
```

The shelter-capacity entry point reports occupied and remaining capacity at
each shelter, including accessible places and medical-support places:

```text
River County shelter capacity
Ridge Hall: people 0/7 (7 remaining), accessible 0/2 (2 remaining), medical 0/1 (1 remaining)
Central School: people 6/6 (0 remaining), accessible 0/1 (1 remaining), medical 0/0 (0 remaining)
West Stadium: people 6/9 (3 remaining), accessible 2/3 (1 remaining), medical 1/2 (1 remaining)
Clinic Annex: people 4/5 (1 remaining), accessible 2/2 (0 remaining), medical 1/3 (2 remaining)
```

The third entry point independently checks the generated plan:

```text
River County plan evaluation
Household coverage: PASS
Assignment counts: 5 placed, 3 held (PASS)
Shelter capacity checks: PASS
Open-route, hazard, and deadline checks: PASS
```

## Planning rules

- Households are processed in fixture order, which is critical, high, then
  standard priority and ascending household ID within each priority.
- A household is assigned as a whole. The planner never splits a household
  across shelters.
- A candidate route must be open, at or below the 10-point hazard limit, and
  arrive by that household's deadline. Among candidates with enough total,
  accessible, and medical-support capacity, the shortest travel time wins.
  Ties retain the first matching route in route-table order; that order
  follows the shelter fixture.
- An assignment consumes people, accessible places, and medical-support
  places at its selected shelter before the next household is considered.
- Held households receive the first applicable reason in this order:
  all routes closed, hazard limit, arrival deadline, whole-group capacity,
  accessible places, then medical-support places. Route hazard points are
  illustrative tabletop scores, not calibrated risk estimates.

The fixed data is deliberately small enough to inspect: five households are
placed, including two constrained households at West Stadium; three are held
for group size, accessible capacity, and arrival-time reasons. The final
totals are hand-checked against the fixture.

## Project structure and concepts

- `model.sai` defines household, route, shelter, placement, and plan records.
- `data.sai` supplies ordered households, four shelters, and the route table.
- `policy.sai` defines the hazard threshold and route eligibility rule.
- `planner.sai` evaluates each route and reserves multiple capacity types.
- `evaluation.sai` checks that every fixture household appears exactly once, along with plan totals, capacity limits, route safety, and deadlines.
- `report.sai` builds the human-readable evacuation and shelter reports.
- `lib.sai` exposes three human-readable report functions as the library interface.

This example combines multiple modules, fixed arrays, enums, route lookup,
stable fixture priority ordering, deadline checks, closed-road and hazard rules,
indivisible group assignment, resource reservation, coverage validation, and
derived reporting.
It is a read-only planning illustration. It does not load live road or shelter
data, optimize fairness across communities, model uncertainty, transport
vehicles, persist assignments, or contact households.

## Package and compiler status

The package targets `wasm32-core` and allows only the `alloc` capability used
to construct report strings. `sai fmt --check sample` parsed and formatted the
complete sample tree. `sai check --offline --target wasm32-core` reaches the
current Stage 0 `sai check` and `@derive` feature gates, so semantic checking,
building, and execution are not verified. The report shown above is
hand-checked against the fixed fixtures.
