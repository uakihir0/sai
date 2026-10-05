# 09. Contracts and Refinements

**Status:** Normative
**Stage:** Stage 3
**Related:** [03 Grammar](03-grammar.md), [05 Types](05-types.md), [06 Expressions and Statements](06-expressions-and-statements.md), [07 Ownership and Borrowing](07-ownership-and-borrowing.md), [08 Effects and Capabilities](08-effects-and-capabilities.md), [10 Errors and Panics](10-errors-and-panics.md), [13 Components and WIT](13-components-and-wit.md), [15 Attributes and Lints](15-attributes-and-lints.md), [impl/04 MIR](../impl/04-mir.md), [impl/09 Optimization](../impl/09-optimization-and-size.md), [decisions](../design/decisions.md)

This chapter defines three things:

- **refined types** (`type R = B where p`, and `distinct D = B where p`)
- **function contracts** (`requires`, `ensures`, with `result` and `old(..)`)
- **type invariants** (`invariant T { .. }`)

It then defines the **Standard Fact Checker** (SFC), the deterministic procedure that alone decides which of these obligations are discharged at compile time, and the runtime checks inserted for the rest. Acceptance of a program depends only on the SFC as specified here, never on optimizer strength or on external tools (D-CONTRACT-02). The syntax is in [03](03-grammar.md) §3.5, §3.6 and §3.8. The panic behavior of failed runtime checks is in [10](10-errors-and-panics.md).

## 9.1 Overview

### 9.1.1 Obligations

An **obligation** is a predicate that must hold at a program point. Every obligation has a **kind**, a **site**, and a **goal** formula. The SFC either **proves** the goal from the **facts** available at the site, or returns **not proven**. Its handling follows D-CONTRACT-01:

| Kind | Site | If proven | If not proven |
|---|---|---|---|
| refinement (B→R conversion) | a coercion site with expected type `R` and a value of type `B` (§9.2.3) | no code | compile error `REFINEMENT_NOT_PROVEN` |
| `requires` | each call of the function (§9.3.3) | no code | runtime check, panic `Contract(id)` |
| `ensures` | each return point of the function (§9.3.4) | no code | runtime check, panic `Contract(id)` |
| invariant | each check point of the type (§9.4.3) | no code | runtime check, panic `Invariant(id)` |
| refinement or invariant at a component export input | lifting (§9.7.3) | (not attempted) | runtime validation, panic `Refinement(id)` / `Invariant(id)` |

- Every runtime check is present in every profile (D-CONTRACT-01, D-TGT-02).
- Each inserted check is reported by the lint `CONTRACT_RUNTIME_CHECK` (info (lint), D-CONTRACT-01). The package policy `deny runtime_contracts` raises it to an error ([package/01](../package/01-package-contract.md)).
- The optimizer may remove a runtime check only with a sound proof (§9.7.2).

### 9.1.2 Predicates

A **predicate** is an expression of type `Bool` that appears in one of these places:

- a refinement clause
- a `requires` or `ensures` clause
- an element of an `invariant` block

Rules for every predicate (D-CONTRACT-03):

1. It MUST type-check against `Bool`, otherwise `TYPE_MISMATCH` ([05](05-types.md)).
2. Its effect set MUST be empty. This excludes `alloc`, so calls are allowed only to pure functions. Otherwise the error is `EFFECTFUL_PREDICATE`, defined with its exact rules in [08](08-effects-and-capabilities.md) §8.11.
3. It MUST NOT contain `return`, `break`, `continue`, `?`, `.await`, assignments, `var` statements, `unsafe` blocks, `region` blocks, or closures. These are `INVALID_CONTRACT_EXPRESSION`. `let` statements inside blocks are allowed, as are `if`, `match`, `and`, `or`, `not`, calls and method calls.
4. Its free names are restricted per position (§9.2.1, §9.3.1, §9.4.1). Any other local is `UNRESOLVED_NAME` ([04](04-modules-and-names.md)).
5. It is a value of the language, with the meaning of [06](06-expressions-and-statements.md). Whether the SFC understands it affects only whether it is proven, never whether it is accepted (D-CONTRACT-03).
6. A predicate is resolved and type-checked in the scope of its declaration (the callee for contracts, the defining module for types), regardless of where its runtime check is placed. Privacy is checked at the declaration only (N-C-43).

### 9.1.3 Identifiers of obligations

Each `requires` clause, `ensures` clause, refinement clause and invariant element has a **contract id**. The id is used in `AssertKind` (`Contract(id)`, `Refinement(id)`, `Invariant(id)`, D-MIR-01), in diagnostics, and in panic messages. It consists of:

- the canonical path of the declaring item ([04](04-modules-and-names.md))
- the clause kind
- the clause's zero-based index among clauses of that kind on that item

For example, `app.bank.withdraw#requires[1]` or `app.bank.Account#invariant[0]`. The id is stable under edits elsewhere in the file (N-C-37).

## 9.2 Refined types

### 9.2.1 Declaration

```text
type R = B where p          (TypeDecl with AliasDef and RefinementClause)
distinct D = pub? B where p (DistinctDecl with RefinementClause)
```

- `R` is a new nominal type, distinct from `B` and from every other refined type, even one with an identical predicate (D-TYPE-03).
- Refined types cannot be generic, and interface type declarations cannot be refined (03 §3.6).
- For the `type` form, the base `B`, after alias expansion, MUST NOT itself be a refined type (`REFINED_BASE_IS_REFINED`, [05](05-types.md) §5.6.4, N-A-25). The base of a distinct type may be any type, including a refined type ([05](05-types.md) §5.6.3).
- `B` cannot mention a type parameter, because neither form may be generic when it has a refinement clause (03 §3.6; `GENERIC_REFINED_TYPE`, [05](05-types.md) §5.6.4). Views such as `Str` are allowed (R-06).
- In `p`, the only free local is `self`, of type `B` (N-C-38). This holds for distinct types too: in `distinct Age = U8 where self <= 150`, `self : U8`.
  - `self` is a place in a place context: it may be read if `B: Copy`, borrowed, used as a method receiver, or compared, but not moved (`MOVE_OUT_OF_BORROW`, [07](07-ownership-and-borrowing.md)).
  - Consts and functions are in scope as usual.
- `p` is a predicate (§9.1.2).

```sai
type Percentage =
    I32 where self >= 0 and self <= 100

type NonEmptyStr = Str where self.len() > 0

distinct Age = U8 where self <= 150
```

### 9.2.2 Values and operations

A value of type `R` has the representation of `B`. Its predicate is guaranteed to hold for the value. The SFC uses this as a fact (§9.6.3). The operations on `R` are defined by **refinement erasure** (N-C-39):

1. **Coercion R→B** (D-TYPE-04 item 6) applies at every coercion site. It is free and needs no proof.
2. **Shared borrows.** `borrow R` coerces to `borrow B` at every coercion site (D-TYPE-04 item 6, N-C-40). `borrow mut R` never coerces to `borrow mut B`, because a write through it could break the predicate.
3. **Every other use** that needs a `B`, whether or not it is a coercion site, also sees the value as `B`. This covers:
   - operands of operators
   - comparison operands
   - built-in and trait indexing on reads
   - f-string interpolations
   - match scrutinees other than a top-level binding (06 N-C-35)
   - field reads (06 §6.6.1)

   The results of these operations have the types the base operation gives (usually `B`), never `R`. `p + 1` with `p : Percentage` has type `I32`.
4. **Method calls** on a receiver of type `R`:
   1. Inherent methods of `R` (declared in `impl R` in the defining package) and trait methods implemented for `R` are found first ([05](05-types.md) §5.13).
   2. Otherwise lookup continues with the base step of [05](05-types.md) §5.13 on `B`. Associated functions without `self` are not found this way. A method of `B` whose receiver is `self: Self` or `self: borrow Self` is applicable, and the receiver is coerced to `B` or `borrow B`. A method of `B` whose receiver is `self: borrow mut Self` is not applicable to an `R` place. Selecting it is the error `REFINED_PLACE_MUTATION`, with the fix "convert to `B`, mutate, and convert back with `R.try_from`".
5. **Trait implementations of `R`.** `R` has these trait implementations, all compiler-provided (forwarded to `B`):
   - `Copy` and `Clone` iff `B: Copy`
   - `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format` iff `B` implements them

   The forwarded traits return no `Self` value except `Clone`, which returns a copy of an existing `R`, so they cannot break the predicate. No other trait is implemented automatically, and `@derive` on a refined type accepts only `Arbitrary` ([05](05-types.md) §5.12). A user impl for `R` of one of the forwarded traits overlaps the provided impl (`OVERLAPPING_IMPLS`, [05](05-types.md) §5.10.2). Users MAY implement other traits for `R` within the coherence rules of [05](05-types.md).
6. **No propagation.** `Vec<R>` is not `Vec<B>`, `Option<R>` is not `Option<B>`, and `fn() -> R` is not `fn() -> B` (D-TYPE-03). A conversion of a container is an explicit `map`.

### 9.2.3 Construction: B→R

A **B→R conversion** occurs at a coercion site (06 §6.1.3) whose expected type is `R`, when the expression has type `B` or is a literal checked against `B`. It also occurs at every construction `D(e)` of a distinct type with a refinement clause. The obligation is

```text
goal:  p[self := ⟦e⟧]
```

where `⟦e⟧` is the SFC term for the converted value (§9.6.4).

- The conversion is accepted iff the SFC proves the goal at that point (D-TYPE-03, D-CONTRACT-01). It emits no code.
- Otherwise the error is `REFINEMENT_NOT_PROVEN` at the expression. The error is never downgraded to a runtime check. The diagnostic carries these fixes, in this order:
  1. "use `R.try_from(e)`" (machine-applicable when the surrounding code can take an `Option<R>`);
  2. "add `requires <p[self := param]>` to the enclosing function", offered when `e` is a parameter of the enclosing function or a term over parameters;
  3. "check the condition with `if` first", with a note that shows the goal and the facts used.
