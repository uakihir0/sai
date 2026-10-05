# 16. Core Library

**Status:** Normative
**Stage:** 2 (default for the items of this chapter; items of Stages 1, 3, 5 and 7 are marked, §16.1.6)
**Related:** [decisions.md](../design/decisions.md) D-TYPE-01, D-TYPE-02, D-TYPE-03, D-TYPE-06, D-TYPE-08, D-TYPE-09, D-OWN-01, D-OWN-05, D-OWN-07, D-EXPR-01, D-EXPR-05, D-EXPR-06, D-EXPR-09, D-EXPR-10, D-EXPR-12, D-EXPR-13, D-ASYNC-01, D-UNSAFE-01, D-CAP-02, D-WASM-02, D-WASM-05, D-PKG-04; [draft-reconciliation.md](../design/draft-reconciliation.md) R-04, R-12, C-02; [01](01-introduction.md), [04](04-modules-and-names.md), [05](05-types.md), [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [09](09-contracts-and-refinements.md), [10](10-errors-and-panics.md), [11](11-async-and-concurrency.md), [12](12-unsafe-and-ffi.md), [13](13-components-and-wit.md), [14](14-testing.md), [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/10](../impl/10-runtime.md)

This chapter defines the two implicit packages `sai:core` and `sai:alloc`: their module trees, the exact contents of the prelude, every lang item together with the language construct that uses it, and the signature, effects, panics, contracts and stage of every public item. It also fixes the few signatures of other standard packages that the language specification relies on (§16.16). The mechanism of implicit packages and of the prelude belongs to [04](04-modules-and-names.md) §4.2.6. Types, traits, coherence and `@derive` belong to [05](05-types.md), expressions and their desugarings to [06](06-expressions-and-statements.md), ownership and the arena rules to [07](07-ownership-and-borrowing.md), effects to [08](08-effects-and-capabilities.md), contracts to [09](09-contracts-and-refinements.md), panics to [10](10-errors-and-panics.md), async semantics to [11](11-async-and-concurrency.md), unsafe code and FFI to [12](12-unsafe-and-ffi.md), and property-test semantics to [14](14-testing.md). This chapter defines library surfaces and their behavior. It does not restate those rules.

---

## 16.1 Packages and conventions

### 16.1.1 The two implicit packages

1. `sai:core` is the dependency-free foundation. It MUST NOT import anything: no allocator, no operating system, no WASI interface, no libc, no `extern "wasm"` function. Every item of `sai:core` is usable on every target and in every profile, including `tiny` ([targets](../targets/targets-and-profiles.md)). No item of `sai:core` has a non-empty declared effect set.
2. `sai:alloc` depends on `sai:core` only. It contains the owned types (`String`, `Vec<T>`, `Box<T>`, `Bytes`), arenas, and every item that reaches the global allocator. The allocation primitives of §16.15.1 are the only functions through which any SAI code reaches the global allocator (08 §8.3.2). Whether a program may use `alloc` is decided by the `alloc` effect and the profile, never by name resolution (04 §4.2.6).
3. Both packages are implicit dependencies of every package and are never declared in `sai.package` (D-PKG-04). Their version is the version of the toolchain that ships them. They are compiled from the library source distributed with the compiler ([impl/01](../impl/01-architecture.md) §1.12).
4. For coherence, `sai:core` and `sai:alloc` count as one package (D-TYPE-08, 05 §5.10.1). The same holds for inherent impls (N-J-6): `sai:core` and `sai:alloc` may declare inherent impls on the primitive types, on `Array<T, N>`, and on every lang-item type of either package. Name uniqueness of inherent items (05 §5.9.2 rule 4) is checked across both packages together. Every other package is still bound by `INHERENT_IMPL_FOREIGN_TYPE` for these types (05).
5. The root modules `sai:core` and `sai:alloc` declare no items and re-export nothing. Every item lives in a named module (§16.2). In prose, `core.m.x` and `alloc.m.x` denote the item `x` of the module `sai:core/m` or `sai:alloc/m` (01 §1.9). In source, the module is imported with `use sai:core/m` and the item is written `m.x`, or the item is imported directly (`use sai:core/mem.{take}`). A submodule is never reached through its parent (04, N-A-3).

### 16.1.2 Reading the listings

1. **Trait declarations** are given in `sai` blocks. They are complete, valid declarations as they appear in the library source, including the bodies of provided methods. The one exception is `Iterator`, whose many provided methods are specified by a table (§16.11.2); the `sai` block shows its required items.
2. **Type declarations** whose fields are public are given in `sai` blocks. The fields of lang-item types such as `Str`, `Vec<T>` or `Ptr<T>` are private to the library. Their layout belongs to [impl/06](../impl/06-wasm-lowering-and-layout.md) and is not part of this chapter.
3. **Inherent functions** are listed in tables. The *Signature* column shows the `FnHead` exactly as declared, without the `pub` marker and without the body. Every listed item is `pub`. Parameter names are normative only for documentation; calls are positional (06).
4. The *Effects* column shows the declared effect set. `—` means that the function declares no effect and its signature set is empty (`Sig(f) = ∅`, 08 §8.6.2). `alloc` means `effects { alloc }`. Effects that are latent in type parameters (closures, `Iterator` arguments, trait-bounded parameters) are attributed to the caller by 08 §8.8 and are never repeated in the listings.
5. The *Panics* column names the panic kind (D-MIR-01 `AssertKind`) and the condition. `—` means that the function never panics, apart from out-of-memory for functions with the `alloc` effect (§16.15.1).
6. A `requires` or `ensures` clause shown in a signature is part of the declaration. It is checked or proved as [09](09-contracts-and-refinements.md) defines, and a failing check panics with kind `Contract(id)` at the call expression (09).
7. **Schemas.** Methods that exist for many primitive types are listed once for a placeholder type, with a substitution rule stated next to the table (for example `X` for every integer type in §16.8.2).
8. **Safety.** For an `unsafe fn`, a *Safety* paragraph states the precondition the caller must establish. Violating it is undefined behavior ([12](12-unsafe-and-ffi.md)).

### 16.1.3 Effects of library items (N-J-7)

1. Every `pub` function of `sai:core` and `sai:alloc` whose signature set is non-empty declares its effect set explicitly. The listings in this chapter are those declarations. An implementation MUST NOT give a library function a larger or a smaller declared set than the one listed.
2. Trait methods follow 08 §8.6.3. A trait method of this chapter with an `effects` clause bounds every impl, and a trait method without a clause has latent effects. Which traits carry a bound is decided by N-J-12 (§16.5.1).
3. Deallocation is not an effect (D-OWN-05). Dropping an owned value, `deallocate` and `Drop.drop` have no effect.

### 16.1.4 Panics raised by library functions (N-J-8)

1. A panic is raised in a library function either by a contract check of a `requires` clause, or by the function body (for example `Option.unwrap` on `None`).
2. The **location** of a panic raised by a library body is the call expression, in the innermost frame outside `sai:core` and `sai:alloc`, through which control entered the library. If a user function calls `Option.unwrap`, the location is that call. If a library function calls a user closure that panics, the panic belongs to the closure and has the closure's own location. The location of a failing `requires` check is the call expression (09).
3. The panic message of each library panic is fixed by the *Panics* column or by the item's prose. The rendering of panic messages, and what happens after a panic, belong to [10](10-errors-and-panics.md).
4. Panics are not effects (D-EFF-03). A function whose *Panics* column is not `—` may still have an empty effect set.

### 16.1.5 Determinism

Every function of `sai:core` and `sai:alloc` is a deterministic function of its arguments and of the memory they reach. The only exceptions are items 1 to 3 of the unspecified behavior of 01 §1.5.3: the bit pattern of a NaN produced by a float operation, the numeric value of addresses (`Ptr.addr`, `PtrMut.addr`), and memory contents read through unsafe code. No item reads clocks, randomness, the environment or the host. Hashing is deterministic (§16.5.6).

### 16.1.6 Stages (N-J-9)

The stage of a library item is the stage at which a compiler that claims the stage MUST provide it (01 §1.4.5). Using an item of a later stage is `FEATURE_NOT_IMPLEMENTED` with the item's canonical path (04 §4.9) as the feature name. A construct that is defined in terms of a lang item (§16.3) uses that item, although it refers to it by identity (§16.3.1 rule 3). For example, a `for` loop before Stage 2 is `FEATURE_NOT_IMPLEMENTED` with the feature `sai:core/iter.IntoIterator`.

| Stage | Items |
|---|---|
| 1 | the primitive types and `Never` (built in, 05 §5.3); `Str` and `Array` as types with their built-in operations (06); `Option`, `Some`, `None`, `Result`, `Ok`, `Err` as types and variants; `panic` and `unreachable`; the integer and float constants of §16.8.1; the non-generic inherent methods of the integer, float, `Bool`, `Byte` and `Char` types (§16.8, §16.9.1), except those that take or return a generic type other than `Option` and `Result`; `Str.len` and `Str.is_empty` |
| 2 | every other item of this chapter unless the table says otherwise, including `Slice`, `SliceMut`, `String`, `Vec`, `Box`, `Bytes`, the traits, the iterators, `core.mem`, `core.ptr`, `core.fmt`, `alloc.arena`, `Future`, `Stream` and `core.task` (11 §11.1.2) |
| 3 | `core.testing` (§16.17); the format marker traits and the specifier checks (§16.14.3, §16.14.4); f-string syntax, which is Stage 3 by D-EXPR-12 although `Format` and `Writer` are Stage 2 |
| 4 | the items of `sai:process`, `sai:fs`, `sai:net` and `sai:http` fixed in §16.16.3 to §16.16.5, except the `async fn` items of `sai:http` |
| 5 | the package `sai:async` (§16.16.2) and the `async fn` items of `sai:http` (§16.16.5) |
| 7 | the C interop items of `core.ffi` and `alloc.ffi`: the C type aliases, `CStr`, `Out<T>` and `CString` (§16.12.4) |

## 16.2 Module list

The primitive types (`I8` … `USize`, `F32`, `F64`, `Bool`, `Byte`, `Char`, `()`, `Never`), tuples, `Array<T, N>`, `borrow T` and `borrow mut T` are built into the language (05). They are not items of any module. Their methods and trait impls are declared in the modules below.

### 16.2.1 Modules of `sai:core`

| Module | Contents | Section |
|---|---|---|
| `sai:core/marker` | `Copy` | §16.5.2 |
| `sai:core/clone` | `Clone` | §16.5.2 |
| `sai:core/default` | `Default` | §16.5.3 |
| `sai:core/cmp` | `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Ordering`, `min`, `max` | §16.5.4 |
| `sai:core/hash` | `Hash`, `Hasher`, `SipHasher13` | §16.5.6 |
| `sai:core/convert` | `From`, `TryFrom` | §16.5.7 |
| `sai:core/ops` | operator traits, `Index`, `IndexMut`, `Fn`, `FnMut`, `FnOnce`, `Drop`, the six range types, `Step`, `SliceRange` | §16.6 |
| `sai:core/option` | `Option` and its methods | §16.7.1 |
| `sai:core/result` | `Result` and its methods | §16.7.2 |
| `sai:core/panic` | `panic`, `unreachable` | §16.7.3 |
| `sai:core/num` | `Integer`, `Float`, `ParseIntError`, `ParseFloatError`, numeric constants, integer and float methods | §16.8 |
| `sai:core/char` | `Bool`, `Byte` and `Char` methods, `CHAR_MAX`, `REPLACEMENT_CHARACTER`, `ParseCharError` | §16.9.1 |
| `sai:core/str` | `Str` and its methods, `Utf8Error`, the `Str` iterators | §16.9.2 |
| `sai:core/slice` | `Slice`, `SliceMut` and their methods, the slice iterators | §16.10.1 |
| `sai:core/array` | `Array` methods, `ArrayIntoIter` | §16.10.2 |
| `sai:core/iter` | `IntoIterator`, `Iterator`, `DoubleEndedIterator`, `ExactSizeIterator`, `FromIterator`, `Sum`, `Product`, the adapter types, `empty`, `once`, `repeat`, `from_fn` | §16.11 |
| `sai:core/mem` | `size_of`, `align_of`, `swap`, `replace`, `take`, `drop`, `forget`, `transmute`, `zeroed`, `MaybeUninit` | §16.12.1 |
| `sai:core/ptr` | `Ptr`, `PtrMut` | §16.12.2 |
| `sai:core/ffi` | re-exports of `Ptr` and `PtrMut`; C type aliases, `CStr`, `Out` (Stage 7) | §16.12.3, §16.12.4 |
| `sai:core/fmt` | `Format`, `Writer`, `FormatSpec`, `FormatPad`, `FormatInt`, `FormatFloat`, `write_padded`, `FixedBuf` | §16.14 |
| `sai:core/future` | `Future`, `Stream`, `ready`, `pending`, `ReadyFuture`, `PendingFuture` | §16.13.1 |
| `sai:core/task` | `Poll`, `Context`, `Waker`, `RawWaker`, `RawWakerVTable` | §16.13.2 |
| `sai:core/testing` | `Arbitrary`, `Gen` (Stage 3) | §16.17 |

### 16.2.2 Modules of `sai:alloc`

| Module | Contents | Section |
|---|---|---|
| `sai:alloc/heap` | `allocate`, `deallocate`, `reallocate` | §16.15.1 |
| `sai:alloc/boxed` | `Box` | §16.15.2 |
| `sai:alloc/vec` | `Vec`, `Bytes`, `VecIntoIter`, `Drain`; allocating methods of `Slice` (`to_vec`) | §16.15.3 |
| `sai:alloc/string` | `String`, `FromUtf8Error`; allocating methods of `Str` (`to_ascii_lowercase`, `to_ascii_uppercase`, `repeat`, `replace`) | §16.15.4 |
| `sai:alloc/fmt` | `ToString` | §16.14.7 |
| `sai:alloc/arena` | `Arena` | §16.15.5 |
| `sai:alloc/ffi` | `CString` (Stage 7) | §16.12.4 |

### 16.2.3 Exhaustiveness

The two tables are the complete module trees. A conforming implementation MUST NOT add public modules, public items, public methods or trait impls to `sai:core` or `sai:alloc` (P7, P8). It MAY add package-private items, which no other package can name (04).

## 16.3 Lang items

### 16.3.1 Rules

1. A **lang item** is a library item that the compiler knows by identity, because a language construct is defined in terms of it (01 §1.9). The lang items are exactly the items of the tables in §16.3.2 to §16.3.4.
2. The compiler identifies a lang item by its canonical path (04 §4.9) inside `sai:core` or `sai:alloc`. No attribute marks it, and no other package can declare one (P8).
3. Desugarings refer to lang items by identity, never by name lookup (06, N-C-21). A module that declares its own `Option`, `Iterator` or `String` (allowed with `SHADOWS_PRELUDE`, 04 §4.4.4) does not change what `?`, `for` or an f-string use.
4. If the library source that a compiler is built with lacks a lang item of a stage the compiler claims, or declares it as an item of another kind, the compiler reports `INTERNAL_COMPILER_ERROR` ([impl/01](../impl/01-architecture.md) §1.10, §1.12). A lang item declared with a signature other than the one in this chapter is a defect of the toolchain, which the conformance suite detects ([impl/15](../impl/15-conformance-and-testing.md) §15.8). Neither can happen with a correctly built toolchain.
5. **Intrinsics** (§16.3.4) are lang-item functions whose bodies are supplied by the compiler instead of by library source. They are declared in the library with an ordinary signature, and their body in the library source is the single expression `unreachable()`, which the compiler never compiles.

### 16.3.2 Lang-item types

| Item | Path | Construct that uses it | Stage |
|---|---|---|---|
| `Str` | `core.str.Str` | type of string literals; `Str + Str` (D-EXPR-13); `Index` impls (§16.9.2) | 1 |
| `Slice<T>`, `SliceMut<T>` | `core.slice` | built-in indexing and slice patterns (06); coercion `SliceMut<T> ⇝ Slice<T>` (05 §5.15) | 2 |
| `Option<T>`, `Some`, `None` | `core.option` | `?` on `Option` ([10](10-errors-and-panics.md)); the result of `Iterator.next` in the `for` desugaring (06); the result of the compiler-provided `R.try_from` of refined types ([09](09-contracts-and-refinements.md) N-C-41) | 1 |
| `Result<T, E>`, `Ok`, `Err` | `core.result` | `?` on `Result` (10); entry points and component boundaries ([13](13-components-and-wit.md)) | 1 |
| `Ordering` and its variants | `core.cmp` | derived `PartialOrd` and `Ord` (05 §5.12.3) | 2 |
| `Range<T>`, `RangeInclusive<T>`, `RangeFrom<T>`, `RangeTo<T>`, `RangeToInclusive<T>`, `RangeFull` | `core.ops` | range expressions (06 §6.19) | 2 |
| `Ptr<T>`, `PtrMut<T>` | `core.ptr` | unsafe dereference `*p` (06 §6.9.1, [12](12-unsafe-and-ffi.md)); extern signatures (12) | 2 |
| `MaybeUninit<T>` | `core.mem` | storage without a validity requirement (12) | 2 |
| `FormatSpec` | `core.fmt` | the specifier argument of the f-string desugaring (06 §6.20.3, §16.14.3) | 2 |
| `Poll<T>`, `Context` | `core.task` | the state machines of `async` code (11, [impl/04](../impl/04-mir.md)) | 2 |
| `CStr`, `Out<T>` | `core.ffi` | `unsafe extern "C"` signatures (12) | 7 |
| `Gen` | `core.testing` | the generator passed to properties ([14](14-testing.md)) | 3 |
| `String` | `alloc.string` | result type of f-strings (06 §6.20.3) and of `Str + Str` (D-EXPR-13); WIT `string` (13) | 2 |
| `Vec<T>`, `Bytes` | `alloc.vec` | WIT `list<T>` and `list<u8>` (13); `Index` impls | 2 |
| `Box<T>` | `alloc.boxed` | dereference `*b` (06 §6.9); moving out of `*b` is not allowed, `Box.into_inner(b)` takes the value (07 N-B-2); fundamental type for coherence (05 §5.10.1) | 2 |
| `Arena` | `alloc.arena` | `region` blocks (07 §7.15, §16.15.5) | 2 |

### 16.3.3 Lang-item traits

| Item | Path | Construct that uses it | Stage |
|---|---|---|---|
| `Copy` | `core.marker` | copy instead of move (07); `@derive` (05) | 2 |
| `Clone` | `core.clone` | closure types (05 §5.5.3); `@derive` | 2 |
| `Drop` | `core.ops` | drop glue (07 §7.13) | 2 |
| `Default` | `core.default` | `@derive` | 2 |
| `PartialEq`, `Eq`, `PartialOrd`, `Ord` | `core.cmp` | `==`, `!=`, `<`, `<=`, `>`, `>=` on non-primitive operands (06 §6.8.10); `@derive` | 2 |
| `Hash`, `Hasher` | `core.hash` | `@derive(Hash)` | 2 |
| `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `BitAnd`, `BitOr`, `BitXor`, `BitNot` | `core.ops` | operator desugaring (06 §6.8.10); `@derive` on distinct types | 2 |
| `Index`, `IndexMut` | `core.ops` | trait-based indexing (06 §6.7.2) | 2 |
| `Fn`, `FnMut`, `FnOnce` | `core.ops` | closure, function item and function pointer types; calls (05 §5.5.4, 06 §6.5) | 2 |
| `IntoIterator`, `Iterator` | `core.iter` | `for` loops (06, D-EXPR-11) | 2 |
| `Format`, `Writer` | `core.fmt` | f-strings (06 §6.20.3) | 2 |
| `FormatPad`, `FormatInt`, `FormatFloat` | `core.fmt` | the applicability check of format specifiers (§16.14.3) | 3 |
| `Future` | `core.future` | `async fn`, `async` blocks, `.await` (11) | 2 (trait); 5 (syntax) |
| `Stream` | `core.future` | `for await` (11) | 2 (trait); 5 (syntax) |
| `Arbitrary` | `core.testing` | property parameters and `@derive(Arbitrary)` (14, 05) | 3 |

### 16.3.4 Lang-item functions and intrinsics

| Item | Path | Construct that uses it | Intrinsic | Stage |
|---|---|---|---|---|
| `panic` | `core.panic.panic` | explicit panics; panic kind `ExplicitPanic` (D-EXPR-06) | yes | 1 |
| `unreachable` | `core.panic.unreachable` | unreachable code; panic kind `ExplicitPanic` | yes | 1 |
| `Array.filled` | `core.array` | lowered to MIR `Repeat(op, N)` (06 §6.18.5, D-MIR-01) | yes | 2 |
| `String.new` | `alloc.string` | f-string desugaring (06 §6.20.3) | no | 2 |
| `wrapping_to`, `saturating_to`, `to_f32`, `to_f64` of the integer types; `trunc_to`, `checked_trunc_to`, `to_f32`, `to_f64` of the float types | `core.num` | lowered to MIR `Convert` (D-MIR-01, D-TYPE-06) | yes | 2 |
| the `Index` impls for the six range types (§16.6.2) | `core.str`, `core.slice`, `core.array`, `alloc.string`, `alloc.vec` | range indexing `e[a..b]` (D-EXPR-10, N-J-14) | yes | 2 |
| `size_of`, `align_of` | `core.mem` | layout queries ([impl/06](../impl/06-wasm-lowering-and-layout.md)) | yes | 2 |
| `transmute`, `zeroed`, `forget` | `core.mem` | reinterpretation and leaking (12, 07) | yes | 2 |
| `Ptr.read`, `PtrMut.write`, `PtrMut.drop_in_place`, the unaligned forms | `core.ptr` | raw memory access (12) | yes | 2 |
| `allocate`, `deallocate`, `reallocate` | `alloc.heap` | the global allocator hook (D-WASM-05, [impl/10](../impl/10-runtime.md)) | yes | 2 |
| `Arena.region_new` (package-private) | `alloc.arena` | creation of the hidden arena of a `region` block (§16.15.5) | no | 2 |

### 16.3.5 Sealed traits (N-J-11)

The traits `Integer`, `Float` (`core.num`), `Step`, `SliceRange` (`core.ops`), `FormatInt` and `FormatFloat` (`core.fmt`, §16.14.4) are **sealed**: only `sai:core` implements them, for exactly the types this chapter lists. An `impl` of a sealed trait in any other package is the error `SEALED_TRAIT_IMPL`. Rationale: these traits describe closed sets of built-in types, and generic code relies on the set being closed (P8). The function traits are protected by `MANUAL_FN_IMPL` and `Copy` by `MANUAL_COPY_IMPL` ([05](05-types.md) §5.9.1). Every other trait of this chapter can be implemented by any package, subject to coherence.

## 16.4 The prelude

The prelude mechanism (an implicit scope after the module scope, reserved names, `SHADOWS_PRELUDE`) is defined in [04](04-modules-and-names.md) §4.2.6 and §4.4.4. Its contents are exactly the following bindings (N-J-10). Each prelude name denotes the item in the *Item* column.

| Prelude names | Item | Kind |
|---|---|---|
| `I8`, `I16`, `I32`, `I64`, `U8`, `U16`, `U32`, `U64`, `ISize`, `USize`, `F32`, `F64`, `Bool`, `Byte`, `Char`, `Never` | the built-in types (05 §5.3) | reserved type names |
| `Int`, `UInt` | `I64`, `U64` | reserved type names |
| `Array` | the built-in array type (05 §5.4.2) | type |
| `Str` | `core.str.Str` | type |
| `Slice`, `SliceMut` | `core.slice.Slice`, `core.slice.SliceMut` | types |
| `Option`, `Some`, `None` | `core.option.Option` and its variants | type, variants |
| `Result`, `Ok`, `Err` | `core.result.Result` and its variants | type, variants |
| `Ordering` | `core.cmp.Ordering` | type |
| `String` | `alloc.string.String` | type |
| `Vec`, `Bytes` | `alloc.vec.Vec`, `alloc.vec.Bytes` | type, alias |
| `Box` | `alloc.boxed.Box` | type |
| `Copy` | `core.marker.Copy` | trait |
| `Clone` | `core.clone.Clone` | trait |
| `Default` | `core.default.Default` | trait |
| `PartialEq`, `Eq`, `PartialOrd`, `Ord` | `core.cmp` | traits |
| `Hash` | `core.hash.Hash` | trait |
| `From`, `TryFrom` | `core.convert` | traits |
| `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `BitAnd`, `BitOr`, `BitXor`, `BitNot` | `core.ops` | traits |
| `Index`, `IndexMut` | `core.ops` | traits |
| `Fn`, `FnMut`, `FnOnce` | `core.ops` | traits |
| `Drop` | `core.ops.Drop` | trait |
| `IntoIterator`, `Iterator`, `FromIterator` | `core.iter` | traits |
| `Format` | `core.fmt.Format` | trait |
| `ToString` | `alloc.fmt.ToString` | trait |
| `Future`, `Stream` | `core.future` | traits |
| `Arbitrary` | `core.testing.Arbitrary` | trait |
| `panic`, `unreachable` | `core.panic` | functions |

Notes:

1. The variants of `Option` and `Result` are bound unqualified. No other variant is. `Ordering.Less` is always written with its type.
2. The prelude contains every trait of D-TYPE-08 (the derivable traits and the forwarding operator traits), as 04 §4.2.6 requires. It also contains the traits whose methods ordinary code calls by method syntax (`Iterator`, `IntoIterator`, `FromIterator`, `ToString`, `From`, `TryFrom`), and `Future` and `Stream` (11 §11.1.3).
3. Items that only implementers or unsafe code need are not in the prelude: `Writer`, `FormatSpec`, `Hasher`, `Gen`, the range types, `Ptr`, `PtrMut`, `MaybeUninit`, `Poll`, `Context`, `Waker`, and the free functions of `core.mem`. They are imported with `use` (for example `use sai:core/fmt.{Writer, FormatSpec}`).
4. Names from `sai:alloc` are in the prelude in every profile. Using them is restricted only through the `alloc` effect (04 §4.2.6, 08).
5. Adding a name to the prelude is a language change that requires a new edition of this chapter. Because prelude names other than reserved type names can be shadowed, an addition never breaks a module that declares the same name.

---

## 16.5 Fundamental traits

### 16.5.1 Effect bounds of the fundamental traits (N-J-12)

The comparison and hashing methods are pure by declaration: `PartialEq.eq`, every method of `PartialOrd` and `Ord`, `Hash.hash`, and the methods of `Hasher` are declared `effects {}`. Every impl of them is therefore pure (`TRAIT_IMPL_EFFECTS_EXCEED_BOUND`, 08 §8.6.3). Rationale: comparisons appear in contract predicates and in the sorting and searching functions of this chapter, and their results must not depend on, or change, the outside world (P8, determinism principle).

`Clone.clone`, `Default.default`, `From.from`, `TryFrom.try_from`, the operator traits, `Index.index`, `IndexMut.index_mut`, the iterator traits, `Format.format`, `Writer.write_str`, `ToString.to_string` and `Arbitrary.arbitrary` have no bound. Their effects are latent (08 §8.8), because cloning a `String`, adding two `Str`, or formatting into a `String` allocates.

### 16.5.2 `Copy` and `Clone`

```sai
pub trait Clone {
    fn clone(self: borrow Self) -> Self
}

pub trait Copy: Clone {}
```

- `Copy` has no methods. Copy semantics are defined in [07](07-ownership-and-borrowing.md). `Copy` cannot be implemented manually (`MANUAL_COPY_IMPL`, 05 §5.9.1).
- For a `Copy` type, `clone` returns a bitwise copy of the referent and has no effects.
- The compiler provides `Copy` and `Clone` for primitives, `()`, `Never`, tuples, arrays, `borrow T`, function items, function pointers and closures (05 §5.11.4). This chapter adds them for library types as each section states.

### 16.5.3 `Default`

```sai
pub trait Default {
    fn default() -> Self
}
```

| Type | `default()` |
|---|---|
| integer types | `0` |
| `F32`, `F64` | `0.0` (positive zero) |
| `Bool` | `false` |
| `Byte` | `b'\0'` |
| `Char` | `'\0'` (U+0000) |
| `()` | `()` |
| `Str` | `""` |
| `Slice<T>` | the empty slice |
| `Option<T>` | `None` (no bound on `T`) |
| `String`, `Vec<T>` | empty, capacity 0, no allocation, no effects |
| `Box<T>` with `T: Default` | `Box.new(T.default())`, `effects { alloc }` |
| `SipHasher13` | `SipHasher13.new()` |

Tuples and arrays get `Default` from the compiler (05 §5.11.4). `Ordering`, `Result`, `SliceMut` and the pointer types have no `Default`.

### 16.5.4 Comparison: `core.cmp`

```sai
@derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)
pub enum Ordering {
    Less
    Equal
    Greater
}

pub trait PartialEq {
    fn eq(self: borrow Self, other: borrow Self) -> Bool
        effects {}
}

pub trait Eq: PartialEq {}

pub trait PartialOrd: PartialEq {
    fn partial_cmp(self: borrow Self, other: borrow Self) -> Option<Ordering>
        effects {}

    fn lt(self: borrow Self, other: borrow Self) -> Bool
        effects {}
    {
        match self.partial_cmp(other) {
            Some(Ordering.Less) => true
            _ => false
        }
    }

    fn le(self: borrow Self, other: borrow Self) -> Bool
        effects {}
    {
        match self.partial_cmp(other) {
            Some(Ordering.Less) => true
            Some(Ordering.Equal) => true
            _ => false
        }
    }

    fn gt(self: borrow Self, other: borrow Self) -> Bool
        effects {}
    {
        match self.partial_cmp(other) {
            Some(Ordering.Greater) => true
            _ => false
        }
    }

    fn ge(self: borrow Self, other: borrow Self) -> Bool
        effects {}
    {
        match self.partial_cmp(other) {
            Some(Ordering.Greater) => true
            Some(Ordering.Equal) => true
            _ => false
        }
    }
}

pub trait Ord: Eq + PartialOrd {
    fn cmp(self: borrow Self, other: borrow Self) -> Ordering
        effects {}

    fn max(self: Self, other: Self) -> Self
        effects {}
    {
        match other.cmp(borrow self) {
            Ordering.Less => self
            _ => other
        }
    }

    fn min(self: Self, other: Self) -> Self
        effects {}
    {
        match other.cmp(borrow self) {
            Ordering.Less => other
            _ => self
        }
    }

    fn clamp(self: Self, lo: Self, hi: Self) -> Self
        effects {}
    {
        if hi < lo {
            panic("clamp: lower bound is greater than upper bound")
        }
        if self < lo {
            lo
        } else if hi < self {
            hi
        } else {
            self
        }
    }
}
```

- `a != b` is `not a.eq(b)` (06 §6.8.10). There is no `ne` method, so `!=` is always the negation of `==` (P8).
- An `Ord` impl MUST be consistent with its `PartialOrd` impl (`partial_cmp` returns `Some(cmp)`) and with `PartialEq` (`eq` is true iff `cmp` is `Equal`), and MUST be a total order. Derived impls satisfy this (05 §5.12.3). The library functions that sort, search, or take a maximum (§16.10, §16.11) rely on it. If a user impl violates it, those functions still terminate, never cause undefined behavior and never panic because of it, but their result is any permutation (for sorts) or any element (for searches and extrema) that the algorithm produces.
- `max` returns `other` when the two are equal and `min` returns `self`, so `a.min(b)` and `a.max(b)` together always return both values.
- `clamp` panics with kind `ExplicitPanic` and the message shown if `hi < lo`.

Ordering methods:

| Signature | Result |
|---|---|
| `fn reverse(self: Self) -> Ordering` | `Less` ↔ `Greater`, `Equal` unchanged |
| `fn then(self: Self, other: Ordering) -> Ordering` | `self` unless `self` is `Equal`, then `other` |
| `fn then_with<F: FnOnce() -> Ordering>(self: Self, f: F) -> Ordering` | `self` unless `self` is `Equal`, then `f()`; `f` is called only in that case |
| `fn is_eq(self: Self) -> Bool`, `is_ne`, `is_lt`, `is_gt`, `is_le`, `is_ge` | the corresponding test |

Free functions of `core.cmp`: `fn max<T: Ord>(a: T, b: T) -> T` is `a.max(b)`, and `fn min<T: Ord>(a: T, b: T) -> T` is `a.min(b)`.

### 16.5.5 Comparison impls for built-in and core types

| Type | Traits | Order |
|---|---|---|
| integer types, `Byte` | `PartialEq`, `Eq`, `PartialOrd`, `Ord` | numeric (`Byte` as its `U8` value) |
| `F32`, `F64` | `PartialEq`, `PartialOrd` | IEEE 754: `eq` is false if either is NaN, `-0.0 == 0.0`; `partial_cmp` is `None` if either is NaN. For a total order use `total_cmp` (§16.8.4) |
| `Bool` | `PartialEq`, `Eq`, `PartialOrd`, `Ord` | `false < true` |
| `Char` | `PartialEq`, `Eq`, `PartialOrd`, `Ord` | by scalar value |
| `()` | `PartialEq`, `Eq`, `PartialOrd`, `Ord` | always equal |
| `Str`, `String` | `PartialEq`, `Eq`, `PartialOrd`, `Ord` | lexicographic by bytes, which equals the order of code points |
| `Slice<T>`, `SliceMut<T>`, `Vec<T>` | each trait when `T` has it | lexicographic by elements with `T`'s method; a proper prefix is less |
| `Box<T>` | each trait when `T` has it | as `T` |
| `Option<T>`, `Result<T, E>`, `Ordering` | derived | `None < Some`, `Ok < Err`, `Less < Equal < Greater` (05 §5.4.5) |
| `Ptr<T>`, `PtrMut<T>` | `PartialEq`, `Eq` | by address |
| range types | `PartialEq`, `Eq` when `T: Eq` | field by field |

The built-in operators on primitive operands (06 §6.8) have exactly the semantics of these impls. The impls exist so that generic code can compare. Tuples, arrays, `Never` and borrows get their impls from the compiler (05 §5.11.4).

### 16.5.6 Hashing: `core.hash`

```sai
pub trait Hasher {
    fn write(self: borrow mut Self, bytes: Slice<Byte>)
        effects {}

    fn finish(self: borrow Self) -> U64
        effects {}
}

pub trait Hash {
    fn hash<H: Hasher>(self: borrow Self, state: borrow mut H)
        effects {}
}
```

- `Hash` impls MUST agree with `Eq`: if `a == b` then `a` and `b` feed the same bytes to the hasher. The library impls below do.
- `finish` does not reset or consume the hasher. It returns the hash of every byte written so far and may be called repeatedly.

The bytes fed by the core impls are fixed, so that a hash value is the same on every target and in every profile:

| Type | Bytes written |
|---|---|
| `I8` … `I64`, `U8` … `U64` | `to_le_bytes()` of the value |
| `ISize`, `USize` | the value converted to `I64` or `U64`, then as that type (8 bytes on every target) |
| `Bool` | one byte, `0` or `1` |
| `Byte` | the byte |
| `Char` | the scalar value as a `U32` |
| `()` | nothing |
| `Str`, `String` | the UTF-8 bytes, then the byte `0xFF` (which never occurs in UTF-8, so concatenations hash differently) |
| `Slice<T>`, `SliceMut<T>`, `Vec<T>` | the length as a `U64`, then each element's `hash` in order |
| `Box<T>` | as `T` |
| `Option<T>`, `Result<T, E>`, `Ordering` | as derived (05 §5.12.3): the discriminant as an `I64`, then the payload |
| `Ptr<T>`, `PtrMut<T>` | the address as a `U64` |
| range types | the fields in declaration order |

Floats do not implement `Hash`, because they do not implement `Eq`. Hash their `to_bits()` explicitly if needed.

`SipHasher13` is the standard hasher. It implements SipHash-1-3 (one compression round per message block and three finalization rounds) of Aumasson and Bernstein with a 128-bit key given as two little-endian `U64` words `k0` and `k1`, over the concatenation of all bytes written, with a 64-bit output.

| Signature | Effects | Notes |
|---|---|---|
| `fn new() -> SipHasher13` | — | keys `k0 = 0`, `k1 = 0` |
| `fn with_keys(k0: U64, k1: U64) -> SipHasher13` | — | |

`SipHasher13` implements `Hasher`, `Clone` and `Default`. There is no randomly keyed hasher in `sai:core` (§16.1.5). Hash tables that need protection against flooding take their keys from a capability, outside this chapter.

### 16.5.7 Conversions: `core.convert`

```sai
pub trait From<T> {
    fn from(value: T) -> Self
}

pub trait TryFrom<T> {
    fn try_from(value: T) -> Option<Self>
}
```

- `T.from(x)` is a lossless conversion and `T.try_from(x)` a checked one that returns `None` when the value cannot be represented (D-TYPE-06). `TryFrom` has no error type (D-TYPE-06).
- There is no reflexive impl (`From<T> for T`) and no blanket impl relating `From` and `TryFrom` (N-J-13). Each conversion that exists is listed in this chapter (P1, P7).
- The numeric impls are listed in §16.8.5, and the impls of the owned types in §16.15.
- **Refined types.** `R.try_from(value: B) -> Option<R>` on a refined type `R = B where p` is not a `TryFrom` impl. It is the compiler-provided associated function of [09](09-contracts-and-refinements.md) N-C-41, found by path lookup as if it were declared inherently (05 §5.13), with an empty effect set. The library declares nothing for it. Because that function is found before trait items, an `impl TryFrom<B> for R` written by a program is never selected by `R.try_from(v)`; it can only be used through a `TryFrom` bound.

```sai
type Percentage = I32 where self >= 0 and self <= 100

fn parse_percentage(v: I32) -> Option<Percentage> {
    Percentage.try_from(v)
}
```

## 16.6 Operators, indexing, functions, drop and ranges: `core.ops`

### 16.6.1 Operator traits

```sai
pub trait Add<Rhs> {
    type Output

    fn add(self: Self, rhs: Rhs) -> Self.Output
}

pub trait Neg {
    type Output

    fn neg(self: Self) -> Self.Output
}
```

The other operator traits are declared exactly like `Add` (binary) or `Neg` (unary), with the trait and method names below. The operators and their desugaring are in 06 §6.8.10.

| Trait | Method | Operator | Arity |
|---|---|---|---|
| `Add<Rhs>` | `add` | `+` | binary |
| `Sub<Rhs>` | `sub` | `-` | binary |
| `Mul<Rhs>` | `mul` | `*` | binary |
| `Div<Rhs>` | `div` | `/` | binary |
| `Rem<Rhs>` | `rem` | `%` | binary |
| `BitAnd<Rhs>` | `bitand` | `&` | binary |
| `BitOr<Rhs>` | `bitor` | `\|` | binary |
| `BitXor<Rhs>` | `bitxor` | `^` | binary |
| `Neg` | `neg` | unary `-` | unary |
| `BitNot` | `bitnot` | `~` | unary |

There are no traits for shifts, for the wrapping operators `+%`, `-%`, `*%`, or for `and`, `or` and `not` (06 §6.8.10). There are no compound-assignment traits: `a += b` is typed as `a + b` followed by an assignment (06).

`sai:core` declares the following impls for the primitive types. Each has `Rhs = Self` and `Output = Self`, an empty declared effect set, and exactly the semantics of the built-in operator (06 §6.8), including its panics (`Overflow(op)`, `DivByZero`, `RemByZero`). They exist so that generic code can use the operators. The built-in operators are not calls to them.

| Types | Traits |
|---|---|
| `I8`, `I16`, `I32`, `I64`, `ISize` | `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `BitAnd`, `BitOr`, `BitXor`, `BitNot` |
| `U8`, `U16`, `U32`, `U64`, `USize` | `Add`, `Sub`, `Mul`, `Div`, `Rem`, `BitAnd`, `BitOr`, `BitXor`, `BitNot` |
| `F32`, `F64` | `Add`, `Sub`, `Mul`, `Div`, `Neg` (IEEE 754, no panics). There is no `Rem`; the float remainder is the method `rem` of §16.8.4 |
| `Byte` | `BitAnd`, `BitOr`, `BitXor`, `BitNot` |
| `Bool`, `Char`, `()` | none |

`sai:alloc` declares the two concatenation impls of D-EXPR-13:

| Impl | Method signature | Effects | Behavior |
|---|---|---|---|
| `impl Add<Str> for Str` with `Output = String` | `fn add(self: Self, rhs: Str) -> String` | alloc | a new `String` with capacity `self.len() + rhs.len()` holding both |
| `impl Add<Str> for String` with `Output = String` | `fn add(self: Self, rhs: Str) -> String` | alloc | appends `rhs` to `self` as `push_str` does and returns `self`. The buffer of `self` is reused and reallocated only if its capacity is too small |

By N-B-12 (07 §7.7.5), the `Str` in the trait argument of an impl takes the single region of the target type, or `'static` if the target carries none. In the first impl both operands share the region of the left `Str`, and subtyping lets operands of different regions meet. In the second impl the right operand is `Str<'static>`, because `String` carries no region. `buf.push_str(s)` (§16.15.4) appends a `Str` of any region.

### 16.6.2 Indexing

```sai
pub trait Index<Idx> {
    type Output

    fn index(self: borrow Self, index: Idx) -> borrow Self.Output
}

pub trait IndexMut<Idx>: Index<Idx> {
    fn index_mut(self: borrow mut Self, index: Idx) -> borrow mut Self.Output
}
```

Indexing with a `USize` on `Array<T, N>`, `Slice<T>` and `SliceMut<T>` is built in (D-EXPR-10, 06 §6.7.1) and uses no impl. The impls are:

| Impl | `Output` | Panics |
|---|---|---|
| `impl<T> Index<USize> for Vec<T>` | `T` | `BoundsCheck` if `index >= self.len()` |
| `impl<T> IndexMut<USize> for Vec<T>` | `T` | `BoundsCheck` if `index >= self.len()` |
| `impl Index<USize> for Str` | `Byte` | `BoundsCheck` if `index >= self.len()` |
| `impl Index<USize> for String` | `Byte` | `BoundsCheck` if `index >= self.len()` |
| `impl Index<Q> for Str`, `impl Index<Q> for String`, for each of the six range types `Q` below | `Str` | `BoundsCheck` if `Q.bounds(len)` is `None`, or if either bound is not a char boundary (D-EXPR-10) |
| `impl<T> Index<Q> for Slice<T>`, `SliceMut<T>`, `Vec<T>`, and `impl<T, const N: USize> Index<Q> for Array<T, N>`, for each `Q` below | `Slice<T>` | `BoundsCheck` if `Q.bounds(len)` is `None` |

Here `Q` ranges over `Range<USize>`, `RangeInclusive<USize>`, `RangeFrom<USize>`, `RangeTo<USize>`, `RangeToInclusive<USize>` and `RangeFull`. The impls are written once per range type, not as one impl generic over `SliceRange`, because the overlap check ignores bounds (05 §5.10.2) and a generic impl would overlap the `USize` impls.

- `Str` and `String` have no `IndexMut`, because writing a byte could break UTF-8. No type has `IndexMut` for ranges. Mutable subviews are obtained with `slice_mut` (§16.10.1, §16.15.3).
- **Range indexing (N-J-14).** The range impls above are intrinsics (§16.3.4). When trait-based indexing (06 §6.7.2) selects one of them, `*Index.index(borrow e, q)` is evaluated as follows: compute the view `e.slice(q)` with the panics listed, store it in a temporary (06 §6.24), and use the temporary as the place. The view has the region of the borrow of `e`. Because `Str` and `Slice<T>` are `Copy` (05 §5.11.4), `s[1..3]` read in a value context is a `Str` value. `borrow s[1..3]` borrows the temporary and is subject to the temporary rules of 07 §7.5.6.
- A `BoundsCheck` panic raised by these impls and by the library functions of §16.9 and §16.10 has the message of the built-in bounds check (10). For a range, `<i>` is the end bound if it exceeds the length, and the start bound otherwise. A char-boundary failure has the message `byte index <i> is not a char boundary`, where `<i>` is the offending bound in decimal.
- `Index.index` is never called with a `borrow` that the caller did not write: the implicit borrow belongs to the desugaring (06 §6.7.2).

### 16.6.3 Function traits

`Fn`, `FnMut` and `FnOnce` are lang-item traits with the receivers `borrow Self`, `borrow mut Self` and `Self`, and with `FnOnce` a supertrait of `FnMut` and `FnMut` of `Fn` (05 §5.5.4, D-TYPE-09). They have no source-level declaration that a program can use: they are written only with the sugar `Fn(A1, …, An) -> R` (03 §3.5), they have no nameable methods or associated types, and calls go through call syntax (06 §6.5). Only the compiler implements them (`MANUAL_FN_IMPL`, 05). Their declarations in the library source are placeholders recognized as lang items (§16.3.1).

### 16.6.4 `Drop`

```sai
pub trait Drop {
    fn drop(self: borrow mut Self)
        effects {}
}
```

Drop glue, the order of drops and the ban on explicit `drop` calls (`EXPLICIT_DROP_CALL`) are defined in 05 §5.9.3 and 07 §7.13. An impl whose body has effects is `DROP_HAS_EFFECTS` (08). To drop a value early, call `core.mem.drop` (§16.12.1).

### 16.6.5 Range types

The six range types are the lang items constructed by the range expressions of 06 §6.19. Their fields are public, so a range can also be built with record construction.

```sai
@derive(Clone, PartialEq, Eq, Hash)
pub type Range<T> {
    pub start: T
    pub end: T
}

@derive(Clone, PartialEq, Eq, Hash)
pub type RangeInclusive<T> {
    pub start: T
    pub end: T
}

@derive(Clone, PartialEq, Eq, Hash)
pub type RangeFrom<T> {
    pub start: T
}

@derive(Copy, Clone, PartialEq, Eq, Hash)
pub type RangeTo<T> {
    pub end: T
}

@derive(Copy, Clone, PartialEq, Eq, Hash)
pub type RangeToInclusive<T> {
    pub end: T
}

@derive(Copy, Clone, PartialEq, Eq, Hash)
pub type RangeFull {
}
```

`Range`, `RangeInclusive` and `RangeFrom` are iterators, so they are deliberately not `Copy`: a `for` loop over a range variable consumes it, and a copy cannot be advanced by mistake. Use `.clone()` to iterate a range twice.

| Signature | Types | Effects | Result |
|---|---|---|---|
| `fn contains(self: borrow Self, value: borrow T) -> Bool` | all six, with `T: PartialOrd` (`RangeFull` has no `T`) | — | `start <= value and value < end` for `Range`; `start <= value and value <= end` for `RangeInclusive`; the one-sided test for the others; `true` for `RangeFull` |
| `fn is_empty(self: borrow Self) -> Bool` | `Range<T>`, `RangeInclusive<T>`, with `T: PartialOrd` | — | `not (start < end)` for `Range`; `not (start <= end)` for `RangeInclusive`. A range with a NaN end is empty |

The ranges implement `Format` when `T` does (§16.14.6).

### 16.6.6 `Step` and range iteration

```sai
pub trait Step: Copy + Ord {
    fn successor(self: Self) -> Option<Self>
        effects {}

    fn predecessor(self: Self) -> Option<Self>
        effects {}
}
```

`Step` is sealed (§16.3.5). It is implemented for the ten integer types, where `successor` is `checked_add(1)` and `predecessor` is `checked_sub(1)`, and for `Char`, where they step to the next or previous Unicode scalar value, skipping the surrogate range U+D800 to U+DFFF, and return `None` past U+10FFFF or below U+0000.

| Impl | `next` | `next_back` (§16.11.2) |
|---|---|---|
| `impl<T: Step> Iterator for Range<T>`, `Item = T` | if `start < end`, return `start` and set `start` to its successor; otherwise `None` | if `start < end`, set `end` to its predecessor and return it; otherwise `None` |
| `impl<T: Step> Iterator for RangeInclusive<T>`, `Item = T` | if `start < end`, return `start` and advance it. If `start == end`, return `start` and empty the range: set `start` to its successor, or if there is none, set `end` to its predecessor. Otherwise `None` | symmetric: if `start == end`, return `end` and empty the range by moving `end` down, or `start` up if `end` has no predecessor |
| `impl<T: Step> Iterator for RangeFrom<T>`, `Item = T` | if `start` has a successor `s`, return `start` and set `start` to `s`. Otherwise panic with kind `Overflow(Add)` and the message `range iteration overflowed`. So the maximum value of `T` is never produced | not implemented |

- Every type `T` with `T: Step` has at least two values, so emptying a `RangeInclusive` always succeeds. After it, `is_empty()` is `true`.
- `Range<T>` and `RangeInclusive<T>` also implement `DoubleEndedIterator` (§16.11.2). `Range<T>` implements `ExactSizeIterator` for the integer types of at most 32 bits, and `RangeInclusive<T>` for those of at most 16 bits, because only then does every length fit in a 32-bit `USize`.
- Each of the three implements `IntoIterator` for itself (§16.11.1).
- `RangeTo`, `RangeToInclusive` and `RangeFull` are not iterators. A `for` loop over them is `NOT_ITERABLE` (06).
- An iteration with a non-`Step` element type, such as `0.0..1.0`, is `NOT_ITERABLE`.

### 16.6.7 `SliceRange`

```sai
pub trait SliceRange {
    fn bounds(self: Self, len: USize) -> Option<(USize, USize)>
        effects {}
}
```

`SliceRange` is sealed (§16.3.5). It is implemented exactly for the six range types over `USize` (`RangeFull` has no element type). `bounds(len)` returns `Some((lo, hi))` with `lo <= hi <= len` when the range selects the elements `lo` up to but excluding `hi` of a sequence of length `len`, and `None` otherwise:

| Range | Result |
|---|---|
| `a..b` | `Some((a, b))` if `a <= b` and `b <= len` |
| `a..=b` | `Some((a, b + 1))` if `b < len` and `a <= b + 1` |
| `a..` | `Some((a, len))` if `a <= len` |
| `..b` | `Some((0, b))` if `b <= len` |
| `..=b` | `Some((0, b + 1))` if `b < len` |
| `..` | `Some((0, len))` |

`SliceRange` is the parameter bound of the `slice`, `get_range` and `slice_mut` methods (§16.9.2, §16.10.1, §16.15.3). An empty range such as `3..3` or `3..=2` with `3 <= len` is valid and selects no element. `5..1` is `None` (06 §6.19 allows constructing it, but it selects nothing valid).

## 16.7 `Option`, `Result` and explicit panics

### 16.7.1 `core.option`

`Option<T>` is declared as in 05 §5.4.5, with the variant order `None`, `Some` and the derives `@derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)`. It is not `@must_use` (D-EXPR-01). It also has `impl<T> Default for Option<T>` returning `None`, `Format` (§16.14.6), and a hand-written library impl of `Arbitrary` whose distribution is given in §16.17. That impl is not derived, because a derived `Arbitrary` chooses variants uniformly (05 §5.12.3).

In the tables of §16.7, `F`, `P` and `O` are closure parameters whose effects are latent (§16.1.2 item 4). A closure is called at most once, and only when the *Result* column says so.

| Signature | Bound of the impl | Panics | Result |
|---|---|---|---|
| `fn is_some(self: borrow Self) -> Bool` | | — | `true` for `Some` |
| `fn is_none(self: borrow Self) -> Bool` | | — | `true` for `None` |
| `fn is_some_and<P: FnOnce(T) -> Bool>(self: Self, predicate: P) -> Bool` | | — | `predicate(v)` for `Some(v)`, else `false` |
| `fn as_ref(self: borrow Self) -> Option<borrow T>` | | — | a borrow of the payload |
| `fn as_mut(self: borrow mut Self) -> Option<borrow mut T>` | | — | a mutable borrow of the payload |
| `fn unwrap(self: Self) -> T` | | `Unwrap`, message ``called `Option.unwrap` on a `None` value`` | the payload |
| `fn expect(self: Self, msg: Str) -> T` | | `Unwrap`, message `msg` | the payload |
| `fn unwrap_or(self: Self, default: T) -> T` | | — | the payload, else `default` |
| `fn unwrap_or_else<F: FnOnce() -> T>(self: Self, f: F) -> T` | | — | the payload, else `f()` |
| `fn unwrap_or_default(self: Self) -> T` | `T: Default` | — | the payload, else `T.default()` |
| `fn map<U, F: FnOnce(T) -> U>(self: Self, f: F) -> Option<U>` | | — | `Some(f(v))` for `Some(v)` |
| `fn map_or<U, F: FnOnce(T) -> U>(self: Self, default: U, f: F) -> U` | | — | `f(v)` for `Some(v)`, else `default` |
| `fn and_then<U, F: FnOnce(T) -> Option<U>>(self: Self, f: F) -> Option<U>` | | — | `f(v)` for `Some(v)`, else `None` |
| `fn or_else<F: FnOnce() -> Option<T>>(self: Self, f: F) -> Option<T>` | | — | `self` if `Some`, else `f()` |
| `fn filter<P: FnOnce(borrow T) -> Bool>(self: Self, predicate: P) -> Option<T>` | | — | `self` if `Some(v)` and `predicate(borrow v)`, else `None` |
| `fn ok_or<E>(self: Self, err: E) -> Result<T, E>` | | — | `Ok(v)` for `Some(v)`, else `Err(err)` |
| `fn ok_or_else<E, F: FnOnce() -> E>(self: Self, f: F) -> Result<T, E>` | | — | `Ok(v)`, else `Err(f())` |
| `fn zip<U>(self: Self, other: Option<U>) -> Option<(T, U)>` | | — | `Some((a, b))` if both are `Some` |
| `fn take(self: borrow mut Self) -> Option<T>` | | — | the old value; `self` becomes `None` |
| `fn replace(self: borrow mut Self, value: T) -> Option<T>` | | — | the old value; `self` becomes `Some(value)` |
| `fn get_or_insert_with<F: FnOnce() -> T>(self: borrow mut Self, f: F) -> borrow mut T` | | — | the payload, after storing `f()` if `self` was `None` |
| `fn flatten(self: Self) -> Option<T>` | `Self = Option<Option<T>>` | — | the inner option |
| `fn copied(self: Self) -> Option<T>` | `Self = Option<borrow T>`, `T: Copy` | — | a copy of the referent |
| `fn cloned(self: Self) -> Option<T>` | `Self = Option<borrow T>`, `T: Clone` | — | `v.clone()` |

There are no methods named `or`, `and` or `xor`: `and` and `or` are keywords (02), and the closure forms above cover their uses. There is no `unwrap_unchecked`.

```sai
fn first_even(xs: Slice<I32>) -> Option<I32> {
    xs.iter().copied().find(fn(x) { *x % 2 == 0 })
}
```

### 16.7.2 `core.result`

`Result<T, E>` is declared as in 05 §5.4.5. It is `@must_use` (D-EXPR-01) and has the derives `@derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)`, `Format` (§16.14.6), and a hand-written library impl of `Arbitrary` (§16.17), not a derived one, for the reason given for `Option` (§16.7.1). It has no `Default`.

| Signature | Bound of the impl | Panics | Result |
|---|---|---|---|
| `fn is_ok(self: borrow Self) -> Bool` | | — | `true` for `Ok` |
| `fn is_err(self: borrow Self) -> Bool` | | — | `true` for `Err` |
| `fn is_ok_and<P: FnOnce(T) -> Bool>(self: Self, predicate: P) -> Bool` | | — | `predicate(v)` for `Ok(v)`, else `false` |
| `fn is_err_and<P: FnOnce(E) -> Bool>(self: Self, predicate: P) -> Bool` | | — | `predicate(e)` for `Err(e)`, else `false` |
| `fn ok(self: Self) -> Option<T>` | | — | `Some(v)` for `Ok(v)` |
| `fn err(self: Self) -> Option<E>` | | — | `Some(e)` for `Err(e)` |
| `fn as_ref(self: borrow Self) -> Result<borrow T, borrow E>` | | — | borrows of the payload |
| `fn as_mut(self: borrow mut Self) -> Result<borrow mut T, borrow mut E>` | | — | mutable borrows of the payload |
| `fn map<U, F: FnOnce(T) -> U>(self: Self, f: F) -> Result<U, E>` | | — | `Ok(f(v))` for `Ok(v)` |
| `fn map_err<E2, O: FnOnce(E) -> E2>(self: Self, op: O) -> Result<T, E2>` | | — | `Err(op(e))` for `Err(e)` |
| `fn map_or<U, F: FnOnce(T) -> U>(self: Self, default: U, f: F) -> U` | | — | `f(v)` for `Ok(v)`, else `default` |
| `fn and_then<U, F: FnOnce(T) -> Result<U, E>>(self: Self, f: F) -> Result<U, E>` | | — | `f(v)` for `Ok(v)` |
| `fn or_else<E2, O: FnOnce(E) -> Result<T, E2>>(self: Self, op: O) -> Result<T, E2>` | | — | `op(e)` for `Err(e)` |
| `fn unwrap(self: Self) -> T` | | `Unwrap`, message ``called `Result.unwrap` on an `Err` value`` | the `Ok` payload |
| `fn expect(self: Self, msg: Str) -> T` | | `Unwrap`, message `msg` | the `Ok` payload |
| `fn unwrap_err(self: Self) -> E` | | `Unwrap`, message ``called `Result.unwrap_err` on an `Ok` value`` | the `Err` payload |
| `fn expect_err(self: Self, msg: Str) -> E` | | `Unwrap`, message `msg` | the `Err` payload |
| `fn unwrap_or(self: Self, default: T) -> T` | | — | the `Ok` payload, else `default` |
| `fn unwrap_or_else<F: FnOnce(E) -> T>(self: Self, f: F) -> T` | | — | the `Ok` payload, else `f(e)` |
| `fn unwrap_or_default(self: Self) -> T` | `T: Default` | — | the `Ok` payload, else `T.default()` |
| `fn copied(self: Self) -> Result<T, E>` | `Self = Result<borrow T, E>`, `T: Copy` | — | a copy of the referent |
| `fn cloned(self: Self) -> Result<T, E>` | `Self = Result<borrow T, E>`, `T: Clone` | — | `v.clone()` |

The panic messages of `unwrap` and `unwrap_err` do not include the payload, so they need no `Format` bound (N-J-15). Code that wants the error in the message writes `match` or `expect` with a formatted message, or propagates with `?` (10).

### 16.7.3 `core.panic`

| Signature | Effects | Panics |
|---|---|---|
| `fn panic(msg: Str) -> Never` | — | always, kind `ExplicitPanic`, message `msg` |
| `fn unreachable() -> Never` | — | always, kind `ExplicitPanic`, message `entered unreachable code` |

Both are intrinsics (§16.3.4). The location is the call expression (§16.1.4). What happens after the panic is defined in [10](10-errors-and-panics.md). A message held in a `String` is passed as `s.as_str()`. `assert` is a statement, not a library function (06 §6.20.2).

## 16.8 Numbers: `core.num`

### 16.8.1 Constants and the numeric traits

Impls cannot contain constants (03 `ImplItem`), so the numeric constants are module-level `const` items of `core.num`. For every integer type `X` (`I8`, `I16`, `I32`, `I64`, `ISize`, `U8`, `U16`, `U32`, `U64`, `USize`), with `<X>` its name in capitals:

| Constant | Type | Value |
|---|---|---|
| `<X>_MIN` | `X` | the smallest value: −2^(w−1) for a signed type of width `w`, `0` for an unsigned type |
| `<X>_MAX` | `X` | the largest value: 2^(w−1) − 1 signed, 2^w − 1 unsigned |
| `<X>_BITS` | `U32` | the width `w` |

So `I32_MIN`, `U8_MAX` and `USIZE_BITS` exist. The width of `ISize` and `USize` is the pointer width of the target ([targets](../targets/targets-and-profiles.md)), so `USIZE_MAX` and `USIZE_BITS` are target-dependent. Every other item of this section is target-independent unless it names `ISize` or `USize`.

For `F` in `F32`, `F64`, with `<F>` its name in capitals:

| Constant | Value |
|---|---|
| `<F>_INFINITY`, `<F>_NEG_INFINITY` | positive and negative infinity |
| `<F>_NAN` | the canonical quiet NaN: sign 0, quiet bit 1, all other payload bits 0 (`0x7FC0_0000` and `0x7FF8_0000_0000_0000`) |
| `<F>_MIN`, `<F>_MAX` | the most negative and the most positive finite value |
| `<F>_MIN_POSITIVE` | the smallest positive normal value |
| `<F>_EPSILON` | the difference between `1.0` and the next larger value |
| `<F>_PI`, `<F>_E` | π and e rounded to nearest |

```sai
pub trait Integer: Copy + Ord + Hash + Default {}

