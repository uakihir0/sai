# Neighborhood Bike-share Rebalancer

Neighborhood Bike-share Rebalancer prepares a fixed morning rebalancing run.
It moves standard bicycles from stations above their targets to higher-need
stations, while preserving empty docks reserved for adaptive cycles. Each
pickup-and-dropoff trip has a bounded van payload and a stable stop order.

## Hand-checked plan

The fixture contains six stations. Destinations are processed by numeric
priority, then station ID; the donor with the lowest station ID is selected
first. One trip moves at most three standard bicycles, and the van returns
empty before the next trip.

```text
Neighborhood bike-share rebalancing plan
Stop order: destination priority, destination ID; donor station ID
Van payload: 3 standard bicycles per direct trip

1. Depot (#13) -> Market Square (#11): 3 bicycles
2. Depot (#13) -> River Library (#12): 2 bicycles
3. Hill Clinic (#15) -> East School (#14): 3 bicycles

Projected station inventory (standard bicycles; accessible docks reserved)
Market Square: 5 / 5 target; adaptive bikes in accessible docks 1 / 2; regular slots 5 / 7
River Library: 6 / 6 target; adaptive bikes in accessible docks 2 / 3; regular slots 6 / 8
Depot: 3 / 3 target; adaptive bikes in accessible docks 1 / 2; regular slots 3 / 10
East School: 4 / 4 target; adaptive bikes in accessible docks 0 / 1; regular slots 4 / 7
Hill Clinic: 4 / 4 target; adaptive bikes in accessible docks 2 / 2; regular slots 4 / 7
North Market: 3 / 5 target; adaptive bikes in accessible docks 2 / 2; regular slots 3 / 5

Trips: 3; bicycles moved: 8
Priority stations brought to target: 3 / 4
Unfilled standard-bike demand: 2
```

The trip list is a deterministic set of direct legs, not a distance-optimized
tour. North Market remains two bicycles short after all donor surplus is
reserved for earlier priorities.

## Modules and language concepts

- `model.sai` defines stations, trips, and the final plan.
- `data.sai` provides station inventory and the van payload limit.
- `planner.sai` selects recipients and donors, preserves accessible docking
  spaces, and records the projected inventory after each trip.
- `report.sai` renders the itinerary and station projection as text.
- `lib.sai` exposes the fixed plan and source-level regression cases.

The example uses records, fixed arrays, module imports, loops, mutable local
state, `Option`/`match`, stable priority and ID tie-breaks, and resource
reservation.

## Limits and review required

- This is a fixed, in-memory illustration. It does not use maps, travel
  distances, traffic, battery charging, service durations, van return time, or
  live dock reservations. Each recorded transfer is a separate direct trip
  that starts and ends with an empty van.
- The van carries standard bicycles only. Accessible bicycles remain in place;
  the configured accessible docks are reserved from standard-bike occupancy.
- Recipient priority, target inventory, donor eligibility, and the three-bike
  payload are teaching choices. Bike-share operators and accessibility
  advocates should review the target levels and dock-reservation policy before
  applying them to a real service.
- The planner is greedy. It can leave a station short even when another
  transfer ordering would produce a different result. It assumes station IDs
  are unique, dock counts are internally consistent, and fixture arithmetic
  fits in `U32`.

## Package and compiler status

This is a dependency-free `wasm32-core` library package. Its expected text is
hand-checked against the fixture; it is not claimed as compiler output.
`sai fmt --check` verifies source and manifest syntax and formatting, but does
not establish semantic correctness or runtime behavior. In this checkout,
`sai check --offline --tests -C sample/neighborhood-bike-share-rebalancer`
stops at the unimplemented Stage 0 checker and `@derive` feature gates, so
semantic checking and embedded test execution are not verified.
