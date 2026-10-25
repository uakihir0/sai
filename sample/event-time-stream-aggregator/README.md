# Event-Time Stream Aggregator

This fixed-fixture library groups fictional weather readings into five-minute
windows while allowing two minutes of out-of-order arrival. It separates
readings that are still eligible for aggregation from late or malformed
source rows, and retains aggregates for windows that have not closed yet.
It makes no network or clock calls.

## Hand-checked fixture

The input contains 20 rows from three weather masts. Input order is arrival
order, and timestamps are integer minutes after midnight on one fictional
service day. The windows are half-open intervals: `[0, 5)`, `[5, 10)`, and so
on. A reading exactly at a window's start belongs to that window; a reading
exactly at its end belongs to the next one.

The stream includes readings that arrive out of event-time order, two readings
that arrive after their windows have closed, a missing measurement, a repeated
event ID with a far-future timestamp, and an unknown sensor. Rejected rows
remain in the report with the watermark observed when each row arrived.

At the end of the fixture, the maximum accepted event minute is 22 and the
watermark is 20. Nine aggregates are closed, and two aggregates remain open:

| Window | Sensor | Count | Total | Minimum | Maximum | State |
|---|---|---:|---:|---:|---:|---|
| `[0, 5)` | North weather mast | 2 | 24 | 10 | 14 | Closed |
| `[0, 5)` | Riverside weather mast | 1 | 4 | 4 | 4 | Closed |
| `[0, 5)` | Market weather mast | 1 | 7 | 7 | 7 | Closed |
| `[5, 10)` | North weather mast | 2 | 31 | 15 | 16 | Closed |
| `[5, 10)` | Riverside weather mast | 1 | 8 | 8 | 8 | Closed |
| `[5, 10)` | Market weather mast | 1 | 11 | 11 | 11 | Closed |
| `[10, 15)` | North weather mast | 1 | 9 | 9 | 9 | Closed |
| `[10, 15)` | Riverside weather mast | 2 | 30 | 12 | 18 | Closed |
| `[15, 20)` | North weather mast | 2 | 28 | 8 | 20 | Closed |
| `[20, 25)` | Riverside weather mast | 1 | 22 | 22 | 22 | Open |
| `[20, 25)` | Market weather mast | 1 | 6 | 6 | 6 | Open |

The five findings, in source arrival order, are: event 1005 is late at
watermark 9; event 1007 has no measurement at watermark 12; the second event
2005 duplicates an earlier ID at watermark 18; event 1010 is late at watermark
20; and event 4001 names an unknown sensor at watermark 20. Fifteen rows are
accepted into aggregates. The duplicate timestamp at minute 30 and the
unknown-sensor timestamp at minute 40 do not move the watermark.

## Algorithm

For every row in arrival order:

1. Reject a repeated event ID, unknown sensor, out-of-range minute, or missing
   or out-of-range measurement, in that validation order.
2. Compare an otherwise valid row with the current watermark. The strict rule
   `event_minute < watermark` marks it late. A row exactly at the watermark is
   admitted.
3. Add an admitted value to its sensor's five-minute aggregate and advance the
   maximum accepted event minute if needed.
4. Compute `watermark = max(0, maximum_event_minute - 2)`. Close every open
   window whose exclusive end is less than or equal to that watermark.
5. Insert closed aggregates by window start and then sensor ID. At end of
   input, return any remaining open aggregates without forcing them closed.

Malformed and duplicate rows cannot advance event-time progress. A late row is
preserved as a finding and does not revise a closed aggregate. Open aggregate
slots are reused deterministically in arrival order; closed results use the
stable sort order described above.

## Source map

- `src/model.sai` defines readings, aggregate windows, findings, and the report.
- `src/data.sai` provides the three sensors and the 20-row arrival stream.
- `src/policy.sai` defines the window, lateness, bounds, watermark, and lookups.
- `src/aggregate.sai` updates per-sensor windows and closes them in stable order.
- `src/stream.sai` validates each row, advances the watermark, and builds the
  final report.
- `src/lib.sai` exposes the fixture result and source-level assertions.

## Concepts and limits

This pure `wasm32-core` sample demonstrates fixed arrays, records, enums,
`Option`, bounded counters, event-time aggregation, late-data side output,
watermark-driven closure, and deterministic ordering. Each report accepts at
most 20 rows, minutes are bounded to 10,000, and measurements are bounded to
1,000,000 so `U32` window and total arithmetic remains in range.

This is not a production streaming engine. It has no persisted offsets,
restarts, concurrency, wall clock, calendar or timezone model, configurable
window sizes, sliding windows, per-source watermark, dynamic sensor registry,
or correction/retraction protocol. The event-time watermark is a simple
maximum-seen-minus-two policy, not proof that no earlier events can arrive.
Only the fixed source-level assertions document the intended output; they have
not executed in the current compiler pipeline.

## Review required

- The late rule is strict: an event exactly at the current watermark is
  accepted. Some stream processors treat the watermark boundary as closed;
  that policy should be confirmed before using the example as a template.
- The watermark is shared by all sensors and is based on the largest accepted
  event timestamp. A fast sensor can therefore close another sensor's windows.
  Production systems often use per-partition progress and idleness policies.
- Two minutes of allowed lateness and five-minute windows are invented
  teaching values. They are not service-level guarantees or operational
  recommendations.
- Invalid rows are excluded from aggregation and cannot advance the watermark.
  A production consumer may instead quarantine rows, fail the partition, or
  apply a source-specific repair rule.
- At end of input, windows are not force-closed. They remain in `open_windows`
  because a finite fixture end is not equivalent to a source watermark of
  infinity.

## Formatting and compiler status

Run `sai fmt --check sample/event-time-stream-aggregator` from the repository
root to check syntax and formatting. The output table is hand-checked, not
compiler output. Current Stage 0 `sai check`, `@derive`, and Stage 3 `sai test`
requirements prevent the package from completing semantic checking and test
execution, so this sample is not claimed to build or run.
