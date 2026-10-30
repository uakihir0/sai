# DUPLICATE_BINDING

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 0 | spec/04-modules-and-names.md | 0.1.0 | active |

## Summary

Two bindings of one name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3, D-MOD-03, N-A-8)

## Explanation

Each scope can give a name to only one binding. Declaring the same name twice in one scope makes it unclear which declaration a reference denotes, so SAI reports `DUPLICATE_BINDING` on the later declaration and points back to the earlier one.

This applies to module items and to local bindings such as parameters and `let` declarations. Two imports of different entities under the same name also conflict; importing the same entity twice is instead reported as `REDUNDANT_IMPORT`.

Rename one binding when both declarations are needed, or remove the duplicate when it was added by mistake.

## Example

```sai
pub const VALUE: U32 = 0
pub const VALUE: U32 = 1 // rejected: DUPLICATE_BINDING
```

## Fix

```sai
pub const VALUE: U32 = 0
pub const OTHER: U32 = 1
```

## See also

- `spec/04-modules-and-names.md` §4.4.3
- `REDUNDANT_IMPORT`
