# 09. Optimization and Size

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 1 (generic cleanup, the `dev` pipeline, the item table); 2 (the `release` and `tiny` pipelines, Wasm passes, identical code folding, final dead-code elimination, size attribution and `sai size`); 3 (size budgets)
**Related:** [decisions.md](../design/decisions.md) (D-MIR-01, D-TGT-02, D-WASM-05, D-ERR-01, D-CONTRACT-01, D-TOOL-03) · [01-architecture.md](01-architecture.md) · [04-mir.md](04-mir.md) · [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) · [07-component-lowering.md](07-component-lowering.md) · [10-runtime.md](10-runtime.md) · [11-diagnostics.md](11-diagnostics.md) · [13-cli.md](13-cli.md) · [01-introduction.md](../spec/01-introduction.md) · [08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) · [09-contracts-and-refinements.md](../spec/09-contracts-and-refinements.md) · [10-errors-and-panics.md](../spec/10-errors-and-panics.md) · [12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) · [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) · [01-package-contract.md](../package/01-package-contract.md) · [targets-and-profiles.md](../targets/targets-and-profiles.md)

This chapter specifies how the reference compiler optimizes a program and how it measures and explains the size of the result. It covers the correctness criterion that every optimization must meet, the pass pipelines of the three profiles and their fixed constants, the MIR passes (cleanup, CFG simplification, constant propagation, copy propagation, dead-store elimination, inlining, check elimination, scalar replacement, reference forwarding, panic-message elimination and dead-allocation elimination), identical code folding, the three stages of dead-code elimination and the retention chains of `sai size --why`, the Wasm-level passes that run on structured function bodies, determinism, the attribution of every byte of an artifact to a group of the `sai size` tree, the measurement of size budgets, and the size test suite. MIR and its validator belong to [04-mir.md](04-mir.md). The lowering to Wasm, the layout of types and the size attribution table belong to [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md). The runtime functions belong to [10-runtime.md](10-runtime.md), the boundary glue to [07-component-lowering.md](07-component-lowering.md). The command line and the rendering of `sai size` belong to [13-cli.md](13-cli.md) §13.6.14, the syntax of budgets to [package/01](../package/01-package-contract.md) §1.14.3, the meaning of `@inline`, `@inline(never)` and `@cold` to [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.4, and the definition of the profiles to D-TGT-02 and [targets-and-profiles.md](../targets/targets-and-profiles.md) §5. Optimization never changes what a program means (C-03): every rule of this chapter is a refinement of that requirement.

---

## 9.1 Overview

### 9.1.1 Position in the pipeline

Optimization is phase 11 of [01-architecture.md](01-architecture.md) §1.2.1, and measurement is part of phase 14. Both run only when the code generation gate has passed ([01](01-architecture.md) §1.2.3), so every program that this chapter sees is accepted.

The work is split between two crates, because `sai_opt` depends only on `sai_mir` and `sai_codegen_wasm` depends on `sai_opt` ([01](01-architecture.md) §1.3.1):

| Work | Crate | Input |
|---|---|---|
| MIR passes (§9.4), pass pipelines (§9.3), item-level dead-code elimination (§9.6.2) | `sai_opt` | MIR in phases `Analyzed` and `Mono` |
| Wasm passes (§9.7), identical code folding (§9.5), the late stack-overflow root (§9.6.3), final dead-code elimination (§9.6.4) | `sai_codegen_wasm` | the structured Wasm function bodies of [06](06-wasm-lowering-and-layout.md) §6.1.4 step 5 |
| Size attribution (§9.10), retention chains (§9.6.5), budget checks (§9.10.6) | `sai_opt` | the attribution table and the final call graph, whose types `sai_opt` defines and `sai_codegen_wasm` fills |

`sai_opt` never sees a layout. Every MIR pass is therefore independent of type sizes: it reasons about MIR values, places and types, not about bytes. The Wasm passes see bytes and Wasm types but no MIR.

### 9.1.2 The driver

The compiler driver runs the following steps for one `(root, target, profile)`. Steps 1 to 4 belong to phase 11, steps 5 to 9 to phases 12 and 13, step 10 to phase 14.

```
build_code(root, target, profile) -> Artifact:
    mode    := panic_mode(root, target, profile)              // [06] §6.8.12.1
    exports := core_exports(root, target)                     // [06] §6.10.5, [07] §7.10.2; fixed (N-F-20)
    imports := core_imports(root, target)                     // [06] §6.10.1; fixed (N-F-20)

    // 1. Generic MIR. Instance collection demands mir_optimized(o) of [04] §4.11.4 for every
    //    owner o whose body it instantiates; it runs the generic pass set of §9.4.2.
    // 2. Instance collection ([04]) substitutes Optimized bodies into Mono bodies and adds
    //    drop glue ([04]), boundary glue ([07] N-D-64) and runtime bodies ([10] §10.1.2).
    mono := mono_instances(root, target)

    // 3. Mono optimization: opt_mono(f) for every f in mono, bottom-up over the
    //    strongly connected components of mono_call_graph (§9.3.3).
    // 4. Item-level dead-code elimination.
    items := item_table(root, target, profile)                // §9.6.2

    // 5. impl/06 steps 1 to 5 (layout, signatures, frames, body lowering, structuring),
    //    per instance in parallel.
    bodies := lower(items)
    bodies := add_stack_overflow_path(bodies, mode)          // §9.6.3

    // 6. Wasm-level optimization.
    if profile != dev: for b in bodies: wasm_passes(b)       // §9.7
    if profile == tiny: bodies := fold_identical(bodies)      // §9.5
    bodies := final_dce(bodies, exports)                      // §9.6.4

    // 7. impl/06 steps 6 and 7: linking, static data, custom sections, attribution table.
    module, table := link(bodies)
    // 8. Component builds: impl/07 wraps the core module.
    artifact := wrap_if_component(module)
    // 9. The artifact bytes are final here; they are what step 10 measures and what
    //    [13] §13.5.1 writes if no error was reported.
    // 10. Measurement.
    report := size_report(artifact, table)                   // §9.10
    check_budgets(report, profile)                            // §9.10.6
    return artifact
```

The order of steps is fixed. In particular, the import set and the function exports are computed before any optimization and are never changed by it ([06](06-wasm-lowering-and-layout.md) §6.10.1, N-F-20), so whether a host can instantiate an artifact never depends on the profile. The only export that optimization can affect is `memory`, which exists exactly when the module has a memory ([06](06-wasm-lowering-and-layout.md) §6.3.4). A module whose boundary passes pointers always has one, so the export is missing only where no host can use it.

### 9.1.3 Queries

| Query | Key | Value | Diagnostics |
|---|---|---|---|
| `mono_call_graph` | `(root, target)` | the call graph of the Mono bodies before Mono optimization, with its strongly connected components and the call-site counts of §9.4.5 | none |
| `opt_mono` | `(instance, target, profile)` | the optimized Mono body | none |
| `item_table` | `(root, target, profile)` | the reachable Mono instances in symbol order (§9.6.2) | none |
| `size_report` | `(root, target, profile)` | the attribution tree of §9.10 | none |
| `retention_chain` | `(root, target, profile, name)` | the chain of §9.6.5, or `None` | none |
| `size_budget_check` | `(root, target, profile)` | the result of §9.10.6 | `SIZE_BUDGET_EXCEEDED` |

`size_budget_check` is the only check query of this chapter ([01](01-architecture.md) §1.9 rule 1). The driver demands it for `sai build`, `sai component build`, `sai run` and `sai size` when the selected profile of the root has a `budget` block. `retention_chain` returning `None` is reported by `sai size` as `NOT_PRESENT_IN_BUILD` ([13](13-cli.md) §13.6.14).

The generic pass set is not a query of this chapter. `sai_opt` implements the hook `MirHooks::optimize` with `OptPassSet::Generic` ([04](04-mir.md) §4.1.4), and the query `mir_optimized` of [04](04-mir.md) §4.11.4 calls it on a copy of each body that it owns. This chapter calls that pass set **`opt_generic`**.

`opt_mono(f)` starts from `mir_mono(f)` ([04](04-mir.md) §4.13.5). It reads `opt_mono(g)` for the callees `g` of `f` in lower components (§9.3.3) and nothing else of other instances except `mono_call_graph`. The Wasm-level steps are not queries: they run inside the code generation step of [06](06-wasm-lowering-and-layout.md) and are recomputed whenever the module is.

---

## 9.2 Correctness

### 9.2.1 Observable behavior

An optimization MUST preserve the **observable behavior** of every execution that has defined behavior (N-F-39). An execution with undefined behavior ([12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md)) constrains nothing.

The observable behavior of an execution is the sequence of the following events, in order:

1. **Import calls.** Each call of a core import: which import, its argument values, and the contents of the escaped memory (below) at the moment of the call.
2. **Export returns.** Each return from a core export: its result values and the contents of the escaped memory at the moment of the return.
3. **Panics.** Each panic, with its kind, message and location ([10-errors-and-panics.md](../spec/10-errors-and-panics.md) §10.4.1), whether or not the panic mode reports it. A resource panic is observable only when it occurs; that it occurs is not (item 7).
4. **Traps that are not panics**, such as the trust-boundary checks of D-CONTRACT-01 ([06](06-wasm-lowering-and-layout.md) §6.6.7) and `Trap` terminators.
5. **Non-termination.** An execution that never reaches another event is different from one that does. No pass may assume that a loop terminates.

The values that the host returns from an import, and the bytes the host writes to memory during an import call or through `cabi_realloc` buffers, are inputs. An optimized program, given the same inputs, MUST produce the same events.

**Escaped memory** is the part of linear memory whose contents are defined at a boundary event:

- the bytes of every object whose address the program has made available to the host: as an argument of an import call, as a result of an export, by storing it into escaped memory, or by converting it to an integer that is so passed or stored;
- the bytes that the host has written or allocated for the program (lowered arguments, `cabi_realloc` buffers, results written by an import);
- the static data of the module, which never changes ([06](06-wasm-lowering-and-layout.md) §6.8.14.2).

Every other byte of linear memory (frames, heap blocks whose address never escaped, freed blocks, padding, and uninitialized memory) has unspecified contents at a boundary event, like padding in [spec/01](../spec/01-introduction.md) §1.5.3. A host may read it, because [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.2 allows the host to read any memory during an import call, but what it reads is not part of the observable behavior.

6. The following are **not** observable, and MAY differ between profiles, between builds with different options, and between toolchain versions: addresses ([spec/01](../spec/01-introduction.md) §1.5.3), the amount of stack and heap used, speed, code size, the generated code and its inlining ([spec/01](../spec/01-introduction.md) §1.5.2 item 8), NaN bit patterns, and the unspecified memory contents above.
7. **Resource panics.** Whether a stack overflow or an out-of-memory panic occurs depends on the amount of memory used, which item 6 leaves free. An optimization MAY therefore remove or introduce the possibility of a resource panic. This is the exception of [spec/10](../spec/10-errors-and-panics.md) §10.4.3 for `StackOverflow` and `OutOfMemory`.

The alias model of §9.4.1 is the conservative implementation of this definition: MIR passes treat every byte that might have escaped as observable at every call.

### 9.2.2 Profiles never change acceptance

1. **The generic pipeline is the same in every profile** (N-F-40). `opt_generic` (§9.1.3) runs only the cleanup of §9.4.2, in every profile. Instance collection therefore sees the same bodies in every profile, so the check collection `mono_items` ([04](04-mir.md) §4.13.1), and every diagnostic that depends on the instance set (such as `TYPE_TOO_LARGE` of [06](06-wasm-lowering-and-layout.md) and the instance limits of [04](04-mir.md) §4.13.3), are the same for `dev`, `release` and `tiny`. The code generation collection `mono_instances` depends on the profile only through its runtime roots, the allocator ([10](10-runtime.md) §10.2.1) and the `Report`-mode reporters, which are fixed bodies that report nothing. All profile-dependent optimization runs on Mono MIR or on Wasm bodies.
2. **No pass reports a diagnostic** (N-F-41). A pass that finds a condition it cannot handle is a defect, reported as `INTERNAL_COMPILER_ERROR` ([11-diagnostics.md](11-diagnostics.md) §11.4.6, [01](01-architecture.md) §1.10.2). In particular, constant propagation that finds an `Assert` that always fails does not report it (§9.4.3): panics found at compile time are not errors ([spec/10](../spec/10-errors-and-panics.md) §10.4.6).
3. **The closed list.** The only diagnostics whose presence can depend on optimization are:

   | Code | Why it depends on optimization |
   |---|---|
   | `LARGE_STACK_FRAME` | frame sizes depend on inlining and scalar replacement ([06](06-wasm-lowering-and-layout.md) §6.7.2) |
   | `SIZE_BUDGET_EXCEEDED` | the artifact size depends on every pass (§9.10.6) |
   | `INTERNAL_COMPILER_ERROR` | a defect may show only under one pipeline |

   `LARGE_STACK_FRAME` is an info, so it never affects acceptance: `--deny-warnings` ([13](13-cli.md) §13.4.4) does not change infos ([11](11-diagnostics.md) §11.3.3 rule 2). `SIZE_BUDGET_EXCEEDED` is a property of the artifact by definition: a budget limits what optimization produces. The capability denials of `tiny` (D-TGT-02) depend on the profile, not on optimization, and are checked before it ([package/01](../package/01-package-contract.md) §1.14.2).
4. **Checks are never removed without a proof.** A check of [spec/06](../spec/06-expressions-and-statements.md) or of D-CONTRACT-01 is removed only when §9.4.6 proves that it cannot fail. Removing a check never changes diagnostics or acceptance ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.2), because no diagnostic depends on it.

### 9.2.3 Pass invariants

Every MIR pass MUST keep the following invariants (N-F-44). In debug builds of the compiler and in the test harness, `sai_opt` runs the validator of [04](04-mir.md) §4.11.1 after every pass, which checks item 1, and tests the CFG for reducibility, which checks item 2. Items 3 to 6 relate a body to its input and are properties of the passes. A violation is `INTERNAL_COMPILER_ERROR`.

1. **Validity.** The body is valid MIR of its phase by the rules of the validator ([04](04-mir.md) §4.11.1): in particular its types agree, its storage statements satisfy rule 4 there, and the destination and source places of an `Assign` are identical or disjoint. A local that a pass creates either gets storage statements around all of its uses or gets none, and is then live in the whole body.
2. **Reducibility.** A reducible CFG stays reducible. No pass duplicates a loop header or creates a second entry into a loop. [06](06-wasm-lowering-and-layout.md) §6.9.3 accepts irreducible graphs, but only as a defense.
3. **Assert order.** No pass adds an `Assert`, or moves one across another `Assert`, a `Call`, a `Drop`, an `Await`, a `Trap` or a write to memory that may have escaped. A pass removes an `Assert` only by the rules of §9.4.3 (constant condition) and §9.4.6 (proof), and keeps the evaluation of the condition's operands when they have effects. The first failing check of an execution is therefore the same before and after.
4. **Call order.** `Call`, `Drop` with drop glue, `Await` and `Trap` terminators are never reordered with respect to each other, and are never removed except by inlining (which replaces a call by the callee's body), by dead-allocation elimination (§9.4.10), and by the removal of unreachable blocks.
5. **Spans.** Every statement and terminator keeps its span and source scope. A statement that a pass creates takes the span and scope of the statement it replaces. Panic locations are therefore unchanged (§9.4.5).
6. **Termination.** No pass removes a reachable cycle of the CFG, except when constant propagation proves the cycle unreachable.

---

## 9.3 Profiles and pass pipelines

### 9.3.1 Pass tables

The pipeline of each profile is fixed (N-F-42). Optimization options do not exist: the profile selects the pipeline, and nothing else changes it. The items of D-TGT-02 map to the pipelines as follows: "minimal" is the `dev` column, "maximal (speed)" the `release` column, "maximal (size)" the `tiny` column; "DCE partial" means that `dev` removes only what is unreachable in unoptimized bodies.

| Step | `dev` | `release` | `tiny` |
|---|---|---|---|
| Generic MIR (`opt_generic`) | cleanup, `simplify_cfg` | same | same |
| Mono MIR (`opt_mono`) | `simplify_cfg` | pipeline M of §9.3.2 | pipeline M of §9.3.2 |
| Item-level DCE (§9.6.2) | yes | yes | yes |
| Wasm passes (§9.7) | no | yes | yes |
| Identical code folding (§9.5) | no | no | yes |
| Final DCE (§9.6.4) | yes | yes | yes |
| Inlining constants (§9.4.5) | — | `release` row | `tiny` row |

### 9.3.2 Pipeline M

For `release` and `tiny`, `opt_mono(f)` runs these passes on the Mono body of `f`, in this order:

1. `inline` (§9.4.5);
2. `trap_only_messages` (§9.4.9), only if the panic mode is `TrapOnly`;
3. `OPT_ROUNDS` times, the round: `simplify_cfg` (§9.4.2), `forward_refs` (§9.4.8), `sroa` (§9.4.7), `const_prop` (§9.4.3), `elim_checks` (§9.4.6), `copy_prop` (§9.4.4), `dse` (§9.4.4);
4. `dead_alloc` (§9.4.10);
5. `copy_prop`, `dse`, `simplify_cfg`, and the renumbering of §9.4.2.

The panic mode is a property of the build ([06](06-wasm-lowering-and-layout.md) §6.8.12.1). It is `TrapOnly` in `tiny`, and also in `release` when no reporting channel exists, so step 2 runs in both cases.

### 9.3.3 Bottom-up order

`mono_call_graph` has one node per Mono instance and an edge `f → g` for every `Call` terminator of the unoptimized Mono body of `f` whose callee is the function item instance `g` ([06](06-wasm-lowering-and-layout.md) §6.8.9). Its strongly connected components are computed with Tarjan's algorithm, visiting nodes in symbol order and successors in order of first occurrence in the body. A component is **lower** than another if it is reachable from it.

`opt_mono(f)` first demands `opt_mono(g)` for every callee `g` in a lower component, then runs pipeline M. The inliner of `f` inlines only optimized bodies of lower components (§9.4.5). Because every query value depends only on its key and on the values it reads, instances of one component, and independent components, can be optimized in parallel without changing any result (§9.9).

### 9.3.4 Constants

Every limit of the optimizer is a fixed constant (N-F-42). None depends on the host, the thread count, time or memory.

| Constant | `release` | `tiny` | Used in |
|---|---|---|---|
| `OPT_ROUNDS` | 2 | 2 | §9.3.2 |
| `INLINE_SMALL` | 40 | 12 | §9.4.5 |
| `INLINE_HINTED` | 400 | 40 | §9.4.5 |
| `INLINE_SINGLE` | 400 | 1500 | §9.4.5 |
| `INLINE_CALLER_LIMIT` | 3000 | 1500 | §9.4.5 |
| `WIDEN_AFTER` | 2 | 2 | §9.4.6 |
| `SROA_MAX_LEAVES` | 16 | 16 | §9.4.7 |
| `WASM_ROUNDS` | 8 | 8 | §9.7.1 |

---

## 9.4 MIR passes

### 9.4.1 The alias model

Every MIR pass uses the same model of which places can change behind its back (N-F-43). The model is computed at the start of each pass, because earlier passes change it.

1. **Exposed locals.** A local `l` is **exposed** in a body if the body contains `Borrow(_, _, P)` or `RawPtr(_, P)` where the place `P` has base `l` and no `Deref` projection. A by-reference capture of a closure is such a `Borrow` in MIR. Arguments are locals like any other.
2. **Tracked places.** A place is **tracked** if its base is a non-exposed local and its projections are `Field` only. A tracked place of scalar type (an integer type, `Bool`, `Char`, `Byte`, a float type) is a **tracked leaf**. A place of a non-exposed local that is not tracked and not in `M` (one with an `Index`, `ConstIndex`, `Subslice` or `Downcast` projection inside the local's own storage) is treated as the whole local: reading it reads every tracked place of the local, and writing it makes every tracked place of the local unknown.
3. **Abstract memory.** All places of exposed locals and all places that contain a `Deref` projection, among them every element of a view, which is reached through the `Deref` of the view ([04](04-mir.md) §4.2.3), form one **abstract memory** `M`. Passes do not distinguish parts of `M`.
4. **Effects on `M`.** A `Call` (of any callee: SAI functions, imports, runtime functions, glue), a `Drop` of a type with drop glue, and an `Await` may read and write all of `M`. An `Assign` whose destination is in `M` may write any part of `M`. A read of a place in `M` reads `M`. No call and no write through a pointer can change a non-exposed local.
5. **Facts never used.** No pass uses the region or loan facts of borrow checking ([05](05-borrow-checker.md)), the uniqueness of `borrow mut`, the immutability of a place behind a shared borrow, rule 3 of [spec/12](../spec/12-unsafe-and-ffi.md) §12.6.4, refinement predicates, `requires` or `ensures` clauses, or Standard Fact Checker results. The model is correct for interior mutability, including the `Arena` methods that change their receiver through `self: borrow Self` ([10](10-runtime.md) §10.4), and for unsafe code.
6. **`Unreachable`.** No pass assumes that a block ending in `Unreachable` is not executed. Such a block is treated like a block ending in `Trap`.

Passes treat each non-exposed local precisely, per tracked place. This is where optimization gains: SAI code keeps most values in non-exposed locals, and inlining (§9.4.5) and reference forwarding (§9.4.8) turn many exposed locals into non-exposed ones.

### 9.4.2 Cleanup, `simplify_cfg` and renumbering

**Cleanup** runs in `opt_generic` only, in every profile. It removes every `FakeRead` and `Nop` statement and replaces the region of every `Borrow` by the erased region, which, with the async transform that `mir_optimized` runs first, gives the form of phase `Optimized` ([04](04-mir.md) §4.11.4). Then it runs `simplify_cfg`. Mono bodies are built from `Optimized` bodies, so they never contain `FakeRead` or regions.

**`simplify_cfg`** applies these rules until none applies:

1. Remove every block that is not reachable from `bb0` through terminator edges.
2. A block `x ≠ bb0` with no statements and the terminator `Goto(t)`, `t ≠ x`, is removed: every edge to `x` is redirected to `t`.
3. A block `x` whose terminator is `Goto(s)`, where `s ≠ bb0`, `s ≠ x`, and `x` is the only predecessor of `s`, is merged with `s`: the statements of `s` are appended to those of `x`, and `x` takes the terminator of `s`.
4. A `SwitchInt` entry whose target equals `otherwise` is removed. A `SwitchInt` with no entries becomes `Goto(otherwise)`.
5. An `Assert` whose `target` block consists only of `Unreachable` is kept (it always panics when its condition fails, and the failure is the only way out).

`simplify_cfg` never removes a cycle: a block `x: Goto(x)` is kept by rule 2, and rule 3 does not apply to it.

**Renumbering** runs at the end of pipeline M. The remaining blocks are renumbered in increasing order of their old index, so `bb0` stays `bb0`. `_0` and the arguments keep their indices; the remaining locals are renumbered in increasing order of their old index. Source scopes are not renumbered.

### 9.4.3 Constant propagation

`const_prop` is sparse conditional constant propagation over the tracked leaves of the body (N-F-45).

**Lattice.** Each tracked leaf has a value `⊤` (no execution reaches it yet), `Const(c)`, or `⊥` (unknown). Each non-exposed local of an enum type also has a **variant value** `⊤`, `Variant(v)` or `⊥`, set by `Aggregate` of variant `v`, by `SetDiscriminant(v)`, and by copies. Each CFG edge is executable or not. The analysis starts with only the entry block reachable and all values `⊤`, processes blocks from a worklist in increasing reverse-postorder position, and stops at the fixpoint. The lattice has height 3, so every value changes at most twice.

**Transfer.** For `Assign(dest, rvalue)` with a tracked destination:

| Rvalue | Result when every operand is `Const` |
|---|---|
| `Use(op)` | the operand's value |
| `BinaryOp(op, a, b)`, integer | the wrapped result of `WrapAdd`, `WrapSub`, `WrapMul`, `BitAnd`, `BitOr`, `BitXor`, and the comparisons. `Div` and `Rem` are folded only when `b ≠ 0` and not (signed, `a = MIN`, `b = -1`). `Shl` and `Shr` are folded only when the amount is less than the bit width. Otherwise `⊥`. |
| `BinaryOp(op, a, b)`, float | `Add`, `Sub`, `Mul`, `Div` and the comparisons, computed as IEEE 754 specifies with round-to-nearest-even, only when neither operand is NaN and the result is not NaN. Otherwise `⊥`. |
| `CheckedBinaryOp(op, a, b)` | the field `.0` gets the wrapped result and `.1` the overflow flag |
| `UnaryOp(Not, a)` | the bitwise or logical complement |
| `UnaryOp(Neg, a)` | the wrapped negation for integers; the sign flip for a float that is not NaN |
| `Convert(kind, a)` | `IntWiden`, `IntWrap`, `IntSaturate` exactly; `IntToFloat`, `FloatPromote`, `FloatDemote` with round-to-nearest-even unless the operand or result is NaN; `FloatToInt` as [06](06-wasm-lowering-and-layout.md) §6.8.8 computes it (truncation toward zero, saturation at the bounds of the target type, 0 for NaN); pointer conversions never |
| `Discriminant(p)` | the discriminant of `v` when `p` has variant value `Variant(v)` |
| `Len(p)` | `N` when `p` has type `Array<E, N>` (also when no operand is constant); `⊥` for views |
| `Aggregate(kind, ops)` | each tracked leaf of the destination gets the value of its operand |
| any other | `⊥` |

A `Call` sets its destination's leaves to `⊥`. An `Assign` to a whole tracked local sets every leaf of the local. Folding follows rule 6 of [01](01-architecture.md) §1.8.3: the compiler never uses an inexact host operation, and a NaN is never produced by folding, so the NaN bit patterns that a program sees are those that the engine computes.

**Terminators.** `SwitchInt` with a `Const` discriminant marks only its matching edge executable. `Assert{cond, expected, ..}` with `cond = Const(c)` marks its target edge executable only if `c = expected`.

**Rewriting.** After the fixpoint:

1. Each operand `Copy(p)` or `Move(p)` of a tracked leaf `p` whose value at that point is `Const(c)` becomes `Const(c)`. An `Assign` whose rvalue evaluates to `Const(c)` gets the rvalue `Use(Const(c))`.
2. A `SwitchInt` whose discriminant is `Const` becomes `Goto` of the matching target.
3. An `Assert` whose condition is `Const(expected)` becomes `Goto(target)`.
4. An `Assert` whose condition is `Const(c)` with `c ≠ expected` always panics. It is kept with its condition, and its target is replaced by a new block whose only content is `Unreachable`. This is not a diagnostic and not a compile-time error ([spec/10](../spec/10-errors-and-panics.md) §10.4.6, N-F-41). The panic happens at run time, with its kind, message and location, if and when the execution reaches it.
5. Blocks without an executable incoming edge are left to `simplify_cfg`.

### 9.4.4 Copy propagation, return-place propagation and dead-store elimination

**`copy_prop`** applies three rules.

1. **Copy forwarding.** For `d = Use(Copy(s))` or `d = Use(Move(s))`, where `d` and `s` are whole non-exposed locals of the same type, `d` is neither `_0` nor an argument, `s` is not `_0`, and this statement is the only definition of `d`: every use of `d` is replaced by `Copy(s)` if the definition dominates the use and, on every path from the definition to the use, `s` is not assigned (wholly or in part), has no `StorageDead` and no `SetDiscriminant`. If all uses were replaced, the definition is dead and `dse` removes it.
2. **Definition forwarding.** For `s = rv` followed later in the same block by `d = Use(Copy(s))` or `d = Use(Move(s))`, where `s` is a non-exposed local that is neither `_0` nor an argument, this definition is the only definition of `s` and this copy its only use, and `d` is a whole non-exposed local of the same type: the definition becomes `d = rv` and the copy is removed, if no statement between the two reads or writes `d` or contains `StorageLive(d)` or `StorageDead(d)`.
3. **Return-place propagation.** If `_0` has exactly one definition, `_0 = Use(Copy(s))` or `_0 = Use(Move(s))`, followed in its block only by `StorageDead` statements and a `Return`, and `s` is a non-exposed local that is not an argument, whose type is the same scalar type as `_0` (an integer type, `Bool`, `Char`, `Byte` or a float type): every occurrence of `s` is renamed to `_0`, the copy and the storage markers of `s` are removed. The restriction to scalar `_0` keeps `_0` out of memory: an `Indirect` return value lives at the caller's sret address ([06](06-wasm-lowering-and-layout.md) §6.6.5), and writing it early could be seen through an argument that aliases the destination.

**`dse`** (dead-store elimination and dead locals):

1. Liveness of tracked places is computed backwards. Uses are operands, the places read by rvalues, the discriminants of `SwitchInt`, the conditions of `Assert`, the arguments of `Call`, and `_0` at `Return`. A `Call`, `Drop`, `Await` or `Return` reads every exposed local, through `M`.
2. An `Assign` whose destination is a tracked place that is not live after it is removed. MIR rvalues have no effects (checks are separate `Assert`s, D-MIR-01), so removing one is always allowed. An `Assign` whose destination is in `M` is never removed.
3. A `SetDiscriminant` of a non-exposed local whose discriminant and fields are not live is removed.
4. A local that is not `_0`, not an argument, and has no remaining occurrence except its `StorageLive` and `StorageDead` statements is removed together with them.

### 9.4.5 Inlining

**Cost.** The cost of a body is the sum over its statements and terminators (N-F-46):

| Element | Cost |
|---|---|
| `Assign` | 1, or `k` for an `Aggregate` with `k > 1` operands |
| `SetDiscriminant` | 1 |
| `StorageLive`, `StorageDead`, `Nop`, `FakeRead` | 0 |
| `Goto`, `Return`, `Unreachable` | 0 |
| `Trap`, `Drop`, `Await` | 1 |
| `SwitchInt` | 1 + the number of entries |
| `Call` | 1 + the number of arguments |
| `Assert` | 2 |

**Eligible call sites.** A `Call` in the body of `f` whose callee is a `Const` of the function item instance `g` is eligible if all of the following hold:

1. `g` has a Mono body: it is not an `extern "wasm"` import, not a lang item or intrinsic that [04](04-mir.md) or [06](06-wasm-lowering-and-layout.md) §6.8.15 lowers specially, and not the panic lang item ([06](06-wasm-lowering-and-layout.md) §6.8.12.2);
2. `g` does not carry `@inline(never)` or `@cold` ([15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.4);
3. `g` is in a lower component of `mono_call_graph` than `f` (§9.3.3), so recursive and mutually recursive calls are never inlined;
4. `g` is not `__sai_panic`, `__sai_stack_overflow` or `__sai_oom`, which are cold;
5. `g` is not `__sai_alloc`, `__sai_realloc` or `__sai_free`, unless `f` is itself a runtime function ([10](10-runtime.md) §10.1.2). Calls of the allocator entry points stay visible to dead-allocation elimination (§9.4.10). Inside the runtime they are ordinary candidates; this is how `cabi_realloc` absorbs `__sai_realloc` ([10](10-runtime.md) §10.3.2);
6. the body of `g` contains no `Await`.

Boundary glue ([07](07-component-lowering.md) N-D-64) and the other runtime functions are ordinary candidates. Inlining glue never removes a core import or export (N-F-20).

**Decision.** With `p` the profile and the constants of §9.3.4:

```
should_inline(f_cost, g):
    c := cost(opt_mono(g))
    if f_cost + c > INLINE_CALLER_LIMIT[p]:                return false
    if c <= INLINE_SMALL[p]:                               return true
    if g carries @inline and c <= INLINE_HINTED[p]:        return true
    if single_use(g) and c <= INLINE_SINGLE[p]:            return true
    return false
```

`single_use(g)` holds if `mono_call_graph` records exactly one `Call` of `g` in all unoptimized Mono bodies, `g` is not referenced in any other way (no function item value of `g` is used as an operand of anything but that call, no `FnItemToFnPtr` of `g`; a constant never names a function, [04](04-mir.md) §4.13.6), and `g` is not a core export.

**Order.** The call sites of `f` are visited in increasing block index of the body before inlining. `f_cost` starts at the cost of that body and grows by the cost of each inlined body. Blocks created by inlining are appended after the existing blocks, and their calls are not visited: each was already decided when `g` was optimized.

**Splicing.** Inlining the call `Call{func: g, args, dest, target}` at the end of block `b` of `f`, with `G = opt_mono(g)`:

```
inline_call(f, b, g, args, dest, target):
    G := opt_mono(g)
    for each source scope s of G, in index order:            // new scopes, N-F-47
        s' := new scope of f with
              span    = s.span
              parent  = (s is the outermost scope of G) ? scope of the Call : map(s.parent)
              inlined = (s is the outermost scope of G)
                        ? Some(InlinedRecord { callee: g, call_span: span of the Call })
                        : map(s.inlined)
    for each local of G: a new local of f with the same type
        // G's _0 becomes r, G's arguments become a_1 .. a_n
    for each block of G, in index order: a new block of f, appended
    copy every statement and terminator of G with its span, mapped locals, blocks and scopes
    replace the Call terminator of b by:
        StorageLive(a_i); a_i = Use(args_i)       for i in 1..n   // operands as written
        StorageLive(r)
        Goto(map(bb0 of G))
    replace each Return of the copy by:
        dest = Use(Move(r)); StorageDead(r); StorageDead(a_i) for i in 1..n; Goto(target)
```

A callee with an uninhabited return type has no `Return` block, and its call has no `target`; the copied body ends in its own panics, traps or loops. The `effects` of the call (D-MIR-01) are dropped: they produce no code ([06](06-wasm-lowering-and-layout.md) §6.8.10.2).

**Scopes and locations** (N-F-47).

- Each inlined statement keeps its span. Panic locations are spans of `Assert`s and constant arguments of panic calls, so they do not change.
- A library function whose panics report the caller's location receives it through the hidden location parameter `_l` of [04](04-mir.md) §4.13.7. The inliner binds that argument like any other (`a_i = args_i`), so an inlined `unwrap` still reports the location of its caller. The inliner never derives a location from a call site.
- The innermost scope with an `InlinedRecord` determines the attribution of a statement: its code is `Inlined(callee, into: f)` in the attribution table ([06](06-wasm-lowering-and-layout.md) §6.11.5). With nested inlining (`h` into `g`, then `g` into `f`) the innermost record names `h`.

**`@inline(never)`.** A function with `@inline(never)` is never inlined, never merged by identical code folding (§9.5), and no pass replaces a call of it by its result: the optimizer has no interprocedural constant propagation. Every reachable instance therefore exists as a function of its own, and every call of it is a `call` of that function ([15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.4). Dead-code elimination still removes it when no call remains.

### 9.4.6 Check elimination

`elim_checks` removes an `Assert` only with one of the proofs below (N-F-48). The proofs use only facts derived from the MIR of the body itself: constants, the types' value ranges, and the comparisons and checks that the body executes. They do not use the facts of [spec/09](../spec/09-contracts-and-refinements.md) §9.6.3, `requires` and `ensures` clauses, refinements, or results of the Standard Fact Checker, in v0.1. Every proof is sound in exact integer arithmetic, as [spec/09](../spec/09-contracts-and-refinements.md) §9.7.2 requires.

**Proof 1: constants.** Rule 3 of §9.4.3.

**Proof 2: intervals.** Each tracked leaf of an integer type has an interval `[lo, hi]` of mathematical integers inside the range of its type, or `⊤` when unreached. The transfer functions are:

| Rvalue | Interval |
|---|---|
| `Use(Const(c))` | `[c, c]` |
| `Use(Copy(p))`, `Use(Move(p))` | the interval of `p` |
| `BinaryOp(WrapAdd / WrapSub / WrapMul, a, b)` | the exact interval of the operation if it lies inside the type's range, otherwise the type's range |
| `CheckedBinaryOp(op, a, b)` | `.0` as for `BinaryOp`; `.1` is `[0, 0]` if the exact interval lies inside the type's range, `[1, 1]` if it lies entirely outside, `[0, 1]` otherwise |
| `BinaryOp(Div, a, b)`, unsigned, `b.lo > 0` | `[a.lo / b.hi, a.hi / b.lo]` |
| `BinaryOp(Rem, a, b)`, unsigned, `b.lo > 0` | `[0, min(a.hi, b.hi - 1)]` |
| `BinaryOp(BitAnd, a, b)`, `a.lo >= 0` or `b.lo >= 0` | `[0, min(hi of the non-negative operands)]` |
| `BinaryOp(Shr, a, Const(k))`, `a.lo >= 0`, `k` less than the width | `[a.lo >> k, a.hi >> k]` |
| `Convert(IntWiden, a)` | the interval of `a` |
| `Convert(IntWrap, a)` | the interval of `a` if it lies inside the target range, otherwise the target range |
| a comparison `Cmp(a, b)` | `[1, 1]` if it holds for all values of the intervals, `[0, 0]` if for none, `[0, 1]` otherwise |
| `Len(p)` | `[N, N]` for `Array<E, N>`; `[0, max(USize)]` for views |
| any other rvalue, a `Call` destination | the type's range |

The analysis is a forward dataflow over executable edges (those of §9.4.3), processed in reverse postorder. The state at a join is the hull of the incoming states. At a loop header, from the `WIDEN_AFTER + 1`-th visit on, every bound that changed since the previous visit is set to the bound of the type, which makes the analysis terminate after a number of steps bounded by the body size.

**Branch refinement.** For `SwitchInt(c)` where `c` is a tracked `Bool` leaf defined in the same block by `c = Cmp(x, y)`, with `x` and `y` tracked leaves or constants not assigned between that definition and the terminator, the state sent along the edge where `c` is 1 is refined by `Cmp(x, y)`, and along the edge where `c` is 0 by its negation. The success edge of `Assert{cond: c, expected: e}` is refined by `Cmp(x, y) = e` in the same way. Refinement by `x < y` sets `x.hi := min(x.hi, y.hi - 1)` and `y.lo := max(y.lo, x.lo + 1)`; `x <= y` sets `x.hi := min(x.hi, y.hi)` and `y.lo := max(y.lo, x.lo)`; `x == y` intersects both; `x != y` with `y` a constant at a bound of `x` moves that bound by one. `>` and `>=` are the mirrored forms. Comparisons use the signedness of the operand type. An empty interval makes the edge not executable.

**Proof 3: dominating facts.** Value numbering assigns a number `vn` to each tracked leaf at each point and to each pure rvalue over value-numbered operands (`BinaryOp`, `UnaryOp`, `Convert`, comparisons, `Len` of a non-exposed local of a view type, `Discriminant` of a non-exposed enum local):

- An assignment gives its destination the number of its rvalue; equal rvalues over equal numbers get equal numbers. `Use(op)` has the number of `op`, and a constant has a number determined by its value. A `Call` destination gets a fresh number.
- At the entry of a block `b ≠ bb0`, a local has the number it has at the exit of `idom(b)` if no block of `Between(idom(b), b)` assigns it, and a fresh number for `(b, local)` otherwise. `Between(d, b)` is the set of blocks `n ≠ d` that are reachable from `d` without passing through `d` again and from which `b` is reachable by a path of at least one edge that does not pass through `d`. A block in a loop that contains `b` but not `d` is in `Between(d, b)`.
- A **fact** `vn = k` is established on the edge where `SwitchInt` on a leaf with number `vn` takes the value `k` (for a `Bool` discriminant, the `otherwise` edge of a switch with the single entry 0 takes the value 1, and of one with the single entry 1 the value 0), and on the success edge of `Assert{cond, expected}` with `k = expected`. A fact established on an edge `(u, t)` holds in every block dominated by `t` when `t` has exactly one predecessor.

An `Assert{cond, expected}` is removed by proof 2 when the interval of `cond` is `[expected, expected]`, and by proof 3 when the fact `vn(cond) = expected` holds at its block.

**Rewriting.** A removed `Assert` becomes `Goto(target)`. Its condition's computation is left to `dse`. Removing an `Assert` never removes another `Assert`: a check inside the computation of a condition is a separate `Assert` and needs its own proof. A `CheckedBinaryOp` whose flag `.1` has the interval `[0, 0]`, and whose flag is read only by `Assert`s that are removed by this rule, becomes two statements: `t.0 = BinaryOp(w, a, b)` and `t.1 = Use(Const(false))`, where `w` is `WrapAdd`, `WrapSub` or `WrapMul` for `op` = `Add`, `Sub` or `Mul` ([04](04-mir.md) §4.2.4: integer `Add`, `Sub` and `Mul` exist only inside `CheckedBinaryOp`).

### 9.4.7 Scalar replacement of aggregates

`sroa` splits a local `l` into one local per field when all of the following hold:

1. `l` is non-exposed, neither `_0` nor an argument, and has a struct type, a tuple type or a closure environment type, with at most `SROA_MAX_LEAVES` fields; or an `Array<E, N>` type with `N <= SROA_MAX_LEAVES`;
2. the type of `l` has no `Drop` impl of its own, and no `Drop` terminator names the whole of `l`;
3. every occurrence of `l` is `StorageLive(l)`, `StorageDead(l)`, a whole assignment `l = Aggregate(kind, ops)`, or a place whose first projection is `Field` (for an array: `ConstIndex` with `from_end = false`).

The rewrite creates the locals `l_0 .. l_k` in field order, gives each the storage markers of `l`, turns `l = Aggregate(kind, ops)` into `l_i = Use(ops_i)` in field order, and turns `l.f_i.rest` into `l_i.rest`. Nested aggregates are split in later rounds. Enums are never split. The new locals are non-exposed, so `const_prop`, `elim_checks` and `copy_prop` track them.

### 9.4.8 Reference forwarding

`forward_refs` removes a borrow that is used only to access its place. For `r = Borrow(_, kind, P)`:

1. `r` is non-exposed and this is its only definition;
2. every use of `r` is the base of a place whose first projection is `Deref`: the reference is never copied, compared, passed or returned;
3. `P` is a local `l` followed only by `Field`, `ConstIndex` and `Downcast` projections;
4. this `Borrow` is the only `Borrow` or `RawPtr` of any place of `l` in the body;
5. on every path from the definition to a use of `r`, `l` is not assigned except through `r`, has no `SetDiscriminant` and no `StorageDead`.

Then each place `(*r).rest` becomes `P.rest`, and the definition of `r` is removed. Conditions 2 and 4 mean that no call can reach `l`, so `l` becomes non-exposed. Writes through a mutable borrow become direct writes of the same place.

### 9.4.9 Panic messages in `TrapOnly` mode

In `TrapOnly` mode, a panic is lowered to `unreachable` without its message and location ([06](06-wasm-lowering-and-layout.md) §6.8.12.2), and neither is observable (§9.2.1). `trap_only_messages` therefore rewrites every call of the panic lang item (N-F-49):

1. each argument that is the `Str` message and is not a constant becomes `Const("")`;
2. each argument that is the location argument ([04](04-mir.md) §4.13.7) and is not a constant becomes `Const(Location(s))` for the call's own span `s` ([04](04-mir.md) §4.13.6).

The computation that produced the old operands is then removed by `copy_prop` and `dse` under their ordinary rules, which never remove a `Call`, an `Assert`, or a write to `M`. Every effect of a message computation is therefore still performed, as [spec/10](../spec/10-errors-and-panics.md) N-C-84 requires, and the omission is unobservable. A message that is a string literal is already a constant and produces no data in `TrapOnly` mode ([06](06-wasm-lowering-and-layout.md) §6.8.14.2).

### 9.4.10 Dead-allocation elimination

`dead_alloc` removes a heap allocation whose address is never used except to store into it and free it (N-F-50). It runs only when the build's allocator is the default allocator or the tiny allocator ([10](10-runtime.md) §10.2.1). With an allocator package the allocator's functions are program code, and the pass does nothing.

For `A = Call{func: __sai_alloc, args: [size, align], dest: p, target}`:

1. `align` is a constant power of two, so `__sai_alloc` does not trap ([10](10-runtime.md) §10.2.2), and `p` is a non-exposed local whose only definition is `A`;
2. `C` is the smallest set that contains `p` and every non-exposed local `q` whose only definition is `q = Use(Copy(c))`, `q = Use(Move(c))` or a pointer `Convert` of `c`, with `c` in `C`;
3. every occurrence of every `c` in `C` is one of: a storage marker; the definition of another member of `C`; the base of the destination place of an `Assign`, whose rvalue does not mention `C` (a store into the block); or the first argument of `Call{func: __sai_free, args: [c, size', align']}`, where `size'` and `align'` are the same constants as `size` and `align`, or copies of the same non-exposed locals with no assignment in between.

Then `A` and every such `__sai_free` call become `Goto` of their targets, the stores into the block and the definitions of the members of `C` are removed, and `dse` removes the locals. This is correct because the block's address never escapes, so its contents are unobservable (§9.2.1); the possibility of an out-of-memory panic is a resource panic that optimization may remove (§9.2.1 item 7); and the allocator's state is not observable. When no call of the allocator remains, the allocator is not linked ([10](10-runtime.md) §10.1.4, D-WASM-05).

---

## 9.5 Identical code folding

Identical code folding (ICF) runs in `tiny` only, after the Wasm passes and before final dead-code elimination (§9.1.2 step 6). It merges defined functions whose code is identical, so that instances of a generic function for types with the same layout, or two closures with the same body, cost their bytes once (N-F-51). `release` does not fold, so that every instance keeps a function of its own and its name in the `name` section ([06](06-wasm-lowering-and-layout.md) §6.11.2).

### 9.5.1 Candidates

Every defined function of the module is a **candidate**, except:

1. a function that a core export names directly ([06](06-wasm-lowering-and-layout.md) §6.10.5): the export section keeps one function per export;
2. an address-taken function ([06](06-wasm-lowering-and-layout.md) §6.10.4): a function pointer is a value that the program can compare and convert, and two distinct functions have distinct table indices;
3. an instance of a function with `@inline(never)` (§9.4.5);
4. a runtime function (a `RuntimeSymbol`, [06](06-wasm-lowering-and-layout.md) §6.10.2): its component is fixed by [10](10-runtime.md) §10.1.1, and so is its group in the size tree (§9.10.2).

A function that is not a candidate is never merged, but it still occurs as a callee in the shapes of candidates.

### 9.5.2 Equivalence

The **shape** of a candidate `f` consists of:

1. its function type (parameter and result types);
2. its local declarations, as the sequence of Wasm value types after the Wasm passes (§9.7.3);
3. its instruction tree, compared instruction by instruction, by opcode and immediates, with these exceptions:
   - the immediate of `call g` is not part of the shape; the callees are collected in instruction order into the list `callees(f)`;
   - a reference to the table index of a function is compared by the function's symbol;
   - a reference to the address of a static data item, with its constant addend, is compared by the item's **content key**: its bytes, its alignment and its relocations, where a relocation to another data item is compared by that item's content key and a relocation to a function by the function's symbol. Two items with equal content keys become one item at linking ([06](06-wasm-lowering-and-layout.md) §6.8.14.3), so code that refers to either refers to the same bytes;
   - a global is compared by its symbol;
   - the owner tags of §9.7.4 are not part of the shape.

`tiny` always uses the `TrapOnly` panic mode (§9.3.2), so no code of a `tiny` build refers to a panic site record and no shape contains a location.

### 9.5.3 Algorithm

```
fold_identical(functions) -> merged: Map<Symbol, Symbol>:
    C := the candidates, in symbol order
    class(g) := g for every function g that is not a candidate          // singleton classes
    P := the partition of C into classes of equal shape
    loop:
        P' := split every class K of P by the key [class(g) for g in callees(f)], f in K
        if P' == P: break
        P := P'
    merged := {}
    for each class K of P with |K| >= 2:
        rep := the member of K with the smallest symbol
        for f in K, f != rep:
            merged[f] := rep
    replace every `call f` with f in merged by `call merged[f]`, in every function
    remove every f in merged from the module
    return merged
```

- The refinement starts from the coarsest partition and only splits classes whose members call into different classes. The result is the greatest fixpoint: two identical functions that call each other, or two identical recursive functions, stay in one class and are merged. The loop ends after at most `|C|` iterations, because every iteration that does not break splits at least one class.
- Members of one class execute the same instructions with the same effects, and their calls go to members of equal classes, so by induction on the length of an execution they behave identically. Which function index executes is not observable (§9.2.1 item 6), and the exclusions of §9.5.1 cover the only ways a program could tell two functions apart.
- `merged` is recorded for size attribution and retention chains (§9.6.5). The bytes of a removed function no longer exist in the module; the representative's bytes keep their owner tags. Data items that only removed functions referred to are removed by final dead-code elimination (§9.6.4).
- The result depends only on the shapes and symbols, never on the order in which classes are processed, so it is deterministic (§9.9).

---

## 9.6 Dead-code elimination

### 9.6.1 Overview

Dead-code elimination (DCE) removes code that no execution can reach from the module's entry points (N-F-52). It runs in three stages, in every profile:

| Stage | When | Works on | Decides |
|---|---|---|---|
| Item-level DCE (§9.6.2) | after Mono optimization | MIR instances | which instances are lowered, and which runtime components are linked ([10](10-runtime.md) §10.1.4) |
| Late root (§9.6.3) | after frames are assigned | the item table | whether the stack overflow path is added |
| Final DCE (§9.6.4) | after the Wasm passes and ICF | Wasm function bodies | which functions, data items and runtime globals the module contains |

In `dev`, no MIR or Wasm optimization runs, so DCE removes only what is unreachable in unoptimized bodies. This is the "DCE partial" of D-TGT-02.

DCE is not the reachability of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.3. That relation is computed on unoptimized code, decides capabilities and the import set, and is the same in every profile (N-F-20). DCE only ever removes code that the reachable set contains; it never adds an import, never removes one, and never changes an export.

### 9.6.2 Item-level DCE

`item_table(root, target, profile)` computes the instances to lower. It reads `opt_mono(f)` of every instance it visits.

**Roots**, in this order:

1. the functions that the core exports name (§9.1.2 `exports`), in the byte order of the export names: each `export fn` instance in a core-target build ([06](06-wasm-lowering-and-layout.md) §6.10.5); in a component build, the export glue functions and post-return functions of [07](07-component-lowering.md) (Mono bodies, N-D-64) and `cabi_realloc` when it is exported (N-D-49);
2. in a `sai test` build, the entry points that the test harness exports ([spec/14](../spec/14-testing.md) §14.7.1, [13](13-cli.md) §13.6.6.2 item 3), in the byte order of their export names: `Item(t)` for `test.<k>` and the harness entries of [04](04-mir.md) §4.13.10 for `case.<k>` and `counterexample.<k>`.

**Edges.** The body of an instance `f` has an edge to each of the following, in order of first occurrence (blocks in increasing index, statements in order, the terminator last):

1. the callee `g` of every `Call` whose callee is a `Const(Instance(g))` with a Mono body, including calls of closure bodies, which Mono MIR resolves statically, and, for a call of a component import, its import glue ([06](06-wasm-lowering-and-layout.md) §6.8.9). A call through a function pointer has no edge: its possible targets are the address-taken functions of item 2;
2. the function `g` of every `Convert(FnItemToFnPtr, Const(Instance(g)), _)`;
3. the drop glue `drop<T>` of every `Drop` whose place has a type `T` with drop glue;
4. in `Report` mode, `__sai_panic` for every `Assert` and every call of the panic intrinsic, because their lowering calls it ([06](06-wasm-lowering-and-layout.md) §6.8.12.2). In `TrapOnly` mode there is no such edge.

A constant `Alloc` names no function: a constant contains no function value, so a static allocation has no function-pointer relocation ([04](04-mir.md) §4.13.6). A function item used as a plain value is a zero-sized value that produces no code ([06](06-wasm-lowering-and-layout.md) §6.8.14.1), and constructing a closure (`Rvalue::Closure`) does not call it, so neither gives an edge: the calls they make possible are edges of the instances that make them. Calls of the allocation primitives are calls of `__sai_alloc`, `__sai_realloc` and `__sai_free` ([10](10-runtime.md) §10.2.2) and are edges of item 1. Calls of `extern "wasm"` functions and of core imports (`CoreImport`, [04](04-mir.md) §4.13.9) are edges to the imports. Imports are never removed (N-F-20), so these edges matter only for retention chains; import glue, which is a Mono body, is removed when nothing calls it.

**Result.** The item table is the set of instances reachable from the roots, listed in symbol order, together with each instance's edges. [10](10-runtime.md) §10.1.4 decides from it which runtime components are linked. An instance that is not in the item table is not lowered and contributes nothing to the module.

### 9.6.3 The late stack-overflow root

In `Report` mode the prologue of every function with a frame (`F > 0`) calls `__sai_stack_overflow` ([06](06-wasm-lowering-and-layout.md) §6.6.6). Frame sizes are known only after step 3 of [06](06-wasm-lowering-and-layout.md) §6.1.4, so this edge cannot be part of the item table. After frames have been assigned, `add_stack_overflow_path` (§9.1.2) therefore checks whether some lowered function has `F > 0`. If one does and the mode is `Report`, `__sai_stack_overflow` and every instance reachable from it by the edges of §9.6.2 that is not yet lowered (`__sai_panic` and its helpers) are optimized by `opt_mono` and lowered. Runtime bodies have no frame ([10](10-runtime.md) §10.1.2 rule 1), so the added functions never require the step to run again.

### 9.6.4 Final DCE

Final DCE runs on the Wasm bodies after the Wasm passes and ICF, immediately before linking.

- **Roots**: the functions named by the core exports, including `$export` wrappers ([06](06-wasm-lowering-and-layout.md) §6.6.7).
- **Edges** of a function: every `call` target; every function whose table index the code uses; every data item whose address the code uses; every global; every runtime-state item. Edges of a data item: the targets of its relocations, data items and functions.
- **Removal.** Every defined function, data item, runtime global and runtime-state item that is not reachable is removed. Imports are never removed.

The Wasm passes can remove calls. W1 (§9.7.2) removes the dead arm of a constant `if`. Final DCE can therefore remove a runtime component that [10](10-runtime.md) §10.1.4 linked. The component's state and globals then go with it, as [10](10-runtime.md) §10.1.4 requires ("present exactly when a linked runtime function refers to them"). The table and the element segment are formed from the remaining address-taken functions ([06](06-wasm-lowering-and-layout.md) §6.10.4), and `needs_stack` and `needs_memory` are computed on the remaining functions ([06](06-wasm-lowering-and-layout.md) §6.3.4). This is how optimization can remove the memory, and with it the `memory` export, of a module whose boundary passes no pointers (§9.1.2).

### 9.6.5 Retention chains

`sai size --why <name>` ([13](13-cli.md) §13.6.14) explains why the artifact contains something. The answer is a **retention chain**: a path in the final call graph from a root to the first code that keeps the item (N-F-53). The chain is computed on the final module, after final DCE, so it explains the bytes that the artifact actually contains.

**Target set.** The name selects a set `T`:

| Name | `T` |
|---|---|
| `allocator`, `panic`, `stack`, `Unicode`, `WASI` | every function, data item, global and import whose group is that node of the size tree (§9.10.2) |
| a package identity `ns:name` | every function, data item and import whose group is that package (the root package's identity selects `SAI code`). With two compatibility lines of the package in the build, `ns:name@version` selects one line. |
| a canonical path ([spec/04](../spec/04-modules-and-names.md) §4.9.1) | every instance whose display (below) equals the path, the bodies of their closures (`.{closure#N}`), their `$export` wrappers, and the data items they own |

A string that matches none of these, or a set `T` none of whose members has a byte in the artifact, gives `None`, which `sai size` reports as `NOT_PRESENT_IN_BUILD`. A function removed by ICF is present as its representative: if `f ∈ T` and `merged[f] = r` (§9.5.3), then `r` is added to `T`.

**Graph.** The nodes are the imported functions, the defined functions and the data items of the final module. The successors of a function are the functions, data items, globals and imports that its code references, in order of first occurrence in its instruction tree. The successors of a data item are its relocation targets, in offset order.

**Hits.** A node `n` **hits** if `n ∈ T`, or if some instruction of `n` references a member of `T`, or if some instruction of `n` has the owner tag `Inlined(g, into: n)` with `g ∈ T`. The **hit instruction** of `n` is the first such instruction in instruction order (for `n ∈ T`, none).

```
retention_chain(T) -> Option<[String]>:
    if no member of T has a byte in the artifact: return None
    for roots in [A, B]:
        // A: export targets other than cabi_realloc and post-return functions, and test entry points
        // B: cabi_realloc and post-return functions
        // both in the byte order of their export names
        parent := {}; seen := set(roots); queue := roots
        while queue is not empty:
            n := pop_front(queue)
            if hits(n): return Some(chain(n, parent))
            for m in successors(n):
                if m not in seen: seen.add(m); parent[m] := n; push_back(queue, m)
    return Some([display(T)])

chain(n*, parent) -> [String]:
    links := [display(T)]
    w := the hit instruction of n*
    if w exists and its owner tag is Inlined(g, into: n*) and g ∉ T:
        links.push(display(g))
    if not (n* ∈ T and display(n*) == display(T)):
        links.push(display(n*))
    n := n*
    while n has a parent: n := parent[n]; links.push(display(n))
    if len(links) > 2 and the last link is a `$export` wrapper or an export glue function:
        remove the last link
    return links
```

- A non-root member of `T` is never the first hit, because its parent references it and hits first. The chain therefore ends in the code that keeps `T`, as the user expects: for the allocator, the function that calls `__sai_alloc`, not `__sai_alloc` itself.
- Group A is searched before group B, so a chain through program code is preferred over the trivial chain "`cabi_realloc` is exported". The chain `[allocator, cabi_realloc]` appears only when no program code keeps the allocator.
- When the hit instruction came from inlining, the chain names the inlined function before the function it was inlined into. `allocator ← String.concat ← build_message ← create_user` is therefore the same chain whether or not `String.concat` was inlined. Only the innermost inlining record is named (N-F-47).
- An export wrapper or export glue function at the end is dropped, because the function it serves is the entry point the user wrote. Import glue in the middle of a chain is kept.
- `[display(T)]` alone means that `T` has bytes but no code references it: only imports can be in this state, because they are fixed before optimization (N-F-20), and every other node of the final module is reachable from a root.

**Display.** `display(n)` of a function or data item:

- an `ItemSymbol` ([06](06-wasm-lowering-and-layout.md) §6.10.2) is shown as its canonical path: every `@Version` and the `TypeArgs` are removed, the `.{closure#N}` suffixes are kept. `acme:app@0.1.0/main.run<I32>.{closure#0}` is shown as `acme:app/main.run.{closure#0}`;
- a `WrapperSymbol` is the display of its item followed by `$export`;
- a `DropSymbol` keeps its type argument, with every `@Version` removed: `drop<acme:app/model.Order>`;
- a data item is shown as the display of its owner, followed by `$data<k>` or `$site<k>`;
- `GlueSymbol`, `ImportSymbol` and `RuntimeSymbol` are shown unchanged;
- with two compatibility lines of one package in the build, the versions are kept for that package.

`display(T)` is the name as the user wrote it, in the canonical form of [spec/04](../spec/04-modules-and-names.md) §4.9.1 for a path. The JSON output lists the links in this form, and the human output shortens them ([13](13-cli.md) §13.6.14). The example of [13](13-cli.md) §13.6.14 is the chain of a build in which `acme:app.create_user`, reached through its export glue, calls `acme:app.build_message`, which calls `sai:alloc/string.String.concat`, whose code calls `__sai_alloc`.

---

## 9.7 Wasm passes

The Wasm passes run in `release` and `tiny` on each structured function body produced by [06](06-wasm-lowering-and-layout.md) §6.1.4 step 5 (N-F-54). They remove the copies that one-local-per-leaf lowering creates, fold branches whose conditions MIR could not see, and shrink local declarations. They are local to one function, run on every function independently and in parallel, and never change the function's type, its calls, its memory accesses or its traps.

### 9.7.1 Representation and rounds

```
WasmBody {
    params:  [ValType],
    results: [ValType],
    locals:  [ValType],                    // declared locals, indices after the params
    code:    [Instr],                      // the function body, without the final `end`
}
Instr = Plain { op: Opcode, imm: Immediates, tag: Owner }
      | Block { kind: Block | Loop, body: [Instr], tag: Owner }
      | If    { then_body: [Instr], else_body: Option<[Instr]>, tag: Owner }
```

References to functions, data items, globals and runtime state are symbolic until linking. Every block, loop and if has the block type `[] -> []`, except the `if (result i32)` instructions of checked 64-bit multiplication ([06](06-wasm-lowering-and-layout.md) §6.8.6), which the passes treat as opaque instructions that pop their operands and push one value.

Terms used below:

- A **sequence** is a `body`, `then_body` or `else_body` list, or the function's `code`. Positions in a sequence are counted from 0.
- The **stack height** at a position is the number of operand values pushed since the start of the sequence and not yet popped, computed by a forward walk with the stack effect of each instruction. After `br`, `br_table`, `return` or `unreachable`, the rest of the sequence is unreachable.
- A **pure producer** is a contiguous run of instructions in one sequence that pops nothing from before the run, pushes exactly one value, and consists only of `T.const`, `local.get`, `global.get`, loads, and numeric instructions other than integer division and remainder. Loads may be removed because code generated from valid MIR loads only from objects that lie inside memory ([06](06-wasm-lowering-and-layout.md) §6.8.1.3); a load that would trap belongs to an execution with undefined behavior (§9.2.1).
- A local `x` is **single-use** if it is not a parameter, is written by exactly one instruction, which is a `local.set x`, and is read by exactly one `local.get x`, and that get **follows** the set: the set is at position `i` of a sequence `S`, and the get is at a position `j > i` of `S` or inside a structured instruction at such a position.

`wasm_passes(b)` repeats the round `W1, W2, …, W9` (§9.7.2) until a round changes nothing or `WASM_ROUNDS` rounds have run, then runs `W10, W11, W12` (§9.7.3) once. Each pass rewrites the whole body before the next pass starts. The result is validated together with the module ([06](06-wasm-lowering-and-layout.md) §6.10.6).

### 9.7.2 The round

**W1. Constant branches and unreachable code.**

1. `i32.const c` followed by `If` becomes a `Block` whose body is `then_body` if `c ≠ 0`, and `else_body` (or nothing) if `c = 0`. The label depth is unchanged, because an `if` is a label like a `block`.
2. `i32.const c` followed by `br_if L` becomes `br L` if `c ≠ 0` and is removed if `c = 0`.
3. `i32.const c` followed by `br_table L_0 … L_{n-1} D` becomes `br L_c` if `c <u n` and `br D` otherwise.
4. Every instruction that follows `br`, `br_table`, `return` or `unreachable` in the same sequence is removed.

**W2. Inverted comparisons.**

1. An integer comparison followed by `i32.eqz` becomes the inverse comparison: `eq ↔ ne`, `lt_s → ge_s`, `lt_u → ge_u`, `gt_s → le_s`, `gt_u → le_u`, `le_s → gt_s`, `le_u → gt_u`, `ge_s → lt_s`, `ge_u → lt_u`, for `i32` and `i64`. `iN.eqz` followed by `i32.eqz` is not rewritten, because its inverse needs a constant.
2. `i32.eqz i32.eqz` immediately followed by `br_if` or `If` is removed: both consumers test for nonzero only.
3. Float comparisons are never inverted, because a comparison with NaN is false both ways.

**W3. Local forwarding.** The locals are visited in increasing index, each on the code as the previous visits left it:

- **Case A.** If `x` is single-use and its get immediately follows its set in the same sequence, both are removed: the value stays on the operand stack.
- **Case B.** If `x` is single-use, and the instruction immediately before its set is `T.const c` or `local.get y` with `y ≠ x`, the producer and the set are removed and the get is replaced by the producer. For `local.get y` it is required that no instruction between the set and the get, in instruction order, writes `y`, and that if the get lies in a `loop` that does not contain the set, the loop does not write `y`.
- **Case C.** Otherwise, every `local.set x` that is immediately followed by `local.get x` in the same sequence becomes `local.tee x`.

**W4.** `local.tee x` followed by `drop` becomes `local.set x`.

**W5. Dead stores.** If no instruction reads `x`, every `local.set x` becomes `drop` and every `local.tee x` is removed. Then every `drop` that is immediately preceded by a pure producer is removed together with the producer. `call`, `call_indirect`, integer division and remainder, `unreachable`, stores, `global.set`, `memory.copy`, `memory.fill`, `memory.grow` and `memory.size` are never removed.

**W6.** `local.get x` immediately followed by `local.set x` is removed.

**W7. Redundant branches.** A `br L` is removed if it is the last instruction of its sequence, the stack height before it is 0, the construct with label `L` is a `Block` or an `If`, not a `Loop`, and every construct between the `br` and that construct (the constructs with labels `0 … L-1`) is the last instruction of its enclosing sequence. Falling off the end then reaches the same place as the branch.

**W8. Untargeted constructs.** A `Block` or `Loop` whose label no branch inside it targets is replaced by its body. Every branch inside the body whose label refers to a construct outside the removed one is decremented by 1. A loop that is never continued runs its body once, like a block.

**W9.** An `If` whose `else_body` is empty loses its `else`.

### 9.7.3 Once at the end

**W10. Local coalescing.** Two locals of the same type **interfere** if one of them is live at an instruction that writes the other. Liveness is computed backwards over the control flow of the structured code. The function entry counts as a write of every declared local, because declared locals start at zero. Locals are colored greedily, in the order of their first write in instruction order (locals that are only written at the entry come last, in index order). Each parameter has its own color, and a declared local may take a parameter's color if it does not interfere with the parameter. A local takes the lowest-numbered color of its type that no interfering local has, where parameter colors come first in parameter order. Every reference is then rewritten to its color, and one declared local remains per color that is not a parameter.

**W11. Initial zero stores.** The **entry prefix** is the run of instructions from the start of the function body, descending into the body of every `Block` it reaches and continuing after the block's `end`, up to the first `If`, `Loop`, branch, `return` or `unreachable`. A `T.const 0` (for floats, `+0.0`) immediately followed by `local.set x` in the entry prefix is removed, if `x` is not a parameter and no earlier instruction of the prefix writes `x`. Such a store writes the value `x` already has.

**W12. Local order.** Declared locals with no remaining reference are removed. The rest are ordered by type (`i32`, `i64`, `f32`, `f64`, `v128`), then by decreasing number of references (`local.get`, `local.set` and `local.tee`), then by first occurrence in instruction order, and renumbered after the parameters. Locals that frequent references use get small indices, which have the shortest LEB128 encodings, and locals of one type form one declaration entry.

### 9.7.4 Owner tags

Every instruction carries the owner tag of [06](06-wasm-lowering-and-layout.md) §6.11.5 (`Function(f)` or `Inlined(g, into: f)`) from the MIR statement or terminator that it was lowered from. The passes keep the tags. An instruction that a pass creates takes the tag of the instruction it replaces: a `local.tee` that of the `local.set`, an inverted comparison that of the comparison, the `Block` or `br` of W1 that of the `If` or branch, the `drop` of W5 that of the `local.set`. An instruction that W3 moves keeps its own tag. The bytes of each instruction in the code section are attributed to its tag. The local declarations, the body's size prefix and its final `end` belong to the function itself.

### 9.7.5 Example A: `classify` in `release`

The function of [06](06-wasm-lowering-and-layout.md) §6.9.8, with `_1 = x` and `_2 = r`. Pipeline M changes one thing: return-place propagation (§9.4.4 rule 3) finds the single definition `_0 = _2` before `Return`, renames `r` to `_0` and removes the copy. Renumbering (§9.4.2) then renames `_3` and `_4` to `_2` and `_3`:

```
bb0: _0 = 0; _2 = Lt(_1, 0); SwitchInt(_2, [(0, bb2)], otherwise: bb1)
bb1: _0 = -1; Goto(bb4)
bb2: _3 = Gt(_1, 0); SwitchInt(_3, [(0, bb4)], otherwise: bb3)
bb3: _0 = 1; Goto(bb4)
bb4: Return
```

The CFG is the one of [06](06-wasm-lowering-and-layout.md) §6.9.8, so the structure is the same, with `$r` replaced by `$_0`, and `bb4` lowered to `local.get $_0` (the `return` is removed by peephole 2). The Wasm passes produce:

```wat
(func $classify (param $x i32) (result i32)
  (local $_0 i32)
  block
    local.get $x
    i32.const 0
    i32.lt_s
    if
      i32.const -1
      local.set $_0
    else
      local.get $x
      i32.const 0
      i32.le_s
      br_if 1
      i32.const 1
      local.set $_0
    end
  end
  local.get $_0)
```

- W3 case A removes `local.set $_2 local.get $_2` and `local.set $_3 local.get $_3`. Both temporaries are single-use and their gets follow their sets directly.
- W2 then turns `i32.gt_s i32.eqz` into `i32.le_s`.
- W7 removes both `br 1`. Each is the last instruction of an arm, the `if` is the last instruction of the `block`, and label 1 is that `block`.
- W11 removes `i32.const 0 local.set $_0` at the start of the `block`: `$_0` is already 0.
- W12 removes the two unused declarations.
- The `block` stays. The `br_if 1` still targets it, so W8 does not apply. The passes do not turn a `br_if` over the rest of an arm into an `if`.

`$_0` is not a parameter. It cannot share `$x`'s color, because `$x` is live at the entry, which counts as the first write of `$_0` (W10). Local names are shown for readability only: a `release` module has function names but no local names ([06](06-wasm-lowering-and-layout.md) §6.11.2).

### 9.7.6 Example B: `total` in `tiny`

```sai
fn total(xs: Slice<I32>) -> I64 {
    var sum: I64 = 0
    var i: USize = 0
    while i < xs.len() {
        sum = sum + I64.from(xs[i])
        i = i + 1
    }
    sum
}
```

The target is `wasm32-core` and the panic mode is `TrapOnly`. `_1 = xs`, `_2 = sum`, `_3 = i`. `xs.len()` calls `Slice.len` (`self: Self`, [spec/16](../spec/16-core-library.md) §16.10.1), an ordinary `sai:core` function whose optimized body reads the length of its receiver (`_0 = Len(_1); Return`, cost 1), so `inline` inlines it (§9.4.5). `I64.from` is a numeric conversion of D-TYPE-06 and is built as `Convert` directly ([04](04-mir.md) §4.2.4). The body after inlining is shown below. The inlined block is already merged into `bb1`, and three kinds of copy are already forwarded: the inlined parameter, the inlined return value and the index temporary of built-in indexing ([04](04-mir.md) §4.5.4). Value numbering and the intervals give a copy the number and the interval of its source (§9.4.6), so the analyses see the same facts with or without the copies. Storage markers are omitted:

```
bb0: _2 = 0; _3 = 0; Goto(bb1)
bb1: _4 = Len(_1); _5 = Lt(_3, _4); SwitchInt(_5, [(0, bb6)], otherwise: bb3)
bb3: _7 = Len(_1); _8 = Lt(_3, _7); Assert(_8, true, BoundsCheck { index: _3, len: _7 }, bb4)
bb4: _6 = _1[_3]; _9 = Convert(IntWiden, _6); _10 = CheckedBinaryOp(Add, _2, _9);
     Assert(_10.1, false, Overflow(Add), bb5)
bb5: _2 = _10.0; _11 = CheckedBinaryOp(Add, _3, 1); Assert(_11.1, false, Overflow(Add), bb7)
bb7: _3 = _11.0; Goto(bb1)
bb6: _0 = _2; Return
```

**The bounds check** is removed by proof 3 of §9.4.6.

- `idom(bb3) = bb1`. `Between(bb1, bb3)` is empty, because every path from a block of the loop body back to `bb3` passes through `bb1`. `_1` and `_3` therefore have the same numbers in `bb3` as at the end of `bb1`.
- So `Len(_1)` in `bb3` gets the number of `_4`, and `_8 = Lt(_3, _7)` gets the number of `_5`.
- The `otherwise` edge of the switch on the `Bool` `_5` establishes the fact `vn(_5) = 1`. `bb3` has the single predecessor `bb1`, so the fact holds there, and `Assert(_8, true)` is removed.

**The check of `i + 1`** is removed by proof 2. At `bb1` the interval of `_3` widens to `[0, max(USize)]`. The edge into `bb3` is refined by `_3 < _4`, which gives `_3.hi = max(USize) - 1`. The exact interval of `_3 + 1` therefore lies inside `USize`, the flag `_11.1` is `[0, 0]`, and `Assert(_11.1, false)` is removed. The `CheckedBinaryOp` becomes `_11.0 = WrapAdd(_3, 1); _11.1 = false`.

**The check of `sum + …` stays.** `_2` widens to the whole range of `I64`, and adding `[-2^31, 2^31 - 1]` can overflow.

The remaining rounds and the final step of pipeline M (§9.3.2) complete the work:

- `simplify_cfg` merges `bb3` with `bb4` and `bb5` with `bb7`.
- `sroa` splits `_11`, and `dse` removes `_11.1 = false`, `_7` and `_8`.
- Definition forwarding (§9.4.4 rule 2) turns `_11.0 = WrapAdd(_3, 1); _3 = _11.0` into `_3 = WrapAdd(_3, 1)`.
- Return-place propagation renames `_2` to `_0`. `_0` has the single definition `_0 = _2`; that `_2` has several definitions does not matter.

After renumbering (`_3, _4, _5, _6, _9, _10` become `_2, _3, _4, _5, _6, _7`; `bb3, bb5, bb6` become `bb2, bb3, bb4`):

```
bb0: _0 = 0; _2 = 0; Goto(bb1)
bb1: _3 = Len(_1); _4 = Lt(_2, _3); SwitchInt(_4, [(0, bb4)], otherwise: bb2)
bb2: _5 = _1[_2]; _6 = Convert(IntWiden, _5); _7 = CheckedBinaryOp(Add, _0, _6);
     Assert(_7.1, false, Overflow(Add), bb3)
bb3: _0 = _7.0; _2 = WrapAdd(_2, 1); Goto(bb1)
bb4: Return
```

**Structuring.** `bb1` is a loop header (the back edge `bb3 → bb1`). The reverse postorder is `bb0, bb1, bb4, bb2, bb3`. There are no merge nodes. Lowering ([06](06-wasm-lowering-and-layout.md) §6.8) declares, after the parameters, `$_0`, `$i`, `$_3`, `$_4`, `$_5`, `$_6`, `$_7.0`, `$_7.1` and the scratch locals `$s0`, `$s1` (the view's pointer and length, §6.8.1.1) and `$s2`, `$s3` (the checked addition's result and flag, §6.8.6):

```wat
(func $total (param $xs.0 i32) (param $xs.1 i32) (result i64)
  (local $_0 i64) (local $i i32) (local $_3 i32) (local $_4 i32) (local $_5 i32)
  (local $_6 i64) (local $_7.0 i64) (local $_7.1 i32)
  (local $s0 i32) (local $s1 i32) (local $s2 i64) (local $s3 i32)
  i64.const 0
  local.set $_0
  i32.const 0
  local.set $i
  loop
    local.get $xs.1
    local.set $_3
    local.get $i
    local.get $_3
    i32.lt_u
    local.set $_4
    local.get $_4
    if
      local.get $xs.0
      local.set $s0
      local.get $xs.1
      local.set $s1
      local.get $s0
      local.get $i
      i32.const 2
      i32.shl
      i32.add
      i32.load
      local.set $_5
      local.get $_5
      i64.extend_i32_s
      local.set $_6
      local.get $_0
      local.get $_6
      i64.add
      local.set $s2
      local.get $s2
      local.get $_0
      i64.xor
      local.get $s2
      local.get $_6
      i64.xor
      i64.and
      i64.const 0
      i64.lt_s
      local.set $s3
      local.get $s2
      local.get $s3
      local.set $_7.1
      local.set $_7.0
      local.get $_7.1
      if
        unreachable
      end
      local.get $_7.0
      local.set $_0
      local.get $i
      i32.const 1
      i32.add
      local.set $i
      br 1
    else
      local.get $_0
      return
    end
  end
  unreachable)
```

The final `unreachable` is peephole 3 of [06](06-wasm-lowering-and-layout.md) §6.9.7. The Wasm passes produce:

```wat
(func $total (param $xs.0 i32) (param $xs.1 i32) (result i64)
  (local $i i32) (local $_0 i64) (local $_6 i64) (local $s2 i64)
  loop
    local.get $i
    local.get $xs.1
    i32.lt_u
    if
      local.get $xs.0
      local.get $i
      i32.const 2
      i32.shl
      i32.add
      i32.load
      i64.extend_i32_s
      local.set $_6
      local.get $_0
      local.get $_6
      i64.add
      local.tee $s2
      local.get $_0
      i64.xor
      local.get $s2
      local.get $_6
      i64.xor
      i64.and
      i64.const 0
      i64.lt_s
      if
        unreachable
      end
      local.get $s2
      local.set $_0
      local.get $i
      i32.const 1
      i32.add
      local.set $i
      br 1
    else
      local.get $_0
      return
    end
  end
  unreachable)
```

- W3 case B moves `local.get $xs.1` to the use of `$_3`, `local.get $xs.0` to the use of `$s0`, `local.get $s3` to the use of `$_7.1`, and `local.get $s2` to the use of `$_7.0`.
- W3 case A removes the pairs of `$_4` and `$_5`. It removes the pair of `$s3` once `local.get $s2` has moved away from between them, which happens in a later round than the move of `local.get $s3`: the locals are visited in increasing index, and the set of `$_7.0` is immediately preceded by a producer only after case B has removed `local.set $_7.1`.
- W3 case C turns `local.set $s2 local.get $s2` into `local.tee $s2`.
- W5 removes the unused copy of the view's length into `$s1`, together with its producer.
- W11 removes the two zero stores before the `loop`.
- The `br 1` that continues the loop stays, because W7 never removes a branch to a `loop`.
- W10 merges nothing: `$_0` is live throughout the loop, and `$_6` is live where `$s2` is written.
- W12 removes the eight unused declarations and orders the rest. `$i` comes first as the only `i32`. Among the `i64` locals, `$_0` has 4 references, and `$_6` and `$s2` have 3 each; `$_6` occurs first.

A `tiny` module has no name section ([06](06-wasm-lowering-and-layout.md) §6.11.1). The names are those of [06](06-wasm-lowering-and-layout.md) §6.7.4, shown for readability only.

### 9.7.7 Example C: `add` in `tiny`

```sai
export fn add(a: I32, b: I32) -> I32 {
    a + b
}
```

On `wasm32-core` in `tiny`, the body after pipeline M is `bb0: _3 = CheckedBinaryOp(Add, _1, _2); Assert(_3.1, false, Overflow(Add), bb1)` and `bb1: _0 = _3.0; Return`. Nothing is known about `a` and `b`, so the check stays. `_3` is assigned by a `CheckedBinaryOp`, so `sroa` does not split it, and `_0 = _3.0` copies a field, so return-place propagation does not apply. The Wasm passes remove the leaf copies of `_3` and `_0` and the flag's scratch local:

```wat
(module
  (type (func (param i32 i32) (result i32)))
  (func (type 0) (param i32 i32) (result i32)
    (local i32)
    local.get 0
    local.get 1
    i32.add
    local.tee 2
    local.get 0
    i32.xor
    local.get 2
    local.get 1
    i32.xor
    i32.and
    i32.const 0
    i32.lt_s
    if
      unreachable
    end
    local.get 2)
  (export "add" (func 0)))
```

The function is exported directly: its parameters are `I32`, which need no boundary check, so it has no `$export` wrapper ([06](06-wasm-lowering-and-layout.md) §6.6.7). The module has no memory, table, global, data, import or custom section ([06](06-wasm-lowering-and-layout.md) §6.3.4, §6.11.1). It is 63 bytes:

| Part | Bytes |
|---|---|
| header (magic and version) | 8 |
| type section | 9 |
| function section | 4 |
| export section | 9 |
| code section: id, size, count, body size prefix | 4 |
| local declarations | 3 |
| instructions, including the final `end` | 26 |
| **total** | **63** |

The size suite (§9.11) requires at most 96 bytes for this program, so that a change of encoding details does not break the test while any added section or function does.

---

## 9.8 Size guarantees

The rules of §9.5 to §9.7, together with [06](06-wasm-lowering-and-layout.md) and [10](10-runtime.md), give every build the following properties. They are requirements on the implementation, and the size suite (§9.11.3) tests them.

1. **Fixed boundary.** The imports and the function exports are those of [06](06-wasm-lowering-and-layout.md) §6.10.1 and §6.10.5, computed before optimization (N-F-20). No profile adds or removes one. Only the `memory` export follows the memory (§9.1.2).
2. **No unused runtime.** After final DCE, the allocator is present only if a remaining function calls `__sai_alloc`, `__sai_realloc` or `__sai_free`, or `cabi_realloc` is exported. The panic reporter is present only in `Report` mode, and only if a remaining function calls `__sai_panic` or `__sai_stack_overflow`. Runtime state items and runtime globals are present only if a remaining runtime function refers to them ([10](10-runtime.md) §10.1.4, D-WASM-05).
3. **No unused module structure.** The module has a memory only if `needs_memory` holds, the global `__sp` only if `needs_stack` holds ([06](06-wasm-lowering-and-layout.md) §6.3.4), and a table only if a remaining function is address-taken ([06](06-wasm-lowering-and-layout.md) §6.10.4). Its data segments contain only data items that remaining code or remaining data items refer to (§9.6.4). It has no start section ([06](06-wasm-lowering-and-layout.md) §6.10.6).
4. **Custom sections** are exactly those of [06](06-wasm-lowering-and-layout.md) §6.11.1 for the profile.
5. **Direct exports.** An `export fn` whose parameters need no boundary check is exported directly, without a wrapper ([06](06-wasm-lowering-and-layout.md) §6.6.7).
6. **`tiny`.** A `tiny` build uses the `TrapOnly` panic mode (§9.3.2), the tiny allocator unless the root declares an allocator package ([10](10-runtime.md) §10.2.1), the `tiny` inlining constants (§9.3.4), and identical code folding (§9.5). Its core module has no custom section except `component-type:sai` in component builds. The capability denials of `tiny` are checked before optimization ([package/01](../package/01-package-contract.md) §1.14.2, [targets-and-profiles.md](../targets/targets-and-profiles.md) §5), so they never depend on what optimization removes.
7. **`@inline(never)`.** Every reachable instance of a function with `@inline(never)` exists as a function of its own, and every remaining call of it is a `call` of that function (§9.4.5).

---

## 9.9 Determinism

Every result of this chapter is a function of the program, the target, the profile and the toolchain version (N-F-55). This holds for the optimized MIR, the Wasm bodies, the ICF map, the item table, the module, the size tree, the retention chains and the budget diagnostics. The rules of [01](01-architecture.md) §1.8.3 apply to every pass. In addition:

1. **Every choice has a specified order.** No pass leaves a choice to the order of a container, of a traversal or of completion. The orders are:

   | Choice | Order | Section |
   |---|---|---|
   | strongly connected components of `mono_call_graph` | Tarjan's algorithm over nodes in symbol order, successors in first-occurrence order | §9.3.3 |
   | `const_prop` and interval worklists | increasing reverse-postorder position | §9.4.3, §9.4.6 |
   | inlining | call sites in increasing block index of the body before inlining | §9.4.5 |
   | renumbering | increasing old index | §9.4.2 |
   | ICF representative | the smallest symbol of the class | §9.5.3 |
   | item-level DCE | roots in export-name order, edges in first-occurrence order, result in symbol order | §9.6.2 |
   | retention chains | group A before group B, breadth first, successors in first-occurrence order, first discoverer as parent | §9.6.5 |
   | W3 | locals in increasing index | §9.7.2 |
   | W10 | locals in order of first write, lowest free color | §9.7.3 |
   | W12 | type, then decreasing reference count, then first occurrence | §9.7.3 |
   | `Dependencies` children | decreasing bytes, then name in byte order | §9.10.1 |
   | budget contributors | decreasing bytes, then tree order | §9.10.6 |

2. **Fixed limits.** Every limit is a constant of §9.3.4 and counts rounds, statements or instructions. No pass stops early because of time, memory or the number of threads, and no pass uses a heuristic whose result depends on them.
3. **Parallel work.** `opt_mono` of instances that do not depend on each other (§9.3.3), the Wasm passes of different functions, and the attribution of different entries may run in parallel. Their results are stored by symbol and merged in symbol order. ICF, final DCE, linking and retention chains run sequentially on the merged results.
4. **Exact folding.** Constant folding uses only exactly specified operations (§9.4.3, [01](01-architecture.md) §1.8.3 rule 6), so its results do not depend on the host.
5. **Incremental equals clean.** `opt_mono`, `item_table`, `size_report`, `retention_chain` and `size_budget_check` are queries. A value read from the cache equals the value a clean build computes ([01](01-architecture.md) §1.6.1), so the artifact never depends on the build history.

Consequently, two builds of the same `(root, target, profile)` with the same toolchain produce byte-identical artifacts and identical `sai size` output, for every `--jobs` value, cold or warm, and in every build directory ([15](15-conformance-and-testing.md) §15.6.3).

---

## 9.10 Size attribution

`sai size` and the budget check explain the bytes of the artifact that the build produces. Size attribution assigns every one of those bytes to exactly one leaf of a fixed tree. The rendering of the tree belongs to [13](13-cli.md) §13.6.14.

### 9.10.1 The size tree

The tree has this shape (N-F-56):

```
SAI code
Dependencies
    <one node per package>
Runtime features
    allocator
    panic
    stack
Unicode
WASI
Other
    module structure
    metadata
    component wrapper
```

```
SizeNode { name: String, bytes: u64, children: [SizeNode] }
```

1. **Partition.** Every byte of the artifact belongs to exactly one leaf. The `bytes` of a node with children are the sum of its children's bytes. The sum over the top-level nodes is the size of the artifact file.
2. **Top level.** The six top-level nodes are always present, in this order, also when they have 0 bytes. In the human output of [13](13-cli.md) §13.6.14, `Unicode 0.0 KiB` is such a node.
3. **`Dependencies`** has one child for each package other than the root that has at least one byte, in decreasing order of bytes, and packages with equal bytes in byte order of their names. The name is the package identity (`acme:hash`). `sai:core` and `sai:alloc` are packages like any other. When two compatibility lines of one package are in the build (D-PKG-02), each of its children is named with its resolved version (`acme:hash@1.4.2`).
4. **`Runtime features`** has the children `allocator`, `panic` and `stack`, and **`Other`** the children `module structure`, `metadata` and `component wrapper`, in this order. A child is present only if it has at least one byte.
5. `SAI code`, `Unicode` and `WASI` are leaves. `SAI code` is the root package, which includes the boundary code of its world (§9.10.3).

`sai size` without `--why` prints this tree. The JSON form of [13](13-cli.md) §13.6.14 lists the six top-level nodes with their children.

### 9.10.2 Input

For a core-target build, the input is the attribution table of the final module ([06](06-wasm-lowering-and-layout.md) §6.11.5), whose entries partition the module's bytes, together with the module's relocation graph (§9.6.4). For a component build, the table is first mapped into the component (§9.10.5). Each entry is assigned to a leaf by its owner (§9.10.3). The leaf's bytes are the sum of the lengths of its entries.

### 9.10.3 Owner mapping

The group of an entry is determined by its owner (N-F-57). For an owner that names a symbol, the group is that of the symbol. The first matching row applies:

| Symbol | Group |
|---|---|
| `__sai_alloc`, `__sai_realloc`, `__sai_free`, `__sai_oom`, the other runtime functions of the allocator component ([10](10-runtime.md) §10.1.1), `cabi_realloc`, and the global `__sai_heap_top` | `allocator` |
| `__sai_panic`, the other runtime functions of the panic reporter, the global `__sai_panicking`, and the import of the root's `@panic_hook` ([10](10-runtime.md) §10.5.3) | `panic` |
| `__sai_stack_overflow` and the global `__sp` | `stack` |
| a `GlueSymbol` whose core name begins with `wasi:`, including the export glue of `wasi:cli/run` | `WASI` |
| a `GlueSymbol` of provision glue, whose core name begins with `[provide]` or `[adapter]` ([07](07-component-lowering.md) §7.11.10) | the node of the providing package under `Dependencies` |
| the functions of the in-module handle table, `__sai_handle_` followed by a name ([10](10-runtime.md) §10.3.3) | `SAI code` |
| any other `GlueSymbol` | `SAI code` |
| an `ImportSymbol` whose module name begins with `wasi:` | `WASI` |
| any other `ImportSymbol`, and the static items of [07](07-component-lowering.md) (`__cabi_ret_area`, `__cabi_pending`, `__cabi_consumed_rep`) | `SAI code` |
| an `ItemSymbol` or `WrapperSymbol` of a Unicode item (§9.10.4) | `Unicode` |
| any other `ItemSymbol` or `WrapperSymbol` | the package of its `VPath`: `SAI code` for the root, otherwise its node under `Dependencies`. A function of an `extern "wasm"` block, and its import entry, belong to the package that declares it. |
| `drop<T>` | the package of the first canonical path in `T`, read from left to right: `drop<sai:alloc/vec.Vec<acme:app/model.Order>>` belongs to `sai:alloc`. `SAI code` if `T` contains no canonical path. |

With an allocator package, the `__sai_*` entry points are still runtime functions and belong to `allocator`, and the package's own functions belong to the package ([10](10-runtime.md) §10.2.7).

Entries are mapped by their owner:

| Owner | Group |
|---|---|
| `Function(s)` | the group of `s` |
| `Inlined(g, into: f)` | the group of `g`: inlining never moves the cost of a dependency into its caller (draft §75, "SAI does not hide dependency cost") |
| `DataItem(o$site<k>)`, and a data item that only panic-site records refer to | `panic` |
| `DataItem(o$data<k>)` | the group of its owner `o` |
| `Import(s)`, `Global(s)`, `ElementOf(s)` | the group of `s` |
| `Export(name)` of a function `f` | the group of `f` |
| `Export("memory")` | `module structure` |
| `FunctionType(i)`, `SectionOverhead`, `ZeroFill` | `module structure` |
| `Metadata(name)` | `metadata` |

The runtime state items of [10](10-runtime.md) §10.1.3 are zero-initialized and occupy no byte of the module, so they appear in no entry. The fixed text fragments of the panic reporter are data items of `__sai_panic` and belong to `panic` through its owner.

### 9.10.4 Unicode

An item is a **Unicode item** if all of the following hold (N-F-58):

1. it belongs to a standard package that the toolchain ships in its pre-populated cache (D-PKG-04, [spec/16](../spec/16-core-library.md) §16.16.1 item 1), other than `sai:core` and `sai:alloc`;
2. the source of that package used by the build is the shipped copy: the checksum of the package in the lock file equals the checksum that the toolchain records for the copy it ships;
3. its canonical path has a module segment `unicode` (as in `sai:text/unicode/normalize.nfc`).

The standard packages that a toolchain ships MUST place Unicode property tables, normalization and segmentation data, and the code that serves only them in modules with such a segment. Their data items belong to `Unicode` through the owner rule. `sai:core` contains no Unicode database ([spec/16](../spec/16-core-library.md) §16.9.1), so its `White_Space` test is ordinary `sai:core` code. Condition 2 means that a package outside the toolchain, or a modified copy, can never move its bytes into `Unicode`: they always appear under its own name.

### 9.10.5 Components

For a component build, the artifact is the component that [07](07-component-lowering.md) §7.11.3 produces from the core module of §9.1.2 step 7 (N-F-59).

1. The component is parsed with `wasmparser`. The embedded core module is the core module section whose non-custom sections are the sections of the step-7 module.
2. Each non-custom section of the embedded module MUST be byte-identical in content to the same section of the step-7 module, because `wit-component` keeps the module ([07](07-component-lowering.md) §7.11.6). A difference is an `INTERNAL_COMPILER_ERROR`. The entries of the attribution table that describe the content of a section are moved to the offset of that section in the component. The section headers (id and size) and the module header of the embedded module are `module structure`.
3. Every custom section of the embedded module is `metadata`. The `component-type:sai` section of the step-7 module is consumed by `wit-component` and is not in the artifact, so it counts nowhere.
4. If the component contains other modules or nested components that come from the artifact of a dependency, each of them belongs to that dependency's node under `Dependencies`.
5. Every other byte of the component (its header, type, import, export, alias, canonical-function and instance sections, and its own custom sections) is `component wrapper`.

A `wasm64-wasi` component build stops with `FEATURE_NOT_IMPLEMENTED` before an artifact exists ([07](07-component-lowering.md) §7.11.7), so it has no attribution.

### 9.10.6 Budgets

`size_budget_check(root, target, profile)` measures the artifact against the budgets of the profile ([package/01](../package/01-package-contract.md) §1.14.3) (N-F-60).

1. **Items.** The items are those of the `budget` block of `profile <profile>` in the root's `sai.package`. Without such a block, the query has no diagnostics and the build is not measured for budgets.
2. **Measured size.** `measured` is the length in bytes of the artifact that a successful build writes to `out/<target>/<profile>/` ([13](13-cli.md) §13.5.1): the core module for a core-target build, the component for a component build. It is taken from the artifact bytes of the same session (§9.1.2 step 9), never from the file system.
3. **Check.** Each item `wasm <= L` is checked separately, in source order, with `limit = L` in bytes. An item with `measured > limit` produces one `SIZE_BUDGET_EXCEEDED` error at the span of the item. `measured = limit` passes.
4. **Content.** The diagnostic reports `measured`, `limit` as written and in bytes, the excess `measured - limit`, and the three largest leaves of the size tree that have at least one byte, in decreasing order of bytes, leaves with equal bytes in tree order. Each contributor is given with its path in the tree (`Dependencies/acme:hash`) and its bytes. Fewer contributors are reported when fewer leaves are non-empty.
5. **Output.** `SIZE_BUDGET_EXCEEDED` is an error: the build fails and no output file is written ([package/01](../package/01-package-contract.md) §1.14.3 item 4, [13](13-cli.md) §13.5.1). `sai size` prints the size tree, or the retention chain with `--why`, computed from the artifact bytes of the session, also when budget errors are the only errors, and then exits as a failed build.

Example: with `profile tiny { budget { wasm <= 16 KiB } }` and an artifact of 18944 bytes, the diagnostic carries the following data. The rendering is defined by [11](11-diagnostics.md):

```json
{"measured":18944,"limit":16384,"limit_text":"16 KiB","excess":2560,"contributors":[{"group":["Dependencies","acme:hash"],"bytes":8294},{"group":["SAI code"],"bytes":3277},{"group":["Runtime features","allocator"],"bytes":2253}]}
```

---

## 9.11 Testing

The tests of this chapter are part of the compiler tests of [15](15-conformance-and-testing.md) §15.12.2 (N-F-61).

### 9.11.1 Pass tests

1. **MIR goldens.** Each MIR pass of §9.4 has golden tests of its input and output bodies in the MIR golden format of [15](15-conformance-and-testing.md) §15.4.2, with at least one test for each rule, each proof of §9.4.6, and each condition of a rule that prevents it from applying. The MIR validator runs after every pass in these tests (§9.2.3).
2. **Wasm goldens.** Each Wasm pass of §9.7 has golden tests of its input and output bodies. The bodies are printed by `wasmprinter` with indices instead of names, and every output module is validated by `wasmparser` with the features of [06](06-wasm-lowering-and-layout.md) §6.2.
3. **Examples.** Examples A, B and C (§9.7.5 to §9.7.7) are exact goldens: the final MIR and the final function bodies shown there MUST be produced, compared with indices instead of names.
4. **Generated programs.** The program generator of [15](15-conformance-and-testing.md) §15.10.2 runs every generated program in `dev`, `release` and `tiny` and compares the observable behavior (§9.2.1). The MIR validator runs after every pass.

### 9.11.2 Profile equivalence

The equivalence tests of [15](15-conformance-and-testing.md) §15.6.1 run every applicable conformance test in the three profiles. In addition, an optimization corpus contains, for each pass, programs on which the pass changes the code, and for each proof of §9.4.6 a program whose check is removed and a variant whose check fails at run time. Each program runs in the three profiles, and the panic, trap or result MUST be the same, with a panic in `dev` or `release` corresponding to a trap in `tiny`.

### 9.11.3 The size suite

The size suite consists of reference programs built in the listed configurations. Each row's requirement MUST hold:

| ID | Program | Configurations | Requirement |
|---|---|---|---|
| S1 | `export fn add(a: I32, b: I32) -> I32 { a + b }` | `wasm32-core`, `tiny` | at most 96 bytes; only the type, function, export and code sections; the body of §9.7.7 |
| S2 | the program of S1 | `wasm32-core`, `release` and `dev` | no memory, table, global, data or import section; the custom sections of the profile and no other; no `__sai_` function ([10](10-runtime.md) §10.11 item 9) |
| S3 | programs that allocate nothing, including Examples A and B | every target and profile | no allocator function, no `__sai_heap_top`, no allocator state; on WASI targets, no `cabi_realloc` unless [07](07-component-lowering.md) N-D-49 exports it |
| S4 | a program whose only allocation `dead_alloc` removes (§9.4.10) | `release`, `tiny` | no allocator, as in S3 |
| S5 | programs with panics of every kind, in `TrapOnly` builds (`tiny`, and `release` on `wasm32-core` without `@panic_hook`) | as listed | no `__sai_panic`, `__sai_stack_overflow`, `__sai_panic_buf`, `__sai_panic_ret` or `__sai_panicking`, and no `$site` data item |
| S6 | the reference programs of [10](10-runtime.md) §10.10 | as listed there | each bound of [10](10-runtime.md) §10.10 |
| S7 | a function with `@inline(never)` and an identical function without it, each called twice | `release`, `tiny` | the `@inline(never)` instance exists, is called by two `call` instructions, and is not merged |
| S8 | a generic function instantiated for `I32` and for `U32` with identical code | `tiny`; `release` | `tiny`: one function, the other removed by ICF; `release`: two functions |
| S9 | every program of the suite | every configuration | the leaves of the size tree sum to the artifact size; every entry has a group; component builds pass the check of §9.10.5 item 2 |
| S10 | the program of the [13](13-cli.md) §13.6.14 example, in two variants: with `String.concat` inlined into `build_message` and with `@inline(never)` on it | `wasm32-wasi`, `tiny` and `release` | `--why allocator` gives the chain of [13](13-cli.md) §13.6.14 in both variants; `--why` of an absent item gives `NOT_PRESENT_IN_BUILD` |
| S11 | a program with budgets of `measured - 1` and `measured` bytes | every profile | the first is `SIZE_BUDGET_EXCEEDED` with the contributors of §9.10.6, no output file is written, and `sai size` prints the tree; the second passes |
| S12 | every program of the suite | `--jobs 1` and `--jobs 8`, cold and warm, twice | byte-identical artifacts, size trees and chains |

---

## 9.12 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-F-39 | An optimization preserves the observable behavior of every execution with defined behavior: the sequence of import calls (with arguments and escaped memory), export returns (with results and escaped memory), panics (kind, message, location), traps that are not panics, and non-termination. Escaped memory is the memory reachable by the host, the memory the host wrote, and static data. Addresses, memory use, speed, code size, inlining, NaN bit patterns and unescaped memory are not observable. Whether a resource panic occurs is not observable (§9.2.1). | One definition that every pass and every profile-equivalence test checks against (P8). Making only the boundary observable is what allows a Wasm-first compiler to remove copies, frames and allocations (P3). The resource-panic exception is that of spec/10 §10.4.3. |
| N-F-40 | `opt_generic` runs the same cleanup and `simplify_cfg` in every profile. All profile-dependent optimization runs on Mono MIR or Wasm bodies (§9.2.2 item 1). | Instance collection, and every diagnostic that depends on the instance set, is then the same in every profile, so a profile never changes acceptance (P8). |
| N-F-41 | No optimization pass reports a diagnostic. A condition that a pass cannot handle is `INTERNAL_COMPILER_ERROR`. The only diagnostics whose presence can depend on optimization are `LARGE_STACK_FRAME`, `SIZE_BUDGET_EXCEEDED` and `INTERNAL_COMPILER_ERROR` (§9.2.2 items 2 and 3). | Acceptance must not depend on how clever the optimizer is (P8). The closed list makes the exceptions explicit and testable; a budget is a limit on optimized output by definition. |
| N-F-42 | The pass pipeline of each profile is fixed by the table of §9.3.1 and pipeline M (§9.3.2). There are no optimization options. Every limit is a constant of §9.3.4 that counts rounds, statements, fields or instructions (§9.3.1, §9.3.4). | A profile is the only knob (D-TGT-02), which keeps builds reproducible and comparable (P4, P8). Counting limits never depend on the host or on time. |
| N-F-43 | Every MIR pass uses one alias model: exposed locals (those with a `Borrow` or `RawPtr` of a place based on them without `Deref`), tracked places (non-exposed bases with `Field` projections), and one abstract memory `M` that calls, drops and awaits may read and write. No pass uses borrow-checking facts, the uniqueness of `borrow mut`, spec/12 §12.6.4 rule 3, refinements or contracts (§9.4.1). | A simple, conservative model is correct for unsafe code and interior mutability (`Arena`), and its correctness does not depend on the soundness of other analyses (P2, P8). The size and speed lost are small for the code shapes that v0.1 produces. |
| N-F-44 | Every MIR pass keeps validity, reducibility, the order of `Assert`s relative to effects, the order of calls, drops, awaits and traps, every span and source scope, and every reachable cycle. The MIR validator checks validity after every pass in debug builds and tests (§9.2.3). | These invariants are what make the first failing check, the panic location and the event sequence the same in every profile (N-F-39). |
| N-F-45 | `const_prop` is sparse conditional constant propagation over tracked leaves and enum variant values, with exact folding only. An `Assert` that always fails is kept with its condition and gets a target block containing only `Unreachable` (§9.4.3). | SCCP finds the constants that monomorphization exposes at linear cost (P3). A compile-time-known panic is a run-time panic, not an error (spec/10 §10.4.6). |
| N-F-46 | Inlining uses the cost table and `should_inline` of §9.4.5 with the constants of §9.3.4. A call is eligible only when the callee has a Mono body, is not `@inline(never)` or `@cold`, is in a lower call-graph component, is not a cold runtime function, and contains no `Await`. Calls of `__sai_alloc`, `__sai_realloc` and `__sai_free` are inlined only into runtime functions. | A fixed, small cost model makes inlining deterministic and its size effect predictable (P3, P4). Keeping allocator calls visible lets dead-allocation elimination see them (N-F-50). |
| N-F-47 | Inlining creates one new source scope per callee scope; the outermost carries `InlinedRecord { callee, call_span }`. Spans are kept, implicit location arguments are bound like other arguments, and code is attributed to the callee of the innermost record (§9.4.5). | Panic locations do not change with inlining (spec/10), and the size of a dependency stays with the dependency (§9.10.3). SourceScopeData needs the field; see [04](04-mir.md) §4.2.7. |
| N-F-48 | An `Assert` is removed only by a constant condition, by an interval analysis with `WIDEN_AFTER` widening and branch refinement, or by a fact of a dominating branch or check found by value numbering. A `CheckedBinaryOp` whose flag is proved false becomes the corresponding `WrapAdd`, `WrapSub` or `WrapMul` (§9.4.6). | Each proof is local, sound in exact integer arithmetic, and removes the common bounds and overflow checks of loops (P3). Using no contract facts keeps check removal independent of the fact checker in v0.1 (spec/09 §9.7.2). |
| N-F-49 | In `TrapOnly` mode, every non-constant message argument of a panic call becomes `Const("")`, and every non-constant location argument the location of the call's own span. The computations are then removed only under the ordinary rules, which keep calls, `Assert`s and writes to `M` (§9.4.9). | The message and location are not observable in `TrapOnly` mode (N-F-39), so their data need not exist; the effects of computing them are kept (spec/10 N-C-84). |
| N-F-50 | `dead_alloc` removes a `__sai_alloc` call with a constant power-of-two alignment whose result, and every copy of it, is used only as the base of stores and as the pointer of a matching `__sai_free` call, together with those calls and stores. It runs only with the default or tiny allocator (§9.4.10). | Such a block is unobservable, the out-of-memory panic is a resource panic (N-F-39), and removing the last allocation removes the allocator (D-WASM-05). An allocator package is program code whose calls must stay. |
| N-F-51 | Identical code folding runs in `tiny` only. Every defined function except direct export targets, address-taken functions, `@inline(never)` instances and runtime functions is a candidate. Classes are found by refining the partition of equal shapes until it is stable, and each class is merged into its member with the smallest symbol (§9.5). | Generic instances with equal layout cost their bytes once (P3). The greatest fixpoint also merges identical recursive functions. `release` keeps one function per instance for profiling and the `name` section. The exclusions are the only ways a program can tell functions apart. |
| N-F-52 | Dead-code elimination runs in three stages in every profile: item-level DCE on Mono instances, the late stack-overflow root after frame assignment, and final DCE on Wasm bodies. DCE never adds or removes an import and never changes a function export (§9.6). | Item-level DCE decides what is lowered and which runtime is linked; final DCE removes what the Wasm passes made dead. The boundary is fixed before optimization (N-F-20). |
| N-F-53 | A retention chain is found by breadth-first search over the final module from the roots of group A, then group B, to the first node that is in the target set or references it. An inlined function is named before the function it was inlined into, a trailing export wrapper or export glue function is dropped, links are shown by the display rules of §9.6.5, and a target without bytes gives `None`, reported as `NOT_PRESENT_IN_BUILD` (§9.6.5). | The chain explains the bytes actually present (P5, P6). Searching program code first gives the chain the user can act on. Naming inlined callees makes the chain independent of inlining decisions. |
| N-F-54 | The Wasm passes W1 to W9 are repeated until a round changes nothing or `WASM_ROUNDS` rounds have run, then W10 to W12 run once. They run in `release` and `tiny`, not in `dev`, never change a function's type, calls, memory accesses or traps, and keep owner tags (§9.7). | They remove the copies of one-local-per-leaf lowering and shrink local declarations (P3), with a small set of local rewrites instead of a general Wasm optimizer, whose result would be harder to attribute and to keep deterministic. Owner tags keep attribution exact (§9.10). |
| N-F-55 | Every result of this chapter is a function of the program, the target, the profile and the toolchain version. Every choice has the order of the table in §9.9; parallel results are merged in symbol order; ICF, final DCE, linking and retention chains are sequential (§9.9). | Byte-identical artifacts for every `--jobs` value, cold or warm (P4, P8, D-TOOL-03). |
| N-F-56 | The size tree has the six top-level nodes `SAI code`, `Dependencies`, `Runtime features`, `Unicode`, `WASI` and `Other`, always present and in this order. `Dependencies` lists the non-empty packages by decreasing bytes, then by name; package nodes carry a version only when two compatibility lines coexist. Every byte of the artifact belongs to exactly one leaf (§9.10.1). | A fixed shape makes outputs comparable between builds and tools (P5, P6). The partition makes the leaves sum to the file size. |
| N-F-57 | Each attribution entry is assigned to a group by its owner and symbol with the tables of §9.10.3. `Inlined(g, into: f)` belongs to the group of `g`; WASI glue and imports from `wasi:` modules belong to `WASI`; `drop<T>` belongs to the package of the first canonical path in `T`; panic-site records belong to `panic` (§9.10.3). | The cost of a dependency is never hidden in its caller (draft §75). Every byte has a defined group, so the tree is total. |
| N-F-58 | An item is a Unicode item if it belongs to a standard package shipped with the toolchain other than `sai:core` and `sai:alloc`, the package's checksum equals that of the shipped copy, and its canonical path has a module segment `unicode`. Shipped standard packages MUST place their Unicode data and the code that serves only it in such modules (§9.10.4). | The `Unicode` node of draft §75 needs a rule that a user package cannot exploit to hide its size. |
| N-F-59 | A component build is attributed by parsing the component with `wasmparser` and locating the embedded core module, whose non-custom sections MUST be byte-identical to those of the core module; a difference is `INTERNAL_COMPILER_ERROR`. Custom sections of the embedded module are `metadata`; modules from artifact dependencies belong to the dependency; every other byte is `component wrapper` (§9.10.5). | Attribution of the written artifact, without relying on internal data of `wit-component`. The identity check catches a wrapper that would change the module. |
| N-F-60 | Each `wasm <= L` item of the profile's `budget` block is checked separately against the length of the written artifact, taken from the bytes of the same session. `measured > limit` is one `SIZE_BUDGET_EXCEEDED` error with the measured size, the limit, the excess and the three largest non-empty leaves. The build fails and writes no output file; `sai size` still reports (§9.10.6). | Budgets must be actionable (P6): the contributors point at the cause, and `sai size` reports the artifact of the session, so no partial output is left behind (package/01 §1.14.3 item 4). |
| N-F-61 | Every pass has golden tests with the MIR validator running after each pass; Examples A, B and C are exact goldens; generated programs and an optimization corpus are run in the three profiles; and the size suite S1 to S12 MUST hold (§9.11). | The guarantees of §9.8 and the determinism of §9.9 are only meaningful if they are tested (P8). |

---

## 9.13 Diagnostics

This chapter defines no new diagnostic code. It uses the following:

| Code | Severity | Description |
|---|---|---|
| `SIZE_BUDGET_EXCEEDED` | error | defined in [package/01](../package/01-package-contract.md) §1.14.3; the artifact is larger than a `wasm <=` item of the profile's budget. Its data and contributors are those of §9.10.6. |
| `NOT_PRESENT_IN_BUILD` | info | defined in [13](13-cli.md) §13.6.14; the name given to `sai size --why` selects nothing with a byte in the artifact (§9.6.5). |
| `LARGE_STACK_FRAME` | info | defined in [06](06-wasm-lowering-and-layout.md) §6.7.2; its presence depends on inlining and scalar replacement (§9.2.2 item 3). |
| `TYPE_TOO_LARGE` | error | defined in [06](06-wasm-lowering-and-layout.md) §6.4.1; independent of the profile because the generic pipeline is the same in every profile (§9.2.2 item 1). |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; a `wasm64-wasi` component build stops before an artifact exists ([07](07-component-lowering.md) §7.11.7), so there is no size attribution (§9.10.5). |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10 and reported as in [11](11-diagnostics.md) §11.4.6; a pass that finds a condition it cannot handle (§9.2.2 item 2), a violated pass invariant (§9.2.3), a module that fails validation after the Wasm passes (§9.7.1), or an embedded core module that differs from the module the compiler produced (§9.10.5 item 2). |