- A diagnostic for a goal the SFC could not decide because of a limit (§9.6.8) adds the note "the Standard Fact Checker exceeded its limits".

`R.try_from(value: B) -> Option<R>` (and `D.try_from(value: B) -> Option<D>` for a refined distinct type) is a compiler-provided associated function (N-C-41). Its effect set is empty. It evaluates `p` with `self` bound to `value`, and returns `Some` of the converted value if `p` is `true`, or `None` otherwise. If `p` itself panics, the panic propagates. `try_from` performs no SFC reasoning and is always a runtime test. This is distinct from `TryFrom.try_from` of the `TryFrom` trait (D-TYPE-06, [16](16-core-library.md) §16.5.7), which any type may implement and whose library impls include the checked numeric conversions ([16](16-core-library.md) §16.8.5). `R.try_from` is not a `TryFrom` impl, and an `impl TryFrom<B> for R` written by a program is never selected by `R.try_from(v)` ([16](16-core-library.md) §16.5.7).

```sai
fn to_percent(x: I32) -> Percentage {
    if x < 0 {
        return 0
    }
    if x > 100 {
        return 100
    }
    x
}

fn bad(x: I32) -> Percentage {
    x    // rejected: REFINEMENT_NOT_PROVEN (fix: `Percentage.try_from(x)`)
}
```

```sai
fn label(s: Str) -> Option<NonEmptyStr> {
    if s.len() > 0 {
        return Some(s)
    }
    None
}
```

In `label`, the goal `s.len() > 0` is proven from the dominating condition `s.len() > 0`, using a call atom for the pure call `s.len()` (R-06, §9.6.4).

### 9.2.4 Flow of refined values

- Moving or copying a value of type `R` keeps its type.
- Passing an `R` where `R` is expected needs no proof.
- Every SFC term whose static type is a refined type `R` contributes the fact `p[self := term]` wherever the term is available (§9.6.3, N-C-42). This includes parameters, locals, fields, call results and pattern bindings.
- Values of type `R` obtained through `unsafe` code (for example `core.mem.transmute`) are assumed to satisfy `p`. Violating this is undefined behavior ([12](12-unsafe-and-ffi.md)).

### 9.2.5 Mutation of refined places

- Assigning to a place of type `R` (`x = e`, `x op= e`) is a coercion site. The right-hand side must therefore be an `R`, or a `B` whose conversion the SFC proves. `x += 1` with `x : Percentage` is `x = x + 1`, where `x + 1 : I32` must be proven to lie in `0..=100`.
- Fields reached through `R` are immutable places (06 N-C-34).
- `borrow mut x` with `x : R` has type `borrow mut R`. Only `R` values can be written through it.
- Methods of `B` with a `borrow mut Self` receiver are not applicable (§9.2.2 rule 4).

```sai
fn bump(var p: Percentage) -> Percentage {
    p += 1    // rejected: REFINEMENT_NOT_PROVEN (`p + 1 <= 100` does not follow)
    p
}
```

## 9.3 Function contracts

### 9.3.1 Clauses and scope

A function head may carry any number of `requires` clauses followed by any number of `ensures` clauses, after its `where`, `borrows` and `effects` clauses (03 §3.5). The conjunction of the `requires` clauses is the function's **precondition**. The conjunction of its `ensures` clauses is its **postcondition**. Each clause is a separate obligation with its own contract id (§9.1.3).

Names in scope:

| Clause | Free locals in scope |
|---|---|
| `requires` | the parameters, including `self` |
| `ensures` | the parameters, including `self`, plus `result` and `old(..)` |

- `requires` and `ensures` are allowed on `fn` items, inherent and trait methods, `async fn`, `unsafe fn`, and functions in `extern` blocks. Closures and function-pointer types have no contracts (§9.5.3).
- A `requires` clause sees each parameter as a place of the parameter's declared type at function entry.
- An `ensures` clause sees each parameter at the return point (§9.3.4). For a `borrow mut T` parameter `p`, `*p` denotes the value after the call.
- On an `extern` function the clauses are trusted declarations about foreign code. `requires` is checked at call sites as usual. `ensures` is never checked at runtime and is used as a fact at call sites (N-C-44). This is part of the `unsafe` contract of [12](12-unsafe-and-ffi.md): an `ensures` that the foreign code violates is undefined behavior. A function of an `extern "wasm"` block with an `ensures` clause MUST be declared in an `unsafe extern "wasm"` block, otherwise `EXTERN_ENSURES_REQUIRES_UNSAFE` ([12](12-unsafe-and-ffi.md) §12.7.3 item 3, N-I-29). An `extern "C"` block is always `unsafe` ([12](12-unsafe-and-ffi.md) §12.9.1).

### 9.3.2 `result` and `old`

- **`result`** denotes the returned value inside an `ensures` clause. Its type is the declared return type, or `()` when none is declared. For an `async fn`, it is the declared output type, not the future.
  - Outside an `ensures` clause, `result` is an ordinary identifier (02 §2.5.2). When it does not resolve inside any other predicate, the diagnostic is `RESULT_OUTSIDE_ENSURES` instead of `UNRESOLVED_NAME`.
  - A function that has at least one `ensures` clause MUST NOT have a parameter named `result`: `RESULT_NAME_CONFLICT` (N-C-45).
  - `result` is a place of the return type. It may be read when the type is `Copy`, borrowed, or used as a receiver. It MUST NOT be moved (`MOVE_OUT_OF_BORROW`).
- **`old(e)`** inside an `ensures` clause denotes the value of `e` evaluated at function entry, after parameter binding and after the precondition checks. Rules (N-C-46):
  1. `old(..)` takes exactly one expression. `e` is subject to the predicate rules of §9.1.2 and may name only parameters.
  2. `old` MUST NOT be nested: `NESTED_OLD`.
  3. `e` MUST NOT mention `result`: `OLD_REFERS_TO_RESULT`.
  4. The type of `e` MUST be `Copy`: `OLD_REQUIRES_COPY`, with the fix "copy the needed scalar, for example `old(v.len())`". This keeps the snapshot from holding a borrow across the body or allocating.
  5. Outside `ensures`, `old(..)` that does not resolve to a function in scope is `OLD_OUTSIDE_ENSURES`. Inside `ensures`, `old(` always has its contract meaning, even if a function named `old` is in scope.
  6. A snapshot `old(e)` is evaluated at entry only when at least one `ensures` clause that mentions it needs a runtime check at some return point (§9.3.4). Otherwise it emits no code. Since `e` has no effects, whether it is evaluated is observable only through a panic in `e`. A predicate whose `old(..)` operand can panic needs a runtime check, and the panic belongs to that check (§9.3.5).

```sai
fn push_counted(v: borrow mut Vec<I32>, x: I32)
    effects { alloc }
    ensures v.len() == old(v.len()) + 1
{
    v.push(x)
}
```

### 9.3.3 Preconditions at call sites

For each direct call of a function `f` (a call whose callee is statically resolved to `f` or to a method of a known impl, 06 §6.5), each `requires` clause of `f` is an obligation at the call site.

- The goal is the clause with each parameter replaced by the term for its argument (§9.6.4), after the arguments have been evaluated (06 §6.5.1). If an argument is not representable as a term, the parameter is a fresh atom, and only the type facts of that argument apply.
- The facts are those at the call site (§9.6.3).
- If the SFC proves the goal, no code is emitted. Otherwise a check is inserted between argument evaluation and the call. It evaluates the clause on the argument values and panics with kind `Contract(id)` when it is `false`. The panic location is the call expression. The message is defined in §9.7.1.
- `requires` clauses are checked in declaration order, and the first failing clause panics.

**Indirect calls.** When a function with `requires` clauses is converted to a function pointer (D-TYPE-04 item 5), or used as a value through an `Fn` trait bound, the resulting value calls a **checked entry**. The checked entry evaluates the unproven-at-entry `requires` clauses before entering the body, with the location of the function's name in its declaration (N-C-47). The checked entry of a library function takes the location that [10](10-errors-and-panics.md) §10.4.5 gives to library panics instead. Every `requires` clause is unproven at a checked entry, because no call-site facts exist there. Each one is reported by `CONTRACT_RUNTIME_CHECK` once, at the conversion site. Direct calls never go through the checked entry.

Inside the body of `f`, each `requires` clause of `f` is a fact at entry (§9.6.3).

### 9.3.4 Postconditions at return points

The **return points** of a function are:

1. each `return e` expression
2. the tail expression of the body, when the body does not diverge
3. each early return produced by `?` ([10](10-errors-and-panics.md) §10.3)

At each return point, each `ensures` clause is an obligation.

- The goal is the clause with `result` replaced by the term for the returned value, and each `old(e)` replaced by the term `e` interpreted at entry (§9.6.6).
- If the SFC proves the goal, no code is emitted. Otherwise, the check runs after the returned value is evaluated and before the function's locals are dropped (06 §6.16.5). It panics with kind `Contract(id)` when the clause is `false`. The panic location is the return point (the `return` keyword, the tail expression, or the `?` token).
- `ensures` clauses are checked in declaration order.
- Invariant checks at the same return point (§9.4.3) run after all `ensures` checks.

A function whose body always diverges has no return points and no `ensures` obligations.

**Moved parameters.** An `ensures` clause that mentions a by-value parameter `p` which is moved, or may be moved, on the path to some return point is rejected with `ENSURES_USES_MOVED_PARAMETER` at that return point, with the fix "copy the needed value with `old(..)`" (N-C-48). The moved-state analysis is that of [07](07-ownership-and-borrowing.md).

**At call sites.** After a direct call of `f` returns normally, each `ensures` clause of `f` becomes a fact at the call site:

- `result` is replaced by the term for the call's value.
- Each parameter is replaced by its argument term, at its version after the call.
- `old(e)` is replaced by `e` over the argument terms at their versions before the call.

