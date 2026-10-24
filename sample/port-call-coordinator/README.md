# Port Call Coordinator

This pure library schedules a fixed list of fictional vessel calls against
berths, tide windows, cranes, reefer plugs, hazmat permits, and a single
one-way channel. It returns one decision per valid call in a stable order.
Calls that cannot be placed receive an explicit reason. Invalid input rejects
the whole batch before scheduling any call.

The fixture and expected plan below are manually reconciled from the rules in
`planner.sai`; they are not compiler output.

## Hand-checked fixture result

Times are minutes after midnight. Inbound and outbound channel passages each
take 30 minutes. Crane service duration is the cargo tonnage divided by the
crane's hourly rate, rounded up to a whole minute.

| Order | Vessel | Result | Berth / crane | Inbound | Service | Outbound or reason |
|---:|---|---|---|---|---|---|
| 1 | Aster | Scheduled | Hazmat Pier / 203 | 10:00–10:30 | 10:30–12:18 | 12:18–12:48 |
| 2 | Brio | Scheduled | North Quay / 201 | 08:00–08:30 | 08:30–10:00 | 10:00–10:30 |
| 3 | Cobalt | Scheduled | Deep Pier / 204 | 12:48–13:18 | 13:18–14:27 | 14:27–14:57 |
| 4 | Delta | Scheduled | North Quay / 202 | 11:24–11:54 | 11:54–13:18 | 13:18–13:48 |
| 5 | Ember | Unplaced | — | — | — | ReeferPowerUnavailable |
| 6 | Fjord | Unplaced | — | — | — | NoSafeTideWindow |
| 7 | Gannet | Unplaced | — | — | — | HazmatPermitUnavailable |
| 8 | Harbor | Unplaced | — | — | — | NoCompatibleCrane |

Decisions are listed by ascending priority, ETA, then call ID. Aster's
11-metre draft requires the 10:00 tide window. Cobalt's oxidizer call uses the
adjacent Deep Pier only after Aster clears the neighboring Hazmat Pier at
12:48. Delta's outbound passage starts exactly when Cobalt's inbound passage
ends, so the two do not overlap in the shared channel. Brio and Aster may
overlap in time at the port because they use separate berths, cranes, and
non-conflicting hazmat classes.

## Modules

- `src/model.sai` defines the fixed-capacity input, resource, assignment,
  decision, error, and report types.
- `src/data.sai` holds the eight-call port fixture, four berths, six cranes,
  and three non-overlapping tide windows.
- `src/planner.sai` validates all counted records before indexing them,
  sorts calls deterministically, performs a priority-ordered greedy search,
  and records assignments or unplaced reasons.
- `src/lib.sai` exposes `sample_plan()` and embeds source-level regression
  tests for ordering, resource choices, channel and hazmat conflicts, and
  fail-closed input validation.
- `sai.package` declares a dependency-free `wasm32-core` library package.

## Scheduling and validation rules

- A call is considered in ascending `(priority, ETA, call ID)` order; lower
  numeric priority is more urgent. The scheduler tries ingress start minutes
  from ETA forward and selects the lowest berth ID, then lowest crane ID, at
  the first feasible minute.
- Berth length, draft, reefer capacity, and hazmat permit must match. A
  non-hazmat call can use any berth. A hazardous call must match the berth's
  single listed permit class.
- The entire 30-minute inbound passage must fit one tide window whose maximum
  draft covers the vessel. Crane service starts after inbound passage and
  when both berth and crane are open. The berth must stay open through
  outbound passage; the crane must stay open through service.
- Each crane handles one cargo kind. Service time is
  `ceil(cargo tons × 60 / crane tons per hour)` minutes.
- The channel is single-lane and one-way: no inbound or outbound passages may
  overlap any earlier assignment's inbound or outbound passage. Minute
  intervals are half-open, so a passage may start at the exact minute another
  passage ends.
- A berth is occupied from inbound start through outbound end. Its crane is
  occupied only during service. Overlapping assignments cannot reuse an
  occupied berth or crane.
- Hazardous calls on adjacent berths cannot overlap when their classes are
  Flammable/Oxidizer or Flammable/Toxic. Other class combinations are treated
  as compatible by this fixture.
- A whole input batch is rejected with zero decisions when a count exceeds
  fixed capacity, a counted slot is empty, an ID is duplicated, a call or
  resource record is inconsistent, a crane or neighbor references an unknown
  berth, berth-neighbor links are asymmetric, or tide windows overlap.
- The first reason category in this fixed precedence is returned when static
  placement is impossible: geometry, hazmat permit, reefer capacity, then
  cargo-compatible crane. If no tide can safely admit the call, the reason is
  `NoSafeTideWindow`. When feasible candidate windows are blocked, reason
  precedence is hazmat separation, channel occupancy, then berth or crane
  occupancy. Remaining timing failures report
  `NoFeasibleOperatingWindow`.

The scheduler is deterministic and greedy. It does not backtrack to rearrange
earlier assignments, split cargo, model partial service, or prove that a
globally complete schedule exists. An unplaced reason explains this planner's
bounded search, not a port authority's final operational decision.

## Review required

The fixture makes illustrative policy choices that need review before any
real operational use:

- Passage times are fixed at 30 minutes in each direction; channel direction
  changes, vessel speed, pilots, tug availability, safety buffers, and
  channel closures are not modeled. The half-open interval rule also assumes
  no extra channel headway or direction-change buffer.
- Crane rates are treated as exact constant rates. Weather, gang size, setup,
  cargo mix, operating breaks, and service uncertainty are omitted.
- Berths have one neighbor and one hazmat permit class. The fictional
  Flammable/Oxidizer and Flammable/Toxic incompatibility table is incomplete;
  real segregation distances and compatibility rules must come from the
  competent port and hazardous-materials authorities.
- Reefer power is a static plug count, without power load, outages, cable
  reach, or reservations outside this fixture.
- Priority-first greedy placement can leave a later call unplaced even when a
  different earlier assignment order could place more calls. Review the
  priority policy and scheduling objective with port operations.
- Input limits (eight calls, four berths, eight cranes, four tide windows),
  minute precision, measurement units, and the noon-like fixture timeline are
  chosen for a small deterministic example.

## Formatting and compiler status

Run the available source-format check with:

```sh
sai fmt --check sample/port-call-coordinator
```

That command passes on this sample. `sai check --offline --tests
-C sample/port-call-coordinator` currently stops at the unimplemented Stage 0
checker and `@derive` gate. `sai test --offline -C
sample/port-call-coordinator` stops at the unimplemented Stage 3 test runner.
The embedded test cases and fixture plan have not been semantically compiled
or executed; the table above is a manual calculation.
