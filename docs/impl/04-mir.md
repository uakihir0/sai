# 04. MIR

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 1 (MIR data, construction of the Stage 1 constructs, checks, the match decision tree, `?`, validation, the text format, monomorphization of non-generic code); 2 (drop scheduling and drop elaboration, borrow-checker facts, closures, regions, generic monomorphization, drop glue); 3 (contract, invariant and refinement checks, the SFC mapping, f-strings); 4 (trust-boundary validation shims); 5 (`.await`, the async transform, `ASYNC_RECURSION`). Marked inline.
**Related:** [../design/decisions.md](../design/decisions.md) D-MIR-01, D-OWN-01, D-OWN-04, D-OWN-05, D-EXPR-02, D-EXPR-05, D-CONTRACT-01, D-CONTRACT-04, D-ASYNC-01, D-ERR-01; [01-architecture.md](01-architecture.md), [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md), [05-borrow-checker.md](05-borrow-checker.md), [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md), [07-component-lowering.md](07-component-lowering.md), [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), [09-optimization-and-size.md](09-optimization-and-size.md), [10-runtime.md](10-runtime.md), [11-diagnostics.md](11-diagnostics.md), [13-cli.md](13-cli.md), [15-conformance-and-testing.md](15-conformance-and-testing.md); [../spec/05-types.md](../spec/05-types.md), [../spec/06-expressions-and-statements.md](../spec/06-expressions-and-statements.md), [../spec/07-ownership-and-borrowing.md](../spec/07-ownership-and-borrowing.md), [../spec/09-contracts-and-refinements.md](../spec/09-contracts-and-refinements.md), [../spec/10-errors-and-panics.md](../spec/10-errors-and-panics.md), [../spec/11-async-and-concurrency.md](../spec/11-async-and-concurrency.md), [../spec/16-core-library.md](../spec/16-core-library.md)

This chapter specifies the mid-level intermediate representation (MIR) of the reference compiler and everything that produces or transforms it. It covers the Rust data structures that extend the skeleton of D-MIR-01 (N-E-40), the instances and compiler-generated bodies (shims), the construction of MIR from typed HIR (places and temporaries, drop scheduling, the match decision tree, `?`, loops, closures, regions, `.await`, and the runtime checks for overflow, bounds, contracts, invariants and refinements), the phases `Built`, `DropElaborated`, `Analyzed`, `Optimized` and `Mono` with drop elaboration and the validator of each phase, the interface between MIR and the Standard Fact Checker (SFC), monomorphization and its limits, the `ASYNC_RECURSION` check, and the text format of MIR dumps with worked examples. The language rules belong to the spec chapters: evaluation order and temporaries to [spec/06](../spec/06-expressions-and-statements.md), moves and drops to [spec/07](../spec/07-ownership-and-borrowing.md), contracts and the SFC to [spec/09](../spec/09-contracts-and-refinements.md), panics to [spec/10](../spec/10-errors-and-panics.md), futures to [spec/11](../spec/11-async-and-concurrency.md). This chapter implements them and does not restate them. The borrow checker that runs on MIR is [05](05-borrow-checker.md). The backend that consumes Mono MIR is [06](06-wasm-lowering-and-layout.md).

---

## 4.1 Overview

### 4.1.1 Pipeline

MIR is built per **root owner**: an item with a body ([02](02-frontend-ast-hir.md) §2.8.2). Closures, `async` blocks and contract clauses are not owners. Their bodies are built, checked and stored together with their root owner (§4.2.1), as [05](05-borrow-checker.md) N-B-31 requires for the borrow checker.

```text
typeck(owner), check_match(owner)                        impl/03                          phase 7
  -> mir_built(owner)              Built                 §4.4 to §4.10                    phase 8
  -> move_check(owner)                                   impl/05 pass A
  -> mir_drop_elaborated(owner)    DropElaborated        §4.11.2, §4.12.3                 phase 8
  -> borrowck(owner)                                     impl/05 pass B                   phase 9
     body_effect_summary, effect_check (per effect body) impl/08 §8.1.4, §8.3             phase 9
     sfc(owner)                                          §4.12, spec/09 §9.6             phase 9
  -> mir_analyzed(owner)           Analyzed              §4.11.3                          phase 9
  -> mono_items(())                instance collection   §4.13.1 to §4.13.4               phase 10
  -> mir_optimized(owner)          Optimized             §4.10.5, impl/09                 phase 11
  -> mono_instances(root, target)  code-generation set   §4.13.1, §4.13.2                 phase 11
  -> mir_mono(instance)            Mono                  §4.13.5 to §4.13.9               phase 11
  -> opt_mono(instance, ..)                              impl/09 §9.3                     phase 11
```

The phase numbers are those of [01](01-architecture.md) §1.2.1. Instance collection is a check (§4.13.1): its diagnostics `INSTANTIATION_DEPTH_EXCEEDED`, `IMPLEMENTATION_LIMIT_EXCEEDED` and `ASYNC_RECURSION` are errors of the program, so `sai check` must report them. It therefore runs in phase 10 on Analyzed MIR, independently of optimization (N-E-51). Code generation uses a second collection with the same rules, `mono_instances`, whose roots are those of the artifact (§4.13.1). The Mono bodies of phase 11 are built for its instances only, and impl/09 optimizes them (`opt_mono`).

### 4.1.2 Queries

All queries belong to the query group of the crate `sai_mir` ([01](01-architecture.md) §1.3.1).

| Query | Key | Value | Check query | Section |
|---|---|---|---|---|
| `mir_built(owner)` | `OwnerId` | `Arc<OwnerMir>` in phase `Built`, or `OwnerMir::Skipped` | no | §4.4 |
| `mir_drop_elaborated(owner)` | `OwnerId` | `Arc<OwnerMir>` in phase `DropElaborated` | no | §4.11.2 |
| `mir_analyzed(owner)` | `OwnerId` | `Arc<OwnerMir>` in phase `Analyzed`, with the `CONTRACT_RUNTIME_CHECK` warnings | yes | §4.11.3 |
| `future_layout(def)` | `DefId` of an `async fn` or `async` block | `Arc<FutureLayout>` (generic) | no | §4.10.6 |
| `mono_items(())` | the session | `Arc<MonoItems>`: the instances of the check collection in collection order, with the diagnostics of §4.13.3 and §4.13.4 | yes | §4.13.1 |
| `mono_instances(root, target)` | `(PackageId, TargetId)` of the session | `Arc<[Instance]>`: the instances to generate code for, in collection order | no | §4.13.1 |
| `mir_optimized(owner)` | `OwnerId` | `Arc<OwnerMir>` in phase `Optimized` | no | §4.11.4 |
| `mir_mono_unlocated(instance)` | `Instance` | the Mono body before caller locations are added (§4.13.5 steps 1 to 6) | no | §4.13.5 |
| `mir_mono(instance)` | `Instance` | `Arc<Body>` in phase `Mono`: the substituted body, the shim or the synthetic body of the instance | no | §4.13.5 |
| `instance_sig(instance)` | `Instance` | the parameter and result types of `mir_mono(instance)`, including the hidden location parameter | no | §4.13.5 |
| `mir_shim(instance)` | `Instance` with a `Shim` definition | `Arc<Body>` in phase `Mono` | no | §4.3.3 |
| `caller_location_forwarding(instance)` | `Instance` | `bool`: whether the instance is forwarding | no | §4.13.7 |
| `obligation_table(owner)` | `OwnerId` | the obligations of the owner's DropElaborated MIR (§4.12.1) | no | §4.12 |
| `harness_entries(())` | the session | `Arc<HarnessEntries>`: the plans and the entry bodies of the selected tests of a `sai test` session | no | §4.13.10 |

`mir_built` and `mir_drop_elaborated` report no diagnostic of the program. Every rule they rely on was checked by [03](03-name-resolution-and-type-checking.md) or is checked by the analyses that consume them. A violation of a MIR invariant found by any query of this chapter is an internal compiler error (`INTERNAL_COMPILER_ERROR`, [11](11-diagnostics.md) §11.4.6, [01](01-architecture.md) §1.10.2). The SFC's own query `sfc(owner)` belongs to the crate `sai_sfc` and carries `REFINEMENT_NOT_PROVEN` ([spec/09](../spec/09-contracts-and-refinements.md) §9.2.3). It is a check query of phase 9.

### 4.1.3 Gates

1. `mir_built(owner)` returns `OwnerMir::Skipped` if `typeck(owner)` is tainted or `check_match(owner)` reported an error ([01](01-architecture.md) §1.2.3 gate 1, [03](03-name-resolution-and-type-checking.md) §3.16 item 6). Every later query of the owner then returns `Skipped`, and no diagnostic is derived from the owner (§4.4.1).
2. `mir_drop_elaborated(owner)` runs whenever `mir_built(owner)` produced bodies, even if `move_check(owner)` reported errors, because pass B of [05](05-borrow-checker.md) runs on its result in that case too ([05](05-borrow-checker.md) §5.1.2). An illegal move produces no move event in pass A, so the initialization results are well defined.
3. `mir_analyzed(owner)` returns `OwnerMir::Errored` if `move_check(owner)` or `borrowck(owner)` reported an error ([05](05-borrow-checker.md) §5.1.2: such a body never reaches `Analyzed`). Errors of effect analysis and of the SFC do not make an owner `Errored`. They stop the build at gate 2 of [01](01-architecture.md) §1.2.3, after phase 10.
4. Phase 10 treats a `Skipped` or `Errored` owner as having no body. An instance whose definition has no body is not collected and reports nothing (§4.13.2), because the error that removed the body has been reported.
5. Phases 11 and later run only behind gate 2 of [01](01-architecture.md) §1.2.3. `mir_optimized` and `mir_mono` therefore never see an `Errored` owner.

### 4.1.4 Crate boundaries and hooks

`sai_mir` depends on `sai_ty` only ([01](01-architecture.md) §1.3.1). The crates `sai_borrowck`, `sai_effects`, `sai_sfc`, `sai_opt` and `sai_runtime` depend on `sai_mir`. The MIR phases need results of those crates: drop elaboration needs the drop classification of pass A, `mir_analyzed` needs the borrow-check error flag, the call effects and the SFC decisions, `mir_optimized` needs the passes of impl/09, and monomorphization needs the runtime bodies of impl/10 and the glue bodies of impl/07. `sai_mir` reaches them through a **hook trait** that is a supertrait of its query group (N-E-56):

```rust
/// Implemented by the root database by forwarding to the queries of the downstream crates.
/// The value types are defined in `sai_mir`, so that no crate depends on a crate that depends on it.
pub trait MirHooks {
    fn drop_classes(&self, owner: OwnerId) -> Arc<DropClasses>;          // sai_borrowck, pass A, impl/05 §5.12.1
    fn borrowck_failed(&self, owner: OwnerId) -> bool;                   // sai_borrowck, pass A or B reported an error
    fn call_effects(&self, owner: OwnerId) -> Arc<CallEffectTable>;      // sai_effects, impl/08 §8.3.2
    fn sfc_decisions(&self, owner: OwnerId) -> Arc<SfcDecisions>;        // sai_sfc, §4.12.2
    fn sfc_boundary_requires(&self, f: DefId) -> Arc<[SfcResult]>;      // sai_sfc, §4.12.5
    fn optimize(&self, body: &mut Body, pass_set: OptPassSet);           // sai_opt, impl/09 §9.1.3 `opt_generic`
    fn runtime_body(&self, symbol: Symbol) -> Arc<Body>;                 // sai_runtime, impl/10 §10.1.2
    fn glue_bodies(&self, root: PackageId, target: TargetId) -> Arc<[Arc<Body>]>;   // sai_component, impl/07 §7.1.1 stage 3
}

#[salsa::query_group(MirStorage)]
pub trait MirDb: TyDb + MirHooks {
    fn mir_built(&self, owner: OwnerId) -> Arc<OwnerMir>;
    fn mir_drop_elaborated(&self, owner: OwnerId) -> Arc<OwnerMir>;
    fn mir_analyzed(&self, owner: OwnerId) -> Arc<OwnerMir>;
    // … the other queries of §4.1.2
}
```

`RootDatabase` implements `MirHooks` by calling the corresponding queries, so every call through a hook is a tracked query dependency ([01](01-architecture.md) §1.4.2 item 1). The crate graph stays acyclic: `sai_borrowck` depends on `sai_mir` for the MIR types and the value type `DropClasses`, and `sai_mir` depends on nothing downstream. `optimize` mutates a private copy that the calling query owns, so it is not a side channel. `OptPassSet` has the single variant `Generic`: the optimization of Mono bodies is `opt_mono` of [09](09-optimization-and-size.md) §9.3, a query of `sai_opt` that reads `mir_mono`, so `sai_mir` never calls it. `glue_bodies` reaches `sai_component`, which depends on `sai_codegen_wasm`. A hook does not add a crate dependency, so this is allowed, but the glue bodies MUST NOT depend on `mono_instances`, or the two queries would form a cycle (§4.13.1).

Contract check code is built by `sai_mir` (§4.12.1), as the crate table of [01](01-architecture.md) §1.3.1 records: it is part of MIR construction and must exist before the borrow checker runs. `sai_sfc` decides the obligations and builds nothing. The hook trait follows rule 6 of [01](01-architecture.md) §1.3.2.

---

## 4.2 MIR data

### 4.2.1 Owners and bodies

```rust
pub enum OwnerMir {
    Skipped,                       // gate 1 of §4.1.3
    Errored,                       // gate 3 of §4.1.3
    Bodies(OwnerBodies),
}

pub struct OwnerBodies {
    pub owner: OwnerId,
    pub bodies: IndexVec<BodyIndex, Body>,   // BodyIndex(0) is the owner's own body
}
```

The bodies of an owner are numbered in this order (N-E-41):

1. The owner's own body.
2. The bodies of its closures and `async` blocks, in pre-order of their HIR nodes, which is the order of their `DefId`s ([02](02-frontend-ast-hir.md) §2.8.2).
3. The bodies of its contract clauses (§4.12.1), in the order of `contract_clauses(def)` ([03](03-name-resolution-and-type-checking.md) §3.7.14): `requires` clauses, then `ensures` clauses, each in declaration order.

A refinement predicate and an `invariant` declaration are owners of their own ([03](03-name-resolution-and-type-checking.md) §3.13.2). An `invariant` owner has one body per element, in declaration order, because each element is a separate goal and a separate check ([spec/09](../spec/09-contracts-and-refinements.md) §9.6.6). A derived impl is an owner whose bodies are built synthetically, one per method of the trait, in the order of the trait's methods (§4.3.3).

```rust
pub struct Body {
    pub source: BodySource,
    pub phase: MirPhase,
    pub locals: IndexVec<Local, LocalDecl>,        // _0 is the return place
    pub arg_count: usize,                          // _1 ..= _arg_count are the parameters
    pub blocks: IndexVec<BasicBlock, BlockData>,   // bb0 is the entry
    pub source_scopes: IndexVec<SourceScope, SourceScopeData>,
    pub obligations: IndexVec<ObligationId, Obligation>,   // §4.12.1; empty from Optimized on
    pub unlowered: Vec<HirId>,                     // unreachable code and arms (impl/03 N-E-29), in source order
    pub coroutine: Option<CoroutineInfo>,          // async bodies, Stage 5, §4.10
    pub caller_location: Option<Local>,            // Mono only, §4.13.7
    pub span: Span,
}

pub enum BodySource {
    Item(OwnerId),                 // fn, method, test, property, refinement predicate
    InvariantElement { owner: OwnerId, index: u32 },
    Closure(LocalDefId),
    AsyncBlock(LocalDefId),        // includes the synthesized block of an `async fn` (impl/02 §2.9.15)
    Clause(ContractId),            // §4.12.1
    Derived { owner: OwnerId, method: Symbol },
    Shim(Instance),                // Mono only, §4.3.3
    Synthetic { producer: Producer, symbol: Symbol },   // Glue (impl/07) | Runtime (impl/10) | Harness (§4.13.10); Mono only, §4.13.9
}

pub enum MirPhase { Built, DropElaborated, Analyzed, Optimized, Mono }
```

`MirPhase` is ordered. A body's phase only increases. Each query of §4.1.2 returns bodies of exactly one phase.

### 4.2.2 Locals

```rust
pub struct LocalDecl {
    pub ty: Ty,
    pub mutability: Mutability,          // Mut | Not
    pub origin: LocalOrigin,
    pub source_info: SourceInfo,         // span and scope of the declaration
}

pub enum LocalOrigin {
    Return,
    Param(u32),
    UserVar(BindingId),
    Temp,
    ArenaStorage(HirId),                 // the arena of `region r { … }`, §4.8.6
    ClosureEnv,                          // `_1` of a closure or async body, §4.8.5
    GuardScrutineeRef,                   // `_sr`, §4.7.4
    GuardBindingRef(BindingId),          // by-reference view of a by-value binding inside a guard, §4.7.4
    DropFlag,                            // appended by drop elaboration, §4.11.2
    DirtyFlag,                           // appended by drop elaboration, §4.12.3
    AwaitSlot,                           // the slot of an `.await`, §4.10.2
    CallerLocation,                      // the hidden parameter of §4.13.7, Mono only
}
```

These are the origins of [05](05-borrow-checker.md) F1 plus `GuardBindingRef`, `DirtyFlag` and `CallerLocation`. `mutability` is `Mut` exactly for `var` bindings, `var` parameters, temporaries, drop flags, dirty flags and await slots. Old snapshots (§4.12.1) are `Temp`s.

### 4.2.3 Places, operands and constants

```rust
pub struct Place { pub local: Local, pub projection: Interned<[PlaceElem]> }

pub enum PlaceElem {
    Field(FieldIdx, Ty),                 // a field of a record, tuple, variant (after Downcast), closure or future
    Deref,
    Index(Local),                        // the index is a local of type USize
    ConstIndex { offset: u64, min_length: u64, from_end: bool },
    Subslice { from: u64, to: u64, from_end: bool },
    Downcast(VariantIdx),
}

pub enum Operand { Copy(Place), Move(Place), Const(Box<Constant>) }

pub struct Constant { pub ty: Ty, pub kind: ConstKind }

pub enum ConstKind {
    Scalar(ScalarBits),                  // integers, floats (bit pattern), Bool, Char, Byte
    Zst,                                 // a zero-sized value: (), a unit variant of a single-variant enum, FnDef
    Str(Symbol),                         // a string literal; the type is Str
    ByteStr(Interned<[u8]>),             // a byte-string literal
    Alloc(AllocId),                      // a promoted constant in a static allocation, §4.13.6
    Aggregate { variant: Option<VariantIdx>, fields: Interned<[Constant]> },
    FnItem(DefId, GenericArgs),          // a function item; its type is FnDef(def, args)
    Location(SpanId),                    // the address of the static location record of a span, Mono only, §4.13.7
    Instance(Instance),                  // a resolved function instance, Mono only, §4.13.5
    Param(ParamConst),                   // a const generic parameter; substituted by monomorphization
}
```

- `Deref` of a place `s` of a view type (`Str`, `Slice<T>`, `SliceMut<T>`) is the **viewed sequence** `*s`, the elements that `s` views. It is not a value, because SAI has no dynamically sized types (D-TYPE-02). `*s` is followed by `Index`, `ConstIndex` or `Subslice`, or it is the operand of `Len` or `Borrow`. `Index` and `ConstIndex` of `*s` are elements of type `T`, and `Borrow(k, *s)` has the type of `s`: it is the reborrow of a view ([05](05-borrow-checker.md) F17). `Len` applies to a view and to its viewed sequence alike. [06](06-wasm-lowering-and-layout.md) §6.8.1.1 lowers these places.
- `Subslice` has the meaning of [05](05-borrow-checker.md) F15: for an owned `Array<U, N>`, `Subslice { from, to, from_end: true }` denotes the positions `from ..< N - to`. On the viewed sequence `*s` of a view it is a view, of the type of `s`, of the same positions.
- A constant item and an explicit discriminant are embedded as their `const_value` ([03](03-name-resolution-and-type-checking.md) N-E-33). `ConstValue::Int`, `Float`, `Bool`, `Char` and `Byte` become `Scalar`; `Str` and `ByteStr` become the literal kinds; `Aggregate` and `Array` become `Aggregate` when every leaf is a scalar, and an `Alloc` otherwise.
- From `Analyzed` on, a `Constant` contains no inference data. In `Mono` it contains no `Param`, no type parameter and no `FnItem`: every function constant is an `Instance`. An `Instance` constant is zero-sized. Its `ty` is `FnDef(d, args)` for an instance `Item(d)` and `()` for every other instance. No consumer reads that type: the validator checks a call against `instance_sig` (§4.13.5), and impl/06 calls or reifies the instance itself.

### 4.2.4 Rvalues

```rust
pub enum Rvalue {
    Use(Operand),
    Borrow(RegionVid, BorrowKind, Place),        // BorrowKind: Shared | Mut | TwoPhaseMut
    RawPtr(Mutability, Place),
    BinaryOp(BinOp, Operand, Operand),
    CheckedBinaryOp(BinOp, Operand, Operand),    // type (T, Bool); BinOp ∈ { Add, Sub, Mul }
    UnaryOp(UnOp, Operand),                      // UnOp: Not | Neg
    Convert(ConvertKind, Operand, Ty),
    Aggregate(AggregateKind, Vec<Operand>),
    Discriminant(Place),
    Len(Place),
    Repeat(Operand, ConstUsize),
    Closure(LocalDefId, Vec<Operand>),           // closures and async blocks, §4.8.5
}

pub enum BinOp {
    Add, Sub, Mul, Div, Rem, WrapAdd, WrapSub, WrapMul,
    BitAnd, BitOr, BitXor, Shl, Shr, Eq, Ne, Lt, Le, Gt, Ge,
}

pub enum AggregateKind {
    Tuple,
    Array(Ty),
    Adt { def: DefId, variant: VariantIdx, args: GenericArgs },   // records, enums, distinct and refined types
}
```