For a `borrow mut` argument `borrow mut x`, the call kills facts about `x` (§9.6.3) before the `ensures` facts are added, so `ensures` can describe the new value of `*p`.

### 9.3.5 Evaluation of runtime checks

- A runtime contract check evaluates the predicate as an ordinary expression (06). A panic raised while evaluating it (for example an overflow) is a panic of the predicate's own kind and location, and is not converted into a contract failure.
- Predicates have no effects (§9.1.2), so a check that does not panic has no observable effect other than its time cost.
- A predicate that the SFC proves is never evaluated.
- The optimizer may remove an unproven check only if it proves the check neither fails nor panics (§9.7.2).
- Temporaries created by a check are dropped at the end of the check.
- **Ownership.** A check reads the places that its predicate mentions, at its check point. These reads are accesses under the rules of [07](07-ownership-and-borrowing.md), and they are checked for every obligation, whether the SFC proves it or not, so acceptance never depends on the SFC result. A mentioned place that is moved, or that a live loan forbids reading, at the check point is therefore an error of 07, for example a postcondition that mentions `*p` when the result holds a `borrow mut` into `*p` (`USE_WHILE_MUT_BORROWED`). The moved by-value parameter of an `ensures` clause is `ENSURES_USES_MOVED_PARAMETER` (§9.3.4).

### 9.3.6 Contracts are not part of the type

- Contracts do not affect a function's type. Two functions with the same signature and different contracts have the same function-pointer type.
- Changing a `pub` function's contracts can still break callers of a library: their code may now need a runtime check, which `deny runtime_contracts` rejects, or a fact they relied on may disappear so that a B→R conversion no longer proves. A change to the contracts of a `pub` function is therefore a change of its API, even though its type is unchanged.

## 9.4 Invariants

### 9.4.1 Declaration

```text
invariant T {
    p1
    p2
}
```

- `T` MUST be a record type, enum type, resource type or distinct type declared in the same module (`INVARIANT_OUTSIDE_DEFINING_MODULE`, 03 §3.8).
- Interface types, refined types (`type R = B where ..`), primitive types and aliases are rejected: `INVARIANT_ON_INVALID_TYPE`. A refined type already carries its predicate.
- `T` MUST NOT have generic parameters: `INVARIANT_ON_GENERIC_TYPE` (N-C-49). The invariant grammar names no parameters (03 §3.8). An invariant over a generic type could not be stated without them, and per-instantiation checking would make acceptance depend on monomorphization.
- A type has at most one `invariant` declaration: `DUPLICATE_INVARIANT`.
- The invariant of `T` is the conjunction of its elements, in order. Each element is a predicate (§9.1.2) whose only free local is `self : borrow T` (03 §3.8). Field access through `self` auto-dereferences (06 §6.9.3).
- The elements of an invariant on a distinct type may use `self.value`. On an enum, they typically use `match *self`, because `match self` on the borrow with a non-trivial pattern is `MATCH_ON_BORROW` (06 §6.15.1).

### 9.4.2 Encapsulation

Writing to fields of a type with an invariant is allowed only in the defining module (D-CONTRACT-04). Outside the defining module, including in other modules of the same package, these are `INVARIANT_FIELD_WRITE`:

- assigning to a field place, or a place nested inside a field (`a.f = e`, `a.f.g += 1`, `a.f[i] = e`)
- `borrow mut` of a field place, including the implicit one of a method call with a `borrow mut self` receiver on a field
- a `borrow mut` binding mode in a pattern that binds a field place of `T` (06 §6.21)

Construction is not a field write (N-C-50). A record construction, functional update, variant construction or `D(e)` of `T` is allowed wherever the visibility rules of [04](04-modules-and-names.md) allow it, and it is always a check point (§9.4.3). Reading fields and pattern matching are also governed by visibility only. A pattern that destructures `T` by value moves the fields out and does not write them.

Assigning a whole value to a place of type `T` (`x = t`) and passing `borrow mut x` to any function remain allowed everywhere. Neither can create a value of `T` that was not produced by a checked construction or a checked return.

### 9.4.3 Check points

The invariant of `T` is an obligation at each of these **check points** (D-CONTRACT-04):

1. **Construction.** Every construction expression that produces a `T`:
   - record construction and functional update
   - a variant construction of `T`
   - `D(e)` for a distinct `T`
   - array and tuple elements are not separate constructions

   The goal is the invariant with `self` replaced by the term for the new value. A runtime check runs immediately after the construction. The location is the construction expression.
2. **Return from a function of the defining module.** At each return point (§9.3.4) of a function, method or closure body lexically inside the defining module:
   1. for each parameter `p : borrow mut T`, the goal for `*p`
   2. for each parameter `p : T` that is still initialized at that return point, the goal for `p`. Parameters that may or may not be initialized (07 drop flags) are checked only when their flag says initialized.
   3. if the return type is exactly `T`, the goal for `result`

   Values of `T` nested inside parameters or results of other types (`Option<T>`, `Vec<T>`, `borrow mut Wrapper` with a `T` field) are not check points (N-C-51). `self` is treated like any other parameter. A parameter of type `borrow T` is not a check point, because the function cannot write through it.

   The runtime check runs after all `ensures` checks of that return point, in parameter order, then the result. The location is the return point.
3. **Trust boundary.** Lifting a `T` from a component export input, or from other untrusted external input (§9.7.3).
4. **Dirty places** (N-C-53). The check points of D-CONTRACT-04 alone would let a value with a broken invariant escape: a local whose field was written could be passed to another function, and a `T` nested in a `borrow mut` parameter of another type could be returned to the caller broken. The following check points close these paths, so the invariant holds wherever a value of `T` is observed as a whole:
   - Inside the defining module, a place `q` of type `T` becomes **dirty** at:
     - an assignment to a place strictly inside `q` (a field of `q` or a place nested in one)
     - the creation of a `borrow mut` of such a place
     - the creation of a `borrow mut` of `q` itself, unless the borrow expression is directly a call argument or an auto-ref'd receiver. A borrow bound to a local or captured by a closure can be used for field writes that the analysis of `q` does not see. Field writes through such a borrow `r` make `*r` dirty by the two rules above.
   - `q` becomes **clean** at a check of `q`'s invariant (proven or run) and when a whole new value is assigned to `q`.
   - A place reached through a built-in index expression `b[i]` (06 §6.7.1), whether or not `i` is a constant, is tracked as the **element set** `b[*]` of all elements of `b`: an event on any element applies to `b[*]`, and assigning a whole value to one element does not clean it. A check of `b[*]` checks every element of `b` in ascending index order. Elements that were not written satisfy the invariant, so the additional checks change only the running time.
   - A place reached through the `Deref` of a `borrow mut` or `SliceMut` value that a local variable or a temporary `r` holds, where `r` is not a parameter or a captured variable, is a **borrowed place** rooted in `r` (N-C-85). Examples are the element that `v[i].f = x` writes through the `borrow mut` of trait-based indexing (06 §6.7.2), and the target of a `borrow mut T` that a method returns. Its value stays in the place it was borrowed from when `r` ends, so the end of `r` does not make it clean. Check point 3 below checks it instead.
   - Dirtiness is a may-analysis over the control flow: `q` is dirty at a point if it is dirty on some path to it. Moving out of a field of `q` (a partial move, [07](07-ownership-and-borrowing.md)) does not make `q` dirty. A partially moved `q` cannot be used as a whole anyway.
   - Check points:
     1. Immediately before each **whole use** of a dirty `q`. A whole use is any use of `q` other than these:
        - reading, assigning, borrowing or moving a place strictly inside `q`
        - assigning a whole value to `q`

        Whole uses include a move or copy of `q`, `borrow q`, `borrow mut q`, an auto-ref'd method receiver `q`, and the drop of `q` when `T` implements `Drop`. The location of the check is the first token of the expression that makes the use.
     2. At each return point, for each dirty place rooted in a parameter or in a captured variable of a closure, since such a place outlives the call. These checks run after the checks of check point 2 at the same return point, and the location is the return point.
     3. For each dirty borrowed place rooted in `r`: immediately before each move of `r` and each assignment to `r`, and immediately after the last use of `r` on each path, that is, where `r` stops being use-live ([07](07-ownership-and-borrowing.md) §7.9.4). The location is the first token of the expression that makes that use of `r`. A temporary is used last in the statement that creates it (06 §6.24.2), so `v[i].f = x` checks the element at the end of that statement. The check reads the place, so a borrow derived from `r` that is still live at that point conflicts with it (07).

   A check is performed only on paths where `q` is dirty and initialized; the implementation tracks this like a drop flag. Once the borrows that made `q` dirty have ended (07), the check reads `q` directly.

A function of the defining module may therefore break the invariant of a value between check points, for example while updating two fields one after the other. The broken state is never observable outside such a window.

```sai
type Range16 {
    lo: U16
    hi: U16
}

invariant Range16 {
    self.lo <= self.hi
}

fn shift(r: borrow mut Range16, by: U16) {
    r.hi = r.hi + by // `*r` becomes dirty
    r.lo = r.lo + by // still dirty
} // check point for `*r`: runtime check (CONTRACT_RUNTIME_CHECK)

fn widen_at(v: borrow mut Vec<Range16>, i: USize, by: U16) {
    v[i].hi = v[i].hi + by // check point 3 for the element, at the end of the statement
}

fn shift_at(v: borrow mut Vec<Range16>, i: USize, by: U16) {
    let e = borrow mut v[i]
    e.hi = e.hi + by // `*e` becomes dirty
    e.lo = e.lo + by // last use of `e`: check point 3 for `*e` after it
}

fn shifted(r: Range16, by: U16) -> Range16 {
    let lo = r.lo + by
    let hi = r.hi + by
    Range16(lo: lo, hi: hi) // check point: proven
}
```

