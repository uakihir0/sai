# MALFORMED_NUMBER

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 0 | spec/02-lexical-structure.md | 0.1.0 | active |

## Summary

Malformed numeric literal

## Explanation

An underscore in a numeric literal separates digits. It cannot appear at the end, directly after a base prefix, or twice in a row. Integer literals also have no type suffix; an identifier character immediately after the digits makes the literal malformed.

Decimal integers cannot add digits after an initial `0`, and base prefixes must be lowercase (`0x`, `0b`, or `0o`). Use a single underscore only between digits, remove unnecessary leading zeros, and write base prefixes in lowercase. For example, write `12` instead of `1__2`.

## Example

```sai noncanonical
pub const VALUE: U8 = 1__2 // rejected: MALFORMED_NUMBER
```

## Fix

```sai
pub const VALUE: U8 = 12
```

## See also

- `spec/02-lexical-structure.md` §2.6.1 (integer literals)
- `spec/02-lexical-structure.md` §2.6.2 (floating-point literals)