- `RegionVid` is a fresh region variable per `Borrow`, numbered from 0 in creation order within the body. It is the loan's region for [05](05-borrow-checker.md) §5.6.2. Regions in local types are `Erased` ([03](03-name-resolution-and-type-checking.md) §3.15.2 item 3). The borrow checker gives them variables of its own. The backend ignores regions.
- `BinaryOp` on integers never panics. Every check that spec/06 requires is a separate `Assert` before or after it (§4.9). `Add`, `Sub` and `Mul` of integers occur only as `CheckedBinaryOp`, whose field 0 is the wrapped result and field 1 the overflow flag ([06](06-wasm-lowering-and-layout.md) §6.1.1). `BinaryOp(Div | Rem, a, b)` is only reachable when `b ≠ 0` and, for signed `Div`, not `MIN / -1`. `Shl` and `Shr` are only reachable with an amount below the bit width. On floats, every `BinOp` except the `Wrap*` and bit operations is IEEE 754 and never panics.
- `Eq`, `Ne`, `Lt`, `Le`, `Gt` and `Ge` are built-in comparisons of primitive operands only. Overloaded comparisons are calls (§4.5.4).
- `Convert` kinds are those of [06](06-wasm-lowering-and-layout.md) §6.8.8: `IntWiden`, `IntWrap`, `IntSaturate`, `IntToFloat`, `FloatToInt`, `FloatPromote`, `FloatDemote`, `Bitcast`, `FnItemToFnPtr`, `ArrayToSlice`, `PtrToPtr`, `PtrToAddr`, `AddrToPtr` and `Transmute`. `FnItemToFnPtr` is the lowering of the adjustment `ReifyFnPointer` of [03](03-name-resolution-and-type-checking.md) §3.10.2 (§4.5.2), and [05](05-borrow-checker.md) §5.6.2 gives the region constraints of every kind. There is no closure kind, because a closure never coerces to a function pointer (§4.5.2). A call of a conversion function of D-TYPE-06 whose callee resolves to the `sai:core` item is lowered to `Convert` directly, with the kind of the table of [06](06-wasm-lowering-and-layout.md) §6.8.8.

### 4.2.5 Statements

```rust
pub struct Statement { pub source_info: SourceInfo, pub kind: StatementKind }

pub enum StatementKind {
    Assign(Box<(Place, Rvalue)>),
    StorageLive(Local),
    StorageDead(Local),
    SetDiscriminant { place: Place, variant: VariantIdx },
    FakeRead(FakeReadCause, Place),          // ForMatch | ForLet | ForGuard
    Nop,
    ProofObligation(ObligationId),           // Built to Analyzed, §4.12.1
    CheckPoint(CheckPointId),                // Built only, §4.12.3
}
```

`ProofObligation` marks the point at which a refinement or invariant goal is posed. It reads and writes nothing. `CheckPoint` marks a return point at which drop elaboration materializes the state-dependent invariant checks (§4.12.3). `mir_analyzed` replaces every `ProofObligation` by `Nop`, so Mono contains only the six statements of D-MIR-01. In `Assign(p, rv)`, `p` and every place read by `rv` are identical or disjoint ([06](06-wasm-lowering-and-layout.md) §6.8.3). Construction ensures it by evaluating a source that may overlap the destination into a temporary first.

### 4.2.6 Terminators

```rust
pub struct Terminator { pub source_info: SourceInfo, pub kind: TerminatorKind }

pub enum TerminatorKind {
    Goto { target: BasicBlock },
    SwitchInt { discr: Operand, targets: SwitchTargets },   // values ascending, then `otherwise`
    Return,
    Unreachable,
    Call { func: Operand, args: Vec<Operand>, dest: Place, target: Option<BasicBlock>,
           effects: EffectSet },                            // effects: empty before Analyzed
    Drop { place: Place, target: BasicBlock, kind: DropKind },          // ScopeExit | Replace
    Assert { cond: Operand, expected: bool, kind: Box<AssertKind>, target: BasicBlock },
    Await { future: Place, kind: AwaitKind, dest: Place, resume: BasicBlock, cancel: BasicBlock },
    CancelExit,
    Trap { kind: TrapKind },                                // BoundaryValue | Intrinsic
}

pub enum AssertKind {
    Overflow(OverflowOp),                 // Add | Sub | Mul | Div | Neg
    DivByZero,
    RemByZero,
    ShiftOverflow,
    BoundsCheck { index: Operand, len: Operand },
    Contract(ContractId),
    Refinement(ContractId),
    Invariant(ContractId),               // one invariant element
    UserAssert,
    Unwrap,
    ExplicitPanic,
}
```

- `Call.target` is `None` exactly when the callee returns `Never`. `dest` is then a dead temporary of type `Never`, and the block of the call has no successor. [05](05-borrow-checker.md) F12 counts successors of `Call` with `target: Some`.
- `Await` (Stage 5) moves `future` into the pending state of the current coroutine. `dest` is initialized only on the edge to `resume`. The blocks reachable from `cancel` contain only `Drop` terminators, `StorageDead` statements and `Goto`, and end in `CancelExit` ([05](05-borrow-checker.md) F4). `AwaitKind` is `Future` or `StreamNext` (impl/03 `await_kinds`). Both are removed by the async transform (§4.10.5).
- `Trap` is a plain trap ([spec/10](../spec/10-errors-and-panics.md) §10.7). `BoundaryValue` is not emitted by this chapter: the boundary-scalar checks belong to [06](06-wasm-lowering-and-layout.md) §6.6.7. `Intrinsic` is the lowering of the trap intrinsic of `sai:core` ([16](../spec/16-core-library.md)). impl/06 emits `unreachable` for both.
- MIR has no unwinding. `Call`, `Drop` and `Assert` have no cleanup edge (D-ERR-01).

**Panic kinds (N-E-54).** MIR building emits `Assert` for the kinds `Overflow`, `DivByZero`, `RemByZero`, `ShiftOverflow`, `BoundsCheck`, `Contract`, `Refinement`, `Invariant`, and `UserAssert` without a message. A panic whose message is an operand is a `Call` of the intrinsic `core.panic.panic(msg: Str)` with `target: None` ([spec/16](../spec/16-core-library.md) §16.3.4, [06](06-wasm-lowering-and-layout.md) §6.8.12.2): a source call of `panic`, an `assert` with a message (§4.9.4), and the library bodies of `unwrap`, `expect` and their relatives. `unreachable()` is a call of the intrinsic `core.panic.unreachable`. `Unwrap` and `ExplicitPanic` are therefore never the kind of an `Assert`. They remain in `AssertKind` because D-MIR-01 lists them, and nothing at run time depends on the kind: a panic site records a location and a message form only ([10](10-runtime.md) §10.5.2). The fixed text of a `UserAssert` without a message is `assertion failed: ` followed by the `cond_text` of the `assert` statement that `SourceInfo.hir` names ([02](02-frontend-ast-hir.md) §2.9.12).

### 4.2.7 Source information and scopes

```rust
pub struct SourceInfo { pub span: Span, pub hir: HirId, pub scope: SourceScope }

pub struct SourceScopeData {
    pub parent: Option<SourceScope>,
    pub span: Span,
    pub kind: ScopeKind,
    pub location: ScopeLocation,       // Static | Caller(Local); Caller only in Mono, §4.13.7
    pub inlined: Option<InlinedRecord>, // Some only for the outermost scope of an inlined body
}

pub struct InlinedRecord { pub callee: Instance, pub call_span: Span }   // [09] §9.4.5, N-F-47

pub enum ScopeKind {
    Normal,
    Predicate { obligation: Option<ObligationId>, clause: PredicateClause },
}

pub enum PredicateClause { Requires, Ensures, Invariant, Refinement, Old }
```

- Every statement and terminator has a `SourceInfo`. `hir` is the HIR node whose lowering produced it ([05](05-borrow-checker.md) F2). `span` is the panic location of [spec/10](../spec/10-errors-and-panics.md) §10.4.5 for an `Assert` and for a call of the panic intrinsic (§4.9.1), and the span of `hir` otherwise.
- A **predicate scope** contains the code of one runtime check, from its first statement to its `Assert`, or the evaluation of one `old(e)` snapshot, which has clause `Old` and no obligation (§4.12.1). [08](08-effect-and-capability-analysis.md) §8.3.1 skips the operations of predicate scopes, and [05](05-borrow-checker.md) maps a use of a moved parameter in an `Ensures` scope to `ENSURES_USES_MOVED_PARAMETER` (§4.12.1).
- `location` says where the panics of the scope are located. `Static` uses `SourceInfo.span`. `Caller(l)` uses the location held by the hidden parameter `l` of a forwarding instance (§4.13.7).
- Scopes are numbered in creation order. Scope 0 is the body, of kind `Normal`. The builder creates no other `Normal` scope, so the other scopes of a built body are its predicate scopes. Inlining appends the scopes of the inlined body and keeps their spans, because inlining never changes a location ([spec/10](../spec/10-errors-and-panics.md) §10.4.5). The outermost of them records the callee and the span of the call in `inlined`, which size attribution reads ([09](09-optimization-and-size.md) §9.4.5, N-F-47). Every other scope has `inlined = None`.

### 4.2.8 Obligations and coroutines

```rust
pub struct Obligation {
    pub kind: ObligationKind,
    pub hir: HirId,                    // the source point that the SFC evaluates (§4.12.2)
    pub location: Location,            // the ProofObligation statement or the check's first statement
    pub scope: Option<SourceScope>,    // the predicate scope of a runtime check, if any
}

pub enum ObligationKind {
    Requires(ContractId),              // at a call, in the caller
    Ensures(ContractId),               // at a return point
    Invariant { element: ContractId, point: InvariantPoint },   // Construction | Return | Dirty
    Refinement { index: u32, clause: ContractId },   // impl/03 refinement_obligations[index]: compile-time only
    BoundaryRequires(ContractId),      // a `requires` at a boundary entry, decided from lazy facts only, §4.12.5
}

pub struct CoroutineInfo {
    pub output: Ty,                    // the future's Output type
    pub awaits: Vec<Location>,         // the Await terminators, in block order
}
```

`Location` is `(BasicBlock, statement index)`, where the index equal to the number of statements denotes the terminator.

---

## 4.3 Instances and shims

### 4.3.1 Instances

```rust
pub struct Instance { pub def: InstanceDef, pub args: GenericArgs }

pub enum InstanceDef {
    Item(DefId),                       // a fn, method, closure, async block, clause or refinement predicate
    InvariantElement(DefId, u32),      // element `index` of the `invariant` declaration `DefId`
    Shim(ShimKind),
    Synthetic { producer: Producer, symbol: Symbol },   // a glue, runtime or harness body; `args` is empty, §4.13.9
    RuntimeIntrinsic(Symbol),          // `rt.copy`, …, impl/10 §10.1.2; it has no body
    CoreImport { module: Symbol, field: Symbol, sig: CoreSig },   // a core import called by a synthetic body, §4.13.9
}

pub struct CoreSig { pub params: Interned<[Ty]>, pub result: Ty }   // core scalar types; `()` for no result
```

`args` are the arguments of every generic parameter in scope of the definition, outermost first, as in the symbols of [06](06-wasm-lowering-and-layout.md) §6.10.2. A closure, `async` block or contract clause takes the arguments of its enclosing function. Monomorphization substitutes `args` into a copy of the definition's Optimized body (§4.13.5). `resolve_instance(def, args)` of [03](03-name-resolution-and-type-checking.md) §3.8.7 gives the instance of a trait method call. impl/04 adds the resolutions of §4.3.4 for the function traits.

### 4.3.2 Shim kinds

```rust
pub enum ShimKind {
    DropGlue(Ty),                                   // §4.13.8
    ClosureAdapter { closure: Ty, call: FnKind },   // a closure called through a stronger trait, §4.3.4
    FnTraitItem { callee: Ty, call: FnKind },       // a fn item or fn pointer called through a function trait
    StructuralClone(Ty), StructuralEq(Ty), StructuralCmp(Ty), StructuralHash(Ty),   // tuples, arrays; Clone of closures
    RefinedForward { refined: DefId, method: DefId },
    RefinedTryFrom(DefId),
    CheckedEntry(DefId),                            // spec/09 §9.3.3
    FuturePoll(DefId),                              // Future.poll of an anonymous future, §4.10.5
    BoundaryValidate(Ty),                           // §4.12.5, Stage 4
    BoundaryEntry(DefId),                           // §4.12.5, Stage 4
    ReifyLocation { item: DefId, site: SpanId },    // a forwarding function used as a function pointer, §4.13.7
    Flags { flags: DefId, op: FlagsOp },            // operators and provided functions of a flags type, impl/16 §16.9.4, Stage 4
    ViewDerive { adt: DefId, method: DefId },       // a derived trait method of a SAI view type, impl/16 §16.4.6, Stage 4
}
```

Shim bodies are built in phase `Mono` by `mir_shim(instance)` from the instance's types, except `FuturePoll`, whose generic body is produced by the async transform (§4.10.5). Shims are not borrow-checked and are not effect-checked. Their construction is correct by design, and the validator of `Mono` (§4.11.5) checks their well-formedness. Their effects are those of the calls they contain, for reachability ([08](08-effect-and-capability-analysis.md) §8.7.3).

### 4.3.3 Compiler-generated bodies

| Body | Built as | Semantics | Reference |
|---|---|---|---|
| refinement predicate of `R` with base `B` | an owner body (`BodySource::Item`) through the whole pipeline | one parameter `_1: borrow B`. `self` denotes `*_1`. The tail is the `Bool` result | [03](03-name-resolution-and-type-checking.md) §3.13.2 |
| element `k` of `invariant T { … }` | a body of the invariant owner (§4.2.1) | one parameter `_1: borrow T`; `self` is `*_1`. The tail is element `k` | [spec/09](../spec/09-contracts-and-refinements.md) §9.4.1 |
| contract clause `id` | a body of the declaring owner (§4.2.1) | one parameter `borrow T_i` for each parameter `p_i` of the function that the clause mentions, in parameter order, then `result: borrow R` for an `ensures` that mentions `result`, then one `T_k` per `old(e_k)` snapshot in source order. A mention of `p_i` reads `*` of its parameter. Unmentioned parameters are not passed, so the check code reads only what the clause mentions ([05](05-borrow-checker.md) F18) | §4.12.1 |
| derived method `m` of `Tr` for `T` | the bodies of the `DerivedImpl` owner, through the whole pipeline. Every statement has the span of the trait path in the `@derive` attribute ([08](08-effect-and-capability-analysis.md) §8.5.3) | the per-field code of [spec/05](../spec/05-types.md) §5.12, fields in declaration order, variants in declaration order | [03](03-name-resolution-and-type-checking.md) §3.8.6 |
| `RefinedTryFrom(R)` | shim | `fn(value: B) -> Option<R>`: call the predicate instance with `borrow value`. If it returns `true`, `_0 = Some(R(value))`. Otherwise `Drop(value)` and `_0 = None`. No obligation, empty effect set | [03](03-name-resolution-and-type-checking.md) §3.13.2, [spec/09](../spec/09-contracts-and-refinements.md) §9.2.3 |
| `RefinedForward { refined: R, method }` | shim | the method of `B`'s impl with every `R`, `borrow R` and `borrow mut R` argument projected to `Field(0)` (§4.5.3), and a `B` result of `Clone.clone` wrapped back into `R` without an obligation, because a clone of a value satisfies its predicate | [03](03-name-resolution-and-type-checking.md) §3.13.2 |
| `StructuralClone(T)`, `StructuralEq(T)`, `StructuralCmp(T)`, `StructuralHash(T)` | shim | the trait method applied element by element in ascending index order; for a closure type, `StructuralClone` clones the captures in capture order (spec/05 §5.5.3). `eq` stops at the first `false`. `cmp` stops at the first result that is not `Equal`. `hash` feeds every element in order | [spec/05](../spec/05-types.md) §5.11 |
| `FnTraitItem`, `ClosureAdapter` | shim | §4.3.4 | |
| `CheckedEntry(f)` | shim | the parameters of `f`. Evaluates every `requires` of `f` in declaration order as a runtime check with the location of §4.13.7, then calls `f` and returns its result | [spec/09](../spec/09-contracts-and-refinements.md) §9.3.3 |
| `FuturePoll(d)` | generic, from the async transform | §4.10.5 | [spec/11](../spec/11-async-and-concurrency.md) §11.3 |
| `BoundaryValidate(T)`, `BoundaryEntry(f)` | shim | §4.12.5 | [spec/09](../spec/09-contracts-and-refinements.md) §9.7.3 |
| `DropGlue(T)` | shim | §4.13.8 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.13.3 |
| `Flags { flags, op }` | shim | integer operations on the hidden field `bits`, masked to the flags of the type, with the bodies of [16](16-interfaces-and-wit.md) §16.9.6. No obligation, empty effect set | [spec/13](../spec/13-components-and-wit.md) §13.9.4 |
| `ViewDerive { adt, method }` | shim | the per-field code of a derived impl ([spec/05](../spec/05-types.md) §5.12) for the view type `adt`, fields and cases in WIT order ([16](16-interfaces-and-wit.md) §16.4.6) | [spec/13](../spec/13-components-and-wit.md) §13.7.3 |

The source information of a shim statement uses the `HirId` and the span of the declaration that the shim implements: the type declaration for drop glue, structural, validation and forwarding shims, the function's name for `CheckedEntry` and `BoundaryEntry`, the async body for `FuturePoll`. Which bodies are **transparent** for panic locations, so that their panics take the location of the user construct that invoked them, is defined in §4.13.7.

### 4.3.4 Calls through function traits

A call of a closure, function item or function pointer through a function trait has [03](03-name-resolution-and-type-checking.md) `CallKind::FnTrait { kind, method }`. Generic MIR represents it as a call of the internal method (`FnCall.call`, `FnMutCallMut.call_mut` or `FnOnceCallOnce.call_once`, [03](03-name-resolution-and-type-checking.md) N-E-26) with two arguments: the callee, as `borrow F`, `borrow mut F` or `F`, and the **tuple** of the arguments ([03](03-name-resolution-and-type-checking.md) N-E-28). This is the form that [05](05-borrow-checker.md) §5.6.4 checks. A direct call `c(a, b)` of a local closure `c` has the same form.

Monomorphization **untuples** these calls (N-E-43). When the instance of the callee is known, the call `call(r, t)` becomes a call with the arguments `r, Move(t.0), …, Move(t.(n-1))` (`Copy` for `Copy` elements), and the tuple temporary's storage ends after the call. Its target is:

| Callee type `F`, called through | Resolved callee |
|---|---|
| closure of kind `k`, through the trait of kind `k` | `Instance::Item(closure)`: the closure body, whose `_1` is the environment and whose `_2 …` are the parameters ([06](06-wasm-lowering-and-layout.md) §6.6.2) |
| closure of kind `Fn`, through `FnMut` | `ClosureAdapter { closure, call: FnMut }`: reborrows `*_1` shared and calls the body |
| closure of kind `Fn` or `FnMut`, through `FnOnce` | `ClosureAdapter { closure, call: FnOnce }`: borrows `_1` (shared or mutable), calls the body, then drops `_1` |
| function item `FnDef(f, ḡ)` | `Instance(f, ḡ)` directly with the untupled arguments. The receiver is a zero-sized value and is dropped by nothing |
| function pointer | an indirect call through `*r` or `r` with the untupled arguments |

`FnTraitItem` exists for the one case where an untupled call is impossible: a function item or pointer used as a value of a type parameter whose instance is itself called indirectly. It has the trait method's signature and performs the rows above. Untupling never changes the order of evaluation: the arguments were evaluated into the tuple in source order before the call.

---

## 4.4 Construction

### 4.4.1 Inputs, gating and invariant 4

`mir_built(owner)` reads `hir_owner(owner)`, `typeck(owner)`, `check_match(owner)`, `fn_sig`, `contract_clauses`, `invariant_of`, `needs_drop`, `is_copy`, `const_value`, the lang items, and the type layouts of [06](06-wasm-lowering-and-layout.md) §6.4 for the `transmute` check of §4.5.5. It applies gate 1 of §4.1.3. For an untainted owner, it first checks invariant 4 of [03](03-name-resolution-and-type-checking.md) §3.15.2: every expression whose meaning depends on types has its table entry, and no `call_kinds` entry is `Pending`. A missing entry is an internal compiler error that names the HIR node and the table. Invariants 1 to 3 are checked by the validator through the local types (§4.11.1). Invariant 5 holds by the gate.

Construction is a single recursive descent over the owner's HIR in evaluation order (spec/06 §6.23). It never consults another owner's MIR. The bodies of closures, `async` blocks and contract clauses are built by the same builder with a fresh `Body` each, after the owner's body, in the order of §4.2.1.

### 4.4.2 The builder

```rust
struct Builder<'tcx> {
    body: Body,
    current: BasicBlock,                     // the block being filled; `None` after a diverging expression
    scopes: Vec<DropScope>,                  // innermost last, §4.6
    loops: Vec<LoopScope>,                   // break and continue targets with their scope depths
    return_points: Vec<ReturnPointInfo>,     // for ensures and invariant checks, §4.12.1
    var_locals: ItemLocalMap<BindingId, Local>,
    old_snapshots: Vec<Local>,               // §4.12.1
}
```

The builder emits statements in the order of evaluation. A diverging expression (`return`, `break`, `continue`, a call returning `Never`) ends the current block, and the rest of the enclosing construct is not lowered. Code after it is in `TypeckResults::unreachable` and goes to `Body.unlowered` ([03](03-name-resolution-and-type-checking.md) N-E-29). Blocks are numbered in creation order. A block is created when it is first needed as a target, so the numbering follows the source order of the construct that needs it. The head and exit blocks of a loop are created when the loop is entered. The targets of a branch are created together, the one whose code comes first in the source first: the `then` branch before the `else` branch, the success edge of a pattern test or guard before its failure edge, and `bb_ok` before `bb_err` of `?` (§4.8.2). The failure branch of a `?` is lowered before the code that follows the `?`.

### 4.4.3 Body entry

1. `_0` is the return place, with the declared return type, the output type of the async body, or `Bool` for a predicate.
2. `_1 …` are the parameters in order, the receiver first. In a closure or `async` body, `_1` is the environment (§4.8.5) and the closure's parameters follow.
3. For each parameter whose pattern is not a single by-value binding, the builder destructures the parameter at entry as if by `let pat = _i` (spec/06 §6.21.1). A single binding `p` uses `_i` itself as `p`'s local.
4. Old snapshots are taken (§4.12.1), then the body block is lowered into `_0`, then the return point of the tail is lowered (§4.6.5).

---

## 4.5 Expressions, places and adjustments