- In `shift`, the check at the return is not proven: assignments produce no definition facts (D-CONTRACT-02, §9.6.3), so nothing is known about the new field values.
- In `widen_at` and `shift_at`, the element lives in the vector, which `v` does not own as a place of the function. Without check point 3, the element would stay broken in the vector after the borrow ends, where every later reader of `v` could observe it.
- In `shifted`, the immutable definitions `lo = r.lo + by` and `hi = r.hi + by` are facts. The additions panic on overflow, so when they complete, these equations hold over the integers. Together with the entry fact `r.lo <= r.hi`, they prove `lo <= hi`.

### 9.4.4 Invariants as facts

Because of the check points of §9.4.3, the invariant of `T` holds for every value of `T` that is not dirty. The SFC uses this as follows (N-C-52):

- For every place term of static type `T`, and for `*p` with `p` of type `borrow T` or `borrow mut T`, the invariant (with `self` replaced by that place) is a fact at the place's current version (§9.6.3). Examples are a parameter at entry, a local initialized by a call, a field read of type `T`, and a pattern binding of type `T`.
- An assignment inside `q`, or a `borrow mut` of a place inside `q`, kills the facts about `q` (§9.6.3). The invariant fact is therefore unavailable while `q` is dirty, and becomes available again after the check that makes `q` clean.

```sai
fn width(r: Range16) -> U16 {
    r.hi - r.lo // the underflow check MAY be removed: `r.lo <= r.hi` is a fact
}
```

In `width`, the subtraction `r.hi - r.lo` would panic on underflow if `r.lo > r.hi`. The invariant gives the SFC the fact `r.lo <= r.hi`. An implementation MAY use that fact to remove the overflow check (§9.7.2). Arithmetic checks are not SFC obligations, so they never affect acceptance.

### 9.4.5 Invariants in const initializers

A construction of a type with an invariant inside a const initializer (06 §6.27) MUST be proven by the SFC. Otherwise the error is `CONST_INVARIANT_NOT_PROVEN`, because a const has no runtime at which a check could run. The same holds for B→R conversions in consts, which are `REFINEMENT_NOT_PROVEN` as everywhere else.

## 9.5 Contracts, traits and generics

### 9.5.1 Trait methods

A method declared in a trait may carry `requires` and `ensures` clauses. They are part of the trait's contract and bind every impl (N-C-54):

1. **Callers.** A call resolved to the trait method `Tr.m`, either through a bound `T: Tr` in generic code or through a concrete impl, has the trait's `requires` as call-site obligations. After the call, the trait's `ensures` are facts. This holds for every impl, so it is sound in generic code.
2. **Impl methods.** In an impl method `impl Tr for S { fn m(..) .. }`:
   - The trait's `requires`, with the impl's parameter names substituted positionally, are facts at the impl method's entry.
   - The trait's `ensures` are obligations at each return point of the impl method, checked or proven like the method's own `ensures`. They have their own contract ids, those of the trait clauses.
   - The impl method MUST NOT declare `requires`: `CONTRACT_REQUIRES_ON_IMPL`. A stronger precondition would break callers that only know the trait (P8). The fix moves the clause to the trait, or turns it into an `assert` in the body.
   - The impl method MAY declare additional `ensures`. They are obligations at its return points like any `ensures`. They are facts only at calls statically resolved to this impl (calls on a concrete `S`), never at calls through a bound.
3. **Default methods** are checked once, generically, with `Self` abstract. An impl that does not override a default method inherits its body and its contract unchanged.
4. **Inherent methods** are ordinary functions for this chapter.

A clause in a trait method is resolved in the trait's scope, so the impl's privacy does not matter (§9.1.2 rule 6).

### 9.5.2 Generic functions

- The SFC decides the obligations of a generic function on its generic body, before monomorphization. Its results are complete when the function reaches the `Analyzed` phase (D-MIR-01). Each obligation in a generic function is decided once, for all instantiations, and the result is the same for every instantiation (N-C-55). Acceptance therefore never depends on which instantiations exist.
- In generic code, operators on values of a type parameter `T` resolve to trait methods and are opaque calls (§9.6.4). A comparison `x <= y` with `x, y : T` is therefore a propositional atom, not an integer constraint, even when `T` is later instantiated to `I32`.
- A runtime check inserted in a generic function exists in every instance. Monomorphization never removes or adds checks. The optimizer may remove checks in an instance under §9.7.2.

### 9.5.3 Function values and closures

- Closures, function-pointer types, and `Fn`/`FnMut`/`FnOnce` bounds carry no contracts. A closure body has no `requires` or `ensures`, and calling a function value creates no obligation at the call site.
- A function item with `requires` converted to a function value uses its checked entry (§9.3.3). Its `ensures` are still checked at its return points, but they are not facts at the indirect call site, because the call site does not know the callee.
- `async fn` (N-C-68): `requires` are checked at the call that creates the future, and `ensures` are checked when the body returns its output, before the future completes. `result` refers to the output value ([11](11-async-and-concurrency.md)).

## 9.6 The Standard Fact Checker

### 9.6.1 Role and results

The SFC maps an obligation, meaning its goal and its program point, to one of these results:

| Result | Meaning |
|---|---|
| `Proven` | the facts at the point, together with the negated goal, are unsatisfiable under §9.6.8 |
| `NotProven(Open)` | some case of §9.6.8 remained satisfiable over the rationals. The goal may still be true, because the SFC is incomplete |
| `NotProven(Limit)` | a limit of §9.6.8 was exceeded |
| `NotProven(Unsupported)` | the goal contains a float atom (§9.6.4) |

Every `NotProven` result has the consequence given in §9.1.1. Diagnostics name the reason, and for `Limit` they also name the limit that was exceeded.

- The SFC analyzes one **body** at a time: a function body, a closure body, an `async` block, or a const initializer. It runs in the Analyzed phase, after borrow checking and effect checking, on generic code (D-MIR-01, §9.5.2).
- It is specified here over source constructs and their evaluation order (06 §6.23). The mapping to MIR is in [impl/04](../impl/04-mir.md).
- A closure body or `async` block inherits no facts from the enclosing body. Its captured places are paths with fresh versions at its entry, which carry only the lazy facts of §9.6.3 (N-C-56).
- Each obligation is decided independently. No fact depends on whether another obligation was proven, so no result depends on another result.

### 9.6.2 Paths and versions

A **path** is a **root** followed by zero or more **projections**:

- **Roots:**
  - a parameter, including `self`
  - a local binding introduced by `let`, `var`, a pattern, or a `for` loop
  - `result`, which exists only in `ensures` goals
- **Projections:**
  - a record field `.f`
  - a tuple field `.N`
  - a dereference `*` of a `borrow`, `borrow mut` or `Box` value, including the dereferences inserted by auto-deref (06 §6.9.3)

  The accessor `.value` of a distinct type is not a projection. A path `d.value` denotes the same path as `d` (D-CONTRACT-02: `.value` is the identity).

A place that goes through indexing, a raw-pointer dereference, or a call result is not a path. Reading it gives a fresh atom (§9.6.4).

Each path has a **version** at each program point. The pair of a path and a version denotes one value. The version changes only at these **kill events** (N-C-57):

1. **Assignment.** `q = e` and `q op= e`, including the reinitialization of a moved place, where `q` is a path. The event kills `q`, every path that extends `q`, and every proper prefix of `q`.

   An assignment to a place that is not a path, such as `a[i] = e` or `a[i].f = e`, is treated as an assignment to its longest path prefix (`a`).
2. **Mutable borrow.** The creation of a `borrow mut q`, whether explicit, an auto-ref'd receiver, a `borrow mut` binding mode, or a capture by `borrow mut`. It kills the same set as an assignment to `q`. A two-phase borrow (07) kills again at its activation.
3. **Loop head.** At the head of a `loop`, `while` or `for`, every path that is killed anywhere in the loop, including in its condition and its nested loops, gets a fresh version.
4. **Join.** At a point with several incoming control-flow edges, a path whose version differs between the edges gets a fresh version.

Nothing else changes a version. Calls that take only shared borrows, moves, drops and `.await` leave versions unchanged, because safe SAI has no interior mutability ([07](07-ownership-and-borrowing.md)). Two `unsafe` code obligations make this sound in the presence of unsafe code ([12](12-unsafe-and-ffi.md), N-C-58):

- Unsafe code MUST NOT change the value of a path between two kill events of that path, except through a `borrow mut` created by a kill event. The places of an `Arena` are the one exception ([12](12-unsafe-and-ffi.md) §12.6.4 rule 5). The SFC never has a fact about them, because no pure function can read them.
- A pure function (empty effect set, [08](08-effects-and-capabilities.md)) MUST return equal results for equal arguments and equal values reachable from them.

Violating either is undefined behavior.

### 9.6.3 Facts

The **fact set** `F(s)` at a program point `s` is a finite set of normalized formulas (§9.6.5). It is computed by one forward pass over the body in evaluation order (N-C-62). Every dominating branch condition of D-CONTRACT-02 reaches `s` by this pass. A fact that holds on every incoming path of a join also reaches it:

1. **Entry.** `F` contains each `requires` clause of the current function, over the parameters at their entry versions. For an impl method this includes the requires of the trait method (§9.5.1). Bodies without contracts start empty.
2. **Definitions.** A `let x = e` or `let x: T = e` statement whose pattern is a single by-value identifier, where `x` has an integer type, `Bool`, `Byte`, `Char`, or a distinct or refined type over one of these, adds a fact:
   - **Integer types:** `x = ⟦e⟧` at the point after the statement.
   - **`Bool`:** `x` is instead **expanded**. In every formula built later, the atom for `x` at that version is replaced by `⟦e⟧`.

   In the same way, a match arm binding `n` (or `n @ P`) on a scrutinee whose term is an integer term defines `n = ⟦scrutinee⟧` in that arm. `var` declarations, assignments, `for` bindings and other patterns add no definitions (D-CONTRACT-02: definitions only for immutable `let`).
