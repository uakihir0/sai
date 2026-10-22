# Invoice Reconciliation

Invoice Reconciliation reviews a fixed accounts-payable batch using a
three-way match: each invoice line is compared with its purchase-order line
and the corresponding goods-receipt line. It keeps the evidence for each
comparison so a reviewer can see why an invoice is ready or held.

The fixture batch is designed to exercise distinct outcomes:

| Invoice | Intended result |
|---|---|
| AP-701 | Ready: two lines match the order and received quantities. The marker line is not included on AP-701. |
| AP-702 | Hold: eight markers were ordered, but only six were received. |
| AP-703 | Hold: the invoice names the wrong vendor and bills markers above the agreed unit price. |
| AP-704 | Hold: `TONER-COLOR` is not a line on the referenced purchase order. |
| AP-705 | Hold: the referenced receipt belongs to a different purchase order. |
| AP-706 | Hold: AP-701 was accepted earlier in the batch for all ten received sheets of paper, so this later invoice cannot claim another sheet. |

All amounts are integer cents. AP-701 has a billed total of $50.00; AP-702
has a billed total of $24.00. The six invoices total $239.50. A downstream
report can display the returned `BatchAudit` as a review queue without
reconstructing the matching evidence.

`model.sai` defines purchase orders, receipts, invoices, line evidence, and
batch totals. `fixtures.sai` provides the two purchase orders, two receipts,
and six invoices. `lookup.sai` resolves document and SKU references.
`matching.sai` evaluates one invoice line, and `audit.sai` combines those
checks into invoice decisions and batch counts. `lib.sai` exposes the sample
batch and keeps the fixture expectations beside the implementation.
Reconciliation reserves quantities only for invoices accepted earlier in the
fixture order, so a later invoice cannot reserve the same ordered or received
units again, even when the order has multiple receipts.

The example permits partial invoicing: an invoice may bill only some of the
items received so far. Across the fixture batch, accepted quantities for an
order/SKU pair must not exceed that ordered quantity; accepted quantities for a
receipt/SKU pair must not exceed the quantity recorded on that receipt. Unit
prices must match exactly. Each source document is assumed to contain at most
one line per SKU, and document numbers are unique in the fixture.
Quantity-variance totals count failed cumulative order and receipt
comparisons; header-issue totals count invoices with one or more missing or
inconsistent header fields or document references, including a vendor
mismatch.
The fixture values are small enough that all `U32` line products and invoice
and batch totals fit; the sample does not check overflow for arbitrary input.
It also does not model taxes, discounts, currency conversion, duplicate
document detection, receipt aggregation, payment execution, or
accounting-system integration.

## Package and compiler status

This is a pure `wasm32-core` library with fixed in-memory fixtures. It has no
process entry point, I/O, external package, persistent state, or payment
side effect. Its bounded multi-module implementation uses records,
fixed-capacity arrays, loops, and `Option` values.

This project is illustrative, not a build guarantee. The current compiler
has not completed the semantic and package pipeline needed to build this
multi-module source. The fixture outcomes above are hand-checked; the SAI
regression tests are source examples and have not been executed.
