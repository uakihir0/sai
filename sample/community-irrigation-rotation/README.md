# Community Irrigation Rotation

Community Irrigation Rotation prepares a read-only watering schedule for a
shared community garden during a drought restriction. It prioritizes urgent
crop requests, accounts for field and canal losses, fits whole requests into
zone-specific flow windows, and keeps total withdrawals within a weekly
allotment.

## Intended commands

- `community-irrigation-rotation plan` lists requests in service order with
  their selected window or the reason they were deferred.
- `community-irrigation-rotation channels` shows each window's water use and
  remaining flow capacity.
- `community-irrigation-rotation summary` reports requested and delivered
  water, withdrawals, and the remaining community allotment.

The fixed fixture has four canal windows and seven requests. A lower priority
number is more urgent. Requests are processed by priority, then by the last
allowed window, then by request ID. For each request, the planner chooses the
eligible window requiring the least source water; equal water use is resolved
in favor of the earlier window. It either satisfies the whole request or
defers it.

| Request | Zone | Priority | Allowed windows | Requested at field | Result |
|---|---|---:|---|---:|---|
| Seedling nursery (#501) | North | 0 | North dawn–North evening | 900 L | North evening; withdraws 1,053 L |
| Orchard (#502) | North | 1 | North dawn–North evening | 1,100 L | North dawn; withdraws 1,528 L |
| Greenhouse beds (#503) | South | 1 | South dawn–South evening | 850 L | South evening; withdraws 1,050 L |
| South field (#504) | South | 2 | South dawn–South evening | 900 L | South dawn; withdraws 1,412 L |
| Seedling expansion (#505) | North | 3 | North dawn–North evening | 1,000 L | Deferred; earlier requests use the remaining channel flow |
| Kitchen beds (#506) | South | 3 | South dawn–South evening | 100 L | South evening; withdraws 118 L |
| Herb planter (#507) | South | 4 | South dawn–South evening | 100 L | Deferred; 39 L remain, below this request's 118 L withdrawal |

The hand-checked summary is five of seven requests scheduled, 3,850 L delivered
to fields, and 5,161 L withdrawn from the shared supply. The 5,200 L weekly
allotment therefore has 39 L remaining. Window use is:

| Window | Withdrawn | Flow limit |
|---|---:|---:|
| North dawn | 1,528 L | 2,200 L |
| South dawn | 1,412 L | 1,500 L |
| North evening | 1,053 L | 2,000 L |
| South evening | 1,168 L | 1,800 L |

## Water calculation and rules

Each request specifies the amount that should reach its field and an
application efficiency in basis points (`10,000` means 100%). Each channel
window has a separate conveyance efficiency. The planner rounds up after each
loss calculation:

1. `field water = ceil(requested liters × 10,000 / application efficiency)`
2. `source water = ceil(field water × 10,000 / conveyance efficiency)`

This conservative integer rule avoids promising fractional liters. A request
can be scheduled only when its zone and inclusive window match, the full
source-water amount fits the remaining window capacity, and the weekly cap has
enough water left. Deferred reasons distinguish a window mismatch, a request
larger than every eligible channel, flow already reserved by earlier
priorities, and a remaining weekly allotment that cannot cover a whole
request.

The fixture is deliberately stored in a different order from its service
order. The planner selects the next request with a stable scan, so it
illustrates explicit priority/deadline/ID tie-breaking as well as stateful
capacity reservations. Channel selection is a deterministic greedy rule; it
does not search globally for the best schedule.

## Modules and language concepts

- `model.sai` defines irrigation zones, channel windows, requests, decisions,
  deferral reasons, and the final plan.
- `data.sai` supplies the static channel, request, and community-allotment
  fixtures.
- `policy.sai` defines request precedence and safe rounded-up water arithmetic.
- `planner.sai` ranks requests, selects feasible windows, and reserves both
  per-window flow and the shared weekly allowance.
- `report.sai` prints the plan, channel utilization, and summary.
- `lib.sai` dispatches CLI commands and records hand-checked regression cases.

The sample combines records and enums, fixed arrays, module imports, loops,
mutable local state, `Option`/`match`, stable tie-breaking, integer rounding,
and aggregate reporting.

## Scope and compiler status

This is a read-only, in-memory planning example. It does not split a request
across windows, change field targets, forecast weather, model soil moisture or
pump pressure, persist a reservation, or coordinate multiple planners. It
assumes unique request IDs, valid zone and window references, efficiencies
from 1 through 10,000 basis points, and fixture totals that fit in `U64`.
Request volumes and efficiencies are `U32`; loss products, rounded division,
and aggregate totals use `U64`, whose range covers these fixed-width inputs
through both efficiency calculations and the seven-request sum. The greedy
policy may defer a request even if a different global arrangement could serve
more water.

The output and regression expectations are hand-calculated, not produced by an
executed SAI program. `sai fmt --check` checks source and manifest syntax and
formatting; it does not prove name resolution, type correctness, or runtime
behavior. The current compiler's complete multi-module package, `sai:process`
WASI CLI, and SAI test pipeline are not available for this example yet.