3. **Branches.** Facts are added on control-flow edges:
   - `if c` and `while c`: the true edge adds `⟦c⟧`, and the false edge (for `while`, the exit edge) adds `¬⟦c⟧`.
   - `a and b`: the evaluation of `b` has the additional fact `⟦a⟧`. `a or b`: the evaluation of `b` has `¬⟦a⟧`.
   - `match e`, where `⟦e⟧` is an integer term (§9.6.4). An **integer pattern** is built only from integer, byte and character literals, integer consts, ranges, `|`, `_`, identifier bindings and `@` bindings. For each arm `i`:
     - If the pattern of arm `i` is an integer pattern, the arm's guard and body get the fact `⟦e⟧ ∈ Pᵢ`. Each literal or const contributes an equality, each range two bounds, `|` a disjunction, and `_` or a binding `⊤`.
     - For every earlier arm `j < i` that has no guard and an integer pattern, arm `i` also gets `¬(⟦e⟧ ∈ Pⱼ)`.
     - Earlier arms with a guard contribute nothing.
     - If arm `i` has a guard `g`, its body also gets `⟦g⟧`.
   - `match e` with `e : Bool` and patterns `true`/`false`: the arm gets `⟦e⟧` or `¬⟦e⟧`. Patterns of other forms add no facts.
   - `if let`, `while let` and `let .. else` are treated as a `match` with one arm and a fallback. The `else` block of `let .. else` gets the negation of the pattern fact when the pattern is an integer pattern.
4. **Calls.** After a direct call returns, the `ensures` facts of §9.3.4 are added. For a trait method, these include the trait's ensures (§9.5.1).
5. **Assertions.** After `assert c`, `⟦c⟧` is a fact (06 §6.20.2), since execution continues only if `c` held.
6. **Joins.** At a join, `F` is the intersection of the incoming fact sets, where two facts are the same iff their normalized forms are identical (§9.6.5). An edge from a diverging expression does not exist (06 §6.26). After `if x < 0 { return 0 }`, the code therefore has `¬(x < 0)`.
7. **Loops.**
   - `F` at a loop head is the fact set of the edge entering the loop, with the versions of rule 3 of §9.6.2 applied. Back edges and `continue` edges contribute nothing (D-CONTRACT-02: no loop invariant inference).
   - The exit edge of `while` adds `¬⟦c⟧` to `F` at the head.
   - `break` edges join at the loop exit like any other edges.
   - The exit of `for` has `F` at the head.

A fact about an old version stays true and stays in `F`. It simply does not mention the current version, so it says nothing about the current value. This is how `old(..)` in `ensures` still sees entry facts.

**Lazy facts** are not stored in `F`. They are generated in §9.6.8 for each atom and path that becomes relevant (N-C-42, N-C-52):

- **Range facts.** For each integer atom of type `T`, `min(T) ≤ a ≤ max(T)`:
  - `USize` uses `[0, 2⁶⁴ − 1]` and `ISize` uses `[−2⁶³, 2⁶³ − 1]` on every target. These are supersets of the actual ranges, so the results are sound and independent of the target (N-C-59).
  - `Byte` uses `[0, 255]`, and `Char` uses `[0, 0x10FFFF]`.
  - A distinct or refined type uses the range of its integer base.
- **Refinement facts.** For each path, or call atom, of static type `R = B where p`, the instance `p[self := that term]`. The same applies to a distinct type with a refinement clause.
- **Invariant facts.** For each path `π` of a type `T` with an invariant, and for `*π` when `π : borrow T` or `π : borrow mut T`, the instance of each invariant element of `T` with `self` replaced by that path. The instance is generated:
  - for the current version, if the place is not dirty at the goal's point (§9.4.3)
  - for the entry version of a parameter

### 9.6.4 Terms

The translation `⟦e⟧` maps an expression, at the point where it is evaluated, to either:

- an **integer term**, a linear combination `c₀ + Σ cᵢ·aᵢ` with integer coefficients over integer atoms
- a **formula** (§9.6.5)
- a **value key**, used only inside the keys of atoms

The integer domain consists of all integer types, `Byte`, `Char`, and distinct and refined types over them. Integer arithmetic that completes without panicking is exact, because overflow panics (D-TYPE-06). A linear term therefore equals the mathematical value.

| Expression | `⟦e⟧` |
|---|---|
| integer, byte or character literal | its numeric value |
| `true`, `false` | `⊤`, `⊥` |
| path to a `const` of the integer domain or `Bool` | the const's value (06 §6.27) |
| path place `π` of the integer domain | the integer atom `(π, version)` |
| path place `π` of type `Bool` | the propositional atom `(π, version)`, or its expansion (§9.6.3 item 2) |
| path place `π` of any other type | the value key `(π, version)` |
| `D(e)`, `e.value`, a coercion R→B, `borrow e` and `*e` (when `⟦e⟧` is known) | `⟦e⟧` (D-CONTRACT-02) |
| `e1 + e2`, `e1 - e2`, `-e` in the integer domain | `⟦e1⟧ + ⟦e2⟧`, `⟦e1⟧ − ⟦e2⟧`, `−⟦e⟧` |
| `e1 * e2` where `⟦e1⟧` or `⟦e2⟧` is a constant | the other term scaled by the constant |
| derived `Add`, `Sub`, `Mul`, `Neg` of a distinct type over an integer base ([05](05-types.md) §5.12) | as the base operator |
| `T.from(x)` between integer types (lossless, D-TYPE-06) | `⟦x⟧` |
| any other integer operation: `*` of two non-constants, `/`, `%`, shifts, bitwise operators, wrapping operators, and calls not covered below | an **opaque atom** keyed by the operator and its operand terms (D-CONTRACT-02: non-linear terms are opaque) |
| `<`, `<=`, `>`, `>=`, `==`, `!=` on integer terms, including derived `PartialEq` and `PartialOrd` of distinct types over integer bases | a linear constraint (§9.6.5) |
| `==`, `!=` on `Bool` | equivalence, non-equivalence |
| `and`, `or`, `not` | `∧`, `∨`, `¬` |
| a call that satisfies the call-atom conditions below | a **call atom** |
| a comparison on other types (`PartialEq` or `PartialOrd`) whose operands have terms or value keys | a propositional **comparison atom** keyed by the resolved impl method and the operand keys |
| a block `{ let x = e1  e2 }` inside a predicate | `⟦e2⟧` with `x` replaced by `⟦e1⟧` |
| an expression of type `F32` or `F64`, or any expression containing one | a **float atom** |
| anything else, including `if` and `match` expressions used as values, indexing, and non-pure calls | a **fresh atom**, a new atom distinct from every other atom (facts may still relate it to others, for example a definition `y = ⟦f(x)⟧` or the `ensures` of `f`) |

A call `f(a1, …, an)` (including a method call) is a **call atom** (N-C-60) when all of the following hold:

1. `f` is statically resolved to a function item, an inherent method, or a trait method (with its generic arguments, possibly type parameters of the current body).
2. Its effect set at this call, including latent effects, is empty ([08](08-effects-and-capabilities.md)).
3. `f` is not an `unsafe fn` and not declared in an `extern` block.
4. No parameter of `f` has a type `borrow mut T` or `SliceMut<T>`.
5. Each argument has a term or a value key. A shared borrow `borrow π` has the value key of `π`.

The key of a call atom is the callee's canonical path, its generic arguments, and the keys of its arguments. Two call atoms with equal keys are the same atom. The result type of a call atom determines its kind: an integer atom, a propositional atom, or a value key (for example `s.trim()` in `s.trim().len()`).

`Byte` and `Char` values take part only in comparisons, since they have no arithmetic (D-TYPE-01). A comparison between `Char` values compares scalar values.

### 9.6.5 Formulas and normalization

Formulas are built from `⊤`, `⊥`, **linear constraints** `L ≤ 0` (where `L` is an integer term), propositional atoms, `¬`, `∧` and `∨`.

Comparisons translate as follows. This is the integer tightening of D-CONTRACT-02, and it is exact because every atom is integer-valued.

| Source | Formula |
|---|---|
| `a < b` | `a − b + 1 ≤ 0` |
| `a <= b` | `a − b ≤ 0` |
| `a > b` | `b − a + 1 ≤ 0` |
| `a >= b` | `b − a ≤ 0` |
| `a == b` | `a − b ≤ 0 ∧ b − a ≤ 0` |
| `a != b` | `a − b + 1 ≤ 0 ∨ b − a + 1 ≤ 0` |
| `p == q` (Bool) | `(p ∧ q) ∨ (¬p ∧ ¬q)` |
| `p != q` (Bool) | `(p ∧ ¬q) ∨ (¬p ∧ q)` |

**Normal form** (N-C-61):

1. **Negation normal form.** `¬` is pushed to propositional atoms by De Morgan's laws. `¬(L ≤ 0)` becomes `−L + 1 ≤ 0`, `¬⊤` becomes `⊥`, and `¬⊥` becomes `⊤`.
2. **Constraint normalization.** In `c₀ + Σ cᵢ·aᵢ ≤ 0`, like atoms are combined and zero coefficients dropped.
   - If no atom remains, the constraint is `⊤` when `c₀ ≤ 0` and `⊥` otherwise.
   - Otherwise, with `g = gcd(|cᵢ|)`, the constraint is replaced by `⌈c₀/g⌉ + Σ (cᵢ/g)·aᵢ ≤ 0`. This tightening is sound over the integers.
   - The atoms are listed in canonical order (§9.6.7).
3. **Simplification.** Nested `∧` and `∨` are flattened. `⊤` and `⊥` are absorbed. Duplicate operands are removed, and operands are sorted by the canonical formula order (§9.6.7). No other simplification is performed.

Two formulas are **identical** iff their normal forms are structurally equal.

