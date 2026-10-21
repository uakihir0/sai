# Temperature Converter

Temperature Converter turns one Celsius reading into Fahrenheit using
`fahrenheit = celsius × 1.8 + 32`.

```text
Celsius  Fahrenheit
0.0      32.0
20.0     68.0
100.0    212.0
```

`src/lib.sai` demonstrates an `F32` parameter, typed intermediate locals, and
floating-point multiplication and addition. It is a pure `wasm32-core`
library. Its function shape is covered by the compiler's primitive
type-checking tests. The current CLI still reports `FEATURE_NOT_IMPLEMENTED`
for `sai check` at Stage 0, so no CLI check or build is claimed. Producing and
running Wasm requires the Stage 1 backend.
