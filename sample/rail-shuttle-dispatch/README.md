# Rail Shuttle Dispatch

Rail Shuttle Dispatch prepares a deterministic replacement-bus plan for a
fixed evening transit disruption. It assigns vehicles to service requests
while checking corridor coverage, passenger capacity, and wheelchair bays.
Unserved requests include the first constraint that prevents an assignment.

```text
rail-shuttle-dispatch plan
Transit disruption plan: Riverfront line suspension — 18:00 snapshot
Allocation order: priority 0–4, then request ID; best-fitting vehicle first

Service assignments
REQ-10 Civic Hall (EastLink, priority 0): unserved — wheelchair bay requirement exceeds corridor fleet
REQ-20 Harbor Hospital (EastLink, priority 1): East 101 — 18/20 seats, 2/2 wheelchair bays
REQ-21 Ferry Terminal (EastLink, priority 1): East 102 — 14/16 seats, 0/0 wheelchair bays
REQ-30 Senior Center (NorthLoop, priority 2): North 201 — 12/14 seats, 1/1 wheelchair bays
REQ-31 Airport (WestBranch, priority 2): West 301 — 24/24 seats, 0/0 wheelchair bays
REQ-40 University (NorthLoop, priority 3): North 202 — 22/22 seats, 0/0 wheelchair bays
REQ-50 Lakeside Homes (EastLink, priority 4): unserved — all compatible vehicles are assigned to higher-priority requests

Assigned 5 of 7 requests; 2 shortfalls
Passengers served: 90; unserved demand: 20
```

## Commands

- `plan` shows each request, its assigned vehicle, or its constraint shortfall.
- `shortfalls` lists only requests that could not be assigned and explains why.
- `fleet` prints the immutable vehicle fixture.

## Allocation policy

Requests are considered by ascending numeric priority (0 is most urgent), then
ascending request ID. Each request can use only a vehicle on the same corridor
with enough seats and wheelchair bays. Among available compatible vehicles,
the planner minimizes unused seats, then unused wheelchair bays, then vehicle
ID. A vehicle can serve at most one request.

The Civic Hall request needs three wheelchair bays, but the largest accessible
vehicle on EastLink has two, so it remains unserved. Harbor Hospital then gets
East 101 because it is the smallest compatible bus for 18 passengers. Ferry
Terminal takes East 102, and Lakeside Homes has no EastLink vehicle left after
the earlier requests. The tie-breaks make these results stable across runs.

`model.sai` defines requests, vehicles, and assignment results. `data.sai`
holds the disruption snapshot. `planner.sai` applies the hard constraints and
stable greedy selection without mutating the fixture. Its reservation lookup
recursively checks only requests that precede the current request, so each
lookup moves strictly earlier in the defined order. `report.sai` presents
assignments and shortfall reasons; `lib.sai` dispatches the command.

The scenario demonstrates enums, records, fixed arrays, module boundaries,
nested iteration, options and matches, multi-constraint filtering, deterministic
tie-breaking, and conflict reports. The priority-greedy policy is deterministic,
but it is not a global optimizer or a guarantee of fairness. This fixture
demonstrates constraint-based shortfalls; it does not claim that a different
fleet-wide assignment would serve more people.

## Assumptions and limits

This is a fixed, read-only planning illustration. Requests and fleet data are
already validated, request IDs are unique, all vehicles are available at the
snapshot time, and each vehicle can cover only its declared corridor. It does
not model travel time, return trips, operators, fuel, road closures within a
corridor, transfers, uncertain demand, partial loads, or dispatch updates. The
priority ranking and fit policy are teaching choices, not transit-service
guidance.

## Package and compiler status

The package targets `wasm32-wasi` and uses the `sai:process` command-line entry
point shape. The `sai:process` Git URL is currently unavailable, so package
resolution needs a matching toolchain bundle index or a published source
repository. This sample is illustrative: the current compiler does not yet
support the complete package, process/WASI entry point, command-line, loop,
formatting, `@derive`, and multi-module pipeline used here. No successful
check, build, or run is claimed. The output above is hand-checked against the
fixed fixture; the command dispatcher assumes the host includes the executable
name in `argv[0]`.
