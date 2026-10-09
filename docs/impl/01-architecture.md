# 01. Compiler Architecture

**Status:** Normative for the reference implementation (see README §2). Other implementations only have to match the observable behavior that the language specification and [13-cli.md](13-cli.md) define.
**Stage:** 0 (pipeline skeleton, crates, query system, VFS, diagnostics collection, internal compiler errors, versioning, the embedded source of `sai:core` and `sai:alloc`, §1.12.1); 1 (their precompiled metadata, §1.12.3); 2 (parallel compilation of owners and function instances); 3 (multi-package sessions and dependency metadata, §1.6.5)
**Related:** [decisions.md](../design/decisions.md) D-MIR-01, D-PKG-04, D-PKG-05, D-TGT-02, D-TOOL-01, D-TOOL-03, D-TOOL-05; [spec/01](../spec/01-introduction.md) §1.4, §1.5.4, §1.6, §1.10; [spec/16](../spec/16-core-library.md) §16.1.6, §16.3; [02-frontend-ast-hir.md](02-frontend-ast-hir.md), [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md), [04-mir.md](04-mir.md), [05-borrow-checker.md](05-borrow-checker.md), [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md), [07-component-lowering.md](07-component-lowering.md), [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), [09-optimization-and-size.md](09-optimization-and-size.md), [10-runtime.md](10-runtime.md), [11-diagnostics.md](11-diagnostics.md), [12-semantic-tooling.md](12-semantic-tooling.md), [13-cli.md](13-cli.md), [14-formatter.md](14-formatter.md), [15-conformance-and-testing.md](15-conformance-and-testing.md); [package/02](../package/02-resolver.md), [package/04](../package/04-distribution-and-security.md)

This chapter defines the structure of the reference compiler: the pipeline from source text to artifacts, the Rust crates that implement it, the demand-driven query system on which every analysis runs, the identifiers and interners shared by all phases, incremental compilation and the on-disk cache, the virtual file system, parallelism and the rules that keep output deterministic, the collection of diagnostics, error handling and internal compiler errors, logging, the build of the embedded `sai:core` and `sai:alloc`, versioning, and performance targets. The algorithms of the individual phases belong to the chapters of the [document map](../README.md): the frontend to [02](02-frontend-ast-hir.md), resolution and type checking to [03](03-name-resolution-and-type-checking.md), MIR to [04](04-mir.md), and so on. The diagnostic data model and the `INTERNAL_COMPILER_ERROR` record belong to [11](11-diagnostics.md), and commands, options and exit codes to [13](13-cli.md). This chapter does not restate them.

---

## 1.1 Overview

### 1.1.1 Terms

- A **session** is one run of the compiler over one build configuration: one invocation of a command of [13](13-cli.md), or the lifetime of one `sai lsp` server. A session owns one query database (§1.4).
- A **build configuration** is the pair (target, profile) of a command (`--target`, `--profile`, [13](13-cli.md) §13.4.1, §13.4.2), together with whether test code is active (`--tests` or `sai test`) and the set of enabled opt-in Wasm features (`--wasm-feature`, [13](13-cli.md) §13.4.7). Every query value is computed for exactly one build configuration.
- A **package unit** is one package of the resolved graph whose source is compiled in the session: the root package, a workspace member, a source dependency, `sai:core` or `sai:alloc`. A dependency used in artifact form ([package/04](../package/04-distribution-and-security.md) §4.5.1) is not a package unit; it is seen only through its WIT ([spec/13](../spec/13-components-and-wit.md) §13.7).
- An **owner** is an `OwnerId` of [02](02-frontend-ast-hir.md) §2.8.2: an item, an associated item, or another definition that has its own HIR. The per-body analyses of [03](03-name-resolution-and-type-checking.md) to [08](08-effect-and-capability-analysis.md) are keyed by owners.
- A **query** is a memoized, pure function from a key to a value, executed by the query system (§1.4). An **input** is a query whose value is set by the driver and is not computed.
- The **driver** is the code of `sai_driver` (§1.3) that creates the session, sets the inputs, demands the queries that a command needs, collects their diagnostics (§1.9), and writes the outputs.

### 1.1.2 Architectural requirements

These requirements bind every crate of the reference implementation. Later sections refine them.

1. **Purity.** Every query is a deterministic function of its key and of the values of the queries it reads. A query performs no I/O, reads no clock, randomness, environment variable or global mutable state, and never emits output (§1.4.2).
2. **Totality.** Every query returns a value for every key, including keys whose source has errors. Errors are reported as diagnostics inside the value and recovered with error placeholders (`Err` nodes, `TyKind::Error`, `ErrorGuaranteed`), never by aborting the session (§1.10.1).
3. **Determinism.** The outputs of a session (artifacts, standard output, diagnostics in both formats, files in `.sai/`) depend only on the inputs that [13](13-cli.md) §13.5.3 lists. They never depend on the number of threads, the scheduling of work, the content of `.sai/cache/`, or whether the session was incremental (§1.8, N-J-30).
4. **One diagnostic channel.** Diagnostics are values returned by queries and collected by the driver (§1.9). No crate other than `sai_cli` and `sai_lsp` writes to standard output or standard error.
5. **No unsafe code outside the interner and the cache decoder.** The crates of §1.3 are `#![forbid(unsafe_code)]`, except `sai_intern` (its arenas, §1.5.2, and the global allocator wrapper, §1.10.4) and the archive reader of `sai_query` (§1.6.4). Each `unsafe` block there has a safety comment and is covered by the tests of [15](15-conformance-and-testing.md) §15.12.

---

## 1.2 The pipeline

### 1.2.1 Phases

A command runs the phases below in this order. Phases 1 to 3 are driver phases that perform I/O. Phases 4 to 13 are queries, demanded by the driver and evaluated on demand, so a phase of one owner may run before an earlier phase of another owner. "Before" in this table therefore means data dependence, not time.

| # | Phase | Main queries or step | Owner chapter | Output |
|---|---|---|---|---|
| 1 | Package discovery | the root package and the workspace | [13](13-cli.md) §13.2 | root path, workspace members |
| 2 | Resolution, lock, fetch | MVS, `sai.lock`, Git fetch into the global cache | [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md), [package/04](../package/04-distribution-and-security.md) | the input `package_graph` (§1.7.3) |
| 3 | File loading | VFS snapshot of every package unit | §1.7, [16](16-interfaces-and-wit.md) §16.2.2 | the inputs `package_files`, `file_contents`, `wit_files` |
| 4 | Lexing and parsing | `source_text`, `parse`, `line_index` | [02](02-frontend-ast-hir.md) §2.2–§2.7 | lossless CST, AST views |
| 5 | Item collection and module tree | `ast_id_map`, `def_collector`, `module_tree` | [02](02-frontend-ast-hir.md) §2.8, [03](03-name-resolution-and-type-checking.md) §3.2, [16](16-interfaces-and-wit.md) §16.5.3 | item trees, `DefPath`s, modules |
| 6 | Name resolution and HIR | `package_scopes`, `hir_owner`, `resolve_owner`, `attrs`, `wit_view`, `interface_impls` | [02](02-frontend-ast-hir.md) §2.9, [03](03-name-resolution-and-type-checking.md) §3.3–§3.4, [16](16-interfaces-and-wit.md) §16.2–§16.4, §16.7, §16.8 | HIR with resolved paths, validated attributes, SAI views of WIT |
| 7 | Signatures, coherence, type checking | `fn_sig`, `adt_def`, `coherence`, `typeck`, `check_match`, `interface_check`, `world_check` | [03](03-name-resolution-and-type-checking.md), [16](16-interfaces-and-wit.md) §16.6–§16.9 | `TypeckResults` |
| 8 | MIR construction | MIR in phase `Built`, then `DropElaborated` | [04](04-mir.md) | generic MIR (D-MIR-01) |
| 9 | Analyses on MIR | borrow checking, effect inference and checking, the Standard Fact Checker | [05](05-borrow-checker.md), [08](08-effect-and-capability-analysis.md), [spec/09](../spec/09-contracts-and-refinements.md) | MIR in phase `Analyzed` |
| 10 | Whole-program checks | reachability and capability checks, WIT and world checks, the check collection of instances (`mono_items`) | [08](08-effect-and-capability-analysis.md), [spec/08](../spec/08-effects-and-capabilities.md) §8.12, [spec/13](../spec/13-components-and-wit.md), [16](16-interfaces-and-wit.md) §16.8.6, §16.10, [04](04-mir.md) §4.13.1 | capability report, world model, generated WIT, the instances of the program |
| 11 | Optimization and monomorphization | generic optimization, the code-generation collection of instances (`mono_instances`), `Mono` MIR, Mono optimization (`opt_mono`), item-level dead-code elimination | [04](04-mir.md), [09](09-optimization-and-size.md) §9.1.2 steps 1–4 | MIR in phases `Optimized` and `Mono`, the item table |
| 12 | Wasm lowering | layout, calling convention, the Wasm passes, identical code folding, final dead-code elimination, module assembly, runtime | [06](06-wasm-lowering-and-layout.md), [09](09-optimization-and-size.md) §9.1.2 steps 5–7, [10](10-runtime.md) | one core module (impl/06 §6.10.1) |
| 13 | Component assembly | Canonical ABI glue, WIT, composition, validation | [07](07-component-lowering.md), [16](16-interfaces-and-wit.md) §16.10.2 | a component |
| 14 | Output | size budgets, `out/` files, `.sai/` | [09](09-optimization-and-size.md), [13](13-cli.md) §13.5 | artifacts and reports |

### 1.2.2 What each command runs

| Command | Phases |
|---|---|
| `sai fmt` | 4 only, for the files it formats, and then the formatter of [14](14-formatter.md). No package resolution is needed for a file argument. |
| `sai check` | 1 to 10, and every lint ([13](13-cli.md) §13.6.2). No code is generated. |
| `sai build`, `sai component build`, `sai run` | 1 to 14 |
| `sai test` | 1 to 12 for the test harness of [spec/14](../spec/14-testing.md), which is a core module ([13](13-cli.md) §13.6.6.2), then test execution ([13](13-cli.md) §13.6.6.3) |
| `sai size`, `sai inspect` | as `sai build`, plus the report queries of [09](09-optimization-and-size.md) and [13](13-cli.md) §13.6.15 |
| `sai lsp`, `sai graph`, `sai diff`, `sai edit` | 1 to 10 on demand, plus the semantic queries of [12](12-semantic-tooling.md) |
| `sai explain`, `sai new`, `sai add`, `sai remove`, `sai update`, `sai lock`, `sai fetch` | no query phases ([13](13-cli.md)) |

A phase that belongs to a stage the compiler does not implement is not run. Its constructs are rejected earlier with `FEATURE_NOT_IMPLEMENTED` ([spec/01](../spec/01-introduction.md) §1.4.5), so a later phase never sees them.

### 1.2.3 Error gates

Analysis continues past errors so that one command reports as many independent errors as possible. Two gates stop the pipeline (N-J-27):

