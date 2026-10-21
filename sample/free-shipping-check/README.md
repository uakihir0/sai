# Free Shipping Check

Free Shipping Check is a tiny pure function that decides whether an order
subtotal reaches a fixed shipping threshold.

```text
subtotal (yen)  free shipping
4,999           no
5,000           yes
```

`src/lib.sai` demonstrates a typed `U32` parameter, a comparison with an
integer literal, and a `Bool` result. The free-shipping threshold is 5,000 yen.
The package has one source module, no imports, and no runtime capabilities. Its
function shape is covered by the compiler's primitive type-checking tests. The
current CLI still reports
`FEATURE_NOT_IMPLEMENTED` for `sai check` at Stage 0, so no CLI check or build
is claimed. It targets `wasm32-core`; generating and running Wasm requires the
Stage 1 backend.
