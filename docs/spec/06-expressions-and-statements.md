# 06. Expressions and Statements

**Status:** Normative
**Stage:** Stage 0–2 (f-strings Stage 3; `async` blocks and `for await` Stage 5)
**Related:** [02 Lexical Structure](02-lexical-structure.md), [03 Grammar](03-grammar.md), [04 Modules and Names](04-modules-and-names.md), [05 Types](05-types.md), [07 Ownership and Borrowing](07-ownership-and-borrowing.md), [08 Effects and Capabilities](08-effects-and-capabilities.md), [09 Contracts and Refinements](09-contracts-and-refinements.md), [10 Errors and Panics](10-errors-and-panics.md), [11 Async and Concurrency](11-async-and-concurrency.md), [12 Unsafe and FFI](12-unsafe-and-ffi.md), [16 Core Library](16-core-library.md), [decisions](../design/decisions.md)

This chapter defines the static and dynamic semantics of every SAI expression and statement form, of patterns, and of `match`. It covers typing rules, evaluation order, place and value expressions, temporaries and their scopes, divergence, and the exhaustiveness and usefulness algorithm. The concrete syntax is in [03](03-grammar.md); this chapter refers to its nonterminals by name. Type inference, method resolution and coercions belong to [05](05-types.md). Moves, borrows, regions and drop order belong to [07](07-ownership-and-borrowing.md). Effects belong to [08](08-effects-and-capabilities.md). This chapter states only what it needs from those chapters and links to them.

## 6.1 Conventions

### 6.1.1 Judgment forms

The judgment conventions of [01](01-introduction.md) apply. This chapter uses these forms:

| Form | Meaning |
|---|---|
| `Γ ⊢ e ⇒ T ! E` | `e` synthesizes type `T`; evaluating it may perform effects `E` |
| `Γ ⊢ e ⇐ T ! E` | `e` checks against the expected type `T` (with coercion if the position is a coercion site, §6.1.3) |
| `Γ ⊢ e : T ! E` | either of the two, where the distinction does not matter |
| `Γ ⊢ₚ e : T @ μ` | `e` is a place expression of type `T` with mutability `μ ∈ {imm, mut}` (§6.2) |
| `Γ ⊢ P ⇐ T ⊣ Δ` | pattern `P` matches values of type `T` and introduces the bindings `Δ` (§6.21) |
| `Γ ⊢ s ⊣ Γ' ! E` | statement `s` is well formed and yields the environment `Γ'` for the following statements |

The environment `Γ` contains:

- `Γ.vars`: a finite map from local names to `(type, kind)`, where kind is `let`, `var`, or `param` / `var param`.
- `Γ.ret`: the return type of the innermost enclosing function or closure body.
- `Γ.loops`: a stack of loop entries `(label?, form, breakTy)`. `form` is `loop`, `while` or `for`. `breakTy` is the type that `break` values must have, or `()` for `while` and `for`.
- `Γ.unsafe`: whether the expression is in an unsafe context ([12](12-unsafe-and-ffi.md) §12.2): lexically inside an `unsafe` block, or in the body of an `unsafe fn`, which is an unsafe context ([12](12-unsafe-and-ffi.md) §12.2.2 item 2, N-I-22). The bodies of closures and `async` blocks nested in either are not, except in an `unsafe module`, where every function, closure and `async` block body is an unsafe context ([12](12-unsafe-and-ffi.md) §12.2.4). Predicates are never unsafe contexts.
- `Γ.async`: whether the innermost body is an `async fn` or `async` block ([11](11-async-and-concurrency.md)).
- `Γ.const`: whether the expression is a const initializer (§6.27).

A closure body and an `async` block start a new function boundary. `Γ.ret` is replaced, and `Γ.loops` is reset to empty.

Effect sets `E` are defined in [08](08-effects-and-capabilities.md). Unless a rule says otherwise, the effect of a compound expression is the union of its subexpressions' effects, plus the latent effects of any function it calls. Rules below elide `! E` when the form adds no effect of its own.

### 6.1.2 Bidirectional checking

Typing is bidirectional (D-TYPE-05). Every form below says whether it synthesizes or checks, and what expected type it passes to its subexpressions. When a form is checked against `T` but only has a synthesis rule, the checker synthesizes `T'` and then requires `T' ≤ T`. That means `T'` equals `T` after unification, or `T'` coerces to `T` if the position is a coercion site. Unification, inference variables and defaulting are owned by [05](05-types.md). A mismatch is `TYPE_MISMATCH` (05).

An expected type is **known** when, after shallow resolution of inference variables, its head is not an unresolved inference variable. Several rules (implicit members §6.4.3, empty arrays §6.18.5, closure parameters §6.17.2) need a known expected type.

### 6.1.3 Coercion sites

Coercions form the closed list of D-TYPE-04 and are applied only at these sites:

1. the initializer of a `let` or `var` with a type annotation
2. call and method-call arguments, against the parameter type
3. the operand of `return`, and the tail expression of a function or closure body, against the return type
4. record field initializers and distinct-constructor arguments, against the field or base type
5. the right-hand side of `=`, against the place type
6. array elements, against the element type
7. `match` arm bodies and `if` branches when the whole expression has a known expected type
8. `break` values, against the loop's break type once it is fixed (§6.16.1)

At every other position, types must unify exactly. There is no numeric widening and no `String` to `Str` coercion (D-TYPE-04). Coercion from `Never` (rule 1 of D-TYPE-04) applies at every position, coercion site or not, because a `Never` value is never produced (§6.26).

### 6.1.4 Panic notation

"Panics with kind `K`" means the operation behaves as a panic of `AssertKind K` (D-MIR-01). The resulting behavior (report, then trap) is defined in [10](10-errors-and-panics.md). Every check described as a panic in this chapter is performed in every profile (D-TGT-02). Profiles never change semantics.

## 6.2 Place and value expressions

### 6.2.1 Classification

Every expression is either a **place expression** (it denotes a memory location) or a **value expression** (it denotes a value). The place expressions are exactly:

1. a path that resolves to a local binding or parameter, including `self`
2. `e.f`, where `e` is a place expression, or a value expression that has been materialized into a temporary (§6.24), and `f` is a field
3. `e.N` (tuple field), under the same condition as 2
4. `e[i]` when the indexing is built-in (§6.7.1) and `e` is a place
5. `*e` (§6.9.1)
6. `(e)` where `e` is a place expression
7. the implicit dereferences that auto-deref inserts for field access (§6.9.3)

Every other expression is a value expression. In particular, results of calls, operators, literals, construction expressions, blocks, `if` and `match` are value expressions, and `Index`-based indexing is a place only through its desugaring `*Index.index(..)` (§6.7.2).

### 6.2.2 Place contexts and value contexts

A **place context** is an operand position that needs a location:

- the left-hand side of `=` and of compound assignment
- the operand of `borrow` and `borrow mut`
- the base of a field access, tuple field access or built-in index
- the receiver of a method call when auto-ref applies ([05](05-types.md))
- the scrutinee of `match`, `if let` and `while let`, and the initializer of `let` when the pattern does not bind the whole value by value (§6.21.6)
- the operands of comparison operators (§6.8.5) and the interpolated expressions of f-strings (§6.20.3)

Every other operand position is a **value context**. Then:

- A place expression in a value context is **read**. If its type is `Copy`, the value is copied. Otherwise it is moved out of the place ([07](07-ownership-and-borrowing.md), D-OWN-01).
- A value expression in a place context is evaluated and stored in a fresh temporary, and the temporary is the place (§6.24).
- A value expression in a place context that requires mutability is allowed; a temporary is mutable.

### 6.2.3 Place mutability

The mutability `μ` of a place is computed structurally. The authoritative rules, including the uniqueness condition for places behind `borrow mut` and `SliceMut`, are in [07](07-ownership-and-borrowing.md) §7.2.4. The table below summarizes them for the common cases:

```text
x bound by var, var param, or a `var` pattern binding  ⇒ mut
x bound by let, param, or a non-var pattern binding    ⇒ imm
a temporary                                            ⇒ mut
p.f, p.N, p[i] (built-in on Array)                     ⇒ μ(p)
s[i] (built-in) with s : SliceMut<T>                   ⇒ mut
s[i] (built-in) with s : Slice<T>                      ⇒ imm
*e with e : borrow mut T                               ⇒ mut
*e with e : borrow T                                   ⇒ imm
*e with e : Box<T>                                     ⇒ μ(e)
*p with p : PtrMut<T> (unsafe context only)            ⇒ mut
*p with p : Ptr<T> (unsafe context only)               ⇒ imm
```

The two raw-pointer rows are those of [12](12-unsafe-and-ffi.md) §12.4.4. A dereference of a raw pointer outside an unsafe context is `UNSAFE_OPERATION_OUTSIDE_UNSAFE` ([12](12-unsafe-and-ffi.md) §12.2.5).

