# Community Repair Café Planner

Community Repair Café Planner turns a fixed Saturday intake list into a
reviewable appointment plan. It screens safety status before reserving
volunteer time or parts, then assigns cleared repairs to qualified volunteers
inside the request's appointment window and the workshop's bench limit.

## Hand-checked report

The request fixture is deliberately shuffled. Requests are considered by
ascending priority number, then request ID. For each cleared request, the
planner chooses the earliest fitting appointment window and the lowest-ID
available volunteer with the required skill. The three one-hour windows begin
at 09:00, 10:15, and 11:30; each window has its own workbench limit.

```text
Community Repair Café — Saturday intake
Service order: priority, then request ID
#7001 Commuter bicycle: Bea / 09:00-10:00 (50 min)
#7002 Smoke-marked toaster: held — intake safety inspection required
#7010 Swollen e-bike battery: held — stop use and refer for qualified review
#7003 Denim jacket: Eli / 09:00-10:00 (40 min)
#7004 Table lamp: held — no appointment slot fits
#7005 Child scooter: Bea / 10:15-11:15 (45 min)
#7006 Wooden stool: Noor / 11:30-12:30 (50 min)
#7007 Second jacket: held — required shared part is unavailable
#7008 Rain umbrella: Eli / 10:15-11:15 (35 min)
#7009 Cracked glass jar: held — no volunteer has the required skill

Scheduled: 5 / 10; held for review or a later session: 5
Bench appointments: 2/2, 2/2, 1/1
Parts remaining: brake cables 0, zipper sliders 0, scooter wheels 0, plugs 1
```

The toaster has not passed an intake safety inspection, so it does not consume
a plug or occupy an appointment. The battery is referred without being
handled further by this fixture. The table lamp is cleared for work but is
limited to the already-full 09:00 window. The second jacket is held because
the first jacket consumed the café's only zipper slider.

## Modules and language concepts

- `model.sai` defines requests, volunteer skills, safety statuses, parts,
  appointment windows, assignments, and hold reasons.
- `data.sai` holds the shuffled Saturday intake, volunteer roster, windows,
  and shared parts inventory.
- `planner.sai` applies safety gates, priority order, volunteer eligibility,
  appointment capacity, and part reservations.
- `report.sai` renders a readable service report from the plan.
- `lib.sai` exposes the plan and report functions and includes regression
  cases for scheduling, holds, and resource totals.

The example combines fixed arrays, records and enums, multi-module imports,
mutable scheduling state, `Option`/`match`, stable ordering, two-dimensional
resource constraints, and explicit safety-related hold outcomes.

## Limits and review required

- This is an appointment allocation illustration, not a safety procedure.
  Intake statuses are trusted fixture data. A real café must define its own
  electrical, battery, tool, and referral rules with qualified safety
  reviewers.
- Every repair starts at the beginning of its selected one-hour appointment
  window. A volunteer can take at most one request in a window, and each
  window's bench count limits simultaneous appointments. The sample does not
  model staggered starts, setup or cleanup time, diagnosis duration, parts
  substitutions, donated-part quality, or cancellations.
- Volunteer skills, availability, request priorities, safety statuses, and
  the one-slider stock level are fictional choices. Café organizers and
  volunteers should review these choices and the hold reasons before adapting
  the policy.
- The deterministic greedy order does not search for a maximum-throughput
  schedule. It assumes unique request and volunteer IDs, valid window ranges,
  and fixture durations and quantities that fit in `U32`.

## Package and compiler status

This is a dependency-free `wasm32-core` library package. A host can print the
string returned by `report.plan_report()`. The output above is hand-checked
against the fixture; it is not compiler output. `sai fmt --check` verifies
source and manifest syntax and formatting, but not semantic correctness or
runtime behavior. In this checkout,
`sai check --offline --tests -C sample/community-repair-cafe-planner` stops at
the unimplemented Stage 0 checker and `@derive` feature gates, so semantic
checking and embedded test execution are not verified.
