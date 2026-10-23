# Transit Fare-Cap Auditor

This pure SAI library audits a fixed tap ledger against a small teaching fare
policy. It checks event identity and per-rider event order, calculates expected
fares with a transfer window and daily cap, compares them with recorded
charges, and leaves unmatched charges visible for investigation.

The audit is read-only. It does not charge or refund riders, resolve
disputes, or claim to represent any real transit agency's policy.

## Fixture and intended result

The fixture has three riders and eleven assigned taps, plus one charge for an
unknown rider ID. Times are minutes after midnight on one fictional service
day. The input keeps each rider's taps in chronological order; riders may be
interleaved without changing their individual order.

The teaching policy charges 240 cents for a first boarding, 60 cents for
another boarding within 30 minutes of the first boarding in that transfer
window, and no more than 650 cents per rider per day. Transfers do not extend
the original window. Once the cap is reached, later accepted taps cost zero.

| Rider | Expected | Recorded | Finding |
|---|---:|---:|---|
| Maya | 600¢ | 600¢ | none |
| Ren | 650¢ | 650¢ | none; the final full fare is clipped to the remaining 170¢ and the next tap is free |
| Aiko | 540¢ | 580¢ | charge mismatch: 40¢ over expected |

The assigned-ledger totals are 1,790 cents expected and 1,830 cents recorded.
The unmatched 240-cent charge remains separate and is not attributed to a
rider. The figures are hand-calculated from the fixture; no successful
compiler or runtime execution is claimed.

## Modules

- `model.sai` defines riders, taps, findings, and audit summaries.
- `data.sai` contains the fixed rider register and tap ledger.
- `policy.sai` states the fare, transfer, and cap rules and checks event IDs.
- `audit.sai` computes rider totals, detects malformed sequences, and reports
  unmatched charges.
- `lib.sai` exposes the fixture audit and regression cases.

## Limits

This bounded example has no calendar, zones, fare products, service-day
rollover, multi-agency rules, transaction reversals, or live payment
integration. It assumes recorded values are nonnegative integer cents and
uses a single fare policy for all riders. A duplicate event or out-of-order
tap holds that rider's sequence at the first structural issue. The expected
total then includes only earlier valid rows, while the recorded total still
includes all rows for that rider. Both totals are informational for a held
sequence and must not be treated as a final fare determination. Duplicate tap
IDs are counted across the entire source ledger, including unmatched rows.

## Review required

- The 240-cent fare, 60-cent transfer charge, 30-minute window, and 650-cent
  cap are invented teaching values. A transit agency would need to supply and
  approve its actual rules.
- The transfer window is anchored to the first boarding in the window. Some
  agencies define transfer eligibility differently; this choice prevents a
  chain of transfers from extending the window indefinitely.
- A structurally held ledger is not used to determine an amount due. The
  partial calculation only demonstrates which valid rows were processed
  before the ledger issue; a production auditor should require source repair
  and review.
- The unknown-rider charge is preserved as an unmatched source record. It is
  not included in any rider's expected or recorded totals.

## Package and compiler status

The manifest targets `wasm32-core`. This library uses records, fixed-size
arrays, loops, `Option`, enum findings, integer-cent arithmetic, and SAI test
declarations. The current compiler's Stage 0 `sai check` and Stage 3 `sai
test` are not implemented, and `@derive` is a Stage 2 feature. Formatting can
check the source shape, but no type-check, test, build, or execution result is
claimed.
