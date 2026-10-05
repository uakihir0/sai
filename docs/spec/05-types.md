# 05. Types

**Status:** Normative
**Stage:** 0 (type syntax, declarations, type checking of non-generic programs); 1 (code generation for primitive, record, enum, `Option` and `Result` types); 2 (generics, traits, impls, coherence, closures, `@derive`, `Copy`/`Clone`/`Drop`); 3 (refined types and refinement clauses, with [09](09-contracts-and-refinements.md); effects in function types); 4 (resources); 5 (async function types)
**Related:** [01 Introduction](01-introduction.md), [02 Lexical Structure](02-lexical-structure.md), [03 Grammar](03-grammar.md), [04 Modules and Names](04-modules-and-names.md), [06 Expressions and Statements](06-expressions-and-statements.md), [07 Ownership and Borrowing](07-ownership-and-borrowing.md), [08 Effects and Capabilities](08-effects-and-capabilities.md), [09 Contracts and Refinements](09-contracts-and-refinements.md), [13 Components and WIT](13-components-and-wit.md), [15 Attributes and Lints](15-attributes-and-lints.md), [16 Core Library](16-core-library.md), [impl/03 Name Resolution and Type Checking](../impl/03-name-resolution-and-type-checking.md), [impl/06 Wasm Lowering and Layout](../impl/06-wasm-lowering-and-layout.md), [decisions](../design/decisions.md)

This chapter defines SAI's static type system. It covers the forms of types and their well-formedness, the primitive and built-in compound types, function and closure types, and type declarations (records, enums, distinct types, refined types, aliases, resources). It also covers generics, traits, impls, coherence, trait solving, method resolution, `@derive`, the `Copy`/`Clone`/`Drop` rules, type equality, coercions, and type inference. Typing rules for individual expressions, patterns and statements are in [06](06-expressions-and-statements.md); this chapter supplies the relations they use. Regions, variance, moves and borrow checking are in [07](07-ownership-and-borrowing.md). Effects in types are in [08](08-effects-and-capabilities.md). Checking of refinement predicates is in [09](09-contracts-and-refinements.md). Memory layout is in [impl/06](../impl/06-wasm-lowering-and-layout.md).

## 5.1 Conventions

### 5.1.1 Judgments

| Judgment | Meaning |
|---|---|
| `Δ ⊢ τ wf` | type `τ` is well-formed in type environment `Δ` (§5.17) |
| `Δ ⊢ τ ≡ σ` | `τ` and `σ` are the same type (§5.14) |
| `Δ ⊢ τ <: σ` | `τ` is a subtype of `σ`; only regions differ ([07](07-ownership-and-borrowing.md)) |
| `Δ ⊢ τ ⇝ σ` | a value of type `τ` coerces to type `σ` (§5.15) |
| `Δ ⊢ τ : Tr<σ̄>` | the trait obligation holds (§5.11) |
| `Δ ⊢ π ↠ τ` | the projection `π` normalizes to `τ` (§5.8.5) |
| `Δ; Γ ⊢ e ⇒ τ` | expression `e` synthesizes type `τ` ([06](06-expressions-and-statements.md)) |
| `Δ; Γ ⊢ e ⇐ τ` | expression `e` checks against type `τ` ([06](06-expressions-and-statements.md)) |

The **type environment** `Δ` holds:

- the generic parameters in scope, each a type parameter `T` or a const parameter `const N: κ`;
- the **predicates** in scope: trait bounds `τ : Tr<σ̄>` and associated-type equalities `π = τ`, taken from generic parameter lists and `where` clauses (§5.7.4);
- the region parameters of [07](07-ownership-and-borrowing.md);
- `Self`, inside a trait or impl.

`Γ` maps local bindings to types ([06](06-expressions-and-statements.md)). Effects (`! E`) are omitted from the rules of this chapter; [08](08-effects-and-capabilities.md) adds them.

### 5.1.2 Semantic types

Source type syntax (03 §3.15) is resolved into **semantic types**. All rules in this chapter work on semantic types:

```text
τ, σ ::= P                                  primitive: I8 I16 I32 I64 U8 U16 U32 U64 ISize USize F32 F64 Bool Byte Char
       | ()                                 unit (the empty tuple)
       | Never                              the empty type
       | (τ1, …, τn)            n ≥ 1       tuple; (τ,) for n = 1
       | Array<τ, c>                        array of c elements, c a const of type USize
       | borrow ρ τ | borrow mut ρ τ        shared and exclusive borrow with region ρ
       | N<τ̄; c̄>                            nominal type: declaration N applied to type and const arguments
       | fnptr(unsafe?, τ̄ → τ, E)           function pointer type with effect set E
       | fndef(f, τ̄; c̄)                     function item type: item f at the given generic arguments
       | closure(k, τ̄)                      closure type: closure expression k in the instantiation τ̄ of its enclosing item
       | T                                  type parameter
       | <τ as Tr<σ̄>>.A                     associated-type projection
       | α | ι | φ                           inference variables: general, integer-literal, float-literal (§5.16)
```

- `Int` and `UInt` are not semantic types. They are names for `I64` and `U64` (D-TYPE-01).
- Type aliases, including `Bytes` (`Vec<Byte>`), are expanded during resolution and never appear in semantic types (§5.6.5).
- `owned τ` resolves to `τ` (D-OWN-01).
- `Str`, `Slice<T>`, `SliceMut<T>`, `String`, `Vec<T>` and `Box<T>` are nominal types declared in `sai:core` and `sai:alloc` as lang items ([16](16-core-library.md)). Their special properties are listed in §5.4.
- Regions `ρ` are never written in source (D-OWN-03). Where they do not matter, rules omit them. `borrow τ` stands for `borrow ρ τ` with some `ρ`.

The **head** of a type is its outermost constructor: the primitive, `()`, `Never`, the tuple arity, `Array`, `borrow`, `borrow mut`, the nominal declaration `N`, `fnptr`, the item `f`, the closure `k`, the parameter `T`, or the projection. An inference variable has no head until it is resolved. **Shallow resolution** of a type replaces a resolved inference variable at the top by its value, repeatedly.

### 5.1.3 Kinds of type

| Category | Types | Defined in |
|---|---|---|
| structural | tuples, `Array`, `borrow`, `borrow mut`, function pointers | D-TYPE-02 |
| nominal | records, enums, distinct types, refined types, resources | D-TYPE-02, D-TYPE-03 |
| primitive | the 15 primitive types, `()`, `Never` | D-TYPE-01 |
| anonymous | function item types, closure types | D-TYPE-09 |
| view | `Str`, `Slice<T>`, `SliceMut<T>`, `borrow T`, `borrow mut T` | D-TYPE-02 |

Two structural types are equal when their components are equal. Two nominal types are equal only when they come from the same declaration and have equal arguments (§5.14).

## 5.2 Type syntax

### 5.2.1 Resolving a type expression

A `Type` (03 §3.15) is resolved to a semantic type as follows:

| Syntax | Resolution |
|---|---|
| `TypePath` | The path is resolved by [04](04-modules-and-names.md) §4.5.2 in type position. It must denote a primitive, a nominal type, an alias, a type parameter, `Self` or an associated type. A function, module, trait, constant or effect is `WRONG_NAME_KIND` (04). In particular a trait is never a type: there is no `dyn` (D-TYPE-02). Generic arguments are checked by §5.2.2. |
| `Self` | In an impl, the impl's self type. In a trait, the trait's implicit `Self` parameter. Elsewhere `SELF_OUTSIDE_IMPL`. |
| `Self.A`, `T.A`, `T.A.B` | an associated-type projection (§5.8.5) |
| `borrow T`, `borrow mut T` | `borrow ρ T`, `borrow mut ρ T`, with ρ given by [07](07-ownership-and-borrowing.md) (elision or inference) |
| `owned T` | `T` |
| `()` / `(T,)` / `(T1, …, Tn)` | unit / 1-tuple / n-tuple |
| `(T)` | `T` (grouping) |
| `_` | a fresh inference variable `α`. Allowed only in types written inside function bodies and in generic arguments of expressions (`INFER_IN_SIGNATURE`, 03). |
| `fn(T1, …, Tn) -> R` | `fnptr(safe, T̄ → R, ∅)`. An omitted `-> R` means `-> ()`. An omitted effects clause means the empty set: function pointers without a clause are pure (D-EFF-03). |
| `fn(T̄) -> R effects { E }` | `fnptr(safe, T̄ → R, E)`, with `E` resolved by [08](08-effects-and-capabilities.md) (Stage 3) |
| `unsafe fn(T̄) -> R …` | `fnptr(unsafe, …)` ([12](12-unsafe-and-ffi.md)) |

Function item types and closure types have no syntax. They are the types of function-item and closure expressions ([06](06-expressions-and-statements.md)).

### 5.2.2 Generic arguments

For a reference to a generic entity with parameters `P1 … Pn` and arguments `A1 … Am` (on a type path, an expression path, a method call, or a trait reference):

1. **Count.** There are no default arguments. In type position and in trait references, m MUST equal n. In expression position, either m = 0 (all arguments inferred) or m = n (D-SYN-04, N-A-21). Any other count is `WRONG_NUMBER_OF_GENERIC_ARGS`, which states the expected count. Associated-type bindings `A = T` are not counted.
2. **Kind.** A type parameter takes a type argument. A const parameter `const N: κ` takes a const argument. A mismatch is `GENERIC_ARG_KIND_MISMATCH`.
3. **Const arguments** (D-TYPE-07). A const argument is one of:
   - an `INT`, or `-` followed by an `INT`, whose value lies in the range of `κ`. Otherwise `LITERAL_OUT_OF_RANGE` (02). `κ` MUST be an integer type for this form;
   - a `CHAR` (`κ = Char`), `true` or `false` (`κ = Bool`);
   - the name of a const parameter in scope whose type is `κ`.

   Anything else, including the name of a `const` item and any arithmetic, is `CONST_ARG_NOT_SIMPLE`. A literal of the wrong type for `κ` is `TYPE_MISMATCH`.
4. **Inferred arguments.** In expression position, an argument written `_` is a fresh inference variable (or a fresh const inference variable for a const parameter).
5. **Associated-type bindings** `A = T` are allowed only in a trait reference used as a bound (03 §3.15). Anywhere else they are `ASSOC_TYPE_BINDING_OUTSIDE_BOUND`. The trait must have an associated type `A`, otherwise `UNKNOWN_ASSOC_TYPE`. Each associated type may be bound at most once per bound (`DUPLICATE_BINDING`, 04).

After substitution, the bounds that the entity declares on its parameters MUST hold. This is checked as part of well-formedness (§5.17) or of the call ([06](06-expressions-and-statements.md)). A failure is `UNSATISFIED_BOUND`.

## 5.3 Primitive types

### 5.3.1 The primitive table

| Type | Values | Copy | Notes |
|---|---|---|---|
| `I8`, `I16`, `I32`, `I64` | two's-complement integers, −2^(w−1) … 2^(w−1)−1 for w = 8, 16, 32, 64 | yes | `Int` is `I64` |
| `U8`, `U16`, `U32`, `U64` | 0 … 2^w − 1 | yes | `UInt` is `U64` |
| `ISize`, `USize` | as `I32`/`U32` on every wasm32 target; the width is the target's pointer width ([targets](../targets/targets-and-profiles.md)) | yes | `USize` is the type of lengths and indices. Neither may appear in a component's public interface ([13](13-components-and-wit.md)). |
| `F32`, `F64` | IEEE 754 binary32 and binary64, including signed zeros, infinities and NaN | yes | arithmetic semantics in [06](06-expressions-and-statements.md) |
| `Bool` | `false`, `true` | yes | |
| `Byte` | 256 values | yes | same representation as `U8` but a distinct type: comparison and bitwise operators only, no arithmetic; explicit conversions ([16](16-core-library.md)) (D-TYPE-01) |
| `Char` | Unicode scalar values U+0000…U+D7FF and U+E000…U+10FFFF, 32 bits | yes | comparison only, no arithmetic (D-TYPE-01) |
| `()` | one value, `()` | yes | zero-sized; the type of statements, blocks without a tail, and functions with no `->` |
| `Never` | no values | yes | the type of diverging expressions; coerces to every type (§5.15) |

- The names in this table, and `Int`, `UInt`, are reserved type names that no declaration may rebind (04 §4.4.4).
- `Int` and `UInt` are transparent: `Int` and `I64` are the same type everywhere, including in diagnostics, which print `I64` with the note "`Int` is `I64`".
- The **integer types** are the ten types `I8 … I64`, `U8 … U64`, `ISize`, `USize`. The **float types** are `F32` and `F64`. The **numeric types** are the integer and float types. `Byte` and `Char` are neither.
- Which operators apply to which primitive is defined by [06](06-expressions-and-statements.md) §6.8. Binary arithmetic needs the same type on both sides, and there is no implicit numeric conversion (D-TYPE-06). Conversions are the functions `T.from`, `T.try_from`, `wrapping_to<T>()` and friends ([16](16-core-library.md)).
- Layouts are in [impl/06](../impl/06-wasm-lowering-and-layout.md). Only the facts stated in decisions are normative here: `Byte` has `U8`'s representation, `Char` is 32 bits, `()` and `Never` are zero-sized.

### 5.3.2 Literal types

| Literal | Type | Rule |
|---|---|---|
| integer `INT` | an integer type | checked against an integer type: that type. Synthesized: a fresh integer-literal variable `ι` (§5.16.2). Never `Byte`, `Char`, a float type, a distinct type or a refined type (but see [06](06-expressions-and-statements.md) §6.3.1 for refined integer types). |
| `-` directly followed by `INT` | as `INT` | the pair is one negative constant (02 §2.7, D-LEX-05) |
| float `FLOAT` | a float type | checked against `F32` or `F64`: that type. Synthesized: a fresh float-literal variable `φ`. |
| `true`, `false` | `Bool` | |
| `CHAR` (`'a'`) | `Char` | |
| `BYTE` (`b'a'`) | `Byte` | |
| `STRING` | `Str` with region `'static` | (02 §2.8) |
| `BYTE_STRING` (`b"…"`) | `Slice<Byte>` with region `'static` | (D-LEX-05) |
| `FSTRING` | `String` (Stage 3) | ([06](06-expressions-and-statements.md)) |
| `()` | `()` | |