### 9.6.6 Goals

| Obligation | Goal | Point |
|---|---|---|
| B→R conversion of `e`, or `D(e)` with a refinement `p` | `p[self := ⟦e⟧]` | after `e` is evaluated |
| `requires c` of `f` at a direct call | `c` with each parameter replaced by its argument (below) | after the arguments are evaluated |
| `ensures c` at a return point | `c` with `result` replaced by the returned value's term, each parameter at its current version, and each `old(e)` replaced by `⟦e⟧` over the entry versions | after the returned value is evaluated |
| invariant element `c` of `T` at a check point for the value `v` | `c[self := v]` | at the check point (§9.4.3) |

- **Substitution.** A parameter `x` is replaced by the argument's term or value key. For a parameter `p : borrow T` or `borrow mut T` with the argument `borrow π` or `borrow mut π`, the path `*p` is replaced by `π`. An argument without a term or key makes the parameter a fresh atom.
- **Construction terms.** In `self := T(f1: e1, …)` or a functional update `T(f1: e1, ..b)`, the projection `self.fᵢ` is `⟦eᵢ⟧`, and a field taken from `b` is the path `b.f`. A tuple or variant construction is treated in the same way with positional fields.
- Each clause, and each invariant element, is a separate goal. Its `and` operands are not split.
- A goal whose normal form is `⊤` is `Proven` without further work. A goal that contains a float atom is `NotProven(Unsupported)`. Facts that contain a float atom are never used.

### 9.6.7 Canonical order

The SFC's limits make results depend on the order of work, so the order is fixed (D-CONTRACT-02, N-C-63). All spans below are byte offsets in the source file of the current body.

**Atom keys** are compared lexicographically, first by kind (`path < opaque < call < comparison < fresh`) and then as follows:

| Kind | Key after the kind |
|---|---|
| path atom | the root's declaration span (start, end); the projection sequence (field index, tuple index, or `*` as −1); the version key |
| opaque atom | the operator's ordinal in the precedence table (06 §6.8.1); the operand keys |
| call atom, comparison atom | the callee's canonical path compared as bytes; the generic arguments in canonical type order ([05](05-types.md)); the argument keys |
| fresh atom | the expression's span (start, end); the evaluation index within the body |

- A **version key** is the span (start, end) of the event that created the version, followed by its index in evaluation order among the events with that span. Entry versions have the key `(0, 0, 0)`.
- The event spans are as follows:
  - assignment: the assignment expression
  - borrow: the borrow expression
  - loop head: the loop keyword
  - join: the whole `if`, `match`, `and` or `or` expression, or the statement after a `let .. else`
- `result` sorts after all parameters and locals.
- The **canonical formula order** compares the kind first (`⊥ < ⊤ < constraint < propositional atom < ¬ < ∧ < ∨`), then the operands. Constraints compare their atom lists, then the coefficients, then `c₀`.
- **Fact order.** Facts in `F` are ordered by the span of the construct that created them (start, end), then by their creation order within it. Lazy facts come after all facts in `F`, ordered by the key of the atom or path that generated them, then by range, refinement, invariant, then element index.
- **Variable order.** Integer atoms are eliminated in increasing atom-key order.

### 9.6.8 Decision procedure and limits

Given the goal `G` and the ordered fact list, the SFC performs these steps:

1. **Trivial goals.** If `G` is `⊤`, the result is `Proven`. If `G` contains a float atom, the result is `NotProven(Unsupported)`.
2. **Relevance.** The relevant set starts with the atoms and paths of `G`. A path is relevant when it is the path of a relevant atom or occurs in the key of one.
   - Repeat until nothing changes: every fact that mentions a relevant atom or path is **selected** and its atoms and paths become relevant, and the lazy facts (§9.6.3) of every relevant atom and path are generated and treated as facts.
   - Facts that are never selected do not take part.
3. **Limits.**
   - The number of distinct **atomic formulas**, meaning normalized constraints and propositional atoms in `¬G` and the selected facts, MUST NOT exceed **256**.
   - The number of distinct **integer variables**, meaning integer atoms in them, MUST NOT exceed **16**.

   Otherwise the result is `NotProven(Limit)` (D-CONTRACT-02). The limits are counted after relevance filtering, so facts unrelated to the goal never cause a limit to be exceeded (N-C-64).
4. **Case splitting.** The conjunct list is `[¬G]` followed by the selected facts in canonical order. Each conjunct, in negation normal form, is converted to a list of **cubes** (conjunctions of literals) by distributing `∧` over `∨`, operands left to right.
   - The current case list starts as one empty cube and is multiplied with each conjunct's cube list in turn, current cube major.
   - After each multiplication, a cube is removed if it contains `⊥`, or a propositional atom together with its negation. Duplicate literals and duplicate cubes are removed.
   - If any conjunct's own cube list, or the current list after a multiplication, has more than **16** cubes, the result is `NotProven(Limit)`.
   - If the list becomes empty, the result is `Proven`.
5. **Elimination.** Each remaining cube is processed in order. Its constraints form the set `S`.
   - If `S` contains `⊥`, the cube is unsatisfiable.
   - Otherwise, each integer variable `x` occurring in `S` is eliminated in canonical order. `S` is partitioned into:
     - `P`, the constraints with a positive coefficient `a` of `x`
     - `N`, the constraints with a negative coefficient `b` of `x`
     - `Z`, the rest

     The new set is `Z` together with, for each `p ∈ P` and `n ∈ N` (in order, `P` major), the normalized combination `|b|·p + a·n`. In each combination, `x` cancels.
   - After each elimination, if the set contains `⊥` the cube is unsatisfiable. `⊤` and duplicates are removed. If the set has more than **256** constraints, the result is `NotProven(Limit)`.
   - If all variables are eliminated without `⊥`, the cube is satisfiable and the result is `NotProven(Open)`.
6. If every cube is unsatisfiable, the result is `Proven`.

All arithmetic in steps 4 and 5 is exact, on unbounded integers. The tightening in each normalization is sound over the integers. Fourier–Motzkin with tightening remains incomplete for integers, and an incompleteness can only make the result `NotProven`.

### 9.6.9 Determinism and conformance

- The result of every obligation is a function of the current body's source and of the declarations it uses (signatures, contracts, types, invariants and consts) only. It does not depend on the target, the profile, optimization settings, compilation order, or other obligations' results (determinism principle, P8).
- An implementation MAY compute the result by any means that yields the identical result for every obligation. It MUST NOT prove more: accepting a B→R conversion that this procedure does not prove is non-conforming.
- The conformance suite ([impl/15](../impl/15-conformance-and-testing.md)) contains SFC cases at and beyond each limit.

## 9.7 Runtime checks

### 9.7.1 Check code and messages

- A runtime check is an `Assert` terminator of MIR whose kind is `Contract(id)`, `Invariant(id)` or `Refinement(id)` (D-MIR-01, [impl/04](../impl/04-mir.md)). It is placed at the point given in §9.3 and §9.4. It evaluates the predicate and panics if the result is `false`.
- Checks are present in every profile (D-CONTRACT-01, D-TGT-02). Profiles never change which checks exist (C-03).
- The panic message (N-C-65) is one of:

  | Kind | Message |
  |---|---|
  | `requires` | `precondition failed: <clause>` |
  | `ensures` | `postcondition failed: <clause>` |
  | invariant | `invariant failed: <Type>: <element>` |
  | refinement (trust boundary only) | `refinement failed: <Type>: <predicate>` |

  - `<clause>`, `<element>` and `<predicate>` are the predicate's source text in canonical formatting ([impl/14](../impl/14-formatter.md)), so they do not depend on the original layout.
  - `<Type>` is the type's canonical path.
  - The location is the one given in §9.3.3, §9.3.4, §9.4.3 or §9.7.3.
  - The way the message and location are reported, or discarded in the tiny profile, is defined in [10](10-errors-and-panics.md) §10.5.
- **Lint.** Each obligation that is `NotProven` and therefore checked at runtime is reported once by `CONTRACT_RUNTIME_CHECK` (info (lint), D-CONTRACT-01) at its site.
  - The message names the clause, its contract id, and the reason (`Open`, `Limit`, or `Unsupported`).
  - The lint is computed from the SFC results, before optimization. It is reported even if the optimizer later removes the check (§9.7.2).
  - It is subject to `@allow`, `@warn` and `@deny` ([15](15-attributes-and-lints.md)). The package policy `deny runtime_contracts` makes it an error in the package's own code ([package/01](../package/01-package-contract.md)).
  - Trust-boundary validation (§9.7.3), including the `requires` checks at the entry of an exported function, is not a fallback and is never reported by this lint. The obligations of the same clauses at direct calls from SAI code are reported as usual.

### 9.7.2 Removal of checks by the optimizer

- The optimizer MAY remove a runtime check only if it has a sound proof that, in every execution that reaches the check, the predicate evaluates to `true` without panicking (D-CONTRACT-01).
- The proof must be sound for the semantics of this specification: exact integer arithmetic for operations that did not panic, and the facts of §9.6.3, which are sound by construction. The optimizer MAY use the SFC, stronger provers (§9.8), or both.
- Removal is observable only in code size and running time. It changes neither diagnostics nor acceptance.
- The same holds for other checks that are not SFC obligations: arithmetic overflow, division by zero, bounds, and shifts. They may be removed only with a sound proof that they cannot fail ([impl/09](../impl/09-optimization-and-size.md)).

### 9.7.3 The trust boundary

Values that enter the program from outside were not produced by checked code, so their refinements and invariants are validated at runtime (D-CONTRACT-01). v0.1 has no way to mark external input as trusted (N-C-67).

