# Roadmap

**Status:** Informative
**Stage:** all
**Related:** [README.md](README.md) §4, [spec/01-introduction.md](spec/01-introduction.md) §1.4, [impl/01-architecture.md](impl/01-architecture.md) §1.12, §1.13, [impl/13-cli.md](impl/13-cli.md) §13.1.2, [impl/15-conformance-and-testing.md](impl/15-conformance-and-testing.md) §15.9, §15.12.6, [design/decisions.md](design/decisions.md), [design/draft-reconciliation.md](design/draft-reconciliation.md) §5

This document describes the implementation stages of SAI as milestones, defines what v0.1 consists of, summarizes how the reference toolchain is released, and lists what is deliberately left for versions after v0.1. It decides nothing. Every statement here restates a rule of a normative document and links to it; where this summary and the linked document differ, the linked document is right (README §2).

---

## 1. Stages as milestones

### 1.1 How stages work

1. Every feature of the language, the library, the package system and the tools is tagged with one of the eight stages of draft §84 (README §4). A chapter header gives the default stage of its contents, and a section or an item can override it ([spec/01](spec/01-introduction.md) §1.4.5 item 1; for the library, [spec/16](spec/16-core-library.md) §16.1.6).
2. Stages are cumulative. A compiler that implements Stage *k* implements every stage below it, so the stage range of every conformance claim starts at 0 ([impl/15](impl/15-conformance-and-testing.md) §15.9.3, N-J-47). One consequence: an implementation that offers the semantic protocol of Stage 6 also implements the async features of Stage 5.
3. A compiler rejects every use of a feature of a stage it does not implement with `FEATURE_NOT_IMPLEMENTED`, naming the stage and the feature ([spec/01](spec/01-introduction.md) §1.4.5 items 2 and 5, N-J-5). It never accepts a staged feature partially (§1.4.5 item 3). For commands, [impl/13](impl/13-cli.md) §13.1.2 gives the stage of each one; for library items, the staged item table of [impl/01](impl/01-architecture.md) §1.12.1 item 4 lists the items that a toolchain release does not yet provide.
4. The stages of this document are unrelated to the two builds of the toolchain bootstrap, which are called the bootstrap compiler and the release compiler for that reason ([impl/01](impl/01-architecture.md) §1.12.2).

### 1.2 Exit criteria

A stage *k* is complete for an implementation when all of the following hold. The same criteria apply to every stage, so the sections below list only what each stage adds.

1. **Features.** Every feature of the stages 0 to *k* is implemented, in every chapter, at the stage that its tags give.
2. **Suite.** The conformance suite is complete for the stage: it contains the required tests of [impl/15](impl/15-conformance-and-testing.md) §15.9.1 for the stages 0 to *k* and satisfies the coverage rules of §15.9.2 (sections, codes, panic kinds and limits).
3. **Claim.** The claim `SAI 0.1, stages 0-k, targets <list>` ([spec/01](spec/01-introduction.md) §1.4.3) is substantiated by the harness as [impl/15](impl/15-conformance-and-testing.md) §15.9.4 describes: every applicable test passes, every test of a higher stage is reported as `staged` or passes, and no test is left unrun.
4. **Commands.** The commands of the stages 0 to *k* in [impl/13](impl/13-cli.md) §13.1.2 are available, and the commands of later stages report `FEATURE_NOT_IMPLEMENTED`.
5. **Reference implementation.** For the reference toolchain, the merge gate of [impl/15](impl/15-conformance-and-testing.md) §15.12.6 passes with the claim, and the coverage report has no uncovered section, code or panic kind for the implemented stages (§15.9.2 item 6).

### 1.3 Stage 0: frontend, diagnostics and formatter

*README §4: lexer, parser, AST, basic type checker, formatter.*

