# 10. Errors and Panics

**Status:** Normative
**Stage:** 1 (recoverable errors, the `?` operator, panics, traps, panic reporting, `@panic_hook`); 3 (the panic kinds of contracts, refinements and invariants, [09](09-contracts-and-refinements.md)); 5 (`e.await?`, [11](11-async-and-concurrency.md))
**Related:** [decisions.md](../design/decisions.md) D-ERR-01, D-EXPR-01, D-EXPR-05, D-EXPR-06, D-MIR-01, D-EFF-03, D-CONTRACT-01, D-UNSAFE-01, D-SYN-09, D-WASM-02, D-WASM-05, D-TGT-01, D-TGT-02, D-CAP-02, D-ASYNC-01; [draft-reconciliation.md](../design/draft-reconciliation.md) R-03, C-03; [01](01-introduction.md), [02](02-lexical-structure.md), [03](03-grammar.md), [05](05-types.md), [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [09](09-contracts-and-refinements.md), [11](11-async-and-concurrency.md), [12](12-unsafe-and-ffi.md), [13](13-components-and-wit.md), [14](14-testing.md), [15](15-attributes-and-lints.md), [16](16-core-library.md), [impl/04](../impl/04-mir.md), [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/07](../impl/07-component-lowering.md), [impl/10](../impl/10-runtime.md), [targets](../targets/targets-and-profiles.md)

This chapter defines how SAI programs fail. It covers recoverable errors, which are ordinary `Result` and `Option` values; the `?` operator, which propagates them; panics, which are defined behavior that ends the instance with a trap; the sources of panics and their kinds, messages and locations; panic reporting in each profile, including the `@panic_hook` import; traps that are not panics; and the behavior of failures at component and core-module boundaries. The types `Result` and `Option` are declared in [05](05-types.md) and their library surface in [16](16-core-library.md). The grammar of `?` is in [03](03-grammar.md) §3.13.2. The individual checks that panic are defined where their operation is defined: arithmetic, indexing and `assert` in [06](06-expressions-and-statements.md), contracts in [09](09-contracts-and-refinements.md), library functions in [16](16-core-library.md) and async executors in [11](11-async-and-concurrency.md). The runtime code that formats and delivers reports is in [impl/10](../impl/10-runtime.md) §10.5, and its lowering in [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.8.12.

---

## 10.1 Overview

### 10.1.1 Three kinds of failure

A SAI program can fail in exactly three ways (N-C-69):

| Kind | What it is | Who handles it | Defined in |
|---|---|---|---|
| **Recoverable error** | a value: `Err(e)` of a `Result<T, E>`, or `None` of an `Option<T>` | the program, by `match`, `?` or library methods | §10.2, §10.3 |
| **Panic** | a defined event that stops the program: a failed runtime check, an explicit `panic`, an exhausted resource | nobody inside the instance. It is reported if a channel exists, then the instance traps | §10.4, §10.5, §10.6 |
| **Plain trap** | a Wasm trap that is not a panic: a failed boundary value check, an engine limit, a trap in the host | nobody inside the instance. It is never reported by SAI | §10.7 |

- Recoverable errors are the only failures that SAI code can observe and handle. There are no exceptions (D-ERR-01, §10.2.3).
- A panic and a plain trap both end the current execution of the instance by a Wasm trap (01 §1.5.1). They differ only in that a panic has a kind, a message and, except for the resource panics of §10.4.3, a location, which are reported in the dev and release profiles when a reporting channel exists (§10.5).
- A panic is defined behavior (01 §1.5.1 item 2). The specification fixes exactly when a panic occurs. Profiles never change which panics occur (C-03, D-TGT-02).
- Undefined behavior ([12](12-unsafe-and-ffi.md)) is none of the three. It has no meaning, and it may or may not show up as a trap.

### 10.1.2 Terms

- A **panic kind** is one of the MIR `AssertKind`s of D-MIR-01 (`Overflow(op)`, `DivByZero`, `RemByZero`, `BoundsCheck`, `ShiftOverflow`, `Contract(id)`, `Refinement(id)`, `Invariant(id)`, `Unwrap`, `UserAssert`, `ExplicitPanic`), or one of the two resource kinds `StackOverflow` and `OutOfMemory` of §10.4.3. "Panics with kind `K`" (06 §6.1.4) means that the operation raises a panic of kind `K`.
- The **panic message** is the text of §10.4.4. It is a UTF-8 string.
- The **panic location** is a source position (§10.4.5). Every panic has one except the resource panics of §10.4.3.
- A **panic report** is the text that carries the message and, when available, the location to the host (§10.5.2).
- A **reporting channel** is the means by which a report reaches the host: WASI standard error or the `@panic_hook` import (§10.5.3).
- The **panic mode** of a build is `Report` or `TrapOnly` (§10.5.3).

### 10.1.3 Stages

| Feature | Stage |
|---|---|
| `Result` and `Option` as error values, `?`, panics of the built-in checks, `assert`, `panic`, `unreachable`, panic reporting, `@panic_hook` | 1 |
| panics of kinds `Contract`, `Refinement` and `Invariant` | 3 (with [09](09-contracts-and-refinements.md)) |
| `e.await?` | 5 (with [11](11-async-and-concurrency.md)) |

A feature used before its stage is `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md) §1.4.5).

---

## 10.2 Recoverable errors

### 10.2.1 `Result` and `Option`

Recoverable errors are values of the core enums `Result<T, E>` and `Option<T>` (D-ERR-01). They are declared in [05](05-types.md) §5.4.5 as ordinary generic enums with the variants `Ok(T)`, `Err(E)`, `Some(T)` and `None`. Nothing about them is special except:

- the prelude binds `Result`, `Ok`, `Err`, `Option`, `Some` and `None` ([16](16-core-library.md) §16.4);
- they are the lang items used by the `?` operator (§10.3) and by other desugarings ([16](16-core-library.md) §16.3.2);
- `Result` is `@must_use` (§10.2.2).

A function that can fail in a way its caller is expected to handle SHOULD return `Result<T, E>`, with an error type `E` that describes the failure. A function whose only failure is "no value" SHOULD return `Option<T>`. Any type can be an error type. An enum with one variant per failure is the usual choice, because callers can `match` on it exhaustively:

```sai
enum AgeError {
    Negative
    TooLarge(I32)
}

fn check_age(years: I32) -> Result<I32, AgeError> {
    if years < 0 {
        return Err(.Negative)
    }
    if years > 150 {
        return Err(.TooLarge(years))
    }
    Ok(years)
}
```

`.Negative` is an implicit member whose type is known from the expected type `AgeError` ([06](06-expressions-and-statements.md)).

### 10.2.2 Results must be used

`Result` is marked `@must_use`, `Option` is not (D-EXPR-01, [05](05-types.md)). Discarding a `Result` in a statement position is the error `UNUSED_MUST_USE`, defined with its exact rule in [06](06-expressions-and-statements.md) §6.25.2 and [15](15-attributes-and-lints.md) §15.5. An error therefore cannot be ignored by accident. It can be ignored on purpose with `let _ = e`, propagated with `?`, or handled with `match`:

```sai
enum LogError {
    Full
}

type Log {
    count: U32
}

fn append(log: borrow mut Log) -> Result<(), LogError> {
    if log.count == 100 {
        return Err(.Full)
    }
    log.count += 1
    Ok(())
}

fn append_three(log: borrow mut Log) -> Result<(), LogError> {
    append(log)                   // rejected: UNUSED_MUST_USE (the Result is discarded)
    let _ = append(log)           // accepted: discarded on purpose
    append(log)?                  // accepted: propagated
    Ok(())
}
```

Each argument `log` is reborrowed, not moved, because the parameter type is `borrow mut Log` ([07](07-ownership-and-borrowing.md) §7.5.5).

### 10.2.3 No exceptions, no null

- SAI has no exceptions. `try`, `catch` and `throw` are not keywords. Using them is the error `NO_EXCEPTIONS` ([02](02-lexical-structure.md) §2.5.3), whose help suggests `Result` and `?`.
- SAI has no null. `null`, `nil` and `undefined` are the error `NO_NULL` ([02](02-lexical-structure.md) §2.5.3), whose help suggests `Option` and `None`.
- No construct of the language catches a panic or a trap (§10.4.1). A failure that a caller must be able to handle MUST therefore be expressed as a `Result` or an `Option`.

### 10.2.4 Errors versus panics

The choice between returning an error and panicking belongs to the API designer. The following rules are normative for the core library ([16](16-core-library.md)) and are recommended for every package:

1. A failure that depends on input the caller does not control (data read from the host, user input, a network response, a parse of untrusted text) SHOULD be a `Result` or an `Option`.
2. A violated precondition, which is a bug in the caller, SHOULD be a `requires` clause ([09](09-contracts-and-refinements.md)) or a panic. A `requires` clause is preferred, because the SFC may prove it at compile time and the contract documents it.
3. A state that the author believes impossible SHOULD be `unreachable()` (§10.4.2) rather than a silently wrong value.
4. A function that panics SHOULD document the condition. In the core library, each function's *Panics* column does so ([16](16-core-library.md) §16.1.2).

Errors and panics never convert into each other implicitly. `Option.unwrap` and `Result.unwrap` turn an error value into a panic explicitly ([16](16-core-library.md)). Nothing turns a panic into an error value (§10.4.1).

### 10.2.5 Converting between error types

`?` never converts error types (D-EXPR-05, §10.3.2). There is no `From`-based conversion and no implicit coercion of error values (P1). A function that calls operations with different error types converts each error explicitly, usually with `Result.map_err` ([16](16-core-library.md)) before `?` (R-03):

```sai
enum FormError {
    MissingAge
    BadAge(AgeError)
}

fn age_field(value: Option<I32>) -> Result<I32, FormError> {
    let years = match value {
        Some(v) => v
        None => return Err(.MissingAge)
    }
    let age = check_age(years).map_err(fn(e) { FormError.BadAge(e) })?
    Ok(age)
}
```

The closure `fn(e) { FormError.BadAge(e) }` returns `FormError`, so `map_err` produces `Result<I32, FormError>`, whose error type is identical to that of the function. When the original error carries no useful information, the closure ignores it, as in R-03: `.map_err(fn(_) { UserError.Network })?`.

---

## 10.3 The `?` operator

### 10.3.1 Syntax

`e?` is a postfix expression ([03](03-grammar.md) §3.13.2, `PostfixOp`). It binds as tightly as a method call and associates to the left, so `a.b()?.c()?` applies the first `?` to `a.b()` and the second to `a.b()?.c()`, and `f(x).await?` applies `?` to the awaited value ([11](11-async-and-concurrency.md)). `?` takes no operand on its right. A `?` at the start of a line never continues the previous line, because `?` is not in the continuation set ([02](02-lexical-structure.md) §2.10.1).

### 10.3.2 Typing

`?` is typed against `Γ.ret`, the return type of the innermost enclosing function body, closure body or `async` block ([06](06-expressions-and-statements.md) §6.1.1). For an `async fn` it is the declared output type, not the future type ([11](11-async-and-concurrency.md)).

```text
(T-Try-Result)
Γ ⊢ e ⇒ Result<T, E> ! E1        Γ.ret = Result<U, E>
──────────────────────────────────────────────────────
Γ ⊢ e? ⇒ T ! E1

(T-Try-Option)
Γ ⊢ e ⇒ Option<T> ! E1           Γ.ret = Option<U>
──────────────────────────────────────────────────────
Γ ⊢ e? ⇒ T ! E1
```

The rules are applied as follows (D-EXPR-05):

1. **The operand.** The head of the operand's type MUST be known at the `?` ([05](05-types.md) §5.16.4); if it is still an inference variable, the error is `TYPE_ANNOTATIONS_NEEDED` ([05](05-types.md)). The head MUST be the lang item `Result` or `Option` ([16](16-core-library.md) §16.3.2). Any other type, including `borrow Result<T, E>`, `borrow Option<T>` and a distinct type whose base is a `Result`, is the error `TRY_INVALID_OPERAND`. Borrows are not dereferenced automatically. The help for a borrowed operand suggests `.as_ref()` or `match`.
2. **The enclosing body.** If `Γ.ret` is an inference variable (a closure or `async` block whose return type is not yet known, [06](06-expressions-and-statements.md) §6.17.2), it is unified with `Result<β, E>` for an operand `Result<T, E>`, or with `Option<β>` for an operand `Option<T>`, where `β` is a fresh inference variable (N-C-70). The tail expression and every `return` operand of the body then determine `β`.
3. **The kind of the body.** Otherwise the head of `Γ.ret` MUST be the head of the operand's type.
   - If `Γ.ret` is neither a `Result` nor an `Option` (for example `()`, `I32`, `Never` or `process.ExitCode`), the error is `TRY_INVALID_RETURN_TYPE`. It names the enclosing body and its return type.
   - If the operand is a `Result` and `Γ.ret` is an `Option`, or the reverse, the error is `TRY_OPTION_RESULT_MIX`. `Option` and `Result` are never mixed (D-EXPR-05). The help suggests an explicit conversion with the library methods of [16](16-core-library.md) §16.7: `.ok_or(err)?` or `.ok_or_else(f)?` to use an `Option` in a `Result` body, and `.ok()?` to use a `Result` in an `Option` body when the error can be discarded.
4. **Identical error types.** For a `Result` operand, the error type `E` of the operand and the error type of `Γ.ret` MUST be the same type (N-C-71). Otherwise the error is `TRY_ERROR_TYPE_MISMATCH`. Its help suggests `.map_err(..)` before the `?` (§10.2.5).
   - "The same type" is type equality after inference ([05](05-types.md) §5.16.3). Transparent aliases are equal to their targets. Distinct types and refined types are different from their bases (D-TYPE-03). Two instantiations of one generic type are equal only if their arguments are equal.
   - No coercion applies to the error type. In particular `Result<T, Never>` in a body returning `Result<U, E>` is `TRY_ERROR_TYPE_MISMATCH` unless `E` is `Never`. The coercions of D-TYPE-04 apply to values at coercion sites, never to type arguments ([05](05-types.md) §5.15).
   - If either error type still contains inference variables, the two types are unified ([05](05-types.md) §5.16.3). If the unification fails, the error is `TRY_ERROR_TYPE_MISMATCH`, not `TYPE_MISMATCH`.
5. **The value types are independent.** `T` and `U` are unrelated. `?` checks only the kind and the error type.
6. **Effects.** `e?` has the effects of `e`. The test and the early return add none ([08](08-effects-and-capabilities.md)).
7. **Where `?` is not allowed.** `?` is an early return, so it is rejected wherever `return` is:
   - in a refinement clause, a contract clause or an invariant element, as `INVALID_CONTRACT_EXPRESSION` ([09](09-contracts-and-refinements.md) §9.1.2);
   - in a const initializer, as `NOT_CONST_EXPRESSION` ([06](06-expressions-and-statements.md) §6.27).

When several of these errors apply to one `?`, only the first in the order of the list above is reported, so that each `?` gets one diagnostic.

```sai
enum NetError {
    Timeout
}

fn read_count(raw: Option<I32>) -> Result<I32, NetError> {
    let n = raw?                  // rejected: TRY_OPTION_RESULT_MIX (an Option in a Result body)
    let m = raw.ok_or(NetError.Timeout)?     // accepted
    Ok(m)
}

fn total(a: Result<I32, NetError>) -> I32 {
    a? + 1                        // rejected: TRY_INVALID_RETURN_TYPE (the body returns I32)
}

fn checked(years: I32) -> Result<I32, NetError> {
    let age = check_age(years)?   // rejected: TRY_ERROR_TYPE_MISMATCH (AgeError, expected NetError)
    Ok(age)
}
```

### 10.3.3 Evaluation

`e?` evaluates as the following `match`, where `Ok`, `Err`, `Some` and `None` denote the lang-item variants by identity, whatever names are in scope, and `return` returns from the body that `Γ.ret` belongs to (N-C-72):

```text
e?   on Result   ≡   match e { Ok(v) => v,  Err(x) => return Err(x) }
e?   on Option   ≡   match e { Some(v) => v,  None => return None }
```

1. `e` is evaluated once. The operand is a value context, so a place operand of a non-`Copy` type is moved ([07](07-ownership-and-borrowing.md)), and a `Copy` operand is copied.
2. The discriminant is tested.
3. **Success.** The payload `v` is moved out of the operand and is the value of `e?`. The rest of the operand is empty and has no drop obligation.
4. **Failure.** For a `Result`, the payload `x` is moved into a new `Err(x)` of type `Γ.ret`. For an `Option`, a new `None` of type `Γ.ret` is built. That value is returned exactly as `return` returns it ([06](06-expressions-and-statements.md) §6.16.5), in this order:
   1. the unproven `ensures` clauses and invariant checks of the function run at this return point, as [09](09-contracts-and-refinements.md) §9.3.4 and §9.4.3 define. A postcondition therefore has to hold for the `Err` or `None` result too;
   2. the temporaries of the enclosing statements and the live locals of every exited block are dropped in the order of [07](07-ownership-and-borrowing.md) (D-OWN-05);
   3. control transfers: a function or closure returns, and an `async fn` or `async` block completes its future with the value ([11](11-async-and-concurrency.md)).
5. `e?` is not a diverging expression. Only the failure path leaves, so the expression has type `T` and the code after it is reachable ([06](06-expressions-and-statements.md) §6.26).

The order of evaluation and drops is the one listed for `e?` in [06](06-expressions-and-statements.md) §6.23. In MIR, `?` is a `SwitchInt` on the discriminant, a `Downcast` and a move of the payload on each branch, and on the failure branch an assignment to the return place followed by the ordinary return path with its drops and checks ([impl/04](../impl/04-mir.md)).

```sai
fn sum_ages(a: I32, b: I32) -> Result<I32, AgeError>
    ensures match result { Ok(total) => total >= 0, Err(_) => true }
{
    // On failure, `?` returns `Err(e)`, and the ensures clause is checked with that result.
    let x = check_age(a)?
    let y = check_age(b)?
    Ok(x + y)
}
```

### 10.3.4 `?` in closures and `async` code

- In a closure, `?` returns from the closure, not from the enclosing function ([06](06-expressions-and-statements.md) §6.17). If the closure's return type is not annotated and has no expected type, N-C-70 determines its kind from the first `?` checked in the body.
- In an `async fn`, `?` completes the future. Awaiting and propagating are written `e.await?` (D-EXPR-05). `e?.await` is valid only if the operand of `?` is a `Result` or `Option` whose payload is a future.
- In an `async` block, `?` completes the block's future. The block's output type is inferred as for a closure ([11](11-async-and-concurrency.md) §11.2.3), so N-C-70 applies.

```sai
fn ages(raw: Slice<I32>) -> Result<I32, AgeError> {
    let check_pair = fn(a: I32, b: I32) {
        let x = check_age(a)? // the closure's return type becomes Result<β, AgeError>
        let y = check_age(b)?
        Ok(x + y) // β = I32
    }
    let first = check_pair(raw[0], raw[1])?
    Ok(first)
}
```

The indexing `raw[0]` and `raw[1]` panics with kind `BoundsCheck` if `raw` is too short (§10.4.2). That is a panic, not an error value, which is why a caller that cannot rule it out SHOULD check `raw.len()` first.

The `async` form (Stage 5) awaits first and propagates second:

```sai
enum LoadError {
    Missing
}

async fn load(key: U32) -> Result<I32, LoadError> {
    if key == 0 {
        return Err(.Missing)
    }
    Ok(42)
}

async fn load_pair(a: U32, b: U32) -> Result<I32, LoadError> {
    let x = load(a).await? // awaits the future, then propagates an Err
    let y = load(b).await?
    Ok(x + y)
}
```

`load(a)?` without `.await` is `TRY_INVALID_OPERAND`, because the operand is a future, not a `Result`.

### 10.3.5 Diagnostics and fixes of `?`

| Code | Fix |
|---|---|
| `TRY_INVALID_OPERAND` | none in general. For `borrow Result<T, E>` or `borrow Option<T>`, the help shows `.as_ref()` and the `match` form |
| `TRY_INVALID_RETURN_TYPE` | a structured fix that changes the declared return type `R` of a named function to `Result<R, E>` (with the operand's `E`) or `Option<R>`. It is offered only when every `return` operand and the tail would still type-check after wrapping them in `Ok` or `Some`, and the fix wraps them too |
| `TRY_OPTION_RESULT_MIX` | the help shows `.ok_or(..)?`, `.ok_or_else(..)?` or `.ok()?` (§10.3.2 item 3). There is no structured fix, because the error value is the user's choice |
| `TRY_ERROR_TYPE_MISMATCH` | the help shows `.map_err(fn(e) { .. })?` with both error types named. A structured fix is offered only when the target error type is an enum in which exactly one variant is a tuple variant whose only payload type is the source error type: it inserts `.map_err(fn(e) { Target.Variant(e) })` before the `?` |

The structured fixes are deterministic: they depend only on the declarations involved (N-C-73).

---

## 10.4 Panics

### 10.4.1 What a panic is

A panic is raised when a check of §10.4.2 fails or when `panic` or `unreachable` is called. It is defined behavior (01 §1.5.1). It has a kind, a message and, except for the resource panics of §10.4.3, a location. When a panic is raised, the following happens, in this order (D-ERR-01, N-C-74):

1. **No further SAI code runs in the instance.** There is no unwinding (D-MIR-01: `Call` and `Drop` have no cleanup edges). In particular:
   - no value is dropped and no `Drop` impl runs, including the drops of locals, temporaries and the captures of closures ([07](07-ownership-and-borrowing.md));
   - no `ensures` clause or invariant check point runs ([09](09-contracts-and-refinements.md));
   - no other task, future or closure is polled or called ([11](11-async-and-concurrency.md));
   - no code of the program runs to build or deliver the report (§10.5.2).
2. **The panic is reported**, if the build's panic mode is `Report` (§10.5).
3. **The instance traps.** The current call into the instance ends with a Wasm trap (§10.7).

Consequences:

- A panic cannot be caught, recovered from or converted into a value by SAI code. It ends the whole instance, not only the current function or task (13 §13.13, [11](11-async-and-concurrency.md)).
- Effects already performed stay performed. Data written to a file or sent on a socket before the panic is not rolled back. Data held only in the instance, for example in a buffered writer that has not been flushed, is lost. Code that must deliver such data SHOULD deliver it before any operation that can panic, or return a `Result` instead of panicking.
- A panic is not an effect (D-EFF-03, [08](08-effects-and-capabilities.md) §8.1). A function whose effect set is empty can panic. Neither the check that panics nor the reporting adds anything to any function's effect set or to any package's required capabilities (§10.6.4).
- Whether a panic occurs, and its kind, message and location, are a deterministic function of the program and its inputs. Profiles never change them (C-03, D-TGT-02), with the single exception of the resource panics of §10.4.3. Profiles change only whether the panic is reported (§10.5.3).

`panic(msg: Str) -> Never` and `unreachable() -> Never` are core functions (D-EXPR-06, [16](16-core-library.md) §16.7.3). Their result type `Never` makes a call usable in any position ([06](06-expressions-and-statements.md) §6.26):

```sai
enum Mode {
    Fast
    Safe
}

fn mode_from(code: U8) -> Mode {
    match code {
        0 => .Fast
        1 => .Safe
        _ => panic("mode_from: unknown mode code")
    }
}

fn halve(n: I32) -> I32
    requires n % 2 == 0
{
    if n % 2 != 0 {
        unreachable() // the requires clause rules this out
    }
    n / 2
}
```

A message held in a `String`, for example an f-string (Stage 3), is passed with `.as_str()` ([16](16-core-library.md) §16.7.3): `panic(f"unknown mode {code}".as_str())`. The argument of `panic` is evaluated like any other argument, in every profile (§10.5.3).

### 10.4.2 Sources of panics

The following table is the complete list of language-level panic sources. The library adds the panics listed in the *Panics* column of each item of [16](16-core-library.md).

| Source | Kind | Defined in |
|---|---|---|
| integer `+`, `-`, `*` and their compound assignments, when the exact result does not fit | `Overflow(Add)`, `Overflow(Sub)`, `Overflow(Mul)` | [06](06-expressions-and-statements.md) §6.8 |
| signed `MIN / -1` | `Overflow(Div)` | 06 §6.8 |
| signed `-MIN` | `Overflow(Neg)` | 06 §6.8 |
| integer `/` and `%` by zero | `DivByZero`, `RemByZero` | 06 §6.8 |
| `<<` and `>>` with a shift amount not less than the bit width | `ShiftOverflow` | 06 §6.8 |
| built-in indexing of `Array`, `Slice` and `SliceMut` out of range | `BoundsCheck` | 06 §6.7.1 |
| a failed `assert` | `UserAssert` | 06 §6.20.2 |
| a call of `panic` or `unreachable` | `ExplicitPanic` | [16](16-core-library.md) §16.7.3 |
| `unwrap`, `expect`, `unwrap_err` and `expect_err` of `Option` and `Result` on the wrong variant | `Unwrap` | 16 §16.7 |
| an unproven `requires` or `ensures` clause that evaluates to `false` | `Contract(id)` | [09](09-contracts-and-refinements.md) §9.3 |
| an invariant check that evaluates to `false` | `Invariant(id)` | 09 §9.4.3 |
| a refinement that fails its validation at the trust boundary | `Refinement(id)` | 09 §9.7.3 |
| polling a completed anonymous future; `block_on` that can make no progress | `ExplicitPanic` | [11](11-async-and-concurrency.md) §11.3.4, §11.9.1 |
| exhaustion of the shadow stack | `StackOverflow` | §10.4.3 |
| failure of the global allocator | `OutOfMemory` | §10.4.3 |

Every check in this table is performed in every profile (D-TGT-02, D-CONTRACT-01). The optimizer MAY remove a check only with a sound proof that it cannot fail ([09](09-contracts-and-refinements.md) §9.7.2). Removing a check that can fail is non-conforming.

A conversion from a base type to a refined type never panics. If it is not proven, it is a compile error, and `R.try_from` returns an `Option` instead (D-CONTRACT-01, 09 §9.2.3).

### 10.4.3 Resource panics

Two panics are raised by the runtime rather than by a check in the program's code (N-C-69):

| Kind | When | Message | Location |
|---|---|---|---|
| `StackOverflow` | a function prologue finds that its frame does not fit in the shadow stack (D-WASM-02) | `stack overflow` | none |
| `OutOfMemory` | the global allocator cannot satisfy a request, because `memory.grow` fails or the requested size is not representable ([impl/10](../impl/10-runtime.md) §10.2.6). This includes the chunk requests of arenas (D-OWN-07) | `out of memory` | none |

- They are panics: they are reported like any other panic in `Report` mode, and then the instance traps ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.6).
- They have no location, because they are detected in shared runtime code that does not know the source position. Their report contains the message only (§10.5.2).
- The call depth and allocation volume at which they occur are implementation-defined (01 §1.5.2 item 3) and MAY differ between profiles, because frame sizes and allocation patterns depend on optimization. They are therefore the only panics whose occurrence a profile may change. They are resource limits, not part of the meaning of a program (C-03). A program SHOULD NOT depend on reaching them or on avoiding them at an exact depth or size.
- Running out of the engine's own call stack, which happens for deep recursion of functions without a shadow-stack frame, is a plain trap, not a panic (§10.7).
- Allocation failure is never returned as an error value in v0.1. The library functions with the `alloc` effect panic with `OutOfMemory` instead ([16](16-core-library.md) §16.1.2 item 5).

### 10.4.4 Panic messages

The message of each panic is fixed (N-C-76):

| Kind | Message |
|---|---|
| `Overflow(Add)` | `integer overflow in addition` |
| `Overflow(Sub)` | `integer overflow in subtraction` |
| `Overflow(Mul)` | `integer overflow in multiplication` |
| `Overflow(Div)` | `integer overflow in division` |
| `Overflow(Neg)` | `integer overflow in negation` |
| `DivByZero` | `division by zero` |
| `RemByZero` | `remainder by zero` |
| `ShiftOverflow` | `shift amount out of range` |
| `BoundsCheck` | `index out of bounds: index <i>, length <n>`, where `<i>` and `<n>` are the index and the length in decimal |
| `UserAssert` | the message operand, or `assertion failed: <condition>` without one ([06](06-expressions-and-statements.md) §6.20.2) |
| `ExplicitPanic` | the argument of `panic`, or `entered unreachable code` for `unreachable` ([16](16-core-library.md) §16.7.3) |
| `Unwrap` | the message given by the library item ([16](16-core-library.md) §16.7) |
| `Contract(id)`, `Invariant(id)`, `Refinement(id)` | the messages of [09](09-contracts-and-refinements.md) §9.7.1 (N-C-65) |
| `StackOverflow`, `OutOfMemory` | §10.4.3 |

- A library item whose *Panics* column names a kind but no message uses the message of that kind from this table. For example, `pow` panics with `integer overflow in multiplication` ([16](16-core-library.md) §16.8.2).
- The message texts are part of the language. A conforming implementation produces them byte for byte, so that tests and tools can match them. The framing around them is implementation-defined (§10.5.2).
- A message is UTF-8, because it is a `Str` or is built from fixed text and decimal digits. It is never truncated.

### 10.4.5 Panic locations

A **location** is the package that contains the source file, the path of the file relative to that package's root (01 §1.6 item 5), and a line and column. Lines and columns are 1-based, and columns count UTF-8 bytes ([02](02-lexical-structure.md) §2.1 item 6). The position of each panic is (N-C-75):

| Panic | Position |
|---|---|
| a binary operator, including a compound assignment | the first character of the operator token (`+`, `+=`, `/`, `<<`, …) |
| a prefix `-` | the `-` token |
| built-in indexing `e[i]` | the `[` token |
| `assert` | the `assert` keyword ([06](06-expressions-and-statements.md) §6.20.2) |
| a call: `panic`, `unreachable`, a panic raised in a library body, a failed `requires` check | the **position of the call**: for a method call `r.m(…)`, the method name `m`; for any other call, the first token of the callee expression |
| an `ensures` or invariant check at a return point | the `return` keyword, or the first token of the tail expression, or the `?` of an early return by `?` ([09](09-contracts-and-refinements.md) §9.3.4) |
| an invariant check after a construction | the first token of the construction expression (09 §9.4.3) |
| an invariant check of a dirty place before a whole use | the first token of the expression that makes the use (09 §9.4.3 item 4) |
| an invariant check of a borrowed place before a move of its holder, before an assignment to it, or after its last use | the first token of the expression that makes that use of the holder (09 §9.4.3 item 4, check point 3) |
| a validation at the trust boundary | the name of the exported function, or the position of the call for an import (09 §9.7.3) |
| `StackOverflow`, `OutOfMemory` | none (§10.4.3) |

- The position of a call refines "the call expression" of [16](16-core-library.md) §16.1.4 and [09](09-contracts-and-refinements.md) §9.3.3. In a chain spread over several lines, the method name identifies the line of the failing call.
- **Library bodies.** A panic raised inside `sai:core` or `sai:alloc` takes the position of the call through which control entered the library from the innermost frame outside it (16 §16.1.4).
- **Compiler-generated bodies.** Derived impls ([05](05-types.md) §5.12), drop glue and boundary glue have no source of their own. A panic raised in them takes the position of the innermost user-written construct that invoked them: the operator, call or construction that used a derived impl, the scope end that dropped the value, or the exported function. The checked entry of a function with `requires` clauses ([09](09-contracts-and-refinements.md) §9.3.3) is invoked through a function value, which passes no position, so its checks are located at the function's name in its declaration (09 N-C-47). The checked entry of a library function is part of the library for the rule on library bodies.
- **Function pointers.** A function pointer has only its declared signature, so a call through it passes no position. When a function whose panics take the position of the call that invoked it (a library function, a method of a derived impl, or the checked entry of a library function) is converted to a function pointer ([05](05-types.md) §5.15.2), those panics take the position of the converted function item expression instead (N-F-81). A function pointer to a user-written function needs no such rule, because the panics of its body are located in that body.
- **Generic and inlined code.** The location is the position in the source of the body that contains the check, whatever the instantiation. Inlining and other optimizations never change a location (C-03).
- **Closures and `async` blocks** report positions in their own bodies. A panic inside a closure that a library function calls belongs to the closure (16 §16.1.4).

```sai
fn first_two(xs: Slice<I32>) -> I32 {
    let a = xs[0] // BoundsCheck at the `[`
    let b = xs[1]
    a + b // Overflow(Add) at the `+`
}

fn scaled(values: Slice<I32>, factor: I32) -> I32 {
    let first = values
        .first()
        .copied()
        .unwrap() // Unwrap at `unwrap`, on this line
    first * factor // Overflow(Mul) at the `*`
}
```

`Slice.first` returns `Option<borrow I32>` and `copied` turns it into `Option<I32>` ([16](16-core-library.md)). The lines that start with `.` continue the statement ([02](02-lexical-structure.md) §2.10.1).

### 10.4.6 Panics at compile time

Constant initializers are evaluated by the compiler. An initializer whose evaluation would panic (overflow, division by zero, shift overflow) is the compile-time error `CONST_EVAL_PANIC` ([06](06-expressions-and-statements.md) §6.27). It is never a runtime panic.

Outside const initializers, a panic that is certain to happen is not a compile error. Code after a call of `panic` or `unreachable` is reported as `UNREACHABLE_CODE` (warning (lint), 06 §6.26.2), because the call has type `Never`. Acceptance never depends on whether the compiler can see that some other check will fail (determinism principle).

---

## 10.5 Panic reporting

### 10.5.1 Principle

Reporting is the only part of a panic that depends on the build. Whether a panic occurs, its kind, its message and its location never depend on the profile or the target (C-03, §10.4.1). What a build decides is whether the report reaches the host before the trap (D-ERR-01, D-TGT-02).

### 10.5.2 The report

A **panic report** is one UTF-8 text produced by the runtime ([impl/10](../impl/10-runtime.md) §10.5) (N-C-77).

1. It contains the panic message (§10.4.4) and, if the panic has one, the location (§10.4.5) with its package, relative file path, line and column. The surrounding framing is implementation-defined (01 §1.5.2 item 6). The format of the reference implementation is in [impl/10](../impl/10-runtime.md) §10.5.
2. It contains no absolute path of the build machine and no address (01 §1.6 item 5). For a given compiler version, target and program, the report of a given panic is the same in the dev and release profiles.
3. Building and delivering the report runs no code of the program: no `Format` or `Drop` impl, no closure, no library code that the program could replace. It does not call the global allocator, so `OutOfMemory` can be reported like any other panic.
4. The report is delivered once, through the build's reporting channel (§10.5.3), and then the instance traps.

### 10.5.3 Panic modes and reporting channels

Each build has one panic mode (N-C-78, consistent with [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.8.12.1):

| Mode | When | A panic is |
|---|---|---|
| `Report` | the profile is `dev` or `release`, **and** the build has a reporting channel | the report through that channel, then a trap |
| `TrapOnly` | the profile is `tiny` (D-TGT-02), or the build has no reporting channel | a trap only |

The reporting channels are exactly:

| Artifact | Channel | Condition |
|---|---|---|
| a component (`wasm32-wasi`, `wasm64-wasi`, or `sai component build`, D-TGT-01) | the standard error stream of `wasi:cli/stderr` | the component already imports `wasi:cli/stderr`, because reachable code of the program uses standard error ([impl/07](../impl/07-component-lowering.md) §7.11.8, N-D-59) |
| a core module (`wasm32-core`, `wasm64-core`) | the `@panic_hook` import | the root package declares one (§10.6) |

- **Reporting never adds an import or a capability.** A component that does not otherwise use standard error, and a core module without a `@panic_hook`, have no channel. They trap without a report in every profile. This keeps the imports of an artifact, which are its capability requirements, independent of whether its code can panic (P5, [08](08-effects-and-capabilities.md)).
- A program that wants panic reports in a component build therefore uses standard error somewhere in its reachable code, with the capability that this requires ([08](08-effects-and-capabilities.md)). A core-module program declares a `@panic_hook`.
- In `TrapOnly` mode no panic site data, message text, formatter or reporting code is linked into the artifact ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.8.12.1). This is what keeps a pure `export fn add(a: I32, b: I32) -> I32 { a + b }` on `wasm32-core` free of runtime code (draft §2.3).
- If writing the report to standard error fails, the instance traps without retrying.

**Evaluation is the same in every mode** (N-C-84). The argument of `panic`, the `msg` argument of `expect` and `expect_err`, and every other operand are evaluated as the program says, in every profile and mode, because they are ordinary arguments ([06](06-expressions-and-statements.md) §6.23). Any effect they perform happens before the panic, whatever the mode. The message operand of `assert` is evaluated only when the assertion fails, also in every mode ([06](06-expressions-and-statements.md) §6.20.2). An implementation MAY omit the computation of a message in `TrapOnly` mode only when the omission is unobservable, as for any optimization ([impl/09](../impl/09-optimization-and-size.md)).

---

## 10.6 `@panic_hook`

### 10.6.1 Declaration

`@panic_hook` (D-SYN-09) marks a core Wasm import as the reporting channel of a core-module build (D-ERR-01). Rules 1 and 2 are the general attribute checks of [15](15-attributes-and-lints.md) §15.2.4 (its entry is §15.14). Rules 3 to 5 are the attribute-specific checks of that list's step 8, defined here. The rules are (N-C-79):

1. **Arguments.** The attribute takes no arguments. `@panic_hook(..)` is `INVALID_ATTRIBUTE_ARGUMENT` ([15](15-attributes-and-lints.md)).
2. **Target.** It is allowed only on a function declared in an `extern "wasm"` block ([03](03-grammar.md) §3.10, [12](12-unsafe-and-ffi.md)). On any other declaration, including a function with a body and an item of an `unsafe extern "C"` block, it is `INVALID_ATTRIBUTE_TARGET` ([15](15-attributes-and-lints.md)). Repeating it on one function is `DUPLICATE_ATTRIBUTE` (15).
3. **Signature.** The hook is first checked like every function of an `extern "wasm"` block ([12](12-unsafe-and-ffi.md) §12.7.4): boundary-scalar types (`EXPORT_FN_NON_SCALAR`), no `async`, generic parameters, `self`, `where` or `borrows` (`INVALID_EXTERN_SIGNATURE`), and an `effects` clause, if written, of exactly `{ host }` (`EXTERN_EFFECTS_MISMATCH`, [08](08-effects-and-capabilities.md) §8.6.1). A hook that passes those checks MUST in addition have exactly two parameters, both of type `USize`, and no result type (its result is `()`). It MUST NOT be an `unsafe fn` and MUST NOT have a `requires` or `ensures` clause. Otherwise the declaration is `PANIC_HOOK_SIGNATURE_MISMATCH`. A declaration that already fails a check of [12](12-unsafe-and-ffi.md) §12.7.4 is not also reported with this code. An alias of `USize` is `USize` ([05](05-types.md)), but a distinct type based on `USize` is not. The parameter names are free. The first parameter receives the address of the report and the second its length in bytes. `USize` lowers to the address type of the target, `i32` on wasm32 and `i64` on wasm64 ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7).
4. **Root package only.** The function MUST be declared in the root package of the build. A `@panic_hook` in any other package is `PANIC_HOOK_OUTSIDE_ROOT`. How a program reports its panics is a decision of the program, not of a library (P5). The check is made per build: a package that declares a hook is accepted when it is the root of the build, and rejected when it is built as a dependency.
5. **At most one.** A build has at most one `@panic_hook`. Every declaration after the first is `DUPLICATE_PANIC_HOOK`. "First" is by file path in byte order, then by position in the file, as in the diagnostic order of [01](01-introduction.md) §1.6 item 4, so the diagnostic is deterministic.
6. **Import names.** The import's module name and field name are those of every `extern "wasm"` function: the string after `module`, and the function name or the string of `@import_name` ([12](12-unsafe-and-ffi.md) §12.7.6).

```sai
extern "wasm" module "env" {
    @panic_hook
    fn report_panic(ptr: USize, len: USize)
}
```

```sai
extern "wasm" module "env" {
    @panic_hook
    fn report_code(code: I32)             // rejected: PANIC_HOOK_SIGNATURE_MISMATCH
}

@panic_hook                               // rejected: INVALID_ATTRIBUTE_TARGET (not in an extern "wasm" block)
fn local_hook(ptr: USize, len: USize) {
}
```

### 10.6.2 Behavior

1. **When it is used.** The hook is the reporting channel only when the artifact is a core module and the profile is `dev` or `release` (§10.5.3). In a `tiny` build and in a component build it is never called. A component imports only WIT interfaces ([08](08-effects-and-capabilities.md) §8.12.7, [13](13-components-and-wit.md) §13.6.4), so it cannot import the hook. The declaration is not an error there, so that one package can be built for every target. Only a reachable call of the hook from SAI code (§10.6.3) is `TARGET_CAPABILITY_UNAVAILABLE` in a component build ([08](08-effects-and-capabilities.md) §8.12.7).
2. **Presence in the artifact.** The hook is an import of the core module exactly when the panic mode is `Report`, or when reachable SAI code also calls the function (§10.6.3, [08](08-effects-and-capabilities.md) §8.12.3) (N-C-80). A `tiny` build of a program whose code never calls the hook therefore does not import it, and its host does not need to supply it.
3. **The call.** When a panic is raised in `Report` mode, the runtime places the report (§10.5.2) in linear memory and calls the hook with the report's address and its length in bytes. The bytes are valid UTF-8. They can be read only during the call, through the module's exported memory, which is named `memory` ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7). A host that wants to keep the text copies it before returning.
4. **After the call.** When the hook returns, the instance traps (§10.4.1). If the hook itself traps, that trap ends the instance instead. Either way the host observes a trap from the call that panicked.
5. **No re-entry** (N-C-82). The host MUST NOT call into the instance while the hook runs. This is an exception to the re-entry permission of [12](12-unsafe-and-ffi.md) §12.7.2 item 4: the frames of the panicking code are still live, and their state may violate invariants and borrows that the new call would rely on. Such a call has no meaning defined by this specification. As in every call of a safe import, the host MUST NOT write to linear memory during the call ([12](12-unsafe-and-ffi.md) §12.7.2 item 2). As a safeguard, a panic raised while a report is in progress, for whatever reason, traps immediately without a second report and without calling the hook again.

A host written in JavaScript might supply the hook as `env.report_panic(ptr, len)`, decoding `len` bytes at `ptr` of `instance.exports.memory` with a UTF-8 decoder and logging them. This is informative. The host's behavior is outside the language.

### 10.6.3 Calling the hook from SAI code

The function marked `@panic_hook` remains an ordinary function of its `extern "wasm"` block. SAI code MAY call it like any other `extern "wasm"` function. Such a call is not a panic. It has the effect `host` and needs the `host` capability (D-UNSAFE-01, [08](08-effects-and-capabilities.md)).

### 10.6.4 Effects and capabilities

The call of the hook by the panic machinery is not a call made by any SAI body (N-C-81):

- It adds no effect to any function. Panics are not effects (D-EFF-03, [08](08-effects-and-capabilities.md) §8.1.1).
- It adds no capability to any package's requirements ([08](08-effects-and-capabilities.md) §8.12.4). Declaring a hook does not require the `host` capability. Only calls of the hook from SAI code do (§10.6.3).
- The hook function is a root of reachability ([08](08-effects-and-capabilities.md) §8.12.3). Because it is an import, it has no body and contributes no effects or capabilities of its own.

This is the core-module counterpart of N-D-59: reporting a panic never changes what the program is allowed to do.


---

## 10.7 Plain traps

### 10.7.1 Sources

A **plain trap** is a trap that is not a panic (§10.1.1). It has no kind, message or location and is never reported by SAI, in any profile. The sources are:

| Source | Defined in |
|---|---|
| Canonical ABI lifting or lowering failure at a component boundary: a `char` that is not a scalar value, a discriminant out of range, a `string` that is not valid UTF-8, an invalid handle, a pointer or length outside linear memory, misalignment | [13](13-components-and-wit.md) §13.13, [impl/07](../impl/07-component-lowering.md) |
| a boundary-scalar value check at a core `export fn` or a result of an `extern "wasm"` function that fails: a `Bool` other than 0 or 1, a `Char` that is not a scalar value, an `I8`, `I16`, `U8`, `U16` or `Byte` that is not correctly extended | [12](12-unsafe-and-ffi.md) §12.7.4 item 4, §12.8 item 5; [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7 (N-F-12) |
| exhaustion of the engine's call stack | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.6 |
| a trap raised by the host during a call of an import, including the `@panic_hook` | [12](12-unsafe-and-ffi.md) §12.7.2 item 5, §10.6.2 |
| a trap inside another component instance that a call of an import reaches | [13](13-components-and-wit.md) §13.13 |
| a nested panic during a report | §10.6.2 item 5 |
| a Wasm trap caused by undefined behavior in unsafe code, for example an out-of-bounds memory access through a raw pointer | [12](12-unsafe-and-ffi.md) §12.6 |

- Undefined behavior has no defined result, so a trap is only one of its possible outcomes. Safe code never causes one (P9).
- A trap that comes from the host or from another instance is outside the program. This specification only fixes that SAI code does not run after it.
- A panic in `TrapOnly` mode (§10.5.3) cannot be told apart from a plain trap by the host. It is still a panic: it occurs at the same point and for the same reason as in `Report` mode.
- The value checks of boundary scalars are plain traps because the value has no SAI type yet. The validation of refinements, invariants and `requires` at the trust boundary is checked SAI code, and a failure there is a panic ([09](09-contracts-and-refinements.md) §9.7.3).

### 10.7.2 After a trap

Every trap ends the execution of the instance (D-ERR-01). Whether a trap comes from a panic or not, the following holds:

1. **No SAI code runs** in the instance after the trap: no drops, no `ensures` or invariant checks, no other task (§10.4.1).
2. **Component instances.** A component instance that has trapped cannot be used again. The host decides what happens to the rest of the component graph ([13](13-components-and-wit.md) §13.13).
3. **Core module instances** (N-C-83). The Wasm engine may allow a host to call the exports of a core module instance again after a trap. A host MUST NOT do so. Nothing in the instance is restored by a trap: the shadow-stack pointer `__sp` keeps the value it had ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.6), the allocator's state may be partially updated ([impl/10](../impl/10-runtime.md)), and values in linear memory may break invariants or borrows. This specification gives such a call no meaning. A host that needs to continue creates a new instance.

---

## 10.8 Failures at the boundaries

This section collects how errors, panics and traps cross the boundary of an artifact. The rules are defined in the chapters cited.

### 10.8.1 Component exports

1. A function exposed through the world of a component reports recoverable errors by returning a `Result` whose error type is declared in the interface. It crosses the boundary as a WIT `result` ([13](13-components-and-wit.md)). This is the only way for an error to reach a client.
2. A panic inside an export, or in any code that it calls, traps the instance. It is never converted into a WIT `result` or any other value, and it does not unwind ([13](13-components-and-wit.md) §13.13, D-ERR-01). Panics are not part of the interface of a function, and no WIT type describes them.
3. Arguments are lifted first. A lifting failure is a plain trap (§10.7.1). The lifted arguments are then validated against refinements and invariants, and the `requires` clauses are checked. A failure is a panic of kind `Invariant`, `Refinement` or `Contract` ([09](09-contracts-and-refinements.md) §9.7.3), reported like any other panic when the build has a reporting channel.

### 10.8.2 Core exports

1. The signature of an exported `export fn` contains only boundary scalars ([12](12-unsafe-and-ffi.md) §12.8 item 1). `Result` and `Option` cannot cross a core boundary (`EXPORT_FN_NON_SCALAR`). A core-module program that reports errors to its host therefore encodes them in scalars, for example as a status code, by a convention outside the language.
2. A host argument that is not a valid value of its type is a plain trap before the body runs. A failed refinement, invariant or `requires` check on the arguments is a panic ([12](12-unsafe-and-ffi.md) §12.8 item 5, [09](09-contracts-and-refinements.md) §9.7.3).
3. A panic in the export traps, after the report through the `@panic_hook` if the build has one (§10.6).

### 10.8.3 Imports

1. A trap inside an import, whether raised by the host, by another component instance or by a failed lowering of the result, ends the calling instance. SAI code cannot catch it ([13](13-components-and-wit.md) §13.13, [12](12-unsafe-and-ffi.md) §12.7.2 item 5).
2. An imported component function whose WIT type returns a `result` returns a SAI `Result`, which is a recoverable error and works with `?` (§10.3) like every other `Result`.
3. Results of imports are validated against refinements and invariants after lifting. A failure is a panic located at the call site ([09](09-contracts-and-refinements.md) §9.7.3 item 3).

### 10.8.4 Async code

A panic in any task, in any future that a combinator polls, or in the executor itself (for example `block_on: deadlock`) traps the whole instance ([11](11-async-and-concurrency.md) §11.7.8). It does not reach the `JoinHandle` of the task, it is not caught by a scope, and sibling tasks do not run afterwards. Errors between tasks are `Result` values, passed with `e.await?` (§10.3.4).

---

## 10.9 Panics in tests

1. A test whose execution panics fails. The panic's message and, when available, its location are the failure reported for that test. How the runner selects, runs and reports tests is defined in [14](14-testing.md).
2. A panic traps the instance that runs the test (§10.4.1), and a trapped instance is never used again (§10.7.2). A runner that continues with other tests after a panic therefore runs them in a new instance.
3. The panic mode of a test build follows §10.5.3 for the profile and target of that build ([impl/13](../impl/13-cli.md) §13.6.6). Under `tiny` a panic is a trap only, so the failure of the test has no message (D-TGT-02). The reporting channel that the test harness provides is defined in [14](14-testing.md).
4. This chapter defines no way to catch a panic, in tests or elsewhere.

---

## 10.10 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) leaves open. They are provisional until merged into it. IDs continue the `N-C` series of [06](06-expressions-and-statements.md) §6.28 and [09](09-contracts-and-refinements.md) §9.10. Decisions added after N-C-84 use the `N-F` series.

| ID | Rule | Rationale |
|---|---|---|
| N-C-69 | There are exactly three kinds of failure: recoverable errors (values), panics (reported if possible, then a trap) and plain traps (never reported). Shadow-stack exhaustion and allocator failure are panics of the resource kinds `StackOverflow` and `OutOfMemory`, with fixed messages and no location. Their occurrence is the only panic behavior a profile may change (§10.1.1, §10.4.3). | D-ERR-01 and D-MIR-01 do not classify resource exhaustion. Treating it as a panic gives it a report wherever a channel exists, while the location-free form matches runtime code that has no source position. |
| N-C-70 | When `Γ.ret` is an inference variable (a closure or an `async` block), `?` unifies it with `Result<β, E>` or `Option<β>` from its operand (§10.3.2 item 2). | Closures and `async` blocks with `?` and no annotation would otherwise always need one. The kind is fixed by the operand, so inference stays local and deterministic (P8). |
| N-C-71 | "Identical error types" in D-EXPR-05 means type equality after inference: aliases are transparent, distinct and refined types differ from their bases, and no coercion, including from `Never`, applies. A failed unification is `TRY_ERROR_TYPE_MISMATCH` (§10.3.2 item 4). | D-EXPR-05 forbids conversion but does not define identity. Equality without coercion keeps `?` free of hidden conversions (P1). |
| N-C-72 | `e?` desugars to a `match` on the lang-item variants by identity. On failure it builds a new `Err(x)` or `None` of type `Γ.ret` and returns it like `return`: the `ensures` and invariant checks run first, then drops, then the transfer of control (§10.3.3). | Makes `?` independent of names in scope and gives the failure path exactly the semantics of `return`, which 06 and 09 already define. |
| N-C-73 | The structured fixes of the `?` diagnostics are offered only in the deterministic cases of §10.3.5: wrapping the return type and its returns for `TRY_INVALID_RETURN_TYPE`, and `.map_err` to the unique single-payload variant for `TRY_ERROR_TYPE_MISMATCH`. | Agents apply structured fixes mechanically, so a fix must be unambiguous and depend only on declarations (P3, P8). |
| N-C-74 | After a panic is raised, no SAI code runs in the instance: no drops, no `ensures` or invariant checks, no other task, and no program code during reporting. The report, if any, is followed by the trap (§10.4.1). | D-ERR-01 and D-MIR-01 exclude unwinding but do not say what runs between the panic and the trap. Running nothing makes the report independent of program state that the panic may have left broken (P9). |
| N-C-75 | The location of each panic is fixed by the table of §10.4.5. A call is located at the method name for method calls and at the first token of the callee otherwise. Library panics use the innermost call from outside `core`/`alloc`. Compiler-generated bodies use the innermost user construct. Inlining never changes a location. | 01 §1.5.2 item 6 requires a source location, when available, without defining it. Fixed positions make reports reproducible across implementations and optimization levels (P8). |
| N-C-76 | The message texts of the built-in panic kinds are fixed by the table of §10.4.4. A library item that names only a kind uses the default message of that kind. Messages are UTF-8 and never truncated. | Tests and tools match panic messages, so the texts must be the same in every implementation (P3, P8). |
| N-C-77 | A report is one UTF-8 text with the message and, if any, the location, without absolute paths or addresses. It is the same in `dev` and `release`. Building and delivering it runs no program code and does not use the global allocator (§10.5.2). | The report must work after any panic, including `OutOfMemory`, and must be reproducible (P8). The framing remains implementation-defined (01 §1.5.2 item 6). |
| N-C-78 | The panic mode is `Report` exactly when the profile is `dev` or `release` and a channel exists: `wasi:cli/stderr` already imported, for a component, or a root-package `@panic_hook`, for a core module. Otherwise it is `TrapOnly` (§10.5.3). | D-ERR-01 and D-TGT-02 name the channels but not the case where none exists. Never adding an import for reporting keeps the imports of an artifact equal to its capability requirements (P5, N-D-59, N-F-14). |
| N-C-79 | `@panic_hook` is allowed only on a function of an `extern "wasm"` block with exactly two `USize` parameters and result `()`, not `unsafe`, without contract clauses, in the root package, and at most once per build (§10.6.1). | D-ERR-01 names the hook but not its signature. Address and length are the minimal form for the host to read the bytes. Restricting it to the root keeps the reporting policy with the program, not with a library (P5). |
| N-C-80 | The hook is imported only in `Report` mode, unless reachable SAI code calls it (§10.6.2). | A `tiny` build should not require its host to supply an import that is never called (D-TGT-02). |
| N-C-81 | The call of the hook by the panic machinery adds no effect to any function and no capability to any package. Calls of the hook from SAI code are ordinary `host` calls (§10.6.4). | Panics are not effects (D-EFF-03). Making the hook require `host` would make panic reporting change the program's capability requirements, contrary to P5. |
| N-C-82 | A panic raised while a report is in progress traps without a report. The host MUST NOT call into the instance while the hook runs, as an exception to [12](12-unsafe-and-ffi.md) §12.7.2 item 4 (§10.6.2). | The frames of the panicking code are still live and may break invariants. A nested report could loop forever (P9). |
| N-C-83 | A host MUST NOT call a core module instance again after it has trapped. Such a call has no meaning defined by this specification (§10.7.2). | Core Wasm engines permit the call, but a trap restores nothing (`__sp`, allocator state, invariants). D-ERR-01's "a trap stops the instance" must therefore be a requirement on the host. |
| N-C-84 | The operands of a panic, including the message arguments of `panic`, `expect` and `expect_err`, are evaluated as written in every profile and panic mode. Their computation may be removed only when the removal is unobservable (§10.5.3). | Profiles must not change what a program does (C-03). Only the delivery of the report depends on the mode. |
| N-F-81 | A panic that would take the position of the call into a library function, a derived method or the checked entry of a library function, when that function is called through a function pointer, takes the position of the function item expression that was converted to the pointer (§10.4.5). | A function pointer has the declared signature, so the call through it cannot pass a position. The conversion is the user-written construct closest to the call that the compiler knows statically, and the rule keeps library positions out of reports (N-C-75, P8). |

---

## 10.11 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `CONST_EVAL_PANIC` | error | defined in [06](06-expressions-and-statements.md) §6.27; a const initializer whose evaluation would panic (§10.4.6) |
| `DUPLICATE_ATTRIBUTE` | error | defined in [15](15-attributes-and-lints.md) §15.2.4; `@panic_hook` repeated on one function (§10.6.1) |
| `DUPLICATE_PANIC_HOOK` | error | a second `@panic_hook` declaration in one build, reported at every declaration after the first in file-path and source order (§10.6.1) |
| `EXPORT_FN_NON_SCALAR` | error | defined in [12](12-unsafe-and-ffi.md) §12.7.4, §12.8; a non-scalar type in a `@panic_hook` or core export signature (§10.6.1, §10.8.2) |
| `EXTERN_EFFECTS_MISMATCH` | error | defined in [08](08-effects-and-capabilities.md) §8.6.1; an `effects` clause other than `{ host }` on a `@panic_hook` function (§10.6.1) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; a feature of this chapter used before its stage (§10.1.3) |
| `INVALID_ATTRIBUTE_ARGUMENT` | error | defined in [15](15-attributes-and-lints.md) §15.2.4; `@panic_hook` with arguments (§10.6.1) |
| `INVALID_ATTRIBUTE_TARGET` | error | defined in [15](15-attributes-and-lints.md) §15.2.4; `@panic_hook` on anything but a function of an `extern "wasm"` block (§10.6.1) |
| `INVALID_CONTRACT_EXPRESSION` | error | defined in [09](09-contracts-and-refinements.md) §9.1.2; `?` in a predicate (§10.3.2 item 7) |
| `INVALID_EXTERN_SIGNATURE` | error | defined in [12](12-unsafe-and-ffi.md) §12.7.4; an `async`, generic, `where` or `borrows` form of a `@panic_hook` function (§10.6.1) |
| `NO_EXCEPTIONS` | error | defined in [02](02-lexical-structure.md) §2.5.3 (§10.2.3) |
| `NO_NULL` | error | defined in [02](02-lexical-structure.md) §2.5.3 (§10.2.3) |
| `NOT_CONST_EXPRESSION` | error | defined in [06](06-expressions-and-statements.md) §6.27; `?` in a const initializer (§10.3.2 item 7) |
| `PANIC_HOOK_OUTSIDE_ROOT` | error | a `@panic_hook` declared in a package that is not the root package of the build (§10.6.1) |
| `PANIC_HOOK_SIGNATURE_MISMATCH` | error | a `@panic_hook` function that does not have exactly two `USize` parameters and result `()`, is an `unsafe fn`, or has a `requires` or `ensures` clause (§10.6.1) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [08](08-effects-and-capabilities.md) §8.12.7; a reachable call of the hook from SAI code in a component build (§10.6.2) |
| `TRY_ERROR_TYPE_MISMATCH` | error | the error type of a `Result` operand of `?` differs from the error type of the enclosing body's return type (§10.3.2 item 4) |
| `TRY_INVALID_OPERAND` | error | the operand of `?` is not a `Result` or an `Option`, including a borrow of one and a future that is not awaited (§10.3.2 item 1) |
| `TRY_INVALID_RETURN_TYPE` | error | `?` in a body whose return type is neither a `Result` nor an `Option` (§10.3.2 item 3) |
| `TRY_OPTION_RESULT_MIX` | error | `?` applied to an `Option` in a body returning a `Result`, or to a `Result` in a body returning an `Option` (§10.3.2 item 3) |
| `TYPE_ANNOTATIONS_NEEDED` | error | defined in [05](05-types.md) §5.16.4; the operand of `?` has an unknown type head (§10.3.2 item 1) |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14; not used for the error types of `?`, which use `TRY_ERROR_TYPE_MISMATCH` (§10.3.2 item 4) |
| `UNREACHABLE_CODE` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.26.2; code after a call of `panic` or `unreachable` (§10.4.6) |
| `UNUSED_MUST_USE` | error | defined in [06](06-expressions-and-statements.md) §6.25.2; a discarded `Result` (§10.2.2) |