pub trait Float: Copy + PartialOrd + Default {}
```

`Integer` is implemented for the ten integer types and `Float` for `F32` and `F64`. Both are sealed (§16.3.5). They have no methods. They are the bounds of the generic conversion methods below.

### 16.8.2 Integer methods

The methods are declared once for each integer type `X`. `S` marks a method that exists only for the signed types, `U` one that exists only for the unsigned types, and `XU` is the unsigned type of the same width as `X` (`U32` for `I32`, `USize` for `ISize`). `W` is the byte width of `X`. `X_MIN`, `X_MAX` and `X_BITS` stand for the constants `<X>_MIN`, `<X>_MAX` and `<X>_BITS` of §16.8.1, for example `I32_MIN`; there are no associated constants (D-TYPE-08). Every method has an empty effect set.

| Signature | Only | Panics | Result |
|---|---|---|---|
| `fn checked_add(self: Self, rhs: Self) -> Option<Self>`, `checked_sub`, `checked_mul` | | — | the exact result, or `None` if it is out of range |
| `fn checked_div(self: Self, rhs: Self) -> Option<Self>`, `checked_rem` | | — | `None` if `rhs == 0` or if the result is out of range (`X_MIN / -1`) |
| `fn checked_neg(self: Self) -> Option<Self>` | | — | `None` if out of range (`X_MIN`; every nonzero unsigned value) |
| `fn checked_shl(self: Self, n: U32) -> Option<Self>`, `checked_shr` | | — | `None` if `n >= X_BITS`, else the shift of 06 §6.8 |
| `fn wrapping_add(self: Self, rhs: Self) -> Self`, `wrapping_sub`, `wrapping_mul` | | — | the result modulo 2^w, as `+%`, `-%`, `*%` (06) |
| `fn wrapping_div(self: Self, rhs: Self) -> Self`, `wrapping_rem` | | `DivByZero`, `RemByZero` if `rhs == 0` | as `/` and `%`, except `X_MIN / -1` gives `X_MIN` and `X_MIN % -1` gives `0` |
| `fn wrapping_neg(self: Self) -> Self` | | — | `0 -% self` |
| `fn wrapping_shl(self: Self, n: U32) -> Self`, `wrapping_shr` | | — | the shift by `n % X_BITS` |
| `fn saturating_add(self: Self, rhs: Self) -> Self`, `saturating_sub`, `saturating_mul` | | — | the exact result clamped to `X_MIN ..= X_MAX` |
| `fn overflowing_add(self: Self, rhs: Self) -> (Self, Bool)`, `overflowing_sub`, `overflowing_mul` | | — | the wrapping result and whether the exact result was out of range |
| `fn pow(self: Self, exp: U32) -> Self` | | `Overflow(Mul)` if out of range | `self` to the power `exp`; `x.pow(0)` is `1` |
| `fn checked_pow(self: Self, exp: U32) -> Option<Self>`, `wrapping_pow`, `saturating_pow` (the last two return `Self`) | | — | as the arithmetic forms above |
| `fn div_euclid(self: Self, rhs: Self) -> Self`, `rem_euclid` | | `DivByZero`, `RemByZero` if `rhs == 0`; `Overflow(Div)` for `X_MIN.div_euclid(-1)` | Euclidean quotient `q` and remainder `r` with `self == q * rhs + r` and `0 <= r < abs(rhs)` |
| `fn abs(self: Self) -> Self` | S | `Overflow(Neg)` for `X_MIN` | the absolute value |
| `fn checked_abs(self: Self) -> Option<Self>`, `wrapping_abs` (returns `Self`) | S | — | as above, without panics |
| `fn unsigned_abs(self: Self) -> XU` | S | — | the exact absolute value |
| `fn abs_diff(self: Self, other: Self) -> XU` | | — | the exact distance |
| `fn signum(self: Self) -> Self` | S | — | `-1`, `0` or `1` |
| `fn is_negative(self: Self) -> Bool`, `is_positive` | S | — | `self < 0`, `self > 0` |
| `fn count_ones(self: Self) -> U32`, `count_zeros`, `leading_zeros`, `trailing_zeros` | | — | bit counts of the two's-complement representation |
| `fn rotate_left(self: Self, n: U32) -> Self`, `rotate_right` | | — | rotation by `n % X_BITS` |
| `fn swap_bytes(self: Self) -> Self`, `reverse_bits` | | — | byte or bit order reversed |
| `fn is_power_of_two(self: Self) -> Bool` | U | — | exactly one bit set |
| `fn checked_next_power_of_two(self: Self) -> Option<Self>` | U | — | the smallest power of two `>= self` (`1` for `0`), or `None` if out of range |
| `fn to_le_bytes(self: Self) -> Array<Byte, W>`, `to_be_bytes` | | — | the representation, least or most significant byte first |
| `fn from_le_bytes(bytes: Array<Byte, W>) -> Self`, `from_be_bytes` | | — | the inverse |
| `fn wrapping_to<T: Integer>(self: Self) -> T` | | — | the value modulo 2^w of `T`, in `T`'s range (sign or zero extension, or truncation, of the two's-complement representation) |
| `fn saturating_to<T: Integer>(self: Self) -> T` | | — | the value clamped to `T`'s range |
| `fn to_f32(self: Self) -> F32`, `to_f64` (returns `F64`) | | — | the value rounded to nearest, ties to even |
| `fn parse(s: Str) -> Result<Self, ParseIntError>` | | — | §16.8.3 |
| `fn parse_radix(s: Str, radix: U32) -> Result<Self, ParseIntError> requires radix >= 2 and radix <= 36` | | `Contract` if the precondition fails | §16.8.3 |

The comparison methods (`min`, `max`, `clamp`, `cmp`) come from `Ord` (§16.5.4). Conversions that cannot lose information are `From` impls, and checked ones are `TryFrom` impls (§16.8.5).

### 16.8.3 Parsing integers

`X.parse(s)` accepts exactly: an optional sign (`+`, or `-` for signed types only), followed by one or more ASCII decimal digits. Leading zeros are allowed. Whitespace, `_` separators and radix prefixes are not. `X.parse_radix(s, radix)` accepts the same with digits `0`–`9` and the ASCII letters `a`–`z` and `A`–`Z` for the values 10 to 35, each smaller than `radix`.

```sai
pub enum ParseIntError {
    Empty
    InvalidDigit
    PosOverflow
    NegOverflow
}
```

The error is `Empty` for `""` and for a lone sign, `InvalidDigit` for any other character outside the grammar (including `-` for an unsigned type), and `PosOverflow` or `NegOverflow` if the value is out of range. The whole string is checked against the grammar before the value is computed, so `InvalidDigit` takes precedence: `"9999999999x"` for `I32` is `InvalidDigit`, and `"9999999999"` is `PosOverflow`. `ParseIntError` has `@derive(Copy, Clone, PartialEq, Eq, Hash)` and `Format` (§16.14.6).

### 16.8.4 Float methods

The methods are declared once for each float type `F` (`F32`, `F64`), with `FB` the unsigned type of the same width. Every method has an empty effect set. Results are those of IEEE 754-2019 with round to nearest, ties to even. Where a result is a NaN, its bit pattern is unspecified (01 §1.5.3), except where the table says otherwise.

| Signature | Panics | Result |
|---|---|---|
| `fn abs(self: Self) -> Self` | — | `self` with the sign bit cleared (also for NaN) |
| `fn copysign(self: Self, sign: Self) -> Self` | — | `self` with the sign bit of `sign` |
| `fn signum(self: Self) -> Self` | — | `1.0` for positive values and `+0.0`, `-1.0` for negative values and `-0.0`, NaN for NaN |
| `fn floor(self: Self) -> Self`, `ceil`, `trunc` | — | rounded toward −∞, +∞, 0 |
| `fn round(self: Self) -> Self` | — | rounded to nearest, halfway cases away from zero |
| `fn round_ties_even(self: Self) -> Self` | — | rounded to nearest, halfway cases to even |
| `fn fract(self: Self) -> Self` | — | `self - self.trunc()` |
| `fn sqrt(self: Self) -> Self` | — | the correctly rounded square root; NaN for negative values other than `-0.0` |
| `fn min(self: Self, other: Self) -> Self`, `max` | — | the Wasm `fmin` / `fmax` result: NaN if either operand is NaN, and `-0.0 < +0.0` |
| `fn clamp(self: Self, lo: Self, hi: Self) -> Self` | `ExplicitPanic`, message `clamp: invalid bounds`, unless `lo <= hi` | `self.max(lo).min(hi)` |
| `fn rem(self: Self, rhs: Self) -> Self` | — | the exact remainder `self - n * rhs` with `n` the quotient truncated toward zero (C `fmod`); the result has the sign of `self` |
| `fn is_nan(self: Self) -> Bool`, `is_infinite`, `is_finite`, `is_normal`, `is_subnormal` | — | the IEEE classification |
| `fn is_sign_negative(self: Self) -> Bool`, `is_sign_positive` | — | the sign bit, also for zeros and NaN |
| `fn to_bits(self: Self) -> FB` | — | the IEEE bit pattern |
| `fn from_bits(bits: FB) -> Self` | — | the float with that bit pattern, NaN payloads preserved |
| `fn total_cmp(self: borrow Self, other: borrow Self) -> Ordering` | — | the IEEE `totalOrder` predicate: `-NaN < -∞ < … < -0.0 < +0.0 < … < +∞ < +NaN`, with NaNs ordered by payload |
| `fn trunc_to<T: Integer>(self: Self) -> T` | — | `self` truncated toward zero and saturated to `T`'s range; NaN gives `0` (N-J-16) |
| `fn checked_trunc_to<T: Integer>(self: Self) -> Option<T>` | — | `Some` of the truncated value if it is in `T`'s range, else `None`; `None` for NaN and infinities |
| `fn to_f32(self: Self) -> F32` (on `F64`) | — | rounded to nearest, ties to even |
| `fn to_f64(self: Self) -> F64` (on `F32`) | — | the exact value |
| `fn parse(s: Str) -> Result<Self, ParseFloatError>` | — | see below |

- **No transcendental functions (N-J-17).** `sai:core` has no `sin`, `exp`, `ln`, `powf` and similar functions, and no fused multiply-add. Wasm provides none of them, and a software implementation whose results differ by one unit in the last place between versions or targets would break the determinism principle (P8, 01 §1.5.3). Every float function of this chapter is either an IEEE basic operation or exactly specified.
- `F.parse(s)` accepts an optional sign followed by either a decimal literal (`digits`, `digits.`, `.digits` or `digits.digits`, then an optional exponent `e` or `E` with an optional sign and one or more digits) or one of `inf`, `infinity`, `nan` in any ASCII case. No whitespace or `_` is accepted. The result is the correctly rounded value of the decimal number, which may be an infinity or zero. `nan` gives `<F>_NAN`. `ParseFloatError` is `enum ParseFloatError { Empty  Invalid }` (one variant per line in the source) with the derives of `ParseIntError`.
- Floats implement `PartialEq`, `PartialOrd` (§16.5.5), `Default`, `Format`, `Arbitrary` (§16.17) and the operator traits of §16.6.1. They do not implement `Eq`, `Ord` or `Hash`.

### 16.8.5 Lossless and checked conversions

`From` impls exist exactly for the conversions that are lossless on every target. `USize` and `ISize` have at least 32 bits on every target and at most 64 ([targets](../targets/targets-and-profiles.md)).

| Target type | `From` source types |
|---|---|
| `I16` | `I8`, `U8` |
| `I32` | `I8`, `I16`, `U8`, `U16` |
| `I64` | `I8`, `I16`, `I32`, `ISize`, `U8`, `U16`, `U32` |
| `ISize` | `I8`, `I16`, `I32`, `U8`, `U16` |
| `U16` | `U8`, `Byte` |
| `U32` | `U8`, `U16`, `Char`, `Byte` |
| `U64` | `U8`, `U16`, `U32`, `USize`, `Byte` |
| `USize` | `U8`, `U16`, `U32`, `Byte` |
| `U8` | `Byte` |
| `Byte` | `U8` |
| `F32` | `I8`, `I16`, `U8`, `U16` |
| `F64` | `F32`, `I8`, `I16`, `I32`, `U8`, `U16`, `U32` |

- `impl TryFrom<A> for B` exists for every ordered pair `(A, B)` of distinct integer types that has no `From` impl. It returns `Some` exactly when the value is in `B`'s range. The `ISize` and `USize` impls give target-dependent results only through that range.
- There are no conversions between `Bool` and the numeric types (write `if b { 1 } else { 0 }`), between `Byte` and `Char` other than the ASCII methods of §16.9.1, or from integers to `Char` other than `Char.from_u32` (§16.9.1).
- Every `From.from` and `TryFrom.try_from` impl of this section has an empty effect set and never panics.
- Narrowing without a check uses `wrapping_to` or `saturating_to` (§16.8.2), and float to integer uses `trunc_to` (§16.8.4), as D-TYPE-06 requires.

```sai
fn checked_index(i: I64) -> Option<USize> {
    USize.try_from(i)
}