1. **Per-owner gate.** The MIR of an owner is built (phase 8) only if `typeck` and `check_match` of that owner returned no error. Phase 9 of an owner runs only if its MIR was built. An owner skipped by this gate contributes its error-free signature to other owners, so their analyses still run.
2. **Code generation gate.** Phases 11 to 14 run only if phases 1 to 10 reported no diagnostic of severity `error` in the whole build, after level resolution ([11](11-diagnostics.md) §11.3). A command that stops at this gate writes nothing to `out/`. An error reported by phases 11 to 14, such as `SIZE_BUDGET_EXCEEDED` ([package/01](../package/01-package-contract.md) §1.14.3), also leaves `out/` unchanged ([13](13-cli.md) §13.5.1).

Whole-program checks (phase 10) run even if some owners failed the per-owner gate. The owning chapters define how they treat a skipped owner, so that no diagnostic is reported that depends only on the missing analysis ([11](11-diagnostics.md) §11.11.4). Because the gates depend only on the set of diagnostics, which is deterministic (§1.9), the set of phases that run is deterministic too.

---

## 1.3 Crates

### 1.3.1 Crate list

The reference implementation is a Rust workspace (D-TOOL-05). Each crate has one responsibility and one owning chapter. The crates and their direct dependencies are fixed by this table; a dependency that is not listed is not allowed (N-J-28).

| Crate | Responsibility | Owner | Depends on |
|---|---|---|---|
| `sai_stable_hash` | the stable hashes and fingerprints of §1.5.4 | §1.5.4 | — |
| `sai_intern` | interners, `Symbol`, arenas, index vectors (§1.5) | §1.5 | `sai_stable_hash` |
| `sai_query` | the query system, the dependency graph, the on-disk cache (§1.4, §1.6) | §1.4, §1.6 | `sai_intern`, `sai_stable_hash` |
| `sai_vfs` | file identities, snapshots and overlays (§1.7) | §1.7 | `sai_intern`, `sai_query`, `sai_stable_hash` |
| `sai_diagnostics` | the `Diagnostic` model, level resolution, ordering, JSON and human rendering | [11](11-diagnostics.md) | `sai_intern`, `sai_vfs` |
| `sai_syntax` | lexer, parser, CST, AST views, `sai.ungram` and its generated code | [02](02-frontend-ast-hir.md) §2.2–§2.7 | `sai_diagnostics`, `sai_query`, `sai_vfs` |
| `sai_hir` | item trees, `DefPath`s, HIR and HIR lowering | [02](02-frontend-ast-hir.md) §2.8–§2.10 | `sai_diagnostics`, `sai_intern`, `sai_query`, `sai_stable_hash`, `sai_syntax`, `sai_vfs` |
| `sai_wit` | WIT units, WIT loading (`wit-parser`), the WIT model and its printer, the `DefId`s of SAI views | [16](16-interfaces-and-wit.md) §16.1.2, §16.2–§16.4 | `sai_hir`, `sai_package` |
| `sai_resolve` | module trees, scopes, name resolution, lang items, interface implementations and world references | [03](03-name-resolution-and-type-checking.md) §3.2–§3.4, [16](16-interfaces-and-wit.md) §16.6–§16.8 | `sai_hir`, `sai_package`, `sai_wit` |
| `sai_ty` | types, signatures, the trait solver, type checking, match checking, constant evaluation, interface and world checks, flags, WIT generation | [03](03-name-resolution-and-type-checking.md), [16](16-interfaces-and-wit.md) | `sai_resolve`, `sai_wit` |
| `sai_mir` | MIR data structures, construction, drop elaboration, phases, monomorphization | [04](04-mir.md) | `sai_ty` |
| `sai_borrowck` | borrow checking on MIR | [05](05-borrow-checker.md) | `sai_mir` |
| `sai_effects` | effect inference and checking, reachability, capability checks | [08](08-effect-and-capability-analysis.md) | `sai_mir` |
| `sai_sfc` | the Standard Fact Checker; the contract check code that it decides on is built by `sai_mir` ([04](04-mir.md) §4.12.1) | [spec/09](../spec/09-contracts-and-refinements.md) | `sai_mir` |
| `sai_opt` | the MIR optimization passes and pass pipelines, item-level dead-code elimination, size attribution, retention chains, budget checks | [09](09-optimization-and-size.md) §9.1.1, §9.3, §9.4, §9.6.2, §9.10 | `sai_mir` |
| `sai_runtime` | the runtime functions and their bodies | [10](10-runtime.md) | `sai_mir` |
| `sai_codegen_wasm` | layout, calling convention, MIR to Wasm, the Wasm passes, identical code folding, final dead-code elimination, module assembly (`wasm-encoder`), filling the attribution table | [06](06-wasm-lowering-and-layout.md), [09](09-optimization-and-size.md) §9.1.1, §9.5–§9.7 | `sai_mir`, `sai_runtime`, `sai_opt` |
| `sai_component` | Canonical ABI glue, components (`wit-component`); it reads WIT through `sai_wit` | [07](07-component-lowering.md), [spec/13](../spec/13-components-and-wit.md) | `sai_codegen_wasm`, `sai_ty`, `sai_wit` |
| `sai_package` | `sai.package`, MVS, `sai.lock`, Git fetching (`gix`), local package sources, the global cache | [package/01](../package/01-package-contract.md) to [package/04](../package/04-distribution-and-security.md) | `sai_syntax`, `sai_diagnostics`, `gix`, `libc`, `rustix`, `sha2`, `unicode-general-category`, `unicode-normalization` |
| `sai_fmt` | the formatter | [14](14-formatter.md) | `sai_syntax` |
| `sai_library` | the embedded source and metadata of `sai:core` and `sai:alloc` (§1.12) | §1.12 | `sai_query`, `sai_vfs` |
| `sai_semantic` | symbol identity, semantic graphs, semantic operations, semantic diff | [12](12-semantic-tooling.md) | `sai_ty`, `sai_mir`, `sai_effects` |
| `sai_driver` | sessions, the root database, the pipeline, output writing, execution for `sai run` and `sai test` (`wasmtime`) | this chapter | every crate above |
| `sai_cli` | the `sai` binary and its commands | [13](13-cli.md) | `sai_driver` |
| `sai_lsp` | the language server | [12](12-semantic-tooling.md) §12.10 | `sai_driver` |
| `sai_test_harness` | the conformance suite runner and the compiler's own test tools | [15](15-conformance-and-testing.md) | `sai_driver`, `wasmtime` |

### 1.3.2 Rules

1. The dependency graph of the crates is acyclic and follows the table. A crate never depends on a crate that appears below it in the table, except `sai_resolve` and `sai_wit` on `sai_package`, which read the package graph types and the contract queries only.
2. `sai_cli` and `sai_lsp` are the only crates with a `main` function in the shipped toolchain. `sai_lsp` is linked into the `sai` binary as the `sai lsp` command ([13](13-cli.md) §13.1.1 rule 1).
3. Each crate that defines queries declares them in one query group (§1.4.1). `sai_driver` defines `RootDatabase`, which implements every query group. No other crate names `RootDatabase`. Phase crates are written against the traits of their query groups, so each can be tested with a smaller database.
4. Third-party crates of the crates of the shipped toolchain are limited to those of D-TOOL-05, the hashing crates of §1.5.4 (including `sha2` 0.11.0 for the package tree and artifact checksums of [package/03](../package/03-lock-file.md) §3.4 and §3.6, with transitive dependencies `block-buffer`, `cfg-if`, `const-oid`, `cpufeatures`, `crypto-common`, `digest`, `hybrid-array`, and `typenum`); the collection crates `indexmap` and `rustc-hash` (§1.8.3), the work-stealing thread pool `rayon` (§1.8.1), the interrupt handler `ctrlc` (§1.10.5), the tracing facade `tracing` (§1.11), the Unicode data crates `unicode-ident`, `unicode-normalization`, and `unicode-general-category` plus `tinyvec`, a transitive normalization dependency ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.4, D-LEX-06), the SPDX expression parser `spdx` 0.13.6 plus its transitive collection crate `smallvec` for `sai_package` license validation ([package/01](../package/01-package-contract.md) §1.17), and `libc` plus `rustix` 1.1.5 for local package filesystem access. `sai_package` uses generated Unicode 16.0.0 case-folding and default-ignorable tables to make collision checks reproducible and diagnostic paths unambiguous; the generated data and its license notice are checked in. `rustix` provides safe handle-relative directory and manifest operations on supported Unix targets; its transitive crates are `bitflags`, `errno`, `linux-raw-sys`, `windows-sys` and `windows-link`. The package crate uses only safe APIs and remains under `#![forbid(unsafe_code)]`. The cache format of §1.6.4 is implemented without a serialization crate. Adding another third-party crate is a change of this chapter. The crates that only tests, the harness, the fuzz targets and the benchmark runner use are listed in [15](15-conformance-and-testing.md) §15.12.1.
5. `sai_test_harness` is not part of the shipped `sai` binary.
6. A crate that needs the result of a query of a crate below it in the table declares a **hook trait** as a supertrait of its query group. The value types of the hooks are defined in the upper crate, and `RootDatabase` implements the hooks by calling the queries of the lower crates, so every hook call is a tracked dependency (§1.4.2) and the crate graph stays acyclic. A hook is not a crate dependency. `MirHooks` of [04](04-mir.md) §4.1.4 is the hook trait of `sai_mir` (N-E-56).

---

## 1.4 The query system

### 1.4.1 Queries and query groups

All analysis runs on a demand-driven, memoizing query system in the style of salsa (N-J-29). A crate declares its queries in a **query group**: a Rust trait with one method per query. The system provides four kinds of query:

| Kind | Value comes from | Memoized | Examples |
|---|---|---|---|
| input | set by the driver with `set_<query>(key, value)` | yes, until set again | `package_graph`, `package_files`, `file_contents`, `build_config` |
| derived | the query function | yes, with its dependencies | `parse`, `typeck`, `fn_sig`, `mir_built` |
| interned | the interner: `intern_<x>(value) -> Id` and `lookup_<x>(Id) -> value` | for the session | `intern_def_path` ([02](02-frontend-ast-hir.md) §2.8.2), `intern_source_file` and `intern_package_file` (§1.7.1) |
| transparent | the function, evaluated on every call | no | small helpers that only combine other queries |

Every derived query has a key type and a value type. Keys are small, `Copy` where possible, and contain only session IDs (§1.5) or plain data. Values are immutable after creation and are usually `Arc`s, so that reading a memoized value does not copy it. Every key and value type implements `Eq` and `StableHash` (§1.5.4). The queries of each phase are listed by their chapters, for example [02](02-frontend-ast-hir.md) §2.1.2 and [03](03-name-resolution-and-type-checking.md) §3.1.2.

### 1.4.2 Rules for query functions