### 4.5.1 Places, operands and temporaries

The builder lowers each expression with one of three functions, chosen by the context of the expression ([spec/06](../spec/06-expressions-and-statements.md) §6.2.2):

- `as_place(e)`, for a place context. It handles a local, a field or tuple field (`Field`), a dereference (`Deref`), a built-in index (§4.5.4), and the base of each of these. A value expression in a place context is evaluated into a fresh temporary `_t`, and the place is `_t` (spec/06 §6.24.1). §4.6.2 chooses the drop scope of the temporary.
- `as_operand(e)`, for a value context. A place expression becomes `Copy(p)` if `is_copy` holds for its type and `Move(p)` otherwise ([spec/07](../spec/07-ownership-and-borrowing.md) §7.3.2). A literal or a constant item becomes `Const` (§4.2.3). Any other expression is evaluated into a temporary, which is then moved.
- `into(dest, e)` evaluates `e` directly into the place `dest`. It is used for a `let` initializer with a single binding pattern, the tail of a block, the operand of `return` and `break`, and the arms of an `if` or `match` whose value is used. A construction is one `Assign(dest, Aggregate(..))` after all of its operands are evaluated (F13).

Temporaries are `Temp` locals. Each gets `StorageLive` where it is created and `StorageDead` at the end of its scope (F11). A temporary is never reused for another value. Two rules decide when a value is copied into a temporary (N-E-44):

1. In `Assign(p, rv)`, an operand of `rv` that reads a place overlapping `p`, other than `p` itself, is first copied or moved into a temporary (§4.2.5).
2. A place operand of an rvalue or a call is used directly only if every later operand of the same rvalue or call is a place or a constant. Otherwise the place operand is copied or moved into a temporary at its own position. A later operand could otherwise change the place before the operation reads it, and the observed value must be the one at the point of evaluation (spec/06 §6.23).

`d.value` of a distinct type is `Field(0)` of `d`. A refined value is accessed through `Field(0)` as well (§4.5.3).

### 4.5.2 Adjustments

The adjustments `adjustments[e]` of [03](03-name-resolution-and-type-checking.md) §3.10.2 are applied in order after `e` is lowered. Each produces a place or a temporary:

| `Adjust` | MIR |
|---|---|
| `NeverToAny` | nothing. `e` diverged, and the current block has ended (§4.4.2) |
| `Deref(Builtin)`, `Deref(Box)` | a `Deref` projection of the place |
| `Borrow(Not)` (receiver auto-ref) | `_t = Borrow(Shared, p)` |
| `Borrow(Mut)` (receiver auto-ref) | `_t = Borrow(TwoPhaseMut, p)` ([spec/07](../spec/07-ownership-and-borrowing.md) §7.10, F7) |
| `Reborrow` | `_t = Borrow(Mut, *p)` (F17). For the receiver of a method call ([03](03-name-resolution-and-type-checking.md) §3.9.4), `_t = Borrow(TwoPhaseMut, *p)` ([spec/07](../spec/07-ownership-and-borrowing.md) §7.10 case 1) |
| `MutToShared` | `_t = Borrow(Shared, *p)` (F17) |
| `SliceMutToSlice` | `_t = Borrow(Shared, *s)` with the type `Slice<T>` (F17) |
| `ReifyFnPointer` | `_t = Convert(FnItemToFnPtr, Const(FnItem(f, ḡ)), FnPtr(..))` |
| `RefinedToBase` | the place `p.0` (`Field(0)`), copied or moved as its type requires |
| `RefinedBorrowToBase` | `_t = Borrow(Shared, (*p).0)` |
| `BaseToRefined(o)` | §4.5.3 |

`TwoPhaseMut` is emitted only for the two cases that spec/07 §7.10 allows: the receiver of a method call, auto-referenced as `borrow mut` or reborrowed from an existing `borrow mut` (case 1), and the base borrow of an `IndexMut.index_mut` call (§4.5.4, case 2). An explicit `borrow mut e` is always `Mut`. Each `TwoPhaseMut` temporary is used exactly once, as a `Move` argument of the call that activates it (F7).

**Function values with preconditions (N-E-42).** A function item `f` can be used as a value instead of being called directly in three ways: it can be reified (`ReifyFnPointer`), it can appear as a `Const(FnItem(f, ḡ))` operand outside the callee position, or it can be called through a local of type `FnDef(f, ḡ)`. In each case the caller cannot evaluate the `requires` clauses of `f` at the call. Monomorphization therefore resolves every such use of an `f` that has `requires` clauses to `Shim(CheckedEntry(f))` with the same arguments. This covers the function pointer of a reification, row 4 of §4.3.4, and a call whose `func` is not a constant. `CheckedEntry(f)` checks the clauses at run time ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.3), and `mir_analyzed` reports `CONTRACT_RUNTIME_CHECK` once at the span of the operand (§4.11.3). A direct call, whose `func` is a `Const(FnItem(f, ḡ))`, keeps its `requires` obligations in the caller (§4.12.1). A closure never becomes a function pointer ([spec/05](../spec/05-types.md) §5.15.2, impl/03 §3.10.2), so impl/04 has no closure-to-pointer conversion. A reified function whose panics are located at their caller is wrapped by `ReifyLocation` (§4.13.7).

### 4.5.3 Refined types, distinct types and invariants

In MIR, a refined type `R` with base `B` and a distinct type `D` with base `B` are single-field ADTs whose field 0 is the base value. impl/06 lays them out as `B`. `D(e)` is `Aggregate(Adt { def: D, variant: 0, args }, [op])`.

**Refinement obligations.** `BaseToRefined(o)` lowers the operand `op` and then emits, for each refinement clause `c` of `R` in declaration order, `ProofObligation(k)` with `ObligationKind::Refinement { index: o, clause: c }` and `hir` set to the coerced expression. It then emits `_t = Aggregate(Adt { def: R, .. }, [op])`. No check code is built. A refinement goal that is not proven is the compile-time error `REFINEMENT_NOT_PROVEN` ([spec/09](../spec/09-contracts-and-refinements.md) §9.2.3), and the run-time conversion is the explicit `R.try_from` (`RefinedTryFrom`, §4.3.3).

**Invariants at construction.** Every construction of a type `T` that has an invariant is a check point ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.3 item 1): a record construction, a functional update, a variant construction of `T`, and `D(e)` for a distinct `T`. After the `Aggregate` into the destination `q`, the builder emits one runtime check per element `k` of the invariant, in element order. Each check is an obligation `Invariant { element: id_k, point: Construction }`, and its check code calls the element body with `borrow q` (§4.12.1). The location is the first token of the construction expression ([spec/10](../spec/10-errors-and-panics.md) §10.4.5).

### 4.5.4 Operators, calls and indexing

**Operators.** [03](03-name-resolution-and-type-checking.md) `op_res[e]` says whether an operator is built in or which method implements it.

- A built-in arithmetic, bitwise, shift or negation operator is lowered with the checks of §4.9. The operands are evaluated left to right, as operands.
- `a and b` and `a or b` are a `SwitchInt` on `a` that evaluates `b` only when it is needed ([spec/06](../spec/06-expressions-and-statements.md) §6.8.8). `not a` is `UnaryOp(Not, a)`.
- A built-in comparison of primitive operands is `BinaryOp(Eq | Ne | Lt | Le | Gt | Ge, a, b)`. Its operands are evaluated as operands. Comparison never moves or copies a non-primitive value, so the implicit borrows of spec/06 §6.8.7 make no observable difference here.
- An overloaded arithmetic or bitwise operator is a `Call` of the trait method with both operands by value ([spec/06](../spec/06-expressions-and-statements.md) §6.8.10). An overloaded comparison evaluates both operands as places (temporaries for value expressions), borrows them as `_ra = Borrow(Shared, pa)` and `_rb = Borrow(Shared, pb)`, and calls `PartialEq.eq` or `PartialOrd.lt`, `le`, `gt` or `ge` with `Move(_ra), Move(_rb)`. `a != b` is `UnaryOp(Not, _c)` of the result `_c` of `eq`.

**Calls.** For `f(a_1, …, a_n)`, the builder evaluates the callee, then the arguments in order as operands, then the `requires` checks of the callee (§4.12.1), and then emits the `Call`. For a method call, the receiver and its adjustment come first ([spec/06](../spec/06-expressions-and-statements.md) §6.23). The callee operand follows `call_kinds[e]`: `Direct(def, args)` is `Const(FnItem(def, args))`, `FnPtr` is a `Copy` of the callee place, and `FnTrait` has the form of §4.3.4. The destination is the `into` place or a fresh temporary. `target` is a fresh block, or `None` if the callee returns `Never`. A call of a function with `requires` clauses evaluates every argument into a temporary, so that the check code can borrow it.

**Built-in indexing** of `Array<T, N>`, `Slice<T>` and `SliceMut<T>` ([spec/06](../spec/06-expressions-and-statements.md) §6.7.1) evaluates the base place `b`, which is `*s` for a view `s`, and the index into a `USize` temporary `_i`. It then emits:

```text
_n = Len(b)
_ok = BinaryOp(Lt, Copy(_i), Copy(_n))
Assert(Move(_ok), true, BoundsCheck { index: Copy(_i), len: Copy(_n) }) -> next      // location: the `[` token
```

The resulting place is `b[_i]` (`Index(_i)`). The check is emitted even for a constant index, and impl/09 removes the checks that it proves.

**Trait-based indexing** ([spec/06](../spec/06-expressions-and-statements.md) §6.7.2) in a shared place context is `_r = Index.index(Move(_rb), i)` with `_rb = Borrow(Shared, b)`, and the place is `*_r`. In a mutable place context (an assignment target, the operand of `borrow mut`, a receiver auto-ref'd mutably) it is `_r = IndexMut.index_mut(Move(_rm), i)` with `_rm = Borrow(TwoPhaseMut, b)`. When the selected impl is one of the range `Index` intrinsics of [spec/16](../spec/16-core-library.md) §16.6.2, the builder calls instead the `slice` function of the base type with `borrow b` and the range, stores the resulting view in a temporary, and uses that temporary as the place (N-J-14). The panics of `slice` are those of the library, located at the call by §4.13.7.

### 4.5.5 Intrinsics and lang-item calls

A `Direct` call whose callee is one of the intrinsics of [spec/16](../spec/16-core-library.md) §16.3.4 is lowered as follows. The callee is recognized by its lang item, never by its name.

| Intrinsic | MIR |
|---|---|
| `core.panic.panic(m)` | `Call` with `target: None` (§4.2.6) |
| `core.panic.unreachable()` | `Call` with `target: None` |
| `Array.filled(x)` | `_t = Repeat(x, N)`, where `x` is evaluated once ([spec/06](../spec/06-expressions-and-statements.md) §6.18.5) |
| the numeric conversions of D-TYPE-06 | `Convert` with the kind of [06](06-wasm-lowering-and-layout.md) §6.8.8 |
| the range `Index` impls | §4.5.4 |
| `size_of`, `align_of`, `transmute`, `zeroed`, `forget`, `Ptr.read`, `PtrMut.write`, `PtrMut.drop_in_place` and the unaligned forms, `allocate`, `deallocate`, `reallocate` | a `Call` of the intrinsic instance, kept through every phase and lowered by §4.13.9 |

**The `transmute` size check.** For every call of `transmute<A, B>`, `mir_built` checks [spec/12](../spec/12-unsafe-and-ffi.md) §12.5.2 item 1 on the generic body: it takes `A` and `B` from `node_args`, normalized in the owner's `ParamEnv`. If either still mentions a type parameter, or if their sizes in the layouts of [06](06-wasm-lowering-and-layout.md) §6.4 for the target of the build configuration differ, it reports `TRANSMUTE_SIZE_MISMATCH` at the callee path, with a note that gives both sizes or names the parameter that a size depends on. The call is still lowered as in the table. The error fails the build, so impl/06 never lowers a `Transmute` between types of different sizes (§4.13.9). Because each generic body is checked once, the result never depends on how the body is instantiated (spec/12 N-I-32).

`String.new` and `Arena.region_new` are ordinary functions.

---

## 4.6 Scopes, temporaries and drops

### 4.6.1 The drop scope stack

The builder keeps a stack of **drop scopes**. Each scope records the locals that it owns, in declaration order, and the temporaries that it owns, in creation order:

```rust
struct DropScope {
    kind: DropScopeKind,                  // Block | Statement | Condition | Guard | Scrutinee | ForIterable
                                          // | Tail | LetElse | ArmBody | LoopBody | Predicate | Region
    hir: HirId,
    end_span: Span,                       // where the scope's ScopeExit drops are located (§4.6.3)
    owned: Vec<(Local, Ownership)>,       // Ownership: Var(decl order) | Temp(creation order)
    source_scope: SourceScope,
}
```

A block opens a `Block` scope. Its parameters belong to the outermost scope of the body, which is opened before every other scope and declares the parameters in parameter order ([spec/07](../spec/07-ownership-and-borrowing.md) §7.13.2 item 3). The return place `_0` belongs to no scope and is never dropped (item 4).

### 4.6.2 Temporary scopes

A temporary belongs to the innermost scope on the stack that is one of the eight temporary scopes of [spec/06](../spec/06-expressions-and-statements.md) §6.24.2. The scope kinds map to them as follows: `Statement` (item 1), `Condition` for the condition of `if` and of the `If` that `while` desugars to (item 2), `Guard` (item 3), `Scrutinee` for the scrutinee of a `match`, including the `match`es that `if let`, `while let` and `for` desugar to (item 4), `ForIterable` for the outer `match` of a `for` desugaring (item 5, impl/02 §2.9.5), `Tail` (item 6), `LetElse` (item 7), and `ArmBody` for an arm body or branch that is not a block (item 8).

- A `Tail` scope is opened around the tail expression of each block. Its temporaries are dropped after the tail value has been written to the block's destination and before the block's own locals.
- The `Scrutinee` scope of a `while let` is opened inside the `LoopBody` scope, so its temporaries end with each iteration. The `MatchSource` of impl/02 identifies these matches.
- **Lifetime extension.** When the initializer of a `let` or `var` is, after parentheses are removed, `borrow e` or `borrow mut e` and `e` is a value expression ([spec/06](../spec/06-expressions-and-statements.md) §6.24.3), the temporary of `e` is owned by the enclosing `Block` scope as a `Var`. Its declaration position is immediately before the `let`. It is therefore dropped with the block's locals, after every local declared later. No other temporary is extended.

### 4.6.3 Scheduling drops and storage

At every exit of a scope, whether the normal end, `break`, `continue`, `return` or `?`, the builder emits for each owned local, in exit order, `Drop(local, ScopeExit)` if `needs_drop` of its type is `Yes` or `Maybe`, and then `StorageDead(local)` unless the local is a parameter, which has no storage statements (F11). The **exit order** is:

1. the temporaries, in reverse creation order;
2. the locals, in reverse declaration order. The bindings of one pattern are declared in the source order of their names, so they are dropped in reverse source order ([spec/07](../spec/07-ownership-and-borrowing.md) §7.13.2 item 2).

An exit that leaves several scopes handles the innermost first. Every `Drop` is emitted unconditionally in Built MIR, even after a move. Drop elaboration removes, keeps or guards it from the classification of [05](05-borrow-checker.md) §5.12.1 (§4.11.2). A `Maybe` drop is resolved by monomorphization (§4.13.5).

**Spans of drops (N-E-44).** A `ScopeExit` drop has the span of the scope's `end_span`, which is used as the location of the panics of the drop glue (§4.13.7) ([spec/10](../spec/10-errors-and-panics.md) §10.4.5: "the scope end that dropped the value"):

| Scope | `end_span` |
|---|---|
| `Block`, `LoopBody` | the closing `}` of the block |
| the outermost scope of a body | the closing `}` of the body block, or the end of the body expression |
| `Statement`, `LetElse` | the last token of the statement |
| `Condition`, `Guard`, `Scrutinee`, `ForIterable`, `Tail`, `ArmBody` | the last token of the expression that the scope surrounds |

An early exit (`break`, `continue`, `return`, `?`) drops with the span of the jump instead: the `break`, `continue` or `return` keyword, or the `?` token. Its `SourceInfo.hir` is the jump expression.

**Storage of user variables.** A local bound by a `let` or `var` whose pattern is a single `move` binding gets `StorageLive` immediately before its initializer is lowered `into` it (§4.8.1). A local bound by any other pattern gets it when its binding is made, after the initializer has been evaluated as a place. The local joins its scope at its `StorageLive`, so every later exit of the scope ends it, including an exit from inside the initializer such as a `?`. Arm bindings get `StorageLive` when the arm's bindings are made (§4.7.3). A loop body is entered anew on each iteration, so its locals get `StorageLive` and `StorageDead` once per iteration.

### 4.6.4 Assignment

`p = e` ([spec/07](../spec/07-ownership-and-borrowing.md) §7.3.6, spec/06 §6.13.1) is lowered in this order: `e` into an operand (a temporary unless it is a constant or a place that §4.5.1 allows), then the place `p` with its index operands and bounds checks, then `Drop(p, Replace)` if `needs_drop` of `p`'s type is `Yes` or `Maybe`, then `Assign(p, Use(op))`. The `Replace` drop is immediately followed by the write of the same place (F3). It has the span of the `=` token. Drop elaboration removes it where `p` is uninitialized and guards it where `p` may be initialized. An assignment to a whole place `q` of a type with an invariant makes `q` clean (§4.12.3). An assignment to a place inside it makes `q` dirty.

Compound assignment is §4.8.3.

### 4.6.5 Return points

A **return point** is a `return e`, the tail of the body, or the failure branch of a `?` ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.4). Each return point is lowered in the order of [spec/06](../spec/06-expressions-and-statements.md) §6.23 and [spec/10](../spec/10-errors-and-panics.md) §10.3.3:

1. The value is evaluated into `_0`.
2. The `ensures` checks of the function, one per clause in declaration order (§4.12.1).
3. `CheckPoint(c)`. Drop elaboration replaces it by the invariant checks of the return point (§4.12.3), because which of them run depends on the initialization state that pass A computes. These checks are: for each parameter in parameter order, `*p` for `p: borrow mut T` and `p` for `p: T` while `p` is initialized; then the result if the return type is exactly `T` ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.3 item 2); then the dirty places rooted in parameters or captures.
4. The drops of every scope that is exited, innermost first (§4.6.3).
5. `Goto` to the single `Return` block of the body.

The return points of a body share one block that holds `Return`. A closure body has its own return points. It has no `ensures` checks, but item 3 applies to it when it is lexically inside the defining module of `T`. In an `async` body, item 5 completes the future (§4.10.5).

---

## 4.7 Pattern matching

### 4.7.1 Scrutinee

The scrutinee of a `match` is lowered with `as_place` inside a `Scrutinee` scope (§4.6.2). A place scrutinee is not moved by itself ([spec/06](../spec/06-expressions-and-statements.md) §6.15.3). After the scrutinee, the builder emits `FakeRead(ForMatch, p)` (F5), and `_sr = Borrow(Shared, p)` with origin `GuardScrutineeRef` if some arm has a guard (F6, §4.7.4). The scrutinee of `let … else` gets `FakeRead(ForLet, p)` instead (§4.8.1). An irrefutable destructuring `let` and a parameter pattern have no `FakeRead`.

### 4.7.2 Tests (N-E-45)

A `match` is lowered to a **decision list**: a decision tree in which each failure edge leads to the next alternative or arm. The arms are tried in source order. The pattern of an arm is tested by the matching procedure of [spec/06](../spec/06-expressions-and-statements.md) §6.21.7, left to right and depth first, and each test that fails jumps to the first test of the next alternative of an or-pattern, or of the next arm. Arms that `check_match` reports unreachable are not lowered: their HIR goes to `Body.unlowered`, and the failure edge skips them. The failure edge of the last arm goes to a block whose terminator is `Unreachable`, because exhaustiveness holds ([03](03-name-resolution-and-type-checking.md) §3.12).

Each test reads a subplace `q` of the scrutinee. Subplaces are formed by `Field`, `Downcast` after a successful variant test, `Deref` for a `borrow` pattern target or a scrutinee reached through a borrow, and `ConstIndex` or `Subslice` for slice patterns. For a view of type `Slice<T>` or `SliceMut<T>`, the elements are subplaces of `*s`.

| Pattern | Test on `q` |
|---|---|
| `_`, a binding, a tuple or record pattern | no test of its own; the subpatterns are tested on the fields, left to right in field order |
| variant `V` of an enum with more than one variant | `_d = Discriminant(q)`, `SwitchInt(Move(_d))` with the target for `V` and `otherwise` to the failure edge; the subpatterns are tested on `(q as V)` |
| integer, `Char`, `Byte` or `Bool` literal, or a const of these types | `SwitchInt(Copy(q))` with one value and `otherwise` |
| `Str` literal or const | `_r = Call PartialEq.eq` of the `sai:core` impl for `Str` (byte equality) with borrows of `q` and of the literal, then `SwitchInt(Move(_r))`. It is the only call a test makes, and no user impl is ever called ([spec/06](../spec/06-expressions-and-statements.md) N-C-33) |
| range `a..=b` (or `a..b`) | `BinaryOp(Ge, q, a)` and then `BinaryOp(Le, q, b)` (or `Lt`), each followed by a `SwitchInt` on its result |
| slice pattern with `n` elements and no rest | `_n = Len(q)`, `BinaryOp(Eq, _n, n)`; then the elements, which are `ConstIndex { offset: i, min_length: n, from_end: false }` |
| slice pattern with `k` elements before and `m` after a rest | `BinaryOp(Ge, _n, k + m)`; the prefix is `ConstIndex` from the start, the suffix is `ConstIndex { from_end: true }`, and a rest binding `..x` is `Subslice { from: k, to: m, from_end: true }` |

A single-variant enum, a record and a tuple have no discriminant test. Every test is a pure read, so a test that an earlier arm has already performed is repeated rather than shared. This costs code size only, and impl/04 requires no later pass to remove the repetition ([09](09-optimization-and-size.md) §9.4.3 folds the tests whose values it knows). The test code of an arm has the `SourceInfo` of its pattern.

### 4.7.3 Bindings and arm bodies

