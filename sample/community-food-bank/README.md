# Community Food Bank

Community Food Bank prepares a read-only allocation plan for one pantry
distribution. The fixed fixture contains five households, twenty requested
item lines, and stock lots with different use-by days. The planner protects
dietary exclusions, removes stock that expired before distribution, and
allocates the remaining units by household priority.

```text
community-food-bank allocations
Harbor Pantry allocation plan — distribution day 5
Allocation order: priority 1, then 2, then 3; ties use household ID

Aki Sato | rice | 2 requested, 2 allocated (allocated)
Aki Sato | pinto beans | 2 requested, 2 allocated (allocated)
Aki Sato | shelf-stable milk | 1 requested, 0 allocated (not eligible)
Aki Sato | wheat pasta | 1 requested, 0 allocated (not eligible)
Ben Ito | rice | 1 requested, 1 allocated (allocated)
Ben Ito | pinto beans | 1 requested, 1 allocated (allocated)
Ben Ito | shelf-stable milk | 1 requested, 0 allocated (not eligible)
Ben Ito | wheat pasta | 2 requested, 2 allocated (allocated)
Chika Mori | rice | 1 requested, 1 allocated (allocated)
Chika Mori | pinto beans | 2 requested, 2 allocated (allocated)
Chika Mori | shelf-stable milk | 1 requested, 1 allocated (allocated)
Chika Mori | wheat pasta | 1 requested, 1 allocated (allocated)
Daisuke Kato | rice | 2 requested, 2 allocated (allocated)
Daisuke Kato | pinto beans | 1 requested, 0 allocated (unfilled)
Daisuke Kato | shelf-stable milk | 2 requested, 2 allocated (allocated)
Daisuke Kato | wheat pasta | 2 requested, 1 allocated (partial)
Emi Watanabe | rice | 1 requested, 0 allocated (unfilled)
Emi Watanabe | pinto beans | 1 requested, 0 allocated (unfilled)
Emi Watanabe | shelf-stable milk | 1 requested, 0 allocated (not eligible)
Emi Watanabe | wheat pasta | 2 requested, 0 allocated (unfilled)

community-food-bank summary
Requested: 28 units
Eligible demand: 24 units
Allocated: 18 units
Unfilled eligible demand: 6 units
Dietary exclusions: 4 units
Available stock: 18 units
Expired stock excluded: 2 units
```

## Rules

- Distribution day is day 5. A lot is usable only when its `best_before_day`
  is on or after that day. Two bean units from a day-4 lot are excluded.
- Household priority is 1 (urgent), 2 (standard), then 3 (routine). Ties are
  resolved by ascending household ID; repeated same-item requests by one
  household use ascending request sequence, which is unique for that
  household-item pair. For each item, earlier eligible requests consume stock
  before later requests.
- Plant-based households cannot receive items marked as containing animal
  products. Dairy-free households cannot receive dairy, and gluten-free
  households cannot receive gluten. These are fixture-level ingredient rules,
  not allergy or medical advice.
- Eligible requests receive up to their requested whole units. The planner
  permits partial allocations and never allocates more than the unexpired
  stock for an item. It does not substitute products or reserve a minimum
  basket for each household.
- Expired stock is removed before any allocations. Usable lots for an item are
  pooled; the example does not choose a particular lot for each household.

The fixture has 18 usable units: 6 rice, 5 beans, 3 milk, and 4 pasta. The
eligible requests total 24 units, leaving 6 unfilled. Four requested units are
excluded by diet rules before stock allocation. The example output is a
hand-checked fixture, not output from a successful compiler run.

## Commands

- `allocations` shows every request with its eligibility and allocated units.
- `stock` lists usable and expired units for each pantry item.
- `summary` totals requested, eligible, allocated, unfilled, excluded, usable,
  and expired units.

## Project structure and concepts

- `model.sai` defines households, pantry items, stock lots, requests, and
  allocation results.
- `data.sai` contains the fixed distribution-day fixture and its ordered
  requests.
- `policy.sai` checks ingredient compatibility against household preferences.
- `planner.sai` excludes expired lots, applies the priority order, and
  calculates allocations and totals.
- `report.sai` formats allocation, stock, and summary views; `lib.sai`
  dispatches the command.

The project illustrates records, fixed arrays, modules, loops, nested lookup,
options and matches, boolean policy rules, stable priority ordering, bounded
resource allocation, and summary arithmetic.

## Assumptions and limits

This is a fixed, read-only planning illustration. It does not accept household
or inventory updates, connect to a pantry system, store personal information,
persist allocations, or reserve stock. Priority levels and dietary flags are
teaching choices; there is no intake assessment, household-size adjustment,
fairness optimization, nutrition model, product substitution, lot-level
pick list, or concurrent inventory handling.

## Package and compiler status

The manifest targets `wasm32-wasi` and uses the `sai:process` command-line
entry-point shape. The `sai:process` Git URL is currently unavailable, so
package resolution needs a matching toolchain bundle index or a published
source repository. This sample is illustrative: the current compiler does not
yet support the complete package, process, command-line, loop, formatting,
`@derive`, and multi-module pipeline used here. No successful check, build, or
run is claimed. The command dispatcher assumes its host supplies the
executable name as `argv[0]`.