1. **Component exports** ([13](13-components-and-wit.md) §13.13). When a function exposed through a world is called from outside the component:
   1. The arguments are lifted by the Canonical ABI ([impl/07](../impl/07-component-lowering.md)). A value that is not a valid value of its WIT type traps during lifting.
   2. Each lifted argument is validated in parameter order. A value is validated by first validating its components, depth first in declaration order: record fields, variant payloads, tuple elements, list elements, and `Option` and `Result` payloads. Then the invariant elements of its own type are validated in order. A failure panics with kind `Invariant(id)` (or `Refinement(id)` for a refined component), located at the exported function's declaration.
   3. Then each `requires` clause of the implementing function is checked at entry, in order, with panic kind `Contract(id)` (13 §13.13). Clauses that the SFC proves from the entry facts alone (range, refinement and invariant facts of the parameters) are not checked.
2. **Core exports.** An `export fn` (D-MOD-02) is called by the host. Its parameters are validated and its `requires` are checked at entry, in the same way.
3. **Imports.** The results of imported component functions and of `extern "wasm"` functions are validated in the same way after lifting, before the call expression produces its value: the calling function validates the result immediately after the call, before any other SAI code runs. The location is the call site. The `ensures` of an `extern` function are not checked (§9.3.1).

Every panic traps (D-ERR-01). A validation failure at an export therefore traps the instance, which realizes "a failure while lifting traps" (D-CONTRACT-01). Values returned by an export are not validated, because checked code produced them.

## 9.8 External provers

- An implementation MAY run provers other than the SFC, for example SMT solvers, on obligations and runtime checks (D-CONTRACT-02).
- Their results MUST NOT affect acceptance, the SFC results, any diagnostic (including `CONTRACT_RUNTIME_CHECK` and the `deny runtime_contracts` policy), or the semantics of the program. They may only justify removing a runtime check under §9.7.2.
- Output from such provers, for example a report that a check was removed, is informational tooling output outside the lint system (N-C-66). It has no diagnostic code and cannot be promoted by `@deny`, `@warn` or a policy. Builds with and without an external prover therefore accept the same programs and report the same diagnostics.

## 9.9 Worked examples

### 9.9.1 Distinct money (R-05)

```sai
@derive(Copy, Clone, PartialEq, PartialOrd, Sub)
distinct Money = I64

fn withdraw(balance: Money, amount: Money) -> Money
    requires amount <= balance
    ensures result >= Money(0)
{
    balance - amount
}
```

The `ensures` obligation at the tail expression is proven:

1. `Sub` and `PartialOrd` are derived, so `balance - amount` and both comparisons translate to their base operations, and `Money(0)` is `0` (§9.6.4).
2. The entry fact is `amount − balance ≤ 0`.
3. The goal is `0 − (balance − amount) ≤ 0`, so `¬G` is `balance − amount + 1 ≤ 0`.
4. `balance` is eliminated first (it is declared first). Combining `¬G` with the entry fact gives `1 ≤ 0`, which is `⊥`. The only cube is unsatisfiable, so the result is `Proven`.

At call sites:

```sai
fn pay(account: Money, price: Money) -> Option<Money> {
    if price <= account {
        return Some(withdraw(account, price)) // requires proven by the branch fact
    }
    None
}

fn pay_anyway(account: Money, price: Money) -> Money {
    withdraw(account, price) // CONTRACT_RUNTIME_CHECK: `amount <= balance` is checked here
}
```

### 9.9.2 An invariant

```sai
type Account {
    balance: Money
}

invariant Account {
    self.balance >= Money(0)
}

fn debit(account: Account, amount: Money) -> Account
    requires amount <= account.balance
{
    Account(balance: withdraw(account.balance, amount))
}
```

- The `requires` of `withdraw` at the call is proven from the entry fact of `debit`.
- The construction `Account(..)` is an invariant check point. Its goal is `w >= 0`, where `w` is the fresh atom of the call's value. The `ensures` of `withdraw` adds the fact `w ≥ 0` after the call, so the goal is proven.
- The return of `debit` is a check point for `result`. The returned value is that construction, so the goal is again `w ≥ 0`.
- The by-value parameter `account` is still initialized at the return, which makes it a check point too. Its goal follows from the invariant fact of its entry version, since `account` is never written.

In another module:

```sai
fn reset(account: borrow mut Account) {
    account.balance = Money(0)    // rejected: INVARIANT_FIELD_WRITE
}
```

### 9.9.3 Non-empty strings (R-06)

```sai
type NonEmptyStr = Str where self.len() > 0

fn non_empty(s: Str) -> Option<NonEmptyStr> {
    if s.len() == 0 {
        return None
    }
    Some(s)
}
```

- `s.len()` is a call atom `ℓ`, since `len` is pure and takes `s` by value or shared borrow.
- After the `if`, the fact is `¬(ℓ == 0)`, that is `ℓ + 1 ≤ 0 ∨ −ℓ + 1 ≤ 0`. The goal `ℓ > 0` gives `¬G = ℓ ≤ 0`. The range fact `−ℓ ≤ 0` comes from `USize`.
- There are two cubes:
  - `{ℓ ≤ 0, ℓ + 1 ≤ 0, −ℓ ≤ 0, …}` gives `1 ≤ 0`.
  - `{ℓ ≤ 0, −ℓ + 1 ≤ 0, …}` gives `1 ≤ 0`.

  Both are unsatisfiable, so the conversion is accepted.

### 9.9.4 No loop invariants

```sai
fn drain(start: Percentage) -> Percentage {
    var p: I32 = start
    while p > 0 {
        p = p - 1
    }
    p    // rejected: REFINEMENT_NOT_PROVEN (`p >= 0` would need a loop invariant)
}
```

`p` is assigned in the loop, so it has a fresh version at the loop head. At the exit the only fact about it is `¬(p > 0)`. The upper bound `p <= 100` follows, but `p >= 0` does not, so the result is `NotProven(Open)`. A conforming implementation rejects this program even if a stronger prover could show `p == 0` (§9.6.9). The fix is `Percentage.try_from(p)`, or a check such as `if p < 0 { return 0 }` before the tail.

## 9.10 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) leaves open. They are provisional until merged into it. IDs continue the `N-C` series of [06](06-expressions-and-statements.md) §6.28.

