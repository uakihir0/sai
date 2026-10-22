# Air Quality Monitor

Air Quality Monitor demonstrates a small alert state machine for a fixed PM2.5
sensor stream. It confirms a high reading with consecutive samples before
opening an alert, then waits for consecutive clean samples before closing it.
A middle band prevents readings near a threshold from rapidly toggling the
alert.

The fixture is a single 12-reading stream from Harborview north pier. Its
expected summary is:

```text
readings: 12 (11 valid, 1 sensor fault)
alerts opened: 2
alerts closed: 1
current phase: alert active
valid-sample peak: 66 µg/m³
```

The first alert opens after two readings at or above 50 µg/m³ and closes after
two readings at or below 35 µg/m³. A later one-sample high is followed by a
sensor fault; that fault resets the pending streak and does not open an alert.
Two later high readings open the second alert. The invalid sample carries a
large sentinel measurement to show that faulty data is excluded from the peak.

`model.sai` defines readings, quality, alert phases, decisions, and the run
summary. `policy.sai` provides the fixed confirmation and recovery thresholds.
`fixtures.sai` holds the chronological sensor stream. `monitor.sai` applies the
hysteresis state machine, and `lib.sai` exposes the fixture and keeps its
expected transitions beside the implementation.

A sensor fault resets a pending high or clean streak. If an alert is already
active, a fault preserves the alert and resets any pending clear streak. A
reading between the clear and trigger thresholds resets pending streaks while
leaving an active alert open. “Normal” means that no alert is open; it does not
certify that air quality is safe.

This sample processes one already ordered series in memory. It does not ingest
or calibrate raw sensor frames, infer missing samples, combine stations,
forecast pollution, or send notifications. The policy assumes a positive
confirmation count and `clear_pm25 < trigger_pm25`; the fixture uses a valid
policy.

## Package and compiler status

This is a pure `wasm32-core` library. It has no process entry point, I/O,
external package, persistent state, or notification side effect. The bounded
multi-module example uses records, enums, fixed-capacity arrays, loops, and a
stateful decision pass.

This project is illustrative, not a build guarantee. The current compiler has
not completed the semantic and package pipeline needed to build this source.
The fixture transitions above are hand-checked; the SAI regression tests are
source examples and have not been executed.
