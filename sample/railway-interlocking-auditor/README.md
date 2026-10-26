# Railway Interlocking Auditor

This fixed-fixture SAI library reviews route requests for a fictional railway
yard. It orders requests deterministically, finds an available start within
each request's window, and records whether the request was accepted, held by a
conflict, or rejected as invalid. Accepted routes reserve every listed track
block, including a protected overlap, and record the turnout alignment that
the route requires.

This is an educational scheduling example, not railway control software or a
railway safety case. It does not operate signals or turnouts, and its fixture
must not be used to make real railway movements safe.

## Hand-checkable fixture

Slots are abstract integer time units. Intervals are start-inclusive and
end-exclusive: a reservation ending at slot 4 does not conflict with one
starting at slot 4. Requests are processed by requested start slot, then
lower numeric priority, then lower request ID. Input position breaks any
remaining tie. Within the allowed start window the auditor tries slots in
ascending order and accepts the first conflict-free interval.
The fixture stores requests out of order so the returned order can be checked
against the declared tie-breaks.

The fixture has a 16-slot horizon and one occupied block: `CEDAR` during
`[5, 7)`. Each route reserves its three listed blocks as one set. The third
block is the protected overlap. A simultaneous reservation conflicts if any
protected block is shared, or if it needs the same turnout in a different
position. A turnout required in the same position is compatible in this
simplified model.

| Process order | Request | Requested window | Outcome | Explanation |
|---:|---|---|---|---|
| 1 | 3107 Museum special | `[1, 1]`, duration 1 | Rejected | Route 999 is unknown |
| 2 | 3101 Express 11 | `[2, 3]`, duration 2 | Accepted `[2, 4)` | West Yard; T1 reverse |
| 3 | 3102 Local 5 | `[2, 3]`, duration 2 | Held | Its blocks overlap request 3101 at every candidate start |
| 4 | 3103 Local 8 | `[4, 6]`, duration 2 | Held | Every candidate overlaps the occupied CEDAR block |
| 5 | 3104 River shuttle | `[4, 8]`, duration 2 | Accepted `[4, 6)` | Starts when request 3101 releases JUNCTION |
| 6 | 3109 Harbor local | `[5, 5]`, duration 1 | Held | T2 is reserved normal by request 3104, but this route requires reverse |
| 7 | 3105 Night local | `[6, 10]`, duration 2 | Accepted `[7, 9)` | `[6, 8)` meets CEDAR occupancy; slot 7 is the first clear start |
| 8 | 3106 Freight 42 | `[7, 8]`, duration 2 | Held | Its protected overlap conflicts with request 3105 |
| 9 | 3108 Late special | `[15, 15]`, duration 2 | Rejected | The interval would exceed the 16-slot horizon |

The result contains 3 accepted, 4 held, and 2 rejected requests. `src/lib.sai`
asserts the order, outcome counts, selected start slots, and principal issue
codes. The fixture can be hand-checked by comparing each candidate interval
with the occupancy and the previously accepted reservations.

## Module map

- `src/model.sai` defines requests, routes, block occupancy, outcomes, issue
  codes, and the report.
- `src/data.sai` provides five routes, nine requests, and the fixed occupied
  block.
- `src/arbitration.sai` orders requests, validates IDs/routes/windows, scans
  candidate slots, checks protected-block overlap and turnout locks, and
  builds the report. Duplicate request IDs are rejected so neither duplicate
  can win a reservation.
- `src/lib.sai` exposes `fixture_report()` and source-level assertions for the
  hand-checked result and deterministic repeatability.

## Toolchain status

From the repository root, run:

```sh
sai fmt --check sample/railway-interlocking-auditor
```

The formatter check confirms that the files parse and are canonically
formatted under the current formatter. The command
`sai -C sample/railway-interlocking-auditor check` currently stops at
`FEATURE_NOT_IMPLEMENTED`: Stage 0 import resolution and type checking are
unavailable, and `@derive` is also gated. Stage 3
`sai test` has not been attempted because the compiler does not implement that
stage. The expected table above is manually derived from the fixed fixture and
the arbitration rules; it is not output from an SAI execution. The embedded
tests are source assertions and have not been executed.

## Review required

- The priority policy gives earlier requested start slots precedence and uses
  lower numeric priority only to break equal-start ties. Railway operating
  rules may require a different arbitration order, fairness policy, or
  dispatcher override.
- Every route reserves exactly three named blocks; the third is treated as a
  full-duration overlap-protection block. Real interlocking topology, flank
  protection, overlaps, release conditions, and approach locking require
  route-by-route engineering review.
- Existing occupancy is a static fixture. Real occupancy data can be stale,
  unavailable, failed, or uncertain; this sample does not model those states
  or require a safe response to them.
- Same-turnout requests in the same position are treated as compatible if
  their protected blocks do not overlap. Actual point detection, locking,
  traversal, movement authority, and route release semantics are outside this
  model.
- Slots, intervals, request windows, and the 16-slot horizon are fictional.
  They omit setup, clearance, train length, speed, braking, communications,
  timing uncertainty, and failures.
- The sample records a proposed alignment and reservation only. It does not
  prove signal aspects, route exclusivity, human procedures, hardware
  behavior, regulatory compliance, or operational safety.
- Duplicate request IDs cause every matching request to be rejected. A real
  system must define identifier provenance and how to reconcile duplicate or
  corrected requests.
- When several constraints block a request, the report preserves the first
  issue found while searching starts in ascending order; occupancy is checked
  before accepted block reservations, which are checked before turnout
  alignment. A real audit may need to report every simultaneous constraint.
