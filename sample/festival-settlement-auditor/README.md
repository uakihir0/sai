# Festival Settlement Auditor

Festival Settlement Auditor reviews a fixed weekend of ticket sales before the
organizer releases payment-processor proceeds. It joins multi-ticket orders to
captures and fees, checks refunds and repeated gate scans, and isolates payment
or scan records that do not match a known order or ticket.

The sample intentionally separates the accounting facts from the release
decision. Captured totals include unmatched payments and held orders; the
releasable payout includes only orders with a consistent payment and no repeated
admission scan. A refund, duplicate capture, amount mismatch, excessive
processor fee, or repeated admission puts the order on hold. Complimentary
orders may have no payment. A ticket may have no scan and still be paid; this
example does not treat a no-show as a settlement defect.

## Hand-checked result

| Order | Ticket value | Captured | Fee | Refund | Result |
|---:|---:|---:|---:|---:|---|
| 5011 | $85.00 | $85.00 | $2.90 | $0.00 | release $82.10 |
| 5012 | $50.00 | $50.00 | $1.75 | $5.00 | hold: refund review |
| 5013 | $0.00 | $0.00 | $0.00 | $0.00 | hold: repeated admission scan |
| 5014 | $50.00 | $55.00 | $1.90 | $0.00 | hold: captured amount mismatch |
| Unknown order | — | $20.00 | $0.60 | $0.00 | unmatched payment |

Across all four payment records, the source reports $210.00 captured,
$7.15 in processor fees, and $5.00 refunded. Only order 5011 qualifies for
release, for $82.10. One payment references an unknown order, one gate scan
references an unknown ticket, and one known ticket was scanned twice. The
repeated-scan counter counts distinct known tickets with at least two scans,
not extra scan events. These
exceptions are visible in the source totals and are not folded into the payout.

## Modules and rules

- `model.sai` defines fixed-size orders, tickets, payment records, gate scans,
  and the audit result types.
- `fixtures.sai` provides four orders, four processor records, and six scans.
- `reconcile.sai` joins records, checks capture counts and amounts, detects
  refunds and repeated scans, and calculates the releasable amount.
- `lib.sai` exposes the fixed audit and contains assertions for the result and
  its exception counts.

All money is stored as integer cents. The fixed fixtures keep totals within
`U32`; the example does not claim to protect arbitrary production inputs from
integer overflow. A refund always requires review, even when the refunded
amount is small. Multiple findings on one order are reduced to the first reason
in a documented precedence: duplicate payment, missing payment, amount
mismatch, fee greater than capture, refund, then repeated admission. The sample
does not model tax, chargebacks, partial capture, processor-specific fee
schedules, ticket transfers, or real payment integrations.

This `wasm32-core` package is illustrative and is not a build guarantee. The
repository's compiler currently lacks the full `sai check` and `@derive`
pipeline required by these sources, so the embedded SAI assertions have not
been executed. The table above is a hand-calculated reference for the fixed
fixture.
