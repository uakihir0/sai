# Insurance Claim Review

This fixed, fictional property-claim batch demonstrates a small adjudication
pipeline: check policy terms, classify incomplete submissions, apply a
per-occurrence deductible and coverage limit, prevent a second payment for an
event, then consume a policy-wide aggregate in a stable order. The result is a
structured review report; the sample does not perform external I/O or issue
payments.

The table is the **hand-calculated expected fixture result**, not program
output. Claims arrive unsorted and are processed by loss day, then claim ID.

```text
Property claim review — sample batch

Claim  Loss day  Outcome                         Pre-aggregate  Payable
9005      217    Pending — documents incomplete        —          $0
9001      219    Paid                                  $4,000   $4,000
9002      220    Capped — aggregate limit applied      $1,800   $1,500
9004      221    Denied — event 77 already paid        —          $0
9003      222    Denied — flood not covered             —          $0
9008      223    Denied — aggregate exhausted          $1,600       $0
9006      230    Denied — notice 40 days after loss     —          $0
9007      235    Denied — unknown policy               —          $0

Summary: 1 paid, 1 capped, 5 denied, 1 pending
Total payable: $5,500
Remaining aggregate: policy 501 $0; policy 502 $5,000
```

## Review policy and calculation order

- A covered loss must occur inside the policy term, be reported on or after
  the loss day and within the policy's 30-day notice period, match a covered
  peril, and have complete documents. Missing documents produce a pending
  decision without reserving aggregate funds. Other failed checks are denied.
- `covered damage = min(estimated damage, coverage limit)` and
  `pre-aggregate payment = max(covered damage - deductible, 0)`. The deductible
  applies once to each claim. The final payment is the lesser of that amount
  and the remaining policy aggregate. In the report, `—` means this calculation
  was not reached because an earlier review rule decided the claim; it is not a
  zero-dollar calculation.
- Policies 501 and 502 have separate aggregate balances. Policy 501 has a
  $6,500 term aggregate with $1,000 already paid, so this batch starts with
  $5,500 available. The $4,000 payment for claim 9001 leaves $1,500; claim 9002
  would pay $1,800 before the aggregate cap and therefore pays $1,500. Claim
  9008 is a different covered event; it has $1,600 payable before aggregate
  review but is denied because the policy balance is already zero.
- The claims for policy 501 and event 77 share a loss event. A later claim is
  flagged as a duplicate only when an earlier processed claim for the same
  policy and event has a positive payment. Thus pending or denied rows do not
  lock the event or consume funds.
- Coverage, documentation, and notice checks run before duplicate-payment and
  amount checks. Claims with the same loss day are ordered by ascending claim
  ID. This makes aggregate use and explanations reproducible for this batch.
- Claim 9003 is flood damage under a policy that does not cover flood. The
  example does not infer that an excluded peril is covered by another rider.

## Project structure and concepts

- `model.sai` defines policies, claims, review outcomes, reasons, and report
  records.
- `data.sai` provides two fictional policies and eight deliberately unsorted
  claims, including an incomplete file, a repeated event, an excluded peril,
  late notice, and an unknown policy.
- `policy.sai` provides policy lookup, peril coverage, per-coverage limits, and
  the opening aggregate balance.
- `adjudicator.sai` orders the batch, evaluates each claim, updates independent
  aggregate balances, detects duplicate payments, and builds the report.
- `lib.sai` exposes `review_sample()` and contains Stage 3 checks for totals,
  the aggregate cap and exhaustion, reason codes, and deterministic order.

## Assumptions and limits

All policy IDs, claims, days, limits, and amounts are fictional. Day values are
synthetic ordinals on one continuous timeline, and all monetary values are
whole dollars. Policy IDs and claim IDs are unique in the fixture. Fixed array
counts must not exceed their capacities. Every slot before each count must be
populated (`Some`), and a policy's paid-to-date amount must not exceed its
aggregate limit. Coverage, adjustment, and payment decisions in this fixture
are teaching rules, not insurance advice or a representation of any insurer's
contract. The sample does not model depreciation, coinsurance, subrogation,
repair evidence, legal disputes, regulatory rules, or payment execution.

## Compiler and test status

The manifest targets `wasm32-core`, has no external dependencies, and declares
no runtime capabilities. Stage 0 `sai check` and `@derive` gates currently
prevent semantic compilation, and Stage 3 `sai test` is not implemented. The
source test declarations have not been run. The current repository formatter
also returns an internal non-idempotent-formatting error for this package, so
source formatting is not verified. The table above is a manual calculation
only; it is not evidence that the source has been built or run.