A place used where `mut` is required (left-hand side of assignment, operand of `borrow mut`, receiver auto-ref'd as `borrow mut`) whose mutability is `imm` is an error. Assignment gives `ASSIGN_TO_IMMUTABLE`. `borrow mut` gives `BORROW_MUT_OF_IMMUTABLE`. Both codes and their structured fixes are defined in [07](07-ownership-and-borrowing.md) §7.2.4.

Fields of a type with an `invariant` are additionally not writable outside its defining module (D-CONTRACT-04). The rule and its diagnostic are in [09](09-contracts-and-refinements.md).

## 6.3 Literals

### 6.3.1 Integer literals

An integer literal `n` (02 §2.6) has an arbitrary-precision value and no type of its own. A literal directly preceded by `-` in the token stream is a single negative constant with value `-n` (D-LEX-04).

```text
(T-IntLit-Check)
T ∈ {I8, I16, I32, I64, ISize, U8, U16, U32, U64, USize}     value(n) ∈ range(T)
───────────────────────────────────────────────────────────────────────
Γ ⊢ n ⇐ T

(T-IntLit-Synth)
α fresh integer-literal inference variable
───────────────────────────────────────
Γ ⊢ n ⇒ α
```

- An integer-literal variable unifies only with the integer types listed above. If it is still unresolved at the end of inference for the enclosing body, it defaults to `Int` (`I64`), as defined in [05](05-types.md).
- If the value does not fit the resolved type, the error is `LITERAL_OUT_OF_RANGE` (02). The check is done on the exact value after resolution, so `-128` checks against `I8` and `-0` checks against `U8`.
- An integer literal never has a floating-point type. `let x: F64 = 1` is `TYPE_MISMATCH` with the fix "write `1.0`" (N-C-1).
- An integer literal never has a distinct type (D-TYPE-03). `let m: Money = 0` is `TYPE_MISMATCH` with the fix "write `Money(0)`".
- An integer literal checked against a refined type `R` whose base is an integer type is checked against the base and then converted B→R. The conversion succeeds statically iff the SFC proves the predicate for the literal's value ([09](09-contracts-and-refinements.md)).
- `Byte` and `Char` have no integer literals. Use `b'a'`, `'a'`, or `Byte.from(..)` ([16](16-core-library.md)).

### 6.3.2 Floating-point literals

A float literal synthesizes a float-literal inference variable that unifies only with `F32` and `F64` and defaults to `F64`. Its value is the decimal value rounded to the nearest representable value of the resolved type, ties to even. If the rounded value is infinite, the literal is `LITERAL_OUT_OF_RANGE` (02 §2.6.2). A nonzero literal that rounds to zero is accepted and has value `+0.0`.

### 6.3.3 Other literals

| Literal | Type | Notes |
|---|---|---|
| `true`, `false` | `Bool` | |
| `'a'` | `Char` | a Unicode scalar value |
| `b'a'` | `Byte` | |
| `"text"` | `Str` | data has region `'static` |
| `b"text"` | `Slice<Byte>` | data has region `'static` |
| `f"..."` | `String` | Stage 3, §6.20.3 |
| `()` | `()` | the unit value |

String and byte-string literals are values of view types. They are `Copy` (D-TYPE-02) and refer to read-only static data. Two evaluations of the same literal MAY yield views of the same data; there is no address-identity operation in safe code.

## 6.4 Paths and names

### 6.4.1 Resolution

A path expression is resolved by [04](04-modules-and-names.md). The case rule (D-LEX-02) applies: a lowercase final segment names a value (local, parameter, function, method via a type), and an uppercase final segment names a type, variant, const or distinct constructor. Local bindings shadow according to D-MOD-03. In expression position, a path resolves to one of:

| Resolves to | Typing | Place? |
|---|---|---|
| local binding `x` | `Γ.vars(x).type` | yes |
| `self` | the receiver type | yes |
| function item `f` or `T.f` | the zero-sized function item type (D-TYPE-09) | no |
| const `C` | the declared type of `C`; the value is inlined at each use | no |
| unit variant `E.V` | `E<args>` | no |
| tuple variant `E.V` (not called) | a function item `fn(fields) -> E<args>` (N-C-11) | no |
| distinct type `D` (not called) | a function item `fn(B) -> D` (N-C-11) | no |
| record type or record variant (not called) | error `RECORD_CONSTRUCTOR_AS_VALUE` | — |
| a type, module or trait | error `EXPECTED_VALUE_FOUND_TYPE` | — |

Generic arguments on a value path (`f<I32>`, `Vec<I32>.new`) are written with `GenericArgsExpr` (D-SYN-04). Their count and kinds are checked by [05](05-types.md).

### 6.4.2 `Self`

Inside an `impl` or `trait`, `Self` in expression position denotes the implementing type. `Self.f(..)` calls an associated function, and `Self(..)` constructs a record or distinct value when `Self` is a record or distinct type. Outside an `impl` or `trait`, `Self` is `SELF_OUTSIDE_IMPL` (05).

### 6.4.3 Implicit members

An `ImplicitMember` `.V` (optionally followed by call arguments) denotes the variant `V` of the expected enum type (D-SYN-02).

```text
(T-Implicit)
expected type T is known     head(T) = E<A>, an enum      V a variant of E
──────────────────────────────────────────────────────────────────────
Γ ⊢ .V ⇐ T     elaborates to   E<A>.V
```

Rules (N-C-3):

1. The expected type is taken as is. It is not auto-dereferenced, and refined and distinct types are not seen through. With an expected type `borrow E`, `Box<E>`, a distinct `D = E`, or a refined `R = E where ..`, `.V` is rejected.
2. If no expected type exists, or it is not known, the error is `IMPLICIT_MEMBER_NEEDS_TYPE`, with the fix "qualify as `E.V`" when exactly one enum in scope has a variant `V`.
3. If the expected type is known but its head is not an enum, the error is `IMPLICIT_MEMBER_NOT_ENUM`.
4. If the enum has no variant `V`, the error is `UNKNOWN_VARIANT` (05).
5. After elaboration, `.V(args)` and `.V(f: e, ..)` are checked exactly like the qualified forms (§6.18).

Expected types for implicit members come from the coercion sites of §6.1.3, from the right operand of `==` and `!=` (§6.8.5), and from patterns, where the expected type is the type being matched (§6.21.3). The left operand of a binary operator has no expected type, so `.Active == s` is `IMPLICIT_MEMBER_NEEDS_TYPE`.

```sai
enum Status {
    Pending
    Active
    Closed(reason: Str)
}

fn close(s: Status) -> Status {
    if s == .Active {
        return .Closed(reason: "done")
    }
    s
}
```

```sai
fn bad() {
    let s = .Pending    // rejected: IMPLICIT_MEMBER_NEEDS_TYPE (no expected type)
}
```

(The `==` in the first example requires `Status: PartialEq`, normally by `@derive(PartialEq)`.)

## 6.5 Calls

### 6.5.1 Function calls

A call `e0(a1, …, an)` is typed as follows.

```text
(T-Call)
Γ ⊢ e0 ⇒ F ! E0     callable(F) = (T1, …, Tn) → R with latent effects L
Γ ⊢ ai ⇐ Ti ! Ei   for i = 1..n
────────────────────────────────────────────────────────────
Γ ⊢ e0(a1, …, an) ⇒ R ! E0 ∪ E1 ∪ … ∪ En ∪ L
```

`callable(F)` is defined by cases:

1. `F` is a function item type or a fn pointer type `fn(T1, …, Tn) -> R`. The latent effects `L` are the declared or inferred effects of the item, or the effects in the pointer type ([08](08-effects-and-capabilities.md), D-EFF-02).
2. `F` is a closure type, or a type parameter or associated type bounded by `Fn`, `FnMut` or `FnOnce`. The parameter and return types come from the bound or the closure signature. The call uses the strongest trait the type implements, in the order `Fn`, `FnMut`, `FnOnce` ([05](05-types.md)). The callee `e0` is then in a place context, like an auto-ref'd receiver:
   - `Fn` borrows it shared;
   - `FnMut` borrows it mutably, so the callee must be a mutable place (`BORROW_MUT_OF_IMMUTABLE` otherwise);
   - `FnOnce` consumes it (a move, [07](07-ownership-and-borrowing.md)).
3. Otherwise the error is `NOT_CALLABLE`.

Argument rules:

- The number of arguments must equal `n`, otherwise `WRONG_ARGUMENT_COUNT`. There are no default, variadic or named parameters (D-SYN-06).
- A named argument `x: e` in a function call is `NAMED_ARGUMENT_ON_FUNCTION` (03). An argument `..e` is `FUNCTIONAL_UPDATE_OUTSIDE_RECORD`.
- Arguments are never borrowed implicitly (D-OWN-02). Suppose `Ti` is `borrow U` or `borrow mut U`, checking `ai ⇐ Ti` fails, and `ai` synthesizes a type that unifies with `U`. Then the error is `MISSING_BORROW` ([07](07-ownership-and-borrowing.md) §7.5.2), with the machine-applicable fix "insert `borrow`" or "insert `borrow mut`" before `ai`.
- Calling an `unsafe fn` requires `Γ.unsafe` ([12](12-unsafe-and-ffi.md)).

Evaluation:

1. Evaluate `e0`. A path to a function item evaluates nothing.
2. Evaluate `a1`, …, `an` left to right, each to completion (D-EXPR-02).
3. Check the callee's `requires` clauses if the static proof failed ([09](09-contracts-and-refinements.md)).
4. Transfer control. The value of the call is the callee's return value.

If any step diverges, the later steps are not performed and values already produced are dropped as defined in [07](07-ownership-and-borrowing.md) (§6.26).

### 6.5.2 Method calls

A method call `e.m<G>(a1, …, an)` is resolved by [05](05-types.md): inherent methods first, then methods of traits in scope, with auto-deref of the receiver through `borrow T`, `borrow mut T` and `Box<T>`, followed by at most one auto-ref selected by the method's receiver type (D-TYPE-08, D-OWN-02). Resolution yields a path `P.m` and a receiver adjustment `adj` that consists of zero or more dereferences followed by at most one `borrow` or `borrow mut`. The call is then exactly

```text
P.m<G>(adj(e), a1, …, an)
```

typed by T-Call, with these differences:

- The receiver `e` is in a place context when `adj` contains a `borrow` or a dereference. If `e` is a value expression, it is materialized into a temporary (§6.24).
- An auto-ref'd `borrow mut` receiver is a two-phase borrow, so `v.push(v.len())` is accepted (D-OWN-04, [07](07-ownership-and-borrowing.md)).
- Evaluation order: the receiver first, then `adj`, then the arguments left to right.

The fully qualified forms `Trait.m(x, …)` and `Type.m(x, …)` are ordinary function calls (§6.5.1). They perform no auto-ref and no auto-deref, so `Trait.m(borrow x)` must be written explicitly.

`r.f(x)` is always a method call. To call a closure stored in a field, write `(r.f)(x)` (N-C-10). If no method `f` is found but a field `f` of callable type exists, the error `METHOD_NOT_FOUND` (05) carries the fix "write `(r.f)(x)`".

```sai
type Handler {
    on_event: fn(I32) -> I32
}

fn fire(h: borrow Handler, v: I32) -> I32 {
    (h.on_event)(v)
}
```

## 6.6 Field access

### 6.6.1 Named fields

The rule for `e.f` uses the auto-deref chain of D-OWN-02:

```text
deref_chain(T0) = T0, T1, …, Tk   where Ti+1 = U if Ti ∈ { borrow U, borrow mut U, Box<U> }
                                   and the chain stops at the first Ti not of that form
```

The checker synthesizes `Γ ⊢ e ⇒ T0` and walks the chain. At the first `Ti` that is one of the following, the access resolves:

- **A record type** (a `TypeDecl` with `StructDef`) that has a field `f` visible at the use site ([04](04-modules-and-names.md)). The result is the place `(*ⁱ e).f` with the declared field type, after substituting the type's generic arguments.
- **A distinct type**, when `f` is `value`. The result is the place `(*ⁱ e).value` of the base type (D-TYPE-03). Visibility of `.value` follows the `pub` marker on the base ([04](04-modules-and-names.md) §4.6.4, D-TYPE-03).
- **A refined type `R`** whose base is a record type with field `f`. The access reads through to the base. The resulting place is always `imm`, so the predicate cannot be invalidated through a field write ([09](09-contracts-and-refinements.md) §9.2.5).

The chain is finite because types are finite. If no `Ti` resolves:

- the error is `UNKNOWN_FIELD`;
- if the field exists but is not visible, it is `PRIVATE_FIELD`;
- enum values have no fields, even when every variant has a field of that name. `UNKNOWN_FIELD` carries the note "match on the variant".

The mutability of the resulting place follows §6.2.3. When the base is refined, or is a distinct type with a refinement clause, the place is `imm` (N-C-34).

### 6.6.2 Tuple fields

`e.N`, where `N` is a decimal integer token, uses the same chain. It resolves at the first `Ti` that is a tuple type of arity `> N`, and yields field `N` (zero-based). `N` MUST be written as plain decimal digits without `_` or leading zeros, otherwise `INVALID_TUPLE_INDEX`. Otherwise:

- if the first non-reference `Ti` is a tuple of arity `≤ N`, the error is `TUPLE_INDEX_OUT_OF_RANGE`;
- if it is not a tuple, the error is `UNKNOWN_FIELD`.

`t.0.1` lexes as two accesses (02 §2.6.2).

### 6.6.3 Evaluation

The base is evaluated as a place (materialized if it is a value expression). Each inserted dereference is then applied, then the field offset. Field access performs no check and cannot panic. A dereference of `Box` cannot fail.

## 6.7 Indexing

### 6.7.1 Built-in indexing

The type of `e[i]` is decided as follows (N-C-12):

1. Synthesize `Γ ⊢ e ⇒ C`. Indexing does **not** auto-deref (D-OWN-02). With `e : borrow Array<T, N>`, write `(*e)[i]`.
2. Synthesize `Γ ⊢ i ⇒ I`.
3. If `C` is `Array<T, N>`, `Slice<T>` or `SliceMut<T>`, and `I` is `USize` or an unresolved integer-literal variable (which is then unified with `USize`), the indexing is **built-in**. The result is a place of type `T`, with mutability as in §6.2.3.
4. Otherwise the indexing is trait-based (§6.7.2). As a special case, if `I` is an unresolved integer-literal variable and `C` has an `Index<USize>` impl, the variable is unified with `USize` before impl selection.

```text
(T-Index-Builtin)
Γ ⊢ e ⇒ C    C ∈ { Array<T, N>, Slice<T>, SliceMut<T> }    Γ ⊢ i ⇐ USize
───────────────────────────────────────────────────────────────────────
Γ ⊢ₚ e[i] : T @ μ            (μ per §6.2.3)
```

Evaluation:

1. Evaluate `e` as a place.
2. Evaluate `i`.
3. Compare `i < len`. `len` is `N` for arrays and the view length for slices. If the comparison is false, panic with kind `BoundsCheck` (D-EXPR-10).
4. Access element `i`.

The bounds check happens in every profile. The optimizer MAY remove it only when it proves it redundant (D-CONTRACT-01 and D-TGT-02 apply the same rule to all checks).

### 6.7.2 Trait-based indexing

For any other combination, `e[i]` desugars as follows. The trait signatures are in [16](16-core-library.md):

| Context | Desugaring | Result |
|---|---|---|
| value context or `imm` place context | `*Index.index(borrow e, i)` | place of type `Index.Output` |
| `mut` place context (assignment target, `borrow mut` operand, `borrow mut` receiver) | `*IndexMut.index_mut(borrow mut e, i)` | `mut` place of type `IndexMut.Output` |

- The implicit `borrow` of `e` here is part of the desugaring and is not an argument borrow, so D-OWN-02 is not violated.
- Reading the resulting place in a value context requires `Output: Copy`. Otherwise the read is a move out of a borrow and is rejected by [07](07-ownership-and-borrowing.md) with the fix "use `borrow e[i]`" or "`.clone()`".
- If no impl applies, the error is `NOT_INDEXABLE`.
- Which impls exist for `Vec`, `String` and `Str` (including `Str` range indexing, which panics unless both ends are on char boundaries) is defined in [16](16-core-library.md) (D-EXPR-10).

Evaluation order: `e`, then `i`, then the call.

## 6.8 Operators

### 6.8.1 Precedence and associativity

This table restates D-SYN-05 and 03 §3.13. Level 1 binds tightest.

| Level | Operators | Associativity |
|---|---|---|
| 1 | postfix `.f` `.N` `.m()` `()` `[]` `?` `.await` | left |
| 2 | prefix `-` `~` `*` `borrow` `borrow mut` | right (prefix) |
| 3 | `*` `/` `%` `*%` | left |
| 4 | `+` `-` `+%` `-%` | left |
| 5 | `<<` `>>` | left |
| 6 | `&` | left |
| 7 | `^` | left |
| 8 | `\|` | left |
| 9 | `..` `..=` | none |
| 10 | `==` `!=` `<` `<=` `>` `>=` | none |
| 11 | `not` | right (prefix) |
| 12 | `and` | left |
| 13 | `or` | left |

`a < b < c` and `a == b == c` are `CHAINED_COMPARISON` (03). `a .. b .. c` is a syntax error (03). Assignment is a statement, not an operator (§6.13).

### 6.8.2 Operand typing

For a binary operator `a ⊕ b` other than `and`, `or` and the comparisons, the operands are typed as follows (N-C-2):

1. Synthesize `Γ ⊢ a ⇒ A`.
2. If `A` (after shallow resolution) is a primitive numeric type, `Byte`, `Bool`, or an integer- or float-literal variable, check `b` against `A`, or against `U32` for shifts (§6.8.5). The operation is then **built-in** if the table in §6.8.3–§6.8.5 defines it for `A`. Otherwise it is `OPERATOR_NOT_SUPPORTED`.
3. Otherwise synthesize `Γ ⊢ b ⇒ B` and select an impl of the operator trait `Tr<B> for A` (§6.8.10) ([05](05-types.md)). The result type is the impl's `Output`. If no impl exists, the error is `OPERATOR_NOT_SUPPORTED`, naming the trait and both types.

**Refined operands.** An operand whose type is a refined type `R` with base `B₀` is seen as a `B₀`, whether or not the operand position is a coercion site (refinement erasure, [09](09-contracts-and-refinements.md) §9.2.2 item 3, N-C-39). In step 2, `A` is then `B₀`, and an operand `b : R` is accepted where `B₀` is expected. The result has the type of the base operation, never `R`: `p + 1` with `p : Percentage` has type `I32`. The same holds for the operands of the comparison operators (§6.8.7).

Operands of the arithmetic, bitwise and shift operators are taken **by value**. A non-`Copy` place operand is moved (N-C-6). This makes `s + "x"` with `s : String` consume `s` and reuse its buffer (D-EXPR-13).

Both operands are evaluated left to right before the operation (D-EXPR-02).

### 6.8.3 Arithmetic

| Operator | Built-in on | Semantics |
|---|---|---|
| `a + b`, `a - b`, `a * b` | all integer types | exact result; if it does not fit the type, panic with kind `Overflow(op)` |
| `a / b` | all integer types | quotient truncated toward zero; `b == 0` panics with kind `DivByZero`; for signed types `MIN / -1` panics with kind `Overflow(Div)` |
| `a % b` | all integer types | remainder with the sign of the dividend (`rem_s`/`rem_u`); `b == 0` panics with kind `RemByZero`; for signed types `MIN % -1` is `0` and does not panic (N-C-8) |
| `+ - * /` | `F32`, `F64` | IEEE 754 operations with roundTiesToEven, as in Wasm; no panics; division by zero yields ±∞ or NaN; NaN bit patterns are unspecified (D-TYPE-06) |
| `%` | floats | not defined (`OPERATOR_NOT_SUPPORTED`, fix: use the library remainder function, [16](16-core-library.md)) |

`Byte`, `Char` and `Bool` have no arithmetic (D-TYPE-01). Operands must have the same type. There is no implicit widening, so `x + y` with `x : I32` and `y : I64` is `TYPE_MISMATCH` with the fix "convert with `I64.from(x)`" (D-TYPE-06).

Overflow checks are performed in every profile (D-TYPE-06, D-TGT-02).

### 6.8.4 Wrapping arithmetic

`+%`, `-%` and `*%` are built-in only on the primitive integer types. The result is the exact result reduced modulo 2^w into the type's range (two's complement for signed types). They never panic. On any other type, including distinct types, `Byte` and floats, they are `OPERATOR_NOT_SUPPORTED` (N-C-31). There are no traits for them. Distinct types use `d.value +% x` and re-wrap.

### 6.8.5 Bitwise operators and shifts

| Operator | Built-in on | Semantics |
|---|---|---|
| `a & b`, `a \| b`, `a ^ b` | integer types, `Byte` | bitwise AND, OR, XOR on the two's-complement representation |
| `~a` | integer types, `Byte` | bitwise NOT |
| `a << s` | integer types, `Byte`; `s : U32` | shift left; bits shifted out are discarded and are not an overflow |
| `a >> s` | integer types, `Byte`; `s : U32` | arithmetic shift for signed types, logical shift for unsigned types and `Byte` |

- The right operand of a shift always has type `U32` (D-TYPE-06). A literal is checked against `U32`, and any other type is `TYPE_MISMATCH`.
- If `s ≥ w`, where `w` is the bit width of the left operand's type (8 for `Byte`), the shift panics with kind `ShiftOverflow`.
- `Bool` has no bitwise operators. Use `and`, `or`, `not`, and `!=` for exclusive or (N-C-7).
- `Byte` supports the bitwise operators and shifts (N-C-9).

### 6.8.6 Unary operators

| Operator | Built-in on | Semantics |
|---|---|---|
| `-a` | signed integer types | negation; `-MIN` panics with kind `Overflow(Neg)` |
| `-a` | `F32`, `F64` | flips the sign bit (`f32.neg` / `f64.neg`), including for NaN |
| `-a` | unsigned integer types, `Byte`, `Char`, `Bool` | not defined: `OPERATOR_NOT_SUPPORTED` (D-TYPE-06) |
| `~a` | §6.8.5 | |
| `not a` | `Bool` | §6.8.8 |

A `-` that directly precedes an integer literal forms a negative constant (§6.3.1), not a negation, so `-128` is valid for `I8`. `-x` on any other type uses `Neg` (§6.8.10), and `~x` uses `BitNot`.

### 6.8.7 Comparison operators

```text
(T-Cmp)
Γ ⊢ a ⇒ A      Γ ⊢ b ⇐ A      A : PartialEq  (for == and !=)
                               A : PartialOrd (for < <= > >=)
──────────────────────────────────────────────────────────
Γ ⊢ a ⊙ b ⇒ Bool
```

