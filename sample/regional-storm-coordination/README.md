# Regional Storm Coordination

Regional Storm Coordination is a bounded tabletop plan for six response
missions during a fictional North Valley storm. The fixture combines road
clearance, shelter power, accessible evacuation, medical transport, water
delivery, and radio coverage. Missions share a directed road map, unit
availability, deadlines, load limits, and prerequisite work.

```text
regional-storm-coordination plan
North Valley regional storm coordination
Window: 06:00 onward; lanes are directed, and Willow Bridge opens after mission 801
#802 [critical] Energize Hilltop cooling center
  North Valley grid crew: 06:00–06:45; load 0
#803 [critical] Move Eastbank residents to Hilltop
  Accessible coach 12: 06:30–07:20; load 8
#804 [critical] Transfer dialysis patients to County Hospital
  held: requested load exceeds available unit capacity
#801 [high] Clear Willow Bridge approach
  Willow road crew: 06:00–06:30; load 0
#806 [high] Stand up Eastbank radio relay
  held: earliest feasible finish misses the due time or shift
#805 [standard] Deliver drinking water to Hilltop
  Water tender 3: 06:45–07:00; load 1000
Dispatched: 4; held: 2; Review complete: PASS
```

The schedule uses minutes after midnight internally. The mission list is
stable and priority-ordered; repeated scans let a dependent mission wait for
an earlier prerequisite without losing its place in the report. The bridge
approach is cleared at 06:30, after which the previously closed Eastbank to
Hilltop lane is usable. The accessible coach carries eight residents. The
medical van is staged at West Clinic in zone 4 and holds four people, so the
five-patient transfer has a feasible route but is held solely because the
request is too large rather than split. The radio relay's earliest finish is
07:05 against a 07:00 due time. Water delivery starts when the cooling
center's generator task finishes at 06:45. “Review complete” means the report
classifies every fixture mission as dispatched or held; it does not mean every
service was completed.

## Project structure

- `model.sai` defines missions, response units, road lanes, assignments, and
  hold reasons.
- `data.sai` provides the fixed priority-ordered mission list and unit roster.
- `routing.sai` validates directed lanes and unlocks the bridge after its
  clearance mission succeeds.
- `dispatch.sai` repeats stable readiness scans, tracks unit location and
  availability, applies load and time constraints, and records hold reasons.
- `evaluation.sai` checks classification completeness (every mission has an
  explicit dispatched or held result) and retrieves outcomes.
- `report.sai` prints the human-readable coordination plan and summary.
- `lib.sai` exposes the CLI and regression examples.

## Coordination rules

1. Requests are considered in fixture order: critical, high, then standard.
   A mission with a prerequisite is skipped until that prerequisite has been
   dispatched. The coordinator repeats at most six scans for the six
   missions, then reports any unresolved dependency as held.
2. A response unit must have the exact required capability and enough
   capacity for the entire load. Passenger groups are indivisible.
3. Travel is allowed only across a listed directed lane. A lane may begin
   closed and become usable when its named clearance mission is dispatched.
   The planner does not infer routes through multiple lanes.
4. A unit's next mission starts after its prior completion. Its finish must
   fit both the mission due time and the unit's shift end. Among feasible
   units, earliest finish wins; unit fixture order breaks ties.
5. A dispatched unit ends at the mission destination. A held mission does
   not reserve its unit or change its location.

## Limits

This is a fixed in-memory tabletop exercise, not a live dispatch system. It
does not read weather, road sensors, passenger manifests, emergency-service
availability, or utility telemetry. Travel is direct-lane only, all durations
and due times are assumed accurate, and each mission needs one unit. It does
not split loads, model uncertainty, or optimize the schedule globally.

## Review required

- The priority order lets earlier critical work consume a unit before a later
  critical request is considered. This is deterministic and easy to inspect,
  but it is not a fairness or life-safety optimization policy.
- The bridge-clearance mission is treated as sufficient evidence to reopen
  both directed bridge lanes. A real road authority would need to verify
  clearance and authorize reopening.
- The all-or-nothing transport capacity rule keeps groups together. A real
  coordinator may need an approved split plan, additional vehicles, or a
  different transfer destination.
- The medical van starts staged at West Clinic in zone 4 so its patient
  transfer route is feasible in this fictional fixture. A live plan would
  need current unit locations from dispatch telemetry.
- Loads, deadlines, crew skills, travel minutes, and service times are
  fictional scenario assumptions. They are not operational guidance.

## Package and compiler status

The package follows the `sai:process` command-line sample shape and targets
`wasm32-wasi`. The documented `sai:process` Git source is currently
unavailable, so package resolution needs a matching bundled package index or
a published source repository. The sample passed `sai fmt --check` in this
checkout. An offline `sai check` reaches the current Stage 0 check, Stage 3
dependency, and `@derive` feature gates, so semantic checking, building, and
execution are not verified. The CLI output above is hand-calculated from the
fixture.