An integer-literal variable left unresolved at the end of inference becomes `I64`, and a float-literal variable becomes `F64` (D-LEX-05, D-TYPE-05, §5.16.5). After the type is known, a value outside its range is `LITERAL_OUT_OF_RANGE` (02).

```sai
fn literals() {
    let a = 1                    // I64 (default)
    let b: U8 = 255              // U8
    let c: I8 = -128             // one negative constant: OK
    let d = 1.5                  // F64 (default)
    let e: Byte = b'x'
    let f: U8 = 256              // rejected: LITERAL_OUT_OF_RANGE
    let g: Byte = 7              // rejected: TYPE_MISMATCH (integer literals never have type Byte)
}
```

## 5.4 Compound and library types

### 5.4.1 Tuples

- `()` is the unit type. `(T,)` is a 1-tuple and `(T1, …, Tn)` an n-tuple. `(T)` is not a tuple.
- Tuple elements are accessed with `.0`, `.1`, … ([06](06-expressions-and-statements.md)).
- A tuple type is well-formed when each element type is. Elements may be any well-formed types, including views and non-`Copy` types.
- Tuples have no arity limit. Their trait impls are compiler-provided for every arity (§5.11.4).

### 5.4.2 Arrays

- `Array<T, N>` holds exactly `N` values of type `T`. `N` is a const argument of type `USize` (§5.2.2): a literal or a const parameter of type `USize`. `Array<T, 0>` is allowed.
- `Array` is structural (D-TYPE-02). Two array types are equal iff their element types are equal and their lengths are the same value. A length that is a const parameter is equal only to the same const parameter.
- There is no implicit conversion from `Array<T, N>` or `borrow Array<T, N>` to `Slice<T>`. A slice is obtained with an explicit method ([16](16-core-library.md)) (P1).

### 5.4.3 Views

The view types carry exactly one region each and are never dynamically sized (D-TYPE-02):

| View | Meaning | Copy |
|---|---|---|
| `borrow T` | shared borrow of a `T` | yes |
| `borrow mut T` | exclusive borrow of a `T` | no |
| `Str` | shared view of valid UTF-8 bytes, represented as (pointer, length) | yes |
| `Slice<T>` | shared view of a contiguous sequence of `T`, (pointer, length) | yes |
| `SliceMut<T>` | exclusive view of a contiguous sequence of `T`, (pointer, length) | no |

- `Str`, `Slice<T>` and `SliceMut<T>` are nominal lang-item types of `sai:core` with one implicit region parameter ([16](16-core-library.md), D-OWN-03). They behave like any other nominal type except for the coercion `SliceMut<T> ⇝ Slice<T>` (§5.15) and indexing ([06](06-expressions-and-statements.md)).
- There are no dynamically sized types. `Str` is a view value, not an unsized `str`. `borrow Str` is a borrow of a view, which is rarely needed.
- A type definition that syntactically contains a view gets exactly one implicit region parameter (D-OWN-03, [07](07-ownership-and-borrowing.md)).

### 5.4.4 Owned library types

`String`, `Vec<T>` and `Box<T>` are nominal lang-item types of `sai:alloc` ([16](16-core-library.md)). They are ordinary nominal types with these additions:

- `Box<T>` is dereferenced by `*` and by auto-deref ([06](06-expressions-and-statements.md) §6.9, D-OWN-02), and is a fundamental type for coherence (§5.10.1). A non-`Copy` value cannot be moved out of `*b`: that is `MOVE_OUT_OF_BOX`, and `Box.into_inner(b)` takes the value ([07](07-ownership-and-borrowing.md) §7.3.5, N-B-2).
- `String + Str` and `Str + Str` use `Add` impls in `sai:alloc` (D-EXPR-13).
- `Bytes` is a transparent alias of `Vec<Byte>` (D-TYPE-02).
- There is no coercion from `String` to `Str`, from `Vec<T>` to `Slice<T>`, or from `Box<T>` to `borrow T` (D-TYPE-04). Write `s.as_str()`, `v.as_slice()`, `borrow *b`.

### 5.4.5 `Option` and `Result`

`Option<T>` and `Result<T, E>` are ordinary generic enums of `sai:core` ([16](16-core-library.md)):

```sai
pub enum Option<T> {
    None
    Some(T)
}

@must_use
pub enum Result<T, E> {
    Ok(T)
    Err(E)
}
```

They have no special typing rules. Their only special properties come from elsewhere: the prelude binds `Option`, `Some`, `None`, `Result`, `Ok` and `Err` ([16](16-core-library.md)); `Result` is `@must_use` (D-EXPR-01, [15](15-attributes-and-lints.md)); and the `?` operator works on them ([10](10-errors-and-panics.md)). The variant order shown is normative, because it determines derived orderings (§5.12.3): `None < Some(_)` and `Ok(_) < Err(_)` (N-A-22).

## 5.5 Function types

### 5.5.1 Function item types

- Every function item `f` (module-level, associated, trait method, or extern) has its own anonymous **function item type** `fndef(f, ḡ)`, where `ḡ` are the item's generic arguments, including those of an enclosing impl or trait (D-TYPE-09).
- A function item type is zero-sized, `Copy` and `Clone`.
- A safe, non-`async` function item type implements `Fn`, `FnMut` and `FnOnce` with the item's parameter and return types (§5.5.4). `unsafe` function items implement none of them. They can only be called directly or through an `unsafe fn` pointer ([12](12-unsafe-and-ffi.md)). The types of `async` function items are defined in [11](11-async-and-concurrency.md).
- A function item type coerces to a compatible function pointer type (§5.15, rule C-FnPtr).
- Two function item types are equal iff they denote the same item with equal generic arguments.

### 5.5.2 Function pointer types

- `fn(T1, …, Tn) -> R effects { E }` is the type of a pointer to code with that signature. `unsafe fn(…)` is the unsafe variant.
- Function pointers are `Copy` and `Clone`. A safe function pointer type also implements `Fn`, `FnMut` and `FnOnce`. Function pointers implement no other trait.
- `fnptr(u1, T̄ → R, E1) ≡ fnptr(u2, Ū → S, E2)` iff `u1 = u2`, n = m, each `Ti ≡ Ui`, `R ≡ S`, and `E1 = E2` as sets after the canonicalization of [08](08-effects-and-capabilities.md).
- Effects in function pointer types are Stage 3. Before Stage 3, writing an effects clause on a function pointer type is `FEATURE_NOT_IMPLEMENTED`, and effects are not compared.

### 5.5.3 Closure types

- Each closure expression `k` has its own anonymous **closure type** `closure(k, ḡ)`, where `ḡ` is the instantiation of the generic parameters of the enclosing item (D-TYPE-09). Two closure types are equal iff they come from the same expression in the same instantiation.
- The closure's signature (parameter and return types) is inferred as [06](06-expressions-and-statements.md) §6.17 and §5.16.6 define. Its captures, and the mode of each capture, are inferred as [07](07-ownership-and-borrowing.md) defines.
- Closure types implement the function traits as follows (D-TYPE-09):
  - `FnOnce`, always;
  - `FnMut`, if the body does not move out of any capture;
  - `Fn`, if additionally the body neither assigns to a capture nor borrows one mutably.
- A closure type is `Copy` iff every capture is a `Copy` value, which includes shared borrows. It is `Clone` iff every capture's type is `Clone`. A capture by `borrow mut` makes the closure neither (N-A-31).
- A closure type never coerces to a function pointer type, even when it captures nothing (N-A-31). A function item does.
- Closure types cannot be named. They are passed through generic parameters bounded by `Fn`, `FnMut` or `FnOnce`, and stored in fields whose type is such a parameter. There is no `impl Trait` (D-TYPE-09).

### 5.5.4 The function traits

`Fn`, `FnMut` and `FnOnce` are lang-item traits of `sai:core` ([16](16-core-library.md)). Their receivers are `borrow Self`, `borrow mut Self` and `Self` (D-TYPE-09). `FnOnce` is a supertrait of `FnMut`, and `FnMut` of `Fn`.

- In bounds they are written with the sugar `Fn(A1, …, An) -> R` (03 §3.5). An omitted `-> R` means `-> ()`. The sugar is the only way to write them. A function-trait bound may carry bound effects ([08](08-effects-and-capabilities.md)): `F: Fn(I32) -> I32 effects {}`.
- Only function item types, function pointer types and closure types implement them. A user impl of `Fn`, `FnMut` or `FnOnce` is `MANUAL_FN_IMPL` (N-A-31).
- A call `e(args)` on a value whose type implements a function trait is defined in [06](06-expressions-and-statements.md) §6.5.

## 5.6 Type declarations

Every `type` (record or refined), `enum`, `distinct` and `resource` declaration introduces a new **nominal type constructor** `N` with the declared generic parameters. A `type` with `=` and no `where` introduces an alias instead (§5.6.5). Each declaration is checked under the environment `Δ_N` made of its generic parameters and its `where` clause.

### 5.6.1 Records

```sai
type User {
    id: UserId
    pub name: String
}

type Pair<T> where T: Clone {
    left: T
    right: T
}

type Marker {}
```

- Each field type MUST be well-formed under `Δ_N` (§5.17). Field names MUST be distinct (`DUPLICATE_FIELD`).
- A record with no fields is allowed. It has exactly one value, written `Marker()` ([06](06-expressions-and-statements.md)).
- Construction, functional update, field access and record patterns are defined in [06](06-expressions-and-statements.md) (D-SYN-01). Field visibility is in [04](04-modules-and-names.md) §4.6.3.
- The bounds of `Δ_N` are **not implied** at uses of the type (N-A-28). Every use `Pair<X>` must prove `X: Clone` (§5.17). A function `fn f<T>(p: Pair<T>)` must itself declare `T: Clone`.
- There are no tuple structs (D-SYN-01). A record with positional data is written with named fields or as a single-variant enum.

### 5.6.2 Enums

```sai
enum Shape {
    Circle(F64)
    Rect(width: F64, height: F64)
    Empty
}

enum Level {
    Low = 1
    Mid
    High = 10
}

enum Void {}
```

- Variants are **unit** (`Empty`), **tuple** (`Circle(F64)`, one or more positional payload types) or **record** (`Rect(width: F64, height: F64)`, one or more named fields) (D-SYN-02). Variant names MUST be distinct (`DUPLICATE_VARIANT`). Field names within a record variant MUST be distinct (`DUPLICATE_FIELD`).
- Payload types MUST be well-formed under `Δ_N`.
- An enum with no variants is allowed. It is uninhabited: no value of it can be constructed, and a `match` on it needs no arms ([06](06-expressions-and-statements.md)).
- Variants have the visibility of their enum ([04](04-modules-and-names.md) §4.6.2). They are named `E.V`, or `.V` when the expected type is known ([06](06-expressions-and-statements.md) §6.4).
- **Discriminants** (N-A-23). Every variant has an integer discriminant:
  - In an enum with any tuple or record variant, explicit discriminants are not allowed (`EXPLICIT_DISCRIMINANT_ON_DATA_ENUM`, 03). The discriminant of the i-th variant (counting from 0, in declaration order) is i.
  - In an enum with only unit variants, a variant with `= n` (or `= -n`) has discriminant n. A variant without one has the discriminant of the previous variant plus one, or 0 if it is the first.
  - The **discriminant type** is the integer type `I` named by `@repr(I)`, `I32` for `@repr(C)`, and `I64` otherwise ([15](15-attributes-and-lints.md) §15.9.2, N-A-43). Every discriminant, explicit or implicit, MUST be in that type's range, otherwise `DISCRIMINANT_OUT_OF_RANGE`. Two variants with the same discriminant are `DUPLICATE_DISCRIMINANT`.
  - Discriminants determine the derived orderings (§5.12.3) and the conversions offered by [16](16-core-library.md). Layout is in [impl/06](../impl/06-wasm-lowering-and-layout.md).

In `Level` above the discriminants are 1, 2 and 10.

### 5.6.3 Distinct types

```sai
distinct UserId = U64
pub distinct Money = pub I64
distinct Meters<T> = T
distinct Age = U8 where self <= 150
```

`distinct D<Ḡ> = B` introduces a nominal type `D` whose **base type** is `B` (D-TYPE-03):

- `B` MUST be well-formed under `Δ_D`. Any type is allowed, including views (then `D` gets the implicit region parameter of D-OWN-03), other distinct types, and refined types.
- `D<ḡ> ≢ B[ḡ/Ḡ]`. There is no coercion between them in either direction (§5.15).
- **Constructor.** `D(e)` checks `e ⇐ B` and has type `D` ([06](06-expressions-and-statements.md) §6.18). When `D` is used as a value without a call, it is a function item of type `fn(B) -> D` ([06](06-expressions-and-statements.md) §6.4).
- **Pattern.** `D(p)` matches a `D` and matches `p` against the base value. It is irrefutable if `p` is.
- **Field `.value`.** `d.value` is a place of type `B` ([06](06-expressions-and-statements.md) §6.6).
- **Nothing is inherited** (N-A-24). `D` has no methods, trait impls or operators of `B`. Method resolution does not look through `D` (§5.13). Traits come only from `@derive`, which forwards to `B` (§5.12), or from explicit impls.
- **Literals** never have a distinct type: `Money(0)`, not `0` (D-TYPE-03, R-05).
- **Refinement clause.** `distinct D = B where p` makes `D` also refined. `D(e)` is then a B→R conversion that needs a proof by the SFC, otherwise `REFINEMENT_NOT_PROVEN` ([09](09-contracts-and-refinements.md)). Places of type `D` reached through `.value` are immutable ([06](06-expressions-and-statements.md) N-C-34). A distinct type with a refinement clause cannot be generic (`GENERIC_REFINED_TYPE`).
- Visibility of the constructor, the pattern and `.value` is in [04](04-modules-and-names.md) §4.6.4.

