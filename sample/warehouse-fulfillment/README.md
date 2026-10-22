# Warehouse Fulfillment

Warehouse Fulfillment builds a read-only shipping plan for a small online
retailer. It chooses one warehouse for each complete order, reserves eligible
stock for higher-priority orders, and checks handling, promised transit time,
and daily lane capacity before accepting a shipment.

The fixture is intentionally small enough to trace by hand:

- North Cold Hub can handle chilled goods and has both usable and too-short
  date-coded meal packs.
- North Ambient Annex has plenty of ambient goods but cannot handle chilled
  meal packs.
- Coast Cold Hub serves coastal orders quickly, but has only two lanterns.
- Orders are considered by ascending priority, requested delivery day, and
  order ID. A successful shipment reserves its stock and lane weight before
  later orders are planned.
- The requested delivery day is an arrival deadline: the fulfillment day plus
  route transit days must not exceed it. Routes also have a separate maximum
  transit-time constraint.
- An order with no positive-quantity lines is held as empty. Zero-quantity
  lines are only unused slots in the fixed-size fixture.
- Shelf-life checks apply only to SKUs whose best-before dates are tracked.
  Shelf-stable SKUs set `best_before_tracked` to false; their fixture lot dates
  are ignored.

## Intended commands

- `warehouse-fulfillment plan` lists each order's selected warehouse and
  shipping lane, or the first reason it could not be planned.
- `warehouse-fulfillment inventory` shows stock that meets the fulfillment
  date and minimum remaining shelf-life requirement at each warehouse.
- `warehouse-fulfillment lanes` compares daily lane capacity with the weight
  reserved by the plan.
- `warehouse-fulfillment summary` totals orders, requested and planned units,
  held units, and freight charges.

The hand-checked fixture is intended to plan orders `8101` and `8104`. Order
`8102` cannot be completed from the timely Coast route because Coast Cold Hub
has only two of its three requested lanterns. Order `8103` has eligible stock
at North Cold Hub, but its 2,800-gram load would exceed that lane's remaining
daily capacity after order `8101`; the ambient North warehouse cannot handle
its chilled items, and the Coast route is too slow for its deadline.

The corresponding summary is intended to show two planned orders, two held
orders, six of fourteen units planned, and $32.50 in freight.

## Modules

- `model.sai` defines SKUs, date-coded stock lots, warehouses, orders, routes,
  shipments, and hold reasons.
- `data.sai` provides the fulfillment-day snapshot, warehouse inventory,
  routes, and deterministically ordered order fixture.
- `inventory.sai` looks up domain records and totals stock whose best-before
  day satisfies each SKU's minimum remaining shelf life.
- `policy.sai` contains the order priority comparison and item-handling rules.
- `planner.sai` checks complete-order stock, reserves earlier shipments'
  inventory and lane weight, selects the best eligible route, and explains
  rejected orders.
- `report.sai` prints the plan, stock, lane utilization, and aggregate summary.
- `lib.sai` dispatches the command-line subcommands.

The example combines records, enums, fixed arrays, module imports, loops,
nested lookup, `Option`/`match`, deterministic tie-breaks, date thresholds,
inventory allocation, capacity checks, and report aggregation. Route choice
prefers lower transit time, then lower freight cost, then lower warehouse ID.
Freight uses integer cents per kilogram and rounds down to a whole cent.
An order whose weight alone exceeds a lane limit and an order that exceeds
capacity after earlier reservations receive the same neutral remaining-capacity
explanation.

## Scope and limitations

The data is a fixed snapshot held in memory. The planner requires every line
in an order to ship from one warehouse and either plans the whole order or
holds it; it does not split shipments, substitute products, update inventory,
write reservations, or coordinate concurrent planners. Its priority-greedy
route choice is deterministic, but it is not a global cost or throughput
optimizer. The fixture assumes valid unique order IDs, positive line quantities
on used lines (zero quantities mark unused fixed-array slots), unique
warehouse/destination routes, and small values that do not overflow `U32`.

The source is an illustrative design and is not claimed to compile or run with
the current compiler. It uses the same `sai:process` command entry point and
multi-module package pattern as the other CLI samples, plus loops, formatting,
`@derive`, fixed-array records, and recursive lookups over strictly earlier
orders. The `sai:process` Git source is currently unavailable, and the
compiler does not yet support the complete package, process/WASI, CLI, loop,
formatting, derive, and multi-module pipeline shown here. The command output
and fixture outcomes are hand-checked expectations, not results from an
executed program.
