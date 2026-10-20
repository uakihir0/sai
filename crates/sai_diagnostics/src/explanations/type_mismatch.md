# TYPE_MISMATCH

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 1 | spec/05-types.md | 0.1.0 | active |

## Summary

Two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation

## Explanation

A type mismatch means that a context requires a value of one type but the value has a different type, and none of SAI's permitted coercions applies. The diagnostic names the expected type, the found type, and where the expectation came from, such as a parameter, an annotation, or a previous branch.

SAI does not silently convert between distinct numeric types or between integers and floats. Use an explicit conversion such as `I32.from(value)`, `I32.try_from(value)`, or `value.wrapping_to<I32>()` when it matches the intended behavior. A reported fix is specific to the expression; another mismatch may need its annotation or value changed instead.

## Example

```sai
// rejected: TYPE_MISMATCH
pub const COUNT: I32 = true
```

## Fix

```sai
pub const COUNT: I32 = 1
```

## See also

- `spec/05-types.md` §5.14 (type equality)
- `spec/05-types.md` §5.15.4 (explicit conversions)
- `spec/05-types.md` §5.16.3 (unification)