fn mean(xs: Slice<I32>) -> F64 {
    var sum = 0.0
    for x in xs.iter() {
        sum = sum + F64.from(*x)
    }
    sum / F64.from(U32.try_from(xs.len()).unwrap())
}
```

## 16.9 Characters and text

### 16.9.1 `Bool`, `Byte` and `Char`: `core.char`

Every method below has an empty effect set and never panics unless the table says so. The classification methods are ASCII-only, except `is_whitespace`, so that no Unicode database is part of `sai:core` (P7). `is_whitespace` and the whitespace-based methods of `Str` use the Unicode `White_Space` property, which is the fixed set U+0009–U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000–U+200A, U+2028, U+2029, U+202F, U+205F and U+3000.

| Signature | Types | Result |
|---|---|---|
| `fn then<T, F: FnOnce() -> T>(self: Self, f: F) -> Option<T>` | `Bool` | `Some(f())` if `self`, else `None` without calling `f` |
| `fn then_some<T>(self: Self, value: T) -> Option<T>` | `Bool` | `Some(value)` if `self`, else `None` |
| `fn is_ascii(self: Self) -> Bool` | `Byte`, `Char` | the value is below 0x80 |
| `fn is_ascii_alphabetic(self: Self) -> Bool`, `is_ascii_uppercase`, `is_ascii_lowercase`, `is_ascii_digit`, `is_ascii_hexdigit`, `is_ascii_alphanumeric`, `is_ascii_punctuation`, `is_ascii_graphic`, `is_ascii_control` | `Byte`, `Char` | the ASCII class: `A`–`Z` and `a`–`z`; `A`–`Z`; `a`–`z`; `0`–`9`; `0`–`9`, `a`–`f`, `A`–`F`; letters and digits; the 32 punctuation characters 0x21–0x2F, 0x3A–0x40, 0x5B–0x60, 0x7B–0x7E; 0x21–0x7E; 0x00–0x1F and 0x7F |
| `fn is_ascii_whitespace(self: Self) -> Bool` | `Byte`, `Char` | one of 0x09, 0x0A, 0x0C, 0x0D, 0x20 |
| `fn to_ascii_uppercase(self: Self) -> Self`, `to_ascii_lowercase` | `Byte`, `Char` | ASCII letters mapped; every other value unchanged |
| `fn eq_ignore_ascii_case(self: Self, other: Self) -> Bool` | `Byte`, `Char` | equality after `to_ascii_lowercase` |
| `fn to_ascii_char(self: Self) -> Option<Char>` | `Byte` | `Some` if `self.is_ascii()` |
| `fn to_ascii_byte(self: Self) -> Option<Byte>` | `Char` | `Some` if `self.is_ascii()` |
| `fn is_whitespace(self: Self) -> Bool` | `Char` | membership in `White_Space` |
| `fn to_digit(self: Self, radix: U32) -> Option<U32> requires radix >= 2 and radix <= 36` | `Char` | the digit value, using `0`–`9`, `a`–`z`, `A`–`Z`, if it is below `radix` |
| `fn from_digit(digit: U32, radix: U32) -> Option<Char> requires radix >= 2 and radix <= 36` | `Char` | the lowercase digit character, if `digit < radix` |
| `fn from_u32(value: U32) -> Option<Char>` | `Char` | `Some` if `value` is a Unicode scalar value (at most 0x10FFFF and not a surrogate) |
| `fn len_utf8(self: Self) -> USize` | `Char` | 1 to 4 |
| `fn encode_utf8(self: Self, buf: borrow mut Array<Byte, 4>) -> Str` | `Char` | writes the UTF-8 encoding at the start of `buf` and returns it as a `Str` borrowing `buf` |
| `fn parse(s: Str) -> Result<Char, ParseCharError>` | `Char` | `Ok(c)` if `s` is exactly one character `c` |

`core.char` also declares `pub const CHAR_MAX: Char = '\u{10FFFF}'` and `pub const REPLACEMENT_CHARACTER: Char = '\u{FFFD}'`, and the enum `ParseCharError` with the variants `Empty` and `TooManyChars`, with the derives of `ParseIntError`. `to_digit` and `from_digit` panic with kind `Contract(id)` when the precondition fails (§16.1.2 item 6).

`Bool`, `Byte` and `Char` implement `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` (§16.5.5, §16.5.6), `Default` (§16.5.3), `Format` (§16.14.6) and `Arbitrary` (§16.17). `Bool` has no bit operators and no operator traits (06 §6.8).

### 16.9.2 `Str`: `core.str`

`Str` is the built-in borrowed UTF-8 view (D-TYPE-02, 05 §5.4.3). It is `Copy`. Its region rules are those of 07. Every method has an empty effect set. All indices and lengths are in bytes. In the signatures, a method whose result is a part of `self` while another parameter is also a view declares `borrows { self }`, so the result does not borrow the other argument (07 §7.7.2).

| Signature | Panics | Result |
|---|---|---|
| `fn len(self: Self) -> USize` | — | the length in bytes |
| `fn is_empty(self: Self) -> Bool` | — | `self.len() == 0` |
| `fn as_bytes(self: Self) -> Slice<Byte>` | — | the bytes |
| `fn as_ptr(self: Self) -> Ptr<Byte>` | — | the address of the first byte; for an empty `Str`, a non-null address that MUST NOT be read |
| `fn chars(self: Self) -> Chars` | — | an iterator over the characters |
| `fn char_indices(self: Self) -> CharIndices` | — | an iterator over `(USize, Char)`: each character with its byte offset |
| `fn bytes(self: Self) -> ByteIter` | — | an iterator over the bytes as `Byte` values |
| `fn split(self: Self, sep: Str) -> Split requires not sep.is_empty()` | `Contract(id)` if `sep` is empty | an iterator over the parts between non-overlapping occurrences of `sep`, found left to right. `"a,b,".split(",")` yields `"a"`, `"b"`, `""`. The iterator borrows both `self` and `sep` |
| `fn split_once(self: Self, sep: Str) -> Option<(Str, Str)> borrows { self }` | — | the parts before and after the first occurrence of `sep` |
| `fn split_whitespace(self: Self) -> SplitWhitespace` | — | the non-empty parts separated by `White_Space` characters |
| `fn lines(self: Self) -> Lines` | — | the parts separated by `"\n"`, each with one trailing `"\r"` removed; a final empty part is not produced |
| `fn trim(self: Self) -> Str`, `trim_start`, `trim_end` | — | `self` without leading and/or trailing `White_Space` characters |
| `fn starts_with(self: Self, prefix: Str) -> Bool`, `ends_with`, `contains` | — | the byte-wise test; the empty string is a prefix, suffix and part of every string |
| `fn find(self: Self, needle: Str) -> Option<USize>`, `rfind` | — | the byte offset of the first or last occurrence; an empty `needle` is found at `0` or `self.len()` |
| `fn strip_prefix(self: Self, prefix: Str) -> Option<Str> borrows { self }`, `strip_suffix` | — | the rest, if `self` starts or ends with it |
| `fn slice<R: SliceRange>(self: Self, range: R) -> Str` | `BoundsCheck` if `range.bounds(self.len())` is `None` or a bound is not a char boundary | the subview (D-EXPR-10) |
| `fn get<R: SliceRange>(self: Self, range: R) -> Option<Str>` | — | `Some` of the subview where `slice` would not panic |
| `fn is_char_boundary(self: Self, index: USize) -> Bool` | — | `true` for `0`, for `self.len()` and for the offset of every character; `false` above `self.len()` |
| `fn split_at(self: Self, mid: USize) -> (Str, Str)` | as `slice` | `(self.slice(..mid), self.slice(mid..))` |
| `fn eq_ignore_ascii_case(self: Self, other: Str) -> Bool` | — | byte-wise equality after ASCII lowercasing |
| `fn from_utf8(bytes: Slice<Byte>) -> Result<Str, Utf8Error>` | — | `Ok` if `bytes` is valid UTF-8 (RFC 3629: no surrogates, no overlong forms, nothing above U+10FFFF) |
| `unsafe fn from_utf8_unchecked(bytes: Slice<Byte>) -> Str` | — | `bytes` viewed as a `Str` |

*Safety* of `from_utf8_unchecked`: `bytes` MUST be valid UTF-8. Every `Str` is valid UTF-8, and safe code relies on it.

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub type Utf8Error {
    pub valid_up_to: USize
}
```

