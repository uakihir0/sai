# Supply Chain Recall Tracer

This fixed, fictional trace follows one confirmed contaminated component lot
through manufacturing batches into finished-product lots and retailer
shipments. It produces a bounded recall list and reconciled unit totals; it
does not contact suppliers, factories, retailers, or customers.

The following is the **hand-calculated expected fixture result**, not program
output. Production batches are listed in ascending batch ID, and affected
shipments are listed in ascending shipment ID, even though the input fixtures
are deliberately unsorted.

```text
Target component lot 701: BRKT-A
Received: 120 units   Consumed in affected batches: 120   Unconsumed: 0

Batch  Finished lot  SKU   Produced  Shipped  Sold  Retail hold  Factory hold
5002          9002  MX-7         60       55    15           40             5
5003          9003  MX-7         40       35    15           20             5
5005          9005  MX-8         20       15     7            8             5

Affected shipments
Shipment  Finished lot  Retailer            Shipped  Sold  Retail hold
8001               9002  Northstar Market         20    10           10
8002               9002  Lakeside Stores          35     5           30
8003               9003  City Market              10    10            0
8004               9003  Northstar Market         25     5           20
8005               9005  Value Outlet             15     7            8

Recall totals: 3 batches, 3 finished lots, 5 shipments
120 affected finished units = 105 shipped + 15 held at the factory
105 shipped units = 37 reported sold + 68 held by retailers
```

## Trace rules

- The trace starts with an operator-confirmed component lot ID. A batch is
  affected when its recorded component lot ID exactly matches that target.
- Each modeled batch consumes one component unit per finished unit and creates
  exactly one finished-product lot. Its component consumption must equal its
  production quantity.
- Each shipment belongs to exactly one finished-product lot. Retailer-held
  quantity is `shipped units - sold units`; factory-held quantity is
  `produced units - all shipped units for that finished lot`.
- Batch IDs and shipment IDs determine output order. Input order never
  determines which records are included or how they are reported.
- The fixture assumes valid, unique IDs, complete consumption and shipment
  records, and that no batch or shipment exceeds its source quantity. These
  preconditions hold in the fixed fixture; source tests check the quantity
  bounds and report reconciliation so that unsigned subtraction is well-defined.

The affected batches consume 60 + 40 + 20 = 120 units from component lot 701.
For finished lot 9002, 20 + 35 = 55 units were shipped, leaving 5 at the
factory; its retailers report 10 + 5 = 15 sold and 40 held. Lot 9003 has 35
shipped, 5 at the factory, 15 sold, and 20 held at retailers. Lot 9005 has 15
shipped, 5 at the factory, 7 sold, and 8 held at retailers. The fixture
therefore reconciles at both boundaries: 120 = 105 + 15, and 105 = 37 + 68.

Batch 5001 uses unaffected component lot 702, and batch 5004 uses unaffected
lot 703. Their finished lots and shipments do not appear in the recall list.
Shipment rows 7999, 8006, and 8007 are also excluded because they refer to
those unaffected finished lots.

## Project structure and concepts

- `model.sai` defines component lots, production batches, retailer shipments,
  the fixed trace input, and the recall report.
- `data.sai` supplies three component lots, six unsorted batches, and eight
  unsorted shipments.
- `lineage.sai` sorts batches and resolves which finished lots descend from
  the target component lot.
- `recall.sai` aggregates factory and retailer quantities, sorts the affected
  shipment list, and builds the trace report.
- `lib.sai` exposes `trace_sample()` and contains fixture and reconciliation
  test declarations.

## Assumptions and limits

All names and IDs are fictional, and the quantities are units, not weights or
currency. The trace assumes the upstream contamination decision has already
been made and treats every finished unit from a matching batch as affected.
It models a single component lot per batch, one-to-one component consumption,
one finished lot per batch, and direct shipments from that finished lot to
retailers. It does not model mixed-lot assemblies, rework, scrap, returns,
inter-warehouse transfers, distribution-center consolidation, consumer
contact, or regulatory reporting. Those cases need explicit additional
lineage records and rules before this example can represent them.

## Compiler and test status

This package is an illustrative fixed-array library, not a verified executable.
The local `sai fmt --check sample/supply-chain-recall-tracer` command passes.
`sai -C sample/supply-chain-recall-tracer check --offline --target wasm32-core`
reaches source parsing without parser diagnostics, then stops at the current
Stage 0 `sai check` and `@derive` feature gates. Semantic analysis has not run,
and the source test declarations have not been executed. The table is a manual
calculation, not program output.