1. A query function reads other data only through queries. It reads no global or thread-local mutable state, except the interners (whose effects are not observable, §1.5.2) and the tracing facade (§1.11).
2. A query function performs no I/O. Files, the package cache and the network are read only by the driver phases of §1.2.1, whose results become inputs.
3. A query function returns its diagnostics inside its value ([02](02-frontend-ast-hir.md) §2.1.2, [03](03-name-resolution-and-type-checking.md) §3.1.2). There are no side channels such as accumulators. A memoized value therefore reproduces its diagnostics whenever it is reused, which is what makes incremental diagnostics equal to those of a clean build (§1.6.1).
4. A query function never inspects the query stack, the revision, or whether a value was recomputed.
5. A query function is deterministic: equal inputs give equal values, including the order of every vector and the content of every diagnostic. Hash maps may be used inside a query function for lookup, but a value never contains an iteration order of a hash map (§1.8.3).

### 1.4.3 Revisions and memoization

1. The database has a **revision** counter, starting at 1. Setting an input to a value that differs from its current value (by `Eq`) increments the revision. Setting it to an equal value does not.
2. A memoized derived value records: the value, its fingerprint (§1.5.4), the list of queries it read in the order it read them, `verified_at` (the last revision in which the value was known to be valid), `changed_at` (the last revision in which the value changed), and its durability (§1.4.4).
3. Reading a derived query in revision `R` proceeds as follows (the red-green algorithm):
   - If the memo has `verified_at == R`, the value is returned.
   - Otherwise, each recorded dependency is brought up to date in recorded order. If every dependency has `changed_at <= verified_at`, the memo is marked green: `verified_at := R`, and the value is returned without running the query.
   - Otherwise the query is re-executed. If the new value is equal to the old one (by fingerprint and then by `Eq`), the old value is kept and `changed_at` is not advanced (**backdating**, also called early cutoff). Otherwise `changed_at := R`.
4. Backdating is what stops edits from spreading. For example, an edit inside a function body changes `parse` and `hir_owner` of that owner, but not `def_collector`, so no signature query and no other owner's `typeck` re-executes.
5. A derived query whose key no longer exists (a deleted owner) is never read again. Its memo is dropped when the persisted cache is written (§1.6.3).

### 1.4.4 Durability

Every input has a durability, and a derived value has the minimum durability of its dependencies. The database records, per durability level, the last revision in which an input of that level changed. A memo whose durability level has not changed since `verified_at` is green without walking its dependencies.

| Durability | Inputs |
|---|---|
| `Low` | files and `sai.package` of the root package and of the workspace members, editor overlays (§1.7.4) |
| `Medium` | files of path dependencies outside the workspace ([package/04](../package/04-distribution-and-security.md) §4.8 item 3), `package_graph`, `build_config` |
| `High` | files of `sai:core`, `sai:alloc`, and of every dependency read from the global cache, whose content is fixed by its checksum ([package/04](../package/04-distribution-and-security.md) §4.4.2) |

With this rule, an edit of the root package never walks the dependency graphs of the queries of `sai:core`, `sai:alloc` and the cached dependencies.

### 1.4.5 Cycles

A query that, directly or through other queries, reads itself is a **query cycle**. The system detects it with the active-query stack of the current thread and the wait-for graph of §1.8.2. A query cycle is a defect of the compiler, reported as `INTERNAL_COMPILER_ERROR` (§1.10). Every cycle that the language rejects, such as `REEXPORT_CYCLE` or `SUPERTRAIT_CYCLE`, is detected by an explicit algorithm inside one query ([03](03-name-resolution-and-type-checking.md) §3.1.3 item 4), never by the query system.

### 1.4.6 Cancellation

1. The LSP server changes inputs while queries run on worker threads. Before it sets an input, it raises the database's cancellation flag and waits until every running query has stopped.
2. Every query checks the flag when it starts, and long-running query functions check it at their loop heads (for example once per basic block in the borrow checker, once per obligation round in the trait solver). A query that sees the flag unwinds with the dedicated panic payload `Cancelled`.
3. `Cancelled` is not an internal compiler error. The unwinding is caught at the outermost query call of the request, and the request is retried in the new revision or dropped. Memos written before the cancellation stay valid, because they are pure.
4. The CLI uses the same mechanism for interrupts: on SIGINT (Ctrl-C on Windows) the driver raises the flag, and the command stops at the next query boundary with exit code 130 ([13](13-cli.md) §13.3.4).

### 1.4.7 Memory

1. Memos are kept for the whole session, except for the queries marked **LRU** in their chapter. `parse` and `line_index` are LRU queries with a capacity of 256 files in the LSP server and unbounded in the CLI. An evicted memo is recomputed when it is needed again, which is safe because queries are pure.
2. Interned values live as long as the session.

**Current implementation slice.** `sai_query` now provides typed `InputQuery`, `DerivedQuery`, `InternedQuery`, and `TransparentQuery` handles over a synchronous session database. Inputs support set, removal, tracked absence through `get_opt`, per-key durability, and batched changes committed at one revision; changed values advance the durability stamps for their level and every less durable level. When an input's value stays equal but its durability changes, dependency versions keep the same `changed_at` while refreshing their durability, so derived query functions do not rerun only to update durability metadata. Derived queries record reads in order, validate dependencies recursively, replace dynamic dependencies after recomputation, detect cycles, and backdate equal values. Transparent queries run on each call and attribute their reads to the nearest enclosing derived query. Interned IDs are tied to their query table and session and are not stable-hashable. Input mutation is rejected during query execution, and query failures cannot be swallowed by a parent query and then memoized. Every query runtime error other than cancellation poisons the session and raises cancellation; the first non-cancellation error remains the reported failure, even if cancellation was observed earlier. Cancellation can only be reset between queries, and transparent queries check for cancellation after their callback returns. Revision and durability counters are private to the runtime so query callbacks cannot inspect them. Cancellation is exposed through an atomic token and can also be checked by long-running query bodies. Query keys and interned values are bucketed by their stable fingerprints and compared by `Eq`, so a fingerprint collision does not alias values. `sai_vfs::VfsInputs` stores exact raw bytes and read failures as `file_contents`, keyed by validated `FileKey` values, with durability supplied per file. `package_files` records whether the source directory exists and every sorted source-relative entry with its file, directory, or symlink kind; each descendant must have listed directory ancestors, so the inventory cannot imply that a file or symlink was followed. `wit_files` records the declared directory and sorted package-relative `.wit` paths directly inside it, distinguishing an inapplicable listing from an existing empty directory. The three logical input families share one query input table, so a complete `VfsInputSnapshot` replacement applies additions, changes, and removals in one revision. Query keys use stable `PackageKey` strings rather than session-local `PackageId`s, and the WIT listing stores paths rather than `FileId`s; the driver must intern those paths and attach session IDs when it wires package and WIT queries. A tracked `source_text` derived query decodes bytes by `FileKey`, retains BOM and invalid UTF-8 metadata, and represents missing, unreadable, and oversized files as values with empty source text. Durability-only changes preserve its memoized decoded source. The VFS API attaches the session-local `FileId` after reading the stable-keyed memo, so it is excluded from stable hashing. `sai_syntax::SyntaxQueries` now memoizes snapshot-independent `GreenParse` values by stable `FileKey`, reading the tracked `source_text` query. Each value includes the lossless green tree, source-range diagnostic templates, and stable fix recipes. Its snapshot adapter verifies that the interner, snapshot, and VFS agree on the file and exact raw bytes, then binds the green root and diagnostics to the requesting `FileId` and snapshot metadata and recreates fixes. The ephemeral red root used for attribute validation stays inside the query computation. The existing `WorkspaceSnapshot` remains a decoded source view for syntax and diagnostics. File keys normalize versioned dependency references after validating their compatibility line and support the embedded `sai:core` and `sai:alloc` packages. A workspace-relative `SnapshotFile::path` is not a stable key path: the driver must combine a `PackageSourceDirectory` with each source's `PackageSourcePath` and pass package metadata/WIT `PackageRelativePath` values directly; `FileKey` stores their canonical package-relative identity. Session-local `FileId` and editor document versions are excluded from that stable key. Query-group traits, parallel execution and wait-for cycle detection, LRU policies, and the persistent cache of §1.6 are also unimplemented.

---

**Implementation update (IR-193–200).** The diagnostic conversion for `SourceTextStatus` is implemented in `sai_diagnostics`. `sai_vfs::VfsInputs` exposes a memoized `line_index(FileKey)` query that reuses the decoded source allocation. `sai_vfs::FileInterner` maps validated stable `FileKey` values to insertion-ordered session-local `FileId`s and supports reverse lookup. `intern_source_file` accepts the configured `PackageSourceDirectory` and a `PackageSourcePath`, then combines them into the canonical package-relative `FileKey`; `intern_package_file` accepts a `PackageRelativePath` directly for files such as `sai.package` and WIT. Both resolve `PackageId` through the session's `PackageInterner`. `sai_syntax::SyntaxQueries` memoizes the snapshot-independent green parse and rebinds its diagnostics and fix recipes at the snapshot boundary. It also memoizes the ordered item-like syntax kinds used by AST IDs, then validates that shape while binding pointers to the current snapshot. Callers must create one file interner per compiler session. This supersedes the corresponding unimplemented items in the current-slice note above; item-tree and HIR query wiring remain open.

**Implementation update (IR-206, partial).** `sai_syntax::SyntaxQueryGroup` now defines the syntax provider boundary for parsing, AST-ID-map binding, and query-local AST access. `sai_hir::ItemTreeQueries` accepts any `SyntaxQueryGroup`; its provider is held in an `Arc`, so providers need not implement `Clone`. This is an adapter-level boundary: calls still receive `QueryDatabase`, file interning, and snapshot state explicitly. The VFS and HIR query-group traits, `sai_driver::RootDatabase`, other query groups, parallel execution and wait-for cycle detection, LRU policies, and persistent caching remain unimplemented.

**Implementation update (IR-207, partial).** `sai_hir::HirQueryGroup` now exposes the implemented `def_collector` adapter, and `ItemTreeQueries` implements that contract. This group contains only `def_collector`; `hir_owner`, `hir_source_map`, `attrs`, and `RootDatabase` integration remain open.

**Implementation update (IR-208, partial).** `sai_vfs::VfsQueryGroup` exposes the read queries for file contents, package and WIT listings, decoded source, and line indexes. `VfsInputs` implements the group, and `SyntaxQueries<Q>` now consumes any `VfsQueryGroup` provider. `SourceText::from_file_contents` lets alternate providers decode and bind their tracked file contents. VFS input updates remain driver-side operations on `VfsInputs`; they are not part of this read-only query group. `RootDatabase` and the remaining architecture-wide integration are still unimplemented.

**Implementation update (IR-209, partial).** The new `sai_driver::RootDatabase` owns the query runtime, VFS inputs, package/file/symbol interners, syntax queries, and item-tree queries for a compiler session. Its driver methods apply individual or batched VFS updates and request decoded source, line indexes, snapshot-bound parses, AST-ID maps, and item trees. This is a composition facade over the existing adapter APIs; it does not yet implement every query group or provide owner-level HIR, source-map, and attribute queries.