`valid_up_to` is the length of the longest valid prefix. The iterator types `Chars`, `CharIndices`, `ByteIter`, `Split`, `SplitWhitespace` and `Lines` are records with private fields and the implicit region of the `Str` they read (07 §7.8.2). Each implements `Iterator`, `IntoIterator` (§16.11.1) and `Clone`. Their items are `Char`, `(USize, Char)`, `Byte` and `Str`. `Chars`, `CharIndices` and `ByteIter` also implement `DoubleEndedIterator`.

`Str` implements the comparison traits and `Hash` (§16.5.5, §16.5.6), `Default`, `Format` and the `Index` impls of §16.6.2. The allocating methods of `Str` are in §16.15.4.

```sai
fn key_value(line: Str) -> Option<(Str, Str)> {
    let (k, v) = line.split_once("=")?
    Some((k.trim(), v.trim()))
}
```

## 16.10 Slices and arrays

### 16.10.1 `Slice<T>` and `SliceMut<T>`: `core.slice`

`Slice<T>` is the shared view and `SliceMut<T>` the exclusive view of a contiguous sequence (D-TYPE-02, 05 §5.4.3). `Slice<T>` is `Copy`, so its methods take `self: Self`. Built-in indexing `s[i]` is defined in 06 §6.7.1. Every method has an empty declared effect set. Closure and `Clone` effects are latent.

| Signature of `Slice<T>` | Bound | Panics | Result |
|---|---|---|---|
| `fn len(self: Self) -> USize` | | — | the number of elements |
| `fn is_empty(self: Self) -> Bool` | | — | `self.len() == 0` |
| `fn get(self: Self, index: USize) -> Option<borrow T>` | | — | `Some` if `index < self.len()` |
| `fn first(self: Self) -> Option<borrow T>`, `last` | | — | the first or last element |
| `fn iter(self: Self) -> SliceIter<T>` | | — | an iterator over `borrow T` |
| `fn slice<R: SliceRange>(self: Self, range: R) -> Slice<T>` | | `BoundsCheck` if `range.bounds(self.len())` is `None` | the subview |
| `fn get_range<R: SliceRange>(self: Self, range: R) -> Option<Slice<T>>` | | — | the subview, or `None` where `slice` would panic |
| `fn split_at(self: Self, mid: USize) -> (Slice<T>, Slice<T>)` | | `BoundsCheck` if `mid > self.len()` | `(self.slice(..mid), self.slice(mid..))` |
| `fn contains(self: Self, value: borrow T) -> Bool` | `T: PartialEq` | — | some element equals `value` |
| `fn starts_with(self: Self, prefix: Slice<T>) -> Bool`, `ends_with` | `T: PartialEq` | — | element-wise test |
| `fn binary_search(self: Self, value: borrow T) -> Result<USize, USize>` | `T: Ord` | — | on a slice sorted by `Ord`: `Ok(i)` with `i` the lowest index of an element equal to `value`, or `Err(i)` with `i` the index where `value` could be inserted keeping the order |
| `fn binary_search_by<F: FnMut(borrow T) -> Ordering>(self: Self, f: F) -> Result<USize, USize>` | | — | as `binary_search`, with `f(e)` giving the order of `e` relative to the target |
| `fn chunks(self: Self, size: USize) -> Chunks<T> requires size > 0` | | `Contract(id)` | consecutive subviews of `size` elements, the last one possibly shorter |
| `fn windows(self: Self, size: USize) -> Windows<T> requires size > 0` | | `Contract(id)` | every subview of `size` consecutive elements, in order; none if `size > self.len()` |
| `fn as_ptr(self: Self) -> Ptr<T>` | | — | the address of the first element; for an empty slice, a non-null, aligned address that MUST NOT be read |
| `unsafe fn from_raw_parts(ptr: Ptr<T>, len: USize) -> Slice<T>` | | — | a view of `len` elements starting at `ptr` |
| `fn as_bytes(self: Self) -> Slice<Byte>` | `Self = Slice<U8>` | — | the same memory viewed as `Byte` |
| `fn as_u8(self: Self) -> Slice<U8>` | `Self = Slice<Byte>` | — | the same memory viewed as `U8` |

*Safety* of `from_raw_parts`: `ptr` MUST be aligned and non-null, the `len` elements MUST be initialized values of `T` within one allocation, and they MUST NOT be mutated while the result or any view derived from it is used. The result has no input region, so by O4 (07 §7.7.2) it is `'static`. The caller is responsible for not using it after the memory is freed (12).

`SliceMut<T>` has every method of `Slice<T>` above except `from_raw_parts`, `as_bytes` and `as_u8`, with the receiver `self: borrow Self` instead of `self: Self`, so the results borrow the `SliceMut<T>`. It also has:

| Signature of `SliceMut<T>` | Bound | Panics | Result |
|---|---|---|---|
| `fn as_slice(self: borrow Self) -> Slice<T>` | | — | the shared view |
| `fn reborrow(self: borrow mut Self) -> SliceMut<T>` | | — | an exclusive view of the same elements for the region of the borrow (07 §7.5.5) |
| `fn get_mut(self: borrow mut Self, index: USize) -> Option<borrow mut T>` | | — | `Some` if `index < self.len()` |
| `fn first_mut(self: borrow mut Self) -> Option<borrow mut T>`, `last_mut` | | — | the first or last element |
| `fn iter_mut(self: borrow mut Self) -> SliceIterMut<T>` | | — | an iterator over `borrow mut T` |
| `fn slice_mut<R: SliceRange>(self: borrow mut Self, range: R) -> SliceMut<T>` | | as `slice` | the exclusive subview |
| `fn split_at_mut(self: borrow mut Self, mid: USize) -> (SliceMut<T>, SliceMut<T>)` | | `BoundsCheck` if `mid > self.len()` | two disjoint exclusive views |
| `fn swap(self: borrow mut Self, a: USize, b: USize)` | | `BoundsCheck` if `a` or `b` is out of bounds | exchanges two elements |
| `fn reverse(self: borrow mut Self)` | | — | reverses the order in place |
| `fn fill(self: borrow mut Self, value: T)` | `T: Clone` | — | every element replaced by a clone of `value`; the old elements are dropped |
| `fn copy_from_slice(self: borrow mut Self, src: Slice<T>) requires src.len() == self.len()` | `T: Copy` | `Contract(id)` | copies `src` into `self` |
| `fn sort(self: borrow mut Self)` | `T: Ord` | — | sorts in place |
| `fn sort_by<F: FnMut(borrow T, borrow T) -> Ordering>(self: borrow mut Self, compare: F)` | | — | sorts in place by `compare` |
| `fn sort_by_key<K: Ord, F: FnMut(borrow T) -> K>(self: borrow mut Self, key: F)` | | — | sorts in place by `key` |
| `fn as_mut_ptr(self: borrow mut Self) -> PtrMut<T>` | | — | as `as_ptr` |
| `unsafe fn from_raw_parts_mut(ptr: PtrMut<T>, len: USize) -> SliceMut<T>` | | — | as `from_raw_parts`, and no other access to the elements may happen while the result is used |

- **Sorting (N-J-18).** The three sorts are stable, do not allocate, and use O(n log² n) comparisons and element moves in the worst case. For a consistent `Ord` (§16.5.4) a stable sort has exactly one result, so sorting is deterministic and independent of the algorithm. `sort_by_key` calls `key` O(n log² n) times. The sorts are available in every profile, including `tiny`, because they do not allocate.
- `swap` with `a == b` does nothing.

The iterator types are records with private fields and the region of the view they read: `SliceIter<T>` (items `borrow T`; `Clone`), `SliceIterMut<T>` (items `borrow mut T`), `Chunks<T>` and `Windows<T>` (items `Slice<T>`; `Clone`). All four implement `IntoIterator`, `DoubleEndedIterator` and `ExactSizeIterator` (§16.11). `Slice<T>` implements `IntoIterator` with `Item = borrow T`, and `SliceMut<T>` with `Item = borrow mut T`. Both views implement the comparison traits and `Hash` when `T` does (§16.5.5), `Format` when `T` does, and the range `Index` impls of §16.6.2. `Slice<T>` implements `Default` (the empty slice).

### 16.10.2 Arrays: `core.array`

| Signature of `Array<T, N>` | Bound | Effects | Result |
|---|---|---|---|
| `fn len(self: borrow Self) -> USize` | | — | `N` |
| `fn as_slice(self: borrow Self) -> Slice<T>` | | — | a shared view of the elements |
| `fn as_mut_slice(self: borrow mut Self) -> SliceMut<T>` | | — | an exclusive view of the elements |
| `fn iter(self: borrow Self) -> SliceIter<T>` | | — | `self.as_slice().iter()` |
| `fn iter_mut(self: borrow mut Self) -> SliceIterMut<T>` | | — | an iterator over `borrow mut T` |
| `fn filled(value: T) -> Array<T, N>` | `T: Copy` | — | every element a copy of `value`; intrinsic lowered to `Repeat` (06 §6.18.5) |
| `fn from_fn<F: FnMut(USize) -> T>(f: F) -> Array<T, N>` | | — | element `i` is `f(i)`, called for `i` from `0` to `N - 1` in order |
| `fn map<U, F: FnMut(T) -> U>(self: Self, f: F) -> Array<U, N>` | | — | element `i` is `f(self[i])`, in order |

`Array<T, N>` implements `IntoIterator` by value with `ArrayIntoIter<T, N>` (items `T`; `DoubleEndedIterator`, `ExactSizeIterator`, `Clone` when `T: Clone`). Dropping an `ArrayIntoIter` drops the elements not yet produced. `borrow Array<T, N>` and `borrow mut Array<T, N>` implement `IntoIterator` with `SliceIter<T>` and `SliceIterMut<T>`. The compiler-provided impls of arrays are listed in 05 §5.11.4. This chapter adds `Format` (§16.14.6) and the range `Index` impls (§16.6.2).

## 16.11 Iteration: `core.iter`

### 16.11.1 `IntoIterator` and the `for` loop

```sai
pub trait Iterator {
    type Item

    fn next(self: borrow mut Self) -> Option<Self.Item>
}

pub trait IntoIterator {
    type Item
    type IntoIter: Iterator<Item = Self.Item>

    fn into_iter(self: Self) -> Self.IntoIter
}
```

The `for` loop calls `IntoIterator.into_iter` once and then `Iterator.next` until it returns `None` (06, D-EXPR-11). Both traits are lang items (§16.3.3) and have latent effects (§16.5.1).

**No blanket impl (N-J-19).** There is no `impl<I: Iterator> IntoIterator for I`. The overlap check ignores bounds (05 §5.10.2), so such an impl would overlap every other `IntoIterator` impl, including the one for `Vec<T>`. Instead, every iterator type of this chapter implements `IntoIterator` for itself, with `IntoIter = Self` and `into_iter` returning `self`, and a program does the same for its own iterators:

```sai
type Countdown {
    n: U32
}

impl Iterator for Countdown {
    type Item = U32

    fn next(self: borrow mut Self) -> Option<U32> {
        if self.n == 0 {
            None
        } else {
            self.n = self.n - 1
            Some(self.n + 1)
        }
    }
}

impl IntoIterator for Countdown {
    type Item = U32
    type IntoIter = Countdown

    fn into_iter(self: Self) -> Countdown {
        self
    }
}

fn total() -> U32 {
    var sum = 0
    for k in Countdown(n: 3) {
        sum = sum + k
    }
    sum
}
```

A `for` loop over a type with an `Iterator` impl and no `IntoIterator` impl is `NOT_ITERABLE` (06), with a note that names the missing impl.

The `IntoIterator` impls of this chapter are:

