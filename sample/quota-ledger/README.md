# Quota Ledger

Quota Ledger is a pure library for reserving and releasing units against a
fixed capacity. A successful operation returns a new ledger; a rejected
operation returns `None` and leaves the input value available to the caller.

```text
starting ledger: capacity 12, reserved 9
available: 3
reserve 3 from starting ledger: capacity 12, reserved 12
reserve 4 from starting ledger: rejected (only 3 units remain)

separate ledger: capacity 12, reserved 3
release 4: rejected (only 3 units are reserved)
```

`src/ledger.sai` contains the `Ledger` record and its `available`, `reserve`,
and `release` operations. The functions state their valid-ledger precondition
and describe both successful and rejected outcomes in postconditions.
`src/lib.sai` contains fixed tests for boundary cases and properties for
capacity bounds, accepted-unit accounting, and round-trip conservation. The
properties generate `U32` inputs and check the operations across many values.

## Package and compiler status

This is a `wasm32-core` library with no process package, I/O, or runtime
capabilities. Its main teaching points are function `requires` and `ensures`
clauses, `result` and `old(..)`, and `test` and `property` declarations.
These are Stage 3 features. The example is illustrative: formatting is the
only compiler check that succeeds here. The current reference compiler reports
`FEATURE_NOT_IMPLEMENTED` for `sai check` at Stage 0 and for `sai test` at
Stage 3, so neither semantic correctness nor property-test execution is
claimed.