- **Language.** The lexical structure ([spec/02](spec/02-lexical-structure.md)) and the grammar ([spec/03](spec/03-grammar.md)); the module tree, `use` within the current package and of `sai:core` and `sai:alloc`, visibility and name resolution ([spec/04](spec/04-modules-and-names.md)); type syntax, declarations and the type checking of non-generic programs ([spec/05](spec/05-types.md), [spec/06](spec/06-expressions-and-statements.md)); attribute syntax and validation, the attributes and lints of Stage 0, and level resolution ([spec/15](spec/15-attributes-and-lints.md)).
- **Tools.** The diagnostic model, the codes, and the human and JSON output ([impl/11](impl/11-diagnostics.md)); `sai explain` (D-TOOL-03); the canonical formatter ([impl/14](impl/14-formatter.md), D-TOOL-04); node IDs, symbol IDs, the symbol index and the base language server ([impl/12](impl/12-semantic-tooling.md)).
- **Compiler.** The pipeline skeleton, the crates, the query system, the virtual file system, the collection of diagnostics, internal compiler errors, versioning, and the embedded source of `sai:core` and `sai:alloc` ([impl/01](impl/01-architecture.md)); the lexer, parser, lossless CST and AST ([impl/02](impl/02-frontend-ast-hir.md)); name resolution and type checking ([impl/03](impl/03-name-resolution-and-type-checking.md)).
- **Commands.** `sai new`, `sai check`, `sai fmt`, `sai explain`, and `sai lsp` without its extensions.
- **Tests.** The Stage 0 row of [impl/15](impl/15-conformance-and-testing.md) §15.9.1; the suite format and the harness (§15.2, §15.3); diagnostics and formatter goldens (§15.4); the grammar, example and `sai explain` checks (§15.8); frontend fuzzing for the reference implementation (§15.7).
- **Builds on.** Nothing.

### 1.4 Stage 1: code generation for `wasm32-core`

*README §4: HIR, MIR, wasm32-core backend, integers, booleans, functions, structs, enums, `Option` / `Result`.*

- **Language.** Code generation for primitive, record, enum, `Option` and `Result` types ([spec/05](spec/05-types.md)); recoverable errors, the `?` operator, panics, traps, panic reporting and `@panic_hook` ([spec/10](spec/10-errors-and-panics.md)); `extern "wasm"`, `@import_name` and `@export_name` ([spec/12](spec/12-unsafe-and-ffi.md), [spec/15](spec/15-attributes-and-lints.md)); the code-generation effects of `@inline`, `@cold` and `@repr`; the library items that [spec/16](spec/16-core-library.md) §16.1.6 marks as Stage 1.
- **Compiler.** HIR and HIR lowering ([impl/02](impl/02-frontend-ast-hir.md)); the MIR, its construction for the Stage 1 constructs, the match decision tree, validation and the text format ([impl/04](impl/04-mir.md), D-MIR-01); the layout and lowering of scalars, functions, structs and enums for `wasm32-core` ([impl/06](impl/06-wasm-lowering-and-layout.md), D-WASM-01 to D-WASM-04); the `dev` optimization pipeline ([impl/09](impl/09-optimization-and-size.md)); panic reporting, stack overflow reporting and initialization ([impl/10](impl/10-runtime.md)); the precompiled library metadata and the bootstrap with its fixpoint ([impl/01](impl/01-architecture.md) §1.12.2, §1.12.3).
- **Commands.** `sai build`; `sai inspect mir`, on which the MIR goldens rely ([impl/15](impl/15-conformance-and-testing.md) §15.4.2).
- **Tests.** The Stage 1 row of impl/15 §15.9.1: execution on `wasm32-core` in the deterministic engine (§15.5), every Stage 1 panic kind on both reporting channels, traps, and the library items of Stage 1.
- **Builds on.** Stage 0: code is generated only for programs that the frontend accepts (the error gates of [impl/01](impl/01-architecture.md) §1.2.3, N-J-27).

### 1.5 Stage 2: ownership, generics, traits and `tiny`

*README §4: ownership, borrow checker, generics, traits, dead-code elimination, tiny profile.*