When the pattern of an arm, or one alternative of it, has matched and the arm has no guard, the **bind block** of that alternative makes the bindings in source order ([spec/06](../spec/06-expressions-and-statements.md) §6.21.5). For each binding it emits `StorageLive(x)` and then `x = Copy(q)` or `Move(q)` for a `move` binding, `x = Borrow(Shared, q)` for `borrow`, and `x = Borrow(Mut, q)` for `borrow mut`. The bindings are owned by the arm's scope, declared in source order (§4.6.3). Every alternative of an or-pattern binds the same locals, from its own subplaces, and its bind block jumps to the arm body, which is lowered once. The arm body is lowered with `into` the destination of the `match` and ends with the drops of the arm's scope. All arms join in one block after the `match`, which ends the `Scrutinee` scope.

### 4.7.4 Guards

A guarded arm follows [spec/06](../spec/06-expressions-and-statements.md) §6.15.2 and [05](05-borrow-checker.md) F6:

```text
// the pattern of the arm has matched
StorageLive(_gx)          _gx = Borrow(Shared, q_x)       // per by-value binding x: origin GuardBindingRef(x)
StorageLive(y)            y = Borrow(Shared | Mut, q_y)   // per borrow binding y
<the guard, in a Guard scope, with x read as *_gx>        -> _g
SwitchInt(Move(_g)) [0: bb_false, otherwise: bb_true]
bb_true:  FakeRead(ForGuard, _sr); StorageDead(_gx); StorageLive(x); x = Move(q_x) …; -> arm body
bb_false: FakeRead(ForGuard, _sr); StorageDead(_gx); StorageDead(y); -> the next arm
```

Both edges first end the `Guard` scope, like the branches of a `Condition` scope (§4.8.7), so the temporaries of the guard are dead before the `FakeRead`. A `borrow mut` binding is bound by `Borrow(Mut, q_y)` before the guard and is used as shared inside it. [03](03-name-resolution-and-type-checking.md) N-E-38 reports `GUARD_MOVE_OR_MUTATION` on the HIR of the guard, and [05](05-borrow-checker.md) §5.10.7 enforces the loan of `_sr` against a mutation of the scrutinee through any other path. The by-value bindings are made only on the true edge, after the `FakeRead`. A false guard continues with the next arm, not with the next alternative of an or-pattern ([spec/06](../spec/06-expressions-and-statements.md) §6.21.7 item 5). Each alternative of a guarded or-pattern has its own copy of the guard code, which binds from that alternative's subplaces.

---

## 4.8 Statements and control flow

### 4.8.1 `let`, `var` and `let … else`

A `let` or `var` whose pattern is a single `move` binding lowers its initializer with `into(x, e)` (value context, [spec/06](../spec/06-expressions-and-statements.md) §6.21.6). Any other pattern evaluates the initializer as a place in the `Statement` scope and binds with §4.7.3. Only the parts bound by `move` are moved. A `let` without an initializer only declares the local, and its first assignment initializes it.

`let P = e else B` ([02](02-frontend-ast-hir.md) §2.9.7) evaluates `e` as a place in a `LetElse` scope and emits `FakeRead(ForLet, p)`. The tests of `P` follow (§4.7.2). On failure, the builder drops the temporaries of the `LetElse` scope and lowers `B`, which diverges. On success, it makes the bindings, which belong to the enclosing block, and the `LetElse` temporaries are dropped at the end of the statement ([spec/06](../spec/06-expressions-and-statements.md) §6.24.2 item 7).

### 4.8.2 `?`

`e?` ([spec/10](../spec/10-errors-and-panics.md) §10.3.3, `TryKind` of impl/03) lowers to:

```text
_s = <e as an operand>                                  // moved, or copied if Copy
_d = Discriminant(_s)
SwitchInt(Move(_d)) [Ok: bb_ok, otherwise: bb_err]      // Some and None for Option
bb_ok:  v = Move((_s as Ok).0)                          // the value of `e?`; the rest of `_s` holds nothing to drop
bb_err: _x = Move((_s as Err).0)
        _0 = Aggregate(Adt { Result, Err, ret_args }, [Move(_x)])   // `None` for Option: no operand
        <return point, §4.6.5, located at the `?` token>
```

The variants are the lang items, by identity. `ret_args` are the arguments of the return type of the body that `return` leaves (the closure or `async` body, if any).

### 4.8.3 Compound assignment

`p op= e` ([spec/06](../spec/06-expressions-and-statements.md) §6.13.2) evaluates `e` into an operand `t` first, then the place `p'` once, with its index operands and bounds checks.

- **Built-in operator.** The read, the operation and its checks (§4.9) come next, and then the write: for `+`, `-` and `*`, `_r = CheckedBinaryOp(op, Copy(p'), t)`, then the `Assert`, then `p' = Move(_r.0)`. The other operators check first and then emit `p' = BinaryOp(op, Copy(p'), t)`. On a panic the place is unchanged, because the check precedes the write.
- **Overloaded operator, non-`Copy` place** (take-and-restore, N-C-13). It is one `Call` of the trait method with the arguments `Move(p'), t` and the destination `p'`, whose `SourceInfo.hir` is the `AssignOp` statement (F14). There is no `Replace` drop, because the old value is the argument.
- **Overloaded operator, `Copy` place.** `p' = Call Tr.op(Copy(p'), t)`.

The final write is the only write of the statement and has the `AssignOp` as its `hir` (F2).

### 4.8.4 Loops, `break` and `continue`

`loop` creates a **head** block and pushes a `LoopScope { label, head, exit, dest, depth }`, where `depth` is the depth of the scope stack outside the loop. The body is lowered in a `LoopBody` scope, and its end drops that scope and jumps to `head`. `while` and `for` are loops after the desugarings of [02](02-frontend-ast-hir.md) §2.9.2 to §2.9.6. The `if` of `while` has its condition in a `Condition` scope, and the outer `match` of `for` holds the iterator in a `ForIterable` scope.

- `break 'l v` evaluates `v` `into` the loop's `dest`, then drops every scope above the loop's `depth`, innermost first, and jumps to `exit`.
- `continue 'l` drops the same scopes and jumps to `head`.
- The head block is the target of every back edge of the loop. It is the **loop head** of the kill rule of [spec/09](../spec/09-contracts-and-refinements.md) §9.6.2 item 3, recorded per loop for the SFC (§4.12.2).

`break` and `continue` never cross a closure or `async` body, which has its own loop stack.

### 4.8.5 Closures and `async` blocks

A closure expression creates its value with `_c = Closure(def, ops)`. The operands are the captures of `closure_captures(def)` ([03](03-name-resolution-and-type-checking.md) §3.11) in capture order (F8). A by-borrow capture of `c` is `_k = Borrow(Shared | Mut, c)` passed as `Move(_k)`. A by-move capture is `Move(c)`, or `Copy(c)` for a `Copy` type. An `async` block builds its value the same way.

In the body of the closure, `_1` is the environment with origin `ClosureEnv`. Its type is `borrow Self` for kind `Fn`, `borrow mut Self` for `FnMut`, and `Self` for `FnOnce` and for every `async` block (F9). Capture `i` is the place `(*_1).i`, or `_1.i` for a `Self` environment. A by-borrow capture is used through `*((*_1).i)`. The closure's parameters are `_2 …`. A `Self` environment is a parameter: it belongs to the outermost scope and is dropped at every return point of the body, component by component where captures have been moved out (§4.11.2). A `return` inside a closure returns from the closure.

### 4.8.6 `region` blocks

`region r { body }` (Stage 2, [spec/07](../spec/07-ownership-and-borrowing.md) §7.15) lowers to the code of [05](05-borrow-checker.md) F10, in a scope of kind `Region`:

```text
StorageLive(_a)
_a = Call Arena.region_new() -> bb1                   // _a: origin ArenaStorage(hir of the block)
bb1: StorageLive(_r); _r = Borrow(Shared, _a)         // _r: the binding of `r`
     <body, into the destination of the block>
     StorageDead(_r); Drop(_a, ScopeExit) -> bb2       // at every exit of the block, including break, return and `?`
bb2: StorageDead(_a)
```

The block's value is written to its destination before the arena is dropped. The `Drop` of `_a` has the span of the closing `}` of the block.

### 4.8.7 Other expressions

- **`if c a else b`.** The condition is lowered in a `Condition` scope, whose temporaries are dropped before the branch runs. Then `SwitchInt` on the result with the target for `false` (`0`) first and `otherwise` for `true`. A branch that is not a block is an `ArmBody` scope.
- **Blocks and `unsafe` blocks.** A block opens a `Block` scope and lowers its statements in order and its tail with `into`. A statement expression's value is discarded: it is evaluated into a temporary of the `Statement` scope, which drops it ([spec/06](../spec/06-expressions-and-statements.md) §6.25.2). An `unsafe` block is lowered like a block.
- **Unit values.** A block without a tail writes `dest = const ()` after its statements, an `if` without `else` writes it on its false edge, and a `break` without a value writes `const ()` to the loop's `dest`. The body block of a loop is lowered `into` a fresh `()` temporary of the `LoopBody` scope, because its value is discarded.
- **Ranges** are `Aggregate(Adt { def: <range lang item>, .. }, [start, end])` with the ends evaluated left to right. `RangeFull` has no operands. Construction never panics.
- **f-strings** (Stage 3) are lowered to the call sequence of [spec/06](../spec/06-expressions-and-statements.md) §6.20.3. The hidden `buf` is a `Temp` of the enclosing `Statement` scope until the sequence ends, then it is moved into the destination. `borrow mut buf` is `Borrow(Mut, buf)`. Each interpolation is evaluated as a place, materialized into a temporary of the enclosing statement if it is a value expression, and passed as `Borrow(Shared, p)`. The specifier is a `Const` of type `FormatSpec`. Empty text segments produce no call.
- **String concatenation** is the `Add` call of §4.5.4.
- **`.await`** is §4.10.2.

---

## 4.9 Runtime checks of operations

### 4.9.1 Locations

The `SourceInfo.span` of an `Assert`, and of a `Call` of `core.panic.panic` or `core.panic.unreachable`, is the panic position of [spec/10](../spec/10-errors-and-panics.md) §10.4.5:

| Check | Span |
|---|---|
| an arithmetic, division, remainder or shift check | the operator token, including the `op=` token of a compound assignment |
| the negation check | the prefix `-` |
| a built-in bounds check | the `[` token |
| `assert` | the `assert` keyword |
| a source call of `panic` or `unreachable`, and a `requires` check at a direct call | the position of the call: the method name of a method call, otherwise the first token of the callee |
| an `ensures` check, and an invariant check at a return point | the `return` keyword, the first token of the tail expression, or the `?` |
| an invariant check after a construction | the first token of the construction |
| an invariant check of a dirty place before a whole use (§4.12.3) | the first token of the expression that uses the place |
| an invariant check of a borrowed place at a use or at the end of the liveness of its holder (§4.12.3) | the first token of the expression that makes that use of the holder |
| a `requires` check in `CheckedEntry(f)`, and every check of `BoundaryEntry(f)` | the name of `f` in its declaration ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.3, §9.7.3). For a library `f`, the checked entry is transparent and its checks take the caller location (§4.13.7) |
| a validation of an import result | the position of the import call |

A check inside a predicate (a clause, invariant element or refinement predicate body) is located in that body, with its own kind ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.5). The panics of transparent bodies (library code, derived impls, drop glue) are located at the user construct that invoked them, which §4.13.7 implements.

### 4.9.2 Integer arithmetic

For a built-in operation on an integer type `T` of width `w`, with operands `a` and `b` already evaluated, the builder emits:

| Operation | MIR |
|---|---|
| `a + b`, `a - b`, `a * b` | `_r = CheckedBinaryOp(op, a, b)`, `Assert(Copy(_r.1), false, Overflow(Add \| Sub \| Mul))`; the value is `Copy(_r.0)` |
| `a / b` | `_z = BinaryOp(Eq, b, 0)`, `Assert(Move(_z), false, DivByZero)`. For a signed `T`, also `_o = BinaryOp(BitAnd, BinaryOp(Eq, a, MIN), BinaryOp(Eq, b, -1))` (each inner operation into its own temporary) and `Assert(Move(_o), false, Overflow(Div))`. Then `BinaryOp(Div, a, b)` |
| `a % b` | `_z = BinaryOp(Eq, b, 0)`, `Assert(Move(_z), false, RemByZero)`, then `BinaryOp(Rem, a, b)`. `MIN % -1` is `0` and needs no check (N-C-8) |
| `a << s`, `a >> s` | `_ok = BinaryOp(Lt, s, w)` with `w` a `U32` constant (8 for `Byte`), `Assert(Move(_ok), true, ShiftOverflow)`, then `BinaryOp(Shl \| Shr, a, s)` |
| `-a` (signed) | `_m = BinaryOp(Eq, a, MIN)`, `Assert(Move(_m), false, Overflow(Neg))`, then `UnaryOp(Neg, a)` |
| `a +% b`, `a -% b`, `a *% b` | `BinaryOp(WrapAdd \| WrapSub \| WrapMul, a, b)`, with no check |
| `&`, `\|`, `^`, `~` | `BinaryOp(BitAnd \| BitOr \| BitXor, a, b)`, `UnaryOp(Not, a)`, with no check |

The checks are emitted in every profile and are never folded during construction. impl/09 removes a check only with a proof that it cannot fail ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.2). Float operations are `BinaryOp` and `UnaryOp` without checks. A `Bool` `not` is `UnaryOp(Not, a)`. An operator on a distinct type is a call of its derived impl (§4.5.4), whose operations are located at the user's operator (§4.13.7).

### 4.9.3 Messages

The message of an `Assert` is derived from its kind by [10](10-runtime.md) §10.5.2 and [06](06-wasm-lowering-and-layout.md) §6.8.12, with the texts of [spec/10](../spec/10-errors-and-panics.md) §10.4.4. For `Contract(id)`, `Invariant(id)` and `Refinement(id)`, the text is built from the contract id by [spec/09](../spec/09-contracts-and-refinements.md) §9.7.1 (`precondition failed: <clause>`, `postcondition failed: <clause>`, `invariant failed: <Type>: <element>`, `refinement failed: <Type>: <predicate>`), where the predicate text is its canonical formatting by [14](14-formatter.md). Whether a `Contract` id names a `requires` or an `ensures` clause is part of the id ([spec/09](../spec/09-contracts-and-refinements.md) §9.1.3). MIR stores no message text, only the kind and the span.

### 4.9.4 `assert`

`assert c` and `assert c, m` ([spec/06](../spec/06-expressions-and-statements.md) §6.20.2, [02](02-frontend-ast-hir.md) §2.9.12) are lowered in a `Statement` scope:

```text
_c = <c>
Assert(Move(_c), true, UserAssert) -> next                 // without a message; text from `cond_text`

_c = <c>                                                   // with a message
SwitchInt(Move(_c)) [0: bb_fail, otherwise: next]
bb_fail: _m = <m>                                          // evaluated only on failure
         _v = Copy(_m)                                     // m: Str
         _v = Call String.as_str(Borrow(Shared, _m))       // m: String, through a borrow temporary
         Call core.panic.panic(Move(_v)) -> None           // located at the `assert` keyword
```

The temporaries of a failed `assert` with a message are never dropped, because the panic does not return. A panic during the evaluation of `m` has its own kind and location. After a successful `assert`, the condition is a fact for the SFC (§4.12.2).

---

## 4.10 Async (Stage 5)

### 4.10.1 Bodies of `async fn` and `async` blocks

- An `async fn f` has an outer body and an async body ([05](05-borrow-checker.md) §5.15.2). The outer body is `_0 = Closure(b, [Move(_1), …, Move(_n)])` followed by `Return` (F8). It has no checks: the `requires` clauses of `f` are checked by the caller at the call that creates the future, like for any direct call ([spec/11](../spec/11-async-and-concurrency.md) N-I-3).
- The async body `b` is the body of the synthesized `async` block ([02](02-frontend-ast-hir.md) §2.9.15). Its `_1` is the future, of type `Future(f, ḡ)` or `Closure(b, ..)`, and its captures are `_1.i` (F9). Its `_0` has the output type.
- The `ensures` checks of `f` and the invariant checks of its return points are lowered at the return points of the async body (§4.6.5). They run when the body returns, before the future completes. A mention of a parameter `p_i` reads the capture `_1.i`.
- `_1` is never dropped as a whole in an async body, because the drop glue of the future type is the state machine of §4.10.5. At every return point and on every cancellation path, the builder drops the captures field by field: the parameters of an `async fn` in reverse parameter order, so that `self` is last, and the by-move captures of an `async` block in capture order ([spec/11](../spec/11-async-and-concurrency.md) §11.5.5, N-I-8).

### 4.10.2 `.await`