- Both operands have the **same type** `A`. The right operand is checked against `A`, so `s == .Active` and `x < 10` work (N-C-4).
- An operand of a refined type `R` with base `B₀` is seen as a `B₀` ([09](09-contracts-and-refinements.md) §9.2.2 item 3, §6.8.2): `A` is then `B₀`, so `p < 101` with `p : Percentage` compares two `I32` values and needs no proof.
- The operands are in place contexts and are passed as implicit shared borrows. Comparison never moves or copies its operands (N-C-5). `a == b` means `PartialEq.eq(borrow a, borrow b)`, and `a < b` means `PartialOrd.lt(borrow a, borrow b)` (§6.8.10). These implicit borrows are part of the operator, not argument borrows (D-OWN-02).
- Comparison is built-in for the primitive types:
  - integers: by mathematical value
  - floats: IEEE 754, so every comparison with NaN is false except `!=`, which is true
  - `Bool`: `false < true`
  - `Char`: by scalar value
  - `Byte`: by unsigned value
  - `Str`: `==` is byte equality, and `<` is lexicographic byte order, which equals code point order for UTF-8
  - `()`: equal
- Comparison on tuples and arrays, and on `Slice<T>`, follows the impls in [16](16-core-library.md). Comparison on user types requires an impl, normally derived (D-TYPE-08).
- Comparing values of different types is `TYPE_MISMATCH`. Comparing `String` with `Str` requires `s.as_str() == "x"`. There is no cross-type `PartialEq`.
- Chained comparisons are rejected by the grammar (`CHAINED_COMPARISON`, 03). The fix rewrites `a < b < c` as `a < b and b < c`.

### 6.8.8 Logical operators

```text
(T-And)                                         (T-Not)
Γ ⊢ a ⇐ Bool ! E1    Γ ⊢ b ⇐ Bool ! E2          Γ ⊢ a ⇐ Bool ! E
────────────────────────────────────            ─────────────────────
Γ ⊢ a and b ⇒ Bool ! E1 ∪ E2                    Γ ⊢ not a ⇒ Bool ! E
```

`or` is typed like `and`. There is no truthiness: a non-`Bool` operand is `TYPE_MISMATCH`.

Evaluation:

- `a and b` evaluates `a`. If it is `false`, the result is `false` and `b` is not evaluated. Otherwise the result is the value of `b`.
- `a or b` evaluates `a`. If it is `true`, the result is `true` and `b` is not evaluated. Otherwise the result is the value of `b`.
- `not a` is logical negation.

The tokens `&&`, `||` and `!` are lexical errors (`USE_AND_OR`, `NO_BANG_OPERATOR`; 02). Because `not` binds more loosely than comparison (level 11), `not a == b` means `not (a == b)`.

### 6.8.9 String concatenation

String concatenation is `Add`, with exactly the two impls of D-EXPR-13, both in `alloc` and both with effect `alloc`:

| Left | Right | Result | Behavior |
|---|---|---|---|
| `Str` | `Str` | `String` | allocates a new buffer |
| `String` | `Str` | `String` | consumes the left operand and appends into its buffer |

`Str + String` and `String + String` have no impl. They are `OPERATOR_NOT_SUPPORTED`, with the fix "append `.as_str()` to the right operand", or for `Str + String`, "convert the left operand with `String.from(..)`".

```sai
fn greeting(name: Str) -> String {
    let s = "Hello, " + name
    s + "!"
}
```

### 6.8.10 Operator desugaring

Operators that are not built-in for the operand types desugar to trait method calls. The trait names are lang items ([16](16-core-library.md)).

| Expression | Desugaring | Operand passing |
|---|---|---|
| `a + b` | `Add.add(a, b)` | by value |
| `a - b` | `Sub.sub(a, b)` | by value |
| `a * b` | `Mul.mul(a, b)` | by value |
| `a / b` | `Div.div(a, b)` | by value |
| `a % b` | `Rem.rem(a, b)` | by value |
| `-a` | `Neg.neg(a)` | by value |
| `a & b` | `BitAnd.bitand(a, b)` | by value |
| `a \| b` | `BitOr.bitor(a, b)` | by value |
| `a ^ b` | `BitXor.bitxor(a, b)` | by value |
| `~a` | `BitNot.bitnot(a)` | by value |
| `a == b` | `PartialEq.eq(borrow a, borrow b)` | implicit shared borrow |
| `a != b` | `not PartialEq.eq(borrow a, borrow b)` | implicit shared borrow |
| `a < b` | `PartialOrd.lt(borrow a, borrow b)` | implicit shared borrow |
| `a <= b` | `PartialOrd.le(borrow a, borrow b)` | implicit shared borrow |
| `a > b` | `PartialOrd.gt(borrow a, borrow b)` | implicit shared borrow |
| `a >= b` | `PartialOrd.ge(borrow a, borrow b)` | implicit shared borrow |
| `a[i]` | §6.7.2 | |

- Shifts, wrapping operators, `and`, `or` and `not` have no traits.
- For distinct types, the operator traits are available only via `@derive`, which forwards to the base (D-TYPE-03, D-TYPE-08).
- The effect of an operator expression includes the latent effects of the selected impl method ([08](08-effects-and-capabilities.md)).
- The built-in operators have no effects. Their panics are not effects (D-EFF-03).

## 6.9 Dereference, borrow and auto-deref

### 6.9.1 Dereference

```text
(T-Deref)
Γ ⊢ e ⇒ D     D ∈ { borrow T, borrow mut T, Box<T> }
──────────────────────────────────────────────────
Γ ⊢ₚ *e : T @ μ          (μ per §6.2.3)
```

- `*e` on `Ptr<T>` or `PtrMut<T>` is allowed only in `unsafe` ([12](12-unsafe-and-ffi.md)).
- `*e` on any other type is `CANNOT_DEREFERENCE`.
- There is no user-defined `Deref` (D-OWN-02).
- Reading `*e` in a value context copies if `T: Copy`. Otherwise it is a move out of the referent, which [07](07-ownership-and-borrowing.md) forbids: through a borrow it is `MOVE_OUT_OF_BORROW` (D-OWN-01), and through a `Box` it is `MOVE_OUT_OF_BOX`; the value is taken out with `Box.into_inner(b)` (07 N-B-2).
- `*e` for borrows performs no runtime check. References are never null (D-WASM-02 guards address 0–7 only against bugs in unsafe code).

### 6.9.2 Borrow expressions

```text
(T-Borrow)                                     (T-BorrowMut)
Γ ⊢ₚ p : T @ μ                                 Γ ⊢ₚ p : T @ mut
──────────────────────────                     ──────────────────────────────
Γ ⊢ borrow p ⇒ borrow T                        Γ ⊢ borrow mut p ⇒ borrow mut T
```

- If the operand is a value expression, it is materialized into a temporary, and the borrow refers to the temporary (§6.24). Temporary lifetime extension applies only to a `let` or `var` whose initializer is `borrow e` (D-OWN-05, §6.24.3).
- `borrow mut` of an `imm` place is `BORROW_MUT_OF_IMMUTABLE` (§6.2.3).
- The region of the resulting type, the rules on conflicting borrows, reborrowing and two-phase borrows are owned by [07](07-ownership-and-borrowing.md).
- `borrow` and `borrow mut` are prefix operators at level 2. `borrow v[i]` borrows the element, and `borrow x.f` borrows the field.

### 6.9.3 Auto-deref limits

Auto-deref (the chain of §6.6.1) is applied **only** to:

1. the base of a field access `e.f` or tuple field access `e.N`
2. the receiver of a method call `e.m(..)`

It is not applied to operands of operators, the base of indexing, match scrutinees (D-EXPR-04), call arguments, `for` iterables, conditions, or the callee of a call. Everywhere else, `*` is explicit (D-OWN-02).

```sai
fn total(p: borrow Array<I32, 3>) -> I32 {
    let a = (*p)[0] // indexing needs an explicit `*`
    a + (*p)[1] + (*p)[2]
}
```

## 6.10 Casts

SAI has no cast expression (D-SYN-05). The keyword `as` appears only in `use` aliases ([04](04-modules-and-names.md)). When `as` follows a complete expression, the expression is rejected with `NO_AS_CAST` (N-C-29). The parser recovers by parsing the type that follows as the intended target, and the error is reported once the operand's type is known ([impl/03](../impl/03-name-resolution-and-type-checking.md) §3.10.3), so that the diagnostic offers the conversion that fits the source and target types:

| Intended conversion | Write |
|---|---|
| lossless widening | `T.from(x)` |
| checked narrowing | `T.try_from(x)` (returns `Option<T>`) |
| modular narrowing | `x.wrapping_to<T>()` |
| clamping | `x.saturating_to<T>()` |
| float to integer, truncating | `x.trunc_to<T>()` |

These functions are defined in [16](16-core-library.md) (D-TYPE-06).

```sai
fn index_of(i: I32) -> USize {
    let n = i as USize      // rejected: NO_AS_CAST (fix: `USize.try_from(i)`)
    n
}
```

## 6.11 Blocks

### 6.11.1 Typing

A block `{ s1 NL … NL sn }` opens a new scope. Its statements are checked in order, and each statement's output environment is the input of the next. The **tail** of the block is `sn` if `n ≥ 1` and `sn` is an `ExprStmt`. Otherwise the block has no tail.

```text
(T-Block-Tail)
Γ0 ⊢ s1 ⊣ Γ1   …   Γn-2 ⊢ sn-1 ⊣ Γn-1      Γn-1 ⊢ e ⇐ T      sn = e
───────────────────────────────────────────────────────────────
Γ0 ⊢ { s1 … sn } ⇐ T

(T-Block-NoTail)
Γ0 ⊢ s1 ⊣ Γ1   …   Γn-1 ⊢ sn ⊣ Γn        (no tail)
──────────────────────────────────────────────
Γ0 ⊢ { s1 … sn } ⇒ ()     or ⇒ Never if some si diverges (N-C-14)
```

- With an expected type `T`, the tail is checked against `T`. The tail of a function body is a coercion site.
- With an expected type `()` and a tail `e` that synthesizes a type other than `()` and `Never`, the value of `e` is discarded (D-EXPR-01) and the discard rules of §6.25.2 apply.
- A block with no tail has type `()`. If, however, one of its statements is an `ExprStmt` of type `Never`, or a `let` or `var` whose initializer has type `Never`, the block has type `Never` (N-C-14). `{}` has type `()`.
- The bindings introduced in the block are not visible after it (D-MOD-03).

### 6.11.2 Evaluation

Statements execute in order. The block's value is the tail's value, or `()`. When control leaves the block (normally, or by `break`, `continue`, `return` or `?`), the block's live locals are dropped in reverse declaration order ([07](07-ownership-and-borrowing.md), D-OWN-05).

### 6.11.3 Special blocks

| Form | Rule |
|---|---|
| `unsafe { … }` | typed as a block with `Γ.unsafe = true`; [12](12-unsafe-and-ffi.md) |
| `async { … }`, `async move { … }` (Stage 5) | creates a future; new function boundary; [11](11-async-and-concurrency.md) |
| `region r { … }` | binds `r : borrow Arena` in the block; the block's value MUST NOT borrow from `r`; [07](07-ownership-and-borrowing.md) (D-OWN-07) |

The condition of `if` and `while`, the scrutinee of `match`, and the iterable of `for` MUST NOT start with `{` (`BLOCK_IN_CONDITION`, 03, D-EXPR-03).

## 6.12 `let` and `var`

### 6.12.1 `let`

```text
(T-Let-Ann)
Γ ⊢ e ⇐ T ! E      Γ ⊢ P ⇐ T ⊣ Δ      P irrefutable for T
──────────────────────────────────────────────────────
Γ ⊢ let P: T = e ⊣ Γ, Δ ! E

(T-Let)
Γ ⊢ e ⇒ T ! E      Γ ⊢ P ⇐ T ⊣ Δ      P irrefutable for T
──────────────────────────────────────────────────────
Γ ⊢ let P = e ⊣ Γ, Δ ! E
```

- An initializer is mandatory (D-SYN-10).
- The annotated initializer is a coercion site.
- `_` in the annotation is an inference hole ([05](05-types.md)).
- The bindings `Δ` become visible **after** the statement, so `let x = x.trim()` refers to the outer `x` on the right (D-MOD-03).
- Rebinding a name declared in the same block is `DUPLICATE_BINDING` (D-MOD-03). Shadowing a name of an enclosing block is allowed. Shadowing an imported module alias is `SHADOWS_MODULE` (D-MOD-01).
- A refutable `P` without `else` is `REFUTABLE_PATTERN_IN_LET`. The diagnostic lists the witnesses of §6.22.5 and offers two fixes: "add `else { … }`" and "use `match`".
- Bindings introduced by `let` are immutable unless the pattern uses `var` (D-SYN-10): `let (var a, b) = pair`.

Evaluation:

1. Evaluate `e`.
2. Match `P` against the value. The match cannot fail for an irrefutable pattern.
3. Bind.

If `P` is `_`, or contains only `_` and bindings for some parts, the parts matched by `_` are **not** moved (N-C-27). For a place initializer, `let _ = x` therefore leaves `x` intact, and the value is neither moved nor dropped. For a value initializer, the parts matched by `_` are dropped at the end of the statement as temporaries (§6.24). `let _ = e` is the explicit way to discard a `@must_use` value (D-EXPR-01).

### 6.12.2 `let`-`else`

```text
(T-LetElse)
Γ ⊢ e : T ! E1      Γ ⊢ P ⇐ T ⊣ Δ      P refutable for T      Γ ⊢ b ⇐ Never ! E2
───────────────────────────────────────────────────────────────────────────────
Γ ⊢ let P (: T)? = e else b ⊣ Γ, Δ ! E1 ∪ E2
```

- `P` MUST be refutable. An irrefutable `P` with `else` is `LET_ELSE_IRREFUTABLE` (the rule is stated in 03 §3.12; the code is defined here), with a fix that removes the `else` clause.
- The `else` block MUST have type `Never`. That holds when its tail, or one of its statements, diverges (§6.11.1, §6.26). Otherwise the error is `LET_ELSE_NOT_DIVERGING`.
- The bindings of `P` are not visible in the `else` block.
- If the match fails, the `else` block runs. Temporaries of `e` are dropped before the `else` block starts.

```sai
fn or_zero(x: Option<I32>) -> I32 {
    let Some(v) = x else {
        return 0
    }
    v
}
```

### 6.12.3 `var`

```text
(T-Var)
Γ ⊢ e (⇐ T | ⇒ T) ! E      x ∉ dom(current block)
──────────────────────────────────────────────
Γ ⊢ var x (: T)? = e ⊣ Γ, x : (T, var) ! E
```

`var` declares exactly one mutable variable (D-SYN-10). A pattern after `var` is a syntax error (03). The initializer is mandatory. Shadowing rules are the same as for `let`.

## 6.13 Assignment

### 6.13.1 Simple assignment

```text
(T-Assign)
Γ ⊢ₚ p : T @ mut ! E1      Γ ⊢ e ⇐ T ! E2
─────────────────────────────────────
Γ ⊢ p = e ⊣ Γ ! E1 ∪ E2
```

- The left-hand side MUST be a place expression (§6.2.1): a local, a field, a tuple field, a built-in or trait-based index, or a dereference. Otherwise the error is `INVALID_ASSIGN_TARGET` (03). For a trait-based index, the `IndexMut` desugaring is used.
- The place MUST be mutable (`ASSIGN_TO_IMMUTABLE`, §6.2.3).
- Assigning to a field of a type with an invariant outside its defining module is rejected by [09](09-contracts-and-refinements.md) (D-CONTRACT-04).
- The right-hand side is a coercion site.

Evaluation (D-EXPR-02):

1. Evaluate `e` to a value `v`.
2. Evaluate the place `p`: its base, index operands and inserted dereferences, left to right. Built-in bounds checks happen here.
3. If the place currently holds an initialized value, drop it ([07](07-ownership-and-borrowing.md)).
4. Write `v`.

Assigning to a moved-out local `var`, or to a moved-out field of one, re-initializes it ([07](07-ownership-and-borrowing.md)). Assigning to a `let` binding is always an error, even if it was moved out.

### 6.13.2 Compound assignment

For `op ∈ { + - * / % +% -% *% & | ^ << >> }`, the statement `p op= e` is defined by D-EXPR-02 as