- **Language.** Moves, copies, borrows, regions, drop and arenas ([spec/07](spec/07-ownership-and-borrowing.md), except spec/07 §7.14 and §7.16); generics, traits, impls, coherence, closures, `@derive`, `Copy`, `Clone` and `Drop` ([spec/05](spec/05-types.md), [spec/15](spec/15-attributes-and-lints.md)); unsafe code and raw pointers ([spec/12](spec/12-unsafe-and-ffi.md)); `sai:core` and `sai:alloc` at their default stage ([spec/16](spec/16-core-library.md)); the `tiny` profile (D-TGT-02).
- **Compiler.** Drop scheduling and elaboration, borrow-checker facts, closures, regions, generic monomorphization and drop glue ([impl/04](impl/04-mir.md)); the borrow checker ([impl/05](impl/05-borrow-checker.md)); the lowering of ownership, generics, closures and drop glue, and `wasm64-core` ([impl/06](impl/06-wasm-lowering-and-layout.md)); the `release` and `tiny` pipelines, the Wasm passes, identical code folding, final dead-code elimination and size attribution ([impl/09](impl/09-optimization-and-size.md)); the allocators and arenas ([impl/10](impl/10-runtime.md), D-WASM-05); parallel compilation ([impl/01](impl/01-architecture.md) §1.8).
- **Commands.** `sai size`.
- **Tests.** The Stage 2 row of impl/15 §15.9.1, including a `run-panic` test for each panic condition of a library function and a `run-pass` test of each library function; the equivalence tests of the profiles and of `wasm64-core` (§15.6).
- **Builds on.** Stage 1: the borrow checker and drop elaboration work on the MIR, and generic code is monomorphized into the MIR of Stage 1.

### 1.6 Stage 3: effects, contracts, packages and tests

*README §4: effects, capabilities, package system, `sai.package`, `sai.lock`, Git resolution, tests.*

- **Language.** Effects, latent effects, effect inference and the capability policy ([spec/08](spec/08-effects-and-capabilities.md), D-EFF-01 to D-EFF-03, D-CAP-01, D-CAP-02); effects in function types ([spec/05](spec/05-types.md)); refined types, `requires` and `ensures`, invariants and the Standard Fact Checker ([spec/09](spec/09-contracts-and-refinements.md), D-CONTRACT-01 to D-CONTRACT-04), with their panic kinds ([spec/10](spec/10-errors-and-panics.md)); `test` and `property` declarations, `@cases`, `@seed` and test-only dependencies ([spec/14](spec/14-testing.md)); f-strings (D-EXPR-12) and the format specs of [spec/16](spec/16-core-library.md) §16.14; dependencies on other packages and user-defined effects ([spec/04](spec/04-modules-and-names.md)); the `lints { }` block and `policy` as sources of lint levels ([spec/15](spec/15-attributes-and-lints.md)).
- **Package system.** The `sai.package` language ([package/01](package/01-package-contract.md), D-PKG-01), Minimal Version Selection ([package/02](package/02-resolver.md), D-PKG-02), the lock file and its `sai1:` checksums ([package/03](package/03-lock-file.md), D-PKG-03), and Git fetching, the global cache, review and approval, and offline operation ([package/04](package/04-distribution-and-security.md)).
- **Compiler.** Contract, invariant and refinement checks, the mapping to the Standard Fact Checker and f-strings in the MIR ([impl/04](impl/04-mir.md)); effect inference and capability checking ([impl/08](impl/08-effect-and-capability-analysis.md)); size budgets ([impl/09](impl/09-optimization-and-size.md)); multi-package sessions and dependency metadata ([impl/01](impl/01-architecture.md) §1.6.5); the why-chains of effect and capability diagnostics ([impl/11](impl/11-diagnostics.md)); the formatting of `sai.package` ([impl/14](impl/14-formatter.md)).
- **Commands.** `sai test`, `sai add`, `sai remove`, `sai update`, `sai lock` and `sai fetch`.
- **Tests.** The Stage 3 row of [impl/15](impl/15-conformance-and-testing.md) §15.9.1, including the limits of the Standard Fact Checker at and beyond each bound, and the Git fixtures of §15.2.7; for the reference implementation, the independent oracles of the Standard Fact Checker and of the resolver (§15.10.3, §15.10.4).
- **Builds on.** Stage 2: effects reach generic code through its type parameters and trait bounds (latent effects, D-EFF-02), and the capability check covers the code that remains reachable after dead-code elimination (D-CAP-01).

