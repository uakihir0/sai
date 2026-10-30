# INVALID_ESCAPE

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 0 | spec/02-lexical-structure.md | 0.1.0 | active |

## Summary

Invalid escape sequence

## Explanation

Character, string, and byte literals accept only the escape sequences defined for their literal kind. For example, `\n`, `\\`, and a valid `\u{...}` escape are supported in a character literal; `\q` is not. A Unicode escape must denote a Unicode scalar value.

Replace the invalid escape with one of the forms allowed for that literal. If the backslash was meant to be literal text, escape it as `\\`.

## Example

```sai noncanonical
pub const VALUE: Char = '\q' // rejected: INVALID_ESCAPE
```

## Fix

```sai
pub const VALUE: Char = '\n'
```

## See also

- `spec/02-lexical-structure.md` §2.6.3 (character and byte literals)
- `spec/02-lexical-structure.md` §2.6.4 (string literals)