| ID | Rule | Rationale |
|---|---|---|
| N-C-37 | Every contract clause, refinement clause and invariant element has a contract id made of the declaring item's canonical path, the clause kind and its index, used in `AssertKind`, diagnostics and messages (§9.1.3). | P3 and P8: ids are stable under unrelated edits and identify obligations semantically. |
| N-C-38 | In a refinement predicate, `self` has the base type `B`, for both `type R = B where p` and `distinct D = B where p` (§9.2.1). | Literals never have a distinct type (D-TYPE-03), so `distinct Age = U8 where self <= 150` would not type-check with `self : Age`. |
| N-C-39 | Refinement erasure: any use of an `R` that needs a `B` sees the value as `B`. Results have base-operation types, method lookup ends with a base step that excludes `borrow mut Self` receivers (`REFINED_PLACE_MUTATION`), and `R` gets compiler-provided forwarding impls of `Copy`/`Clone` (iff `B: Copy`), `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format` (§9.2.2). | A refined value is a `B` that is known to satisfy `p` (D-TYPE-03). Forwarding only traits that return no fresh `Self` keeps the predicate intact (P9). |
| N-C-40 | `borrow R` coerces to `borrow B` at coercion sites. `borrow mut R` never coerces (§9.2.2). | D-TYPE-04 item 6 lists `borrow R` to `borrow B` together with R→B. A shared borrow cannot break `p`, and a mutable one could. |
| N-C-41 | `R.try_from(value: B) -> Option<R>` (and `D.try_from` for a refined distinct type) is compiler-provided, pure, and always a runtime test of `p` (§9.2.3). | D-TYPE-03 names `R.try_from` as the explicit validation. Its exact signature was not fixed. |
| N-C-42 | Every term of a refined static type contributes its instantiated predicate as a fact, wherever it is relevant (§9.2.4, §9.6.3). | D-CONTRACT-02 "predicates of refined-type variables in scope", extended uniformly to fields, call results and bindings (P8). |
| N-C-43 | Predicates are resolved, type-checked and privacy-checked in the scope of their declaration, independent of where their checks are placed (§9.1.2). | A caller-side `requires` check must not fail to compile because the clause mentions items private to the callee (P8). |
| N-C-44 | `ensures` on `extern` functions are trusted, never checked, and used as facts. Violating them is undefined behavior. `requires` on them are checked at call sites as usual (§9.3.1). | Foreign code has no return points SAI controls. Trusting declared postconditions is part of the `unsafe` FFI contract (P5). |
| N-C-45 | A function with an `ensures` clause MUST NOT have a parameter named `result` (`RESULT_NAME_CONFLICT`) (§9.3.2). | `result` is contextual (02 §2.5.2). A parameter named `result` would make an `ensures` clause ambiguous to readers (P1). |
| N-C-46 | `old(e)`: one operand naming only parameters, no nesting, no `result`, the type MUST be `Copy`, and it is evaluated at entry only if some clause using it needs a runtime check (§9.3.2). | Copy-only snapshots never allocate or hold borrows across the body (P8, P9). Evaluating it only when needed keeps proven contracts free. |
| N-C-47 | A function with `requires` that is used as a function value goes through a checked entry that checks all of its `requires` at entry. Direct calls never do (§9.3.3). | Indirect callers have no facts, and D-CONTRACT-01 still requires a runtime check (P2). |
| N-C-48 | An `ensures` clause that mentions a by-value parameter that may be moved at a return point is `ENSURES_USES_MOVED_PARAMETER` (§9.3.4). | Checking it would read moved memory. `old(..)` provides the snapshot instead (P9). |
| N-C-49 | Invariants are allowed only on non-generic record, enum, resource and distinct types (`INVARIANT_ON_GENERIC_TYPE`, `INVARIANT_ON_INVALID_TYPE`), at most one per type (`DUPLICATE_INVARIANT`) (§9.4.1). | The grammar names no generic parameters (03 §3.8). Per-instantiation checks would make acceptance depend on monomorphization. |
| N-C-50 | Construction of a type with an invariant is not a field write. It is allowed wherever visibility allows it and is always a check point (§9.4.2). | D-CONTRACT-04 lists every construction expression as a check point, which presupposes constructions outside the module. |
| N-C-51 | Return check points cover exactly parameters of type `T` and `borrow mut T`, and results of type `T`. Values of `T` nested in other types are not covered by this rule (§9.4.3). | D-CONTRACT-04 taken literally. Nested values are covered by N-C-53. |
| N-C-52 | Invariant facts are generated for every clean place of an invariant type and for entry versions of parameters (§9.4.4, §9.6.3). | D-CONTRACT-02 "invariants of values whose types have invariants", made sound by N-C-53. |
| N-C-53 | Dirty places. Inside the defining module, a place of an invariant type becomes dirty at field writes, at field `borrow mut`s, and at whole `borrow mut`s that are not direct call arguments. A dirty place is checked before each whole use and, if rooted in a parameter or capture, at each return point (§9.4.3). A place reached through a `borrow mut` or `SliceMut` held by a local or temporary is also checked before a move of that local and after its last use (N-C-85). | Without these check points, the check points of D-CONTRACT-04 let a value with a broken invariant escape through locals and nested places. That would make invariant facts, and every optimization based on them, unsound (P9). |
| N-C-54 | Trait method contracts bind every impl. An impl method MUST NOT add `requires` (`CONTRACT_REQUIRES_ON_IMPL`). It may add `ensures`, which are facts only at calls statically resolved to that impl (§9.5.1). | Behavioral subtyping. Generic callers rely only on the trait (P8). |
| N-C-55 | The SFC runs once per generic body before monomorphization. Its results hold for all instances, and monomorphization never adds or removes checks (§9.5.2). | Determinism principle: acceptance never depends on the set of instantiations. |
| N-C-56 | Closure bodies and `async` blocks inherit no facts from the enclosing body. Their captures are fresh paths with only lazy facts (§9.6.1). | Closures may run later, after the enclosing facts have been invalidated (P9). |
| N-C-57 | SFC paths are roots with field, tuple and dereference projections (`.value` is transparent). Versions change only at assignments, `borrow mut` creation (and two-phase activation), loop heads and joins. Assignments to non-path places kill the longest path prefix (§9.6.2). | Makes D-CONTRACT-02's "facts about a `var` are invalidated at assignments and at loop heads" precise and covers writes through `borrow mut` (P8). |
| N-C-58 | Unsafe code MUST NOT change a path's value between its kill events except through a `borrow mut` created at a kill event (the fields of an `Arena` excepted, [12](12-unsafe-and-ffi.md) §12.6.4 rule 5). Pure functions MUST be deterministic in their arguments and the memory reachable from them. Violations are undefined behavior (§9.6.2). | The SFC's facts are sound only under these assumptions. They belong to the `unsafe` contract of [12](12-unsafe-and-ffi.md) (P5). |
| N-C-59 | Range facts for `USize` and `ISize` use the 64-bit ranges on every target, and `Char` uses `[0, 0x10FFFF]` (§9.6.3). | Both are sound supersets on wasm32 and wasm64, so SFC results are target-independent (P4, P8). |
| N-C-60 | A pure, statically resolved call with no `borrow mut`/`SliceMut` parameters, outside `unsafe fn` and `extern`, whose arguments have terms or keys, is a call atom identified by callee, generic arguments and argument keys (§9.6.4). | Needed for R-06 (`self.len() > 0`). Equal keys mean equal values for pure functions (N-C-58). |
| N-C-61 | Normal form: NNF, integer tightening of strict and negated constraints, gcd normalization with ceiling of the constant, canonical sorting, and no other simplification. Formulas are identical iff their normal forms are equal (§9.6.5). | Fact-set intersection and deduplication must be syntactic and implementation-independent (P8). |
| N-C-62 | Facts are computed by a single forward pass with intersection at joins. A loop head takes only the facts of the entering edge, with fresh versions for paths killed in the loop. Branch facts come from `if`, `while`, `and`/`or`, Bool patterns, and integer patterns of `match`, `if let`, `while let` and `let .. else` (§9.6.3). | Contains every dominating branch condition of D-CONTRACT-02. It is deterministic and needs no fixpoint (no loop invariant inference). |
| N-C-63 | The canonical order of atoms, versions, formulas and facts is defined over source spans, canonical paths and structural keys (§9.6.7). | D-CONTRACT-02 requires a fixed elimination order. Limits make results order-dependent (P8). |
| N-C-64 | The limits count distinct atomic formulas (256) and distinct integer variables (16) after relevance filtering, cubes after each multiplication (16), and constraints after each elimination step (256) (§9.6.8). | Interprets D-CONTRACT-02's "256 atoms, 16 variables, 16 DNF branches". Counting after filtering keeps unrelated facts from causing `Limit`. |
| N-C-65 | Runtime contract failures panic with the messages `precondition failed: …`, `postcondition failed: …`, `invariant failed: <Type>: …` and `refinement failed: <Type>: …`, using canonical predicate text (§9.7.1). | Messages are reproducible and independent of source layout (P8). |
| N-C-66 | External prover results are informational output without diagnostic codes. They cannot be promoted by attributes or policies (§9.8). | D-CONTRACT-02: acceptance depends only on the SFC. Promoting prover output would reintroduce prover-dependent builds. |
| N-C-67 | Trust-boundary validation covers component export arguments (depth first, then `requires`), core `export fn` arguments, and results of component imports and `extern "wasm"` functions. v0.1 has no trusted marker (§9.7.3). | D-CONTRACT-01's trust boundary made concrete, consistent with [13](13-components-and-wit.md) §13.13. |
| N-C-68 | For `async fn`, `requires` are checked when the future is created and `ensures` when the body produces its output. `result` is the output value (§9.5.3). | The call site is where the caller's facts exist, and the output is what the caller receives (P8). |
| N-C-85 | Borrowed places. A dirty place reached through the `Deref` of a `borrow mut` or `SliceMut` that a local variable or temporary holds (not a parameter or capture) is not cleaned when the holder ends. It is checked before each move of and assignment to the holder, and immediately after the holder's last use (§9.4.3 item 4, check point 3). | Without this rule, `v[i].f = x` through trait-based indexing, or a field write through a returned `borrow mut T`, would leave a broken value inside a container after the borrow ends, where no check point sees it (P9). The end of use-liveness is a property of the MIR that does not depend on region inference, and a check there does not extend the borrow that `r` holds (P8). |

## 9.11 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `CONST_INVARIANT_NOT_PROVEN` | error | a construction of a type with an invariant in a const initializer that the SFC does not prove (§9.4.5) |
| `CONTRACT_REQUIRES_ON_IMPL` | error | a `requires` clause on a trait impl method (§9.5.1) |
| `CONTRACT_RUNTIME_CHECK` | info (lint) | an obligation that is not proven and is checked at runtime; named by D-CONTRACT-01, defined here (§9.7.1) |
| `DUPLICATE_INVARIANT` | error | a second `invariant` declaration for the same type (§9.4.1) |
| `EFFECTFUL_PREDICATE` | error | defined in [08](08-effects-and-capabilities.md) §8.11 |
| `ENSURES_USES_MOVED_PARAMETER` | error | an `ensures` clause mentions a by-value parameter that may be moved at a return point (§9.3.4) |
| `EXTERN_ENSURES_REQUIRES_UNSAFE` | error | defined in [12](12-unsafe-and-ffi.md) §12.7.3; an `ensures` clause on a function of an `extern "wasm"` block without `unsafe` (§9.3.1) |
| `GENERIC_REFINED_TYPE` | error | defined in [05](05-types.md) §5.6.4 |
| `INVALID_CONTRACT_EXPRESSION` | error | a predicate contains `return`, `break`, `continue`, `?`, `.await`, an assignment, `var`, `unsafe`, `region` or a closure (§9.1.2) |
| `INVARIANT_FIELD_WRITE` | error | a field of a type with an invariant is written or mutably borrowed outside its defining module (§9.4.2) |
| `INVARIANT_ON_GENERIC_TYPE` | error | an `invariant` on a type with generic parameters (§9.4.1) |
| `INVARIANT_ON_INVALID_TYPE` | error | an `invariant` on a refined type, alias, primitive or interface type (§9.4.1) |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | defined in [03](03-grammar.md) §3.8 |
| `MOVE_OUT_OF_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.17 |
| `NESTED_OLD` | error | `old(..)` inside another `old(..)` (§9.3.2) |
| `OLD_OUTSIDE_ENSURES` | error | `old(..)` outside an `ensures` clause that does not resolve to a function (§9.3.2) |
| `OLD_REFERS_TO_RESULT` | error | the operand of `old(..)` mentions `result` (§9.3.2) |
| `OLD_REQUIRES_COPY` | error | the operand of `old(..)` has a type that is not `Copy` (§9.3.2) |
| `OVERLAPPING_IMPLS` | error | defined in [05](05-types.md) §5.10.2 |
| `REFINED_BASE_IS_REFINED` | error | defined in [05](05-types.md) §5.6.4 |
| `REFINED_PLACE_MUTATION` | error | a base method with a `borrow mut Self` receiver selected on a refined place (§9.2.2) |
| `REFINEMENT_NOT_PROVEN` | error | a B→R conversion, or a refined distinct construction `D(e)`, that the SFC does not prove (§9.2.3) |
| `RESULT_NAME_CONFLICT` | error | a parameter named `result` in a function with an `ensures` clause (§9.3.2) |
| `RESULT_OUTSIDE_ENSURES` | error | `result` outside an `ensures` clause that does not resolve to a binding (§9.3.2) |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14 |
| `UNRESOLVED_NAME` | error | defined in [04](04-modules-and-names.md) §4.5.1 |