```text
{ let t = e
  p' = p' op t }
```

Here `p'` is the place `p` with its subexpressions (the base of each index or field, the index operands, dereferenced pointers) evaluated **once**, after `t`, and reused for both the read and the write.

- The typing is that of `p' op t` (§6.8) followed by `p' = …`. The result type of `op` MUST equal the place type, otherwise `TYPE_MISMATCH`. `s += "x"` is valid for `s : String` (D-EXPR-13). There are no compound-assignment traits.
- Reading `p'` for a non-`Copy` type is a **take-and-restore** (N-C-13). The old value is moved out of `p'`, passed to the operator, and the result is written back to `p'` before any other code can observe `p'`. This is permitted for every `mut` place, including places behind `borrow mut`, because no unwinding exists (D-ERR-01). A panic inside the operator terminates the program before the place can be observed. In this sense the take is not a "move out of a borrow" in the sense of D-OWN-01.
- For built-in operators, the panics of §6.8 apply. The check happens before the write, so on overflow the place is unchanged when the program traps.

```sai
fn bump(counts: SliceMut<U32>, i: USize) {
    counts[i] += 1
}
```

### 6.13.3 Other restrictions

- Assignment is a statement and has no value. `a = b = c` and `if (x = 1) …` are syntax errors (03).
- There are no `++` and `--` operators.
- Destructuring assignment (`(a, b) = (b, a)`) does not exist. Write `let (x, y) = (b, a)` followed by two assignments, or use `core.mem.swap` ([16](16-core-library.md)).

## 6.14 Conditionals

### 6.14.1 `if`

```text
(T-If-Else)
Γ ⊢ c ⇐ Bool ! E0      Γ ⊢ b1 : T ! E1      Γ ⊢ b2 : T ! E2
──────────────────────────────────────────────────────
Γ ⊢ if c b1 else b2 : T ! E0 ∪ E1 ∪ E2

(T-If)
Γ ⊢ c ⇐ Bool ! E0      Γ ⊢ b1 ⇐ () ! E1
─────────────────────────────────────
Γ ⊢ if c b1 ⇒ () ! E0 ∪ E1
```

The type `T` of an `if` with `else` is determined as follows:

1. If the `if` has a known expected type `X`, both branches are checked against `X` (a coercion site), and `T = X`.
2. Otherwise `b1` is synthesized as `T1`.
   - If `T1` is not `Never`, `b2` is checked against `T1` and `T = T1`.
   - If `T1` is `Never`, `b2` is synthesized and `T` is its type.
   - In both cases a branch of type `Never` is accepted by coercion.

An `if` without `else` has type `()`. Its block is checked against `()` (so a tail value is discarded per §6.25.2). If the `if` appears where a non-`()` value is required, the error is `IF_WITHOUT_ELSE`, with a note that suggests adding an `else` branch (N-C-32).

`else if` is `else` followed by an `IfExpr`, and nests to the right.

Evaluation: evaluate `c`, drop the condition's temporaries (§6.24.2), then evaluate exactly one branch.

### 6.14.2 `if let`

```text
(T-IfLet)
Γ ⊢ e ⇒ S ! E0      Γ ⊢ P ⇐ S ⊣ Δ      Γ, Δ ⊢ b1 : T ! E1      Γ ⊢ b2 : T ! E2
────────────────────────────────────────────────────────────────────────
Γ ⊢ if let P = e b1 else b2 : T ! E0 ∪ E1 ∪ E2
```

- The type rules for `T`, and for the form without `else`, are those of §6.14.1. The bindings `Δ` are visible only in `b1`.
- The scrutinee `e` is in a place context. Its temporaries live until the end of the whole `if let` expression, including the `else` branch (§6.24.2).
- If `P` is irrefutable for `S`, the warning is `IRREFUTABLE_CONDITION_PATTERN`, with the fix "use `let`".
- `if let` is equivalent to the match below, except that no exhaustiveness check applies to it and an omitted `else` is `{}`:

  ```text
  match e { P => b1, _ => b2 }
  ```

There are no `if let … and …` chains. Nest an `if` inside the branch instead.

## 6.15 `match`

### 6.15.1 Typing

```text
(T-Match)
Γ ⊢ e ⇒ S ! E0
for each arm i:  Γ ⊢ Pi ⇐ S ⊣ Δi     Γ, Δi ⊢ gi ⇐ Bool ! Gi (if present)     Γ, Δi ⊢ ai : T ! Ai
the arms are exhaustive for S (§6.22)
──────────────────────────────────────────────────────────────────────────
Γ ⊢ match e { P1 (if g1)? => a1 … } : T ! E0 ∪ ⋃Gi ∪ ⋃Ai
```

- The scrutinee is synthesized without an expected type.
- If `S` is `borrow U` or `borrow mut U`, it is **not** auto-dereferenced (D-EXPR-04). If any arm then has a pattern other than `_`, a binding or `x @ _`, the error is `MATCH_ON_BORROW`, with the fix "write `match *e`".
- The arm type `T` is determined as for `if` (§6.14.1), generalized to many branches:
  1. With a known expected type `X`, every arm body is checked against `X`.
  2. Without one, the first arm body whose type is not `Never` fixes `T`, and every other arm body is checked against `T`.
  3. If every arm has type `Never`, `T = Never`.
- `match e {}` with no arms is valid iff `S` has an empty constructor set (§6.22.2), and then has type `Never`. Otherwise it is `NON_EXHAUSTIVE_MATCH`.
- A non-exhaustive match is the error `NON_EXHAUSTIVE_MATCH`. The diagnostic lists up to three witnesses (§6.22.5) and offers the fix "add arms" for each witness. An arm, or an or-alternative, that can never match is the warning `UNREACHABLE_PATTERN` (D-EXPR-04, §6.22.4).

### 6.15.2 Guards

A guard `if g` is evaluated after the pattern has matched and before any binding becomes observable. During the guard (N-C-23):

- each binding `x` of the arm denotes the corresponding subplace of the scrutinee, viewed through a shared borrow;
- `borrow mut x` bindings are usable only as shared within the guard;
- the guard MAY read and copy these places. It MUST NOT move out of them, assign to them, or `borrow mut` them, nor do any of these to the scrutinee. A violation is `GUARD_MOVE_OR_MUTATION`.

If the guard yields `false`, matching continues with the next arm, and no binding has taken effect. A guard may have any effects. An arm with a guard does not count toward exhaustiveness (§6.22.3).

### 6.15.3 Evaluation

1. Evaluate the scrutinee once. A place scrutinee is not moved by itself. A value scrutinee is materialized into a temporary that lives until the end of the `match` (§6.24.2).
2. For each arm in source order:
   1. Test the pattern against the scrutinee (§6.21.7).
   2. If it matches and has a guard, evaluate the guard.
   3. If the pattern matches and the guard (if any) is `true`, perform the bindings (moves, copies or borrows, §6.21.5), evaluate the arm body, and finish. The match's value is the body's value.
3. Exhaustiveness guarantees that some arm is selected.

Pattern tests have no side effects and call no user code (§6.21.7).

### 6.15.4 Moves out of a borrowed scrutinee

When the scrutinee is a place reached through a dereference of `borrow T` or `borrow mut T`, a by-value binding of a non-`Copy` subvalue is `MOVE_OUT_OF_BORROW` (07) (D-EXPR-04). The fix is "bind with `borrow x`".

```sai
fn name_len(u: borrow User) -> USize {
    match *u {
        User(name: borrow n, ..) => n.len()
    }
}
```

(Here `User` is a record type with a field `name: String`, and `String.len` is assumed from [16](16-core-library.md).)

## 6.16 Loops and jumps

### 6.16.1 `loop`

```text
(T-Loop)
β = X if the loop has a known expected type X, else β fresh
Γ[loops += (label?, loop, β)] ⊢ b ⇐ () ! E
────────────────────────────────────────────────────────────
Γ ⊢ label? loop b : τ ! E
    where τ = β     if at least one break targets this loop
          τ = Never otherwise                              (N-C-15)
```

- Each `break v` that targets the loop checks `v ⇐ β`. This is a coercion site once `β` is fixed. A `break` without a value unifies `β` with `()`.
- The body is checked against `()`, and a tail value is discarded per §6.25.2.
- Evaluation: execute the body repeatedly until a `break` targets the loop or control leaves it by `return`, `?`, an outer `break` or `continue`, or a panic.

### 6.16.2 `while` and `while let`

```text
(T-While)
Γ ⊢ c ⇐ Bool ! E0      Γ[loops += (label?, while, ())] ⊢ b ⇐ () ! E1
────────────────────────────────────────────────────────────────
Γ ⊢ label? while c b ⇒ () ! E0 ∪ E1
```

- A `while` always has type `()`. The condition is not evaluated statically, so `while true { … }` has type `()` and code after it is considered reachable (P8).
- `break v` with a value targeting a `while` or `for` loop is `INVALID_BREAK_VALUE` (N-C-15). The fix suggests `loop`.
- Evaluation: evaluate `c`, drop its temporaries, and if the result is `true` execute `b` and repeat. Otherwise the loop ends with `()`.
- `label? while let P = e b` is typed and evaluated exactly as follows, with the label applying to the generated `loop`:

  ```text
  label? loop { match e { P => b, _ => break } }
  ```

  Its type is `()`, and a `break` with a value targeting it is `INVALID_BREAK_VALUE`. If `P` is irrefutable for the type of `e`, the warning is `IRREFUTABLE_CONDITION_PATTERN`. The temporaries of `e` live until the end of each iteration's `match`.

### 6.16.3 `for`

`label? for P in e b` is defined by this desugaring. The names `it` and `next` are hygienic: no user code can refer to them.

```text
{
    var it = IntoIterator.into_iter(e)
    label? loop {
        match Iterator.next(borrow mut it) {
            Some(P) => b
            None => break
        }
    }
}
```

Rules:

- `e` is consumed by value (D-EXPR-11). `for x in borrow xs` uses `IntoIterator for borrow Vec<T>` ([16](16-core-library.md)). The type of `e` MUST implement `IntoIterator`, otherwise `NOT_ITERABLE`.
- `P` MUST be irrefutable for the item type, otherwise `REFUTABLE_PATTERN_IN_FOR`.
- The body is checked against `()`. The `for` expression has type `()`.
- `break v` targeting a `for` loop is `INVALID_BREAK_VALUE`.
- The temporaries of `e` live until the end of the whole `for` expression (N-C-26), not to the end of the hidden `var` statement.
- `continue` targets the generated `loop`, so it calls `next` again.
- The effects are those of `e`, `into_iter`, `next` and `b`.
- `for await P in e b` (Stage 5) is defined in [11](11-async-and-concurrency.md).

The `IntoIterator` and `Iterator` signatures assumed here are `into_iter(self: Self) -> Self.IntoIter` and `next(self: borrow mut Self) -> Option<Self.Item>` ([16](16-core-library.md)).

### 6.16.4 `break` and `continue`

Target resolution, performed on `Γ.loops` (N-C-16):

1. An unlabeled `break` or `continue` targets the top entry of `Γ.loops`. If the stack is empty, the error is `BREAK_OUTSIDE_LOOP` or `CONTINUE_OUTSIDE_LOOP`.
2. A labeled `break 'l` or `continue 'l` targets the topmost entry whose label is `'l`. If there is none, the error is `UNDECLARED_LABEL`.
3. `Γ.loops` is empty at the start of every function body, closure body and `async` block. A `break` or `continue` therefore never crosses a closure or `async` boundary. A diagnostic produced by rule 1 or 2 inside a closure notes the boundary.
4. Declaring a label `'l` on a loop while an enclosing loop in the same body already has label `'l` is `LABEL_SHADOWED`.
5. Only `loop`, `while` and `for` can carry labels (03).

```text
(T-Break)                                  (T-Continue)
target = (l, loop, β)    Γ ⊢ v ⇐ β          target exists
───────────────────────────────────         ───────────────────────
Γ ⊢ break l? v ⇒ Never                      Γ ⊢ continue l? ⇒ Never
```

Evaluation: evaluate `v` if present, then drop the live locals of every scope being exited, innermost first ([07](07-ownership-and-borrowing.md)), then transfer control. `continue` in a `while` re-evaluates the condition. `continue` in a `for` calls `next`.

```sai
fn find_pair(xs: borrow Array<I32, 4>, target: I32) -> Option<(USize, USize)> {
    var i: USize = 0
    'outer: loop {
        if i >= 4 {
            break 'outer None
        }
        var j = i + 1
        while j < 4 {
            if (*xs)[i] + (*xs)[j] == target {
                break 'outer Some((i, j))
            }
            j += 1
        }
        i += 1
    }
}
```

### 6.16.5 `return`

```text
(T-Return)
Γ ⊢ e ⇐ Γ.ret ! E
────────────────────────
Γ ⊢ return e ⇒ Never ! E
```

- `return` without an operand means `return ()`. It requires `Γ.ret = ()`, otherwise `TYPE_MISMATCH` with the note "missing return value".
- The operand is a coercion site.
- `return` in a const initializer is `RETURN_OUTSIDE_FUNCTION`. In a predicate (an `invariant` declaration, a refinement clause or a contract clause), `return`, `break` and `continue` are `INVALID_CONTRACT_EXPRESSION` instead ([09](09-contracts-and-refinements.md) §9.1.2), and neither `RETURN_OUTSIDE_FUNCTION` nor an error of §6.16.4 is reported for them.
- Inside a closure, `return` returns from the closure. Inside an `async` block, it completes the future ([11](11-async-and-concurrency.md)).

Evaluation:

1. Evaluate `e`.
2. Check the `ensures` clauses not proven statically ([09](09-contracts-and-refinements.md)), and the invariant check points of D-CONTRACT-04.
3. Drop all live locals of the function, innermost scope first.
4. Return the value.

## 6.17 Closures

### 6.17.1 Typing

```text
(T-Closure)
Γc = Γ.vars + {x1 : T1, …, xn : Tn},  ret = R,  loops = [],  unsafe = false (true in an unsafe module)
Γc ⊢ body ⇐ R ! L
──────────────────────────────────────────────────────────────────────
Γ ⊢ move? fn(x1 (: T1)?, …, xn (: Tn)?) (-> R)? body ⇒ C
    C: a fresh closure type with signature (T1, …, Tn) -> R,
       latent effects L, and captures per §6.17.3
```

- Creating a closure has no effect. Its latent effects `L` are the effects of the body ([08](08-effects-and-capabilities.md)).
- `unsafe` does not propagate into a closure body. An `unsafe` block inside the closure is required ([12](12-unsafe-and-ffi.md) §12.2.1), except in an `unsafe module`, where every closure body is an unsafe context ([12](12-unsafe-and-ffi.md) §12.2.4).
- Each closure expression has its own unique, unnameable type (D-TYPE-09). It implements the `Fn` traits of §6.17.4.
- A closure type does not coerce to a fn pointer. The coercion list of D-TYPE-04 contains only fn items.
- A closure type is `Copy` or `Clone` as [05](05-types.md) §5.5.3 defines: when every capture is (N-A-31).
- Closures have no name, so they cannot be recursive, and they cannot be generic.
- A `var` parameter is a mutable local of the closure body. Patterns are not allowed as closure parameters (D-SYN-06).

### 6.17.2 Inference of parameter and return types

1. An annotated parameter type is used as is.
2. Otherwise, if the closure has a known expected type that is a fn pointer type, or a type with a known `Fn`, `FnMut` or `FnOnce` bound (for example a generic parameter of the callee after its other arguments have been checked), the parameter types come from that signature.
3. Otherwise each unannotated parameter gets a fresh inference variable, resolved from the body's uses (D-TYPE-05). If it is unresolved at the end of inference of the enclosing body, the error is `TYPE_ANNOTATIONS_NEEDED` (05).
4. The return type is the annotation if present. Otherwise it comes from the expected signature if one is known. Otherwise it is inferred from the body's tail and `return` operands.

### 6.17.3 Capture inference