`e.await` evaluates `e` into an await slot `_s` (a local of origin `AwaitSlot` with the future's type) and emits the `Await` terminator:

```text
StorageLive(_s)
_s = <e as an operand>
Await { future: _s, kind: Future, dest: <destination>, resume: bb_r, cancel: bb_c }
bb_r: StorageDead(_s); …                       // the slot is empty again (spec/11 §11.3.3 step 4)
bb_c: <cancellation path, §4.10.3>
```

The `Await` of `for await` ([02](02-frontend-ast-hir.md) §2.9.6) has kind `StreamNext`. Its operand is `_b = Borrow(Mut, stream)`, where `stream` is the hidden `var` binding of the desugaring, and its slot has the type `borrow mut S`. The stream stays in its binding across iterations. The binding is owned by the arm scope of the outer `match`, so it is dropped after the locals of the loop body and when the loop is left ([spec/11](../spec/11-async-and-concurrency.md) §11.6.2). The location of an `Await` is the `.await` token.

### 4.10.3 Cancellation paths

The `cancel` successor of an `Await` holds the drops of [spec/11](../spec/11-async-and-concurrency.md) §11.5.5 step 2, except the drop of the occupied slot, which the async transform prepends (§4.10.5). The builder emits:

1. the pending temporaries of the statement that contains the `.await`, in reverse creation order, then the locals of each active scope, innermost scope first, in reverse declaration order (§4.6.3). Each is a `Drop(_, ScopeExit)` followed by `StorageDead`;
2. the captures of `_1`, field by field in the order of §4.10.1;
3. `CancelExit`.

The path has no checks and no `Return` ([05](05-borrow-checker.md) F4). Its drops have the span of the `.await` token. Drop elaboration treats it like any other path, so a value that has been moved out by the time of the suspension is not dropped.

### 4.10.4 Saved locals

A **saved local** of an async body is a local that must survive a suspension. It is computed from the Analyzed body:

- a local other than `_0`, `_1` and the await slots is saved if a `StorageLive` of it reaches some `Await` terminator without an intervening `StorageDead` (a forward may-analysis over the CFG);
- every `DropFlag` and `DirtyFlag` local is saved if the body has an `Await`.

Each await slot and each saved local becomes a field of the future (§4.10.6). The analysis is over storage, not over values, so a local is saved whenever its scope contains a suspension point. This is predictable from the source (P8), at the cost of a larger future when a value is dead before the suspension. impl/09 does not change the set, because the layout is computed before it runs.

### 4.10.5 The async transform (N-E-50)

The async transform runs as the first step of `mir_optimized(owner)` on each async body, on the generic Analyzed MIR and before the passes of impl/09. It produces two generic bodies, which replace the async body and are appended to the owner's bodies in Optimized MIR:

- the **poll body**, `unsafe fn poll(self: borrow mut Fut, cx: borrow mut Context) -> Poll<T>`, which is the body of `Shim(FuturePoll(b))`;
- the **future drop body**, `fn(v: Fut) -> ()`, which is the body of `DropGlue(Fut)` for the future type (§4.13.8).

The **state** is field 0 of the future, of type `U32`: `0` is *Unresumed*, `1` is *Returned*, and `2 + k` is *Suspended at await k*, where `k` is the index of the `Await` in `CoroutineInfo.awaits`. In the poll body:

1. Every place rooted at a saved local `_l` becomes `(*_1).f_l`, a place rooted at the old `_1` becomes `(*_1)` with the same projection, and the old `_0` becomes a fresh local `_ret`. The `StorageLive` and `StorageDead` statements of saved locals are removed. The context is `_2`.
2. The entry block reads the state and switches: `0` to the old entry block, `1` to a block that calls `core.panic.panic("future polled after completion")` ([spec/11](../spec/11-async-and-concurrency.md) §11.3.4), and `2 + k` to the poll block of await `k`.
3. Each `Await { future: _s, kind, dest, resume, cancel }` at index `k` becomes:

   ```text
   (*_1).slot_k = Move(_s)                                   // _s is the await slot, now a field
   -> poll_k
   poll_k: _p = Borrow(Mut, (*_1).slot_k); _c = Borrow(Mut, *_2)
           _r = Call Future.poll(Move(_p), Move(_c)) -> t_k   // Stream.poll_next on *(*_1).slot_k for StreamNext
   t_k:    _d = Discriminant(_r); SwitchInt(Move(_d)) [Ready: ready_k, otherwise: pending_k]
   ready_k: Drop((*_1).slot_k, ScopeExit) -> w_k              // only for kind Future: the slot is emptied
   w_k:    dest = Move((_r as Ready).0) -> resume
   pending_k: (*_1).0 = Const(2 + k); _0 = Aggregate(Poll, Pending, []); Return
   ```

4. Each `Return` of the async body becomes `(*_1).0 = Const(1)`, `_0 = Aggregate(Poll, Ready, [Move(_ret)])`, `Return`. The `ensures` checks have already run on the return path (§4.10.1).
5. The `cancel` paths are removed from the poll body. They become the future drop body: its entry switches on `v.0`. State `0` drops the captures in the order of §4.10.1. State `1` does nothing. State `2 + k` drops `v.slot_k` (for kind `Future`) and then runs the cancellation path of await `k`, with places rewritten as in item 1 (rooted at `v` instead of `(*_1)`) and `CancelExit` replaced by `Return`.

The calls to `Future.poll` and `Stream.poll_next` are calls of `unsafe` methods inside compiler-built code. Their obligations are met by construction: the slot is never moved after the first poll ([spec/11](../spec/11-async-and-concurrency.md) §11.5). The panic of item 2 is located at the name of the `async fn`, or at the `async` keyword of a block. A poll body of a library future is transparent (§4.13.7).

### 4.10.6 Future layout

`future_layout(b)` computes the fields of the anonymous future of the async body `b` from its Analyzed MIR, with generic types:

```rust
pub struct FutureLayout {
    pub body: DefId,
    pub fields: Vec<FutureField>,      // in field order
}

pub enum FutureField {
    State,                             // field 0, U32
    Capture { index: u32, ty: Ty },    // the captures of `_1`, in capture order
    Slot { await_index: u32, ty: Ty }, // one per Await, in await order
    Saved { local: Local, ty: Ty },    // the saved locals of §4.10.4, in local order
}
```

Every slot and every saved local has its own field. Fields are never overlapped between states (P8). impl/06 lays out a future type as a record of these fields with its usual record layout. The same query provides the edges of the `ASYNC_RECURSION` check (§4.13.4).

---

## 4.11 Phases and validators

### 4.11.1 The validator

Every query that produces bodies of a phase runs the **validator** of that phase on each body before returning it (N-E-58). The structural rules below are checked in every build of the compiler. The rules marked *(dataflow)* need a dataflow pass and are checked in debug builds of the compiler and in the test harness, as for the pass invariants of [09](09-optimization-and-size.md) §9.2.3. A violation is `INTERNAL_COMPILER_ERROR`, whose message names the body, the phase, the `Location` and the rule.

Rules of every phase:

1. **Types.** Every local type is well formed and contains no inference variable ([03](03-name-resolution-and-type-checking.md) §3.15.2 invariants 1 to 3). In `Assign(p, rv)`, the type of `rv` equals the type of `p` up to regions. A `Call` has as many arguments as its callee's signature has parameters, with equal types up to regions. The operand of `SwitchInt` is an integer, `Bool`, `Char` or `Byte`. The condition of an `Assert` is a `Bool`.
2. **Control flow.** Every block has a terminator, and every target is a block of the body. The successor counts are those of [05](05-borrow-checker.md) F12. The blocks reachable from a `cancel` edge satisfy §4.2.6. Exactly one block holds `Return`, unless the body has no return point.
3. **Places.** `Index` names a `USize` local. `Downcast(v)` is followed by a `Field` and applies to an enum place. `Deref` applies to a `borrow`, `borrow mut`, `Box`, raw pointer or view, and the `Deref` of a view is used only as §4.2.3 allows. An `Assign` satisfies the disjointness rule of §4.2.5.
4. **Storage** *(dataflow)*. On every path from `bb0`, every use of a local that has a storage statement in the body lies after a `StorageLive` and before a `StorageDead` of it. A local without storage statements is live in the whole body: `_0`, the parameters, the drop and dirty flags, and the locals that the async transform (§4.10.5), shims and synthetic bodies create without them.
5. **Source information.** Every `SourceInfo` names a scope of the body, and every scope has a parent with a smaller index, except scope 0. The predicate scopes are contiguous: the blocks of one check are entered only through its first statement and left only through the `target` of its `Assert`.

### 4.11.2 `Built` and drop elaboration

Additional rules of `Built`: no local has origin `DropFlag`, `DirtyFlag` or `CallerLocation`; every local other than `_0` and the parameters has storage statements (F11); every `Call.effects` is empty; every `Borrow` has a distinct `RegionVid`, numbered from 0 in statement order; every `TwoPhaseMut` temporary satisfies F7; the return points hold their `CheckPoint`s (§4.6.5); the bodies of an owner are in the order of §4.2.1.

`mir_drop_elaborated(owner)` copies the Built bodies and, for each body, reads `drop_classes(owner)` ([05](05-borrow-checker.md) §5.12.1):

```rust
pub struct DropClasses {
    /// For every Drop terminator of Built MIR, and every component of an Open drop.
    pub drops: FxHashMap<(BodyIndex, Location, MovePathKey), DropClass>,
    /// For every CheckPoint and every parameter of the body (in an async body, every capture
    /// `_1.i`): the class that a drop of that path at that point would have (§4.12.3).
    pub check_points: FxHashMap<(BodyIndex, Location, MovePathKey), DropClass>,
    /// The paths whose drop flags elaboration must maintain.
    pub flagged: Vec<(BodyIndex, MovePathKey)>,
}

pub enum DropClass { Dead, Static, Conditional, Open(Vec<(FieldPath, DropClass)>) }
```

`MovePathKey` is a local and a projection of `Field`, `Deref` and `ConstIndex` elements, as in [05](05-borrow-checker.md) §5.3. Elaboration then does, in this order:

1. **Drop flags.** For each flagged path it appends a `Bool` local of origin `DropFlag`, after all existing locals, in the order of `flagged`, so that no local index of the Built body changes (F16). The entry block sets the flag of a parameter path to `true` and every other flag to `false` ([05](05-borrow-checker.md) §5.12.2). Every initialization of the path or of an ancestor is followed by `flag = true`, and every move of the path or of an ancestor, and every `StorageDead` of its local, is followed by `flag = false`. When a terminator makes the initialization or the move (the destination or an argument of a `Call`), the assignment is the first statement of the terminator's target, which is a block of its own (§4.5.4).
2. **Drops.** A `Dead` drop becomes `Goto(target)`. A `Static` drop stays. A `Conditional` drop becomes `SwitchInt(Copy(flag)) [0: target, otherwise: d]`, where `d` holds the drop and then sets the flag to `false`. An `Open` drop becomes one drop per component, in the drop order of [spec/07](../spec/07-ownership-and-borrowing.md) §7.13.3, each elaborated by its own class. For an enum, the components are those of the active variant, so the elaboration switches on `Discriminant(p)` first. A drop of a component whose type has no drop glue (`needs_drop` is `No`) is not emitted. Every new terminator has the `SourceInfo` of the original drop. New blocks are appended after the existing ones in creation order, so no block index of the Built body changes either.
3. **Check points and dirty places.** Each `CheckPoint` is replaced by its checks, and the checks of dirty places are inserted (§4.12.3).
4. The obligations of the new checks are appended to `Body.obligations`, and the phase becomes `DropElaborated`.

A `Replace` drop is elaborated like a `ScopeExit` drop. After it, the following `Assign` reinitializes the place, so its flag becomes `true`.

Additional rules of `DropElaborated`: no `CheckPoint`; every flag local is assigned in `bb0` before any other use.

### 4.11.3 `Analyzed`

`mir_analyzed(owner)` applies gate 3 of §4.1.3, then copies the DropElaborated bodies and:

1. **Effects.** Copies the contribution of every `Call` from `call_effects(owner)` into `Call.effects` ([08](08-effect-and-capability-analysis.md) §8.3.2). A body that is not part of an effect body (a predicate) keeps empty sets.
2. **Decisions.** Reads `sfc_decisions(owner)`. For each obligation of a check that is `Proven`, removes the check: the block that holds the check's first statement is split before it and jumps to the `target` of the check's `Assert`, and the blocks of the check, which are no longer reachable, are removed. The predicate is therefore never evaluated ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.5). A `NotProven` check stays as it is.
3. **Snapshots.** Removes the code of every `old(e)` snapshot scope whose clause has no remaining check in the body, in the same way, and its local (N-E-57, [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2 rule 6).
4. **Obligations.** Replaces every `ProofObligation` by `Nop`. A `NotProven` refinement obligation is an error, which `sfc(owner)` reports. Its owner never reaches phase 11 (gate 2 of [01](01-architecture.md) §1.2.3), so no check code is needed for it.
5. **Lint.** Reports `CONTRACT_RUNTIME_CHECK` once for each `NotProven` obligation of kind `Requires`, `Ensures` or `Invariant`, at the span of its check, with the clause, its contract id and the reason ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.1). For every use of a function item `f` with `requires` clauses as a value (§4.5.2), it reports the lint once per clause of `f`, at the span of the operand. The checks of `BoundaryEntry` and `BoundaryValidate` (§4.12.5) are trust-boundary validation, which the lint never reports.

The lint is computed from the SFC results before optimization, so it does not depend on the profile. Additional rules of `Analyzed`: no `ProofObligation`; every `Call` of a `Fn` effect body has its effects.

### 4.11.4 `Optimized`

`mir_optimized(owner)` copies the Analyzed bodies, runs the async transform on each async body (§4.10.5), and then calls `hooks.optimize(body, OptPassSet::Generic)` on every body, which is `opt_generic` of [09](09-optimization-and-size.md) §9.4.2. Additional rules of `Optimized`: no `FakeRead`, `Nop`, `ProofObligation`, `CheckPoint` or `Await`; every region is `Erased`; `obligations` is empty; `coroutine` is `None`. The bodies of the owner are those of §4.2.1, with each async body replaced by its poll body and the future drop bodies appended in the order of the async bodies (§4.10.5).

### 4.11.5 `Mono`

Additional rules of `Mono`:

1. No type, constant or `GenericArgs` contains a type parameter, a `Param` constant or a projection.
2. Every `Drop` names a place whose type has drop glue. No `needs_drop` is `Maybe`.
3. Every call through a function trait is untupled (§4.3.4), except inside `FnTraitItem`.
4. Every callee is a `Const(Instance(i))`, where `i` has a body, is an intrinsic that §4.13.9 leaves to impl/06 or `core.panic.panic` or `core.panic.unreachable`, an `extern "wasm"` import, a component import, a `RuntimeIntrinsic` or a `CoreImport`; or it is a `Copy` of a function-pointer place.
5. `caller_location` is `Some` exactly for a forwarding instance (§4.13.7). Scopes with `Caller(l)` and operands `Copy(l)` of origin `CallerLocation` occur only in such an instance.
6. A `RuntimeIntrinsic` callee ([10](10-runtime.md) §10.1.2) occurs only in a body of `Synthetic { producer: Runtime }`, except that `rt.copy` and `rt.fill` also occur in bodies of `Synthetic { producer: Glue }` (§4.13.9). A `CoreImport` callee occurs only in a synthetic body, and its arguments and result have the types of its `sig`.

The same rules apply to shims and to the synthetic bodies of impl/07 and impl/10. They are not borrow-checked, so the validator is the only check of their construction.

---

## 4.12 Contracts, invariants and the SFC (Stage 3)

### 4.12.1 Obligations and check code

Every obligation of [spec/09](../spec/09-contracts-and-refinements.md) §9.1.1 that arises in a body is an `Obligation` of that body. They are numbered in creation order, which is the evaluation order of the builder, followed by the obligations that drop elaboration adds (§4.12.3).

| Kind | Created at | `hir` | Code in Built MIR |
|---|---|---|---|
| `Requires(id)` | a direct call of a function with `requires` clauses (§4.5.4), one per clause in declaration order, between the arguments and the `Call` | the call | a check |
| `Ensures(id)` | each return point (§4.6.5), one per clause in declaration order | the return point | a check |
| `Invariant { point: Construction }` | each construction of a type with an invariant (§4.5.3), one per element | the construction | a check |
| `Invariant { point: Return \| Dirty }` | drop elaboration (§4.12.3) | the return point or the whole use | a check, added by elaboration |
| `Refinement { index, clause }` | each refinement obligation of impl/03 at the point where the value exists: `BaseToRefined`, the argument of a refined distinct construction `D(e)`, and the value assigned by a compound assignment to a refined place ([spec/09](../spec/09-contracts-and-refinements.md) §9.2.5), one per clause | the converted expression | a `ProofObligation` statement |

The calls of a trait method use the clauses of the trait ([spec/09](../spec/09-contracts-and-refinements.md) §9.5.1). The return points of an impl method check the trait's `ensures` with the trait's contract ids, then its own, each in declaration order.

**Check code.** A check of obligation `o` for clause `c` is lowered in a new predicate scope `Predicate { obligation: Some(o), clause }` and a `Predicate` drop scope:

```text
StorageLive(_a1); _a1 = Borrow(Shared, x_1)        // per parameter that c mentions, in parameter order; hir: the mention
…
StorageLive(_ar); _ar = Borrow(Shared, _0)         // `result`, for an `ensures` that mentions it
StorageLive(_c)
_c = Call c(Move(_a1), …, Move(_ar), Copy(_old1), …) -> bb_a      // the clause body's instance (§4.3.3)
bb_a: StorageDead(_ar); …; StorageDead(_a1)
      Assert(Copy(_c), true, Contract(id)) -> bb_n                // located by §4.9.1
bb_n: StorageDead(_c)
```

`x_i` is the argument temporary of parameter `i` for a `requires` check (§4.5.4), and the parameter itself for an `ensures` check: `_i` in a function body, the capture `_1.i` in an async body (§4.10.1). For a receiver whose auto-ref is `TwoPhaseMut`, the check borrows the receiver temporary shared. This is a read during the reservation, before the activating `Call` (F7, [05](05-borrow-checker.md) §5.11). An invariant check of element `k` on a place `q` has the same form with one argument `Borrow(Shared, q)`, the element's body as the callee and the kind `Invariant(id_k)`. The check has no other temporaries, so nothing is dropped at its end ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.5). Because the clause is evaluated by a call of its own body, a panic inside it is located in the clause and keeps its own kind.

The check code exists in Built MIR whether or not the SFC later proves the goal ([05](05-borrow-checker.md) F18). Pass A therefore sees every read that a check makes. A read of a moved parameter in an `Ensures` scope is mapped to `ENSURES_USES_MOVED_PARAMETER` by [05](05-borrow-checker.md) N-B-41, and pass B checks the borrows of the checks like any other code.

**Old snapshots.** At the entry of a body with `ensures` clauses (§4.4.3), each `old(e_k)` of each clause, in clause order and then source order, is evaluated into a `Temp` local `_old_k` of its `Copy` type, in a scope `Predicate { obligation: None, clause: Old }`. The expression is lowered from the HIR of the clause, with the parameters as the places of the body (`_i`, or `_1.i` in an async body). `mir_analyzed` removes the snapshots that no check uses (§4.11.3).

### 4.12.2 The SFC interface

The SFC is specified over source constructs in evaluation order ([spec/09](../spec/09-contracts-and-refinements.md) §9.6.1). The reference implementation runs it on the typed HIR (N-E-46):

1. `obligation_table(owner)` lists, for each body of the owner in the order of §4.2.1, its obligations in the DropElaborated MIR: the kind, the `hir` and the scope.
2. `sfc(owner)` (crate `sai_sfc`) runs the forward pass of [spec/09](../spec/09-contracts-and-refinements.md) §9.6.3 once per body over the body's typed HIR, in the order of [spec/06](../spec/06-expressions-and-statements.md) §6.23, which is the order of the builder. The kill events, joins and loop heads of [spec/09](../spec/09-contracts-and-refinements.md) §9.6.2 are HIR constructs. The loop head of a `while` or `for` is the loop's own keyword, which is the head block of §4.8.4.
3. Each obligation is decided at the HIR point of its kind: after the arguments of the call `hir` for `Requires`, after the returned value of the return point `hir` for `Ensures` and `Invariant { point: Return }`, after the construction `hir`, before the use `hir` for `Invariant { point: Dirty }`, and after the evaluation of the converted expression for `Refinement`.
4. The result is an `SfcDecisions` value:

```rust
pub struct SfcDecisions { pub bodies: IndexVec<BodyIndex, IndexVec<ObligationId, SfcResult>> }

pub enum SfcResult { Proven, NotProven(NotProvenReason) }

pub enum NotProvenReason { Open, Limit(SfcLimit), Unsupported }     // spec/09 §9.6.1
```

`sfc(owner)` reports `REFINEMENT_NOT_PROVEN` for each `NotProven` refinement obligation, with the fixes of [spec/09](../spec/09-contracts-and-refinements.md) §9.2.3. `mir_analyzed` reports `CONTRACT_RUNTIME_CHECK` (§4.11.3).

[spec/09](../spec/09-contracts-and-refinements.md) §9.5.2 requires only that the SFC decide a generic function on its generic body before monomorphization, with its results complete at `Analyzed`. A HIR pass computes the result that spec/09 §9.6 defines: every fact and every version of spec/09 §9.6 is defined by source constructs, and spec/09 §9.6.9 allows any computation with identical results. The HIR pass does not depend on the shape of the MIR, so a change of construction cannot change an SFC result (P8). The MIR contributes only the set of obligations, including the dirty-place check points, which need the initialization and dirtiness states of MIR.

### 4.12.3 Return-point invariants and dirty places

Drop elaboration materializes the invariant checks that depend on MIR dataflow (N-E-47). It considers each type `T` with an invariant such that the body is lexically inside the module that defines `T` ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.3).

**Return points.** Each `CheckPoint` is replaced by these checks, in this order, each of them the element checks of §4.12.1 in element order:

1. For each parameter in parameter order: `*p` for `p: borrow mut T`; `p` for `p: T`, guarded by the class of `p` at the check point (§4.11.2): `Static` checks unconditionally, `Conditional` checks under the drop flag, `Dead` does not check, and `Open` checks under the conjunction of its components' classes, so that a parameter that is moved in part is not checked. A parameter of a `Copy` type is always initialized.
2. `_0`, if the return type is exactly `T`.
3. Each dirty place rooted in a parameter or a capture that item 1 has not checked, in the order of its move path, guarded by its dirty flag and by the class of its root.

**Dirty places.** A **dirty place** is a place of type `T` that has a dirtying event in the body. It is either a static place (a local with `Field`, `Deref`, `Downcast` and `ConstIndex` projections) or an **element set** `b[*]`, the elements of an array or view place `b` reached through an `Index` projection. The events, on MIR:

| Event | Effect on `q` |
|---|---|
| an `Assign` to a place strictly inside `q`; a `Borrow(Mut \| TwoPhaseMut, _)` of a place strictly inside `q` | dirty |
| a `Borrow(Mut, q)` whose HIR is not directly a call argument or an auto-ref'd receiver | dirty, after the check of the whole use |
| a check of `q` (proven or run); an `Assign` of a whole value to `q` | clean |
| `StorageDead` of the root, a move of `q` or of a place that contains `q`, except for a borrowed place | clean (the place no longer holds a value) |

For an element set, an event on any element applies to `b[*]`, and an assignment of a whole value to one element does not clean the set. Dirtiness is a forward may-analysis ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.3). A place whose state at some check point differs between the paths that reach it gets a `Bool` local of origin `DirtyFlag`, appended after the drop flags, which is `false` at entry, set at each dirtying event and cleared at each clean event. A check under a flag is `SwitchInt(Copy(flag))` around the check code.

**Whole uses.** Before each statement or terminator that makes a whole use of a dirty place `q`, elaboration inserts the checks of `q`: a `Copy` or `Move` operand of `q` or of a place that contains `q`, a `Borrow` of `q` or of such a place, an auto-ref'd receiver `q`, and a `Drop` of `q` when `T` implements `Drop`. The check makes `q` clean. The location is the first token of the expression that makes the use (§4.9.1), and the obligation's `hir` is that expression.

**Element sets.** A check of `b[*]` checks every element in ascending index order, with a loop that elaboration emits: `_j = 0`, `_n = Len(b)`, and while `Lt(_j, _n)` the checks of `b[_j]` followed by `_j = BinaryOp(WrapAdd, _j, 1)`. Elements that were not written are checked too. They satisfy the invariant, because a value of `T` is valid wherever it is observed as a whole, so the extra checks cannot fail and change only the running time. This is the element-set rule of [spec/09](../spec/09-contracts-and-refinements.md) §9.4.3 item 4.

**Borrowed places.** A dirty place whose move path contains a `Deref` of a `borrow mut` or `SliceMut` and whose root `r` is neither a parameter nor the environment of a closure (origin `UserVar` or `Temp`) is a borrowed place ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.3 item 4, check point 3; N-E-62). The end of `r` does not clean it, because its value lives in the place that the borrow refers to. Elaboration computes the use-liveness of [spec/07](../spec/07-ownership-and-borrowing.md) §7.9.4 on the MIR before it inserts any check, and inserts the checks of a borrowed place rooted in `r`, guarded by its dirty flag:

1. before each statement or terminator that moves `r` or assigns a whole value to `r`;
2. after each statement, and at the start of each successor of each terminator, at which `r` is use-live before and not use-live after. A critical edge is split for the check. The check of item 2 reads `r` only where `r` dies, so it does not move the end of any other check's liveness.

The location is the first token of the expression that makes the use of `r` (§4.9.1). For `v[i].f = x`, `r` is the temporary that holds the result of `index_mut`, and the check runs after the assignment.

The materialized checks are part of the DropElaborated MIR that pass B of [05](05-borrow-checker.md) checks. A check that reads a place while a conflicting loan is live, for example `*p` at a return point whose result holds a `borrow mut` into `*p`, is therefore a borrow error at the check's span.

### 4.12.4 Const initializers

A const initializer has no MIR ([03](03-name-resolution-and-type-checking.md) N-E-33). `const_value(def)` asks `sfc_const(def)` (crate `sai_sfc`) for the obligations of the initializer: its refinement obligations and its constructions of types with invariants, in evaluation order, each with its HIR point. The SFC decides them by the same forward pass over the initializer's HIR (§4.12.2). A `NotProven` refinement is `REFINEMENT_NOT_PROVEN`, and a `NotProven` invariant is `CONST_INVARIANT_NOT_PROVEN` ([spec/09](../spec/09-contracts-and-refinements.md) §9.4.5), both reported by `sfc_const`. No runtime check exists for a const.

### 4.12.5 The trust boundary (Stage 4)

A **boundary function** is an `export fn` (D-MOD-02), a function that implements a component export ([07](07-component-lowering.md)), or a function of a hybrid dependency in source form that implements a function of an interface provided within the build ([07](07-component-lowering.md) §7.11.10). It is entered from outside only through `Shim(BoundaryEntry(f))` (N-E-48), which has the signature of `f` and does, with every check located at the name of `f` in its declaration:

1. for each parameter in order whose type needs validation, `Call BoundaryValidate(T_i)(Borrow(Shared, _i), loc)`;
2. for each `requires` clause of `f` in declaration order whose result in `sfc_boundary_requires(f)` is `NotProven`, the check of §4.12.1;
3. `_0 = Call f(Move(_1), …)`, then `Return`.

`sfc_boundary_requires(f)` decides each clause at the entry of `f` from the lazy range, refinement and invariant facts of the parameters only ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.3 item 1.3). It reports nothing. A type **needs validation** if it has an invariant or a refinement, or one of its components does. If no parameter needs validation and no clause is `NotProven`, `BoundaryEntry(f)` is not created, and the boundary calls `f` directly. Otherwise `BoundaryEntry(f)` takes the place of `f` at the boundary: the core export of an `export fn` ([06](06-wasm-lowering-and-layout.md) §6.6.7, §6.10.5) calls it, from its scalar-checking wrapper when there is one, and the export glue of [07](07-component-lowering.md) §7.8.2 performs its step 2 by calling it after lifting.

