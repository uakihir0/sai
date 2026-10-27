# Robust Statistics Lab

This fixed-fixture library summarizes three fictional sensor batches with the
median and median absolute deviation (MAD), then reports measurements that
fall outside a documented robust threshold. The values are scaled, nonnegative
integers. The analyzer is deterministic and uses no clock, file, or network
access.

## Expected fixture result

Each batch has nine readings. The analyzer sorts a copy of each batch to find
its middle value. It then takes the median of the absolute differences from
that center. A reading is flagged when its absolute difference is greater
than four times the MAD.

| Sensor | Median | MAD | Threshold | Outliers |
|---|---:|---:|---:|---|
| North greenhouse probe | 12 | 2 | 8 | Reading 8: 100 (deviation 88) |
| Riverside probe | 20 | 1 | 4 | None |
| Market cold-room probe | 7 | 0 | 0 | Reading 8: 20 (deviation 13) |

The report contains 27 measurements and two flagged readings. For the first
batch, the median stays at 12 despite the extreme value 100; its MAD is 2.
This contrasts with the arithmetic mean, which is pulled upward by that
extreme value.

When MAD is zero, the threshold is also zero and any value different from the
median is flagged. The third fixture exercises that explicit fallback.

## Modules

- `src/model.sai` defines batches, summaries, individual findings, and the
  complete report.
- `src/fixtures.sai` contains the three nine-reading fixtures.
- `src/policy.sai` defines the robust threshold multiplier and absolute
  difference calculation.
- `src/statistics.sai` sorts a copy of the readings and calculates the median,
  MAD, and per-batch outliers.
- `src/analyzer.sai` combines results in stable batch and reading order.
- `src/lib.sai` exposes the fixture report and records source-level checks for
  the expected values, zero-MAD fallback, and input preservation.

## Scope and limits

This pure `wasm32-core` sample demonstrates fixed-size arrays, records,
insertion sort, median calculation, a second-pass dispersion statistic,
bounded result aggregation, and explicit edge-case policy. Each input batch
contains exactly nine readings; there are exactly three batches; each reading
is at most 1,000,000. Inputs over that bound return `ReadingOutOfRange` before
threshold multiplication, keeping the arithmetic safely inside `U32`.
The exported threshold helper also returns `ThresholdOutOfRange` when a direct
caller supplies a MAD above `1,073,741,823`, the greatest `U32` value that can
be multiplied by four without overflow.

The four-times-MAD threshold is a teaching policy, not a calibrated statistical
recommendation. The data is already grouped into batches; the example does
not infer windows, account for time dependence, estimate sensor drift, or
replace domain review. A zero MAD can mean either a highly stable process or
coarse quantization, so its fallback can overstate anomalies in real data.
Values are treated as magnitudes with no signed offset or unit conversion.

The expected results above are hand-checked. Embedded assertions describe the
intended behavior; they have not run in the current SAI compiler pipeline.

## Review required

- The multiplier of four was selected to leave the Riverside batch's ordinary
  variation unflagged while surfacing the deliberately isolated fixture
  values. It is an illustrative threshold, not a universally valid false
  positive rate.
- A zero MAD uses a zero threshold and flags every non-median observation.
  Other systems might suppress classification, apply a quantization floor, or
  require more data. This fallback should be reviewed before reusing it.
- Readings are nonnegative scaled integers and batches have a fixed odd size.
  Even-sized samples, negative values, missing readings, and uncertainty
  estimates need separate, explicit policies.
- The maximum accepted reading is 1,000,000 to bound `U32` arithmetic. That
  limit is chosen for this fixture's scale, not taken from a sensor
  specification.
- The exported threshold helper returns an error above the greatest safe
  `U32` input for multiplication by four instead of saturating the threshold.
  The batch-reading bound makes this unreachable through the analyzer, but
  direct callers still receive an explicit result.

## Formatting and compiler status

Run `sai fmt --check sample/robust-statistics-lab` from the repository root to
check formatting. The fixture table is not compiler output. The current
compiler still gates semantic checking at Stage 0 and `sai test` is not yet
implemented, so this project is not claimed to build or run.