```sai
distinct UserId = U64

fn next(id: UserId) -> UserId {
    UserId(id.value + 1)             // OK: arithmetic on the base, then re-wrap
}

fn bad(id: UserId) -> UserId {
    id + UserId(1)                   // rejected: OPERATOR_NOT_SUPPORTED (06); derive Add to forward it
}
```

### 5.6.4 Refined types

`type R = B where p` introduces a nominal **refined type** `R` with base type `B` and predicate `p` (D-TYPE-03). This section defines the type-level part. Checking `p` is defined in [09](09-contracts-and-refinements.md).

- `R` cannot have generic parameters (03 §3.6). A violation is `GENERIC_REFINED_TYPE`, which this chapter defines (§5.22).
- `B` MUST be well-formed. `B`, after alias expansion, MUST NOT itself be a refined type (`REFINED_BASE_IS_REFINED`) (N-A-25). Write the combined predicate on the original base instead. `B` may be a view (`type NonEmptyStr = Str where self.len() > 0`, R-06). `R` then carries the region of `B` (D-OWN-03).
- `R ≢ B`. Two refined types with the same base and predicate are still different types.
- **R → B** is an implicit coercion (C-RefinedToBase, §5.15).
- **B → R** is an implicit coercion only when the SFC proves `p` for the value (C-BaseToRefined). Otherwise it is `REFINEMENT_NOT_PROVEN` ([09](09-contracts-and-refinements.md)), and the program must use `R.try_from(v)`, which returns `Option<R>` (compiler-provided, [09](09-contracts-and-refinements.md) N-C-41).
- Refinement is **top level only**. `Vec<R>`, `(R, R)` and `Option<R>` are not related to `Vec<B>`, `(B, B)` and `Option<B>` by any coercion (D-TYPE-03). The single exception is shared borrows: `borrow R` coerces to `borrow B` (C-RefinedBorrow, §5.15, from [09](09-contracts-and-refinements.md) N-C-40). `borrow mut R` never coerces to `borrow mut B`.
- **Base fallback** (N-A-25, refinement erasure of [09](09-contracts-and-refinements.md) §9.2.2). A refined type has the methods of its own inherent impls and trait impls. In addition:
  - field reads on `R` read through to `B` and give immutable places ([06](06-expressions-and-statements.md) §6.6, N-C-34);
  - method resolution has a final **base step** that looks up methods of `B` with receiver `self: Self` or `self: borrow Self`, applied to the receiver viewed as a `B` (§5.13). A method of `B` with receiver `borrow mut Self` found this way is `REFINED_PLACE_MUTATION` ([09](09-contracts-and-refinements.md)), because a refined value cannot be mutated in place. Associated functions without `self` are not inherited: `Percentage.max(..)` does not find `I32.max`.
  - operands of operators and comparisons see the value as `B`, and the result has the type the base operation gives ([09](09-contracts-and-refinements.md) §9.2.2 item 3). `p + 1` with `p : Percentage` has type `I32`.
- **Trait impls.** `R` has compiler-provided impls that forward to `B` (§5.11.4, [09](09-contracts-and-refinements.md) §9.2.2 item 5): `Copy` and `Clone` iff `B: Copy`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format` iff `B` implements them. No other trait is implemented automatically. A user impl of one of these traits for `R` overlaps the provided impl (`OVERLAPPING_IMPLS`, §5.10.2). `@derive` on a refined type accepts only `Arbitrary` (§5.12.4).

```sai
type Percentage = I32 where self >= 0 and self <= 100

fn scale(p: Percentage, total: I32) -> I32 {
    let base: I32 = p // R → B coercion at an annotated let
    base * total / 100
}

fn is_full(p: Percentage) -> Bool {
    p.is_positive() // base step: I32's method with `self: Self`
}
```

### 5.6.5 Type aliases

`type A<Ḡ> = T` introduces a transparent name for `T` (D-TYPE-03; 03 §3.6).

- A use `A<ḡ>` is replaced by `T[ḡ/Ḡ]` during resolution. Aliases never appear in semantic types, and a diagnostic that prints a type prints the expansion. If the user wrote an alias, the diagnostic also names it.
- Every generic parameter of an alias MUST occur in `T`. Otherwise the error is `UNUSED_ALIAS_PARAM`, because the argument would be silently ignored (P8).
- Bounds on alias parameters (`type Sorted<T: Ord> = Vec<T>`) are checked at each use after substitution, like the bounds of any generic entity (N-A-26). A failure is `UNSATISFIED_BOUND`.
- An alias whose expansion reaches itself is `ALIAS_CYCLE`. Expansion follows aliases through every position of `T`, including generic arguments.
- An alias of a nominal type can be used wherever that type can: in impl headers, in constructor and pattern paths (`A(..)`, `A.V`), and in associated paths (`A.new()`).
- `pub` aliases follow [04](04-modules-and-names.md) §4.6.7.

### 5.6.6 Resources

`resource R { fields }` introduces a nominal **resource type** (Stage 4; semantics in [13](13-components-and-wit.md)).

- Resources are affine and never `Copy` or `Clone` (D-OWN-06). `@derive` on a resource is `DERIVE_NOT_SUPPORTED`.
- Resources cannot be generic (03 §3.6). Field types MUST be well-formed. A field type that contains a view or a borrow is `RESOURCE_WITH_BORROW` (N-A-27), because a resource value can be handed to the host, where no region applies.
- A resource may implement `Drop` (§5.9.3).
- Fields follow the rules for records, including visibility ([04](04-modules-and-names.md) §4.6.3).

### 5.6.7 Infinite size

A nominal type that contains itself **by value** has no finite size. This is the error `INFINITE_SIZE_TYPE`, reported on the declaration, with the fix "wrap the recursive field in `Box`". The by-value containment relation is computed on declarations, as follows:

1. The by-value components of a type are: the type itself; the elements of a tuple; the element type of an `Array`; the fields and variant payloads of a nominal type, after substituting its generic arguments; the base type of a distinct or refined type.
2. These types are **indirections**, and their arguments are not by-value components: `Box<T>`, `Vec<T>`, `String`, `borrow T`, `borrow mut T`, `Str`, `Slice<T>`, `SliceMut<T>`, function pointers, function item types, and closure types.
3. A declaration `N` is infinite if expanding the by-value components of `N<Ḡ>` transitively reaches **any** application of `N` again, with any arguments.

```sai
enum List {
    Cons(I32, Box<List>)            // OK: Box is an indirection
    Nil
}

type Node {
    next: Option<Node>              // rejected: INFINITE_SIZE_TYPE (Option holds its payload by value)
}
```

## 5.7 Generics

### 5.7.1 Generic parameters

```sai
fn largest<T: PartialOrd + Copy>(items: Slice<T>) -> Option<T> {
    None
}

type Buffer<T, const N: USize> {
    data: Array<T, N>
    len: USize
}
```

- Generic parameters may appear on functions, records, enums, distinct types without a refinement clause, aliases, traits and impls (D-TYPE-07). Type and const parameters may be listed in any order.
- A **type parameter** `T` ranges over well-formed types. It may carry bounds `T: B1 + … + Bn`.
- A **const parameter** `const N: κ` ranges over values of `κ`. `κ` MUST be an integer type, `Bool` or `Char` (03 §3.5). Otherwise the error is `INVALID_CONST_PARAM_TYPE`. Const parameters cannot have bounds.
- Two parameters of one list with the same name are `DUPLICATE_BINDING` (04). A method parameter that reuses the name of an impl or trait parameter is `SHADOWS_GENERIC_PARAM` (04).
- A type parameter that no field or payload of a type declaration uses is allowed. No diagnostic is given. Its variance is defined in [07](07-ownership-and-borrowing.md).
- Programs are monomorphized (D-TYPE-07). Type checking, however, is done **once per generic item**, against its bounds only. A generic body that type-checks is valid for every instantiation that satisfies the bounds. Instantiation reports no type errors, except the limit of §5.7.6.

### 5.7.2 Bounds

A bound is a trait reference `Tr<Ā>` with optional associated-type bindings, or the function-trait sugar (§5.5.4). Either may carry bound effects ([08](08-effects-and-capabilities.md)).

- The path of a bound MUST resolve to a trait ([04](04-modules-and-names.md)). Otherwise the error is `NOT_A_TRAIT`.
- The trait's generic arguments follow §5.2.2. Associated-type bindings (`Iterator<Item = I32>`) add equalities (§5.7.4).
- The same trait with the same arguments twice in one bound list is allowed and has no extra effect.

### 5.7.3 `where` clauses

A `where` clause holds predicates `τ: Bounds` (03 §3.5). The subject `τ` may be any type in which the item's generic parameters occur: `T`, `T.Item`, `Vec<T>`, `(T, U)`. A predicate whose subject contains no generic parameter is `WHERE_WITHOUT_PARAM`. Such a predicate is either always true or always false, so it has no use (P8). A `where` predicate is equivalent to the same bound written on the parameter list.

On a **trait** declaration, the subject of every `where` predicate MUST be `Self` or a projection `Self.A`. Any other subject is `INVALID_TRAIT_WHERE_SUBJECT` (N-A-28).

### 5.7.4 Predicates in scope and implied bounds

Inside an item, `Δ` contains the **elaborated** predicates of the item and of its enclosing impl or trait:

1. every bound and `where` predicate written on the item and its enclosing impl or trait;
2. for every bound `τ: Tr`, the supertrait bounds of `Tr`, recursively, with the trait's arguments substituted. The `where` predicates of `Tr` on `Self` and `Self.A` are treated the same way;
3. for every bound `τ: Tr` and every associated type `A` declared as `type A: Bs` in `Tr`, the bounds `<τ as Tr>.A : Bs`;
4. for every binding `Tr<A = σ>` on a bound `τ: Tr<…>`, the equality `<τ as Tr>.A = σ`;
5. inside a trait, the predicate `Self: Tr<Ḡ>`.

**Nothing else is implied** (N-A-28). In particular, the bounds declared on a type's parameters are not implied by mentioning the type in a signature (§5.6.1). Rationale: every requirement of a signature is visible in the signature (P1).

### 5.7.5 Const generics

- A const parameter can be used as a const argument (`Array<T, N>`) and as a value of type `κ` in expressions ([06](06-expressions-and-statements.md)).
- There is no const arithmetic in types (D-TYPE-07). `Array<T, N + 1>` is a syntax error (03).
- Const arguments are compared by value (§5.14). Const inference variables unify only with a const of the same type.

### 5.7.6 Instantiation limit

Monomorphization starts from every non-generic function and constant of the package being built and, only when tests are compiled (`sai test`, `sai check --tests`; [14](14-testing.md) §14.2.4, N-I-39), from every test and property of the selected packages. The roots of a test harness are those of [14](14-testing.md) §14.7.1 item 3. It instantiates every generic item they reference, transitively, including impl methods selected by trait solving. If an instantiation has a generic argument whose **type nesting depth** exceeds 64, the error is `INSTANTIATION_DEPTH_EXCEEDED`, reported at the call that produces it (N-A-30). The nesting depth of a primitive or type parameter is 1. The depth of any other type is 1 plus the maximum depth of its components. Generic items that are never instantiated are never checked against this limit. This happens before and independently of any optimization, so acceptance does not depend on optimizer strength.

```sai
fn grow<T>(x: T, n: I32) {
    if n > 0 {
        grow((x,), n - 1) // INSTANTIATION_DEPTH_EXCEEDED once a non-generic caller reaches it
    }
}
```

## 5.8 Traits

### 5.8.1 Declarations

```sai
pub trait Shape {
    fn area(self: borrow Self) -> F64
    fn describe(self: borrow Self) -> String {
        String.from("shape")
    }
}