| Type | `Item` | `IntoIter` |
|---|---|---|
| every iterator type of this chapter | its item | itself |
| `Vec<T>` | `T` | `VecIntoIter<T>` (§16.15.3) |
| `borrow Vec<T>` | `borrow T` | `SliceIter<T>` |
| `borrow mut Vec<T>` | `borrow mut T` | `SliceIterMut<T>` |
| `Slice<T>` | `borrow T` | `SliceIter<T>` |
| `SliceMut<T>` | `borrow mut T` | `SliceIterMut<T>` |
| `Array<T, N>` | `T` | `ArrayIntoIter<T, N>` |
| `borrow Array<T, N>` | `borrow T` | `SliceIter<T>` |
| `borrow mut Array<T, N>` | `borrow mut T` | `SliceIterMut<T>` |
| `Range<T>`, `RangeInclusive<T>`, `RangeFrom<T>` with `T: Step` | `T` | itself |

The regions of `borrow T` in these impls are the single region of the target type (N-B-12, 07 §7.7.5). `String`, `Str` and `Option<T>` are not iterable; use `chars()`, `bytes()` or `iter()`.

### 16.11.2 Iterator methods

`Iterator` has the provided methods below, declared without effect clauses, so their effects are latent (08 §8.6.3): they are the effects of `Self.next` and of the closures passed in. Each adapter is lazy: it calls `next` on the inner iterator, and the closures, only when its own `next` is called, in the order of the items, and exactly as often as the *Result* column implies. Every iterator type of this chapter is **fused**: after `next` has returned `None`, every further call returns `None` without calling a closure or an inner `next`. The only exception is `from_fn`, which calls its closure on every `next`.

| Signature | Result |
|---|---|
| `fn count(self: Self) -> USize` | the number of items; panics with `Overflow(Add)` if it exceeds `USIZE_MAX` |
| `fn last(self: Self) -> Option<Self.Item>` | the last item |
| `fn nth(self: borrow mut Self, n: USize) -> Option<Self.Item>` | discards `n` items and returns the next one |
| `fn step_by(self: Self, step: USize) -> StepBy<Self> requires step > 0` | the first item, then every `step`-th item |
| `fn chain<U: IntoIterator<Item = Self.Item>>(self: Self, other: U) -> Chain<Self, U.IntoIter>` | the items of `self`, then those of `other` |
| `fn zip<U: IntoIterator>(self: Self, other: U) -> Zip<Self, U.IntoIter>` | pairs `(a, b)`, until either side ends; `self` is advanced first |
| `fn map<B, F: FnMut(Self.Item) -> B>(self: Self, f: F) -> Map<Self, F>` | `f(x)` for each item |
| `fn filter<P: FnMut(borrow Self.Item) -> Bool>(self: Self, predicate: P) -> Filter<Self, P>` | the items for which `predicate` holds |
| `fn filter_map<B, F: FnMut(Self.Item) -> Option<B>>(self: Self, f: F) -> FilterMap<Self, F>` | the `Some` payloads of `f(x)` |
| `fn enumerate(self: Self) -> Enumerate<Self>` | pairs `(i, x)` with `i` counting from `0`; panics with `Overflow(Add)` past `USIZE_MAX` |
| `fn peekable(self: Self) -> Peekable<Self>` | an iterator with `fn peek(self: borrow mut Self) -> Option<borrow Self.Item>` |
| `fn skip(self: Self, n: USize) -> Skip<Self>`, `take` | without the first `n` items; only the first `n` items |
| `fn skip_while<P: FnMut(borrow Self.Item) -> Bool>(self: Self, predicate: P) -> SkipWhile<Self, P>`, `take_while` | without the longest prefix satisfying `predicate`; only that prefix |
| `fn flat_map<U: IntoIterator, F: FnMut(Self.Item) -> U>(self: Self, f: F) -> FlatMap<Self, U, F>` | the items of each `f(x)` in turn |
| `fn flatten(self: Self) -> Flatten<Self> where Self.Item: IntoIterator` | the items of each item in turn |
| `fn inspect<F: FnMut(borrow Self.Item)>(self: Self, f: F) -> Inspect<Self, F>` | each item, after calling `f` on it |
| `fn fold<B, F: FnMut(B, Self.Item) -> B>(self: Self, init: B, f: F) -> B` | the left fold |
| `fn for_each<F: FnMut(Self.Item)>(self: Self, f: F)` | calls `f` on each item |
| `fn all<P: FnMut(Self.Item) -> Bool>(self: borrow mut Self, predicate: P) -> Bool`, `any` | short-circuiting conjunction and disjunction; `true` and `false` when empty |
| `fn find<P: FnMut(borrow Self.Item) -> Bool>(self: borrow mut Self, predicate: P) -> Option<Self.Item>` | the first item satisfying `predicate` |
| `fn find_map<B, F: FnMut(Self.Item) -> Option<B>>(self: borrow mut Self, f: F) -> Option<B>` | the first `Some` result |
| `fn position<P: FnMut(Self.Item) -> Bool>(self: borrow mut Self, predicate: P) -> Option<USize>` | the index of the first item satisfying `predicate` |
| `fn min(self: Self) -> Option<Self.Item> where Self.Item: Ord`, `max` | the first minimum; the last maximum |
| `fn min_by_key<K: Ord, F: FnMut(borrow Self.Item) -> K>(self: Self, key: F) -> Option<Self.Item>`, `max_by_key` | as `min` and `max`, comparing `key(x)` |
| `fn min_by<F: FnMut(borrow Self.Item, borrow Self.Item) -> Ordering>(self: Self, compare: F) -> Option<Self.Item>`, `max_by` | as `min` and `max`, comparing with `compare` |
| `fn sum<S: Sum<Self.Item>>(self: Self) -> S`, `product` with `Product` | `S.sum(self)` |
| `fn collect<C: FromIterator<Self.Item>>(self: Self) -> C` | `C.from_iter(self)` |
| `fn rev(self: Self) -> Rev<Self> where Self: DoubleEndedIterator` | the items in reverse order |
| `fn copied<T: Copy>(self: Self) -> Copied<Self> where Self: Iterator<Item = borrow T>`, `cloned` with `T: Clone` | copies or clones of the referents |

### 16.11.3 Further iterator traits and constructors

```sai
pub trait DoubleEndedIterator: Iterator {
    fn next_back(self: borrow mut Self) -> Option<Self.Item>
}

pub trait ExactSizeIterator: Iterator {
    fn len(self: borrow Self) -> USize
        effects {}
}

pub trait FromIterator<A> {
    fn from_iter<I: IntoIterator<Item = A>>(iter: I) -> Self
}

pub trait Sum<A> {
    fn sum<I: Iterator<Item = A>>(iter: I) -> Self
}

pub trait Product<A> {
    fn product<I: Iterator<Item = A>>(iter: I) -> Self
}
```

- `next_back` produces items from the back. `next` and `next_back` on the same iterator meet in the middle and never produce an item twice.
- `len` is the exact number of remaining items. An `ExactSizeIterator` impl whose `len` is wrong is a logic error: library code never relies on it for memory safety, only for capacity reservation.
- `Sum<A>` and `Product<A>` are implemented for every integer and float type with `A = Self`. Integer sums and products panic with `Overflow(Add)` and `Overflow(Mul)` on overflow, in every profile. The sum of no items is `0` and the product is `1`. Float sums add left to right. Iterators over borrows use `copied()` first: `xs.iter().copied().sum()`. An impl `Sum<borrow I32> for I32` would bind the region of `borrow I32` to `'static` (N-B-12) and is therefore not provided.
- `FromIterator` impls: `Vec<T>` from `T` and `String` from `Char` (§16.15), and the short-circuiting impls `impl<A, C: FromIterator<A>> FromIterator<Option<A>> for Option<C>` and `impl<A, E, C: FromIterator<A>> FromIterator<Result<A, E>> for Result<C, E>`. The last two stop at the first `None` or `Err`, return it, and drop the partially built `C`.

The adapter types `StepBy`, `Chain`, `Zip`, `Map`, `Filter`, `FilterMap`, `Enumerate`, `Peekable`, `Skip`, `Take`, `SkipWhile`, `TakeWhile`, `FlatMap`, `Flatten`, `Inspect`, `Rev`, `Copied` and `Cloned` are records of `core.iter` with private fields. Each implements `Iterator` and `IntoIterator`, and `Clone` when its fields do. `Rev`, `Map`, `Filter`, `FilterMap`, `Chain`, `Inspect`, `Copied`, `Cloned`, `Skip` and `Take` implement `DoubleEndedIterator` when their inner iterators do (for `Skip` and `Take`, also `ExactSizeIterator`). `Map`, `Enumerate`, `Rev`, `Copied`, `Cloned`, `Skip` and `Take` implement `ExactSizeIterator` when their inner iterator does.

| Signature | Result |
|---|---|
| `fn empty<T>() -> Empty<T>` | no items |
| `fn once<T>(value: T) -> Once<T>` | exactly `value` |
| `fn repeat<T: Clone>(value: T) -> Repeat<T>` | clones of `value`, forever |
| `fn from_fn<T, F: FnMut() -> Option<T>>(f: F) -> FromFn<F>` | the results of `f()` until the first `None`; `f` is called on every `next`, also after `None` |

```sai
use sai:core/iter.{from_fn}

fn squares_below(limit: U32) -> Vec<U32>
    effects { alloc }
{
    var i = 0
    from_fn(fn() {
        i = i + 1
        if i * i < limit {
            Some(i * i)
        } else {
            None
        }
    }).collect()
}
```

## 16.12 Memory, pointers and C interop

The rules for `unsafe` code, raw-pointer dereference and FFI belong to [12](12-unsafe-and-ffi.md). This section lists the library surface. Every function has an empty declared effect set unless the table says otherwise.

### 16.12.1 `core.mem`

| Signature | Panics | Result |
|---|---|---|
| `fn size_of<T>() -> USize` | — | the size of `T` in bytes ([impl/06](../impl/06-wasm-lowering-and-layout.md)); intrinsic |
| `fn align_of<T>() -> USize` | — | the alignment of `T`; intrinsic |
| `fn swap<T>(a: borrow mut T, b: borrow mut T)` | — | exchanges the two values |
| `fn replace<T>(dest: borrow mut T, value: T) -> T` | — | stores `value` and returns the old value |
| `fn take<T: Default>(dest: borrow mut T) -> T` | — | `replace(dest, T.default())`; the effects of `T.default` are latent |
| `fn drop<T>(value: T)` | — | drops `value` now (07 §7.13, 05 §5.9.3) |
| `fn forget<T>(value: T)` | — | consumes `value` without running its drop glue; its resources are leaked. Safe; intrinsic |
| `unsafe fn transmute<A, B>(value: A) -> B` | — | the bits of `value` reinterpreted as a `B`; intrinsic. The sizes are compared on the generic signature, and a size that depends on a type parameter or differs for the target is the error `TRANSMUTE_SIZE_MISMATCH` (12 §12.5.2) |
| `unsafe fn zeroed<T>() -> T` | — | a value whose bytes are all zero; intrinsic |

*Safety*: `transmute` requires the bits of `value` to be a valid value of `B`. `zeroed` requires all-zero bytes to be a valid value of `T`; the types for which this holds are listed in 12 §12.5.3 item 1.

`MaybeUninit<T>` is a lang-item type (§16.3.2): storage for a `T` with no validity requirement, whose bytes may be uninitialized. Its size and alignment equal those of `T` (impl/06). It is not `Copy` and has no drop glue: dropping a `MaybeUninit<T>` never drops the `T` it may contain.

| Signature | Result |
|---|---|
| `fn uninit() -> MaybeUninit<T>` | uninitialized storage |
| `fn new(value: T) -> MaybeUninit<T>` | storage holding `value` |
| `fn write(self: borrow mut Self, value: T) -> borrow mut T` | stores `value` without dropping any previous content, and returns a borrow of it |
| `fn as_ptr(self: borrow Self) -> Ptr<T>`, `fn as_mut_ptr(self: borrow mut Self) -> PtrMut<T>` | the address of the storage |
| `unsafe fn assume_init(self: Self) -> T` | the stored value |

*Safety* of `assume_init`: the storage MUST hold a valid `T`.

### 16.12.2 `core.ptr`

`Ptr<T>` and `PtrMut<T>` are raw pointers (D-UNSAFE-01). They are never null: a nullable pointer is `Option<Ptr<T>>`, which has the layout of a single address with `None` as 0 (impl/06). They carry no region and are not checked by the borrow checker. Creating, copying, comparing and converting pointers is safe. Dereferencing (`*p`, 06 §6.9.1) and the functions marked `unsafe` require `unsafe` (12). Reading `*p` by value requires `T: Copy`; otherwise it is `MOVE_OUT_OF_BORROW` (07 §7.17 item 2; 12 §12.4.4), and `p.read()` is the alternative.

Both types are `Copy`, `Clone`, `PartialEq`, `Eq` and `Hash` by address (§16.5.5, §16.5.6), and implement `Format`, which writes the address in lowercase hexadecimal with the prefix `0x`. Addresses are unspecified values (01 §1.5.3), so output that contains them is not deterministic across implementations.

| Signature | Types | Result |
|---|---|---|
| `fn from(r: borrow T) -> Ptr<T>` | `Ptr<T>` | the address of the referent |
| `fn from(r: borrow mut T) -> PtrMut<T>` | `PtrMut<T>` | the address of the referent |
| `fn dangling() -> Self` | both | the address `align_of<T>()`: non-null and aligned, valid only for accesses of size 0 (12 §12.4.2) |
| `fn from_addr(addr: USize) -> Option<Self>` | both | `None` for `0` |
| `fn addr(self: Self) -> USize` | both | the address |
| `fn cast<U>(self: Self) -> Ptr<U>` (on `PtrMut<T>`: `-> PtrMut<U>`) | both | the same address, as a pointer to `U` |
| `fn is_aligned(self: Self) -> Bool` | both | `self.addr() % align_of<T>() == 0` |
| `fn as_const(self: Self) -> Ptr<T>` | `PtrMut<T>` | the same address |
| `fn from_const(p: Ptr<T>) -> PtrMut<T>` | `PtrMut<T>` | the same address; writing through it is valid only where writing through the original is (12) |
| `unsafe fn offset(self: Self, count: ISize) -> Self`; `unsafe fn add(self: Self, count: USize) -> Self`; `unsafe fn sub(self: Self, count: USize) -> Self` | both | the address moved by `count * size_of<T>()` bytes |
| `unsafe fn offset_from(self: Self, origin: Self) -> ISize` | both | the distance in elements |
| `unsafe fn read(self: Self) -> T`, `read_unaligned` | both | a bitwise copy of the pointee; for a non-`Copy` `T` the pointee is logically moved out and must not be used or dropped again |
| `unsafe fn write(self: Self, value: T)`, `write_unaligned` | `PtrMut<T>` | stores `value` without dropping the old pointee |
| `unsafe fn drop_in_place(self: Self)` | `PtrMut<T>` | runs the drop glue of the pointee |

- `Ptr.from` and `PtrMut.from` are inherent functions, not `From` impls. A `From<borrow T>` impl would bind the region of `borrow T` to `'static`, because the pointer types carry no region (N-B-12, 07 §7.7.5).
- *Safety*: `offset`, `add` and `sub` require the result to be within, or one element past the end of, the allocation `self` points into, and the byte distance to fit in an `ISize`. `offset_from` requires both pointers to be in the same allocation and their distance to be a multiple of `size_of<T>()`. `read` and `write` require an aligned pointer to memory valid for reading or writing a `T`, and `read` requires the pointee to be a valid `T`. The unaligned forms drop the alignment requirement. `drop_in_place` requires a valid pointee that is not used afterwards. `Box.into_raw` and `Box.from_raw` are in §16.15.2.

### 16.12.3 `core.ffi`

`core.ffi` re-exports `Ptr` and `PtrMut` (`pub use`, 04), so `use sai:core/ffi.{Ptr}` and `use sai:core/ptr.{Ptr}` name the same item. These re-exports are Stage 2. The items of §16.12.4 are Stage 7.

### 16.12.4 C interop (Stage 7)

These items exist for `unsafe extern "C"` declarations (12). They are Stage 7 (§16.1.6). `unsafe extern "C"` is native-only (D-UNSAFE-01): a reachable foreign call requires the target capability `native_ffi`, which only native targets provide (12 §12.9.2, 08 §8.12.7). The items are nevertheless declared on every target, so that a non-portable package type-checks on a Wasm target on which none of its foreign calls is reachable (12 §12.9.2 item 3).

**C type aliases.** Each is a transparent alias (`pub type CInt = I32`, 05 §5.6.5) of the integer or float type that matches the C type of the target's C ABI. Wasm targets provide no C ABI to SAI code; on them the aliases follow the C data model of `wasm32` (ILP32) and `wasm64` (LP64):

| Alias | C type | Wasm targets (`wasm32`) | Wasm targets (`wasm64`) |
|---|---|---|---|
| `CChar` | `char` | `I8` | `I8` |
| `CSChar`, `CUChar` | `signed char`, `unsigned char` | `I8`, `U8` | `I8`, `U8` |
| `CShort`, `CUShort` | `short`, `unsigned short` | `I16`, `U16` | `I16`, `U16` |
| `CInt`, `CUInt` | `int`, `unsigned int` | `I32`, `U32` | `I32`, `U32` |
| `CLong`, `CULong` | `long`, `unsigned long` | `I32`, `U32` | `I64`, `U64` |
| `CLongLong`, `CULongLong` | `long long`, `unsigned long long` | `I64`, `U64` | `I64`, `U64` |
| `CSize`, `CSSize` | `size_t`, `ssize_t` | `USize`, `ISize` | `USize`, `ISize` |
| `CFloat`, `CDouble` | `float`, `double` | `F32`, `F64` | `F32`, `F64` |

On native targets the aliases follow the target's C ABI as listed in [targets](../targets/targets-and-profiles.md). Only `CChar`, `CLong` and `CULong` differ between targets. Code that uses the aliases instead of fixed-width types is portable across them.

**`CStr`** is a borrowed view of a NUL-terminated byte string with no interior NUL. Like `Str`, it is `Copy` and carries one region (07). Its ABI is `const char*` (12).

| Signature | Effects | Result |
|---|---|---|
| `fn from_bytes_with_nul(bytes: Slice<Byte>) -> Option<CStr>` | — | `Some` if the last byte of `bytes` is NUL and no other byte is |
| `unsafe fn from_ptr(ptr: Ptr<CChar>) -> CStr` | — | the bytes from `ptr` up to and including the first NUL |
| `fn as_ptr(self: Self) -> Ptr<CChar>` | — | the address of the first byte |
| `fn len(self: Self) -> USize` | — | the number of bytes before the NUL |
| `fn to_bytes(self: Self) -> Slice<Byte>`, `to_bytes_with_nul` | — | the bytes without or with the NUL |
| `fn to_str(self: Self) -> Result<Str, Utf8Error>` | — | the bytes without the NUL as a `Str`, if they are UTF-8 |

*Safety* of `from_ptr`: `ptr` MUST point to a NUL-terminated sequence of bytes within one allocation that is not mutated while the result is used. The result has region `'static` by O4 (07 §7.7.2).

`CStr` implements `Copy`, `Clone`, `PartialEq`, `Eq` and `Hash` (by the bytes before the NUL).

**`Out<T>`** is an exclusive view of storage for one `T` that a C function initializes. Its ABI is `T*` (12). It carries the region of the `MaybeUninit<T>` it views and is not `Copy`.

| Signature | Result |
|---|---|
| `fn from(slot: borrow mut MaybeUninit<T>) -> Out<T>` | a view of `slot` |
| `unsafe fn from_ptr(ptr: PtrMut<T>) -> Out<T>` | a view of the storage at `ptr`, which MUST be valid for writing a `T` while the result is used; region `'static` by O4 |
| `fn as_mut_ptr(self: borrow mut Self) -> PtrMut<T>` | the address |
| `fn write(self: Self, value: T)` | stores `value` without dropping any previous content |

`Out.from` is an inherent function, for the reason given for `Ptr.from` (§16.12.2). After the C call, the caller reads the value with `MaybeUninit.assume_init` (`unsafe`).

**`CString`** (`alloc.ffi`) is an owned NUL-terminated byte string with no interior NUL. It owns its buffer and frees it when dropped.

| Signature | Effects | Result |
|---|---|---|
| `fn new(s: Str) -> Option<CString>` | alloc | the bytes of `s` followed by NUL, or `None` if `s` contains a NUL |
| `fn from_bytes(bytes: Slice<Byte>) -> Option<CString>` | alloc | as `new`, for arbitrary bytes |
| `fn as_c_str(self: borrow Self) -> CStr` | — | a view of the string |
| `fn as_ptr(self: borrow Self) -> Ptr<CChar>` | — | `self.as_c_str().as_ptr()` |
| `fn into_raw(self: Self) -> PtrMut<CChar>` | — | releases ownership; the buffer is no longer freed automatically |
| `unsafe fn from_raw(ptr: PtrMut<CChar>) -> CString` | — | takes back ownership of a pointer returned by `into_raw` |

*Safety* of `CString.from_raw`: `ptr` MUST come from `CString.into_raw`, and the bytes before the NUL MUST NOT have changed length. `CString` implements `Clone` (alloc), `PartialEq`, `Eq` and `Hash`.

```sai
use sai:core/ffi.{CInt, CStr, Out, Ptr}
use sai:core/mem.{MaybeUninit}

/// An opaque `sqlite3` connection handle (12 §12.9.5).
type Sqlite {}

unsafe extern "C" module "sqlite3" {
    fn sqlite3_open(path: CStr, db: Out<Ptr<Sqlite>>) -> CInt
}

fn open(path: CStr) -> Option<Ptr<Sqlite>>
    effects { ffi }
{
    var slot = MaybeUninit<Ptr<Sqlite>>.uninit()
    let rc = unsafe { sqlite3_open(path, Out.from(borrow mut slot)) }
    if rc == 0 {
        Some(unsafe { slot.assume_init() })
    } else {
        None
    }
}
```

---

## 16.13 Futures and tasks: `core.future` and `core.task`

The meaning of these items, the poll contract and the executors belong to [11](11-async-and-concurrency.md). This section lists the declarations that 11 fixes, together with the helper items that 11 delegates to this chapter. The items exist from Stage 2 (11 §11.1.2): unsafe code can implement and poll futures before `async` syntax is available at Stage 5.

### 16.13.1 `core.future`

`Future` and `Stream` are declared exactly as in 11 §11.4.1 and §11.6.1. In the library source, `Context` and `Poll` denote the items of `core.task`.

```sai
pub trait Future {
    type Output

    unsafe fn poll(self: borrow mut Self, cx: borrow mut Context) -> Poll<Self.Output>
}

pub trait Stream {
    type Item

    unsafe fn poll_next(self: borrow mut Self, cx: borrow mut Context) -> Poll<Option<Self.Item>>
}
```

1. Both traits are safe to implement and unsafe to call (D-ASYNC-01, 11 §11.4.1). `poll` and `poll_next` have no effect bound. Their effects are latent and are attributed by 08 §8.10.
2. Both are bound by the prelude (§16.4) and are lang items (§16.3.3).
3. The library provides no impl of `Future` or `Stream` for any type of `sai:core` or `sai:alloc` other than the two helper types below and `Box` (`Box<F>: Future` and `Box<S>: Stream` by forwarding, §16.15.2, 11 §11.3.2, N-I-4). In particular, `Option<F>` does not implement `Future`, and there is no impl for `borrow mut F` (11 N-I-4). An executor that stores futures of different types stores them behind its own type-erased wrapper (11 §11.9.4).
4. The two helper types below are declared `@must_use` (D-EXPR-01, 11 §11.3.2, 15 §15.5.1), so a call of `ready` or `pending` whose result is discarded is `UNUSED_MUST_USE` (06). `Box<F>` is not a must-use type (15 §15.5.1).