**Implementation update (IR-210, partial).** `RootDatabase::intern_wit_files(PackageId)` reads the tracked WIT inventory, interns each package-relative WIT path as a session-local `FileId`, and preserves the differences between a missing inventory, `NotApplicable`, an empty `Listed` set, and listed files. The rest of package discovery and WIT loading remains outside this driver slice.

**Implementation update (IR-211, partial).** `RootDatabase::intern_package_files(PackageId, PackageSourceDirectory)` reads the tracked source inventory and maps each file's source-relative path through the configured source directory to a session-local `FileId`. Directory and symlink entries remain in the result without file IDs, and a missing inventory, missing source directory, and existing empty directory remain distinct.

**Implementation update (IR-212, partial).** `RootDatabase::line_index(FileId)` resolves the numeric file index through its interner before requesting the stable-keyed VFS line index. `FileId` carries no session provenance, so callers must supply an ID from that same root database; the root can reject out-of-range values but cannot detect an in-range foreign ID.

**Implementation update (IR-213, partial).** `RootDatabase::source_text_and_diagnostic(FileId, SourceRef)` returns decoded source together with the `SOURCE_READ_FAILED` or `IMPLEMENTATION_LIMIT_EXCEEDED` diagnostic derived from its tracked status. The caller supplies a source reference identifying the same file and a snapshot/version corresponding to the VFS source returned by the call because stable file keys do not encode workspace-relative paths or document versions; the adapter does not validate the correspondence.

## 1.5 Identifiers, interning, and arenas

### 1.5.1 Session identifiers

The compiler refers to everything through small integer identifiers. The table lists the identifiers that this chapter assigns; the identifiers of later phases are defined by their chapters ([02](02-frontend-ast-hir.md) §2.8.2, [03](03-name-resolution-and-type-checking.md) §3.5.1).

| Identifier | Representation | Interns | Assigned by |
|---|---|---|---|
| `Symbol` | opaque `(session ID, local u32)` | a string: identifier, keyword text, path segment | `sai_intern` |
| `PackageId` | opaque `(session ID, local u32)` | the package key: identity and compatibility line for a graph node ([package/02](../package/02-resolver.md) §2.1 item 2), including the root and the workspace members with the line of their declared version; identity alone for `sai:core` and `sai:alloc` (N-G-35) | `intern_package` |
| `FileId` | `u32` | (package key string, canonical package-relative path) | `intern_source_file` or `intern_package_file` (§1.7.1) |
| `LocalDefId` | `DefIndex` (`u32`) | a `DefPath` of the package being compiled | `intern_def_path` |
| `DefId` of a foreign package | (`PackageId`, `DefIndex`) | the position of the definition in the package metadata, in `DefPathHash` order | §1.6.5 |
| `Ty`, `GenericArgs`, predicates, trait references | pointer into the type arena | a structural type value | [03](03-name-resolution-and-type-checking.md) §3.5.1 |

Rules (N-J-29):

1. A session identifier is valid only in the session that created it. It is never written to a file, never sent to another process (the LSP protocol and `.sai/index` use the stable keys of §1.6.2), and never part of the output.
2. A session identifier is never used for ordering anything that reaches the output. Its numeric value depends on the order in which queries ran, which can differ between runs and between `--jobs` values. Output order uses the keys of §1.8.3.
3. Interning is idempotent: interning an equal value twice gives the same identifier. Identifier equality is therefore value equality.
4. Interned values are never removed during a session.

### 1.5.2 Interners and arenas

1. `sai_intern` provides the interners and the arenas. An interner is a sharded, concurrent hash table from value to identifier, together with an append-only vector from identifier to value. The copy-only `Arena<T>` allocates individual values and copies of slices into stable boxes; its references remain valid until the arena is dropped. A value of the type arena is allocated once and never moved, so `Ty` can be a reference with pointer equality ([03](03-name-resolution-and-type-checking.md) §3.1.4 item 3).
2. `sai_intern` is the only crate besides the archive reader (§1.6.4) that contains `unsafe` code. The unsafe code is limited to the arena allocator, which hands out references of the arena's lifetime, to the lock-free reads of the append-only vectors, and to the global allocator wrapper of §1.10.4. Each `unsafe` block states its safety argument in a comment, and the crate is tested under Miri and with the concurrency tests of [15](15-conformance-and-testing.md) §15.12.
3. Interning is the only shared mutable state that query functions may touch (§1.4.2 rule 1). It is unobservable, because rules 1 to 3 of §1.5.1 make the result independent of the identifier values.
4. Per-query data is kept in typed index vectors, `IndexVec<I, T>`, whose index types are newtypes over `u32` (`ItemLocalId`, `BindingId`, `BasicBlock`, `Local`). A raw `usize` index is never used across a module boundary. Index types are created with one macro, which also derives `StableHash` (§1.5.4) and the debug format used in dumps.

### 1.5.3 Values

1. Memoized values that are larger than a few words are `Arc<T>` or `Arc<[T]>`. Values are immutable. A phase that transforms a value, such as a MIR pass, takes a clone of the value and returns a new memoized value; it never mutates a value that another query can read.
2. A value that needs to refer to the syntax tree uses `AstPtr` or `AstId` ([02](02-frontend-ast-hir.md) §2.7.4), never a reference into a tree, so that LRU eviction of parse trees (§1.4.7) cannot invalidate it.

### 1.5.4 Stable hashing

Two hash functions are used, each for a fixed purpose:

| Function | Output | Used for |
|---|---|---|
| SipHash-1-3, key zero | 128 bits | `DefPathHash` ([02](02-frontend-ast-hir.md) §2.8.2), the fingerprints of memoized values (§1.4.3), the keys of the on-disk cache (§1.6.2) |
| BLAKE3 | 256 bits | content hashes of files in the cache, checksums of cache files and of the embedded metadata (§1.6.3, §1.12.3) |

1. A **fingerprint** is the stable hash of a value. Stable hashing is defined by the trait `StableHash`, implemented for every key and value type. It hashes the structure of the value with explicit tags and length prefixes, in field order, and encodes integers in little-endian order.
2. Stable hashing never hashes a session identifier. It hashes the stable key instead: `DefPathHash` for a `DefId`, the pair (package key string, canonical package-relative path) for a `FileId`, the string for a `Symbol`, and the structure for a `Ty`. Spans are hashed as (file key, start offset, end offset).
3. Stable hashing never hashes a pointer, a hash map in iteration order, or anything that depends on the host, the platform's pointer width, or the time.
4. The fingerprint of a value is equal on every host for equal values. This is what makes the on-disk cache and the dependency metadata (§1.6) portable between hosts with the same toolchain build.
5. The hash functions are implemented by the `siphasher` and `blake3` crates. Package checksums (`sai1:`) are owned by [package/03](../package/03-lock-file.md) and use their own function; this chapter does not use them except as opaque strings.

---

## 1.6 Incrementality and on-disk caching

### 1.6.1 Two levels of reuse

1. **Within a session**, the query system reuses memoized values with the red-green algorithm of §1.4.3. This is the only level that the LSP server needs while it runs.
2. **Across sessions**, the driver persists the dependency graph of the session and the values of selected queries in `.sai/cache/` ([13](13-cli.md) §13.5.2). The next session in the same package directory with the same build configuration loads them and continues as if the previous session had been the previous revision.
3. The **equivalence invariant** binds both levels (N-J-31): for the same inputs (§1.1.2 item 3), a cold session (empty `.sai/cache/`), a warm session (a cache written for exactly these inputs) and an incremental session (a cache written for other inputs) produce byte-identical outputs: the same artifacts, the same diagnostics in the same order, and the same JSON records. The persisted cache is therefore an optimization only. Deleting it never changes a result ([13](13-cli.md) §13.5.2). [15](15-conformance-and-testing.md) §15.6 tests the invariant.

### 1.6.2 Stable keys

Values persisted across sessions, or exchanged with other processes, never contain session identifiers (§1.5.1 rule 1). The encoder translates every identifier to a stable key, and the decoder translates it back by interning:

| Session identifier | Stable key |
|---|---|
| `DefId`, `LocalDefId`, `OwnerId` | `DefPathHash` ([02](02-frontend-ast-hir.md) §2.8.2) |
| `HirId` | (`DefPathHash` of the owner, `ItemLocalId`) |
| `PackageId` | the package key string: `<namespace>:<name>#<line>`, or `sai:core`, `sai:alloc`, or for the view package of a WIT unit `wit:wasi:<name>@<version>`, `wit:dir:<package key>` or `wit:gen:<package key>` ([16](16-interfaces-and-wit.md) §16.4.1) |
| `FileId` | (package key string, canonical package-relative path) |
| `Symbol` | the string |
| `Ty`, `GenericArgs`, predicates | the structural value, with every `DefId` encoded as above; a type that occurs more than once in one file is encoded once and referenced by its position |
| span | (file key, start byte offset, end byte offset) |

A decoded `DefPathHash` for which the current session has no definition makes the persisted value unusable. The node is treated as red (§1.6.3 rule 5), never as an error.

### 1.6.3 The query cache

1. **Location.** The query cache of a build configuration is the directory `.sai/cache/query/<config>/`, where `<config>` is the first 32 hexadecimal digits of the BLAKE3 hash of the stable encoding of the build configuration (§1.1.1). It contains two files: `graph.bin`, the dependency graph, and `values.bin`, the persisted values.
2. **What is persisted.** `graph.bin` contains one node for every query that the previous sessions executed or verified: its stable key (query name and the stable encoding of its key), the fingerprint of its value, and the list of its dependencies in read order. `values.bin` contains the values of these queries only: `typeck` and `check_match` ([03](03-name-resolution-and-type-checking.md) §3.1.2), the per-owner results of phase 9 (borrow checking, effect checking and the Standard Fact Checker, including their diagnostics), the optimized MIR of each owner (phase 11), and, in the `dev` profile, the encoded Wasm function body of each `Mono` instance (phase 12). Every other query is recomputed when its value is needed, even when its node is green; its node still lets later queries be marked green.
3. **Header.** Both files start with the magic bytes `SAIQ`, the format version as a little-endian `u32`, the toolchain build ID (§1.13.2), and the stable encoding of the build configuration. They end with the BLAKE3 hash of everything before it.
4. **Loading.** At the start of a session, the driver reads both files, if they exist, while it holds the lock on `.sai/lock` ([13](13-cli.md) §13.5.4). If either file is missing, has another magic, format version, build ID or build configuration, fails its checksum, or cannot be read, the driver deletes the directory `.sai/cache/query/<config>/` and starts cold. It reports nothing ([13](13-cli.md) §13.5.2, N-J-31).
5. **Marking.** The inputs of the new session are compared with the input nodes of the loaded graph by fingerprint. An input whose fingerprint differs, or that is new, is red; an input that no longer exists is red. A derived query that the new session needs for the first time is looked up in the loaded graph by its stable key. If a node exists and all its dependencies can be marked green, recursively and in read order, the query is green: its value is decoded from `values.bin` if it is persisted, and recomputed otherwise. If a dependency is red and the recomputed value of that dependency has a different fingerprint, the query is executed. This is the red-green algorithm of §1.4.3 applied to the previous session's graph.
6. **Writing.** At the end of a session whose command writes `.sai/` ([13](13-cli.md) §13.6), and in the LSP server after 10 seconds without input changes and at shutdown, the driver writes a new `graph.bin` and `values.bin`. They contain every node that this session executed or marked green, and every node of the loaded graph that this session did not visit and whose key does not refer to a definition or a file that is gone. Each file is written to a temporary file and renamed into place ([13](13-cli.md) §13.5.4). The values are written in the order of the stable keys, so that equal caches are byte-identical. A cache that cannot be written (a full disk, missing permissions) is skipped without a diagnostic and without an effect on the exit code; the next session starts cold. The same holds for the metadata of §1.6.5.
7. **Interrupted and failed sessions.** A session that ends with an internal compiler error or an interrupt writes no cache (§1.10.3). A session that reported errors writes the cache normally: values that carry errors are valid values, and reusing them reproduces the errors.
8. **Configurations.** `.sai/cache/query/` keeps at most 8 configuration directories. When the driver writes a ninth, it removes the directory that was written least recently, by the time stamp it records in `.sai/cache/query/configs`, a list of (configuration, sequence number) pairs. The time stamp is a counter, not a clock reading, so the eviction is deterministic too.

