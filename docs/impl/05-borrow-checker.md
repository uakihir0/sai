# 05. Borrow Checker

**Status:** Normative for the reference implementation
**Stage:** 2 (borrows held across `.await`: Stage 5)
**Related:** [../spec/07-ownership-and-borrowing.md](../spec/07-ownership-and-borrowing.md), [04-mir.md](04-mir.md), [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md), [01-architecture.md](01-architecture.md), [02-frontend-ast-hir.md](02-frontend-ast-hir.md), [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), [11-diagnostics.md](11-diagnostics.md), [15-conformance-and-testing.md](15-conformance-and-testing.md), [../spec/06-expressions-and-statements.md](../spec/06-expressions-and-statements.md), [../spec/11-async-and-concurrency.md](../spec/11-async-and-concurrency.md), [../spec/16-core-library.md](../spec/16-core-library.md); [decisions.md](../design/decisions.md) D-MIR-01, D-OWN-01..07, D-TYPE-04, D-TYPE-09, D-ERR-01, D-ASYNC-01, D-TOOL-01

This document specifies how the reference compiler decides the ownership and borrow rules of [07](../spec/07-ownership-and-borrowing.md) on MIR. It covers the two passes of the checker: move checking (move paths, initialization dataflow, move legality, mutability) and borrow checking (region variables, constraint generation, the region solver, loans in scope, the access-conflict table, two-phase borrows, drops, closures, arenas, and `.await`). It also covers error reporting with structured fixes, complexity, determinism, incremental compilation, worked examples, and test hooks. The MIR data structures belong to [impl/04](04-mir.md) (D-MIR-01). §5.2 lists every MIR property this document relies on. The language rules belong to 07. This document defines an algorithm that decides them and fixes the details that 07 leaves to the implementation: which diagnostic is reported when several apply, which spans and labels it carries, and when a fix is machine-applicable.

---

## 5.1 Overview

### 5.1.1 Goal and authority

1. 07 is authoritative. The checker MUST accept exactly the programs that 07 accepts and MUST report the diagnostic codes that 07 names. A disagreement between this document and 07 is a defect of this document (README, order of authority).
2. The checker is **deterministic** (decisions §0.1). For the same input, it produces the same diagnostics, the same labels, the same fixes, and the same fact dumps (§5.20), byte for byte. It does not depend on hash-map iteration order, thread scheduling, addresses, or time.
3. The checker has **no budgets**. No iteration limit, timeout, or size threshold changes a result. Every procedure in this document terminates by construction, within the bounds of §5.17.
4. The checker does not modify MIR. Drop elaboration, which rewrites MIR, belongs to impl/04. It consumes the results of pass A (§5.12).

### 5.1.2 Pipeline

The checker consists of two queries per root owner (an item with a body, impl/02 §2.8.2). Closures and `async` blocks are not owners (impl/02). Their bodies are checked together with their root owner (N-B-31).

```text
HIR + TypeckResults (impl/03)
  -> mir_built(owner)               impl/04; phase Built
  -> move_check(owner)              pass A: §5.3, §5.4, §5.12.1
  -> mir_drop_elaborated(owner)     impl/04; uses MoveCheckResults; phase DropElaborated
  -> borrowck(owner)                pass B: §5.5 to §5.16
  -> mir_analyzed(owner)            impl/04; phase Analyzed
```

- **Pass A, `move_check`**, runs on Built MIR. It computes move paths and the MaybeInit and MaybeUninit dataflows. It reports move errors, use-after-move errors, and the structural errors of writes (`ASSIGN_TO_IMMUTABLE`, `BORROW_MUT_OF_IMMUTABLE`, `INVARIANT_FIELD_WRITE`). Its results classify every `Drop` for drop elaboration (§5.12.1).
- **Pass B, `borrowck`**, runs on DropElaborated MIR. It builds the region problem, solves it, computes the loans in scope, checks every access against them, checks the solution against the signature, and reports loan conflicts and region errors.

The split is N-B-29. Pass B needs elaborated drops, because only the drops that remain after elaboration make a local drop-live (spec/07 §7.9.4). Drop elaboration in turn needs the initialization results of pass A.

**Phase gating.** Following impl/11 §11.11.4, pass A runs on a body only if name resolution and type checking of that body reported no error. Pass B runs whenever pass A ran, including when pass A reported errors, but it suppresses diagnostics that pass A has already explained (§5.16.6, N-B-30). A body for which pass A or pass B reports an error never advances to phase Analyzed. A diagnostic of either pass whose cause path involves the signature of a callee that has a signature error is suppressed as cascade C4 (impl/11 §11.11.4). Its cause key includes the callee signatures on the cause path.

### 5.1.3 Queries

| Query | Input | Output | Section |
|---|---|---|---|
| `region_signature(def)` | a function, method, or trait method `DefId` | its universal regions, assumptions, implied bounds, and signature diagnostics | §5.5.2 |
| `move_check(owner)` | a root owner | `MoveCheckResults`: per body, the move data, the drop classification, `used_mut`, and diagnostics | §5.3, §5.4 |
| `borrowck(owner)` | a root owner | `BorrowckResults`: diagnostics and an error flag | §5.5 to §5.16 |
| `item_regions(def)` | a type declaration or an `impl` | `RESOURCE_CONTAINS_BORROW` for a resource, the region binding of N-B-12 and `AMBIGUOUS_IMPL_REGION` for an impl | §5.5.3 |
| `borrowck_facts(owner)` | a root owner | the facts and the solution in the dump format of §5.20.1 | §5.20 |

impl/04 reads the results through its hooks (impl/04 §4.1.4): `drop_classes(owner)` is the drop classification of `move_check` (§5.12.1), in the value type `DropClasses` that impl/04 defines, and `borrowck_failed(owner)` is the error flag of §5.16.6.

`region_signature`, `item_regions`, `move_check`, and `borrowck` are check queries in the sense of impl/01 §1.9: their values carry diagnostics. A signature diagnostic is reported once, by the query of the item, and never by the bodies that call it. `used_mut` is the set of `var` bindings whose mutability some access needs (§5.4.6). The `UNNECESSARY_VAR` lint uses it ([15](../spec/15-attributes-and-lints.md) §15.18.2, N-A-65).

### 5.1.4 Notation

- `x`, `y` are locals. `p`, `q` are places in the canonical form `x.π` of 07 §7.2.2. `m` is a move path. `'r`, `'l` are region variables. `L` is a loan.
- A **point** is the position of one statement or one terminator: `bbB[i]` is statement `i` of block `B`, and `bbB[n]` with `n` the number of statements of `B` is its terminator. In a joint problem (§5.13), a point also names its body.
- MIR listings use the informal rendering below. The authoritative syntax of MIR dumps belongs to impl/04.

```text
bb0: {
    StorageLive(_2)
    _2 = Borrow(Shared, _1)              // statement bb0[1]
    _3 = Call print_len(move _2) -> bb1  // terminator bb0[2]
}
```

- `type(p)` is the type of a place after substitution. `regions(T)` and `droplive(T)` are the functions of 07 §7.6.2 and §7.13.7.

---

## 5.2 MIR inputs relied on

### 5.2.1 Constructs of D-MIR-01

The checker uses these constructs of D-MIR-01 with the meaning given there:

| Construct | Used by |
|---|---|
| `Body { locals, blocks, arg_count, return_local: _0, source_scopes, phase }` | everything |
| projections `Field`, `Deref`, `Index(Local)`, `ConstIndex { offset, from_end }`, `Subslice { from, to }`, `Downcast` | move paths (§5.3), overlap (§5.10.2), reborrow walk (§5.6.3) |
| operands `Copy`, `Move`, `Const` | uses and moves (§5.4), value flow (§5.6) |
| rvalues `Use`, `Borrow(region, Shared \| Mut \| TwoPhaseMut, place)`, `RawPtr`, `BinaryOp`, `CheckedBinaryOp`, `UnaryOp`, `Convert`, `Aggregate`, `Discriminant`, `Len`, `Repeat`, `Closure` | accesses (§5.10.1), constraints (§5.6.2) |
| statements `Assign`, `StorageLive`, `StorageDead`, `SetDiscriminant`, `FakeRead`, `Nop`, and the impl/04 statements `ProofObligation` and `CheckPoint`, which access nothing (impl/04 §4.2.5) | §5.4, §5.10 |
| terminators `Goto`, `SwitchInt`, `Return`, `Unreachable`, `Call { func, args, dest, target, effects }`, `Drop { place, target }`, `Assert`, `Await { future, dest, resume }`, `Trap` | CFG, §5.4, §5.6, §5.10 |
| no cleanup edges | §5.10.4: nothing runs after a `Trap` |
| phases `Built`, `DropElaborated`, `Analyzed` | §5.1.2 |