pub trait Container: Clone {
    type Item: Clone
    fn get(self: borrow Self, index: USize) -> Option<Self.Item>
    fn put(self: borrow mut Self, item: Self.Item)
}
```

A trait declaration `trait Tr<Ḡ>: S1 + … + Sk where … { items }` (D-TYPE-08) has:

- an implicit type parameter `Self`, ahead of `Ḡ`;
- **supertraits** `S1 … Sk`. Every impl of `Tr` requires impls of all supertraits for the same `Self` (§5.9.1). The supertrait graph MUST be acyclic, otherwise `SUPERTRAIT_CYCLE`;
- **associated types** `type A: Bs`, with optional bounds. Associated types have no defaults in v0.1;
- **methods and associated functions**, each a `FnDecl`, with or without a body. A function with a body is a **default** that impls may override.

Not in v0.1: associated constants, generic associated types, higher-kinded types, specialization, negative impls and auto traits (D-TYPE-08). The grammar has no syntax for them.

Items of one trait MUST have distinct names (`DUPLICATE_TRAIT_ITEM`).

`unsafe trait` is defined in [12](12-unsafe-and-ffi.md).

### 5.8.2 Receivers

A trait or impl function whose first parameter is `self` is a **method**. Otherwise it is an **associated function**. The receiver type MUST be written and MUST be one of `Self`, `borrow Self` or `borrow mut Self` (D-TYPE-08). Any other type is `INVALID_SELF_TYPE` (03). `var self: Self` makes the receiver binding mutable ([06](06-expressions-and-statements.md)).

### 5.8.3 Generic traits and trait methods

- A trait may have generic parameters: `trait Add<Rhs> { type Output … }`. `Tr<A>` and `Tr<B>` are different traits for coherence and solving. Trait parameters have no defaults, so `Add<Money>` is always written in full (P1).
- A trait method may have its own generic parameters, with bounds. An impl must declare the same ones (§5.9.1).

### 5.8.4 Effects of trait methods

The `effects` clause of a trait method is an upper bound for every impl ([08](08-effects-and-capabilities.md)). A method that omits it has the empty bound in a `pub` trait, as [08](08-effects-and-capabilities.md) defines.

### 5.8.5 Associated-type projections

The source forms `Self.A`, `T.A` and `π.A` (where `π` is itself a projection) denote the projection `<σ as Tr<ḡ>>.A`, where `σ` is the subject:

1. The subject MUST be `Self`, a type parameter, or a projection (N-A-29). `Vec<I32>.Item` and other projections on concrete types are `PROJECTION_ON_CONCRETE_TYPE`: write the concrete type instead (P8).
2. The trait `Tr<ḡ>` is the unique trait among the elaborated bounds of the subject in `Δ` (§5.7.4) that declares an associated type `A`. If there is none, the error is `UNKNOWN_ASSOC_TYPE`. If there are two or more different ones (for example `T: Iterator + Container`, both with `Item`), the error is `AMBIGUOUS_ASSOCIATED_TYPE`. v0.1 has no syntax to choose; restructure the bounds instead.
3. Inside a trait, `Self.A` for an associated type `A` of the trait itself always refers to that trait.

**Normalization** `Δ ⊢ π ↠ τ` replaces a projection by a more concrete type, repeatedly:

- if `Δ` has an equality `π = τ` (§5.7.4, item 4), then `π ↠ τ`;
- if the subject of `π` is (after substitution) a type for which trait solving selects an impl (§5.11), and the impl defines `type A = τ`, then `π ↠ τ` with the impl's parameters substituted;
- otherwise `π` is **rigid**. A rigid projection is a type of its own. It is equal only to itself (the same subject, trait and name, §5.14), and its only properties are the bounds `Δ` gives it.

Normalization happens on every type before unification and equality tests.

```sai
fn first<I: Iterator<Item = I32>>(it: I) -> I32 {
    var it = it
    match it.next() {
        Some(x) => x // I.Item normalizes to I32 through the binding
        None => 0
    }
}
```

## 5.9 Impls

### 5.9.1 Trait impls

```sai
impl Shape for Circle {
    fn area(self: borrow Self) -> F64 {
        3.14159 * self.r * self.r
    }
}

impl<T: Clone> Container for Stack<T> {
    type Item = T

    fn get(self: borrow Self, index: USize) -> Option<T> {
        None
    }

    fn put(self: borrow mut Self, item: T) {}
}
```

For `impl<Ḡ> Tr<ā> for S where W { items }`, under `Δ_I` = `Ḡ` plus the elaborated predicates of the bounds and of `W`:

1. `Tr` MUST resolve to a trait (`NOT_A_TRAIT`). `Tr<ā>` and `S` MUST be well-formed under `Δ_I`.
2. Every parameter of `Ḡ` MUST occur in `S` or in `ā` (`UNCONSTRAINED_IMPL_PARAM`). Otherwise the impl could not be selected by unification.
3. **Supertraits.** For every supertrait `Sup<b̄>` of `Tr` (with `Self := S`), `Δ_I ⊢ S : Sup<b̄>` MUST hold. Otherwise the error is `MISSING_SUPERTRAIT_IMPL`.
4. **Items.** Every associated type of `Tr` MUST be defined (`type A = τ`), and every function without a default MUST be implemented. Missing items are `MISSING_TRAIT_ITEM`, which lists all of them and carries a fix that inserts stubs. An item that `Tr` does not declare is `UNKNOWN_TRAIT_ITEM`. Two items with the same name are `DUPLICATE_TRAIT_ITEM`.
5. **Associated type definitions.** `τ` MUST be well-formed under `Δ_I` and MUST satisfy the bounds the trait declares for `A`, with `Self := S` (`UNSATISFIED_BOUND`).
6. **Signature match** (N-A-32). Let `sig_T` be the trait's signature of a function with `Self := S`, the trait parameters `:= ā` and every associated type of `Tr` replaced by the impl's definition. The impl's function MUST match `sig_T` exactly up to renaming of its own generic parameters:
   - the same number and kinds of own generic parameters, with equivalent bounds (the same elaborated predicate set);
   - the same receiver form, and the same number of parameters with equal types (§5.14);
   - an equal return type;
   - the same `unsafe` marker.

   Any difference is `TRAIT_ITEM_SIGNATURE_MISMATCH`, naming the first one. A trait method cannot be `async`, and so neither can a function of a trait impl: an `async fn` declared in a trait or implemented in a trait impl is `ASYNC_FN_IN_TRAIT` ([11](11-async-and-concurrency.md) §11.2.6, N-I-3). Parameter names may differ. Effects follow [08](08-effects-and-capabilities.md) (`TRAIT_IMPL_EFFECTS_EXCEED_BOUND`), `borrows` clauses follow [07](07-ownership-and-borrowing.md), and contracts follow [09](09-contracts-and-refinements.md).
7. **Defaults.** A function that is not implemented uses the trait's default body, checked once in the trait under `Self: Tr` ([06](06-expressions-and-statements.md)).
8. Items of a trait impl take no visibility marker (`VISIBILITY_ON_TRAIT_IMPL_ITEM`, 03). They are as visible as the trait ([04](04-modules-and-names.md) §4.6.2).
9. Coherence (§5.10) MUST hold.

Additional restrictions on particular traits:

| Trait | Restriction |
|---|---|
| `Copy` | cannot be implemented manually; only `@derive(Copy)` or the compiler-provided impls of §5.11.4 (`MANUAL_COPY_IMPL`) (N-A-39) |
| `Fn`, `FnMut`, `FnOnce` | cannot be implemented (`MANUAL_FN_IMPL`) (§5.5.4) |
| `Drop` | §5.9.3 |
| all others | none beyond the rules above |

### 5.9.2 Inherent impls

```sai
impl User {
    pub fn create(id: UserId, name: String) -> User {
        User(id, name)
    }

    pub fn name(self: borrow Self) -> Str {
        self.name.as_str()
    }
}
```

For `impl<Ḡ> S where W { items }` (N-A-33):

1. `S`, after alias expansion, MUST be a nominal type (record, enum, distinct, refined or resource) **declared in the current package**. Primitives, structural types, lang-item types of `sai:core`/`sai:alloc` and nominal types of other packages are `INHERENT_IMPL_FOREIGN_TYPE`. The fix suggests a trait or a distinct wrapper. The impl may be in any module of the package.
   - **Exception for the core library.** `sai:core` and `sai:alloc`, which count as one package here as they do for coherence (§5.10.1), may also declare inherent impls on the primitive types, on `Array<T, N>` and on every lang-item type of either package (`Str`, `Slice<T>`, `SliceMut<T>`, `String`, `Vec<T>`, `Box<T>`, …), such as `I32.checked_add`, `Str.len` and `Slice.to_vec` ([16](16-core-library.md) §16.1.1 item 4, N-J-6). Rule 4 is then checked across both packages together. Every other package is bound by `INHERENT_IMPL_FOREIGN_TYPE` for these types.
2. The generic arguments of `S` may be parameters or concrete types (`impl Wrapper<I32> { … }`). Every parameter of `Ḡ` MUST occur in `S` (`UNCONSTRAINED_IMPL_PARAM`).
3. Items are functions only. An associated type in an inherent impl is `ASSOC_TYPE_IN_INHERENT_IMPL`.
4. **Names are unique per type.** Across *all* inherent impls of the same declaration `N`, whatever their generic arguments, two items with the same name are `DUPLICATE_INHERENT_ITEM`. Rationale: a name on a type means one function (P8), and canonical paths stay unique ([04](04-modules-and-names.md) §4.9).
5. Each item has its own visibility: package-private, or `pub` ([04](04-modules-and-names.md) §4.6.2).

### 5.9.3 `Drop` impls

`Drop` is the lang-item trait with the single method `fn drop(self: borrow mut Self)`, whose effect set is empty ([16](16-core-library.md), [08](08-effects-and-capabilities.md)). An `impl Drop for S` is valid only if (N-A-34):

1. `S` is a nominal type declared in the current package (record, enum, distinct or resource; a refined type cannot have its own drop). Otherwise `DROP_IMPL_INVALID_TYPE`.
2. The impl is exactly as generic as the type. `S` is `N<P1, …, Pn>` with the declaration's own parameters, pairwise distinct, and the impl's predicates are exactly the declaration's predicates. A concrete argument, a repeated parameter, or an extra bound is `DROP_IMPL_NOT_GENERIC_ENOUGH`. Rationale: whether a value runs drop code must not depend on its type arguments (P8).
3. `S` is not `Copy` (`COPY_AND_DROP`, [07](07-ownership-and-borrowing.md)).

`Drop.drop` cannot be called explicitly, in method form or in path form (`EXPLICIT_DROP_CALL`, [07](07-ownership-and-borrowing.md) §7.13.6). A value is dropped early by moving it into the core function `core.mem.drop` ([16](16-core-library.md)). When drop code runs is defined in [07](07-ownership-and-borrowing.md).

## 5.10 Coherence

Coherence guarantees that for every trait obligation with fully known types at most one impl applies, across the whole program (D-TYPE-08).

### 5.10.1 The orphan rule

For coherence, `sai:core` and `sai:alloc` count as **one package** (D-TYPE-08), called `core` below.

The **owner** of a type `τ` is found by first stripping the **fundamental** wrappers `borrow`, `borrow mut` and `Box<…>` from its outside, repeatedly, and then taking the head (N-A-35):

| Head after stripping | Owner |
|---|---|
| nominal type `N` | the package that declares `N` |
| primitive, `()`, `Never`, tuple, `Array`, function pointer | `core` |
| type parameter | none (the impl is a **blanket impl**) |
| function item, closure | impossible: these types cannot be written in an impl header |

`impl<Ḡ> Tr<ā> for S` in package `P` is allowed iff `Tr` is declared in `P`, or the owner of `S` is `P`. Otherwise the error is `ORPHAN_IMPL`, which explains both conditions (D-TYPE-08). Consequences:

- A blanket impl `impl<T: X> Tr for T` (also `for borrow T`, `for Box<T>`) is allowed only in the package of `Tr` (D-TYPE-08).
- Impls for tuples, arrays and primitives of a trait from another package are possible only in `core`.
- A package may implement a foreign trait for `borrow Local` or `Box<Local>`, because both are owned by the local package.

### 5.10.2 The overlap check

Two impls **overlap** if some type could match both headers. The check is purely syntactic and ignores `where` clauses and bounds (D-TYPE-08, N-A-35):

```text
overlap(I1, I2):
    if trait(I1) ≠ trait(I2): return false           // different trait declarations never overlap
    rename the generic parameters of I1 and I2 apart, as fresh unification variables
    return unify([ā1, S1], [ā2, S2]) succeeds        // trait arguments, then self types
```

`unify` is first-order unification with an occurs check, on semantic types after alias expansion:

- regions are ignored (any two regions unify);
- a const variable unifies with any const of its type; two const values unify iff they are equal;
- distinct types, refined types and every other nominal type unify only with the same declaration;
- associated-type projections in impl headers are not allowed: a projection in an impl header is `PROJECTION_IN_IMPL_HEADER`.

The check is run for every package being compiled. For each impl `I` declared in it, the check considers every other impl of the same trait in the package and in its transitive dependencies, including the compiler-provided impls of §5.11.4 and the impls generated by `@derive`. Every overlapping pair is the error `OVERLAPPING_IMPLS`, reported on the impl of the current package that comes later in source order, with a note on the other one. Under the orphan rule, any two overlapping impls are in packages where one depends on the other, so the check sees every pair (N-A-35).

```sai
trait Describe {
    fn describe(self: borrow Self) -> String
}

impl<T: Clone> Describe for Vec<T> {
    fn describe(self: borrow Self) -> String { String.from("vec") }
}