The **captured variables** of a closure are the locals of enclosing bodies, including parameters and `self`, that occur free in the closure body or in a nested closure in it. The capture unit is the **whole variable**, never a field path (N-C-17).

For a non-`move` closure, each captured variable `x` has the mode determined by its most demanding use in the body, in the order `shared < mut < move`:

| Use of `x` (or of a place rooted at `x`) in the body | Mode |
|---|---|
| read in a value context where the read type is not `Copy` (a move of `x` or a part of it) | `move` |
| left-hand side root of `=` or `op=`; operand root of `borrow mut`; root of a receiver auto-ref'd as `borrow mut`; callee of an `FnMut` call | `mut` |
| any other use: reading `Copy` data, `borrow`, shared auto-ref, comparison operands, f-string interpolation | `shared` |

- A `shared` capture stores a `borrow` of `x`. A `mut` capture stores a `borrow mut` of `x`, and `x` MUST be a mutable place (`BORROW_MUT_OF_IMMUTABLE`). A `move` capture moves `x` into the closure.
- For a `move fn` closure, every captured variable is captured by value. It is copied if `Copy` and moved otherwise. Mutating such a capture in the body mutates the closure's own copy. It still requires `x` to be declared `var`, so a name has one mutability everywhere.
- The regions of captured borrows and the move-checking of captures are owned by [07](07-ownership-and-borrowing.md).

### 6.17.4 Implemented `Fn` traits

| Condition on the body | Implements |
|---|---|
| does not move out of any capture, and does not mutate any capture (no `mut` capture, and no mutation of a by-value capture) | `Fn`, `FnMut`, `FnOnce` |
| does not move out of any capture, but mutates at least one | `FnMut`, `FnOnce` |
| moves out of at least one capture | `FnOnce` |

The call rules of §6.5.1 case 2 apply. Calling an `FnMut`-only closure requires a mutable place, so the closure is usually bound with `var`.

```sai
fn count_over(a: I32, b: I32, limit: I32) -> U32 {
    var n: U32 = 0
    var bump = fn(x: I32) {
        if x > limit {
            n += 1
        }
    }
    bump(a)
    bump(b)
    n
}
```

`n` is captured `mut` and `limit` `shared`. `bump` implements `FnMut` and `FnOnce` but not `Fn`. The borrow of `n` ends after the last call (NLL, D-OWN-04).

## 6.18 Construction expressions

### 6.18.1 Record construction

A call whose callee resolves to a record type `T<A>`, a record variant `E<A>.V`, or `Self` of record type, or an implicit member `.V` of a record variant, is a **record construction** (D-SYN-01):

```text
(T-Record)
fields(T<A>) = { f1 : U1, …, fk : Uk }
each fj initialized exactly once by an explicit argument or by the base
Γ ⊢ ej ⇐ Uj ! Ej  (explicit)        Γ ⊢ b ⇐ T<A> ! Eb  (base, if present)
──────────────────────────────────────────────────────────────────
Γ ⊢ T(f: e, …, ..b) ⇒ T<A> ! ⋃Ej ∪ Eb
```

Arguments:

- `f: e` initializes field `f`.
- A bare identifier `f` is punning, short for `f: f` (D-SYN-01).
- An unnamed argument that is not a bare identifier is `RECORD_FIELD_NEEDS_NAME`. Records have no positional form (D-SYN-01).
- `..b`, the functional update base, MUST be the last argument (03). Its type MUST be exactly `T<A>`. The fields not given explicitly are moved or copied out of `b` ([07](07-ownership-and-borrowing.md)).
- Generic arguments are inferred from the expected type and the field initializers ([05](05-types.md)), or written explicitly: `Pair<I32>(left: 1, right: 2)`.

Errors:

| Condition | Diagnostic |
|---|---|
| a field is named twice | `DUPLICATE_FIELD` |
| a named field does not exist | `UNKNOWN_FIELD` |
| a field is not visible at the construction site | `PRIVATE_FIELD` |
| a field is neither initialized nor covered by `..b` | `MISSING_FIELD`, listing every missing field |
| `..b` is given but every field is explicit | `REDUNDANT_UPDATE_BASE` (warning); `b` is still evaluated and dropped |

Evaluation:

1. Evaluate the explicit initializers in **source order**, not declaration order (D-EXPR-02).
2. Evaluate the base `b` last.
3. Assemble the value.
4. If `T` has an `invariant`, the construction is a check point ([09](09-contracts-and-refinements.md), D-CONTRACT-04).

```sai
type User {
    id: U64
    name: String
    active: Bool
}

fn rename(u: User, name: String) -> User {
    User(name, ..u)
}
```

### 6.18.2 Tuple variants and distinct construction

- A call whose callee resolves to a tuple variant `E<A>.V` with fields `(U1, …, Uk)` is typed as a call of a function `fn(U1, …, Uk) -> E<A>` (§6.5.1). The arguments are coercion sites. Named arguments are `NAMED_ARGUMENT_ON_FUNCTION`.
- A unit variant takes no arguments. `E.V()` is `WRONG_ARGUMENT_COUNT`.
- `D(e)` for a distinct type `D = B` checks `e ⇐ B` (D-TYPE-03). Visibility of the constructor follows the `pub` marker on the base ([05](05-types.md)).
  - If `D` has a refinement clause, `D(e)` is a B→R conversion, accepted only if the SFC proves the predicate. Otherwise the error is `REFINEMENT_NOT_PROVEN` with the fix "use `D.try_from(e)`" ([09](09-contracts-and-refinements.md)).
- Tuple variant paths and distinct type paths used as values without a call denote function items (N-C-11):

```sai
fn wrap_all(xs: Vec<I32>) -> Vec<Option<I32>> {
    xs.into_iter().map(Some).collect()
}
```

(`into_iter`, `map` and `collect` are library methods, [16](16-core-library.md).)

### 6.18.3 Tuples and parentheses

| Form | Type |
|---|---|
| `()` | `()` |
| `(e)` | the type of `e`; a place if `e` is |
| `(e,)` | `(T,)` |
| `(e1, …, en)`, `n ≥ 2` | `(T1, …, Tn)` |

With a known expected tuple type, the elements are checked against the corresponding element types. They are evaluated left to right.

### 6.18.4 Arrays

```text
(T-Array)
expected element type U known:   Γ ⊢ ei ⇐ U    for i = 1..n
otherwise:                       Γ ⊢ e1 ⇒ U, then Γ ⊢ ei ⇐ U for i = 2..n
────────────────────────────────────────────────────────────
Γ ⊢ [e1, …, en] ⇒ Array<U, n>
```

- Elements are coercion sites when the element type is known, and are evaluated left to right.
- `[]` has type `Array<U, 0>` and requires a known expected element type, otherwise `TYPE_ANNOTATIONS_NEEDED` (05).
- If an expected type `Array<U, m>` is known and `m ≠ n`, the error is `ARRAY_LENGTH_MISMATCH`.

### 6.18.5 `Array.filled`

There is no `[x; n]` syntax (D-SYN-10). `Array.filled(x)`, declared in [16](16-core-library.md), produces `Array<T, N>` with every element equal to `x`. `N` comes from explicit generic arguments (`Array<I32, 8>.filled(0)`) or from the expected type. If it cannot be inferred, the error is `TYPE_ANNOTATIONS_NEEDED`. `T` MUST be `Copy` (N-C-18), so no user code runs per element. The value is evaluated once and lowered as MIR `Repeat(op, N)` (D-MIR-01).

```sai
fn zeros() -> Array<U8, 16> {
    Array.filled(0)
}
```

## 6.19 Ranges

| Expression | Type | Fields |
|---|---|---|
| `a..b` | `Range<T>` | `start`, `end` |
| `a..=b` | `RangeInclusive<T>` | `start`, `end` |
| `a..` | `RangeFrom<T>` | `start` |
| `..b` | `RangeTo<T>` | `end` |
| `..=b` | `RangeToInclusive<T>` | `end` |
| `..` | `RangeFull` | none |

- `Range` and `RangeInclusive` are named by D-EXPR-09. The other four names are introduced here (N-C-19). All six are lang items of `core` ([16](16-core-library.md)).
- Both ends MUST have the same type: the right end is checked against the left end's type, and an integer-literal end defaults as usual.
- Ranges are non-associative (level 9), so `a..b..c` is a syntax error. Because ranges bind more loosely than arithmetic, `0..n + 1` means `0..(n + 1)`.
- Construction never panics and imposes no ordering: `5..1` is a valid, empty range.
- Iteration (integer ranges implement `Iterator`), slicing, and `contains` are library behavior ([16](16-core-library.md)).
- Ends are evaluated left to right.
- In argument position, `..e` is the record update syntax (§6.18.1), not a range. To pass a `RangeTo`, write `(..e)`.

## 6.20 Other expression forms

### 6.20.1 Hooks owned by other chapters

| Form | Typing hook | Owner |
|---|---|---|
| `e?` | If `e : Result<T, E>` and `Γ.ret = Result<U, E>`, the type is `T`; if `e : Option<T>` and `Γ.ret = Option<U>`, the type is `T`. The error types MUST be identical, with no conversion (D-EXPR-05). `?` uses `Γ.ret` of the innermost function or closure body. | [10](10-errors-and-panics.md) |
| `e.await` (Stage 5) | `e : F` with `F: Future`, giving `F.Output`; only when `Γ.async` | [11](11-async-and-concurrency.md) |
| `unsafe { … }` | a block with `Γ.unsafe = true` | [12](12-unsafe-and-ffi.md) |
| `region r { … }` | `r : borrow Arena` in the block | [07](07-ownership-and-borrowing.md) |
| `async { … }` (Stage 5) | a new function boundary producing a future | [11](11-async-and-concurrency.md) |

### 6.20.2 `assert`

`assert` is a statement (D-EXPR-06, 03 `AssertStmt`):

```text
(T-Assert)
Γ ⊢ c ⇐ Bool ! E1        Γ ⊢ m : M ! E2,  M ∈ { Str, String }   (if present)
────────────────────────────────────────────────────────────
Γ ⊢ assert c (, m)? ⊣ Γ ! E1 ∪ E2
```

- The message `m` MUST have type `Str` or `String` (N-C-20). An f-string (Stage 3) is therefore allowed. Its `alloc` effect is counted statically even though it runs only on failure.
- Semantics: `assert c, m` behaves as follows, where the panic has kind `UserAssert` and its location is that of the `assert` keyword:

  ```text
  if not c { <panic UserAssert with message m> }
  ```

- `m` is evaluated **only** if `c` is `false`.
- Without `m`, the message is the fixed text `assertion failed: ` followed by the source text of `c`, taken verbatim from the source file with leading and trailing whitespace removed.
- The check is performed in every profile (D-EXPR-06). The message format and its reporting are in [10](10-errors-and-panics.md).
- After a successful `assert c`, the SFC may use `c` as a fact ([09](09-contracts-and-refinements.md), D-CONTRACT-02).
- `assert` is not allowed in const initializers (§6.27).

```sai
fn average(total: I64, count: I64) -> I64 {
    assert count > 0, "count must be positive"
    total / count
}
```

### 6.20.3 f-strings (Stage 3)

An f-string `f"s0{e1:spec1}s1…{en:specn}sn"` (02 §2.6.5) is a value expression of type `String`. It desugars as follows, using hygienic names (N-C-21):

```text
{
    var buf = String.new()
    Writer.write_str(borrow mut buf, "s0")
    Format.format(borrow e1, borrow mut buf, spec1)
    Writer.write_str(borrow mut buf, "s1")
    …
    Format.format(borrow en, borrow mut buf, specn)
    Writer.write_str(borrow mut buf, "sn")
    buf
}
```

- Each interpolated expression `ei` is in a place context and is passed by implicit shared borrow. It is never moved or copied. Value expressions are materialized into temporaries that live until the end of the enclosing statement.
- `ei` MUST have a type that implements `Format`, otherwise `FORMAT_NOT_IMPLEMENTED`.
- The specifier `speci` is a `FormatSpec` (`LIDENT` with an optional integer argument). Its meaning, its representation as an argument value, and the error for an unknown or inapplicable specifier are defined in [16](16-core-library.md). An absent specifier selects the default format.
- The interpolations are evaluated left to right, each exactly once.
- The effect is `alloc`, plus the effects of the `ei`, plus the latent effects of the selected `Format` impls (D-EXPR-12).
- `{{` and `}}` produce literal braces. Empty segments are omitted from the desugaring.
- `Writer`, `Format`, the signature of `Format.format`, and `String.new` are defined in [16](16-core-library.md) (`core.fmt.Writer`). This chapter fixes only the evaluation order, the borrowing of operands, the result type and the effects.

```sai
fn describe(name: Str, n: U32) -> String {
    f"{name} has {n:hex} items"
}
```

## 6.21 Patterns

### 6.21.1 Pattern positions

| Position | Requirement |
|---|---|
| `let P = e` (no `else`) | irrefutable (`REFUTABLE_PATTERN_IN_LET`) |
| `let P = e else { … }` | refutable (`LET_ELSE_IRREFUTABLE`) |
| `for P in e` | irrefutable (`REFUTABLE_PATTERN_IN_FOR`) |
| `match` arm | any; the arms together must be exhaustive (§6.22) |
| `if let P = e`, `while let P = e` | any; irrefutable gives the warning `IRREFUTABLE_CONDITION_PATTERN` |
| function and closure parameters | not patterns; only a name or `_` (D-SYN-06) |

A pattern `P` is **irrefutable for `T`** iff the usefulness algorithm of §6.22 finds no witness for the one-row matrix `[P]` at type `T`. That is, the match `match x { P => … }` would be exhaustive.

### 6.21.2 Pattern typing

The judgment `Γ ⊢ P ⇐ T ⊣ Δ` checks `P` against the type `T` of the value it matches, and yields the bindings `Δ`. Each binding has a name, a type, a mode (`move`, `borrow`, `borrow mut`) and a mutability. The expected type always flows from the scrutinee into the pattern. Patterns never determine the scrutinee's type, except that integer-literal and float-literal inference variables in the scrutinee are unified as described below.

**Refined scrutinee.** If `T` is a refined type `R` with base `B` (D-TYPE-03), then:

- a top-level binding or `_` matches at type `R`;
- every other pattern form is checked against `B`. The value coerces R→B, which needs no proof.

Exhaustiveness uses `B` (D-EXPR-04, N-C-35).

| Form | Rule |
|---|---|
| `_` | any `T`; `Δ = ∅`; matches everything; never moves or borrows |
| `x` | `Δ = {x : T, move, imm}` |
| `var x` | `Δ = {x : T, move, mut}` |
| `borrow x` | `Δ = {x : borrow T, borrow, imm}` |
| `borrow mut x` | `Δ = {x : borrow mut T, borrow mut, imm}`; the matched place MUST be `mut` (`BORROW_MUT_OF_IMMUTABLE`) |
| `x @ P` | `Γ ⊢ P ⇐ T ⊣ Δ'`; `Δ = Δ' ∪ {x : …}` per the mode of `x`; restrictions in §6.21.4 |
| `(P)` | same as `P` |
| `P1 \| … \| Pn` | each `Γ ⊢ Pi ⇐ T ⊣ Δi`; all `Δi` MUST have the same names, types, modes and mutabilities (`OR_PATTERN_BINDING_MISMATCH`, 03); `Δ = Δ1` |
| literal | §6.21.3 |
| range | §6.21.3 |
| tuple `(P1, …, Pn)` | `T` MUST be a tuple type of arity `n` (`(P,)` for arity 1, `()` for unit), each `Pi ⇐ Ti`; otherwise `PATTERN_ARITY_MISMATCH` (wrong arity) or `TYPE_MISMATCH` (not a tuple) |
| slice `[…]` | §6.21.3 |
| constructor `Path(…)`, `Path` | §6.21.3 |
| implicit member `.V`, `.V(…)` | `T` MUST be an enum type, with no see-through, as in §6.4.3 (`IMPLICIT_MEMBER_NOT_ENUM`); then as `E<A>.V…` |

