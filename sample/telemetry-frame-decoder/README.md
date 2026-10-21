# Telemetry Frame Decoder

A pure `wasm32-core` library that validates and decodes one fixed eight-byte
sensor frame. It demonstrates byte slices, bit masks, XOR checksums, typed
records, structured errors, and a deterministic validation order.

The frame layout is an illustrative protocol invented for this sample, not a
standard sensor format.

## Frame format

| Byte | Meaning |
|---|---|
| 0 | Sync marker, `0xA5` |
| 1 | Protocol version, `0x01` |
| 2 | Flags: bit 0 is calibrated, bit 1 is alarm; bits 2–7 are reserved and must be zero |
| 3 | Sensor identifier |
| 4 | Channel identifier |
| 5–6 | Unsigned raw value in little-endian order |
| 7 | XOR of bytes 0–6 |

The valid fixture is `A5 01 03 07 02 34 12 84`. It decodes to sensor 7,
channel 2, raw value `0x1234`, calibrated and alarm flags set.

The decoder reports the first error in this order: wrong length, bad sync
marker, unsupported version, reserved flags, and checksum mismatch. The fixed
length check occurs before any indexing. The XOR byte is only a simple
transmission-error check; it is not a cryptographic integrity or authenticity
mechanism.

The tests also cover a short frame and fixtures where an earlier validation
failure takes precedence over a later checksum mismatch.

## Package and compiler status

This package uses only `sai:core`; it performs no I/O and requires no external
packages. `Byte` bitwise operators are specified for the `wasm32-core` target,
but the current compiler does not yet implement all stages needed to build the
example. The compiler implements only part of Stage 0 `sai check`; this
multi-module sample hits unsupported module-resolution and `@derive` gates.
The Stage 3 `sai test` command is not yet implemented. Neither command
completes semantic checking or test execution for this sample.