`Shim(BoundaryValidate(T))` is `fn(v: borrow T, loc) -> ()`. It validates the components of `*v` depth first in declaration order (record fields, the payload of the active variant after a switch on the discriminant, tuple elements, list elements in index order, and the payloads of `Option` and `Result`), each by a call of `BoundaryValidate` of the component's type when that type needs validation. It then checks the invariant elements of `T` in order (`Invariant(id)`). For a refined type it validates the base and then evaluates each refinement clause as a check of kind `Refinement(id)`. `BoundaryValidate` is transparent: its panics take the location `loc` (§4.13.7).

The result of a call of an imported component function or an `extern "wasm"` function whose result type needs validation is validated in the caller, after the `Call`, by `Call BoundaryValidate(T)(Borrow(Shared, dest), Const(Location(call)))`. Monomorphization inserts this call (§4.13.5), because imports are never generic. The `ensures` of an import are not checked ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.1).

---

## 4.13 Monomorphization

### 4.13.1 Roots and the two collections

Monomorphization gives every instance its own Mono body. A generic function is copied and substituted once for each distinct `GenericArgs` with which it is reached, and two instances never share a body, even when their code would be identical: there is no polymorphization (N-E-52). The identical code folding of [09](09-optimization-and-size.md) §9.5 works on Wasm functions after lowering and does not change the set of instances.

There are two collections. They use the edges of §4.13.2 and differ in their roots and in what they check (N-E-59):

| Collection | Query | Phase | Roots | Checks |
|---|---|---|---|---|
| check | `mono_items(())` | 10 | the program roots | the limits of §4.13.3 and `ASYNC_RECURSION` (§4.13.4) |
| code generation | `mono_instances(root, target)` | 11 | the artifact roots | none |

Both are computed for the build configuration of the session ([01](01-architecture.md) §1.2): its target, its profile, whether tests are compiled, and its build key `K` ([08](08-effect-and-capability-analysis.md) §8.7.1). An artifact root is mapped to an instance as follows:

| Root of `build_roots(K)` ([08](08-effect-and-capability-analysis.md) §8.7.2) | Instance |
|---|---|
| `Main(f)`, `EnvForEntry(f)`, `Test(f)` | `Item(f)`; for a property `f` in a `TestHarness` key, its two harness entries instead (§4.13.10) |
| `ExportFn(f)` | `Shim(BoundaryEntry(f))` if it exists (§4.12.5), otherwise `Item(f)` |
| `Glue(key)` | the body of `glue_bodies(root, target)` that [07](07-component-lowering.md) produces for `key` |
| `PanicHook(f)` | none: the hook is an import and has no body |

**Program roots**, in this order:

1. The artifact roots other than `Glue` and `PanicHook`, mapped by the table, in the order of `build_roots(K)`.
2. In `DefId` order, every function of the package being built ([spec/05](../spec/05-types.md) §5.7.6) that has no generic parameter in scope: free functions and the methods of non-generic impls. Constants are not roots: they have no MIR ([03](03-name-resolution-and-type-checking.md) N-E-33), and a `ConstValue` contains no function value, so a constant reaches no instance.
3. For each boundary function `f` of the root package or of a provider of an interface provided within the build (§4.12.5), `Shim(BoundaryEntry(f))` if it exists, and `DropGlue(R)` of the result type `R` of `f` if `R` has drop glue. The same `DropGlue(R)` for `main`.
4. For each resource that the root package exports ([07](07-component-lowering.md) §7.6), and each resource provided within the build ([07](07-component-lowering.md) §7.6.5), `DropGlue(T)` of its representation type `T`.
5. When tests are compiled (`sai test`, `sai check --tests`), every test and property of the selected packages, in `DefId` order ([spec/14](../spec/14-testing.md) §14.7.1 item 3 for a harness). Each property is followed by its harness callees (§4.13.10).
6. When the root package declares an allocator package, its functions `add_region`, `allocate`, `deallocate` and `reallocate` ([10](10-runtime.md) §10.2.7), in this order.

**Artifact roots**: the mapped roots of `build_roots(K)` in their order; then every body of `glue_bodies(root, target)` that is not yet collected, in the order of that list; then `Synthetic { Runtime, cabi_realloc }` when the component exports `cabi_realloc` ([07](07-component-lowering.md) §7.5.1, N-D-49, [10](10-runtime.md) §10.3); then, when the panic mode of the build is `Report` ([06](06-wasm-lowering-and-layout.md) §6.8.12.1), `Synthetic { Runtime, __sai_panic }` and `Synthetic { Runtime, __sai_stack_overflow }`. The calls of these two functions are made by the lowering of impl/06, not by MIR ([09](09-optimization-and-size.md) §9.6.2 edge 4, §9.6.3), so they are roots.

The check collection treats glue, runtime and harness bodies as leaves: it never asks for them, because runtime bodies depend on the profile through the choice of allocator ([10](10-runtime.md) §10.2.1), glue depends on code generation, and harness bodies depend on the selection of tests. Items 3 and 4 and the import edges of §4.13.2 stand in for the instances that glue reaches, and item 5 for those that harness bodies reach. In a `sai test` session, `mono_items` uses the `WithTests` key and `mono_instances` the `TestHarness` key ([08](08-effect-and-capability-analysis.md) §8.7.1). Every root of `build_roots` of the second is a root of the first, and the instances that its harness entries reference are program roots of the first by item 5. A glue body MUST reference only instances of these edges, `BoundaryEntry(f)` or `f` of a boundary function, runtime functions, core imports and the runtime intrinsics of §4.11.5 rule 6. It MUST NOT depend on `mono_instances` (§4.1.4).

`mono_instances` scans the Mono bodies (`mir_mono`) of the instances it collects, with the edges of §4.13.2 read from the Mono form: every `Const(Instance(i))` operand, every `Drop`, and every call of drop glue added by §4.13.7. It applies no limit and reports nothing. Every instance that it reaches and that has a body MUST be in `mono_items`, except a synthetic instance and a `ReifyLocation` shim whose item is in `mono_items`. A violation is an internal compiler error, so the program checked by `sai check` is exactly the program that is compiled. The item-level dead-code elimination of [09](09-optimization-and-size.md) §9.6.2 then removes what the artifact does not reach. The **instantiation key** of an instance, which [13](13-cli.md) §13.6.15 uses for ordering, is its position in `mono_instances`.

```rust
pub struct MonoItems {
    pub instances: Vec<Instance>,                   // collection order
    pub discovered_by: Vec<Option<(u32, Span)>>,    // per instance: the discovering instance and the edge span; None for a root
}
```

### 4.13.2 Collection

Both collections use a FIFO worklist and a set of seen instances. The roots are pushed in order. Each instance that is popped is **scanned**, and each instance reached by an edge that is not yet seen is pushed. The collection order is the order in which instances are pushed. It depends only on the roots and on the bodies, so it is deterministic (P8).

The check collection scans the Analyzed body of the instance's definition, with the instance's `args` substituted into every type before the edge is resolved. The scan visits the blocks reachable from `bb0` in block order, and in each block the statements in order, then the terminator, with operands from left to right. The edges are:

| Edge | Source | Target |
|---|---|---|
| E1 | the `func` of a `Call` | a `Const(FnItem(d, ḡ))`: `resolve_instance(d, ḡ)`, with the function-trait resolutions of §4.3.4 and the future resolution of §4.13.5 step 2; a place of type `FnDef(d, ḡ)`: `CheckedEntry(d)` if `d` has `requires` clauses, otherwise as for a constant (§4.5.2); a function pointer: no edge. An allocation intrinsic gives the runtime function of §4.13.9, which is a leaf. A call of a component import adds `DropGlue(T)` for each parameter of type `T` that is passed by value and has drop glue, because the import glue releases it ([07](07-component-lowering.md) §7.7.2 step 4), and `BoundaryValidate(R)` if the result type `R` needs validation (§4.12.5) |
| E2 | a `Const(FnItem(d, ḡ))` operand that is not a `func` | `CheckedEntry(d)` if `d` has `requires` clauses, otherwise `resolve_instance(d, ḡ)` |
| E3 | a `Drop` | `DropGlue(T)` if the type `T` of the place has drop glue |
| E4 | `Rvalue::Closure(b, _)` of an async body `b` | `FuturePoll(b)` and `DropGlue(F)`, with `F` the future type, both with the instance's arguments |
| E5 | an `Await` | the `Future.poll` instance of the slot type, or `Stream.poll_next` for `StreamNext`; for kind `Future` also `DropGlue(S)` of the slot type `S` |

The scan of a shim follows its body: `FuturePoll(b)` scans the Analyzed body of `b`; `DropGlue(F)` of a future type has an E3 edge to `DropGlue` of each field type of the substituted `future_layout` that has drop glue, in field order; every other shim scans `mir_shim(instance)`, which is built from types alone and already contains `Instance` constants. An instance without a body is not collected and is not scanned: an `extern "wasm"` or component import, an intrinsic, a `RuntimeIntrinsic`, a `CoreImport`, and an instance whose owner is `Skipped` or `Errored` (§4.1.3 gate 4).

In debug builds of the compiler, `mono_instances` checks that every edge of a Mono body is also an edge of the check scan of the same instance, up to `ReifyLocation` (§4.13.7) and the replacement of allocation intrinsics by runtime calls (§4.13.9). Optimization can remove edges but never adds one.

### 4.13.3 Limits

The check collection applies two limits when an edge reaches an instance `i` that is not yet seen:

1. **Type depth** ([spec/05](../spec/05-types.md) §5.7.6). If some type in `i.args`, or the type of a `DropGlue`, `Structural*` or `BoundaryValidate` shim, has a nesting depth above 64, the error is `INSTANTIATION_DEPTH_EXCEEDED`. The depth of a primitive type is 1, and the depth of any other type is 1 plus the maximum depth of its components.
2. **Nesting depth** ([spec/01](../spec/01-introduction.md) §1.5.4, "depth of nested monomorphization"). The nesting depth of a root, and of an instance whose `args` are empty and whose shim kind names no type, is 0. The nesting depth of any other instance is 1 plus that of the instance that discovered it, which is the first instance in collection order with an edge to it. If it exceeds 1024, the error is `IMPLEMENTATION_LIMIT_EXCEEDED` with the limit `depth of nested monomorphization` and the value reached. 1024 is above the minimum of 128 that spec/01 requires.

In both cases `i` is not collected, so the collection stays finite. The error is reported at the span of the edge in the discovering instance: the callee operand of a call, which is the position of the call ([spec/10](../spec/10-errors-and-panics.md) §10.4.5), the operand of E2, or the span of the `Drop`, `Closure` or `Await`. For an edge inside a shim, which has no user span, the span is that of the nearest instance on the discovery chain that has one. A note lists the discovery chain from the root to the discovering instance, one instance per line, printed as in §4.14. Each `(definition of i, span)` pair is reported once. `mono_instances` never applies the limits, because its instances are a subset of `mono_items` (§4.13.1).

### 4.13.4 `ASYNC_RECURSION` (Stage 5)

The check of [spec/11](../spec/11-async-and-concurrency.md) §11.2.4 runs on the anonymous future types of the instances of `mono_items` (N-E-53):

1. **Nodes.** The future type `F` of each `FuturePoll(b)` instance with its arguments, in collection order.
2. **Edges.** `F → G` if the node `G` occurs **inline** in the type of a field of the substituted `future_layout(b)` (§4.10.6). A type occurs inline in itself, in the field types of a record, enum variant, distinct or refined type, in the elements of a tuple or array, and in the field types of a closure or anonymous future. The search does not enter a `borrow`, `borrow mut`, view, raw pointer or function pointer. `Box`, `Vec` and `String` hold their contents through a raw pointer, so they end the search without a special case.
3. **Components.** Tarjan's algorithm runs over the nodes in node order, with the successors of a node in field order. A strongly connected component is **cyclic** if it has more than one node or a node with an edge to itself.

Each cyclic component is one `ASYNC_RECURSION` error. It is reported at the first field of its first node, in field order, whose type leads into the component: the `.await` token for a `Slot`, the declaration of the local for a `Saved` field, and the parameter or the captured variable for a `Capture`. The message names the cycle: the shortest path from the first node back to itself through the component, with ties broken by field order, each future printed as `future of <path>` ([03](03-name-resolution-and-type-checking.md) §3.5.7). The fix is the one of spec/11, "box the recursive call", with the replacement `Box.new(…)` around the awaited operand of the reported slot.

### 4.13.5 Mono bodies

`mir_mono(instance)` starts from the **source body** of the instance:

| Instance | Source body |
|---|---|
| `Item(d)`, `InvariantElement(d, k)` | the Optimized body of `d` in `mir_optimized` of its owner (§4.2.1): the function's own body, a closure or clause body, or element `k` |
| `Shim(FuturePoll(b))`, `Shim(DropGlue(F))` of the future type `F` of `b` | the poll body and the future drop body of `b` (§4.10.5) |
| `Shim(DropGlue(T))` of an imported resource type `T` | the drop body of `T` in `glue_bodies` ([07](07-component-lowering.md) §7.6) |
| any other `Shim(k)` | `mir_shim(instance)` (§4.3.3, §4.13.8) |
| `Synthetic { Glue, s }` | the body with symbol `s` in `glue_bodies(root, target)` |
| `Synthetic { Runtime, s }` | `runtime_body(s)` ([10](10-runtime.md) §10.1.2) |
| `Synthetic { Harness, s }` | the entry body with symbol `s` in `harness_entries(())` (§4.13.10) |

It then applies these steps, in order:

1. **Substitution.** `args` are substituted into every local type, constant, `GenericArgs` and shim kind, and every projection is normalized ([03](03-name-resolution-and-type-checking.md) §3.8). `Param` constants become their values.
2. **Function constants** (N-E-60). Every `Const(FnItem(d, ḡ))` becomes `Const(Instance(i))`. As the `func` of a `Call`, `i` is `resolve_instance(d, ḡ)`. In every other position, it is `CheckedEntry(d)` with `ḡ` if `d` has `requires` clauses, and `resolve_instance(d, ḡ)` otherwise (§4.5.2). A `Call` whose `func` is a place of type `FnDef(d, ḡ)` gets the constant of the second rule as its `func`, and the place, which is zero-sized, is no longer read. `Future.poll` of the future type of an async body `b` resolves to `Shim(FuturePoll(b))` with the type's arguments, in addition to the resolutions of [03](03-name-resolution-and-type-checking.md) §3.8.7 and §4.3.4.
3. **Drops.** A `Drop` whose place has a type without drop glue becomes `Goto(target)`, which resolves every `Maybe` drop (§4.6.3).
4. **Untupling** of calls through function traits (§4.3.4).
5. **Intrinsics** (§4.13.9).
6. **Import results.** After each `Call` of a component import or an `extern "wasm"` function whose result type needs validation, the validation call of §4.12.5 is inserted, with the `SourceInfo` of the import call. Its location argument is added by step 7.
7. **Caller locations** (§4.13.7).
8. **Validation** by the rules of `Mono` (§4.11.5).

`mir_mono_unlocated(instance)` is the body after step 6. A synthetic body is Mono MIR from its producer: steps 1, 2, 4, 5 and 6 find nothing to do in it, and steps 3, 7 and 8 apply to it as to any other body. `instance_sig(instance)` is the list of the types of `_1 … _arg_count` and the type of `_0` of `mir_mono(instance)`. For an instance without a body, it is the declared signature with `args` substituted, `sig` for a `CoreImport`, plus the location parameter for `core.panic.panic` and `core.panic.unreachable` (§4.13.7). `mir_mono` does not optimize: impl/09 does that in `opt_mono` ([09](09-optimization-and-size.md) §9.3).

### 4.13.6 Static allocations

A constant that is not a scalar, a literal or an all-scalar aggregate (§4.2.3), and every location record of §4.13.7, is a **static allocation**:

```rust
pub struct StaticAlloc { pub ty: Ty, pub value: AllocValue }

pub enum AllocValue {
    Const(ConstValue),                                   // impl/03 §3.7.15, with a monomorphic type
    Location { file: Symbol, line: u32, column: u32 },   // the record of a span, of type (Str, U32, U32)
}
```

`AllocId` interns a `StaticAlloc` by content. The id is a handle only: nothing orders allocations by id, and the dump prints the content (§4.14). impl/06 computes the bytes and the relocations of an allocation from the layout of `ty` ([06](06-wasm-lowering-and-layout.md) §6.8.14), and orders data items by first reference, so the numbering of ids never reaches the artifact. A `ConstValue` contains no function value, so an allocation has no function-pointer relocation.

`SpanId` interns a `Span`. `Const(Location(s))` has the type `Ptr<(Str, U32, U32)>` and denotes the address of the allocation `Location { file, line, column }` of the start of `s`. `file` is the file string of [10](10-runtime.md) §10.5.2 (`<namespace>:<name>@<version>/<path>`), and `line` and `column` are 1-based, the column counted in UTF-8 bytes ([spec/10](../spec/10-errors-and-panics.md) §10.4.5), from the line index of [02](02-frontend-ast-hir.md). Equal records share one allocation.

### 4.13.7 Caller locations

A panic in a library body takes the position of the call that entered the library ([spec/10](../spec/10-errors-and-panics.md) §10.4.5). Mono MIR passes that position as an explicit hidden argument (N-E-49).

**Transparent bodies.** An instance is transparent if it is:

- an `Item` or `InvariantElement` whose definition belongs to `sai:core` or `sai:alloc`, including their closures, clauses and refinement predicates, and the `FuturePoll` of their async bodies;
- a method of a derived impl ([spec/10](../spec/10-errors-and-panics.md) §10.4.5, compiler-generated bodies);
- a shim of kind `DropGlue`, `BoundaryValidate`, `RefinedForward`, `RefinedTryFrom`, `Structural*`, `FnTraitItem` or `ClosureAdapter`;
- `CheckedEntry(f)` of an `f` that belongs to `sai:core` or `sai:alloc` ([spec/10](../spec/10-errors-and-panics.md) §10.4.5, compiler-generated bodies).

Any other `CheckedEntry(f)` and every `BoundaryEntry(f)` are not transparent: their checks are located at the name of `f` in its declaration ([spec/09](../spec/09-contracts-and-refinements.md) §9.3.3, §9.7.3). `ReifyLocation` is not transparent. User code, user `FuturePoll` bodies and synthetic bodies are not transparent.

**Forwarding.** `caller_location_forwarding(i)` is true if `i` is transparent and its `mir_mono_unlocated` body contains an `Assert`, a call of `core.panic.panic` or `core.panic.unreachable`, a call of a forwarding instance, or a `Drop` whose drop glue is forwarding. It is the least fixed point of this rule. The query computes it from `i` alone: it walks the transparent instances that `i` reaches by calls and drops, reading their `mir_mono_unlocated` bodies, and solves the rule over that subgraph. It never demands itself recursively, so a recursive library function is handled without a query cycle.

**Rewriting** (step 7 of §4.13.5):

1. A forwarding body gets a parameter `_l: Ptr<(Str, U32, U32)>` of origin `CallerLocation` after its last parameter. Its index is `arg_count + 1` of the unlocated body, `arg_count` grows by one, and the later locals are renumbered in order. `instance_sig` therefore ends with `_l`. `caller_location` is `Some(_l)`, and every source scope gets `location: Caller(_l)`.
2. Each `Call` of a forwarding instance, and of `core.panic.panic` and `core.panic.unreachable`, gets a final argument: `Copy(_l)` in a forwarding body, and otherwise `Const(Location(p))`, where `p` is the position of the call: the panic span of §4.9.1 for a panic call, the position of the HIR construct `hir` of [spec/10](../spec/10-errors-and-panics.md) §10.4.5 for a call in a body built from HIR (the method name or the first token of the callee, the operator token, the `[`, the `.await` token, and otherwise the first token of `hir`), and the `SourceInfo.span` set by the producer in a shim or synthetic body.
3. A `Drop` whose drop glue is forwarding becomes `_u = Call DropGlue(T)(Move(place), loc) -> b`, where `_u` is a fresh `()` temporary with `StorageLive` before the call, `b` is a new block with `StorageDead(_u)` and `Goto` to the old target, and `loc` is as in item 2 with the span of the `Drop`, which is the scope end, jump or `=` token of §4.6.3.
4. `Convert(FnItemToFnPtr, Const(Instance(i)), ty)` of a forwarding `i`, which is `Item(f)` or the `CheckedEntry(f)` of a library `f`, becomes the same conversion of `Shim(ReifyLocation { item: f, site })` with the arguments of `f`, where `site` is the span of the operand ([spec/10](../spec/10-errors-and-panics.md) N-F-81). `i` is `CheckedEntry(f)` exactly when `f` has `requires` clauses (§4.13.5 step 2), so `item` and the arguments determine it. The body of the shim calls `i` with its own parameters and `Const(Location(site))`, and returns the result. A function pointer therefore always has the declared signature.

A `CheckedEntry(f)` that is not transparent calls a forwarding `f` with the location of the name of `f` in its declaration, the location of its own checks. A transparent one passes its own `_l`. The rewriting is the same in every panic mode: in `TrapOnly` mode, impl/09 replaces the location arguments of panic calls by constants and removes their computations ([09](09-optimization-and-size.md) §9.4.9), and the inliner binds the parameter like any other ([09](09-optimization-and-size.md) §9.4.5).

### 4.13.8 Drop glue

`mir_shim(DropGlue(T))` builds `drop<T>(v: T) -> ()` ([06](06-wasm-lowering-and-layout.md) §6.1.1), which consumes `_1`. It drops by the table of [spec/07](../spec/07-ownership-and-borrowing.md) §7.13.3, each component by a `Drop` that step 3 of §4.13.5 removes if its type has no glue:

| `T` | Body |
|---|---|
| a type with a `Drop` impl | `_r = Borrow(Mut, _1)`, `Call Drop.drop(Move(_r))`, then the fields as for its kind |
| record, distinct or refined type | `Drop(_1.i)` for each field, in declaration order |
| enum | `_d = Discriminant(_1)`, `SwitchInt` to one block per variant with fields, which drops `(_1 as V).i` in declaration order |
| tuple | `Drop(_1.i)` in index order |
| `Array<U, N>` | the loop `_j = 0; while Lt(_j, N) { Drop(_1[_j]); _j = WrapAdd(_j, 1) }` |
| closure | `Drop(_1.i)` for each by-move capture, in capture order |
| anonymous future | the future drop body (§4.10.5) |
| imported resource | the body of [07](07-component-lowering.md) §7.6, which calls `[resource-drop]` |

`Box`, `Vec`, `String` and the other library owners are records with library `Drop` impls over raw pointers ([05](05-borrow-checker.md) N-B-32), so their glue is the first row. The statements have the span of the type declaration (§4.3.3). Drop glue is transparent, so glue that can panic, through a user `Drop` impl that panics or an invariant of a field, is forwarding and has the signature `drop<T>(v: T, loc)` (§4.13.7).

### 4.13.9 Intrinsics and synthetic bodies

Step 5 of §4.13.5 lowers the intrinsics of §4.5.5 (N-E-60) that are kept until monomorphization, with their substituted types:

| Intrinsic | Mono MIR |
|---|---|
| `size_of<T>()`, `align_of<T>()`, `zeroed<T>()` | kept as a call of the intrinsic instance; impl/06 lowers it from the layout of `T` ([06](06-wasm-lowering-and-layout.md) §6.8.13.4); `zeroed` writes a value whose bytes are all zero |
| `transmute<A, B>(x)` | `dest = Convert(Transmute, x, B)`. The sizes are equal, because §4.5.5 checks them on the generic body ([spec/12](../spec/12-unsafe-and-ffi.md) §12.5.2) |
| `forget<T>(x)` | `dest = Aggregate(Tuple, [])` and `Goto(target)`. `x` was moved into the call, so it is not dropped |
| `Ptr.read`, `PtrMut.read`, `read_unaligned` | `dest = Copy((*p))` for a `Copy` pointee, `Move((*p))` otherwise |
| `PtrMut.write`, `write_unaligned` | `(*p) = Move(v)`, with no `Replace` drop |
| `PtrMut.drop_in_place` | `Drop((*p), ScopeExit)`, removed by step 3 if the pointee has no glue |
| `allocate(size, align)` | `_a = Call __sai_alloc(size, align)`, then `dest = Convert(AddrToPtr, Move(_a), PtrMut<Byte>)` |
| `deallocate(p, size, align)` | `_a = Convert(PtrToAddr, p, USize)`, then `Call __sai_free(Move(_a), size, align)` |
| `reallocate(p, size, align, new_size)` | `PtrToAddr` of `p`, `Call __sai_realloc(_a, size, align, new_size)`, then `AddrToPtr` of the result |

The callees of the last three rows are `Instance(Synthetic { Runtime, symbol })` with the symbols of [10](10-runtime.md) §10.2.2. The unaligned forms produce the same MIR, because the alignment of a Wasm memory access is a hint. The `requires` clause of `allocate` has been checked at the call (§4.12.1).

**Raw memory in synthetic bodies.** The operations that impl/07 and impl/10 use to build glue and runtime bodies are ordinary Mono MIR: places through `Deref` of `Ptr` and `PtrMut`, `RawPtr`, `Convert` with `AddrToPtr`, `PtrToAddr` and `PtrToPtr`, calls of `__sai_alloc`, `__sai_realloc` and `__sai_free`, and the runtime intrinsics of [10](10-runtime.md) §10.1.2, of which glue uses only `rt.copy` and `rt.fill` (§4.11.5 rule 6). A synthetic body has no type parameter, and its source spans are those that its producer sets.

**Core imports** (N-E-61). A synthetic body calls a core Wasm import that has no SAI declaration, such as the core function of an imported WIT function in import glue ([07](07-component-lowering.md) §7.7.2) or a standard-error function in the runtime ([10](10-runtime.md) §10.5.4), as `Const(Instance(CoreImport { module, field, sig }))`. Every type of `sig` is `I32`, `I64`, `F32`, `F64` or `USize`, the SAI types of the core value types `i32`, `i64`, `f32`, `f64` and `ptr`, and the result is `()` when the core function has none. A `CoreImport` has no body and is never collected. impl/06 calls the import named `(module, field)` ([06](06-wasm-lowering-and-layout.md) §6.8.9). A function of an `extern "wasm"` block, such as a `@panic_hook` import, is called as `Item(d)` like in user code.

### 4.13.10 Test harness entries

In a `sai test` session, `harness_entries(())` builds the bodies through which the runner of [13](13-cli.md) §13.6.6.3 drives the properties, and the manifest of [13](13-cli.md) §13.6.6.2 item 4 (N-E-67). It reads:
- `test_selection(())` and the `--seed` value of `test_options(())` ([13](13-cli.md) §13.6.6.1);
- the `@cases` and `@seed` arguments of each selected property, which [02](02-frontend-ast-hir.md) §2.10.4 validates;
- the parameters of each selected property, with their types;
- trait resolution.

In every other session the selection is empty, and so is the result.

```rust
pub struct HarnessEntries {
    pub entries: Vec<HarnessEntry>,            // by index `k` (impl/13 §13.6.6.1)
}

pub struct HarnessEntry {
    pub owner: OwnerId,
    pub property: Option<PropertyPlan>,        // None for a `test`
}

pub struct PropertyPlan {
    pub seed: u64,                             // S (spec/14 §14.5.2)
    pub cases: u32,                            // N (spec/14 §14.5.1)
    pub case_entry: Arc<Body>,                 // `__sai_harness_case$<k>`
    pub counterexample_entry: Arc<Body>,       // `__sai_harness_counterexample$<k>`
}
```

**Plan.** The plan of a property comes from its declaration and the session options.
- `S` is the `@seed` argument; otherwise the `--seed` value; otherwise the FNV-1a 64 hash of the test path text `path` of the `CollectedTest`, computed by the compiler ([spec/14](../spec/14-testing.md) §14.5.2).
- `N` is the `@cases` argument, or 100 ([spec/14](../spec/14-testing.md) §14.5.1).
- A `test` has no entry body: its export is `Item(t)` itself ([13](13-cli.md) §13.6.6.2 item 3).

**Bodies.** For a property `t` with the index `k` and the parameters `p_1: T_1, …, p_n: T_n`, the two entries are Mono bodies with the source `Synthetic { producer: Harness, symbol }`. They are built as the glue and runtime bodies are (§4.13.9): they have no type parameter, they are not borrow-checked or effect-checked, and the validator of §4.11.5 checks them. Their effects are modeled by the entry of [08](08-effect-and-capability-analysis.md) §8.7.12. `S` and `N` are constants of the bodies. All arithmetic in them either wraps or stays in range, so they contain no `Assert`.

```text
__sai_harness_case$<k>(i: U32) -> U32:
    s := mix(S +% (U64(i) +% 1) *% 0x9E37_79B9_7F4A_7C15)   // output of SplitMix64 step i + 1 from S
    z := U32((U64(i) + 1) * 100 / U64(N))                   // (i + 1) * 100 < 2^39, and N >= 1
    gen := Gen.from_seed(s, z)
    p_1 := T_1.arbitrary(borrow mut gen)                    // span: the type of p_1
    ...
    p_n := T_n.arbitrary(borrow mut gen)                    // span: the type of p_n
    call CoreImport { sai-harness, generated, () -> () }
    r := Item(t)(move p_1, ..., move p_n)                   // span: the name string of t
    Drop(gen)
    return 0 if the result type of t is Bool and r is false, otherwise 1

__sai_harness_counterexample$<k>(i: U32):
    s, z, gen and p_1 ... p_n exactly as above, without the call of `generated`
    buf := FixedBuf<4096>.new()
    for j = 1 ... n:
        write_str(borrow mut buf, c_j)                       // c_1 = "(<name_1>: ", c_j = ", <name_j>: " for j > 1
        if T_j implements Format:
            Format.format(borrow p_j, borrow mut buf, FormatSpec.Debug)   // span: the type of p_j
        else:
            write_str(borrow mut buf, "<T_j>")               // the CanonType form, spec/04 §4.9.1
    write_str(borrow mut buf, if n = 0 then "()" else ")")
    text := FixedBuf.as_str(borrow buf)
    call CoreImport { sai-harness, counterexample, (USize, USize, I32) -> () }(
        Convert(PtrToAddr, Str.as_ptr(text), USize), Str.len(text), 1 if FixedBuf.is_truncated(borrow buf) else 0)
    Drop(buf); Drop(p_n); ...; Drop(p_1); Drop(gen)

mix(x):                                                      // the output function of SplitMix64, spec/16 §16.17
    x := (x ^ (x >> 30)) *% 0xBF58_476D_1CE4_E5B9
    x := (x ^ (x >> 27)) *% 0x94D0_49BB_1331_11EB
    return x ^ (x >> 31)
```

- **Seeds.** After `j` steps, the SplitMix64 state is `S +% j *% 0x9E37_79B9_7F4A_7C15`, and the output of step `j` is `mix` of that state. `s` is therefore the case seed `s_i` of [spec/14](../spec/14-testing.md) §14.5.3 item 1, computed without a loop. `z` is the size `z_i` of item 2.
- **Values.** Both entries generate the values from the same generator with the same calls, so they produce the values of case `i` twice ([spec/14](../spec/14-testing.md) §14.5.6 item 2). `<name_j>` is the name of `p_j`, or `_` for a parameter without a name. `write_str` is the method of `impl Writer for FixedBuf<4096>`.
- **Drops.** The case entry moves every value into `Item(t)`, which drops the values that the body does not move. The generator is dropped last ([spec/14](../spec/14-testing.md) §14.5.5 item 1 step 4).
- **Locations.** Every statement for `p_j` has the span of the type of `p_j`. Every other statement has the span of the property's name string. A transparent `Arbitrary` impl that panics therefore reports the position of the parameter's type ([spec/14](../spec/14-testing.md) §14.5.4 item 3), through the location argument of §4.13.7 rewriting item 2.

**Callees.** The **harness callees** of a property `t` are the instances that its two entries reference. They depend only on `t`, so they are defined in every session that compiles tests, including one without entries. Each is resolved as an E1 edge of §4.13.2 resolves it, and they are listed in this order:
1. `Gen.from_seed`;
2. the `Arbitrary.arbitrary` instance of each `T_j`, in parameter order;
3. `Item(t)`;
4. `FixedBuf<4096>.new`, the `write_str` of `impl Writer for FixedBuf<4096>`, `FixedBuf<4096>.as_str`, `FixedBuf<4096>.is_truncated`, `Str.as_ptr` and `Str.len`;
5. the `Format.format` instance of each `T_j` that implements `Format`, with the method argument `W := FixedBuf<4096>`, in parameter order;
6. `DropGlue(X)` for each `X` among `Gen`, `FixedBuf<4096>` and `T_1 … T_n`, in this order, if `X` has drop glue.

They follow `Item(t)` in the program roots of §4.13.1 item 5. This holds whenever tests are compiled, so `sai check --tests` and `sai test` apply the limits of §4.13.3 to the same instances. `mono_instances` scans the entries like other synthetic bodies, and every instance that they reach is in `mono_items`.

---

## 4.14 The text format

### 4.14.1 Use

The text format is printed by `sai inspect mir` ([13](13-cli.md) §13.6.15), in the MIR goldens of [15](15-conformance-and-testing.md) §15.4.2, and in internal compiler errors (§4.11.1). It is a notation of the toolchain, not stable across compiler versions, and not parsed back (N-E-55). It is deterministic: equal MIR prints as equal text, because every element is printed from its content and in the order of the data, never from an address or a hash.

The `--phase` names are `built`, `drop-elaborated`, `analyzed`, `optimized`, `mono` and `final`. The first four print the bodies of the owners, generic and in the order of §4.2.1. `mono` prints `mir_mono` and `final` prints the bodies after `opt_mono`, which are what impl/06 lowers. Both print one body per instance, in the order of the instantiation key (§4.13.1). `final` is the default.

### 4.14.2 Grammar

```text
body        ::= 'fn' name '(' params? ')' '->' type '[' phase ']' '{' NL decl* scope* block* '}' NL
name        ::= instance | owner-path body-tag?
body-tag    ::= '{closure ' LINE ':' COL '}' | '{async ' LINE ':' COL '}' | '{requires ' INT '}'
              | '{ensures ' INT '}' | '{invariant ' INT '}' | '{derived ' IDENT '}' | '{poll}' | '{drop}'
params      ::= local ':' type ( ',' local ':' type )*
decl        ::= ( 'let' | 'var' ) local ':' type ( '//' origin )? NL
origin      ::= 'return' | 'param ' INT ( ' ' IDENT )? | IDENT | 'temp' | 'arena' | 'env' | 'scrutinee-ref'
              | 'guard-ref ' IDENT | 'drop-flag' | 'dirty-flag' | 'await-slot' | 'caller-location'
scope       ::= 'scope ' INT ( ' (parent ' INT ')' )? ( ' predicate ' clause ( ' obligation ' INT )? )?
                ( ' caller ' local )? NL
block       ::= 'bb' INT ': {' NL ( stmt NL )* term NL '}' NL
stmt        ::= place ' = ' rvalue | 'StorageLive(' local ')' | 'StorageDead(' local ')'
              | 'discriminant(' place ') = ' INT | 'FakeRead(' cause ', ' place ')' | 'nop'
              | 'obligation ' INT | 'checkpoint ' INT
term        ::= 'goto -> ' bb | 'switchInt(' operand ') -> [' ( INT ': ' bb ', ' )* 'otherwise: ' bb ']'
              | 'return' | 'unreachable' | 'cancel_exit' | 'trap(' ( 'boundary' | 'intrinsic' ) ')'
              | place ' = ' operand '(' operands? ') -> ' ( bb | 'never' ) ( ' effects ' effect-set )?
              | 'drop(' place ')' ' replace'? ' -> ' bb
              | 'assert(' '!'? operand ', ' assert-kind ') -> ' bb
              | place ' = await' ' stream'? ' ' place ' -> [resume: ' bb ', cancel: ' bb ']'
place       ::= local | '(*' place ')' | place '.' INT | place '[' local ']' | place '[' '-'? INT ' of ' INT ']'
              | place '[' INT '..' '-'? INT ']' | '(' place ' as ' IDENT ')'
operand     ::= 'copy ' place | 'move ' place | 'const ' const
local       ::= '_' INT
bb          ::= 'bb' INT
```

`decl` lists every local in index order, `scope` every source scope in index order, and `block` every block in index order. The parameters appear in the header and again as `decl`s. The grammar omits the spaces after a keyword and around `//`: each is one space, as in `let _2: U32 // total`. The `decl`, `scope` and `block` lines of a body are indented by four spaces, and the statements and the terminator of a block by eight. `LINE` and `COL` are the 1-based position of the closure or `async` keyword. An `assert` with `!` expects `false`. An `INT` in a `switchInt` is the value in decimal: `0` is `false` for a `Bool`, and the values of a `Discriminant` are discriminants of the enum, whose type is its discriminant type ([spec/05](../spec/05-types.md) §5.6.2, N-A-23). `ConstIndex { offset: k, min_length: n, from_end }` prints as `p[k of n]` or `p[-k of n]`, and `Subslice { from, to, from_end }` as `p[from..to]` or `p[from..-to]`.

### 4.14.3 Rvalues, constants, types and instances

| Element | Text |
|---|---|
| `Use(op)` | the operand |
| `Borrow(r, k, p)` | `borrow(r3) p`, `borrow mut(r3) p`, `borrow two_phase(r3) p`; without `(rN)` once regions are erased |
| `RawPtr(m, p)` | `raw p`, `raw mut p` |
| `BinaryOp`, `CheckedBinaryOp`, `UnaryOp` | `Add(a, b)` with the names of §4.2.4; `CheckedAdd(a, b)`, `CheckedSub`, `CheckedMul`; `Not(a)`, `Neg(a)` |
| `Convert(k, op, T)` | `convert IntWiden(op) as T`, with the kind names of §4.2.4 |
| `Aggregate` | `(a, b)` for a tuple, `[a, b]` for an array, `T(a, b)` for a record, distinct or refined type `T`, `T.V(a, b)` for a variant, `T.V` for a unit variant |
| `Discriminant(p)`, `Len(p)`, `Repeat(op, n)` | `discriminant(p)`, `len(p)`, `[op; n]` |
| `Closure(d, ops)` | `closure name(ops)`, with the body name of `d` |
| `Scalar` | integers in decimal with the type as suffix (`5_I32`, `-1_I64`, `255_Byte`); floats as the shortest decimal that reads back to the same bits, with at least one digit after the point and the suffix (`0.1_F64`, `1.0_F64`), and `inf_F64`, `-inf_F64` and `nan_F64(0x7ff8000000000000)` with the bit pattern; `true`, `false`; a `Char` as a SAI character literal |
| `Str`, `ByteStr` | a SAI string literal, `b"…"`, with the escapes of [spec/02](../spec/02-lexical-structure.md) |
| `Zst`, `Aggregate`, `Alloc` | the value: `()`, `T.V`, or the aggregate, printed by content. An allocation is `alloc(value)`; nothing prints an `AllocId` |
| `FnItem(d, ḡ)`, `Instance(i)` | `fn path<ḡ>`, and the instance as below; `<ḡ>` is omitted when the arguments are empty |
| `Location(s)` | `loc "file":LINE:COL`, with the file string of §4.13.6 |
| `Param(c)` | the name of the const parameter |

Types are printed in source syntax with the forms of [03](03-name-resolution-and-type-checking.md) §3.5.7, except that every nominal type is printed by its canonical path ([spec/04](../spec/04-modules-and-names.md) §4.9.1) unless the prelude binds it, and a closure or future type by the body name of its definition (`closure acme:app.f{closure 3:12}`, `future acme:app.f{async 1:1}`). Regions print as `rN` in `Borrow` only. An instance prints as `path<ḡ>` for `Item`, as the body name with `<ḡ>` for a closure, clause or element, as `Kind(…)` for a shim (`DropGlue(acme:app.User)`, `CheckedEntry(acme:app.f)`, `ReifyLocation(sai:core/option.Option.unwrap<I32>, loc "…":4:9)`), as `glue s` or `runtime s` for a synthetic body, as its symbol (`rt.copy`) for a runtime intrinsic, and as `import module#field` for a core import. An `AssertKind` prints as in §4.2.6 with its operands (`BoundsCheck(copy _4, copy _5)`, `Contract(acme:app.f#requires[0])`). An effect set prints as `{e1, e2}` in the order of [08](08-effect-and-capability-analysis.md) and is omitted when empty.

---

## 4.15 Worked examples

The examples are functions of the root module of the package `acme:app`. Each shows the source and the dump of one phase in the format of §4.14, as `sai inspect mir --phase <phase>` prints it ([13](13-cli.md) §13.6.15). Their block and local numbers follow from §4.4.2, §4.5.1 and §4.6.

### 4.15.1 A loop

```sai
fn sum_to(n: U32) -> U32 {
    var total: U32 = 0
    var i: U32 = 0
    while i < n {
        i += 1
        total += i
    }
    total
}
```

```text
fn acme:app.sum_to(_1: U32) -> U32 [analyzed] {
    let _0: U32 // return
    let _1: U32 // param 1 n
    var _2: U32 // total
    var _3: U32 // i
    var _4: () // temp
    var _5: () // temp
    var _6: Bool // temp
    var _7: (U32, Bool) // temp
    var _8: (U32, Bool) // temp
    scope 0
    bb0: {
        StorageLive(_2)
        _2 = const 0_U32
        StorageLive(_3)
        _3 = const 0_U32
        StorageLive(_4)
        goto -> bb1
    }
    bb1: {
        StorageLive(_5)
        StorageLive(_6)
        _6 = Lt(copy _3, copy _1)
        switchInt(move _6) -> [0: bb4, otherwise: bb3]
    }
    bb2: {
        StorageDead(_4)
        _0 = copy _2
        StorageDead(_3)
        StorageDead(_2)
        goto -> bb8
    }
    bb3: {
        StorageDead(_6)
        StorageLive(_7)
        _7 = CheckedAdd(copy _3, const 1_U32)
        assert(!copy _7.1, Overflow(Add)) -> bb5
    }
    bb4: {
        StorageDead(_6)
        _4 = const ()
        StorageDead(_5)
        goto -> bb2
    }
    bb5: {
        _3 = move _7.0
        StorageDead(_7)
        StorageLive(_8)
        _8 = CheckedAdd(copy _2, copy _3)
        assert(!copy _8.1, Overflow(Add)) -> bb6
    }
    bb6: {
        _2 = move _8.0
        StorageDead(_8)
        _5 = const ()
        goto -> bb7
    }
    bb7: {
        StorageDead(_5)
        goto -> bb1
    }
    bb8: {
        return
    }
}
```

- The `while` is `loop { if i < n { … } else { break } }` ([02](02-frontend-ast-hir.md) §2.9.2). Its head `bb1` and exit `bb2` are created when the loop is entered (§4.4.2). `bb1` is the target of the only back edge, and it is the loop head of the SFC (§4.8.4).
- `_4` holds the discarded value of the `while` statement and `_5` that of the loop body (§4.8.7). The `break` in `bb4` writes `_4`, ends the `LoopBody` scope and leaves the loop. `_6` is the condition, a temporary of the `Condition` scope, so it is dead at the start of both branches.
- Each `+=` is the checked form of §4.8.3: the `assert` is located at the `+=` token (§4.9.1), and the write follows it, so a panic leaves the place unchanged. The checks stay in every profile until impl/09 proves them (§4.9.2).
- The Built body has the statement `checkpoint 0` after `_0 = copy _2`. Drop elaboration removes it, because no type has an invariant and there is nothing to drop. The body makes no call, so no effect set is printed.

### 4.15.2 A `match` with a guard

```sai
enum Shape {
    Circle(radius: F64)
    Rect(width: F64, height: F64)
    Empty
}

fn kind(s: borrow Shape) -> I32 {
    match *s {
        .Circle(radius: r) if r > 1.0 => 2
        .Circle(radius: _) => 1
        _ => 0
    }
}
```