A name MUST NOT be bound twice in one pattern, except across the alternatives of an or-pattern (`DUPLICATE_BINDING`, D-MOD-03). An uppercase identifier never introduces a binding (D-LEX-02). An unresolvable `UIDENT` in a pattern is `UNRESOLVED_NAME` (04), not a fresh binding.

### 6.21.3 Pattern forms in detail

**Literal patterns** (N-C-33):

| Literal | Allowed scrutinee type `T` | Test |
|---|---|---|
| `n`, `-n` | a primitive integer type; an unresolved integer-literal variable is unified with the default `Int` only at the end of inference | mathematical equality; the value MUST be in range (`LITERAL_OUT_OF_RANGE`) |
| `true`, `false` | `Bool` | equality |
| `'c'` | `Char` | scalar equality |
| `b'c'` | `Byte` | equality |
| `"s"` | `Str` only | byte equality of contents |
| `b"s"` | `Slice<Byte>` only | length and byte equality |

- A literal pattern against any other type is `TYPE_MISMATCH`. In particular, a string literal does not match `String`; write `match s.as_str()`. An integer literal does not match a distinct type; write `D(5)`.
- Literal tests use the built-in equality and never call `PartialEq`.

**Range patterns.**

- The forms are `a..=b` (inclusive), `a..b` (end-exclusive), `a..` (from `a` to the maximum) and `..=b` (from the minimum to `b`).
- An end is an integer literal (optionally negative), a `Char` or `Byte` literal, or a `ConstPath` naming a const of the scrutinee type.
- `T` MUST be a primitive integer type, `Char` or `Byte`. Otherwise the error is `TYPE_MISMATCH`.
- Both ends MUST have type `T`.
- The range MUST be nonempty: `a ≤ b` for `..=`, and `a < b` for `..`. An empty range is `EMPTY_RANGE_PATTERN`.
- A range pattern matches `v` iff `a ≤ v` and `v ≤ b` (or `v < b`). Open ends impose no bound.
- `..b` is not a pattern, because `..` is the rest marker in slice patterns.

**Const patterns.**

- A `ConstPath` resolving to a const `C` is a const pattern. The type of `C` MUST be `T`, and MUST be a primitive integer type, `Bool`, `Char`, `Byte` or `Str` (N-C-33). Otherwise the error is `CONST_PATTERN_TYPE`.
- It matches iff the value equals the const's value under the built-in equality. For exhaustiveness, it is treated exactly like the literal pattern of its value.

**Slice patterns.** Let `k` be the number of non-rest elements, with `i` of them before the rest marker (the prefix) and `j` after it (the suffix). At most one `..` element is allowed (03).

| Scrutinee `T` | Elements | Length condition | Rest binding `..r` |
|---|---|---|---|
| `Array<U, N>` | each `⇐ U` | without rest `k = N`; with rest `k ≤ N`; otherwise `SLICE_PATTERN_LENGTH_MISMATCH` | `r : Array<U, N-k>` (move), `borrow r : Slice<U>`, `borrow mut r : SliceMut<U>` |
| `Slice<U>` | each `⇐ U`; element places are `imm` | tested at run time | `r : Slice<U>` (a subview; `Slice` is `Copy`), `borrow r : borrow Slice<U>` |
| `SliceMut<U>` | each `⇐ U`; element places are `mut` | tested at run time | `r : SliceMut<U>` (a reborrow of the scrutinee view, [07](07-ownership-and-borrowing.md)), `borrow r : Slice<U>` |

- `borrow mut r` on a `Slice<U>`, or `var r` on a view, follows the general place-mutability rules. For a rest of an `Array` behind a `let`, `borrow mut r` requires the array to be `mut`. On a `SliceMut`, `borrow mut r` is the same as `r` and gives the warning `REDUNDANT_BINDING_MODE`.
- The elements of a `Slice` or `SliceMut` scrutinee are places behind a view. A by-value binding of a non-`Copy` element is therefore `MOVE_OUT_OF_BORROW` (07) (D-EXPR-04).
- A slice pattern against any other type, including `Vec<U>` (write `match v.as_slice()`), is `TYPE_MISMATCH` (N-C-36).
- The test for a view of length `L` is `L = k` without rest, or `L ≥ k` with rest. Element `m` of the prefix is matched against index `m`, element `m` of the suffix against index `L - j + m`, and the rest covers indices `i … L-j-1`.

**Constructor patterns.** The `ConstPath` (or `Self`) resolves by [04](04-modules-and-names.md) to one of the following:

| Resolves to | Arguments | Rule |
|---|---|---|
| unit variant `E.V` | none; `E.V()` is `PATTERN_ARITY_MISMATCH` | matches iff the discriminant is `V` |
| tuple variant `E.V` with fields `U1 … Uk` | positional `P1 … Pn`, optionally followed by `, ..`, or `..` alone | without `..`, `n = k`; with `..`, `n ≤ k` and the missing trailing fields are `_`; otherwise `PATTERN_ARITY_MISMATCH`; a named argument is `UNKNOWN_FIELD`; a bare `E.V` without parentheses is `PATTERN_ARITY_MISMATCH` unless `k = 0` |
| record type `T` or record variant `E.V` | named `f: P`, punned binding `x`, `borrow x`, `borrow mut x` or `var x` (meaning `x: <binding>`), optional trailing `..` | each field at most once (`DUPLICATE_FIELD`); a field that is not named needs `..` (`MISSING_FIELD`); an unknown field is `UNKNOWN_FIELD`; a field that is not visible is `PRIVATE_FIELD`; an unnamed non-binding argument is `RECORD_FIELD_NEEDS_NAME` |
| distinct type `D = B` | exactly one positional `P ⇐ B`, or `..` | matches iff `P` matches the base value (D-TYPE-03); visibility follows the constructor's visibility |
| const | none | const pattern, see above |
| anything else (a type alias to a non-constructor, a function, a module) | — | `EXPECTED_PATTERN` |

- Generic arguments of the constructor are taken from `T`. The pattern may not write them.
- Record patterns are matched field by field in **declaration order** for testing, regardless of source order (§6.21.7).

### 6.21.4 Binding modes and `@`

There are no default binding modes ("match ergonomics" do not exist, D-EXPR-04). A binding without a mode moves or copies the matched subvalue. `borrow`, `borrow mut` and `var` must be written.

For `x @ P` (N-C-24):

1. If `x` binds by `move` and `T` is not `Copy`, then `P` MUST NOT contain any binding.
2. If `x` binds by `borrow`, then every binding in `P` MUST be either a `borrow` binding or a `move` binding of a `Copy` type.
3. If `x` binds by `borrow mut`, then `P` MUST NOT contain any binding.
4. If `x` binds by `move` and `T` is `Copy`, `P` may contain any bindings.

A violation is `AT_PATTERN_BINDING_CONFLICT`. `P` in `x @ P` cannot itself be an or-pattern without parentheses: `x @ (A | B)` (03 `AltPattern`).

### 6.21.5 Binding semantics

The bindings of a pattern take effect only after the whole pattern has matched and the guard, if any, has yielded `true` (§6.15.2). For each binding, in source order:

| Mode | Effect on the matched subplace `q` |
|---|---|
| `move`, `T: Copy` | copies `q` into the new local |
| `move`, `T` not `Copy` | moves `q` into the new local; this is a partial move of the scrutinee ([07](07-ownership-and-borrowing.md)) |
| `borrow` | the local holds `borrow q` |
| `borrow mut` | the local holds `borrow mut q` |

- `var` makes the new local mutable. It is combined with `move` only.
- A `move` of a non-`Copy` part out of a place reached through a `borrow`, a `borrow mut` or a view is `MOVE_OUT_OF_BORROW` (07).
- A `move` of a part of a value whose type implements `Drop` is rejected by [07](07-ownership-and-borrowing.md) (D-OWN-01).

### 6.21.6 Scrutinee contexts

- The scrutinee of `match`, `if let` and `while let` is in a place context.
- The initializer of `let P = e` is in a **value** context iff `P`, after removing parentheses, is a single `move` binding (`x` or `var x`). Then `e` is moved or copied as a whole.
- Otherwise the initializer of `let` is in a place context, so only the parts that are bound by `move` are moved:
  - `let _ = x` moves nothing (N-C-27);
  - `let (a, _) = pair` moves only `pair.0`.

### 6.21.7 Matching procedure

Matching a value `v` (stored in a place) against `P` is a pure test with no side effects. It calls no user code, so the order of tests is not observable. For definiteness, it is defined as follows:

1. `_` and bindings succeed. `x @ P` tests `P`.
2. Literals, consts and ranges compare with built-in equality and ordering.
3. A constructor pattern first tests the discriminant (for enums), then tests its subpatterns left to right in field declaration order, stopping at the first failure.
4. A tuple pattern tests elements left to right. A slice pattern tests the length, then the prefix left to right, then the suffix left to right.
5. An or-pattern tests the alternatives left to right. The first one that succeeds supplies the bindings.

The order in which bindings take effect is source order. Since bindings only move, copy or borrow, and none of these runs user code, the order is likewise unobservable.

```sai
enum Shape {
    Circle(radius: F64)
    Rect(width: F64, height: F64)
    Empty
}

fn area(s: borrow Shape) -> F64 {
    match *s {
        .Circle(radius: r) => 3.14159 * r * r
        .Rect(width, height) => width * height
        .Empty => 0.0
    }
}
```

```sai
fn classify(xs: Slice<I32>) -> Str {
    match xs {
        [] => "empty"
        [0, ..] => "starts with zero"
        [_, .., last] if last < 0 => "ends negative"
        [_, ..] => "other"
    }
}
```

## 6.22 Exhaustiveness and usefulness

This section defines the deterministic algorithm used for:

- exhaustiveness of `match` (`NON_EXHAUSTIVE_MATCH`)
- irrefutability (§6.21.1)
- unreachable arms (`UNREACHABLE_PATTERN`)

It is the usefulness algorithm of Maranget ("Warnings for pattern matching", 2007), with constructor splitting for integer ranges and slice lengths (N-C-22). Every conforming implementation MUST produce the same accept/reject result and the same set of warnings. The witnesses listed in diagnostics MUST be the first three in the order of §6.22.5.

### 6.22.1 Deconstructed patterns

Before analysis, every pattern is normalized into a **deconstructed pattern**:

| Source pattern | Deconstructed |
|---|---|
| `_`, any binding `x` / `var x` / `borrow x` / `borrow mut x` | wildcard `_` |
| `x @ P` | the deconstruction of `P` |
| `(P)` | the deconstruction of `P` |
| `P1 \| … \| Pn` | `Or(P1', …, Pn')` |
| literal or const of an integer, `Char` or `Byte` type with value `n` | `Range(n, n)` |
| range pattern | `Range(lo, hi)`, inclusive; `a..b` becomes `Range(a, b-1)`; open ends become the domain bounds (§6.22.2) |
| `true`, `false` | constructor `true` / `false`, arity 0 |
| `"s"`, `b"s"`, const of type `Str` | constructor `Lit(s)`, arity 0 |
| tuple `(P1, …, Pn)` | constructor `Tuple`, arity `n` |
| record pattern of type or variant `C` with fields `f1 … fk` in declaration order | constructor `C`, arity `k`; the subpattern at position `m` is the pattern given for `fm`, or `_` if `fm` is covered by `..` |
| tuple variant `C(P1, …, Pn)` (with `..` expanding to trailing `_`) | constructor `C`, arity `k` |
| unit variant `C` | constructor `C`, arity 0 |
| distinct `D(P)` | constructor `D`, arity 1 |
| slice pattern on `Array<U, N>` | `Slice(Fixed(k))` with `k` subpatterns, or `Slice(Var(i, j))` with `i + j` subpatterns (prefix, then suffix) |
| slice pattern on `Slice<U>` / `SliceMut<U>` | same as on arrays |

When the scrutinee type is refined, deconstruction uses the base (§6.21.2).

### 6.22.2 Constructor sets

Each type `T` has a **constructor set** `CS(T)`:

| Type | `CS(T)` |
|---|---|
| `Bool` | `{false, true}`, in that order |
| enum `E<A>` | its variants, in declaration order, each with arity equal to its field count; an enum with no variants has `CS = ∅` |
| `Never` | `∅` |
| tuple, record type, `()`, distinct type | a single constructor of the appropriate arity |
| `I8 … I64`, `U8 … U64` | the integer interval `[MIN, MAX]` of the type |
| `Byte` | `[0, 255]` |
| `Char` | `[0x0, 0xD7FF] ∪ [0xE000, 0x10FFFF]` |
| `USize` | `[0, +∞]` |
| `ISize` | `[-∞, +∞]` |
| `Array<U, N>` | the single constructor `Slice(Fixed(N))` of arity `N` |
| `Slice<U>`, `SliceMut<U>` | the lengths `0, 1, 2, …` (split as in §6.22.3) |
| every other type (`Str`, `String`, floats, `borrow T`, `borrow mut T`, `Box<T>`, `Vec<T>`, fn types, closures, type parameters, resources, …) | **opaque**: `{Opaque}`, where `Opaque` is covered only by wildcards; `Lit(s)` constructors may appear in rows but never make the set complete |

- For `USize` and `ISize`, `±∞` are symbolic bounds that no literal can reach. Only an open-ended range (`a..`, `..=b`) or a wildcard covers them. The result of the analysis therefore does not depend on the pointer width of the target (P4, N-C-22).
- Refinements are ignored (D-EXPR-04).

### 6.22.3 Splitting

Let `Σ` be the set of head constructors in the first column of a matrix (§6.22.4), after expanding `Or` rows.

**Integer splitting.** For an integer-like type with domain `Dom`, collect the boundary set `Bd`:

- `lo` and `hi + 1` for every `Range(lo, hi)` in `Σ`
- the same for the query constructor, if any
- the start and end+1 of each interval of `Dom`

Sort `Bd` and cut `Dom` at every boundary. The result is a sequence of disjoint maximal intervals `I1 < I2 < …`, each of which lies either entirely inside or entirely outside every range in `Σ`. These intervals are the **split constructors**.

- A range constructor `Range(a, b)` **covers** a split interval `I` iff `I ⊆ [a, b]`.
- `Σ` is **complete** iff every split interval is covered by some range in `Σ`.

**Slice splitting.** Over `Σ` for a `Slice`/`SliceMut` column:

- `maxFixed` is the largest `k` of any `Fixed(k)` in `Σ`, or `-1` if there is none.
- `maxPre` and `maxSuf` are the largest `i` and the largest `j` of any `Var(i, j)` in `Σ`, or `0` if there is none.
- If no `Var` constructor is present, set `L = maxFixed + 1`, `maxPre = L`, `maxSuf = 0`.
- Otherwise, if `maxFixed + 1 > maxPre + maxSuf`, set `maxPre = maxFixed + 1 - maxSuf`. Then set `L = maxPre + maxSuf`.

The split constructors are `Fixed(0), Fixed(1), …, Fixed(L-1)`, followed by `Var(maxPre, maxSuf)`, which stands for every length `≥ L` and has arity `L`.

- `Fixed(k)` covers only `Fixed(k)`.
- `Var(i, j)` covers `Fixed(n)` for every `n ≥ i + j`, and covers `Var(p, s)` iff `p ≥ i` and `s ≥ j`.
- `Σ` is complete iff every split constructor is covered.

For `Array<U, N>`, every slice pattern is converted to `Fixed(N)` before analysis, with `Var(i, j)` expanded by inserting `N - i - j` wildcards between prefix and suffix. The constructor set is then a singleton.

**Other types.**

- For `Bool` and enums, the split constructors are `CS(T)` itself, and `Σ` is complete iff it contains every constructor in `CS(T)`.
- For single-constructor types, `Σ` is complete iff it is nonempty.
- For opaque types, `Σ` is never complete.
- For types with `CS(T) = ∅`, `Σ` is vacuously complete.

### 6.22.4 The usefulness function

A **row** is a vector of deconstructed patterns. A **matrix** `M` is a list of rows of the same length `n`, with known column types `T1 … Tn`.

