# UNKNOWN_FIELD

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 1 | spec/06-expressions-and-statements.md | 0.1.0 | active |

## Summary

A field name that the record, variant or tuple type does not have

## Explanation

A record field access, record or variant construction, or constructor pattern can name only a field declared by the resolved type. A misspelled field name or a field that belongs to a different type produces `UNKNOWN_FIELD`. If the field exists but is not visible at the use site, the error is `PRIVATE_FIELD` instead.

An enum value does not have fields before its variant is matched. Accessing a name that appears in a variant's payload still produces `UNKNOWN_FIELD`; match on the variant first. Tuple field indices are zero-based: an index outside the tuple's arity produces `TUPLE_INDEX_OUT_OF_RANGE`, while using a field index on a non-tuple receiver produces `UNKNOWN_FIELD`.

## Example

```sai
type User {
    age: I32
}

impl User {
    pub fn age(self: borrow Self) -> I32 {
        self.ag // rejected: UNKNOWN_FIELD
    }
}
```

## Fix

```sai
type User {
    age: I32
}

impl User {
    pub fn age(self: borrow Self) -> I32 {
        self.age
    }
}
```

## See also

- `PRIVATE_FIELD`
- `TUPLE_INDEX_OUT_OF_RANGE`
- `spec/06-expressions-and-statements.md` §6.6