impl Describe for Vec<I32> {                           // rejected: OVERLAPPING_IMPLS (bounds are ignored)
    fn describe(self: borrow Self) -> String { String.from("ints") }
}
```

## 5.11 Trait solving

### 5.11.1 Obligations

An **obligation** is a goal `τ : Tr<σ̄>`, possibly with associated-type bindings, or an equality `π = τ`. Obligations arise from bounds at calls and type uses (§5.2.2), from supertraits and associated-type bounds of impls (§5.9.1), from operators ([06](06-expressions-and-statements.md) §6.8), from method resolution (§5.13), and from `Copy` checks ([07](07-ownership-and-borrowing.md)). Each obligation is solved in the environment `Δ` of the item where it arises.

### 5.11.2 The solving procedure

`solve(Δ, τ : Tr<σ̄>, depth)` (N-A-36):

1. If `depth > 128`, the error is `TRAIT_SOLVER_OVERFLOW`. The obligation fails.
2. Shallow-resolve and normalize `τ` and `σ̄` (§5.8.5).
3. If the goal is identical to a goal on the current solving stack, it fails. Solving is inductive.
4. **Environment candidates.** Collect the predicates of `Δ` (elaborated, §5.7.4) that unify with the goal.
   - If exactly one does, or several do and they are identical after unification, select it, apply the unification, and succeed.
   - If several different ones do, the goal is **ambiguous**: defer it (§5.11.3).
5. **Impl candidates.** Collect the impls of `Tr` in the current package and its transitive dependencies (whether or not they were imported: impls have no visibility), and the compiler-provided impls of §5.11.4. Keep those whose header unifies with the goal, with the impl's parameters fresh.
   - If exactly one remains, select it. Apply the unification. Then solve each of the impl's predicates (bounds and `where` clause, substituted) at `depth + 1`. The goal succeeds iff all of them succeed.
   - If two or more remain, the goal contains unresolved inference variables (coherence excludes it otherwise). Defer it.
   - If none remains, the goal fails.
6. A failed goal reported to the user is `UNSATISFIED_BOUND`. It names the type, the trait and the origin of the obligation. If the type is a nominal type of the current package and the trait is derivable, it carries the fix "add `@derive(Tr)`".

Environment candidates win over impl candidates. Inside `fn f<T: Clone>(x: T)`, the goal `T: Clone` is answered by the bound even if a blanket impl would also apply. Rationale: a generic body relies only on its declared bounds (P8).

### 5.11.3 Deferred obligations

An ambiguous obligation stays pending. It is retried whenever an inference variable in it is resolved (§5.16). At the end of inference for a body, after literal defaulting (§5.16.5), every pending obligation is retried once more. If it is still ambiguous, the error is `TYPE_ANNOTATIONS_NEEDED`. If it fails, the error is `UNSATISFIED_BOUND`.

### 5.11.4 Compiler-provided impls

The following impls exist without any declaration. Except for the refined-type row, they belong to `core` for coherence, so no other package can implement these traits for these types. Any user impl that unifies with one of them is `OVERLAPPING_IMPLS` (§5.10.2):

| Type | Traits |
|---|---|
| every primitive type, `()` | `Copy`, `Clone`. Further impls (`PartialEq`, `Ord`, `Hash`, `Default`, operator traits, …) are ordinary declarations in `sai:core` ([16](16-core-library.md)). |
| `Never` | `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` |
| `(τ1, …, τn)`, n ≥ 1 | `Tr` for each `Tr` in `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Default`, `Arbitrary`, when every `τi : Tr`. Semantics as derived for a record with fields `0 … n−1` (§5.12.3). |
| `Array<τ, N>` | the same traits, when `τ : Tr`. For `Default` and `Arbitrary`, element by element. |
| `borrow τ` | `Copy`, `Clone`. `PartialEq`, `Eq`, `PartialOrd`, `Ord` and `Hash` when `τ : Tr`, comparing and hashing the referents. |
| `borrow mut τ` | `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` when `τ : Tr`, as for `borrow τ`; not `Copy` or `Clone` |
| function item types | `Copy`, `Clone`; and `Fn`, `FnMut`, `FnOnce` for safe non-`async` items (§5.5.1) |
| function pointer types | `Copy`, `Clone`; and `Fn`, `FnMut`, `FnOnce` when safe |
| closure types | `FnOnce`, `FnMut`, `Fn`, `Copy`, `Clone` as in §5.5.3 |
| refined type `R = B where p` (`type` form) | `Copy` and `Clone` iff `B: Copy`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format` iff `B` implements them. Each method is `B`'s method applied to the receiver (and argument) viewed as `B`; `clone` returns a copy of the receiver as an `R` ([09](09-contracts-and-refinements.md) §9.2.2 item 5). These impls belong to the package that declares `R`. |

`Str` and `Slice<T>` are `Copy` and `Clone` by compiler-provided impls, like `borrow τ`. `SliceMut<T>` is neither. All other impls of `Str`, `Slice<T>`, `SliceMut<T>`, `String`, `Vec<T>` and `Box<T>` are ordinary declarations in [16](16-core-library.md).

## 5.12 `@derive`

### 5.12.1 General rules

`@derive(Tr1, …, Trn)` on a record, enum, distinct type, or refined type (`Arbitrary` only) generates one impl per listed trait (D-TYPE-08). The attribute's syntax, its targets, and duplicate-argument errors are in [15](15-attributes-and-lints.md). This section defines what each generated impl is (N-A-38).

- The names are fixed: `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Default`, `Arbitrary`, and, for distinct types only, `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `BitAnd`, `BitOr`, `BitXor`, `BitNot`. The names are not looked up in scope. They always mean the `sai:core` traits. Any other name is `UNKNOWN_DERIVE`. A name that is not derivable for the kind of declaration is `DERIVE_NOT_SUPPORTED` (table in §5.12.4).
- **Generated header.** For a record or enum `N<Ḡ>` with type parameters `T1 … Tk` and declared predicates `W`, the impl is `impl<Ḡ> Tr for N<Ḡ> where W, T1: Tr, …, Tk: Tr`. Const parameters get no bound. For a distinct type `D<Ḡ> = B`, the impl is `impl<Ḡ> Tr for D<Ḡ> where W, B: Tr` (the base, not each parameter). For a refined type, which has no parameters, the impl has no parameters and requires `B: Arbitrary`.
- **Field check.** The generated body must type-check. For records and enums, each field and payload type `F` must satisfy `F: Tr` under the generated predicates. A failure is `DERIVE_FIELD_MISSING_IMPL`, reported on the attribute with a note on the field. For distinct and refined types, an unsatisfiable `B: Tr` with a concrete `B` is reported the same way.
- **Supertraits are not derived implicitly.** `@derive(Copy)` requires `Clone` to be implemented too, by the same `@derive` or by an impl. The same goes for `Eq` (needs `PartialEq`), `PartialOrd` (needs `PartialEq`) and `Ord` (needs `Eq` and `PartialOrd`). A missing supertrait is `MISSING_SUPERTRAIT_IMPL`, with the fix "add `Tr` to the derive list" (P1).
- A derived impl is an ordinary impl for coherence and solving. A derived impl and a hand-written impl of the same trait for the same type are `OVERLAPPING_IMPLS`.
- **Effects.** A generated method has no clause of its own, and its effects are inferred from the impls it calls ([08](08-effects-and-capabilities.md) §8.7). The methods of `PartialEq`, `PartialOrd`, `Ord` and `Hash` are bounded by `effects {}` in their traits, so their derived impls are pure ([16](16-core-library.md) §16.5.1). Derived `Clone`, `Default`, `Arbitrary` and operator impls have the effects of the field impls they call: `@derive(Clone)` on a record with a `String` field has `{ alloc }`, and for a generic type the effects of the field impls are latent in the type parameters (08 §8.8).

### 5.12.2 Order of evaluation

Generated methods visit fields in **declaration order** and enum variants by **discriminant**. Every method of a generated impl is deterministic and calls no user code other than the impls of the field types.

### 5.12.3 Generated behavior

Below, `f1 … fn` are the fields of a record (or the payload of a variant) in declaration order. A tuple variant's payload elements play the role of fields `0 … n−1`. The exact method signatures are those of [16](16-core-library.md).

| Trait | Record | Enum |
|---|---|---|
| `Clone` | a new value whose every field is `fi.clone()`, in order | match the variant; clone its payload as for a record |
| `Copy` | marker: no methods; all fields must be `Copy` | same |
| `PartialEq` (`eq`) | `self.f1 == other.f1 and … and self.fn == other.fn`, short-circuit, left to right. No fields: `true` | different variants: `false`. Same variant: as for a record over the payload |
| `Eq` | marker | marker |
| `PartialOrd` (`partial_cmp`) | lexicographic: for i = 1 … n, `c = self.fi.partial_cmp(other.fi)`; if `c` is not `Some(Equal)`, return `c`. At the end, `Some(Equal)` | compare the discriminants as integers of the discriminant type; if they differ, `Some(Less)` or `Some(Greater)`. Otherwise lexicographic over the payload |
| `Ord` (`cmp`) | as `PartialOrd` with `cmp`, ending in `Equal` | as `PartialOrd` with `cmp` |
| `Hash` | feed each field to the hasher, in order | feed the discriminant as an `I64`, then the payload fields, in order |
| `Default` | a value with every field `F.default()`, in order | `DERIVE_NOT_SUPPORTED` |
| `Arbitrary` | generate each field in order, with the field type's `Arbitrary` | choose a variant by index from the generator (see below), then generate its payload in order; an enum with no variants is `DERIVE_NOT_SUPPORTED` |

**Recursive types and invariants in `Arbitrary`** (N-A-66). A type `N` is **recursive** if `N` lies on a cycle of the type-definition graph, whose edges go from a nominal type to each nominal type named in its field or payload types, type arguments included. A field or variant of `N` is recursive if its type mentions a type of `N`'s cycle. The derived `Arbitrary` of a recursive type:

1. for an enum, chooses with `gen.below(k)` among all `k` variants if `gen.size() > 0`, and among the non-recursive variants only if `gen.size() == 0`. An enum whose variants are all recursive is `DERIVE_NOT_SUPPORTED`;
2. generates every recursive field, and the payload of a recursive variant, with the size set to `gen.size() / 2`, and restores the previous size afterwards.

A recursive field of a record is **guarded** if every path from its type to a type of the cycle passes through `Vec`, `String`, `Option`, or an enum of the cycle. An unguarded recursive record field, as in `type Loop { next: Box<Loop> }`, admits no finite value, and deriving `Arbitrary` for its record is `DERIVE_NOT_SUPPORTED`. Otherwise generation terminates: the size reaches 0 after at most 33 levels, and at size 0 the library impls of `Vec<T>`, `String` and `Option<T>` produce no element ([16](16-core-library.md) §16.17) and enums choose non-recursive variants. A non-recursive enum chooses its variant uniformly in every size.

For a record or enum with an `invariant` declaration ([09](09-contracts-and-refinements.md) §9.4), the derived impl filters like a refined type: each attempt generates the fields as above, evaluates the invariant's predicates with `self` bound to the candidate, and constructs the value only if they hold, with the rejection limit of [14](14-testing.md) §14.5.4.

For a **distinct type** `D = B` every derived method **forwards** to `B` through `.value` and re-wraps where the result has type `Self`:

| Trait | Generated behavior for `D` |
|---|---|
| `Clone` | `D(self.value.clone())` |
| `PartialEq`, `PartialOrd`, `Ord`, `Hash` | the method of `B` applied to `self.value` (and `other.value`) |
| `Copy`, `Eq` | marker |
| `Default` | `D(B.default())` |
| `Arbitrary` | `D(b)`, with `b` generated by `B`'s `Arbitrary` |
| `Add`, `Sub`, `Mul`, `Div`, `Rem`, `BitAnd`, `BitOr`, `BitXor` | `impl Tr<D> for D` with `type Output = D`, and the method `D(Tr.m(self.value, rhs.value))`. It requires `B: Tr<B, Output = B>` |
| `Neg`, `BitNot` | `impl Tr for D` with `type Output = D`, and the method `D(Tr.m(self.value))`. It requires `B: Tr<Output = B>` |

Forwarded operators keep the base's runtime behavior, including panics on overflow and division by zero (D-TYPE-06). The impls `B: Add<B, Output = B>` and so on for the primitive types are declared in `sai:core` ([16](16-core-library.md)).

For a **distinct type with a refinement clause** `distinct D = B where p`, generated code never constructs a `D` from a computed base value, because that would need a proof. `Clone` is derivable only when `B: Copy`, and returns a copy of the receiver. `Arbitrary` generates `B` values and keeps those that satisfy `p`, with the rejection limit that [14](14-testing.md) defines. `Default` and the operator traits are `DERIVE_NOT_SUPPORTED`.

For a **refined type** `type R = B where p`, the comparison, hashing and copy traits are compiler-provided (§5.11.4) and are not listed in `@derive`. Only `Arbitrary` may be derived; it filters as for a distinct type with a refinement clause.

```sai
@derive(Copy, Clone, PartialEq, PartialOrd, Sub)
distinct Money = pub I64

