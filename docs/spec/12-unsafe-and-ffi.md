# 12. Unsafe Code and FFI

**Status:** Normative
**Stage:** 2 (unsafe code and raw pointers); 1 (`extern "wasm"`, `@import_name`, `@export_name`); 7 (`unsafe extern "C"`, `core.ffi` native types)
**Related:** [decisions.md](../design/decisions.md) D-UNSAFE-01, D-SYN-08, D-MOD-02, D-CAP-02, D-WASM-03, D-WASM-04, D-EFF-03, D-ERR-01, D-THREAD-01, D-TOOL-02, D-TOOL-03; [draft-reconciliation.md](../design/draft-reconciliation.md) R-09, R-12; [01](01-introduction.md), [03](03-grammar.md), [04](04-modules-and-names.md), [05](05-types.md), [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [09](09-contracts-and-refinements.md), [10](10-errors-and-panics.md), [11](11-async-and-concurrency.md), [13](13-components-and-wit.md), [15](15-attributes-and-lints.md), [16](16-core-library.md), [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/12](../impl/12-semantic-tooling.md), [impl/13](../impl/13-cli.md), [package/01](../package/01-package-contract.md), [targets](../targets/targets-and-profiles.md)

This chapter defines unsafe code and the foreign function interfaces of SAI: unsafe contexts (`unsafe` blocks, `unsafe fn`, `unsafe trait` and `unsafe impl`, the `unsafe module` header), the complete list of unsafe operations, the raw pointer types `Ptr<T>` and `PtrMut<T>`, the intrinsics of `sai:core/mem`, the list of undefined behavior and the invariants unsafe code must uphold, core Wasm imports with `extern "wasm"`, core Wasm exports and `@export_name`, native FFI with `unsafe extern "C"` and the types of `sai:core/ffi`, the effects of unsafe and foreign calls, and auditing. The block form `unsafe { … }` and its typing belong to [06](06-expressions-and-statements.md), the borrow-checker side of raw pointers to [07](07-ownership-and-borrowing.md) §7.17, the effects `host` and `ffi` to [08](08-effects-and-capabilities.md), attribute syntax to [15](15-attributes-and-lints.md), and memory layout to [impl/06](../impl/06-wasm-lowering-and-layout.md).

---

## 12.1 The safety model

### 12.1.1 Safe and unsafe code

**Safe code** is every part of a program that is not in an unsafe context (§12.2). **Unsafe code** is code in an unsafe context. The guarantee of [01](01-introduction.md) §1.5.1 is:

1. Safe code can never cause undefined behavior, whatever it does and however it uses the libraries it calls.
2. Undefined behavior arises only when unsafe code violates a precondition or invariant stated in this chapter, in [11](11-async-and-concurrency.md) §11.4.2 (the `poll` contract), in [09](09-contracts-and-refinements.md) N-C-58, or in the `# Safety` section of an `unsafe fn` or `unsafe trait`.

A program whose execution performs undefined behavior has no meaning. The specification places no requirement on any part of that execution, including the parts before the undefined operation, and the compiler MAY assume that undefined behavior never happens.

### 12.1.2 What `unsafe` means

`unsafe` never turns a check off. The type checker, the borrow checker ([07](07-ownership-and-borrowing.md) §7.17 item 4) and the effect analysis ([08](08-effects-and-capabilities.md)) apply to unsafe code exactly as to safe code. `unsafe` only permits the operations of §12.3, each of which has preconditions that the compiler cannot check:

| Construct | Who promises what |
|---|---|
| an `unsafe` block | its author promises that every unsafe operation in it meets its preconditions |
| an `unsafe fn` | its author states preconditions in a `# Safety` section. Every caller promises to meet them |
| an `unsafe trait` | its author states an invariant. Every `unsafe impl` promises to uphold it |
| an `unsafe module` | its author takes on the promise of an `unsafe` block for every body in the file |

A safe function whose body contains unsafe code is **sound** if no call of it from safe code, with any arguments and in any state reachable by safe code, can cause undefined behavior. Every safe function MUST be sound. An unsound safe function is a bug in the package that declares it, and the undefined behavior it allows is attributed to its unsafe code.

### 12.1.3 The Wasm sandbox

On Wasm targets, an instance cannot access memory outside its own linear memory, and it can act on the world only through its imports. Undefined behavior in SAI code is therefore confined to the instance and to what its imports permit. This is a property of the target, not a definition of behavior: the compiler still MAY assume that undefined behavior never happens, and a program with undefined behavior MAY corrupt its own memory, return wrong results, or trap.

### 12.1.4 Stages

| Feature | Stage |
|---|---|
| `extern "wasm"` blocks, `@import_name`, core `export fn`, `@export_name` | 1 |
| unsafe contexts, unsafe operations, `Ptr`, `PtrMut`, `sai:core/mem`, `sai:core/ptr`, `unsafe trait`, `unsafe impl`, `unsafe module` | 2 |
| `unsafe extern "C"`, the native types of `sai:core/ffi` (`CStr`, C integer aliases, `Out<T>`) and `CString` of `sai:alloc/ffi`, the effect `ffi` | 7 |

Before its stage, every use of a feature is `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md)). `Ptr` and `PtrMut` are defined in `sai:core/ptr` at Stage 2 and re-exported by `sai:core/ffi` (R-12), so raw pointers do not depend on Stage 7.

---

## 12.2 Unsafe contexts

An expression is in an **unsafe context** when `Γ.unsafe` holds for it ([06](06-expressions-and-statements.md) §6.1.1). This section defines exactly when that is.

### 12.2.1 `unsafe` blocks

`unsafe { … }` is an expression ([03](03-grammar.md) `PrimaryExpr`). It is typed as a block whose contents are checked with `Γ.unsafe = true` ([06](06-expressions-and-statements.md) §6.20.1). Its value and type are those of the block.

- The unsafe context covers every expression lexically inside the block (N-I-21), **except** the bodies of closures and of `async` blocks nested in it. A closure body and an `async` block are new function boundaries and start with `Γ.unsafe = false` ([06](06-expressions-and-statements.md) N-C-17, [11](11-async-and-concurrency.md) §11.2.3). An unsafe operation inside them needs its own `unsafe` block.
- An `unsafe` block that contains no unsafe operation outside such nested bodies is the warning `UNUSED_UNSAFE`. An `unsafe` block that is itself already in an unsafe context is `UNUSED_UNSAFE` too, with the note "redundant: already in an unsafe context". Both carry a fix that removes the `unsafe` keyword.
- An `unsafe` block SHOULD be as small as possible, and SHOULD be preceded by a comment that explains why its operations meet their preconditions.

```sai
use sai:core/ptr.{Ptr}

fn first_byte(bytes: borrow Array<Byte, 4>) -> Byte {
    let p = Ptr.from(bytes)
    // In bounds and initialized: `bytes` has 4 bytes.
    unsafe { *p.cast<Byte>() }
}
```

### 12.2.2 `unsafe fn`

A function declared with the `unsafe` marker ([03](03-grammar.md) `FnHead`) is an **unsafe function** (N-I-22):

1. **Calling it** is an unsafe operation (§12.3). Outside an unsafe context the call is `UNSAFE_OPERATION_OUTSIDE_UNSAFE`.
2. **Its body is an unsafe context** ([06](06-expressions-and-statements.md) §6.1.1, `Γ.unsafe`). Closures and `async` blocks inside the body are not (§12.2.1).
3. Its item type implements none of `Fn`, `FnMut`, `FnOnce`, and coerces only to `unsafe fn` pointer types ([05](05-types.md) §5.5.1, §5.15.2). A safe function item coerces to both safe and `unsafe fn` pointer types (05 §5.15.2 item 2). Calling through a value of an `unsafe fn(…)` pointer type is an unsafe operation.
4. A `pub` unsafe function, method or trait method without a doc comment section whose heading is `# Safety` is the warning `MISSING_SAFETY_DOC`.
5. An unsafe function cannot be an interface function or implement one (`INTERFACE_FN_CLAUSE`, [13](13-components-and-wit.md)), and it cannot be a core export (`UNSAFE_EXPORT`, §12.8). `unsafe async fn` is defined in [11](11-async-and-concurrency.md) §11.2.2.
6. A trait method declared `unsafe fn` is implemented by an impl method declared `unsafe fn`. The markers must match ([05](05-types.md) §5.9.1 rule 6, `TRAIT_ITEM_SIGNATURE_MISMATCH`).

The body of an unsafe function MAY assume its stated preconditions. A call that violates a stated precondition is undefined behavior (§12.6.1 item 11), whether or not the body relies on that precondition, so that the author can later rely on it without breaking callers.

### 12.2.3 `unsafe trait` and `unsafe impl`

An **unsafe trait** is a trait declared with the `unsafe` marker ([03](03-grammar.md) `TraitDecl`). It has an invariant that the compiler cannot check. The invariant is stated in a doc section headed `# Safety`, and generic code MAY rely on it for soundness: a function with a bound `T: Tr`, where `Tr` is unsafe, may use unsafe operations whose preconditions follow from the invariant.

1. Every impl of an unsafe trait MUST be written `unsafe impl` ([03](03-grammar.md) `ImplDecl`). By writing it, the author promises that the implementing type upholds the invariant.
2. The following are `IMPL_UNSAFETY_MISMATCH` (N-I-23): an impl of an unsafe trait without `unsafe`, an `unsafe impl` of a trait that is not unsafe, and an `unsafe impl` that is an inherent impl or an interface impl ([03](03-grammar.md) §3.7).
3. The marker concerns the impl, not its items. The methods of an unsafe trait are safe to call unless they are themselves declared `unsafe fn`, and the bodies inside an `unsafe impl` are not unsafe contexts.
4. `@derive` never generates an impl of an unsafe trait. None of the derivable traits of D-TYPE-08 is unsafe.
5. A `pub` unsafe trait without a `# Safety` section is the warning `MISSING_SAFETY_DOC`.

```sai
/// A type whose every bit pattern of `size_of<Self>()` bytes is a valid value.
///
/// # Safety
///
/// An impl promises that `Self` has no padding bytes and no invalid bit patterns.
pub unsafe trait Plain {}

unsafe impl Plain for U32 {}

impl Plain for Bool {}              // rejected: IMPL_UNSAFETY_MISMATCH (and Bool is not plain)
```

### 12.2.4 `unsafe module`

The header `unsafe module` ([03](03-grammar.md) `ModuleHeader`, D-SYN-08, R-09) declares that the whole file is unsafe code (N-I-24):

1. Every function body in the file, including the bodies of closures and of `async` blocks, is an unsafe context. This is the only way in which closure and `async` block bodies become unsafe contexts without an `unsafe` block of their own.
2. Predicates are never unsafe contexts: refinement predicates ([05](05-types.md) §5.6.4), `requires` and `ensures` clauses and `invariant` declarations ([09](09-contracts-and-refinements.md)) are not unsafe contexts even in an unsafe module. An unsafe operation in a predicate is `UNSAFE_OPERATION_OUTSIDE_UNSAFE`, and an `unsafe` block in it is `INVALID_CONTRACT_EXPRESSION` (09 §9.1.2).
3. The header does not change any other rule. Calls of `unsafe fn` from other files still need an unsafe context there, `unsafe impl` is still required for unsafe traits, and `unsafe fn` declarations keep their meaning.
4. Every `unsafe` block in an unsafe module is `UNUSED_UNSAFE` (redundant). An unsafe module that contains no unsafe operation is `UNUSED_UNSAFE`, with a fix that removes the header.
5. The header is reported by `sai inspect unsafe` as one entry that covers the file (§12.11).

An unsafe module is meant for files that consist mostly of unsafe code, such as the implementation of a data structure on raw pointers. It SHOULD contain only that code, so that the rest of the package keeps the guarantees of safe code.

```sai
//! Intrusive list nodes. Every function of this file manipulates raw pointers.
unsafe module

use sai:core/ptr.{PtrMut}

type Node {
    value: I64
    next: Option<PtrMut<Node>>
}

fn link(head: PtrMut<Node>, tail: PtrMut<Node>) {
    (*head).next = Some(tail) // unsafe operation, allowed by the header
}
```

### 12.2.5 Operations outside an unsafe context

An unsafe operation (§12.3) that is not in an unsafe context is the error `UNSAFE_OPERATION_OUTSIDE_UNSAFE`. The diagnostic names the operation and the reason it is unsafe, and carries a fix that wraps the smallest enclosing expression statement in an `unsafe` block. The fix is marked as requiring review, because the author must establish the preconditions (D-TOOL-01).

```sai
use sai:core/ptr.{Ptr}

fn peek(p: Ptr<I32>) -> I32 {
    *p                              // rejected: UNSAFE_OPERATION_OUTSIDE_UNSAFE
}
```

---

## 12.3 Unsafe operations

### 12.3.1 The complete list

The following operations, and only these, are **unsafe operations**. Each one is permitted only in an unsafe context (§12.2), and each one has preconditions stated in the section given.

| # | Operation | Preconditions |
|---|---|---|
| 1 | a dereference `*e` where `e : Ptr<T>` or `e : PtrMut<T>`, in any use: reading it, assigning to it, accessing a field or an element through it, borrowing it (`borrow *e`, `borrow mut *e`), or calling a method on it | §12.4.4 |
| 2 | a call of an `unsafe fn`: a function, method or trait method declared `unsafe fn`, including `Future.poll`, `Stream.poll_next` ([11](11-async-and-concurrency.md) §11.4), the unsafe functions of `sai:core` and `sai:alloc` ([16](16-core-library.md)), and `unsafe fn` items of an `extern "wasm"` block (§12.7.2) | the `# Safety` section of the callee |
| 3 | a call through a value of an `unsafe fn(…)` pointer type ([05](05-types.md) §5.5.2) | the `# Safety` section of the function the pointer designates |
| 4 | a call of a function declared in an `unsafe extern "C"` block | §12.9 |
| 5 | an `unsafe impl` of an unsafe trait | the `# Safety` section of the trait (§12.2.3) |

Item 5 is not evaluated at run time. It is listed because it is an assertion that the compiler cannot check and that `sai inspect unsafe` reports. It is allowed in every context, since the `unsafe` keyword of the impl is itself the marker.

### 12.3.2 Operations that are not unsafe

The following operations are safe, because no use of them in safe code can cause undefined behavior:

- creating a raw pointer from a borrow, from a `Box`, or from a slice (§12.4.2), copying and comparing raw pointers, and converting them to and from addresses (§12.4.3);
- calling a safe function of an `extern "wasm"` block (§12.7). It has the effect `host` instead (D-UNSAFE-01, [08](08-effects-and-capabilities.md));
- `size_of`, `align_of`, `forget`, `swap`, `replace`, `take` and `drop` (§12.5). Leaking a value is not undefined behavior ([07](07-ownership-and-borrowing.md));
- every operation on `MaybeUninit<T>` except `assume_init` and the access through its raw pointer (§12.5.3).

### 12.3.3 No unions

SAI has no untagged unions (N-I-36). Reinterpreting the bytes of a value as another type is done with `transmute` (§12.5.2) or through raw pointers, and storage that may hold no valid value is `MaybeUninit<T>` (§12.5.3).

---

## 12.4 Raw pointers

### 12.4.1 The types `Ptr<T>` and `PtrMut<T>`

`Ptr<T>` (a pointer for reading) and `PtrMut<T>` (a pointer for reading and writing) are the raw pointer types (N-I-25). They are lang-item types of `sai:core/ptr` ([16](16-core-library.md) §16.3.2), re-exported by `sai:core/ffi` (R-12), and not in the prelude. Their methods and signatures are listed in [16](16-core-library.md) §16.12.2. This section defines what the operations mean and what they require.

1. A raw pointer is an address in linear memory, of the target's pointer width ([impl/06](../impl/06-wasm-lowering-and-layout.md)). Because every SAI type is sized ([05](05-types.md)), a raw pointer is always a single address.
2. A raw pointer is never null. A pointer that may be null is written `Option<Ptr<T>>` or `Option<PtrMut<T>>`. At the `extern "C"` boundary (§12.9.3) such an option is passed as one address, with `None` as 0. Raw pointers do not cross the `extern "wasm"` and core export boundaries, which take addresses as `USize` (§12.7.4).
3. Raw pointers are `Copy` and `Clone`. They implement `PartialEq`, `Eq` and `Hash` by address ([16](16-core-library.md) §16.5.5).
4. A raw pointer carries no region ([07](07-ownership-and-borrowing.md) §7.17 item 1). Safe code may create, store, copy and compare raw pointers without limit. The obligations arise only when a pointer is used by an unsafe operation.
5. The places behind a raw pointer are not tracked by the borrow checker: they are not move paths, and their initialization is not tracked (07 §7.17 item 2). Unsafe code is responsible for them.

### 12.4.2 Creating raw pointers

The following are safe:

| Operation | Result |
|---|---|
| `Ptr.from(borrow p)`, `PtrMut.from(borrow mut p)` | the address of the place `p` |
| `Ptr.from(r)`, `PtrMut.from(r)` for a value `r : borrow T` or `r : borrow mut T` | the address that `r` refers to |
| `Ptr.dangling()`, `PtrMut.dangling()` | the address `align_of<T>()`: non-null and aligned, valid only for accesses of size 0 |
| `PtrMut.from_const(p)`, `q.as_const()`, `p.cast<U>()` | the same address with another pointer type |
| `Box.into_raw(b)` | the address of the boxed value. The box is consumed and its value is not dropped |
| `as_ptr` and `as_mut_ptr` of views and containers ([16](16-core-library.md)) | the address of the first element |

`Ptr.from` and `PtrMut.from` are inherent functions, not `From` impls (16 §16.12.2): the pointer types carry no region, so a `From<borrow T>` impl would bind the region of the borrow to `'static`. When the argument is a borrow expression of a place, the call is the creation operation of [07](07-ownership-and-borrowing.md) §7.17 item 1: it is a deep read access (for `Ptr`) or a deep write access (for `PtrMut`) of the place and creates no loan. It is lowered to the MIR rvalue `RawPtr(Const | Mut, p)` (D-MIR-01). `PtrMut.from(borrow mut p)` requires `p` to be a mutable place (`BORROW_MUT_OF_IMMUTABLE`, 07 §7.2.4).

### 12.4.3 Addresses

1. `p.addr()` returns the address as a `USize`. It is safe. Its numeric value is unspecified ([01](01-introduction.md) §1.5.3 item 2), but it is stable for the lifetime of the allocation, and two pointers to the same place have the same address.
2. `Ptr.from_addr(a)` and `PtrMut.from_addr(a)` return `None` if `a` is 0 and `Some` pointer with address `a` otherwise. They are safe.
3. SAI has no pointer provenance. A pointer is valid for an access if its address satisfies §12.4.4, however the pointer was obtained, including from an integer. The aliasing rules of §12.6.4 are stated in terms of places and loans, not in terms of where a pointer came from (N-I-25).

### 12.4.4 Dereference

For `e : Ptr<T>` or `e : PtrMut<T>`, `*e` is a place expression of type `T` ([06](06-expressions-and-statements.md) §6.9.1), allowed only in an unsafe context (§12.3.1 item 1). Its mutability ([06](06-expressions-and-statements.md) §6.2.3) is `mut` if `e : PtrMut<T>` and `imm` if `e : Ptr<T>`. Assigning to `*e` with `e : Ptr<T>` is `ASSIGN_TO_IMMUTABLE`, and `borrow mut *e` is `BORROW_MUT_OF_IMMUTABLE` (07 §7.2.4).

Every access through `*e` (a read, a write, or an access through a field or element projection of `*e`) MUST meet these preconditions at the time of the access. A violation is undefined behavior (§12.6.1):

1. **Alignment.** The address is a multiple of `align_of<T>()`.
2. **Bounds.** The `size_of<T>()` bytes at the address lie inside one live allocation: the storage of a live local, a heap allocation that has not been freed, or the storage of a constant or literal. For a zero-sized `T`, every non-null aligned address is valid.
3. **Validity.** A read requires the bytes to hold a valid value of `T` (§12.6.2).
4. **Aliasing.** The access does not conflict with a live loan (§12.6.4). The storage of constants and literals is never written.

The language rules for each use of `*e` are:

- **Reading** `*e` in a value context copies the value if `T: Copy`. Otherwise it would move out, which is `MOVE_OUT_OF_BORROW` (07 §7.17 item 2), with a fix that uses `e.read()` (§12.4.5).
- **Assigning** `*e = v` first drops the old value of `*e` (07 §7.13), so the old value MUST be valid. To write to memory that holds no valid value, use `e.write(v)`.
- **Borrowing** `borrow *e` or `borrow mut *e` creates a loan whose region has no reborrow constraint (07 §7.17 item 3). The unsafe code MUST choose a region in which every access through the resulting borrow meets the four preconditions above. The borrow is then an ordinary borrow, and all rules of 07 apply to it.
- **Auto-deref** ([06](06-expressions-and-statements.md) §6.9.3) never goes through a raw pointer. `p.f` and `p.m()` refer to the members of `Ptr<T>` itself. A field or method of the pointee is reached as `(*p).f` or `(*p).m()`.

### 12.4.5 Raw memory operations

The unsafe functions of `sai:core/ptr` and the related functions of `sai:alloc/boxed` have the following meaning. Their exact signatures are in [16](16-core-library.md) §16.12.2 and §16.15.2. "Accessible" means that the access meets preconditions 1 to 4 of §12.4.4, except where a row says otherwise.

| Function | Unsafe | Meaning | Preconditions in addition to accessibility |
|---|---|---|---|
| `p.read()` | yes | returns a bitwise copy of `*p` and leaves the memory unchanged | the bytes hold a valid `T`. If `T` is not `Copy`, the caller must ensure that only one of the two copies is dropped or used as an owner |
| `q.write(v)` | yes | stores `v` in `*q` without reading or dropping the old contents | none. The old contents may be uninitialized, and a valid old value is leaked |
| `p.read_unaligned()`, `q.write_unaligned(v)` | yes | as `read` and `write` | precondition 1 (alignment) does not apply |
| `q.drop_in_place()` | yes | runs the drop glue of `T` on `*q` (07 §7.13.3). Afterwards the memory holds no valid value | the bytes hold a valid `T` that no other owner will drop |
| `p.add(n)`, `p.sub(n)`, `p.offset(n)` | yes | the address moved by `n × size_of<T>()` bytes (`n : USize` for `add` and `sub`, `n : ISize` for `offset`) | the result lies in the same allocation as `p` or one element past its end, and the address computation does not overflow. For a zero-sized `T`, the result is `p` |
| `p.offset_from(origin)` | yes | the distance `(p.addr() − origin.addr()) / size_of<T>()` as an `ISize` | both pointers lie in the same allocation, the distance is a multiple of `size_of<T>()`, and `T` is not zero-sized |
| `Box.from_raw(q)` | yes | the box that owns `*q` | `q` was returned by `Box.into_raw` for a `Box<T>`, and no other box owns it |

`Box.into_raw` followed by `Box.from_raw` gives back an equivalent box. Freeing memory that was not allocated by the global allocator, or freeing it twice, is undefined behavior (§12.6.1 item 5).

### 12.4.6 Example

A safe function implemented with raw pointers. It is sound because both pointers are in bounds, aligned and initialized, and no other borrow of the array is used between the creation of the pointers and their last use.

```sai
use sai:core/ptr.{PtrMut}

/// Exchanges the first and the last element.
fn swap_ends(xs: borrow mut Array<I64, 4>) {
    let first = PtrMut.from(borrow mut (*xs)[0])
    let last = PtrMut.from(borrow mut (*xs)[3])
    unsafe {
        let tmp = *first
        *first = *last
        *last = tmp
    }
}
```

---

## 12.5 `sai:core/mem`

The items of `sai:core/mem` are listed in [16](16-core-library.md) §16.12.1. `size_of`, `align_of`, `transmute`, `zeroed` and `forget` are intrinsics (16 §16.3.4). This section defines the semantics that unsafe code relies on.

### 12.5.1 Safe items

1. `size_of<T>()` and `align_of<T>()` return the size and the alignment of `T` in bytes. The values are implementation-defined ([01](01-introduction.md) §1.5.2, D-WASM-03): they are fixed for a given compiler version and target, and they are the same in every build of a program for that target. `size_of<T>()` is a multiple of `align_of<T>()`, and `align_of<T>()` is a power of two.
2. `forget(v)` consumes `v` without running its drop glue. It is safe: leaking is not undefined behavior ([07](07-ownership-and-borrowing.md)), and safe code can already leak with `Box.into_raw`.
3. `swap`, `replace`, `take` and `drop` are ordinary safe functions ([16](16-core-library.md) §16.12.1). `drop(v)` is the same as letting `v` go out of scope.

### 12.5.2 `transmute`

`transmute<A, B>(value: A) -> B` is an unsafe function that reinterprets the bytes of `value` as a value of type `B` (N-I-32):

1. `size_of<A>()` and `size_of<B>()` MUST be equal. The check is made on the generic signature: if either size depends on a type parameter, or if the sizes differ for the target being compiled, the call is the error `TRANSMUTE_SIZE_MISMATCH`. The diagnostic gives both sizes.
2. `value` is consumed and its drop glue does not run. The result is a bitwise copy of its bytes.
3. The bytes MUST form a valid value of `B` (§12.6.2), and the result MUST NOT break a safety invariant of `B` that safe code relies on (§12.6.3). Transmuting between a type and a borrow, or between two borrow types, creates a borrow whose region is unconstrained, as in §12.4.4. This is the "region-changing transmute" of [07](07-ownership-and-borrowing.md) §7.17 item 5, and the unsafe code MUST ensure that the region is valid.

`transmute` between two types of different alignment is allowed, because the value is copied.

### 12.5.3 `zeroed` and `MaybeUninit<T>`

1. `zeroed<T>()` is unsafe. It returns a value of `T` whose bytes are all zero. The all-zero pattern MUST be valid for `T` (§12.6.2). It is valid for the integer, float and `Byte` types, `Bool` (`false`), `Char` (`'\0'`), `()`, `Option<Ptr<T>>` and `Option<PtrMut<T>>` (`None`), and records, tuples and arrays of such types. It is not valid for raw pointers, borrows, `Box<T>`, function pointers, or `Never`.
2. `MaybeUninit<T>` is storage for a `T` that may hold no valid value. It has the size and alignment of `T`. Every byte pattern, including uninitialized bytes, is a valid `MaybeUninit<T>`. It is not `Copy`, and dropping it never drops the `T` it may contain.

| Item | Unsafe | Meaning |
|---|---|---|
| `MaybeUninit.uninit()` | no | storage whose bytes are uninitialized |
| `m.write(v)` | no | stores `v`. The previous contents are not dropped |
| `m.as_ptr()`, `m.as_mut_ptr()` | no | a `Ptr<T>` or `PtrMut<T>` to the storage. Access through it follows §12.4.4 |
| `m.assume_init()` | yes | consumes `m` and returns the `T`. The storage MUST hold a valid `T` (§12.6.2) |

A typical use is an out-parameter of a foreign function (§12.9.5).

---

## 12.6 Undefined behavior

### 12.6.1 The list of undefined behavior

The execution of a program has undefined behavior if, and only if, it does one of the following. Every item can only be caused by an unsafe operation or by code reached through one (§12.1.1).

1. Accessing memory through a raw pointer, or through a borrow created from one, that is misaligned, out of the bounds of a live allocation, or dangling (§12.4.4).
2. Reading a value that does not satisfy the validity invariant of its type, or producing such a value, for example with `transmute`, `zeroed` or `assume_init` (§12.6.2).
3. Making a value that breaks a safety invariant reachable from safe code (§12.6.3).
4. An access that violates the aliasing rules (§12.6.4).
5. Using a value after its drop glue has run, running drop glue twice on the same value, freeing memory that was not allocated by the global allocator, freeing memory twice, or freeing it with a layout other than the one it was allocated with ([16](16-core-library.md) §16.15.1).
6. Moving or reusing the storage of a future or stream after its first poll, or freeing that storage without running the drop glue of the future (§12.6.5).
7. Violating the contract of `Future.poll`, `Stream.poll_next`, or the waker vtable ([11](11-async-and-concurrency.md) §11.4.2, §11.4.3).
8. Performing a primitive operation that the effect set of the code does not permit (§12.6.6).
9. Violating an obligation of [09](09-contracts-and-refinements.md) N-C-58, or returning from a foreign function in a state that violates its `ensures` clause (09 N-C-44, §12.6.7).
10. Calling a function through a function pointer whose type differs from the function's type, including its parameter and return types, its effect set and its `unsafe` marker.
11. Violating a precondition stated in the `# Safety` section of an `unsafe fn` or an invariant stated in the `# Safety` section of an `unsafe trait`.
12. A call of a safe function of an `extern "wasm"` block during which the host writes to linear memory (§12.7.2).
13. A data race with a native thread, or an unwind into SAI code from a foreign function (§12.9.6).

### 12.6.2 Validity invariants

The **validity invariant** of a type is the condition that the bytes of a value of that type MUST satisfy whenever the value is read, copied, moved, passed or returned, even by unsafe code (N-I-26). The compiler relies on it, for example for niches (D-WASM-03) and for the facts of the Standard Fact Checker ([09](09-contracts-and-refinements.md)).

| Type | Validity invariant |
|---|---|
| `I8` … `I64`, `U8` … `U64`, `ISize`, `USize`, `F32`, `F64`, `Byte` | every byte is initialized. Every bit pattern is valid |
| `Bool` | one byte, 0 (`false`) or 1 (`true`) |
| `Char` | a Unicode scalar value: at most 0x10FFFF and outside 0xD800–0xDFFF |
| `()`, and every zero-sized type | always valid |
| `Never` | no value is valid. Producing a value of type `Never` is undefined behavior |
| records, tuples, `Array<T, N>` | every field or element is valid. Padding bytes may hold anything |
| enums | the discriminant is the discriminant of a declared variant, and the payload is valid for that variant |
| distinct types | the invariant of the base type |
| refined types | the base type's invariant, and the refinement predicate holds ([09](09-contracts-and-refinements.md) §9.2) |
| `Ptr<T>`, `PtrMut<T>` | the address is not 0 |
| `borrow T`, `borrow mut T` | the address is not 0, is aligned for `T`, and for every point of the borrow's region the referent is accessible and holds a valid `T` (§12.4.4) |
| `Box<T>` | as `borrow mut T`, and the box owns its allocation, which was allocated by the global allocator (or is `Ptr.dangling()` for a zero-sized `T`) |
| `Slice<T>`, `SliceMut<T>`, `Str` | the address is not 0 and is aligned for the element type, and the `len × size_of<T>()` bytes lie in one live allocation and hold valid elements |
| function pointers | the value designates a function whose type is exactly the pointer type (§12.6.1 item 10) |
| closures | every capture is valid |
| `MaybeUninit<T>` | always valid |
| imported resource handles | a handle that the instance currently owns or borrows ([13](13-components-and-wit.md)) |

Uninitialized bytes are valid only as padding and inside `MaybeUninit<T>`. Reading an uninitialized integer is undefined behavior, not an unspecified value.

### 12.6.3 Safety invariants

A **safety invariant** is a condition that the safe API of a type relies on, but that the compiler does not rely on for the value itself. The safety invariants are:

1. the bytes of a `Str` are valid UTF-8 ([05](05-types.md) §5.4.3);
2. the invariants of the library types of `sai:alloc` (for example, the length of a `Vec<T>` is at most its capacity, and its first `len` elements are valid), as stated in [16](16-core-library.md);
3. the type invariants declared with `invariant` (D-CONTRACT-04, [09](09-contracts-and-refinements.md) §9.4);
4. the invariants stated in the `# Safety` section of an unsafe trait, for the types that implement it.

Unsafe code MAY break a safety invariant temporarily, inside its own code. It MUST restore it before the value becomes reachable from safe code: before it is returned, passed to a safe function, stored where safe code can reach it, or before a panic can occur. Making a value that breaks a safety invariant reachable from safe code is undefined behavior, because the safe code that receives it may then cause undefined behavior without any unsafe operation of its own.

### 12.6.4 Aliasing

The aliasing rules extend the borrow rules of [07](07-ownership-and-borrowing.md) to accesses that the borrow checker does not see (N-I-27). They are stated in terms of loans and regions, as computed by 07 for the whole program, including unsafe code:

1. A **pointer access** is a read or write of a place `P` through a raw pointer, or through a borrow created from a raw pointer, at a program point `q`. It is treated as a direct access of `P` at `q`.
2. A pointer access is undefined behavior if it conflicts, by the conflict rules of 07 (a read conflicts with a live mutable loan of an overlapping place, and a write conflicts with any live loan of an overlapping place), with a loan that is live at `q`, unless the pointer is **derived from** that loan and the access is permitted by it. A pointer is derived from a loan `L` if it was created from the borrow value of `L`, from a place reached through that borrow value (a reborrow), or from another pointer derived from `L` (by `cast`, `add`, `sub`, `offset`, `as_const` or `from_const`). A shared loan permits reads through pointers derived from it. A mutable loan permits reads and writes.
3. A borrow created from a raw pointer (`borrow *p`, `borrow mut *p`, a region-changing `transmute`) is a loan of the pointee for the region that 07 infers from its uses. While that region is live, every other access of the pointee is checked against it by rule 2. In particular, a place under a live shared loan is not written by any means, and a place under a live mutable loan is not accessed except through that loan.
4. Constants, literals, and the data of `Str` literals are under a shared loan for the whole execution. Writing to them is undefined behavior.
5. **The arena exception.** The places of a value of the lang-item type `Arena` ([16](16-core-library.md) §16.15.5) are not protected by shared loans. The methods `alloc`, `alloc_str` and `alloc_slice` of `sai:alloc` write them while the shared loan held by the `r` of a `region` block is live, and that is not undefined behavior. `Arena` is the only such type. Unsafe code outside `sai:alloc` MUST NOT read or write the places of an `Arena`.

Rule 3 is the "no interior mutability" property: safe code can rely on the value behind a live `borrow T` never changing ([09](09-contracts-and-refinements.md) §9.6.2, N-C-58; [11](11-async-and-concurrency.md) §11.12). Rule 5 is its only exception, and safe code cannot observe it: `Arena` has no public fields and no method with an empty effect set, so no value that safe code reads through a `borrow Arena`, and no pure function, depends on the state that the methods change.

### 12.6.5 Futures and streams

The state of a future may contain borrows of its own fields ([11](11-async-and-concurrency.md) §11.5). After the first call of `poll` on a future, or of `poll_next` on a stream, until the future is dropped:

1. its storage MUST NOT be moved, copied by `read`, or reused for another value;
2. its storage MUST NOT be freed, or go out of scope, before its drop glue has run (`forget` on a polled future that is still in storage it does not own, or freeing a `Box` whose future was polled without dropping it, is undefined behavior).

Safe code cannot violate these rules, because `poll` is unsafe and `.await` and the executors of `sai:async` uphold them. They are part of the `poll` contract of 11 §11.4.2.

### 12.6.6 Effect integrity

Unsafe code MUST NOT perform a primitive operation ([08](08-effects-and-capabilities.md) §8.3) that the effect set assigned to it by the effect analysis does not permit (N-I-28). This covers, in particular, a call through a function pointer whose effect set is smaller than the effects of the function it designates (§12.6.1 item 10), and a waker vtable function that performs effects ([11](11-async-and-concurrency.md) §11.4.3 requires them to perform none).

The effect analysis and the capability checks of 08 apply to unsafe code without change. They see every call that the program text contains. Effect integrity makes the soundness statement of [08](08-effects-and-capabilities.md) §8.15.5 hold for programs with unsafe code: the imports and the allocator that the artifact can reach are those that `Req` lists.

### 12.6.7 Contracts and determinism

The Standard Fact Checker and the runtime contract checks of [09](09-contracts-and-refinements.md) rely on three obligations of unsafe code:

1. N-C-58: unsafe code MUST NOT change the value of a path between its kill events except through a `borrow mut` created at a kill event, and a pure function MUST be deterministic in its arguments and the memory reachable from them.
2. N-C-44: a foreign function MUST establish its `ensures` clause when it returns. The clause is a trusted claim that is never checked (§12.7.3).
3. A value of a refined type that unsafe code produces MUST satisfy the predicate (09 §9.2.4, §12.6.2).

Violating any of them is undefined behavior (§12.6.1 items 2 and 9).

### 12.6.8 What is not undefined behavior

The following have defined behavior. Each is either a panic ([10](10-errors-and-panics.md)), a trap, or a result whose value is unspecified ([01](01-introduction.md) §1.5.3):

- arithmetic overflow, division by zero, failed bounds checks, failed contract and refinement checks, `unreachable()`, `panic`;
- out-of-memory, stack exhaustion, and any Wasm trap;
- leaking memory or other resources, including with `forget` and `Box.into_raw`;
- deadlock and non-termination;
- the numeric value of `addr()`, and comparisons of pointers into different allocations.

---

## 12.7 Core Wasm imports: `extern "wasm"`

### 12.7.1 Form

An `extern "wasm"` block declares functions that a core module imports from its host (D-UNSAFE-01). The grammar is [03](03-grammar.md) §3.10.

```sai
extern "wasm" module "env" {
    /// Writes one number to the host's log.
    fn log(v: I32)

    @import_name("now_ms")
    fn now() -> U64
}
```

1. The block MUST name the import module with `module "name"`. A block without it is `INVALID_EXTERN_BLOCK`. An `extern "C"` block without `unsafe` is `INVALID_EXTERN_BLOCK` too (§12.9.1). An ABI string other than `"wasm"` and `"C"` is `UNKNOWN_ABI` ([03](03-grammar.md)).
2. Every item is a function declaration without a body ([03](03-grammar.md) §3.5, §3.10).
3. Each function is a module-level item of the module that contains the block, with the canonical path `m.f` ([04](04-modules-and-names.md) §4.9.1). The block introduces no binding.
4. Extern items have no visibility marker and are package-private (D-MOD-02). A package that wants to offer an import to its dependents wraps it in a `pub` function. The imports of a package are therefore visible only through its own API, and its host requirements are visible in its effects (P5).

### 12.7.2 Calls and linear memory

Calling a function of an `extern "wasm"` block is safe (D-UNSAFE-01). It has the effect set `{ host }` (§12.7.7). The host is code that the borrow checker does not see, so a call is subject to the following rules (N-I-29):

1. **Reading.** During a call, the host MAY read any part of the module's linear memory. A module has at most one memory, and it is exported under the name `memory` whenever it exists, so that is the memory the host sees ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7, N-F-4). Addresses passed to and from imports are offsets in it.
2. **Writing.** During a call of a safe import, the host MUST NOT write to the module's linear memory. A write is undefined behavior (§12.6.1 item 12). Safe code may hold loans anywhere in memory (§12.6.4), and the host cannot know which.
3. **Unsafe imports.** An import that writes to linear memory MUST be declared `unsafe fn` inside the block. Calling it is an unsafe operation (§12.3.1 row 2), and its `# Safety` section (§12.2.2 item 4) states which memory the host writes. The caller MUST ensure that the written memory is not reachable through a live loan (§12.6.4), and that after the call it holds valid values (§12.6.2) of the types through which it is used.
4. **Re-entry.** During a call, the host MAY call the module's exports. This is defined behavior. SAI has no mutable static state (D-CAP-02), so an export can reach the caller's values only through addresses that it receives, and using an address is an unsafe operation (§12.4.3). The one exception is the panic hook (§12.7.5): the host MUST NOT call into the instance while the hook runs, because the frames of the panicking code are still live ([10](10-errors-and-panics.md) §10.6.2 item 5, N-C-82).
5. **Traps.** A host function that traps ends the instance, like every trap ([10](10-errors-and-panics.md)).

```sai
use sai:core/mem.{size_of}
use sai:core/ptr.{PtrMut}

extern "wasm" module "env" {
    /// Writes `len` random bytes to the memory at `addr`.
    ///
    /// # Safety
    ///
    /// `addr` must be the address of `len` writable bytes that no live borrow
    /// refers to, and every bit pattern must be valid for them.
    unsafe fn fill_random(addr: USize, len: USize)
}

fn random_word() -> U64
    effects { host }
{
    var word: U64 = 0
    let p = PtrMut.from(borrow mut word)
    // `word` has no loan, and every `U64` is valid.
    unsafe { fill_random(p.addr(), size_of<U64>()) }
    word
}
```

### 12.7.3 Contracts on imports

1. `requires` clauses on an import are allowed. They are obligations of the caller and are checked at call sites as usual ([09](09-contracts-and-refinements.md) §9.3.1).
2. An `ensures` clause on an import is a trusted claim about the host. It is never checked and is used as a fact at call sites (N-C-44). A false claim is undefined behavior (§12.6.7 item 2).
3. Because an `ensures` clause is a promise that the declaring code makes and nobody checks, an import with an `ensures` clause MUST be declared in an `unsafe extern "wasm"` block. In a block without `unsafe` it is `EXTERN_ENSURES_REQUIRES_UNSAFE` (N-I-29). The `unsafe` marker of the block makes the claim visible to `sai inspect unsafe` (§12.11). It does not make calls unsafe.
4. An `unsafe extern "wasm"` block that contains no import with an `ensures` clause is `UNUSED_UNSAFE` (§12.2.1), with a fix that removes `unsafe`. An `unsafe fn` item is unsafe for its callers and does not need the marker on the block.

```sai
unsafe extern "wasm" module "env" {
    /// Returns a random number below `bound`.
    fn random_below(bound: U32) -> U32
        requires bound > 0
        ensures result < bound
}
```

### 12.7.4 Signatures

A function of an `extern "wasm"` block has a restricted signature (D-UNSAFE-01, D-WASM-04; N-I-30):

1. Every parameter type and the result type MUST be a **boundary scalar**: an integer type, `F32`, `F64`, `Bool`, `Char`, `Byte`, or a distinct type without a refinement clause whose base type is a boundary scalar ([05](05-types.md)). The result type may also be `()`, written by omitting `->`. Any other type, including raw pointers, references, records, enums, refined types and `Never`, is `EXPORT_FN_NON_SCALAR`. The check applies to every function of an `extern "wasm"` block, in every build, because the restriction is part of the declaration (D-UNSAFE-01). Memory is passed as an address of type `USize`, obtained with `addr()` (§12.4.3).
2. The function MUST NOT be `async`, MUST NOT have generic parameters or a `self` parameter, and MUST NOT have a `where` or `borrows` clause. Each of these is `INVALID_EXTERN_SIGNATURE`.
3. An `effects` clause is optional. If written, it MUST be exactly `{ host }` (`EXTERN_EFFECTS_MISMATCH`, [08](08-effects-and-capabilities.md) §8.6.1).
4. The Wasm type of each boundary scalar, and the value checks that apply to values coming from the host, are those of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7. A result that is not a valid value of its declared type (a `Bool` other than 0 or 1, a `Char` that is not a scalar value, an `I8` that is not sign-extended) traps at the call, without a report (impl/06 N-F-12). The trust boundary of D-CONTRACT-01 therefore holds for imports without unsafe code: SAI code never observes an invalid value from a safe import.

### 12.7.5 `@panic_hook`

An import marked `@panic_hook` is the reporting channel of panics in a core-module build. Its additional rules (two `USize` parameters, root package only, at most one) are in [10](10-errors-and-panics.md) §10.6. The host reads the report from memory and does not write it, so the hook is a safe import.

### 12.7.6 Import names

1. The **import field name** of a function is its name, without a raw-identifier prefix `r#`, or the string of `@import_name` ([15](15-attributes-and-lints.md)). The **import module name** is the string after `module`. Any string is a valid Wasm name, so neither name has further restrictions (N-I-31).
2. Two declarations in the build, in one package or in different packages, that name the same pair of module name and field name are one import if their lowered signatures are equal. Otherwise the build fails with `CONFLICTING_WASM_IMPORT`. Lowered signatures and the merging of imports are those of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.3. Each declaration keeps its own result checks and contracts.
3. The function name is the name of the item in SAI. `@import_name` changes only the field name, so a host name that is not a SAI identifier can be imported under a snake-case name.

### 12.7.7 The `host` effect and capability

1. The effect of every function of an `extern "wasm"` block is `host` (D-EFF-03, [08](08-effects-and-capabilities.md) §8.6.1). Its callers carry `host` in their effect sets by the ordinary rules of 08, including through generic code.
2. A build that reaches a call of an import requires the target capability `host` ([08](08-effects-and-capabilities.md) §8.12.7). The policy MUST allow it ([package/01](../package/01-package-contract.md) §1.11). The `tiny` profile denies it by default ([package/01](../package/01-package-contract.md) §1.14.2).
3. `host` is provided only by core-module targets. In a component build, a reachable call of an import is `TARGET_CAPABILITY_UNAVAILABLE` ([13](13-components-and-wit.md) §13.6.4, N-D-46). A hybrid package keeps its imports behind `@target` ([15](15-attributes-and-lints.md)) or behind a capability trait.

---

## 12.8 Core Wasm exports: `export fn`

An `export fn` (D-MOD-02) of the root package is a core Wasm export when the artifact is a core module ([04](04-modules-and-names.md) §4.6.6, [13](13-components-and-wit.md) §13.6.2). In every other package, and in component builds, it is not exported ([04](04-modules-and-names.md) N-A-15, [13](13-components-and-wit.md) §13.6.4). The following rules apply (N-I-33):

1. **Scalar signatures.** The parameter types and the result type of an exported `export fn` MUST be boundary scalars, or `()` for the result (§12.7.4 item 1, D-WASM-04), exactly as for an `extern "wasm"` import. Any other type, including refined types, records, enums, views and raw pointers, is `EXPORT_FN_NON_SCALAR`. The check applies to exactly the `export fn` items that are exported in the build: those of the root package when the artifact is a core module ([04](04-modules-and-names.md) §4.6.6, N-A-15). In a component build, `export fn` items are not exported ([13](13-components-and-wit.md) §13.6.4), so their signatures are not restricted. The Wasm type of each boundary scalar is fixed by [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7. Rich types are exported through a component build ([13](13-components-and-wit.md)).
2. **Form.** An exported `export fn` MUST NOT have generic parameters, a `where` clause or a `borrows` clause (`INVALID_EXTERN_SIGNATURE`). An `export async fn` is `ASYNC_EXPORT_UNSUPPORTED` ([11](11-async-and-concurrency.md) §11.10.4).
3. **No unsafe exports.** An `export unsafe fn` is `UNSAFE_EXPORT`, in every package and every build. The host cannot read or keep a `# Safety` section, so an exported function MUST establish its own preconditions. This rule depends only on the declaration, so a package is accepted or rejected independently of whether it is the root.
4. **Export names.** The export name is the function name, without `r#`, or the string of `@export_name` ([15](15-attributes-and-lints.md)). `@export_name` is allowed only on an `export fn` (15). Two exports with the same name, and an export named `memory`, are `DUPLICATE_EXPORT_NAME` ([15](15-attributes-and-lints.md), [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.5). Any other string is a valid export name.
5. **Values from the host.** An argument that is not a valid value of its parameter type traps before the body runs, without a report ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7, N-F-12). `requires` clauses and refinements of exported functions are validated as trust-boundary input ([09](09-contracts-and-refinements.md) §9.7.3).
6. **Effects.** An `export fn` is a root of reachability, and it MUST declare its effects if its inferred set is not empty ([08](08-effects-and-capabilities.md) §8.14.1).

```sai
/// Returns the area of a rectangle.
@export_name("area")
export fn rect_area(width: U32, height: U32) -> U64 {
    U64.from(width) * U64.from(height)
}
```

```sai
export fn count(names: Slice<Str>) -> USize {    // rejected: EXPORT_FN_NON_SCALAR
    names.len()
}

export unsafe fn reset(addr: USize) {           // rejected: UNSAFE_EXPORT
}
```

---

## 12.9 Native foreign functions: `unsafe extern "C"` (Stage 7)

### 12.9.1 Form

An `unsafe extern "C"` block declares functions with the C calling convention of the target, defined in native code (D-UNSAFE-01). The rules are (N-I-34):

1. The block MUST be marked `unsafe` ([03](03-grammar.md) §3.10). Without the marker it is `INVALID_EXTERN_BLOCK`. The marker states that the declaring code vouches for the declared signatures and contracts, which no tool can check.
2. `module "name"` is optional. If present, it names the native library that defines the functions. How a library name is found at link time is defined per target in [targets](../targets/targets-and-profiles.md). Without it, the functions are resolved against the libraries that the target links by default.
3. The **symbol name** of a function is its name, without `r#`, or the string of `@import_name` ([15](15-attributes-and-lints.md)). It MUST be a C identifier: an ASCII letter or `_`, followed by ASCII letters, digits and `_`. Any other string is `INVALID_LINK_NAME`.
4. Every item is a function declaration without a body. Its functions are package-private module-level items, as in §12.7.1 items 3 and 4.
5. Calling any function of the block is an unsafe operation (§12.3.1 row 4). An `unsafe fn` marker on an item is redundant and is `UNUSED_UNSAFE`, with a fix that removes it. `@panic_hook` is not allowed on these items ([10](10-errors-and-panics.md) §10.6).
6. `requires` clauses are checked at call sites, and `ensures` clauses are trusted claims (§12.7.3 items 1 and 2).

### 12.9.2 Effect, capability and portability

1. A call of a function of the block has the effect `ffi` (D-EFF-03, [08](08-effects-and-capabilities.md) §8.3). An `effects` clause on such a function is optional. If written, it MUST be exactly `{ ffi }` (`EXTERN_EFFECTS_MISMATCH`, [08](08-effects-and-capabilities.md) §8.6.1).
2. A build that reaches a call requires the target capability `native_ffi`, which the policy MUST allow ([package/01](../package/01-package-contract.md) §1.11). Only native targets provide it ([08](08-effects-and-capabilities.md) §8.12.7). On every Wasm target a reachable call is `TARGET_CAPABILITY_UNAVAILABLE`.
3. A package whose source contains an `unsafe extern "C"` block, including one removed by `@target` for the current build, is **non-portable**. `sai inspect unsafe` reports the mark for the package (§12.11). The mark records a fact and does not change acceptance: a non-portable package builds for every target on which none of its foreign calls is reachable.
4. Before Stage 7, every `unsafe extern "C"` block is `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md)).

### 12.9.3 FFI types

The parameter and result types of a function of an `unsafe extern "C"` block MUST be **FFI types**. Any other type is `INVALID_EXTERN_SIGNATURE`. The FFI types are:

| Type | C type |
|---|---|
| the integer types, `Byte`, and the C aliases of `sai:core/ffi` (§12.9.4) | the integer type of the same width and signedness |
| `F32`, `F64` | `float`, `double` |
| `Bool` | `_Bool` |
| `Ptr<T>`, `PtrMut<T>`, for any `T` | `const T*`, `T*` |
| `Option<Ptr<T>>`, `Option<PtrMut<T>>` | a pointer that may be null; `None` is null |
| `CStr` | `const char*`, pointing to a NUL-terminated string |
| `Out<T>`, for an FFI type `T` | `T*`, written by the callee (§12.9.5) |
| a record marked `@repr(C)` whose field types are FFI types ([15](15-attributes-and-lints.md) §15.9) | the C `struct` with the same fields in the same order |
| an enum without payloads marked `@repr(I)` for an integer type `I` | `I`; the value is the discriminant ([05](05-types.md) §5.6.2) |
| a distinct type without a refinement clause whose base type is an FFI type | as its base type |
| `()` and `Never`, as the result only | `void`; for `Never`, a function that does not return |

1. Everything else is rejected, in particular `borrow T`, `borrow mut T`, `Str`, `Slice<T>`, `SliceMut<T>`, `String`, `Vec<T>`, `Box<T>`, tuples, arrays, enums with payloads, records without `@repr(C)`, refined types, function pointers and closures. v0.1 has no callbacks from C into SAI and no variadic functions.
2. The function MUST NOT be `async`, and MUST NOT have generic parameters, a `self` parameter, a `where` clause or a `borrows` clause (`INVALID_EXTERN_SIGNATURE`).
3. The pointee type `T` of a raw pointer is not restricted. If `T` is not an FFI type, its layout is compiler-private (D-WASM-03), and the foreign code MUST NOT access the pointee: it may only store the pointer and pass it back.
4. Values that the foreign code returns, or writes through `Out<T>` and through pointers, MUST be valid values of their SAI types (§12.6.2). There is no value check at this boundary: a `Bool` other than 0 or 1, or an enum value that is not a discriminant, is undefined behavior. A function with result `Never` that returns is undefined behavior.

### 12.9.4 C types

The C integer aliases (`CChar`, `CSChar`, `CUChar`, `CShort`, `CUShort`, `CInt`, `CUInt`, `CLong`, `CULong`, `CLongLong`, `CULongLong`, `CSize`, `CSSize`, `CFloat`, `CDouble`), `CStr` and `Out<T>` are declared in `sai:core/ffi`, and `CString` in `sai:alloc/ffi` ([16](16-core-library.md) §16.12.4). The aliases are transparent ([05](05-types.md) §5.6.5), so a value of `CInt` is an `I32` wherever `CInt` is `I32`. Code that uses the aliases where a C header uses the C types is portable across targets with different C ABIs.

`CStr` is a borrowed view with one region ([07](07-ownership-and-borrowing.md)). Passing it to a foreign function lends the bytes for the duration of the call only. A foreign function that keeps the pointer after it returns MUST be given memory that outlives that use, for example the buffer of a `CString` released with `into_raw`.

### 12.9.5 Out-parameters and opaque types

`Out<T>` ([16](16-core-library.md) §16.12.4) passes storage that the foreign function initializes. It is created from a `borrow mut MaybeUninit<T>` (§12.5.3), so the borrow checker sees the loan for the duration of the call. After the call, the caller reads the value with `assume_init`, which is unsafe because only the foreign function's documentation says that it wrote one.

A C type whose fields SAI never accesses (an **opaque type**) is represented by a record that SAI code never constructs and never dereferences, conventionally a record without fields. Pointers to it are passed back and forth unchanged (§12.9.3 item 3).

```sai
use sai:core/ffi.{CInt, CStr, Out, Ptr}
use sai:core/mem.{MaybeUninit}

/// An `sqlite3` connection handle. Its fields are private to the C library.
type Sqlite {}

unsafe extern "C" module "sqlite3" {
    fn sqlite3_open(filename: CStr, db: Out<Ptr<Sqlite>>) -> CInt
}

fn open(path: CStr) -> Option<Ptr<Sqlite>>
    effects { ffi }
{
    var slot = MaybeUninit<Ptr<Sqlite>>.uninit()
    let rc = unsafe { sqlite3_open(path, Out.from(borrow mut slot)) }
    if rc != 0 {
        return None
    }
    // `sqlite3_open` writes the handle when it returns 0.
    Some(unsafe { slot.assume_init() })
}
```

### 12.9.6 Threads, unwinding and foreign memory

Foreign code is unsafe code that the compiler does not see. In addition to the rules of §12.6, it MUST respect the following. A violation is undefined behavior (§12.6.1 item 13).

1. **No unwinding.** A foreign function MUST return normally or not at all. It MUST NOT unwind through SAI frames, for example by a C++ exception or a `longjmp` to a point outside the call. SAI frames have no unwind tables, and panics never unwind (D-ERR-01).
2. **One thread.** SAI code runs on one thread (D-THREAD-01). A thread created by foreign code MUST NOT access memory that SAI code owns or borrows, and MUST NOT call SAI functions. The guarantees of [11](11-async-and-concurrency.md) §11.12 assume this.
3. **Memory.** The foreign code accesses memory that it receives through pointers under the same rules as SAI unsafe code: alignment and bounds (§12.4.4), validity (§12.6.2) and aliasing (§12.6.4). In particular it MUST NOT write through a `Ptr<T>` or a `CStr`, and MUST NOT keep a pointer after the call unless the memory outlives that use.
4. **Allocators.** Memory allocated by the foreign code is freed by the foreign code, and memory of the SAI global allocator only by SAI ([16](16-core-library.md) §16.15.1). Freeing memory with the other allocator is item 5 of §12.6.1.

---

## 12.10 Unsafe code and effects

1. **Unsafety is not an effect** ([08](08-effects-and-capabilities.md) §8.1). An unsafe block, an unsafe function and an unsafe operation contribute nothing to an effect set. Only the callees they reach do: `host` for `extern "wasm"`, `ffi` for `extern "C"`, `alloc` for the allocation primitives, and the declared effects of every other function.
2. **Effects are checked in unsafe code.** The effect analysis, the `effects` clauses and the capability checks of 08 apply to unsafe code without change. Unsafe code cannot perform an effect that its function does not declare or infer (§12.6.6).
3. **No escape hatch.** There is no way to hide an effect from the analysis, for example by transmuting a function pointer to a type with fewer effects. Calling it is undefined behavior (§12.6.1 items 8 and 10). Every primitive operation therefore appears in the effect set of some function, and the capabilities that a build requires are complete (08 §8.15.5).
4. **Drop glue** has the effect set `{}` in unsafe code too ([07](07-ownership-and-borrowing.md) §7.13, [08](08-effects-and-capabilities.md) N-D-4). `drop_in_place` and `assume_init` add no effects.

---

## 12.11 Auditing unsafe code

### 12.11.1 `sai inspect unsafe`

`sai inspect unsafe` (D-TOOL-03) lists every use of unsafe code in a package and in its whole dependency graph (D-UNSAFE-01). Its content is normative (N-I-35). The command line and the output format are in [impl/13](../impl/13-cli.md) and [impl/12](../impl/12-semantic-tooling.md).

1. **Entries.** One entry per occurrence of each of the following, in the source of every package of the resolved graph, including `sai:core` and `sai:alloc`:
   - an `unsafe` block;
   - an `unsafe fn` declaration, including the `unsafe fn` items of `extern "wasm"` blocks;
   - an `unsafe trait` declaration and an `unsafe impl`;
   - an `unsafe module` header (one entry for the file, §12.2.4);
   - an `unsafe extern "wasm"` block and an `unsafe extern "C"` block.
2. **Fields.** Each entry has its kind, the canonical path of the enclosing item ([04](04-modules-and-names.md) §4.9), its symbol ID and node ID (D-TOOL-02, [impl/12](../impl/12-semantic-tooling.md)), and its source location. An entry that is an unsafe context also lists the unsafe operations it contains, each with its row of §12.3.1 and, for calls, the canonical path of the callee. The inventory is the same for every command and is computed without test analysis ([14](14-testing.md) §14.2.4), so an entry inside a test or property is listed with the test path as its enclosing item and without operations; the operations of test code are checked, and reported, by `sai test` ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.10.4 item 8, N-D-97). The `# Safety` section of an unsafe function or trait is included as text: its lines joined with LF, without the blank lines at its start and end, or nothing if the function or trait has no such section (impl/08 §8.10.4 item 5).
3. **Packages.** Entries are grouped by package. Each package is reported with the number of entries of each kind and the mark **non-portable** if §12.9.2 item 3 applies.
4. **Targets.** Declarations are listed whatever their `@target` predicates, together with those predicates. Unsafe operations depend on name resolution, so they are listed for each target that the command is run for, after `@target` filtering ([15](15-attributes-and-lints.md) §15.11).
5. **Order.** Entries are ordered by package identity, then by canonical path, then by source position, so the output is deterministic ([01](01-introduction.md) §1.6).

### 12.11.2 Semantic diff

The semantic diff of two versions of a package ([impl/12](../impl/12-semantic-tooling.md)) reports every entry of §12.11.1 that was added or removed, and every change of the `# Safety` section of a public unsafe function or trait. The diff also reviews dependencies: for every dependency whose lock entry changes its version, source or content, and for every new dependency, it compares the entries of §12.11.1 of the dependency's two versions, and the `# Safety` sections of its unsafe functions and traits, and reports every addition, removal and change ([impl/12](../impl/12-semantic-tooling.md) §12.8.7, N-I-51). A new unsafe entry in a dependency is therefore visible in review, even when the public API of the dependency is unchanged (P3).

### 12.11.3 Policy

`policy { deny unsafe }` in `sai.package` makes every construct of the package's own source that §12.11.1 item 1 lists, except `unsafe trait` declarations, the error `UNSAFE_DENIED` ([package/01](../package/01-package-contract.md) §1.12). Declaring an unsafe trait states an obligation and is not unsafe code; implementing one is. The policy does not constrain dependencies; `sai inspect unsafe` is the graph-wide view.

---

## 12.12 Decisions introduced by this chapter

These entries are provisional until they are merged into [decisions.md](../design/decisions.md). IDs are shared with [11](11-async-and-concurrency.md) and [14](14-testing.md).

| ID | Rule | Rationale | Section |
|---|---|---|---|
| N-I-21 | An `unsafe` block makes every expression lexically inside it an unsafe context, except the bodies of nested closures and `async` blocks. A block with no unsafe operation, or one already in an unsafe context, is `UNUSED_UNSAFE`. | P1: the unsafe region is visible locally and never leaks into code that runs later. P8: a fixable warning keeps blocks minimal. | §12.2.1 |
| N-I-22 | The body of an `unsafe fn` is an unsafe context. A `pub` unsafe function or trait without a `# Safety` section is `MISSING_SAFETY_DOC`. Unsafe functions are never interface functions or core exports. Violating a stated precondition is undefined behavior whether or not the body relies on it. | P1: the obligations are explicit where callers read them, and a precondition means the same for every implementation of the function. | §12.2.2 |
| N-I-23 | Every impl of an `unsafe trait` is `unsafe impl`, and `unsafe impl` is only for unsafe traits. Any mismatch, and `unsafe impl` on an inherent or interface impl, is `IMPL_UNSAFETY_MISMATCH`. | P1: the promise is explicit at each impl. P9: one way to write it. | §12.2.3 |
| N-I-24 | In an `unsafe module`, every function, closure and `async` block body is an unsafe context; predicates never are. Unsafe blocks inside are `UNUSED_UNSAFE`, and so is a header with no unsafe operation in the file. | P9: one marker for a file of low-level code instead of a block around every operation. P8: an unused header is reported. | §12.2.4 |
| N-I-25 | `Ptr<T>` and `PtrMut<T>` are non-null addresses with no region, `Copy`, and compared and hashed by address. Creating one from a place lowers to `RawPtr` and creates no loan. There is no pointer provenance: validity depends only on the address and the rules of §12.6. | P8: validity depends only on addresses, which unsafe code can check by hand. P7: no provenance model. P1: only dereference is unsafe. | §12.4 |
| N-I-26 | Every type has a validity invariant, listed per type. Uninitialized bytes are invalid for every type except `MaybeUninit<T>`, including integers. | P2: niches and SFC facts rely on validity. P1: one list instead of case-by-case folklore. | §12.6.2 |
| N-I-27 | Accesses through raw pointers obey the loans that 07 computes for the whole program: no write while a shared loan of the place is live, no access while another exclusive loan is live, except through pointers derived from that loan. String and array literals are under a permanent shared loan. There is no interior mutability, except that the methods of `Arena` write its fields under the shared loan of a `region` block, which no safe code can observe. | P1: unsafe code is held to the same loans as safe code. P9: one aliasing model, not two. The arena exception keeps `region` blocks free of allocation until the first `alloc` (P7) without a general cell type. | §12.6.4 |
| N-I-28 | Unsafe code MUST NOT perform a primitive operation that its effect set does not permit. | P5: the capability requirements of an artifact stay complete with unsafe code (08 §8.15.5). | §12.6.6 |
| N-I-29 | The host may read linear memory during an `extern "wasm"` call; it may write it only during a call of an import declared `unsafe fn`. Re-entry into exports is allowed. An import with `ensures` must be in an `unsafe extern "wasm"` block (`EXTERN_ENSURES_REQUIRES_UNSAFE`). | P1: host writes and trusted claims are explicit at the declaration and listed by `sai inspect unsafe`. P8: safe code keeps the guarantees of the borrow checker across imports, while hosts that only read buffers (logging, `@panic_hook`) need no unsafe code. | §12.7.2, §12.7.3 |
| N-I-30 | `extern "wasm"` signatures use boundary scalars only (integers, floats, `Bool`, `Char`, `Byte`, unrefined distinct types over them, and `()` as the result; otherwise `EXPORT_FN_NON_SCALAR`). Generic, `async`, `self`, `where` and `borrows` forms are `INVALID_EXTERN_SIGNATURE`. Invalid values from the host trap. | P4: the boundary maps one-to-one to Wasm value types. D-CONTRACT-01: trust-boundary failures trap. | §12.7.4 |
| N-I-31 | Import field names are the function name without `r#` or the `@import_name` string, with no restriction. Equal (module, field) pairs with equal lowered signatures are one import. | P4: any Wasm name is reachable. P8: merging is deterministic (impl/06 §6.10.3). | §12.7.6 |
| N-I-32 | `transmute` requires equal sizes known on the generic signature (`TRANSMUTE_SIZE_MISMATCH`), consumes its argument without drop glue, and needs a valid result. `zeroed` is unsafe and valid only for the listed types. `MaybeUninit<T>` has no validity requirement and no drop glue. | P8: the size check never depends on instantiation order. P1: every reinterpretation is an explicit unsafe operation. | §12.5.2, §12.5.3 |
| N-I-33 | An exported `export fn` has boundary-scalar parameters and result, like an `extern "wasm"` import (`EXPORT_FN_NON_SCALAR`), and no generic, `where` or `borrows` form. `export unsafe fn` is `UNSAFE_EXPORT` in every build. Export names are the function name without `r#` or `@export_name`; `memory` is reserved. | P1: an export establishes its own preconditions, because the host cannot read a `# Safety` section. P8: acceptance of the unsafe rule does not depend on which package is the root. | §12.8 |
| N-I-34 | `extern "C"` blocks are `unsafe`, may name a library with `module`, use C-identifier symbols (`INVALID_LINK_NAME`) and FFI types only (`INVALID_EXTERN_SIGNATURE`), have the effect `ffi`, need `native_ffi`, and mark the package non-portable. Foreign code never unwinds and never touches SAI memory from another thread. | P4: native interop is explicit and does not leak into Wasm builds. P5: the capability shows in the effect set. P7: no callbacks or variadics in v0.1. | §12.9 |
| N-I-35 | `sai inspect unsafe` lists every unsafe block, unsafe fn, unsafe trait and impl, unsafe module and unsafe extern block of the whole graph, with canonical path, symbol and node IDs, location, contained operations and `# Safety` text, in a deterministic order. The semantic diff reports added and removed entries. | P3: entries are identified by symbol and node IDs and compared by the semantic diff, so unsafe code is auditable across dependencies. P8: deterministic output. | §12.11 |
| N-I-36 | SAI has no untagged unions. Reinterpretation uses `transmute` or raw pointers, and possibly invalid storage uses `MaybeUninit<T>`. | P7: one fewer type form. P2: every other aggregate keeps a validity invariant. | §12.3.3 |
| N-I-51 | The semantic diff compares the unsafe inventory of every dependency whose lock entry changes version, source or checksum, and of every new dependency, against its previous version, and reports added and removed entries and changed `# Safety` sections. A dependency used in artifact form is reported without entries. | P3: new unsafe code in a dependency must reach review without reading its source. P9: the comparison reuses the inventory of `sai inspect unsafe`. P8: entries are matched by kind, canonical path and rank, so the result does not depend on edits or symbol ID allocation. | §12.11.2 |

---

## 12.13 Diagnostics

Codes defined in another document are marked with that document. The warnings of this chapter are lints; their default levels are in [15](15-attributes-and-lints.md) §15.17.2.

| Code | Severity | Description |
|---|---|---|
| `ASSIGN_TO_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4; an assignment through a `Ptr<T>` dereference (§12.4.4) |
| `ASYNC_EXPORT_UNSUPPORTED` | error | defined in [11](11-async-and-concurrency.md) §11.10.4; an `export async fn` (§12.8) |
| `BORROW_MUT_OF_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4; `borrow mut` of a `Ptr<T>` dereference (§12.4.4) |
| `CONFLICTING_WASM_IMPORT` | error | two imports with the same module and field name and different lowered signatures, as computed by [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.3 (§12.7.6) |
| `DUPLICATE_EXPORT_NAME` | error | defined in [15](15-attributes-and-lints.md) §15.7; two exports with one name, or an export named `memory` (§12.8) |
| `EXPORT_FN_NON_SCALAR` | error | a parameter or result type of an `extern "wasm"` function, or of an `export fn` exported from a core module, that is not a boundary scalar (§12.7.4, §12.8) |
| `EXTERN_EFFECTS_MISMATCH` | error | defined in [08](08-effects-and-capabilities.md) §8.6.1; an `effects` clause other than `{ host }` on an `extern "wasm"` function, or other than `{ ffi }` on an `extern "C"` function (§12.7.4, §12.9.2) |
| `EXTERN_ENSURES_REQUIRES_UNSAFE` | error | an `extern "wasm"` function with an `ensures` clause in a block without `unsafe` (§12.7.3) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; a feature of this chapter used before its stage (§12.1.4, §12.9.2) |
| `IMPL_UNSAFETY_MISMATCH` | error | an impl of an unsafe trait without `unsafe`, or `unsafe impl` of a safe trait, an inherent impl or an interface impl (§12.2.3) |
| `INTERFACE_FN_CLAUSE` | error | defined in [13](13-components-and-wit.md) §13.2.2; an `unsafe fn` as an interface function (§12.2.2) |
| `INVALID_CONTRACT_EXPRESSION` | error | defined in [09](09-contracts-and-refinements.md) §9.1.2; an `unsafe` block in a predicate (§12.2.4) |
| `INVALID_EXTERN_BLOCK` | error | an `extern "wasm"` block without `module`, or an `extern "C"` block without `unsafe` (§12.7.1, §12.9.1) |
| `INVALID_EXTERN_SIGNATURE` | error | an extern function or an exported `export fn` that is `async` (extern only), generic, has a `self` parameter, or has a `where` or `borrows` clause; a type of an `extern "C"` function that is not an FFI type (§12.7.4, §12.8, §12.9.3) |
| `INVALID_LINK_NAME` | error | the symbol name of an `extern "C"` function is not a C identifier (§12.9.1) |
| `MISSING_SAFETY_DOC` | warning (lint) | a `pub` unsafe function, method, trait method or trait without a `# Safety` doc section (§12.2.2, §12.2.3) |
| `MOVE_OUT_OF_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.17; moving a non-`Copy` value out of a raw pointer dereference (§12.4.4) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; a reachable `extern "wasm"` call in a component build, or a reachable `extern "C"` call on a Wasm target (§12.7.7, §12.9.2) |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | defined in [05](05-types.md) §5.9.1; the `unsafe` markers of a trait method and its implementation differ (§12.2.2) |
| `TRANSMUTE_SIZE_MISMATCH` | error | the sizes of the source and target types of `transmute` differ or depend on a type parameter (§12.5.2) |
| `UNKNOWN_ABI` | error | defined in [03](03-grammar.md) §3.10; an ABI string other than `"wasm"` and `"C"` (§12.7.1) |
| `UNSAFE_DENIED` | error | defined in [package/01](../package/01-package-contract.md) §1.12; an unsafe construct in a package with `policy { deny unsafe }` (§12.11.3) |
| `UNSAFE_EXPORT` | error | an `export unsafe fn` (§12.2.2, §12.8) |
| `UNSAFE_OPERATION_OUTSIDE_UNSAFE` | error | an unsafe operation outside an unsafe context (§12.2.5, §12.3) |
| `UNUSED_UNSAFE` | warning (lint) | an `unsafe` block, `unsafe module` header or `unsafe extern "wasm"` marker with nothing to cover, an `unsafe` block already in an unsafe context, or an `unsafe fn` marker on an `extern "C"` item (§12.2.1, §12.2.4, §12.7.3, §12.9.1) |