| Signature | Result |
|---|---|
| `fn ready<T>(value: T) -> ReadyFuture<T>` | a future whose first poll returns `.Ready(value)` |
| `fn pending<T>() -> PendingFuture<T>` | a future that never completes |

`ReadyFuture<T>` and `PendingFuture<T>` are `@must_use` records with private fields.

- `impl<T> Future for ReadyFuture<T>` with `Output = T`. Its first poll returns `.Ready(value)` without using the context. A second poll panics with kind `ExplicitPanic` and the message ``ReadyFuture polled after completion``, which satisfies item 7 of 11 §11.4.2.
- `impl<T> Future for PendingFuture<T>` with `Output = T`. Every poll returns `.Pending` and does not register the waker. This is the one deliberate exception to item 5 of 11 §11.4.2: the future can make no progress, so no wake is owed. A task that awaits it alone is never polled again, and `block_on` reports it as a deadlock (11 §11.9.1). It is useful as an operand of `select` that never wins.
- Neither type is `Copy` or `Clone`. `ReadyFuture<T>` implements `Format` when `T: Format` (§16.14.6). `PendingFuture<T>` implements `Format` for every `T`.

### 16.13.2 `core.task`

`Poll<T>` is declared exactly as in 11 §11.4.1:

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum Poll<T> {
    Ready(T)
    Pending
}
```

The derives follow 05 §5.12: `Poll<T>` is `Copy` when `T: Copy`, and so on. It also implements `Format` when `T: Format` (§16.14.6). Its variants are not in the prelude and are written `Poll.Ready` and `Poll.Pending`, or `.Ready` and `.Pending` where the expected type is known (06).

| Signature | Result |
|---|---|
| `fn is_ready(self: borrow Self) -> Bool` | `true` for `Ready` |
| `fn is_pending(self: borrow Self) -> Bool` | `true` for `Pending` |
| `fn map<U, F: FnOnce(T) -> U>(self: Self, f: F) -> Poll<U>` | `.Ready(f(v))` for `.Ready(v)`, else `.Pending`; the effects of `f` are latent |

`Context`, `Waker`, `RawWaker` and `RawWakerVTable` are declared as in the signature listing of 11 §11.4.3, which this chapter adopts without change. The following table repeats the inherent functions with their effects. None of them panics.

| Signature | Effects | Result |
|---|---|---|
| `fn from_waker(waker: borrow Waker) -> Context` | — | a context with an empty executor extension (11 §11.4.3) |
| `fn waker(self: borrow Self) -> borrow Waker` (on `Context`) | — | the waker of the context |
| `fn wake(self: Self)` (on `Waker`) | — | schedules the task and consumes the waker |
| `fn wake_by_ref(self: borrow Self)` | — | schedules the task |
| `fn will_wake(self: borrow Self, other: borrow Waker) -> Bool` | — | `true` only if both wake the same task |
| `fn noop() -> Waker` | — | a waker whose wake does nothing |
| `unsafe fn from_raw(raw: RawWaker) -> Waker` | — | a waker that calls the functions of `raw.vtable` |

1. `Context` is a region-carrying record (07 §7.8). Its single region is the region of the waker it borrows. It is neither `Copy` nor `Clone`, and it has no `Format` impl.
2. The executor extension is a package-private field of `Context`. `sai:async` reaches it through package-private functions of `sai:core` that are part of the toolchain and are not items of this chapter (11 §11.10.2, §16.2.3). User code cannot read or replace it.
3. `Waker` implements `Clone` (its `clone` calls the vtable's `clone`) and `Drop` (its `drop` calls the vtable's `drop` and is declared `effects {}`, 07 §7.13.6). It is not `Copy`. It implements `Format`, which writes `Waker` and nothing else.
4. `Waker.noop()` allocates nothing. Its `wake`, `clone` and `drop` do nothing, and `will_wake` between two no-op wakers returns `true`.
5. `RawWaker` is `Copy` and `Clone`. `RawWakerVTable` is `Copy` and `Clone`. Neither has further impls.
6. The vtable functions, and therefore `wake`, `wake_by_ref`, `clone` and `drop` of `Waker`, have no effects (11 §11.4.3, 12 §12.6.6).
7. *Safety* of `Waker.from_raw`: the conditions of 11 §11.4.3 ("Raw wakers").

---

## 16.14 Formatting: `core.fmt` and `alloc.fmt`

f-strings (Stage 3) desugar into calls of `Writer.write_str` and `Format.format` on a fresh `String` (06 §6.20.3, D-EXPR-12, N-C-21). This section defines the traits, the specifiers and their meaning, and the impls. The traits and impls exist from Stage 2, so that code can format values by calling `format` directly before f-strings exist.

### 16.14.1 `Writer`

```sai
pub trait Writer {
    fn write_str(self: borrow mut Self, s: Str)

    fn write_char(self: borrow mut Self, c: Char) {
        var buf: Array<Byte, 4> = Array.filled(b'\0')
        self.write_str(c.encode_utf8(borrow mut buf))
    }
}
```

1. Writers are **infallible**: `write_str` returns nothing (N-J-20). A writer that cannot store all of its input either grows (`String`), truncates and records the truncation (`FixedBuf`, §16.14.5), or panics. It never reports an error to the formatting code.
2. `Writer` has no effect bound (N-J-12). `impl Writer for String` declares `effects { alloc }`. `FixedBuf` writes without effects.
3. `Writer` is a lang item (§16.3.3) and is not in the prelude.

### 16.14.2 `Format`

```sai
pub trait Format {
    fn format<W: Writer>(self: borrow Self, out: borrow mut W, spec: FormatSpec)
}
```

1. `format` writes a textual representation of `self` to `out`. The text MUST depend only on the value of `self` and on `spec` (§16.1.5). The library impls (§16.14.6) never call `out` with an empty string.
2. `format` has no effect bound. Its effects are latent, and an f-string has them (06 §6.20.3).
3. Through an f-string, an impl receives only the specifiers that the type checker accepts for its type (§16.14.3). A direct call of `format` may pass any specifier. The library impls treat a specifier they do not support as `Plain`, and user impls SHOULD do the same. An impl of `Format` alone receives `Plain` and `Debug`. An impl that is also declared `FormatPad` receives `Pad` and `Left` as well, and MUST honor them. The helper `write_padded` (§16.14.5) does this.
4. `Format` cannot be derived (D-TYPE-08). Refined types forward it to their base (05 §5.11.4).

```sai
use sai:core/fmt.{FormatSpec, Writer}

type Point {
    x: I32
    y: I32
}

impl Format for Point {
    fn format<W: Writer>(self: borrow Self, out: borrow mut W, spec: FormatSpec) {
        out.write_str("(")
        self.x.format(out, spec)
        out.write_str(", ")
        self.y.format(out, spec)
        out.write_str(")")
    }
}
```

`out` is passed on by reborrow coercion (07 §7.5.5) and stays usable. With this impl, `f"{p}"` and `f"{p:debug}"` both give `(1, -2)` for `Point(x: 1, y: -2)`. `f"{p:hex}"` is rejected with `FORMAT_SPEC_NOT_APPLICABLE`, because `Point` is not `FormatInt`.

### 16.14.3 `FormatSpec`

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum FormatSpec {
    Plain
    Debug
    Hex
    UpperHex
    Bin
    Oct
    Exp
    Prec(U32)
    Pad(U32)
    Left(U32)
    Zpad(U32)
}
```

`FormatSpec` is a lang item (§16.3.2). The specifier of an interpolation `{e:spec}` (02 §2.6.5) is translated into the argument value of the desugaring as follows:

| Source specifier | Argument value | Required trait of the operand type | Meaning |
|---|---|---|---|
| (none) | `FormatSpec.Plain` | `Format` | the plain form |
| `debug` | `FormatSpec.Debug` | `Format` | the debug form: strings and characters quoted and escaped |
| `hex` | `FormatSpec.Hex` | `FormatInt` | base 16, digits `0-9a-f` |
| `upper_hex` | `FormatSpec.UpperHex` | `FormatInt` | base 16, digits `0-9A-F` |
| `bin` | `FormatSpec.Bin` | `FormatInt` | base 2 |
| `oct` | `FormatSpec.Oct` | `FormatInt` | base 8 |
| `zpad(N)` | `FormatSpec.Zpad(N)` | `FormatInt` | decimal, padded with zeros after the sign to at least `N` characters |
| `exp` | `FormatSpec.Exp` | `FormatFloat` | scientific notation |
| `prec(N)` | `FormatSpec.Prec(N)` | `FormatFloat` | fixed notation with exactly `N` digits after the point |
| `pad(N)` | `FormatSpec.Pad(N)` | `FormatPad` | the plain form, right-aligned with spaces to at least `N` characters |
| `left(N)` | `FormatSpec.Left(N)` | `FormatPad` | the plain form, left-aligned with spaces to at least `N` characters |

Rules (N-J-21):

1. A specifier name that is not in the first column is the error `UNKNOWN_FORMAT_SPEC`. Its help lists the known names. A specifier that is not syntactically `LIDENT ( "(" INT ")" )?` is rejected earlier by the lexer (`INVALID_FORMAT_SPEC`, [impl/02](../impl/02-frontend-ast-hir.md)).
2. `zpad`, `prec`, `pad` and `left` require the argument, and the other names forbid it. A missing or unexpected argument, or an argument outside `0..=65535`, is the error `FORMAT_SPEC_ARGUMENT`.
3. If the operand type does not implement the trait of the third column, the error is `FORMAT_SPEC_NOT_APPLICABLE`, which names the type and the trait. If it does not implement `Format` at all, the error is `FORMAT_NOT_IMPLEMENTED` (06), and no specifier error is reported.
4. One interpolation has at most one specifier. Specifiers do not combine: a zero-padded hexadecimal number is written with `zpad` on a value converted by the program, or with `write_padded`.
5. Widths count Unicode scalar values (`Char`s) of the produced text, not bytes. A text that is already at least `N` characters long is written unchanged; nothing is truncated.

### 16.14.4 The marker traits

```sai
pub trait FormatPad: Format {}

pub trait FormatInt: FormatPad {}

pub trait FormatFloat: FormatPad {}
```

1. The three traits are lang items (§16.3.3), used only by the check of §16.14.3 rule 3. They have no items.
2. `FormatInt` and `FormatFloat` are sealed (§16.3.5). `FormatInt` is implemented by the ten integer types and `Byte`. `FormatFloat` is implemented by `F32` and `F64`. Both are also implemented by `borrow T`, `borrow mut T` and `Box<T>` when `T` implements them, so `{r:hex}` with `r : borrow U32` is accepted.
3. `FormatPad` is open. The library implements it for the integer types, `Byte`, the float types, `Bool`, `Char`, `Str`, `String`, and for `borrow T`, `borrow mut T` and `Box<T>` when `T: FormatPad`. A user type that implements `FormatPad` promises that its `format` honors `Pad` and `Left`. Breaking that promise is a logic error, not undefined behavior.
4. Refined types implement `Format` by forwarding (05 §5.11.4) but none of the marker traits, so only the plain and debug forms apply to them.

### 16.14.5 Helpers: `write_padded` and `FixedBuf`

| Signature | Effects | Result |
|---|---|---|
| `fn write_padded<W: Writer>(out: borrow mut W, text: Str, spec: FormatSpec)` | — | writes `text` aligned as `spec` requires: right-aligned for `Pad(n)`, left-aligned for `Left(n)`, unchanged for every other specifier |

`FixedBuf<const N: USize>` is a writer that stores at most `N` bytes in place, without allocation. It is usable in every profile, including `tiny`, and in functions without the `alloc` effect (08). Its fields are private.

| Signature | Effects | Result |
|---|---|---|
| `fn new() -> FixedBuf<N>` | — | an empty buffer |
| `fn as_str(self: borrow Self) -> Str` | — | the text written so far |
| `fn len(self: borrow Self) -> USize` | — | its length in bytes |
| `fn is_truncated(self: borrow Self) -> Bool` | — | `true` if some written text was dropped |
| `fn clear(self: borrow mut Self)` | — | empties the buffer and resets the truncation flag |

`impl<const N: USize> Writer for FixedBuf<N>` appends the longest prefix of `s` that fits and ends on a character boundary. If that prefix is shorter than `s`, the flag is set and every later write is ignored until `clear`. The content is therefore always valid UTF-8. `FixedBuf<N>` is not `Copy`. It implements `Clone` and `Default`.

```sai
use sai:core/fmt.{FixedBuf, FormatSpec}

fn render_code(code: U16) -> FixedBuf<8> {
    var buf: FixedBuf<8> = FixedBuf.new()
    code.format(borrow mut buf, FormatSpec.Zpad(4))
    buf
}
```

### 16.14.6 Forms and library impls

**Integers and `Byte`.**

1. `Plain` and `Debug` write the decimal value, with a leading `-` for negative values and no `+`. `Byte` is formatted as its numeric value (`0` to `255`).
2. `Hex`, `UpperHex`, `Bin` and `Oct` write the digits of the bit pattern of the value in its type's width, read as unsigned (two's complement for signed types), without leading zeros (`0` for zero) and without a prefix: `-1 : I8` with `hex` gives `ff`.
3. `Zpad(n)` writes the decimal form, inserting zeros after the sign until the text has at least `n` characters: `-42` with `zpad(5)` gives `-0042`.
4. `Pad(n)` and `Left(n)` pad the decimal form with spaces.

**Floats** (N-J-22).

1. `Plain` and `Debug` write the shortest decimal digit string that parses back (§16.8.4) to the same value. If the value is zero or `1e-5 ≤ |x| < 1e16`, it is written in fixed notation, with `.0` appended when it has no fractional digits: `1.0`, `0.1`, `100.0`, `-0.0`. Otherwise it is written in scientific notation: the first significant digit, then a point and the remaining digits if there are any, then `e`, then the decimal exponent with a leading `-` if it is negative and without leading zeros: `1e16`, `1.5e-7`.
2. `Exp` always uses the scientific notation of item 1: `1500.0` gives `1.5e3`.
3. `Prec(n)` writes fixed notation with exactly `n` digits after the point, rounded from the exact binary value with ties to even. `Prec(0)` writes no point: `2.5` gives `2` and `3.5` gives `4`.
4. In every specifier, NaN is written `NaN` (the sign bit is ignored), and the infinities are written `inf` and `-inf`. `Pad(n)` and `Left(n)` pad the `Plain` form with spaces.
5. The output is a deterministic function of the value (§16.1.5). The bits of a NaN never influence it.

**Text.**

1. `Str` and `String`: `Plain` writes the text unchanged. `Debug` writes it between `"` characters, with `\"`, `\\`, `\n`, `\r`, `\t` and `\0` escaped, and every other character in U+0000–U+001F and U+007F written as `\u{h}`, where `h` is the code in lowercase hexadecimal without leading zeros. All other characters are written unchanged.
2. `Char`: `Plain` writes the character. `Debug` writes it between `'` characters with the escapes of item 1, except that `\'` is escaped instead of `\"`.
3. `Bool`: `true` or `false` in both forms.

**Compound types.** In the following, an element is always written in its `Debug` form, whatever the outer specifier (only `Plain` and `Debug` reach these impls).

| Type | Bound | Form |
|---|---|---|
| `()` | | `()` |
| `(T1, …, Tn)`, 1 ≤ n ≤ 12 | every `Ti: Format` | `(a, b)`; a 1-tuple is `(a,)` |
| `Array<T, N>`, `Slice<T>`, `SliceMut<T>`, `Vec<T>` | `T: Format` | `[a, b, c]`; empty is `[]` |
| `Option<T>` | `T: Format` | `None` or `Some(a)` |
| `Result<T, E>` | `T: Format, E: Format` | `Ok(a)` or `Err(e)` |
| `Poll<T>`, `ReadyFuture<T>` | `T: Format` | `Ready(a)` or `Pending`; `ReadyFuture(a)` or `ReadyFuture(<taken>)` |
| `PendingFuture<T>` | | `PendingFuture` |
| `Ordering` | | `Less`, `Equal` or `Greater` |
| the six range types | `T: Format` | `a..b`, `a..=b`, `a..`, `..b`, `..=b`, `..`, with the endpoints in their `Plain` form |
| `borrow T`, `borrow mut T`, `Box<T>` | `T: Format` | the form of `T`, with the same specifier |
| `Ptr<T>`, `PtrMut<T>` | | `0x` followed by the address in lowercase hexadecimal |
| `Waker` | | `Waker` |

**Error types.** `Plain` writes the message of the following table. `Debug` writes the variant name, or `Utf8Error(valid_up_to: n)` and `FromUtf8Error(valid_up_to: n)` for the records.

| Value | Plain message |
|---|---|
| `ParseIntError.Empty` | `cannot parse integer from empty string` |
| `ParseIntError.InvalidDigit` | `invalid digit found in string` |
| `ParseIntError.PosOverflow` | `number too large to fit in target type` |
| `ParseIntError.NegOverflow` | `number too small to fit in target type` |
| `ParseFloatError.Empty` | `cannot parse float from empty string` |
| `ParseFloatError.Invalid` | `invalid float literal` |
| `ParseCharError.Empty` | `cannot parse char from empty string` |
| `ParseCharError.TooManyChars` | `too many characters in string` |
| `Utf8Error`, `FromUtf8Error` | `invalid utf-8 sequence at byte <n>`, with `<n>` the decimal `valid_up_to` |

No other type of this chapter implements `Format`. In particular `Context`, `RawWaker`, `MaybeUninit<T>`, `Gen`, `Arena` and the iterator types do not.

### 16.14.7 `ToString`: `alloc.fmt`

```sai
pub trait ToString {
    fn to_string(self: borrow Self) -> String
}

impl<T: Format> ToString for T {
    fn to_string(self: borrow Self) -> String
        effects { alloc }
    {
        var buf = String.new()
        self.format(borrow mut buf, FormatSpec.Plain)
        buf
    }
}
```

1. `ToString` is in the prelude (§16.4). `x.to_string()` equals `f"{x}"` for every `x` whose type implements `Format`.
2. The blanket impl is the only impl of `ToString`. Because the overlap check ignores bounds (05 §5.10.2), any other impl of `ToString` overlaps it and is `OVERLAPPING_IMPLS`. A type gets `to_string` by implementing `Format`.

---

## 16.15 Owned types and allocation: `sai:alloc`

Every function of this section that can allocate declares `effects { alloc }` (N-J-7). Allocation failure is never returned as a value: it panics with kind `OutOfMemory` and the message `out of memory` ([10](10-errors-and-panics.md) §10.4.3). The *Panics* columns do not repeat this. Freeing memory is not an effect (D-OWN-05).

### 16.15.1 The global allocator: `alloc.heap`

| Signature | Effects | Panics | Result |
|---|---|---|---|
| `fn allocate(size: USize, align: USize) -> PtrMut<Byte> requires align.is_power_of_two()` | `alloc` | `Contract(id)` | a pointer to `size` fresh bytes aligned to `align`, uninitialized |
| `unsafe fn deallocate(ptr: PtrMut<Byte>, size: USize, align: USize)` | — | — | frees the block |
| `unsafe fn reallocate(ptr: PtrMut<Byte>, size: USize, align: USize, new_size: USize) -> PtrMut<Byte>` | `alloc` | — | a block of `new_size` bytes with the same alignment whose first `min(size, new_size)` bytes equal those of the old block; the old block is freed |

1. These three intrinsics (§16.3.4) are the only path from SAI code to the global allocator (§16.1.1, 08 §8.3.2). Their implementation, including the size classes and `memory.grow`, belongs to [impl/10](../impl/10-runtime.md). The global allocator is linked only when one of them is reachable (D-WASM-05).
2. For `size == 0`, `allocate` returns the non-null pointer whose address is `align` (the payload of `PtrMut.from_addr(align)`, §16.12.2) and allocates nothing. `deallocate` and `reallocate` accept such a pointer with `size == 0`.
3. A request whose size, rounded up to `align`, exceeds `ISIZE_MAX` panics with `OutOfMemory`.
4. *Safety* of `deallocate` and `reallocate`: `ptr` MUST have been returned by `allocate` or `reallocate` with exactly this `size` and `align`, and MUST NOT have been freed. Violations are undefined behavior (12 §12.6.1 item 5).

### 16.15.2 `Box<T>`: `alloc.boxed`

`Box<T>` owns one value of type `T` on the heap. It is a lang item, a fundamental type for coherence (05 §5.10.1), and has the layout of a single pointer ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4.5). `*b` reads, writes, borrows and moves out of the box (06 §6.9, 07). A `Box<T>` of a zero-sized `T` allocates nothing.

| Signature | Effects | Result |
|---|---|---|
| `fn new(value: T) -> Box<T>` | `alloc` | a box holding `value` |
| `fn into_inner(self: Self) -> T` | — | the value; the allocation is freed. This is the only way to move a non-`Copy` value out of a `Box` (07 N-B-2) |
| `fn as_ref(self: borrow Self) -> borrow T` | — | a borrow of the value |
| `fn as_mut(self: borrow mut Self) -> borrow mut T` | — | a mutable borrow of the value |
| `fn into_raw(self: Self) -> PtrMut<T>` | — | the address of the value; the box is consumed without freeing |
| `unsafe fn from_raw(ptr: PtrMut<T>) -> Box<T>` | — | the box that owns `*ptr` (12 §12.4.5) |

*Safety* of `from_raw`: `ptr` MUST have been returned by `Box.into_raw` for a `Box<T>`, and no other box may own it.

`Box<T>` implements `Drop` (drops the value, then frees the allocation), `Clone` and `Default` with `effects { alloc }` when `T` implements them, the comparison traits and `Hash` by forwarding to `T`, `Format` and the format marker traits by forwarding (§16.14), and `Future` (`Output = T.Output`) and `Stream` (`Item = T.Item`) when `T` implements them: `poll` and `poll_next` poll `*self` in place and add no effect (11 §11.3.2, N-I-4). It implements no other trait. In particular, `Box<T>` is not an `Iterator` or a function type, whatever `T` is.

### 16.15.3 `Vec<T>` and `Bytes`: `alloc.vec`

`Vec<T>` is a growable array that owns its elements. Its layout (pointer, length, capacity) belongs to impl/06 §6.4.5. Its invariants are: `len() <= capacity()`, the first `len()` elements are valid, and the buffer was obtained from §16.15.1 with the size and alignment of `capacity()` elements of `T`, unless `capacity()` is zero or `T` is zero-sized.

```sai
pub type Bytes = Vec<Byte>
```

**Growth.** When an operation needs room for `k` more elements than the capacity provides, the new capacity is `max(2 × capacity(), len() + k, 4)`. `reserve` and `with_capacity` request exactly what they are given if that is larger. If `T` is zero-sized, `capacity()` is `USIZE_MAX` and no allocation ever happens. The capacity never shrinks, except through `shrink_to_fit`. These rules make `capacity()` deterministic (§16.1.5).