The region of a `Borrow` is a per-body index (impl/04 §4.2.4). The checker maps it to the loan's fresh variable (§5.5.4 item 2). Every region inside a MIR type is `Erased`, except `Static` for the regions of literal data (impl/03 §3.5.2, §3.15.2 invariant 3). The checker maps `Static` to `'static` and replaces every `Erased` region by a fresh region variable (§5.5.4). impl/04 does not lower unreachable code (impl/04 §4.2.1 `unlowered`), so the checker never sees it.

### 5.2.2 Facts required beyond D-MIR-01

The checker needs the following facts, which D-MIR-01 does not state. They are requirements on impl/04 (N-B-34). If impl/04 provides them under other names, its names govern, and the meaning given here is required.

| # | Fact | Used by |
|---|---|---|
| F1 | Every `LocalDecl` has `ty`, `mutability: Mut \| Not`, a span, the source scope of its declaration, and an `origin`: `Return`, `Param(index)`, `UserVar(BindingId)`, `Temp`, `ArenaStorage(HirId)`, `ClosureEnv`, `GuardScrutineeRef`, `DropFlag`, `AwaitSlot`. `mutability` is `Mut` exactly for `var` bindings, `var` parameters, and temporaries (07 §7.2.4 rules 1 and 8). Further origins that impl/04 defines are treated as `Temp`. | §5.4.6, §5.10.5, §5.16 |
| F2 | Every statement and terminator has `SourceInfo { hir: HirId, scope }`. The final write of an assignment expression `p = e` or `p op= e` is exactly one `Assign` or `Call` destination whose `hir` is that assignment (impl/02 `ExprKind::Assign`, `StmtKind::AssignOp`). | §5.4.6, §5.16 |
| F3 | `Drop` carries `kind: ScopeExit \| Replace`. `Replace` is the drop of the old value by an assignment (spec/07 §7.3.6 step 4). It is immediately followed by the write of the same place. | §5.10.6 |
| F4 | `Await` has a fourth successor, `cancel`. The blocks reachable from `cancel` contain only `Drop` terminators, `StorageDead` statements, and `Goto`, and end in a terminator `CancelExit` with no successors. They perform the drops of spec/07 §7.16 item 5 in the order of 11 §11.5.5 (N-I-8). `CancelExit` is removed by the async transform together with `Await`. | §5.15 |
| F5 | `FakeRead` carries a cause: `ForMatch` (the scrutinee of `match` and `if let`), `ForLet` (the scrutinee of `let .. else`), or `ForGuard` (§5.10.7). | §5.10.1, §5.10.7 |
| F6 | Match guards: across every guard, a local `_sr` with origin `GuardScrutineeRef` holds `Borrow(Shared, p)` of the scrutinee place `p`. It is created after the scrutinee is evaluated and before any binding of any arm is made. This is the shallow shared borrow of impl/03 N-E-38. If impl/04 gives it a borrow kind of its own, that kind replaces the origin test. Each successor of the guard's `SwitchInt` starts with `FakeRead(ForGuard, _sr)`. The guard never moves a binding of its arm: by-value bindings are made after that `FakeRead`. | §5.10.7 |
| F7 | A two-phase borrow is `_t = Borrow(TwoPhaseMut, p)` with `_t` a `Temp` that is assigned once and used once, as a `Move` argument of a `Call`. That call is the activation. The construction emits `TwoPhaseMut` exactly in the cases of 07 §7.10, which include the implicit reborrow of a receiver that already has type `borrow mut Self` (07 §7.10 case 1). | §5.11 |
| F8 | A closure value is `Closure(def, ops)` with the operands in capture order (07 §7.11.2). A by-borrow capture of `c` is a temporary `_k = Borrow(Shared \| Mut, c)` passed as `Move(_k)`. A by-move capture is `Move(c)`, or `Copy(c)` for Copy. An `async` block value is built the same way, with the `DefId` of the block. Each closure and `async` block has its own `Body`, keyed by its `DefId` (impl/02 `DefPathData::Closure`, `AsyncBlock`). | §5.13, §5.15 |
| F9 | In a closure body, `_1` has origin `ClosureEnv` and type `borrow Self` (`Fn`), `borrow mut Self` (`FnMut`), or `Self` (`FnOnce`, and every `async` block). Capture `i` is the place `(*_1).i`, or `_1.i` for a `Self` environment. A by-borrow capture is used through `*((*_1).i)`. | §5.4.5, §5.13 |
| F10 | `region r { body }` lowers to `_a = Call Arena.region_new() -> ..` with `_a` of origin `ArenaStorage`, then `_r = Borrow(Shared, _a)` with `_r` the binding of `r`, then the body, then at every exit `StorageDead(_r)`, `Drop(_a, ScopeExit)`, `StorageDead(_a)`. | §5.14 |
| F11 | Every user variable and temporary has one `StorageLive` before its first use and a `StorageDead` at every later exit of its scope, each time the scope is entered (so once per loop iteration). A temporary gets its `StorageLive` where it is created (impl/04 §4.5.1). A user variable gets it immediately before its initializer is lowered into it, or when its binding is made (impl/04 §4.6.3), so an exit from inside the initializer, such as a `?`, ends it too. Parameters and `_0` have neither. A local's `Drop` precedes its `StorageDead`. | §5.4.2, §5.10 |
| F12 | `Return`, `Unreachable`, `Trap`, and `CancelExit` have no successors. A `Call` with `target: Some` and an `Assert` have exactly one successor. A `Call` with `target: None` (a callee that returns `Never`, impl/04 §4.2.6) has none. | §5.6.6, §5.10.4 |
| F13 | MIR never writes a strict sub-place of a local that is not initialized, except through a user assignment (aggregates are built with `Aggregate`). MIR never reads a local before initializing it on some path that starts at its `StorageLive`. | §5.4.4 |
| F14 | The take-and-restore form of a non-Copy compound assignment `p op= e` (spec/06 §6.13.2, N-C-13) is a `Call` whose `SourceInfo.hir` is the `AssignOp`, with an argument `Move(p')` and destination `p'`, where `p'` is the lowered place of `p`. | §5.4.3, §5.10.1 |
| F15 | `Subslice { from, to }` of an owned `Array<U, N>` denotes the positions `from ..< N - to`. `to` counts from the end (spec/06 §6.21.3). | §5.3.1 |
| F16 | Drop elaboration keeps the index of every existing local. It appends drop flags (origin `DropFlag`) after them. A statement it inserts carries the `SourceInfo` of the `Drop` it elaborates. | §5.12 |
| F17 | Implicit reborrows (07 §7.5.5) and receiver auto-refs (07 §7.5.3) are explicit `Borrow` rvalues. A shared or mutable reborrow of the data of a slice view `s` is `Borrow(k, *s)`, whose result type is the view type (`Slice<T>` or `SliceMut<T>`), not `borrow`. The adjustments `MutToShared` and `SliceMutToSlice` of impl/03 §3.10.2 are `Borrow(Shared, *p)` (impl/04 §4.5.2), the adjustment `RefinedToBase` is a `Field(0)` projection of the refined place, and `RefinedBorrowToBase` is `Borrow(Shared, (*p).0)` (impl/04 §4.5.2), so none of them is a `Convert`. A method receiver that is reborrowed from an existing `borrow mut` is `Borrow(TwoPhaseMut, *p)` (F7). | §5.6.2, §5.6.3 |
| F18 | Contract check code exists in Built MIR, whether or not the SFC later proves the goal (impl/04 §4.1.4). The check of an `ensures` clause at a return point lies in a predicate scope `Predicate { obligation: Some(o), clause }` whose obligation `o` has kind `Ensures` (an **`Ensures` scope**, impl/04 §4.2.7, §4.12.1), which ends in the check's `Assert`. Inside it, the code reads a parameter of the owner only if the clause mentions that parameter, and each such read has the mention as its `SourceInfo.hir`. | §5.4.4 |

### 5.2.3 Queries used from other documents

| Query or item | Owner | Use |
|---|---|---|
| `fn_sig(def)` with `Region::Sig { item, index }` placeholders, numbered in pre-order: parameters (receiver first), then the return type | impl/03 §3.5.2 | universal regions (§5.5.2) |
| `type_of`, `adt_def`, `param_env`, `impl_header` | impl/03 | field types, impl regions, bounds |
| `region_params(PackageId)` (`has_region_param`) | impl/03 §3.5.2 | implicit region parameters (07 §7.8.2) |
| `variances_of(DefId)` | impl/03 §3.6.8 | subtyping (§5.5.5) |
| `typeck(owner)`: `closure_captures` (`CapturedVar { var, mode, ty, first_use }`), `closure_kinds`, `call_kinds` (`Direct \| FnPtr \| FnTrait`), `index_kinds` | impl/03 §3.11, §3.15 | §5.4.5, §5.6.4, §5.13 |
| `closure_sig(c)`, whether `c` had an expected signature, and the obligations `Closure(c, ..): Fn*(sig)` that the owner's type checking proved, each with the expression that required it | impl/03 §3.11.1, §3.11.5 (the last two are requirements of this document) | §5.13 |
| `fn_sig(f).async_output`, `await_kinds` | impl/03 §3.6.3, §3.7.11 | §5.15 |
| `invariant_of(adt)` (impl/03 §3.7.14) and the kind of a nominal type: refined, or distinct with a refinement clause (impl/03 §3.13.2) | impl/03 | §5.4.6 |
| `is_copy(ty, env)`, drop glue, `Drop` impl lookup, resource kind | impl/03 (05 §5.9.3) | §5.4.3, §5.12 |
| drop-transparent types | 16 (N-B-22) | `droplive` (§5.12.3) |
| lang items `Arena`, `Arena.region_new`, `Box`, `Drop`, `Copy`, `Index`, `IndexMut`, `Fn`, `FnMut`, `FnOnce`, `Future` | 16 §16.3 | §5.4.3, §5.13, §5.14, §5.15 |
| the designated item `sai:async.scope`, found by canonical path when `sai:async` is in the package graph | [spec/16](../spec/16-core-library.md) §16.16.1 item 3 (N-J-26) | §5.15.3 |

---

## 5.3 Move paths

### 5.3.1 Definition

A move path is a place that is owned along its path and has no `Index` projection (07 §7.4.1). The checker represents only the move paths that the body needs:

1. Every local `x` has a root path `x`.
2. For every place `p` that appears as a moved operand (`Move(p)`), as the destination of an `Assign`, `Call`, or `Await`, or as the place of a `Drop`, the checker creates the path `mp(p)` and all its prefixes. `mp(p)` is `p` truncated before its first `Deref` or `Index` projection.
3. A `Subslice { from, to }` of an owned `Array<U, N>` is not a path of its own. It is expanded into the `ConstIndex` paths of the positions `from ..< N - to`, each with `from_end = false` (F15, N-B-28). A `ConstIndex` with `from_end = true` on an owned `Array<U, N>` is normalized to the position `N - offset`.
4. Copies and borrows create no paths. A use of a place without a path is checked at its longest existing prefix (§5.4.4).

**Paths stop at every `Deref`, including the `Deref` of a `Box` (N-B-32).** 07 §7.4.1 counts `(*b).f` as a move path for `b : Box<T>`. The truncation is equivalent: nothing can be moved out of `*b` (N-B-2), and the only drop of `*b` alone is a `Replace` drop, which the write of the same place follows immediately (F3). So `*b` and every place below it are initialized exactly when `b` is.

### 5.3.2 Representation

```text
MovePath { place: Place, parent: Option<MovePathIndex>, first_child: Option<MovePathIndex>,
           next_sibling: Option<MovePathIndex> }
MoveData { paths: [MovePath], root_of: [MovePathIndex; locals], lookup: Map<(MovePathIndex, Elem), MovePathIndex>,
           moves: [MoveOut { path, point }], inits: [Init { path, point, kind }] }
```

- Paths are numbered in creation order. Creation visits the blocks in index order, the statements in order, and the operands of each statement left to right. The root paths are created first, in local order.
- Children are linked in creation order. Every traversal of the tree uses `first_child` and `next_sibling`. The `lookup` map is used only for point queries. It is never iterated (§5.17.2).
- `moves` and `inits` are the event lists of 07 §7.4.2, in point order. `Init.kind` is `Assign`, `CallDest`, `AwaitDest`, `ParamEntry`, or `TakeRestore` (§5.4.3).

### 5.3.3 Events

At each point, the events of the point are applied in this order: all moves of the point, then all initializations. This is the order of evaluation: the operands of a statement are read before its destination is written.

| MIR element | Event |
|---|---|
| `Move(p)` as an operand anywhere | move of `mp(p)`, if `p` has no `Deref` and no `Index`, and the move is legal (§5.4.3) |
| `Assign(p, ..)` | initialization of `mp(p)`, if `p` has no `Deref` and no `Index` |
| `Call { dest: p, .. }` | the moves of its arguments, then initialization of `mp(p)` on the edge to `target` |
| `Await { future, dest: p, resume, cancel }` | move of `future`, then initialization of `mp(p)` on the edge to `resume` only |
| `Drop { place: p }` | uninitialization of `mp(p)` |
| `StorageLive(x)`, `StorageDead(x)` | uninitialization of `x` |
| function entry | initialization of every parameter path. In a closure body, this includes `_1`, which holds the captures |

A move, initialization, or uninitialization of a path applies to the path and to every path below it. It does not change the path's ancestors (07 §7.4.2). A write through a `Deref` or an `Index` changes no path, because the place before that projection stays initialized.

---

## 5.4 Initialization dataflow and move errors

### 5.4.1 The two analyses

The checker computes two forward dataflow analyses per body, over bitsets indexed by `MovePathIndex`:

| Analysis | Bit `m` is set at `P` when | Join | Entry state |
|---|---|---|---|
| MaybeInit | on some path to `P`, the last event affecting `m` is an initialization | union | the parameter paths and their descendants |
| MaybeUninit | on some path to `P`, there is no such event, or the last one is an uninitialization or a move | union | every other path |

`m` is definitely initialized at `P` (07 §7.4.3) if and only if bit `m` is clear in MaybeUninit at `P`.

### 5.4.2 Transfer and iteration

- The transfer function of a point applies its events (§5.3.3) to both bitsets: a move or uninitialization of `m` sets the bits of `m`'s subtree in MaybeUninit and clears them in MaybeInit. An initialization does the opposite.
- Edge effects: the destination of `Call` is initialized on its only edge. The destination of `Await` is initialized on the `resume` edge only. On the `cancel` edge it stays as it was before the `Await`.
- The solver is a worklist over blocks, seeded and ordered by reverse postorder from the entry block, with ties broken by block index. A block is re-queued when its entry state changes. The analyses are monotone over a finite lattice, so they terminate. The fixed point is unique, so the visiting order affects only the time taken.
- The state **at** a point is the state before its own events. Use checks (§5.4.4) read this state.

### 5.4.3 Move legality

A `Move(p)` operand whose type is not Copy is checked before it produces a move event. The checker walks the projections of `p` from the root and stops at the **first** projection that makes the move illegal (spec/07 §7.3.5):

| First offending projection | Code |
|---|---|
| `Deref` of the closure environment `_1` (`borrow Self` or `borrow mut Self`) | `MOVE_OUT_OF_CAPTURE` (07 §7.11.3) |
| `Deref` of `borrow T`, `borrow mut T`, a view type, `Ptr<T>`, or `PtrMut<T>` | `MOVE_OUT_OF_BORROW` |
| `Deref` of `Box<T>` | `MOVE_OUT_OF_BOX` |
| `Index` | `MOVE_OUT_OF_INDEX` |
| `Field`, `ConstIndex`, `Subslice`, or `Downcast` of a base whose type has a `Drop` impl or is a resource | `MOVE_OUT_OF_DROP_TYPE` (N-B-23) |

Exceptions:

- The `Move(p')` argument of a take-and-restore call (F14) is exempt. spec/06 §6.13.2 allows it behind `borrow mut`. It produces a move event and the `TakeRestore` initialization of `mp(p')` at the same point, so the path is never observed uninitialized. Its access classification is in §5.10.1.
- A `Move` of a `Temp` local or of `_0` is always legal.
- A by-borrow capture inside a closure body is never moved: capture analysis makes such a capture by-move (07 §7.11.2). A `Move` through `*((*_1).i)` is therefore an internal compiler error (impl/11 §11.4.6).

An illegal move produces the diagnostic and **no move event**, so it causes no later `USE_AFTER_MOVE` and no drop-flag change.

### 5.4.4 Use checks

At every point `P`, every read of a place `p` is checked: a `Copy` or `Move` operand, the place of a `Borrow` or `RawPtr`, the place of `Discriminant` and `Len`, and the place of a `FakeRead`. A `Drop` is not a use. A write through a `Deref` or `Index` reads the place before that projection (spec/07 §7.3.6 item 3), and the local of an `Index(i)` projection is read.

1. Let `m` be the longest existing move path that is a prefix of `p` (by `lookup`, following the projections of `p` until a projection has no path).
2. If `m` is the path of `p` itself, the checked set is `m` and its whole subtree. Otherwise it is `{m}` alone, because no path exists below `p`.
3. If a bit of the checked set is set in MaybeUninit at `P`, the use is `USE_AFTER_MOVE`. The **failing path** is the first such path in pre-order of the tree.

For an assignment to `p` (the destination of `Assign`, `Call`, or `Await`) whose `mp(p)` is `p` itself, every strict prefix `q` of `p` MUST have its own bit clear in MaybeUninit at `P`. The subtree of `q` is not consulted: after a move of `x.g`, `x` is still initialized, so `x.f = v` is accepted (spec/07 §7.3.6 item 3). Otherwise the code is `PARTIAL_ASSIGN_TO_MOVED`. By F13, a violation at an assignment that is not a user assignment is an internal compiler error.

**Move-outs.** For a `USE_AFTER_MOVE` at `P` with failing path `f`, the checker collects the move-outs that reach `P`: a backward search over the CFG from `P`, visiting each point at most once and following predecessors in increasing order of `(block, statement)`. At each point it records a move of `f`, of an ancestor of `f`, or of a descendant of `f`, and it does not continue past a point that records a move or that initializes `f` or an ancestor of `f`. The move-outs are sorted by point. If the search finds no move-out, the use reads an uninitialized local, which F13 excludes, and the checker reports an internal compiler error.

**Grouping (N-B-37).** The checker reports one `USE_AFTER_MOVE` per distinct set of move-outs. It visits the uses in point order and reports the first use for each set. Later uses with the same set are not reported, because they would repeat the same explanation. For each move-out `M`, `in_loop` is true if `M` is reachable from `P` by a non-empty CFG path (the move happened in an earlier iteration). If the failing path is a strict descendant of the used path, the diagnostic names the moved field (07 §7.4.4).

**Moved parameters in `ensures` (N-B-41).** spec/09 §9.3.4 (N-C-48) rejects an `ensures` clause that mentions a by-value parameter that is moved, or may be moved, on the path to a return point. By F18, the check of the clause at that return point reads the mentioned parameters inside an `Ensures` scope. So the violation is exactly a `USE_AFTER_MOVE` at a point of that scope whose failing path is rooted at a parameter local. Pass A reports such a use as `ENSURES_USES_MOVED_PARAMETER` instead, once per predicate scope and parameter, with the move-out search and grouping above. The primary span is the return point, which is the span of the `Assert` that ends the scope (its panic location, impl/04 §4.12.1). The labels are the mention (the `hir` of the use) and the move-outs. The fix "copy the needed value with `old(..)`" is in §5.16.5. Because the check code exists in Built MIR whether or not the SFC proves the goal (F18), the result does not depend on the SFC.

### 5.4.5 Closure bodies

A closure body has its own move data. Its root paths include `_1`. A by-move capture `i` is the path `_1.i` in a `FnOnce` environment. In a `borrow Self` or `borrow mut Self` environment it is behind a `Deref` and is not a path. Moving it is `MOVE_OUT_OF_CAPTURE` (§5.4.3). Captures are initialized at entry (§5.3.3). In the enclosing body, the operands of the `Closure` rvalue are ordinary moves, copies, and borrows (F8).

### 5.4.6 Mutability

Pass A also checks the structural mutability rules of 07 §7.2.4, because they need no regions. `mutable(p)` is computed by walking `p` from the root:

| Element | Mutable if |
|---|---|
| root `x` | `LocalDecl(x).mutability = Mut` (F1) |
| `Field`, `ConstIndex`, `Subslice`, `Downcast`, `Index` of an `Array`, `Deref` of `Box` | the base is mutable |
| `Deref` of `borrow mut T` or `SliceMut<T>` | the base is **unique**: its path has no `Deref` of a shared view or of `Ptr<T>` (rule 4). The base's own mutability does not matter |
| `Deref` of `borrow T`, `Str`, `Slice<T>`, `Ptr<T>` | never |
| `Deref` of `PtrMut<T>` | always (unsafety is checked by impl/03, 12) |
| capture field `(*_1).i` or `_1.i` of a by-move capture | the captured variable is mutable in the enclosing body (spec/06 §6.17.3: a `move` capture that is mutated still requires `var`) |
| `Field(0)` of a refined type, or `.value` of a distinct type with a refinement clause | never (06 N-C-34, impl/03 §3.13.2) |

The checks are:

- At the final write of a user assignment (F2) to `p`: if `p` is not mutable, `ASSIGN_TO_IMMUTABLE`.
- At every `Borrow(Mut | TwoPhaseMut, p)`: if `p` is not mutable, `BORROW_MUT_OF_IMMUTABLE`. This covers every source of a mutable borrow (explicit, receiver, `IndexMut` base, pattern binding, capture, closure call), because each one is a `Borrow` in MIR (F7, F8, F17).
- At both of these points: if a `Field` projection of `p` selects a field of a type `T` that has an invariant (`invariant_of`, impl/03), and the root owner is not in `T`'s defining module, `INVARIANT_FIELD_WRITE` (spec/09 §9.4.2). A write or `borrow mut` of a whole place of type `T` is not a field write.
- Moves, assignments, and mutable borrows inside a match guard are rejected earlier, as `GUARD_MOVE_OR_MUTATION` (impl/03 N-E-38), so pass B never sees them. Mutation of the scrutinee through other paths conflicts with the guard's shallow borrow (§5.10.7).
- `used_mut` records the binding of every root whose `Mut` flag decided a check that passed: the walk reached a root, or a capture field, without passing a rule-4 `Deref`.

Pass A guarantees the following property, which impl/06 relies on: if `move_check(owner)` reports no error, then in Built MIR, and in DropElaborated MIR because elaboration preserves it, no point reads a move path that is not definitely initialized. Drop flags are initialized at entry (§5.12.2).

---

## 5.5 Regions

### 5.5.1 The region problem

Pass B solves one **region problem** per root body (N-B-31). The root body of an ordinary owner is its own body (impl/04 §4.2.1, `BodyIndex(0)`), and its problem contains every body of the owner in the order of impl/04 §4.2.1: the root body, the closure and `async` block bodies in `DefId` order (source pre-order), and the bodies of contract clauses. An `invariant` owner has one body per element, and a derived impl one body per method. Each of these is the root body of a problem of its own, together with the closure bodies nested in it, with the signature that impl/04 §4.3.3 gives it (`fn(self: borrow T) -> Bool` for an invariant element and a refinement predicate, the trait method's signature for a derived method). `region_signature` interprets these signatures like `fn_sig`. A contract clause body (impl/04 §4.3.3) is checked like the body of a closure that has an expected signature and no environment: the root universals inside the parameter types `T_i` and `R` are those of the root, and every other region position of its parameter types (the outer regions of `borrow T_i` and of `result: borrow R`, and the regions of the snapshot types) is a bound input region of the clause (§5.13.2). Its output is `Bool`. A region variable is a `RegionVid`, a dense index. There are four kinds. A scope region is a local variable with an extra seed:

| Kind | Created for | Seed of its value (§5.7.2) |
|---|---|---|
| `'static` | vid 0 | every point of the problem and every end marker |
| root universal | the impl regions, the input regions, and `'o` of the root's signature (§5.5.2) | every point of the problem, its own end marker, and the end markers of its assumptions |
| closure universal | the environment region and the bound regions of a closure (§5.13.2) | every point of the closure's **scope** (its body and the bodies nested in it), its own end marker, and the end markers of its assumptions |
| local | everything else (§5.5.4) | the liveness points of §5.6.7 |
| local, scope region | the region `'s_k` of a call of `sai:async.scope` (§5.15.3) | the liveness points of §5.6.7, and every point of the bodies nested in the call's argument expression |

Vids are assigned in this order: `'static`; the root universals in the order of §5.5.2; the closure universals, closure by closure in body order; then the local variables in creation order (§5.5.4). An end marker `end('u)` exists for every root and closure universal `'u`. `'static` has none.

A region **belongs to the scope** of closure `c` if it is a closure universal of `c` or of a closure nested in `c`, or a local variable created while constraints were generated for a body of `c`'s scope. The upvar regions of `c` and the free regions of its signature (§5.13.1) are created in the enclosing body. They do not belong to `c`'s scope.

### 5.5.2 Signatures: `region_signature(def)`

`region_signature(def)` interprets the `Sig` placeholders of `fn_sig(def)` (impl/03 §3.5.2) by the elision rules of 07 §7.7. It never looks at a body. Its value is:

```text
RegionSignature {
    impl_regions: [SigRegion]                 // positions of the impl target type, pre-order
    inputs:       [SigRegion]                 // I1, I2: parameter positions, pre-order, receiver first
    output:       None | Static | Out         // O1; Static by O4
    universal_of: [(SigRegion, Universal)]    // every Sig position outside fn pointer types
    assumptions:  BitMatrix<Universal>        // transitive closure of 07 §7.6.4
    param_bounds: [(TypeParamOrProjection, Universal)]   // implied type-parameter bounds, closed
    sources:      [Universal]                 // the regions that outlive 'o (O2, O3, O3')
    diagnostics:  [Diagnostic]
}
```

The steps are:

1. **Bound positions.** Positions inside a function pointer type or an `Fn`-family bound in the signature are bound by that type (I3, N-B-7). They are not universals. They are interpreted at each use (§5.6.4).
2. **Impl regions.** For a method, the positions whose `SigRegion.item` is the impl are the impl regions. They are numbered in the pre-order of `impl_header(impl).self_ty` (spec/07 §7.7.5 item 1).
3. **Inputs.** The positions of the parameter types whose `item` is `def`, in index order (I1, I2). By impl/03 §3.5.2, the index order is the order of 07 §7.7.1.
4. **Output.** All positions of the return type map to one universal `Out` (O1, N-B-10). For an `async fn`, the return type of this step is the declared output `T` (spec/07 §7.16 item 3), not the future type. If O3′ applies and `inputs` is empty, the output maps to `'static` and no `Out` exists (O4, N-B-11).
5. **The `borrows` clause** is validated by V1–V4 of 07 §7.7.3, in the order of its entries. V1–V3 failures are `BORROWS_INVALID_PROVENANCE` on the entry. V4 is `BORROWS_WITHOUT_OUTPUT_REGION` on the clause, with the fix of §5.16.5. An entry equal to an earlier one, or with an earlier or later entry as a strict prefix, is `REDUNDANT_PROVENANCE` on the redundant entry. An invalid entry contributes no source.
6. **Sources** are computed by O2, O3, or O3′, as a sorted list of universals.
7. **Assumptions.** The relation contains `'static : 'u` for every `'u`, reflexivity, `'x : 'o` for every source `'x`, and the implied bounds: the region relations required by the WF conditions (spec/07 §7.6.5) of every parameter type, the return type, and the impl target type. Its transitive closure is computed by a breadth-first search from each universal, in vid order. `param_bounds` collects the type-parameter conditions `P : 'u` of the same WF conditions, closed under the assumptions (`P : 'a` and `'a : 'u` give `P : 'u`).
8. **Trait compatibility.** For a method of a trait impl, the checker computes the trait method's `RegionSignature` with `Self`, the trait arguments, and the associated types substituted (spec/07 §7.7.5 item 4, N-B-13). It maps the positions of both signatures to each other by their pre-order position in the parameter and return types. If the substitution produces a position in one signature that is a different universal, or the same universal under a different mapping, or if the two assumption relations restricted to the output differ, the result is `IMPL_SIGNATURE_REGION_MISMATCH` on the impl method's return type, with the fixes of spec/07 §7.18.4.

A failure in steps 5 or 8 marks the signature as erroneous. Pass B of every caller then suppresses each diagnostic whose cause path contains a call of `def` (cascade C4, §5.1.2).

### 5.5.3 Items: `item_regions(def)`

- For a `resource` declaration, every field type that has a region position (a view type, or a nominal type with `has_region_param`, outside function pointer types) is `RESOURCE_CONTAINS_BORROW` on that field type (07 §7.8.4, N-B-14).
- For a trait impl, the region positions in associated type definitions and trait arguments are bound by N-B-12: to the single region of the target type, to `'static` if it has none, and otherwise each such position is `AMBIGUOUS_IMPL_REGION`. The query records this binding for step 8 of §5.5.2 and for normalization of `Self.Item` in callers.

### 5.5.4 Local region variables

Constraint generation (§5.6) visits the bodies of the problem in order, the locals of a body in index order, then the blocks in index order and the statements of each block in order. It creates a fresh local variable:

1. for each `Erased` region position of the type of each local, in pre-order of the type;
2. for each `Borrow` rvalue (the loan's region, §5.9.1), and for each `Erased` position of the type it produces;
3. for each `Erased` position of the type produced by `Aggregate`, `Convert`, or a `Const` of a function item type (its generic arguments);
4. for each universal of a callee and each bound region of a called function pointer type or `Fn`-family bound, once per `Call` (§5.6.4);
5. for the upvar regions and the free signature regions of each closure, once per closure (§5.13.1).

A `Static` position is `'static` (impl/03 §3.15.2 invariant 3). A `Sig` position in a body type is an internal compiler error.

### 5.5.5 Relating two types

`relate(S, T, v)` walks two types that are equal except for regions, with the variance `v` of the current position, starting covariant. A difference other than in regions is an internal compiler error, because impl/03 has unified the types.

- At a pair of region positions `'s` and `'t`, it emits `'s : 't` if `v` is covariant, `'t : 's` if contravariant, and both if invariant.
- Below a type constructor, it composes `v` with the variance of the argument: the table of 07 §7.6.3 for built-in types, `variances_of(def)` (impl/03 §3.6.8) for a nominal type, whose implicit region parameter is argument 0.
- Function pointer types have no free position and are skipped. Type parameters and projections have no position.
- A closure or `async` block type is **invariant** in every region it carries (N-B-42). These are the upvar regions and the free regions of its signature (§5.13.1).

`flow(S, T)` is `relate(S, T, covariant)`. It expresses one value flow `S <: T` (07 §7.6.3, N-B-9).

---

## 5.6 Constraint generation

### 5.6.1 Constraints

An outlives constraint is `('sup : 'sub, cause)`, where `cause = (point, kind)`. The kinds are `Assign`, `Borrow`, `Reborrow`, `WellFormed`, `CallArg(i)`, `CallReturn`, `CallAssumption`, `AggregateField(i)`, `Entry(i)`, `Return`, `Capture(i)`, `Await`, `Convert`, `ScopeOutlives`, and `SpawnOutlives` (the last two only at the calls of §5.15.3). Constraints are stored in emission order. The emission order is the visiting order of §5.5.4, and within one MIR element the order of the rows below. Liveness constraints are kept separately (§5.6.7).

A **type test** is `(P, 'l, cause)`, where `P` is a type parameter or a projection. It is checked after solving (§5.8.3).

`wf(T, cause)` emits the WF conditions of spec/07 §7.6.5. For `borrow 'a U`, `borrow mut 'a U`, or a slice view with region `'a` over `U`, it emits `'r : 'a` for every region `'r` of `U` and a type test `(P, 'a)` for every type parameter or projection `P` in `U`. For a nominal type, it emits the conditions of its field types with its arguments substituted. The checker caches these conditions per nominal `DefId`, in terms of the type's parameters.

### 5.6.2 Statements

| MIR | Constraints |
|---|---|
| `Assign(p, Use(op))` | `flow(type(op), type(p))` |
| `Assign(p, Borrow(k, q))` | with `'l` the loan's fresh region: `flow(borrow 'l type(q), type(p))` (`borrow mut` for `Mut` and `TwoPhaseMut`, the view type for F17), then `reborrow(q, 'l)` (§5.6.3), then `wf(borrow 'l type(q))` |
| `Assign(p, RawPtr(m, q))` | `flow` of the pointee types. No loan, no reborrow constraint (spec/07 §7.17 item 1) |
| `Assign(p, Aggregate(kind, ops))` | with `A` the aggregate's type (fresh regions): `flow(type(op_i), field_i(A))` for each operand in order, `wf(A)`, then `flow(A, type(p))` |
| `Assign(p, Repeat(op, n))` | `flow(type(op), element(type(p)))` |
| `Assign(p, Convert(kind, op, T))` | `ArrayToSlice` (impl/04 §4.2.4) relates the outer region of `borrow Array<U, N>` or `borrow mut Array<U, N>` to that of the view, and `U` to the element type, covariantly (invariantly for the mutable forms). `PtrToPtr` relates nothing (spec/07 §7.17). `FnItemToFnPtr` is checked by §5.6.5. The numeric, address, and `Bitcast` kinds carry no region. `Transmute` is an unsafe operation ([12](../spec/12-unsafe-and-ffi.md)) and relates nothing. Then `flow(T, type(p))`. The coercions of F17 are not `Convert`s |
| `Assign(p, Closure(def, ops))` | §5.13.1 |
| `Assign(p, BinaryOp \| CheckedBinaryOp \| UnaryOp \| Discriminant \| Len)` | none. The results carry no region |
| `StorageLive`, `StorageDead`, `SetDiscriminant`, `FakeRead`, `Nop`, `ProofObligation`, `CheckPoint` | none |

### 5.6.3 Reborrow constraints

`reborrow(q, 'l)` implements spec/07 §7.9.3. It walks the projections of `q` from the last to the first and looks at each `Deref`, with `B` the type of the base of that `Deref`:

```text
for each Deref in q, from the last projection toward the root:
    B := type of the base of this Deref
    if B is borrow mut 'a T or SliceMut with region 'a:  emit 'a : 'l (Reborrow); continue
    if B is borrow 'a T, Str, or Slice with region 'a:   emit 'a : 'l (Reborrow); stop
    if B is Box<T>:                                     continue
    if B is Ptr<T> or PtrMut<T>:                        stop
```

The closure environment is an ordinary base: a `Deref` of `_1 : borrow 'e Self` emits `'e : 'l` (§5.13.2).

### 5.6.4 Calls

For `Call { func, args, dest, target }` the checker first determines the callee signature from `call_kinds` of the call's HIR node (F2, impl/03 §3.15):

| `call_kinds` | Signature |
|---|---|
| `Direct` | `func` is a `Const` of type `FnDef(f, ḡ)`. The signature is `fn_sig(f)` with `ḡ` substituted, interpreted by `region_signature(f)`. Each universal of `f` (impl regions, inputs, `Out`) becomes a fresh local variable. An output mapped to `'static` stays `'static`. For a trait method that impl/03 resolved to an impl, the impl method is `f`. Otherwise `f` is the trait method, with `Self` and the trait arguments taken from `ḡ`. By N-B-13 both give the same constraints |
| `FnPtr` | the type of `func` is a function pointer type. Each bound input position becomes a fresh variable, in pre-order. All return positions become one fresh variable `'r`, with the assumptions `'b : 'r` for every bound input `'b`, or `'r = 'static` if there is none (spec/07 §7.7.6) |
| `FnTrait { kind, method }` | the arguments are the callee reference and a tuple (impl/03 N-E-26, N-E-28). For a closure type, the signature is the closure's (§5.13.3). For a type parameter or projection with an `Fn`-family bound, it is the bound's signature, instantiated as for `FnPtr`. The receiver's borrow region is a fresh variable |

Then it emits, in this order:

1. `CallAssumption`: for every assumption `'x : 'o` of the callee, the same relation between the fresh variables.
2. `WellFormed`: `wf` of every instantiated parameter type and of the instantiated return type. A type test that results is attributed to the call.
3. `CallArg(i)`: `flow(type(args[i]), param_i)` for each argument in order. For a tuple argument, the flow is element by element.
4. `CallReturn`: `flow(ret, type(dest))`. The constraint is location-insensitive, so it does not matter that the write happens on the edge to `target`.

The callee's implied bounds are not assumed at the call. Item 2 establishes them. For an `async fn` callee, the return type is its future type. §5.15.1 gives its regions.

### 5.6.5 Function item to function pointer

At `Convert(FnItemToFnPtr, op, T)` (07 calls it reification), where `op` has type `FnDef(f, ḡ)`, the checker decides the identity rule of spec/07 §7.7.6 structurally:

1. `f` has no impl region, and no region of `ḡ` occurs in the instantiated signature.
2. The input positions of the instantiated signature correspond position by position to the bound input positions of `T`. They have the same pre-order positions, because the region-erased types are equal (05 §5.15.2).
3. The set of inputs `'x` with `'x : 'o` in `region_signature(f).assumptions` is the set of all inputs, as in the pointer type (O3′). If neither has an input, both outputs are `'static`. If neither has an output, both are equal.

If any condition fails, the code is `FN_POINTER_REGION_MISMATCH` on `op`'s expression. The conversion emits no outlives constraint, because `T` has no free region.

### 5.6.6 Entry, return, and other terminators

| MIR | Constraints |
|---|---|
| entry of a body | `Entry(i)`, emitted after the local variables of the body are created and before the constraints of its first block: `flow(declared_i, type(_i))` for each parameter `i`. For the root, `declared_i` is the parameter type with its universals. For a closure, it is the closure's signature (§5.13.3). `_1` of a closure body gets the environment type |
| `Return` | `Return`: `flow(type(_0), declared_ret)`. For the root, `declared_ret` is the return type with `Out` or `'static`. For a closure, it is the closure's return type (§5.13.3). For an `async` block, it is the block's output type (§5.15.2) |
| `Call` | §5.6.4 |
| `Await` | §5.15.1 |
| `Goto`, `SwitchInt`, `Assert`, `Drop`, `Unreachable`, `Trap`, `CancelExit` | none. `Drop` acts through liveness only (§5.6.7) |

### 5.6.7 Liveness

Liveness is computed per body by a backward dataflow over bitsets indexed by local, with the worklist discipline of §5.4.2 in reverse. At each point, `live_in = uses ∪ (live_out − defs)`. There are two analyses with the same definitions, except for which accesses count as uses:

| | Use-live (spec/07 §7.9.4) | Drop-live (spec/07 §7.9.4) |
|---|---|---|
| uses at a point | every access to a place rooted at `x` (§5.10.1), except an assignment to the whole of `x`, a `Drop`, and `StorageDead(x)`. The local of an `Index(x)` projection is used. An assignment to a strict sub-place of `x` is a use of `x` | a `Drop` of a place rooted at `x` |
| defs at a point | an `Assign`, `Call` destination, or `Await` destination that is exactly `x`; `StorageLive(x)`; `StorageDead(x)` | the same |

The liveness constraints are these:

- If `x` is use-live at `P`, `P` is added to the seed of every region of `type(x)`.
- If `x` is drop-live at `P`, `P` is added to the seed of every region of `droplive(type(x))` (§5.12.3).

Only the `Drop` terminators that remain after drop elaboration count. A `Drop` that elaboration removed (class `Dead`, §5.12.1) makes nothing drop-live.

---

## 5.7 The region solver

### 5.7.1 Graph

The **constraint graph** has one node per region variable. Each outlives constraint `'a : 'b` is an edge `a → b`, stored in the adjacency list of `a` in emission order. Several constraints may give the same edge. All are kept for cause search (§5.16.3), and the solver ignores duplicates.

### 5.7.2 Seeds

The seed of a region is given by its kind (§5.5.1):

- `'static`: every point of the problem and every end marker.
- A root universal `'u`: every point of the problem, `end('u)`, and `end('v)` for every `'v` with `'u : 'v` in the assumptions.
- A closure universal `'b` of closure `c`: every point of `c`'s scope, `end('b)`, and `end('v)` for every `'v` with `'b : 'v` in `c`'s assumptions (§5.13.2).
- A local variable: its liveness points (§5.6.7).
- A scope region `'s_k` (§5.15.3): its liveness points, and every point of the bodies nested in the argument expression of scope call `k`.

### 5.7.3 Algorithm

The least solution of spec/07 §7.9.4 assigns to each region the union of its seed and the values of all regions that it reaches in the graph. The checker computes it once, over the strongly connected components:

```text
fn solve(graph, seeds) -> [Value; regions]
    sccs := tarjan(graph)              // iterative; roots in vid order; successors in edge order
                                       // Tarjan emits a component after every component it reaches
    for scc in sccs, in emission order:
        v := union of seeds[r] for r in scc.members, in vid order
        for r in scc.members, for each edge r -> s in edge order:
            if scc_of[s] != scc:
                v := v ∪ value[scc_of[s]]
        value[scc] := v
    return [value[scc_of[r]] for r in regions]
```

Every successor component is finished before its predecessors, so one pass suffices. The result is the least fixed point of `value(a) ⊇ seed(a) ∪ value(b)` for all edges `a → b`, which is unique. The traversal orders affect only the numbering of components, never the values. They are fixed so that dumps are deterministic (§5.17.2).

### 5.7.4 Representation

- Points are numbered densely across the problem: bodies in problem order, blocks in index order, then the statements of a block followed by its terminator. Each block is a contiguous range.
- A value is a pair `(points, ends)`. `points` is a sorted vector of disjoint half-open intervals of point numbers. Union is a linear merge. `ends` is a bitset over the root and closure universals.
- The value of `'static` and the values of root universals share one interned representation of "all points". Union with it is constant time.

The checker MAY use another representation if it computes the same values. The verify mode of §5.20.2 compares it with a naive fixed-point iteration.

---

## 5.8 Conformance to the signature

These checks implement spec/07 §7.9.7 and §7.11.1. They run after solving and before the loan checks of §5.10. Each check produces **candidates**, in the order given. A candidate has a code and a cause point, found by the cause search of §5.16.3. For each cause point, only the first candidate becomes a diagnostic (N-B-37).

### 5.8.1 Root universals

For each root universal `'u` in vid order, and each end marker `end('v)` of a root universal `'v` in `value('u)`, in vid order of `'v`, where `'u : 'v` is not in the assumptions:

- if `'v` is `Out`, the candidate is `BORROWS_CONTRACT_VIOLATED`. The missing source is `'u`.
- otherwise, the candidate is `BORROW_ESCAPES_SIGNATURE`. The message names `'u` and `'v` by spec/07 §7.18.2.

The provenance fix for `BORROWS_CONTRACT_VIOLATED` names the path `π` that selects `'u` most precisely. Among the parameter paths that V2 accepts (07 §7.7.3), it is the one with the smallest `regions(type(π))` that contains `'u`. Ties are broken by fewer fields, then by the order of a breadth-first search over parameters in order and fields in declaration order. The search never enters a nominal type twice on one path. If no path contains `'u` (an impl region that no parameter mentions), no fix is offered. With an existing clause, the fix appends `π`. Under rule (b), it creates `borrows { self, π }`. Rule (c) cannot produce this candidate, except for such an impl region.

### 5.8.2 Closure universals

For each closure `c` in body order, and each closure universal `'b` of `c` in vid order (§5.13.2):

1. If `value('b)` contains a point outside `c`'s scope, the candidate is `BORROW_ESCAPES_SIGNATURE`.
2. For each end marker `end('v)` in `value('b)`, other than `end('b)`, where `'b : 'v` is not among `c`'s assumptions, the candidate is `BORROW_ESCAPES_SIGNATURE`.
3. For each region `'x` that does not belong to `c`'s scope (§5.5.1), in vid order, whose value contains `end('b)`, where `'x : 'b` is not among `c`'s assumptions, the candidate is `BORROW_ESCAPES_SIGNATURE`.

The signature named in these diagnostics is the closure's expected bound, or the closure expression if the violated region is the environment region. Rule 3 also covers a root universal that would contain `end('b)`.

### 5.8.3 Type tests

A type test `(P, 'l, cause)` holds if and only if, for every end marker `end('u)` in `value('l)`, `P : 'u` is in the `param_bounds` of the root, or, for a closure universal `'u`, in the implied bounds of that closure. A projection `P.A` also passes when its base `P` passes (spec/07 §7.6.5, N-B-26). Points never make a type test fail. A failing type test is the candidate `BORROW_ESCAPES_SIGNATURE`.

---

## 5.9 Loans

### 5.9.1 The loan set

Each `Borrow` rvalue creates one loan (spec/07 §7.9.3):

```text
Loan { index: LoanIndex, body: BodyId, point: Point, kind: Shared | Mut | TwoPhaseMut,
       place: Place, region: RegionVid, guard: bool, activation: Option<Point> }
```

- Loans are numbered per body, in point order. Each body has its own loan bitsets. A loan of the enclosing body is never in scope inside a closure body, because no CFG path leads into another body. A capture is a loan of the enclosing body, created where the closure value is built (F8).
- `region` is the fresh variable that §5.5.4 item 2 created for the `Borrow`.
- `guard` is true if the destination is a local of origin `GuardScrutineeRef` (F6). Such a loan is **shallow** (§5.10.7, N-B-35).
- `activation` is set for `TwoPhaseMut`. It is the `Call` that moves the loan's temporary (F7). If impl/04 produces a `TwoPhaseMut` whose temporary is not used exactly once as a `Move` argument of a `Call`, that is an internal compiler error.
- A loan of a place behind a shared view is recorded too. No access conflicts with it (spec/07 §7.9.3). The checker MAY drop such loans early, because they can never produce a diagnostic.
- `RawPtr` creates no loan (spec/07 §7.17).

### 5.9.2 Loans in scope

The checker computes the loans in scope at every point with a forward dataflow over loan bitsets, using the worklist discipline of §5.4.2:

```text
in(Q)  = ∪ over predecessors Q' of Q of  { L ∈ out(Q') : Q ∈ value(L.region) }
out(Q) = (in(Q) − kill(Q)) ∪ gen(Q)
gen(Q)  = the loans created at Q
kill(Q) = the loans whose place is rooted at x, if Q assigns the whole of x
          (Assign, Call destination, or Await destination equal to x), or Q is StorageDead(x)
```

For a `Call` or `Await`, the kill of the destination applies on the outgoing edge, after the accesses of the point (§5.10.1). `in(Q)` is the set of loans in scope at `Q`.

This is the definition of spec/07 §7.9.5:

- A loan enters `in(Q)` only along a CFG edge into `Q`, and only if `Q` is in its region. So it is carried along exactly the non-empty paths whose points after the creation point are all in the region.
- `gen` is applied after `kill`, so the creation point's own kill does not affect the new loan. A kill at `Q` removes the loan from `out(Q)`, but not from `in(Q)`, so the killing access itself is still checked.
- In a loop, a loan reaches its own creation point again if its region covers the back edge. The conflict is then reported against the borrow "in the previous iteration" (§5.16.4).

The equivalent per-loan search is a forward depth-first search from `L.point` that enters a point only if the point is in `value(L.region)` and stops after a point that kills `L`. Verify mode compares the two (§5.20.2).

### 5.9.3 Activation state

For a `TwoPhaseMut` loan, a second forward dataflow over the same bitsets computes `activated(Q)`. The loan is added on the edge out of its activation point and removed at its creation point and wherever it is killed. The join is union. At `Q`, a two-phase loan in `in(Q)` and not in `activated(Q)` is **reserved** (07 §7.10). Otherwise it is **active**.

---

## 5.10 Accesses and conflicts

### 5.10.1 The accesses of a point

Each point performs the accesses below, in the order given. Within one rvalue, operands are accessed from left to right, before the rvalue's own access.

| MIR element | Accesses |
|---|---|
| operand `Copy(p)` | read `p`, deep |
| operand `Move(p)` | move `p`, deep (a move of a Copy type is a read) |
| `Borrow(Shared, p)` | read `p`, deep. If the loan is a guard loan: read `p`, shallow |
| `Borrow(Mut, p)` | write `p`, deep |
| `Borrow(TwoPhaseMut, p)` | the reservation: read `p`, deep |
| `RawPtr(Const, p)` / `RawPtr(Mut, p)` | read / write `p`, deep (spec/07 §7.17) |
| `Discriminant(p)`, `Len(p)` | read `p`, shallow |
| `Assign(p, rv)` | the accesses of `rv`, then write `p`, shallow |
| `SetDiscriminant(p, ..)` | write `p`, shallow |
| `FakeRead(cause, p)` | read `p`, shallow |
| `Call { func, args, dest }` | `func`, then each argument in order, then the activation of every loan whose temporary is moved by an argument (write of the loan's place, deep), then write `dest`, shallow |
| take-and-restore `Call` (F14) | the argument `Move(p')` is a read `p'`, deep, instead of a move. The destination `p'` is a write, shallow (N-B-36) |
| `Await { future, dest }` | move `future`, deep, then write `dest`, shallow |
| `Drop { place: p, kind }` | drop `p`, if `type(p)` has drop glue |
| `StorageDead(x)` | storage-dead `x`, shallow |
| `Return`, `CancelExit` | storage-dead of every local other than `_0` whose storage has not ended, shallow (N-B-33) |
| `StorageLive`, `Nop`, `ProofObligation`, `CheckPoint`, `Goto`, `SwitchInt`, `Assert`, `Unreachable`, `Trap` | the operands of `SwitchInt` and `Assert` are read. Otherwise none |

Two rows need a reason:

- **`Return`.** Parameters have no `StorageDead` (F11). A parameter without drop glue would otherwise never end, and returning a borrow of it would not be detected. N-B-33 treats the end of the body as the end of the storage of every remaining local. Locals whose `StorageDead` has already run are excluded, using the storage-liveness of F11.
- **Take-and-restore.** The value of `p'` is taken and a new value is written back at the same point. No code can observe `p'` in between. A read and a write give the codes that the user expects for `p op= e` (`USE_WHILE_MUT_BORROWED` and `ASSIGN_WHILE_BORROWED`), instead of `MOVE_WHILE_BORROWED`.

### 5.10.2 Relevance

The relevance test of 07 §7.9.2 compares an access place `p` with a loan place `q`. Both have the same root. Otherwise the access is not relevant. Let `c` be the longest common prefix:

- **Overlap.** `p` and `q` are disjoint if, after `c`, the next projections of both exist and form one of the disjoint pairs of 07 §7.2.3: different `Field`s, `ConstIndex` projections with different resolved positions (for a slice view only from the same end), or `Downcast`s to different variants. Otherwise they overlap.
- **Deep access.** Relevant if `p` and `q` overlap.
- **Shallow access.** Relevant if `q` is a prefix of `p`, or `q` extends `p` and no projection of `q` after `p` is a `Deref`.
- **Drop access.** Relevant if `p` and `q` overlap, the path of `q` has no `Deref` of `borrow T`, `Str`, or `Slice<T>`, and, if `q` extends `p`, walking the projections of `q` after `p` from left to right with their base types meets a base whose type has a `Drop` impl or is a resource before it meets a `Deref` of a view type or of a raw pointer, or meets no such `Deref` at all (N-B-15).
- **Guard loan.** A guard loan is relevant only to an access whose place is a prefix of `q` (§5.10.7).

The test is linear in the length of the places.

### 5.10.3 Conflict table

The **effective kind** of a loan at `Q` is `Shared` for a `Shared` or reserved `TwoPhaseMut` loan, and `Mut` for a `Mut` or active `TwoPhaseMut` loan. The conflict table is the one of spec/07 §7.9.6:

| Access kind | Effective `Shared` | Effective `Mut` |
|---|---|---|
| read | no conflict | conflict |
| write, move, drop, storage-dead | conflict | conflict |

The activation of a loan is never checked against that loan itself.

### 5.10.4 The check

```text
for each body in problem order:
    for each block B in index order, for each point Q of B in order:
        S := in(Q)                                    // §5.9.2
        for each access A of Q, in the order of §5.10.1:
            for each loan L in S, in loan index order:
                if A activates L: continue
                if relevant(A, L) and conflicts(A.kind, effective_kind(L, Q)):
                    record candidate (Q, A, L)
```

Every block is checked, reachable or not. An access in an unreachable block can only conflict with a loan created in an unreachable block, which matches spec/07 §7.9.5. A `Trap` has no successor (F12), so no loan is in scope after it, and no drop follows it (07 §7.9.1 item 5). The candidates are then filtered by §5.10.8 and reported with the code of §5.10.5.

### 5.10.5 Choosing the code

For a candidate `(Q, A, L)`, the code is given by the first row that applies (spec/07 §7.9.6):

| # | Condition | Code |
|---|---|---|
| 1 | `A` is the access of a `Borrow` (shared, mutable, or the reservation of a two-phase borrow), an activation, or a `RawPtr` | `CONFLICTING_BORROW` |
| 2 | `A` is a read | `USE_WHILE_MUT_BORROWED` |
| 3 | `A` is a shallow write, or the drop of a `Drop` with `kind = Replace` (F3) | `ASSIGN_WHILE_BORROWED` |
| 4 | `A` is a move | `MOVE_WHILE_BORROWED` |
| 5 | `A` is a scope drop or a storage-dead of a place rooted at `x`, and `x` has origin `ArenaStorage` (F10) | `BORROW_ESCAPES_ARENA` |
| 6 | as 5, and `L.region` reaches in the constraint graph a region of the type of the body's `_0`, the output universal `Out` (root body), or the bound output region (closure body) | `RETURNS_LOCAL_BORROW` |
| 7 | as 5, and `x` has origin `Temp` | `TEMPORARY_DROPPED_WHILE_BORROWED` |
| 8 | as 5, otherwise | `DOES_NOT_LIVE_LONG_ENOUGH` |

A candidate that row 6, 7, or 8 classifies is `CHILD_TASK_BORROWS_LOCAL` instead if the cause path for `Q` in `value(L.region)` (§5.16.3) contains a constraint of kind `ScopeOutlives` or `SpawnOutlives` (§5.15.3 item 7). By-borrow captures are `Borrow`s (row 1) and by-move captures are moves (row 4), as 07 requires. `core.mem.drop(x)` is a `Move` argument (row 4). Row 6 uses a reachability search over the graph of §5.7.1, from `L.region`, in edge order. Its result is cached per loan.

### 5.10.6 Assignments

An assignment to an initialized place whose type has drop glue is a `Drop(p, Replace)` followed by the write of `p` (F3). The drop is checked like every drop, but it is reported as an assignment (row 3), with the primary span of the assignment (the `hir` of the following write, F2). If both the `Replace` drop and the following write conflict with the same loan, only the drop is reported (§5.10.8).

### 5.10.7 Match guards

The guard loan of F6 implements the shallow scrutinee borrow of impl/03 N-E-38. Its creation is a shallow read, and it is relevant only to an access of a prefix of its place (§5.10.2). So the bindings of an arm, which borrow parts of the scrutinee, never conflict with it, and reads never do. What conflicts is a write, move, drop, or storage-dead of the scrutinee place itself or of one of its prefixes, while the guard is evaluated. impl/03 rejects moves and mutations written in a guard (`GUARD_MOVE_OR_MUTATION`). The guard loan therefore catches the remaining cases, which are writes through other places that impl/04 lowers into the guard. In a well-formed program they do not occur. The conflict codes are those of §5.10.5. The loan is kept live by the `FakeRead(ForGuard, _sr)` at the end of each guard.

### 5.10.8 Deduplication

The candidates are processed in the order of §5.10.4 (N-B-37):

1. For each point `Q`, only the first candidate is kept, which is the first conflicting access and, for it, the loan with the lowest index.
2. Of the candidates that have the same loan and the same code, only the first is kept. A loan that conflicts with many accesses is reported once, at the earliest one.
3. A candidate at the write that follows a `Replace` drop is removed if the drop has a candidate with the same loan.

The kept candidates become diagnostics (§5.16).

---

## 5.11 Two-phase borrows

The construction emits `TwoPhaseMut` exactly in the two cases of 07 §7.10 (F7). The checker treats the loan in three stages:

1. **Reservation**, at the `Borrow(TwoPhaseMut, q)`: a deep read of `q` (§5.10.1). It is checked against the loans in scope. A conflict is `CONFLICTING_BORROW`, with `access_kind = "borrow_mut"` (§5.16.2).
2. **Reserved**, from the reservation up to the activation: the loan's effective kind is `Shared` (§5.10.3). Arguments may read `q`, copy from it, and borrow it shared. A write, move, mutable borrow, or drop of `q` conflicts.
3. **Activation**, at the `Call`: a deep write of `q`, checked against every other loan in scope (§5.10.1). A shared loan that an argument created and that is still live, because the call's result or another argument uses it, conflicts here. After the activation, the loan's effective kind is `Mut`.

Nothing else is special. The loan's region, kills, and reborrow constraints are those of an ordinary mutable borrow. `TwoPhaseMut` is a mutable borrow for pass A (§5.4.6) and for the regions (§5.6.2).

---

## 5.12 Drops

### 5.12.1 Drop classification

Pass A classifies every `Drop { place: p }` of Built MIR, with `m = mp(p)` and the states at the drop point. The first class that applies is used:

| Class | Condition | Elaboration (impl/04) |
|---|---|---|
| `Dead` | no bit of `m`'s subtree is set in MaybeInit | the `Drop` is removed |
| `Static` | no bit of `m`'s subtree is set in MaybeUninit | an unconditional drop of `p` |
| `Conditional(flag)` | no strict descendant of `m` has a move event in the body | a drop of `p` guarded by the drop flag of `m` |
| `Open` | otherwise: some strict descendant of `m` is moved somewhere in the body | the components of `p` are dropped separately, in the order of spec/07 §7.13.3. A component with its own move path gets the class of that path, computed by this table. A component without one gets the class of `m`'s own bit |

`Open` never applies to a type with a `Drop` impl or a resource, because a move out of a field of such a type is illegal and produces no move event (§5.4.3). For an enum, the components are those of the active variant. Elaboration switches on the discriminant.

`MoveCheckResults` records the class per `Drop`, and per component of every `Open` drop, keyed by the point and the move path. It also records which move paths need a drop flag: those of the `Conditional` drops and of the `Conditional` components. At every `CheckPoint` (impl/04 §4.6.5), it classifies by the same table a drop of each parameter path, and in an async body of each capture `_1.i`, with the states at that point. These classes guard the return-point invariant checks (impl/04 §4.12.3), and their `Conditional` paths need a drop flag too. `drop_classes(owner)` returns all three in the type `DropClasses` of impl/04 §4.11.2, whose `Conditional` carries no flag: the flag of a path is found through `flagged`.

### 5.12.2 Drop flags

The drop flag of a path `m` is a `Bool` local of origin `DropFlag` (F16). Elaboration MUST maintain this invariant at every point where the flag is read: the flag is true if and only if the path taken has initialized `m` since its last move or uninitialization. That is, it follows the events of §5.3.3 on the path taken:

- At function entry, the flag is true for a parameter path and false for every other path.
- At an initialization of `m` or an ancestor, it becomes true.
- At a move or uninitialization of `m` or an ancestor, it becomes false.

For a path that is `Conditional`, no descendant is moved separately, so one flag describes the whole subtree. The flags are what pass A's MaybeInit and MaybeUninit summarize: a flag is constant at a point exactly where the two analyses agree. Pass B does not read the flags. It sees only the `Drop` terminators that elaboration kept (spec/07 §7.9.4).

### 5.12.3 `droplive`

`droplive(T)` (spec/07 §7.13.7) is computed per type and memoized for the problem:

| `T` | `droplive(T)` |
|---|---|
| no drop glue (Copy types, views, function pointers, raw pointers) | `∅` |
| a nominal type with a `Drop` impl, a resource, an `async` block or future type | `regions(T)` |
| a drop-transparent type (`Box<U>`, `Vec<U>`, and those that 16 designates) | the union of `droplive` of its type arguments |
| a record or enum without a `Drop` impl | the union of `droplive` of its field types, with the arguments substituted, over all variants |
| a tuple, `Array<U, N>` | the union of `droplive` of the element types |
| a closure type | the union of `droplive` of its by-move capture types (§5.13.1). By-borrow captures are views and have no drop glue |
| a type parameter or a projection | `∅`. Its regions are chosen by the caller and contain the whole body |

A recursive type is computed as a least fixed point: a type that is already being computed contributes `∅` to itself. The result does not depend on the order of the fields, because union is commutative. Whether a type has drop glue is the query of impl/03 (§5.2.3).

---

## 5.13 Closures

A closure body is checked in the problem of its root owner (N-B-31, 07 §7.11.1). This section defines the regions that connect the two bodies. `async` blocks follow the same rules, with the additions of §5.15.

### 5.13.1 Regions in the enclosing body

The type of closure `c` is `Closure(c, args)` with two parts (impl/03 §3.11.2): the signature, as a function pointer type, and the capture tuple `υ`. Once per closure, when constraint generation first meets `c`'s `Closure` rvalue, the checker creates two sets of local variables of the enclosing body:

- **Upvar regions `U_c`**: one variable per region position of `υ`, in pre-order. The type of capture `i` is `CapturedVar.ty`: `borrow T` or `borrow mut T` for a by-borrow capture (impl/03 gives these as views with an `Erased` region), and `T` for a by-move capture. So `U_c` contains the loan regions of the by-borrow captures and the regions of the captured types (07 §7.11.4).
- **Signature regions `S_c`**: if `c` has no expected signature (07 §7.11.1), one variable per region position of its parameter and return types, in pre-order. If it has one, `S_c` is empty, and the signature's regions are closure universals (§5.13.2).

The signature part is a function pointer type. It has no free position for `relate` (§5.5.5). `S_c` is attached to `c` itself, not to its type. A position of `υ` in any type of the problem is the corresponding variable of `U_c` after `relate`, because closure types are invariant (N-B-42).

At `Assign(p, Closure(c, ops))`:

1. `Capture(i)`: `flow(type(ops[i]), υ_i(U_c))` for each capture in order. A by-borrow capture is `Move(_k)` of a borrow temporary (F8), so its loan region flows into the capture's region in `U_c`.
2. `Assign`: `flow(Closure(c, U_c), type(p))`, invariant in `U_c`.

### 5.13.2 Closure universals

Closure `c` has these closure universals (07 §7.11.1, N-B-26), created in this order:

| Universal | When | Assumptions |
|---|---|---|
| `'e`, the environment region | `closure_kind(c)` is `Fn` or `FnMut` | `'static : 'e`. The implied bounds of `borrow 'e Closure(c, U_c)`, which are `'u : 'e` for every `'u` in `U_c` |
| the bound input regions `B_c`, in pre-order of the expected signature's parameter types | an expected signature exists | `'static : 'b`. The implied bounds of the parameter types |
| the bound output region `'r_c` | the expected signature has a return region | `'b : 'r_c` for every `'b` in `B_c` (spec/07 §7.7.6). If `B_c` is empty, the output is `'static` and `'r_c` is not created |

The assumptions are closed transitively, as for the root (§5.5.2 step 7). They may mention regions of the enclosing body (`'u : 'e`). They are used by the checks of §5.8.2. The seeds are those of §5.7.2.

### 5.13.3 The closure body and calls of the closure

In the body of `c`:

- `_1` has the environment type: `borrow 'e Closure(c, U_c)` (`Fn`), `borrow mut 'e Closure(c, U_c)` (`FnMut`), or `Closure(c, U_c)` (`FnOnce`) (F9). The declared types of the other parameters use `B_c` or `S_c`. The entry constraints of §5.6.6 flow from these declared types into the local types.
- The `Return` constraint flows `type(_0)` into the return type with `'r_c`, `'static`, or `S_c`.
- A by-borrow capture `i` is used through `*((*_1).i)`. The reborrow walk (§5.6.3) passes the capture's loan region in `U_c` and, through a `borrow mut` environment, `'e`. So returning a borrow of a by-borrow capture from an `FnMut` closure puts the points of the caller into `'e`, which §5.8.2 rule 1 rejects.

At a call of a closure value of type `Closure(c, ..)` (`call_kinds` = `FnTrait`, §5.6.4), in any body of the problem:

- The receiver is `borrow 'x` or `borrow mut 'x` of the closure, or the closure by value, by the closure's kind (07 §7.11.3, N-B-19). `'e` is replaced by a fresh variable per call, which the receiver's region flows into.
- If `c` has an expected signature, `B_c` and `'r_c` are replaced by fresh variables per call, with the assumptions `'b : 'r_c` instantiated (as for `FnPtr`).
- If not, the signature uses `S_c` itself. All calls of `c` share these regions (N-B-17).
- `U_c` appears through the closure's type, unchanged.

A call of a closure from outside its root owner, for example through a type parameter of a generic function, is checked with the bound's signature (§5.6.4). Its regions reach the closure only through the type argument (07 §7.11.4).

### 5.13.4 Closures used against a bound

A closure without an expected signature satisfies a later `Fn`-family bound only where that bound binds no region (07 §7.11.1). The checker reads the obligations `Closure(c, ..): Fn*(sig)` that impl/03 proved in the bodies of the problem, with the expression that required them (§5.2.3). For each one in which `c` has no expected signature, it walks the bound's signature and `c`'s signature in parallel. The two are equal up to regions.

- If a position is bound in the bound's signature (an input, or an output that the bound ties to its inputs), and `c`'s signature has a region there, the result is `BORROW_ESCAPES_SIGNATURE` on the expression. The cause point is the MIR point of that expression. The fix "write the closure in place" (spec/07 §7.18.4) is offered if the closure's local is initialized by the closure expression and has no other use.
- If the bound's output is `'static` (O4), the checker emits `'s : 'static` for the closure's output region `'s`, with cause `CallAssumption` at the expression.

### 5.13.5 Captures in pass A

The captures are ordinary moves, copies, and borrows of the enclosing body (F8). Pass A therefore checks them as such. A mutable capture of an immutable variable is `BORROW_MUT_OF_IMMUTABLE` at the closure expression (§5.4.6), and a move out of a by-borrow capture's environment is `MOVE_OUT_OF_CAPTURE` (§5.4.3). In the closure body, the mutability of a capture field is that of the captured variable (§5.4.6).

---

## 5.14 Arenas

A `region r { body }` block is lowered by F10. The arena local `_a` has origin `ArenaStorage`, and `r` holds the shared loan of `_a`. Everything allocated through `r` carries a region that this loan's region outlives, through the ordinary signatures of `Arena.alloc`, `alloc_str`, and `alloc_slice` (spec/07 §7.15.2, rule (b)). No rule of this document is specific to arenas except these two:

1. **Code.** A conflict at the `Drop` or `StorageDead` of an `ArenaStorage` local is `BORROW_ESCAPES_ARENA` (§5.10.5 row 5). The `Drop` comes first (F10), so the `StorageDead` is deduplicated (§5.10.8).
2. **Primary span** (spec/07 §7.18.1). The cause search (§5.16.3) explains why the arena loan's region contains the drop point. It gives a constraint path from the loan's region to the region that liveness seeded. The primary span is the point of the first constraint on that path whose target region is a region of the type of a local that is declared **outside** the block: its declaration scope (F1) is not nested in the scope of `_a`. That is the first flow of an arena value into a place that is live after the block. The destination of the block's value is such a local, so the tail expression is found the same way. If no constraint qualifies (the region is kept by a universal), the primary span is the block's tail expression.

The labels are the loan ("the arena `r` is created here"), the drop ("the arena is freed here, at the end of the block"), and the later use (§5.16.4). `ARENA_REQUIRES_COPY` is reported by impl/03 (spec/07 §7.15.2).

---

## 5.15 Async (Stage 5)

This section applies from Stage 5 (D-ASYNC-01). Before Stage 5, MIR contains no `Await`, `CancelExit`, or future types, and the checker treats each as an internal compiler error.

### 5.15.1 Future types and `.await`

**Future type of an `async fn`.** `fn_sig(f).output` is `Future(f, ḡ)` and `fn_sig(f).async_output` is the declared output `T` (impl/03 §3.6.3). By 11 N-I-7, `regions(Future(f, ḡ))` is the union of the regions of all parameter types of `f`. MIR does not spell them as positions. The checker therefore gives every occurrence of `Future(f, ḡ)` the positions of `ḡ` followed by one position per input universal of `region_signature(f)`, in order (N-B-42). These positions are invariant. At a `Direct` call of `f`, the input positions of the returned future are the fresh variables that instantiate `f`'s inputs (§5.6.4). So while the future is live, every argument stays borrowed (spec/07 §7.16 item 3).

**Future type of an `async` block.** It is `Closure(b, args)` with the parts `[output, υ]` (impl/03 §3.11.2). Its regions are `U_b` (§5.13.1). The regions of the output part are signature regions `S_b` of the enclosing body, as for a closure without an expected signature.

**`Await { future, dest, resume, cancel }`.** The accesses are a deep move of `future` and a shallow write of `dest` (§5.10.1). The constraint is `Await`: `flow(Output, type(dest))`, where `Output` is the future's output type:

- For `Future(f, ḡ)`: `async_output` with `ḡ` substituted, each input universal of `f` replaced by the corresponding input position of the future type, and `Out` replaced by a fresh variable `'o'` with `'x : 'o'` for every source `'x` (or by `'static` under O4).
- For an `async` block future: its output part, with `S_b`.
- For any other type `F: Future`: the normalized `<F as Future>.Output` of impl/03 (`await_kinds`), whose positions are fresh variables of the `Await`.

The `resume` successor continues normally. The `cancel` successor (F4) runs the drops of the cancellation exit (spec/07 §7.16 item 5, N-B-27, 11 N-I-8) and ends in `CancelExit`, whose accesses are those of `Return` (§5.10.1). So a value whose drop reads a borrow of another local is checked on the cancellation path too. `droplive` of a future type is all its regions (§5.12.3).

### 5.15.2 The body of an `async fn`

An `async fn f` has two bodies in the problem: the outer body, which builds the future, and the async body, whose `DefId` is the block of impl/03 §3.7.11:

- The outer body is `_0 = Closure(b, [Move(_1), .., Move(_n)])`, then `Return` (F8). Every parameter is captured by value (N-B-25). The type of `_0` is `Future(f, identity args)`. The `Return` constraint flows it into the declared `fn_sig(f).output`, whose input positions are the root's input universals. By invariance, the captured parameter types in the async body carry exactly the root universals.
- The async body is a `FnOnce`-like body with `_1 : Future(f, ..)` holding the parameters. It has no closure universals. Its `Return` constraint flows `type(_0)` into `async_output` with the root's `Out` (or `'static`). The root's assumptions and implied bounds apply to it, because root universals contain every point of the problem (§5.7.2).
- So `BORROWS_CONTRACT_VIOLATED` and `BORROW_ESCAPES_SIGNATURE` in an `async fn` are found exactly as in a synchronous function, with the declared output `T` (spec/07 §7.16 item 3).

An `async` block in an ordinary body has local return regions `S_b`. A borrow of a block-local value that flows into the block's output is `RETURNS_LOCAL_BORROW` for the block body (§5.10.5 row 6).

The children of `scope` (spec/07 §7.16 item 6) are not ordinary calls. The signature of `scope` cannot express their rule, so the checker implements the typing rule of spec/11 §11.7.3 (§5.15.3).

### 5.15.3 Scope calls

This subsection implements the scope region rule of spec/11 §11.7.3 (11 N-I-11), which replaces the ordinary check of the bound of `scope`. A **scope call** is a `Direct` call (§5.6.4) whose callee is the designated item `sai:async.scope` (§5.2.3). The checker finds it by canonical path, as impl/03 §3.3.6 finds lang items, and only when `sai:async` is in the package graph. Otherwise no call is a scope call. `Scope` is the type that the parameter of the bound `B: FnOnce(borrow Scope) -> F` in `fn_sig(scope)` borrows. A **spawn call** is a `Direct` call of the inherent method `spawn` of that type. A spawn call need not lie inside a scope call of the same problem. The checker treats the region parameter of `Scope` and of `JoinHandle` as invariant, whatever `variances_of` gives, because the rule fixes them to `'s` (spec/11 §11.7.3 item 1, N-B-44).

For scope call `k`, with type arguments `B` and `F` in `ḡ`, the checker does the following in addition to the constraints of §5.6.4, in this order:

1. **Scope region.** It creates a fresh local variable `'s_k`, the call's **scope region** (§5.5.1). The future type `Future(scope, ḡ)` of this call gets one more invariant position after those of §5.15.1, and at this call that position is `'s_k`. So `'s_k` contains every point at which the scope future, or a value that holds it, is live (rule item 1). Its extra seed (§5.7.2) is every point of the bodies nested in the call's argument expression: the closure that is the argument, and the closures and `async` blocks inside it, among them the children's futures. These bodies run only while the scope is live.
2. **The body's parameter** (rule item 2). Both region positions of `borrow Scope`, the view and the region parameter of `Scope`, are `'s_k`. If the argument operand is a closure `c` built by a `Closure` rvalue of the call's own argument expression, and `c` has an expected signature (it is then the bound of `scope`, impl/03 §3.11.1), these positions are not in `B_c`. The declared parameter type of `c` has `'s_k` at both positions, and `c` has no bound output region: the positions of its return type are those of `F` in `ḡ`, so `F` may mention `'s_k`. If `c` has no expected signature, §5.13.4 treats these two positions as free and emits `'s_k : 'p` with cause `CallArg(0)` for the region `'p` of `c`'s signature at each of them, instead of reporting. Any other argument is related as a call of the bound at the scope call: the bound is instantiated as for `FnTrait` (§5.6.4) with its input positions replaced by `'s_k`, and its output is related to `F` by `flow`.
3. **The body outlives the scope** (rule item 3). For every region position `'x` of `B` and then of `F` in `ḡ`, in pre-order, it emits `'x : 's_k` with cause `(call point, ScopeOutlives)`. A position that is already `'s_k` gives a trivial constraint.

For each spawn call, with receiver type `borrow 'a Scope` whose `Scope` region is `'r`, and with type argument `C`:

4. **The child outlives the scope** (rule item 4). For every region position `'x` of `C`, in pre-order, it emits `'x : 'r` with cause `(call point, SpawnOutlives)`. Inside the argument of scope call `k`, `'r` equals `'s_k`, because the region parameter of `Scope` is invariant.
5. **Join handles** (rule item 1). The region of the returned `JoinHandle` is `'r`. The checker emits `'r : 'o` and `'o : 'r` for the call's output universal `'o`, instead of the assumptions `'x : 'o` of O1–O3.

After solving, and after the checks of §5.8:

6. **Escape** (rule item 5). For each scope call `k` in body order, and each region position `'o` of the call's `F.Output` (the `Output` of §5.15.1 for this future), in pre-order, the checker tests whether `'o` is `'s_k` or `'s_k` reaches `'o` in the constraint graph (§5.7.1), with the search of §5.10.5 row 6. The first position that passes gives the candidate `SCOPE_REGION_ESCAPE`. There is at most one candidate per scope call. Its primary span is the scope call, its label is the blame constraint of the path (§5.16.3), and its message names the type of `F.Output` that mentions the scope (11 N-I-11, N-B-45).
7. **Child borrows.** The two cause kinds make the long regions of rule items 3 and 4 visible to the cause search. A loan conflict at the storage end of a local of the body or of a child, whose cause path contains a `ScopeOutlives` or `SpawnOutlives` constraint, is `CHILD_TASK_BORROWS_LOCAL` (§5.10.5). A candidate of §5.8 whose cause path contains one is `CHILD_TASK_BORROWS_LOCAL` instead of `BORROW_ESCAPES_SIGNATURE` or `BORROWS_CONTRACT_VIOLATED`. This is the case of a spawn call in a function that receives `s: borrow Scope` as a parameter, where `'r` is a root universal. The primary span is the argument of the spawn call, or of the scope call for `ScopeOutlives`, of the first such constraint on the path. The labels name the borrowed local at the loan ("`x` is borrowed here"), its storage end ("`x` is dropped here, when the body or the child completes"), and the spawn call ("the child task may run until the scope ends").

The loan conflicts that spec/11 §11.7.3 lists after the rule need nothing more. A capture loan of a child flows into `'s_k` by item 4, so it is live at every point of the body after the spawn call. A second `borrow mut` of the same place is `CONFLICTING_BORROW` at that borrow (§5.10.5 row 1), and a write by the body is `ASSIGN_WHILE_BORROWED` or `CONFLICTING_BORROW`. Joining a child does not shorten any region (P8).

---

## 5.16 Error reporting

### 5.16.1 Assembly

Every candidate that survives deduplication (§5.4.4, §5.8, §5.10.8) and suppression (§5.16.6) becomes one `Diagnostic` of impl/11:

- `code`, and `severity` from the table of §5.22. `REDUNDANT_PROVENANCE` is a warning. Every other code of this document is an error.
- `primary`: the span of spec/07 §7.18.1, computed as in §5.16.3 to §5.16.4 and §5.14.
- `message`: regions are named by spec/07 §7.18.2. Places are rendered from source: locals by name, fields as `.f`, `*` for an explicit dereference, a capture by the captured variable, and a temporary as "the value of `<expr>`".
- `labels` and `related`: the three-point explanation (§5.16.4) as a `borrow_conflict` entry, or a `use_after_move` entry, or a `declared_here` entry for a region of a signature (impl/11 §11.8.3).
- `fixes`: §5.16.5.
- `node` and `symbol`: the HIR node of the primary span and its enclosing item. The cause key for cascade suppression (impl/11 §11.11.4) contains the root owner, the code, the primary node, and the signatures of the callees on the cause path.

`move_check` returns its diagnostics in point order, body by body. `borrowck` returns the diagnostics of §5.8 first (in the order of their checks), then those of §5.13.4, then the loan conflicts in point order. impl/11 sorts the final output.

### 5.16.2 Field mappings

The `borrow_conflict` and `use_after_move` entries of impl/11 §11.8.3 use these values (N-B-39):

| Field | Value |
|---|---|
| `loan_kind` | `"shared"` for a `Shared` loan and a guard loan. `"mutable"` for a `Mut` or `TwoPhaseMut` loan. impl/11 has no two-phase value. The message says "two-phase" when it matters |
| `access_kind` | `"read"` for a read. `"borrow"` for `Borrow(Shared)` and `RawPtr(Const)`. `"borrow_mut"` for `Borrow(Mut)`, a reservation, an activation, and `RawPtr(Mut)`. `"write"` for a shallow write and a `Replace` drop. `"move"` for a move. `"storage_dead"` for a scope drop, a `StorageDead`, and the implicit storage ends of `Return` and `CancelExit`. impl/11 has no `"drop"` value |
| `move_kind` | `"argument"` for a `Move` argument of a `Call` and the future of an `Await`. `"capture"` for an operand of a `Closure` rvalue. `"return"` for an assignment to `_0`. `"pattern"` when the `SourceInfo.hir` of the move is a binding pattern (F2). `"assign"` otherwise |
| `later_use` | the point of §5.16.4, or `null` when the loan is kept live by a universal region (impl/11 §11.8.3), with a note that names the return |
| `in_loop` | §5.4.4 |

### 5.16.3 Cause search

The cause search explains why a region `'r` contains a target, which is a point `Q` or an end marker `end('v)` (N-B-38). It is a breadth-first search over the constraint graph (§5.7.1) from `'r`. Each region is visited once, and edges are followed in emission order. Each visited region records the constraint through which it was reached. The search stops at the first region, in visiting order, whose **seed** (§5.7.2) contains the target:

- For a point target, the search is run up to three times. The first run accepts only use-liveness seeds, the second only drop-liveness seeds, the third only universals and the extra seeds of scope regions (§5.7.2). The first run that succeeds gives the cause. So a later use is preferred to a drop, and a drop to the signature.
- For an end-marker target, the accepting seeds are those of the universals and closure universals that contain the marker.

The result is the **cause path**, the sequence of constraints from `'r` to the accepting region, and the **cause seed**: a local and a liveness kind, or a universal. The search always succeeds, because the target is in `value('r)` and values are unions of the seeds of reachable regions (§5.7.3).

The **blame constraint** of a cause path is used as the primary span of the diagnostics of §5.8. It is the constraint of the path with the highest category, and the first one on the path among those of equal category:

| Category (highest first) | Constraint kinds |
|---|---|
| return | `Return`, and `Assign` whose destination is rooted at `_0` |
| argument | `CallArg(i)`, `CallReturn`, `Capture(i)`, `Await` |
| field | `AggregateField(i)` |
| assignment | `Assign`, `Convert` |
| borrow | `Borrow`, `Reborrow` |
| other | `Entry(i)`, `WellFormed`, `CallAssumption` |

For the return category, an `Assign` to `_0` is preferred to the `Return` terminator, because its `hir` is the returned expression (F2). For `CallArg(i)`, the span is argument `i`.

### 5.16.4 The three-point explanation

For a loan conflict `(Q, A, L)` (spec/07 §7.18.3):

1. **Created.** The span of `L.point`: "`q` is borrowed here", with the loan kind. A capture says "`c` is captured here by the closure". If `L.point = Q`, or `Q` comes before `L.point` in the reverse postorder of the body, the loan reached `Q` around a loop, and the label says "in the previous iteration of the loop".
2. **Conflict.** The primary span, the access `A` (§5.16.1).
3. **Later use.** The cause search for `Q` in `value(L.region)` gives the cause seed:
   - A use-live local `x`: a breadth-first search from the successors of `Q`, in `(block, statement)` order, visiting each point once and not continuing past a point that defines `x` (§5.6.7). The first point that uses `x` is the later use: "the borrow is used later here". If that point is `Q` or comes before it in reverse postorder, the label adds "in the next iteration".
   - A drop-live local `x`: the same search for a `Drop` of a place rooted at `x`: "the borrow is used later when `x` is dropped".
   - A universal: `later_use` is `null`. The label is placed on the blame constraint of the cause path (§5.16.3) and names the signature: "the borrow is returned here, so it must outlive the call".

The primary spans of the storage-end codes are not the access:

- `DOES_NOT_LIVE_LONG_ENOUGH` and `TEMPORARY_DROPPED_WHILE_BORROWED`: the borrow at `L.point`. The access becomes the label "`x` is dropped here while still borrowed".
- `RETURNS_LOCAL_BORROW`: the first constraint, on the path found by §5.10.5 row 6, whose target is a region of the type of `_0`. That is an assignment to `_0`, whose `hir` is the returned or tail expression. If the path reaches `Out` without such a constraint, the `Return` terminator.
- `BORROW_ESCAPES_ARENA`: §5.14.

### 5.16.5 Fixes

The checker offers the fixes of spec/07 §7.18.4, built with the operations of impl/11 §11.7:

| Code | Fix | Operation | Machine-applicable when |
|---|---|---|---|
| `ASSIGN_TO_IMMUTABLE`, `BORROW_MUT_OF_IMMUTABLE` | change `let` to `var`, or add `var` to the binding or parameter | `replace_node` on the `let` statement, with `let` replaced by `var`, or `insert_before` the binding or parameter with `var ` | the root is a `let` or a pattern binding or a parameter, its path has no `Deref` of a shared view, and pass B has no suppressed candidate on that root (§5.16.6) |
| same | change the parameter type to `borrow mut T` | `replace_node` on the parameter type | never: it changes a signature |
| `USE_AFTER_MOVE`, `MOVE_OUT_OF_BORROW` | add `.clone()` at the earliest move-out | `insert_after` | never: it changes behavior |
| `USE_AFTER_MOVE` | add `borrow` at the argument and change the parameter type | `insert_borrow` and `replace_node` | never: it changes a signature |
| `MOVE_OUT_OF_BORROW`, `MOVE_OUT_OF_DROP_TYPE` | use `core.mem.take` | `replace_node` | never |
| `MOVE_OUT_OF_BORROW` in a pattern | bind by `borrow` | `insert_borrow { mutable: false }` on the binding | the binding is only read and copied, by pass A's uses |
| `MOVE_OUT_OF_BOX` | use `Box.into_inner` | `replace_node` | the `Box` local is neither use-live nor drop-live after the move |
| `CONFLICTING_BORROW` at an argument (07 §7.10) | bind the argument first | `insert_before` the statement and `replace_node` the argument | the argument's type has no region and no drop glue, and every operand evaluated before it is a place, a borrow, or a constant |
| `TEMPORARY_DROPPED_WHILE_BORROWED` | bind the temporary | `insert_before` and `replace_node` | never: it changes the drop point |
| `DOES_NOT_LIVE_LONG_ENOUGH` with a capture loan | add `move` | `insert_before` the closure's `fn` | never: it changes the capture mode |
| `BORROWS_CONTRACT_VIOLATED` | add provenance (§5.8.1) | `insert_after` the last entry of the clause, or `insert_after` the clause that precedes `borrows` in the order of D-SYN-06 | never: it changes a signature |
| `BORROWS_WITHOUT_OUTPUT_REGION`, `REDUNDANT_PROVENANCE` | remove the clause or the entry | `remove_node` | always: the clause or entry has no meaning |
| `IMPL_SIGNATURE_REGION_MISMATCH` | write `Self.Item` | `replace_node` | never |
| `BORROW_ESCAPES_SIGNATURE` for a closure (§5.13.4) | write the closure in place | `replace_node` and `remove_node` | never: it moves the capture point |
| `ENSURES_USES_MOVED_PARAMETER` (§5.4.4) | copy the needed value with `old(..)` (spec/09 §9.3.4) | `replace_node` on the mention in the clause, with `old(<mention>)` | never: it changes what the clause states |

A fix is machine-applicable only if the condition of its row holds and it is the only fix of the diagnostic. The conditions are decided from the results that the checker already has, without re-checking. Each condition guarantees what spec/07 §7.18.4 requires: the edit is the only correct repair, it cannot change run-time behavior, the diagnostic disappears, and no new error appears. For `var`, a suppressed candidate of pass B on the same root could become visible after the edit, so its absence is required (N-B-40). Every other fix is `MaybeIncorrect`. Provenance fixes are always `MaybeIncorrect`.

### 5.16.6 Suppression

Pass B suppresses a candidate (N-B-30) in each of these cases:

1. Pass A reported a diagnostic at the same point.
2. The access place or the loan place is rooted at a local for which pass A reported `USE_AFTER_MOVE`, `ENSURES_USES_MOVED_PARAMETER`, `PARTIAL_ASSIGN_TO_MOVED`, `ASSIGN_TO_IMMUTABLE`, `BORROW_MUT_OF_IMMUTABLE`, or a `MOVE_OUT_OF_*` code.
3. The cause path contains a call of a callee whose `region_signature` has a diagnostic (cascade C4, §5.1.2).
4. The candidate is a conformance check (§5.8) of a root or closure whose signature has a diagnostic.

Suppressed candidates are kept in `BorrowckResults` and in the fact dump. They are not reported. They make the `var` fix of §5.16.5 maybe-incorrect. `BorrowckResults.error` is true if pass A or pass B has a diagnostic of severity error, reported or suppressed.

`GUARD_MOVE_OR_MUTATION` is reported by impl/03 (N-E-38), not by this checker. The other ownership codes that impl/03 and impl/08 report are listed in §5.22.

---

## 5.17 Complexity and determinism

### 5.17.1 Bounds

The bounds below use these sizes for one region problem: `N` points, `B` blocks, `V` locals, `M` move paths, `R` region variables, `E` outlives constraints, `K` loans, `t` the largest number of region positions in one type, `ℓ` the longest place, and `d` the loop nesting depth. `w` is the machine word size in bits.

| Step | Bound | Reason |
|---|---|---|
| move paths (§5.3) | `M ≤ V + N·ℓ` | each operand and destination creates at most `ℓ` paths |
| MaybeInit, MaybeUninit, liveness (§5.4.2, §5.6.7) | `O((d + 2) · N · M / w)` and `O((d + 2) · N · V / w)` | gen/kill bitset problems on a reducible CFG converge in `d + 2` passes in reverse postorder |
| constraint generation (§5.6) | `E = O(N · t)`, `R = O((V + N) · t)` | each element relates or instantiates the positions of a bounded number of types |
| solving (§5.7.3) | `O((R + E) · I)` | one Tarjan pass, then one union per edge between components. `I ≤ 2N` is the number of intervals of a value |
| loans in scope, activation (§5.9) | `O(K · I + (d + 2) · N · K / w)` | the per-point masks `{L : Q ∈ value(L.region)}` are built from the interval lists. The transfer function `((x ∩ mask) − kill) ∪ gen` is a bitset problem of the same kind |
| access check (§5.10.4) | `O(N · a · k · ℓ)` | `a` accesses per point, `k ≤ K` loans in scope, linear relevance test (§5.10.2) |
| conformance checks (§5.8) | `O(U · (R + E))` | `U` universals. Each check reads one value or runs one reachability search |
| cause search and later use (§5.16.3, §5.16.4) | `O(R + E + N)` per diagnostic | at most three searches over the graph, one over the CFG |
| `droplive` (§5.12.3) | linear in the number of distinct field types | memoized per problem |

The CFG of a body is reducible, because SAI has only structured control flow (06). A cancellation path (F4) leaves the loop structure unchanged, because it only reaches drops and `CancelExit`. Recursion in the checker is limited to walks over types and places, which recurse once per nesting level. The nesting limits of [spec/01](../spec/01-introduction.md) §1.5.4 bound their depth (impl/01 §1.8.1 item 2). Tarjan's algorithm, the dataflow worklists, and every search use explicit stacks or queues.

No step has a budget (§5.1.1 item 3, impl/01 §1.8.3 rule 7). The worst case is quadratic in the size of a body, in the access check and the loan masks. The benchmark corpus of impl/15 §15.11 measures the typical case against the targets of impl/01 §1.14.

### 5.17.2 Determinism

The checker satisfies the determinism rules of impl/01 §1.8.3 as follows:

1. **Dense indices.** Bodies, locals, blocks, points, move paths, region variables, constraints, loans, and candidates are dense indices into vectors. Sets are bitsets or sorted interval lists. Hash maps are used only for lookups: the move path `lookup` (§5.3.2), the `wf` cache (§5.6.1), the `droplive` memo (§5.12.3), and the cache of row 6 of §5.10.5. None of them is iterated.
2. **Fixed orders.** Every order that can reach a result is defined in this document: bodies in impl/04 order, locals and blocks in index order, statements in order, region variables in creation order (§5.5.4), constraints in emission order (§5.6.1), loans in point order (§5.9.1), Tarjan roots in vid order and successors in edge order (§5.7.3), breadth-first searches in edge order or `(block, statement)` order, candidates in the order of §5.10.4, and diagnostics in the order of §5.16.1.
3. **Order-independent values.** The region values, the loans in scope, and the dataflow states are least fixed points, which are unique. The visiting orders above affect only the numbering of components and the time taken. They are fixed so that dumps (§5.20.1) and the choice among equal causes are deterministic.
4. **No identifiers in messages.** Messages and labels never contain region vids, point numbers, or loan indices. Regions are named by spec/07 §7.18.2 and places are rendered from source (§5.16.1). Vids and points appear only in the fact dump.
5. **Parallelism.** `move_check` and `borrowck` are per-owner units of impl/01 §1.8.2 and run in parallel across owners. Inside one query, the checker is sequential.
6. **Cancellation.** The checker tests the cancellation flag once per basic block in every dataflow pass, in constraint generation, and in the access check (impl/01 §1.4.6 item 2). A cancelled query unwinds with `Cancelled` and memoizes nothing, so cancellation never changes a result.

---

## 5.18 Incremental compilation

All queries of §5.1.3 belong to the query group of the crate `sai_borrowck` (impl/01 §1.3.1). `region_signature`, `item_regions`, `move_check`, and `borrowck` are check queries (impl/01 §1.9): the driver demands `region_signature` for every function and method of the package, `item_regions` for every type declaration and impl, and `move_check` and `borrowck` for every owner whose `mir_built` has bodies. `borrowck_facts` is not a check query. Only tests demand it (§5.20).

| Query | Reads | Does not read |
|---|---|---|
| `region_signature(def)` | `fn_sig(def)`, `impl_header`, `region_params`, `variances_of`, `param_env`, `item_regions` of the impl, and for a trait impl method the trait method's `fn_sig` | any body |
| `item_regions(def)` | `type_of`, `adt_def`, `impl_header`, `region_params` | any body |
| `move_check(owner)` | `mir_built(owner)`, `typeck(owner)`, `is_copy`, drop glue and `Drop` impls, `invariant_of`, lang items | other owners' MIR |
| `borrowck(owner)` | `mir_drop_elaborated(owner)`, `move_check(owner)`, `region_signature` of the root and of every callee, `variances_of`, `typeck(owner)`, the inputs of `droplive` | callee bodies |

Consequences:

1. **Signatures are a firewall.** A caller's `borrowck` depends on a callee only through `region_signature(callee)`, whose inputs are the callee's declared signature. An edit inside a callee body changes neither, so the caller's `borrowck` stays green (impl/01 §1.4.3 item 4). This is D-OWN-03 in incremental form.
2. **Backdating.** An edit that changes a signature without changing its region interpretation, for example a renamed parameter, produces an equal `RegionSignature`, which is backdated (impl/01 §1.4.3 item 3). The callers are not re-checked. A changed `borrows` clause changes `region_signature` of that function and re-checks the bodies that call it.
3. **Values.** `MoveCheckResults` and `BorrowckResults` contain diagnostics, the drop classification, `used_mut`, the error flag, and the suppressed candidates. The region values, loan bitsets, and constraint graph are dropped when the query returns, except inside `borrowck_facts`. `BorrowckResults` is persisted with the per-owner results of phase 9 (impl/01 §1.6.3 item 2). It contains spans, `DefId`s, and plain indices only, so it has a stable encoding (impl/01 §1.6.2).
4. **The hooks.** `mir_drop_elaborated` reads `drop_classes(owner)`, and `mir_analyzed` reads `borrowck_failed(owner)` (impl/04 §4.1.4). `sai_borrowck` provides both as derived queries that project the drop classification of `move_check` and the error flag of `borrowck`. A change in a diagnostic that leaves the projection equal is backdated there (impl/01 §1.4.3 item 3), so it does not re-run drop elaboration or later phases.

---

## 5.19 Worked examples

The examples below show the facts of both passes for small programs. Each `sai` block is a test (impl/15 §15.8.3). Its helper functions are part of the block.

The listings use these conventions:

- The MIR is DropElaborated MIR, the input of pass B, in the informal rendering of §5.1.4. The authoritative rendering belongs to impl/04.
- Each local is listed with its type, its origin (F1), and, for user variables, its mutability.
- Statements that access nothing and relate no region (`CheckPoint`, `Nop`, `ProofObligation`) are omitted. They change no result.
- `'n` is the region variable with vid `n`, numbered by §5.5.1 and §5.5.4. `c0`, `c1`, and so on are the constraints in emission order (§5.6.1).
- A value is a union of half-open intervals `bbB[i..j]`, which contain the points `bbB[i]` to `bbB[j-1]`, plus end markers.
- A loan is written `L = (point, kind, place, region)` (§5.9.1).

### 5.19.1 Assignment while shared

This is the example of spec/07 §7.9.8, changed so that `*r` is the result. A `let y` that is never used would add a lint warning.

```sai
fn assign_while_shared() -> I32 {
    var x = 1
    let r = borrow x
    x = 2 // rejected: ASSIGN_WHILE_BORROWED
    *r
}
```

```text
fn assign_while_shared() -> I32
    _0: I32          Return
    _1: I32          UserVar(x)   Mut
    _2: borrow I32   UserVar(r)   Not

bb0: {
    StorageLive(_1)             // bb0[0]
    _1 = Use(const 1)           // bb0[1]
    StorageLive(_2)             // bb0[2]
    _2 = Borrow(Shared, _1)     // bb0[3]   L0
    _1 = Use(const 2)           // bb0[4]
    _0 = Use(copy *_2)          // bb0[5]
    StorageDead(_2)             // bb0[6]
    StorageDead(_1)             // bb0[7]
    Goto -> bb1                 // bb0[8]
}
bb1: {
    Return                      // bb1[0]
}

regions       '0 'static    '1 in type(_2)    '2 the region of L0
constraints   c0  '2 : '1   Borrow   bb0[3]
use-live      _2: bb0[4..6]
values        '1 = bb0[4..6]    '2 = bb0[4..6]
loans         L0 = (bb0[3], Shared, _1, '2)
in scope      L0: bb0[4]. Killed at bb0[4] by the write of the whole of _1
candidates    (bb0[4], write _1 shallow, L0): row 3, ASSIGN_WHILE_BORROWED
```

- The function has no input region, so there is no root universal.
- `r` is used at bb0[5]. So `_2` is use-live at bb0[4] and bb0[5], and the region of its type gets these points (§5.6.7). The loan's region outlives it through `c0`.
- At bb0[4], L0 is in `in(bb0[4])`, because bb0[4] is in `value('2)` (§5.9.2). The write of `_1` is shallow. The loan place `_1` is a prefix of it, so the loan is relevant (§5.10.2). A write conflicts with an effectively shared loan (§5.10.3). The write also kills L0, but only in `out(bb0[4])`, so the access is still checked.
- **Explanation** (§5.16.4). The loan is created at `borrow x` ("`x` is borrowed here"). The primary span is the assignment `x = 2` (F2). For the later use, the cause search for bb0[4] in `value('2)` starts at `'2`, whose seed is empty, and follows `c0` to `'1`, whose use-liveness seed contains bb0[4]. The cause seed is `_2`, use-live. The search over the CFG from bb0[5] finds the use `*r` ("the borrow is used later here").
- The `borrow_conflict` entry has `loan_kind = "shared"`, `access_kind = "write"`, and `later_use` at `*r` (§5.16.2). §5.16.5 offers no fix for this code.

### 5.19.2 A two-phase receiver

This is `two_phase` of 07 §7.10. The receiver `v` of `push` already has type `borrow mut Vec<USize>`, so it is reborrowed implicitly, and the reborrow is two-phase (07 §7.10 case 1, F7).

```sai
fn two_phase(v: borrow mut Vec<USize>) {
    v.push(v.len())
}
```

```text
fn two_phase(_1: borrow mut Vec<USize>) -> ()
    _0: ()                      Return
    _1: borrow mut Vec<USize>   Param
    _2: borrow mut Vec<USize>   Temp     the receiver of `push`
    _3: USize                   Temp     the value of `v.len()`
    _4: borrow Vec<USize>       Temp     the receiver of `len`

bb0: {
    StorageLive(_2)                               // bb0[0]
    _2 = Borrow(TwoPhaseMut, *_1)                 // bb0[1]   L0
    StorageLive(_3)                               // bb0[2]
    StorageLive(_4)                               // bb0[3]
    _4 = Borrow(Shared, *_1)                      // bb0[4]   L1
    _3 = Call Vec.len(move _4) -> bb1             // bb0[5]
}
bb1: {
    _0 = Call Vec.push(move _2, move _3) -> bb2   // bb1[0]   activates L0
}
bb2: {
    StorageDead(_4)                               // bb2[0]
    StorageDead(_3)                               // bb2[1]
    StorageDead(_2)                               // bb2[2]
    Goto -> bb3                                   // bb2[3]
}
bb3: {
    Return                                        // bb3[0]
}

regions       '0 'static    '1 the input of `v` (root universal)
              '2 in type(_1)    '3 in type(_2)    '4 in type(_4)
              '5 the region of L0    '6 the region of L1
              '7 the input of `Vec.len` at bb0[5]    '8 the input of `Vec.push` at bb1[0]
constraints   c0  '1 : '2   Entry(1)
              c1  '5 : '3   Borrow     bb0[1]
              c2  '2 : '5   Reborrow   bb0[1]
              c3  '6 : '4   Borrow     bb0[4]
              c4  '2 : '6   Reborrow   bb0[4]
              c5  '4 : '7   CallArg(0) bb0[5]
              c6  '3 : '8   CallArg(0) bb1[0]
use-live      _1: bb0[0..5]    _2: bb0[2..6], bb1[0..1]    _4: bb0[5..6]
values        '3 = '5 = bb0[2..6] ∪ bb1[0..1]    '4 = '6 = bb0[5..6]
              '2 = bb0[0..6] ∪ bb1[0..1]    '7 = '8 = ∅
              '1 = every point, end('1)
loans         L0 = (bb0[1], TwoPhaseMut, *_1, '5), activation bb1[0]
              L1 = (bb0[4], Shared, *_1, '6)
in scope      L0: bb0[2] to bb1[0], reserved throughout    L1: bb0[5]
candidates    none
```

- At bb0[4], the shared borrow for `len` reads `*_1` while L0 is in scope. L0 is reserved, so its effective kind is `Shared` (§5.10.3), and a read does not conflict with it.
- The temporary `_4` is moved into `Vec.len` at bb0[5] and never used again. So `value('6)` is only bb0[5], and L1 is not in scope at bb1[0].
- At bb1[0], the activation of L0 is a deep write of `*_1`. It is checked against `in(bb1[0]) = {L0}` and skipped for L0 itself (§5.10.4). There is no conflict.
- Neither loan reaches bb2. The `StorageDead`s there conflict with nothing.

With an explicit `borrow mut`, the same call has no reservation (07 §7.10):

```sai
fn explicit_borrow_mut() {
    var v: Vec<USize> = Vec.new()
    Vec.push(borrow mut v, v.len()) // rejected: CONFLICTING_BORROW
}
```

The argument `borrow mut v` is `Borrow(Mut, _1)`, effectively `Mut` from its creation. It stays in scope up to the `Call`, which moves it. The receiver of `len` is `Borrow(Shared, _1)`, a deep read of `_1` that conflicts with it. Row 1 of §5.10.5 applies, because the access is a `Borrow`. The primary span is `v` in `v.len()`. The fix "bind the argument first" (§5.16.5) is machine-applicable here: the argument has type `USize`, which has no region and no drop glue, and the only operand evaluated before it is a borrow.

### 5.19.3 A borrow that does not live long enough

This is the example of spec/07 §7.9.8, with its helper.

```sai
fn print_len(s: borrow String) { }

fn does_not_live_long_enough() {
    let outer = String.new()
    var r = borrow outer
    {
        let inner = String.new()
        r = borrow inner // rejected: DOES_NOT_LIVE_LONG_ENOUGH
    }
    print_len(r)
}
```

```text
fn does_not_live_long_enough() -> ()
    _0: ()              Return
    _1: String          UserVar(outer)   Not
    _2: borrow String   UserVar(r)       Mut
    _3: String          UserVar(inner)   Not
    _4: borrow String   Temp             the value of `borrow inner`

bb0: {
    StorageLive(_1)                         // bb0[0]
    _1 = Call String.new() -> bb1           // bb0[1]
}
bb1: {
    StorageLive(_2)                         // bb1[0]
    _2 = Borrow(Shared, _1)                 // bb1[1]   L0
    StorageLive(_3)                         // bb1[2]
    _3 = Call String.new() -> bb2           // bb1[3]
}
bb2: {
    StorageLive(_4)                         // bb2[0]
    _4 = Borrow(Shared, _3)                 // bb2[1]   L1
    _2 = Use(copy _4)                       // bb2[2]
    StorageDead(_4)                         // bb2[3]
    Drop(_3, ScopeExit) -> bb3              // bb2[4]   the `}` of the inner block
}
bb3: {
    StorageDead(_3)                         // bb3[0]
    _0 = Call print_len(copy _2) -> bb4     // bb3[1]
}
bb4: {
    StorageDead(_2)                         // bb4[0]
    Drop(_1, ScopeExit) -> bb5              // bb4[1]
}
bb5: {
    StorageDead(_1)                         // bb5[0]
    Goto -> bb6                             // bb5[1]
}
bb6: {
    Return                                  // bb6[0]
}

regions       '0 'static    '1 in type(_2)    '2 in type(_4)
              '3 the region of L0    '4 the region of L1    '5 the input of `print_len` at bb3[1]
constraints   c0  '3 : '1   Borrow      bb1[1]
              c1  '4 : '2   Borrow      bb2[1]
              c2  '2 : '1   Assign      bb2[2]
              c3  '1 : '5   CallArg(0)  bb3[1]
use-live      _2: bb2[3..5] ∪ bb3[0..2]    _4: bb2[2..3]
drop-live     _1 at bb4[1] and _3 at bb2[4], with droplive(String) = ∅
values        '1 = '3 = bb2[3..5] ∪ bb3[0..2]
              '2 = '4 = bb2[2..5] ∪ bb3[0..2]    '5 = ∅
loans         L0 = (bb1[1], Shared, _1, '3)
              L1 = (bb2[1], Shared, _3, '4)
in scope      L0: never    L1: bb2[2] to bb3[0]. Killed at bb3[0] by StorageDead(_3)
candidates    (bb2[4], drop _3, L1): row 8, DOES_NOT_LIVE_LONG_ENOUGH
              (bb3[0], storage-dead _3, L1): row 8, removed by §5.10.8 rule 2
```

- `r` has one type and therefore one region, `'1`. That region contains the points where `r` holds the borrow of `inner`, and through `c0` so does the region of L0. But L0 is never in scope: bb1[2] is not in `value('3)`, so L0 is not carried out of its creation point (§5.9.2). A reassigned variable does not keep its old loan alive (spec/07 §7.9.5).
- At bb2[4], `Drop(_3)` is a drop access to `_3`, and L1 is relevant (§5.10.2). `_3` is a user variable, and `L1`'s region reaches no region of `type(_0)`, so row 8 of §5.10.5 applies.
- **Explanation.** The primary span is the borrow at `L1.point`, `borrow inner` (§5.16.4). The access becomes the label "`inner` is dropped here while still borrowed", at the closing `}` of the inner block, which is the span of the `ScopeExit` drop (impl/04 §4.6.3). The cause search for bb2[4] in `value('4)` visits `'4` (empty seed), then `'2` through `c1`, whose seed bb2[2] does not contain the target, then `'1` through `c2`, whose use-liveness seed does. The later use is `r` in `print_len(r)`.
- The `borrow_conflict` entry has `loan_kind = "shared"` and `access_kind = "storage_dead"`. §5.16.5 offers no fix, because the loan is not a capture.

### 5.19.4 A `borrows` clause that is too narrow

This is `pick` of spec/07 §7.7.4.

```sai
fn pick(a: Str, b: Str) -> Str
    borrows { a }
{
    b // rejected: BORROWS_CONTRACT_VIOLATED
}
```

```text
fn pick(_1: Str, _2: Str) -> Str
    _0: Str   Return
    _1: Str   Param
    _2: Str   Param

bb0: {
    _0 = Use(copy _2)     // bb0[0]
    Goto -> bb1           // bb0[1]
}
bb1: {
    Return                // bb1[0]
}

regions       '0 'static    '1 the input of `a`    '2 the input of `b`    '3 Out
              '4 in type(_0)    '5 in type(_1)    '6 in type(_2)
assumptions   'static : every universal, reflexivity, and '1 : '3 (the source `a`)
constraints   c0  '1 : '5   Entry(1)
              c1  '2 : '6   Entry(2)
              c2  '6 : '4   Assign   bb0[0]
              c3  '4 : '3   Return   bb1[0]
seeds         '1: every point, end('1), end('3)    '2: every point, end('2)
              '3: every point, end('3)    '6: bb0[0]    '4, '5: none
values        '4 = '6 = every point, end('3)
              '2 = every point, end('2), end('3)
candidates    §5.8.1: end('3) in value('2), and '2 : '3 is not an assumption:
              BORROWS_CONTRACT_VIOLATED, missing source '2
```

- `region_signature(pick)` maps `a` and `b` to the input universals `'1` and `'2`, and the result to `Out` (O1). The clause `borrows { a }` gives the source `'1` (O2), so the only assumption besides the trivial ones is `'1 : '3`.
- `b` flows into `_0` and `_0` into the declared return type. So `value('2)` contains `end('3)`. The body creates no loan, so the error is a conformance error (§5.8.1), not a loan conflict.
- **Cause and blame** (§5.16.3). The search for `end('3)` from `'2` follows `c1`, `c2`, `c3` and accepts at `'3`, whose seed contains `end('3)`. On the path `[c1, c2, c3]`, `c2` and `c3` are in the return category, and the `Assign` to `_0` is preferred. So the primary span is `b`, the `hir` of `c2`.
- The `declared_here` entry points at the clause `borrows { a }`. The message names the region of `b` by spec/07 §7.18.2.
- **Fix** (§5.8.1, §5.16.5). The most precise path that selects `'2` is the parameter `b`. With an existing clause, the fix is `insert_after` the entry `a`, with the text `, b`, which gives `borrows { a, b }`. It is `MaybeIncorrect`, because it changes a signature.

### 5.19.5 An arena borrow that escapes

This is `arena_escape` of spec/07 §7.15.4.

```sai
fn arena_escape() -> I32 {
    var keep = borrow 0
    region r {
        let p = r.alloc(5)
        keep = borrow *p // rejected: BORROW_ESCAPES_ARENA
    }
    *keep
}
```

```text
fn arena_escape() -> I32
    _0: I32              Return
    _1: I32              Temp            the extended temporary of `0` (impl/04 §4.6.2)
    _2: borrow I32       UserVar(keep)   Mut
    _3: Arena            ArenaStorage
    _4: borrow Arena     UserVar(r)      Not
    _5: borrow mut I32   UserVar(p)      Not
    _6: borrow I32       Temp            the value of `borrow *p`

bb0: {
    StorageLive(_1)                                       // bb0[0]
    _1 = Use(const 0)                                     // bb0[1]
    StorageLive(_2)                                       // bb0[2]
    _2 = Borrow(Shared, _1)                               // bb0[3]   L0
    StorageLive(_3)                                       // bb0[4]
    _3 = Call Arena.region_new() -> bb1                   // bb0[5]
}
bb1: {
    StorageLive(_4)                                       // bb1[0]
    _4 = Borrow(Shared, _3)                               // bb1[1]   L1
    StorageLive(_5)                                       // bb1[2]
    _5 = Call Arena.alloc<I32>(copy _4, const 5) -> bb2   // bb1[3]
}
bb2: {
    StorageLive(_6)                                       // bb2[0]
    _6 = Borrow(Shared, *_5)                              // bb2[1]   L2
    _2 = Use(copy _6)                                     // bb2[2]
    StorageDead(_6)                                       // bb2[3]
    StorageDead(_5)                                       // bb2[4]
    StorageDead(_4)                                       // bb2[5]
    Drop(_3, ScopeExit) -> bb3                            // bb2[6]   the `}` of the region block
}
bb3: {
    StorageDead(_3)                                       // bb3[0]
    _0 = Use(copy *_2)                                    // bb3[1]
    StorageDead(_2)                                       // bb3[2]
    StorageDead(_1)                                       // bb3[3]
    Goto -> bb4                                           // bb3[4]
}
bb4: {
    Return                                                // bb4[0]
}

regions       '1 in type(_2)    '2 in type(_4)    '3 in type(_5)    '4 in type(_6)
              '5, '6, '9 the regions of L0, L1, L2
              '7, '8 the input and Out of `Arena.alloc` at bb1[3]
constraints   c0  '5 : '1   Borrow           bb0[3]
              c1  '6 : '2   Borrow           bb1[1]
              c2  '7 : '8   CallAssumption   bb1[3]   (rule (b): the output borrows `self`)
              c3  '2 : '7   CallArg(0)       bb1[3]
              c4  '8 : '3   CallReturn       bb1[3]
              c5  '9 : '4   Borrow           bb2[1]
              c6  '3 : '9   Reborrow         bb2[1]
              c7  '4 : '1   Assign           bb2[2]
use-live      _2: bb2[3..7] ∪ bb3[0..2]    _4: bb1[2..4]    _5: bb2[0..2]    _6: bb2[2..3]
drop-live     _3 at bb2[6], with droplive(Arena) = ∅
values        '1 = '5 = bb2[3..7] ∪ bb3[0..2]
              '4 = '9 = bb2[2..7] ∪ bb3[0..2]
              '3 = '7 = '8 = bb2[0..7] ∪ bb3[0..2]
              '2 = '6 = bb1[2..4] ∪ bb2[0..7] ∪ bb3[0..2]
loans         L0 = (bb0[3], Shared, _1, '5)     in scope: never
              L1 = (bb1[1], Shared, _3, '6)     in scope: bb1[2] to bb3[0], killed by StorageDead(_3)
              L2 = (bb2[1], Shared, *_5, '9)    in scope: bb2[2] to bb2[4], killed by StorageDead(_5)
candidates    (bb2[6], drop _3, L1): row 5, BORROW_ESCAPES_ARENA
              (bb3[0], storage-dead _3, L1): row 5, removed by §5.10.8 rule 2
```

- The constraints chain the arena loan to `keep`: `'6` (the loan) outlives the type of `r`, which outlives the input of `alloc`. That input outlives `alloc`'s output by the callee's assumption. The output outlives the type of `p`, which outlives the reborrow `borrow *p`, which flows into `keep`. `keep` is use-live after the block, so `value('6)` contains the drop of the arena.
- At bb2[4], `StorageDead(_5)` does not conflict with L2: its access is shallow, and L2's place `*_5` extends `_5` through a `Deref` (§5.10.2). The storage of `p` may end while borrows of its referent live on.
- **Primary span** (§5.14 item 2). The cause search for bb2[6] in `value('6)` gives the path `c1, c3, c2, c4, c6, c5, c7` and accepts at `'1`, where `keep` is use-live. The targets of `c1`, `c4`, and `c5` are regions of the types of `r`, `p`, and the temporary, which are declared inside the block. The targets of `c3`, `c2`, and `c6` are not regions of a local's type. The first qualifying constraint is `c7`, whose target is the type of `keep`. Its `hir` is the assignment `keep = borrow *p` (F2), which is the primary span.
- **Labels.** "The arena `r` is created here" at `region r`, "the arena is freed here, at the end of the block" at the closing `}`, and the later use `*keep`.

### 5.19.6 Closures against a bound

```sai
fn run<F: Fn(borrow I32) -> I32>(f: F) -> I32 {
    let x = 1
    f(borrow x)
}

fn closure_in_place() -> I32 {
    run(fn(v: borrow I32) -> I32 { *v })
}

fn closure_later() -> I32 {
    let get = fn(v: borrow I32) -> I32 { *v }
    run(get) // rejected: BORROW_ESCAPES_SIGNATURE
}
```

- In `run`, `f(borrow x)` is an `FnTrait` call (§5.6.4). The bound's input region becomes a fresh variable of this call, and the loan of `x` flows into it. The output `I32` has no region, so the loan ends at the call.
- In `closure_in_place`, impl/03 deduces the expected signature from the bound of `F` (impl/03 §3.11.1). The region of `v` is a bound input region `'b` of the closure (§5.13.2). Its seed is the closure's scope and `end('b)`. The body only reads `*v` and returns an `I32`, so no region of the enclosing body contains `end('b)`, and §5.8.2 reports nothing.
- In `closure_later`, the `let` gives no expected type. The region of `v` is a signature region in `S_c` of the enclosing body (§5.13.1). impl/03 proves `Closure(c, ..): Fn(borrow I32) -> I32` at the argument `get`. §5.13.4 finds that the bound's input position is bound and that `c`'s signature has a region there. The result is `BORROW_ESCAPES_SIGNATURE` on `get`.
- `get` is initialized by the closure expression and has no other use. So the fix "write the closure in place" (spec/07 §7.18.4) is offered: `replace_node` of the argument with the closure expression, and `remove_node` of the `let`. It is never machine-applicable (§5.16.5).

### 5.19.7 A conditional drop and a use after a possible move

```sai
fn consume(s: String) { }

fn print_len(s: borrow String) { }

fn maybe_consume(c: Bool) {
    let s = String.new()
    if c {
        consume(s)
    }
}

fn use_after_maybe_move(c: Bool) {
    let s = String.new()
    if c {
        consume(s)
    }
    print_len(borrow s) // rejected: USE_AFTER_MOVE
}
```

The Built MIR of `maybe_consume`, which is the input of pass A, is:

```text
fn maybe_consume(_1: Bool) -> ()
    _2: String   UserVar(s)   Not

bb0: {
    StorageLive(_2)                                   // bb0[0]
    _2 = Call String.new() -> bb1                     // bb0[1]
}
bb1: {
    SwitchInt(copy _1) -> [0: bb3, otherwise: bb2]    // bb1[0]
}
bb2: {
    _0 = Call consume(move _2) -> bb4                 // bb2[0]   move of the path _2
}
bb3: {
    _0 = Use(const ())                                // bb3[0]
    Goto -> bb4                                       // bb3[1]
}
bb4: {
    Drop(_2, ScopeExit) -> bb5                        // bb4[0]
}
bb5: {
    StorageDead(_2)                                   // bb5[0]
    Goto -> bb6                                       // bb5[1]
}
bb6: {
    Return                                            // bb6[0]
}
```

- **Classification** (§5.12.1). At bb4[0], the bit of `_2` is set in MaybeInit (along bb3) and in MaybeUninit (along bb2). `_2` has no strict descendant with a move event. The class is `Conditional`, and `_2` needs a drop flag.
- **Elaboration** (impl/04). The flag is appended as `_3: Bool` with origin `DropFlag` (F16). By §5.12.2 it is true after bb0[1] and false after the move in bb2. The `Drop` runs only where the flag is true. In pass B, the remaining `Drop` makes `_2` drop-live at its point. `droplive(String)` is `∅`, so this adds nothing to any region.
- **Use after the move.** In `use_after_maybe_move`, `borrow s` after the `if` is a read of `_2` at a point where its bit is set in MaybeUninit (§5.4.4). The failing path is `_2`. The backward search for move-outs finds the move at `consume(s)`. Along the other branch it stops at the initialization at bb0[1]. The diagnostic has its primary span at `s` in `borrow s` and the label "`s` is moved here" at `consume(s)`. The `use_after_move` entry has `move_kind = "argument"` and `in_loop = false`. Both fixes of §5.16.5 for this code are `MaybeIncorrect`.
- Pass B still runs on this body (N-B-30). Any of its candidates whose place is rooted at `_2` would be suppressed by §5.16.6 rule 2.

---

## 5.20 Test hooks

### 5.20.1 The fact dump

`borrowck_facts(owner)` runs both passes on the owner again, keeps their internal data, and renders it as text (N-B-43). `borrowck` itself keeps none of this data (§5.18 item 3). The text is UTF-8 with LF line ends. Its first line is `borrowck-facts 1`, the format version (impl/01 §1.13.3). Every other line is one record: a keyword followed by fields separated by one space. The sections come in this order, and the records of a section in index order:

| Section | Record |
|---|---|
| owner | `owner <path>`, the canonical path of the root owner |
| bodies | `body <k> <source> <path>`, then per local `local _<i> <type> <origin> <mutability>`, then per block `block bb<B> <first point>..<last point>` |
| regions | `region '<n> <kind> <what> seed <value> value <value>`, with the kinds of §5.5.1 and what created the variable (§5.5.4) |
| assumptions | `assume '<a> : '<b>`, the closed relations of the root and of each closure |
| constraints | `constraint c<n> '<a> : '<b> <kind> <point>`, then `typetest <P> '<l> <point> holds\|fails` |
| liveness | `live use\|drop _<i> <points>` per body |
| loans | `loan L<n> <point> <kind> <place> '<r>`, with `guard` and `activation <point>` when they apply, then `inscope L<n> <points>` |
| move data | `path m<n> <place> parent m<p>`, `move m<n> <point>`, `init m<n> <point> <kind>`, `drop <point> <class>` (§5.12.1) |
| candidates | `candidate <code> <point> <loan or region> cause c<a>,c<b>,.. <outcome>`, where the outcome is `reported`, `deduplicated <rule>`, or `suppressed <rule>` (§5.10.8, §5.16.6) |

Types are rendered as in the MIR text of impl/04, with `'<n>` at each region position. Places are MIR places. A point is `bbB[i]`, prefixed by `b<k>:` in a problem with more than one body. A value is a list of intervals `bbB[i..j]` and end markers `end('<n>)`, separated by commas, in point order. The dump contains no address, no hash, and no time, so it is byte-for-byte deterministic (§5.17.2).

The format has no compatibility guarantee between toolchain versions, like the MIR text (impl/01 §1.13.3). Its version is incremented whenever the record syntax changes. A change of the format re-blesses every fact golden in the same change (impl/15 §15.4.1). Only the test harness demands `borrowck_facts`, through the API of `sai_borrowck`. No command or option of impl/13 exposes it (N-H-18, impl/01 N-J-34).

### 5.20.2 Verify mode

The Cargo feature `borrowck-verify` of `sai_borrowck` enables cross-checks. The shipped binary never enables it, like `ice-injection` (impl/15 §15.12.3). No command-line option and no environment variable enables it (N-B-43). With the feature, each query also performs these checks:

1. **Solver.** A naive fixed-point iteration over the constraints, in emission order and repeated until nothing changes, gives the same value for every region as §5.7.3.
2. **Order independence.** Solving with the constraints in reverse emission order, and computing the dataflows with the blocks in reverse index order, gives the same values and the same states (§5.17.2 item 3).
3. **Loans in scope.** For every loan, the depth-first search of §5.9.2 gives the same points as the dataflow.
4. **Initialization.** After a pass A without errors, every read in DropElaborated MIR of a move path is definitely initialized (§5.4.6), and every drop flag satisfies the invariant of §5.12.2 at every point where it is read.
5. **Cause search.** For every diagnostic, every constraint on the cause path is in the graph, and the target is in the seed of the accepting region (§5.16.3).

A failed check is an internal compiler error (impl/11 §11.4.6) that names the owner and the check. The checks never change a diagnostic. The compiler-level tests run the whole conformance suite once with the feature enabled (§5.20.3).

### 5.20.3 Test suites

The checker's tests live in `tests/impl/borrowck/` (impl/15 §15.12.2):

1. **Examples.** Every `sai` block of 07 and of this document is a test (impl/15 §15.8.3).
2. **Fact goldens.** A test `<name>.sai` with a golden `<name>.facts` compares the fact dump of every owner, blessed by impl/15 §15.4.1. The goldens cover every example of §5.19, every row of §5.10.5, every constraint kind of §5.6.1, every region kind of §5.5.1, and every drop class of §5.12.1.
3. **Diagnostic goldens.** For every code of §5.22 that this checker reports, at least one test compares the JSON diagnostics (impl/15 §15.4.4): the primary span, the labels, the `borrow_conflict` or `use_after_move` entry (§5.16.2), and the fixes with their applicability.
4. **Fixes.** For every fix row of §5.16.5, a test applies the fix and compiles again. For a machine-applicable fix, the diagnostic MUST disappear, the program MUST NOT get a new error, and a program that runs MUST produce the same output as before (spec/07 §7.18.4).
5. **Verify mode.** The suite of impl/15 runs once with `borrowck-verify` (§5.20.2), with `--jobs 1` and `--jobs 16`. The diagnostics and the fact dumps MUST be identical in both runs.
6. **Rust oracle.** The harness SHOULD translate the programs of the generator of impl/15 §15.10.2 that use only the constructs of spec/07 §7.19 into Rust, by the translation of spec/07 §7.19, and check them with the borrow checker of the pinned Rust toolchain of the build. Every program that this checker accepts MUST be accepted by Rust. A program that Rust accepts and this checker rejects is listed in the report and does not fail the test, because 07 is stricter than Rust in places (07 §7.1.3).

---

## 5.21 Decisions introduced by this document

decisions.md and 07 are silent on the following points. They are decided here by the principles P1–P9 and are provisional until they are merged into decisions.md. The IDs continue the series of 07 (N-B-1 to N-B-28).

| ID | Rule | Rationale | Section |
|---|---|---|---|
| N-B-29 | The checker is two queries per root owner. `move_check` (pass A) runs on Built MIR and reports move, use-after-move, and write-structure errors. `borrowck` (pass B) runs on DropElaborated MIR and reports loan and region errors. Drop elaboration sits between them and uses the results of pass A. | Pass B needs the drops that remain after elaboration to compute drop-liveness (spec/07 §7.9.4), and elaboration needs initialization. One pass cannot have both. P8: each error has exactly one producing pass. | §5.1.2 |
| N-B-30 | Pass B runs whenever pass A ran, even if pass A reported errors. It suppresses a candidate at a point where pass A reported, on a place rooted at a local that pass A reported, on a cause path through a callee with a signature error, and for a conformance check of a signature with an error. Suppressed candidates are kept, and they make `BorrowckResults.error` true. | P8: the user sees every independent error in one run, but never two explanations of one mistake (impl/11 §11.11.4). | §5.1.2, §5.16.6 |
| N-B-31 | One region problem per root body. It contains the root body, its closure and `async` block bodies, and its contract clause bodies. Each `invariant` element and each derived method is a separate root with the signature that impl/04 §4.3.3 gives it. A contract clause body is checked like a closure with an expected signature and no environment. | Closure regions are related to the enclosing body without propagating requirements (07 §7.11.1, N-B-17), which needs both bodies in one problem. P8: a separate problem per element keeps one element's errors from affecting another's. | §5.1.2, §5.5.1 |
| N-B-32 | A move path stops at every `Deref`, including the `Deref` of a `Box`. `*b` and the places below it are initialized exactly when `b` is. | Equivalent to 07 §7.4.1, because nothing can be moved out of `*b` (N-B-2) and its only lone drop is a `Replace` drop followed by a write (F3). P9: one rule for all pointers. | §5.3.1 |
| N-B-33 | `Return` and `CancelExit` are a shallow storage-dead of every local other than `_0` whose storage has not ended, including parameters. | Parameters have no `StorageDead` (F11). Without this rule, returning a borrow of a parameter without drop glue would not be detected (spec/07 §7.7.4). P2: soundness. | §5.10.1 |
| N-B-34 | The facts F1–F18 are requirements on impl/04. If impl/04 provides them under other names, its names govern, and the meaning given here is required. | D-MIR-01 defines the data structures, and it is impl/04's to extend (D-MIR-01). The checker states what it relies on so that a change of impl/04 that breaks it is visible. | §5.2.2 |
| N-B-35 | The loan created by a borrow whose destination has origin `GuardScrutineeRef` is a guard loan. It is shallow: it conflicts only with an access of a prefix of its place, and its creation is a shallow read. It is reported as a shared loan. | It implements the shallow scrutinee borrow of impl/03 N-E-38 without rejecting the arm bindings, which borrow parts of the scrutinee. P9: the same mechanism as an ordinary loan. | §5.9.1, §5.10.7 |
| N-B-36 | In a take-and-restore call (F14), the argument `Move(p')` is a deep read of `p'` and the destination `p'` is a shallow write, instead of a move and a write. | spec/06 §6.13.2 allows the call behind `borrow mut`, and no code observes `p'` between the take and the restore. A read and a write give the codes that users expect for `p op= e` (`USE_WHILE_MUT_BORROWED`, `ASSIGN_WHILE_BORROWED`) instead of `MOVE_WHILE_BORROWED` (P8). | §5.10.1 |
| N-B-37 | Deduplication. Pass A reports one `USE_AFTER_MOVE` per distinct set of move-outs, at the first use. Each check of §5.8 keeps the first candidate per cause point. Pass B keeps the first candidate per point, then the first per loan and code, and removes the write after a `Replace` drop that has a candidate with the same loan. | P8: one diagnostic per mistake, chosen by a total order of points and indices, so the output is deterministic (§5.17.2). | §5.4.4, §5.8, §5.10.8 |
| N-B-38 | The cause search is a breadth-first search over the constraint graph in emission order. It prefers use-liveness to drop-liveness, and drop-liveness to universals. The blame constraint is the first one of the highest category in the order return, argument, field, assignment, borrow, other. | P8: a deterministic explanation that points at the use the user wrote, not at an internal constraint. The order of categories follows how users locate escaping borrows. | §5.16.3 |
| N-B-39 | The `borrow_conflict` and `use_after_move` entries of impl/11 §11.8.3 are filled by the table of §5.16.2. A two-phase loan is `"mutable"`, and a drop is `"write"` (a `Replace` drop) or `"storage_dead"` (a scope drop). | impl/11 has neither a two-phase loan kind nor a drop access kind. The mapping uses only the existing values, so the JSON schema of impl/11 stays as it is (D-TOOL-01). | §5.16.2 |
| N-B-40 | A fix is machine-applicable only if the condition of its row in §5.16.5 holds and it is the only fix of the diagnostic. The conditions use only the checker's results, without compiling again. The `var` fix also requires that pass B has no suppressed candidate on the same root. | spec/07 §7.18.4 requires that a machine-applicable fix removes the diagnostic, adds no error, and keeps behavior (impl/11 §11.7.2). A suppressed candidate could become visible after `var`. P8: no speculative compilation. | §5.16.5 |
| N-B-41 | `ENSURES_USES_MOVED_PARAMETER` is found by pass A, as a use after move of a parameter local inside an `Ensures` scope (F18), and reported instead of `USE_AFTER_MOVE`, once per scope and parameter. | spec/09 §9.3.4 (N-C-48) defines the error by paths to return points, which is exactly the MaybeUninit dataflow. The result is independent of the SFC (D-CONTRACT-04). | §5.4.4 |
| N-B-42 | A closure or `async` block type is invariant in every region it carries. `Future(f, ḡ)` gets one invariant position for each input universal of `region_signature(f)` after those of `ḡ`. | 11 N-I-7 makes every parameter region a region of the future, but MIR has no positions for them. Invariance avoids a variance computation over the captures (P9), and it is what spec/07 §7.16 item 3 needs: the arguments stay borrowed while the future is live. | §5.5.5, §5.13.1, §5.15.1 |
| N-B-43 | The fact dump (`borrowck_facts`) and the verify mode (Cargo feature `borrowck-verify`) are test-only. No command, option, configuration key, or environment variable exposes them, and the dump format has no compatibility guarantee. | impl/13 has no developer flags (N-H-18) and impl/01 has no tracing flags (N-J-34). The tests need stable goldens within a toolchain version only (impl/15 §15.4.1). | §5.20.1, §5.20.2 |
| N-B-44 | A call of `sai:async.scope` creates a local scope region `'s_k`. It is the extra invariant position of the call's future type, and its seed adds every point of the bodies nested in the argument. The two positions of the bound's parameter are `'s_k`, not bound regions. Rule items 3 and 4 of spec/11 §11.7.3 are the constraints `ScopeOutlives` and `SpawnOutlives`. The region parameters of `Scope` and `JoinHandle` are invariant, and a `JoinHandle` has exactly the receiver's scope region. A conflict or a universal check whose cause path contains one of the two constraints is `CHILD_TASK_BORROWS_LOCAL`. | 11 N-I-11 replaces the bound check by a typing rule. Expressing it with the existing solver, loans and cause search keeps one mechanism for every borrow error (P9). Invariance keeps a `Scope` from being shortened below `'s`, which would weaken rule item 4. The seed is what makes a child's loan live at the storage end of a body local. | §5.5.1, §5.7.2, §5.10.5, §5.15.3 |
| N-B-45 | `SCOPE_REGION_ESCAPE` is decided by reachability from `'s_k` to a region position of the scope call's `F.Output` in the constraint graph, once per scope call, after the checks of §5.8. | A position that `'s_k` reaches is bounded by the scope, which is what "mentions `'s`" means for an inferred type (P8). A structural test of the written types cannot see through inference and nested scopes. Reachability is already computed for §5.10.5 row 6 (P9). | §5.15.3 |

---

## 5.22 Diagnostics

This document defines no diagnostic code of its own. Every code below is defined in the document and section named at the start of its description ("defined in"); most are codes of [spec/07](../spec/07-ownership-and-borrowing.md) §7.21. The first part lists the codes that this checker reports, with the query and section that produce them. The second part lists the ownership codes that other components report, so that the codes of 07 are accounted for. Severities are those of the defining document.

**Reported by this checker:**

| Code | Severity | Description |
|---|---|---|
| `USE_AFTER_MOVE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.4.4; pass A: a use of a move path that is not definitely initialized, once per set of move-outs (§5.4.4) |
| `MOVE_OUT_OF_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.17; pass A: a non-Copy move through the `Deref` of a borrow, a view type, or a raw pointer (§5.4.3) |
| `MOVE_OUT_OF_DROP_TYPE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.3.5; pass A: a non-Copy move out of a field, element, or variant of a value whose type has a `Drop` impl or is a resource (§5.4.3) |
| `MOVE_OUT_OF_BOX` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.3.5; pass A: a non-Copy move out of `*b` for a `Box` (§5.4.3) |
| `MOVE_OUT_OF_INDEX` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.3.5; pass A: a non-Copy move out of an `Index` place (§5.4.3) |
| `MOVE_OUT_OF_CAPTURE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.11.3; pass A: a move through the environment of a closure that is required to be `Fn` or `FnMut` (§5.4.3, §5.13.5) |
| `PARTIAL_ASSIGN_TO_MOVED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.3.6; pass A: an assignment to a place with a prefix that may be uninitialized (§5.4.4) |
| `ASSIGN_TO_IMMUTABLE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.2.4; pass A: a write to a place that is not mutable (§5.4.6) |
| `BORROW_MUT_OF_IMMUTABLE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.2.4; pass A: a mutable borrow, receiver, capture, or closure call of a place that is not mutable (§5.4.6, §5.13.5) |
| `INVARIANT_FIELD_WRITE` | error | defined in [09](../spec/09-contracts-and-refinements.md) §9.4.2; pass A: a write or mutable borrow of a field of a type with an invariant outside its defining module (§5.4.6) |
| `ENSURES_USES_MOVED_PARAMETER` | error | defined in [09](../spec/09-contracts-and-refinements.md) §9.3.4; pass A: an `ensures` clause reads a by-value parameter that may be moved at the return point (§5.4.4, N-B-41) |
| `CONFLICTING_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.6; pass B: a borrow, a reservation, or an activation conflicts with a loan in scope (§5.10.5, §5.11) |
| `USE_WHILE_MUT_BORROWED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.6; pass B: a read conflicts with a mutable loan in scope (§5.10.5) |
| `ASSIGN_WHILE_BORROWED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.6; pass B: a write or a `Replace` drop conflicts with a loan in scope (§5.10.5) |
| `MOVE_WHILE_BORROWED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.6; pass B: a move conflicts with a loan in scope (§5.10.5) |
| `DOES_NOT_LIVE_LONG_ENOUGH` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.6; pass B: a scope drop or storage-dead of any other local conflicts with a loan in scope (§5.10.5 row 8) |
| `TEMPORARY_DROPPED_WHILE_BORROWED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.5.6; pass B: a scope drop or storage-dead of a temporary conflicts with a loan in scope (§5.10.5 row 7) |
| `RETURNS_LOCAL_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.7.4; pass B: a scope drop or storage-dead of a local, including the implicit storage ends of `Return` and `CancelExit`, conflicts with a loan whose region reaches the output (§5.10.5 row 6, N-B-33) |
| `BORROWS_CONTRACT_VIOLATED` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.7.4; pass B: the output region needs an input universal that the elision rules or the `borrows` clause do not name as a source (§5.8.1) |
| `BORROW_ESCAPES_SIGNATURE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.7; pass B: an outlives relation between two universals that the assumptions do not imply, a closure universal that escapes its closure, or a failed type test (§5.8.1 to §5.8.3, §5.13.4) |
| `BORROW_ESCAPES_ARENA` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.15; pass B: a scope drop or storage-dead of the storage of an arena conflicts with a loan in scope (§5.10.5 row 5, §5.14) |
| `FN_POINTER_REGION_MISMATCH` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.7.6; pass B: a function item is converted to a function pointer type whose regions differ from the item's (§5.6.5) |
| `CHILD_TASK_BORROWS_LOCAL` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.7.3; pass B: a loan conflict or a universal check whose cause path contains a `ScopeOutlives` or `SpawnOutlives` constraint of a scope or spawn call (§5.10.5, §5.15.3 items 3, 4 and 7) |
| `SCOPE_REGION_ESCAPE` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.7.3; pass B: the scope region of a scope call reaches a region position of its `F.Output` (§5.15.3 item 6) |
| `BORROWS_INVALID_PROVENANCE` | error | `region_signature`: a `borrows` entry fails V1, V2, or V3 of 07 §7.7.3 (§5.5.2) |
| `BORROWS_WITHOUT_OUTPUT_REGION` | error | `region_signature`: a `borrows` clause on a function whose output has no region (§5.5.2) |
| `REDUNDANT_PROVENANCE` | warning (lint) | `region_signature`: a `borrows` entry that repeats or is subsumed by another (§5.5.2) |
| `IMPL_SIGNATURE_REGION_MISMATCH` | error | `region_signature`: an impl method whose regions differ from the trait method's (§5.5.2) |
| `RESOURCE_CONTAINS_BORROW` | error | `item_regions`: a field type of a resource has a region position (§5.5.3) |
| `AMBIGUOUS_IMPL_REGION` | error | `item_regions`: a region position of an impl's associated type or trait argument when the target has several regions (§5.5.3) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6; an impossible MIR shape (§5.4.3) or a failed check of the verify mode (§5.20.2) |

**Reported by other components** (listed for completeness, never reported by this checker):

| Code | Severity | Description |
|---|---|---|
| `MISSING_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.5.2; reported by [03](03-name-resolution-and-type-checking.md) §3.7.12; an argument without the `borrow` that its parameter type requires |
| `ARENA_REQUIRES_COPY` | error | reported by impl/03 §3.7.4 and §3.7.12; a non-Copy value allocated in an arena (§5.14) |
| `COPY_AND_DROP` | error | reported by impl/03 §3.8.2; a type that is both Copy and `Drop` |
| `EXPLICIT_DROP_CALL` | error | reported by impl/03 §3.9.5; an explicit call of `Drop.drop` |
| `DROP_IMPL_INVALID_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.9.3; reported by impl/03 §3.8.2; a `Drop` impl for a type that is not a local nominal type |
| `DROP_IMPL_NOT_GENERIC_ENOUGH` | error | defined in [spec/05](../spec/05-types.md) §5.9.3; reported by impl/03 §3.8.2; a `Drop` impl that is not exactly as generic as its type |
| `DERIVE_NOT_SUPPORTED` | error | defined in [spec/05](../spec/05-types.md) §5.12.4; reported by impl/03 §3.8.6; among others, `@derive(Copy)` on a resource (spec/07 §7.14) |
| `GUARD_MOVE_OR_MUTATION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.2; reported by impl/03 §3.12.2 (N-E-38); a guard that moves, assigns, or mutably borrows a binding or the scrutinee. The guard loan of §5.10.7 covers the remaining cases |
| `DROP_HAS_EFFECTS` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.3; reported by [08](08-effect-and-capability-analysis.md) §8.5.3; a `Drop.drop` impl with effects |
| `INVALID_ASSIGN_TARGET` | error | defined in [spec/03](../spec/03-grammar.md) §3.16; reported by [02](02-frontend-ast-hir.md); an assignment to an expression that is not a place |
| `TYPE_MISMATCH` | error | defined in [spec/05](../spec/05-types.md) §5.14; reported by impl/03; among others, `borrow e` passed to a by-value parameter (spec/07 §7.5.2) |
