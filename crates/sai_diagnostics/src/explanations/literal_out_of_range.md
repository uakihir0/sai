# LITERAL_OUT_OF_RANGE

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 0 | spec/02-lexical-structure.md | 0.1.0 | active |

## Summary

Literal out of range for its type

## Explanation

An integer literal's value must fit the type assigned to it. SAI keeps integer literals exact until their type is known, then reports `LITERAL_OUT_OF_RANGE` when the value is outside that type's range. For example, `U8` holds values from 0 through 255, so `999` does not fit.

Use a value within the type's range, or choose a type that can represent the intended value.

## Example

```sai
pub const VALUE: U8 = 999 // rejected: LITERAL_OUT_OF_RANGE
```

## Fix

```sai
pub const VALUE: U8 = 255
```

## See also

- `spec/02-lexical-structure.md` §2.6.1 (integer literals)
- `spec/05-types.md` §5.3.2 (literal types)
