# Island Grid Dispatch

Island Grid Dispatch plans one hour of power for a small feeder that has been
cut off from the main grid during a storm. Renewable forecasts, a protected
battery reserve, and six fixed demand blocks become a deterministic dispatch
plan:

```text
island-grid-dispatch plan
One-hour islanded plan: North Pier storm watch
Renewable generation: 700 kW (solar 520, wind 180)
Battery headroom: 140 kWh above the 280 kWh reserve
Demand: 980 kW across 6 service blocks

Dispatch
  serve [critical] Harbor clinic — 300 kW
  serve [essential] Water pumps — 180 kW
  serve [essential] Emergency shelter — 170 kW
  serve [essential] Vaccine cold room — 130 kW
  defer [flexible] Pier lighting — 90 kW (whole block does not fit)
  defer [flexible] Workshop — 110 kW (whole block does not fit)

Served: 780 kW; deferred: 200 kW across 2 blocks
Unused dispatch capacity: 60 kW
Battery dispatch: 80 kWh; ending charge: 340 kWh (60 kWh above reserve)
```

The planner applies three service tiers in order: critical, essential, then
flexible. Within a tier it keeps the fixture order. A demand block is either
served in full or deferred. If the next block does not fit, that block and all
remaining blocks are deferred rather than skipping ahead to fit a smaller
request. The report uses renewable generation first and draws only the
remaining served demand from the battery.

The inputs are deliberately small enough to check by hand: the renewable
forecast supplies 700 kW, and the battery can offer at most 140 kWh above its
reserve over this one-hour window. That gives an 840 kW ceiling. The first four
blocks use 780 kW, leaving 60 kW—too little for either flexible block. The
battery supplies the 80 kW gap between served load and renewable generation,
leaving 340 kWh in storage.

`model.sai` defines the service tiers, demand blocks, grid snapshot, and
summary. `data.sai` holds the storm-watch fixture. `supply.sai` enforces the
reserve when calculating available battery energy, and `planner.sai` applies
the stable priority policy. `report.sai` presents each decision, while the
root module provides the `plan` command. The sample demonstrates records,
enums, fixed arrays, nested iteration, module boundaries, budget arithmetic,
and a policy that can defer work under a hard resource limit.

Each demand block has a unique ID within its grid snapshot. The planner uses
that ID to associate a reported block with its decision; it does not validate
arbitrary external load data.

## Assumptions and limits

- This is a planning illustration, not a grid-control system. Its forecasts
  are already derated for weather and outages.
- The horizon is exactly one hour. The fixture treats one kWh of battery
  headroom as one kW available throughout that hour; inverter limits, losses,
  ramp rates, voltage, and frequency stability are outside the model.
- Load blocks are indivisible, and the tier order and input order are teaching
  choices. The planner is not an optimizer and does not estimate the harm of
  deferred loads.
- The fixture is fixed and read-only. The sample does not connect to meters,
  actuate equipment, persist a schedule, or accept operator input.

## Package and compiler status

The package targets `wasm32-wasi` and uses the `sai:process` command-line entry
point shape. The `sai:process` Git URL is currently unavailable, so resolution
needs a matching toolchain bundle index or a published source repository. This
example is illustrative: the current compiler does not yet support the full
package, `sai:process`/WASI entry point, command-line, loop, formatting,
`@derive`, and multi-module pipeline used here. No successful check, build, or
run is claimed. The command dispatcher assumes the host includes the
executable name in `argv[0]`.
