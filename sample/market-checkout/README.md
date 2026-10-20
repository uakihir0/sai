# Market Checkout

Market Checkout is a small order desk for a fictional neighborhood market. It
keeps a fixed catalog and a few example orders in memory, then lets an operator
inspect products, review order quotes, and see which orders are ready to
fulfill.

```text
market-checkout catalog
Market catalog
BEAN-ETHIOPIA  Ethiopian beans  coffee  $14.50  stock: 8
BEAN-DECAF  Decaf beans  coffee  $12.50  stock: 5
TEA-SENCHA  Sencha tea  tea  $9.80  stock: 3
MUG-TRAVEL  Travel mug  equipment  $22.00  stock: 4
FILTER-01  Paper filters  equipment  $7.00  stock: 12

market-checkout quote
Order #1042 for Jun: ready
  subtotal $51.00, discount $3.50, tax $3.80, total $51.30
Order #1043 for Emi: hold — stock is too low
Order #1044 for Kai: ready
  subtotal $39.00, discount $1.50, tax $3.00, total $40.50
Order #1045 for Ren: hold — unknown product code

market-checkout inventory
5 products; 32 units on hand; retail value $379.90
```

The root module dispatches commands. `model.sai` defines the shared domain
records, `catalog.sai` and `orders.sai` provide sample data, `pricing.sai`
contains the discount and tax rules, `checkout.sai` joins orders to products,
and `report.sai` renders operator-facing output. The quote path checks every
line against stock, caps discounts at the order subtotal, applies an 8% tax to
the discounted subtotal, and explains why held orders need review. Stock checks
aggregate repeated lines for the same SKU: Emi's order splits four tea boxes
across two lines even though only three are in stock.

## Commands

- `catalog` lists products, unit prices, and stock.
- `orders` prints every sample order with its line items.
- `quote` calculates totals and shows ready or held orders.
- `fulfill` prints only orders that pass the stock and product checks.
- `inventory` reports product count, total units, and the retail value of
  current stock.

## Package and compiler status

The package targets `wasm32-wasi` and uses the same `sai:process` package and
entry-point shape as `greet-cli` and `mini-task-board`. This is an illustrative
project, not a build guarantee: the current compiler does not yet implement
the full package, process, command-line, loop, formatting, and multi-module
pipeline used here. In particular, `sai:process` and the WASI entry point are
Stage 4, and f-strings are Stage 3. No file or network access, persistent
storage, or mutation of the sample catalog is implied by the `fulfill`
command; it reports which orders could be fulfilled from the in-memory stock
snapshot.

All amounts are integer cents. The example rounds tax down to a whole cent.
The small fixed data set is chosen to make the workflow easy to follow, not to
model production accounting or concurrent inventory reservations.
