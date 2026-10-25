# Expense Reimbursement Auditor

Expense Reimbursement Auditor reviews a bounded travel-claim batch before a
manager approves a reimbursement run. It links each claim to an active
employee and that employee's trip, checks the travel dates and business
purpose, validates receipt evidence, and then applies category limits. The
audit returns an ordered decision for every claim and separates the original
claimed total from the policy-eligible reimbursement recommendation.

All amounts are integer cents. Claims are evaluated by expense day, purchase
minute, then claim ID. This ordering makes the meal allowance allocation and
duplicate-receipt finding stable even when the fixture input order changes.

## Hand-checked fixture outcomes

| Claim | Claimed | Suggested reimbursement | Result |
|---:|---:|---:|---|
| 1001 | $820.00 | $800.00 | Adjusted: airfare claim limit |
| 1002 | $42.00 | $42.00 | Approved |
| 1003 | $38.00 | $33.00 | Adjusted: $75 daily meal limit after claim 1002 |
| 1016 | $9.00 | $0.00 | Denied: daily meal allowance is already exhausted |
| 1013 | $5.00 | $0.00 | Held: employee is unknown |
| 1004 | $600.00 | $540.00 | Adjusted: three nights at $180 per night |
| 1005 | $42.00 | $42.00 | Approved |
| 1006 | $42.00 | $0.00 | Held: receipt 5005 was already submitted by claim 1005 |
| 1007 | $24.00 | $0.00 | Denied: alcohol is excluded by this sample policy |
| 1008 | $90.00 | $0.00 | Held: trip 8002 belongs to another employee |
| 1014 | $15.00 | $0.00 | Held: business purpose is missing |
| 1010 | $200.00 | $0.00 | Held: lodging requires an itemized receipt |
| 1012 | $320.00 | $0.00 | Held: airfare requires an itemized receipt |
| 1015 | $42.00 | $0.00 | Held: expenses at or above $25 require receipt evidence |
| 1009 | $95.00 | $0.00 | Held: expense date falls outside the trip |
| 1011 | $18.00 | $0.00 | Held: employee is inactive |

The batch contains 16 claims, with $2,402.00 claimed and $1,457.00 suggested
for reimbursement. Two claims are approved at the claimed amount, three are
adjusted to the applicable cap, nine are held for a person to review, and two
are denied by the sample policy. Held and denied claims contribute nothing to
the suggested amount. Adjusted claims contribute the allowed portion; this is
a recommendation and the sample does not issue payments.

## Modules and rules

- `model.sai` defines employees, trips, expense claims, policy evidence,
  decisions, and audit totals.
- `fixtures.sai` supplies two employees, two trips, and sixteen claims covering
  successful, adjusted, held, and denied outcomes.
- `lookups.sai` resolves employee and trip identities.
- `policy.sai` applies receipt requirements and category reimbursement caps.
- `review.sai` checks one claim in precedence order, allocates the remaining
  daily meal allowance, and detects reuse of a receipt reference.
- `audit.sai` sorts claims, evaluates them against prior decisions, and
  calculates the batch totals.
- `lib.sai` exposes the fixture audit and includes source-level assertions for
  its expected outcomes.

The sample policy allows meals up to $75 per employee per expense day, lodging
up to $180 per night, airfare up to $800 per claim, and ground transport up to
$180 per claim. Lodging and airfare always require itemized evidence. Other
categories require some receipt evidence for claims of $25 or more. Alcohol is
excluded. If a claim reaches the duplicate-reference check, an earlier
non-denied claim with the same nonzero receipt reference causes it to be held
and linked. Earlier held claims still count; earlier denied claims do not.

Only the first issue found is stored for a claim. The review order is employee
existence and active status, trip existence and ownership, trip dates, business
purpose, positive amount, prohibited category, lodging nights, receipt
evidence, repeated receipt reference, then any daily or category limit.

The audit assumes its fixed-capacity counts are within their declared array
bounds, employee and trip IDs are unique, claims have unique IDs, and money
and limit products fit in `U32`. It does not parse dates, inspect receipt
files, convert currency, model card transactions, aggregate shared meal
receipts, calculate taxes, or call a payment service.

## Review required

- **Daily meal allocation:** The $75 cap is shared by one employee across a
  local expense day, regardless of trip ID. Claims consume it in purchase-time
  order; claim ID breaks ties when the fixture has no finer timestamp. Confirm
  whether the real policy groups by trip, calendar day, or meal period and
  whether a per-diem allowance should consider lodging or destination.
- **Exhausted meal allowance:** A meal submitted after earlier claims consume
  the full daily cap is denied at $0; a meal that has some cap remaining is
  adjusted to that remainder. This treats a known policy excess as a
  deterministic denial instead of leaving it in the hold queue. Confirm
  whether the policy owner wants exhausted per-diem claims denied, held, or
  reported as adjusted to $0.
- **Partial reimbursement:** The audit counts the portion under the cap as a
  suggested reimbursement even when it marks the claim `Adjusted`. Confirm
  whether an employee or manager must accept that reduction before it can be
  included in a payment run.
- **Receipt reuse:** A later claim that reaches the duplicate-reference check
  is held when an earlier non-denied claim used the same nonzero receipt
  reference, even when two employees split a shared bill or the first claim is
  itself held. Claims denied by an earlier check do not reserve a reference.
  Confirm how shared receipts should be allocated and whether a receipt should
  be reserved only after approval.
- **Lodging cap:** The limit is nightly rate multiplied by reported nights.
  Taxes, fees, room class, destination, occupancy, and policy exceptions are
  not modeled. Confirm which of those count toward the real cap.
- **Evidence and policy scope:** Receipt kind and business purpose are fixture
  metadata, not verified documents. The alcohol exclusion and $25 threshold
  are example rules, not legal or accounting advice. Confirm the evidence
  standard, exceptions, and category rules with the policy owner.

## Package and compiler status

This is a pure `wasm32-core` library with fixed in-memory fixtures. It has no
command entry point, file access, network dependency, persistent state, or
payment side effect. The multi-module design uses records, enums, fixed
arrays, sorting, loops, and source-level assertions.

This project is illustrative, not a build guarantee. The repository supports
some narrow single-package checks and Phase 1 local module-alias and item-import
resolution, but those slices do not cover this sample's combined features.
Full multi-module semantic checking and derive processing remain gated by
`FEATURE_NOT_IMPLEMENTED`. `sai fmt --check` does pass for all sample files.
The embedded assertions and expected totals above are hand-checked and have
not been executed by SAI.
