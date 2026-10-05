# 08. Effect and Capability Analysis

**Status:** Normative for the reference implementation
**Stage:** 3 (unsafe check: Stage 2; interface effects, world imports and boundary glue: Stage 4; async: Stage 5; semantic-diff output: Stage 6)
**Related:** [spec/08](../spec/08-effects-and-capabilities.md), [spec/05](../spec/05-types.md), [spec/09](../spec/09-contracts-and-refinements.md), [spec/10](../spec/10-errors-and-panics.md), [spec/11](../spec/11-async-and-concurrency.md), [spec/12](../spec/12-unsafe-and-ffi.md), [spec/13](../spec/13-components-and-wit.md), [impl/02](02-frontend-ast-hir.md), [impl/03](03-name-resolution-and-type-checking.md), [impl/04](04-mir.md), [impl/06](06-wasm-lowering-and-layout.md), [impl/07](07-component-lowering.md), [impl/11](11-diagnostics.md), [impl/12](12-semantic-tooling.md), [impl/13](13-cli.md), [impl/15](15-conformance-and-testing.md), [impl/16](16-interfaces-and-wit.md), [package/01](../package/01-package-contract.md), [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md), [package/04](../package/04-distribution-and-security.md), [targets](../targets/targets-and-profiles.md), [decisions.md](../design/decisions.md) D-EFF-01, D-EFF-02, D-EFF-03, D-CAP-01, D-CAP-02, D-MIR-01, D-OWN-05, D-ASYNC-01, D-PKG-03, D-TOOL-01, D-TOOL-02, D-TOOL-03, D-UNSAFE-01

This chapter specifies how the reference compiler implements [spec/08](../spec/08-effects-and-capabilities.md): the representation of effect atoms and effect sets, the resolution of effect references and the checks on effect declarations, the per-body summaries computed on generic MIR, the latent expansion of generic instantiations without monomorphization, the least-fixpoint solution of the inference equations over the strongly connected components of the call graph, the permits relation and its decision procedure, the checks of declared clauses, bound caps, function pointer coercions, `Drop` impls, predicates and constants, the shortest "why" chain with deterministic tie-breaking, the reachability-based capability analysis with its per-package, transitive, grant and target checks, the import set of a component, the resolution-time capability footprint, the incremental queries with their keys and invalidation, the data produced for `sai inspect`, for semantic diff and for `sai.lock`, and the unsafe-operation check of [spec/12](../spec/12-unsafe-and-ffi.md) that [impl/03](03-name-resolution-and-type-checking.md) delegates to this chapter. The language rules are owned by spec/08 and are not repeated; this chapter fixes the data structures, the algorithms, their order and their complexity, and decides the questions spec/08 leaves to the implementation. It never changes which programs are accepted.

---

## 8.1 Overview

### 8.1.1 Position in the pipeline

```
impl/03 typeck ─► impl/04 MIR Built ─► DropElaborated ─┬─► impl/05 borrowck ─────────┐
                                                        ├─► impl/08 effects (this) ───┼─► Analyzed ─► Optimized ─► Mono
                                                        └─► SFC (impl/04, spec/09) ───┘
                                     impl/08 capability analysis (per build) ◄── effect solutions of all packages
```

1. Effect analysis runs on the MIR of every effect body (§8.1.5) in phase `DropElaborated` (D-MIR-01). It runs on **generic** MIR: a generic body is analyzed once, and the effects of its instantiations are obtained by latent expansion at the instantiating operations (§8.3.4), never by monomorphization (D-EFF-02, spec/08 §8.8.1).
2. Effect analysis and borrow checking are independent: neither reads the results of the other, and they may run in either order or in parallel. A body reaches phase `Analyzed` when both, and the Standard Fact Checker, have finished on it.
3. When [impl/04](04-mir.md) produces the `Analyzed` MIR of a body, it copies the contribution of every `Call` terminator (§8.3.2) from the body's summary into the terminator's `effects` field. It reads the contributions through its `call_effects` hook (impl/04 §4.1.4), which is the query `call_effects` of §8.1.4. In `Built` and `DropElaborated` MIR the field is the empty set. The field is informative for later phases and for `sai inspect mir`; no later phase uses it to accept or reject a program (N-D-65).
4. Capability analysis (§8.7) runs once per build after the effect solutions of all packages of the build are available. It uses reachability, not the optimizer, so its result is identical for every optimization level (spec/08 §8.12.3, N-D-13 of spec/08).
5. Predicates have no MIR of their own in the sense of this chapter. Their operations are taken from their typed HIR (§8.3.7). Constant initializers are not analyzed: the forms that [spec/06](../spec/06-expressions-and-statements.md) §6.27 admits in them contribute no effect (spec/08 §8.7.8, N-D-8 of spec/08).

### 8.1.2 Inputs

| Input | Provider | Used for |
|---|---|---|
| HIR `EffectRef { hir_id, segments }`, `Bound.effects`, fn pointer clauses, `ItemKind::Effect { covers }` | [impl/02](02-frontend-ast-hir.md) | resolving effect references (§8.1.7) |
| module scopes and path lookup | [impl/03](03-name-resolution-and-type-checking.md) §3.3–§3.4 | resolving the non-built-in form of `EffectRef` |
| `typeck` results (`call_kinds`, `node_args`, method and operator resolutions, `await_kinds`, captures, `tainted_by_errors`) | impl/03 | predicate sites (§8.3.7), the unsafe check (§8.10), gating (§8.1.9) |
| `predicates_of`, `param_env` (elaborated predicates, with the cap of each `Fn`-family and trait bound), `generics_of`, `fn_sig`, `trait_def`, `impl_header` | impl/03 | bound sets, caps and residual bounds (§8.3.4, §8.3.5) |
| `evaluate_goal` with the selected candidate | impl/03 §3.8.4 | selection in latent expansion (§8.3.4) |
| MIR in phase `DropElaborated` | [impl/04](04-mir.md) | body summaries (§8.3) |
| WIT packages, interfaces, worlds, the elaborated export world `W'` | [spec/13](../spec/13-components-and-wit.md), [impl/16](16-interfaces-and-wit.md) (`elaborated_world`, `component_world`, `interface_impls`), [impl/07](07-component-lowering.md) | interface atoms, boundary allocation, roots, `Imp` (§8.7) |
| the resolved package graph, `sai.package` policies, the selected profile | [package/01](../package/01-package-contract.md), [package/02](../package/02-resolver.md) | policies and grants (§8.7.6) |
| the target capability table | [targets](../targets/targets-and-profiles.md) | the target check (§8.7.7) |
| dependency metadata of foreign packages | [impl/01](01-architecture.md) | foreign `Sig`, edges and footprints (§8.3.8) |

### 8.1.3 Outputs and consumers

| Output | Consumer |
|---|---|
| diagnostics of spec/08 and of the unsafe check | the driver, rendered by [impl/11](11-diagnostics.md) |
| `Sig(f)` of every function, as metadata | dependent packages (§8.3.8), [impl/12](12-semantic-tooling.md) |
| `Call.effects` contributions | impl/04 `Analyzed` MIR |
| `Imp`, the boundary-allocation flags and the roots of a component build | [impl/07](07-component-lowering.md) |
| the reachable set and `alloc ∈ Own` of reachable bodies | [impl/06](06-wasm-lowering-and-layout.md) (linking the allocator, D-WASM-05) |
| effect sets and chains for `sai inspect effects`, capabilities and chains for `sai inspect capabilities`, unsafe entries for `sai inspect unsafe` | [impl/13](13-cli.md) |
| the `EffectInterface` of a package | impl/12 semantic diff (§8.9.3) |
| the capability footprint of a package | [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md), [package/04](../package/04-distribution-and-security.md) |

### 8.1.4 Queries

Every analysis of this chapter is a query in the sense of [impl/01](01-architecture.md). Every query value that carries diagnostics returns them inside the value; no query emits a diagnostic as a side effect ([impl/03](03-name-resolution-and-type-checking.md) §3.1.2).

| Query | Key | Value | Section |
|---|---|---|---|
| `effect_def` | `DefId` of an `effect` item | `EffectDef` | §8.1.7 |
| `covers_graph` | `PackageId` | the `covers` SCCs of the package's user effects, with `EFFECT_COVERS_CYCLE` | §8.1.7 |
| `covers_star` | `DefId` of an `effect` item | `covers*(u)` as an `EffectSetId` | §8.4.1 |
| `resolved_effect_set` | `HirId` of an `EffectSet` node | `ResolvedEffectSet` | §8.1.7 |
| `effect_decl_check` | `OwnerId` | diagnostics of the declaration checks | §8.1.7 |
| `ambient_imports` | `PackageId` | `AMBIENT_IMPORT` diagnostics | §8.1.7 |
| `effect_bodies` | `PackageId` | the effect bodies of the package, in key order | §8.1.5 |
| `body_effect_summary` | `EffectBodyId` | `BodySummary` | §8.3 |
| `body_effect_shape` | `EffectBodyId` | the summary without spans, read by the solver | §8.8.1 |
| `call_effects` | `OwnerId` | `CallEffectTable` (the impl/04 hook): for every body of the owner that is part of an effect body, and every `Call` terminator in it, `A(O(o))` of the solution | §8.3.2 |
| `latent_node` | `LatentKey` | `LatentNode` | §8.3.4 |
| `effect_solution` | `PackageId` | the least solution for the package's nodes | §8.2 |
| `sig_effects` | `DefId` of a function | `EffectSetId` (`Sig`) | §8.2.6 |
| `permit_index` | `(EffectSetId, PackageId)` | `PermitIndex` | §8.4 |
| `effect_check` | `EffectBodyId` | diagnostics of §8.5.2–§8.5.5, §8.5.7, §8.5.8 | §8.5 |
| `predicate_effect_check` | `PredicateSiteId` | diagnostics of §8.5.4–§8.5.6 | §8.5.6 |
| `foreign_why` | `(DefId, Atom)` of a foreign body | `WhyEntry` from metadata | §8.6.3 |
| `build_roots` | `BuildKey` | the roots of the build, in root order | §8.7.2 |
| `reach_from` | `ReachKey` | `ReachSet` of a set of roots | §8.7.3 |
| `predicate_reach` | `PredicateSiteId` | the reachability edges of the site's MIR bodies | §8.7.3 |
| `drop_glue_edges` | `(EnvKey, Ty)` | the reachability edges of the drop glue of a type | §8.7.3 |
| `reachable` | `BuildKey` | `ReachSet` of the build's roots | §8.7.3 |
| `required_capabilities` | `BuildKey` | `Req(Q)` for every package `Q` of the build | §8.7.5 |
| `component_imports` | `BuildKey` | `Imp` and `WORLD_IMPORT_UNDECLARED` | §8.7.9 |
| `capability_check` | `BuildKey` | the diagnostics of §8.7.6–§8.7.8 | §8.7.6 |
| `capability_footprint` | `(PackageId, TargetId)` | the footprint for one target, or the diagnostics that prevent it | §8.7.10 |
| `test_effect_bodies` | `PackageId` | the effect bodies of the package's tests and properties | §8.7.12 |
| `test_effect_solution` | `PackageId` | the least solution for the test bodies, on top of `effect_solution` | §8.7.12 |
| `test_effect_check` | `OwnerId` of a test or property | diagnostics of §8.5 and `TEST_EFFECT_NOT_PERMITTED` | §8.7.12 |
| `effect_interface` | `PackageId` | `EffectInterface` | §8.9.3 |
| `unsafety_check` | `OwnerId` | `UnsafetyResult`: diagnostics, unsafe entries and the operations that the module header covers | §8.10.1 |
| `unsafe_module_check` | `FileId` | the header diagnostics and the `unsafe_module` entry of the file | §8.10.6 |
| `unsafe_inventory` | `(PackageId, TargetId)` | `UnsafeInventory` | §8.9.2 |

The step `effect_summary(owner)` of the pipeline of [impl/04](04-mir.md) §4.1.1 is `body_effect_summary` of every effect body of the owner, followed by `effect_check` of each; it is not a separate query. `call_effects(owner)` is the only query of this chapter that impl/04 calls. Its value for a body whose `Call` terminators are all skipped (a contract clause body, an invariant element, a refinement predicate) maps every terminator to `∅`.

### 8.1.5 Effect bodies

An **effect body** is a body whose operations contribute to an own set (spec/08 §8.7.1):

```rust
pub enum EffectBodyId {
    Fn(OwnerId),             // fn, method, impl method, trait default method, interface impl fn, test, property
    Closure(LocalDefId),     // DefPathData::Closure
    AsyncBlock(LocalDefId),  // DefPathData::AsyncBlock
    Derived(OwnerId, Symbol),// a method of a DerivedImpl owner; its MIR body is the shim of impl/04
    Glue(GlueKey),           // export glue of a component build, or provision glue (impl/07), synthetic
}
```

- Every owner with a MIR body is a `Fn` effect body, except the owners whose bodies are predicates: a refinement predicate (the `Item` body of a refinement owner) and an `invariant` owner (its `InvariantElement` bodies) are predicate sites, like the contract clause bodies (`Clause`) of a function ([impl/04](04-mir.md) §4.2.1; fourth item below). Closures and `async` blocks are not owners ([impl/02](02-frontend-ast-hir.md)) but have `DefId`s and MIR bodies of their own (D-MIR-01, impl/04 §4.2.1); each is an effect body. A trait default method is one effect body, analyzed under `Self: Tr`.
- **`async fn`.** The code of an `async fn` `f` is the body of the `async` block that [impl/02](02-frontend-ast-hir.md) §2.9.15 synthesizes for it; the owner's own MIR body only creates that block's future, whose type is the anonymous future type `Future(f, identity arguments)` ([impl/03](03-name-resolution-and-type-checking.md) §3.11.5). The effect body `Fn(f)` comprises the operations of the owner's own body followed by those of the synthesized block (§8.3.1, §8.3.6 item 4). The synthesized block is not a separate `AsyncBlock` effect body, and the statement that creates its future is not an operation. `Own(f)` is therefore the set of effects of the code of the async function, as spec/08 §8.10 defines it, and row 14 of §8.3.2 refers to `I(f)` directly (N-D-91).
- A `DerivedImpl` owner ([impl/02](02-frontend-ast-hir.md) §2.9.17) has one effect body per method of its trait, keyed by the owner and the method name; its MIR is the shim that [impl/04](04-mir.md) builds for it ([impl/03](03-name-resolution-and-type-checking.md) §3.8.6). A derived method has no clause of its own: its `Sig` is its inferred set, and it is checked against the trait method's clause like every impl method (§8.5.3).
- Functions without a body (functions of `extern "wasm"` and `extern "C"` blocks, imported interface functions, world-import functions) are not effect bodies. Their signature sets are constants (§8.3.2).
- Refinement predicates, invariants and the `requires`/`ensures` clauses of a function are **predicate sites**, not effect bodies. They are checked by §8.5.6 and never contribute to an own set (spec/08 §8.7.8, §8.11). Their MIR bodies are read only for reachability (§8.7.3); their operations are taken from their typed HIR (§8.3.7, N-D-72). Const initializers are neither: they contain no operation (§8.1.1 item 5).
- `GlueKey` is `(BuildKey, GlueItem)` with `GlueItem` one of `Export(fn)`, for every exported function of `W'` (spec/08 §8.14.3), `Provide(fn)` and `ProvideDtor(resource)`, for the functions and resources of interfaces provided within the build (§8.7.4, [impl/07](07-component-lowering.md) §7.11.10), and `Dtor(resource)`, for every resource that `W'` exports ([impl/07](07-component-lowering.md) §7.6). Glue bodies exist only in capability analysis (§8.7.4); they have no MIR in this chapter and no node of the constraint system.
- **Tests and properties.** A `test` or `property` owner (spec/14 §14.2) is a `Fn` effect body only when its package is analyzed with tests (spec/14 §14.2.4). Its body, and the closures and `async` blocks inside it, are listed by `test_effect_bodies(P)` and solved by `test_effect_solution(P)` (§8.7.12); they are not in `effect_bodies(P)`, so that nothing a package exports depends on its tests.
- `effect_bodies(P)` lists the effect bodies of package `P`, other than test bodies, ordered by the `DefPathHash` of their `DefId` ([impl/02](02-frontend-ast-hir.md) §2.8). This order is the **stable key order** used for every iteration of this chapter that could otherwise depend on hash-map order.

### 8.1.6 Atoms and effect sets

```rust
pub enum Atom {
    Builtin(BuiltinEffect),     // u8 0..=14, in the table order of spec/08 §8.3.1
    Interface(Symbol),          // interned "ns:pkg/iface", or "ns:pkg/world" for a world-import effect
    User(DefId),                // the `effect` item
}

pub struct EffectSet {
    builtins: u16,              // bit i set iff BuiltinEffect(i) is a member
    others: Arc<[Atom]>,        // Interface and User atoms, sorted by the internal order, no duplicates
}

pub struct EffectSetId(u32);    // interned EffectSet; equal sets have equal ids
```

1. **Internal order.** `Builtin` atoms by index, then `Interface` atoms by the bytes of their identity string, then `User` atoms by the `DefPathHash` of their `DefId`. The internal order is used for storage, hashing and iteration. It is stable across sessions and compiler runs because it uses no session-local number.
2. **Print order.** The canonical order of spec/08 §8.2.3 is computed only when a set is printed (diagnostics, fixes, `sai inspect`, semantic diff, generated documentation): built-in effects by index, interface effects by identity bytes, user effects by the bytes of their spelling in the printing context (§8.1.8). The internal and print orders differ only among user effects (N-D-66).
3. Interning makes equality and hashing O(1). Union of two interned sets is computed on the representations in time linear in their sizes and the result is interned. Sets are immutable.
4. A **world-import effect** (spec/08 §8.5.1) is an `Interface` atom whose identity is `ns:pkg/world`. It is an interface effect for every purpose of this chapter.
5. The persistent form of an `EffectSet` (in metadata, in the incremental cache and in the `EffectInterface`) is the list of atoms in internal order, with `User` atoms as `DefPathHash` values and `Interface` atoms as strings.

### 8.1.7 Resolving effect references; declaration checks

`resolved_effect_set(set)` resolves the `EffectRef`s of one `EffectSet` node: a function clause, a trait method clause, a bound cap, a function pointer type's clause or a `covers` clause. Its value is

```rust
pub struct ResolvedEffectSet {
    pub set: EffectSetId,                     // the atoms of the refs that resolved
    pub refs: Vec<(HirId, Option<Atom>)>,     // per EffectRef, in source order
    pub complete: bool,                       // false if some ref failed to resolve
    pub diagnostics: Vec<Diagnostic>,
}
```

For each `EffectRef s1.….sn`, in source order, the rules of spec/08 §8.2.2 are applied:

1. If `s1` is a built-in root, the dotted text is looked up in the built-in table. A miss is `UNKNOWN_EFFECT` at the `EffectRef`, with `data { path }`. If the module scope also contains a binding named `s1`, the diagnostic carries the help "rename the binding with `as`" and names the binding.
2. Otherwise the path is looked up in the value/type namespace of the enclosing module with the path lookup of [impl/03](03-name-resolution-and-type-checking.md) §3.4, in its non-reporting form, so that this query decides the code. The result is mapped:
   - an `effect` item: `Atom::User(def)`; if the item is not visible at the reference, `EFFECT_NOT_VISIBLE`, with `data { effect, visibility }`;
   - an import binding of a WIT interface, directly or through a re-export or alias: `Atom::Interface("ns:pkg/iface")`, the versionless name of the interface (N-D-1 of spec/08);
   - a world that declares at least one `import FnHead` item: `Atom::Interface("ns:pkg/world")`;
   - anything else, or nothing: `UNKNOWN_EFFECT`, with `data { path }` and the candidates of [impl/11](11-diagnostics.md) §11.4.5 drawn from the effects, interface bindings and worlds in scope.
3. A ref whose atom equals the atom of an earlier ref of the same set is `DUPLICATE_EFFECT` (lint, default `warn`) at the later ref, with a machine-applicable fix `remove_node` on the later ref.

A failed ref contributes nothing and makes `complete` false. A function whose clause is incomplete has `Sig(f) = D(f)` with the refs that resolved; diagnostics comparing against `D(f)` are dropped by cause key C4 of [impl/11](11-diagnostics.md) §11.11.4, because the failed resolution poisons `f`'s signature.

`effect_def(u)` returns `EffectDef { name, visibility, covers: ResolvedEffectSet, package }`. If the name is a built-in root, it reports `EFFECT_NAME_RESERVED` at the name. `covers_graph(P)` builds the graph whose nodes are the user effects of `P` and whose edges are `u → v` for every user effect `v` of `P` in `covers(u)`, and runs Tarjan's algorithm (§8.2.4) over it. Every declaration in a non-trivial SCC, or with a self-edge, is reported with `EFFECT_COVERS_CYCLE`, once per declaration, with `data { cycle }` listing the members in stable key order and related `declared_here` entries for the others. Edges to user effects of other packages cannot close a cycle, because the package graph is acyclic.

`effect_decl_check(owner)` reports, for one owner:

- `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` for every `User` atom whose `effect` item is not declared `pub`, named in the clause of a `pub` function, in a clause of a method of a `pub` trait, in a bound cap of a `pub` signature, or in a function pointer type of a `pub` signature (spec/08 §8.4.2), once per `EffectRef`;
- `EFFECTS_ON_INTERFACE_FN` for an `effects` clause on a function of an `interface` declaration, at the clause, with a machine-applicable fix `remove_node`;
- `TRAIT_ITEM_SIGNATURE_MISMATCH` for a trait impl method one of whose own bounds carries a cap `E` that the trait method's corresponding bound does not imply (spec/08 §8.8.3, N-D-27 of spec/08): after the positional renaming of [impl/03](03-name-resolution-and-type-checking.md) `compare_impl_fn`, for every elaborated predicate of the impl method with a cap list `caps_I` and its equal predicate in the trait method with cap list `caps_T`, every `E ∈ caps_I` needs some `E' ∈ caps_T` with `E' ⊑ₚ E` (§8.4), `P` the impl's package. impl/03 compares the predicate sets without caps and reports the other differences; this check runs only if impl/03 reported none for the item (N-D-87). The diagnostic is at the first offending cap, with a `declared_here` entry at the trait's bound and a maybe-incorrect fix `remove_node` of the impl's cap;
- `EXTERN_EFFECTS_MISMATCH` for a function of an `extern "wasm"` block whose clause is not exactly `{ host }`, or of an `unsafe extern "C"` block whose clause is not exactly `{ ffi }`, at the clause, with a machine-applicable fix `set_effects { effects: ["host"] }`, respectively `["ffi"]`.

`ambient_imports(P)` reports `AMBIENT_IMPORT` (lint, default `warn`; error under `policy { deny ambient_imports }`) once per `use` declaration of `P` whose target is an interface of a `wasi:` package, unless `P` is in the `sai` namespace (spec/08 §8.5.3). The level decision is that of [impl/11](11-diagnostics.md) §11.3.

### 8.1.8 Spelling atoms

Fixes and messages print atoms. The spelling of an atom `a` in module `M` is `spell(a, M)`:

1. A built-in effect: its name.
2. An interface or user atom: among the paths that resolve to `a` from the scope of `M` by rule 2 of §8.1.7, and whose first segment is not a built-in root, the shortest by number of segments, then the least by bytes. Such a path exists if `a` is named by a binding in scope, by a path through a module in scope, or by a re-export.
3. If no such path exists, `a` is **not spellable** in `M`.

Outside a module context (in messages that are not inside a fix, in `data` members, in chains, in `sai inspect capabilities`), a user effect is displayed as `<PKG_PATH of its module>.<name>` (for example `acme:db/db.write`), and an interface atom by its identity. This is the **display form**. When the build contains two compatibility lines of the declaring package ([package/02](../package/02-resolver.md) §2.9), the PKG_PATH alone does not identify the effect, so the display form of a user effect inserts `@` and the selected version of its node after the package name (`acme:db@2.1.0/db.write`). An interface atom needs no version: its identity is versionless and is the same atom in both lines (spec/08 N-D-1, [spec/13](../spec/13-components-and-wit.md) §13.7.1). The effect set of a function in `sai inspect effects` is printed in the function's module, with `spell` and the display form for an atom that is not spellable there (§8.9.1); the semantic diff prints its `effects` aspect by its own rule ([impl/12](12-semantic-tooling.md) §12.8.2). A structured fix that would have to print an atom that is not spellable in the module of the fix is not offered; the diagnostic instead carries a help that names the atom in display form and the `use` declaration that would bring it into scope (N-D-67).

### 8.1.9 Error handling and gating