### 1.7 Stage 4: components and WASI

*README §4: WIT generation and import, Component Model, resources, worlds, WASI.*

- **Language.** Interfaces and worlds ([spec/04](spec/04-modules-and-names.md), [spec/13](spec/13-components-and-wit.md), D-WIT-01 to D-WIT-03); importing WIT, the name conversion and the type mapping (spec/13 §13.7 to §13.9); resources and handles ([spec/05](spec/05-types.md) §5.6.6, [spec/07](spec/07-ownership-and-borrowing.md) §7.14, spec/13 §13.10, D-OWN-06); generated WIT (spec/13 §13.11); the interface effects and world rules of [spec/08](spec/08-effects-and-capabilities.md), and the effects, capabilities and failures at the boundary (spec/13 §13.12, §13.13); `@flags` ([spec/15](spec/15-attributes-and-lints.md)).
- **Package system.** The `artifact`, `wit` and `export world` declarations ([package/01](package/01-package-contract.md)), and artifact and hybrid packages ([package/04](package/04-distribution-and-security.md) §4.5, D-PKG-05).
- **Compiler.** WIT loading, SAI views, the checks of interfaces, interface impls and worlds, flags types and generated WIT ([impl/16](impl/16-interfaces-and-wit.md)); the trust-boundary validation shims ([impl/04](impl/04-mir.md)); the core modules for `wasm32-wasi` and `wasm64-wasi` ([impl/06](impl/06-wasm-lowering-and-layout.md)); Canonical ABI lifting and lowering and component assembly ([impl/07](impl/07-component-lowering.md)); interface effects, world imports and boundary glue ([impl/08](impl/08-effect-and-capability-analysis.md)); `cabi_realloc` ([impl/10](impl/10-runtime.md)).
- **Commands.** `sai component build`, `sai run` and `sai bindgen wit`.
- **Tests.** The Stage 4 row of impl/15 §15.9.1: every row of the type mapping with its `.wit/` sidecar, resources, command components under the deterministic WASI host, reactors called with WAVE values, and artifact and hybrid packages; the WIT and component goldens (§15.4.7) and the WIT suite of [impl/16](impl/16-interfaces-and-wit.md) §16.11.
- **Builds on.** Stage 3: WIT packages and interfaces are capability names of the policy (D-CAP-01), functions exposed through interfaces and worlds write their effects explicitly (D-EFF-01, C-04), and artifact packages are a form of the packages of Stage 3.

Completing Stage 4, together with structured diagnostics, is the v0.1 milestone (§2).

### 1.8 Stage 5: async

*README §4: async, `Future`, `Stream`, WASI async mapping.*

