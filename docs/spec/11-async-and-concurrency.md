# 11. Async and Concurrency

**Status:** Normative
**Stage:** 5 (threads: not implemented in v0.1, D-THREAD-01)
**Related:** [decisions.md](../design/decisions.md) D-ASYNC-01, D-THREAD-01, D-EFF-02, D-ERR-01; [draft-reconciliation.md](../design/draft-reconciliation.md) C-02, R-04; [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [10](10-errors-and-panics.md), [12](12-unsafe-and-ffi.md), [13](13-components-and-wit.md), [14](14-testing.md), [16](16-core-library.md), [impl/04](../impl/04-mir.md), [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/10](../impl/10-runtime.md), [targets](../targets/targets-and-profiles.md)

This chapter defines asynchronous code in SAI: `async fn`, `async` blocks, `.await`, the `Future` and `Stream` traits, implicit pinning, `for await`, structured concurrency in `sai:async`, executors including synchronous driving with `block_on`, the mapping to WASI 0.2 pollables and the Component Model async ABI, the v0.1 position on threads, and why safe code cannot have data races. The typing hook of `.await` belongs to [06](06-expressions-and-statements.md), the ownership rules to [07](07-ownership-and-borrowing.md), effect attribution to [08](08-effects-and-capabilities.md) §8.10, the component export ABI to [13](13-components-and-wit.md), library signatures to [16](16-core-library.md), MIR lowering of state machines to [impl/04](../impl/04-mir.md), and the runtime to [impl/10](../impl/10-runtime.md). This chapter defines the semantics those documents rely on.

---

## 11.1 Overview and stage

### 11.1.1 Model

SAI async follows D-ASYNC-01:

1. **Futures are lazy.** Calling an `async fn` or evaluating an `async` block constructs a future value and runs no part of its body. The body runs only when the future is polled.
2. **No `Pin`.** The `Future` trait has an `unsafe` `poll` method whose contract forbids moving a future after its first poll. Safe code never calls `poll`. It drives futures only through `.await` and `for await`, which place the future in a hidden slot of the enclosing state machine that safe code cannot name or move. This is **implicit pinning** (C-02, §11.5).
3. **Borrows may be held across `.await`.** They are checked by the ordinary rules of [07](07-ownership-and-borrowing.md). No extra annotation is needed.
4. **Executors and combinators are library code.** `block_on`, `join`, `select` and `scope` live in the package `sai:async`. They use `unsafe` code internally ([12](12-unsafe-and-ffi.md)) and expose safe APIs.
5. **Structured concurrency only.** Every child task belongs to a scope that outlives it. There are no detached tasks. Dropping a future cancels it.
6. **Single-threaded.** In v0.1 a SAI instance runs on exactly one thread (D-THREAD-01). Concurrency means interleaving at `.await` points, never parallel execution.

### 11.1.2 Stage

All syntax in this chapter (`async fn`, `async` blocks, `.await`, `for await`) parses at Stage 0 ([03](03-grammar.md)). Before Stage 5 the compiler MUST reject every use of it after parsing with `FEATURE_NOT_IMPLEMENTED`, naming "async (Stage 5)". The traits `Future` and `Stream` and the types of `core.task` exist from Stage 2 so that `unsafe` code can implement them. A program that uses them without `async` syntax compiles at Stage 2.

Threads are not part of any stage in v0.1 (§11.11).

### 11.1.3 Packages

| Item | Location | Imported by |
|---|---|---|
| `Future`, `Stream` | `sai:core` prelude | implicit |
| `Poll`, `Context`, `Waker`, `RawWaker`, `RawWakerVTable` | `sai:core/task` | `use sai:core/task.{...}` |
| `block_on`, `scope`, `Scope`, `JoinHandle`, `join`, `join_all`, `select`, `Either`, `FutureReader`, `StreamReader` | `sai:async` | `use sai:async.{...}`, with `sai:async` declared in `requires` (D-PKG-04) |

The package name `sai:async` ends in the strict keyword `async`. A bare `use sai:async` would bind the keyword and is rejected with `USE_BINDING_IS_KEYWORD` ([04](04-modules-and-names.md)). Code MUST import items with a list (`use sai:async.{block_on, scope}`) or with an alias (`use sai:async as tasks`) (N-I-10). This chapter writes `scope`, `join` and `select` for the items that D-ASYNC-01 calls `async.scope` and so on.

### 11.1.4 Conventions

Examples import `use sai:async.{block_on, scope, join, select, Either, Scope}` where they need these items, and use the following declarations unless they declare their own. `Http` is a type with a method `async fn get(self: borrow Self, url: Str) -> Result<String, HttpError>` whose clause is `effects { net.http, alloc }`. `User` is a record with a field `name: String` and a function `User.parse(text: Str) -> Result<User, HttpError>` with `effects { alloc }`.

```sai
@derive(Copy, Clone)
distinct UserId = U64

async fn fetch_user(http: borrow Http, id: UserId) -> Result<User, HttpError>
    effects { net.http, alloc }
{
    let url = f"/users/{id.value}"
    let body = http.get(url.as_str()).await? // the borrow of `url` is held across `.await`
    User.parse(body.as_str())
}
```

---

## 11.2 `async fn` and `async` blocks

### 11.2.1 `async fn`

An `async fn` is a function declared with the `async` marker (`FnHead`, [03](03-grammar.md) §3.5):

```sai
async fn greet(http: borrow Http, id: UserId) -> Result<String, HttpError>
    effects { net.http, alloc }
{
    let user = fetch_user(http, id).await?
    Ok(f"hello, {user.name}")
}
```

The **declared output type** of an `async fn` is the type written after `->`, or `()` if none is written. The body is checked as an ordinary function body with `Γ.ret` equal to the declared output type and `Γ.async = true` ([06](06-expressions-and-statements.md) §6.1.1). `return e` and the tail expression complete the future with their value. `?` in the body uses the declared output type (D-EXPR-05).

Calling an `async fn` `f<C̄>(ā)`:

1. evaluates the arguments left to right, as for any call ([06](06-expressions-and-statements.md));
2. moves every argument into a new value of the **anonymous future type** `Fut_f<C̄>` of `f`, and returns it. No statement of the body runs.

`Fut_f<C̄>` has these properties:

| Property | Rule |
|---|---|
| `Future` impl | `Fut_f<C̄>: Future` with `Output` equal to the declared output type after substitution of `C̄` |
| `Copy`, `Clone` | never implemented |
| other traits | none. In particular no `Stream`, `Eq`, `Hash` or `Format` impl |
| regions | the regions of every parameter type, after substitution (§11.5.3, 07 N-B-25) |
| effects | its `poll` has the effect set `Sig(f) ∪ Lat(f; C̄)` ([08](08-effects-and-capabilities.md) §8.10, E-Await) |
| drop | dropping it drops every value it owns, in the order of §11.5.5 |
| name | the type cannot be written in source. It appears in diagnostics as `async fn f` |
| layout | private ([impl/06](../impl/06-wasm-lowering-and-layout.md)) |

The effects clause of an `async fn` bounds the effects of its body, not of the call ([08](08-effects-and-capabilities.md) §8.10). Where [09](09-contracts-and-refinements.md) requires a run-time check of a `requires` clause, the check runs at the call (N-I-3), after the arguments are evaluated and before the future is returned, so a violation is reported at the caller's location. The check is pure (D-CONTRACT-03), so the call still contributes no effect. Run-time checks of `ensures` clauses run when the body completes, with `result` bound to the output value (09 §9.3).

### 11.2.2 Async function item types

[05](05-types.md) §5.5.1 leaves the types of `async` function items to this chapter. The item type of a safe `async fn f<Ḡ>(x1: T1, …, xn: Tn) -> R` with arguments `C̄`:

- implements `Fn`, `FnMut` and `FnOnce` with parameters `T̄[C̄/Ḡ]` and return type `Fut_f<C̄>`. Calling the item through these traits constructs the future and contributes no effect;
- does not coerce to any function pointer type, since `Fut_f<C̄>` cannot be written. A coercion attempt is `TYPE_MISMATCH` ([05](05-types.md));
- is `Copy` and `Clone`, like every function item type.

The item type of an `unsafe async fn` implements none of the `Fn` traits. Calling it requires an unsafe context ([12](12-unsafe-and-ffi.md) §12.3). The construction is the unsafe operation. Awaiting the resulting future is safe, so the obligations of the function's `# Safety` section MUST hold for the whole execution of the body, not only at the call.

### 11.2.3 `async` blocks

`async { … }` and `async move { … }` are expressions ([03](03-grammar.md) `PrimaryExpr`). Evaluating one constructs a value of a fresh anonymous future type `Fut_b` and runs no statement of the block.

1. The block is a new function boundary ([06](06-expressions-and-statements.md) §6.1.1). `Γ.loops` is empty, `Γ.async = true`, and `Γ.unsafe = false` even if the block is lexically inside an unsafe context (06 N-C-17, [12](12-unsafe-and-ffi.md) §12.2.1).
2. `Γ.ret` is a fresh inference variable. It is unified with the type of the tail expression and with the operand of every `return` in the block, as for a closure without a return annotation ([06](06-expressions-and-statements.md) §6.17.2 item 4). `return e` completes the future with `e`. `?` uses this type.
3. **Captures.** `async { … }` captures like a non-`move` closure and `async move { … }` like a `move fn` closure ([06](06-expressions-and-statements.md) §6.17.3, [07](07-ownership-and-borrowing.md) §7.11). A shared or mutable capture stores a borrow of the captured variable, so the future carries that borrow's region.
4. `Fut_b: Future` with `Output = Γ.ret`. `Fut_b` is never `Copy` or `Clone`. Its regions are the regions of its captures ([07](07-ownership-and-borrowing.md) §7.6.2).
5. **Effects.** Constructing the block contributes `∅`. The inferred effect set of the block body is carried by `Fut_b` and is contributed by `.await` on it, in the same way as the effects of a closure body are contributed by a call to the closure ([08](08-effects-and-capabilities.md) §8.7.3, §8.10).

```sai
fn make_greeting(http: borrow Http, id: UserId) -> Result<String, HttpError>
    effects { net.http, alloc }
{
    let task = async move {
        let user = fetch_user(http, id).await?
        Ok(f"hello, {user.name}")
    }
    block_on(task)
}
```

The block captures `http` (a `borrow Http`, which is `Copy`) and `id` by value. `block_on` is defined in §11.9.

### 11.2.4 Recursion

The anonymous future of an `async fn` contains the await slots of its body (§11.3.3). If the future type of `f` would contain itself, directly or through other anonymous future types, without a `Box` in between, the error is `ASYNC_RECURSION`. It names the cycle and carries the fix "box the recursive call": `Box.new(f(…)).await`, which is valid because `Box<F>: Future` (§11.3.2). The check runs on the monomorphic call graph of anonymous futures ([impl/04](../impl/04-mir.md)).

```sai
async fn countdown(n: U32) -> U32
    effects { alloc }
{
    if n == 0 {
        return 0
    }
    1 + Box.new(countdown(n - 1)).await // accepted: the cycle goes through Box
}
```

Writing `countdown(n - 1).await` instead is rejected with `ASYNC_RECURSION`.

### 11.2.5 No async closures

There are no async closures (N-I-1). The grammar has no `async fn (…)` closure form ([03](03-grammar.md) `ClosureExpr`), and the parser MUST report `async` followed by `fn` or `move fn` in expression position as `NO_ASYNC_CLOSURE` rather than `EXPECTED_TOKEN` (03). Its fix rewrites `async fn(x) { b }` to `fn(x) { async move { b } }` and `async move fn(x) { b }` to `move fn(x) { async move { b } }`. A closure that returns an `async move` block has the same behaviour as an async closure would, and its type is an ordinary closure type (`Fn(T̄) -> Fut_b`).

```sai
fn names(http: borrow Http, ids: borrow Vec<UserId>) -> Vec<String>
    effects { net.http, alloc }
{
    let load = fn(id: UserId) { async move { fetch_user(http, id).await } }
    var out = Vec.new()
    for id in ids.iter() {
        match block_on(load(*id)) {
            Ok(user) => out.push(user.name)
            Err(_) => {}
        }
    }
    out
}
```

### 11.2.6 Restrictions

The following rules apply to `async fn` declarations (N-I-3):

| Construct | Rule |
|---|---|
| an `async fn` declared in a trait, or implemented in a trait impl (including `Drop.drop`) | `ASYNC_FN_IN_TRAIT`. The future type of an async trait method would have to be an implicit associated type, and SAI has neither opaque return types nor trait objects (C-02). Generic code takes a type parameter `F: Future`, or a parameter `G: Fn(T) -> F` with `F: Future`, instead. With this rule, the requirement of [05](05-types.md) §5.9.1 rule 6 that `async` markers match is always met |
| an `async fn` in an `extern` block | `INVALID_EXTERN_SIGNATURE` ([12](12-unsafe-and-ffi.md) §12.7) |
| `async fn main` | `ASYNC_MAIN`. The entry point `pub fn main(env: owned process.Env) -> process.ExitCode` is synchronous. The fix wraps the body in `block_on(async move { … })` (§11.9) |
| an `async fn` in an interface or an interface impl | defined by [13](13-components-and-wit.md) (Stage 5) and §11.10.4 |
| `unsafe async fn` | allowed. §11.2.2 defines where the unsafety applies |
| an `async fn` whose future type contains itself | `ASYNC_RECURSION` (§11.2.4) |

---

## 11.3 `.await`

### 11.3.1 Where `.await` is allowed

`e.await` is a postfix expression ([03](03-grammar.md) `PostfixOp`). It binds like a method call, so `fetch_user(http, id).await?` applies `?` to the awaited value. It is well formed only where `Γ.async` holds (N-I-2):

| Body | `Γ.async` |
|---|---|
| the body of an `async fn` | true |
| an `async` or `async move` block | true |
| a closure body, even inside an `async fn` or `async` block | false (a closure is a new function boundary) |
| any other function body, including `main`, `Drop.drop`, trait default bodies and `unsafe fn` bodies | false |
| `test` and `property` bodies ([14](14-testing.md)) | false (§11.13) |
| const initializers, refinement predicates, `requires`, `ensures` and `invariant` expressions | false. [09](09-contracts-and-refinements.md) also rejects `.await` in contracts with `INVALID_CONTRACT_EXPRESSION`, which takes precedence |

`Γ.async` is a property of the innermost body. It is not inherited across a closure boundary. `.await` where `Γ.async` is false is the error `AWAIT_OUTSIDE_ASYNC`, reported at the `.await` token. When the innermost body is a closure inside an async body, a note points at the closure boundary. When the innermost body is a free function or inherent method that is not `main`, the diagnostic carries the fix "add `async` to the function". The same rule and code apply to `for await` (§11.6).

```sai
fn load_now(http: borrow Http, id: UserId) -> Result<User, HttpError>
    effects { net.http, alloc }
{
    fetch_user(http, id).await         // rejected: AWAIT_OUTSIDE_ASYNC; use block_on (§11.9)
}
```

### 11.3.2 Typing

The typing hook of [06](06-expressions-and-statements.md) §6.20.1 is:

```text
(T-Await)   Γ.async    Γ ⊢ e : F    Δ ⊢ F : Future
            ─────────────────────────────────────────
            Γ ⊢ e.await : F.Output
```

- `e` is checked in a value context. If `F` does not implement `Future`, the error is `UNSATISFIED_BOUND` ([05](05-types.md)), with the note "`.await` requires `Future`".
- The operand is **consumed**. `e.await` uses `e` by value ([07](07-ownership-and-borrowing.md) §7.16 item 1). Awaiting a local moves it, and a later use is `USE_AFTER_MOVE` (07). Awaiting a place behind a borrow is `MOVE_OUT_OF_BORROW` (07).
- There is no impl of `Future` for `borrow mut F` (N-I-4). A future therefore cannot be polled through a borrow by safe code, and its owner can never move it after a poll that it did not observe. Awaiting a future more than once is impossible. To race a future against another, pass both by value to a combinator such as `select` (§11.7.6).
- `Box<F>: Future` with `Output = F.Output` when `F: Future` (N-I-4). Its `poll` polls `*self` in place and adds no effect. Moving the box does not move the heap value, and safe code cannot move `*b` out of a box (`MOVE_OUT_OF_BOX`, 07), so this impl is sound. It is how recursive async functions are written (§11.2.4).
- The effects of `e.await` are defined by [08](08-effects-and-capabilities.md) §8.10 (E-Await).
- Every anonymous future type (of an `async fn` call or an `async` block) is a must-use type (D-EXPR-01, [15](15-attributes-and-lints.md) §15.5.1 rule 3). A named type that implements `Future` is a must-use type only if its declaration carries `@must_use`. A discarded, never-awaited future of a must-use type is `UNUSED_MUST_USE` ([06](06-expressions-and-statements.md) §6.25.2). Its body never runs.

```sai
async fn both_names(http: borrow Http, a: UserId, b: UserId) -> Result<String, HttpError>
    effects { net.http, alloc }
{
    let pending = fetch_user(http, a)
    let first = pending.await?
    let again = pending.await?        // rejected: USE_AFTER_MOVE (07)
    Ok(f"{first.name} {again.name}")
}
```

### 11.3.3 Dynamic semantics

Each `.await` expression in a body owns one **await slot**: a hidden place in the frame of the enclosing future, of type `F`. The slot is not a local. It cannot be named, borrowed or moved by source code. Evaluating `e.await` while the enclosing future is being polled with the context `cx`:

1. Evaluate `e` to a value `v`.
2. Move `v` into the await slot. The slot is now **occupied**.
3. Call `F.poll(borrow mut slot, cx)`. This call is the only place where safe code causes a `poll`. It is sound because the slot does not move until it is emptied (§11.5).
4. If the result is `.Ready(x)`: drop the future in the slot in place (its drop glue runs, §11.5.5). The slot is now **empty**. The value of `e.await` is `x`, and execution continues after the `.await`.
5. If the result is `.Pending`: the enclosing future's own `poll` returns `.Pending`. The frame is **suspended** at this `.await`, with the slot occupied. When the enclosing future is polled again with a context `cx'`, execution resumes at step 3 with `cx'`.

The awaited future is therefore dropped before the code that follows the `.await` runs. An `.await` is one MIR terminator `Await` (D-MIR-01, [impl/04](../impl/04-mir.md) §4.2.6), and one point of the borrow checker's CFG ([07](07-ownership-and-borrowing.md) §7.9.1 item 6). It has two successors: the continuation after step 4, and the **cancellation exit** of [07](07-ownership-and-borrowing.md) §7.16 item 5, which performs the drops of §11.5.5 when the suspended future is dropped at this `.await` (N-B-27, N-I-8).

The context passed to the inner `poll` is the context of the current poll of the enclosing future. A waker registered by the inner future therefore wakes the task that owns the whole chain of futures (§11.4.3).

### 11.3.4 Completion

An anonymous future (`Fut_f` or `Fut_b`) is **completed** once its `poll` has returned `.Ready`. Polling a completed anonymous future panics with the message `"future polled after completion"` ([10](10-errors-and-panics.md), MIR `ExplicitPanic`) (N-I-5). This is memory-safe. Safe code cannot reach it, because `.await` consumes its operand and empties the slot on completion. Only an incorrect hand-written executor can.

A panic inside a future's body traps the instance (D-ERR-01). There is no unwinding and no "poisoned" future state to observe.

---

## 11.4 `Future`, `Poll`, `Context` and `Waker`

### 11.4.1 Declarations

The following declarations are fixed by this chapter. Their full API (helper methods, `Format` impls) is listed in [16](16-core-library.md).

```sai
// sai:core prelude
pub trait Future {
    type Output

    unsafe fn poll(self: borrow mut Self, cx: borrow mut Context) -> Poll<Self.Output>
}

// sai:core/task
pub enum Poll<T> {
    Ready(T)
    Pending
}
```

`Future` is a safe trait: implementing it is safe. Its `poll` method is `unsafe` to **call** (D-ASYNC-01), because the caller promises the contract of §11.4.2. An impl declares the method as `unsafe fn poll` (the markers must match, [05](05-types.md) §5.9.1 rule 6), so the body of an impl's `poll` is an unsafe context ([12](12-unsafe-and-ffi.md) §12.2.2).

### 11.4.2 The poll contract

Let `v` be a value of a type `F: Future` stored at a place `p`. The **caller** of `F.poll` MUST ensure (N-I-6):

1. **No move after the first poll.** From the start of the first call of `poll` on `p` until `v` has been dropped in place, `v` MUST NOT be moved, copied bit by bit to another place, or overwritten, and the storage of `p` MUST NOT be deallocated or reused. Before the first poll, `v` is an ordinary value and may be moved freely.
2. **Drop guarantee.** Once `v` has been polled, its storage MUST NOT be deallocated or reused unless `v`'s drop glue has run in place first. A polled future is never leaked by freeing its memory without dropping it. (Leaking it by never freeing the storage is allowed. That is not undefined behavior.)
3. **Valid context.** `cx` MUST be valid for the duration of the call. The context, and its waker, MAY differ between calls.
4. **Exclusive access.** No other access to `v` may happen during the call (this is already implied by `borrow mut`, [07](07-ownership-and-borrowing.md)).

A violation of items 1 to 3 is undefined behavior ([12](12-unsafe-and-ffi.md) §12.6). The **implementation** of `poll` MAY rely on items 1 to 4, for example by keeping raw pointers into its own fields across polls. The implementation MUST in turn:

5. return `.Pending` only after arranging for the waker of the current context, or a clone of it, to be woken when progress may be possible. A violation is not undefined behavior. The task is never polled again, and the executor reports a deadlock if it can detect one (§11.9.2);
6. tolerate spurious polls: `poll` MAY be called when no wake happened, and MUST then behave correctly;
7. remain memory-safe when polled after it has returned `.Ready`. The trait imposes no other requirement on such a call. Anonymous futures panic (§11.3.4).

Safe code cannot violate items 1 to 4 because it never calls `poll`. `.await` and `for await` satisfy them by construction (§11.5). Executors and combinators are unsafe code that MUST satisfy them (§11.9.4).

### 11.4.3 `Context` and `Waker`

```text
// sai:core/task (signature listing)
pub type Context { … }            // fields private; carries the region of its waker

impl Context {
    pub fn from_waker(waker: borrow Waker) -> Context
    pub fn waker(self: borrow Self) -> borrow Waker
}

pub type Waker { … }              // fields private

impl Waker {
    pub fn wake(self: Self)
    pub fn wake_by_ref(self: borrow Self)
    pub fn will_wake(self: borrow Self, other: borrow Waker) -> Bool
    pub fn noop() -> Waker
    pub unsafe fn from_raw(raw: RawWaker) -> Waker
}

impl Clone for Waker { … }
impl Drop for Waker { … }

pub type RawWaker {
    pub data: Ptr<()>
    pub vtable: Ptr<RawWakerVTable>
}

pub type RawWakerVTable {
    pub clone: unsafe fn(Ptr<()>) -> RawWaker
    pub wake: unsafe fn(Ptr<()>)
    pub wake_by_ref: unsafe fn(Ptr<()>)
    pub drop: unsafe fn(Ptr<()>)
}
```

This block is a signature listing, not source code: bodies and private fields are elided with `{ … }`. The same convention is used for every `text` listing in this chapter.

- A `Context` is created by an executor for one call of `poll` and passed down by every `.await`. `Context` is a region-carrying record ([07](07-ownership-and-borrowing.md) §7.8). It cannot outlive the waker it borrows. Besides the waker it carries a private **executor extension** used by `sai:async` to register WASI pollables (§11.10.2). `Context.from_waker` creates a context with an empty extension.
- `wake` and `wake_by_ref` schedule the task that the waker belongs to. After a wake, the executor MUST poll that task at least once more. Several wakes before that poll MAY coalesce into one poll. Waking a task that has completed, or whose executor no longer exists, MUST be a no-op.
- `clone` produces a waker for the same task. `will_wake` returns `true` only if both wakers wake the same task. It MAY return `false` for two such wakers.
- `Waker.noop()` returns a waker whose wake does nothing. It is used to poll a future once, for example in tests of hand-written futures.
- **Raw wakers.** `Waker.from_raw` is unsafe. Its caller MUST guarantee that the four vtable functions are safe to call with `data` from any code that holds the waker, that `clone` returns a raw waker for the same task, and that `drop` releases what `clone` acquired. The vtable function pointers are pure (D-EFF-03): they MUST NOT perform effects ([12](12-unsafe-and-ffi.md) §12.6.6). Waking only changes executor state in linear memory.
- `Waker` is not `Copy`. Its `Drop` impl calls the vtable's `drop` function and has `effects {}` ([07](07-ownership-and-borrowing.md) §7.13.6).

### 11.4.4 A hand-written future

```sai
use sai:core/task.{Context, Poll}

type YieldOnce {
    yielded: Bool
}

impl Future for YieldOnce {
    type Output = ()

    unsafe fn poll(self: borrow mut Self, cx: borrow mut Context) -> Poll<()> {
        if self.yielded {
            return .Ready(())
        }
        self.yielded = true
        cx.waker().wake_by_ref()
        .Pending
    }
}

async fn pause() {
    YieldOnce(yielded: false).await
}
```

`pause` suspends once and is resumed on the next round of its executor. `YieldOnce` does not rely on items 1 and 2 of §11.4.2: it has no self-references, so it would stay correct even if moved. A future that does keep pointers into itself MUST document that it relies on the contract.

---

## 11.5 Implicit pinning

### 11.5.1 Why there is no `Pin`

In a design where safe code can obtain a mutable borrow of a future that has been polled, safe code can also move the future out of that borrow (by swapping or replacing it), so the language needs a `Pin` wrapper and an `Unpin` auto trait to forbid it. SAI removes the capability instead of guarding it (C-02):

1. `poll` is `unsafe`, so safe code never polls (D-ASYNC-01).
2. Safe code polls only through `.await` and `for await`, which own a hidden slot that source code cannot name (§11.3.3, §11.6.2).
3. There is no `Future` impl for `borrow mut F` (N-I-4), so a polled future is never reachable through a borrow from safe code.

There is no `Pin` type, no pin projection and no `Unpin`. SAI has no auto traits at all (D-TYPE-08). The rules are smaller (P7) and the obligations sit in the unsafe code that drives futures, where `sai inspect unsafe` lists them (P9, [12](12-unsafe-and-ffi.md) §12.11).

### 11.5.2 Soundness argument

The claim is: in a program whose unsafe code meets its obligations, every future polled by safe code satisfies items 1 to 3 of §11.4.2.

1. A future is polled by safe code only in an await slot. An await slot is a place inside the frame of the enclosing future `E`.
2. The slot does not move while `E` does not move. `E` is itself polled either in an await slot of its own enclosing future, or by an executor or combinator, which is unsafe code that MUST satisfy the contract (§11.9.4). By induction on the nesting of futures, every frame in the chain stays in place from its first poll until it is dropped.
3. Before `E` is first polled, no statement of its body has run, so all its await slots are empty. Moving an unpolled `E` therefore moves no polled future. This is why unpolled futures may be passed by value to `spawn`, `join` or `select`, stored in records, or returned.
4. A slot is emptied by dropping its future in place (§11.3.3 step 4), and an occupied slot is dropped in place when `E` is dropped (§11.5.5). The drop guarantee holds.
5. The locals of a frame live in the frame. A borrow of a local that is held across a suspension stays valid, because the frame does not move.

### 11.5.3 Regions of futures

The regions of an anonymous future type are (N-I-7, refining 07 N-B-25 without weakening it):

- `regions(Fut_f<C̄>)` is the union of the regions of every parameter type of `f` after substitution, including `self`. A borrow passed to an async function therefore stays live until the future is dropped, whether or not it has been awaited ([07](07-ownership-and-borrowing.md) §7.16 item 3).
- `regions(Fut_b)` is the union of the regions of the types of the block's captures, plus the region of every by-reference capture ([07](07-ownership-and-borrowing.md) §7.11).
- The output type's regions follow the elision rules and the `borrows` clause of [07](07-ownership-and-borrowing.md) §7.7, applied to the declared output type.

```sai
async fn text_length(text: Str) -> USize {
    text.len()
}

async fn caller() -> USize
    effects { alloc }
{
    let pending = {
        let owned = String.from("abc")
        text_length(owned.as_str())      // rejected: DOES_NOT_LIVE_LONG_ENOUGH (07)
    }
    pending.await
}
```

The future returned by `text_length` carries the region of `owned.as_str()`, and it is still live when `owned` is dropped at the end of the inner block.

### 11.5.4 Borrows held across `.await`

An `.await` is an ordinary CFG point for the borrow checker ([07](07-ownership-and-borrowing.md) §7.16 item 2). Loans that are live before it stay live after it, under the same rules as across a call. No annotation is needed.

```sai
async fn label(http: borrow Http, id: UserId) -> Result<String, HttpError>
    effects { net.http, alloc }
{
    let prefix = String.from("user ")
    let view = prefix.as_str() // a borrow of a frame local
    let user = fetch_user(http, id).await? // held across the suspension: accepted
    Ok(f"{view}{user.name}")
}
```

While the frame is suspended, `view` points into `prefix`, and both are stored in the frame. This is sound because the frame does not move after its first poll (§11.5.2 item 5).

A mutable loan held across an `.await` excludes every other access to its place for as long as it is live, as anywhere else. Within one task this needs no special rule. Between concurrent tasks, §11.7.3 applies.

### 11.5.5 Drop order and cancellation

Dropping a future **cancels** it (D-ASYNC-01). No further statement of its body runs. There is no "finally" block, and `ensures` clauses are not checked. Dropping is a drop access with `droplive = regions(F)` ([07](07-ownership-and-borrowing.md) §7.13.7). The drop glue of an anonymous future runs exactly the following steps, in this order (N-I-8):

1. **Not yet polled.** The future owns only what the call or the block captured. For `Fut_f`, the parameters are dropped in reverse parameter order with `self` last, exactly as if the body had returned before its first statement ([07](07-ownership-and-borrowing.md) §7.13.2 item 3). For `Fut_b`, the by-move captures are dropped in capture order, as for a closure ([07](07-ownership-and-borrowing.md) §7.13.3).
2. **Suspended at an `.await`.**
   1. The future in the occupied await slot of that `.await` is dropped in place first. It is the innermost owner, so cancellation propagates inward before any outer value is released.
   2. Then every scope that is active at the suspension point is left, innermost first. Within each scope, the owned locals that are still initialized are dropped in reverse declaration order, and the pending temporaries of the statement that contains the `.await` are dropped first, in reverse order of creation ([07](07-ownership-and-borrowing.md) §7.13.2 items 1 to 8).
   3. Finally the parameters of `Fut_f`, or the by-move captures of `Fut_b`, that are still owned are dropped as in step 1.
3. **Completed.** The body has returned, so every local, parameter and capture has already been dropped or moved. The drop glue does nothing.

Values that were moved out are not dropped (drop flags, [07](07-ownership-and-borrowing.md) §7.13). Each drop has `effects {}` (D-OWN-05), so cancellation is pure.

```sai
type Noisy {
    tag: USize
}

impl Drop for Noisy {
    fn drop(self: borrow mut Self) {}
}

async fn steps(http: borrow Http, id: UserId) -> Result<USize, HttpError>
    effects { net.http, alloc }
{
    let a = Noisy(tag: 1)
    let b = Noisy(tag: 2)
    let user = fetch_user(http, id).await?
    Ok(a.tag + b.tag + user.name.len())
}
```

If the future of `steps` is dropped while it is suspended at the `.await`, the order is: the `fetch_user` future in the slot, then `b`, then `a`, then the parameters `id` and `http` (which have no drop glue).

---

## 11.6 `Stream` and `for await`

### 11.6.1 The `Stream` trait

```sai
// sai:core prelude
pub trait Stream {
    type Item

    unsafe fn poll_next(self: borrow mut Self, cx: borrow mut Context) -> Poll<Option<Self.Item>>
}
```

- The contract of §11.4.2 applies to `poll_next` unchanged, with "first poll" meaning the first call of `poll_next`. A stream that has been polled MUST NOT be moved, and MUST be dropped in place.
- `.Ready(Some(x))` yields an item. `.Ready(None)` means the stream is **exhausted**. A caller MUST NOT rely on any result of `poll_next` after exhaustion, and the implementation MUST stay memory-safe if it is called. `for await` never polls an exhausted stream.
- `Box<S>: Stream` with `Item = S.Item` when `S: Stream`, forwarding to `*self` (N-I-4). There is no impl for `borrow mut S`.
- There are no generators. `yield` is a reserved word ([02](02-lexical-structure.md)). Streams are written by implementing `Stream`, or obtained from libraries (for example `StreamReader` of §11.10.3).

### 11.6.2 `for await`

`for await P in e B` (with an optional label, [03](03-grammar.md) `ForExpr`) is allowed only where `Γ.async` holds. Otherwise the error is `AWAIT_OUTSIDE_ASYNC` (§11.3.1). Its static rules (N-I-9):

1. `e` is checked in a value context and MUST have a type `S: Stream`. There is no `IntoStream` conversion: the operand is the stream itself. Otherwise the error is `UNSATISFIED_BOUND` ([05](05-types.md)), with the note "`for await` requires `Stream`". If `S` implements `IntoIterator`, the note also suggests a plain `for`.
2. The operand is consumed. `e` is used by value, as for `.await`.
3. `P` MUST be irrefutable for `S.Item` (`REFUTABLE_PATTERN_IN_FOR`, [06](06-expressions-and-statements.md)).
4. The expression has type `()`. `break` with a value is `INVALID_BREAK_VALUE` (06). `break` and `continue` target this loop, by label or as the innermost loop.
5. Effects follow [08](08-effects-and-capabilities.md) §8.10: a concrete stream contributes the effects of its `poll_next` impl, and a stream whose type is a type parameter contributes nothing (latent).

The meaning of `'l: for await P in e B` is the following expansion, where `$slot` and `$next` are hidden names that source code cannot refer to:

```text
{
    let $slot = e                       // the stream slot: a hidden place in the frame
    'l: loop {
        let $next = await_next($slot)   // one suspension point (below)
        match $next {
            Some(P) => B
            None => break 'l
        }
    }
}
```

`await_next($slot)` is a suspension point with the dynamic semantics of §11.3.3 steps 3 and 5, applied to `S.poll_next(borrow mut $slot, cx)`: on `.Pending` the enclosing future suspends, and on `.Ready(r)` the value is `r`. Unlike `.await`, it does not empty the slot. The stream stays in the slot for the next iteration.

- The stream slot is pinned exactly like an await slot (§11.5.2). The stream is dropped in place when the expansion's outer block is left: after the loop ends with `None`, on `break`, `return` or `?`, or when the enclosing future is cancelled while suspended in the loop (§11.5.5). The locals of `B` are dropped before the stream.
- Each `await_next` is one MIR suspension point and one CFG point for the borrow checker ([impl/04](../impl/04-mir.md), [07](07-ownership-and-borrowing.md) §7.16).

### 11.6.3 Example

```sai
use sai:core/task.{Context, Poll}

type Countdown {
    remaining: U32
}

impl Stream for Countdown {
    type Item = U32

    unsafe fn poll_next(self: borrow mut Self, cx: borrow mut Context) -> Poll<Option<U32>> {
        if self.remaining == 0 {
            return .Ready(None)
        }
        self.remaining -= 1
        .Ready(Some(self.remaining))
    }
}

async fn total(start: U32) -> U32 {
    var sum: U32 = 0
    for await n in Countdown(remaining: start) {
        sum += n
    }
    sum
}
```

`total(4)` completes with `6` (3 + 2 + 1 + 0). `Countdown` never returns `.Pending`, so the loop never suspends.

---

## 11.7 Structured concurrency

### 11.7.1 Principles

Concurrency inside one SAI instance is provided by the `sai:async` package (D-ASYNC-01):

1. **Every child task has a parent scope.** Child tasks are started only through a `Scope` handed out by `scope`. A scope completes only when its body and all its children have completed. There is no free-standing task start and no detached task. (The method that starts a child is `Scope.spawn`, N-I-14. `spawn` is an ordinary identifier, not a reserved word, D-LEX-04.)
2. **Dropping is cancelling.** Dropping any future, including a scope future or a combinator future, cancels everything it owns, synchronously and in a defined order.
3. **Interleaving only at suspension points.** Tasks run on one thread (§11.11). A task runs until it reaches an `.await` or `await_next` that returns `.Pending`. Between two suspension points, no other task's code runs.
4. **Deterministic scheduling.** The order in which a scope or combinator polls its operands is fully defined by this section, given the order of wakes. Which wakes happen, and when, depends on the host (01 §1.5.3, unspecified behavior item 4).

### 11.7.2 Signatures

The following signatures of `sai:async` are assumed by this chapter and fixed in [16](16-core-library.md) §16.16. `Scope` and `JoinHandle` are region-carrying types: each carries the scope region `'s` of §11.7.3. Region names are written here only for explanation. They are not source syntax.

```text
// sai:async (signature listing)
pub async fn scope<B, F>(body: B) -> F.Output
    where B: FnOnce(borrow Scope) -> F, F: Future
    effects { alloc }

pub type Scope { … }                                    // carries 's

impl Scope {
    pub fn spawn<C: Future>(self: borrow Self, child: C) -> JoinHandle<C.Output>
        effects { alloc }
}

pub type JoinHandle<T> { … }                            // carries 's; not Copy, not Clone

impl<T> JoinHandle<T> {
    pub async fn join(self: Self) -> T
    pub fn cancel(self: Self)
    pub fn is_finished(self: borrow Self) -> Bool
}

pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A.Output, B.Output)
pub async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F.Output>
    effects { alloc }
pub async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A.Output, B.Output>

pub enum Either<A, B> {
    Left(A)
    Right(B)
}
```

### 11.7.3 The scope region rule

The bound `B: FnOnce(borrow Scope) -> F` cannot express, with the region rules of [07](07-ownership-and-borrowing.md), that the body's future `F` may keep using the `borrow Scope` it receives. `scope` is therefore a designated item of `sai:async` ([16](16-core-library.md) §16.16.1, N-J-26), not a lang item, with the following typing rule, which replaces the ordinary check of the bound (N-I-11). For each call `scope(body)`:

1. A fresh region `'s` is introduced. It is live from the call until the scope future completes or is dropped. `'s` is the region of the `borrow Scope` passed to `body`, of every `JoinHandle` that the scope's `spawn` returns, and of the `Scope` value itself.
2. `body` is checked against `FnOnce(borrow Scope) -> F`, where `F` MAY mention `'s`.
3. Every region of the type of `body` and of `F`, other than `'s`, MUST outlive `'s`.
4. For each call `x.spawn(c)` where `x : borrow Scope` with region `'s` and `c : C`, every region of `C` other than `'s` MUST outlive `'s`. A child task may therefore borrow anything that the caller of `scope` can borrow for the whole scope, and it may use the `Scope` itself to start further children, but it MUST NOT borrow a local of the body future or of another child. Those locals are dropped when the body or that child completes, which can be before the scope ends. A violation is `CHILD_TASK_BORROWS_LOCAL`, which names the borrowed local and the `spawn` call.
5. `F.Output`, and therefore the output of the scope future, MUST NOT mention `'s`. A `JoinHandle` or a `borrow Scope` that would escape the scope through its output is `SCOPE_REGION_ESCAPE`.

The rest is the ordinary borrow checker. A child's future is owned by the scope until the scope completes, so every loan the child holds is live until then. Consequently:

- Two children that hold `borrow mut` of the same place, or one holding `borrow mut` and another holding `borrow` of it, are rejected with `CONFLICTING_BORROW` ([07](07-ownership-and-borrowing.md)) at the second `spawn`.
- The body cannot write a place that a child borrows. The error is `ASSIGN_WHILE_BORROWED` or `CONFLICTING_BORROW` (07).
- Joining a child does not end its loans early. The checker does not track completion (P8).

This gives aliasing XOR mutability across concurrent tasks without any `Send` or `Share` trait (§11.12).

```sai
async fn count_both(http: borrow Http, a: UserId, b: UserId, hits: borrow mut U32)
    effects { net.http, alloc }
{
    scope(fn(s) {
        async move {
            let left = borrow mut *hits
            let first = s.spawn(async move {
                if fetch_user(http, a).await.is_ok() {
                    *left += 1
                }
            })
            let right = borrow mut *hits         // rejected: CONFLICTING_BORROW (07); `left` is live until the scope ends
            let second = s.spawn(async move {
                if fetch_user(http, b).await.is_ok() {
                    *right += 1
                }
            })
            first.join().await
            second.join().await
        }
    }).await
}
```

Moving `hits` itself into both children is rejected with `USE_AFTER_MOVE` instead, and borrowing a local of the body from a child is `CHILD_TASK_BORROWS_LOCAL`. The fix is to let each child return its result and combine the results in the body after joining.


### 11.7.4 Scheduling inside a scope

A scope future owns its body future and an ordered list of children, in the order of their `spawn` calls. Each of them has its own waker. Waking it marks that operand as **woken** and wakes the waker of the context with which the scope future was last polled. Each poll of the scope future is one **round** (N-I-13):

1. If the body has not completed and is woken or has never been polled, the body is polled. If it returns `.Ready(v)`, `v` is stored as the scope's output and the body future is dropped in place.
2. Then the children are visited in list order, starting from the first. A child that has not completed and is woken or has never been polled is polled once, and its woken mark is cleared before the poll. A child that completes with `.Ready(x)` has its future dropped in place, and `x` is stored for its `JoinHandle`. A child started during the round, by the body or by another child, is appended to the list and is visited in the same round when the visit reaches it.
3. If the body and every child have completed, the scope drops every stored child output that was never taken, in list order, and returns `.Ready` with the body's output. Otherwise it returns `.Pending`.

Starting a child does not poll it. Its first poll happens in the round in which `spawn` is called or in a later one. The body may complete before its children. The scope then keeps polling the children and completes after the last one.

### 11.7.5 `JoinHandle`

- `spawn` returns a `JoinHandle<T>` for the child's output `T`. It is not `Copy` or `Clone`. It carries `'s`, so it cannot leave the scope (§11.7.3).
- `handle.join()` constructs a future that completes with the child's output once the child has completed, and takes the output out of the scope. If the child has not completed, the join future registers its context's waker with the child's slot and returns `.Pending`. When the child completes, that waker is woken.
- `handle.cancel()` drops the child synchronously: the child's future is dropped in place, by the rules of §11.5.5, if it has not completed, or its stored output is dropped if it has. The child is then considered completed for the purpose of §11.7.4 step 3. `cancel` has `effects {}`.
- Dropping a `JoinHandle` without calling `join` or `cancel` does **not** cancel the child (N-I-14). The child keeps running, and the scope still waits for it. Its output is dropped by §11.7.4 step 3. This keeps "the scope waits for all children" (D-ASYNC-01) true regardless of what the body does with handles.
- `is_finished` returns `true` once the child has completed or been cancelled.

### 11.7.6 `join`, `join_all` and `select`

These combinators are futures that own their operands (N-I-15). Their operands are unpolled futures when passed in, so they may be moved freely until the combinator is first polled (§11.5.2 item 3).

- **`join(a, b)`** passes its own context unchanged to its operands. On each poll it polls `a` if `a` has not completed, then `b` if `b` has not completed. A completed operand is dropped in place and its output stored. When both have completed, it returns `.Ready((x, y))`. It allocates nothing and has no effect of its own.
- **`join_all(futures)`** moves the futures into a buffer and gives each its own waker, as a scope does for children. Each poll visits the operands in vector order and polls those that are woken or have never been polled. It completes with the outputs in vector order. It has the effect `alloc`.
- **`select(a, b)`** passes its own context unchanged to its operands. On each poll it polls `a`. If `a` is ready with `x`, it drops `b` in place and returns `.Ready(.Left(x))`, without polling `b` in that poll. Otherwise it polls `b`. If `b` is ready with `y`, it drops `a` in place and returns `.Ready(.Right(y))`. Otherwise it returns `.Pending`. Selection is therefore **left-biased and deterministic**: when both operands would be ready in the same poll, `a` wins. The losing operand is cancelled before `select` returns, so none of its code runs afterwards.
- There is no separate `race` combinator. Selecting among more than two futures is written by nesting `select`, with the bias going to the leftmost operand.

```sai
async fn first_of(http: borrow Http, a: UserId, b: UserId) -> Result<User, HttpError>
    effects { net.http, alloc }
{
    match select(fetch_user(http, a), fetch_user(http, b)).await {
        Either.Left(user) => user
        Either.Right(user) => user
    }
}
```

### 11.7.7 Cancellation and drop order of tasks

Dropping a scope future that has not completed cancels it (N-I-13):

1. the body future, if it has not completed, is dropped in place first, by §11.5.5;
2. then the children are processed in **reverse** list order (the last started first). For each child, its future is dropped in place if it has not completed, otherwise its stored output is dropped if it has not been taken;
3. then the scope's own allocations are freed.

The body is dropped first because it is the only operand that can observe the others through `JoinHandle`s. Children are dropped in reverse spawn order to mirror the reverse declaration order of locals (D-OWN-05). Dropping `join` drops its incomplete operands in reverse argument order (`b`, then `a`), and their stored outputs in the same order. Dropping `join_all` drops its incomplete operands and stored outputs in reverse vector order. Dropping `select` drops `b`, then `a`. All of this is synchronous and has `effects {}`.

Cancellation is the only way to stop a task early. A task cannot be "aborted" from inside without returning, and there is no cancellation token in the language. A task that should stop at a point it chooses can select against a signal future.

### 11.7.8 Panics in tasks

A panic in any task (the body, a child, or any future polled by a combinator) is a trap of the whole instance (D-ERR-01, [10](10-errors-and-panics.md)). It does not propagate to the `JoinHandle` and is not caught by the scope. Sibling tasks do not run again, and no drop code runs. `JoinHandle.join` therefore returns `T`, not a `Result`. A task that can fail returns a `Result` and its parent handles it.

### 11.7.9 Example

```sai
async fn fetch_both(
    http: borrow Http,
    a: UserId,
    b: UserId,
) -> (Result<User, HttpError>, Result<User, HttpError>)
    effects { net.http, alloc }
{
    scope(fn(s) {
        async move {
            let first = s.spawn(fetch_user(http, a))
            let second = s.spawn(fetch_user(http, b))
            (first.join().await, second.join().await)
        }
    }).await
}
```

Both requests are started before the body first suspends. In the first round, the body runs until `first.join().await` returns `.Pending`, and then both children are polled in start order. The two children borrow `*http`, a shared borrow whose region is the caller's, which outlives `'s`.

---

## 11.8 Effects of async code

The normative rules are in [08](08-effects-and-capabilities.md) §8.10 (E-AsyncCall, E-Await). In summary:

| Construct | Contribution to the enclosing body |
|---|---|
| a call `f<C̄>(ā)` of an `async fn` | the effects of the arguments only. Constructing the future is pure |
| evaluating `async { … }` or `async move { … }` | `∅` |
| `e.await` with `e : Fut_f<C̄>` | `Sig(f) ∪ Lat(f; C̄)` |
| `e.await` with `e : Fut_b` | the inferred effect set of the block body (§11.2.3 item 5) |
| `e.await` with `e : Q` for a concrete type `Q` with a hand-written impl | the effects of `Q`'s `poll` impl, as for a call (08 §8.7.2) |
| `e.await` with `e : F` for a type parameter `F: Future` | `∅`. The effects are latent in `F` (08 §8.8) |
| `for await` | as `.await`, with `poll_next` in place of `poll` |

The `effects` clause of an `async fn` bounds the effects of its body. A caller that only constructs the future needs none of them. A caller that awaits it needs all of them.

`sai:async` receives no special treatment (N-I-12). Its combinators are generic over their futures, so the general latent-effect rule attributes the effects of every operand to the code that instantiates the combinator:

- Awaiting `scope(body)` contributes `{alloc} ∪ Lat(scope; B, F)`. That includes the effects of calling `body` and of polling `F`, the body future. The body future's own effects include, at each `x.spawn(c)` call in it, `{alloc} ∪ Lat(spawn; C)`, which is the effect set of polling the child `c`. The effects of every child are therefore attributed to the body in which it is started, and from there to the function that awaits the scope.
- Awaiting `join(a, b)` contributes the poll effects of both operands. `select` likewise, even though only one operand completes: an effect is attributed when it may happen (P2).
- Calling `block_on(f)` contributes `{alloc} ∪ Lat(block_on; F)`, which is the poll effect set of `f` (§11.9.1). `block_on` is the one place where the effects of a future are attributed to a *synchronous* call.
- A bound `F: Future` MAY carry an effect cap ([08](08-effects-and-capabilities.md) §8.8.3). An executor or combinator written for pure futures only can require it.
- Waiting for WASI pollables inside `block_on` is not an effect of its own (§11.10.2).
- The unsafety of `poll` and `poll_next` is not an effect ([08](08-effects-and-capabilities.md) §8.1).

---

## 11.9 Executors

### 11.9.1 `block_on`

```text
// sai:async (signature listing)
pub fn block_on<F: Future>(future: F) -> F.Output
    effects { alloc }
```

`block_on` drives one future, the **root task**, to completion on the calling thread and returns its output (N-I-16):

1. It moves `future` into a slot owned by the call. The slot is not moved until the call returns.
2. It creates a root waker. Waking it sets the root's **woken flag**. The waker is reference-counted. Clones of it may outlive the call, and waking or dropping a clone after `block_on` has returned is a no-op.
3. It creates a reactor (§11.10.2) local to the call. There is no global executor state (D-CAP-02).
4. It repeats the following steps:
   1. Clear the woken flag. Poll the root with a context whose waker is the root waker and whose executor extension refers to the reactor.
   2. If the result is `.Ready(v)`: drop the root future in place, mark the waker as dead, release the reactor, and return `v`.
   3. If the result is `.Pending` and the woken flag is set: go to step 4.1.
   4. If the result is `.Pending`, the flag is not set, and the reactor holds at least one registered pollable: block until at least one of them is ready, wake the tasks registered on the ready ones (§11.10.2), and go to step 4.1.
   5. Otherwise, no wake can ever arrive: the instance has one thread and nothing is pending in the host. `block_on` panics with the message `"block_on: deadlock"` ([10](10-errors-and-panics.md)).

`block_on` adds no nondeterminism of its own: given the same results from the host, it performs the same polls in the same order.

**Nesting.** `block_on` MAY be called inside a future that is itself being driven by an executor, including another `block_on`. The inner call drives its own root to completion with its own reactor before it returns. While it runs, no other task of the outer executor makes progress, because the instance has one thread. Pollables registered with the outer reactor are not waited on by the inner one. A program in which the inner root waits for an outer task therefore panics with the deadlock message instead of hanging.

### 11.9.2 Synchronous driving

`block_on` is how synchronous code uses asynchronous code (R-04). In particular, a component can implement an exported interface function synchronously before the Component Model async ABI is available, or on a target without it (§11.10.4):

```sai
impl users {
    fn fetch_user(id: U64) -> Result<users.User, users.UserError>
        effects { net.http, alloc }
    {
        block_on(load_user(id))
    }
}
```

Here `users` is the interface of R-04, and `load_user` is an `async fn load_user(id: U64) -> Result<users.User, users.UserError>` with `effects { net.http, alloc }`, written like `fetch_user` in R-04. The effects of `load_user`'s body are attributed to the synchronous call of `block_on` (§11.8), so the clause of the exported function names them as [08](08-effects-and-capabilities.md) §8.9.3 requires.

On a target without WASI, such as `wasm32-core` ([targets](../targets/targets-and-profiles.md)), there are no pollables. `block_on` there completes only futures that make progress through their own wakes, and reports every other wait as a deadlock (step 4.5).

### 11.9.3 Executors are library code

The language defines no executor. `block_on`, `scope`, `join`, `join_all` and `select` are ordinary code in `sai:async` that uses `unsafe` to call `poll`. The scheduler, wakers and reactor are linked only when reachable (D-WASM-05, [impl/10](../impl/10-runtime.md)). A program that never uses `sai:async` contains no scheduler.

### 11.9.4 Obligations of executors and combinators

Any code that calls `poll` or `poll_next`, whether in `sai:async` or in a user package, is unsafe code and MUST:

1. satisfy the caller obligations of §11.4.2 for every operand it polls: no move after the first poll, drop in place, a valid context, exclusive access;
2. never poll a future re-entrantly: a `wake` or `wake_by_ref` that runs during a `poll` MUST NOT synchronously poll the same task. It only records the wake;
3. poll a task at least once after every wake of that task, unless the task is dropped first;
4. drop every operand it owns when it is dropped, in an order it documents. The combinators of `sai:async` use the orders of §11.7.7;
5. make waking a completed or dropped task a no-op.

A violation of items 1 and 2 is undefined behavior ([12](12-unsafe-and-ffi.md) §12.6). A violation of items 3 to 5 is a bug in the library but not undefined behavior, unless it also breaks item 1 or 2.

---

## 11.10 WASI and the Component Model

### 11.10.1 Status

This section has two parts with different status:

- §11.10.2, the use of WASI 0.2 `wasi:io/poll`, is **normative**. WASI 0.2 is stable (D-TGT-01).
- §11.10.3, the Component Model async ABI, is **provisional**. The async ABI of the Component Model was not final when this chapter was written. The rules marked *Provisional* state the intended mapping. They MAY change when the upstream ABI is final, and such a change is not a breaking change of the language (P6 covers source, not the binary interface of async exports). The ABI details belong to [13](13-components-and-wit.md) and [impl/07](../impl/07-component-lowering.md).

### 11.10.2 Pollables and the reactor

WASI 0.2 represents "an operation that may complete later" by a `pollable` resource of `wasi:io/poll`. A pollable has `ready() -> bool` and `block()`, and the interface has `poll(in: list<borrow<pollable>>) -> list<u32>`, which blocks until at least one pollable is ready and returns the indices of the ready ones.

Each `block_on` call owns a **reactor** (§11.9.1). The reactor is reached through the executor extension of `Context` (§11.4.3) (N-I-17):

1. A **leaf future** is a hand-written future that wraps a WASI operation, for example in an I/O library. When polled, it checks whether its pollable is ready. If it is not, it registers the pollable and a clone of the context's waker with the reactor, and returns `.Pending`.
2. Registrations are one-shot. A leaf future registers again on every poll that returns `.Pending`. A registration is removed when the reactor's next wait returns, or earlier when the registration is dropped. A leaf future that is cancelled drops its registration before its pollable, so the reactor never holds a dead handle.
3. When the executor finds no woken task (§11.9.1 step 4.4), the reactor calls `wasi:io/poll.poll` once, with the registered pollables in registration order. For each returned index, in ascending order, it wakes every waker registered for that pollable. Then the executor polls again.
4. A context without a reactor (one made by `Context.from_waker`) cannot register. A leaf future polled with such a context MUST call `block()` on its pollable and then continue as if it had been woken. This keeps hand-written executors correct, at the cost of concurrency.
5. **Effects.** The reactor's calls of `poll` and `block` contribute no effect of their own. Every effect that produces a pollable (for example `net.http` or `fs.read`) already implies use of `wasi:io`, and the operation that created the pollable already carries that effect. This requires that the capability of every WASI effect implies `wasi:io/poll`, which [targets](../targets/targets-and-profiles.md) §3.3 item 5 states for the WASI capabilities.

The registration API between leaf futures and the reactor is assumed to be:

```text
// sai:async (signature listing; wasm32-wasi and wasm64-wasi only)
pub type Registration { … }                // not Copy; dropping it removes the registration

pub unsafe fn register(cx: borrow mut Context, pollable: borrow Pollable) -> Option<Registration>
```

`register` returns `None` when `cx` has no reactor. It is `unsafe` because the caller MUST keep the pollable alive until the `Registration` is dropped. The registration does not carry the pollable's region, so a leaf future can store both.

### 11.10.3 The Component Model async ABI (Provisional)

When the target supports the Component Model async ABI, async interface functions lower to it ([13](13-components-and-wit.md), [impl/07](../impl/07-component-lowering.md)). No v0.1 target supports it ([targets](../targets/targets-and-profiles.md) §3.4), so the mapping below applies to no v0.1 build: async exports are `ASYNC_EXPORT_UNSUPPORTED` (§11.10.4), and imported async WIT items are rejected where they are used (targets §3.4 item 2). The intended mapping is:

1. **Exported `async fn`** (an `async fn` in an interface impl exported by the world, 13 §13.3.2). The export uses the **callback** form. The entry point lifts the arguments, constructs the future, and polls it once with a context whose waker marks the export task as woken. On `.Ready(v)` it lowers `v` and calls `task.return`. On `.Pending` it returns a request to wait on the waitable set that holds the waitables the task registered. The host calls the callback when an event arrives, and the callback polls the future again. The export never blocks the instance, so the host can run other exports of the same component concurrently between polls.
2. **Imported `async fn`.** An interface function that is `async` in WIT is an `async fn` in SAI ([13](13-components-and-wit.md)). Calling it constructs a future and starts nothing. The first poll starts the subtask through the async-lowered import. Dropping the future before the subtask completes requests `subtask.cancel` and then drops the subtask. That cancellation is synchronous from the point of view of SAI code, like every drop.
3. **`future<T>` and `stream<T>`.** These WIT types map to concrete types of `sai:async`: `FutureReader<T>`, which implements `Future` with `Output = T`, and `StreamReader<T>`, which implements `Stream` with `Item = T`, plus the writer ends `FutureWriter<T>` and `StreamWriter<T>`. `Future` and `Stream` are traits, not types (C-02), so the WIT mapping cannot use them directly.
4. **Canonical built-ins** (`task.return`, `waitable-set.*`, `subtask.cancel`) are part of the ABI. They are not WIT imports, and they carry no effect.
5. In an async export, the reactor also waits on CM waitables. A WASI 0.2 pollable is waited on through the host's adapter between the two models. The details belong to [impl/10](../impl/10-runtime.md).

### 11.10.4 Availability

(N-I-18):

- An `async fn` that is a core export (`export async fn`, [03](03-grammar.md) `export` on a function) is the error `ASYNC_EXPORT_UNSUPPORTED`. Core exports are scalar-only and synchronous (D-WASM-04).
- An exported interface whose functions include an `async fn`, on a target without the Component Model async ABI, is `ASYNC_EXPORT_UNSUPPORTED`. Its fix suggests the synchronous form of §11.9.2.
- Before Stage 5, any `async fn` in an interface is `FEATURE_NOT_IMPLEMENTED` ([13](13-components-and-wit.md) §13.13).

### 11.10.5 No stack switching

Async functions and blocks lower to state machines in MIR ([impl/04](../impl/04-mir.md)). The compiler MUST NOT rely on stack switching, JavaScript promise integration or any other engine-specific suspension mechanism. Every async program runs on every engine that supports the target's baseline (P4).

---

## 11.11 Threads

v0.1 has no threads (D-THREAD-01):

1. No package of the standard distribution provides an API that starts a thread, shares linear memory, or uses Wasm atomics. The capability `threads` and the effect `thread.spawn` exist as names only ([08](08-effects-and-capabilities.md) §8.3). No v0.1 target provides `threads`, so a package that requires it is rejected with `TARGET_CAPABILITY_UNAVAILABLE` (08, [targets](../targets/targets-and-profiles.md)).
2. Threads are not assigned to any stage of v0.1. They will be designed together with the auto traits below.
3. **`Send` and `Share` are reserved** for future auto traits (D-THREAD-01). SAI has no auto traits in v0.1 (D-TYPE-08). To keep the names free, declaring an item named `Send` or `Share` in any module, or importing an item under one of these names, is `RESERVED_TYPE_NAME` ([04](04-modules-and-names.md) N-A-9 applied to these names) (N-I-19).
4. Instead of `Send` and `Share`, v0.1 relies on two facts. Every instance runs on one thread, and the borrow rules of [07](07-ownership-and-borrowing.md) apply between tasks through the scope region rule (§11.7.3). Futures, closures, wakers and executors therefore need no thread-safety bounds, and `scope` does not require its children to be `Send`. When threads are added, code that is valid in v0.1 will remain valid for single-threaded use (P6).

---

## 11.12 Why safe code has no data races

A **data race** is two accesses to the same memory location from different threads, not ordered by synchronization, at least one of them a write. Safe SAI code cannot have one, for the following reasons:

1. **One thread per instance.** A v0.1 instance executes on exactly one thread (§11.11). There is no second thread inside the instance that could access its memory.
2. **No shared memory.** Linear memory is not shared between instances. The Component Model gives each component instance its own memory, and values cross component boundaries only by copying through the Canonical ABI ([13](13-components-and-wit.md)). SAI does not use shared memories.
3. **Cooperative interleaving.** Tasks interleave only at suspension points (§11.7.1 item 3). Between two suspension points a task has the instance to itself. Even when the host runs several async exports of one instance concurrently (§11.10.3), they interleave at suspension points only.
4. **Aliasing XOR mutability across tasks.** A task that holds a `borrow mut` across an `.await` excludes every other access to that place, including from sibling tasks, for as long as the loan is live (§11.5.4, §11.7.3). Logical races between tasks are therefore limited to state the tasks own or to explicit library types.
5. **No global mutable state.** There are no statics and no global mutable variables (D-CAP-02). Tasks can share data only through borrows checked by §11.7.3 or by moving owned values.
6. **No interior mutability.** Safe SAI has no interior mutability ([07](07-ownership-and-borrowing.md), [09](09-contracts-and-refinements.md) N-C-58). Its one exception, the fields of an `Arena`, is not observable from safe code ([12](12-unsafe-and-ffi.md) §12.6.4 rule 5). A shared borrow held by one task guarantees that no other task changes the place. State that executors change behind a waker lives in memory reached only through raw pointers, inside `sai:async`.

Only unsafe code can break these guarantees, for example by letting a native thread access SAI memory through `extern "C"`. That is undefined behavior ([12](12-unsafe-and-ffi.md) §12.9.6).

---

## 11.13 Async code in tests

`test` and `property` bodies are synchronous: `Γ.async` is false in them (§11.3.1) (N-I-20). There is no `async test` form. A test that exercises async code drives it with `block_on`:

```sai
use sai:async.{block_on}

test "countdown sums its items" {
    let sum = block_on(total(4))
    assert sum == 6, "expected 6"
}
```

- `sai:async` MUST be a dependency of the package, either in `requires` or in `requires test` (D-PKG-01, D-PKG-04). The rules for using test-only dependencies are in [14](14-testing.md) §14.2.5.
- `.await` written directly in a test body is `AWAIT_OUTSIDE_ASYNC`. Its fix wraps the body in `block_on(async move { … })`.
- Tests are hermetic ([14](14-testing.md) §14.3). The futures a test can drive therefore perform no host I/O, and `block_on` registers no pollables. A test whose future waits for something that never happens panics with `"block_on: deadlock"` and fails, rather than hanging.
- `block_on` is deterministic (§11.9.1) and there is no clock in a hermetic test, so async tests are reproducible. The rules of [14](14-testing.md) §14.6 apply.

---

## 11.14 Decisions introduced by this chapter

These entries are provisional until they are merged into [decisions.md](../design/decisions.md). IDs are shared with [12](12-unsafe-and-ffi.md) and [14](14-testing.md).

| ID | Rule | Rationale | Section |
|---|---|---|---|
| N-I-1 | There are no async closures. `async fn(…)` and `async move fn(…)` in expression position are `NO_ASYNC_CLOSURE`, with a fix to a closure returning an `async move` block. | P7: a closure returning an async block expresses the same thing with no new type form. P1: the two boundaries stay visible. | §11.2.5 |
| N-I-2 | `.await` and `for await` are allowed only when the innermost body is an `async fn` or `async` block. Closures, tests, properties, consts, contracts and `Drop.drop` are never async. Otherwise `AWAIT_OUTSIDE_ASYNC`. | P8: suspension points are visible from the enclosing body alone. P1. | §11.3.1 |
| N-I-3 | `async fn` is rejected in traits and trait impls (`ASYNC_FN_IN_TRAIT`), in extern blocks (`INVALID_EXTERN_SIGNATURE`) and as `main` (`ASYNC_MAIN`). A future type that contains itself without a `Box` is `ASYNC_RECURSION`. Run-time `requires` checks of an `async fn` run at the call, and `ensures` checks run at completion. | P7 and C-02: no implicit associated types without opaque types. P8: contract failures are reported at the caller. P2: infinite-size futures are rejected statically. | §11.2.1, §11.2.4, §11.2.6 |
| N-I-4 | There is no `Future` or `Stream` impl for `borrow mut F`. `Box<F>` implements `Future` (and `Box<S>` `Stream`) by forwarding. | P9: a polled future is never reachable through a borrow from safe code, which is what makes `Pin` unnecessary. `Box` keeps recursion possible. | §11.3.2, §11.6.1 |
| N-I-5 | Polling a completed anonymous future panics with `"future polled after completion"`. | P9: memory-safe and loud. Safe code cannot reach it. | §11.3.4 |
| N-I-6 | The `poll` contract consists of no move after the first poll, the drop guarantee (no freeing or reuse of a polled future's storage without dropping it in place), a valid context and exclusive access. Implementations must tolerate spurious polls and stay memory-safe after `.Ready`. | P9: this is the minimum needed for self-referential frames, stated where unsafe code can check it. | §11.4.2 |
| N-I-7 | `regions(Fut_f)` is the union of the regions of all parameter types. `regions(Fut_b)` is the regions of its captures. Output regions follow the elision rules of 07. | P2 and P8: a future may use any argument until it is dropped. This refines 07 N-B-25 without weakening it. | §11.5.3 |
| N-I-8 | Dropping a suspended future drops the occupied await slot first, then the locals of the active scopes innermost-first in reverse declaration order, then the parameters (reverse order, `self` last) or the by-move captures (capture order). An unpolled future drops only its parameters or captures. | P8: cancellation order is exactly the order of an early `return` at the suspension point, with the inner future first. | §11.5.5 |
| N-I-9 | `for await` takes a `Stream` by value (no `IntoStream`), keeps it in a hidden pinned slot, and expands to a `loop` over an `await_next` suspension point. The stream is dropped after the locals of the body when the loop is left. | P7 and P8: one expansion, no conversion trait. | §11.6.2 |
| N-I-10 | Because `async` is a strict keyword, `sai:async` is imported with an item list or an alias. The specification writes `scope`, `join` and `select` for D-ASYNC-01's `async.scope` and so on. | Consistency with 04 N-A-5 (`USE_BINDING_IS_KEYWORD`). | §11.1.3 |
| N-I-11 | `scope` is a designated item ([16](16-core-library.md) N-J-26), not a lang item, with a fresh region `'s`. The body future and every child may mention `'s`. All their other regions must outlive `'s` (`CHILD_TASK_BORROWS_LOCAL`). The scope's output must not mention `'s` (`SCOPE_REGION_ESCAPE`). A child's loans are live until the scope ends. | P2: aliasing XOR mutability across concurrent tasks, checked by the ordinary borrow checker, without `Send` or higher-ranked regions in the surface language (P7). | §11.7.3 |
| N-I-12 | The combinators and executors of `sai:async` get no special effect rule. Their effects follow from the latent-effect rule of 08, so a child's effects are attributed at its `spawn` call. | P7 and P8: one effect rule for all generic code. | §11.8 |
| N-I-13 | A scope polls, in each round, the body first and then the woken or never-polled children in start order, including children started during the round. It completes when the body and all children have completed. On cancellation it drops the body first, then the children in reverse start order. | P8: deterministic scheduling given the host's wakes. Reverse order mirrors D-OWN-05. | §11.7.4, §11.7.7 |
| N-I-14 | The child task method is `Scope.spawn`, the name of the draft's vocabulary (`spawn` is not a reserved word, D-LEX-04). Dropping a `JoinHandle` does not cancel its child. `cancel` drops it synchronously. `join` consumes the handle. | P1: cancellation is explicit. "The scope waits for all children" holds unconditionally (D-ASYNC-01). | §11.7.5 |
| N-I-15 | `join` and `select` pass the parent context through and poll every incomplete operand on every poll, in argument order. `select` is left-biased and drops the loser before returning. `join_all` uses per-operand wakers. There is no `race`. | P8: deterministic selection. P7: one selection primitive. | §11.7.6 |
| N-I-16 | `block_on` drives one root task with a refcounted waker and a reactor local to the call. It polls again while woken, waits on pollables when there are some, and otherwise panics with `"block_on: deadlock"`. Nesting is allowed. A wake after return is a no-op. | P8 and P9: no hangs that can be detected, and no global executor (D-CAP-02). R-04. | §11.9.1 |
| N-I-17 | WASI leaf futures register pollables, one-shot, with the reactor of the context's executor extension. The reactor calls `wasi:io/poll.poll` with the registrations in order and wakes in ascending index order. Without a reactor, a leaf future blocks on its pollable. Waiting is not an effect. The capability of every WASI effect implies `wasi:io/poll`. | P4: plain WASI 0.2. P8: deterministic wake order. P5: the effect lies with the operation that created the pollable. | §11.10.2 |
| N-I-18 | A core `export async fn`, or an exported interface with an `async fn` on a target without the CM async ABI, is `ASYNC_EXPORT_UNSUPPORTED`. WIT `future<T>` and `stream<T>` map to `FutureReader<T>` and `StreamReader<T>` (provisional). Async lowers to state machines, never stack switching. | P4: portable lowering. C-02: `Future` is not a type. | §11.10.3–§11.10.5 |
| N-I-19 | `Send` and `Share` cannot be declared or used as import aliases (`RESERVED_TYPE_NAME`). No thread API exists in v0.1. | P8: the future auto traits can be added without breaking code. | §11.11 |
| N-I-20 | There is no async test form. Tests use `block_on`, and `.await` in a test body is `AWAIT_OUTSIDE_ASYNC` with a fix. | P7: one test form. P8: tests are synchronous and deterministic. | §11.13 |

---

## 11.15 Diagnostics

All codes are errors. Codes defined in another document are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `ASSIGN_WHILE_BORROWED` | error | defined in [07](07-ownership-and-borrowing.md) §7.9.6; a scope body writes a place that a child borrows (§11.7.3) |
| `ASYNC_EXPORT_UNSUPPORTED` | error | an `async fn` exported as a core export, or through an interface on a target without the Component Model async ABI (§11.10.4) |
| `ASYNC_FN_IN_TRAIT` | error | an `async fn` declared in a trait or implemented in a trait impl (§11.2.6) |
| `ASYNC_MAIN` | error | the entry point `main` is declared `async` (§11.2.6) |
| `ASYNC_RECURSION` | error | the future type of an async function contains itself without a `Box` (§11.2.4) |
| `AWAIT_OUTSIDE_ASYNC` | error | `.await` or `for await` where the innermost body is not an `async fn` or `async` block (§11.3.1) |
| `CHILD_TASK_BORROWS_LOCAL` | error | a future passed to `Scope.spawn`, or the body of `scope`, borrows something that does not outlive the scope (§11.7.3) |
| `CONFLICTING_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.9.6; two children hold conflicting loans of one place (§11.7.3) |
| `DOES_NOT_LIVE_LONG_ENOUGH` | error | defined in [07](07-ownership-and-borrowing.md) §7.9.6; a future outlives a local it borrows (§11.5.3) |
| `EXPECTED_TOKEN` | error | defined in [03](03-grammar.md) §3.16 |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; async syntax before Stage 5 (§11.1.2) |
| `INVALID_BREAK_VALUE` | error | defined in [06](06-expressions-and-statements.md) §6.16.2; `break` with a value in `for await` |
| `INVALID_CONTRACT_EXPRESSION` | error | defined in [09](09-contracts-and-refinements.md) §9.1.2; `.await` in a contract |
| `INVALID_EXTERN_SIGNATURE` | error | defined in [12](12-unsafe-and-ffi.md) §12.7.4; an `async fn` in an extern block |
| `MOVE_OUT_OF_BORROW` | error | defined in [07](07-ownership-and-borrowing.md) §7.17; awaiting a place behind a borrow |
| `MOVE_OUT_OF_BOX` | error | defined in [07](07-ownership-and-borrowing.md) §7.3.5 |
| `NO_ASYNC_CLOSURE` | error | `async` before a closure expression (§11.2.5) |
| `REFUTABLE_PATTERN_IN_FOR` | error | defined in [06](06-expressions-and-statements.md) §6.16.3; a refutable pattern in `for await` |
| `RESERVED_TYPE_NAME` | error | defined in [04](04-modules-and-names.md) §4.4.4; also an item or import alias named `Send` or `Share` (§11.11) |
| `SCOPE_REGION_ESCAPE` | error | the output of a scope mentions the scope region, for example a `JoinHandle` or `borrow Scope` (§11.7.3) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; the `threads` capability (§11.11) |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14; an async function item coerced to a function pointer (§11.2.2) |
| `UNSATISFIED_BOUND` | error | defined in [05](05-types.md) §5.11.2; the operand of `.await` is not a `Future`, or the operand of `for await` is not a `Stream` |
| `UNUSED_MUST_USE` | error | defined in [06](06-expressions-and-statements.md) §6.25.2; a discarded anonymous future, or a discarded value of a `@must_use` future type (§11.3.2) |
| `USE_AFTER_MOVE` | error | defined in [07](07-ownership-and-borrowing.md) §7.4.4; a future used after `.await` consumed it |
| `USE_BINDING_IS_KEYWORD` | error | defined in [04](04-modules-and-names.md) §4.3.3; `use sai:async` without an alias or item list (§11.1.3) |