1. A body whose type checking produced an error (`tainted_by_errors`, impl/03) has no MIR. Its effect body exists, but its summary is **incomplete**: it has no operations and the flag `incomplete`. A predicate site in an owner with type errors is likewise incomplete.
2. The value of a node that reaches an incomplete node (§8.2.2) is computed normally, from the operations that are known, and is marked **incomplete** as well. Incompleteness propagates along the same edges as atoms.
3. The checks of §8.5 for an effect body or predicate site run only if its own summary is complete (phase gating, [impl/11](11-diagnostics.md) §11.11.4). A check whose outcome depends on an incomplete value (a caller's `EFFECT_NOT_DECLARED`, `MISSING_EFFECTS_DECLARATION` or `BOUND_EFFECTS_EXCEEDED` that involves an incomplete node) is not reported, because the error that made the node incomplete poisons the callee's body (cause key C5) or signature (C4).
4. Capability analysis of a build (§8.7) runs only if every effect body and predicate site of every package of the build is complete and no effect diagnostic of severity error was reported. Otherwise `capability_check` returns no diagnostics and records that it did not run; `sai inspect capabilities` then reports the errors instead of a table. Capability checks are defined on the effect sets of a correct program (spec/08 §8.12.6: "after effect inference has succeeded"), and partial reachability would produce misleading capability errors (N-D-68).

---

## 8.2 Lattice and fixpoint

This section is the algorithmic basis that spec/08 §8.7.9 cites.

### 8.2.1 The lattice

Let `U` be the finite set of atoms that occur in a build: the built-in effects, the interface and world-import atoms of the WIT packages of the build, and the user effects of its packages. The effect sets of the build are the subsets of `U`, ordered by `⊆`, with join `∪` and bottom `∅`. This is a finite lattice of height `|U|`.

Inference uses only `⊆` and `∪`. The permits relation `⊑ₚ` (§8.4) is used only for checks, never for inference.

The value of every node of §8.2.2 is a quadruple

```
val(n) = (A(n), R(n), inc(n), ovf(n))
```

- `A(n) ⊆ U` is an effect set;
- `R(n)` is a set of **residual bounds** (spec/08 §8.8.2): trait predicates of the parameter environment of the generic context of `n` that answered a goal of a latent expansion or of a call through a type parameter, each with its caps (§8.3.5);
- `inc(n) ∈ {false, true}` is the incompleteness flag of §8.1.9, ordered `false < true`;
- `ovf(n) ⊆ {depth, solver}` records that a latent expansion reached from `n` exceeded the instantiation depth or the solver's limit (§8.3.5).

Each component is a finite join-semilattice with join `∪` (or `∨`), so the quadruples form a finite lattice under the componentwise order.

A node with `ovf(n) ≠ ∅` is treated as incomplete for every consumer: the operation whose latent reference first reaches the overflowing expansion reports `INSTANTIATION_DEPTH_EXCEEDED` or `TRAIT_SOLVER_OVERFLOW` (§8.3.5), and the body containing it is marked incomplete. spec/08 §8.8.2 (N-D-26 of spec/08) states that such an operation contributes no effect; this chapter computes the atoms that the expansion did reach, but no consumer can observe the difference, because incomplete values are never compared against a clause (§8.1.9 rule 3), capability analysis does not run (rule 4), and `sai inspect effects` omits incomplete functions (§8.9.1).

### 8.2.2 The constraint system

The nodes of the system for a package `P` are:

| Node | Meaning | Exists for |
|---|---|---|
| `I(b)` | the own set of effect body `b`, and `Inf(b)` when `b` has no clause | every effect body of `P` |
| `S(p)` | the effect set of predicate site `p` (§8.3.7) | every predicate site of `P` |
| `O(o)` | the contribution of operation `o` of a body or predicate site (§8.3.1) | every operation of `P` |
| `Λ(k)` | the latent effects of a latent key `k` (§8.3.4) | every latent key reached from an operation of `P` |
| `Inst(K, ḡ)` | the effects of calling closure or `async` block body `K` under arguments `ḡ` other than its identity arguments (§8.3.3) | every such pair reached from `P` |

Each node has a set of **constants** `K(n)` (a quadruple) and a list of **references** `Ref(n)`, each reference being an edge `n → m` with a **projection**:

- `full`: the edge contributes `val(m)`;
- `atoms`: the edge contributes `(A(m), ∅, inc(m), ovf(m))`. A reference to `I(f)` of a named function (not a closure or `async` block) is always an `atoms` edge, because the residual bounds of `f`'s own operations are bounds of `f`, which mean nothing in the caller (spec/08 §8.8.2).

`I(b)` and `S(p)` have no constants of their own except the incompleteness flag of §8.1.9; their references are `full` edges to `O(o)` for every contributing operation `o` of the body or site, in operation order. The constants and references of `O(o)` are given by §8.3.2, those of `Λ(k)` by §8.3.4, those of `Inst(K, ḡ)` by §8.3.3. Operation nodes exist so that each check of §8.5 can read the value of a single operation (N-D-73). References to nodes of other packages do not occur: a foreign function's `Sig` is a constant from metadata, and a latent key whose expansion is needed by `P` is a node of `P` even if all its impls are foreign (§8.3.8).

The equations are

```
val(n) = K(n) ∪ ⋃ { π(val(m)) | (n → m, π) ∈ Ref(n) }
```

where `π` is the projection of the edge. For every body `b` with a clause, `I(b)` is still a node, because its own set is checked against the clause (§8.5); but references from other nodes to a function with a clause never point to `I(b)`: they use the constant `D(b)` (spec/08 §8.7.9).

### 8.2.3 The least solution

**Theorem.** The system of §8.2.2 has a least solution. In it, for every node `n`,

```
A(n)   = ⋃ { A(K(m)) | m reachable from n }
R(n)   = ⋃ { R(K(m)) | m reachable from n through full edges only }
inc(n) = ∨ { inc(K(m)) | m reachable from n }
ovf(n) = ⋃ { ovf(K(m)) | m reachable from n }
```

where "reachable" includes `n` itself. The least solution equals the limit of Kleene iteration from the bottom element, and it is unique.

*Proof.* Every right-hand side is a union of constants and projections of other values; union and both projections are monotone, so the system is a monotone function on a finite lattice and has a least fixpoint, the limit of Kleene iteration from bottom (Knaster–Tarski, Kleene). The formula of the theorem is a solution: unions along edges compose, and an `atoms` projection discards the residual component of everything reached through it. It is the least, because every constant of a reachable node must be included in any solution by induction on the length of the path. ∎

Consequences used by this chapter:

- all nodes of one strongly connected component have the same `A`, `inc` and `ovf`, and the same `R` if the component's internal edges are all `full` (spec/08 §8.7.9: functions of one SCC have the same inferred set);
- the solution is independent of the order in which nodes are visited;
- non-terminating recursion produces no atom that no operation contributes.

### 8.2.4 Algorithm

`effect_solution(P)` computes the least solution by one pass of Tarjan's strongly-connected-components algorithm over the graph `(nodes, Ref)`, in its iterative form (an explicit stack, so that deep call graphs never overflow the native stack):

1. The roots of the traversal are the nodes `I(b)` for the effect bodies of `P` in stable key order (§8.1.5), then the nodes `S(p)` of the predicate sites of `P` in the same order, then the operation nodes `O(o)` that no `I(b)` or `S(p)` references (the non-contributing operations of rows 2, 20 and 21 of §8.3.2), in operation order. Successors of a node are visited in the order of `Ref(n)`, which is the order in which the summary lists the references (MIR order of the operations, §8.5.1, then the order of §8.3.4 within an expansion).
2. Tarjan's algorithm emits SCCs in reverse topological order: every SCC is emitted after all SCCs it references. When an SCC `S` is emitted, its value is computed directly:
   - `A(S)`, `inc(S)` and `ovf(S)` are the union of the constants of the members and of the values of the SCCs referenced from members by edges leaving `S`;
   - `R` is computed per member: the residual component of a member `n` is the union of the residual constants of the members reachable from `n` by `full` edges inside `S` and of the residual components of `full` successors outside `S`. Inside an SCC this is a reachability closure on the `full` subgraph of `S`, computed by a second Tarjan pass over that subgraph restricted to `S`.
   - Every member's value is stored.
3. Latent nodes are created on demand: when a node's references are first needed, `latent_node(k)` is called, and the node joins the traversal.

No iteration to a fixpoint is needed: because the system contains only unions, the value of an SCC is the union of everything it reaches (§8.2.3), and SCCs are emitted after their successors. The algorithm visits every node and every edge exactly once, and performs one set union per edge.

### 8.2.5 Determinism

The traversal order of §8.2.4 is fixed, but the solution does not depend on it (§8.2.3). Order matters only for the identity of SCCs as reported in data and for the time at which latent nodes are created; neither affects any diagnostic. Diagnostics are produced by `effect_check` (§8.5) from the solution, in the order of [impl/11](11-diagnostics.md) §11.11.1.

### 8.2.6 Signature sets

`sig_effects(f)` for a function `f`:

| `f` | `Sig(f)` |
|---|---|
| has a clause | `D(f)` from `resolved_effect_set` (§8.1.7) |
| has a body and no clause | `A(I(f))` from `effect_solution(package of f)` |
| imported interface function, resource constructor, method or static of interface `i` | `{ i }`, plus `alloc` if it allocates at the boundary (spec/08 §8.14.3) |
| world-import function of world `w` | `{ ns:pkg/w }`, plus `alloc` under the same condition |
| function of an `extern "wasm"` block | `{ host }` |
| function of an `unsafe extern "C"` block | `{ ffi }` |
| function of an `interface` declaration (as a trait-like declaration, spec/13) | no set; calls of interface functions are calls of imports or of the implementing functions |
| function of another package | the `Sig` recorded in that package's metadata (§8.3.8) |

`sig_effects` of a function with a clause never depends on its body, so editing a body below a clause never invalidates a caller (§8.8.3).

The boundary-allocation condition is evaluated on the WIT function type (spec/08 §8.14.3):

```
allocates(fn) = contains_string_or_list(result)
             ∨ ∃ parameter p: contains(p, list<u>) with u not flat-primitive
```

where `contains_string_or_list` and `contains` descend through records, tuples, variants, options, results and lists, but not through resource handles. Type aliases are expanded. The result is cached per WIT function.

### 8.2.7 Several packages

The constraint system is solved per package. A dependency's solution is complete before a dependent's system is built, because the dependent only reads the dependency's `Sig` values and metadata edges (§8.3.8), never its nodes. Latent keys that involve only foreign impls are nodes of the consuming package; two consuming packages that need the same key compute it independently with the same result, because the key's expansion depends only on foreign metadata and on the key (N-D-69).

---

## 8.3 Body summaries

`body_effect_summary(b)` reads the MIR of one effect body and produces its **summary**: the list of its operations, each with its constants, references, residual bounds and reachability edges. The summary is the only input of the constraint system (§8.2) that depends on a body; it is computed once per body and never per instantiation.

### 8.3.1 Operations

An **operation** is a MIR element that can contribute effects, take part in a check, or create a reachability edge:

| MIR element | Operation kind |
|---|---|
| terminator `Call { func, args, .. }` | `Call` |
| terminator `Await { future, .. }` | `Await` |
| terminator `Drop { place, .. }` | `Drop` |
| statement `Assign(_, Closure(def, ops))` | `CreateClosure` (closure or `async` block, by the `DefPathData` of `def`) |
| statement `Assign(_, Convert(FnItemToFnPtr, op, _))` (the MIR form of the `ReifyFnPointer` adjustment of impl/03 C-FnPtr, [impl/04](04-mir.md) §4.5.2) | `Reify` |
| an operand `Const` whose type is `FnDef(g, ḡ)`, outside the `func` position of a `Call` and not the operand of a `Reify` | `FnItemValue` |

Every other statement, rvalue and terminator (`Use`, borrows, raw pointers, primitive `BinaryOp`/`CheckedBinaryOp`/`UnaryOp`, other conversions, `Aggregate`, `Discriminant`, `Len`, `Repeat`, `SetDiscriminant`, `FakeRead`, `StorageLive`/`StorageDead`, `Nop`, `ProofObligation`, `Goto`, `SwitchInt`, `Return`, `Unreachable`, `Assert`, `CancelExit`, `Trap`) contributes `∅` and is not an operation (spec/08 §8.3.3, §8.7.8). Operators on non-primitive types, indexing through `Index`, `for` loops, `?`, f-strings and string concatenation are `Call` terminators in MIR ([impl/04](04-mir.md)), so they are classified like every other call (spec/08 §8.7.4).

**Operation order.** The operations of a body are numbered in **MIR order**: basic blocks by index, the statements of a block in order, then its terminator; within one statement, operands from left to right. This is the order of spec/08 §8.9.1 ("the first operation that contributes it, in MIR order"). Because basic block indices are fixed by MIR construction, the order depends neither on optimization nor on the order in which bodies are analyzed.

**Instantiating operations.** An operation **instantiates** a function item if it names the item with its arguments: a `Call` whose `func` is a `Const` operand (rows 1–4 of §8.3.2; for row 4 only the bounds of the method's own type parameters are instantiated there), a `Reify` whose operand is a `Const` (row 20), and a `FnItemValue` (row 21). Only instantiating operations have `bound_refs`, and only they are checked by the bound-cap check (§8.5.4, N-D-77). A `Call` through a local or a parameter of `FnDef` type (note 4 of §8.3.2), a call of a function-trait method on a function item (row 5), and the `Await` of an `async fn` future (row 14) instantiate nothing: the item and its arguments were named, and checked, at the operation that created the value. Their constants and references are still those of the table, so their contributions are complete.

**Predicate scopes.** The runtime checks that [impl/04](04-mir.md) inserts for `requires`, `ensures`, invariants and refinements evaluate predicate expressions inside the body. Their operations are recognized by their source scope, which impl/04 marks as a **predicate scope** (the scope of the inserted check, ending at its `Assert { kind: Contract | Refinement | Invariant }`), and are skipped: they are analyzed as predicate sites on HIR (§8.3.7), and they never contribute to `Own(b)` (spec/08 §8.7.8). A `Call` of a contract clause body, of an invariant element body or of a refinement-predicate instance ([impl/04](04-mir.md) §4.3.3, [impl/03](03-name-resolution-and-type-checking.md) §3.13), wherever it occurs, is skipped for the same reason; it keeps its reachability edge (§8.7.3). The reachability edges of skipped operations are recorded in `skipped_reach` of the summary, because a skipped operation has no `OpRecord`.

**Records.**

```rust
pub struct BodySummary {
    pub body: EffectBodyId,
    pub env: ParamEnvKey,             // the ParamEnv of the generic context (the owner's, for closures and async blocks)
    pub ops: Vec<OpRecord>,           // in operation order
    pub incomplete: bool,             // §8.1.9
    pub skipped_reach: Vec<ReachEdge>,// reachability edges of skipped operations (predicate scopes), §8.7.3
}

pub struct OpRecord {
    pub index: u32,                   // position in operation order
    pub loc: OpLoc,                   // Mir { block, statement_or_terminator }, Hir(HirId) (§8.3.7), or Entry { param, part } (§8.7.12)
    pub span: Span,                   // impl/11 §11.4.2 primary location of the operation
    pub kind: OpKind,                 // Call, Await, Drop, CreateClosure, Reify, FnItemValue
    pub callee: Callee,               // the classification of §8.3.2
    pub contributes: bool,            // whether I(b) references O(o)
    pub consts: EffectSetId,          // constant atoms of O(o)
    pub refs: Vec<(NodeRef, Projection, LinkKind)>,   // references of O(o), with the why-link they represent (§8.6)
    pub bound_refs: Vec<BoundRef>,    // per bound of the callee's signature: the latent node and the bound's caps (§8.3.4)
    pub residuals: Vec<Residual>,     // residual bounds introduced directly by this operation (§8.3.5)
    pub reach: Vec<ReachEdge>,        // reachability edges (§8.7.3)
}
```

`NodeRef` is one of `I(b)`, `Λ(k)` and `Inst(K, ḡ)`. `BoundRef { bound: PredicateRef, caps: Arc<[EffectSetId]>, node: Λ(k) }` keeps the latent node of each bound of the callee separate, so that bound caps (§8.5.4) can be checked per bound; the same nodes also appear in `refs` with projection `full`.

### 8.3.2 Contributions

The **callee** of an operation is classified from types only, never from names. For a `Call`, the type of `func` decides; when `func` is a trait method item (`FnDef(m, ḡ)` with `m` an item of trait `B`), the trait goal `ḡ[0]: B<ḡ[1..]>` is selected first with `evaluate_goal` in the body's `ParamEnv` ([impl/03](03-name-resolution-and-type-checking.md) §3.8.4), and the selected source decides. The call of a closure or of a type parameter through the function traits (`CallKind::FnTrait`, impl/03 §3.7.12, §3.11.4) is a call of the method of `Fn`, `FnMut` or `FnOnce` and is classified by the same selection.

Let `Contrib(g, ḡ)` (a function item applied to arguments) denote: the constant `Sig(g)` if `g` has a clause, has no body, or belongs to another package; an `atoms` reference to `I(g)` otherwise; and in both cases one `full` reference to `Λ(k)` for every bound `W: B` of `g`'s signature, with `k` the latent key of `W[ḡ]: B[ḡ]` in the body's environment (§8.3.4). This is `Sig(g) ∪ Lat(g; ḡ)` of spec/08 §8.7.2.

| # | Operation and callee | Constants and references of `O(o)` | Residuals | Contributes |
|---|---|---|---|---|
| 1 | `Call`, `func: FnDef(g, ḡ)`, `g` not a trait item, not `async` | `Contrib(g, ḡ)` | — | yes |
| 2 | `Call`, `func: FnDef(g, ḡ)`, `g` an `async fn` (free or inherent) | no constants; the bound references of `Contrib(g, ḡ)` only (one `full` reference to `Λ(k)` per bound of `g`'s signature), read only by the bound-cap check (§8.5.4); the call itself contributes nothing (spec/08 §8.10) | — | no |
| 3 | `Call` of trait method `B.m`, selection `Impl { impl, args }` | `Contrib(m_impl, args ++ D̄)`, where `m_impl` is the impl's item `m`, or the trait's default body when the impl does not define `m` (impl/03 §3.8.7); a derived impl gives the `Derived` body of `m` | — | yes |
| 4 | `Call` of trait method `B.m`, selection `Param(p)` (the receiver's type is a type parameter, `Self` in a default body, a rigid projection, or a type with a matching environment predicate) | `Latₘ(m; D̄)`: one `full` reference to `Λ(k)` per bound on `m`'s own type parameters, instantiated with the method arguments `D̄` | the predicate `p` | yes |
| 5 | `Call` of a function-trait method, selection `Builtin` on `FnDef(g, ḡ)` | as row 1 or row 2 for `g` | — | yes |
| 6 | `Call` of a function-trait method, selection `Builtin` on `FnPtr` with effect set `E` | the constant `E` | — | yes |
| 7 | `Call` of a function-trait method, selection `Builtin` on `Closure(c, ḡ)` | the closure reference of §8.3.3 for the body of `c` | — | yes |
| 8 | `Call` of any other trait method with a `Builtin` selection (structural `Clone`, comparison, hashing, `Default`, `Arbitrary` and `Format` of tuples, arrays, borrows and refined types) | one `full` reference to `Λ(k)` per nested obligation of the builtin candidate (impl/03 §3.8.5), in the order the solver lists them; none for primitives | — | yes |
| 9 | `Call` with `func` of type `FnPtr` with effect set `E` | the constant `E` | — | yes |
| 10 | `Call` of an imported interface function of `i` | `{ i }`, plus `alloc` if `allocates(fn)` (§8.2.6) | — | yes |
| 11 | `Call` of a world-import function of world `w` | `{ ns:pkg/w }`, plus `alloc` if `allocates(fn)` | — | yes |
| 12 | `Call` of an `extern "wasm"` function | `{ host }` | — | yes |
| 13 | `Call` of an `unsafe extern "C"` function | `{ ffi }` | — | yes |
| 14 | `Await` of `Future(g, C̄)`, `g` an `async fn` | `Contrib(g, C̄)` (spec/08 §8.10) | — | yes |
| 15 | `Await` of the future of an `async` block `d` | the closure reference of §8.3.3 for the body of `d` | — | yes |
| 16 | `Await` of a future whose `Future` goal selects `Param(p)` | `∅` | the predicate `p` | yes |
| 17 | `Await` of any other future type `Q` | as row 3 for `Future.poll` with `Self := Q` | — | yes |
| 18 | `Drop` | `∅` (spec/08 §8.7.5) | — | no |
| 19 | `CreateClosure` | `∅` (spec/08 §8.7.6) | — | no |
| 20 | `Reify` of `FnDef(g, ḡ)` to `FnPtr` with effect set `E` | `Contrib(g, ḡ)`, read by the coercion check (§8.5.5) | — | no |
| 21 | `FnItemValue` of `FnDef(g, ḡ)` | no constants; the bound references of `Contrib(g, ḡ)` only, read only by the bound-cap check (§8.5.4) | — | no |

Notes on the table:

1. Rows 3 and 5 use the arguments of the selected impl followed by the method's own arguments `D̄`, which is how impl/03 §3.8.7 names the callee instance. The bounds of `m_impl`'s signature are those of the impl and of `m` itself; for a default body they are the trait's elaborated predicates with `Self` and the trait arguments substituted, and the bounds of `m`. Bounds of `m`'s own type parameters are therefore attributed at the call in every row, as spec/08 §8.7.3 requires.
2. A trait method with a clause whose impl is reached by row 3 contributes `Sig(m_impl)`, not the trait's clause: the call resolves statically to the impl (spec/08 §8.7.2). A trait default body is a function with a clause exactly when the trait method declares one; that clause is then its `D` (spec/08 §8.6.3).
3. Rows 2 and 14 split an `async fn` call into its two halves: the call constructs the anonymous future and contributes nothing, the `.await` contributes the body's effects with the type arguments of the constructing call, which are the arguments of the future type `Future(g, C̄)` ([impl/03](03-name-resolution-and-type-checking.md) §3.11.5). `for await` reaches row 17 through the `Await` of the future returned by `lang::StreamNext` (impl/03 §3.7.11); the `Stream` effects of the stream type are part of that future's `poll` impl through its bound `S: Stream`.
4. A `Call` whose `func` is an operand of type `FnDef` stored in a local is classified like a direct call: the function item type determines the callee (D-TYPE-09, spec/08 §8.7.3 row 4).
5. A trait method call whose selection is `Ambiguous` or `Failed` after type checking succeeded, or a `func` of any other type, is an internal compiler error (`INTERNAL_COMPILER_ERROR`, [impl/11](11-diagnostics.md) §11.4.6): type checking proved every obligation of the body (N-D-73).
6. Rows 2 and 18–21 do not contribute, but they carry reachability edges (§8.7.3); rows 2, 20 and 21 carry checks (§8.5.4, §8.5.5). The effects of a function item passed as a value are attributed where the value is called: through a function trait (row 5 inside a latent expansion, §8.3.4), through a coercion (row 20), or by `.await` (row 14).

### 8.3.3 Closures and `async` blocks

A closure body or `async` block body `K` is analyzed in the generic context of its enclosing owner (spec/08 §8.7.6; impl/03 §3.14.1: closures and `async` blocks use the `ParamEnv` of their owner). The **closure reference** to `K` with arguments `ḡ` is:

- a `full` reference to `I(K)` if the parent part of `ḡ` is the identity of the current body's generic context; this is the case whenever the closure type was created in the current owner, which is the only way a closure type can reach a body in which it is called, because closure and future types cannot be named in signatures;
- otherwise a `full` reference to the node `Inst(K, ḡ)`, whose value is `A(I(K))` together with `Λ(k)` for the latent key of `ρ[ḡ]` for every residual bound `ρ ∈ R(I(K))`: an `atoms` reference to `I(K)`, and one `full` reference per residual. Residuals of `I(K)` are known only after `I(K)`'s SCC is emitted, so `Inst` nodes are expanded when Tarjan's algorithm first visits them (§8.2.4 rule 3), after the SCC of `I(K)` is complete; an `Inst` node therefore never belongs to the SCC of `I(K)`, and finding `I(K)` on the Tarjan stack when `Inst(K, ḡ)` is visited is an internal compiler error. The second case does not arise from source programs accepted by spec/05; it is defined so that latent expansion is total.

The reference is `full` because a closure is not a function with a signature: its residual bounds are bounds of the enclosing owner and stay latent there (spec/08 §8.8.2, third item). Named functions, in contrast, are referenced through `atoms` (§8.2.2).

### 8.3.4 Latent keys and their expansion

```rust
pub struct LatentKey {
    pub env: EnvKey,                 // Empty, or the ParamEnv of the generic context in which the goal is posed
    pub goal: TraitPredicate,        // W[ḡ]: B<ā>, normalized, interned
}
```

1. **Environment.** `env` is `Empty` when the goal mentions no type parameter and no projection and the context's `ParamEnv` contains no predicate without type parameters; otherwise it is the `ParamEnv` of the context. A goal under `Empty` is answered by impls alone, so keys with `Empty` are shared by every body of the package (§8.2.7, N-D-74).
2. **Normalization.** The goal is normalized with impl/03 §3.5.5 in its environment before the key is built. A projection that normalizes to a type is replaced by it; a rigid projection stays, and its goal is answered by an environment candidate (an elaborated predicate or an associated-type bound, impl/03 §3.14.1 item 5).
3. **Depth.** If the type nesting depth of the goal's self type (spec/05 §5.7.6) exceeds 64, the node is not expanded; its constant is `ovf = { depth }` (§8.3.5).
4. **Selection.** Otherwise `evaluate_goal(Canonical<(env, goal)>)` selects a source, and the node's constants and references are:

| Source | Constants and references of `Λ(k)` |
|---|---|
| `Param(p)` | the residual `p`, with its caps (§8.3.5); no atoms |
| `Impl { impl, args }` | for every method `m` of `B` in declaration order: `Sig(m_impl)` (the impl's item, the default body, or the `Derived` body), as a constant or an `atoms` reference to `I(m_impl)` as in `Contrib`; then one `full` reference to `Λ` of `q[args]` for every trait predicate `q` of the impl's elaborated predicates, in their order; then, if some method uses the default body, one `full` reference to `Λ` of `q[Self := self type, trait arguments]` for every trait predicate `q` of the trait's elaborated predicates, in their order |
| `Builtin` function trait on `FnDef(g, ḡ)` | `Contrib(g, ḡ)`, or `∅` for an `async fn` (row 2 of §8.3.2) |
| `Builtin` function trait on `FnPtr` with effect set `E` | the constant `E` |
| `Builtin` function trait on `Closure(c, ḡ)` | the closure reference of §8.3.3 |
| `Builtin` `Future` on `Future(g, C̄)` of an `async fn` | `Contrib(g, C̄)` |
| `Builtin` `Future` on the future of an `async` block `d` | the closure reference of §8.3.3 for `d` |
| any other `Builtin` | one `full` reference to `Λ` of every nested obligation, in solver order; none if there is none |
| `Failed(Overflow)` | the constant `ovf = { solver }` |
| `Failed(Unsatisfied)`, `Ambiguous` | `INTERNAL_COMPILER_ERROR` |

5. **Methods without bodies of their own.** Only methods are expanded. Associated types and constants of `B` contribute nothing directly; a bound declared on an associated type (`type A: Bs`) contributes through the elaborated predicates of the context in which the projection is used, or through the impl's predicates when the impl's definition of `A` has bounds to satisfy.
6. **Cycles.** A key reached again during its own expansion, for example through a default body whose predicates contain `Self: B`, is an ordinary back edge of the graph. It is solved by the SCC computation of §8.2.4 without special treatment, which is the least fixpoint that spec/08 §8.8.2 requires.
7. **Selection is final.** Within one environment the selected source for a key never changes, because coherence admits at most one impl and environment candidates take precedence (impl/03 §3.8.4 item 4). For a key under `Empty`, the result is the same in every body of every package that can see the same impls; a package that sees more impls (a dependent) builds its own node (N-D-69).

**Invariant.** For every named function `f`, every residual in `R(I(f))` is a predicate of `f`'s parameter environment, and every such predicate that is a trait predicate is among the bounds of `f`'s signature (spec/08 §8.8.2) or is an associated-type bound of one. A caller that instantiates `f` therefore expands exactly these predicates through `Lat(f; C̄)`. This is what makes the `atoms` projection of references to `I(f)` sound: the residual effects of `f`'s body are not lost, they are re-expanded at every caller with the caller's arguments.

`latent_node(k)` is a query: its value is the list of constants and references of `Λ(k)` together with the selection that produced it, not the node's value. Values are computed only by `effect_solution` (§8.2.4). This keeps the query graph acyclic ([impl/01](01-architecture.md) §1.4.5) even when the constraint graph has cycles.

### 8.3.5 Residual bounds, caps and overflow

```rust
pub struct Residual {
    pub pred: TraitPredicate,          // the environment predicate that answered the goal, interned
    pub origin: ResidualOrigin,        // Elaborated(index in param_env) | AssocBound(trait item, bound index)
    pub caps: Arc<[EffectSetId]>,      // every cap that applies to the predicate, in elaboration order
    pub span: Span,                    // the bound in the signature, for messages
}
```

1. **Identity.** A residual is identified by `(pred, origin)`. impl/03 drops structurally equal predicates from the elaborated list (impl/03 §3.14.1 item 4), so two operations that are answered by the same bound produce the same residual.
2. **Caps.** `caps` lists the cap written on the bound that produced the predicate, followed by the caps of every predicate from which it was elaborated (supertrait and `where` elaboration of impl/03 §3.14.1 item 3, and the bound on which an associated-type bound was found). A cap on a bound applies to every bound elaborated from it (N-D-25 of spec/08). An uncapped predicate with no capped ancestor has an empty list.
3. **Capped by.** A residual `ρ` is **capped by** a set `E` in package `P` if some `E' ∈ caps(ρ)` satisfies `E' ⊑ₚ E` (§8.4). One cap suffices: every instantiation of the enclosing generic is checked against every cap in the list (§8.5.4), so the latent effects behind `ρ` are permitted by each of them, and `⊑ₚ` is transitive (spec/08 §8.2.4).
4. **Where residuals arise.** Rows 4 and 16 of §8.3.2 add the selected predicate directly; a latent node adds it when its selection is `Param(p)` (§8.3.4). They propagate to the enclosing body through `full` edges only (§8.2.3), so `R(O(o))` is the set of residual bounds of operation `o` in the sense of spec/08 §8.8.2: those of the operation itself, those reached inside its latent expansion, and those of the closure or `async` block bodies it calls.

**Overflow.** A latent node whose expansion stops at the depth limit or at a solver overflow has a non-empty `ovf` constant (§8.3.4 items 3 and 4). An operation `o` **overflows** if a `Λ` node with an `ovf` constant is reachable from the `Λ` references of `O(o)` through `Λ`-to-`Λ` edges only; references through `I(·)` and `Inst(·)` nodes are excluded, so that the error is reported once, at the operation whose own expansion overflows, and not again at every operation that calls its body (N-D-75).

- For each overflowing operation, in operation order, the analysis reports `INSTANTIATION_DEPTH_EXCEEDED` if `depth` is reached and `TRAIT_SOLVER_OVERFLOW` if `solver` is reached (both if both are), at the operation's span, with the data that [impl/03](03-name-resolution-and-type-checking.md) and [impl/04](04-mir.md) §4.13 define for these codes. A note names the goal at which the limit was hit and lists the chain of goals from the bound of the callee to that goal: the shortest path in the `Λ` subgraph, with ties broken by comparing the printed goals byte-wise, element by element.
- The body containing the operation is marked incomplete. `ovf` propagates like `inc`, so every check that depends on the body is suppressed (§8.1.9, cause key C5 of [impl/11](11-diagnostics.md) §11.11.4).
- When the same build later monomorphizes the instantiation, [impl/04](04-mir.md) §4.13 would report the same `INSTANTIATION_DEPTH_EXCEEDED` at the same call; a build with an effect error does not reach monomorphization ([impl/01](01-architecture.md) §1.2.3), and the deduplication of impl/11 §11.11.2 removes the repetition in any case.

### 8.3.6 Trait default bodies, derived bodies and `async fn` bodies

1. **Default bodies.** A trait method with a default body is the effect body `Fn(owner)` of the trait item. Its environment is the trait's, with `Self: B<Ḡ>` first (impl/03 §3.14.1 item 1), so every call of a method of `B` on `self` selects `Param` and is a residual (spec/08 §8.8.4). When an impl uses the default body, the residual is re-expanded with the impl's self type by the trait-predicate references of §8.3.4, and the cycle through `Self: B` is solved by the SCC computation.
2. **Derived bodies.** A `Derived(owner, m)` body is analyzed like any other body: its MIR is the shim that impl/04 builds, its environment is the generated header of impl/03 §3.8.6, and the field calls in it select the impls of the field types, or `Param` for fields of a type-parameter type. Derived `Clone` of a type with a `String` field therefore has `{ alloc }`, latent effects for its parameters, and no clause.
3. **Compiler-provided impls** (impl/03 §3.8.5) have no effect bodies. Their effects are those of their nested obligations (row 8 of §8.3.2, the `Builtin` rows of §8.3.4), and the function-trait and `Future` impls of function items, function pointers, closures and futures are defined by the callee rules.
4. **`async fn` bodies.** The operations of `Fn(f)` for an `async fn` `f` are those of the owner's own MIR body, which creates the future, followed by those of the synthesized `async` block (§8.1.5), both in phase `DropElaborated`, before the async transform. The operation order of §8.3.1 numbers the owner's operations first and the block's after them, so "the first contributing operation" is well defined. The `Await` terminators of the block are classified by rows 14–17 (spec/08 §8.10). The own set is checked against the clause of `f` like every function (the clause bounds the body, not the call). The async transform ([impl/04](04-mir.md) §4.10) runs after phase `Analyzed` and never changes an effect result.
5. **Refinement shims.** A call of `R.try_from` ([impl/03](03-name-resolution-and-type-checking.md) §3.13, N-C-41) is a call of a compiler shim whose signature set is `∅`. It contributes nothing; it has a reachability edge to the refinement predicate of `R` (§8.7.3), whose effect set is `∅` by §8.5.6.

### 8.3.7 Predicate sites

```rust
pub enum PredicateSiteId {
    Clause(OwnerId, ContractClauseId),   // a requires or ensures clause of a function (impl/03 contract_clauses)
    Invariant(OwnerId),                  // the invariant of a type
    Refinement(OwnerId),                 // the refinement predicate of a refined type or distinct type
}
```

A predicate site has no effect body. Its operations are taken from its typed HIR and the owner's `TypeckResults` ([impl/03](03-name-resolution-and-type-checking.md) §3.15), in **HIR order**: by the start offset of the operation's span, then by `ItemLocalId`. The operations are:

| HIR form | Classified as |
|---|---|
| a call with `call_kinds[e] = Direct(def, args)` | `Call` with `func: FnDef(def, args)` |
| a call with `call_kinds[e] = FnPtr` | `Call` with `func` of the callee's function pointer type |
| a call with `call_kinds[e] = FnTrait { kind, method }` | `Call` of the function-trait method on the callee's type |
| a method call, through `type_dependent_defs[e]` | `Call` of the selected item with its instantiated arguments |
| an operator, comparison or indexing expression resolved to a trait method | `Call` of that method |
| an f-string | one `Call` of `Format.format` per interpolated value, with `Self` the value's type (impl/03 §3.7.13), and one `Call` of each `String` building function that the f-string desugaring of spec/06 names |
| a `for` loop | the calls of the iterator protocol that [impl/02](02-frontend-ast-hir.md) desugars it into |
| a path to a function item outside callee position, with no adjustment | `FnItemValue` of `FnDef(def, node_args)` |
| an expression with a `ReifyFnPointer` adjustment (impl/03 C-FnPtr) | `Reify` of the expression's `FnDef` type to the adjusted function pointer type |

The forms that have no counterpart here (closures, `.await`, `?`, `unsafe`, assignments) are rejected in predicates by `INVALID_CONTRACT_EXPRESSION` ([spec/09](../spec/09-contracts-and-refinements.md) §9.1.2). Each operation is classified by §8.3.2 and gets a node `O(o)`; the site's node `S(p)` references all of them. The site is analyzed in the environment of its owner (for an `ensures` clause, the function's; for a refinement, the refined type's, which has no parameters, impl/03 §3.13). Implicit drops in a predicate contribute nothing, as everywhere.

The predicate code that [impl/04](04-mir.md) inserts into function bodies is the same code: the HIR analysis decides its effects, and the MIR copy is skipped by §8.3.1 except for reachability.

### 8.3.8 Foreign packages and metadata

A package of the build other than the root and the workspace members is read from its metadata ([impl/01](01-architecture.md) §1.6.5). This chapter contributes the following record to it:

```rust
pub struct EffectMetadata {
    pub sigs: Vec<SigFacts>,                               // every function of the package, DefPathHash order
    pub covers: Vec<(DefPathHash, PersistentEffectSet)>,   // covers(u) of every effect item, DefPathHash order
    pub bodies: Vec<BodyFacts>,                            // every effect body, stable key order
    pub predicate_reach: Vec<(PersistentSiteId, Vec<PersistentReachEdge>)>,
    pub footprint: Footprint,                              // §8.7.10
    pub unsafe_entries: Vec<UnsafeEntry>,                  // §8.10.4
}

pub struct BodyFacts {
    pub body: PersistentBodyRef,          // DefPathHash of the DefId, and the method name for Derived bodies
    pub own: PersistentEffectSet,         // A(I(b))
    pub reach: Vec<PersistentReachEdge>,  // §8.7.3, resolved within the package
    pub why: Vec<WhyEntry>,               // one per atom of own, in internal order (§8.6.3)
}

pub struct SigFacts {
    pub def: DefPathHash,
    pub set: PersistentEffectSet,         // Sig(f)
    pub declared: bool,                   // f has a clause
    pub refs: Vec<(PersistentAtom, PersistentLocation)>,  // for a clause: the EffectRef of each atom, in internal order
}

pub struct WhyEntry {
    pub atom: PersistentAtom,
    pub dist: u32,                        // length of the shortest chain from the body to a terminal (§8.6.2)
    pub first: PersistentLink,            // the first link of that chain, chosen by the tie-break of §8.6.2
}

pub struct PersistentLink {
    pub relation: ChainRelation,          // Calls | Instantiates | Awaits | Imports | Declares (impl/11 §11.8.2)
    pub location: Option<PersistentLocation>,
    pub next: PersistentLinkTarget,
}

pub enum PersistentLinkTarget {
    Body(PersistentBodyRef),              // a body without a clause: continue with its WhyEntry
    Clause { def: DefPathHash, atom_location: PersistentLocation },  // a function with a clause: Declares at the EffectRef
    Import { def: DefPathHash, wit_location: Option<PersistentLocation>,
             iface: Symbol, iface_location: Option<PersistentLocation> },
    Extern { def: DefPathHash, location: PersistentLocation },
    FnPtr { printed_type: Arc<str> },
    AllocBoundary { def: DefPathHash, wit_location: Option<PersistentLocation> },
}
```

`PersistentLocation` is a byte span in a file named relative to the package root, which the consumer renders as `dep:<ns>:<pkg>@<ver>/<path>` ([impl/11](11-diagnostics.md) §11.5.4). The `refs` of `SigFacts` and the `why` tables are what chains into foreign functions need (§8.6.3); nothing else of the foreign graph is stored (N-D-76).

1. Metadata is written only for a package whose analysis reported no error (impl/01 §1.6.5 item 7). Every foreign `Sig`, `Own` and edge is therefore complete, and a consumer never sees an incomplete foreign value.
2. A consumer reads `Sig` of a foreign function as a constant (§8.2.6) and `covers` of a foreign effect for `covers*` (§8.4.1) and for `caps` (§8.7.5). It never builds nodes for foreign bodies: closure and future types of a foreign package cannot occur in the consumer's code, because they cannot be named in signatures, and references to foreign named functions are `atoms` references, whose value is the constant `Sig`.
3. The reachability edges of a foreign body are independent of the consumer: they are computed in the body's own environment, every latent expansion in it that reaches a type parameter of the body stops there (§8.7.3), and every concrete selection in it is final, because a dependent cannot add an impl that a dependency's concrete goal could select (coherence, spec/05 §5.10).
4. The `why` table lets a consumer extend a chain into a foreign function that has no clause without the foreign graph (§8.6.3).
5. The values are equal whether decoded or computed (impl/01 §1.6.5 item 6). The persistent forms use the stable keys of impl/01 §1.6.2 and the atom encoding of §8.1.6 item 5.

---

## 8.4 Permits

This section is the decision procedure that spec/08 §8.2.4 cites. It decides `D ⊨ₚ a` and `A ⊑ₚ D` for a declared set `D` and the package `P` of the check.

### 8.4.1 Covers closure

`covers_star(u)` is `covers*(u)` of spec/08 §8.4.1: `u`, the atoms of `covers(u)`, and `covers_star(v)` for every user effect `v ∈ covers(u)`, whatever the package of `v`. It is computed by a depth-first traversal over the `covers` edges, using `effect_def` for local effects and the metadata `covers` for foreign ones, and interned. On a cycle (already reported as `EFFECT_COVERS_CYCLE`, §8.1.7), every member of the cycle gets the union over the cycle's SCC, so that the query terminates and its result is order-independent; the program is rejected anyway.

The **expanded set** of `D` in `P` is

```
D*(D, P) = D ∪ ⋃ { covers_star(u) | u ∈ D a user effect whose defining package is P }
```

User effects of other packages are not expanded (opacity, spec/08 §8.4.3), but the expansion of an own effect continues into the `covers` of foreign effects that it names, as the definition of `covers*` requires.

### 8.4.2 The permit index

```rust
pub struct PermitIndex {
    pub direct: EffectSetId,      // D
    pub expanded: EffectSetId,    // D*(D, P)
    pub wit_pkgs: Arc<[Symbol]>,  // { cap(e) | e ∈ D* a built-in effect, cap(e) a WIT package }, sorted by bytes
    pub wasi_io: bool,            // some member of wit_pkgs is in the wasi namespace (spec/08 N-D-100)
}
```

`permit_index(D, P)` builds the index once per pair and interns it. The decision procedure is

```
permits(ix, a) =  a ∈ ix.expanded
               ∨  (a is an Interface atom ns:pkg/name  ∧  ns:pkg ∈ ix.wit_pkgs)
               ∨  (a is an Interface atom wasi:io/name  ∧  ix.wasi_io)
```

**Lemma (correctness).** `permits(permit_index(D, P), a)` holds iff `D ⊨ₚ a` (spec/08 §8.2.4).

*Proof.* (⇐) Rule 1: `a ∈ D ⊆ D*`. Rule 3: `a ∈ covers*(u) ⊆ D*` for an own `u ∈ D`. Rule 2: `e ∈ D ⊆ D*` is built-in and `a` is an interface of `cap(e)`, so `cap(e) ∈ wit_pkgs` and the prefix of `a` is `cap(e)`; or `a` is an interface of `wasi:io` and `cap(e)` is in the `wasi` namespace (rule `P-Io` of spec/08 §8.15.4), so `wasi_io` holds. Rule 4: `e ∈ covers*(u) ⊆ D*` for an own `u`, and the same argument applies. (⇒) If `a ∈ D*`, then `a ∈ D` (rule 1) or `a ∈ covers*(u)` for an own `u ∈ D` (rule 3). If the prefix of an interface atom `a` is `cap(e)` for a built-in `e ∈ D*`, or `a` is an interface of `wasi:io` and `wasi_io` holds, so that `cap(e)` is in the `wasi` namespace for a built-in `e ∈ D*`, then `e ∈ D` (rule 2) or `e ∈ covers*(u)` for an own `u ∈ D` (rule 4). ∎

World-import atoms (`ns:pkg/world`) are interface atoms (§8.1.6 item 4) and are treated by the same prefix test.

**Complexity.** Built-in atoms are tested with the bitmask of `EffectSet`, other atoms by binary search in the sorted `others` of `D*` and in `wit_pkgs`. A test costs `O(log |D*|)`, so `A ⊑ₚ D` costs `O(|A| · log |D*|)` after the index is built, and building it costs `O(|D*| log |D*|)`. This is within the bound "linear in the size of `A` times the size of the expanded `D`" that spec/08 §8.2.4 states.

### 8.4.3 Derived operations

1. `missing(ix, A)` returns the atoms of `A` that `ix` does not permit, in internal order. `A ⊑ₚ D` iff `missing` is empty.
2. **Single-atom witnesses.** Every rule of spec/08 §8.2.4 uses exactly one member of `D`, so `D ⊨ₚ a` iff `{d} ⊨ₚ a` for some `d ∈ D`. `witnesses(ix, a)` returns these `d` in internal order. It is used by `UNUSED_EFFECT_DECLARATION` (§8.5.8), which is therefore independent of the order in which the clause lists its atoms, and by the `expansion` data of chains (§8.6.4).
3. Every check names the package `P` whose permits relation it uses: the package of the function for clauses (§8.5.2), of the impl for trait impl bounds (§8.5.3), of the call site for bound caps (§8.5.4), of the coercion for function pointers (§8.5.5), and of the predicate's owner for predicates (§8.5.6).

---

## 8.5 Checking

This section implements spec/08 §8.6.3, §8.6.4, §8.8.3, §8.9 and §8.11 on the least solution of §8.2. Every check reads only solution values `val(n)`, `Sig` values, resolved clauses and permit indexes; none of them changes the solution, so the checks of different bodies are independent and may run in any order or in parallel.

### 8.5.1 Order of checks and the first contributing operation

`effect_check(b)` runs the following checks for one effect body `b`, if its summary is complete (§8.1.9 rule 3), and returns their diagnostics in this order; the driver sorts all diagnostics by [impl/11](11-diagnostics.md) §11.11.1 afterwards:

1. the clause check (§8.5.2), if `b` has a clause;
2. the trait impl check (§8.5.3), if `b` is a trait impl method or a `Derived` body;
3. the bound-cap check (§8.5.4), for every instantiating operation of `b` (§8.3.1), in operation order;
4. the coercion check (§8.5.5), for every `Reify` of `b`, in operation order;
5. the clause-requirement check (§8.5.7), if `b` is a `Fn` body without a clause;
6. the unused-declaration check (§8.5.8), if `b` has a clause.

`predicate_effect_check(p)` runs the bound-cap check and the coercion check for the operations of predicate site `p`, and then the predicate check (§8.5.6).

**First contributing operation.** For an atom `a ∈ A(I(b))`, the **first contributing operation** of `a` in `b` is the first operation `o` in operation order (§8.3.1) with `contributes(o)` and `a ∈ A(O(o))`. It exists, because `I(b)` has no atom constants and references exactly the contributing operations (§8.2.2), so `A(I(b))` is the union of their `A(O(o))` (§8.2.3); if it does not exist, the solution is corrupt and the compiler reports `INTERNAL_COMPILER_ERROR`. For a predicate site the same definition is used with HIR order (§8.3.7). This is the operation that spec/08 §8.9.1 and §8.11 call "the first operation that contributes it": MIR order for bodies, because the MIR is the analyzed form, and source order for predicates, which have no MIR of their own.

**Grouping.** Several missing atoms may have the same first contributing operation. A check that reports at the first contributing operation reports **one diagnostic per operation**, carrying every atom whose first contributing operation it is, rather than one diagnostic per atom (N-D-70). The diagnostics of one body are therefore ordered by operation, and each primary location is distinct.

### 8.5.2 Declared clauses: `EFFECT_NOT_DECLARED`

For an effect body `b` with a clause `D = D(b)` in package `P`, where `P` is the package of `b` (for a trait default body, the package of the trait), let `ix = permit_index(D, P)` and `M = missing(ix, A(I(b)))` (§8.4.3). If `M` is empty, the check passes. Otherwise:

1. For every operation `o` that is the first contributing operation of at least one atom of `M`, in operation order, one `EFFECT_NOT_DECLARED` diagnostic is reported. Let `M_o ⊆ M` be the atoms whose first contributing operation is `o`.
2. **Message.** "`` `<f>` has effect `<a>`, which its `effects` clause does not permit ``" if `|M_o| = 1`, and "`` `<f>` has effects `{ a, b }`, which its `effects` clause does not permit ``" otherwise, with the atoms of `M_o` in canonical order (§8.1.8, display form outside fixes). `<f>` is the function's path as printed by [impl/11](11-diagnostics.md) §11.6.1; for a closure or `async` block inside a function with a clause, the check concerns the enclosing function, because closures have no clause (spec/08 §8.6.1), so `b` is never a closure.
3. **Locations.** The primary location is the span of `o` (§8.3.1), with the label "`` `{ … }` comes from this call ``" (`this .await` for an `Await`, `this operator` for a call desugared from an operator, as recorded by impl/04 in the terminator's source info). A secondary label at the clause reads "`` declared `effects { … }` ``", with `D` in canonical order.
4. **Data.** `{ function, missing, declared }`: the function's canonical path, the atoms of `M_o` and the atoms of `D`, as display strings in canonical order.
5. **Chains.** One `effect_chain` entry per atom of `M_o`, in canonical order, whose first link is fixed to `o` (§8.6.2 item 4).
6. **Fix.** A help "add the missing effects to the clause" with the fix `set_effects { effects: D ∪ M }`, spelled in the module of `b` (§8.1.8), in canonical order. Every diagnostic of `b` carries the same fix with all of `M`, not only `M_o`, so that applying any one of them repairs all of them. The fix is `maybe-incorrect`, because it widens `Sig(b)` and can therefore create errors in callers, and because the intended repair may be to change the body ([impl/11](11-diagnostics.md) §11.7.2). If some atom of `M` is not spellable in the module of `b`, or if `b` is `pub` and some atom of `M` is a non-`pub` user effect (which would be `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE`), no fix is offered and the help names the atoms in display form, with the `use` declaration that would make them spellable where one exists.
7. **Cause key.** `C4(b)`, because the condition compares with `b`'s signature, and `C4(g)` for the callee `g` of `o` when `o` is a call of a function item, because the contribution is `g`'s signature ([impl/11](11-diagnostics.md) §11.11.4). If `b`'s clause is incomplete (§8.1.7), the check is not run at all.

A trait default body is checked here against the trait method's clause, which is its `D` (§8.3.2 note 2). A `Drop.drop` impl method with a clause is checked here like every function with a clause; §8.5.3 adds the checks specific to `Drop`.

```sai
fn read_config(path: borrow String) -> String
    effects { fs.read }
{
    let text = fs.read_to_string(path) // fs.read: permitted
    return f"config: {text}" // rejected: EFFECT_NOT_DECLARED; the f-string needs alloc
}
```

### 8.5.3 Trait impl methods: `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` and `DROP_HAS_EFFECTS`

Let `b` be a trait impl method body (an impl item, or a `Derived` body) of method `m` of trait `B`, `P` the package of the impl, and `D_B` the resolved clause of `B.m`, if any. The check uses `ix_B = permit_index(D_B, P)`: the impl's package decides `⊑ₚ`, so user effects of the trait's package are opaque to an impl in another package (spec/08 §8.4.3, §8.6.3).

**`Drop.drop`.** If `B` is the lang item `Drop`, `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` is never reported; `DROP_HAS_EFFECTS` replaces it (spec/08 §8.6.3):

1. If the impl method has a clause with at least one atom, one diagnostic at the clause, with the message "`` `drop` of `<T>` declares effects; `Drop.drop` is `effects {}` ``" and `data { type, effects }`. If `A(I(b)) = ∅`, it carries a fix `remove_node` of the clause, `maybe-incorrect` (replacing the clause by `effects {}` is equally correct).
2. Otherwise, if the impl method has no clause and `A(I(b)) ≠ ∅`, one diagnostic per first contributing operation, grouped as in §8.5.1, with the message "`` `drop` of `<T>` has effect `<a>` ``" (or `effects { … }`), `data { type, effects }`, one `effect_chain` per atom, and no fix.
3. Independently, for every operation `o` of `b` with `contributes(o)` or of kind `Reify`, in operation order, such that `R(O(o))` contains a residual whose trait has at least one method (every `Fn`-family and `Future` residual is one), one diagnostic at `o`, with the message "`drop` of `<T>` calls through a type parameter" and `data { type, residuals }`, listing the offending residuals printed as predicates in elaboration order. Rows 2 and 21 of §8.3.2 are not checked: a function item value created in `drop` has its latent effects attributed where it is called or coerced, which is itself an operation of `b` or of an instantiating caller that is checked. This is N-D-22 of spec/08: drop glue has no instantiating call site, so no latent effect may remain.

**Other traits.** If `B.m` has no clause, there is no bound and the check passes (spec/08 §8.6.3). Otherwise:

1. **Impl method with a clause `D_I`.** Let `X = missing(ix_B, D_I)`. If `X ≠ ∅`, one `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` diagnostic is reported. Its primary location is the first `EffectRef` of the impl's clause, in source order, whose atom is in `X`; every further such `EffectRef` gets a secondary label "not permitted by the trait". The message is "`` `<T>.<m>` declares effects that `<B>.<m>` does not permit ``". A `declared_here` entry points at the trait method's clause (in `dep:` form for a foreign trait), with `what` "`` the clause of `<B>.<m>` ``". `data { method, trait_method, excess, bound }` with `excess = X` and `bound = D_B`. If `A(I(b)) ⊑ₚ D_B`, a fix `remove_node` of the impl's clause is offered, `maybe-incorrect`; otherwise no fix. The check uses the clause, not the body (spec/08 §8.6.3).
2. **Impl method without a clause.** `Sig(b) = A(I(b))`. Let `X = missing(ix_B, A(I(b)))`. If `X ≠ ∅`, one diagnostic per first contributing operation of the atoms of `X`, grouped (§8.5.1), with primary location at the operation, the message "`` `<T>.<m>` has effect `<a>`, which `<B>.<m>` does not permit ``" (or `effects { … }`), the same `declared_here` entry and `data`, one `effect_chain` per atom, and no fix: the repair is a change of the body or of the trait.
3. **`Derived` bodies.** A derived method has no clause and is checked by item 2. The operations of its shim carry the span of the trait path in the `@derive` attribute ([impl/04](04-mir.md) builds the shim with that span), which is therefore the primary location; step 0 of its chains is named `<T>.<m>`.

The cause key is `C4(B.m)` and `C2` when a type of the impl header contains the error type; impls whose header failed to type-check have no effect bodies.

### 8.5.4 Bound caps: `BOUND_EFFECTS_EXCEEDED`

A **capped reference** is a `full` reference to a node `Λ(k)` that was created for a bound with a non-empty cap list `caps`:

- a `BoundRef` of an instantiating operation (§8.3.1) whose bound has caps;
- inside a `Λ` node with an `Impl` source (§8.3.4), the reference for an impl predicate `q[args]` or a trait predicate `q[Self := …]` whose `q` has caps;
- inside a `Λ` node with a `Builtin` function-trait or `Future` source on a function item, the bound references of `Contrib(g, ḡ)` whose bounds have caps.

For an instantiating operation `o` of a body or predicate site in package `P`, the **checked references** of `o` are the capped references reachable from the `BoundRef`s of `o` through edges between `Λ` nodes only, including the `BoundRef`s themselves; references through `I(·)` and `Inst(·)` nodes are excluded (as for overflow, §8.3.5), because an impl or function body that is reached is checked at its own operations. Every checked reference is visited once per operation, in depth-first preorder of the references, which is deterministic because references are ordered (§8.2.4).

For each checked reference to `Λ(k)` with caps `E₁, …, Eₙ` (in elaboration order) and the first `Eᵢ` for which one of the following fails, one diagnostic `BOUND_EFFECTS_EXCEEDED` is reported at `o`:

1. `missing(permit_index(Eᵢ, P), A(Λ(k))) = ∅`;
2. every residual `ρ ∈ R(Λ(k))` is capped by `Eᵢ` in `P` (§8.3.5 item 3).

Checking the first failing cap only is sufficient for completeness, because the diagnostic is an error: after the program is changed, the next run checks the remaining caps (N-D-78).

- **Message.** "`` the instantiation of `<g>` exceeds the effect cap of bound `<W: B>` ``". `<g>` is the instantiated function; for a capped impl or trait predicate, `<W: B>` is that predicate and the note "required by `impl <…>`" names the impl.
- **Data.** `{ function, callee, bound, cap, missing, residuals }`: the calling body, `<g>`, the bound printed as a predicate after substitution, `Eᵢ`, the atoms that `Eᵢ` does not permit (canonical order) and the residuals that it does not cap (printed predicates).
- **Related.** A `declared_here` entry at the cap of the bound (in `dep:` form for a foreign signature), with `what` "`` the cap of `<W: B>` ``", and one `effect_chain` per missing atom, starting at the calling body with an `Instantiates` link at `o` into `Λ(k)` (§8.6.2).
- **Help and fix.** For each uncapped residual `ρ` whose origin is a bound written on the signature of the calling owner without any cap, a help "add `effects { … }` to the bound `ρ`" with a fix `insert_after` of the bound's node with text `` effects { Eᵢ } `` spelled in the owner's module, `maybe-incorrect` (it restricts every caller of the owner). If `ρ` comes from an elaborated supertrait or associated-type bound, or `Eᵢ` is not spellable, the help names the bound and the cap without a fix. For missing atoms, the help names them and no fix is offered.
- **Package.** `P` is the package of the call site (spec/08 §8.8.3).
- **Cause key.** `C4(g)`, and `C2` if a type argument contains the error type.

### 8.5.5 Coercions: `FN_POINTER_EFFECTS_MISMATCH`

For every `Reify` operation `o` of `FnDef(g, ḡ)` to a function pointer type with effect set `E`, in package `P` (the package of the coercion), the check requires `missing(permit_index(E, P), A(O(o))) = ∅` and that every residual of `R(O(o))` is capped by `E` (N-D-24 of spec/08). `O(o)` is `Contrib(g, ḡ)` (row 20 of §8.3.2), which is `Sig(g) ∪ Lat(g; ḡ)` with its residuals. If the check fails, one diagnostic is reported at the span of the coerced expression:

- message "`` `<g>` has effects that the function pointer type `<fn(..) effects E>` does not permit ``";
- `data { function, pointer_effects, missing, residuals }`;
- one `effect_chain` per missing atom, whose step 0 is the body containing the coercion and whose first link is the `Calls` link of the coercion to `<g>`, or an `Instantiates` link into a latent node (§8.6.2);
- a help naming the missing atoms or uncapped residuals; no fix, because both changing the pointer type and changing the function change other code.

Closures never coerce to function pointer types ([spec/05](../spec/05-types.md), impl/03 C-FnPtr), so `Reify` of a closure type does not occur; the closure rule of spec/08 §8.6.4 has no instance in v0.1. Function pointer types are compared as sets for type identity (N-D-19 of spec/08); that is a type check of impl/03 and not part of this check.

### 8.5.6 Predicates: `EFFECTFUL_PREDICATE`

For a complete predicate site `p` with owner package `P` (the package of the function, type or refined type), the check requires `A(S(p)) = ∅` and that every residual of `R(S(p))` is capped by `∅` in `P`. A cap `E'` satisfies `E' ⊑ₚ ∅` only if `E' = ∅`, so a residual is accepted only if one of its caps is `effects {}` (spec/08 §8.11). If the check fails:

1. One diagnostic is reported per site, at the first operation `o` of `p` in HIR order with `A(O(o)) ≠ ∅` or with a residual in `R(O(o))` that has no empty cap.
2. Message: "`` <site> has effects; predicates must be pure ``", where `<site>` is "`` the `requires` clause of `<f>` ``", "`` the `ensures` clause of `<f>` ``", "`` the invariant of `<T>` ``" or "`` the refinement of `<R>` ``".
3. `data { site, effects, residuals }`: the site in the form of step 0 below, `A(S(p))` in canonical order, and the offending residuals as printed predicates.
4. One `effect_chain` per atom of `A(S(p))`, in canonical order. Step 0 is named `requires of <f>`, `ensures of <f>`, `invariant of <T>` or `refinement of <R>`, and its symbol is that of `<f>`, `<T>` or `<R>`.
5. For each offending residual, a help "add `effects {}` to the bound `ρ`" without a fix; no fix is offered for atoms (spec/08 §8.11).

A predicate site is incomplete if its owner has a type error (§8.1.9 rule 1); the check is then not run. A refinement shim call `R.try_from` in a predicate is a call with `Sig = ∅` (§8.3.6 item 5), so refinements may use other refined types.

### 8.5.7 Clause requirements: `MISSING_EFFECTS_DECLARATION` and `PUBLIC_EFFECTS_INFERRED`

For every complete effect body `b = Fn(f)` of a function `f` without a clause, with `Inf(f) = A(I(f)) ≠ ∅`, the function is classified by the first of the following that applies:

| # | Condition | Reason | Code |
|---|---|---|---|
| 1 | `f` is the default body of a trait method or a trait impl method (closures, `async` blocks, `Derived` bodies and glue are not `Fn` bodies and are never classified) | — | none (N-D-7 of spec/08) |
| 2 | `f` is an `export fn` | `export` | `MISSING_EFFECTS_DECLARATION` |
| 3 | `f` is exposed through a world or an interface (spec/08 §8.9.3: a function of an `impl` of an interface exported by a world of the package, or a root-module function implementing a world's `export FnHead`) | `world` | `MISSING_EFFECTS_DECLARATION` |
| 4 | `f` is declared `pub` and the major version of `f`'s package is at least 1 | `pub` | `MISSING_EFFECTS_DECLARATION` |
| 5 | `f` is declared `pub` and the major version is 0 | `pub` | `PUBLIC_EFFECTS_INFERRED` |
| 6 | otherwise | — | none |

Exactly one diagnostic is reported per function (spec/08 §8.9.3):

- **Message.** "`` exported function `<f>` does not declare its effects ``" (`export`), "`` function `<f>` exported through `<world>` does not declare its effects ``" (`world`, naming the world or interface), "`` public function `<f>` does not declare its effects ``" (`pub`, error), and "`` public function `<f>` has inferred effects ``" (`PUBLIC_EFFECTS_INFERRED`).
- **Location.** The primary location is the function's name, with the label "`` inferred effects: `{ … }` ``" ([impl/11](11-diagnostics.md) §11.10.9).
- **Data.** `{ function, inferred, reason }`, with `inferred` in canonical order and `reason` one of `export`, `world`, `pub`.
- **Chains.** One `effect_chain` per atom of `Inf(f)`, in canonical order, starting at `f`.
- **Fix.** A help "declare the inferred effects" with the fix `set_effects { effects: Inf(f) }`, spelled in the module of `f`, in canonical order. The fix is `machine-applicable`: after it, `D(f) = Inf(f)`, so `Sig(f)` and every caller's set are unchanged, `Own(f) ⊑ₚ D(f)` holds by rule 1 of permits, and the members of `f`'s SCC see the constant `D(f)` instead of the equal value `A(I(f))`. It is not offered if some atom is not spellable in `f`'s module, or if `f` is `pub` and `Inf(f)` contains a non-`pub` user effect (the inserted clause would be `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE`); the help then names the atoms in display form and, in the second case, the effect that must be made `pub`.
- **Cause key.** `C5(f)` ([impl/11](11-diagnostics.md) §11.11.4).

The version is the version of the `package` declaration of `f`'s own `sai.package` (for a workspace member, the member's). Tests and properties are not `pub` and are never reported.

### 8.5.8 Unused declarations: `UNUSED_EFFECT_DECLARATION`

For every complete effect body `b` with a complete clause `D` of a function `f` that is neither `pub`, nor `export`, nor exposed through a world or an interface (rows 2–5 of §8.5.7 do not apply), and that is not a trait method declaration, the check computes

```
used = ⋃ { witnesses(ix, a) | a ∈ A(I(b)) }        with ix = permit_index(D, P)
```

(§8.4.3 item 2) and reports, for every atom `d ∈ D \ used`, the warning `UNUSED_EFFECT_DECLARATION` at the first `EffectRef` of the clause whose atom is `d` (later duplicates are already `DUPLICATE_EFFECT`, §8.1.7), with the message "`` effect `<d>` is declared but not needed by `<f>` ``", `data { function, effect }` and the fix `remove_node` of that `EffectRef`. The fix is `machine-applicable` if the `targets` list of the package ([package/01](../package/01-package-contract.md) §1.8) has exactly one element, and `maybe-incorrect` otherwise: the atoms that a body uses can depend on `@target` predicates, and the check sees only the target of the command (N-D-95).

- **Correctness.** By the single-atom witness property, `d` is unneeded exactly when no atom of `Own(f)` is permitted by `{d}`; then `Own(f) ⊑ₚ D \ {d}` still holds, and since `⊑ₚ` uses one witness per atom, removing all unused atoms at once preserves it as well. The result does not depend on the order of the clause (N-D-71).
- **Why the fix is safe.** Removing `d` narrows `Sig(f)`. No check of spec/08 is violated by a narrower signature set: callers' own sets shrink, a narrower set still satisfies every cap, trait bound and pointer type it satisfied before, and `f` is not `pub`, so no other package depends on it. A clause from which every atom is removed stays as `effects {}`, which keeps the function's declared purity explicit.
- Trait impl methods with a clause are checked: they are not `pub` (N-D-7 of spec/08). Trait method declarations are not: their clause bounds every impl, and an atom unused by the default body may be needed by an impl.
- **Cause key.** `C5(f)` (the code is a lint, default `warn`).

### 8.5.9 Incomplete values and cascades

1. A body whose `TypeckResults` contain the error type in any expression type, or that is `tainted_by_errors`, has an incomplete summary (§8.1.9) and is not checked. Its errors poison `C5(f)`, and `C4(f)` if its signature contains the error type.
2. A check of a complete body whose decisive value is incomplete is not run: the clause check of §8.5.2 if `inc(I(b))`, the impl check of §8.5.3 if `inc(I(b))`, the cap check of §8.5.4 for a checked reference whose `Λ(k)` is incomplete or overflowing, the coercion check if `inc(O(o))`, the predicate check if `inc(S(p))`, and §8.5.7 and §8.5.8 if `inc(I(b))`. The responsible error has been reported where the incompleteness originates (a type error in a callee body, an unresolved effect reference, an overflow), and impl/11 counts nothing as suppressed, because the diagnostic is never created (phase gating, impl/11 §11.11.4).
3. A function whose clause contains a failed `EffectRef` (§8.1.7) is not clause-checked and not checked for unused atoms; its callers use `Sig(f) = D(f)` with the refs that resolved, and the diagnostics comparing against it carry `C4(f)`, so they are dropped.
4. These rules never hide an error in a program that has no other error: a complete body whose solution values are complete is always checked.

---

## 8.6 Why-chains

This section is the algorithm that spec/08 §8.13.2 cites: it computes the shortest chain for an atom, with the deterministic tie-breaking of spec/08 §8.13.2, and maps it to the `ChainStep`s of [impl/11](11-diagnostics.md) §11.8.2.

### 8.6.1 The link graph of an atom

For an atom `a`, the **link graph** `G_a` has the following vertices:

| Vertex | Meaning | Terminal | `dist` of a terminal |
|---|---|---|---|
| `Body(b)` | an effect body of the current package with `a ∈ A(I(b))`; a glue body (§8.7.4) with `a = alloc` and `alloc` in its own set is terminal | glue only | 1 |
| `Site(p)` | a predicate site with `a ∈ A(S(p))` | no | — |
| `Op(o)` | a single operation, used as the start vertex when the first link is fixed | no | — |
| `Clause(g)` | a function with a clause and `a ∈ D(g)`, local or foreign | yes | 1 |
| `Foreign(g)` | a function of another package without a clause and `a ∈ Sig(g)` | yes | `dist` of its `WhyEntry` (§8.6.3) |
| `Import(g)` | an imported interface or world-import function `g` whose interface atom is `a` | yes | 2 |
| `Alloc(g)` | an imported function `g` with `a = alloc` and `allocates(g)` (§8.2.6) | yes | 1 |
| `Extern(g)` | a function of an `extern "wasm"` or `unsafe extern "C"` block, `a = host` or `a = ffi` | yes | 1 |
| `FnPtrTy(T)` | a function pointer type `T` whose effect set contains `a` | yes | 1 |

`dist` counts the steps from a vertex to the end of the chain, including the vertex's own step: an `Import` terminal produces two steps (the function and its interface), every other terminal one (§8.6.4).

The **links** out of `Body(b)` are the links of every operation `o` of `b` with `contributes(o)` and `a ∈ A(O(o))`; out of `Site(p)`, those of every operation of `p` with `a ∈ A(O(o))`; out of `Op(o)`, those of `o` alone. The links of an operation `o` are derived from the constants and references of `O(o)` (§8.3.2), keeping only those whose target contains `a`:

1. a constant `Sig(g)` from `Contrib(g, ·)`: a link to `Clause(g)` if `g` has a clause, otherwise to `Foreign(g)`;
2. an `atoms` reference to `I(g)`: a link to `Body(g)`;
3. a closure reference to `I(K)`, or the `atoms` part of `Inst(K, ḡ)`: a link to `Body(K)`;
4. a constant `E` of a function pointer call (rows 6 and 9): a link to `FnPtrTy(T)`;
5. the constants of an import call (rows 10 and 11): a link to `Import(g)` for the interface atom and to `Alloc(g)` for `alloc`;
6. the constant of an extern call (rows 12 and 13): a link to `Extern(g)`;
7. a `full` reference to a latent node `Λ(k)`, and the residual part of `Inst(K, ḡ)`: **collapsed links**. The search descends from `Λ(k)` through `Λ` and `Inst` nodes only, without producing steps, and every constant or reference of a visited node of the kinds 1–6 (for example `Sig(m_impl)` of a selected impl method, an `atoms` reference to the `I` of a local impl method, a closure reference, the `Contrib` of a function item behind a function-trait bound, a function pointer type) yields one link from `o`'s vertex to its target. The `Λ` subgraph has cycles (§8.3.4 item 6); the descent visits each node once.

Each link has a **relation**: `Awaits` for links 1–3 of an `Await` operation, `Instantiates` for collapsed links, and `Calls` for every other link, including those of a coercion, of an operator call and of an f-string call (spec/08 §8.13.1 treats these as call links). Its **location** is the span of `o`. Links with the same operation, target and relation are one link.

**Link key.** Links are compared by the key

```
key(l) = ( file of loc(l) in SourceRef form (impl/11 §11.5.4), byte offset of loc(l),
           SymbolId of the target (absent last), display name of the target (bytes),
           stable key of the target (DefPathHash, or the printed type of FnPtrTy),
           relation (Calls < Awaits < Instantiates) )
```

The first three components are those of spec/08 §8.13.2; the others make the order total, so that no two distinct links compare equal (N-D-80). A link without a location (only glue links of §8.7.4) sorts after every link with one.

### 8.6.2 The algorithm

`why_chain(s, a)` for a start vertex `s` (`Body(b)`, `Site(p)` or `Op(o)`) and an atom `a` contained in the value of `s`:

1. **Forward closure.** Collect the vertices `V_s` and links reachable from `s` in `G_a`. Links are generated on demand from the summaries and the solution; vertices of other packages are terminals, so the closure stays inside the current package.
2. **Distances.** Compute `dist(v)` for every `v ∈ V_s` by a multi-source shortest-path search on the reversed links, with all terminals of `V_s` as sources at their terminal `dist` (§8.6.1). Because every link has weight 1 and the source distances are small integers, a bucket queue indexed by distance is used (Dial's algorithm); every vertex is settled once, at its least `dist`. For a non-terminal `v`, `dist(v) = 1 + min { dist(w) | v → w }`.
3. **Existence.** `dist(s)` is finite: by the theorem of §8.2.3, `a` in the value of `s` is the constant of some node reachable from `s`, and every such constant is represented by a terminal (§8.6.1). An infinite `dist(s)` is `INTERNAL_COMPILER_ERROR`.
4. **Walk.** Starting at `v = s`, repeatedly choose, among the links `v → w` with `dist(w) = dist(v) − 1` the link with the least key, until a terminal is reached (a link into an `Import` terminal satisfies the condition with the terminal's `dist` of 2); then append the steps of the terminal (§8.6.4). For a start vertex `Op(o)`, the vertex is named by the body that contains `o`, so the chain starts at that body and its first link is one of `o`'s links: this is how the checks of §8.5 fix the first link to their primary operation.

**Theorem.** The walk produces a chain with the fewest steps among all chains from `s` to a terminal for `a`, and among those the least in the lexicographic order of link keys, which is the chain that spec/08 §8.13.2 specifies.

*Proof.* A chain from `s` of length `dist(s)` must, by the definition of `dist`, pass through vertices whose `dist` decreases by one per link, so every shortest chain is a sequence of links chosen as in step 4. Lexicographic comparison of two such chains is decided by the first link in which they differ; both links leave the same vertex, so choosing the least link at each vertex yields the least chain. Distinct links have distinct keys (§8.6.1). ∎

The restriction to `V_s` is an optimization: distances to terminals do not depend on the start vertex, so a single multi-source search over all of `G_a` gives the same `dist` values. The metadata of §8.6.3 is computed that way.

### 8.6.3 Chains through foreign functions

For every effect body `b` of a package and every atom `a ∈ A(I(b))`, `WhyEntry(b, a)` records `dist(Body(b))` in `G_a` and the first link of the walk from `Body(b)` (§8.3.8). The entries are computed when the package's metadata is written, by one multi-source search per atom over the whole graph (§8.6.2, last paragraph), and the first links by step 4.

A consumer's walk that reaches `Foreign(g)` continues with the entries of `g`'s package (query `foreign_why`): it appends the step of `g` with the recorded link, and continues at the link's target (`Body` with its own entry, or a terminal). The concatenated chain is the chain that the consumer would compute on the union of the graphs:

- the links out of a foreign vertex, and their targets, depend only on the foreign package and its dependencies, not on the consumer (§8.3.8 item 3), so the foreign graph is closed under the walk;
- the walk compares only links that leave the same vertex. All links of one body have locations in the body's file, so the file component of the key is equal among them in every rendering, and the remaining components are independent of how the file is named (package-relative in the producer, `dep:` form in the consumer). The foreign choice is therefore the consumer's choice;
- `dist` of a foreign vertex is its recorded `dist`, which is its distance in the union graph because no link leads from a foreign package back into a dependent one.

### 8.6.4 Mapping chains to steps

A walk visits vertices `v₀ = s, v₁, …, v_k` through links `l₁, …, l_k`, with `v_k` a terminal. Each non-terminal `vᵢ` becomes one step `ChainStep { name, symbol, location: loc(lᵢ₊₁), relation: rel(lᵢ₊₁) }`. The terminal becomes:

| Terminal | Steps |
|---|---|
| `Clause(g)` | `{ g, location: the EffectRef of a in g's clause, Declares }` |
| `Import(g)` | `{ g, location: the WIT declaration of g, Imports }`, then `{ i, location: the WIT declaration of the interface i (of the world, for a world-import function), Declares }` |
| `Alloc(g)` | `{ g, location: the WIT declaration of g, Declares }` |
| `Extern(g)` | `{ g, location: the written host or ffi EffectRef of g's clause, or g's name if it has none, Declares }` |
| `FnPtrTy(T)` | `{ name: the printed type T, symbol: none, location: none, Declares }` |
| `Body(glue)` | `{ export <iface>.<fn>, location: the WIT declaration of the exported function, Declares }` |
| `Foreign(g)` | the steps of the foreign walk (§8.6.3), with locations rendered in `dep:` form |

- **WIT locations.** A WIT file of a workspace package is named workspace-relative, a WIT file of a dependency in `dep:` form ([impl/11](11-diagnostics.md) §11.5.4). The WASI WIT files shipped with the toolchain have no source-reference form ([impl/11](11-diagnostics.md) §11.5.4); their locations are `none`, and the step is rendered without ` at <loc>`.
- **Relations.** `Calls`, `Awaits`, `Instantiates`, `Imports` and `Declares` are produced. `Covers` is never produced: spec/08 §8.13.3 does not make expansion links steps, and a chain ends at the function that declares the atom (N-D-80). Where an expansion matters (capability chains), it is in the diagnostic's `data.expansion` (§8.6.5). Reachability-only links (§8.7.3) map to `Calls`, except latent and instantiated-drop-glue edges (`Instantiates`) and `poll` edges (`Awaits`).
- **Names.** In diagnostics, a step's `name` is the item's path printed by the rule of [impl/11](11-diagnostics.md) §11.6.1 in the module of the diagnostic's primary location; in `sai inspect` output, it is the canonical path with the root package's prefix removed (`users.fetch_all`). Closures are named `{closure@<file>:<line>:<col>}`, `async` blocks `{async block@<file>:<line>:<col>}`, `Derived` bodies `<T>.<m>`, glue bodies `export <iface>.<fn>` (`export <fn>` for a world-level function export), interfaces by their identity, function pointer types by their printed type, and the pseudo-roots of §8.6.5 by `component <ns:pkg>`, `target <name>` and `export world <path>`.
- **Symbols.** `symbol` is the `SymbolId` of [impl/12](12-semantic-tooling.md) §12.2 of the item, and none for closures, `async` blocks, glue, function pointer types and pseudo-roots.
- **Truncation.** A chain of more than 32 steps is shortened as [impl/11](11-diagnostics.md) §11.8.2 prescribes (the first 16 steps, a `…` step, the last 15). The shortening is applied after the walk, so it never changes which chain is chosen.

```text
error[EFFECT_NOT_DECLARED]: `load` has effect `alloc`, which its `effects` clause does not permit
  --> src/store.sai:7:12
   = note: `load` has effect `alloc` because:
             `load` calls `decode` at src/store.sai:7:12
             `decode` calls `String.from_utf8` at src/codec.sai:22:5
             `String.from_utf8` declares `alloc` at dep:sai:alloc@0.1.0/src/string.sai:41:9
   = help: add the missing effects to the clause
```

### 8.6.5 Capability chains

A capability chain explains why a required capability `r` (§8.7.5) is reached from a root (spec/08 §8.13.1: it starts at a root and ends at the atom whose `caps` contain the capability). For a build `K`, a capability `r` and an **owner filter** `F` (the package `Q` for a per-package check of `Q`, and every package of the build for the root's policy check, the target check and `sai inspect capabilities`), the search runs on states:

- `R(f)` for every body `f` of the reachable set (§8.7.3); reach edges `R(f) → R(g)` have weight 1, the location of the operation that creates the edge, and the relation of §8.6.4; the **merged edge** from an export glue body to the function it calls (§8.7.4) has weight 0 and produces no step;
- a **switch** `R(f) → E_a(f)` of weight 0 if `owner(f) ∈ F` (§8.7.5), `a ∈ Own(f)` and `r ∈ caps(a)`;
- `E_a(v)` for the vertices of `G_a`, with the links and terminals of §8.6.1.

The distances are computed backwards: first `dist_a` in every `G_a` that has a switch (§8.6.2), then `distR(f) = min( exit(f), 1 + min { distR(g) | f → g } )` with `exit(f)` the least `dist_a(f)` over the switches at `f`, by a bucket-queue search on the reversed reach edges with initial values `exit(f)`; a vertex reached through a merged edge is inserted into the bucket being processed (0-1 bucket search), so every vertex is still settled once. The walk from a root `R(ρ)` chooses, at each `R(f)`, among the switch-and-link combinations and the reach edges that stay on a shortest path, the least by key (§8.6.1); at equal keys an `E` transition precedes an `R` transition, and `E` transitions are ordered by the internal order of their atoms. After a switch the walk continues as in §8.6.2. The atom of the switch is the chain's `effect`.

- **Diagnostics.** The chain of a capability diagnostic is the chain of the root with the least `(distR(ρ), canonical name of ρ, link keys)`. Roots are those of §8.7.2.
- **Inspection.** `sai inspect capabilities` lists every root `ρ` with a finite `distR(ρ)`, in byte order of the canonical names, each with its walk ([impl/13](13-cli.md)). The distances are computed once per `(K, r, F)`.
- **Expansion.** The diagnostic's `data.expansion` is the shortest path from the chain's atom to `r` in the graph whose edges are `u → v` for `v ∈ covers(u)` and `e → cap(e)` (ties by the internal order of atoms): a list of `{ from, to, relation }` with relation `covers` or `capability`, printed in display form. A built-in or interface atom gives one `capability` entry. The human renderer prints it as the note "`` `db.write` covers `net.tcp`, which requires `wasi:sockets` ``" when it contains a `covers` entry; otherwise the chain heading of impl/11 states it.
- **Sources without a body.** A required capability that no body contributes (§8.7.5 items 2–4) has a **pseudo-root** and no search: `component <ns:pkg>` for an import of a binary component, located at the root's `requires` entry for `ns:pkg`, or, when `ns:pkg` is an indirect dependency, at the root's `requires` entry of the least direct dependency, in identity-byte order, from which `ns:pkg` is reachable in the active graph; `export world <path>` for an interface of `TypeImp`, located at the `export world` declaration; `target <name>` for `memory64`, located at the element of `targets`, all in the root's `sai.package` ([impl/11](11-diagnostics.md) §11.4.3 requires these locations to be in that file). In `sai inspect capabilities` the pseudo-root's chain is one `Declares` step at that location. In a diagnostic, the source is a `declared_here` entry at the same location, with `what` "`` binary component `<ns:pkg>` imports `<r>` ``", "`` the types of the exports of world `<path>` use `<r>` ``" or "`` target `<name>` requires `memory64` ``", instead of a `capability_chain`, because no effect is involved (N-D-81).
- **Policy field.** The `policy` field of a `capability_chain` is set by the check that reports it: §8.7.6 for the policy checks, §8.7.7 for grants, §8.7.8 and §8.7.12 set it to `null`.

---

## 8.7 Reachability and capabilities

This section implements the capability rules of spec/08 §8.12: the roots and the reachable set (§8.12.3), the required capabilities (§8.12.4), the checks (§8.12.6, §8.12.7), the import set `Imp` of a component ([spec/13](../spec/13-components-and-wit.md) §13.5.2), the capability footprint (spec/08 §8.12.8), and the effect rule for tests ([spec/14](../spec/14-testing.md) §14.3). Every query of this section other than `capability_footprint`, `policy_names` (§8.7.13) and the test queries is keyed by a `BuildKey`, and none of them runs while effect analysis of the build is incomplete (§8.1.9 rule 4): the checks of this section report nothing for a build with effect errors, because the errors already reject it.

### 8.7.1 Builds

```rust
pub struct BuildKey {
    pub root: PackageId,
    pub target: TargetId,             // the selected target (package/01 §1.8)
    pub profile: Symbol,              // the selected profile
    pub artifact: ArtifactKind,
    pub mode: BuildMode,
    pub tests: Arc<[PackageId]>,      // packages with test analysis (spec/14 §14.2.4), by identity bytes
}

pub enum ArtifactKind { CoreModule, Component(ComponentKind) }
pub enum ComponentKind { Command, Reactor }
pub enum BuildMode { Normal, WithTests, TestHarness }
```

The driver ([impl/13](13-cli.md)) builds the key from the command:

| Command | `artifact` | `mode` |
|---|---|---|
| `sai build`, `sai check`, `sai size` | `Component` on a WASI target, `CoreModule` on a core target ([spec/13](../spec/13-components-and-wit.md) §13.6.1) | `Normal` |
| `sai run` | `Component` (the target is a WASI target, impl/13) | `Normal` |
| `sai component build` | `Component` on every target | `Normal` |
| `sai check --tests` | as `sai check` | `WithTests` |
| `sai test` | as `sai check`, for the check that precedes the test build; then `CoreModule` for the test build ([impl/13](13-cli.md) §13.6.6.2 item 1) | `WithTests`, then `TestHarness` |

- **Component kind.** A component is a `Command` exactly when the target is a WASI target and the root package has a valid entry point, `entry_point(root)` of [impl/03](03-name-resolution-and-type-checking.md) §3.6.9 ([spec/13](../spec/13-components-and-wit.md) §13.5.3; a candidate `main` with an error stops the build); a component of a core target is always a `Reactor` (spec/13 §13.6.2 item 3, whose `COMPONENT_TARGET_MISMATCH` is reported before this section runs).
- **`Normal`.** No test is analyzed and `tests` is empty. Test edges of the root are inactive ([package/02](../package/02-resolver.md) §2.5.3).
- **`WithTests`.** The roots are those of `Normal` and the entries of every test and property of the packages in `tests` (§8.7.2). Test edges of the root are active, and the `grant` clauses of `requires test` entries apply ([package/01](../package/01-package-contract.md) §1.10.8 item 5).
- **`TestHarness`.** The roots are the entries of the selected tests and properties only ([spec/14](../spec/14-testing.md) §14.7.1 item 3), with the target and profile of the preceding `WithTests` check and the artifact kind `CoreModule`. Its `ReachSet` decides what the harness imports and links (§8.7.3); its capability checks are not run, because they are implied by the preceding check (N-D-89): every root of the harness is a root of the `WithTests` key, `Req(Q)` is monotone in the set of roots, and the test build is made only when that check reported no error (spec/14 §14.7.1 item 2). The harness's private runner interface (spec/14 §14.7.1 item 5), whose entries [impl/04](04-mir.md) §4.13.10 builds and [impl/13](13-cli.md) §13.6.6.2 lists, is not SAI code and is not analyzed. Its effects are those of the property entries of §8.7.12.

`reach_from(ReachKey { build: BuildKey, roots: Arc<[Root]> })` computes the reachable set of an arbitrary root set in the build; `reachable(K)` is `reach_from` of `build_roots(K)`. The other users of `reach_from` are [impl/12](12-semantic-tooling.md), which asks for the code reachable from one `Test` root or from one `Fn` root per member of one interface impl (impl/12 §12.4.2, §12.7.2), and [impl/10](10-runtime.md) §10.2.7, which checks `ALLOCATOR_REENTRANCY` on the set reachable from one `Fn` root per function of the allocator package.

### 8.7.2 Roots

```rust
pub enum Root {
    Main(DefId),            // `entry_point(root)` (impl/03 §3.6.9); command components only
    EnvForEntry(DefId),     // the designated item `sai:process.Env.for_entry`, with `Main`
    ExportFn(DefId),        // an `export fn` of the root package; core modules only
    Glue(GlueKey),          // export glue of `W'`; components only (§8.7.4)
    PanicHook(DefId),       // the `@panic_hook` function; core modules only
    Test(OwnerId),          // the entry of a test or property; `WithTests` and `TestHarness`
    Fn(DefId),              // a function with a body; only in the `reach_from` keys of impl/12 and impl/10, never in `build_roots`
}
```

`build_roots(K)` lists the roots of spec/08 §8.12.3 for the key, refined as follows:

| Root | `CoreModule` | `Component(Command)` | `Component(Reactor)` | `TestHarness` |
|---|---|---|---|---|
| `Main`, `EnvForEntry` | — | yes | — | — |
| `ExportFn` | every `export fn` | — ([spec/13](../spec/13-components-and-wit.md) §13.6.4) | — | — |
| `Glue(Export(fn))` | — | every exported function of `W'` except `wasi:cli/run` | every exported function of `W'` | — |
| `Glue(Dtor(r))` | — | every exported resource of `W'` | every exported resource of `W'` | — |
| `PanicHook` | if declared | — | — | — |
| `Test` | in `WithTests` | in `WithTests` | in `WithTests` | the selected tests of `test_selection(())` ([impl/13](13-cli.md) §13.6.6.1) |

1. **Commands.** The `run` export of a command is generated glue whose own set is `∅`: its WIT type `func() -> result` has no `string` or `list` and flattens to one value (spec/08 §8.14.3). It is represented by its callees, `main` and `Env.for_entry` ([impl/07](07-component-lowering.md) §7.8.4), and by the drop glue of the result type of `main`, which is attached to the `Main` root as an edge without a location (N-D-82). The reachable set is the one that a `Glue` root for `run` would produce.
2. **Interface impls of a core module** are not roots: the export world is not applied ([spec/13](../spec/13-components-and-wit.md) §13.6.2 item 1), and they are reachable only from an `export fn`.
3. **Panic hook.** The `@panic_hook` function is the reporting channel of a core module only ([spec/10](../spec/10-errors-and-panics.md) §10.5.3), and a test build never imports it ([spec/14](../spec/14-testing.md) §14.4.2). It has no body; the root reaches it as an `Extern` leaf that contributes nothing to any own set, so that reporting never adds a capability (spec/10 §10.5.3). A call of the hook by SAI code is an ordinary row 12 call with `host`.
4. **Not roots.** `cabi_realloc`, the post-return functions and the other runtime entry points of [impl/10](10-runtime.md) §10.7.2 are not roots. They contain no SAI code other than the allocator and the release walk of impl/07 §7.5.4, which are reached through the allocator edge (§8.7.3) and the drop glue of the result types (§8.7.4). Provision glue is not a root either: it is reached only through the calls and drops of provided interfaces (§8.7.3, §8.7.4).
5. **Names.** The canonical name of a root, used for ordering and in chains, is the canonical path of its item for `Main`, `EnvForEntry`, `ExportFn`, `PanicHook` and `Fn`; `export <iface>.<fn>` (`export <fn>` for a function of the world itself) and `export <iface>.[dtor]<resource>` for glue, with `<fn>` the WIT function name including its `[constructor]`, `[method]` or `[static]` prefix; and the test path of [spec/14](../spec/14-testing.md) §14.2.3 for a test. `build_roots` lists roots in byte order of their canonical names.
6. **Availability.** `build_roots(K)` reads the HIR of the build's packages, the key and the elaborated world `W'`, and no effect result, so it is not gated by §8.1.9 rule 4. It is available in phase 10, with every query keyed by a `BuildKey` ([impl/01](01-architecture.md) §1.9), and the check collection `mono_items(())` of [impl/04](04-mir.md) §4.13.1 uses its roots. The per-call effects that impl/04 copies into `Call.effects` come from `call_effects` (§8.1.4), which projects the contributions of §8.3.2, and not from this section.

### 8.7.3 Reachability edges

The reachable set is computed on **reach vertices**: effect bodies (§8.1.5), predicate sites (§8.3.7), glue bodies (§8.7.4), latent keys, drop-glue types, and leaves. An edge has a target, a relation and an optional location:

```rust
pub struct ReachEdge {
    pub target: ReachTarget,
    pub relation: ReachRelation,      // Calls | Awaits | Instantiates (§8.6.4)
    pub span: Option<Span>,           // the operation's span; none for glue edges and root edges
}

pub enum ReachTarget {
    Body(EffectBodyId),               // Fn, Closure, AsyncBlock, Derived
    Site(PredicateSiteId),
    Latent(LatentKey),                // expanded below
    DropGlue(EnvKey, Ty),             // expanded below
    Import(DefId),                    // an imported interface or world-import function: leaf, unless provided (below)
    Extern(DefId),                    // a function of an extern "wasm" or extern "C" block: leaf
    ResourceDrop(DefId),              // `[resource-drop]` of an imported resource type: leaf, unless provided (below)
    Allocator,                        // expanded below
}
```

**Operation edges.** `body_effect_summary` records the edges of each operation in `OpRecord.reach`, from the same selection that classifies it (§8.3.2). For a function item `g` applied to `ḡ`, let `Use(g, ḡ)` be: `Body(g)` if `g` has a body of the current or of another package; one `Latent` edge (relation `Instantiates`) for every bound of `g`'s signature, with the latent key of `Contrib` (§8.3.4); and one `DropGlue` edge (relation `Instantiates`) for every type argument of `ḡ` (spec/08 §8.12.3, fifth item).

| Rows of §8.3.2 | Edges |
|---|---|
| 1, 2 | `Use(g, ḡ)` |
| 3 | `Use(m_impl, args ++ D̄)`: the selected impl's method, default body or `Derived` body, with the impl arguments and the method arguments |
| 4 | a `Latent` edge for every bound on the method's own type parameters, and a `DropGlue` edge for every type of `D̄` |
| 5 | `Use(g, ḡ)` of the function item |
| 6, 9, 16 | none |
| 7 | `Body(c)` of the closure |
| 8 | a `Latent` edge for every nested obligation of the builtin candidate |
| 10, 11 | `Import(fn)` |
| 12, 13 | `Extern(fn)` |
| 14 | `Use(g, C̄)`, relation `Awaits` for `Body(g)` |
| 15 | `Body(d)`, relation `Awaits` |
| 17 | a `Latent` edge, relation `Awaits`, for `Q: Future` (the `poll` impl, spec/08 §8.12.3 sixth item) |
| 18 | `DropGlue` of the dropped place's type, relation `Calls` |
| 19 | `Body` of the created closure or `async` block, relation `Calls` |
| 20, 21 | `Use(g, ḡ)`, and a `Site` edge for every `requires` clause of `g`: a function item used as a value is called through the `CheckedEntry` shim, which evaluates them ([impl/04](04-mir.md) §4.3.3) |

The relation is `Calls` unless the table says otherwise. Every edge of an operation has the operation's span.

**Skipped operations and predicate sites.** An operation skipped by §8.3.1 (inside a predicate scope, or a call of a clause body, an invariant element or a refinement predicate) has no `OpRecord`; its edges are in `BodySummary.skipped_reach`. A call of the body of a contract clause, an invariant element or a refinement predicate is an edge to the corresponding `Site`. `predicate_reach(p)` gives the edges of a site: the operation edges, by the same table, of the MIR bodies of the site (the clause body, every element body of the invariant, the refinement predicate; [impl/04](04-mir.md) §4.3.3). The effects of a site are taken from HIR (§8.3.7); only its edges are taken from MIR, because only MIR shows its drops.

**Body edges.** A reached effect body `f` has, in addition to the edges of its operations, an `Allocator` edge when `alloc ∈ Own(f)` (spec/08 §8.12.3, seventh item), with the span of the first operation that contributes `alloc` (§8.5.1).

**Latent expansion.** A `Latent(k)` vertex is expanded from `latent_node(k)` (§8.3.4) by its selection:

| Selection | Edges |
|---|---|
| `Impl { impl, args }` | `Body` of every method of the trait as the impl provides it (the impl's item, the default body, or the `Derived` body); `Latent` of every trait predicate of the impl's elaborated predicates, substituted with `args`, and of the trait's elaborated predicates when a default body is used; `DropGlue` of every type of `args` and of every associated type that the impl defines, substituted |
| `Param(p)` | none: the residual is expanded where a caller instantiates it (§8.3.4, invariant) |
| `Builtin` function trait or `Future` on `FnDef(g, ḡ)` or `Future(g, C̄)` | `Use(g, ḡ)` or `Use(g, C̄)` |
| `Builtin` function trait or `Future` on a closure or `async` block | its `Body` |
| `Builtin` function trait on `FnPtr` | none |
| any other `Builtin` | `Latent` of every nested obligation |
| `Failed(Overflow)` | none (the overflow is reported, §8.3.5, and the build is gated) |

All edges of a latent expansion have the relation `Instantiates` and the span of the operation through which the vertex was first reached; latent vertices are internal and never become steps (§8.6.4).

**Drop glue.** `drop_glue_edges(env, T)` is memoized per normalized type and environment:

| Type `T` | Edges |
|---|---|
| a type parameter, a rigid projection, a borrow, a raw pointer, a primitive, a function item or pointer type | none |
| an ADT with a `Drop` impl | `Latent(T: Drop)` and the edges of every field type of every variant, substituted |
| an ADT without a `Drop` impl, a tuple, an array | the edges of every field, element or variant field type |
| a closure type | the edges of every capture type |
| the future of an `async` block or `async fn` | the edges of every capture type, or of every parameter type of the `async fn` |
| an imported resource type `R` | `ResourceDrop(R)` |
| an exported resource's bound type `C` | as its own kind (it is an ordinary type inside the component) |

Every edge of drop glue has the relation `Instantiates` if the `DropGlue` vertex was reached by an `Instantiates` edge, and `Calls` otherwise. The search stops at type parameters and projections of the environment, as spec/08 §8.12.3 requires; recursive types terminate because each `(env, T)` vertex is visited once.

**Shims.** Shims of [impl/04](04-mir.md) §4.3.2 do not exist in generic MIR; the operation that monomorphization later resolves to a shim already has the shim's edges:

| Shim | Edges, at the operation |
|---|---|
| `StructuralClone`, `StructuralEq`, `StructuralCmp`, `StructuralHash`, `RefinedForward` | row 8 (the nested obligations select the element impls, or the base type's impl) |
| `FnTraitItem`, `ClosureAdapter` | rows 5 and 7 |
| `FuturePoll` | rows 15 and 17 |
| `DropGlue` | row 18 and the drop glue table |
| `RefinedTryFrom(R)` | a `Site(Refinement(R))` edge (§8.3.6 item 5) |
| `CheckedEntry(f)` | rows 20 and 21 |
| `BoundaryValidate`, `BoundaryEntry` | the `Site` edges of §8.7.4 |
| `ReifyLocation` | the edges of the item it forwards to |

**Allocator.** The `Allocator` vertex has edges to the four functions of the allocator package if the root declares `allocator` ([package/01](../package/01-package-contract.md) §1.15; `add_region`, `allocate`, `deallocate`, `reallocate` of [impl/10](10-runtime.md) §10.2.7), as `Body` edges without location; otherwise it is a leaf, the runtime allocator of impl/10, which contains no SAI code of any package.

**Provision.** For every key, `provision_plan(K)` of [impl/07](07-component-lowering.md) §7.11.10 names the interfaces provided within the build and their providers; it does not depend on reachability. An `Import(fn)` vertex of a function of a provided interface is not a leaf: it has one edge `Body(Glue((K, Provide(fn))))`. A `ResourceDrop(R)` vertex of a resource of a provided interface has one edge `Body(Glue((K, ProvideDtor(R))))`. Both edges have the relation `Calls` and no location, and the glue bodies are those of §8.7.4. The vertices stay in `imports` and `resource_drops`, and §8.7.9 removes the provided ones from the lists that impl/07 imports.

**The reachable set.**

```rust
pub struct ReachSet {
    pub bodies: Vec<EffectBodyId>,        // every reached effect body and glue body
    pub sites: Vec<PredicateSiteId>,
    pub imports: Vec<DefId>,              // reached imported functions
    pub externs: Vec<DefId>,              // reached extern functions, the panic hook included
    pub resource_drops: Vec<DefId>,       // imported resource types whose drop glue is reached
    pub allocator: bool,
    pub graph: ReachGraph,                // vertices and edges, for §8.6.5
}
```

`reach_from` runs a worklist from the roots: each vertex is expanded once, its edges are generated in the order of the tables above (operations in operation order), and new targets are appended to the worklist. The order in which vertices are visited affects only `graph`'s internal numbering, never a result. Every list is sorted by the identity bytes of the owning package, then the `DefPathHash`, then the method name or the printed type, and `graph` keeps every edge, so that §8.6.5 can choose among them. Foreign bodies are expanded from `BodyFacts.reach` and `predicate_reach` of the metadata (§8.3.8): the persistent edges carry their relation and their `dep:` location, and their latent and drop-glue targets are already flattened into the bodies, sites and leaves that they reach in the foreign package.

`resource_drops` lists every imported resource whose drop glue is reached; [impl/07](07-component-lowering.md) imports `[resource-drop]` only for those whose interface is in `Imp`. Drop glue of a resource whose interface is not in `Imp` cannot run (impl/07 §7.7.1), and §8.7.9 removes it from the list that impl/07 receives.

### 8.7.4 Glue bodies

A glue body (§8.1.5) has no MIR in this chapter: [impl/07](07-component-lowering.md) generates the glue after effect analysis (N-D-64 of impl/07). Its own set and edges are computed from the WIT types of the exported function `fn` of `W'` and from the SAI function `g` that implements it ([spec/13](../spec/13-components-and-wit.md) §13.3):

- **Own set.** `{ alloc }` if a condition of spec/08 §8.14.3 holds for `fn`, otherwise `∅`. It is never in the constraint system and no check of §8.5 applies to it.
- **Owner.** The root package, whose `export world` causes the glue (spec/08 §8.14.3: the package of the export).
- **Merged edge.** One edge `Body(g)` of weight 0 in the chain search (§8.6.5), with relation `Calls` and no location. A chain through it shows one step for the pair: the step is named `export <iface>.<fn>` and located at the declaration of `g`, which is what the compact form of spec/08 §8.13.3 shows as step 0 ([spec/13](../spec/13-components-and-wit.md) §13.12).
- **Drop glue.** A `DropGlue` edge for the SAI type of every parameter and of the result, without location: the glue drops converted temporaries and releases the result after the call ([impl/07](07-component-lowering.md) §7.8.2).
- **Borrowed handles.** A `ResourceDrop(R)` edge for every parameter of type `borrow R` of an imported resource, because the glue drops the borrowed handle after the call.
- **Boundary checks.** A `Site` edge for every `requires` clause of `g`, and for the invariant and the refinement predicate of every type that occurs in a parameter type of `g`, transitively through fields and type arguments ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.3, [impl/04](04-mir.md) §4.12.5).
- **Allocator.** An `Allocator` edge if the own set is `{ alloc }`.

The glue of a destructor, `Dtor(r)` for an exported resource `r` bound to `C`, has the own set `∅` (deallocation is not an effect, D-OWN-05), a `DropGlue(C)` edge and an `Allocator` edge, because it drops and frees the representation ([impl/07](07-component-lowering.md) §7.6.3). It is a root because the host may call it whenever a handle of `r` exists, and it is always exported.

**Provision glue.** `Provide(fn)` stands for the glue of a call of `fn`, a function of an interface provided within the build by `n`: the adapter, the provider's export glue and its post-return function ([impl/07](07-component-lowering.md) §7.11.10). `ProvideDtor(r)` stands for the drop adapter and the provider's destructor of a provided resource `r`. Their own sets and edges are those of `Export(fn)` and `Dtor(r)` above, with `g` the function of `n` that implements `fn` and `C` the type that `n` binds to `r`, with these differences:

- **Owner.** The provider `n`, whose `export world` causes the glue ([spec/08](../spec/08-effects-and-capabilities.md) §8.14.3).
- **Borrowed handles.** No `ResourceDrop` edge: the provider's export glue drops no borrowed handle (impl/07 §7.11.10).
- **Provided resources.** A `Body(Glue((K, ProvideDtor(r))))` edge for every provided resource `r` whose handles occur in the WIT type of `fn`, because the provider's lift of an owned handle drops it through the drop adapter (impl/07 §7.6.3, §7.6.5).
- **Chains.** The merged edge `Body(g)` has weight 0 as for `Export(fn)`. A chain through it shows the caller's call of `fn` followed by the steps of `g`.
- **Builds.** Provision glue exists for every artifact kind, core modules included, and is never a root (§8.7.2 item 4).

The own set of the caller's import glue is the contribution of rows 10 and 11 of §8.3.2, as for every imported function. Each call therefore has the same own sets, and the same owners, as in artifact form ([package/04](../package/04-distribution-and-security.md) §4.5.3 item 4).

### 8.7.5 Required capabilities

`required_capabilities(K)` computes `Req(Q)` of spec/08 §8.12.4 for every package `Q` of the build, and the total `Req`:

```rust
pub enum Capability { Alloc, WitPackage(Symbol), WitInterface(Symbol), Target(TargetCap) }

pub struct RequiredCap {
    pub cap: Capability,
    pub sources: Vec<CapSource>,          // in the order of the items below
}

pub enum CapSource {
    Bodies,                               // item 1: reached bodies of the owner; their chains come from §8.6.5
    Component(PackageId),                 // item 2
    Memory64,                             // item 3
    TypeImp(Symbol),                      // item 4: the interface whose types cause it
}
```

Capabilities are ordered by the bytes of their printed names (`alloc`, `host`, `memory64`, `wasi:cli`, `wasi:cli/stdout`, …), which is the order of every list and every diagnostic of this section.

1. **Bodies.** For every body `f` of the reachable set, `caps(Own(f))` is added to `Req(owner(f))`. The owner is the package of the `DefId` of the body: for a closure or `async` block, of its enclosing owner; for a trait default body, of the trait; for a `Derived` body, of the `DerivedImpl` owner; for a glue body, the root, and for provision glue, the provider (§8.7.4); for a test entry, the test's package ([spec/14](../spec/14-testing.md) §14.3.2 item 1). Predicate sites have no own set (spec/08 §8.11) and add nothing. `caps` of an atom is computed once per atom by `atom_caps`: a built-in effect gives `cap(e)` from the table of spec/08 §8.3.1, an interface effect itself, a world-import effect `ns:pkg/w`, and a user effect the union over its `covers`, transitively and ignoring opacity (spec/08 §8.12.2), read from the metadata of the declaring package when it is foreign. The own set of a foreign body is its `BodyFacts.own` (§8.3.8).
2. **Binary components.** For every package of the active graph whose chosen form is a binary component ([package/04](../package/04-distribution-and-security.md) §4.5.2), every interface that the component imports is added to its `Req`, read from its component type. These imports count as reachable whenever the package is active (D-CAP-01).
3. **memory64.** `memory64` is added to `Req(root)` when the target is `wasm64-core` or `wasm64-wasi`.
4. **TypeImp.** When the artifact is a component, every interface of `Imp` by rules 2 and 3 of [spec/13](../spec/13-components-and-wit.md) §13.5.2 (§8.7.9) is added to `Req(root)`.

`Req` is the union of `Req(Q)` over the packages of the build, with the sources of all of them. Items 2–4 have no body and no chain; they are explained by the pseudo-roots of §8.6.5.

### 8.7.6 Policy checks

`capability_check(K)` runs the checks of spec/08 §8.12.6 and of [package/01](../package/01-package-contract.md) §1.10.8, in this order: the per-package checks, the root check, the grant check (§8.7.7), and the target check (§8.7.8). The diagnostics are emitted in that order, and within each check in the order stated there; [impl/11](11-diagnostics.md) sorts them for output.

1. **Per package.** For every package `Q ≠ root` of the build with a non-empty `Req(Q)`, in identity-byte order, every `r ∈ Req(Q)` is checked against `Q`'s own policy `(allow(Q), deny(Q))`, independent of the profile. The owner filter of the chain is `{ Q }` (§8.6.5).
2. **Root.** Every `r ∈ Req` is checked once against the root's **effective policy**. The owner filter is every package of the build. The per-package check of the root is not run separately (N-D-83): the effective `allow` is contained in the root's own `allow` and the `deny` is the same, so every violation of the root's own policy is a violation of the effective policy for the same `r` with the same code, and spec/08 §8.12.6 reports a violation once per (policy, capability).

**Effective policy.** For the selected profile `p` ([package/01](../package/01-package-contract.md) §1.14.2 item 3), let `Den(p)` be the default denials of `p` from the [targets doc](../targets/targets-and-profiles.md) (for `tiny`: every capability of the `wasi` namespace, `host` and `threads`; for `dev` and `release`: none) and `Lift(p)` the names of `profile p { allow { … } }`. A denial `wasi` covers every WIT capability whose namespace is `wasi`; the others cover themselves. A name `a` **implies** `r` when `r` is `wasi:io` or an interface of `wasi:io` and `a` is a capability of the `wasi` namespace that is neither `wasi:io` nor an interface of it (the `wasi:io` implication, [targets](../targets/targets-and-profiles.md) §3.3 item 1); write `a ⊒ r` for "`a` covers `r` or `a` implies `r`". Then:

```
allowed_eff(r) ⟺ (∃ a ∈ allow(root): a ⊒ r) ∧ (¬∃ d ∈ Den(p): d covers r  ∨  ∃ l ∈ Lift(p): l ⊒ r)
granted(r, policy) ⟺ allowed(r) ∧ ¬∃ d ∈ deny: d overlaps r
```

with `allowed` the first conjunct alone for a package's own policy. A capability removed by `Den(p)` is therefore not allowed, not denied: package/01 subtracts it from the allowed set. The implication enlarges what an `allow` set or a profile `allow` grants (targets §3.3 item 2, §5.5 item 4), never what a denial removes, so `Den(p)` and `deny` use "covers" and "overlaps" alone (targets §3.3 item 3). Because every class of `Den(p)` is closed under containment (targets §5.5 item 2) and a name that implies `r` is denied by `tiny` exactly when `r` is, the formula grants the same capabilities as the entrywise effective policy `(A_p, D)` of targets §5.5.

**Codes.** If some `deny` entry overlaps `r`, the violation is `CAPABILITY_DENIED`; otherwise, if `r` is not allowed, `CAPABILITY_NOT_ALLOWED` (spec/08 §8.12.6: deny takes precedence). Each is reported once per (checked policy, capability), in byte order of the capabilities.

**Locations.** Every location is in the `sai.package` of the checked package, in `dep:` form for a dependency ([impl/11](11-diagnostics.md) §11.5.4):

| Violation | Primary location | `policy` field of the chain |
|---|---|---|
| `CAPABILITY_DENIED` | the first `deny` entry, in source order, that overlaps `r` | that entry |
| `CAPABILITY_NOT_ALLOWED`, `r` removed by `Den(p)` (the **profile case**) | the `allow` declaration of `profile p` if it has one, else the `profile p` header, else the **witness entry** of `r` | the witness entry of `r` |
| `CAPABILITY_NOT_ALLOWED`, otherwise | the `allow` keyword of the package-level `allow`, or the `package` declaration if there is none | `null` |

The witness entry of `r` is the first package-level `allow` entry, in source order, that covers `r`, or, when none covers it, the first that implies it ([targets](../targets/targets-and-profiles.md) §5.6 item 3). In the profile case a note says "profile `p` denies `r` by default".

**Messages and data.** The messages are those of spec/08 §8.13.3: "capability `r` is required but not allowed by package `P`" and "capability `r` is required but denied by package `P`"; the profile case appends " with profile `p`". `data` is `{ package, capability, effect, profile, target }`, with `effect` the atom of the reported chain (`null` for a pseudo-root) and `profile` the profile name in the profile case and `null` otherwise. The related entry is the capability chain of §8.6.5 for `(K, r, F)` with the `policy` field of the table, or the `declared_here` entry of a pseudo-root when the least source of `r` has no body (the order of `CapSource`).

**Fixes.** `CAPABILITY_NOT_ALLOWED` has one `maybe-incorrect` fix ([impl/11](11-diagnostics.md) §11.7) when the checked package is the root or a workspace member; a dependency's policy is not edited ([impl/13](13-cli.md) `--fix`). Granting authority is a policy decision that the user confirms (P5), so the fix is never `machine-applicable` (N-D-79):

- outside the profile case: insert `, r` after the last entry of the package-level `allow`; replace an empty `allow { }` by `allow { r }`; or, without `allow`, insert a line `allow { r }` at the end of the file, after one blank line;
- in the profile case: the same edits on `profile p`'s `allow`, or a line `allow { r }` as the first line of the `profile p` block, or a block `profile p { allow { r } }` at the end of the file (N-D-84), where `r` is replaced by the witness entry of `r` when no package-level `allow` entry covers `r` (targets §5.6 item 3). Lifting the default denial is the smallest change, and the inserted name is covered by the package-level `allow` (it is `r`, which an entry covers, or the witness entry itself), so `PROFILE_ALLOW_EXCEEDS_PACKAGE` cannot follow.

`CAPABILITY_DENIED` has no fix (spec/08 §8.12.6).

**Suppression for tests.** In `WithTests`, a violation of `r` under the owner filter `F` is not reported when every root with a finite `distR` for `(K, r, F)` (§8.6.5) is a test or property for which `TEST_EFFECT_NOT_PERMITTED` with `r` is reported (§8.7.12, [spec/14](../spec/14-testing.md) §14.3.2 item 3). A source without a body is never a test, so it prevents the suppression. The same rule applies to §8.7.7 and §8.7.8 (N-D-88).

### 8.7.7 Grant check

For every active requirement edge `P → Q` that has a `grant { G }` clause ([package/01](../package/01-package-contract.md) §1.10.8), in the order of the identity bytes of `P` and then of `Q`, the check covers `C(Q) = ⋃ { Req(R) | R ∈ closure(Q) }`, with `closure(Q)` the packages reachable from `Q` in the active graph, `Q` included. An entry of `requires test` is checked only when its edge is active, that is in `WithTests` (package/01 §1.10.8 item 5).

- Every `r ∈ C(Q)` that no name of `G` covers or implies (§8.7.6; `G` is the policy `(G, ∅)` of [targets](../targets/targets-and-profiles.md) §3.3 item 2) is `CAPABILITY_NOT_GRANTED`, once per (edge, capability), in byte order of the capabilities.
- **Location.** The primary location is the `grant` clause of the edge in `P`'s `sai.package` (`dep:` form if `P` is a dependency). The chain is that of §8.6.5 for `(K, r, closure(Q))`, with `policy` set to the `grant` clause; a source without a body gives its `declared_here` entry.
- **Message.** "capability `r` is required by `O` but not granted by `P`", where `O` is the owner of the switch body of the chain, or the package of the pseudo-root; when `O ≠ Q`, ", a dependency of `Q`," follows `O`. `data` is `{ from: P, to: Q, capability, grant, owner: O }`, with `grant` the location of the clause.
- **Fix.** When `P` is the root or a workspace member, a `maybe-incorrect` fix inserts `, r` after the last name of `G`, or replaces `grant { }` by `grant { r }` (N-D-79).

### 8.7.8 Target check

A capability is **provided** for a key `K` according to the target table of the [targets doc](../targets/targets-and-profiles.md), whose consequences spec/08 §8.12.7 fixes:

| Capability | Provided when |
|---|---|
| `alloc` | always |
| `host` | `artifact = CoreModule` |
| `memory64` | the target is `wasm64-core` or `wasm64-wasi` (it is required only then, §8.7.5 item 3, so it is never unavailable) |
| `threads`, `native_ffi`, `process` | never in v0.1 |
| a WIT capability of the `wasi` namespace | the target is a WASI target and the package is one of its WASI 0.2 packages |
| any other WIT capability | the target is a WASI target (composition supplies it; a component of a core target has no imports, [spec/13](../spec/13-components-and-wit.md) §13.6.2 item 2), or the capability is an interface provided within the build, or a WIT package all of whose interfaces are provided within the build ([impl/07](07-component-lowering.md) §7.11.10; spec/13 §13.6.2 item 4) |

Every `r ∈ Req` that is not provided is `TARGET_CAPABILITY_UNAVAILABLE`, once per capability, in byte order. The primary location is the location of the last step of the chain for `(K, r, all packages)` that has one (the use of the import, extern or allocating function, or the declaration in a clause), or the canonical location of the root if no step has one, or the pseudo-root's location. The message is "capability `r` is not available on target `t`", and for `host` in a component "capability `host` is not available in a component" with a note pointing to [spec/13](../spec/13-components-and-wit.md) §13.6.4. `data` is `{ capability, target, artifact }`; the chain's `policy` is `null`. There is no fix.

### 8.7.9 Component imports

`component_imports(K)` is computed when `K.artifact` is a component, and is empty otherwise: a core module applies no export world ([spec/13](../spec/13-components-and-wit.md) §13.6.2 item 1). Provision within the build does not depend on it: impl/07 reads `provision_plan(K)` for every key ([impl/07](07-component-lowering.md) §7.11.10).

```rust
pub struct ComponentImports {
    pub imp: Vec<ImpEntry>,               // by identity bytes
    pub functions: Vec<DefId>,            // reached imported functions whose interface is in `imp` and not provided
    pub resource_drops: Vec<DefId>,       // reached resource drops whose interface is in `imp` and not provided
    pub allocator: bool,                  // `ReachSet.allocator`
    pub plan: Option<CompositionPlan>,    // item 6; impl/07 §7.11.9
    pub diagnostics: Vec<Diagnostic>,     // WORLD_IMPORT_UNDECLARED, PROVIDED_INTERFACE_AT_BOUNDARY
}

pub struct ImpEntry {                     // an interface, or the world-import block of a world
    pub import: ImportName,
    pub rule: ImpRule,
    pub provider: Option<PackageId>,      // the provider when the interface is provided within the build (impl/07 §7.11.10)
}
pub enum ImpRule { Effect, ExportType, ImportType }                 // rules 1, 2, 3 of spec/13 §13.5.2
```

1. **Rule 1.** Every interface atom and every world-import atom in `Own(f)` of a reached body `f`. Only atoms written in own sets count; a user effect whose `covers` contain an interface does not import it, because the function that actually calls the interface is itself reached and has the atom in its own set (row 10 of §8.3.2).
2. **Rules 2 and 3.** Every interface whose types occur in the WIT types of an export of `W'`, and, until nothing changes, every interface whose types occur in the WIT types of an interface already in `Imp`. These are computed from the resolved WIT of `W'` ([spec/13](../spec/13-components-and-wit.md) §13.4.2) and do not depend on reachability. An interface found by rule 1 keeps the rule `Effect`.
3. **`WORLD_IMPORT_UNDECLARED`.** Every entry of `Imp` that `W'` does not import, that is not provided within the build, and that has no provider in the composition of item 6, once per entry, in identity-byte order. The primary location is the declaration of the world in its WIT file if the world is a WIT file of the root package (a **local** world), and the `export world` declaration of the root's `sai.package` otherwise. A rule 1 entry has the capability chain of §8.6.5 for the interface, with every package as owner filter and `policy` `null`; a rule 2 or 3 entry has a `declared_here` entry at the `export world` declaration with `what` "`` the types of the exports of world `<path>` use `<iface>` ``". For a local world, a `maybe-incorrect` fix inserts the line `import <iface>;` after the last `import` of the world, or as its first item.
4. **Output.** [impl/07](07-component-lowering.md) reads `functions`, `resource_drops`, `imp` and `plan`: it imports exactly these functions (impl/07 §7.7.1), the `[resource-drop]` intrinsics of these resources, and declares the interfaces of `imp` that are not provided within the build in the component type. Drop glue of an imported resource whose interface is not in `imp` is not listed (§8.7.3). The functions and resource drops of provided interfaces are not listed either: impl/07 links them to their providers (impl/07 §7.11.10).
5. **Core targets.** For `sai component build` on a core target, every entry of `Imp` is also in `Req` (rule 1 through the bodies, rules 2 and 3 through `TypeImp`), so the target check reports every entry that is not provided within the build as `TARGET_CAPABILITY_UNAVAILABLE` ([spec/13](../spec/13-components-and-wit.md) §13.6.2 items 2 and 4), and a provided entry is available (§8.7.8); `WORLD_IMPORT_UNDECLARED` is still reported for an entry that `W'` lacks.
6. **Artifact dependencies.** When some active node is used in artifact form ([package/04](../package/04-distribution-and-security.md) §4.5.1), `plan` is `composition_plan(K)` of [impl/07](07-component-lowering.md) §7.11.9; otherwise it is `None`. It changes the check of item 3 and nothing else ([spec/13](../spec/13-components-and-wit.md) §13.5.2, N-D-101):
   - An entry of `Imp` that has a provider for `C₀` is satisfied inside the composition, so item 3 does not report it. It stays in `Imp`, and its capability stays in `Req` (§8.7.5).
   - An outer import of the plan that is imported by a member of `U`, is not an entry of `Imp`, and is not imported by `W'` is `WORLD_IMPORT_UNDECLARED`, once per interface. These diagnostics are merged with those of item 3 in identity-byte order. The primary location and the fix are those of item 3. The source is the `declared_here` entry of the pseudo-root `component <ns:pkg>` (§8.6.5) of the first member of `U`, in instantiation order, that imports the interface.
7. **`PROVIDED_INTERFACE_AT_BOUNDARY`.** Let `Prov` be the interfaces provided within the build (`provision_plan(K)`) and the interfaces that an artifact of `plan` provides to `C₀` or to another member of `U` (impl/07 §7.11.9 item 1). An interface `I ∈ Prov` is reported in each case of [package/04](../package/04-distribution-and-security.md) §4.5.3 item 5 that holds:
   - **`export`** (case 1): `W'` exports `I`, or rule 2 finds `I`, whatever rule its entry of `Imp` records;
   - **`import`** (case 2): rule 3 finds `I` from an interface of `Imp` that is not in `Prov`, directly or through other interfaces;
   - **`artifact`** (case 3): `I` is provided within the build and a member of `U` imports it.

   The diagnostics are reported once per interface and case, ordered by the identity bytes of the interface and then by the case number, after those of item 3. The primary location is that of item 3. A `declared_here` entry points to the `export world` declaration of the root for cases 1 and 2, and to the pseudo-root `component <ns:pkg>` (§8.6.5) of the first member of `U` that imports `I` for case 3. The message is "interface `I`, provided by `P`, crosses the boundary of the component", with `P` the providing package. `data` is `{ interface, provider, case }`. In case 3, the help suggests `overrides { <id> use artifact }` for `P` when package/04 §4.5.1 allows that override for `P`. There is no fix.

### 8.7.10 Capability footprints

The footprint of spec/08 §8.12.8 is an upper bound of what a package can contribute to `Req` in **dependency position** (N-D-85):

1. **Source packages.** `capability_footprint(P, t)` is `caps(⋃ Own(f))` over every effect body `f` of `effect_bodies(P)` for target `t`. Tests and properties are excluded (test code of a dependency is never active, [package/02](../package/02-resolver.md) §2.5.3), and so are glue bodies, `memory64` and `TypeImp`, which only the root of a build has (§8.7.5 items 3 and 4). The exception is the provision glue of a hybrid package (§8.7.4), which it has in dependency position: its footprint also contains `alloc` when a function of an interface that its `export world` exports meets a condition of [spec/08](../spec/08-effects-and-capabilities.md) §8.14.3. Effect inference does not depend on the profile ([spec/15](../spec/15-attributes-and-lints.md) N-A-44), so neither does the footprint. The footprint recorded in the lock is the union over the targets that the package declares ([package/01](../package/01-package-contract.md) §1.8), computed by the resolver once per target.
2. **Normalization.** The union is normalized: a capability covered by another capability of the set is removed (`wasi:http/types` is dropped when `wasi:http` is present), and the rest is sorted by bytes, which is the order of the lock's `capabilities` field ([package/03](../package/03-lock-file.md)). Normalization does not change which capabilities are covered, so the escalation rule of package/02 §2.12 is unaffected.
3. **Artifacts.** The footprint of a binary component is the set of interfaces it imports, read from its component type, per target, which is the lock's `imports <target>` field.
4. **Errors.** A package whose effect analysis for `t` has an error has no footprint for `t`; the query returns the diagnostics instead, and the resolver reports them. A package with an incomplete analysis therefore never gets a footprint smaller than its code.

### 8.7.11 `sai inspect capabilities`

The command of [impl/13](13-cli.md) reads `required_capabilities(K)` and the chain search for the key of the selected target and profile, in mode `Normal`. For every `r ∈ Req`, in byte order:

1. **Status.** `unavailable` if the target check fails for `r` (§8.7.8); otherwise `denied` or `not_allowed` if the root check (§8.7.6 item 2) fails with that code; otherwise `allowed`. The precedence follows the checks: a capability that the target cannot provide fails whatever the policy says, and a deny entry takes precedence over `allow`. The status reflects the root's effective policy and the target only; violations of a dependency's own policy and of grants are reported as diagnostics, not as a status.
2. **Roots.** Every root with a finite `distR` for `(K, r, all packages)`, in byte order of canonical names, each with its walk of §8.6.5 in the compact form of spec/08 §8.13.3. A source without a body is listed as its pseudo-root, after the roots, with one `Declares` step at its location.
3. **Gating.** If effect analysis of the build reported an error, the command reports those diagnostics and prints no capability list (§8.1.9 rule 4).

### 8.7.12 Tests and properties

Test analysis (spec/14 §14.2.4) adds the test bodies of every package in `K.tests`:

- **Bodies and solution.** `test_effect_bodies(P)` lists, in stable key order, the effect body `Fn(t)` of every test and property `t` of `P` and the closures and `async` blocks inside them. `test_effect_solution(P)` solves their nodes on top of `effect_solution(P)`: no non-test body refers to a test body, so every reference from a test body into the package's nodes is a constant read from the package solution, and new latent keys and `Inst` nodes of test bodies are nodes of the test solution only. `Sig`, the metadata of §8.3.8 and the footprint are unaffected by tests (N-D-90).
- **Property entries.** For a property with parameters `p1: T1, …, pn: Tn`, the body `Fn(t)` is its **entry** ([spec/14](../spec/14-testing.md) §14.3.1 item 1). The summary has synthesized operations with `loc = Entry { param: i, part }` and the span of the parameter `pi`: first, for `i = 1 … n`, a `Call` of `Arbitrary.arbitrary` with `Self := Ti` (`part = Arbitrary`), classified by row 3 or row 8 of §8.3.2; then the operations of the property body; then, for every `Ti` that implements `Format`, a `Call` of `Format.format` with `Self := Ti` and the method argument `W := FixedBuf<4096>` (`part = Format`), and a `Drop` of `Ti`. The construction of the local `Gen` and of the `FixedBuf` is harness code without effects and is not an operation.
- **Checks.** `test_effect_check(t)` runs the checks of §8.5 that apply to a function without a clause on the bodies of `t`, reading the test solution: bound caps (§8.5.4), coercions (§8.5.5) and the predicate checks of the predicates inside them. A test is not `pub` and has no clause, so §8.5.2, §8.5.7 and §8.5.8 report nothing for it.
- **`TEST_EFFECT_NOT_PERMITTED`.** For every `r ∈ caps(A(I(t)))` other than `alloc`, in byte order, one error at the test's name string. The message is "test `<name>` requires capability `r`, which tests do not have" (`property` for a property). `data` is `{ test, capability, effect }` with `test` the test path of spec/14 §14.2.3. The related entry is a capability chain whose `effect` is the least atom `a ∈ A(I(t))`, in internal order, with `r ∈ caps(a)`, whose steps are `why_chain(Body(t), a)` (§8.6.2), and whose `policy` is `null`.
- **Roots.** In `WithTests` and `TestHarness`, `Test(t)` roots reach the operations of the entry, including the synthesized ones. Their capabilities are charged to `t`'s package (§8.7.5 item 1), and the suppression rule of §8.7.6 applies to them.
- **Gating.** In `WithTests`, the gating of §8.1.9 rule 4 includes the test bodies: an effect error in a test prevents the capability checks of the build.

### 8.7.13 Policy names

`policy_names(P)` validates the capability names of the contract of package `P` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.1, §8.12.5; [package/01](../package/01-package-contract.md) §1.11). Its value is a list of diagnostics, and it is keyed by `P` alone: it reads no body, and no `BuildKey`, target or profile.

- **Inputs.** The names of the package-level `allow` and `deny`, of every `profile p { allow { … } }` (package/01 §1.14.2), and of every `grant` clause of `requires` and `requires test` (package/01 §1.10.8), in source order, as the queries of package/01 parse `P`'s `sai.package` ([impl/01](01-architecture.md) §1.7.3). The WIT packages visible to the build ([spec/13](../spec/13-components-and-wit.md) §13.7.1) with the interface names of each: the WASI packages and interfaces of the [targets](../targets/targets-and-profiles.md) §2.5 table, which every target of a toolchain release shares, and the WIT packages of items 2 to 4 of spec/13 §13.7.1 for the graph in `package_graph`.
- **Skipped names.** A name that the contract parser reported as `INVALID_CAPABILITY_NAME` (package/01 §1.11.2) is not checked again. A `WORD` name that passed that check is one of the fixed allocation and target capability names and is always known.
- **Unknown names.** A WIT package name `ns:pkg` is known when it is a WASI package of targets §2.5 item 2 or the name of a visible WIT package. A WIT interface name `ns:pkg/iface` is known when it is listed in the table of targets §2.5 item 4, or a visible WIT package `ns:pkg` has an interface `iface`; two compatibility lines of `ns:pkg` count as one package (package/01 §1.11.2 item 3). Every other name is `UNKNOWN_CAPABILITY`, once per entry, at the entry. The message is "capability `n` is not known to the build", and `data` is `{ package, capability }`. When the package part is known and the interface is not, a help lists the interfaces of that package in byte order, so that `wasi:filesystem/read` gets the help "`wasi:filesystem` has the interfaces `preopens`, `types`". There is no fix.
- **Conflicts.** Every entry of the package-level `deny` whose name is equal, token for token, to a name of the package-level `allow` is `CAPABILITY_ALLOW_DENY_CONFLICT`, at the `deny` entry, with a secondary label at the first equal `allow` entry. Different names that overlap are not compared (spec/08 §8.12.5), and a profile `allow` or a `grant` is not compared with `deny`. An unknown name is still compared, because the two rules are independent. There is no fix: which entry is wrong is a policy decision (P5).
- **Order and locations.** The occurrences of the unknown-name rule come first, in source order, then those of the conflict rule, in the source order of the `deny` entries. Every location is in `P`'s `sai.package`, in `dep:` form for a dependency ([impl/11](11-diagnostics.md) §11.5.4).
- **Callers.** The resolver evaluates `policy_names(Q)` for every node `Q` whose contents it has, with the resolved graph as `package_graph`, before the lock is written: all nodes in Mode B and the active nodes in Mode A ([package/02](../package/02-resolver.md) §2.10.2, §2.12.1 rule 1). Mode A is a resolution too, so an edit of the root's `allow` that keeps the lock usable is still validated by the next command. An error stops the resolution, as package/02 requires, so no build runs with an unknown or conflicting name and the build-time checks of §8.7.6 to §8.7.8 never read one. (N-D-110)

---

## 8.8 Incrementality

### 8.8.1 Keys, values and early cutoff

Every key of §8.1.4 is built from stable identifiers ([impl/01](01-architecture.md) §1.6.2): `DefPathHash`es, package keys, interned types and predicates, and the fields of `BuildKey`. Every value is compared by fingerprint for early cutoff (impl/01 §1.4.3).

- **Shapes.** `body_effect_shape(b)` is `body_effect_summary(b)` with every `span`, every `OpLoc` and every edge location removed. `effect_solution` reads only shapes, latent nodes, signature sets and metadata, never spans. An edit that moves code inside a body, or changes it without changing its operations, recomputes the summary and the body's own checks (which report at spans), and backdates the shape, so that no solution, no other body's check and no capability query is recomputed.
- **Solutions.** `effect_solution(P)` returns the value of every node of the package. Its consumers read it through per-item projections (`sig_effects`, `call_effects`, the inputs of `effect_check`), which backdate when the values they read are unchanged; a change of one function's inferred set therefore re-runs only the queries that read that set.
- **Reachability.** `reach_from` reads the summaries' edges, `predicate_reach`, `latent_node`, `drop_glue_edges` and the solution's `Own` sets of reached bodies. Its value backdates when the reached set and its edges are unchanged.

### 8.8.2 What an edit recomputes

| Edit | Recomputed | Not recomputed |
|---|---|---|
| inside a body, operations unchanged | the summary and the checks of that body | the solution, other bodies, capability queries (§8.8.1) |
| inside a body, operations changed, the function has a clause | its summary, the solution of its SCC, its checks; reachability and capability queries of builds that reach it | callers' checks and solutions (§8.8.3) |
| inside a body of a function without a clause | its summary, the solution, every body whose value reads `I(f)`, their checks; capability queries | bodies whose values are unchanged (backdated) |
| an `effects` clause | `Sig`, every caller's value, `EFFECT_NOT_DECLARED` of the function, and the metadata of the package | — |
| a `covers` list of a user effect | `covers_star`, every permit index that contains the effect, the checks that use it, `atom_caps`, `Req` | effect values (spec/08 §8.2.4: `covers` changes permits, not sets) |
| a new or removed impl | the latent nodes whose selection changes, and everything that reads them | latent nodes of other goals |
| `allow`, `deny`, `grant`, a `profile` block | `capability_check` | every effect query and every reachability query |
| the target or the profile | queries keyed by `BuildKey`; for the target, the HIR of items with `@target` predicates and what reads it | effect queries of code without `@target` predicates (the profile changes none, N-A-44 of spec/15) |
| a test or property | `test_effect_*` of its package and the `WithTests` capability queries | every query of `effect_bodies`, the metadata, the footprint |

### 8.8.3 Clauses are firewalls

A caller refers to a function `g` with a clause through the constant `Sig(g) = D(g)` (§8.3.2, `Contrib`), never through `I(g)`. An edit inside the body of `g` therefore never changes the value of a caller's node, never re-runs a caller's check, and never changes the metadata that dependents read, unless it changes `D(g)`. Only `g`'s own `EFFECT_NOT_DECLARED` check and the reachability of builds that reach `g` (whose edges come from bodies, not clauses) are recomputed. This is the incremental counterpart of spec/08's rule that callers depend only on signatures, and it is why public functions are required to declare their effects (spec/08 §8.9.3).

### 8.8.4 Persistence

- **Query cache.** The per-owner results of phase 9, which include `effect_check` and `unsafety_check`, are persisted in the query cache ([impl/01](01-architecture.md) §1.6.3 item 2). The other queries of this chapter are recomputed when their nodes are red or not persisted; their nodes still let later queries be marked green.
- **Metadata.** `EffectMetadata` (§8.3.8) is part of the dependency metadata of impl/01 §1.6.5 item 4. Its key includes the target, so a dependency's effect facts are stored per target. It is written only for a package without errors (impl/01 §1.6.5 item 7), which is also why every foreign value read by this chapter is complete.
- **Durability.** The queries of `sai:core`, `sai:alloc` and cached dependencies depend only on high-durability inputs (impl/01 §1.4.4), so an edit of the root package never walks their dependencies. `capability_check` depends on the root's `sai.package` and on `build_config`, both of which have low or medium durability, and is cheap to re-run.

---

## 8.9 Outputs

This section defines the data that the commands of [impl/13](13-cli.md), the semantic diff of [impl/12](12-semantic-tooling.md) and the other phases read from this chapter. `sai inspect capabilities` is §8.7.11.

### 8.9.1 `sai inspect effects`

The command is a driver function of impl/13, not a query. It reads `effect_interface(P)` (§8.9.3) of the root package and of every workspace member for the selected target, and `sig_effects` and the why-tables of §8.6.3 for the functions it names. The analysis is that of `sai check` without `--tests`.

1. **Listed functions.** The listing contains the entries of `EffectInterface.functions` of each package, merged and sorted by the bytes of their canonical paths ([spec/04](../spec/04-modules-and-names.md) §4.9.1). These are the functions that have a `Sig` (§8.2.6): module-level functions, inherent associated functions and methods, trait impl methods, interface impl functions, trait methods with a default body, trait method declarations with a clause (whose `Sig` is the clause), the methods of derived impls (`m.impl[Tr for T].x`), and the functions of `extern` blocks. Closures, `async` blocks and glue bodies are not listed, because they are not symbols ([impl/12](12-semantic-tooling.md) §12.2.1). Tests and properties are not listed, because they are not analyzed without tests (§8.7.12). Imported WIT functions belong to WIT packages, not to the package. Items removed by `@target` do not exist in the build. A trait method declaration without a clause and without a default body has no `Sig`: it bounds nothing (spec/08 §8.6.3).
2. **Incomplete functions.** A function whose `Sig` is incomplete (a clause with a failed `EffectRef`, or no clause and an incomplete `I(f)`, §8.1.9) is omitted, and the diagnostics that made it incomplete are reported, as the command reports every diagnostic of the analysis. A function whose `Sig` is complete is listed even if checks of its body failed; a function with a clause and a tainted body is therefore listed with its clause.
3. **Columns.** `path` is the canonical path. `declared` is true if the function has an `effects` clause; for a trait default body, if the trait method declares one; for an extern function, if a clause is written on it. `effects` is `Sig(f)` in print order (§8.1.6 item 2), each atom printed as `spell(a, M)` with `M` the module of `f`, or in display form if it is not spellable there (§8.1.8). `symbol` is the symbol ID of impl/12 §12.2. `location` is the function's name, and for a derived method the trait path in the `@derive` attribute.
4. **`<path>`.** The path is resolved by [impl/12](12-semantic-tooling.md) §12.2.5. It may name a function of any package of the graph, not only of the listed packages: a dependency function's `Sig` and its why-entries are in its package's metadata (§8.3.8), so the line and the chain can be printed without its source. A path that resolves to a symbol or module that has no `Sig`, or that is the syntactic path of an item removed by `@target` (§8.10.4 item 7), is the info diagnostic `NOT_PRESENT_IN_BUILD`, exit 0, with a note that says why: "`` `<path>` is not a function ``", "`` a trait method without an `effects` clause or a default body has no effect set ``", "`` tests and properties are analyzed by `sai test` ``", or "`` removed by `@target` for target `<t>` ``". A path that names nothing is `UNKNOWN_SYMBOL_PATH`.
5. **`--why <text>`.** The text is matched by bytes against `spell(a, M)` and against the display form of every atom `a ∈ Sig(f)`. At most one atom matches: two distinct atoms have distinct spellings in one module and distinct display forms, a spelling never starts with a built-in root (§8.1.8 rule 2) and never contains `:`, and the display form of every interface and user atom contains `:`. If no atom matches, the command reports `NOT_PRESENT_IN_BUILD` (info, exit 0) with the message "`` `<f>` does not have effect `<text>` ``" and `data { function, effect }`, and the help lists `Sig(f)` in the form of item 3.
6. **The chain.** For the matched atom `a` (N-D-96):
   - if `f` has an effect body whose summary is complete and `a ∈ A(I(f))`, the chain is `why_chain(Body(f), a)` (§8.6.2), mapped by §8.6.4 with the names of `sai inspect`; for a dependency function without a clause, the walk starts at its `WhyEntry` (§8.6.3);
   - otherwise `a ∈ D(f)`, and the chain is one step `{ f, location: the EffectRef of a in f's clause, Declares }` (in `dep:` form for a dependency);
   - for an extern function, the chain is the terminal steps of `Extern(f)` (§8.6.4).
   The chain is printed in the compact form of spec/08 §8.13.3, or as the `steps` of [impl/11](11-diagnostics.md) §11.9.5 in the `inspect.effects.why` record. `--why` without `<path>` is the usage error `MISSING_ARGUMENT` of impl/13.

### 8.9.2 `sai inspect unsafe`

`unsafe_inventory(P, t)` computes the inventory of spec/12 §12.11.1 for one package and one target:

```rust
pub struct UnsafeInventory {
    pub package: PackageId,
    pub non_portable: bool,                  // some entry has kind UnsafeExternC (spec/12 §12.9.2 item 3)
    pub counts: [u32; 7],                    // per UnsafeEntryKind, in the order of §8.10.4
    pub entries: Arc<[UnsafeEntry]>,         // §8.10.4, in the order of §8.10.4 item 9
    pub diagnostics: Vec<Diagnostic>,        // errors of the analysis that limit the entries
}
```

1. **Graph and mode.** The command of [impl/13](13-cli.md) lists every package of the active graph of the selected target ([package/02](../package/02-resolver.md) §2.5.3), in the order of package identities, each with `unsafe_inventory(Q, t)`, filtered by `--package`. The analysis is that of `sai check` without `--tests`, so test code is never analyzed (spec/14 §14.2.4) and its entries are syntactic (§8.10.4 item 8).
2. **Source packages of the workspace.** For the root and the workspace members, the inventory is computed from the `UnsafetyResult` of every owner of the package (§8.10.1), the `unsafe_module_check` of every file (§8.10.6), and a scan of the CST of every file for the entries of items that are removed by `@target` or that are tests or properties (§8.10.4 items 7 and 8).
3. **Dependencies.** For a source dependency, the inventory is `EffectMetadata.unsafe_entries` (§8.3.8) when the metadata exists. The metadata is written per target, without tests, by the computation of item 2, so the decoded and the computed inventories are equal (impl/01 §1.6.5 item 6). A dependency without metadata (one whose analysis has an error) is computed from source as in item 2, and its errors are reported.
4. **Binary dependencies.** A package used in artifact form (D-PKG-05) has no SAI source: it is listed with every count 0, no entry and `non_portable` false.
5. **Errors.** A package whose analysis has errors is still listed. Its tainted owners contribute their entries with the operations that §8.10.3 item 6 recognizes, and the errors are in `diagnostics` and reported by the command. Entries never depend on the outcome of the checks of §8.10.5–§8.10.8.
6. **Symbols and nodes.** `UnsafeEntry` stores canonical paths and node IDs, not symbol IDs. The command computes the symbol ID of `item` by [impl/12](12-semantic-tooling.md) §12.2 from the package and the canonical path, and prints `null` for entries whose `is_symbol` is false. Node IDs are pre-order indices of the file's CST and therefore depend only on the file's content (impl/12 §12.3.2), so stored and recomputed IDs are equal for an unchanged file.
7. **Rendering.** `kind`, `item`, `node`, `location`, `target`, `safety` and `operations` of the JSON record of impl/13 are the fields of `UnsafeEntry` (§8.10.4); `counts` has one member per kind; the human output's operation count of an entry is the length of its `operations`.

### 8.9.3 The effect interface

`effect_interface(P)` is the effect-related public data of one package for the target of the session:

```rust
pub struct EffectInterface {
    pub package: PackageId,
    pub target: TargetId,
    pub functions: Vec<FnEffectEntry>,     // by the bytes of `path`
    pub footprint: Option<Footprint>,      // capability_footprint(P, target); None if it reports errors (§8.7.10 item 4)
    pub incomplete: Vec<FnItem>,           // functions without a complete Sig, by the bytes of their paths
}

pub struct FnEffectEntry {
    pub item: FnItem,                      // Def(DefId) | Derived(OwnerId, Symbol)
    pub path: Arc<str>,                    // canonical path (spec/04 §4.9.1)
    pub declared: bool,                    // §8.9.1 item 3
    pub set: EffectSetId,                  // Sig(f)
}
```

1. The functions are those of §8.9.1 item 1, without tests (§8.7.12).
2. [impl/12](12-semantic-tooling.md) reads `set` for the `effects` aspect of function symbols (impl/12 §12.8.2) and for the `effects` member of the symbol record, and `footprint` for the package-level `capabilities` list. It prints atoms by its own rules.
3. `sai inspect effects` reads `functions` and `incomplete` (§8.9.1).
4. The value is a projection of `sig_effects` and `capability_footprint`, so it backdates whenever those do (§8.8.1); an edit inside a body below a clause never changes it (§8.8.3).

### 8.9.4 Other outputs

| Output | Consumer | Defined in |
|---|---|---|
| `ComponentImports` (`imp`, `functions`, `resource_drops`, `allocator`) | [impl/07](07-component-lowering.md) | §8.7.9 |
| `ReachSet` of a build and its `allocator` flag | [impl/06](06-wasm-lowering-and-layout.md) (allocator linking, D-WASM-05) | §8.7.3 |
| the `capabilities` field of `sai.meta`: the printed names of `Req(root)` of the build, from every source of §8.7.5, in byte order | impl/06 §6.11.4 | §8.7.5 |
| `capability_footprint(P, t)` | [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md), [package/04](../package/04-distribution-and-security.md) | §8.7.10 |
| `reach_from(ReachKey)` | [impl/12](12-semantic-tooling.md) (the `tests` edges of its graphs and the exported interfaces of impact analysis), [impl/10](10-runtime.md) §10.2.7 | §8.7.3 |
| `call_effects(owner)` | [impl/04](04-mir.md) `Analyzed` MIR | §8.3.2 |
| `EffectMetadata` | dependent packages | §8.3.8 |
| diagnostics of every query | the driver, rendered by [impl/11](11-diagnostics.md) | §8.5, §8.7, §8.10 |

---

## 8.10 The unsafe check

This section implements the static rules of [spec/12](../spec/12-unsafe-and-ffi.md): unsafe contexts ([spec/12](../spec/12-unsafe-and-ffi.md) §12.2), unsafe operations ([spec/12](../spec/12-unsafe-and-ffi.md) §12.3), the declaration rules of [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.2, §12.2.3, §12.7.3, §12.8 item 3 and §12.9.1 item 5, the policy of spec/12 §12.11.3, and the inventory of spec/12 §12.11.1. The check belongs to Stage 2; row 4 of spec/12 §12.3.1 and the kind `unsafe_extern_c` take part from Stage 7, and before it every `unsafe extern "C"` block is rejected earlier with `FEATURE_NOT_IMPLEMENTED` (spec/12 §12.9.2 item 4) by [impl/03](03-name-resolution-and-type-checking.md) §3.14.4, so the check never sees one.

This chapter owns the whole check (N-D-86). It computes unsafe contexts itself from the HIR; [impl/03](03-name-resolution-and-type-checking.md) does not track them while type checking (impl/03 §3.7.8). It reads the HIR of [impl/02](02-frontend-ast-hir.md), the `TypeckResults` and item signatures of impl/03, and the CST for doc comments, tokens and fixes. It does not read MIR, does not depend on effect analysis, and runs in phase 9 for every owner, including owners whose type checking failed.

### 8.10.1 Queries and results

`unsafety_check(owner)` checks one owner and returns

```rust
pub struct UnsafetyResult {
    pub diagnostics: Vec<Diagnostic>,       // §8.10.5, §8.10.7, §8.10.8
    pub entries: Vec<UnsafeEntry>,          // the entries whose construct belongs to the owner (§8.10.4)
    pub module_ops: Vec<UnsafeOp>,          // the operations whose context is Module (§8.10.2), in source order
    pub body_ops: bool,                     // some row 1–4 operation is in a body of the owner outside predicates
    pub complete: bool,                     // false if the owner is tainted (§8.10.3 item 6)
}
```

1. **Owners.** Every owner is checked: functions (module-level, associated, trait methods with or without a body, extern and interface functions), traits, impls, `extern` blocks, refined and distinct types with a predicate, invariant declarations, constants, tests and properties. Other owners have an empty result. The owner of a trait or impl item is the item itself; the trait and impl owners carry only the entries and checks of their own headers.
2. **Tests and properties** are checked only by test analysis (spec/14 §14.2.4, `sai test`). In every other command their owners are not queried, and their entries are syntactic (§8.10.4 item 8).
3. **Constants** are not walked for operations: every unsafe operation and every block is `NOT_CONST_EXPRESSION` in a const initializer (spec/06 §6.27). An `unsafe` block in one is still an entry, with no operations.
4. `unsafe_module_check(file)` (§8.10.6) combines the results of the owners of one file for its `unsafe module` header. `unsafe_inventory(P, t)` (§8.9.2) collects the entries.
5. The results are per-owner values of phase 9 and are persisted with the other phase 9 results (§8.8.4). An owner's result depends on its HIR, its `TypeckResults`, the signatures and extern blocks of its callees, its file's header flag and the package's `policy`. An edit of one function body therefore re-runs only that owner and the `unsafe_module_check` of its file.

### 8.10.2 Contexts

Every expression of a walked body has a **context** `ctx ∈ { Safe, Block(b), UnsafeFn, Module }`; every context other than `Safe` is an unsafe context.

1. **Base.** The body of a function owner starts with `UnsafeFn` if its `FnSig.safety` is `Unsafe`, otherwise with `Module` if `ItemTree.is_unsafe_module` holds for its file, otherwise with `Safe`. Test and property bodies start with `Module` or `Safe` by the same rule.
2. **Blocks.** The contents of an `unsafe` block `b` (a HIR `Block` with `safety: Unsafe`, CST `UNSAFE_BLOCK_EXPR`) have the context `Block(b)`, whatever the context of `b` itself. The innermost enclosing unsafe context of an expression is therefore always well defined.
3. **Closures and `async` blocks.** The body of a closure and of an `async` block starts again with `Module` if the file is an unsafe module and with `Safe` otherwise (spec/12 §12.2.1, §12.2.4 item 1; spec/06 N-C-17). The exception is the `async` block that impl/02 §2.9.15 synthesizes for the body of an `async fn`: it is the function's body and keeps the function's context, so the body of an `unsafe async fn` is an unsafe context (spec/11 §11.2.2).
4. **Predicates.** Every predicate is walked with the context `Safe`, in an unsafe module too (spec/12 §12.2.4 item 2): the `requires` and `ensures` expressions of a function (`FnSig.requires`, `FnSig.ensures`), the predicate body of a refined type or of a distinct type with a refinement clause, and the predicates of an invariant declaration. An `unsafe` block inside a predicate is `INVALID_CONTRACT_EXPRESSION` ([spec/09](../spec/09-contracts-and-refinements.md) §9.1.2, reported by its owner). It creates no context, the operations inside it are not reported, and it gets no `UNUSED_UNSAFE`, because the diagnostic of the block already covers them (N-D-98).
5. **Trait and impl headers.** The `unsafe` marker of a trait or impl creates no context: the items of an `unsafe impl` are checked by rule 1 (spec/12 §12.2.3 item 3).

### 8.10.3 Operations

The walk visits the expressions of a body in HIR pre-order, which is source order (impl/03 §3.15.1). An expression is an unsafe operation in these cases, and in no other:

1. **Row 1, dereference.** `Unary(Deref, x)` where the type of `x` after its adjustments (`node_types` and `adjustments`, impl/03 §3.15.1) is `Ptr<T>` or `PtrMut<T>` (lang items `Ptr`, `PtrMut`). Auto-deref never goes through a raw pointer (spec/12 §12.4.4), so a written `*` is the only form; every use of the place `*x` (read, write, projection, borrow, method call on it) is covered by the one operation.
2. **Row 4, foreign call.** `Call` with `call_kinds[e] = Direct(g, _)` where `g` is a function of an `ExternBlock` with `abi: C`. Row 4 is tested before row 2, so a redundant `unsafe fn` marker on the item does not change the row.
3. **Row 2, call of an unsafe function.** `Call` with `call_kinds[e] = Direct(g, _)` where the `FnSig.safety` of `g` is `Unsafe`. For a method call or a type-relative path, `g` is the item recorded in `type_dependent_defs` (`TypeDependentDef::Assoc`); for a call of a trait method through a bound, `g` is the trait method, whose marker equals the marker of every impl (spec/05 §5.9.1 rule 6). This includes `Future.poll`, `Stream.poll_next`, the `unsafe fn` items of `extern "wasm"` blocks and the construction of an `unsafe async fn` future.
4. **Row 3, call through an unsafe function pointer.** `Call` with `call_kinds[e] = FnPtr` where the adjusted type of the callee is `FnPtr` with `safety: Unsafe`.
5. **Never unsafe.** `call_kinds[e] = FnTrait { .. }` (an unsafe function item implements no function trait, spec/12 §12.2.2 item 3), and every operation that is not a HIR `Call` or `Unary(Deref, _)`: operator, index, f-string and `await` resolutions select safe trait methods of `sai:core`, and `Await` is not a call of `poll`. The calls that impl/02 synthesizes for `for` ([impl/02](02-frontend-ast-hir.md) §2.9.5) are ordinary `Direct` calls of safe lang items and are classified by items 2–4 like every call. `?` calls nothing: it is a discriminant switch and an early return ([impl/04](04-mir.md) §4.8.2, D-EXPR-05).
6. **Tainted owners.** If `tainted_by_errors` is set, an expression is recognized only if what its row needs is known: the adjusted operand type for row 1, the call kind and callee item for rows 2 and 4, the adjusted callee type for row 3, none of them `{error}` or containing `{error}`, and no `Err` entry in `type_dependent_defs`. Recognized operations are checked and listed as in a complete owner; the others are skipped, because their meaning is unknown and the type error is already reported (cause key C5 of [impl/11](11-diagnostics.md) §11.11.4). The result has `complete: false` (N-D-86).
7. **Row 5.** Every `unsafe impl` entry has exactly one operation of row 5, at its `unsafe` keyword (§8.10.4 item 6). It is allowed in every context (spec/12 §12.3.1).

Each operation is recorded as

```rust
pub struct UnsafeOp {
    pub row: UnsafeRow,                     // Deref | CallUnsafeFn | CallUnsafeFnPtr | CallExternC | UnsafeImpl
    pub location: PersistentLocation,
    pub callee: Option<Arc<str>>,           // rows 2 and 4: the canonical path of g (spec/04 §4.9.1)
}
```

The location is the whole `*x` expression for row 1; for a call, the callee path expression of a path call, the method `NAME_REF` of a method call (the callee `FIELD_EXPR`'s name), and the callee expression of a function pointer call; for row 5, the `unsafe` keyword. The rows are spelled `deref`, `call_unsafe_fn`, `call_unsafe_fn_ptr`, `call_extern_c` and `unsafe_impl` ([impl/13](13-cli.md)). `callee` is `null` for rows 1, 3 and 5.

### 8.10.4 Inventory entries

```rust
pub enum UnsafeEntryKind { UnsafeBlock, UnsafeFn, UnsafeTrait, UnsafeImpl, UnsafeModule, UnsafeExternWasm, UnsafeExternC }

pub struct UnsafeEntry {
    pub kind: UnsafeEntryKind,
    pub item: Arc<str>,                     // item 2
    pub is_symbol: bool,                    // item 2
    pub node: u64,                          // pre-order index of the construct (impl/12 §12.3.2)
    pub location: PersistentLocation,       // the `unsafe` keyword
    pub target: Vec<Arc<str>>,              // item 4
    pub safety: Option<Arc<str>>,           // item 5
    pub operations: Vec<UnsafeOp>,          // item 6
}
```

1. **Kinds.** One entry per occurrence of the `unsafe` keyword in these positions (spec/12 §12.11.1 item 1): an `UNSAFE_BLOCK_EXPR` (`unsafe_block`); an `FN_DECL` with the `unsafe` marker, wherever it occurs, including the items of `extern` blocks, trait and impl items and interface items (`unsafe_fn`); a `TRAIT_DECL` (`unsafe_trait`); an `IMPL_DECL` (`unsafe_impl`), whether or not the marker is correct; the `MODULE_HEADER` (`unsafe_module`); an `EXTERN_BLOCK` with ABI `"wasm"` (`unsafe_extern_wasm`) or `"C"` (`unsafe_extern_c`). An `unsafe` extern block with another ABI string is `UNKNOWN_ABI` and has no entry. `node` is the construct named in this item. The kinds are spelled as in [impl/13](13-cli.md), and `counts` of §8.9.2 uses this order.
2. **Item.** For an `unsafe fn`, `unsafe trait` or `unsafe impl` entry, the canonical path of the declaration itself. For an `unsafe` block, the canonical path of the innermost enclosing declaration that has one: a function, a trait or impl item, a test or property, a constant, or, for a block in a predicate, the function that owns the clause or the type that owns the refinement or invariant. For a header or an extern block, the canonical path of the module. `is_symbol` is false for module paths and for removed items (item 7), so the symbol ID is printed as `null` (modules are not symbols, impl/12 §12.2.1).
3. **Location.** The `unsafe` keyword of the construct, as a `PersistentLocation`: a byte span in a file named relative to the package root (§8.3.8).
4. **Target.** The `@target` predicate of the module-level item that contains the construct, as one string, the canonical one-line text ([impl/14](14-formatter.md) §14.13) of the predicate inside `@target(…)`; an empty list if that item has no valid `@target`. Members cannot carry `@target` (spec/15 §15.11.3), and a header has none.
5. **Safety.** For `unsafe_fn` and `unsafe_trait` entries, the `# Safety` section of the declaration's doc comment (§8.10.7, item 4 of `MISSING_SAFETY_DOC`): the doc text values of the lines after the heading, up to the end of the section, joined with LF, without the blank lines at its start and end. It is the empty string for a section without text and `null` if the section is absent. Other kinds have `null`.
6. **Operations.** An operation of rows 1–4 is listed in exactly one entry: the entry of the innermost unsafe context of its expression (§8.10.2), which is the block for `Block(b)`, the `unsafe fn` entry for `UnsafeFn`, and the header entry for `Module`. Because closure and `async` block bodies restart their context (§8.10.2 rule 3), an operation in a closure is never listed under a block or function outside the closure. Operations are listed in source order. An `unsafe_impl` entry lists its row-5 operation. `unsafe_trait` and extern block entries list none. An operation outside every unsafe context is an error and is listed nowhere.
7. **Removed items.** An entry inside a module-level item that is removed by `@target` for the target of the command is found by a scan of the CST (spec/12 §12.11.1 item 4: declarations are listed whatever their predicates). Its `item` is a syntactic path, because removed items are never resolved (N-D-94): `<module>.<name>` for a module-level item; `<module>.impl[<text>]` for an impl, where `<text>` is the one-line text of the trait path and the self type joined by ` for `, of the self type of an inherent impl, or of the interface path; `<parent>.<name>` for an item of a removed trait or impl; `<module>.test["name"]` and `<module>.property["name"]` for tests and properties. `is_symbol` is false and `operations` is empty, because operations depend on name resolution, which is per target (spec/12 §12.11.1 item 4).
8. **Tests and properties.** The inventory is computed without test analysis (§8.9.2 item 1), so the entries inside a test or property that exists in the build are found by the scan of the CST as well. Their `item` is the test path `m.test["name"]` (a symbol, `is_symbol` true, spec/14 §14.2.3), and `operations` is empty (N-D-97). An operation in test code is checked, and reported, by `sai test`.
9. **Order.** Entries are ordered by package identity (§8.9.2), then by the bytes of `item`, then by the file path bytes and the start offset of `location` (spec/12 §12.11.1 item 5). The order does not depend on the order in which owners were checked.

### 8.10.5 Operations outside an unsafe context: `UNSAFE_OPERATION_OUTSIDE_UNSAFE`

Every recognized operation of rows 1–4 whose context is `Safe` is the error `UNSAFE_OPERATION_OUTSIDE_UNSAFE` at its location (§8.10.3), except an operation inside an `unsafe` block of a predicate (§8.10.2 rule 4).

1. **Message and note.** The message names the operation: "dereference of raw pointer type `<T>` is unsafe" (row 1), "call of unsafe function `<g>` is unsafe" (row 2), "call through unsafe function pointer type `<T>` is unsafe" (row 3), "call of foreign function `<g>` is unsafe" (row 4), with types printed as in impl/11 §11.6.1 and functions by canonical path. A note gives the reason: "raw pointers carry no guarantee of alignment, bounds, validity or aliasing (spec/12 §12.4.4)", "`<g>` is declared `unsafe fn`; its `# Safety` section states what the caller must ensure", "the function that the pointer designates may have preconditions", or "`<g>` is declared in an `unsafe extern "C"` block". `data` is `{ operation, callee }` with the row name of §8.10.3 and the callee's canonical path or `null`.
2. **Fix.** One `maybe-incorrect` fix `replace_node` that replaces the **wrap target** with `unsafe { <text> }`, where `<text>` is the source text of the target, which the formatting guarantee of impl/11 §11.7.4 item 4 then lays out (N-D-92). The applicability is never `machine-applicable`, because the author must establish the preconditions (spec/12 §12.2.5, D-TOOL-01). The wrap target is the innermost of these CST nodes that contains the operation: the expression of an `EXPR_STMT`; an `ASSIGN_STMT` (simple or compound) or an `ASSERT_STMT` as a whole; the initializer expression of a `LET_STMT`, `LET_ELSE` or `VAR_STMT`; the tail expression of a `BLOCK`. Closure and `async` bodies are blocks, so the target never contains the closure or `async` boundary that the operation is in. This is the smallest enclosing statement of spec/12 §12.2.5 in a form whose wrapped text is again valid: a `let` keeps its bindings in scope, and a tail keeps its value. Several operations with the same wrap target get identical fixes, which impl/11 §11.7.5 merges.
3. **Predicates.** An operation in a predicate gets no fix, because no `unsafe` block is allowed there, and the note "predicates are never unsafe contexts, even in an `unsafe module` (spec/12 §12.2.4)".

### 8.10.6 The module header: `unsafe_module_check`

`unsafe_module_check(file)` reads `ItemTree.is_unsafe_module` and, if the header is present, the `UnsafetyResult` of every owner of the file that exists in the build, except tests and properties. It returns the `unsafe_module` entry, its `UNSAFE_DENIED` (§8.10.8) and its `UNUSED_UNSAFE`:

1. **Entry.** `item` is the module path, `node` the `MODULE_HEADER`, `location` its `unsafe` keyword, `target` empty, and `operations` the `module_ops` of the owners merged in source order.
2. **Unused header.** The header is **used** if some owner has `body_ops`: some operation of rows 1–4 is in a function, closure or `async` body of the file outside predicates, in any context (spec/12 §12.2.4 item 4: "an unsafe module that contains no unsafe operation"). Row 5 does not count, because `unsafe impl` needs no context, and operations in predicates do not count, because the header does not cover them (N-D-93).
3. **When it is decided.** An unused header is the warning `UNUSED_UNSAFE` only if the decision is certain for the build: no owner of the file has `complete: false`, the file declares no test or property, and no module-level item of the file is removed by `@target`. Otherwise an unrecognized operation, an operation in test code that only `sai test` analyzes, or an operation in code of another target could use the header, and the warning is not reported.
4. **Diagnostic.** At the `unsafe` keyword of the header, with the message "unused `unsafe module` header", the note "the file contains no unsafe operation", and a fix `replace_node` of the `MODULE_HEADER` whose text is the text of its `DOCS` child with its line breaks, or empty if it has none: the fix removes the `unsafe` and `module` tokens and their `NEWLINE` and keeps the inner doc comment. Its applicability follows N-D-95: `machine-applicable` if the package's `targets` list has exactly one element, `maybe-incorrect` otherwise.

### 8.10.7 Declaration checks

`unsafety_check` reports the following for the declarations of its owner. None of them depends on the context of §8.10.2 except item 1.

1. **`UNUSED_UNSAFE` on blocks** (lint, default `warn`, at the `unsafe` keyword of the block, with `data { kind: "block" }`):
   - **Redundant.** A block whose own context is not `Safe` (an enclosing block, an `unsafe fn` body, or an unsafe module) is reported with the note "redundant: already in an unsafe context" and a related entry at the `unsafe` keyword of the enclosing context (the block, the function, or the header). The fix `remove_token` of the keyword is `machine-applicable`, because the context depends only on the syntax and the file's header.
   - **Unused.** Otherwise, a block with no recognized operation of rows 1–4 inside it outside nested closure and `async` bodies is reported with the note "the block contains no unsafe operation". It is not reported in an owner with `complete: false`. The fix `remove_token` of the keyword, which leaves a plain block, follows N-D-95, because whether a call is unsafe depends on name resolution, which can differ between targets.
   - A block is reported at most once; redundancy is decided first. Blocks in predicates are not reported (§8.10.2 rule 4).
2. **`UNUSED_UNSAFE` on extern declarations.** An `unsafe extern "wasm"` block none of whose functions has an `ensures` clause is reported at the block's `unsafe` keyword with the note "the marker is needed only for imports with an `ensures` clause (spec/12 §12.7.3)" and `data { kind: "extern_block" }`. An `unsafe fn` marker on a function of an `unsafe extern "C"` block is reported at the function's `unsafe` keyword with the note "every function of an `unsafe extern "C"` block is unsafe to call (spec/12 §12.9.1)" and `data { kind: "extern_fn" }`. Both carry a `machine-applicable` fix `remove_token`, because both conditions are syntactic.
3. **`IMPL_UNSAFETY_MISMATCH`** (error), for an impl owner, with `data { impl, trait, expected }`, where `impl` is the impl's canonical path (for an inherent impl, the `CanonType` of its self type), `trait` the trait's canonical path or `null`, and `expected` `"unsafe"` or `"safe"`:
   - a trait impl without `unsafe` whose trait is an `unsafe trait`: at the `impl` keyword, message "impl of unsafe trait `<Tr>` must be written `unsafe impl`", a related `declared_here` entry at the trait's `unsafe` keyword, and a `maybe-incorrect` fix `insert_before` of `unsafe ` on the `impl` token, because writing it is a promise that the author must check (spec/12 §12.2.3 item 1);
   - an `unsafe impl` of a trait that is not unsafe: at the `unsafe` keyword, message "`<Tr>` is not an unsafe trait", and the fix `remove_token`, whose applicability follows N-D-95, because which trait the path names is decided by name resolution;
   - an `unsafe impl` that is inherent or implements an interface: at the `unsafe` keyword, message "an inherent impl cannot be unsafe" or "an interface impl cannot be unsafe", and a `machine-applicable` fix `remove_token`.
   The check is skipped if the trait path did not resolve, because that error is already reported. Derived impls are never checked: `@derive` generates no impl of an unsafe trait (spec/12 §12.2.3 item 4).
4. **`MISSING_SAFETY_DOC`** (lint, default `warn`), at the declaration's name, with `data { item }` and no fix, for a declaration without a `# Safety` section:
   - a module-level `unsafe fn` with visibility `pub` (an `export unsafe fn` is `UNSAFE_EXPORT` instead, item 6);
   - an inherent associated `unsafe fn` with visibility `pub`;
   - an `unsafe fn` method of a trait whose visibility is `pub`;
   - a `pub unsafe trait`.
   Trait impl items are not checked, because their contract is the trait method's section. Extern functions are package-private (spec/12 §12.7.1 item 4), and interface functions cannot be unsafe. **The section** (N-D-99): the doc comment of a declaration is the `DOCS` child of its node (impl/02 §2.6), read line by line as doc text values (`ast::DocComment::text()`). Lines from a line whose text starts with three backticks up to the next such line are fenced code and are skipped. The heading is the first other line whose text, without trailing whitespace, is exactly `# Safety`. The section runs from the line after the heading to the line before the next level-1 heading (a line outside fenced code that starts with `# `), or to the end of the comment. A heading with an empty section counts as present: the lint checks that the obligations have a place, not their wording.
5. **`EXTERN_ENSURES_REQUIRES_UNSAFE`** (error). For every function with an `ensures` clause in an `extern "wasm"` block without `unsafe`, one error at the function's first `ensures` keyword, with `data { function }` and a `maybe-incorrect` fix `insert_before` of `unsafe ` on the block's `extern` token, because the marker accepts a claim that nobody checks (spec/12 §12.7.3 item 3). The fixes of several functions of one block are identical.
6. **`UNSAFE_EXPORT`** (error). A function with visibility `export` and `FnSig.safety = Unsafe`, at its `unsafe` keyword, with `data { function }` and no fix: removing the marker or the export changes the function's contract or its interface, so the author must choose (spec/12 §12.8 item 3). It is reported in every package and every build, because the rule depends only on the declaration.

Every diagnostic of this section is reported only for declarations that exist in the build: removed items produce no diagnostic (spec/15 §15.11.3 item 2).

### 8.10.8 Policy: `UNSAFE_DENIED`

If the `policy` of the package's `sai.package` contains `deny unsafe` ([package/01](../package/01-package-contract.md) §1.12), every entry of the package's own source that exists in the build, other than `unsafe_trait`, is the error `UNSAFE_DENIED` at the entry's `unsafe` keyword (spec/12 §12.11.3). The message is "`unsafe` is denied by the policy of package `<P>`", `data` is `{ package, kind }` with the kind spelled as in §8.10.4, and a related `declared_here` entry points at the `deny unsafe` item. There is no fix: the policy is the author's decision, and removing unsafe code requires rewriting it. The policy applies whenever the package is compiled, including as a dependency (package/01 §1.12 item 3). `unsafety_check` reports the entries of its owner and `unsafe_module_check` the header, so every construct is reported once. Test code is checked by test analysis only, and removed items not at all, like every other diagnostic.

### 8.10.9 Example

```sai
//! Raw buffer helpers.
use sai:core/ptr.{Ptr, PtrMut}

/// Reads the value at `p`.
///
/// # Safety
///
/// `p` must be aligned and point to an initialized `I32`.
pub unsafe fn read_i32(p: Ptr<I32>) -> I32 {
    *p                                      // allowed: the body of an unsafe fn
}

pub unsafe fn clear(p: PtrMut<I32>) {       // warning: MISSING_SAFETY_DOC
    *p = 0
}

fn sum(p: Ptr<I32>, q: Ptr<I32>) -> I32 {
    let a = *p                              // rejected: UNSAFE_OPERATION_OUTSIDE_UNSAFE; fix `let a = unsafe { *p }`
    let b = unsafe { read_i32(q) }
    a + b
}

fn twice(p: Ptr<I32>) -> I32 {
    unsafe {
        let f = fn() -> I32 { *p }          // rejected: UNSAFE_OPERATION_OUTSIDE_UNSAFE; a closure body restarts as Safe
        unsafe { read_i32(p) } + f()        // warning: UNUSED_UNSAFE (redundant)
    }
}

fn one() -> I32 {
    unsafe { 1 }                            // warning: UNUSED_UNSAFE (no unsafe operation)
}

pub unsafe trait Plain {}                   // warning: MISSING_SAFETY_DOC

unsafe impl Plain for U32 {}

impl Plain for Bool {}                      // rejected: IMPL_UNSAFETY_MISMATCH; fix `unsafe impl`

unsafe extern "wasm" module "env" {         // warning: UNUSED_UNSAFE (no import has an `ensures` clause)
    fn log(v: I32)
}

extern "wasm" module "env" {
    fn random_below(bound: U32) -> U32
        requires bound > 0
        ensures result < bound              // rejected: EXTERN_ENSURES_REQUIRES_UNSAFE
}

export unsafe fn poke(addr: USize) {}       // rejected: UNSAFE_EXPORT
```

In module `acme:raw/buf`, the inventory lists these entries, in the order of §8.10.4 item 9:

| Kind | `item` | Operations |
|---|---|---|
| `unsafe_extern_wasm` | `acme:raw/buf` | — |
| `unsafe_trait` | `acme:raw/buf.Plain` | — |
| `unsafe_fn` | `acme:raw/buf.clear` | `deref` |
| `unsafe_impl` | `acme:raw/buf.impl[acme:raw/buf.Plain for U32]` | `unsafe_impl` |
| `unsafe_block` | `acme:raw/buf.one` | — |
| `unsafe_fn` | `acme:raw/buf.poke` | — |
| `unsafe_fn` | `acme:raw/buf.read_i32` | `deref` |
| `unsafe_block` | `acme:raw/buf.sum` | `call_unsafe_fn acme:raw/buf.read_i32` |
| `unsafe_block` | `acme:raw/buf.twice` (outer) | — |
| `unsafe_block` | `acme:raw/buf.twice` (inner) | `call_unsafe_fn acme:raw/buf.read_i32` |

The `*p` in the closure of `twice` and in `sum` are errors and are listed nowhere (§8.10.4 item 6).

---

## 8.11 Complexity

This section states the cost of every algorithm of this chapter. The notation is:

- for one package `P`: `N` nodes and `E` references of its constraint system (§8.2.2), `B` the total size of the MIR of its effect bodies, `L` the number of latent keys and `Inst` nodes reached from it;
- `|U|` the number of atoms of the build (§8.2.1), `|S|` the size of an effect set, `|D*|` the size of an expanded set (§8.4.1);
- for one build: `V_R` reach vertices and `E_R` reach edges (§8.7.3), `|Req|` required capabilities, and `G` the number of packages.

A set operation on two interned sets costs `O(1)` for the built-in part (the bitmask of §8.1.6) and `O(|S₁| + |S₂|)` for the other atoms; interning costs one hash of the result. Equality is `O(1)`.

### 8.11.1 Effect analysis

| Step | Cost | Remark |
|---|---|---|
| `resolved_effect_set`, `effect_decl_check` | linear in the number of `EffectRef`s, times one path lookup each | §8.1.7 |
| `covers_graph(P)` | `O(V + E)` over the user effects of `P` and their `covers` edges | one Tarjan pass |
| `covers_star(u)` | `O(V + E)` over the `covers` graph reachable from `u`, once per effect | memoized and interned |
| `body_effect_summary(b)` | `O(\|b\|)` for the walk of the MIR of `b`, plus one `evaluate_goal` per trait-method call and per `Await` of rows 3–8 and 14–17 | selections are cached by [impl/03](03-name-resolution-and-type-checking.md) §3.8.4 |
| `latent_node(k)` | one `evaluate_goal`, plus `O(m + q)` for the `m` methods and `q` predicates of the selected impl | once per key; keys are interned |
| `effect_solution(P)` | `O(N + E)` visits and one set union per edge; at most `O((N + E) · \|U\|)` atom operations | one iterative Tarjan pass (§8.2.4), no iteration to a fixpoint |
| residuals inside an SCC `S` | `O(\|S\| + E_S)` for the second Tarjan pass over the `full` subgraph of `S`, plus one union of residual sets per edge of its condensation | §8.2.4 item 2 |
| `sig_effects`, `call_effects` | `O(1)` per function or terminator after the solution | projections |

The number of latent keys `L` is bounded by the number of distinct normalized goals that the bodies of `P` instantiate. Polymorphic recursion cannot create infinitely many keys, because a goal whose self type is nested more than 64 levels deep is not expanded (§8.3.4 item 3) and the solver has its own limit ([impl/03](03-name-resolution-and-type-checking.md) §3.8.4). Each key is expanded once, whatever the number of operations that reach it, so the analysis of generic code is linear in the number of distinct instantiations that it names, never in the number of their uses, and never requires monomorphization (D-EFF-02).

### 8.11.2 Permits and checks

| Step | Cost | Remark |
|---|---|---|
| `permit_index(D, P)` | `O(\|D*\| log \|D*\|)` to build | once per pair, interned |
| `permits(ix, a)` | `O(log \|D*\|)` | bitmask for built-in atoms, binary search otherwise (§8.4.2) |
| `missing(ix, A)`, `A ⊑ₚ D` | `O(\|A\| · log \|D*\|)` | §8.4.3 |
| `witnesses(ix, a)` | `O(\|D\| · log \|D*\|)` | §8.4.3 item 2 |
| clause check of one body | `O(\|A\| · log \|D*\|)`, plus one scan of the operations of the body for the first contributing operations of the missing atoms | the scan runs only if `missing` is non-empty |
| bound-cap and overflow checks of one package | `O(N_Λ + E_Λ)` for the `Λ` subgraph, plus one permit test per capped reference and cap | see below |
| unused-declaration check of one body | `O(\|A\| · \|D\| · log \|D*\|)` | §8.5.8 |

**Λ-only reachability.** The bound-cap check (§8.5.4) and the overflow rule (§8.3.5) ask which `Λ` nodes are reachable from the references of an operation through `Λ`-to-`Λ` edges. Each capped reference `(Λ(k), caps)` is tested once per package, because its verdict depends only on `A(Λ(k))`, `R(Λ(k))`, its caps and `P`. One Tarjan pass over the `Λ` subgraph then computes, for every `Λ` node, whether a failing capped reference or an `ovf` constant is reachable from it through `Λ` edges only. Only an operation whose `BoundRef`s reach a failure performs the depth-first traversal in preorder that orders its diagnostics. A package without bound-cap errors and overflows therefore pays `O(N_Λ + E_Λ)` in total, and the cost of the traversals is proportional to the errors that are reported.

### 8.11.3 Why-chains

| Step | Cost | Remark |
|---|---|---|
| link generation for one atom `a` | linear in the operations whose values contain `a`, plus the collapsed links | §8.6.1 |
| collapsed links | `O(N_Λ + E_Λ)` per atom for a Tarjan pass over the `Λ` and `Inst` subgraph that collects the targets reachable from each node | memoized per `(Λ(k), a)` |
| `why_chain(s, a)` | `O(V_s + L_s)` for the closure and the bucket-queue search, plus `O(dist(s) · d)` for the walk, with `d` the largest out-degree on the path | Dial's algorithm (§8.6.2) |
| why tables of a package (§8.6.3) | one multi-source search per atom of the package's own sets: `O(\|U_P\| · (V + L))` | computed when metadata is written |
| capability chain search for `(K, r, F)` | `O(Σ (V_a + L_a) + V_R + E_R)`, the sum over the atoms `a` with a switch for `r` | 0-1 bucket search (§8.6.5) |

Chains are computed only for a reported diagnostic, for `sai inspect` and for the why tables of metadata. A build that reports no diagnostic performs no chain search other than the why tables.

### 8.11.4 Reachability and capabilities

| Step | Cost | Remark |
|---|---|---|
| `reach_from` | `O(V_R + E_R)` for the worklist, plus `O(n log n)` to sort each list of `n` elements of `ReachSet` | each vertex expanded once (§8.7.3) |
| `drop_glue_edges(env, T)` | linear in the size of `T`'s field and variant types, once per `(env, T)` | memoized |
| `required_capabilities` | `O(Σ_f \|Own(f)\|)` over the reached bodies, plus one `atom_caps` per atom | `atom_caps` is memoized per atom |
| per-package and root policy checks | `O(\|Req\| · (\|allow\| + \|deny\| + \|Den(p)\| + \|Lift(p)\|))` per checked policy | §8.7.6 |
| grant check | `O(G + edges)` to compute `C(Q)` for every `Q` bottom-up over the package graph, plus `O(\|C(Q)\| · \|G_e\|)` per edge `e` with grant names `G_e` | `C(Q)` is memoized per `Q` |
| target check | `O(\|Req\|)` | §8.7.8 |
| `component_imports` | linear in `Own` sets of reached bodies plus the WIT type references of the interfaces of `W'` and of `Imp`, each interface visited once | rules 2 and 3 until nothing changes (§8.7.9) |
| `capability_footprint(P, t)` | `O(Σ_f \|Own(f)\|)` over `effect_bodies(P)`, plus `O(c log c)` for the normalization of `c` capabilities | §8.7.10 |

### 8.11.5 The unsafe check

| Step | Cost | Remark |
|---|---|---|
| `unsafety_check(owner)` | linear in the HIR of the owner, plus the doc-comment lines of its declarations for `MISSING_SAFETY_DOC` and `safety` | one pre-order walk (§8.10.3) |
| `unsafe_module_check(file)` | linear in the number of owners of the file and the length of their `module_ops` | §8.10.6 |
| CST scan for removed items and test code | linear in the size of the CST of the file | §8.10.4 items 7 and 8 |
| `unsafe_inventory(P, t)` | linear in the number of entries and operations, plus `O(n log n)` to sort `n` entries | §8.10.4 item 9 |

### 8.11.6 Memory

A summary stores `O(|b|)` records for a body `b`. The solution stores one value per node; its effect sets are interned, so equal sets are shared. Metadata stores one `SigFacts` per function, one `BodyFacts` per body with one `WhyEntry` per atom of its own set, and the reach edges of each body (§8.3.8): linear in the size of the package and independent of its dependents. No structure of this chapter grows with the number of monomorphic instantiations.

---

## 8.12 Testing

The rules that this chapter implements are tested at two levels ([impl/15](15-conformance-and-testing.md) §15.1.2): portable conformance tests of the language rules, and compiler-level tests of the algorithms and invariants of this chapter. Both are part of the merge gate of the reference implementation (impl/15 §15.12.6).

### 8.12.1 Conformance tests

The portable tests are those of [spec/08](../spec/08-effects-and-capabilities.md), [spec/12](../spec/12-unsafe-and-ffi.md) and [spec/14](../spec/14-testing.md) §14.3, in the chapter directories of impl/15 §15.2.1, with the stages of impl/15 §15.9.1 (unsafe code: Stage 2; effects and capabilities: Stage 3; the boundary rules: Stage 4). For the claim of the reference implementation (impl/15 §15.9.5), the suite MUST contain, in addition to the coverage that impl/15 §15.9.2 requires:

1. **Each code, accepted and rejected.** For every code of §8.14 that this chapter reports, a `check-fail` test with the annotation and a `check-pass` test of the nearest accepted program. The capability codes use directory tests with a hand-written `sai.package`; `CAPABILITY_NOT_GRANTED` uses a direct and a transitive dependency, so that both forms of the message of §8.7.7 occur. The `wasi:io` implication has three tests of its own: a package with `allow { wasi:cli }` whose code writes to standard output is accepted, adding `deny { wasi:io/streams }` makes it `CAPABILITY_DENIED`, and `grant { wasi:cli }` on an edge grants the `wasi:io` interfaces of the dependency ([targets](../targets/targets-and-profiles.md) §3.3). A root whose `W'` lacks an interface that a composed artifact provides is accepted (§8.7.9 item 6).
2. **Each data shape.** A JSON golden (impl/15 §15.4.4 item 3) for every form of the following diagnostics:
   - `EFFECT_NOT_DECLARED`: one atom; several atoms at one operation (N-D-70); a fix; no fix because an atom is not spellable (N-D-67); no fix because a `pub` function would name a private effect;
   - `TRAIT_IMPL_EFFECTS_EXCEED_BOUND`: impl method with and without a clause; `DROP_HAS_EFFECTS`: each of the three items of §8.5.3;
   - `BOUND_EFFECTS_EXCEEDED`: a missing atom, an uncapped residual with the `insert_after` fix, a capped impl predicate with the note "required by `impl …`", and a second failing cap of the same bound that is not reported (N-D-78);
   - `EFFECTFUL_PREDICATE`: each of the four sites; `MISSING_EFFECTS_DECLARATION`: each reason; `PUBLIC_EFFECTS_INFERRED` in a package of version 0.x;
   - `UNUSED_EFFECT_DECLARATION`: a single-target package (machine-applicable) and a package with two `targets` (maybe-incorrect, N-D-95);
   - `CAPABILITY_NOT_ALLOWED`: the profile case for each placement of the fix (N-D-84), and for a `wasi:io` capability that `allow { wasi:cli }` grants only by the implication, whose location, `policy` field and fix use the witness entry `wasi:cli`; the other case; and each pseudo-root (N-D-81), including an indirect artifact dependency; `CAPABILITY_DENIED`; `TARGET_CAPABILITY_UNAVAILABLE` for `host` in a component and for a `wasi` capability on a core target;
   - `WORLD_IMPORT_UNDECLARED`: each `ImpRule`, for a local and a non-local world, and an import of a composed artifact (§8.7.9 item 6);
   - `TEST_EFFECT_NOT_PERMITTED`: with the suppressed policy violation (N-D-88) and with a violation that a non-test root keeps;
   - `UNSAFE_OPERATION_OUTSIDE_UNSAFE`: each row of §8.10.3 in a safe body, and each wrap target of §8.10.5 item 2 (N-D-92); `UNUSED_UNSAFE`: each `kind`, the redundant and the unused block, and the unused header (N-D-93); `IMPL_UNSAFETY_MISMATCH`: each of its three forms; `MISSING_SAFETY_DOC`: each declaration form; `EXTERN_ENSURES_REQUIRES_UNSAFE`; `UNSAFE_EXPORT`; `UNSAFE_DENIED`: each entry kind, and in a dependency.
3. **Contexts.** One test per pair of a row of §8.10.3 and a context: a safe body, an `unsafe` block, an `unsafe fn` body, an `unsafe module`, a closure and an `async` block inside an `unsafe` block, the body of an `unsafe async fn`, and a predicate in an `unsafe module` (N-D-98).
4. **`# Safety` sections** (N-D-99). A heading with trailing whitespace; a `# Safety` line inside fenced code, which is not a heading; `## Safety`, which is not a heading; an empty section; a section ended by the next `# ` heading.
5. **Chains.** JSON goldens fix the chains of `EFFECT_NOT_DECLARED`, `BOUND_EFFECTS_EXCEEDED` and the capability codes for: two candidate calls on one line, decided by the byte offset; two links at one location, decided by the target; a chain through a latent expansion (`Instantiates`); a chain through `.await` (`Awaits`); a chain into a function of a dependency without a clause, with `dep:` locations (§8.6.3); a chain into a WASI import, whose last step has no location (N-D-80); a chain through export glue (§8.7.4); and a chain of more than 32 steps, which is shortened.
6. **Fixes.** Every fix of this chapter has a `.fixed.sai` sidecar test (impl/15 §15.4.4 item 4). For every `machine-applicable` fix, the following `sai check` reports neither the fixed diagnostic nor any diagnostic that the original program did not have.
7. **Profiles and targets.** The equivalence tests of impl/15 §15.6.1 compare the diagnostics of `check-*` tests across profiles. In addition, tests restricted with `profiles` directives cover the default denials of `tiny` (§8.7.6).

### 8.12.2 Compiler-level tests

The compiler-level tests of this chapter live in `tests/impl/effects/` and `tests/impl/unsafe/` (impl/15 §15.12.2) and use the crates of the reference implementation directly:

1. **Reference solution.** For every program of the conformance suite of Stages 0–4 and every program of the generator of impl/15 §15.10.2, the solution of §8.2.4 equals, node by node and in all four components, the solution of a naive round-robin Kleene iteration over the same constraint system, started from the bottom element (§8.2.3).
2. **Order independence.** A test-only setting of the solver visits the roots of §8.2.4 in reverse and in a seeded random order, and permutes the successors of every node. The solution, every diagnostic, the metadata and every chain are byte-identical to those of the stable order (§8.2.5).
3. **Permits.** A property test generates `covers` graphs over several packages, declared sets and atoms, and compares `permits` with a direct evaluation of the four rules of spec/08 §8.2.4 (the lemma of §8.4.2), and checks the single-atom witness property of §8.4.3 item 2.
4. **Chains.** A property test generates link graphs with ties, cycles and several terminals, enumerates every chain of up to 8 links, and compares the least by `(length, link keys)` with the walk of §8.6.2. For multi-package tests, a test-only mode builds the union of the link graphs of all packages from source and runs §8.6.2 on it; its chains equal the chains computed with the why tables of §8.6.3, step by step, with locations compared as (package, file, offset).
5. **Metadata.** The metadata test of impl/15 §15.6.6 item 1 compares the fingerprints of `EffectMetadata`, including `unsafe_entries`, decoded and computed. An additional test checks that a package with an effect error writes no metadata and has no footprint (§8.3.8 item 1, §8.7.10 item 4).
6. **Independence from optimization and profiles.** For every build test, `reachable`, `required_capabilities`, `component_imports` and the `capabilities` field of `sai.meta` are equal for `dev`, `release` and `tiny`, because they are computed before optimization (§8.1.1 item 4).
7. **Soundness of reachability.** Every `run-*` test is also built with the test-only instrumentation `reach-trace`, which records each SAI function entered and each import and extern called during execution. Every recorded function, mapped from its instance to its generic body, is in `ReachSet.bodies`, and every recorded import is in `ReachSet.imports` or `ReachSet.externs`.
8. **Incrementality.** One test of `tests/impl/incremental/` (impl/15 §15.6.4 item 2) per row of the table of §8.8.2, with the queries of this chapter that the edit may execute. A body edit that keeps the operations executes no `effect_solution`; a body edit below a clause executes no `effect_check` of a caller (§8.8.3); a policy edit executes only `capability_check`.
9. **Limits.** A program with polymorphic recursion reaches the depth limit of §8.3.4 item 3: exactly one `INSTANTIATION_DEPTH_EXCEEDED` is reported, at the operation whose own expansion overflows, and none at its callers (N-D-75). A program whose bound-cap check fails behind a cycle of latent nodes is reported once per operation.
10. **Internal errors.** The injection tests of impl/15 §15.12.3 include `effect_solution`, `capability_check` and `unsafety_check`.
11. **Inspection.** Goldens of the JSON records of `sai inspect effects` (the listing; a `<path>` in a dependency, N-D-96; `--why` with a spelled and with a display-form atom; each note of `NOT_PRESENT_IN_BUILD`), of `sai inspect capabilities` (each status and its precedence; pseudo-roots), and of `sai inspect unsafe` (the example of §8.10.9; removed items, N-D-94; entries in test code, N-D-97; a binary dependency).
12. **`maybe-incorrect` fixes.** Every `maybe-incorrect` fix of this chapter is applied with the fix engine of [impl/11](11-diagnostics.md) §11.7.5. The result is syntactically valid and formats to a canonical file, and the fixed diagnostic is no longer reported. Other diagnostics may appear, which is why these fixes are not `machine-applicable`.

---

## 8.13 Decisions introduced by this chapter

**N-D-65 `Call.effects` is informative.** The `effects` field of a MIR `Call` terminator is a copy of the operation's contribution, filled when impl/04 produces `Analyzed` MIR and empty before. No later phase uses it to accept or reject a program; every effect check reads the solution of this chapter (§8.1.1 item 3). *Rationale:* a program is accepted by a single source of truth, so the field and the solution can never disagree on a decision (P9), and the field still makes the contributions visible in `sai inspect mir` (P1).

**N-D-66 Internal order and print order.** Atoms are stored in an internal order that does not depend on any module, and the canonical order of spec/08 §8.2.3 is computed only when a set is printed. The two orders differ only among user effects, whose print order depends on their spelling in the printing module (§8.1.6). *Rationale:* the print order of user effects depends on the printing context, so a context-free internal order keeps stored sets, query keys and metadata stable (P8).

**N-D-67 Unspellable atoms get a help, not a fix.** A structured fix that would have to print an atom that is not spellable in the module of the fix is not offered. The diagnostic instead carries a help that names the atom in display form and the `use` declaration that would bring it into scope (§8.1.8). *Rationale:* a fix that also inserted a `use` would choose an import name for the user and could change the resolution of other names (P1, P9).

**N-D-68 Capability analysis is gated.** Capability analysis of a build runs only if every effect body and predicate site of every package is complete and no effect error was reported; otherwise `capability_check` reports nothing and records that it did not run (§8.1.9 rule 4). *Rationale:* spec/08 §8.12.6 defines the checks "after effect inference has succeeded", and capability errors computed from partial reachability would mislead (P9, P8).

**N-D-69 Shared latent keys.** A latent key that involves only foreign impls is a node of each consuming package, computed independently with the same result. A package that sees more impls than another, such as a dependent, builds its own node for the key (§8.2.7, §8.3.4 item 7). *Rationale:* the expansion of a key depends only on the key and on the impls visible to the consumer, so independent computation is deterministic and needs no cross-package node (P8, P7).

**N-D-70 One diagnostic per first contributing operation.** A check that reports at the first contributing operation reports one diagnostic per operation, carrying every atom whose first contributing operation it is, not one diagnostic per atom (§8.5.1). *Rationale:* several diagnostics at one location with overlapping fixes are noise; one diagnostic per operation keeps every primary location distinct and every fix single (P8).

**N-D-71 Unused declarations are order independent.** `UNUSED_EFFECT_DECLARATION` is decided for each declared atom against the whole own set, and the result does not depend on the order of the clause (§8.5.8). *Rationale:* the result must not depend on how the clause is written (P8), and because the permit relation uses one witness per atom, removing every reported atom at once keeps the function valid (P9).

**N-D-72 Predicate operations come from HIR.** The operations of a predicate site are taken from its typed HIR, not from MIR; its MIR is read only for reachability (§8.1.5, §8.3.1, §8.3.7). *Rationale:* one analysis per site, on the representation that every predicate has, gives a single answer for every predicate (P9).

**N-D-73 Operation nodes.** Every contributing operation has its own node `O(o)` in the constraint system, so that each check reads the value of a single operation. A trait method call whose selection is `Ambiguous` or `Failed` after type checking succeeded is `INTERNAL_COMPILER_ERROR` (§8.2.2, §8.3.2 note 5, §8.3.4). *Rationale:* the checks need per-operation values, and a node per operation gives them without a second pass (P7); type checking proved every obligation of the body, so a failed selection is a compiler defect, not a property of the program (P2, P9).

**N-D-74 `Empty` environment keys.** A latent key whose goal mentions no type parameter and no projection, in a context whose `ParamEnv` has no predicate without type parameters, uses the environment `Empty` and is shared by every body of the package (§8.3.4 item 1). *Rationale:* such a goal is answered by impls alone, so sharing the key saves work without changing any selection (P7, P9).

**N-D-75 Overflow is reported through `Λ`-only paths.** An operation overflows only if an overflowing latent node is reachable from its own latent references through latent-to-latent edges; references through `I(·)` and `Inst(·)` nodes do not count (§8.3.5). *Rationale:* the error is reported once, at the operation whose own expansion overflows, and not again at every caller of its body (P8).

**N-D-76 Foreign metadata contents.** The effect metadata of a package is the record `EffectMetadata`: the `Sig` of every function with the locations of its clause atoms, the `covers` sets, the own set, reachability edges and why-table of every effect body, the reachability edges of predicate sites, the footprint and the unsafe entries, with locations as `PersistentLocation` relative to the package root. No node, latent key or other part of the package's constraint graph is stored (§8.3.8). *Rationale:* dependents need exactly these facts for checks and chains into foreign functions; minimal metadata that is independent of any dependent keeps the artifact small and reproducible (P7, P8).

**N-D-77 Only instantiating operations are checked for bound caps.** An operation instantiates a function item only if it names the item with its arguments: a `Call` with a `Const` callee, a `Reify` of a `Const` and a `FnItemValue`. Only these operations have bound references and are checked by `BOUND_EFFECTS_EXCEEDED`; calls through function values, function-trait calls on function items and the `Await` of an `async fn` future are not (§8.3.1). *Rationale:* the arguments of an item are checked where the item is named, so each instantiation is checked once, at the location the user wrote (P8).

**N-D-78 Only the first failing cap is reported.** For a checked reference with caps `E₁, …, Eₙ`, one `BOUND_EFFECTS_EXCEEDED` is reported, for the first `Eᵢ` in elaboration order that fails (§8.5.4). *Rationale:* the other caps of the reference fail for the same cause, so further errors would repeat it (P8); the diagnostic is an error, so the next run after a change checks the remaining caps and nothing is missed (P9).

**N-D-79 Authority fixes are `maybe-incorrect` and local.** The fix of `CAPABILITY_NOT_ALLOWED` (an `allow` edit), the fix of `CAPABILITY_NOT_GRANTED` (a `grant` edit) and the fix of `WORLD_IMPORT_UNDECLARED` (an `import` line in a local world) are never `machine-applicable`. The policy and grant fixes are offered only when the edited package is the root or a workspace member; a dependency's manifest is never edited (§8.7.6, §8.7.7, §8.7.9). *Rationale:* granting authority is a decision that the user confirms explicitly (P5, P1).

**N-D-80 Total link order, no `Covers` links, no WASI WIT locations.** Links of a why-chain are compared by a key that extends the three components of spec/08 §8.13.2 with the target's symbol, display name, stable key and the relation, so the order is total; a link without a location sorts last. Chains never contain the relation `Covers`, and a step at a WASI WIT file shipped with the toolchain has no location (§8.6.1, §8.6.4). *Rationale:* a total order makes every chain deterministic (P8); spec/08 §8.13.3 ends a chain at the declaring function and does not make expansion links steps; impl/11 has no `SourceRef` form for toolchain files, and a fabricated path would differ between installations (P8, P4).

**N-D-81 Pseudo-roots.** A required capability that no body contributes (an import of a binary component, an interface used by the types of the export world, `memory64` of the target) has a pseudo-root located in the root's `sai.package`: a one-step `Declares` chain in `sai inspect capabilities` and a `declared_here` entry instead of a `capability_chain` in a diagnostic (§8.6.5). *Rationale:* every required capability is explained by a location the user can act on, even when no effect is involved (P1).

**N-D-82 Run glue and the result drop of `main`.** The `run` export of a command is represented by its callees, `main` and `Env.for_entry`, and by the drop glue of the result type of `main`, which is an edge of the `Main` root without a location (§8.7.2 item 1, §8.7.4). *Rationale:* the reachable set is the one a `Glue` root for `run` would produce, without a second root kind (P7); the drop is generated code, so no source location is invented for it (P1).

**N-D-83 The root is checked only against its effective policy.** Each required capability is checked once against the root's effective policy, and the per-package check of the root against its own policy is not run separately (§8.7.6 item 2). *Rationale:* every violation of the root's own policy is a violation of the effective policy for the same capability with the same code, so a second check could only duplicate diagnostics (P8).

**N-D-84 Profile-case fix target.** When a capability is denied only by the default denial of a profile, the fix edits that profile's `allow`: it extends the profile's `allow`, inserts a line `allow { r }` as the first line of the `profile` block, or appends a block `profile p { allow { r } }`; when `r` is granted only by the `wasi:io` implication, the inserted name is the first package-level `allow` entry that implies `r` instead of `r` (§8.7.6, targets §5.6 item 3). *Rationale:* lifting the profile's denial is the smallest change, and because the inserted name is covered by the package-level `allow`, the fix cannot cause `PROFILE_ALLOW_EXCEEDS_PACKAGE` (P9, P5).

**N-D-85 The footprint is an upper bound in dependency position.** `capability_footprint(P, t)` is the set of capabilities of the union of the own sets of every effect body of `P` for `t`, excluding tests, glue, `memory64` and `TypeImp`, normalized and taken over the targets the package declares; a package whose analysis has an error has no footprint (§8.7.10). *Rationale:* the footprint must not depend on which dependent uses the package or on its profile (P8), and it never reports less authority than the package's code can exercise (P5).

**N-D-86 Ownership of the unsafe check.** This chapter owns the whole check of spec/12: the unsafe diagnostics, the inventory and `UNSAFE_DENIED`. It computes unsafe contexts from the HIR, not from the `Γ.unsafe` flag of impl/03, reads the CST for doc comments, tokens and fixes, and runs for every owner, including owners whose type checking failed. In a tainted owner, only operations whose relevant types and resolutions are known are recognized, and the result is marked incomplete (§8.10, §8.10.3 item 6). *Rationale:* one owner gives one answer for the diagnostics and the inventory (P9); the inventory of D-UNSAFE-01 needs the CST and every listed use, including those in code that failed to type check; skipping unknown operations avoids cascades from type errors (P8).

**N-D-87 The impl-method cap check follows `compare_impl_fn`.** The check that each cap of an impl method's own bound is implied by the trait method's corresponding bound runs after the positional renaming of impl/03 `compare_impl_fn`, and only if impl/03 reported no other signature difference for the item; its failures are reported as `TRAIT_ITEM_SIGNATURE_MISMATCH` (§8.1.7). *Rationale:* the bounds correspond only after the renaming, and a signature that already differs would produce a second, derived error (P8).

**N-D-88 Test suppression covers policy, grant and target checks.** In a `WithTests` check, a violation is not reported when every root that can reach it is a test or property for which `TEST_EFFECT_NOT_PERMITTED` with the same capability is reported. The rule applies to the policy, grant and target checks alike, and a source without a body prevents it (§8.7.6, §8.7.7, §8.7.8). *Rationale:* the test error already reports the same cause, so one cause gives one error (P8).

**N-D-89 `TestHarness` checks are implied.** The `TestHarness` build key computes reachability for the selected tests and properties, which decides what the harness imports and links, but runs no capability check (§8.7.1). *Rationale:* every root of the harness is a root of the preceding `WithTests` check, `Req` is monotone in the set of roots, and the harness is built only when that check reported no error, so a second check could never fail (P7, P9).

**N-D-90 Tests are solved separately.** The effect bodies of tests and properties and the closures and `async` blocks inside them are solved by `test_effect_solution(P)` on top of `effect_solution(P)`; `Sig`, the metadata of §8.3.8 and the footprint do not depend on tests (§8.7.12, §8.1.5). *Rationale:* what a package exports to its dependents must be the same whether or not its tests are analyzed (P8), and no non-test body refers to a test body, so the separate solution is exact (P9).

**N-D-91 The body of an `async fn`.** The effect body `Fn(f)` of an `async fn` comprises the operations of the owner's own body followed by those of the `async` block synthesized for it; the synthesized block is not a separate effect body, and the creation of its future is not an operation (§8.1.5, §8.3.6 item 4). *Rationale:* spec/08 §8.10 defines the effects of an `async fn` as those of its code, and one body gives one operation order for first contributing operations and chains (P8, P9).

**N-D-92 The wrap target of `UNSAFE_OPERATION_OUTSIDE_UNSAFE`.** The fix replaces the innermost enclosing expression statement, assignment or assertion statement, `let`, `let else` or `var` initializer, or block tail expression with `unsafe { <text> }`, and is always `maybe-incorrect` (§8.10.5 item 2). *Rationale:* this is the smallest enclosing statement of spec/12 §12.2.5 whose wrapped text is again valid, keeping `let` bindings in scope and tail values in place (P9); the author must establish the preconditions, so the fix is never applied automatically (P1, D-TOOL-01).

**N-D-93 When an `unsafe module` header is used.** The header is used if some operation of rows 1–4 of §8.10.3 is in a function, closure or `async` body of the file outside predicates. `UNUSED_UNSAFE` for the header is reported only if no owner of the file is incomplete, the file declares no test or property, and no module-level item of the file is removed by `@target` (§8.10.6). *Rationale:* `unsafe impl` (row 5) needs no context and predicates are not covered by the header, so neither uses it; a warning that suggests deleting the header must be certain for every code path that the header can cover (P8, P9).

**N-D-94 Syntactic paths for removed items.** An inventory entry inside an item removed by `@target` for the command's target has a syntactic `item` path (`<module>.<name>`, `<module>.impl[<text>]`, `<parent>.<name>`, `<module>.test["name"]`), `is_symbol` false and no operations (§8.10.4 item 7). *Rationale:* spec/12 §12.11.1 item 4 lists declarations whatever their predicates, but removed items are never resolved; a path built from the source text alone is deterministic and still lets the user find the entry (P8, P1).

**N-D-95 Fixes that depend on name resolution.** A removal fix whose correctness depends on name resolution is `machine-applicable` if the package's `targets` list has exactly one element and `maybe-incorrect` otherwise. This applies to `UNUSED_EFFECT_DECLARATION`, to `UNUSED_UNSAFE` for an unused block or module header, and to `IMPL_UNSAFETY_MISMATCH` for an `unsafe impl` of a trait that is not unsafe (§8.5.8, §8.10.6, §8.10.7). *Rationale:* the check sees only the target of the command, and the same source can resolve differently for another declared target, so the fix is applied automatically only when no other target exists (P9).

**N-D-96 `sai inspect effects`.** The listing contains every function with a `Sig` of the root and the workspace members, omits incomplete functions, tests, properties and non-symbols, and prints each set in print order in the function's module. A `<path>` may name a function of any package of the graph; a path without a `Sig` gets `NOT_PRESENT_IN_BUILD` with a note that says why. `--why` matches by bytes against the spelling and the display form, of which at most one atom can match, and the chain is the why-chain of the body, a `Declares` step at the clause, or the terminal steps of an extern function (§8.9.1). *Rationale:* every `Sig` that a user can name is explained by a chain (P1); the matching rule is unambiguous (P8); dependency functions need no source because their `Sig` and why-entries are in metadata (P7).

**N-D-97 Unsafe entries in test code.** The inventory is computed without test analysis. Entries inside a test or property are found by the CST scan, with the test path as `item` and no operations; operations in test code are checked and reported by `sai test` (§8.10.4 item 8, §8.9.2). *Rationale:* spec/12 §12.11.1 requires every use of unsafe in the inventory, and spec/14 §14.2.4 analyzes tests only under `sai test`; listing the entry without operations satisfies both (D-UNSAFE-01), and the inventory is the same for every command (P8).

**N-D-98 An `unsafe` block in a predicate.** An `unsafe` block inside a predicate creates no unsafe context; the operations inside it are not reported, and the block gets no `UNUSED_UNSAFE` (§8.10.2 rule 4). *Rationale:* the block is already `INVALID_CONTRACT_EXPRESSION` (spec/09 §9.1.2), and further diagnostics inside it would only cascade from that error (P8).

**N-D-99 Recognizing a `# Safety` section.** The doc comment is read line by line; lines between two lines that start with three backticks are fenced code and are skipped. The section is the first other line that is exactly `# Safety`, without trailing whitespace, up to the next level-1 heading or the end of the comment. A heading with an empty section counts as present (§8.10.7 item 4, §8.10.4 item 5). *Rationale:* a deterministic line-based rule gives the same result in every tool, and skipping fenced code avoids false matches in examples (P8); the lint checks that the obligations have a place, not their wording (P9); the recognized section text is stable input for the inventory (P8).

**N-D-110 Policy names are checked per package, at resolution.** `policy_names(P)` depends only on `P`'s contract and the WIT packages visible to the graph, never on a `BuildKey`. The resolver evaluates it for every node whose contents it has before it writes the lock, and an error stops the resolution. `CAPABILITY_ALLOW_DENY_CONFLICT` compares only the package-level `allow` and `deny`, token for token (§8.7.13). *Rationale:* a name in a dependency's contract is known or unknown regardless of the target and profile, so every build sees the same result (P8). Checking before the lock is written means that no build-time capability check ever reads an unknown name (P9). Overlapping names that differ are a policy choice, not an error (P5).

---

## 8.14 Diagnostics

This chapter introduces no diagnostic code. It reports the codes of [spec/08](../spec/08-effects-and-capabilities.md) that arise after parsing, the static unsafe codes of [spec/12](../spec/12-unsafe-and-ffi.md), and the capability codes of the package and test documents, all defined elsewhere. Severities are those of the owner; "(lint)" marks a code whose level can be changed ([spec/15](../spec/15-attributes-and-lints.md) §15.17.2).

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_EFFECT` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.2.2; an `EffectRef` resolves to no effect, interface binding or world (§8.1.7 items 1 and 2) |
| `EFFECT_NOT_VISIBLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.2.2, §8.4.2; an `EffectRef` names an `effect` item that is not visible there (§8.1.7 item 2) |
| `DUPLICATE_EFFECT` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.2.3; a later `EffectRef` of one set has the atom of an earlier one; machine-applicable removal (§8.1.7 item 3) |
| `EFFECT_NAME_RESERVED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.4.1; `effect_def` finds a user effect named after a built-in root (§8.1.7) |
| `EFFECT_COVERS_CYCLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.4.1; a declaration in a non-trivial SCC, or with a self-edge, of `covers_graph(P)` (§8.1.7, §8.4.1) |
| `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.4.2; `effect_decl_check` finds a non-`pub` user effect in a public clause, cap or function pointer type, once per `EffectRef` (§8.1.7) |
| `EFFECTS_ON_INTERFACE_FN` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.1; an `effects` clause on an interface function; machine-applicable removal (§8.1.7) |
| `EXTERN_EFFECTS_MISMATCH` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.1; an extern function whose clause is not exactly `{ host }` or `{ ffi }`; machine-applicable `set_effects` (§8.1.7) |
| `EFFECT_NOT_DECLARED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.1; a body's own set is not permitted by its clause, one diagnostic per first contributing operation (§8.5.1, §8.5.2, N-D-70) |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.3; an impl method's effects are not permitted by the trait method's clause (§8.5.3) |
| `DROP_HAS_EFFECTS` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.3; a `Drop.drop` impl with a non-empty clause or own set, or with an operation that calls through a type parameter (§8.5.3) |
| `BOUND_EFFECTS_EXCEEDED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.8.3; a checked reference of an instantiating operation exceeds its first failing cap (§8.5.4, N-D-77, N-D-78) |
| `FN_POINTER_EFFECTS_MISMATCH` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.4; a function value is coerced to a function pointer type whose clause does not permit its effects (§8.5.5) |
| `EFFECTFUL_PREDICATE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.11; a predicate site has a non-empty set (§8.5.6, N-D-72) |
| `MISSING_EFFECTS_DECLARATION` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; a function that must declare its effects has no clause and a non-empty inferred set (§8.5.7) |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; a `pub` function of a pre-1.0 package has no clause and a non-empty inferred set (§8.5.7) |
| `UNUSED_EFFECT_DECLARATION` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.2; a declared atom of a private function permits no atom of its own set; removal fix per N-D-95 (§8.5.8) |
| `AMBIENT_IMPORT` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.5.3; `ambient_imports(P)`: a `use` of a `wasi:` interface outside the `sai` namespace; error under `policy { deny ambient_imports }` (§8.1.7) |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; a required capability not allowed by a package's own policy or by the root's effective policy; `maybe-incorrect` fix for the root and workspace members (§8.7.6, N-D-79, N-D-84) |
| `CAPABILITY_DENIED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; a required capability that overlaps a `deny` entry of the checked policy; no fix (§8.7.6) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6, §8.12.7; a required capability that the target does not provide, including `host` in a component build and every component import on a core target other than an interface provided within the build (§8.7.8, §8.7.9 item 5) |
| `CAPABILITY_NOT_GRANTED` | error | defined in [package/01](../package/01-package-contract.md) §1.10.8; a capability of a dependency's closure neither covered nor implied by the requirement's `grant`; `maybe-incorrect` fix (§8.7.7, N-D-79) |
| `UNKNOWN_CAPABILITY` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.1; `policy_names(P)` finds an `allow`, `deny`, profile `allow` or `grant` name that denotes no visible WIT package or interface, at the entry, evaluated by the resolver (§8.7.13). |
| `CAPABILITY_ALLOW_DENY_CONFLICT` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.5; `policy_names(P)` finds a package-level `deny` entry equal to an `allow` name, at the `deny` entry, with a label at the `allow` entry; no fix (§8.7.13). |
| `WORLD_IMPORT_UNDECLARED` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; an import of the component, from `Imp` or from a composed artifact, that the export world does not import, once per interface; `maybe-incorrect` fix for a local world (§8.7.9 items 3 and 6) |
| `PROVIDED_INTERFACE_AT_BOUNDARY` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; an interface provided by a dependency, within the build or by composition, that the root's component would export, that a remaining import uses, or that an artifact imports while it is provided within the build, once per interface and case; no fix (§8.7.9 item 7) |
| `TEST_EFFECT_NOT_PERMITTED` | error | defined in [spec/14](../spec/14-testing.md) §14.3.1; a test or property whose entry set requires a capability other than `alloc`; it suppresses the build-level violations of the same capability (§8.7.12, N-D-88) |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | defined in [spec/05](../spec/05-types.md) §5.7.6; an operation whose own latent expansion reaches a type nested deeper than 64 (§8.3.5, N-D-75) |
| `TRAIT_SOLVER_OVERFLOW` | error | defined in [spec/05](../spec/05-types.md) §5.11.2; an operation whose own latent expansion overflows the trait solver (§8.3.5, N-D-75) |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; a cap of an impl method's own bound that the trait method's corresponding bound does not imply; `maybe-incorrect` removal (§8.1.7, N-D-87) |
| `UNSAFE_OPERATION_OUTSIDE_UNSAFE` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.5, §12.3; an operation of rows 1–4 of §8.10.3 in a safe context; `maybe-incorrect` wrap fix (§8.10.5, N-D-92) |
| `UNUSED_UNSAFE` | warning (lint) | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.1, §12.2.4, §12.7.3, §12.9.1; an unused or nested `unsafe` block, an unused `unsafe module` header, an unused `unsafe` marker of an `extern "wasm"` block, or `unsafe fn` on an `extern "C"` item (§8.10.6, §8.10.7, N-D-93, N-D-95) |
| `IMPL_UNSAFETY_MISMATCH` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.3; a safe impl of an unsafe trait, or `unsafe impl` of a safe trait, an inherent impl or an interface impl (§8.10.7, N-D-95) |
| `MISSING_SAFETY_DOC` | warning (lint) | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.2, §12.2.3; a `pub` unsafe function, method, trait method or trait without a `# Safety` section (§8.10.7, N-D-99) |
| `EXTERN_ENSURES_REQUIRES_UNSAFE` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.3; an `extern "wasm"` function with an `ensures` clause in a block without `unsafe` (§8.10.7) |
| `UNSAFE_EXPORT` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.2, §12.8; an `export unsafe fn` (§8.10.7) |
| `UNSAFE_DENIED` | error | defined in [package/01](../package/01-package-contract.md) §1.12; every inventory entry other than `unsafe_trait` in the own source of a package with `policy { deny unsafe }` (§8.10.6, §8.10.8) |
| `NOT_PRESENT_IN_BUILD` | info | defined in [impl/13](13-cli.md) §13.6.15; `sai inspect effects` with a `<path>` that has no `Sig`, or a `--why` text that matches no atom (§8.9.1, N-D-96) |
| `UNKNOWN_SYMBOL_PATH` | error | defined in [impl/12](12-semantic-tooling.md) §12.2.5; `sai inspect effects` with a `<path>` that names nothing (§8.9.1) |
| `MISSING_ARGUMENT` | error | defined in [impl/13](13-cli.md) §13.3.5; `sai inspect effects --why` without `<path>` (§8.9.1) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6; a selection that is `Ambiguous` or `Failed` after type checking succeeded, or another state that a type-correct program cannot produce (§8.3.2 note 5, §8.3.4, §8.5.1, §8.6.2, N-D-73). |
| `ALLOCATOR_REENTRANCY` | error | defined in [impl/10](10-runtime.md) §10.2.7; referenced: reported by impl/10 on the set computed by `reach_from` (§8.7.1) |
| `COMPONENT_TARGET_MISMATCH` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2; referenced: reported before capability analysis of a component build (§8.7.1) |
| `PROFILE_ALLOW_EXCEEDS_PACKAGE` | error | defined in [package/01](../package/01-package-contract.md) §1.14.2; referenced: the profile-case fix of `CAPABILITY_NOT_ALLOWED` cannot cause it (§8.7.6, N-D-84) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; referenced: [impl/03](03-name-resolution-and-type-checking.md) §3.14.4 rejects every `unsafe extern "C"` block before Stage 7, so the unsafe check never sees one (§8.10) |
| `INVALID_CONTRACT_EXPRESSION` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.1.2; referenced: an `unsafe` block in a predicate, reported by its owner; this chapter adds nothing (§8.3.7, §8.10.2, N-D-98) |
| `NOT_CONST_EXPRESSION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.27; referenced: every unsafe operation and block in a const initializer, reported by its owner (§8.10.1) |
| `UNKNOWN_ABI` | error | defined in [spec/03](../spec/03-grammar.md) §3.10; referenced: an `unsafe` extern block with an unknown ABI string, which gets no inventory entry (§8.10.4 item 1) |