### 1.6.4 The cache format and the archive reader

1. The cache files and the dependency metadata (§1.6.5) use one format, implemented in `sai_query`: a header, a string table, a type table, and fixed-layout records with little-endian integers and 8-byte alignment. Variable-length data is stored in the string and type tables and referenced by offsets.
2. The **archive reader** reads a file into an 8-byte aligned buffer, verifies the header and the BLAKE3 checksum, and then reads records in place, without copying or per-record validation. It is the only `unsafe` code of `sai_query`. The safety argument is that the buffer is aligned, that the checksum and the build ID prove that the buffer was written by the same toolchain build, and that every offset is checked against the buffer length when the record that contains it is first read.
3. The encoder writes values only through the stable keys of §1.6.2 and the stable hashing rules of §1.5.4, so that the bytes of a cache file depend only on the values it contains.

### 1.6.5 Dependency metadata (Stage 3)

A source dependency is compiled from source once and then reused through its **metadata**: the persisted values of the queries that other packages read (N-J-32).

1. **Scope.** Metadata is used for every package unit except the root package and the workspace members, whose owners are always analyzed by the query cache of §1.6.3. `sai:core` and `sai:alloc` use the embedded metadata of §1.12.3 instead.
2. **Key.** The metadata key of a package unit is the BLAKE3 hash of the stable encoding of: the toolchain build ID (§1.13.2); the package key and the resolved version; the `sai1:` tree hash ([package/03](../package/03-lock-file.md) §3.4) of its files, which the driver computes from the snapshot (§1.7.2) for a path package and takes from the verified cache entry otherwise; the metadata keys of its direct dependencies, in package key order; and the target and the profile of the build configuration. Test code of a dependency is never active ([package/02](../package/02-resolver.md) §2.5.3), and the enabled Wasm features affect only phase 12, which the metadata does not contain ([06](06-wasm-lowering-and-layout.md) §6.2.2), so the key contains neither.
3. **Location.** The metadata of a package unit is the file `.sai/cache/deps/<key>.bin`, where `<key>` is the first 32 hexadecimal digits of the metadata key. Its format is that of §1.6.4.
4. **Content.** The metadata contains, for every definition of the package, the values of the signature queries of [03](03-name-resolution-and-type-checking.md) §3.1.2 (`module_tree`, `package_scopes`, `generics_of`, `predicates_of`, `type_of`, `fn_sig`, `adt_def`, `trait_def`, `impl_header`, `impls_of_trait`, `inherent_impls`, `variances_of`, `const_value`), the validated attributes, the effect signatures and capability facts of [08](08-effect-and-capability-analysis.md), the contracts that callers check ([spec/09](../spec/09-contracts-and-refinements.md)), and the analyzed and optimized MIR of every function, which whole-program code generation needs ([06](06-wasm-lowering-and-layout.md) §6.10.1). It also contains the diagnostics of the package that survive the dependency cap ([11](11-diagnostics.md) §11.3.3), which the driver reports as if they had been computed.
5. **Definition indices.** The `DefIndex` of a definition in the metadata is its position in ascending `DefPathHash` order ([02](02-frontend-ast-hir.md) §2.8.2). `DefId`s of the package are therefore equal in every session that loads the same metadata.
6. **Equality.** Every query of the package returns the same value whether it is decoded from the metadata or computed from source ([03](03-name-resolution-and-type-checking.md) §3.1.3 item 3). [15](15-conformance-and-testing.md) §15.6 tests this by compiling the conformance suite's multi-package tests both ways and comparing the values by fingerprint.
7. **Creation.** If no valid metadata exists for the key, the package unit is compiled from source in the session, like the root package, and its metadata is written at the end of the session. Metadata is written only for a package whose analysis reported no diagnostic of severity `error`. A dependency with errors is therefore compiled from source in every session, and its errors are reported every time.
8. **Invalid metadata.** A metadata file that fails the checks of §1.6.3 rule 4 is deleted and the package is compiled from source, without a diagnostic. Metadata files whose key no package of the current graph has are removed when the driver writes the cache, so `.sai/cache/deps/` contains only the metadata of the last graph.
9. **Files.** The files of a package unit are loaded (§1.7) even when its metadata is valid. They are high-durability inputs that are parsed only when a diagnostic or a tool needs their text, for example to render a secondary label in a dependency.

---

## 1.7 The virtual file system and inputs

### 1.7.1 Files and paths

1. Every file the compiler reads is identified within a session by a `FileId`, interned by `FileInterner` from its stable `FileKey`. SAI source files use `intern_source_file` with the configured source directory and a `PackageSourcePath` relative to that directory; `sai.package` and WIT files use `intern_package_file` with a `PackageRelativePath` relative to the package root. Both forms become the same canonical package-relative path in `FileKey`, so a dependency `dep:` reference to a source file resolves to the same key. Paths use `/` as separator and have no `.` or `..` segment. Source-reference forms for diagnostics are defined separately in [11](11-diagnostics.md) §11.5.4.
2. Paths are compared and sorted as byte strings. No case folding or Unicode normalization is applied. Paths that would collide on a normalizing or case-insensitive file system are rejected by the package system (`PATH_COLLISION`, [package/03](../package/03-lock-file.md) §3.4.4).
3. Absolute host paths never enter a query. The driver alone maps a `FileId` to a host path, to read the file and, in the human renderer, to print paths relative to the current directory ([13](13-cli.md) §13.2.3).
4. Inputs and stable hashes use `FileKey`, the validated stable package key and canonical package-relative path from §1.5.4, instead of hashing the session-local `FileId`. Files without a package reference must still supply their package key explicitly. A workspace-relative path from a source snapshot is not sufficient; the driver must pass source files' configured source directory and source-relative path, or pass package-root files' package-relative path. `FileId` remains the handle used by syntax, spans, and other session-local structures.

### 1.7.2 Inputs and the snapshot

The VFS provides the inputs of the query system. They are set by the driver in phase 3 and never by a query.

| Input | Key | Value |
|---|---|---|
| `file_contents` | `FileKey` (stable package/path identity; §1.7.1) | the exact raw bytes of the file, as `Arc<[u8]>`, or a read failure carrying the I/O error kind |
| `package_files` | `PackageKey` | whether the configured source directory exists and its sorted source-relative entries, including each file, directory, or symlink kind ([03](03-name-resolution-and-type-checking.md) §3.1.3 item 2) |
| `wit_files` | `PackageKey` | `NotApplicable`, or the declared WIT directory and sorted package-relative paths of its direct `.wit` files; an empty existing directory is distinct from `NotApplicable` ([16](16-interfaces-and-wit.md) §16.2.2) |
| `package_graph` | `()` | the resolved package graph (§1.7.3) |
| `build_config` | `()` | the build configuration (§1.1.1) |
| `test_options` | `()` | in a `sai test` session, the selected packages, the filters, `--exact` and `--seed` ([13](13-cli.md) §13.6.6.1); empty otherwise |

1. `source_text(FileId)` is the driver-facing derived query of [02](02-frontend-ast-hir.md) §2.2.1. It resolves the session ID to a stable `FileKey` and decodes `file_contents`: it removes the BOM, exposes invalid UTF-8 locations for `INVALID_UTF8`, and checks the size limit. The current VFS query is keyed directly by `FileKey`; its API reattaches `FileId` to the returned source. [02](02-frontend-ast-hir.md) §2.1.2 calls `source_text` an input. In this architecture the raw bytes are the input and the decoding is a query, so that a file whose bytes did not change is never decoded again.
2. **Snapshot.** In the CLI, the driver reads every file of every package unit once, at the start of phase 3, and keeps the bytes for the session. A file that changes on disk during the session is not read again. The session sees one consistent snapshot, and its output depends only on that snapshot.
3. A file that is listed by `package_files` but cannot be read (permissions, an I/O error, a file removed between listing and reading) is reported by the driver in phase 3 as `SOURCE_READ_FAILED`, an error with the data fields `path` (the source reference of [11](11-diagnostics.md) §11.5.4) and `reason` (the I/O error kind in lowercase, for example `permission denied`). A requested listed file with no `file_contents` input has the reason `not found`. Its `file_contents` value is the error, `source_text` treats it as an empty file, and the code generation gate (§1.2.3) stops the build. Files read by the package system (`sai.package`, `sai.lock`) are reported by [package/01](../package/01-package-contract.md) to [package/04](../package/04-distribution-and-security.md).
4. Symbolic links are listed with their kind and are never followed. Their treatment is owned by [03](03-name-resolution-and-type-checking.md) §3.2.2.

### 1.7.3 The package graph input

Phase 2 is performed by `sai_package` on behalf of the driver: discovery of the workspace, resolution, the lock, and fetching ([package/02](../package/02-resolver.md) to [package/04](../package/04-distribution-and-security.md)). Its result becomes the input `package_graph`, which contains, for each package unit:

1. its `PackageId`, identity and resolved version;
2. its form, source or artifact ([package/04](../package/04-distribution-and-security.md) §4.5.1);
3. its active edges, each with the `PackageId` of the dependency and the name under which the dependency is visible to `use` ([package/01](../package/01-package-contract.md));
4. whether it is the root package, a workspace member, a dependency, or one of `sai:core` and `sai:alloc`.

Nothing else reaches the queries ([package/04](../package/04-distribution-and-security.md) §4.8 item 2, N-G-56). The source URL, the commit, and the location of the global cache are known only to the driver, which uses them to read the files in phase 3. The contract of each package (`sai.package`) is one of its files and is analyzed by the queries of [package/01](../package/01-package-contract.md) like any other file. `sai:core` and `sai:alloc` are added by the driver to every graph without being nodes of it ([package/02](../package/02-resolver.md) N-G-35); their files come from the toolchain itself (§1.12).

