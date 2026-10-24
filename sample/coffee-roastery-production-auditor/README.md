# Coffee Roastery Production Auditor

Coffee Roastery Production Auditor reviews a fixed set of proposed roast
batches against three customer orders, three green-bean lots, and two roasters.
It applies roast loss to estimate saleable output, then reserves beans and
order quantity only for batches that pass every check.

```text
proposed batches: 14
accepted: 4
held: 10
accepted roasted output: 4,250 g

order CAFE-101: 1,700 / 1,700 g fulfilled
order COOP-204: 1,700 / 1,700 g fulfilled
order LAB-330: 850 / 850 g fulfilled

bean lot ETH-07: 2,000 / 2,600 g reserved; 600 g remain
bean lot BRA-12: 2,000 / 3,000 g reserved; 1,000 g remain
bean lot GUA-03: 1,000 / 1,500 g reserved; 500 g remain
```

## Fixture decisions

| Batch | Result | Reason |
|---|---|---|
| RB-100 | Accept, 1,700 g roasted | 2,000 g from BRA-12 at 15% loss meets CAFE-101. |
| RB-101 | Accept, 1,020 g roasted | SAMPLE-3 supports the light profile and ETH-07 is certified. |
| RB-102 | Accept, 680 g roasted | Completes the remaining 680 g of COOP-204. |
| RB-103 | Accept, 850 g roasted | Completes LAB-330 using certified GUA-03. |
| RB-104 | Hold | Would produce 425 g beyond LAB-330's completed order quantity. |
| RB-105 | Hold | SAMPLE-3 does not support CAFE-101's espresso profile. |
| RB-106 | Hold | COOP-204 requires certified beans; BRA-12 is not certified. |
| RB-107 | Hold | Its 2,500 g charge exceeds DRUM-12's 2,200 g limit. |
| repeated RB-100 | Hold | Batch IDs must be unique, including IDs on earlier held proposals. |
| RB-109 | Hold | CAFE-999 is not a known order. |
| RB-110 | Hold | A 100% roast-loss input is invalid. |
| RB-111 | Hold | BRA-12 has only 1,000 g left after RB-100 reserves 2,000 g. |
| RB-112 | Hold | LOT-MISSING is not a known bean lot. |
| RB-113 | Hold | ROASTER-MISSING is not a known roaster. |

`model.sai` defines orders, bean lots, roasters, proposed batches, decisions,
and the final balances. `fixtures.sai` supplies the fixed scenario.
`lookup.sai` resolves references and totals earlier accepted reservations.
`auditor.sai` checks each batch in fixture order, records its first failure,
and builds the order and lot summaries. `lib.sai` exposes the fixture audit
and keeps regression assertions beside the example.

## Audit rules

1. A batch ID must be unique. The first occurrence reserves resources only if
   it passes; a repeated ID is held regardless of the first occurrence's
   result.
2. The referenced order, bean lot, and roaster must exist. The roaster must
   support the order's requested roast profile, and orders marked organic
   require a certified lot.
3. Roast loss must be below 100%. Estimated roasted grams are integer
   arithmetic: `green_grams * (100 - roast_loss_percent) / 100`, rounded down.
4. A single charge cannot exceed its roaster's maximum. Accepted batches may
   not reserve more green beans than the lot contains or produce more roasted
   grams than the order requests.
5. Checks run in the order above and report the first failure. A held batch
   reserves neither its green-bean quantity nor its estimated roasted output.
   The summary marks an order fulfilled only when accepted production exactly
   matches its target.

The fixture calculations are small: RB-100 yields 1,700 g, RB-101 and RB-102
yield 1,020 g and 680 g, and RB-103 yields 850 g. Together they reserve 5,000 g
of green beans and yield 4,250 g of roasted coffee. The other ten proposals
are held and leave those balances unchanged.

## Limits

This sample audits a supplied batch list; it does not assign jobs to roasters,
schedule roast or cooling time, split batches, or choose among lots. It does
not model bean quality, blend recipes, moisture variation, packaging, shipping,
or financial transactions. Counts and fixed-array inputs are assumed to match
their declared bounds. Arithmetic is suitable for these fixtures and is not
an overflow-safe general-purpose production system.

## Review required

- Processing follows the fixture order, so an earlier accepted batch can use
  stock or order capacity before a later proposal. This is deterministic but
  does not optimize production or define customer priority.
- Organic eligibility is a Boolean property supplied with each lot. A real
  process would need authenticated certificates, scope, and validity dates.
- Roast loss is a whole percentage and fractional grams are rounded down.
  Actual yield depends on the bean, profile, equipment, and measurement method.
- Roaster profile support and maximum charge are fictional fixture values.
  The example is not operational or food-safety advice.

## Package and compiler status

This is a pure `wasm32-core` library with fixed in-memory fixtures and no
external dependencies, I/O, or persistent state. It uses records, enums,
fixed-capacity arrays, loops, and `Option` values. `sai fmt --check` passes.
The current compiler parses the package and source, but `sai check` stops at
the documented Stage 0 `sai check` and `@derive` feature gates. The Stage 3
`sai test` command is not available in this compiler slice, so the assertions
are source examples and have not been executed. The expected results above
are hand-calculated from the fixtures.