**Specialization** `S(c, M)`, for a split constructor `c` of arity `a`, maps each row `(p1, p2, …, pn)` as follows:

| `p1` | Resulting rows |
|---|---|
| `_` | `(_ × a, p2, …, pn)` |
| `Or(q1, …, qm)` | the rows obtained by specializing `(q1, p2, …)`, …, `(qm, p2, …)` in order |
| a constructor `c'` that covers `c`, with subpatterns `r1 … ra'` | `(r1', …, ra', p2, …, pn)`, where for slices `Var(i, j)` specialized by `Fixed(n)` or `Var(P, S)` inserts `a - i - j` wildcards between its prefix and its suffix; for ranges `a = 0` |
| a constructor that does not cover `c` | no row |

**Default matrix** `D(M)` keeps the rows whose `p1` is `_` (expanding `Or` rows first), with `p1` removed.

**Usefulness** `U(M, q)`, where `q = (q1, …, qn)`, is defined as follows:

1. If `n = 0`: `U` is true iff `M` has no rows.
2. If `q1 = Or(r1, …, rm)`: `U` is true iff `U(M, (ri, q2, …, qn))` is true for some `i`.
3. If `q1` is a constructor `c` (including a range): split `c` against the first column of `M` (for integers, the split intervals inside `c`; for slices, the split constructors covered by `c`; otherwise `{c}`). `U` is true iff `U(S(c', M), S(c', q))` is true for some split `c'`.
4. If `q1 = _`: let `Σ` be the first-column constructors of `M`, and split `CS(T1)` against `Σ`.
   - If `Σ` is complete: `U` is true iff `U(S(c, M), S(c, q))` is true for some split constructor `c`.
   - Otherwise: `U` is true iff `U(D(M), (q2, …, qn))` is true.

Rows of arms with a guard are never added to `M` (they may fail at run time). Their patterns are still checked for reachability.

**Reachability.** Let `M0` be empty. For the arms in source order:

1. For arm `i` with pattern `P` and top-level alternatives `A1 … Am` (`m = 1` if `P` is not an or-pattern): for each alternative `Al`, if `U(Mi ∪ {earlier alternatives of the same arm}, (Al))` is false, report `UNREACHABLE_PATTERN` on `Al`. If every alternative is unreachable, the warning is reported once, on the whole arm.
2. If the arm has no guard, `Mi+1 = Mi ∪ {(P')}`. Otherwise `Mi+1 = Mi`.

Only whole arms and top-level alternatives are reported. Redundant alternatives nested inside other patterns are not diagnosed. This keeps the warning set small and deterministic (P8).

**Exhaustiveness.** A `match` with arms `P1 … Pk` on scrutinee type `S` is exhaustive iff `U(Mk+1, (_))` is false.

### 6.22.5 Witnesses

When a `match` is not exhaustive (or a `let` / `for` pattern is refutable), the diagnostic lists witnesses, which are patterns describing values that no arm matches. The witness function `W(M, n)` returns a list of `n`-vectors, in this deterministic order:

1. If `n = 0`: `[()]` if `M` has no rows, else `[]`.
2. Let `Σ` be the first-column constructors of `M`, and split `CS(T1)`.
3. If `Σ` is complete: for each split constructor `c` in order, and for each `w ∈ W(S(c, M), a + n - 1)`, yield `(c(w1 … wa), wa+1 … )`.
4. Otherwise, for each `w ∈ W(D(M), n - 1)`:
   - if `Σ` is empty, yield `(_, w)`;
   - otherwise, for each split constructor `c` not covered by `Σ` (in order), yield `(c(_, …, _), w)`. For opaque types, yield `(_, w)` once.

Generation stops after three witnesses. The order of constructors is:

- declaration order for enums;
- `false` before `true`;
- ascending intervals for integers;
- increasing length for slices, with `Var` last.

Rendering:

| Witness | Rendered as |
|---|---|
| wildcard | `_` |
| enum variant | `.V`, `.V(_, _)` or `.V(f: _, ..)`, with its subwitnesses |
| integer interval `[a, a]` | `a` |
| integer interval `[a, b]` | `a..=b`; `a..` or `..=b` when a bound is infinite or equals the domain bound |
| `Char` interval | as `Char` literals with escapes per 02 |
| slice `Fixed(k)` | `[_, _, …]` with `k` elements |
| slice `Var(i, j)` | `[_, …, .., …, _]` |
| tuple | `(…)` |
| record | `T(f: …, ..)`, omitting `_` fields |
| distinct | `D(…)` |

```sai
fn sign(x: I8) -> I8 {
    match x {
        -128..=-1 => -1
        0 => 0
        1..=126 => 1
    }
    // rejected: NON_EXHAUSTIVE_MATCH, witness `127`
}
```

```sai
fn describe(b: Option<Bool>) -> Str {
    match b {
        Some(true) => "yes"
        Some(_) => "no"
        Some(false) => "never" // warning: UNREACHABLE_PATTERN
        None => "unknown"
    }
}
```

## 6.23 Evaluation order

Evaluation is strictly left to right and depth first, with each operand evaluated to completion before the next starts (D-EXPR-02). No optimization may change the observable order of effects, panics or drops. The order for each form:

| Form | Order |
|---|---|
| binary operator `a ⊕ b` (not `and`/`or`) | `a`, `b`, then the operation (and its checks) |
| `a and b`, `a or b` | `a`; `b` only if needed (§6.8.8) |
| prefix operator | operand, then the operation |
| call `f(a1, …, an)` | `f`, `a1`, …, `an`, unproven `requires` checks, then the call |
| method call `e.m(a1, …)` | `e`, the receiver adjustment, `a1`, …, then the call |
| field `e.f`, tuple field `e.N` | `e` |
| built-in index `e[i]` | `e`, `i`, bounds check |
| trait index `e[i]` | `e`, `i`, the `index`/`index_mut` call |
| record construction | explicit initializers in source order, then the base `..b` |
| tuple, array | elements left to right |
| `Array.filled(x)` | `x` once |
| range | start, then end |
| `p = e` | `e`, then the place `p` (base, then index operands, then bounds checks), drop of the old value, then the write |
| `p op= e` | `e`, then the place `p` once, the read, the operation and its checks, then the write |
| `let P = e` | `e`, the match, then the bindings |
| `if c a else b` | `c`, then one branch |
| `match e { … }` | `e`, then arms in order (pattern test, then guard), then the selected body |
| `assert c, m` | `c`; `m` only on failure |
| f-string | segments and interpolations left to right |
| `return e`, `break e` | `e`, unproven `ensures` and invariant checks (`return` only), drops of exited scopes, then the transfer |
| `e?` | `e`, then the test, then on the error path drops and the return ([10](10-errors-and-panics.md)) |
| block | statements in order |

Panics raised by built-in checks occur at the step shown, and every later step is not performed (§6.26).

## 6.24 Temporaries

### 6.24.1 Creation

A **temporary** is an unnamed local created by the compiler when a value expression must be a place (§6.2.2):

- the operand of `borrow` or `borrow mut`
- the base of a field, tuple field or built-in index
- a receiver that is auto-ref'd or auto-deref'd
- the operands of comparison operators
- f-string interpolations
- the scrutinee of `match`, `if let` and `while let`
- a `let` initializer in a place context

### 6.24.2 Temporary scopes

Every temporary is dropped at the end of its **temporary scope**. The scope is the innermost of the following that contains the expression that created the temporary (D-OWN-05, N-C-25):

1. **The enclosing statement.** This is the default (D-OWN-05).
2. **The condition of `if` or `while`** (not `if let` / `while let`). Temporaries are dropped after the condition yields its `Bool` and before the branch or body runs.
3. **A match guard.** Temporaries are dropped after the guard yields its `Bool`.
4. **The scrutinee of `match`, `if let` or `while let`.** Temporaries live until the end of the whole `match` / `if let` expression, including all arms and the `else` branch. For `while let`, they live until the end of each iteration.
5. **The iterable of `for`.** Temporaries live until the end of the whole `for` expression (N-C-26).
6. **The tail expression of a block,** including a function or closure body. Temporaries are dropped when the block completes: after the tail value has been produced and moved out, and before the block's named locals are dropped.
7. **The initializer of `let … else`.** If the match fails, temporaries are dropped before the `else` block runs. If it succeeds, they are dropped at the end of the statement.
8. **Arm bodies of `match` and branches of `if`.** An arm body or branch that is not a block is its own temporary scope.

Within one scope, temporaries are dropped in reverse order of creation. The general drop rules, including drop flags for conditionally moved temporaries, are in [07](07-ownership-and-borrowing.md).

### 6.24.3 Lifetime extension

There is exactly one case of temporary lifetime extension (D-OWN-05; [07](07-ownership-and-borrowing.md) §7.5.6 is authoritative). The case applies when the initializer of a `let` or `var`, after removing parentheses, is syntactically `borrow e` or `borrow mut e` and `e` is a value expression. The temporary created for `e` then lives until the end of the block that contains the `let`, and is dropped together with the block's locals, in reverse declaration order, as if it were declared just before the `let`.

No other expression extends a temporary's lifetime. `let p = (borrow make(), 1)` does not extend. A later use of `p` is `TEMPORARY_DROPPED_WHILE_BORROWED` ([07](07-ownership-and-borrowing.md)).

```sai
fn first_len() -> USize {
    let names = borrow make_names() // the temporary lives until the end of the body
    names.len()
}
```

(`make_names` is assumed to return a `Vec<String>`.)

## 6.25 Statements

### 6.25.1 Statement forms and restrictions

A block contains only the statements of 03 `Stmt`: `let`, `var`, assignment, `assert`, and expression statements.

- Items are not permitted inside blocks (D-SYN-10). This includes `fn` with a name, `type`, `enum`, `distinct`, `trait`, `impl`, `const`, `use`, `effect`, `invariant` and `test`. Each is `ITEM_IN_BLOCK`, and for `fn` the fix suggests a closure (`let f = fn(..) { .. }`).
- `;` is never a statement terminator (`NO_SEMICOLON`, 02). Statements are terminated by NEWLINE (D-LEX-01).
- An expression statement that begins with an implicit member (`.V`) has no expected type, so it is `IMPLICIT_MEMBER_NEEDS_TYPE`.

### 6.25.2 Discarded values

A value is **discarded** when it is the value of an expression statement that is not the tail of its block, or the value of a tail that is checked against `()` (§6.11.1). When a discarded value has type `T ∉ { (), Never }`:

1. If `T` is a must-use type, or the expression is a must-use call, the error is `UNUSED_MUST_USE` (D-EXPR-01). Both terms, and the message of the error, are defined by `@must_use` in [15](15-attributes-and-lints.md) §15.5.1–§15.5.3: `Result<T, E>` and the anonymous future types of `async fn` calls and `async` blocks are must-use types, and `Option<T>` is not. The fix is "use `let _ = …`" or "handle with `?`".
2. Otherwise, if the expression has no side effect (defined below), the warning is `STATEMENT_HAS_NO_EFFECT` (N-C-30).
3. Otherwise the value is dropped at the end of the statement.

An expression has **no side effect** iff it is one of the following, where every operand is recursively side-effect free:

- a literal, a path, `self` or `Self`
- a field or tuple-field access
- `borrow` or `borrow mut`
- a closure expression
- a construction of a record, variant, tuple or array
- a range
- a built-in comparison, logical, bitwise or shift operator

Calls, method calls, operator trait calls, indexing, arithmetic (which may panic), `?`, `.await`, `assert` and control flow are considered to have effects. The rule is deliberately syntactic (P8).

`STATEMENT_HAS_NO_EFFECT` applies also when `T = ()`, for example to a bare path of unit type.

```sai
fn check(r: Result<I32, Str>) {
    r                 // rejected: UNUSED_MUST_USE
}
```

## 6.26 Divergence and `Never`

### 6.26.1 Diverging expressions

An expression **diverges** iff its type is `Never`. The forms that diverge:

- `return`, `break` and `continue`
- calls of functions whose return type is `Never` (`panic`, `unreachable` (D-EXPR-06), `process.exit`, …)
- `loop` without a `break` that targets it (§6.16.1)
- `match` with all arms diverging, or with no arms on an empty type
- `if`/`else` with both branches diverging
- blocks per §6.11.1

`Never` coerces to every type at every position (§6.1.3). `?` does not make its expression diverge (only one path leaves).

A `let` may bind a value of type `Never` (`let x = panic("x")`). The statement then diverges.

### 6.26.2 Unreachable code

After a diverging subexpression in evaluation order (§6.23), the rest of the code is **unreachable**:

- the remaining operands of the enclosing expression;
- the following statements in the same block, up to the block's end.

The first unreachable statement or operand gives the warning `UNREACHABLE_CODE` (N-C-28), reported once per diverging expression. Unreachable code is still name-resolved and type-checked, and its effects are still included in the effect set of the enclosing function, so acceptance does not depend on flow analysis (P8, determinism principle). It is not lowered to MIR.

```sai
fn stop() -> I32 {
    return 1
    let x = 2 // warning: UNREACHABLE_CODE
    x
}
```

## 6.27 Constant expressions

The initializer of a `const` and the arguments of an explicit discriminant are evaluated at compile time (D-EXPR-07). With `Γ.const = true`, only these forms are allowed:

- literals, including `Str` and `b"…"` literals
- paths to other consts and to unit variants
- built-in operators on primitives: arithmetic, wrapping, bitwise, shifts, comparisons, `and`, `or`, `not`, unary `-` and `~`
- record, variant, distinct, tuple and array construction
- parentheses

Rules:

- Every other form is `NOT_CONST_EXPRESSION`. That includes calls, method calls, operator trait calls, `if`, `match`, blocks, closures, borrows, indexing, f-strings, `Array.filled` and `assert`. There is no const fn (D-EXPR-07).
- Evaluation follows §6.8 exactly, including all checks. An operation that would panic at run time is the compile-time error `CONST_EVAL_PANIC`, naming the panic kind. Float operations are evaluated with IEEE 754 semantics. A NaN result is allowed, and its bit pattern is unspecified (D-TYPE-06).
- A const that refers to itself, directly or through other consts, is `CONST_CYCLE`.
- A B→R conversion or a construction of a type with an invariant inside a const initializer MUST be proven by the SFC, because no runtime check can exist. Otherwise the error is `REFINEMENT_NOT_PROVEN` or `CONST_INVARIANT_NOT_PROVEN` ([09](09-contracts-and-refinements.md)).
- Each use of a const is an independent value expression (an inlined copy of the evaluated value). It is not a place.

## 6.28 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) leaves open. They are provisional until merged into it.