- **Language.** `async fn`, `async` blocks, `.await`, `Future`, `Poll`, `Context`, implicit pinning, `Stream` and `for await`, structured concurrency, the effects of async code and executors ([spec/11](spec/11-async-and-concurrency.md), D-ASYNC-01; the draft's `Pin` is replaced by implicit pinning, C-02); borrows held across `.await` ([spec/07](spec/07-ownership-and-borrowing.md) §7.16); `e.await?` ([spec/10](spec/10-errors-and-panics.md)); async function types ([spec/05](spec/05-types.md)); `future`, `stream` and async exports ([spec/13](spec/13-components-and-wit.md), spec/11 §11.10); async code in tests ([spec/14](spec/14-testing.md) §14.6).
- **Compiler.** `.await`, the async transform and `ASYNC_RECURSION` ([impl/04](impl/04-mir.md)); borrows across `.await` ([impl/05](impl/05-borrow-checker.md)); the async ABI of the Component Model ([impl/07](impl/07-component-lowering.md)); the attribution of the effects of async code ([impl/08](impl/08-effect-and-capability-analysis.md)); runtime support for async code ([impl/10](impl/10-runtime.md), marked there as provisional).
- **Feature names.** `async-fn`, `async-block`, `await`, `for-await` and `wasi-async` ([spec/01](spec/01-introduction.md) §1.4.5 item 5).
- **Tests.** The Stage 5 row of [impl/15](impl/15-conformance-and-testing.md) §15.9.1. No test depends on an interleaving of tasks that [spec/11](spec/11-async-and-concurrency.md) leaves unspecified.
- **Builds on.** Stage 4: the WASI async mapping lowers to the async ABI of the Component Model. Exporting an `async` function requires that ABI (R-04).

### 1.9 Stage 6: semantic tooling

*README §4: semantic protocol, structured edits, impact analysis, semantic diff.*

- **Tools.** The semantic graphs, the semantic protocol, semantic operations, impact analysis and semantic diff ([impl/12](impl/12-semantic-tooling.md), D-TOOL-02; draft §62 to §67); the semantic-diff output of effect and capability analysis ([impl/08](impl/08-effect-and-capability-analysis.md)). The draft's "AI Agent integration" item of Stage 6 is this protocol and these operations: agents use the same interface as every other client (draft §63, impl/12 §12.6).
- **Commands.** `sai graph`, `sai diff`, `sai edit` and the extensions of `sai lsp`.
- **Feature name.** `semantic-protocol` (spec/01 §1.4.5 item 5), which [impl/13](impl/13-cli.md) §13.1.2 also uses for these commands.
- **Tests.** A `protocol` transcript for each method and each error of the protocol ([impl/15](impl/15-conformance-and-testing.md) §15.9.1, §15.4.8).
- **Builds on.** The node and symbol IDs, the symbol index and the base language server of Stage 0. The structured fixes of diagnostics, which also refer to node IDs, exist from Stage 0 ([impl/12](impl/12-semantic-tooling.md) §12.6).

### 1.10 Stage 7: native code and C interop

*README §4: native backend, C FFI.*

- **Language.** `unsafe extern "C"` blocks and calls to their functions ([spec/12](spec/12-unsafe-and-ffi.md)); the native types of `core.ffi` ([spec/16](spec/16-core-library.md) §16.12.4); the target capabilities `native_ffi` and `process`, which only native targets provide ([spec/08](spec/08-effects-and-capabilities.md) §8.12.7).
- **Targets.** The native targets of [targets/targets-and-profiles.md](targets/targets-and-profiles.md) (draft §57).
- **Feature names.** `native-target` and `extern-c` (spec/01 §1.4.5 item 5).
- **Tests.** The Stage 7 row of impl/15 §15.9.1: native executables run as child processes, bounded by the wall clock instead of fuel.
- **Builds on.** All earlier stages. The implementation documents of v0.1 describe the Wasm backends only; a native backend is added to them when the reference implementation takes up Stage 7.

---

## 2. v0.1

### 2.1 The focus of the draft

Draft §85 names the focus of the first usable version: SAI source compiled through a typed MIR to `wasm32-core`; `sai.package` and Git-based dependency distribution; WIT and Component Model interoperability; extremely small binaries; and structured compiler diagnostics. Native code, eBPF, GPUs and other targets remain possible by design but do not complicate the first compiler. Each item of the focus is specified as follows.

| Draft §85 item | Specified in |
|---|---|
| SAI source to typed MIR | [spec/02](spec/02-lexical-structure.md) to [spec/10](spec/10-errors-and-panics.md); [impl/02](impl/02-frontend-ast-hir.md) to [impl/05](impl/05-borrow-checker.md), [impl/08](impl/08-effect-and-capability-analysis.md) |
| `wasm32-core` | [impl/06](impl/06-wasm-lowering-and-layout.md), [impl/10](impl/10-runtime.md), D-TGT-01, D-WASM-01 to D-WASM-05 |
| `sai.package` | [package/01](package/01-package-contract.md) |
| Git-based dependency distribution | [package/02](package/02-resolver.md) to [package/04](package/04-distribution-and-security.md) |
| WIT and Component interoperability | [spec/13](spec/13-components-and-wit.md), [impl/07](impl/07-component-lowering.md), `wasm32-wasi` (D-TGT-01) |
| extremely small binaries | the `tiny` profile (D-TGT-02), dead-code elimination and size attribution ([impl/09](impl/09-optimization-and-size.md)), size budgets ([package/01](package/01-package-contract.md) §1.14) |
| structured compiler diagnostics | [impl/11](impl/11-diagnostics.md), [spec/01](spec/01-introduction.md) §1.4.4 |

### 2.2 v0.1 conformance

A **v0.1 conformance** claim covers Stages 0 to 4 and structured diagnostics ([spec/01](spec/01-introduction.md) §1.4.3 item 1, README §4). A compiler that makes it satisfies every requirement of [spec/01](spec/01-introduction.md) §1.4.2:

1. it implements every feature of Stages 0 to 4, and reports `FEATURE_NOT_IMPLEMENTED` for every feature of Stages 5 to 7 that it does not implement;
2. it supports `wasm32-core` and `wasm32-wasi`, each in the `dev`, `release` and `tiny` profiles (N-J-1); `wasm64-core` and `wasm64-wasi` are optional, and a compiler that offers one conforms on it;
3. it provides `sai:core` and `sai:alloc` as [spec/16](spec/16-core-library.md) specifies them for Stages 0 to 4;
4. it provides the conforming tools of spec/01 §1.4.4: structured diagnostics with JSON output, the canonical formatter, and `sai explain` for every code it can emit. The semantic protocol is optional, because it is Stage 6;
5. it never crashes, and it reports any internal failure as `INTERNAL_COMPILER_ERROR`.

The claim is substantiated with the conformance suite ([impl/15](impl/15-conformance-and-testing.md) §15.9.4). The specification, not the suite, defines conformance (spec/01 §1.4.3 item 2).

Stages 5 to 7 are fully specified in v0.1, so that implementing them later does not change the v0.1 design (README §4). A v0.1 compiler may implement any prefix of them, since claims are cumulative (§1.1 item 2).

### 2.3 The reference toolchain 0.1.0

Version 0.1.0 of the reference toolchain is the first release that satisfies the definition of §2.2 ([impl/01](impl/01-architecture.md) §1.13.1 item 3). Concretely:

1. it substantiates the claim `SAI 0.1, stages 0-4, targets wasm32-core wasm32-wasi wasm64-core` ([impl/15](impl/15-conformance-and-testing.md) §15.9.5). It does not claim `wasm64-wasi`: that target is experimental because it depends on memory64 support in the Component Model (D-TGT-01), and the reference implementation stops a component build for it with `FEATURE_NOT_IMPLEMENTED` after producing and validating the core module ([impl/07](impl/07-component-lowering.md) §7.11.7, N-D-58);
2. it meets the release requirements of §3.2;
3. it meets the performance targets of [impl/01](impl/01-architecture.md) §1.14.1 on the reference machine;
4. its staged item table lists exactly the library items of Stages 5 to 7 ([impl/01](impl/01-architecture.md) §1.12.1 item 4).

In v0.1 the toolchain version is also the language version against which `compiler` requirements are checked, and the version of `sai:core` and `sai:alloc` (impl/01 §1.13.1 item 1). The bundled standard packages keep their own versions ([package/04](package/04-distribution-and-security.md) §4.4.3).

---

## 3. Releases of the reference toolchain

### 3.1 Every change

Every change to the compiler repository, including the library, the tests and these documents, passes the merge gate of [impl/15](impl/15-conformance-and-testing.md) §15.12.6 on an `x86_64` host and an `aarch64` host. The gate includes the conformance claim of §2.3 item 1 with no deviation, the coverage check, the equivalence, differential and compiler tests, the replay of the fuzzing corpus, and the bootstrap with its fixpoint ([impl/01](impl/01-architecture.md) §1.12.2). The reports of the two hosts must be equal apart from durations, and no failing test is ever quarantined (impl/15 §15.12.6 items 2 and 3). The benchmarks run beside the gate, and a confirmed performance regression blocks the merge unless the change states it and its reason ([impl/01](impl/01-architecture.md) §1.14.2, impl/15 §15.11.3).

### 3.2 A release

1. The binary is built by `cargo xtask dist` in two builds, a bootstrap compiler and a release compiler, so that the embedded library metadata was produced by the compiler that uses it (impl/01 §1.12.2, N-J-35). Its build ID depends on sources only (impl/01 §1.13.2).
2. Fuzzing and the differential tests on fresh seeds run continuously. An open finding of either blocks the next release until it is fixed together with its regression test (impl/15 §15.12.6 item 4).
3. The release candidate is fuzzed for at least 24 CPU hours per fuzz target without a new finding, the full suite is run on both hosts with the released binary, and the claim report of [impl/15](impl/15-conformance-and-testing.md) §15.9.4 is published with the release (impl/15 §15.12.6 item 5, §15.9.5).
4. Every persistent and exchanged format carries a version, and its owner states its compatibility rules ([impl/01](impl/01-architecture.md) §1.13.3, N-J-36). A release that changes a format follows those rules; for example, a new layout of the global package cache uses a new directory, and a new kind of dependency source needs a new lock format version ([package/03](package/03-lock-file.md) §3.10).
5. Diagnostic codes are never reused once assigned (D-TOOL-01), so a code in a published report keeps its meaning in every later release.

---

## 4. After v0.1

### 4.1 Excluded from v0.1 by decision

The following are not part of v0.1. Each exclusion is a deliberate decision of the cited source, not an omission, and a v0.1 program cannot depend on any of them.

| Feature | In v0.1 | Source |
|---|---|---|
| trait objects (`dyn Trait`) | none; dynamic dispatch uses function pointers or enums. `dyn` is a reserved word | D-TYPE-02, D-LEX-04, O-02 |
| opaque return types (`impl Trait`) | none; closures and futures are passed through generic parameters or concrete types | D-TYPE-09, O-02 |
| associated consts, generic associated types, higher-kinded types, specialization, negative impls, auto traits | none | D-TYPE-08, [spec/05](spec/05-types.md) §5.8.1 |
| threads | gated by the target capability `threads` and the effect `thread.spawn`, which no v0.1 target provides; `Send` and `Share` are reserved as the names of future auto traits | D-THREAD-01, [spec/08](spec/08-effects-and-capabilities.md) §8.12.7, [spec/11](spec/11-async-and-concurrency.md) §11.11 |
| linear types | none; resources are affine | D-OWN-06, O-03 |
| derivations offered by libraries | none; `@derive` takes a closed list of traits | D-SYN-09, D-TYPE-08, O-01 |
| macros that rewrite tokens | none; `macro` is a reserved word | draft §83, D-LEX-04 |
| package registries and signing | Git and path sources only | D-PKG-01, O-04, [package/04](package/04-distribution-and-security.md) §4.12 |
| `const fn` and arithmetic in const arguments | none; a const argument is a literal or a const parameter | D-EXPR-07, D-TYPE-07 |
| items declared inside blocks | none; closures serve as local helpers | D-SYN-10 |
| unwinding | none; a panic ends in a trap on every target | D-ERR-01 |
| refinements in exhaustiveness checking | none; integer exhaustiveness uses the natural range of the type | D-EXPR-04 |
| non-`Copy` values in arenas | none; only `T: Copy` values are allocated in an arena | D-OWN-07 |
| profiles other than `dev`, `release` and `tiny` | none; `sai.package` can extend a profile but not define a new one | D-TGT-02 |
| components for `wasm64-wasi` in the reference implementation | `FEATURE_NOT_IMPLEMENTED`; the target is experimental | D-TGT-01, [impl/07](impl/07-component-lowering.md) N-D-58 |
| DWARF for variables, types, scopes and inlined code | none; `dev` has line tables and subprogram entries | [impl/06](impl/06-wasm-lowering-and-layout.md) §6.12.1, N-F-22 |

The other reserved words of D-LEX-04 (`actor`, `catch`, `class`, `defer`, `nil`, `null`, `static`, `throw`, `try`, `undefined`, `yield`) are reserved either for future use or so that their misuse gets a helpful diagnostic ([spec/02](spec/02-lexical-structure.md) §2.5.3); reserving a word does not commit a later version to a feature.

Some things are outside the design altogether rather than postponed: the non-goals of draft §83 and [spec/01](spec/01-introduction.md) §1.1.5, and WASI preview 1, which `wasm32-wasi` does not target (D-TGT-01).

### 4.2 Open issues

The open issues of [draft-reconciliation.md](design/draft-reconciliation.md) §5 are questions for versions after v0.1. Each one is closed in v0.1 by the decision cited, so none of them leaves a gap in this specification. A later version that takes one up records its answer in the decision record.

1. **O-01 Deriving library traits.** How can a library let users derive its traits, such as the decoding trait that `response.json<User>()` needs (draft §80), without macros? The draft points to compile-time metadata (draft §70) and excludes arbitrary token rewriting (draft §83). In v0.1 such impls are written by hand, and the `@derive` list is closed (D-SYN-09, D-TYPE-08).
2. **O-02 Trait objects and opaque return types.** Should SAI add `dyn Trait`, `impl Trait` return types, or both? Today closures and futures can be returned only through generic parameters or concrete types, and `Future<T>` is read as "a type that implements `Future<Output = T>`", not as a type (C-02). Any design has to fit the static-polymorphism model of draft §15 and the explicit effects of function types (D-EFF-02).
3. **O-03 Linear types.** Should SAI have types that must be consumed explicitly, such as a transaction that must be committed or rolled back? Resources are affine in v0.1: nothing forces a resource to be consumed (D-OWN-06, [spec/07](spec/07-ownership-and-borrowing.md) §7.14 item 2). Such a design has to account for panics, which end in a trap without unwinding (D-ERR-01), and for the cancellation of futures by drop (D-ASYNC-01).
4. **O-04 Package registries and signing.** Should SAI add registries, package signatures, checksum transparency, advisories and artifact provenance? [package/04](package/04-distribution-and-security.md) §4.12 sketches how each could be added without changing the meaning of existing `sai.package` and `sai.lock` files; MVS would not change, and a new source kind needs a new lock format version.

### 4.3 How later versions stay compatible

The v0.1 documents already contain the mechanisms that let later versions add features without breaking programs or tools:

1. reserved words keep the syntax of planned features available ([spec/02](spec/02-lexical-structure.md) §2.5.3);
2. a name added to the prelude never breaks a program, because a module-level declaration or import shadows it, with a warning ([spec/04](spec/04-modules-and-names.md) §4.4.4, N-A-9);
3. diagnostic codes are never reused (D-TOOL-01), and every format has a version with stated compatibility rules ([impl/01](impl/01-architecture.md) §1.13.3);
4. Stages 5 to 7 are already fully specified, so implementing them adds behavior without changing the v0.1 rules (README §4);
5. a `compiler` requirement in `sai.package` states the language version that a package needs ([package/01](package/01-package-contract.md) §1.5).