| Signature | Bound | Effects | Panics | Result |
|---|---|---|---|---|
| `fn new() -> Vec<T>` | | — | — | an empty vector; allocates nothing |
| `fn with_capacity(capacity: USize) -> Vec<T>` | | `alloc` | — | an empty vector with at least that capacity |
| `fn from_slice(s: Slice<T>) -> Vec<T>` | `T: Clone` | `alloc` | — | a vector of clones of the elements of `s` |
| `fn len(self: borrow Self) -> USize` | | — | — | the number of elements |
| `fn is_empty(self: borrow Self) -> Bool` | | — | — | `len() == 0` |
| `fn capacity(self: borrow Self) -> USize` | | — | — | the capacity |
| `fn push(self: borrow mut Self, value: T)` | | `alloc` | — | appends `value` |
| `fn pop(self: borrow mut Self) -> Option<T>` | | — | — | removes and returns the last element |
| `fn insert(self: borrow mut Self, index: USize, value: T)` | | `alloc` | `BoundsCheck` if `index > len()` | shifts the elements from `index` right and stores `value` there |
| `fn remove(self: borrow mut Self, index: USize) -> T` | | — | `BoundsCheck` if `index >= len()` | removes the element and shifts the rest left |
| `fn swap_remove(self: borrow mut Self, index: USize) -> T` | | — | `BoundsCheck` if `index >= len()` | removes the element and moves the last one into its place |
| `fn clear(self: borrow mut Self)` | | — | — | drops every element; the capacity is kept |
| `fn truncate(self: borrow mut Self, len: USize)` | | — | — | drops the elements from `len` on; no effect if `len >= len()` |
| `fn reserve(self: borrow mut Self, additional: USize)` | | `alloc` | — | ensures `capacity() >= len() + additional` |
| `fn shrink_to_fit(self: borrow mut Self)` | | `alloc` | — | reduces the capacity to `len()` |
| `fn extend_from_slice(self: borrow mut Self, other: Slice<T>)` | `T: Clone` | `alloc` | — | appends clones of the elements of `other` |
| `fn extend<I: IntoIterator<Item = T>>(self: borrow mut Self, items: I)` | | `alloc` | — | appends the items in order |
| `fn append(self: borrow mut Self, other: borrow mut Vec<T>)` | | `alloc` | — | moves every element of `other` to the end of `self`; `other` becomes empty |
| `fn retain<P: FnMut(borrow T) -> Bool>(self: borrow mut Self, keep: P)` | | — | — | drops, in order, the elements for which `keep` is `false`; `keep` is called once per element, in order |
| `fn dedup(self: borrow mut Self)` | `T: PartialEq` | — | — | drops each element equal to the element kept before it |
| `fn drain<R: SliceRange>(self: borrow mut Self, range: R) -> Drain<T>` | | — | as `Slice.slice` | an iterator that removes and yields the elements of `range` |
| `fn as_slice(self: borrow Self) -> Slice<T>` | | — | — | a view of the elements |
| `fn as_mut_slice(self: borrow mut Self) -> SliceMut<T>` | | — | — | an exclusive view of the elements |

**Forwarded slice methods.** `Vec<T>` also has every method of `Slice<T>` and `SliceMut<T>` of §16.10.1 except `as_slice`, `reborrow`, `from_raw_parts` and `from_raw_parts_mut`, with the same name, bounds, panics and results. A method whose slice receiver is `self: Self` on `Slice<T>` takes `self: borrow Self` on `Vec<T>` and applies to `self.as_slice()`. A method of `SliceMut<T>` takes `self: borrow mut Self` and applies to `self.as_mut_slice()`. Results that view the elements borrow the vector. So `v.iter()`, `v.get(i)`, `v.sort()` and `v.binary_search(borrow x)` work on a `Vec<T>` directly.

| Signature (on `Slice<T>`, declared in `alloc.vec`) | Bound | Effects | Result |
|---|---|---|---|
| `fn to_vec(self: Self) -> Vec<T>` | `T: Clone` | `alloc` | the same as `Vec.from_slice(self)` |

**Impls of `Vec<T>`.**

- `Drop`: drops the elements in index order, then frees the buffer.
- `Clone` (`T: Clone`, `alloc`; the clone has capacity `len()`), `Default` (empty), `PartialEq`, `Eq`, `PartialOrd`, `Ord` and `Hash` as for `Slice<T>` (§16.5.5, §16.5.6), and `Format` (§16.14.6).
- `Index<USize>` and `IndexMut<USize>` with `Output = T`, and the range `Index` impls (§16.6.2).
- `FromIterator<T>` with `effects { alloc }` (§16.11.3). `IntoIterator` for `Vec<T>` (`VecIntoIter<T>`), `borrow Vec<T>` (`SliceIter<T>`) and `borrow mut Vec<T>` (`SliceIterMut<T>`) (§16.11.1).
- `From<Array<T, N>> for Vec<T>` (`alloc`), and `From<String> for Vec<Byte>`, which reuses the buffer of the string and allocates nothing.
- There is no `From<Slice<T>> for Vec<T>`: by N-B-12 the trait argument could only be a `Slice<'static, T>`. `Vec.from_slice` and `Slice.to_vec` accept every region (N-J-23).

`VecIntoIter<T>` yields the elements by value from first to last. It implements `DoubleEndedIterator`, `ExactSizeIterator` and, when `T: Clone`, `Clone` (`alloc`). Dropping it drops the elements not yet produced and frees the buffer.

`Drain<T>` is a region-carrying iterator over the removed range. It yields the removed elements by value and implements `DoubleEndedIterator` and `ExactSizeIterator`. When it is dropped, the elements it has not yet produced are dropped, and the elements after the range are moved down to close the gap. While the `Drain` is live, the vector is exclusively borrowed (07). If a `Drain` is leaked with `forget`, the vector keeps the elements before the range and loses the rest; this is safe and drops nothing twice.

### 16.15.4 `String`: `alloc.string`

`String` is a growable, owned UTF-8 string. Its layout equals that of `Vec<Byte>` (impl/06 §6.4.5), its growth follows the rule of §16.15.3, and its invariant is that its bytes are valid UTF-8 (12 §12.6.3).

| Signature | Effects | Panics | Result |
|---|---|---|---|
| `fn new() -> String` | — | — | an empty string; allocates nothing. A lang item (§16.3.4) |
| `fn with_capacity(capacity: USize) -> String` | `alloc` | — | an empty string with at least that capacity in bytes |
| `fn from_str(s: Str) -> String` | `alloc` | — | a copy of `s` |
| `fn from_utf8(bytes: Bytes) -> Result<String, FromUtf8Error>` | — | — | the bytes as a string, reusing the buffer; `Err` if they are not UTF-8 |
| `unsafe fn from_utf8_unchecked(bytes: Bytes) -> String` | — | — | the bytes as a string |
| `fn as_str(self: borrow Self) -> Str` | — | — | a view of the text |
| `fn as_bytes(self: borrow Self) -> Slice<Byte>` | — | — | a view of the bytes |
| `fn into_bytes(self: Self) -> Bytes` | — | — | the buffer as a byte vector |
| `fn len(self: borrow Self) -> USize` | — | — | the length in bytes |
| `fn is_empty(self: borrow Self) -> Bool` | — | — | `len() == 0` |
| `fn capacity(self: borrow Self) -> USize` | — | — | the capacity in bytes |
| `fn push_str(self: borrow mut Self, s: Str)` | `alloc` | — | appends `s` |
| `fn push(self: borrow mut Self, c: Char)` | `alloc` | — | appends the UTF-8 encoding of `c` |
| `fn pop(self: borrow mut Self) -> Option<Char>` | — | — | removes and returns the last character |
| `fn clear(self: borrow mut Self)` | — | — | empties the string; the capacity is kept |
| `fn truncate(self: borrow mut Self, len: USize)` | — | `BoundsCheck` if `len < self.len()` and `len` is not a char boundary | keeps the first `len` bytes; no effect if `len >= self.len()` |
| `fn reserve(self: borrow mut Self, additional: USize)` | `alloc` | — | ensures `capacity() >= len() + additional` |
| `fn shrink_to_fit(self: borrow mut Self)` | `alloc` | — | reduces the capacity to `len()` |
| `fn insert_str(self: borrow mut Self, index: USize, s: Str)` | `alloc` | `BoundsCheck` if `index > len()` or `index` is not a char boundary | inserts `s` at byte `index` |

**Forwarded `Str` methods.** `String` also has every method of `Str` of §16.9.2 except `from_utf8`, `from_utf8_unchecked` and `as_bytes`, which it declares itself. A method whose `Str` receiver is `self: Self` takes `self: borrow Self` on `String` and applies to `self.as_str()`. Results that view the text borrow the string. So `s.trim()`, `s.split(",")` and `s.starts_with("x")` work on a `String` directly.

`FromUtf8Error` is a record with private fields that holds the rejected bytes:

| Signature | Result |
|---|---|
| `fn utf8_error(self: borrow Self) -> Utf8Error` | the error found in the bytes (§16.9.2) |
| `fn valid_up_to(self: borrow Self) -> USize` | `self.utf8_error().valid_up_to` |
| `fn into_bytes(self: Self) -> Bytes` | the rejected bytes |

It implements `Clone` (`alloc`), `PartialEq`, `Eq` and `Format` (§16.14.6).

**Allocating methods of `Str`**, declared in `alloc.string`:

| Signature (on `Str`) | Effects | Panics | Result |
|---|---|---|---|
| `fn to_ascii_lowercase(self: Self) -> String` | `alloc` | — | a copy with `A`–`Z` mapped to `a`–`z` |
| `fn to_ascii_uppercase(self: Self) -> String` | `alloc` | — | a copy with `a`–`z` mapped to `A`–`Z` |
| `fn repeat(self: Self, n: USize) -> String` | `alloc` | `OutOfMemory` if `len() × n` overflows `USize` | `n` copies of `self` |
| `fn replace(self: Self, from: Str, to: Str) -> String requires not from.is_empty()` | `alloc` | `Contract(id)` | a copy with every non-overlapping occurrence of `from`, scanning left to right, replaced by `to` |

**Impls of `String`.**

- `Drop` (frees the buffer), `Clone` (`alloc`), `Default` (empty), and `PartialEq`, `Eq`, `PartialOrd`, `Ord` and `Hash` with the same results as for the `Str` of `as_str()` (§16.5.5, §16.5.6). `Format` and `FormatPad` (§16.14.6).
- `Writer` with `effects { alloc }` on `write_str` and `write_char`, which append (§16.14.1).
- `Add<Str>` with `Output = String` and `effects { alloc }`, which appends the right operand and returns the string (D-EXPR-13, §16.6.1). `s += t` with `s : String` and `t : Str` is `s = s + t` (06).
- `Index<USize>` with `Output = Byte`, and the range `Index` impls with `Output = Str` (§16.6.2).
- `FromIterator<Char>` with `effects { alloc }`.
- `From<Str>` and `From<Char>` with `effects { alloc }`. By N-B-12 the argument of `String.from` is a `Str<'static>`, for example a literal. A `Str` of any other region is converted with `String.from_str` or `to_string()` (N-J-23).
- There is no `FromIterator<Str>`, `Add<String>` or `Add<borrow String>` for `String` (06 §6.8.9).

```sai
fn join_words(words: Slice<Str>, sep: Str) -> String
    effects { alloc }
{
    var out = String.new()
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push_str(sep)
        }
        out.push_str(*w)
    }
    out
}
```

### 16.15.5 Arenas: `alloc.arena`

`Arena` is the lang-item type of `region` blocks (07 §7.15, D-OWN-07). It holds a list of chunks obtained from the global allocator. It has no public constructor: the only arenas are the hidden locals of `region` blocks.

**Desugaring** (N-J-24). The expression `region r { body }` is exactly

```text
{
    let %arena = Arena.region_new()
    let r = borrow %arena
    body
}
```

where `%arena` is a hygienic local that no source can name (N-C-21), and `Arena.region_new` is the package-private lang item of §16.3.4, declared as `fn region_new() -> Arena` with `effects { alloc }`. It allocates nothing itself; chunks are obtained by the first allocation that needs one. The block's value is the value of `body`. The locals are dropped at block exit in reverse order of declaration, `r` first and then `%arena` (07 §7.13), and dropping `%arena` frees every chunk. Consequently the `region` expression has the effect `alloc` and every rule of 07 §7.15 follows from the ordinary rules applied to this block. `r` is a `let` binding of type `borrow Arena`, so it cannot be reassigned.

| Signature | Effects | Result |
|---|---|---|
| `fn alloc<T: Copy>(self: borrow Self, value: T) -> borrow mut T` | `alloc` | `value` stored in fresh arena memory |
| `fn alloc_str(self: borrow Self, s: Str) -> Str` | `alloc` | a copy of `s` in the arena |
| `fn alloc_slice<T: Copy>(self: borrow Self, s: Slice<T>) -> Slice<T>` | `alloc` | a copy of the elements of `s` in the arena |

1. The signatures are those of 07 §7.15.2, with their effects. Every result borrows the arena, not the argument (07). A non-`Copy` `T` is `ARENA_REQUIRES_COPY` (07, 05).
2. Each call returns memory that is disjoint from every other live allocation. A request that does not fit in the current chunk obtains a new chunk of at least the requested size. The chunk sizes are part of the runtime ([impl/10](../impl/10-runtime.md)); they affect only when `OutOfMemory` occurs, never a result.
3. `Arena` implements `Drop` and nothing else. It is not `Copy`, `Clone`, `Default` or `Format`.
4. **State.** The chunk list and the position in the current chunk are fields of the `Arena` value. `alloc`, `alloc_str` and `alloc_slice` change them while the shared loan held by `r` is live. That is the arena exception of [12](12-unsafe-and-ffi.md) §12.6.4 rule 5, the only interior mutability of SAI. It is not observable from safe code: the fields are private to `sai:alloc`, and every method has the effect `alloc`, so no pure function reads the state and the Standard Fact Checker never has a fact about it ([09](09-contracts-and-refinements.md) §9.6.4, N-C-58).

---

## 16.16 Signatures of other standard packages

### 16.16.1 Status and designated items

1. The standard packages other than `sai:core` and `sai:alloc` are ordinary packages, declared in `requires` and shipped in the pre-populated cache of the toolchain (D-PKG-04, package/01 N-G-9): `sai:collections`, `sai:text`, `sai:fmt`, `sai:io`, `sai:fs`, `sai:net`, `sai:http`, `sai:async` and `sai:process`.
2. This section fixes only the items that the specification itself refers to (01 §1.1.4 item 5). These items are normative: every release of the package that a toolchain ships MUST contain them with exactly these signatures and effects. Every other item of these packages, and every item of `sai:collections`, `sai:text`, `sai:fmt` and `sai:io`, is defined by the package's own documentation and versioned with the package (package/01). This split is deliberate (N-J-25): the language does not freeze library design beyond what its rules need (P7).
3. **Designated items.** Two groups of items outside `sai:core` and `sai:alloc` are known to the compiler by canonical path (N-J-26):
   - `sai:process` items `Env`, `ExitCode` and the package-private function `Env.for_entry`, which the `run` glue of a command calls to construct the argument of `main` (D-CAP-02, 13 N-D-31, [impl/07](../impl/07-component-lowering.md) §7.8.4, 08 §8.12.3);
   - `sai:async` item `scope`, whose typing rule is special (11 §11.7.3, N-I-11).
   They are not lang items (§16.3.1): no desugaring uses them (D-PKG-04), and the compiler looks them up only when the package that contains them is a dependency of the build. A command whose package does not depend on `sai:process` cannot name the types of `main`, which is an ordinary resolution error, not an internal compiler error.
4. Effects follow N-J-7. Capability values (`Env`, `Stdout`, `FileSystem`, `Network`, `HttpClient`) are obtained without effects. Every operation that reaches the host declares the effect of 08 §8.3 that covers it. Whether a build may use the effect is decided by `allow` (08), not by whether the capability value exists.

### 16.16.2 `sai:async` (Stage 5)

The meaning of these items is defined in [11](11-async-and-concurrency.md) §11.7, §11.9 and §11.10. The package name ends in a keyword, so items are imported with a list or an alias (11 §11.1.3, N-I-10). `Scope` and `JoinHandle<T>` are region-carrying (11 §11.7.2).

| Signature | Effects | Section |
|---|---|---|
| `fn block_on<F: Future>(future: F) -> F.Output` | `alloc` | 11 §11.9.1 |
| `async fn scope<B, F>(body: B) -> F.Output where B: FnOnce(borrow Scope) -> F, F: Future` | `alloc` | 11 §11.7.2, §11.7.3 |
| `fn spawn<C: Future>(self: borrow Self, child: C) -> JoinHandle<C.Output>` (on `Scope`) | `alloc` | 11 §11.7.2 |
| `async fn join(self: Self) -> T` (on `JoinHandle<T>`) | — | 11 §11.7.5 |
| `fn cancel(self: Self)` (on `JoinHandle<T>`) | — | 11 §11.7.5 |
| `fn is_finished(self: borrow Self) -> Bool` (on `JoinHandle<T>`) | — | 11 §11.7.5 |
| `async fn join<A: Future, B: Future>(a: A, b: B) -> (A.Output, B.Output)` | — | 11 §11.7.6 |
| `async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F.Output>` | `alloc` | 11 §11.7.6 |
| `async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A.Output, B.Output>` | — | 11 §11.7.6 |
| `unsafe fn register(cx: borrow mut Context, pollable: borrow Pollable) -> Option<Registration>` | — | 11 §11.10.2 |

```sai
pub enum Either<A, B> {
    Left(A)
    Right(B)
}
```

1. The effects listed are the declared ones. The effects of the operands are latent and are attributed to the caller (11 §11.8, 08 §8.8).
2. `Scope`, `JoinHandle<T>` and `Registration` have private fields and are neither `Copy` nor `Clone`. The futures returned by the `async fn` items are anonymous future types, which are must-use types (D-EXPR-01, 15 §15.5.1 rule 3). `Either<A, B>` has `@derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)` and `Format` with the forms `Left(a)` and `Right(b)`.
3. `register`, `Registration` and `Pollable` exist only on targets with WASI (`wasm32-wasi`, `wasm64-wasi`, [targets](../targets/targets-and-profiles.md)). `Pollable` is the type of the resource `pollable` of `wasi:io/poll` (13).
4. **Component Model streams and futures (Provisional, 11 §11.10.3).** `FutureReader<T>` implements `Future` with `Output = T`, and `StreamReader<T>` implements `Stream` with `Item = T`. Their writer ends are `FutureWriter<T>` and `StreamWriter<T>`:

| Signature | Effects | Result |
|---|---|---|
| `fn future_pair<T>() -> (FutureWriter<T>, FutureReader<T>)` | `alloc` | a connected pair |
| `fn stream_pair<T>() -> (StreamWriter<T>, StreamReader<T>)` | `alloc` | a connected pair |
| `async fn write(self: Self, value: T) -> Bool` (on `FutureWriter<T>`) | — | `false` if the reader was dropped, and `value` is then dropped |
| `async fn write(self: borrow mut Self, value: T) -> Bool` (on `StreamWriter<T>`) | — | `false` if the reader was dropped |

   Dropping a `StreamWriter<T>` ends the stream: the reader's next `poll_next` returns `.Ready(None)`. Dropping a `FutureWriter<T>` without writing makes the reader panic with `ExplicitPanic` and the message `future writer dropped`. These items follow the Provisional status of 11 §11.10.3 and are available only on targets with the Component Model async ABI.

### 16.16.3 `sai:process` (Stage 4)

`sai:process` provides the entry-point types of commands (D-CAP-02, 13 §13.5.3) and the standard streams. A command imports it with `use sai:process`, which binds the name `process`, so the entry point is written as in D-CAP-02.

| Signature | Effects | Result |
|---|---|---|
| `fn for_entry() -> Env` (on `Env`; package-private) | — | the environment of the current run; called only by the `run` glue |
| `fn args(self: borrow Self) -> Vec<String>` (on `Env`) | `env.read`, `alloc` | the command-line arguments, starting with the program name if the host supplies one |
| `fn var(self: borrow Self, name: Str) -> Option<String>` (on `Env`) | `env.read`, `alloc` | the value of the environment variable `name` |
| `fn stdin(self: borrow Self) -> Stdin` | — | the standard input stream |
| `fn stdout(self: borrow Self) -> Stdout` | — | the standard output stream |
| `fn stderr(self: borrow Self) -> Stderr` | — | the standard error stream |
| `fn exit(self: borrow Self, code: ExitCode) -> Never` (on `Env`) | `process.exit` | terminates the instance; drops nothing |
| `fn read_line(self: borrow mut Self) -> Option<String>` (on `Stdin`) | `stdio.read`, `alloc` | the next line without its terminator, or `None` at end of input |
| `fn success() -> ExitCode` | — | the exit code `0` |
| `fn failure() -> ExitCode` | — | the exit code `1` |
| `fn from_code(code: U8) -> ExitCode` | — | the given code |
| `fn code(self: borrow Self) -> U8` (on `ExitCode`) | — | the code |
| `fn is_success(self: borrow Self) -> Bool` (on `ExitCode`) | — | `code() == 0` |

1. `Env` is neither `Copy` nor `Clone`, and has private fields. `Env.for_entry` performs no host call. Each method that reads the host does so when it is called, so `Env` itself carries no data and no effect.
2. `Stdout` and `Stderr` implement `Writer` (§16.14.1) with `effects { stdio.write }`. Each `write_str` writes the bytes to the host stream and flushes it. A host write error is ignored, which keeps writers infallible (N-J-20). Both implement `Clone`.
3. `ExitCode` has `@derive(Copy, Clone, PartialEq, Eq, Hash)` and `Format` (the decimal code). On WASI 0.2 only success and failure can be transmitted (13 N-D-31).
4. Lines read by `read_line` end at `\n`, and a preceding `\r` is removed. Bytes that are not valid UTF-8 are replaced by `REPLACEMENT_CHARACTER`, one per maximal invalid subsequence.

```sai
use sai:core/fmt.{Writer}
use sai:process

pub fn main(env: owned process.Env) -> process.ExitCode
    effects { env.read, stdio.write, alloc }
{
    var out = env.stdout()
    let args = env.args()
    if args.len() < 2 {
        out.write_str("usage: greet <name>\n")
        return process.ExitCode.failure()
    }
    out.write_str(f"hello, {args[1]}\n".as_str())
    process.ExitCode.success()
}
```

### 16.16.4 `sai:fs` and `sai:net` (Stage 4)

**`sai:fs`.** `FileSystem` is the capability for the directories that the host preopens (`wasi:filesystem/preopens`). Paths are `Str`s with `/` as the separator. An absolute path is resolved against the preopened directory with the longest matching name, and a relative path against the preopened directory named `.`. A path that matches no preopened directory, or that leaves its directory through `..`, is `IoError.NotFound`.

| Signature | Effects | Result |
|---|---|---|
| `fn from_env(env: borrow Env) -> FileSystem` (on `FileSystem`) | — | the capability of the current run |
| `fn read(self: borrow Self, path: Str) -> Result<Bytes, IoError>` | `fs.read`, `alloc` | the contents of the file |
| `fn read_to_string(self: borrow Self, path: Str) -> Result<String, IoError>` | `fs.read`, `alloc` | the contents as text; `InvalidData` if they are not UTF-8 |
| `fn write(self: borrow Self, path: Str, data: Slice<Byte>) -> Result<(), IoError>` | `fs.write` | creates or truncates the file and writes `data` |
| `fn remove_file(self: borrow Self, path: Str) -> Result<(), IoError>` | `fs.write` | removes the file |

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum IoError {
    NotFound
    PermissionDenied
    AlreadyExists
    InvalidData
    Unsupported
    Other
}
```

`FileSystem` implements `Clone`. `IoError` implements `Format`; the plain form is the variant name in lowercase words (`not found`, `permission denied`, `already exists`, `invalid data`, `unsupported`, `other`). Host error codes that have no variant map to `Other`.

**`sai:net`.** `Network` is the capability for `wasi:sockets`. `Socket` is a connected TCP stream. Operations block the instance until they complete.

| Signature | Effects | Result |
|---|---|---|
| `fn from_env(env: borrow Env) -> Network` (on `Network`) | — | the capability of the current run |
| `fn connect(self: borrow Self, host: Str, port: U16) -> Result<Socket, NetError>` | `net.connect`, `alloc` | resolves `host` and connects to the first address that accepts |
| `fn send(self: borrow mut Self, data: Bytes) -> Result<(), NetError>` (on `Socket`) | `net.connect` | sends all of `data` |
| `fn receive(self: borrow mut Self, max: USize) -> Result<Bytes, NetError>` (on `Socket`) | `net.connect`, `alloc` | at most `max` bytes; empty at end of stream |

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum NetError {
    HostNotFound
    ConnectionRefused
    ConnectionReset
    TimedOut
    Other
}
```