```text
fn acme:app.kind(_1: borrow acme:app.Shape) -> I32 [built] {
    let _0: I32 // return
    let _1: borrow acme:app.Shape // param 1 s
    let _2: borrow acme:app.Shape // scrutinee-ref
    var _3: I64 // temp
    let _4: borrow F64 // guard-ref r
    var _5: Bool // temp
    let _6: F64 // r
    var _7: I64 // temp
    scope 0
    bb0: {
        FakeRead(ForMatch, (*_1))
        StorageLive(_2)
        _2 = borrow(r0) (*_1)
        StorageLive(_3)
        _3 = discriminant((*_1))
        switchInt(move _3) -> [0: bb1, otherwise: bb2]
    }
    bb1: {
        StorageLive(_4)
        _4 = borrow(r1) ((*_1) as Circle).0
        StorageLive(_5)
        _5 = Gt(copy (*_4), const 1.0_F64)
        switchInt(move _5) -> [0: bb4, otherwise: bb3]
    }
    bb2: {
        StorageLive(_7)
        _7 = discriminant((*_1))
        switchInt(move _7) -> [0: bb6, otherwise: bb7]
    }
    bb3: {
        StorageDead(_5)
        FakeRead(ForGuard, _2)
        StorageDead(_4)
        StorageLive(_6)
        _6 = copy ((*_1) as Circle).0
        _0 = const 2_I32
        StorageDead(_6)
        goto -> bb5
    }
    bb4: {
        StorageDead(_5)
        FakeRead(ForGuard, _2)
        StorageDead(_4)
        goto -> bb2
    }
    bb5: {
        StorageDead(_7)
        StorageDead(_3)
        StorageDead(_2)
        checkpoint 0
        goto -> bb8
    }
    bb6: {
        _0 = const 1_I32
        goto -> bb5
    }
    bb7: {
        _0 = const 0_I32
        goto -> bb5
    }
    bb8: {
        return
    }
}
```

- The scrutinee `*s` is a place and is not moved. Because the first arm has a guard, `_2` borrows it for the guards (§4.7.1).
- `Shape` has data variants, so its discriminants are 0, 1 and 2 in declaration order, of the discriminant type `I64` ([spec/05](../spec/05-types.md) §5.6.2). `0` is `Circle`.
- The guard reads `r` through `_4`, the shared reference of origin `GuardBindingRef` (§4.7.4). `r` itself is bound in `bb3`, after the `FakeRead` of the true edge. The false edge `bb4` continues with the second arm, which repeats the variant test (§4.7.2).
- The last arm has no test, so the decision list needs no `unreachable` block. The temporaries of the tests and `_2` belong to the `Scrutinee` scope and end at the join `bb5`, also on paths where a test did not run.
- `mir_optimized` removes the `FakeRead`s and the `checkpoint` is gone after drop elaboration. The regions `r0` and `r1` are the loans of [05](05-borrow-checker.md) F6.

### 4.15.3 Borrows

```sai
fn bump(counter: borrow mut U32) {
    *counter += 1
}

fn append_len(var v: Vec<USize>, counter: borrow mut U32) -> Vec<USize> {
    v.push(v.len())
    bump(counter)
    v
}
```

```text
fn acme:app.append_len(_1: Vec<USize>, _2: borrow mut U32) -> Vec<USize> [analyzed] {
    let _0: Vec<USize> // return
    var _1: Vec<USize> // param 1 v
    let _2: borrow mut U32 // param 2 counter
    var _3: () // temp
    var _4: borrow mut Vec<USize> // temp
    var _5: USize // temp
    var _6: borrow Vec<USize> // temp
    var _7: () // temp
    var _8: borrow mut U32 // temp
    scope 0
    bb0: {
        StorageLive(_3)
        StorageLive(_4)
        _4 = borrow two_phase(r0) _1
        StorageLive(_5)
        StorageLive(_6)
        _6 = borrow(r1) _1
        _5 = const fn sai:alloc/vec.Vec.len<USize>(move _6) -> bb1
    }
    bb1: {
        _3 = const fn sai:alloc/vec.Vec.push<USize>(move _4, move _5) -> bb2 effects {alloc}
    }
    bb2: {
        StorageDead(_6)
        StorageDead(_5)
        StorageDead(_4)
        StorageDead(_3)
        StorageLive(_7)
        StorageLive(_8)
        _8 = borrow mut(r2) (*_2)
        _7 = const fn acme:app.bump(move _8) -> bb3
    }
    bb3: {
        StorageDead(_8)
        StorageDead(_7)
        _0 = move _1
        goto -> bb4
    }
    bb4: {
        goto -> bb5
    }
    bb5: {
        return
    }
}
```

- The receiver of `push` is auto-referenced with mode `borrow mut`, so `_4` is a two-phase borrow (§4.5.2). The argument `v.len()` is evaluated after it and reads `v` through the shared borrow `_6`. [05](05-borrow-checker.md) accepts the read because the loan `r0` is only reserved until `_4` is moved into the `push` call that activates it (F7).
- `bump(counter)` passes `counter` where `borrow mut U32` is expected, which is a reborrow coercion ([spec/07](../spec/07-ownership-and-borrowing.md) §7.5.5). `_8` borrows `*_2`, so `counter` is not moved.
- `effects {alloc}` is the contribution of the `push` call from [08](08-effect-and-capability-analysis.md) §8.3.2. The other calls have empty sets.
- In Built, `bb3` ends with `drop(_1) -> bb4`, the drop of the parameter `v` at the return point. `v` has been moved into `_0` on every path, so drop elaboration classifies the drop `Dead` and replaces it by `goto -> bb4` (§4.11.2). The parameter `counter` has no drop glue and no storage statements.

### 4.15.4 A conditional drop

`consume` is a function `fn consume(s: String)` of the same module.

```sai
fn maybe_consume(flag: Bool) {
    let s = String.new()
    if flag {
        consume(s)
    }
}
```

```text
fn acme:app.maybe_consume(_1: Bool) -> () [drop-elaborated] {
    let _0: () // return
    let _1: Bool // param 1 flag
    let _2: String // s
    var _3: Bool // drop-flag
    scope 0
    bb0: {
        _3 = const false
        StorageLive(_2)
        _2 = const fn sai:alloc/string.String.new() -> bb1
    }
    bb1: {
        _3 = const true
        switchInt(copy _1) -> [0: bb3, otherwise: bb2]
    }
    bb2: {
        _0 = const fn acme:app.consume(move _2) -> bb4
    }
    bb3: {
        _0 = const ()
        goto -> bb5
    }
    bb4: {
        _3 = const false
        goto -> bb5
    }
    bb5: {
        switchInt(copy _3) -> [0: bb6, otherwise: bb8]
    }
    bb6: {
        StorageDead(_2)
        _3 = const false
        goto -> bb7
    }
    bb7: {
        return
    }
    bb8: {
        drop(_2) -> bb9
    }
    bb9: {
        _3 = const false
        goto -> bb6
    }
}
```

- The `if` is the tail of the body, so both of its branches write `_0`: the `then` branch by the call, the missing `else` by `const ()` (§4.8.7).
- In Built, `bb5` holds `checkpoint 0` and the terminator `drop(_2) -> bb6`, and there is neither `_3`, `bb8` nor `bb9`. At `bb5`, `s` is initialized on the path through `bb3` and moved on the path through `bb2`, so [05](05-borrow-checker.md) §5.12.1 classifies the drop `Conditional`.
- Elaboration appends the flag `_3` after the locals and the blocks `bb8` and `bb9` after the blocks (§4.11.2). The flag is `false` at entry, `true` at the start of the target of the call that initializes `s`, and `false` at the start of the target of the call that moves it, after the drop, and after `StorageDead(_2)`.
- The drop and its flag code have the span of the closing `}` of the body (§4.6.3). In Mono, the drop names `DropGlue(String)`, the glue of the library `Drop` impl of `String` (§4.13.8).

### 4.15.5 `?`

`first_digit` is a function `fn first_digit(s: Str) -> Result<U32, ParseError>` of the same module.

```sai
enum ParseError {
    Empty
    NotADigit
}

fn tagged(a: Str) -> Result<(String, U32), ParseError> {
    let tag = String.new()
    let d = first_digit(a)?
    Ok((tag, d))
}
```

```text
fn acme:app.tagged(_1: Str) -> Result<(String, U32), acme:app.ParseError> [analyzed] {
    let _0: Result<(String, U32), acme:app.ParseError> // return
    let _1: Str // param 1 a
    let _2: String // tag
    let _3: U32 // d
    var _4: Result<U32, acme:app.ParseError> // temp
    var _5: I64 // temp
    var _6: acme:app.ParseError // temp
    var _7: (String, U32) // temp
    scope 0
    bb0: {
        StorageLive(_2)
        _2 = const fn sai:alloc/string.String.new() -> bb1
    }
    bb1: {
        StorageLive(_3)
        StorageLive(_4)
        _4 = const fn acme:app.first_digit(copy _1) -> bb2
    }
    bb2: {
        StorageLive(_5)
        _5 = discriminant(_4)
        switchInt(move _5) -> [0: bb3, otherwise: bb4]
    }
    bb3: {
        _3 = move (_4 as Ok).0
        StorageDead(_6)
        StorageDead(_5)
        StorageDead(_4)
        StorageLive(_7)
        _7 = (move _2, copy _3)
        _0 = Result<(String, U32), acme:app.ParseError>.Ok(move _7)
        goto -> bb7
    }
    bb4: {
        StorageLive(_6)
        _6 = move (_4 as Err).0
        _0 = Result<(String, U32), acme:app.ParseError>.Err(move _6)
        StorageDead(_6)
        StorageDead(_5)
        StorageDead(_4)
        StorageDead(_3)
        drop(_2) -> bb5
    }
    bb5: {
        StorageDead(_2)
        goto -> bb6
    }
    bb6: {
        return
    }
    bb7: {
        StorageDead(_7)
        StorageDead(_3)
        goto -> bb8
    }
    bb8: {
        StorageDead(_2)
        goto -> bb6
    }
}
```

- `d` gets its storage before its initializer is lowered into it, so the failure branch of the `?` ends it too (§4.6.3). `_4` is the operand `_s` of §4.8.2, and `Ok` has the discriminant 0 ([spec/05](../spec/05-types.md) §5.4.5).
- The failure branch `bb4` is a return point located at the `?` token (§4.6.5). It builds the `Err` result, then ends the temporaries of the `let` statement and the locals of the block, innermost first and in reverse order. The drop of `tag` is `Static` and stays. It has the span of the `?` token. The branch is lowered first, so it creates the shared `Return` block `bb6` (§4.4.2).
- A temporary belongs to its scope from its creation. `_6` was created by the failure branch, so the normal end of the `let` in `bb3` ends it as well.
- On the success path, `tag` is moved into `_7` and `_7` into `_0`. In Built, `bb3` ends with `checkpoint 1` and `drop(_7) -> bb7`, and `bb7` with `drop(_2) -> bb8`. Both drops are `Dead` and become `goto`s. `checkpoint 0` belongs to the `?` in `bb4`.

---

## 4.16 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) and the language chapters leave open. They are provisional until merged into the decision record. IDs continue from [03](03-name-resolution-and-type-checking.md).

| ID | Rule | Rationale |
|---|---|---|
| N-E-40 | MIR extends the skeleton of D-MIR-01: `ConstIndex.min_length` and `Subslice.from_end`; the target type of `Convert`; the constants `Alloc`, `Location` and `Instance`; the statements `ProofObligation` and `CheckPoint`, which are gone by `Analyzed`; the kind of `Drop`; the kind and the `cancel` edge of `Await` with the terminator `CancelExit`; the kind of `Trap`; the local origins `GuardBindingRef`, `DirtyFlag` and `CallerLocation`; obligations, coroutine data, the caller location and the unlowered HIR of a body; predicate scopes and scope locations (§4.2). | Each addition is data that a consumer needs to see explicitly (impl/05, the SFC, impl/06), not reconstruct (P1). Mono holds only the statements of D-MIR-01, so the backend input stays as small as D-MIR-01 fixed it (P7). |
| N-E-41 | The bodies of an owner are numbered: its own body, then its closures and `async` blocks in HIR pre-order, then its contract clauses in the order of `contract_clauses`. An invariant has one body per element and a derived impl one body per trait method (§4.2.1). | Body indices are keys of the borrow checker, the SFC decisions and the dumps. A fixed order makes them reproducible (P8). |
| N-E-42 | A function item with `requires` clauses that is used as a value, not as the callee of a direct call, resolves to `CheckedEntry(f)`, which checks the clauses at run time, located at the name of `f`. `CONTRACT_RUNTIME_CHECK` is reported at the operand (§4.5.2). | spec/09 §9.3.3 requires the check, and an indirect caller cannot evaluate the clauses statically (P2). A named shim makes the check visible in MIR and in the lint (P1). |
| N-E-43 | Monomorphization untuples a call through a function trait when the callee instance is known (§4.3.4). | Generic MIR keeps the trait-call form that the borrow checker sees. The backend receives an ordinary call with the declared parameters (P7). |
| N-E-44 | A place operand is used directly only if every later operand of the same rvalue or call is a place or a constant, otherwise it is copied or moved into a temporary at its position. A `ScopeExit` drop has the fixed `end_span` of its scope, an early exit the span of its jump token, and a `Replace` drop the `=` token (§4.5.1, §4.6.3). | spec/06 §6.23 requires the value at the point of evaluation. spec/10 §10.4.5 locates a drop at "the scope end that dropped the value", which needs one fixed token per scope kind (P8). |
| N-E-45 | A `match` is a decision list: arms in source order, each pattern tested by spec/06 §6.21.7, each failed test jumping to the next alternative or arm. Tests are repeated, not shared. Unreachable arms are not lowered, and the last failure edge is `Unreachable` (§4.7.2). | The MIR follows the sequential semantics of spec/06 line by line (P9). Repetition costs only code size, which impl/09 recovers by folding known tests. |
| N-E-46 | The SFC runs on typed HIR in evaluation order. MIR contributes only the set of obligations (§4.12.2). | spec/09 §9.6.9 allows any computation with identical results. An SFC result then cannot change when MIR construction changes (P8). |
| N-E-47 | The invariant checks of return points and dirty places are materialized by drop elaboration from the initialization classes of pass A and a forward may-analysis of dirtiness. A place with a computed index is tracked as the element set `b[*]`, checked in ascending index order (§4.12.3). | Which checks run depends on initialization states that exist only after pass A (P2). The element set is the conservative reading of spec/09 §9.4.3 that keeps every required check (P9). |
| N-E-48 | A boundary function is entered from outside only through `BoundaryEntry(f)`, created only when a parameter needs validation or a clause is not proven at the entry. Import results are validated in the caller after the call (§4.12.5). | The trust boundary is a visible shim, not a property of the calling convention (P1, P4). spec/09 §9.7.3 requires the validation, and the shim costs nothing where nothing is validated (P7). |
| N-E-49 | The caller location of a transparent body is a hidden parameter `_l` of every forwarding instance, a least fixed point computed per instance. A forwarding function used as a pointer is wrapped by `ReifyLocation`. The rewriting is the same in every panic mode (§4.13.7). | The location is an explicit value with no stack walking at run time (P1, P7). A function pointer keeps its declared signature, and the result does not depend on the profile (P8). |
| N-E-50 | The async transform runs on generic Analyzed MIR at the start of `mir_optimized`. It produces a poll body and a future drop body, with the state in field 0 (`0` unresumed, `1` returned, `2 + k` suspended). Saved locals are computed from storage, and future fields never overlap (§4.10.4 to §4.10.6). | The future size follows from the source alone and does not depend on impl/09 (P8). Non-overlapping fields avoid a layout optimization whose errors would be memory-unsafe (P9). |
| N-E-51 | Instance collection with its diagnostics runs in phase 10 on Analyzed MIR (`mono_items`), separately from the collection of phase 11 for code generation (§4.1.1). | `INSTANTIATION_DEPTH_EXCEEDED`, `IMPLEMENTATION_LIMIT_EXCEEDED` and `ASYNC_RECURSION` are errors of the program, which `sai check` must report independently of optimization (P8). |
| N-E-52 | Monomorphization is deterministic, a FIFO worklist from ordered roots that scans in block, statement and operand order, and has no polymorphization: every distinct `GenericArgs` gets its own body (§4.13.1, §4.13.2). | Reproducible artifacts and instantiation keys (P8) with a simple collector (P7). Identical code is folded on Wasm functions by impl/09 §9.5. |
| N-E-53 | `ASYNC_RECURSION` is decided on the monomorphic future types by Tarjan's algorithm over inline field containment. Each cyclic component is one error at its first field, with the shortest cycle in the message (§4.13.4). | A recursion through generic arguments is visible only after monomorphization (P2). The report is the same in every build (P8). |
| N-E-54 | MIR building emits `Assert` for the check kinds and a call of `core.panic.panic` for every panic with a message. `Unwrap` and `ExplicitPanic` are never the kind of an `Assert` (§4.2.6). | One panic entry for messages keeps the runtime small (P7). A panic site records only a location and a message form, so no consumer depends on the kind (P8). |
| N-E-55 | The text format of MIR is deterministic, printed from content, not stable across compiler versions and never parsed back. Its phases are `built` to `final` (§4.14). | MIR goldens and internal compiler errors must be reproducible (P8). No stability promise keeps MIR free to change (P7). |
| N-E-56 | `sai_mir` reaches the downstream crates through the `MirHooks` supertrait, which the root database implements by forwarding to their queries (§4.1.4). | The crate graph of impl/01 stays acyclic, and every hook call is a tracked query dependency (P8). |
| N-E-57 | `mir_analyzed` removes every `old(e)` snapshot whose clause has no remaining runtime check (§4.11.3). | spec/09 §9.3.2 rule 6. A proven contract costs nothing at run time (P7). |
| N-E-58 | Every query that produces bodies runs the validator of their phase. The structural rules are checked in every build of the compiler, the dataflow rules in debug builds and the test harness, and a violation is an internal compiler error (§4.11.1). | Shims and synthetic bodies are not borrow-checked, so the validator is their only check (P9). |
| N-E-59 | There are two collections: a check collection over the program roots, in a fixed order, with the limits 64 for type depth and 1024 for nesting depth, and a code-generation collection over the artifact roots, whose instances MUST be in the check collection. A call of a component import adds edges to the drop glue of its by-value arguments and to the validation of its result (§4.13.1 to §4.13.3). | `sai check` checks exactly the program that is compiled (P8). The limits are fixed numbers, with 1024 above the minimum of 128 of spec/01 §1.5.4 (P8). The glue never depends on the code-generation collection, so the queries form no cycle. |
| N-E-60 | In Mono, every function constant is a resolved, zero-sized `Instance` constant, and `mir_mono` lowers the intrinsics kept until monomorphization to plain MIR or runtime calls. Of these, only `size_of`, `align_of` and `zeroed` reach impl/06, which also lowers the panic intrinsics `core.panic.panic` and `core.panic.unreachable` (§4.13.5 step 2, §4.13.9). | The backend never resolves a function and handles only the intrinsics that need a layout or the panic mode (P1, P7). |
| N-E-61 | A synthetic body calls a core import that has no SAI declaration through the instance `CoreImport { module, field, sig }`, whose signature consists of core scalar types. It has no body, occurs only in synthetic bodies, and is lowered by impl/06 to a call of the import (§4.13.9). | Glue and runtime bodies need the raw core functions of WIT imports and of standard error, which no SAI item declares. A dedicated instance keeps every callee of Mono MIR explicit (P1) without a SAI declaration for each import (P7). |
| N-E-62 | A dirty place reached through a `Deref` of a `borrow mut` or `SliceMut` held by a non-parameter local or a temporary is not cleaned by the end of its root. Its checks are inserted before a move of or an assignment to the root, and where the root stops being use-live (§4.12.3). | spec/09 N-C-85 requires these checks. Use-liveness is a dataflow fact of the MIR, independent of region inference, so the check positions are deterministic (P8). |
| N-E-67 | A selected property has two synthetic harness entries: a case entry that generates and runs case `i`, and a counterexample entry that generates case `i` again and formats it. `S` and `N` are constants of these entries, and the case seed is computed in closed form. Their callees follow the property in the program roots whenever tests are compiled (§4.13.10). | The runner needs no SAI source or second computation of the seeds (P9). The check collection covers exactly what the harness compiles, so `sai check --tests` checks the program that `sai test` runs (P8). Each case costs O(1) for its seed. |

## 4.17 Diagnostics

This chapter defines no diagnostic code of its own. Every code below is defined by the document named at the start of its description, and §4.x is the section of this chapter that reports it or relies on it. The codes that another crate reports are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `ASYNC_RECURSION` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.4; an anonymous future type contains itself inline; reported by `mono_items` (§4.13.4) |
| `CONST_INVARIANT_NOT_PROVEN` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.5; a construction of a type with an invariant in a const initializer that the SFC does not prove; reported by `sfc_const` of `sai_sfc` (§4.12.4) |
| `CONTRACT_RUNTIME_CHECK` | info (lint) | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.7.1; an obligation that is not proven and is checked at run time; reported by `mir_analyzed` (§4.11.3) |
| `ENSURES_USES_MOVED_PARAMETER` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.4; an `ensures` clause reads a parameter that may be moved at a return point; reported by [05](05-borrow-checker.md) N-B-41 on the check code of §4.12.1 |
| `GUARD_MOVE_OR_MUTATION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.2; a guard moves, assigns or mutably borrows a binding or the scrutinee; reported by [03](03-name-resolution-and-type-checking.md) N-E-38, with the loan of §4.7.4 enforced by [05](05-borrow-checker.md) |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | defined in [spec/01](../spec/01-introduction.md) §1.5.4; the nesting depth of monomorphization exceeds 1024; reported by `mono_items` (§4.13.3) |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | defined in [spec/05](../spec/05-types.md) §5.7.6; an instance has a type argument nested deeper than 64; reported by `mono_items` (§4.13.3) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11](11-diagnostics.md) §11.4.6; a MIR invariant or a validator rule is violated (§4.1.2, §4.11.1) |
| `REFINEMENT_NOT_PROVEN` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.2.3; a refinement obligation that the SFC does not prove; reported by `sfc` and `sfc_const` of `sai_sfc` (§4.12.2, §4.12.4) |
| `TRANSMUTE_SIZE_MISMATCH` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.5.2; the sizes of the source and target types of a `transmute` call differ or depend on a type parameter; reported by `mir_built` (§4.5.5) |