fn change(paid: Money, price: Money) -> Money {
    // derived PartialOrd and Sub (R-05)
    if paid < price {
        Money(0)
    } else {
        paid - price
    }
}
```

### 5.12.4 Derivability by declaration kind

| Trait | Record | Enum | Distinct | Distinct with `where` | Refined | Resource |
|---|---|---|---|---|---|---|
| `Copy` | yes | yes | yes | yes | no (provided) | no |
| `Clone` | yes | yes | yes | yes, iff `B: Copy` | no (provided) | no |
| `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` | yes | yes | yes | yes | no (provided) | no |
| `Default` | yes | no | yes | no | no | no |
| `Arbitrary` | yes (filtered with an invariant) | yes (≥ 1 variant, ≥ 1 non-recursive variant if recursive; filtered with an invariant) | yes | yes (filtered) | yes (filtered) | no |
| operator traits | no | no | yes | no | no | no |

`no` is `DERIVE_NOT_SUPPORTED`. For `no (provided)`, the diagnostic notes that the refined type already has the impl and offers the fix "remove it from the derive list". `Clone` on a distinct type with a refinement clause whose base is not `Copy` is `DERIVE_FIELD_MISSING_IMPL` with a note that `B: Copy` is required. `Default` and the operators are not derivable where they would have to build a refined value without a proof. `@derive` on a type alias, trait, function, or any other item is `INVALID_ATTRIBUTE_TARGET` ([15](15-attributes-and-lints.md)).

## 5.13 Method resolution and associated items

This section defines how `e.m(..)` and `P.name` find their target (D-TYPE-08, N-A-37). The typing of the call that follows is in [06](06-expressions-and-statements.md) §6.5. Field access `e.f` is in [06](06-expressions-and-statements.md) §6.6. It uses the autoderef chain of §5.13.2 but no trait lookup.

### 5.13.1 Inputs

A method call `e.m<Ḡ>(a1, …, an)` is resolved with:

- the receiver type `τ0`, synthesized for `e` in a place context and shallow-resolved;
- the method name `m`;
- the optional explicit generic arguments `Ḡ`;
- the environment `Δ` and the module `M` of the call.

If `τ0` is a general inference variable `α` after the pending obligations have been processed, the error is `TYPE_ANNOTATIONS_NEEDED`. If it is an integer-literal or float-literal variable, it is defaulted at this point to `I64` or `F64` (§5.16.4). Lookup then proceeds on the default type.

### 5.13.2 The autoderef chain

The **steps** are the types `τ0, τ1, …, τk`, computed as follows:

1. `τ(i+1) = υ` if `τi` is `borrow υ`, `borrow mut υ` or `Box<υ>` (after shallow resolution and normalization).
2. The chain stops at the first `τk` that is none of these. If `τk` is a general inference variable, the error is `TYPE_ANNOTATIONS_NEEDED`.
3. If `τk` is a refined type `R` with base `B`, one more step `τ(k+1) = B` is appended. It is marked as the **base step** (N-A-25).

Distinct types are never looked through (N-A-24). Views (`Str`, `Slice`, `SliceMut`), `String` and `Vec` are not dereferenced either. Their methods are declared on them ([16](16-core-library.md)). The chain is finite because types are finite.

### 5.13.3 Candidates

A **method** is an associated function whose first parameter is named `self`. Its receiver type is `Self`, `borrow Self` or `borrow mut Self` (D-TYPE-08). Associated functions without `self` are never method candidates.

- **Inherent candidates** are the methods named `m` in every inherent impl (§5.9.2) whose head type constructor is the head of some step `τi`. Inherent impls are found regardless of imports.
- **Trait candidates** are the methods named `m` of every trait in the **traits in scope** of `M`. These are:
  1. the traits bound in the module scope table of `M` (04 §4.4.1), by declaration or by `use`, under any binding name, except that a trait bound by a test-only use is in scope only in tests and properties ([14](14-testing.md) §14.2.5 item 2);
  2. the traits of the prelude (04 §4.2.6);
  3. for each step `τi` that is a type parameter, `Self`, or a rigid projection, the traits of the predicates on `τi` in the elaborated `Δ` (§5.7.4). These are in scope even if they are not imported. A bound can only be written by naming the trait, so the reader can see it in the signature (P1).

  Traits reachable only through a module path (`model.Describe`) are not in scope for method calls, except by rule 3.

### 5.13.4 The probe

For each step `τi` in order, and for each **adjustment** in the order *by value*, *`borrow`*, *`borrow mut`*, form the adjusted type `A`: `τi`, `borrow τi` or `borrow mut τi`. Then:

1. A candidate **matches** `A` if its receiver type unifies with `A`. For an inherent candidate, the impl's generic parameters are fresh. For a trait candidate, `Self` is a fresh variable `σ`, and the goal `σ : Tr<fresh args>` (§5.11) must not fail. An ambiguous goal is kept as an obligation.
2. If an inherent candidate matches, it is selected. By §5.9.2 at most one inherent method named `m` exists per type constructor.
3. Otherwise, if trait candidates of exactly one trait match, that trait's method is selected.
4. Otherwise, if trait candidates of two or more traits match, the error is `AMBIGUOUS_METHOD`. It lists the traits and offers the fix `Tr.m(e, …)` for each.
5. Otherwise, continue with the next adjustment, then the next step.

At the base step only the adjustments *by value* and *`borrow`* are tried. If a candidate of `B` with receiver `borrow mut Self` would match `borrow mut B` and no earlier candidate was found, the error is `REFINED_PLACE_MUTATION` ([09](09-contracts-and-refinements.md)).

The **receiver adjustment** of the selected method is `i` dereferences, then the adjustment's `borrow` or `borrow mut` if any (06 §6.5). At the base step, the R→B view (C-RefinedToBase, or C-RefinedBorrow under `borrow`) replaces the last dereference position. Whether a by-value adjustment at a step reached through a dereference may move the receiver is decided by [07](07-ownership-and-borrowing.md). Typically only `Copy` receivers can be used that way.

After selection, the method's signature is instantiated. The impl's or trait's parameters are fresh, and are unified with the adjusted receiver. Explicit arguments `Ḡ` instantiate the **method's own** generic parameters only, all or none (N-A-21, §5.2.2). The remaining arguments are checked against the parameter types at coercion sites (§5.15).

```sai
type Point {
    x: I64
    y: I64
}

impl Point {
    fn sum(self: borrow Self) -> I64 {
        self.x + self.y
    }
}

fn total(p: Point, r: borrow Point, b: Box<Point>) -> I64 {
    p.sum() + r.sum() + b.sum()
    // p: step Point, adjustment borrow
    // r: step borrow Point, by value (the receiver type is borrow Point)
    // b: step Point after one dereference of Box, adjustment borrow
}
```

### 5.13.5 Failure

- If no step matches, the error is `METHOD_NOT_FOUND`. It names the receiver type and the method, with these additions:
  - If a trait **not in scope** has a method `m` that would have matched, the error is instead `TRAIT_NOT_IN_SCOPE`. It names the trait and offers the fix `use <package path>.{Tr}` (machine-applicable when exactly one such trait exists).
  - If the receiver has a field `m` of callable type, the fix is "write `(e.m)(..)`" ([06](06-expressions-and-statements.md) N-C-10).
  - If an associated function `m` without `self` exists, the note is "call it as `T.m(..)`".
  - If a step is a distinct type whose base has a method `m`, the note is "distinct types inherit nothing; use `.value.m(..)` or derive the trait" (N-A-24).
- **Visibility does not affect selection.** If the selected inherent method is not visible from `M` (04 §4.6), the error is `PRIVATE_ITEM` (04). Lookup does not continue to later candidates. Trait methods have the visibility of their trait. Rationale: adding a private method to a type never changes which public method a dependent package calls (P8).

```sai
trait Named {
    fn name(self: borrow Self) -> String
}

trait Labeled {
    fn name(self: borrow Self) -> String
}

fn show<T: Named + Labeled>(x: borrow T) -> String {
    x.name()                         // rejected: AMBIGUOUS_METHOD (fix: `Named.name(x)` or `Labeled.name(x)`)
}

fn show_named<T: Named + Labeled>(x: borrow T) -> String {
    Named.name(x)                    // OK: path call, no probe
}
```

### 5.13.6 Associated item paths

A value path `P.name` whose prefix `P` resolves to a type (04 §4.5.2, [06](06-expressions-and-statements.md) §6.4) is resolved as follows. `P` may be a nominal type with or without generic arguments, a primitive, a type parameter, `Self`, or a projection.

1. **Variants.** If `name` is a UIDENT and `P` is an enum type, `P.name` is the variant `name`. If the enum has no such variant, the error is `UNKNOWN_VARIANT`, with "did you mean" suggestions. If `P` is not an enum, a UIDENT member in value position is `UNKNOWN_VARIANT` with the note "`P` is not an enum". (By D-LEX-02, variants are UIDENT and functions are LIDENT, so the two cannot be confused.)
2. **Inherent associated functions.** If `name` is a LIDENT, the inherent impls of the head of `P` are searched first (methods and non-method functions alike).
3. **Trait associated functions.** Otherwise the traits in scope (§5.13.3, with rule 3 applied to `P` itself) are searched for an item `name` such that `P : Tr` does not fail. One trait selects that item. Two or more traits are `AMBIGUOUS_METHOD`.
4. **Compiler-provided functions.** `R.try_from` on refined types and on distinct types with a refinement clause ([09](09-contracts-and-refinements.md) N-C-41) are found at step 2, as if declared inherently.
5. If nothing is found, the error is `METHOD_NOT_FOUND`, with the same additions as §5.13.5.

A path with a trait prefix `Tr.name` names the trait item directly. `Self` is inferred from the arguments, and an unknown item is `METHOD_NOT_FOUND`. A path `T.name` with `T` a type parameter searches only the traits of `T`'s bounds (rule 3).

Path calls perform **no autoderef and no auto-ref**. Every argument, the receiver included, is an ordinary argument at a coercion site. `Point.sum(borrow p)` is the explicit form of `p.sum()`.

`Self` in a type or value path outside an impl or trait is `SELF_OUTSIDE_IMPL`. Inside an inherent impl it is the impl's self type. Inside a trait impl it is the impl's self type. Inside a trait it is the trait's `Self` parameter (§5.2.1).

## 5.14 Type equality

Two types are **equal** (`τ ≡ υ`) iff, after alias expansion and normalization of projections (§5.8.5), they are syntactically identical modulo regions:

- **Nominal types** (records, enums, distinct, refined, resources) are equal iff they are the same declaration and their generic arguments are pairwise equal. Declarations are identified by canonical path (04 §4.9). Two declarations with identical bodies are different types.
- **Primitives** are equal iff they are the same name after `Int ↦ I64` and `UInt ↦ U64`. `Byte ≢ U8`.
- **Tuples**, **arrays**, **views**, **`borrow`** and **`borrow mut`** are compared structurally. `borrow τ ≢ borrow mut τ`.
- **Const arguments** are equal iff they are the same value of the same type, or the same const parameter.
- **Function item, function pointer and closure types** are as in §5.5.
- **Rigid projections** `π = υ.Item` (for `υ : Tr<σ̄>`) are equal iff the trait, the self type and the trait arguments are equal, and the item is the same.
- **Regions** are ignored by `≡`. Region relations are checked by [07](07-ownership-and-borrowing.md). Region subtyping (D-TYPE-04 item 8) is the only subtyping in SAI. There is no subtyping between nominal types, and no variance-based conversion of type arguments other than regions.

**Unification** is equality with inference variables (§5.16.3). A failure is `TYPE_MISMATCH`. The diagnostic names the expected and found types and the origin of the expectation, such as a parameter, an annotation, or the previous branch. It adds a targeted fix for the explicit conversions of §5.15.4 when one applies.

## 5.15 Coercions

### 5.15.1 Rules

The implicit coercions are exactly the following (D-TYPE-04). `F ⇝ E` means a value of type `F` is accepted where `E` is expected.

| Rule | From `F` | To `E` | Condition | D-TYPE-04 item |
|---|---|---|---|---|
| C-Never | `Never` | any `E` | none | 1 |
| C-Reborrow | `borrow mut τ` | `borrow mut υ` | `τ ≡ υ`; the new borrow has a shorter region and does not move the original ([07](07-ownership-and-borrowing.md)) | 2 |
| C-MutToShared | `borrow mut τ` | `borrow υ` | `τ ≡ υ` | 3 |
| C-SliceMut | `SliceMut<τ>` | `Slice<υ>` | `τ ≡ υ` | 4 |
| C-FnPtr | `fndef(f, ḡ)` | `fnptr(u, Ū → S, E)` | §5.15.2 | 5 |
| C-RefinedToBase | `R` (refined, base `B`) | `υ` | `B ≡ υ` | 6 |
| C-RefinedBorrow | `borrow R` (refined, base `B`) | `borrow υ` | `B ≡ υ` ([09](09-contracts-and-refinements.md) N-C-40) | 6 (shared-borrow form) |
| C-BaseToRefined | `υ` | `R` (refined, base `B`) | `υ ≡ B` and the SFC proves `R`'s predicate for the value; otherwise `REFINEMENT_NOT_PROVEN` ([09](09-contracts-and-refinements.md) §9.2.3) | 7 |
| C-Region | `τ` with regions `ρ̄` | `τ` with regions `ρ̄'` | region subtyping, checked by [07](07-ownership-and-borrowing.md) | 8 |

A refined type in these rules means `type R = B where p`. A distinct type with a refinement clause takes no part in C-RefinedToBase, C-RefinedBorrow or C-BaseToRefined. It is converted explicitly with `D(e)` and `.value` (D-TYPE-03).

C-RefinedBorrow is the shared-borrow form of item 6. [09](09-contracts-and-refinements.md) N-C-40 introduces it. `borrow mut R` never coerces to `borrow mut B`.

### 5.15.2 Function item to function pointer

`fndef(f, ḡ) ⇝ fnptr(u, U1 … Un → S, E)` holds iff:

1. `f` is not `async`, and it is not a trait method whose `Self` is still an unresolved variable.
2. If `f` is `unsafe`, then `u` is `unsafe`. A safe `f` may coerce to either a safe or an `unsafe` pointer.
3. `f` has exactly `n` parameters, and after instantiating `f`'s generic parameters with `ḡ` (fresh variables if not yet known), each parameter type unifies with `Ui` and the return type unifies with `S`. Parameter and return types must be equal. No coercion is applied inside the signature.
4. From Stage 3, `f`'s effects are permitted by `E` as [08](08-effects-and-capabilities.md) defines. Otherwise the error is `FN_POINTER_EFFECTS_MISMATCH` (08). Before Stage 3, effects are not compared (§5.5.2).

Closure types never take part in this rule (N-A-31).

### 5.15.3 Where and how coercions apply

