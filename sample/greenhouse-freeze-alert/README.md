# Greenhouse Freeze Alert

Greenhouse Freeze Alert calibrates one temperature sensor reading and checks
whether the corrected temperature has reached a freeze-warning threshold. The
public function passes the result of one primitive helper call directly into
another primitive helper call:

```sai
is_at_or_below(
    calibrated_tenths(raw_tenths, offset_tenths),
    threshold_tenths
)
```

All three inputs and the calibrated result are signed integer tenths of a
degree Celsius. For example, `15` means `1.5 °C`, and `-4` means `-0.4 °C`.
The offset is added to the raw reading. The warning is inclusive: a calibrated
reading exactly equal to the threshold triggers an alert.

| Raw (0.1 °C) | Offset (0.1 °C) | Threshold (0.1 °C) | Calibrated (0.1 °C) | Freeze alert |
|---:|---:|---:|---:|:---|
| 18 | -3 | 15 | 15 | Yes, at the threshold |
| 16 | -3 | 15 | 13 | Yes, below the threshold |
| 18 | -2 | 15 | 16 | No, above the threshold |
| -4 | 2 | 0 | -2 | Yes, below zero |

The `wasm32-core` library has no imports, I/O, or runtime dependencies. Its
source illustrates only one level of direct-call nesting in a positional
argument. Deeper call nesting and other unsupported expression shapes are
intentionally omitted. The compiler's CLI check remains gated at Stage 0, and
this sample has not been built or run; the table describes intended behavior,
not verified compiler output. Generating and running Wasm also requires the
Stage 1 backend.