| ID | Rule | Rationale |
|---|---|---|
| N-C-1 | An integer literal never takes a floating-point type. `let x: F64 = 1` is `TYPE_MISMATCH`, with the fix "write `1.0`" (§6.3.1). | P1 and P8: no implicit numeric conversion, consistent with D-TYPE-04's ban on widening. |
| N-C-2 | Binary operators type the left operand by synthesis. For primitive left types, the right operand is checked against the left type (against `U32` for shifts). Otherwise both operands are synthesized and the operator trait is selected (§6.8.2). | P8: a single, local, predictable direction of inference; literals on the right adopt the left operand's type. |
| N-C-3 | An implicit member `.V` uses the expected type exactly, with no see-through of `borrow`, `Box`, distinct or refined types. Expected types come only from the coercion sites, the right operand of `==` / `!=`, and patterns. A non-enum expected type is `IMPLICIT_MEMBER_NOT_ENUM` (§6.4.3). | P1 and P8: the reader can always see which enum is meant. |
| N-C-4 | Comparison operands have the same type. The right operand is checked against the left operand's type. There is no cross-type `PartialEq` / `PartialOrd` (§6.8.7). | P9 and P8; matches "same-type operands only" of D-TYPE-06. |
| N-C-5 | Comparison operators take their operands by implicit shared borrow and never move them (§6.8.7, §6.8.10). | P9: `a == b` must not consume `String`s; the borrow is part of the operator, so D-OWN-02 (no implicit argument borrows) is not weakened. |
| N-C-6 | Arithmetic, bitwise and shift operators take their operands by value; non-`Copy` operands are moved (§6.8.2). | P1; required for `String + Str` to reuse the buffer (D-EXPR-13). |
| N-C-7 | `Bool` has no bitwise operators (`&`, `\|`, `^`, `~`). Use `and`, `or`, `not`, and `!=` (§6.8.5). | P9: one way to write each logical operation; avoids non-short-circuit surprises. |
| N-C-8 | For signed integers, `MIN % -1` evaluates to `0` and does not panic. Only `MIN / -1` panics with `Overflow(Div)` (§6.8.3). | P8 and P7: the mathematical remainder is representable, and Wasm `rem_s` returns 0 without trapping, so no extra check is needed. Resolves the ambiguous wording "and so does `MIN / -1`" in D-TYPE-06. |
| N-C-9 | `Byte` supports shifts `<<` and `>>` (logical) in addition to `& \| ^ ~` (§6.8.5). | P9: byte manipulation needs shifts; D-TYPE-01 allows bitwise operations on `Byte`, and shifts are bit operations. |
| N-C-10 | `r.f(x)` is always a method call. A callable field is called as `(r.f)(x)` (§6.5.2). | P8: no ambiguity between fields and methods. |
| N-C-11 | A tuple-variant path or a distinct-type path used as a value denotes a function item. A record type or record variant used as a value is `RECORD_CONSTRUCTOR_AS_VALUE` (§6.4.1, §6.18.2). | P9: `map(Some)` is useful; record constructors need names, so they have no positional function form. |
| N-C-12 | Indexing never auto-derefs its base. It is built-in for `Array`, `Slice` and `SliceMut` with a `USize` index, and an unresolved integer-literal index is unified with `USize` (§6.7.1). | P1 and D-OWN-02 (auto-deref only for fields and methods). |
| N-C-13 | Compound assignment on a non-`Copy` place is take-and-restore. It is allowed for every `mut` place, including places behind `borrow mut` (§6.13.2). | P9: `s += "x"` must work through `borrow mut String`; this is sound because there is no unwinding (D-ERR-01). |
| N-C-14 | A block with no tail, one of whose statements diverges, has type `Never` (§6.11.1). | P8: `{ return x }` and `{ panic("..") }` are usable as diverging else blocks (`let`-`else`). |
| N-C-15 | `break` with a value is allowed only in `loop`. `while`, `while let` and `for` always have type `()`. A `loop` with no `break` targeting it has type `Never` (§6.16). | P8: a value-carrying `while` would need a fallback value; the conditions of `while` are never evaluated statically. |
| N-C-16 | Jump targets resolve against a loop stack that is reset at every function, closure and `async` boundary. A label that shadows an enclosing label in the same body is `LABEL_SHADOWED` (§6.16.4). | P8: jumps are local and unambiguous. |
| N-C-17 | Closures capture whole variables, never field paths. Closures are neither recursive nor generic, and do not inherit `unsafe` outside an `unsafe module` (§6.17). | P8 and P7: a simple capture model with a predictable borrow footprint. |
| N-C-18 | `Array.filled(x)` requires `T: Copy`. `x` is evaluated once and lowered as `Repeat` (§6.18.5). | P8: no hidden per-element user code. |
| N-C-19 | The range types are `Range`, `RangeInclusive` (D-EXPR-09), `RangeFrom`, `RangeTo`, `RangeToInclusive` and `RangeFull`. Range construction never panics and imposes no ordering (§6.19). | P9: Rust-like names; construction is total. |
| N-C-20 | The message of `assert` has type `Str` or `String`, and is evaluated only on failure. The default message is `assertion failed: ` followed by the condition's source text (§6.20.2). | P8; f-string messages (Stage 3) work without a new formatting mechanism. |
| N-C-21 | f-strings desugar into `Writer.write_str` and `Format.format` calls on a fresh `String`. Interpolated operands are borrowed, never moved, and are evaluated left to right once (§6.20.3). | P3 and P8: a deterministic semantic desugaring (D-EXPR-12). |
| N-C-22 | Exhaustiveness and reachability use the algorithm of §6.22: Maranget usefulness with integer and slice splitting. `USize` and `ISize` have symbolic infinite bounds. Only whole arms and top-level alternatives are reported as unreachable. Empty types have empty constructor sets. | P4 (target independence), P8 (determinism), P2. |
| N-C-23 | In a match guard, bindings are shared views of the scrutinee. The guard may not move, assign or mutably borrow them (`GUARD_MOVE_OR_MUTATION`) (§6.15.2). | P8: a failed guard must leave the scrutinee untouched for later arms. |
| N-C-24 | `x @ P` restrictions on combining binding modes (§6.21.4). | P8: avoid simultaneous move and borrow of the same data. |
| N-C-25 | Temporary scopes (§6.24.2): conditions and guards drop their temporaries after producing the `Bool`; scrutinees keep them to the end of the construct; block tails drop them before the block's locals; temporaries are dropped in reverse creation order. | P8: drop points are visible from the syntax; avoids the "tail temporary outlives locals" trap. |
| N-C-26 | Temporaries of a `for` iterable live until the end of the `for` (§6.16.3). | P9: `for x in make().iter()` works. |
| N-C-27 | `_` never moves. A `let` whose pattern is not a single `move` binding treats its initializer as a place (§6.12.1, §6.21.6). | P1: `let _ = x` does not silently drop `x`. |
| N-C-28 | `UNREACHABLE_CODE` is a warning. Unreachable code is still type-checked and counted for effects, but not lowered (§6.26.2). | P8 and the determinism principle. |
| N-C-29 | `as` after an expression is `NO_AS_CAST`, with conversion suggestions (§6.10). | P1 and D-SYN-05 ("`as` is never numeric conversion"). |
| N-C-30 | "Without side effects" (D-EXPR-01) is defined syntactically (§6.25.2), and the warning is `STATEMENT_HAS_NO_EFFECT`. | P8: no dependence on effect inference or optimization. |
| N-C-31 | `+% -% *%` are built-in on primitive integers only, and have no traits (§6.8.4). | P7 and P9. |
| N-C-32 | An `if` without `else` used where a non-`()` value is required is `IF_WITHOUT_ELSE` (§6.14.1). | P8: a dedicated, actionable diagnostic. |
| N-C-33 | Literal-pattern typing: string literals only on `Str`, byte strings only on `Slice<Byte>`. Const patterns are limited to primitive integers, `Bool`, `Char`, `Byte` and `Str` (§6.21.3). | P8: matching never calls user `PartialEq`. |
| N-C-34 | Fields reached through a refined type, or `.value` of a refined distinct type, are immutable places (§6.6.1). | P2: a refinement predicate cannot be broken by a field write. |
| N-C-35 | A refined scrutinee: a top-level binding or `_` matches at the refined type; all other patterns and exhaustiveness use the base (§6.21.2). | D-EXPR-04 (refinements ignored for exhaustiveness); P8. |
| N-C-36 | Slice patterns apply only to `Array`, `Slice` and `SliceMut`, and have the rest-binding types of §6.21.3. | P1: `Vec` is matched through `as_slice()`. |

## 6.29 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `ARRAY_LENGTH_MISMATCH` | error | an array literal's length differs from the expected `Array<T, N>` |
| `ASSIGN_TO_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4 |
| `AT_PATTERN_BINDING_CONFLICT` | error | `x @ P` combines binding modes that conflict (§6.21.4) |
| `BLOCK_IN_CONDITION` | error | defined in [03](03-grammar.md) §3.13.5 |
| `BORROW_MUT_OF_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4; also raised for `FnMut` calls and `borrow mut` bindings on immutable places |
| `BREAK_OUTSIDE_LOOP` | error | `break` with no enclosing loop in the current body |
| `CANNOT_DEREFERENCE` | error | `*e` on a type that is not `borrow T`, `borrow mut T`, `Box<T>`, `Ptr<T>` or `PtrMut<T>`; a raw-pointer dereference outside `unsafe` is `UNSAFE_OPERATION_OUTSIDE_UNSAFE` ([12](12-unsafe-and-ffi.md)) |
| `CHAINED_COMPARISON` | error | defined in [03](03-grammar.md) §3.13.1 |
| `CONST_CYCLE` | error | a const depends on itself |
| `CONST_EVAL_PANIC` | error | evaluating a const initializer would panic (overflow, division by zero, shift overflow) |
| `CONST_INVARIANT_NOT_PROVEN` | error | defined in [09](09-contracts-and-refinements.md) §9.4.5 |
| `CONST_PATTERN_TYPE` | error | a const used as a pattern has a type other than an integer, `Bool`, `Char`, `Byte` or `Str` |
| `CONTINUE_OUTSIDE_LOOP` | error | `continue` with no enclosing loop in the current body |
| `DUPLICATE_BINDING` | error | defined in [04](04-modules-and-names.md) §4.4.3 (D-MOD-03); also a name bound twice in one pattern |
| `DUPLICATE_FIELD` | error | defined in [05](05-types.md) §5.6.1; also raised for a field named twice in a record construction or record pattern |
| `EMPTY_RANGE_PATTERN` | error | a range pattern that matches no value |
| `EXPECTED_PATTERN` | error | a path in a pattern that is not a constructor, variant or const |
| `EXPECTED_VALUE_FOUND_TYPE` | error | a type, module or trait used as a value |
| `FORMAT_NOT_IMPLEMENTED` | error | an f-string interpolation whose type does not implement `Format` |
| `FUNCTIONAL_UPDATE_OUTSIDE_RECORD` | error | `..e` as an argument of a call that is not a record construction |
| `GUARD_MOVE_OR_MUTATION` | error | a match guard moves, assigns or mutably borrows a binding or the scrutinee |
| `IF_WITHOUT_ELSE` | error | an `if` without `else` used where a non-`()` value is required |
| `IMPLICIT_MEMBER_NEEDS_TYPE` | error | `.V` without a known expected type (named in [03](03-grammar.md) §3.13.4, defined here) |
| `IMPLICIT_MEMBER_NOT_ENUM` | error | `.V` whose expected type is not an enum |
| `INVALID_ASSIGN_TARGET` | error | defined in [03](03-grammar.md) §3.16 |
| `INVALID_BREAK_VALUE` | error | `break` with a value targeting `while`, `while let` or `for` |
| `INVALID_TUPLE_INDEX` | error | a tuple index written with `_` or leading zeros |
| `IRREFUTABLE_CONDITION_PATTERN` | warning (lint) | `if let` or `while let` with an irrefutable pattern |
| `ITEM_IN_BLOCK` | error | an item declaration inside a block |
| `LABEL_SHADOWED` | error | a loop label that repeats an enclosing label in the same body |
| `LET_ELSE_IRREFUTABLE` | error | `let`-`else` with an irrefutable pattern |
| `LET_ELSE_NOT_DIVERGING` | error | the `else` block of `let`-`else` does not have type `Never` |
| `LITERAL_OUT_OF_RANGE` | error | defined in [02](02-lexical-structure.md) §2.6.1 |
| `MATCH_ON_BORROW` | error | a non-trivial pattern on a scrutinee of type `borrow T` / `borrow mut T` (fix: `match *e`) |
| `METHOD_NOT_FOUND` | error | defined in [05](05-types.md) §5.13.5 |
| `MISSING_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.5.2; an argument passed by value where the parameter is `borrow T` / `borrow mut T` (fix: insert `borrow`) |
| `MISSING_FIELD` | error | a record construction or record pattern misses fields |
| `MOVE_OUT_OF_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.17; raised by patterns for by-value bindings behind a borrow or view (fix: `borrow x`) |
| `NAMED_ARGUMENT_ON_FUNCTION` | error | defined in [03](03-grammar.md) §3.13.2 |
| `NON_EXHAUSTIVE_MATCH` | error | a `match` that does not cover every value; lists witnesses |
| `NOT_CALLABLE` | error | a call of a value whose type is not callable |
| `NOT_CONST_EXPRESSION` | error | a form not allowed in a const initializer |
| `NOT_INDEXABLE` | error | `e[i]` with no built-in indexing and no `Index` / `IndexMut` impl |
| `NOT_ITERABLE` | error | the `for` iterable does not implement `IntoIterator` |
| `NO_AS_CAST` | error | `as` used as a conversion operator |
| `NO_BANG_OPERATOR` | error | defined in [02](02-lexical-structure.md) §2.8 |
| `NO_SEMICOLON` | error | defined in [02](02-lexical-structure.md) §2.8 |
| `OPERATOR_NOT_SUPPORTED` | error | an operator not defined for the operand types |
| `OR_PATTERN_BINDING_MISMATCH` | error | defined in [03](03-grammar.md) §3.14 |
| `PATTERN_ARITY_MISMATCH` | error | wrong number of subpatterns for a tuple, tuple variant or unit variant |
| `PRIVATE_FIELD` | error | defined in [04](04-modules-and-names.md) §4.6.1 and §4.6.3; raised for field access, construction and patterns |
| `RECORD_CONSTRUCTOR_AS_VALUE` | error | a record type or record variant used as a function value |
| `RECORD_FIELD_NEEDS_NAME` | error | an unnamed, non-punning argument or subpattern in a record construction or pattern |
| `REDUNDANT_BINDING_MODE` | warning (lint) | `borrow mut r` rest binding on a `SliceMut` |
| `REDUNDANT_UPDATE_BASE` | warning (lint) | `..base` in a record construction that names every field |
| `REFINEMENT_NOT_PROVEN` | error | defined in [09](09-contracts-and-refinements.md) §9.2.3 |
| `REFUTABLE_PATTERN_IN_FOR` | error | a refutable pattern in `for` |
| `REFUTABLE_PATTERN_IN_LET` | error | a refutable pattern in `let` without `else`; lists witnesses |
| `RETURN_OUTSIDE_FUNCTION` | error | `return` in a const initializer; in a predicate the error is `INVALID_CONTRACT_EXPRESSION` (§6.16.5) |
| `SELF_OUTSIDE_IMPL` | error | defined in [05](05-types.md) §5.2.1 |
| `SHADOWS_MODULE` | error | defined in [04](04-modules-and-names.md) §4.5.1 (D-MOD-01) |
| `SLICE_PATTERN_LENGTH_MISMATCH` | error | a slice pattern that cannot match the length of an `Array<U, N>` |
| `STATEMENT_HAS_NO_EFFECT` | warning (lint) | a discarded expression without side effects (§6.25.2) |
| `TEMPORARY_DROPPED_WHILE_BORROWED` | error | defined in [07](07-ownership-and-borrowing.md) §7.5.6 |
| `TUPLE_INDEX_OUT_OF_RANGE` | error | `e.N` with `N` not less than the tuple's arity |
| `TYPE_ANNOTATIONS_NEEDED` | error | defined in [05](05-types.md) §5.16.4 |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14 |
| `UNDECLARED_LABEL` | error | a `break` or `continue` label that names no enclosing loop |
| `UNKNOWN_FIELD` | error | a field name that the record, variant or tuple type does not have |
| `UNKNOWN_VARIANT` | error | defined in [05](05-types.md) §5.13.6 |
| `UNREACHABLE_CODE` | warning (lint) | code after a diverging expression |
| `UNREACHABLE_PATTERN` | warning (lint) | a match arm or top-level alternative that can never match |
| `UNRESOLVED_NAME` | error | defined in [04](04-modules-and-names.md) §4.5.1 |
| `UNSAFE_OPERATION_OUTSIDE_UNSAFE` | error | defined in [12](12-unsafe-and-ffi.md) §12.2.5; for example `*p` on a raw pointer outside an unsafe context (§6.2.3, §6.9.1) |
| `UNUSED_MUST_USE` | error | a discarded value of a must-use type, or of a must-use call ([15](15-attributes-and-lints.md) §15.5, D-EXPR-01) (§6.25.2) |
| `USE_AND_OR` | error | defined in [02](02-lexical-structure.md) §2.8 |
| `WRONG_ARGUMENT_COUNT` | error | a call with the wrong number of arguments |