Coercions apply only at **coercion sites** (D-TYPE-04): annotated `let` initializers, function and method arguments (excluding the auto-ref'd receiver), `return` operands and tail expressions of bodies, field initializers in constructions, the right-hand side of assignments, array elements, and the arms of `match` and branches of `if` when an expected type exists. [06](06-expressions-and-statements.md) §6.1.3 lists the sites in grammar terms. Everywhere else, types are unified.

At a coercion site with expected type `E` and found type `F` (N-A-40):

1. Shallow-resolve both. If `F` is `Never`, accept (C-Never). An inference variable in `E` is left unconstrained by this.
2. If either `F` or `E` is an inference variable, unify them. No coercion is attempted, so a coercion is never guessed.
3. Otherwise try to unify `F` and `E` in a snapshot. On success, commit (no coercion).
4. Otherwise roll back, and select the rule by the **heads** of `F` and `E`:
   - `borrow mut` → `borrow mut`: C-Reborrow (region only, already covered by unification of the referents).
   - `borrow mut` → `borrow`: C-MutToShared.
   - `SliceMut` → `Slice`: C-SliceMut.
   - function item → function pointer: C-FnPtr.
   - `borrow R` → `borrow` of a non-`R` type: C-RefinedBorrow.
   - refined `R` → any type other than `R`: C-RefinedToBase.
   - any type other than `R` → refined `R`: C-BaseToRefined.

   Then unify the inner types as the rule states. If no rule applies, or the inner unification fails, the error is `TYPE_MISMATCH`.
5. At most **one** coercion other than C-Region is applied per site. Coercions do not chain: `borrow mut R` is not accepted where `borrow B` is expected. Coercions do not apply below a type constructor: `Option<borrow mut T>` does not coerce to `Option<borrow T>`.

Branches without an expected type (`if`/`match` whose value is synthesized) are unified with each other. A branch of type `Never` is skipped, and the remaining branches determine the type ([06](06-expressions-and-statements.md)). There is no least-upper-bound computation.

```sai
fn first(items: Slice<I64>) -> I64 { 0 }
fn double(x: I64) -> I64 { x * 2 }
fn apply(f: fn(I64) -> I64, x: I64) -> I64 { f(x) }

fn from_mut(items: SliceMut<I64>) -> I64 {
    first(items)                     // C-SliceMut at an argument
}

fn from_item() -> I64 {
    apply(double, 21)                // C-FnPtr
}

fn from_vec(v: Vec<I64>) -> I64 {
    first(v)                         // rejected: TYPE_MISMATCH (fix: `v.as_slice()`)
}

fn from_closure() -> I64 {
    apply(fn(x) { x * 2 }, 21)       // rejected: TYPE_MISMATCH (closures never coerce to function pointers)
}
```

### 5.15.4 Explicit conversions

The following are **not** coercions. The `TYPE_MISMATCH` diagnostic offers the explicit form shown:

| From | To | Explicit form |
|---|---|---|
| `String`, `borrow String` | `Str` | `s.as_str()` |
| `Vec<T>`, `borrow Vec<T>` | `Slice<T>` | `v.as_slice()` |
| `Array<T, N>` | `Slice<T>` | `a.as_slice()` |
| `T` | `borrow T` | `borrow e` (D-OWN) |
| `Box<T>` | `borrow T` | `borrow *b` ([06](06-expressions-and-statements.md) §6.9) |
| a numeric type | another numeric type | `T.from(x)`, `T.try_from(x)`, `x.wrapping_to<T>()` and friends (D-TYPE-06) |
| `B` | distinct `D` | `D(b)` |
| distinct `D` | `B` | `d.value` |
| `B` | refined `R`, unprovable | `R.try_from(b)` ([09](09-contracts-and-refinements.md)) |
| `Vec<R>`, `Option<R>`, … | `Vec<B>`, `Option<B>`, … | an explicit `map` |
| closure | function pointer | a function item, or a generic parameter bounded by `Fn` |

## 5.16 Type inference

### 5.16.1 Scope

Inference is **local to a body**: a function body, a `const` initializer, or a `test` or `property` body. The closures inside it are inferred together with the body. Item signatures are always fully annotated (D-TYPE-05, `INFER_IN_SIGNATURE` in 03). No information flows between bodies, except through signatures.

### 5.16.2 Inference variables

| Variable | Created for | Unifies with |
|---|---|---|
| `α` (general) | omitted generic arguments, `_` in annotations and generic arguments, unannotated `let` bindings and closure parameters, `Vec.new()`-style results | any type, subject to the occurs check |
| `ι` (integer literal) | a synthesized integer literal (§5.3.2) | another `ι` or `α`, or one of the ten integer types (§5.3.1) |
| `φ` (float literal) | a synthesized float literal | another `φ` or `α`, or `F32` or `F64` |
| `κ` (const) | an omitted const generic argument | another `κ` or a const value of the parameter's type |

Unifying `ι` with any other type (`Byte`, a float type, a distinct type, `Bool`, …) is `TYPE_MISMATCH`. Unifying `α` with `ι` makes the result an `ι`.

### 5.16.3 Unification

Unification is first-order and syntactic:

- A variable unifies with a type that does not contain it, and is bound to it. A type that does contain it is `TYPE_MISMATCH` with the note "the type would be infinite".
- Two types with known heads unify iff their heads are equal (§5.14) and their arguments unify pairwise.
- A projection containing variables is not unified structurally. It is deferred as an equality obligation `π = τ`, and retried when its variables are resolved (§5.11.3).
- Bindings are recorded in a trail, so a **snapshot** can be rolled back (§5.15.3 step 3, §5.16.6).

### 5.16.4 Points where a type must be known

At the following points the checker needs the head of a type now. Pending obligations are processed first (§5.11.3). If the type is still an `ι` or `φ`, it is **defaulted immediately** to `I64` or `F64` (N-A-41). If it is still an `α`, the error is `TYPE_ANNOTATIONS_NEEDED`.

- the receiver of a method call (§5.13.1), and the base of a field access (06 §6.6);
- the operand of a dereference `*e` (06 §6.9), and the base of an index expression (06 §6.7);
- the callee of a call whose type is not a function item ([06](06-expressions-and-statements.md) §6.5);
- the expected type of an implicit member `.V` (06 N-C-3), which must already be an enum;
- the operand of `?` ([10](10-errors-and-panics.md)), and the iterated value of `for` ([06](06-expressions-and-statements.md) §6.16);
- the scrutinee of `match`, `if let` and `let … else` when a pattern other than a binding or `_` must be checked against it.

Immediate defaulting makes these points deterministic: `let n = 7` followed by `n.count_ones()` uses `I64`, even if `n` is later used where `U8` is expected. That later use is then a `TYPE_MISMATCH`, whose fix suggests annotating `let n: U8 = 7`.

### 5.16.5 End-of-body defaulting

When the body has been checked:

1. Process pending obligations to a fixed point.
2. Default every remaining `ι` to `I64` and every `φ` to `F64` (D-TYPE-05).
3. Process obligations again. A literal's range is checked now that its type is known (`LITERAL_OUT_OF_RANGE`, 02).
4. Every remaining `α` or `κ` is `TYPE_ANNOTATIONS_NEEDED`. It is reported once per variable, at the leftmost expression whose type contains it, with a fix that names where an annotation would help (the `let`, or explicit generic arguments on the path).

```sai
fn defaults() {
    let a = 1                        // ι, defaulted to I64 at the end
    let b = [1.5, 2.5]               // Array<φ, 2>, defaulted to Array<F64, 2>
    var v = Vec.new()                // Vec<α>
    v.push(a)                        // α := I64
    let w = Vec.new()                // rejected: TYPE_ANNOTATIONS_NEEDED (fix: `let w: Vec<I64> = Vec.new()`)
}
```

### 5.16.6 Expected types, calls and closures

- **Bidirectional checking.** A form checked against an expected type passes it down, as [06](06-expressions-and-statements.md) defines for each form.
- **Generic calls.** At a call of a generic function or a constructor without explicit generic arguments, each generic parameter gets a fresh variable (`α` or `κ`). If an expected type exists, the instantiated return type is unified with it **in a snapshot**. On failure the snapshot is rolled back, and the mismatch, if any, is reported later at the coercion site. This lets `let v: Vec<I64> = Vec.new()` and closure arguments see the expected types.
- **Argument order.** Arguments are checked left to right, except that closure-expression arguments are checked **after** all other arguments of the same call, still left to right among themselves (N-A-41). Evaluation order is unaffected ([06](06-expressions-and-statements.md)).
- **Closure signatures.** A closure checked against an expected type that is a type parameter or variable `F` with a pending obligation `F : Fn(Ā) -> S` (or `FnMut`, `FnOnce`) takes its unannotated parameter types from `Ā` and its expected body type from `S`. Annotated parameters must equal `Ā`. Without such an expectation, unannotated parameters are fresh `α`, resolved from the body ([06](06-expressions-and-statements.md) §6.17). An expected function pointer type does not give a closure its signature, because the closure could not coerce to it anyway.
- **Explicit generic arguments** are all or none (N-A-21). `_` in an explicit argument list is a fresh variable: `parse<_>(s)` is the same as `parse(s)`. A const argument `_` is a fresh `κ`.
- **Annotations are required** on function parameters and returns (D-TYPE-05), and wherever §5.16.4 or §5.16.5 reports `TYPE_ANNOTATIONS_NEEDED`. They are never required merely because a type is generic.

## 5.17 Well-formedness

`Δ ⊢ τ wf` states that `τ` is a valid type in the environment `Δ`. It is checked for every type written in a declaration, signature, impl header, bound, annotation or explicit generic argument, and for every instantiation of a generic entity at a use:

| Form | Well-formed iff |
|---|---|
| primitive, `()`, `Never`, `Str` | always |
| type parameter `T` | `T` is declared in `Δ` |
| `Self` | inside an impl or a trait (`SELF_OUTSIDE_IMPL`); it denotes the impl's self type or the trait's `Self` |
| nominal `N<ā>` | the arguments match `N`'s parameters in number and kind (§5.2.2); each argument is wf; `N`'s declared predicates, substituted, hold in `Δ` (`UNSATISFIED_BOUND`; no implied bounds, N-A-28); `N` is visible ([04](04-modules-and-names.md) §4.6) |
| alias `A<ā>` | it is expanded first (§5.6.5), and the expansion is wf; the alias's bounds hold for `ā` (N-A-26) |
| tuple `(τ1, …, τn)` | each `τi` is wf |
| `Array<τ, c>` | `τ` is wf; `c` is a const of type `USize`: a literal or a const parameter (D-TYPE-07, §5.7.5) |
| `borrow τ`, `borrow mut τ`, `Slice<τ>`, `SliceMut<τ>`, `Box<τ>`, `Vec<τ>` | `τ` is wf |
| `fn(τ̄) -> υ effects { E }` | each `τi` and `υ` is wf; `E` is a valid effect set ([08](08-effects-and-capabilities.md)) |
| projection `υ.A` | `υ` is `Self`, a parameter or a projection (N-A-29); exactly one trait of the bounds of `υ` in `Δ` has an associated type `A` (§5.8.5) |
| `_` | only in bodies (`INFER_IN_SIGNATURE`, 03) |

A generic item is checked once, under its own `Δ` (§5.7.1). Instantiating it never re-checks its body. The only check at instantiation is that the arguments satisfy the item's predicates, which is a well-formedness check at the use (`UNSATISFIED_BOUND`). Monomorphization adds only the depth limit of §5.7.6.

Public signatures are additionally subject to the visibility rule `PRIVATE_TYPE_IN_PUBLIC_API` ([04](04-modules-and-names.md) §4.6.7). This is a name rule, not part of `wf`.

## 5.18 `Copy`, `Clone` and `Drop`

This section collects the type-level rules. Moves, copies and drop order are in [07](07-ownership-and-borrowing.md).

**`Copy` types.** A type is `Copy` iff one of these applies:

- it is a primitive, `()` or `Never`;
- it is `borrow τ`, `Str` or `Slice<τ>`;
- it is a function item type or a function pointer type;
- it is a tuple or `Array` whose elements are `Copy` (D-TYPE-08);
- it is a closure type all of whose captures are `Copy` (§5.5.3);
- it is a refined type whose base is `Copy` (§5.6.4);
- it is a record, enum or distinct type with `@derive(Copy)` whose instantiation satisfies the derived impl's predicates (§5.12.1).

No other type is `Copy`. In particular `borrow mut τ`, `SliceMut<τ>`, `String`, `Vec<τ>`, `Box<τ>`, resources (D-OWN-06) and every nominal type without `@derive(Copy)` are not.

**Rules.**

1. `Copy` can only come from `@derive(Copy)` or from the compiler-provided impls of §5.11.4. A hand-written `impl Copy for …` is `MANUAL_COPY_IMPL` (N-A-39, D-TYPE-08).
2. `Copy` has `Clone` as a supertrait. A use of a `Copy` value by value copies its bits and never calls `Clone.clone` (N-A-39). `.clone()` written explicitly calls the `Clone` impl, which for derived impls is a copy.
3. `Clone` is an ordinary trait. It is never called implicitly.
4. A type cannot be both `Copy` and have a `Drop` impl (`COPY_AND_DROP`, [07](07-ownership-and-borrowing.md)). A `Copy` type cannot contain a field with drop code, because every field of a `Copy` type must be `Copy` (`DERIVE_FIELD_MISSING_IMPL`).
5. `Drop` impls follow §5.9.3. A type has **drop code** if it has a `Drop` impl, or has a field, element or payload with drop code, or is `String`, `Vec`, `Box` or a resource. Drop code runs as [07](07-ownership-and-borrowing.md) defines.

## 5.19 Numeric semantics

Integer overflow, division, shifts and float behavior follow D-TYPE-06 and are specified with the operators in [06](06-expressions-and-statements.md) §6.8. This chapter fixes only the types: binary arithmetic operands have the same type, shift amounts are `U32`, and no numeric type converts implicitly to another (§5.15.4).

## 5.20 Stages

| Stage | This chapter |
|---|---|
| 0 | Type syntax and resolution (§5.2), primitives and literals (§5.3), tuples, arrays, views and the library types as opaque nominal types (§5.4), records, enums, distinct types without a refinement clause, aliases (§5.6), function item types (§5.5.1), equality, the coercions C-Never, C-Reborrow, C-MutToShared, C-SliceMut and C-Region, and inference (§5.14–§5.16), for non-generic programs. Using a generic declaration of the program is `FEATURE_NOT_IMPLEMENTED` (01). The library items of Stage 1 ([16](16-core-library.md) §16.1.6), among them `Option`, `Result` and `Array` with explicit or inferred arguments, are type-checked at Stage 0 and compiled at Stage 1. The other library types, such as `Slice`, `Vec` and `Box`, are Stage 2 items of §16.1.6 (N-J-9). |
| 1 | Code generation for the Stage 0 types ([impl/06](../impl/06-wasm-lowering-and-layout.md)). |
| 2 | Generics (§5.7), traits (§5.8), impls (§5.9), coherence (§5.10), trait solving (§5.11), `@derive` (§5.12), method resolution through traits (§5.13), closures and function pointers (§5.5.2–§5.5.4), C-FnPtr, `Copy`/`Clone`/`Drop` (§5.18). |
| 3 | Refined types and distinct types with a refinement clause (§5.6.3, §5.6.4), the coercions C-RefinedToBase, C-RefinedBorrow and C-BaseToRefined, the refined base step (§5.13.2) and the refined-type impls of §5.11.4, all together with [09](09-contracts-and-refinements.md), whose stage is 3. Effects in function pointer types and Fn bounds, and their comparison (§5.5.2, §5.15.2). |
| 4 | Resources (§5.6.6) and their interaction with components ([13](13-components-and-wit.md)). |
| 5 | Types of `async` functions ([11](11-async-and-concurrency.md)). |

A toolchain at a lower stage MUST reject a construct of a higher stage with `FEATURE_NOT_IMPLEMENTED` (01), and MUST NOT give it a different meaning.

## 5.21 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) leaves open. They are provisional until merged into it. N-A-1 to N-A-20 are in [04](04-modules-and-names.md) §4.11.

| ID | Rule | Rationale |
|---|---|---|
| N-A-21 | In expression position, generic arguments are all or none. In type position and in trait references, all must be given (§5.2.2). | One rule, no partial inference (P1). |
| N-A-22 | The variant orders `None, Some` and `Ok, Err` of `Option` and `Result` are normative (§5.4.5). | Derived orderings must be the same everywhere (P8). |
| N-A-23 | Data-carrying enums have discriminants 0, 1, … in declaration order. Unit-only enums may set them explicitly, and the others continue at previous + 1. The discriminant type is the `@repr(I)` type, `I32` for `@repr(C)`, or `I64`. Range and uniqueness are checked (§5.6.2). | Deterministic ordering and layout (P8). |
| N-A-24 | A distinct type inherits nothing: no methods, impls, operators or autoderef. Only `@derive` forwards (§5.6.3). | D-TYPE-03 made precise; the wrapper's API is explicit (P1). |
| N-A-25 | The base of a refined type cannot be refined. A refined type falls back to its base for field reads (immutable), for `self`/`borrow self` methods through a final autoderef base step, and for operators (§5.6.4, §5.13.2). | Refinement erasure ([09](09-contracts-and-refinements.md) N-C-39), stated at the type level; no predicate stacking (P1). |
| N-A-26 | Every alias parameter must be used (`UNUSED_ALIAS_PARAM`). Alias bounds are checked at each use after substitution (§5.6.5). | An alias is pure substitution (P1). |
| N-A-27 | Resource fields cannot contain views or borrows (`RESOURCE_WITH_BORROW`), and resources take no `@derive` (§5.6.6). | Resources cross to the host, where regions do not exist; resources are affine (D-OWN-06). |
| N-A-28 | No implied bounds from types in signatures. Only supertraits and associated-type bounds of traits are implied (§5.7.4). Subjects of `where` on a trait are `Self` or `Self.A`. | Every requirement is visible in the signature (P1). |
| N-A-29 | Projections `υ.A` are allowed only when `υ` is `Self`, a type parameter or a projection (§5.8.5). | No hidden normalization of concrete types (P8). |
| N-A-30 | Monomorphization fails with `INSTANTIATION_DEPTH_EXCEEDED` when a type argument nests deeper than 64 (§5.7.6). | Polymorphic recursion terminates with a clear error. |
| N-A-31 | A closure is `Copy`/`Clone` iff its captures are; a `borrow mut` capture makes it neither. Closures never coerce to function pointers. The `Fn` traits cannot be implemented by users (§5.5.3, §5.5.4). | Small, predictable function model (P1). |
| N-A-32 | A trait impl item must match the trait's signature exactly, up to renaming of its own generic parameters (§5.9.1). | No variance or refinement in impls (P1). |
| N-A-33 | Inherent impls are allowed only for nominal types of the current package, except that `sai:core` and `sai:alloc`, as one package, may also declare them on the primitive types, `Array<T, N>` and their lang-item types ([16](16-core-library.md) N-J-6). Item names are unique across all inherent impls of one type. No associated types (§5.9.2). | One name means one function; canonical paths stay unique (P8). |
| N-A-34 | A `Drop` impl must be exactly as generic as its type, for a local nominal type that is not refined (§5.9.3). | Whether a value has drop code never depends on type arguments (P8). |
| N-A-35 | Coherence: `borrow`, `borrow mut` and `Box` are fundamental; the owner table of §5.10.1; overlap is syntactic unification of headers, ignoring bounds; `core`+`alloc` are one package (§5.10). | D-TYPE-08 made precise. |
| N-A-36 | Trait solving: environment candidates before impl candidates; impls are global; inductive cycles fail; depth limit 128; ambiguity is deferred, then `TYPE_ANNOTATIONS_NEEDED` (§5.11). | Deterministic, terminating solving (P8). |
| N-A-37 | Method resolution: autoderef steps through `borrow`, `borrow mut`, `Box`, then a refined base step. At each step the adjustments are by value, then `borrow`, then `borrow mut`. Inherent candidates come before trait candidates. Traits in scope include the bound traits of type parameters. Visibility does not affect selection (§5.13). | D-TYPE-08 and D-OWN-02 made precise; adding a private method never changes a dependent's call (P8). |
| N-A-38 | `@derive` semantics: generated bounds `Ti: Tr`, or `B: Tr` for distinct types; supertraits are never derived implicitly; field order and discriminant order; forwarding for distinct types; refined types derive only `Arbitrary` (§5.12). | D-TYPE-08 made precise (P1, P8). |
| N-A-39 | `Copy` comes only from `@derive(Copy)` or compiler-provided impls. Copying never calls `Clone` (§5.18). | Copies are always bitwise and free of user code (P8). |
| N-A-40 | Coercions apply only when both heads are known, after a failed unification, at most one per site (other than regions), and never below a type constructor (§5.15.3). | Coercions are never guessed (P1). |
| N-A-41 | Integer and float literal variables default immediately at the points of §5.16.4, and otherwise at the end of the body. Closure arguments are checked after the other arguments of a call (§5.16). | Deterministic inference that serves the common closure case (P1). |
| N-A-66 | Derived impls have inferred effects. The derived `Arbitrary` of a recursive type halves the size for recursive fields and payloads and, at size 0, chooses only non-recursive variants; unguarded recursion is `DERIVE_NOT_SUPPORTED`. With an `invariant`, the derived `Arbitrary` filters with the rejection limit of 14 (§5.12.3). | A derived impl must terminate and must not panic for every type it accepts (P5, P8); the rule is deterministic and matches the size discipline of 14. |

## 5.22 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `TYPE_MISMATCH` | error | two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation |
| `TYPE_ANNOTATIONS_NEEDED` | error | a type that must be known (§5.16.4), or remains unknown at the end of the body (§5.16.5), or an obligation that stays ambiguous |
| `WRONG_NUMBER_OF_GENERIC_ARGS` | error | a generic argument list with the wrong number of arguments (§5.2.2) |
| `GENERIC_ARG_KIND_MISMATCH` | error | a type argument for a const parameter, or the reverse |
| `CONST_ARG_NOT_SIMPLE` | error | a const argument that is not a literal or a const parameter (D-TYPE-07) |
| `ASSOC_TYPE_BINDING_OUTSIDE_BOUND` | error | `A = T` in a trait reference that is not a bound |
| `UNKNOWN_ASSOC_TYPE` | error | an associated-type binding or projection naming no associated type of the trait |
| `UNSATISFIED_BOUND` | error | a trait obligation that no candidate satisfies (§5.11.2) |
| `DISCRIMINANT_OUT_OF_RANGE` | error | an enum discriminant outside the discriminant type |
| `DUPLICATE_DISCRIMINANT` | error | two variants with the same discriminant |
| `DUPLICATE_VARIANT` | error | two variants with the same name |
| `DUPLICATE_FIELD` | error | two fields with the same name in a record or record variant |
| `GENERIC_REFINED_TYPE` | error | a refined type, or a distinct type with a refinement clause, with generic parameters (03 §3.6) |
| `REFINED_BASE_IS_REFINED` | error | the base of a refined type is itself refined (N-A-25) |
| `UNUSED_ALIAS_PARAM` | error | a type alias parameter that does not occur in the aliased type (N-A-26) |
| `ALIAS_CYCLE` | error | a type alias that expands to itself |
| `RESOURCE_WITH_BORROW` | error | a resource field whose type contains a view or borrow (N-A-27) |
| `INFINITE_SIZE_TYPE` | error | a nominal type that contains itself by value (§5.6.7) |
| `INVALID_CONST_PARAM_TYPE` | error | a const parameter whose type is not an integer type, `Bool` or `Char` |
| `NOT_A_TRAIT` | error | a bound or impl names something that is not a trait |
| `WHERE_WITHOUT_PARAM` | error | a `where` predicate whose subject mentions no generic parameter of the item |
| `INVALID_TRAIT_WHERE_SUBJECT` | error | a `where` predicate on a trait whose subject is not `Self` or `Self.A` (N-A-28) |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | monomorphization nests a type argument deeper than 64 (N-A-30) |
| `SUPERTRAIT_CYCLE` | error | a trait that is its own supertrait, directly or transitively |
| `DUPLICATE_TRAIT_ITEM` | error | two items with the same name in one trait |
| `PROJECTION_ON_CONCRETE_TYPE` | error | `τ.A` with a concrete `τ` (N-A-29) |
| `AMBIGUOUS_ASSOCIATED_TYPE` | error | `T.A` where several bound traits of `T` have an associated type `A` |
| `UNCONSTRAINED_IMPL_PARAM` | error | an impl parameter that does not occur in the impl header |
| `MISSING_SUPERTRAIT_IMPL` | error | an impl (or derive) of a trait whose supertrait is not implemented for the type |
| `MISSING_TRAIT_ITEM` | error | a trait impl that lacks a required item |
| `UNKNOWN_TRAIT_ITEM` | error | a trait impl item that the trait does not declare |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | a trait impl item whose signature does not match the trait exactly (N-A-32) |
| `MANUAL_COPY_IMPL` | error | a hand-written `impl Copy` (N-A-39) |
| `MANUAL_FN_IMPL` | error | a hand-written impl of `Fn`, `FnMut` or `FnOnce` (N-A-31) |
| `INHERENT_IMPL_FOREIGN_TYPE` | error | an inherent impl for a type not declared as a nominal type in the current package, other than the core-library exception of §5.9.2 rule 1 (N-A-33) |
| `ASSOC_TYPE_IN_INHERENT_IMPL` | error | an associated type in an inherent impl |
| `DUPLICATE_INHERENT_ITEM` | error | two inherent items with the same name across the inherent impls of one type |
| `DROP_IMPL_INVALID_TYPE` | error | `impl Drop` for a type that is not a local record, enum, distinct type or resource |
| `DROP_IMPL_NOT_GENERIC_ENOUGH` | error | a `Drop` impl that is not exactly as generic as its type (N-A-34) |
| `ORPHAN_IMPL` | error | a trait impl that violates the orphan rule (§5.10.1) |
| `PROJECTION_IN_IMPL_HEADER` | error | a projection in the self type or trait arguments of an impl header |
| `OVERLAPPING_IMPLS` | error | two impls, derived impls or compiler-provided impls whose headers unify (§5.10.2) |
| `TRAIT_SOLVER_OVERFLOW` | error | trait solving exceeded depth 128 (N-A-36) |
| `UNKNOWN_DERIVE` | error | a name in `@derive` that is not a derivable trait |
| `DERIVE_NOT_SUPPORTED` | error | a derivable trait that cannot be derived for this kind of declaration (§5.12.4) |
| `DERIVE_FIELD_MISSING_IMPL` | error | a field, payload or base type that does not implement the derived trait |
| `METHOD_NOT_FOUND` | error | no method or associated function of that name for the type (§5.13.5) |
| `TRAIT_NOT_IN_SCOPE` | error | the method exists only in a trait that is not in scope; the fix adds a `use` |
| `AMBIGUOUS_METHOD` | error | methods or associated functions of two or more traits match at the same step |
| `UNKNOWN_VARIANT` | error | `E.V` naming no variant of `E`, or a UIDENT member of a non-enum type |
| `SELF_OUTSIDE_IMPL` | error | `Self` outside an impl or trait |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5 |
| `LITERAL_OUT_OF_RANGE` | error | defined in [02](02-lexical-structure.md) §2.6.1 |
| `INFER_IN_SIGNATURE` | error | defined in [03](03-grammar.md) §3.15 |
| `INVALID_SELF_TYPE` | error | defined in [03](03-grammar.md) §3.5 |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | error | defined in [03](03-grammar.md) §3.6 |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | defined in [03](03-grammar.md) §3.7 |
| `DUPLICATE_BINDING` | error | defined in [04](04-modules-and-names.md) §4.4.3 |
| `SHADOWS_GENERIC_PARAM` | error | defined in [04](04-modules-and-names.md) §4.5.1 |
| `WRONG_NAME_KIND` | error | defined in [04](04-modules-and-names.md) §4.5.1 |
| `PRIVATE_ITEM` | error | defined in [04](04-modules-and-names.md) §4.3.2 |
| `PRIVATE_TYPE_IN_PUBLIC_API` | error | defined in [04](04-modules-and-names.md) §4.6.7 |
| `OPERATOR_NOT_SUPPORTED` | error | defined in [06](06-expressions-and-statements.md) §6.8.2 |
| `UNKNOWN_FIELD` | error | defined in [06](06-expressions-and-statements.md) §6.6.1 |
| `COPY_AND_DROP`, `EXPLICIT_DROP_CALL` | error | defined in [07](07-ownership-and-borrowing.md) §7.13.6 |
| `FN_POINTER_EFFECTS_MISMATCH` | error | defined in [08](08-effects-and-capabilities.md) §8.6.4 |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | defined in [08](08-effects-and-capabilities.md) §8.6.3 |
| `ASYNC_FN_IN_TRAIT` | error | defined in [11](11-async-and-concurrency.md) §11.2.6; an `async fn` in a trait or a trait impl (§5.9.1 rule 6) |
| `REFINEMENT_NOT_PROVEN` | error | defined in [09](09-contracts-and-refinements.md) §9.2.3 |
| `REFINED_PLACE_MUTATION` | error | defined in [09](09-contracts-and-refinements.md) §9.2.2 |
| `INVALID_ATTRIBUTE_TARGET` | error | defined in [15](15-attributes-and-lints.md) §15.2.4 |
| `DUPLICATE_ATTRIBUTE_ARGUMENT` | error | defined in [15](15-attributes-and-lints.md) §15.2.4 |