`Socket` is neither `Copy` nor `Clone`. Dropping it closes the connection. `Network` implements `Clone`. `NetError` implements `Format` like `IoError`.

### 16.16.5 `sai:http` (Stage 4; `async` items Stage 5)

The module `sai:http/client` provides outgoing HTTP over `wasi:http/outgoing-handler`. Its `HttpClient` is the type that examples of 03, 11 and 13 call `HttpClient` or `Http`; 11 §11.1.4 uses the same `get` signature.

| Signature | Effects | Result |
|---|---|---|
| `fn from_env(env: borrow Env) -> HttpClient` (on `HttpClient`) | — | the capability of the current run |
| `async fn get(self: borrow Self, url: Str) -> Result<String, HttpError>` | `net.http`, `alloc` | the body of a `GET` response with a status in `200..=299`, as text |
| `async fn send(self: borrow Self, request: Request) -> Result<Response, HttpError>` | `net.http`, `alloc` | the response, whatever its status |
| `fn get_blocking(self: borrow Self, url: Str) -> Result<String, HttpError>` | `net.http`, `alloc` | as `get`, blocking the instance; available from Stage 4 |

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum Method {
    Get
    Head
    Post
    Put
    Delete
    Patch
    Options
}

@derive(Clone, PartialEq, Eq)
pub type Request {
    pub method: Method
    pub url: String
    pub headers: Vec<(String, String)>
    pub body: Bytes
}

@derive(Clone, PartialEq, Eq)
pub type Response {
    pub status: U16
    pub headers: Vec<(String, String)>
    pub body: Bytes
}

@derive(Copy, Clone, PartialEq, Eq, Hash)
pub enum HttpError {
    InvalidUrl
    ConnectionFailed
    TimedOut
    Status(U16)
    InvalidBody
    Other
}
```

`get` returns `Err(HttpError.Status(s))` for a status `s` outside `200..=299`, and `Err(HttpError.InvalidBody)` if the body is not UTF-8. `HttpClient` implements `Clone`. `HttpError` implements `Format` like `IoError`, with `Status(s)` written as `status <s>`.

---

## 16.17 Property testing: `core.testing` (Stage 3)

The meaning of properties, seeds, sizes and the rejection limit belongs to [14](14-testing.md). This section fixes the signatures.

```sai
pub trait Arbitrary {
    fn arbitrary(gen: borrow mut Gen) -> Self
}
```

1. `Arbitrary` has no effect bound (N-J-12). The library impls below are pure except those that allocate, which declare `effects { alloc }`. There is no shrinking in v0.1: `Arbitrary` has no other method.
2. `Arbitrary` is derivable (05 §5.12, D-TYPE-08), and tuples and arrays have compiler-provided impls (05 §5.11.4). The impls for `Option` and `Result` below are hand-written library impls, not derived ones: their variant probabilities are not uniform.

`Gen` is a lang item (§16.3.2): a deterministic pseudo-random generator with a **size** parameter. Its fields are private.

| Signature | Effects | Panics | Result |
|---|---|---|---|
| `fn from_seed(seed: U64, size: U32) -> Gen` | — | — | a generator whose state is the xoshiro256** state seeded from `seed` by SplitMix64 |
| `fn next_u64(self: borrow mut Self) -> U64` | — | — | the next output of xoshiro256** |
| `fn below(self: borrow mut Self, bound: U64) -> U64` | — | `ExplicitPanic`, message `Gen.below: bound is zero`, if `bound == 0` | `⌊d × bound / 2^64⌋` for the next draw `d`, computed exactly |
| `fn size(self: borrow Self) -> U32` | — | — | the current size |
| `fn set_size(self: borrow mut Self, size: U32) -> U32` | — | — | sets the size and returns the old one |

The algorithms are fixed so that a seed reproduces the same values on every target and in every profile (§16.1.5, P8):

- **SplitMix64.** State `z`, initially `seed`. Each step adds `0x9E37_79B9_7F4A_7C15` to `z` (wrapping), then computes `x = z`, `x = (x ^ (x >> 30)) × 0xBF58_476D_1CE4_E5B9`, `x = (x ^ (x >> 27)) × 0x94D0_49BB_1331_11EB`, `x = x ^ (x >> 31)`, with wrapping multiplication. The four words of the xoshiro256** state are the outputs of four consecutive steps.
- **xoshiro256\*\*.** With state `s0, s1, s2, s3`: the result is `rotl(s1 × 5, 7) × 9`; then `t = s1 << 17`, `s2 ^= s0`, `s3 ^= s1`, `s1 ^= s2`, `s0 ^= s3`, `s2 ^= t`, `s3 = rotl(s3, 45)`, all wrapping, on `U64`.

**Library impls of `Arbitrary`.** Each impl draws from `gen` in the order given.

| Type | Generated value |
|---|---|
| `Bool` | `gen.next_u64() >> 63 == 1` |
| the integer types and `Byte` | with probability 1/8 an edge value, chosen by `below` among `0`, `1`, `<X>_MIN`, `<X>_MAX` (§16.8.1) and, for signed types, `-1`. Otherwise, with `w` the number of value bits (the width, minus one for signed types) and `k = min(size, w)`: `v = next_u64()` masked to its low `k` bits; for a signed type, then `below(2) == 1` negates `v` |
| `F32`, `F64` | with probability 1/8 an edge value, chosen by `below` among `0.0`, `-0.0`, `1.0`, `-1.0`, `<F>_MIN_POSITIVE`, `<F>_MAX`, `<F>_INFINITY`, `<F>_NEG_INFINITY` and `<F>_NAN` (§16.8.1). Otherwise `(2u − 1) × max(size, 1)` rounded to nearest, where `u = (next_u64() >> 11) / 2^53` |
| `Char` | with probability 1/2 the ASCII character `below(128)`; otherwise `v = below(0x10F800)`, giving the scalar value `v` if `v < 0xD800` and `v + 0x800` otherwise |
| `()` | `()` |
| `Option<T>` | `None` if `gen.size() == 0`; otherwise `None` with probability 1/8, else `Some(T.arbitrary(gen))` |
| `Result<T, E>` | `Err(E.arbitrary(gen))` with probability 1/4, else `Ok(T.arbitrary(gen))` |
| `Box<T>` | `Box.new(T.arbitrary(gen))`; `alloc` |
| `Vec<T>` | a length `below(size + 1)`, then that many elements in order; `alloc` |
| `String` | a length `below(size + 1)`, then that many `Char`s; `alloc` |

A probability `p = 1/k` means "`gen.below(k) == 0`". There are no impls for `Str`, `Slice<T>`, borrows, raw pointers or function types. A property that needs one generates an owned value and borrows it.

---

## 16.18 Diagnostics

The codes defined by this chapter are `SEALED_TRAIT_IMPL`, `UNKNOWN_FORMAT_SPEC`, `FORMAT_SPEC_ARGUMENT` and `FORMAT_SPEC_NOT_APPLICABLE`. Every other code in the table is defined in the chapter named in its row and is listed here for the library situations in which it arises.

| Code | Severity | Description |
|---|---|---|
| `ARENA_REQUIRES_COPY` | error | defined in [07](07-ownership-and-borrowing.md) §7.15.2; `Arena.alloc` or `Arena.alloc_slice` with a non-`Copy` element type (§16.15.5) |
| `DROP_HAS_EFFECTS` | error | defined in [08](08-effects-and-capabilities.md) §8.6.3; a `Drop.drop` impl with effects (§16.6.4) |
| `EXPLICIT_DROP_CALL` | error | defined in [07](07-ownership-and-borrowing.md) §7.13.6; a call of `Drop.drop`, whose replacement is `core.mem.drop` (§16.6.4) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; a library item used with a compiler that does not implement its stage (§16.1.6) |
| `FORMAT_NOT_IMPLEMENTED` | error | defined in [06](06-expressions-and-statements.md) §6.20.3; an interpolated type without `Format`. When it is reported, no specifier diagnostic is reported for the same interpolation (§16.14.3) |
| `FORMAT_SPEC_ARGUMENT` | error | a format specifier with a missing or unexpected argument, or with an argument outside `0..=65535` (§16.14.3) |
| `FORMAT_SPEC_NOT_APPLICABLE` | error | a known format specifier applied to a type that does not implement the marker trait it requires (`FormatInt`, `FormatFloat` or `FormatPad`); data: `spec`, `type`, `required_trait` (§16.14.3) |
| `INHERENT_IMPL_FOREIGN_TYPE` | error | defined in [05](05-types.md) §5.9.2; not reported for the inherent impls of `sai:core` and `sai:alloc` on built-in types and lang-item types (§16.1.1 item 4) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [01](01-introduction.md) §1.10; the library source lacks a lang item of a stage the compiler claims, or declares it with the wrong kind (§16.3.1) |
| `INVALID_FORMAT_SPEC` | error | defined in [02](02-lexical-structure.md) §2.6.5; a format specifier whose form is not `name` or `name(N)`, reported by the lexer (§16.14.3) |
| `MANUAL_COPY_IMPL` | error | defined in [05](05-types.md) §5.9.1; a hand-written `impl Copy` (§16.3.5, §16.5.2) |
| `MANUAL_FN_IMPL` | error | defined in [05](05-types.md) §5.9.1; a hand-written impl of `Fn`, `FnMut` or `FnOnce` (§16.3.5, §16.6.3) |
| `MOVE_OUT_OF_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.17; reading `*p` by value through a raw pointer when the pointee is not `Copy` (§16.12.2) |
| `NOT_ITERABLE` | error | defined in [06](06-expressions-and-statements.md) §6.16.3; a `for` over `RangeTo`, `RangeToInclusive` or `RangeFull`, over a range whose element type is not `Step`, or over a type that implements `Iterator` but not `IntoIterator` (N-J-19) (§16.6.6, §16.11.1) |
| `OVERLAPPING_IMPLS` | error | defined in [05](05-types.md) §5.10.2; an impl of `ToString` other than the blanket impl of `sai:alloc` (§16.14.7) |
| `SEALED_TRAIT_IMPL` | error | an impl of a sealed trait (`Integer`, `Float`, `Step`, `SliceRange`, `FormatInt`, `FormatFloat`) outside `sai:core`; data: `trait` (§16.3.5) |
| `SHADOWS_PRELUDE` | warning (lint) | defined in [04](04-modules-and-names.md) §4.4.4; a module-level binding of a prelude name (§16.3.1, §16.4) |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | defined in [08](08-effects-and-capabilities.md) §8.6.3; an impl method of a fundamental trait whose effects exceed the trait's effect bound (§16.5.1) |
| `TRANSMUTE_SIZE_MISMATCH` | error | defined in [12](12-unsafe-and-ffi.md) §12.5.2; `transmute` between types whose sizes differ or are not known on the generic signature (§16.12.1) |
| `UNKNOWN_FORMAT_SPEC` | error | a format specifier name that is not one of `debug`, `hex`, `upper_hex`, `bin`, `oct`, `exp`, `prec`, `pad`, `left` and `zpad`; data: `name` (§16.14.3) |
| `UNUSED_MUST_USE` | error | defined in [06](06-expressions-and-statements.md) §6.25.2; a discarded `Result` or future returned by a library function (§16.7.2, §16.13.1) |

---

## 16.19 Decisions introduced by this chapter

### N-J-6 Inherent impls of the implicit packages

- **Rule:** `sai:core` and `sai:alloc` count as one package for inherent impls as they do for coherence. They may declare inherent impls on the primitive types, on `Array<T, N>` and on every lang-item type of either package, and the uniqueness of inherent item names is checked across both together. `INHERENT_IMPL_FOREIGN_TYPE` still applies to every other package (§16.1.1 item 4).
- **Rationale:** methods such as `I32.checked_add`, `Str.len` and `Slice.to_vec` must live somewhere, and the primitive types are declared by no package. Treating the two implicit packages as one keeps `String` (alloc) and `Str` (core) on one footing without opening primitive types to other packages (P8, P9).

### N-J-7 Library effects are declared

- **Rule:** Every `pub` function of `sai:core` and `sai:alloc` with a non-empty signature set declares its effect set explicitly, exactly as listed in this chapter. An implementation MUST NOT declare a larger or smaller set. Deallocation is not an effect (§16.1.3).
- **Rationale:** effects are part of a signature and must be visible in it (P1, P2). Inferred effects of library bodies would make a program's effect requirements depend on how the library is written, and could change between toolchain versions (P8).

### N-J-8 Location of library panics

- **Rule:** A panic raised by a library body is located at the call expression, in the innermost frame outside `sai:core` and `sai:alloc`, through which control entered the library. A panic in a user closure called by the library keeps the closure's location. Every library panic message is fixed by this chapter (§16.1.4).
- **Rationale:** the location must point at code the programmer can change, not at library internals (P8). Fixed messages make panic output identical across conforming implementations (determinism principle, P8).

### N-J-9 Stages of library items

- **Rule:** Each library item has the stage given in §16.1.6. A compiler that claims a stage MUST provide every item of that stage, and use of an item of a later stage is `FEATURE_NOT_IMPLEMENTED` with the item's canonical path (04 §4.9) as the feature name.
- **Rationale:** the stages of the language (README §4) must fix which library surface is available, or a Stage 1 conformance claim would say nothing about the library (P8). Reusing `FEATURE_NOT_IMPLEMENTED` keeps one diagnostic for staged features (01 N-J-5, P9).

### N-J-10 Prelude contents

- **Rule:** The prelude contains exactly the bindings of the table in §16.4: the built-in and reserved type names, `Array`, `Str`, `Slice`, `SliceMut`, `Option` and `Result` with their variants, `Ordering`, `String`, `Vec`, `Bytes`, `Box`, the traits of D-TYPE-08, the iterator traits, `From`, `TryFrom`, `Format`, `ToString`, `Future`, `Stream`, `Arbitrary`, `panic` and `unreachable`. Nothing else is in the prelude, and only the `Option` and `Result` variants are bound unqualified.
- **Rationale:** the prelude holds what ordinary code names without thinking about it. Items for implementers and unsafe code (`Writer`, `Ptr`, `Context`) are imported explicitly, so their use is visible (P1). A closed list keeps resolution predictable (P8), and 04 §4.4.4 lets a later version add names without breaking programs.

### N-J-11 Sealed traits

- **Rule:** `Integer`, `Float`, `Step`, `SliceRange`, `FormatInt` and `FormatFloat` are sealed. Only `sai:core` implements them, for exactly the types this chapter lists. An impl in any other package is `SEALED_TRAIT_IMPL` (§16.3.5, §16.14.4).
- **Rationale:** these traits describe closed sets of built-in types. Generic code and the format specifier rules rely on the set being closed, and an open set would let a dependency change the meaning of `0..n` or `:hex` (P8).

### N-J-12 Effect bounds of the fundamental traits

- **Rule:** `PartialEq.eq`, the methods of `PartialOrd`, `Ord`, `Hash` and `Hasher` are declared `effects {}`. `Clone`, `Default`, `From`, `TryFrom`, the operator traits, `Index`, `IndexMut`, the iterator traits, `Format`, `Writer`, `ToString` and `Arbitrary` have no bound, so their effects are latent (§16.5.1).
- **Rationale:** comparisons and hashes appear in contracts, sorting and hashing, where their results must not depend on or change the world (P8). The other traits must be able to allocate, because cloning a `String` or formatting into one allocates. Their effects stay tracked as latent effects (08 §8.8), so the allocation remains visible at each use (P1, P5).

### N-J-13 No reflexive or blanket conversions

- **Rule:** There is no `impl<T> From<T> for T` and no blanket impl relating `From` and `TryFrom`. Every conversion that exists is listed in this chapter (§16.5.7).
- **Rationale:** the overlap check ignores bounds (05 §5.10.2), so a blanket impl would forbid most user conversions. A listed set lets a reader see every conversion (P1, P7).

### N-J-14 Range indexing through a temporary place

- **Rule:** The range `Index` impls of `Str`, `Slice`, `SliceMut`, `Array`, `String` and `Vec` are intrinsics. When trait-based indexing selects one, `*Index.index(borrow e, q)` computes the view `e.slice(q)`, stores it in a temporary and uses the temporary as the place. The view has the region of the borrow of `e` (§16.6.2).
- **Rationale:** D-EXPR-10 requires `s[1..3]` to be a `Str`, while 06 defines indexing as a place behind `Index.index`. A view is a value, not a place inside `s`, so a temporary bridges the two without a second indexing mechanism (P9). It is a library rule because the impls are library items.

### N-J-15 `unwrap` messages without the payload

- **Rule:** `Result.unwrap` and `Result.unwrap_err` panic with fixed messages that do not include the payload, so they need no `Format` bound (§16.7.2).
- **Rationale:** a `Format` bound on `unwrap` would make it unavailable for every error type without `Format`, and the panic message would depend on user formatting code (P8). `expect` and `match` let the program choose the message explicitly (P1).

### N-J-16 Float-to-integer conversion saturates

- **Rule:** `trunc_to` truncates toward zero and saturates to the target range, and NaN converts to `0` (§16.8.4).
- **Rationale:** the conversion must be total and deterministic (P8). Saturation matches the Wasm `trunc_sat` instructions, so it needs no runtime support (P7).

### N-J-17 No transcendental float functions

- **Rule:** `sai:core` has no transcendental functions (`sin`, `exp`, `ln`, `powf` and similar) and no fused multiply-add. Every float function of this chapter is an IEEE basic operation or is exactly specified (§16.8.4).
- **Rationale:** Wasm provides none of them, and a software version whose results differ in the last place between versions or targets would break the determinism principle (P8). Leaving them to an ordinary package keeps `sai:core` small (P7).

### N-J-18 Sorting

- **Rule:** `sort`, `sort_by` and `sort_by_key` are stable, do not allocate, and use O(n log² n) comparisons and moves in the worst case. They are available in every profile (§16.10.1).
- **Rationale:** for a consistent `Ord`, a stable sort has exactly one result, so the output does not depend on the algorithm an implementation chooses (P8). Not allocating makes sorting usable in `tiny` builds and without the `alloc` effect (P5, P7).

### N-J-19 No blanket `IntoIterator`

- **Rule:** There is no `impl<I: Iterator> IntoIterator for I`. Every iterator type of this chapter implements `IntoIterator` for itself, and a program's iterator does the same to be used in `for` (§16.11.1).
- **Rationale:** the overlap check ignores bounds (05 §5.10.2), so the blanket impl would overlap every other `IntoIterator` impl, including those of `Vec<T>` and its borrows (P8). One explicit line per iterator type is the price (P1).

### N-J-20 Infallible writers

- **Rule:** `Writer.write_str` returns nothing. A writer either stores all of its input, grows, truncates and records the truncation, or panics. Host writers ignore host write errors (§16.14.1, §16.16.3).
- **Rationale:** a fallible writer would make every `Format` impl and every f-string propagate an error that the common writers (`String`, `FixedBuf`) never produce (P9). Programs that must observe I/O errors use the I/O APIs of their packages, which return `Result` (P1).

### N-J-21 Format specifier rules

- **Rule:** The specifier names are `debug`, `hex`, `upper_hex`, `bin`, `oct`, `exp`, `prec(N)`, `pad(N)`, `left(N)` and `zpad(N)`. An unknown name is `UNKNOWN_FORMAT_SPEC`. A wrong argument, or `N` outside `0..=65535`, is `FORMAT_SPEC_ARGUMENT`. A type without the required marker trait is `FORMAT_SPEC_NOT_APPLICABLE`, unless `FORMAT_NOT_IMPLEMENTED` applies. An interpolation has at most one specifier, and widths count `Char`s without truncation (§16.14.3).
- **Rationale:** checking specifiers against marker traits at compile time catches `{name:hex}` on a `String` before the program runs (P2). A small closed set of specifiers, with no combinations, keeps f-strings readable (P9).

### N-J-22 Float formatting

- **Rule:** `Plain` and `Debug` write the shortest round-trip digits, in fixed notation for zero and for `1e-5 ≤ |x| < 1e16` (with `.0` for integral values) and in scientific notation otherwise. `Exp` always uses scientific notation, `Prec(n)` rounds the exact value with ties to even, and NaN and the infinities are written `NaN`, `inf` and `-inf` (§16.14.6).
- **Rationale:** the output must be a deterministic function of the value, identical on every implementation (P8). Shortest round-trip digits are both exact and readable (P9).

### N-J-23 Region-polymorphic constructors instead of `From`

- **Rule:** Because a `From<borrow T>`-like trait argument can only have region `'static` (07 N-B-12), the conversions from views are inherent functions: `Vec.from_slice`, `Slice.to_vec`, `String.from_str`, `Ptr.from` and `PtrMut.from`. `From<Str> for String` exists and accepts `Str<'static>` only. There is no `From<Slice<T>> for Vec<T>` (§16.12.2, §16.15.3, §16.15.4).
- **Rationale:** a conversion that silently accepted only literals would surprise users (P8). An inherent function accepts every region and says what it does (P1).

### N-J-24 Region desugaring

- **Rule:** `region r { body }` is exactly a block that binds a fresh arena from the package-private `Arena.region_new()`, binds `r` to a borrow of it, and evaluates `body`. `r` is dropped before the arena. `Arena` has no public constructor (§16.15.5).
- **Rationale:** 07 defines the region rules in terms of one arena per `region` block. Making the block the only way to obtain an arena ties every arena borrow to a lexical scope that the borrow checker already understands (P2, P9).

### N-J-25 Normative items of other standard packages

- **Rule:** In the standard packages other than `sai:core` and `sai:alloc`, only the items listed in §16.16 are normative. Every release that a toolchain ships MUST contain them with exactly the listed signatures and effects. Every other item is defined by the package's own documentation and versioned with the package (§16.16.1).
- **Rationale:** the specification refers to some items of these packages, for example the signature of `main` and the `sai:async` structured concurrency API, so those must be fixed (P8). The rest is library design, which the language does not freeze (P7).

### N-J-26 Designated items

- **Rule:** `sai:process.Env`, `sai:process.ExitCode`, the package-private `Env.for_entry`, and `sai:async.scope` are designated items. The compiler knows them by canonical path, looks them up only when their package is a dependency of the build, and does not treat them as lang items. When their package is not a dependency, a program that needs them (for example a command whose `main` names `process.Env`) has an ordinary resolution error, not an internal compiler error (§16.16.1).
- **Rationale:** D-PKG-04 makes desugarings depend only on `sai:core` and `sai:alloc`, yet the entry point (D-CAP-02) and the typing of `scope` (11) need known items. Designated items give the compiler that knowledge without making the standard packages implicit dependencies (P5, P7).

---