### 1.7.4 Overlays in the language server

1. The LSP server sets `file_contents` of an open document to the bytes of the editor's buffer (an **overlay**), and restores the file's contents from disk when the document is closed.
2. It watches the package directories and re-reads a file when the client or the file system reports a change. Each batch of changes is applied in one revision, after cancellation (§1.4.6).
3. A change of a `sai.package` or `sai.lock` file runs phases 1 and 2 again and sets `package_graph`. Files of packages that leave the graph keep their memos until the persisted cache is written (§1.6.3 rule 6).
4. The CLI never uses overlays. Its snapshot is the file system at the start of phase 3 (§1.7.2 rule 2).

---

## 1.8 Parallelism and determinism

### 1.8.1 The thread pool

1. A session has one work-stealing thread pool (`rayon`) with exactly `--jobs` worker threads ([13](13-cli.md) §13.4.5). The driver thread only submits work and waits. The LSP server uses the same pool for requests, with the same default size.
2. Each worker thread has a stack of 32 MiB. The recursive algorithms of the compiler (parsing, lowering, type checking, the trait solver, MIR construction) recurse at most once per nesting level of the program, and the nesting limits of [spec/01](../spec/01-introduction.md) §1.5.4 bound the depth. Exceeding a limit is `IMPLEMENTATION_LIMIT_EXCEEDED`, reported by the phase that measures it (for example [02](02-frontend-ast-hir.md) §2.5.3). A native stack overflow is therefore never a response to a program; if it happens, it is a defect of the compiler (§1.10.4).
3. The limits are fixed numbers, chosen by the owning chapters at or above the minimums of spec/01 §1.5.4 (for example the parser's counted depth of 512, which guarantees the combined source nesting of 256, [02](02-frontend-ast-hir.md) §2.5.3). They do not depend on the host, the stack size or the thread count, so a program is accepted by the reference implementation on every host or on none (N-J-4).

### 1.8.2 Units of parallel work

| Unit | Work done in parallel | Section |
|---|---|---|
| package | packages whose dependencies are complete, in the order of the package graph | §1.6.5 |
| file | `parse`, `def_collector` | [02](02-frontend-ast-hir.md) |
| owner | `typeck`, `check_match`, MIR construction and analyses, optimization of generic MIR | phases 7 to 11 |
| function instance | `opt_mono` of instances that do not depend on each other, in bottom-up order over the call graph; lowering of `Mono` instances to Wasm function bodies; the Wasm passes of each function | [09](09-optimization-and-size.md) §9.3.3, §9.9 item 3, [06](06-wasm-lowering-and-layout.md) |

1. The driver demands the queries of a unit for all units at once (`par_iter` over a sorted key list). The query system lets two threads that demand the same query share one execution: the second thread blocks until the first finishes.
2. A thread that blocks records the edge (waiting thread, query) in the wait-for graph. A cycle in the wait-for graph is a query cycle (§1.4.5) and is detected when the edge that closes it is added.
3. Steps that combine many units run sequentially, in a fixed order: identical code folding, final dead-code elimination and retention chains work on the results merged in symbol order ([09](09-optimization-and-size.md) §9.9 item 3), module assembly orders functions by symbol ([06](06-wasm-lowering-and-layout.md) §6.10.3), and the diagnostic sink sorts diagnostics ([11](11-diagnostics.md) §11.11.1).

### 1.8.3 Determinism rules

The rules below implement [spec/01](../spec/01-introduction.md) §1.6 and [13](13-cli.md) §13.5.3 in the reference implementation (N-J-30). They apply to every crate.

1. **No unordered iteration into results.** A value of a query, a diagnostic, an artifact or a report is never built by iterating a `HashMap` or `HashSet`. Code that needs a map in output order uses `BTreeMap`, `IndexMap` built in a deterministic order, or a vector sorted by an ordered key.
2. **Ordered keys.** The keys used for ordering are: source positions within a file; source-relative paths in byte order; package rank ([11](11-diagnostics.md) §11.11.1 rule 2); `DefPathHash`; symbols as byte strings ([06](06-wasm-lowering-and-layout.md) §6.10.2); and the orders that the owning chapters define. Session identifiers (§1.5.1 rule 2), pointers and thread identifiers are never ordering keys.
3. **No random state.** `std::collections::hash_map::RandomState` is never used. Hash maps use a fixed-seed hasher (`FxHasher`). The workspace's `clippy.toml` lists `RandomState`, `std::collections::HashMap::new` and `std::collections::HashSet::new` under `disallowed-types` and `disallowed-methods`, and the lint is denied.
4. **No ambient inputs.** No query and no output depends on the clock, the date, random numbers, the host name, the user, environment variables other than those of [13](13-cli.md) §13.7, the current directory, absolute paths, the order of directory listings, or the locale. The only nondeterministic values in any output are the `duration_us` fields of [13](13-cli.md) §13.6.6 and the content of internal compiler error records (§1.10.3).
5. **Parallel results are merged by key.** Results computed in parallel are stored by key and read back in key order. A list is never formed in completion order.
6. **Floating point.** The compiler never uses host floating-point arithmetic to decide anything that reaches an output, except the IEEE 754 operations whose results are exactly specified (constant folding of SAI floats follows [spec/05](../spec/05-types.md) and the bit patterns of [06](06-wasm-lowering-and-layout.md)). Formatting of floats in diagnostics uses the shortest round-trip algorithm of [spec/16](../spec/16-core-library.md) §16.14.6.
7. **Limits are counted, not timed.** No analysis has a time limit or a memory limit. Bounded searches (the trait solver's recursion depth, the SFC's limits, monomorphization depth) count steps ([spec/01](../spec/01-introduction.md) §1.5.4 item 2).

These rules are checked by the equivalence tests of [15](15-conformance-and-testing.md) §15.6: every conformance test runs with `--jobs 1` and with `--jobs 8`, cold and warm, and the outputs are compared byte for byte.

---

## 1.9 Collecting diagnostics

1. **Check queries.** For each command, the driver demands a fixed list of **check queries**: the queries whose values carry diagnostics, for every key that the build contains. Each owning chapter states which of its queries carry diagnostics ([02](02-frontend-ast-hir.md) §2.1.2, [03](03-name-resolution-and-type-checking.md) §3.1.2, and the corresponding sections of [04](04-mir.md) to [09](09-optimization-and-size.md) and [package/01](../package/01-package-contract.md)). The list is one table in `sai_driver`. For every package unit compiled from source, it covers every file, module, owner, trait and type of the package, in the order of §1.8.3 rule 2. The table also contains the **session-level** check queries, whose key is the build rather than an item of a package: `mono_items(())` of [04](04-mir.md) §4.13.1 and the queries keyed by `BuildKey` of [08](08-effect-and-capability-analysis.md) §8.7, all of phase 10. The phases of a command (§1.2.2) and the gates (§1.2.3) decide which entries of the table apply.
2. **Independence from reuse.** The driver demands every check query in every session, whether its value is recomputed, reused within the session, or decoded from the cache. Since memoized values carry their diagnostics (§1.4.2 rule 3), the collected set is the same in a cold and in an incremental session (§1.6.1 item 3). A package unit whose metadata is reused contributes the diagnostics stored in its metadata (§1.6.5 rule 4).
3. **Driver diagnostics.** The diagnostics of phases 1 to 3 (discovery, resolution, lock, fetch, `SOURCE_READ_FAILED`) are created by the driver and join the same collection.
4. **The sink.** The collected diagnostics are handed to the sink of `sai_diagnostics`, which runs the stages of [11](11-diagnostics.md): level resolution (11 §11.3), the dependency cap (11 §11.3.3, N-H-4), cascade suppression (11 §11.11.4), deduplication (11 §11.11.2), ordering (11 §11.11.1), the `--max-errors` limit (11 §11.11.3), and rendering in the human (11 §11.10) or JSON (11 §11.9) format. The sink is the only producer of output from diagnostics. `sai_cli` writes what it renders, after the command's analyses are complete ([11](11-diagnostics.md) §11.11.1).
5. **The language server** runs the same sink stages except the limit, and publishes the result per file ([11](11-diagnostics.md) §11.14). It demands the check queries of open files first, so that their diagnostics arrive early, and then those of the rest of the workspace. A published set is always the result of the complete pipeline for the files it covers.
6. **Completeness check.** In debug builds of the compiler and in the test harness, the driver verifies after each session that every `ErrorGuaranteed` created during the session (§1.10.1) belongs to a diagnostic that was collected. A violation is an internal compiler error, so a check query missing from the table is found by the conformance suite.

---

## 1.10 Error handling and internal compiler errors

### 1.10.1 Errors in the program

1. An error in the program is never a failure of the compiler. The phase that finds it pushes a diagnostic into its query's value and continues with a recovery value: an `Err` HIR node, the error type, or an `Err` MIR operand, as its chapter defines.
2. A recovery value carries an `ErrorGuaranteed` token. `ErrorGuaranteed` is a zero-sized type with a private constructor; the only way to obtain one is to push a diagnostic of severity `error` (`Diagnostics::error(..) -> ErrorGuaranteed`). Code that meets a recovery value reports nothing more about it ([11](11-diagnostics.md) §11.11.4 rule C2) and propagates the token.
3. The gates of §1.2.3 use the tokens: an owner whose `typeck` or `check_match` value holds a token is not lowered to MIR, and the code generation gate tests the collected set after level resolution.
4. A lint diagnostic whose level is raised to `deny` is an error for the exit code and for the code generation gate, but it never creates an `ErrorGuaranteed`, because the code it reports on is well formed and later phases can analyze it.

### 1.10.2 Internal invariants

1. A violated internal invariant is a defect of the compiler. Code reports it with the macro `ice!(fmt, args..)`, which panics with a payload that carries the formatted description and the name of the crate (`component`). `assert!`, `unreachable!`, `expect` on an invariant, an index out of bounds, and any other Rust panic are treated the same way, with the crate taken from the panic location. A phase that can still finish its value after detecting a violation, such as the parser's progress guard ([02](02-frontend-ast-hir.md) §2.5.3) and the totality rule of [02](02-frontend-ast-hir.md) §2.1.3 item 1, calls `ice_recover(description)` instead of panicking: it records the internal compiler error exactly as a caught panic is recorded (§1.10.3), and the phase completes its value with error recovery. The session is poisoned either way.
2. Typical invariants are: `LangItems::get` for a lang item that does not exist ([03](03-name-resolution-and-type-checking.md) §3.3.6), a query cycle (§1.4.5), an `ErrorGuaranteed` without a collected diagnostic (§1.9 item 6), a MIR phase invariant of [04](04-mir.md), an invalid module rejected by the validator of [06](06-wasm-lowering-and-layout.md) or [07](07-component-lowering.md), and any diagnostic in `sai:core` or `sai:alloc` (§1.12.4).
3. A defect MUST NOT be reported with any other code. In particular, the compiler never turns a panic into an ordinary error diagnostic, and never continues as if nothing had happened.

### 1.10.3 Reporting an internal compiler error

1. **Catching.** The query system runs every query execution inside `std::panic::catch_unwind`. A panic with the payload `Cancelled` is cancellation (§1.4.6). Every other panic is an internal compiler error. The driver installs a panic hook that records the panic message and location and prints nothing, so that the only output of a panic is the record below (§1.1.2 item 4).
2. **Query stack.** Every worker thread keeps a stack of the queries it is executing. When a query panics, the stack is copied into the report, innermost first. Each entry is written as `<query>(<key>)`, with the key in the stable form of §1.6.2, for example `typeck(acme:app@0.1.0/main.run)` or `parse(acme:app@0.1.0 src/main.sai)`. A query that waits for a query running on another thread contributes that thread's stack below its own entry.
3. **Poisoning.** The first internal compiler error, caught or recorded by `ice_recover`, **poisons** the session. The query system raises the cancellation flag, so that no further query starts and running queries stop at their next check (§1.4.6). Later panics, on any thread, are discarded. A poisoned session writes no artifact, no file in `out/`, no `.sai/cache/` and no `.sai/index/`.
4. **The record.** The driver then passes the diagnostics of the check queries that completed before the poisoning to the sink (§1.9), and after them exactly one `INTERNAL_COMPILER_ERROR` record ([11](11-diagnostics.md) §11.4.6). Its fields are:
   - `message`: `internal compiler error: <description>`, where `<description>` is the `ice!` description or the panic message;
   - `component`: the crate name;
   - `panic_message`: the panic message followed by ` at <file>:<line>:<column>` of the Rust source location;
   - `query_stack`: the stack of rule 2;
   - `compiler_version`: the toolchain version with the build ID as SemVer build metadata, for example `0.1.0+3f2a9c1e5b7d4a60` (§1.13.2);
   - `primary`: the span of the innermost query key that denotes a source range (a file, an owner, a definition), or `null`.
5. **Exit.** The command exits with 101 ([13](13-cli.md) §13.3.4). The set of diagnostics emitted before the record, and the record itself, may depend on scheduling, because the poisoning interrupts the session at a scheduling-dependent point. This is the only exception to the determinism rules (§1.8.3 rule 4, N-J-33).
6. **In the language server**, an internal compiler error in a request is answered with an LSP error response that contains the record's message, and the database is discarded and rebuilt from the inputs. The server keeps running.

### 1.10.4 Failures that cannot be caught

1. **Out of memory.** `sai_intern` defines the global allocator, a wrapper around the system allocator. When an allocation fails, the wrapper writes the fixed line `sai: internal compiler error: out of memory` to standard error, without allocating, and ends the process with `std::process::exit(101)`. No record is written in either output format.
2. **Stack overflow.** The depth limits of §1.8.1 make a native stack overflow impossible for programs within the limits of [spec/01](../spec/01-introduction.md) §1.5.4. If one happens anyway, the process is ended by the Rust runtime, and its exit status is that of the host. The fuzzing targets of [15](15-conformance-and-testing.md) §15.7 generate deeply nested input to find such defects.
3. These two are the only defects that are not reported with an `INTERNAL_COMPILER_ERROR` record.

### 1.10.5 Interrupts

1. The driver installs a handler for SIGINT (Ctrl-C on Windows) through the `ctrlc` crate. The handler raises the cancellation flag (§1.4.6) and sets the interrupted state.
2. After the queries have stopped, the driver passes the diagnostics of the completed check queries to the sink and exits with 130 ([13](13-cli.md) §13.3.4). An interrupted session writes no artifact and no cache, like a poisoned one.
3. A second interrupt ends the process immediately with exit code 130.

---

## 1.11 Logging and tracing

1. The crates are instrumented with the `tracing` facade: one span per query execution, with the query name and the stable key (§1.6.2), and events at the boundaries of the driver phases (§1.2.1).
2. The shipped `sai` binary installs no subscriber. No command-line option and no environment variable enables tracing or logging ([13](13-cli.md) §13.7, N-H-18). The instrumentation therefore never produces output, and a user cannot make a command behave differently by enabling it (N-J-34).
3. `sai_test_harness` and the benchmark runner of [15](15-conformance-and-testing.md) §15.11 install subscribers through the `sai_driver` API (`SessionBuilder::tracing_subscriber`). They use them to count query executions, which the incremental tests of [15](15-conformance-and-testing.md) §15.6 compare with expected counts, and to measure the time of each phase.
4. A subscriber only receives data. No query reads the tracing state, and the presence of a subscriber never changes a value, a diagnostic or an output.
5. The intermediate representations that compiler developers need are printed by `sai inspect` ([13](13-cli.md) §13.6.15), for example `sai inspect mir --phase <phase>`, and not by logging.

---

## 1.12 Building `sai:core` and `sai:alloc`

### 1.12.1 Sources

1. The source of `sai:core` and `sai:alloc` is part of the compiler repository, in the package directories `library/core/` and `library/alloc/`. Each has a `sai.package` with the identity `sai:core` or `sai:alloc` and the toolchain version as its version ([package/02](../package/02-resolver.md) N-G-35, D-PKG-04). Their modules are those of [spec/16](../spec/16-core-library.md) §16.2.
2. `sai_library` embeds every file of both directories into the `sai` binary. The driver adds both packages to every session (§1.7.3) and sets their `file_contents` from the embedded bytes, with `High` durability (§1.4.4). They are never read from the file system, and no installation directory is needed.
3. Source references to their files use the form `dep:sai:core@<version>/<path>` of [11](11-diagnostics.md) §11.5.4, so that a diagnostic that points into the library, such as a secondary label at a trait declaration, names the same file on every host.
4. The library source of a toolchain release contains the items of [spec/16](../spec/16-core-library.md) of every stage that the release implements, and no others. The items of later stages are listed in the **staged item table** `library/staged-items.txt`: one line per item, with its canonical path ([spec/04](../spec/04-modules-and-names.md) §4.9) and its stage (§16.1.6), sorted by path. The table is embedded too (§1.12.5).

### 1.12.2 The bootstrap

The `sai` binary is built by `cargo xtask dist` in two builds, so that the metadata it embeds was produced by the compiler that uses it (N-J-35). The two builds are called the bootstrap compiler and the release compiler, to keep them apart from the stages of the roadmap:

1. **Bootstrap compiler.** The workspace is built with `sai_library` in its `bootstrap` configuration, which embeds the library source but no metadata. A bootstrap compiler always compiles `sai:core` and `sai:alloc` from source. Until the toolchain implements Stage 1, the bootstrap compiler is the shipped binary and the remaining steps are skipped.
2. **Library check.** For every supported build configuration (the four targets of D-TGT-01 and the three profiles `dev`, `release` and `tiny`, with test code inactive), the bootstrap compiler checks both packages with every lint at level `deny`. Any diagnostic fails the build of the toolchain.
3. **Lang-item validation.** For every `LangItemKind` of [03](03-name-resolution-and-type-checking.md) §3.3.6 whose stage the toolchain implements, the bootstrap compiler resolves the canonical path and checks that the item exists and has the kind that the tables of [spec/16](../spec/16-core-library.md) §16.3.2 to §16.3.4 give it. For every line of the staged item table, it checks that the path does not resolve, so that an item is never both present and staged. A failure fails the build.
4. **Metadata.** For every supported build configuration, the bootstrap compiler writes the metadata of §1.6.5 for `sai:core` and `sai:alloc`.
5. **Release compiler.** The workspace is built again, with `sai_library` embedding the source, the staged item table, and the metadata of step 4. The release compiler is the shipped binary.
6. **Fixpoint.** The release compiler repeats step 4 in a temporary directory. The metadata it writes MUST be byte-identical to the embedded metadata. A difference fails the build, because it means that the metadata depends on something other than the library source and the compiler source.

The toolchain's build ID (§1.13.2) is computed from sources only, so it is the same for both compilers, and the embedded metadata carries the build ID of the binary that embeds it.

### 1.12.3 Loading the embedded metadata

1. A release compiler takes the values of the queries of `sai:core` and `sai:alloc` from the embedded metadata of the session's target and profile. It verifies the metadata's checksum and build ID once per session and then reads it in place with the archive reader (§1.6.4).
2. A failed check of embedded metadata is an internal compiler error, because it means that the binary is damaged. The compiler never falls back to compiling the library from source silently.
3. The values decoded from the embedded metadata are equal to the values the compiler would compute from the embedded source (§1.6.5 rule 6). The fixpoint of §1.12.2 step 6 and the tests of [15](15-conformance-and-testing.md) §15.6 check this.
4. In the embedded metadata, the `DefIndex` of a definition is its position in `DefPathHash` order (§1.6.5 rule 5), so the `DefId`s of `sai:core` and `sai:alloc` are the same in every session of one toolchain build.

### 1.12.4 Invariants

1. Every lang item exists and has its kind, by §1.12.2 step 3. `LangItems::get` for a missing item is therefore an internal compiler error, never a diagnostic ([03](03-name-resolution-and-type-checking.md) §3.3.6, [spec/16](../spec/16-core-library.md) §16.3.1).
2. When a bootstrap compiler compiles the library from source, a diagnostic in `sai:core` or `sai:alloc` is an internal compiler error (§1.10.2).
3. The signatures of lang items and of every library item are compared with the listings of [spec/16](../spec/16-core-library.md) by the conformance suite ([15](15-conformance-and-testing.md) §15.8), not by the compiler at run time.

### 1.12.5 Items of later stages

1. `sai_library` provides the query `staged_library_item(path) -> Option<Stage>`, which looks up a canonical path, or the longest listed prefix of it, in the embedded staged item table.
2. When name resolution finds no binding for a path into `sai:core`, `sai:alloc`, or another standard package of [spec/16](../spec/16-core-library.md) §16.16, it calls `staged_library_item` before it reports `UNRESOLVED_NAME` ([03](03-name-resolution-and-type-checking.md) §3.4.4). If the path is listed, it reports `FEATURE_NOT_IMPLEMENTED` with the stage of the table and the canonical path as the feature name ([spec/01](../spec/01-introduction.md) §1.4.5, [spec/16](../spec/16-core-library.md) §16.1.6, N-J-9) instead.
3. A method of a later stage, called with method syntax on a type of an earlier stage, is found in the same way by method lookup when no method of that name exists: the table lists it under its canonical path (`sai:core/str.Str.split` for a method `split` of `Str`, for example).

---
## 1.13 Versioning

### 1.13.1 The toolchain version

1. The toolchain has one SemVer version, printed by `sai --version` ([13](13-cli.md) §13.3.7). In v0.1 it is also the language version that `compiler` requirements are checked against ([package/01](../package/01-package-contract.md) §1.5, [13](13-cli.md) N-H-24), and the version of `sai:core` and `sai:alloc` (§1.12.1).
2. The bundled standard packages have their own versions, which the toolchain does not change ([package/04](../package/04-distribution-and-security.md) §4.4.3).
3. Version 0.1.0 is the toolchain that satisfies the v0.1 definition of the [roadmap](../roadmap.md).

### 1.13.2 The build ID

1. The **build ID** identifies one build of the toolchain. It is the first 16 hexadecimal digits of the BLAKE3 hash of: the toolchain version; the output of `rustc -vV` of the Rust compiler used; the Rust target triple of the binary; the Cargo profile and features that `cargo xtask dist` passes; `Cargo.lock`; and every source file of the workspace crates and of `library/`, each as its repository-relative path followed by its contents, in path order.
2. The build ID depends on sources only, never on the time or the machine of the build (§1.12.2). Two binaries with the same build ID behave identically, so they can share caches.
3. The build ID is stored in the headers of the query cache (§1.6.3) and of all metadata (§1.6.5, §1.12.3), and appears in the `compiler_version` field of internal compiler error records (§1.10.3). `sai --version` does not print it.

### 1.13.3 Formats and their versions

Every persistent or exchanged format has an explicit version. The owner of a format decides its compatibility rules. This table collects them (N-J-36).

| Format | Version marker | Owner | Compatibility |
|---|---|---|---|
| JSON diagnostics stream | header `"format": "sai-diagnostics"`, `"version": 1` | [11](11-diagnostics.md) §11.9.3 | additive changes within a version |
| JSON result records | schema `$id` `urn:sai:schema:diagnostics:1` | [13](13-cli.md) §13.8 | as the diagnostics stream |
| `sai.lock` | header line `lock 1`; checksums `sai1:` | [package/03](../package/03-lock-file.md) §3.2.2, §3.4 | stable within a format version |
| `sai.package` | the `compiler` requirement | [package/01](../package/01-package-contract.md) §1.5 | language version |
| global package cache | directory `v1/` | [package/04](../package/04-distribution-and-security.md) §4.4.1 | a new layout uses a new directory |
| `.sai/index/` | its own header | [12](12-semantic-tooling.md) §12.9 | owned by impl/12 |
| semantic protocol | its own version | [12](12-semantic-tooling.md) | owned by impl/12 |
| MIR text of `sai inspect mir` | none | [04](04-mir.md), [13](13-cli.md) §13.6.15 | unstable; may change with every toolchain version |
| borrow-checker fact dump (test harness only) | first line `borrowck-facts 1` | [05](05-borrow-checker.md) §5.20.1 | unstable; the version is incremented whenever the record syntax changes, and the fact goldens are re-blessed in the same change |
| query cache, dependency metadata, embedded metadata | magic `SAIQ`, format version, build ID | §1.6.3, §1.6.5, §1.12.3 | valid only for the build that wrote it; anything else is discarded silently |
| artifacts (`.wasm`, `.wit`) | the Wasm binary version, the component and WIT versions | [06](06-wasm-lowering-and-layout.md), [07](07-component-lowering.md) | as defined by the Wasm and Component Model specifications |

---

## 1.14 Performance targets

### 1.14.1 Targets

The compiler serves the edit-check-repair loop of draft §78 and §79. The reference implementation MUST meet the targets below on the **reference machine**: 8 physical cores at about 3 GHz, 16 GiB of memory, an SSD, and a supported host operating system, with the default `--jobs` (N-J-37).

| Measurement | Target |
|---|---|
| lexing and parsing, per worker thread | at least 20 MiB of source per second |
| `sai check` of a new package (`sai new`), cold | at most 100 ms |
| `sai check` after an edit inside one function body, in a 100 kLOC workspace with a warm cache | at most 300 ms at the 95th percentile |
| `sai check` of a 100 kLOC workspace, cold | at most 10 s |
| `sai build --profile dev` of a 10 kLOC package, cold | at most 2 s |
| language server: syntax diagnostics after a keystroke | at most 100 ms |
| language server: semantic diagnostics of the open file after a keystroke | at most 500 ms at the 95th percentile |
| peak memory of `sai check` of a 100 kLOC workspace | at most 1 GiB |

1. kLOC counts non-blank, non-comment lines of SAI source in the root package and its workspace members. Dependencies are counted separately and are compiled cold once before the measurement.
2. The workspaces used for the measurements are the generated benchmark corpus of [15](15-conformance-and-testing.md) §15.11, not arbitrary code.
3. The language server targets are measured from the arrival of the change notification to the publication of the diagnostics.

### 1.14.2 Enforcement

1. The benchmarks of [15](15-conformance-and-testing.md) §15.11 measure every target on every merge into the main branch of the compiler repository. A change that makes a measurement more than 10% slower than the median of the previous 10 measurements, or that breaks a target, is not merged unless the change states the regression and the reason for it.
2. Performance never affects results. No analysis has a time budget (§1.8.3 rule 7), so a slow machine produces the same output as the reference machine, later.

---

## 1.15 Decisions introduced by this chapter

### N-J-27 Error gates

- **Rule:** MIR is built for an owner only if its `typeck` and `check_match` reported no error, and the analyses on MIR run only for owners with MIR. Optimization, monomorphization, code generation, component assembly and output run only if phases 1 to 10 reported no error after level resolution (§1.2.3).
- **Rationale:** MIR construction and the analyses on MIR are defined for well-typed bodies only, so running them on recovered bodies would produce diagnostics that depend on the recovery strategy (P8). A clear gate keeps every later phase free of error handling for ill-typed input (P9), while the per-owner granularity still reports the errors of all independent owners in one run.

### N-J-28 A fixed crate graph

- **Rule:** The crates of the reference implementation, their responsibilities and their direct dependencies are those of the table in §1.3.1, and third-party crates are limited to the list of §1.3.2 rule 4. Changing either is a change of this chapter.
- **Rationale:** One owner per responsibility keeps the boundaries between chapters visible in the code (P7). A closed list of dependencies keeps the toolchain auditable and its behavior reproducible (P9, P8).

### N-J-29 Pure queries and session identifiers

- **Rule:** Every analysis is a query of §1.4: a pure, deterministic function that performs no I/O and returns its diagnostics in its value. Inputs have the durability of §1.4.4. Session identifiers are interned, never persisted, never exchanged with other processes, and never used to order output (§1.5.1).
- **Rationale:** Purity is what makes memoization, incremental reuse, parallel evaluation and cancellation correct without special cases (P9). Keeping session identifiers out of persisted data and out of ordering removes the most common source of nondeterminism in incremental compilers (P8).

### N-J-30 Determinism rules

- **Rule:** No result is built by iterating an unordered collection; ordering uses the keys of §1.8.3 rule 2; `RandomState` is forbidden by a denied lint; no query or output depends on ambient inputs; parallel results are merged by key; and no analysis is bounded by time or memory. The only nondeterministic values in any output are `duration_us` and the content of internal compiler error output (§1.8.3).
- **Rationale:** These rules implement the determinism principle of [spec/01](../spec/01-introduction.md) §1.6 and the byte-identical outputs of [13](13-cli.md) §13.5.3 (P8). Stating them as mechanical rules, checked by lints and by equivalence tests, makes them enforceable in review (P9).

### N-J-31 The persisted cache is an optimization only

- **Rule:** A cold, a warm and an incremental session over the same inputs produce byte-identical outputs. The query cache is keyed by the build configuration and the build ID. A cache that does not match, fails its checksum or cannot be read is discarded silently, and a cache that cannot be written is skipped silently (§1.6.1, §1.6.3).
- **Rationale:** If a cache could change a result, users would have to know when to delete it, and agents could not trust a single run (P8, P1). Silent discarding is safe because the cache never carries information that the inputs do not determine (P9).

### N-J-32 Dependency metadata

- **Rule:** A source dependency is compiled once per metadata key (§1.6.5 rule 2) and then read from its metadata. Metadata is written only for packages without errors, carries the diagnostics that survive the dependency cap, and yields values equal to those computed from source.
- **Rationale:** Compiling the dependency graph again on every command would make the cold-check targets of §1.14 unreachable for realistic graphs. Keying the metadata by content and toolchain build, and never caching a package with errors, keeps the reuse unobservable (P8, P9).

### N-J-33 Internal compiler errors

- **Rule:** Every panic other than cancellation is caught at the query boundary and poisons the session. The command emits the diagnostics of the completed check queries and then exactly one `INTERNAL_COMPILER_ERROR` record, writes no artifact and no cache, and exits with 101. Out of memory and native stack overflow are the only defects reported otherwise (§1.10).
- **Rationale:** A defect must be reported as a defect, never disguised as an ordinary diagnostic or ignored (P1, P9). Writing nothing after a defect prevents a broken session from leaving output that later sessions would trust (P8). The content of the report may depend on scheduling, which is acceptable because no correct session produces it.

### N-J-34 No logging configuration in the shipped binary

- **Rule:** The shipped `sai` binary installs no tracing subscriber, and no option or environment variable enables logging. Only the test harness and the benchmark runner install subscribers, through the driver API (§1.11).
- **Rationale:** [13](13-cli.md) N-H-18 limits the environment variables of the toolchain to a closed list, so that a command cannot behave differently because of the environment (P8). Developer visibility is provided by `sai inspect` and by the test tools instead (P1).

### N-J-35 The embedded library is built by the compiler that uses it

- **Rule:** `cargo xtask dist` builds a bootstrap compiler, checks `sai:core` and `sai:alloc` with every lint denied, validates the lang items and the staged item table, writes the metadata for every supported target and profile, embeds it in the release compiler, and requires the release compiler to reproduce the metadata byte for byte (§1.12.2).
- **Rationale:** The library is part of the toolchain (D-PKG-04), so its metadata must match the exact compiler that reads it. The fixpoint check proves that it does, and that the metadata depends only on sources (P8, P9).

### N-J-36 Every format has a version

- **Rule:** Every persistent or exchanged format has an explicit version marker, listed in §1.13.3, and the private formats are additionally bound to the build ID (§1.13.2).
- **Rationale:** A reader must be able to tell whether it understands a file before it interprets it (P1). Binding private formats to the build ID means that they never need compatibility rules of their own (P7).

### N-J-37 Performance targets

- **Rule:** The reference implementation meets the targets of §1.14.1 on the reference machine, measured by the benchmarks of [15](15-conformance-and-testing.md) §15.11 on every merge. A regression of more than 10% blocks the merge unless the change states it.
- **Rationale:** Draft §78 and §79 make fast checking part of the language's purpose: agents repeat the edit-check-repair loop many times. Measured targets turn that purpose into a reviewable requirement, while §1.8.3 rule 7 keeps performance from ever changing a result (P8).

---

## 1.16 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; a library item of a stage the toolchain does not implement, found through the staged item table (§1.12.5) |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | defined in [spec/01](../spec/01-introduction.md) §1.5.4; the nesting limits that bound recursion in the compiler (§1.8.1) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11](11-diagnostics.md) §11.4.6; the record, its fields and the session poisoning (§1.10.3) |
| `SOURCE_READ_FAILED` | error | a file of a package unit is listed but cannot be read; data: `path`, `reason` (§1.7.2 rule 3) |
